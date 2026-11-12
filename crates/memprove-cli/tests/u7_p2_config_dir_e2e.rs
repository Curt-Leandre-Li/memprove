//! U7 P2 (D2 F13, Owner O4; ADR 0014 "Amendment (U7, W-P)"), through the
//! real `memprove` and `memproved`: `hook session-start` and `mcp serve`
//! take `--config-dir <DIR>`, which wins over `MEMPROVE_CONFIG_DIR` of the
//! process environment (D2: a shell that already exported another
//! installation's directory); `hook install` writes it into the hook's and
//! the `.mcp.json` entry's `args` (keeping `env.MEMPROVE_CONFIG_DIR`),
//! rewrites entries an earlier install wrote without it, and is idempotent;
//! `mcp print-config` prints the same; `doctor`'s `hook` and `mcp-entry`
//! lines check the argument when there is one (PASS when it is doctor's
//! directory, FAIL when another), else the `env` (WARN when neither); and
//! the hook's statement when it finds no context names where its directory
//! came from, `--config-dir` included.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::{
    fs,
    io::Write as _,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Output, Stdio},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use support::{Fixture, cli_binary, isolated, text};

/// `memprove <words>` in the repository with `MEMPROVE_CONFIG_DIR` set
/// to `config` and `MEMPROVE_DATA_DIR` to the fixture's, reading `input`.
fn run_in(fixture: &Fixture, config: &Path, words: &[&str], input: &str) -> Output {
    let mut child = isolated(cli_binary())
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("MEMPROVE_CONFIG_DIR", config)
        .env("MEMPROVE_DATA_DIR", &fixture.data)
        .args(words)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    println!(
        "$ memprove {}\n{}{}[exit {:?}]",
        words.join(" "),
        text(&output.stdout),
        text(&output.stderr),
        output.status.code()
    );
    output
}

/// The hook's output for a new client session, with `extra` arguments.
fn hook(fixture: &Fixture, config: &Path, extra: &[&str]) -> String {
    let mut words = vec!["hook", "session-start"];
    words.extend_from_slice(extra);
    let input = format!(
        r#"{{"hook_event_name":"SessionStart","session_id":"client-p2","source":"startup","cwd":"{}"}}"#,
        fixture.repo.display()
    );
    let output = run_in(fixture, config, &words, &input);
    assert_eq!(output.status.code(), Some(0));
    text(&output.stdout)
}

/// The first text of the MCP `pack` tool's result, and whether it is an
/// error, with `extra` arguments to `mcp serve`.
fn mcp_pack(fixture: &Fixture, config: &Path, extra: &[&str]) -> (String, bool) {
    let mut words = vec!["mcp", "serve"];
    words.extend_from_slice(extra);
    let request = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":\"2026-07-28\",\"io.modelcontextprotocol/clientCapabilities\":{}},\"name\":\"pack\",\"arguments\":{}}}\n";
    let output = run_in(fixture, config, &words, request);
    let stdout = text(&output.stdout);
    let reply = parse_strict(stdout.lines().next().unwrap_or_else(|| panic!("no reply"))).unwrap();
    let result = reply.get("result").unwrap_or_else(|| panic!("{stdout}"));
    let first = result
        .get("content")
        .and_then(JsonValue::as_array)
        .and_then(|content| content.first())
        .and_then(|item| item.get("text"))
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned();
    let error = result.get("isError") == Some(&JsonValue::Bool(true));
    (first, error)
}

fn read(path: &Path) -> JsonValue {
    parse_strict(&fs::read_to_string(path).unwrap()).unwrap()
}

fn strings(value: &JsonValue) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_owned())
        .collect()
}

fn handler_args(settings: &JsonValue) -> Vec<Vec<String>> {
    settings
        .get("hooks")
        .and_then(|hooks| hooks.get("SessionStart"))
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .flat_map(|group| group.get("hooks").and_then(JsonValue::as_array).unwrap())
        .map(|handler| strings(handler.get("args").unwrap()))
        .collect()
}

fn server_args(mcp_json: &JsonValue) -> Vec<String> {
    strings(
        mcp_json
            .get("mcpServers")
            .and_then(|servers| servers.get("memprove"))
            .and_then(|server| server.get("args"))
            .unwrap(),
    )
}

