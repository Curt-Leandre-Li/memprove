//! Shared harness of the P4-G process-level tests (ADR 0014 E7.11): a
//! minimal MCP client written here, which drives the real `memprove mcp
//! serve` over piped standard streams, one JSON line per message, one line
//! read per answer within 15 s, standard error kept whole. No SDK. Token
//! secrets are never printed.

#![allow(dead_code)] // Each test file uses part of the harness.

use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::p4b_support::{cli_binary, isolated};

/// The longest a test waits for one answer.
pub const DEADLINE: Duration = Duration::from_secs(15);

/// Where a server looks: its configuration directory, data directory,
/// token sources, project directory, and client session.
#[derive(Clone, Debug)]
pub struct McpEnv {
    pub config_dir: PathBuf,
    pub data_dir: Option<PathBuf>,
    pub token_file: Option<PathBuf>,
    pub token: Option<String>,
    pub project_dir: PathBuf,
    pub client: Option<String>,
    /// U1 (A1): `CLAUDE_CODE_SESSION_ID`, as Claude Code sets it.
    pub claude_code_session: Option<String>,
}

impl McpEnv {
    /// The agent's configuration, data directory, and token file, in the
    /// repository `project_dir`.
    pub fn of(config_dir: &Path, data_dir: &Path, token_file: &Path, project_dir: &Path) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            data_dir: Some(data_dir.to_path_buf()),
            token_file: Some(token_file.to_path_buf()),
            token: None,
            project_dir: project_dir.to_path_buf(),
            client: None,
            claude_code_session: None,
        }
    }
}

/// One `tools/call` answer.
#[derive(Debug, Clone)]
pub struct Called {
    /// The line as received.
    pub line: String,
    /// `result.content[0].text`.
    pub text: String,
    /// `result.structuredContent`; T21 (V6-F1): for a `pack` or `claims`
    /// success, which has none, the object `result.content[1].text` holds.
    pub structured: Value,
    /// `result.isError`.
    pub is_error: bool,
}

impl Called {
    /// `structuredContent.failure`, when it is an error.
    pub fn failure(&self) -> Option<&str> {
        self.is_error
            .then(|| self.structured["failure"].as_str())
            .flatten()
    }
}

/// A running `memprove mcp serve`.
pub struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    readers: Vec<thread::JoinHandle<()>>,
    next_id: u64,
}

