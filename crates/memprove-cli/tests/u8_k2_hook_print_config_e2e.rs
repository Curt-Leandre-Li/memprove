//! U8 K2 (U7 V-N2; ADR 0014 "Amendment (U8, S-K)"), through the real
//! `memprove`: `hook print-config` prints the `SessionStart` handler with
//! `--config-dir <the configuration directory of that command>` after `hook
//! session-start`, as `hook install` writes it and `mcp print-config`
//! prints it, so a pasted fragment is not warned about by `doctor`. With no
//! configuration directory to name it fails as `mcp print-config` does.

#[path = "u1e_support/mod.rs"]
mod support;

use std::{fs, path::Path, process::Output};

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use support::{cli_binary, isolated, repository, short_tempdir, text};

/// `memprove <words>` in `repo`; `config` is its `MEMPROVE_CONFIG_DIR`,
/// and with `None` it has no `HOME` and no configuration directory either.
fn run(repo: &Path, config: Option<&Path>, words: &[&str]) -> Output {
    let mut command = isolated(cli_binary());
    command.current_dir(repo).env_remove("HOME").args(words);
    if let Some(config) = config {
        command.env("MEMPROVE_CONFIG_DIR", config);
    }
    let output = command.output().unwrap();
    println!(
        "$ memprove {}\n{}{}[exit {:?}]",
        words.join(" "),
        text(&output.stdout),
        text(&output.stderr),
        output.status.code()
    );
    output
}

/// The `args` of the first `SessionStart` handler of a settings value.
fn handler_args(settings: &JsonValue) -> Vec<String> {
    settings
        .get("hooks")
        .and_then(|hooks| hooks.get("SessionStart"))
        .and_then(JsonValue::as_array)
        .and_then(|groups| groups.first())
        .and_then(|group| group.get("hooks"))
        .and_then(JsonValue::as_array)
        .and_then(|handlers| handlers.first())
        .and_then(|handler| handler.get("args"))
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no handler args in {settings:?}"))
        .iter()
        .map(|item| item.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn u8_k2_the_three_ways_to_get_the_handler_give_the_same_args() {
    let (_guard, root) = short_tempdir();
    let (repo, config) = (root.join("r"), root.join("cfg"));
    repository(&repo);
    fs::create_dir(&config).unwrap();
    let expected = [
        "hook",
        "session-start",
        "--config-dir",
        config.to_str().unwrap(),
    ]
    .map(str::to_owned)
    .to_vec();

    let printed = run(&repo, Some(&config), &["hook", "print-config"]);
    assert_eq!(printed.status.code(), Some(0));
    let hook_print = parse_strict(&text(&printed.stdout)).unwrap();
    assert_eq!(handler_args(&hook_print), expected);

    let printed = run(&repo, Some(&config), &["mcp", "print-config"]);
    assert_eq!(printed.status.code(), Some(0));
    let stdout = text(&printed.stdout);
    let first = stdout.split("==> ").nth(1).unwrap();
    let mcp_print = parse_strict(first.split_once('\n').unwrap().1.trim()).unwrap();
    assert_eq!(handler_args(&mcp_print), expected);

    let installed = run(&repo, Some(&config), &["hook", "install"]);
    assert_eq!(installed.status.code(), Some(0));
    let settings =
        parse_strict(&fs::read_to_string(repo.join(".claude/settings.local.json")).unwrap())
            .unwrap();
    assert_eq!(handler_args(&settings), expected);

    // The fragment of `hook print-config` is the `hooks` member of the
    // other two: the same handler, command and timeout included.
    assert_eq!(hook_print.get("hooks"), mcp_print.get("hooks"));
    assert_eq!(hook_print.get("hooks"), settings.get("hooks"));
}

#[test]
fn u8_k2_print_config_without_a_configuration_directory_fails_like_mcp_print_config() {
    let (_guard, root) = short_tempdir();
    let repo = root.join("r");
    repository(&repo);
    let hook = run(&repo, None, &["hook", "print-config"]);
    let mcp = run(&repo, None, &["mcp", "print-config"]);
    assert_eq!(hook.status.code(), Some(1));
    assert_eq!(mcp.status.code(), Some(1));
    assert!(hook.stdout.is_empty());
    assert_eq!(hook.stderr, mcp.stderr, "{}", text(&hook.stderr));
    assert!(
        text(&hook.stderr).contains("no configuration directory"),
        "{}",
        text(&hook.stderr)
    );
}
