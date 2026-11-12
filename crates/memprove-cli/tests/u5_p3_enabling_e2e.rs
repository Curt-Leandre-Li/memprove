//! U5 P3 (D1 F1, F5, Owner O5; ADR 0014, "Amendment (U5, Z-P)"), revised
//! in U5 Z-F round 1 (S1, the Owner's O5 revision; ADR 0014, "Amendment
//! (U5, Z-F round 1, 2026-10-19)"): the enabling configuration is a
//! `.claude/settings.local.json` part and a `.mcp.json` part, and the hook
//! and `doctor` say which configuration directory they use.
//!
//! - `mcp print-config` prints both parts, each under a line `==> <file>
//!   <==`: `hooks.SessionStart` (the exec form), `env.MEMPROVE_CONFIG_DIR`,
//!   and the suggested `permissions.allow` (`mcp__memprove`,
//!   `Bash(memprove run:*)`) for the settings; `mcpServers.memprove`
//!   with the same `env` for `.mcp.json`. It writes nothing.
//! - `hook install` writes the hook and `env` into the settings and the
//!   server into `.mcp.json`; `--allow-tools` also writes the rules; `hook
//!   uninstall` takes back what it wrote and leaves the rules.
//! - With no context here, the hook's statement names the configuration
//!   directory it used and where that came from.
//! - `doctor`'s `hook` line compares the settings' `env.MEMPROVE_CONFIG_DIR`
//!   with the configuration directory it uses (missing WARN, different
//!   FAIL), and its `mcp-entry` line checks `mcpServers.memprove` of
//!   `.mcp.json` (an entry in a settings file counts for nothing).
//!
//! Everything runs in a temporary HOME, configuration directory, and
//! repository; no daemon is started.

mod u1e_support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use u1e_support::{cli_binary, isolated, repository, short_tempdir, text};

struct Place {
    _guard: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
    repo: PathBuf,
}

impl Place {
    fn new() -> Self {
        let (guard, root) = short_tempdir();
        let home = root.join("h");
        let config = root.join("cfg");
        let repo = root.join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        repository(&repo);
        Self {
            _guard: guard,
            home,
            config,
            repo,
        }
    }

    /// `memprove <words>` in the repository, HOME the temporary one, and
    /// `MEMPROVE_CONFIG_DIR` set to `config` when given.
    fn command(&self, config: Option<&Path>, words: &[&str]) -> Command {
        let mut command = isolated(cli_binary());
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .args(words);
        if let Some(config) = config {
            command.env("MEMPROVE_CONFIG_DIR", config);
        }
        command
    }

    fn run(&self, config: Option<&Path>, words: &[&str]) -> Output {
        let output = self.command(config, words).output().unwrap();
        println!(
            "$ memprove {}\n{}{}[exit {:?}]",
            words.join(" "),
            text(&output.stdout),
            text(&output.stderr),
            output.status.code()
        );
        output
    }

    fn settings_path(&self) -> PathBuf {
        self.repo.join(".claude/settings.local.json")
    }

    fn settings(&self) -> JsonValue {
        parse_strict(&fs::read_to_string(self.settings_path()).unwrap()).unwrap()
    }

    /// `doctor` in the repository: the exit code and each line's status by
    /// check, with its text.
    fn doctor(&self, config: &Path) -> (Option<i32>, Vec<(String, String, String)>) {
        let output = self.run(Some(config), &["doctor"]);
        let rows = text(&output.stdout)
            .lines()
            .filter_map(|line| {
                let (status, rest) = line.split_once(' ')?;
                let (check, detail) = rest.split_once(": ")?;
                Some((status.to_owned(), check.to_owned(), detail.to_owned()))
            })
            .collect();
        (output.status.code(), rows)
    }
}

fn path_text(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn member<'a>(value: &'a JsonValue, path: &[&str]) -> &'a JsonValue {
    path.iter().fold(value, |value, name| {
        value
            .get(name)
            .unwrap_or_else(|| panic!("no member {name} in {path:?}"))
    })
}

fn strings(value: &JsonValue) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_owned())
        .collect()
}

