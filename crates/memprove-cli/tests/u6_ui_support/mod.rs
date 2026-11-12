//! Shared fixture of the U6 process-level tests (once of `ui export`): the real
//! `memprove` and `memproved` in short temporary directories under
//! `/tmp`, a Git repository with one committed file and one passing test
//! script, a control token (the operator's) and an agent token with the
//! scopes `mcp-read` and `mcp-write` (the one the CLI uses). Nothing outside
//! the temporary directory is read or written, and no token is printed.

#![allow(dead_code)] // Each test file uses part of the fixture.

#[path = "../u1e_support/mod.rs"]
mod base;

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Output,
};

#[allow(unused_imports)] // Each test file uses part of the fixture.
pub use base::{
    Daemon, bootstrap, cli_binary, git, isolated, json, repository, short_tempdir, string, text,
};

pub struct Fixture {
    pub _guard: tempfile::TempDir,
    pub root: PathBuf,
    pub data: PathBuf,
    /// The operator's control token file.
    pub control: PathBuf,
    /// The agent token file: `<config>/token`, scopes `mcp-read` and
    /// `mcp-write`.
    pub agent: PathBuf,
    pub agent_secret: String,
    pub agent_token_id: String,
    pub config: PathBuf,
    pub repo: PathBuf,
    pub project: String,
    pub daemon: Option<Daemon>,
}

impl Fixture {
    pub fn new() -> Self {
        let (guard, root) = short_tempdir();
        let data = root.join("d");
        let control = root.join("c");
        let config = root.join("cfg");
        let repo = root.join("r");
        fs::create_dir(&config).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
        repository(&repo);
        bootstrap(&data, &control);
        let daemon = Daemon::start(&data, &root.join("log"));
        let operator = |words: &[&str]| {
            isolated(cli_binary())
                .current_dir(&root)
                .env("HOME", &root)
                .env("MEMPROVE_CONFIG_DIR", &config)
                .arg("--data-dir")
                .arg(&data)
                .arg("--token-file")
                .arg(&control)
                .args(words)
                .output()
                .unwrap()
        };
        let registered = json(&operator(&["project", "register", repo.to_str().unwrap()]));
        let project = string(&registered, "project_id").to_owned();
        let created = json(&operator(&[
            "token",
            "create",
            "--label",
            "agent",
            "--scope",
            "mcp-read",
            "--scope",
            "mcp-write",
        ]));
        let agent = config.join("token");
        let agent_secret = string(&created, "secret").to_owned();
        fs::write(&agent, format!("{agent_secret}\n")).unwrap();
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            _guard: guard,
            root,
            data,
            control,
            agent,
            agent_secret,
            agent_token_id: string(&created, "token_id").to_owned(),
            config,
            repo,
            project,
            daemon: Some(daemon),
        }
    }

    /// `memprove` in `cwd` with the agent token and the configuration
    /// directory of this fixture.
    pub fn agent_in(&self, cwd: &Path, words: &[&str]) -> Output {
        isolated(cli_binary())
            .current_dir(cwd)
            .env("HOME", &self.root)
            .env("MEMPROVE_CONFIG_DIR", &self.config)
            .arg("--data-dir")
            .arg(&self.data)
            .arg("--token-file")
            .arg(&self.agent)
            .args(words)
            .output()
            .unwrap()
    }

    /// [`Self::agent_in`] in the repository.
    pub fn agent(&self, words: &[&str]) -> Output {
        self.agent_in(&self.repo, words)
    }

    /// Like [`Self::agent`], and the exit code must be 0.
    pub fn agent_ok(&self, words: &[&str]) -> Output {
        let output = self.agent(words);
        assert_eq!(
            output.status.code(),
            Some(0),
            "memprove {}: {}",
            words.join(" "),
            text(&output.stderr)
        );
        output
    }

    /// The operator's CLI.
    pub fn control_in(&self, cwd: &Path, words: &[&str]) -> Output {
        isolated(cli_binary())
            .current_dir(cwd)
            .env("HOME", &self.root)
            .env("MEMPROVE_CONFIG_DIR", &self.config)
            .arg("--data-dir")
            .arg(&self.data)
            .arg("--token-file")
            .arg(&self.control)
            .args(words)
            .output()
            .unwrap()
    }

    /// `ui export --out <out>` plus `extra` in the repository, with the
    /// agent token.
    pub fn export(&self, out: &Path, extra: &[&str]) -> Output {
        let mut words = vec!["ui", "export", "--out", out.to_str().unwrap()];
        words.extend_from_slice(extra);
        self.agent(&words)
    }

    /// `begin --new-task <title>`, one passing `run`, and one Checkpoint
    /// stating `fact` on that run, in the repository; the Session stays
    /// open. Returns the Task id.
    pub fn task_with_fact(&self, title: &str, fact: &str) -> String {
        let begun = json(&self.agent(&["begin", "--project", &self.project, "--new-task", title]));
        let task = string(&begun, "task_id").to_owned();
        self.agent_ok(&[
            "run",
            "--test-format",
            "cargo",
            "--",
            "sh",
            "tests/check.sh",
        ]);
        self.agent_ok(&[
            "checkpoint",
            "create",
            "--fact",
            fact,
            "--evidence",
            "r1",
            "--next",
            "carry on",
        ]);
        task
    }

    /// Ends the context's Session.
    pub fn end(&self) {
        self.agent_ok(&["end"]);
    }

    /// A Task that was created and never begun (`proposed`).
    pub fn idle_task(&self, title: &str) -> String {
        let created = json(&self.agent(&[
            "task",
            "create",
            "--project",
            &self.project,
            "--title",
            title,
        ]));
        string(&created, "task_id").to_owned()
    }

    /// The ledger check of the Project: whether it is valid, and the chain
    /// head.
    pub fn ledger(&self) -> (bool, String) {
        let verified = json(&self.control_in(
            &self.root,
            &["ledger", "verify", "--project", &self.project],
        ));
        let valid =
            verified.get("valid") == Some(&memprove_protocol::strict_json::JsonValue::Bool(true));
        (valid, string(&verified, "head_event_sha256").to_owned())
    }

    pub fn stop(mut self) {
        if let Some(daemon) = self.daemon.take() {
            daemon.stop();
        }
    }
}

/// Every file below `dir` (relative, `/`-separated) with its bytes. A
/// directory that does not exist is an empty map.
pub fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(name, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The text between `<pre id="<id>">` and the `</pre>` after it, decoded:
/// the five named references of the pages and the numeric ones.
pub fn pre_text(page: &str, id: &str) -> String {
    let open = format!("<pre id=\"{id}\">");
    let start = page
        .find(&open)
        .unwrap_or_else(|| panic!("no <pre id=\"{id}\">"))
        + open.len();
    let end = start + page[start..].find("</pre>").expect("no </pre>");
    decode(&page[start..end])
}

fn decode(escaped: &str) -> String {
    let mut out = String::with_capacity(escaped.len());
    let mut rest = escaped;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest.find(';').expect("an unterminated reference");
        let reference = &rest[1..end];
        out.push(match reference {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "#39" => '\'',
            numeric => {
                let digits = numeric.strip_prefix("#x").expect("an unknown reference");
                char::from_u32(u32::from_str_radix(digits, 16).unwrap()).unwrap()
            }
        });
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}