fn other_dir(fixture: &Fixture) -> PathBuf {
    let other = fixture.root.join("other-cfg");
    fs::create_dir_all(&other).unwrap();
    fs::set_permissions(&other, fs::Permissions::from_mode(0o700)).unwrap();
    other
}

fn path_text(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn u7_p2_the_config_dir_argument_wins_over_the_environment() {
    let fixture = Fixture::new();
    fixture.task_with_fact("P2 config dir", "the check passes");
    let other = other_dir(&fixture);
    let config = path_text(&fixture.config).to_owned();

    // D2 F13: another MEMPROVE_CONFIG_DIR in the process environment.
    let lost = hook(&fixture, &other, &[]);
    assert!(
        lost.contains("this directory has no memprove context"),
        "{lost}"
    );
    assert!(
        lost.contains(&format!(
            "the hook's configuration directory is {} (from MEMPROVE_CONFIG_DIR)",
            path_text(&other)
        )),
        "{lost}"
    );
    // The argument wins: the Pack of the context in that directory.
    let found = hook(&fixture, &other, &["--config-dir", &config]);
    assert!(found.contains("\n```text\nPack: "), "{found}");

    // With the argument naming a directory with no context: the statement
    // says it came from --config-dir, and the order the hook takes them in.
    let named = hook(
        &fixture,
        &fixture.config,
        &["--config-dir", path_text(&other)],
    );
    assert!(
        named.contains(&format!(
            "the hook's configuration directory is {} (from --config-dir)",
            path_text(&other)
        )),
        "{named}"
    );
    assert!(
        named.contains(
            "the hook takes, in order, --config-dir, MEMPROVE_CONFIG_DIR, HOME/.memprove/config \
             when it exists, XDG_CONFIG_HOME, HOME"
        ),
        "{named}"
    );

    // The MCP server: the same.
    let (text, error) = mcp_pack(&fixture, &other, &[]);
    assert!(error, "{text}");
    let (text, error) = mcp_pack(&fixture, &other, &["--config-dir", &config]);
    assert!(!error, "{text}");
    // U12 T2 (Owner O4): the result's first block is the opening, the Pack
    // the next.
    assert!(
        text.starts_with("memprove: below is the Context Pack of Task "),
        "{text}"
    );
    fixture.end();
    fixture.stop();
}

/// The two files `hook install` edits in the fixture's repository, and
/// what their entries name.
struct Entries<'a> {
    fixture: &'a Fixture,
    settings_path: PathBuf,
    mcp_path: PathBuf,
    executable: String,
    config: String,
}

impl<'a> Entries<'a> {
    fn of(fixture: &'a Fixture) -> Self {
        fs::create_dir_all(fixture.repo.join(".claude")).unwrap();
        Self {
            fixture,
            settings_path: fixture.repo.join(".claude/settings.local.json"),
            mcp_path: fixture.repo.join(".mcp.json"),
            executable: path_text(&fs::canonicalize(cli_binary()).unwrap()).to_owned(),
            config: path_text(&fixture.config).to_owned(),
        }
    }

    /// `<first> <second> --config-dir <config>`, as `install` writes it.
    fn args(&self, first: &str, second: &str) -> Vec<String> {
        [first, second, "--config-dir", &self.config]
            .map(str::to_owned)
            .to_vec()
    }