/// The four parts of the fragment, checked against `executable` and
/// `config`; `rules` says whether the two rules must be in
/// `permissions.allow` (once each) or must not. U5 Z-F round 1 (S1):
/// `value` is the settings part and `mcp_json` the `.mcp.json` part, which
/// holds `mcpServers` (before, `value` held all four).
fn assert_fragment(
    value: &JsonValue,
    mcp_json: &JsonValue,
    executable: &str,
    config: &str,
    rules: bool,
) {
    let groups = member(value, &["hooks", "SessionStart"])
        .as_array()
        .unwrap();
    let handlers = groups
        .iter()
        .flat_map(|group| member(group, &["hooks"]).as_array().unwrap())
        .filter(|handler| handler.get("command").and_then(JsonValue::as_str) == Some(executable))
        .collect::<Vec<_>>();
    assert_eq!(handlers.len(), 1, "one handler of this executable");
    assert_eq!(member(handlers[0], &["type"]).as_str(), Some("command"));
    // U7 P2 (Owner O4): with the directory as an argument.
    assert_eq!(
        strings(member(handlers[0], &["args"])),
        ["hook", "session-start", "--config-dir", config]
    );
    assert_eq!(
        member(value, &["env", "MEMPROVE_CONFIG_DIR"]).as_str(),
        Some(config)
    );
    assert!(
        value.get("mcpServers").is_none(),
        "mcpServers in the settings"
    );
    let server = member(mcp_json, &["mcpServers", "memprove"]);
    assert_eq!(member(server, &["type"]).as_str(), Some("stdio"));
    assert_eq!(member(server, &["command"]).as_str(), Some(executable));
    // U7 P2 (Owner O4): with the directory as an argument.
    assert_eq!(
        strings(member(server, &["args"])),
        ["mcp", "serve", "--config-dir", config]
    );
    assert_eq!(
        member(server, &["env", "MEMPROVE_CONFIG_DIR"]).as_str(),
        Some(config)
    );
    let allow = value
        .get("permissions")
        .and_then(|permissions| permissions.get("allow"));
    if rules {
        let allow = strings(allow.unwrap());
        for rule in ["mcp__memprove", "Bash(memprove run:*)"] {
            assert_eq!(
                allow.iter().filter(|seen| *seen == rule).count(),
                1,
                "{rule} in {allow:?}"
            );
        }
    } else {
        let allow = allow.map(strings).unwrap_or_default();
        assert!(
            !allow
                .iter()
                .any(|rule| rule == "mcp__memprove" || rule == "Bash(memprove run:*)"),
            "the rules were written: {allow:?}"
        );
    }
}

fn executable() -> String {
    path_text(&fs::canonicalize(cli_binary()).unwrap()).to_owned()
}

#[test]
fn u5_p3_print_config_gives_the_whole_settings_fragment_and_writes_nothing() {
    let place = Place::new();
    let printed = place.run(Some(&place.config), &["mcp", "print-config"]);
    assert_eq!(printed.status.code(), Some(0));
    // U5 Z-F round 1 (S1): two parts (before, one JSON object with the
    // members hooks, env, mcpServers, permissions).
    let printed_parts = parts(&text(&printed.stdout));
    let [(_, fragment), (_, mcp_json)] = &printed_parts[..] else {
        panic!("not two parts: {printed_parts:?}")
    };
    assert_fragment(
        fragment,
        mcp_json,
        &executable(),
        path_text(&place.config),
        true,
    );
    assert_eq!(names(fragment), ["hooks", "env", "permissions"]);
    assert_eq!(names(mcp_json), ["mcpServers"]);
    let stderr = text(&printed.stderr);
    assert!(stderr.contains(".claude/settings.local.json"), "{stderr}");
    assert!(stderr.contains("hook install"), "{stderr}");
    assert!(!place.repo.join(".claude").exists(), "it wrote a file");
    assert!(!place.repo.join(".mcp.json").exists(), "it wrote a file");

    // Without MEMPROVE_CONFIG_DIR, the directory the CLI itself uses.
    let printed = place.run(None, &["mcp", "print-config"]);
    let printed_parts = parts(&text(&printed.stdout));
    let default = place.home.join(".config/memprove");
    assert_fragment(
        &printed_parts[0].1,
        &printed_parts[1].1,
        &executable(),
        path_text(&default),
        true,
    );
}

