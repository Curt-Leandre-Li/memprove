//! P4-E (ADR 0014 E4, Owner condition p): `memprove hook print-config`,
//! `hook install`, and `hook uninstall`, run as the real binary in temporary
//! directories only. `install` writes only `<project>/.claude/
//! settings.local.json` (U5 Z-F round 1, S1: and the server's entry into
//! `<project>/.mcp.json`), adds its own item once, keeps everything else,
//! and refuses a file that is not valid JSON without changing it;
//! `uninstall` takes out only its own item.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{Value, json};

mod p4b_support;

use p4b_support::cli_binary;

fn cli(directory: &Path, words: &[&str]) -> Output {
    let mut command = Command::new(cli_binary());
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("MEMPROVE_") || name == "CLAUDE_ENV_FILE" {
            command.env_remove(&*name);
        }
    }
    let output = command
        .current_dir(directory)
        .env("HOME", directory)
        .env("XDG_CONFIG_HOME", directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(words)
        .output()
        .unwrap();
    println!(
        "$ memprove {}\n{}{}[exit {:?}]",
        words.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        output.status.code()
    );
    output
}

fn binary() -> PathBuf {
    fs::canonicalize(cli_binary()).unwrap()
}

/// The handler `print-config` and `install` write. U7 P2 (Owner O4,
/// amended by the Owner's decision of 2026-10-21): `install` passes its
/// configuration directory `config` as `--config-dir <config>` after `hook
/// session-start`; U8 K2 (U7 V-N2): `print-config` passes its own too.
fn own_handler(config: Option<&str>) -> Value {
    let mut args = vec!["hook", "session-start"];
    if let Some(config) = config {
        args.extend(["--config-dir", config]);
    }
    json!({
        "type": "command",
        "command": binary().to_str().unwrap(),
        "args": args,
        "timeout": 10,
    })
}

/// U5 P3 (Owner O5): the server entry `install` writes, with the
/// configuration directory of a command run in `directory` (whose
/// `XDG_CONFIG_HOME` [`cli`] sets to it). U7 P2 (Owner O4, amended
/// 2026-10-21): also as `--config-dir <it>` after `mcp serve`.
fn own_server(directory: &Path) -> Value {
    json!({
        "type": "stdio",
        "command": binary().to_str().unwrap(),
        "args": ["mcp", "serve", "--config-dir", config_of(directory)],
        "env": {"MEMPROVE_CONFIG_DIR": config_of(directory)},
    })
}

/// The configuration directory of a command run in `directory`.
fn config_of(directory: &Path) -> String {
    directory.join("memprove").to_str().unwrap().to_owned()
}

/// A Git repository in a temporary directory, with a subdirectory.
fn project() -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(base.path()).unwrap().join("project");
    fs::create_dir_all(root.join("src/deep")).unwrap();
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    (base, root)
}

fn settings(root: &Path) -> PathBuf {
    root.join(".claude/settings.local.json")
}