    /// Writes a handler and a server entry with the extra `args` JSON
    /// (`""`, or `,"--config-dir","<dir>"`), each with
    /// `env.MEMPROVE_CONFIG_DIR` of the fixture when `env`.
    fn write(&self, extra: &str, env: bool) {
        let (executable, config) = (&self.executable, &self.config);
        let env = if env {
            format!(r#","env":{{"MEMPROVE_CONFIG_DIR":"{config}"}}"#)
        } else {
            String::new()
        };
        fs::write(
            &self.settings_path,
            format!(
                r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{executable}","args":["hook","session-start"{extra}],"timeout":10}}]}}]}}{env}}}"#
            ),
        )
        .unwrap();
        fs::write(
            &self.mcp_path,
            format!(
                r#"{{"mcpServers":{{"memprove":{{"type":"stdio","command":"{executable}","args":["mcp","serve"{extra}]{env}}}}}}}"#
            ),
        )
        .unwrap();
    }

    fn run(&self, words: &[&str]) -> Output {
        run_in(self.fixture, &self.fixture.config, words, "")
    }

    /// The status and the text of `doctor`'s line `check`.
    fn doctor_line(&self, check: &str) -> (String, String) {
        text(&self.run(&["doctor"]).stdout)
            .lines()
            .find_map(|line| {
                let (status, rest) = line.split_once(' ')?;
                let (name, detail) = rest.split_once(": ")?;
                (name == check).then(|| (status.to_owned(), detail.trim_start().to_owned()))
            })
            .unwrap()
    }
}

/// Entries an earlier install wrote (env only, no argument) are rewritten
/// with the argument, `env` kept; a second install writes nothing.
fn install_rewrites_old_entries_once(entries: &Entries<'_>) {
    entries.write("", true);
    let installed = entries.run(&["hook", "install"]);
    assert_eq!(installed.status.code(), Some(0));
    let settings = read(&entries.settings_path);
    assert_eq!(
        handler_args(&settings),
        [entries.args("hook", "session-start")]
    );
    assert_eq!(
        settings
            .get("env")
            .and_then(|env| env.get("MEMPROVE_CONFIG_DIR"))
            .and_then(JsonValue::as_str),
        Some(entries.config.as_str())
    );
    let mcp_json = read(&entries.mcp_path);
    assert_eq!(server_args(&mcp_json), entries.args("mcp", "serve"));
    assert!(
        mcp_json
            .get("mcpServers")
            .and_then(|servers| servers.get("memprove"))
            .and_then(|server| server.get("env"))
            .and_then(|env| env.get("MEMPROVE_CONFIG_DIR"))
            .is_some()
    );
    let files = || {
        (
            fs::read(&entries.settings_path).unwrap(),
            fs::read(&entries.mcp_path).unwrap(),
        )
    };
    let bytes = files();
    let again = entries.run(&["hook", "install"]);
    assert_eq!(text(&again.stderr).matches("not written").count(), 2);
    assert_eq!(files(), bytes);
}

/// `mcp print-config` prints the same arguments, and the command line.
fn print_config_gives_the_argument(entries: &Entries<'_>) {
    let printed = entries.run(&["mcp", "print-config"]);
    let stdout = text(&printed.stdout);
    let parts = stdout.split("==> ").skip(1).collect::<Vec<_>>();
    let value = |part: &str| parse_strict(part.split_once('\n').unwrap().1.trim()).unwrap();
    assert_eq!(
        handler_args(&value(parts[0])),
        [entries.args("hook", "session-start")]
    );
    assert_eq!(server_args(&value(parts[1])), entries.args("mcp", "serve"));
    assert!(
        text(&printed.stderr).contains(&format!("mcp serve --config-dir {}", entries.config)),
        "{}",
        text(&printed.stderr)
    );
}

/// `doctor`'s `hook` and `mcp-entry` lines: PASS when the argument is its
/// directory, WARN with neither argument nor `env`, FAIL when the argument
/// names another directory while the `env` is doctor's.
fn doctor_checks_the_argument(entries: &Entries<'_>) {
    for check in ["hook", "mcp-entry"] {
        let (status, detail) = entries.doctor_line(check);
        assert_eq!(status, "PASS", "{check}: {detail}");
        assert!(detail.contains("--config-dir"), "{check}: {detail}");
    }
    entries.write("", false);
    for check in ["hook", "mcp-entry"] {
        assert_eq!(entries.doctor_line(check).0, "WARN", "{check}");
    }
    let elsewhere = path_text(&entries.fixture.root.join("elsewhere")).to_owned();
    entries.write(&format!(r#","--config-dir","{elsewhere}""#), true);
    for check in ["hook", "mcp-entry"] {
        let (status, detail) = entries.doctor_line(check);
        assert_eq!(status, "FAIL", "{check}: {detail}");
        assert!(detail.contains(&elsewhere), "{check}: {detail}");
    }
}

#[test]
fn u7_p2_install_writes_the_argument_rewrites_old_entries_and_doctor_checks_it() {
    let fixture = Fixture::new();
    let entries = Entries::of(&fixture);
    install_rewrites_old_entries_once(&entries);
    print_config_gives_the_argument(&entries);
    doctor_checks_the_argument(&entries);
    fixture.stop();
}