#[test]
fn u5_p3_hook_install_writes_three_members_and_the_rules_only_when_asked() {
    let place = Place::new();
    fs::create_dir_all(place.repo.join(".claude")).unwrap();
    fs::write(
        place.settings_path(),
        r#"{"model": "x", "permissions": {"allow": ["Bash(ls:*)"]}, "env": {"OTHER": "1"}}"#,
    )
    .unwrap();
    let config = path_text(&place.config).to_owned();

    let installed = place.run(Some(&place.config), &["hook", "install"]);
    assert_eq!(installed.status.code(), Some(0));
    let settings = place.settings();
    assert_fragment(&settings, &place.mcp_json(), &executable(), &config, false);
    assert_eq!(
        strings(member(&settings, &["permissions", "allow"])),
        ["Bash(ls:*)"],
        "the rules are only printed"
    );
    assert_eq!(member(&settings, &["model"]).as_str(), Some("x"));
    assert_eq!(member(&settings, &["env", "OTHER"]).as_str(), Some("1"));
    let stderr = text(&installed.stderr);
    assert!(stderr.contains("--allow-tools"), "{stderr}");
    assert!(stderr.contains("mcp__memprove"), "{stderr}");
    let again = place.run(Some(&place.config), &["hook", "install"]);
    assert!(text(&again.stderr).contains("not written"));

    let allowed = place.run(Some(&place.config), &["hook", "install", "--allow-tools"]);
    assert_eq!(allowed.status.code(), Some(0));
    let settings = place.settings();
    assert_fragment(&settings, &place.mcp_json(), &executable(), &config, true);
    assert_eq!(
        strings(member(&settings, &["permissions", "allow"])),
        [
            "Bash(ls:*)".to_owned(),
            "mcp__memprove".to_owned(),
            "Bash(memprove run:*)".to_owned(),
            format!("Bash({} run:*)", executable())
        ]
    );
    let again = place.run(Some(&place.config), &["hook", "install", "--allow-tools"]);
    assert!(text(&again.stderr).contains("not written"));

    let removed = place.run(Some(&place.config), &["hook", "uninstall"]);
    assert_eq!(removed.status.code(), Some(0));
    let settings = place.settings();
    assert!(settings.get("hooks").is_none() && settings.get("mcpServers").is_none());
    assert!(place.mcp_json().get("mcpServers").is_none());
    assert!(
        settings
            .get("env")
            .and_then(|env| env.get("MEMPROVE_CONFIG_DIR"))
            .is_none()
    );
    assert_eq!(member(&settings, &["env", "OTHER"]).as_str(), Some("1"));
    assert_eq!(
        strings(member(&settings, &["permissions", "allow"])),
        [
            "Bash(ls:*)".to_owned(),
            "mcp__memprove".to_owned(),
            "Bash(memprove run:*)".to_owned(),
            format!("Bash({} run:*)", executable())
        ],
        "uninstall leaves the rules, which are the operator's"
    );
}