impl Mcp {
    pub fn start(env: &McpEnv) -> Self {
        let mut command = isolated(cli_binary());
        command
            .args(["mcp", "serve"])
            .current_dir(&env.project_dir)
            .env("MEMPROVE_CONFIG_DIR", &env.config_dir)
            .env("CLAUDE_PROJECT_DIR", &env.project_dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(data_dir) = &env.data_dir {
            command.env("MEMPROVE_DATA_DIR", data_dir);
        }
        if let Some(token_file) = &env.token_file {
            command.env("MEMPROVE_TOKEN_FILE", token_file);
        }
        if let Some(token) = &env.token {
            command.env("MEMPROVE_TOKEN", token);
        }
        if let Some(client) = &env.client {
            command.env("MEMPROVE_CLIENT_SESSION", client);
        }
        if let Some(session) = &env.claude_code_session {
            command.env("CLAUDE_CODE_SESSION_ID", session);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        let (sender, lines) = mpsc::channel();
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let all = Arc::clone(&stdout);
        let reader = thread::spawn(move || {
            loop {
                let mut line = Vec::new();
                match out.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        all.lock().unwrap().extend_from_slice(&line);
                        let text = String::from_utf8(line).unwrap();
                        if sender.send(text).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut err = child.stderr.take().unwrap();
        let log = Arc::clone(&stderr);
        let logger = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = err.read_to_end(&mut bytes);
            log.lock().unwrap().extend_from_slice(&bytes);
        });
        Self {
            child,
            stdin,
            lines,
            stdout,
            stderr,
            readers: vec![reader, logger],
            next_id: 0,
        }
    }

    /// Sends `line` and returns the next line of standard output.
    pub fn ask(&mut self, line: &str) -> String {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
        let answer = self
            .lines
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|_| panic!("no answer within {DEADLINE:?}"));
        answer.strip_suffix('\n').unwrap().to_owned()
    }

    /// A modern `tools/call` of `tool` with `arguments`.
    pub fn call(&mut self, tool: &str, arguments: &Value) -> Called {
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": "tools/call",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
                "name": tool,
                "arguments": arguments,
            },
        });
        let line = self.ask(&request.to_string());
        let value: Value = serde_json::from_str(&line).unwrap();
        let result = &value["result"];
        assert_eq!(result["resultType"], "complete", "{line}");
        let shown = if line.len() > 2000 {
            let head = line.chars().take(600).collect::<String>();
            format!("{head}… ({} bytes)", line.len())
        } else {
            line.clone()
        };
        println!("[mcp] {tool} {arguments}\n{shown}");
        let is_error = result["isError"].as_bool().unwrap();
        let blocks = result["content"].as_array().unwrap();
        // T21 (V6-F1): a Pack result (`pack`, `claims`) carries its object as
        // a second text block and has no `structuredContent`; every other
        // result has one text block and `structuredContent`.
        let pack_result = !is_error && matches!(tool, "pack" | "claims");
        // U12 T2 (Owner O4; the Owner's O8 exception of 2026-11-05, ledger
        // decision 2493): a `pack` success without `query` is [opening, Pack,
        // object], the opening starting with `memprove: below is the Context Pack of Task `; with a
        // query, and for `claims`, [Pack, object].
        let opening = pack_result && tool == "pack" && arguments.get("query").is_none();
        let first = usize::from(opening);
        let structured = if pack_result {
            assert_eq!(blocks.len(), 2 + first, "{line}");
            if opening {
                assert!(
                    blocks[0]["text"]
                        .as_str()
                        .unwrap()
                        .starts_with("memprove: below is the Context Pack of Task "),
                    "{line}"
                );
            }
            assert!(result.get("structuredContent").is_none(), "{line}");
            serde_json::from_str(blocks[first + 1]["text"].as_str().unwrap()).unwrap()
        } else {
            assert_eq!(blocks.len(), 1, "{line}");
            result["structuredContent"].clone()
        };
        Called {
            text: blocks[first]["text"].as_str().unwrap().to_owned(),
            structured,
            is_error,
            line,
        }
    }

    /// A call that must succeed.
    pub fn ok(&mut self, tool: &str, arguments: &Value) -> Called {
        let called = self.call(tool, arguments);
        assert!(!called.is_error, "{}", called.line);
        called
    }

    /// Closes standard input and waits for the exit; returns everything
    /// written on standard output and standard error.
    pub fn finish(mut self) -> (String, String) {
        drop(self.stdin.take());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(started.elapsed() < DEADLINE, "the server did not exit");
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(0));
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
        let stdout = String::from_utf8(self.stdout.lock().unwrap().clone()).unwrap();
        let stderr = String::from_utf8(self.stderr.lock().unwrap().clone()).unwrap();
        (stdout, stderr)
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The tokens Claude Code would count for `text` by ADR 0014 E7.6's
/// ceilings: 1.5 per non-ASCII UTF-16 code unit, 1 per 3.5 ASCII ones.
pub fn estimated_tokens(text: &str) -> f64 {
    let (ascii, other) = text
        .encode_utf16()
        .fold((0_u32, 0_u32), |(ascii, other), unit| {
            if unit < 0x80 {
                (ascii + 1, other)
            } else {
                (ascii, other + 1)
            }
        });
    f64::from(ascii) / 3.5 + f64::from(other) * 1.5
}

/// Claude Code's warning line for one tool output (Owner condition w).
pub const WARNING_TOKENS: f64 = 10_000.0;

/// Standard base64 of `bytes` (for the token search).
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |value, (index, byte)| {
                value | u32::from(*byte) << (16 - 8 * index)
            });
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(
                    ALPHABET[(value >> (18 - 6 * index) & 63) as usize],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Every regular file under `root`, with its bytes.
pub fn files_under(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() && !path.is_symlink() {
                pending.push(path);
            } else if path.is_file() {
                found.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
    }
    found.sort();
    found
}

/// The last-Pack files (`*.pack.json`) under `config_dir`, by name.
pub fn last_packs(config_dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    files_under(config_dir)
        .into_iter()
        .filter(|(path, _)| path.to_string_lossy().ends_with(".pack.json"))
        .collect()
}

/// The level the Pack's `Omitted: ` line names.
pub fn level(pack: &str) -> u32 {
    let line = pack
        .lines()
        .find(|line| line.starts_with("Omitted: level "))
        .unwrap_or_else(|| panic!("no Omitted line:\n{pack}"));
    line["Omitted: level ".len()..]
        .split([' ', ';'])
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

/// `OsString` of a path.
pub fn os(path: &Path) -> OsString {
    path.as_os_str().to_owned()
}