/// U5 Z-F round 1 (S1): the project's `.mcp.json`.
fn mcp_json(root: &Path) -> PathBuf {
    root.join(".mcp.json")
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn print_config_prints_the_settings_fragment_as_json() {
    let (_base, root) = project();
    let output = cli(&root, &["hook", "print-config"]);
    assert_eq!(output.status.code(), Some(0));
    let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
    // U9 K3 (U8 V-N9): the fragment also holds `env.MEMPROVE_CONFIG_DIR`,
    // the entry `install` writes beside the handler.
    assert_eq!(
        printed,
        json!({
            "hooks": {"SessionStart": [{"hooks": [own_handler(Some(&config_of(&root)))]}]},
            "env": {"MEMPROVE_CONFIG_DIR": config_of(&root)},
        })
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(".claude/settings.local.json"), "{stderr}");
    // P4-G (T20 N-5, accepted in P4-H1): one statement, which says what
    // `install` keeps and that it does not keep the file's bytes; it never
    // claims that nothing else changes.
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains(
            "keeps every other value, the spelling of numbers, and the order of members, but \
             not the file's bytes: it rewrites the file indented by two spaces"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("changes nothing else"), "{stderr}");
    // Nothing was written.
    assert!(!root.join(".claude").exists());
}

#[test]
fn install_adds_one_item_and_keeps_the_rest() {
    let (_base, root) = project();
    step_install_into_missing_file(&root);
    let original = json!({
        "permissions": {"allow": ["Bash(ls:*)"], "deny": []},
        "env": {"NAME": "value", "N": 1.50},
        "hooks": {
            "SessionStart": [
                {"matcher": "startup", "hooks": [{"type": "command", "command": "/usr/bin/true"}]}
            ],
            "Stop": [{"hooks": [{"type": "command", "command": "echo stop"}]}]
        },
        "model": "unchanged"
    });
    fs::write(
        settings(&root),
        serde_json::to_string_pretty(&original).unwrap(),
    )
    .unwrap();
    // From a subdirectory: the file is the Workspace root's.
    let output = cli(&root.join("src/deep"), &["hook", "install"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(!root.join("src/deep/.claude").exists());
    let installed = read(&settings(&root));
    let mut expected = original.clone();
    let deep = root.join("src/deep");
    expected["hooks"]["SessionStart"]
        .as_array_mut()
        .unwrap()
        .push(json!({"hooks": [own_handler(Some(&config_of(&deep)))]}));
    // U5 P3: and env.MEMPROVE_CONFIG_DIR; the permissions are left as
    // they are. U5 Z-F round 1 (S1): mcpServers.memprove goes into
    // .mcp.json (before, into the settings file).
    expected["env"]["MEMPROVE_CONFIG_DIR"] = json!(config_of(&deep));
    assert_eq!(installed, expected);
    assert_eq!(
        read(&mcp_json(&root)),
        json!({"mcpServers": {"memprove": own_server(&deep)}})
    );
    assert!(!root.join("src/deep/.mcp.json").exists());
    // Installing again changes nothing, not even a byte.
    let bytes = (
        fs::read(settings(&root)).unwrap(),
        fs::read(mcp_json(&root)).unwrap(),
    );
    let output = cli(&deep, &["hook", "install"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(fs::read(settings(&root)).unwrap(), bytes.0);
    assert_eq!(fs::read(mcp_json(&root)).unwrap(), bytes.1);
    // Uninstall takes out only its own item.
    let output = cli(&root, &["hook", "uninstall"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(read(&settings(&root)), original);
    assert_eq!(read(&mcp_json(&root)), json!({}));
    let bytes = fs::read(settings(&root)).unwrap();
    let output = cli(&root, &["hook", "uninstall"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(fs::read(settings(&root)).unwrap(), bytes);
}

fn step_install_into_missing_file(root: &Path) {
    let output = cli(root, &["hook", "install"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        read(&settings(root)),
        json!({
            "hooks": {"SessionStart": [{"hooks": [own_handler(Some(&config_of(root)))]}]},
            "env": {"MEMPROVE_CONFIG_DIR": config_of(root)},
        })
    );
    // U5 Z-F round 1 (S1): the server's entry is in .mcp.json.
    assert_eq!(
        read(&mcp_json(root)),
        json!({"mcpServers": {"memprove": own_server(root)}})
    );
    let output = cli(root, &["hook", "install"]);
    assert_eq!(output.status.code(), Some(0));
    let handlers = read(&settings(root))["hooks"]["SessionStart"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(handlers, 1);
    let output = cli(root, &["hook", "uninstall"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(read(&settings(root)), json!({}));
    assert_eq!(read(&mcp_json(root)), json!({}));
}

#[test]
fn a_file_that_is_not_valid_json_is_refused_and_left_alone() {
    let (_base, root) = project();
    fs::create_dir_all(root.join(".claude")).unwrap();
    for broken in [
        "{\"hooks\": {",
        "{\"a\": 1, \"a\": 2}",
        "[]",
        "{\"hooks\": []}",
        "{\"hooks\": {\"SessionStart\": {}}}",
        "",
    ] {
        fs::write(settings(&root), broken).unwrap();
        for command in ["install", "uninstall"] {
            let output = cli(&root, &["hook", command]);
            assert_eq!(output.status.code(), Some(1), "{broken:?} {command}");
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains("settings.local.json"), "{stderr}");
            assert_eq!(fs::read_to_string(settings(&root)).unwrap(), broken);
            // U5 Z-F round 1 (S1): nor is .mcp.json written.
            assert!(!mcp_json(&root).exists(), "{broken:?} {command}");
        }
    }
}

#[test]
fn the_hook_commands_have_help() {
    let (_base, root) = project();
    for name in ["session-start", "print-config", "install", "uninstall"] {
        let output = cli(&root, &["hook", name, "--help"]);
        assert_eq!(output.status.code(), Some(0));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.starts_with(&format!("memprove hook {name}:")),
            "{text}"
        );
    }
    // P4-G (T20 N-5): neither `install --help`, `uninstall --help`, nor the
    // general usage claims that the rest of the file stays byte for byte.
    let flat = |words: &[&str]| {
        let output = cli(&root, words);
        String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let install = flat(&["hook", "install", "--help"]);
    assert!(
        install.contains(
            "keeps every other value, the spelling of numbers, and the order of members, but not \
             the file's bytes: it rewrites the file indented by two spaces"
        ),
        "{install}"
    );
    let uninstall = flat(&["hook", "uninstall", "--help"]);
    assert!(
        uninstall.contains("every other value stays, but not the file's bytes"),
        "{uninstall}"
    );
    let general = flat(&["--help"]);
    assert!(
        general.contains(
            "hook uninstall removes that entry again; every other value stays, not the file's \
             bytes"
        ),
        "{general}"
    );
    for text in [&install, &uninstall, &general] {
        for claim in [
            "changes nothing else",
            "nothing else in the file changes",
            "keeps everything else",
            "everything else stays",
        ] {
            assert!(!text.contains(claim), "{claim:?} in {text}");
        }
    }
    assert!(!root.join(".claude").exists());
}