#[test]
fn u5_p3_the_hook_names_its_configuration_directory_when_it_finds_no_context() {
    let place = Place::new();
    let input = format!(
        r#"{{"session_id":"u5-p3","cwd":"{}","hook_event_name":"SessionStart","source":"startup"}}"#,
        path_text(&place.repo)
    );
    let statement = |config: Option<&Path>| {
        let mut child = place
            .command(config, &["hook", "session-start"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(0));
        text(&output.stdout)
    };
    let said = statement(Some(&place.config));
    println!("{said}");
    assert!(
        said.starts_with("memprove: this directory has no memprove context"),
        "{said}"
    );
    assert!(
        said.contains(&format!(
            "the hook's configuration directory is {} (from MEMPROVE_CONFIG_DIR)",
            path_text(&place.config)
        )),
        "{said}"
    );
    assert!(
        said.trim_end()
            .ends_with("no Context Pack was injected into this session."),
        "{said}"
    );
    let said = statement(None);
    println!("{said}");
    assert!(
        said.contains(&format!(
            "the hook's configuration directory is {} (from HOME; MEMPROVE_CONFIG_DIR is not set)",
            path_text(&place.home.join(".config/memprove"))
        )),
        "{said}"
    );
}

#[test]
#[allow(clippy::too_many_lines)] // Each case of the two lines.
fn u5_p3_doctor_checks_the_settings_env_and_the_mcp_entry() {
    let place = Place::new();
    let line = |rows: &[(String, String, String)], check: &str| {
        rows.iter().find(|(_, seen, _)| seen == check).map_or_else(
            || panic!("no {check} line in {rows:?}"),
            |(status, _, detail)| (status.clone(), detail.clone()),
        )
    };
    let config = path_text(&place.config).to_owned();
    let other = place.home.join("elsewhere");
    fs::create_dir_all(place.repo.join(".claude")).unwrap();
    let write = |value: &str| fs::write(place.settings_path(), value).unwrap();
    let hook = |env: Option<&str>| {
        let env = env.map_or_else(String::new, |dir| {
            format!(r#""env": {{"MEMPROVE_CONFIG_DIR": "{dir}"}},"#)
        });
        format!(
            r#"{{{env} "hooks": {{"SessionStart": [{{"hooks": [{{"type": "command", "command": "{}", "args": ["hook", "session-start"]}}]}}]}}}}"#,
            executable()
        )
    };

    // The hook without env: WARN; no MCP entry: WARN.
    write(&hook(None));
    let (_, rows) = place.doctor(&place.config);
    let (status, detail) = line(&rows, "hook");
    assert_eq!(status, "WARN", "{detail}");
    assert!(detail.contains("env.MEMPROVE_CONFIG_DIR"), "{detail}");
    assert!(detail.contains(&config), "{detail}");
    assert_eq!(line(&rows, "mcp-entry").0, "WARN");

    // env names another directory: FAIL, and doctor exits 1.
    write(&hook(Some(path_text(&other))));
    let (code, rows) = place.doctor(&place.config);
    let (status, detail) = line(&rows, "hook");
    assert_eq!(status, "FAIL", "{detail}");
    assert!(
        detail.contains(path_text(&other)) && detail.contains(&config),
        "{detail}"
    );
    assert_eq!(code, Some(1));

    // The same directory: PASS.
    write(&hook(Some(&config)));
    let (_, rows) = place.doctor(&place.config);
    assert_eq!(line(&rows, "hook").0, "PASS", "{rows:?}");

    // hook install: the MCP entry with the same env: PASS.
    fs::remove_file(place.settings_path()).unwrap();
    let installed = place.run(Some(&place.config), &["hook", "install"]);
    assert_eq!(installed.status.code(), Some(0));
    let (_, rows) = place.doctor(&place.config);
    assert_eq!(line(&rows, "hook").0, "PASS", "{rows:?}");
    let (status, detail) = line(&rows, "mcp-entry");
    assert_eq!(status, "PASS", "{detail}");
    assert!(detail.contains(&config), "{detail}");

    // doctor with another configuration directory: both lines FAIL.
    fs::create_dir_all(&other).unwrap();
    let (code, rows) = place.doctor(&other);
    assert_eq!(line(&rows, "hook").0, "FAIL", "{rows:?}");
    assert_eq!(line(&rows, "mcp-entry").0, "FAIL", "{rows:?}");
    assert_eq!(code, Some(1));

    // An entry without env (as an older `.mcp.json` has it): WARN. (U5 Z-F
    // round 1: install wrote the entry into .mcp.json, not into the
    // settings, which this case first renamed away.)
    fs::write(
        place.repo.join(".mcp.json"),
        format!(
            r#"{{"mcpServers": {{"memprove": {{"type": "stdio", "command": "{}", "args": ["mcp", "serve"]}}}}}}"#,
            executable()
        ),
    )
    .unwrap();
    let (_, rows) = place.doctor(&place.config);
    let (status, detail) = line(&rows, "mcp-entry");
    assert_eq!(status, "WARN", "{detail}");
    assert!(detail.contains(".mcp.json"), "{detail}");
}

// ---- U5 Z-F round 1, S1 (Owner's O5 revision, 2026-10-19) ----------------
//
// Claude Code does not read `mcpServers` from `.claude/settings.local.json`;
// the server's entry goes into `<root>/.mcp.json`. `mcp print-config` gives
// two parts, each one JSON value under a line `==> <file> <==`.

/// The parts of `mcp print-config`'s standard output: each starts with a
/// line `==> <file> <==` and runs up to the next such line; each part's text
/// is one JSON value.
fn parts(stdout: &str) -> Vec<(String, JsonValue)> {
    let mut parts: Vec<(String, String)> = Vec::new();
    for line in stdout.lines() {
        if let Some(name) = line
            .strip_prefix("==> ")
            .and_then(|rest| rest.strip_suffix(" <=="))
        {
            parts.push((name.to_owned(), String::new()));
        } else {
            let (_, part) = parts
                .last_mut()
                .unwrap_or_else(|| panic!("a line before the first part: {line:?}"));
            part.push_str(line);
            part.push('\n');
        }
    }
    parts
        .into_iter()
        .map(|(name, part)| {
            let value = parse_strict(part.trim())
                .unwrap_or_else(|error| panic!("{name} is not one JSON value ({error}): {part}"));
            (name, value)
        })
        .collect()
}

fn names(value: &JsonValue) -> Vec<&str> {
    let JsonValue::Object(members) = value else {
        panic!("not an object: {value:?}")
    };
    members.iter().map(|(name, _)| name.as_str()).collect()
}

/// The server entry `install` and `print-config` give.
fn assert_server(value: &JsonValue, executable: &str, config: &str) {
    let server = member(value, &["mcpServers", "memprove"]);
    assert_eq!(names(server), ["type", "command", "args", "env"]);
    assert_eq!(member(server, &["type"]).as_str(), Some("stdio"));
    assert_eq!(member(server, &["command"]).as_str(), Some(executable));
    // U7 P2 (Owner O4): with the directory as an argument.
    assert_eq!(
        strings(member(server, &["args"])),
        ["mcp", "serve", "--config-dir", config]
    );
    assert_eq!(names(member(server, &["env"])), ["MEMPROVE_CONFIG_DIR"]);
    assert_eq!(
        member(server, &["env", "MEMPROVE_CONFIG_DIR"]).as_str(),
        Some(config)
    );
}

impl Place {
    fn mcp_json_path(&self) -> PathBuf {
        self.repo.join(".mcp.json")
    }

    fn mcp_json(&self) -> JsonValue {
        parse_strict(&fs::read_to_string(self.mcp_json_path()).unwrap()).unwrap()
    }
}

#[test]
fn u5_zf1_s1_print_config_gives_the_settings_part_and_the_mcp_json_part() {
    let place = Place::new();
    let config = path_text(&place.config).to_owned();
    let printed = place.run(Some(&place.config), &["mcp", "print-config"]);
    assert_eq!(printed.status.code(), Some(0));
    let stdout = text(&printed.stdout);
    let parts = parts(&stdout);
    assert_eq!(
        parts
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [".claude/settings.local.json", ".mcp.json"],
        "{stdout}"
    );
    let (_, settings) = &parts[0];
    assert_eq!(names(settings), ["hooks", "env", "permissions"]);
    assert_eq!(names(member(settings, &["env"])), ["MEMPROVE_CONFIG_DIR"]);
    assert_eq!(
        strings(member(settings, &["permissions", "allow"])),
        [
            "mcp__memprove".to_owned(),
            "Bash(memprove run:*)".to_owned(),
            format!("Bash({} run:*)", executable())
        ]
    );
    assert!(settings.get("mcpServers").is_none(), "{stdout}");
    let handlers = member(settings, &["hooks", "SessionStart"])
        .as_array()
        .unwrap();
    assert_eq!(handlers.len(), 1);
    let (_, mcp_json) = &parts[1];
    assert_eq!(names(mcp_json), ["mcpServers"]);
    assert_eq!(names(member(mcp_json, &["mcpServers"])), ["memprove"]);
    assert_server(mcp_json, &executable(), &config);
    // Each part is the indented text `hook install` writes, and the parts
    // are apart by one empty line.
    assert!(
        stdout.starts_with("==> .claude/settings.local.json <==\n{\n"),
        "{stdout}"
    );
    assert!(stdout.contains("\n}\n\n==> .mcp.json <==\n{\n"), "{stdout}");
    assert!(stdout.ends_with("\n}\n"), "{stdout}");

    let stderr = text(&printed.stderr);
    let settings_file = place.settings_path();
    let mcp_file = place.mcp_json_path();
    assert!(stderr.contains(path_text(&settings_file)), "{stderr}");
    assert!(stderr.contains(path_text(&mcp_file)), "{stderr}");
    assert!(
        stderr.contains("does not read mcpServers from a settings file"),
        "{stderr}"
    );
    assert!(!place.repo.join(".claude").exists(), "it wrote a file");
    assert!(!mcp_file.exists(), "it wrote a file");
}

#[test]
#[allow(clippy::too_many_lines)] // Install, again, --allow-tools, uninstall.
fn u5_zf1_s1_hook_install_puts_the_server_into_mcp_json() {
    let place = Place::new();
    let config = path_text(&place.config).to_owned();
    fs::create_dir_all(place.repo.join(".claude")).unwrap();
    // The layout an earlier build wrote: the server in the settings file.
    let stale = format!(
        r#"{{"model": "x", "mcpServers": {{"memprove": {{"type": "stdio", "command": "{}", "args": ["mcp", "serve"]}}}}}}"#,
        executable()
    );
    fs::write(place.settings_path(), stale).unwrap();
    fs::write(
        place.mcp_json_path(),
        r#"{"mcpServers": {"other": {"type": "stdio", "command": "/bin/other"}}, "x": 1}"#,
    )
    .unwrap();

    let installed = place.run(Some(&place.config), &["hook", "install"]);
    assert_eq!(installed.status.code(), Some(0));
    let stderr = text(&installed.stderr);
    for file in [place.settings_path(), place.mcp_json_path()] {
        assert!(
            stderr.contains(&format!("memprove: {} written", path_text(&file))),
            "{stderr}"
        );
    }
    let settings = place.settings();
    assert_eq!(names(&settings), ["model", "hooks", "env"]);
    assert_eq!(
        member(&settings, &["env", "MEMPROVE_CONFIG_DIR"]).as_str(),
        Some(config.as_str())
    );
    let mcp_json = place.mcp_json();
    assert_eq!(names(&mcp_json), ["mcpServers", "x"]);
    assert_eq!(
        names(member(&mcp_json, &["mcpServers"])),
        ["other", "memprove"]
    );
    assert_server(&mcp_json, &executable(), &config);

    // Again: neither file is touched.
    let bytes = (
        fs::read(place.settings_path()).unwrap(),
        fs::read(place.mcp_json_path()).unwrap(),
    );
    let again = place.run(Some(&place.config), &["hook", "install"]);
    assert_eq!(again.status.code(), Some(0));
    assert_eq!(
        text(&again.stderr)
            .matches("already as asked; not written")
            .count(),
        2
    );
    assert_eq!(
        (
            fs::read(place.settings_path()).unwrap(),
            fs::read(place.mcp_json_path()).unwrap()
        ),
        bytes
    );

    // --allow-tools writes the rules into the settings file only.
    let allowed = place.run(Some(&place.config), &["hook", "install", "--allow-tools"]);
    assert_eq!(allowed.status.code(), Some(0));
    assert_eq!(
        strings(member(&place.settings(), &["permissions", "allow"])),
        [
            "mcp__memprove".to_owned(),
            "Bash(memprove run:*)".to_owned(),
            format!("Bash({} run:*)", executable())
        ]
    );
    assert_eq!(fs::read(place.mcp_json_path()).unwrap(), bytes.1);

    // Uninstall takes the server out of .mcp.json and keeps the rest.
    let removed = place.run(Some(&place.config), &["hook", "uninstall"]);
    assert_eq!(removed.status.code(), Some(0));
    let mcp_json = place.mcp_json();
    assert_eq!(names(&mcp_json), ["mcpServers", "x"]);
    assert_eq!(names(member(&mcp_json, &["mcpServers"])), ["other"]);
    let settings = place.settings();
    assert_eq!(names(&settings), ["model", "permissions"]);

    // No .mcp.json before: install makes one, uninstall leaves `{}` (as it
    // leaves a settings file it emptied).
    fs::remove_file(place.mcp_json_path()).unwrap();
    let installed = place.run(Some(&place.config), &["hook", "install"]);
    assert_eq!(installed.status.code(), Some(0));
    assert_eq!(names(&place.mcp_json()), ["mcpServers"]);
    assert_server(&place.mcp_json(), &executable(), &config);
    let removed = place.run(Some(&place.config), &["hook", "uninstall"]);
    assert_eq!(removed.status.code(), Some(0));
    assert_eq!(fs::read_to_string(place.mcp_json_path()).unwrap(), "{}\n");

    // A .mcp.json that is not valid JSON: refused, and neither file written.
    fs::remove_file(place.settings_path()).unwrap();
    fs::write(place.mcp_json_path(), "{\"mcpServers\": ").unwrap();
    let refused = place.run(Some(&place.config), &["hook", "install"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused.stderr).contains(".mcp.json"),
        "{}",
        text(&refused.stderr)
    );
    assert!(!place.settings_path().exists());
    assert_eq!(
        fs::read_to_string(place.mcp_json_path()).unwrap(),
        "{\"mcpServers\": "
    );
}

#[test]
fn u5_zf1_s1_doctor_takes_the_mcp_entry_from_mcp_json_only() {
    let place = Place::new();
    let config = path_text(&place.config).to_owned();
    let other = place.home.join("elsewhere");
    fs::create_dir_all(&other).unwrap();
    let line = |rows: &[(String, String, String)]| {
        rows.iter()
            .find(|(_, check, _)| check == "mcp-entry")
            .map_or_else(
                || panic!("no mcp-entry line in {rows:?}"),
                |(status, _, detail)| (status.clone(), detail.clone()),
            )
    };
    let server = |env: Option<&str>| {
        let env = env.map_or_else(String::new, |dir| {
            format!(r#", "env": {{"MEMPROVE_CONFIG_DIR": "{dir}"}}"#)
        });
        format!(
            r#"{{"mcpServers": {{"memprove": {{"type": "stdio", "command": "{}", "args": ["mcp", "serve"]{env}}}}}}}"#,
            executable()
        )
    };
    // The entry in the settings file only (as P3 wrote it): Claude Code does
    // not read it, so WARN, with a hint.
    fs::create_dir_all(place.repo.join(".claude")).unwrap();
    fs::write(place.settings_path(), server(Some(&config))).unwrap();
    let (_, rows) = place.doctor(&place.config);
    let (status, detail) = line(&rows);
    assert_eq!(status, "WARN", "{detail}");
    assert!(
        detail.contains(path_text(&place.mcp_json_path())),
        "{detail}"
    );
    assert!(
        detail.contains(&format!(
            "{} has an mcpServers.memprove, which Claude Code does not read",
            path_text(&place.settings_path())
        )),
        "{detail}"
    );

    // In .mcp.json with the same env: PASS (the hint stays).
    fs::write(place.mcp_json_path(), server(Some(&config))).unwrap();
    let (_, rows) = place.doctor(&place.config);
    let (status, detail) = line(&rows);
    assert_eq!(status, "PASS", "{detail}");
    assert!(
        detail
            .trim_start()
            .starts_with(path_text(&place.mcp_json_path())),
        "{detail}"
    );
    assert!(detail.contains("Claude Code does not read"), "{detail}");

    // Another directory: FAIL; no env: WARN.
    fs::remove_file(place.settings_path()).unwrap();
    fs::write(place.mcp_json_path(), server(Some(path_text(&other)))).unwrap();
    let (code, rows) = place.doctor(&place.config);
    assert_eq!(line(&rows).0, "FAIL", "{rows:?}");
    assert_eq!(code, Some(1));
    fs::write(place.mcp_json_path(), server(None)).unwrap();
    let (_, rows) = place.doctor(&place.config);
    let (status, detail) = line(&rows);
    assert_eq!(status, "WARN", "{detail}");
    assert!(!detail.contains("does not read"), "{detail}");
}

// ---- U20 T1 (U19-O2) ------------------------------------------------------

/// The settings file and `.mcp.json` as an installation of the previous name
/// left them: its handler (the program `old`, which the rename took away),
/// `env.CONTINUITY_CONFIG_DIR`, the rules of `--allow-tools`, and
/// `mcpServers.continuity`; beside them a handler, a variable, rules and a
/// server that are not its own.
fn write_previous_name_files(place: &Place, old: &str, config: &str) {
    fs::create_dir_all(place.repo.join(".claude")).unwrap();
    fs::write(
        place.settings_path(),
        format!(
            r#"{{"hooks": {{"SessionStart": [
  {{"hooks": [{{"type": "command", "command": "{old}", "args": ["hook", "session-start"], "timeout": 10}}]}},
  {{"matcher": "x", "hooks": [{{"type": "command", "command": "/bin/true"}}]}}]}},
 "env": {{"CONTINUITY_CONFIG_DIR": "{config}", "OTHER": "1"}},
 "permissions": {{"allow": ["Bash(ls:*)", "mcp__continuity", "Bash(continuity run:*)", "Bash({old} run:*)", "Bash(continuity:*)"]}}}}"#
        ),
    )
    .unwrap();
    fs::write(
        place.mcp_json_path(),
        format!(
            r#"{{"mcpServers": {{
  "continuity": {{"type": "stdio", "command": "{old}", "args": ["mcp", "serve", "--config-dir", "{config}"], "env": {{"CONTINUITY_CONFIG_DIR": "{config}"}}}},
  "other": {{"command": "/bin/cat"}}}}}}"#
        ),
    )
    .unwrap();
}

/// The program of every `SessionStart` handler of `settings`, in order.
fn handler_commands(settings: &JsonValue) -> Vec<String> {
    member(settings, &["hooks", "SessionStart"])
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| member(group, &["hooks"]).as_array().unwrap())
        .map(|handler| member(handler, &["command"]).as_str().unwrap().to_owned())
        .collect()
}

/// U20 T1 (U19-O2): in a repository an installation of the previous name
/// enabled, `hook install --allow-tools` replaces that installation's
/// handler, `env.CONTINUITY_CONFIG_DIR`, rules and `mcpServers.continuity`
/// with its own instead of adding its own beside them, so `doctor`'s `hook`
/// line no longer fails on a program that is gone; everything that is not
/// its own stays, and `hook uninstall` then leaves only that (and the rules,
/// which are the operator's). The settings file is shaped as the one this
/// repository held before the rename (U19 report, section 9).
#[test]
fn u20_t1_hook_install_replaces_the_entries_of_the_previous_name() {
    let place = Place::new();
    let config = path_text(&place.config).to_owned();
    let old = path_text(&place.home.join("trial/bin/continuity")).to_owned();
    write_previous_name_files(&place, &old, &config);
    let (_, rows) = place.doctor(&place.config);
    let hook = rows.iter().find(|(_, check, _)| check == "hook").unwrap();
    assert_eq!(hook.0, "FAIL", "{hook:?}");
    assert!(hook.2.contains(&old), "{hook:?}");

    let installed = place.run(Some(&place.config), &["hook", "install", "--allow-tools"]);
    assert_eq!(installed.status.code(), Some(0));
    let settings = place.settings();
    let mcp_json = place.mcp_json();
    assert_fragment(&settings, &mcp_json, &executable(), &config, true);
    assert_eq!(
        handler_commands(&settings),
        ["/bin/true".to_owned(), executable()]
    );
    assert_eq!(
        names(member(&settings, &["env"])),
        ["OTHER", "MEMPROVE_CONFIG_DIR"]
    );
    let rules = [
        "Bash(ls:*)".to_owned(),
        "Bash(continuity:*)".to_owned(),
        "mcp__memprove".to_owned(),
        "Bash(memprove run:*)".to_owned(),
        format!("Bash({} run:*)", executable()),
    ];
    assert_eq!(strings(member(&settings, &["permissions", "allow"])), rules);
    assert_eq!(
        names(member(&mcp_json, &["mcpServers"])),
        ["other", "memprove"]
    );
    assert_server(&mcp_json, &executable(), &config);
    let (_, rows) = place.doctor(&place.config);
    let hook = rows.iter().find(|(_, check, _)| check == "hook").unwrap();
    assert_eq!(hook.0, "PASS", "{hook:?}");
    let again = place.run(Some(&place.config), &["hook", "install", "--allow-tools"]);
    assert!(text(&again.stderr).contains("not written"));

    let removed = place.run(Some(&place.config), &["hook", "uninstall"]);
    assert_eq!(removed.status.code(), Some(0));
    let settings = place.settings();
    assert_eq!(handler_commands(&settings), ["/bin/true"]);
    assert_eq!(names(member(&settings, &["env"])), ["OTHER"]);
    assert_eq!(strings(member(&settings, &["permissions", "allow"])), rules);
    assert_eq!(names(member(&place.mcp_json(), &["mcpServers"])), ["other"]);

    // Without install first, uninstall removes the previous name's entries
    // too; the rules stay.
    write_previous_name_files(&place, &old, &config);
    let removed = place.run(Some(&place.config), &["hook", "uninstall"]);
    assert_eq!(removed.status.code(), Some(0));
    let settings = place.settings();
    assert_eq!(handler_commands(&settings), ["/bin/true"]);
    assert_eq!(names(member(&settings, &["env"])), ["OTHER"]);
    assert_eq!(names(member(&place.mcp_json(), &["mcpServers"])), ["other"]);
    assert_eq!(
        member(&settings, &["permissions", "allow"])
            .as_array()
            .unwrap()
            .len(),
        5
    );
}
