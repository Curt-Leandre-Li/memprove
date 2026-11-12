//! U9 K3 (U8 V-N9, V-N10; ADR 0014 and ADR 0015, "Amendment (U9, S-K)"),
//! through the real `memprove`:
//!
//! 1. `hook print-config` prints `env.MEMPROVE_CONFIG_DIR` beside the
//!    handler, the two entries `hook install` writes into
//!    `.claude/settings.local.json`: on a fresh repository the file `install`
//!    writes is the printed fragment, byte for byte.
//! 2. `mcp serve --config-dir=<DIR>` and `hook session-start
//!    --config-dir=<DIR>` (one argument) are not accepted, and say so, in
//!    words of the same kind as the `--config-dir` that has no value, instead
//!    of the general "unknown option" or "takes no argument"; exit codes are
//!    those of the other refusals (the hook 0, `mcp serve` 2).

#[path = "u1e_support/mod.rs"]
mod support;

use std::{
    fs,
    io::Write as _,
    path::Path,
    process::{Output, Stdio},
};

use memprove_protocol::strict_json::parse_strict;
use support::{cli_binary, isolated, repository, short_tempdir, text};

fn run(repo: &Path, config: Option<&Path>, words: &[&str], input: &str) -> Output {
    let mut command = isolated(cli_binary());
    command
        .current_dir(repo)
        .env_remove("HOME")
        .args(words)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(config) = config {
        command.env("MEMPROVE_CONFIG_DIR", config);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    println!(
        "$ memprove {words:?}\n{}{}[exit {:?}]",
        text(&output.stdout),
        text(&output.stderr),
        output.status.code()
    );
    output
}

#[test]
fn u9_k3_print_config_is_what_install_writes_byte_for_byte() {
    let (_guard, root) = short_tempdir();
    let (repo, config) = (root.join("r"), root.join("cfg"));
    repository(&repo);
    fs::create_dir(&config).unwrap();

    let printed = run(&repo, Some(&config), &["hook", "print-config"], "");
    assert_eq!(printed.status.code(), Some(0));
    let fragment = text(&printed.stdout);
    let value = parse_strict(&fragment).unwrap();
    let env = value.get("env").expect("no env in the fragment");
    assert_eq!(
        env.get("MEMPROVE_CONFIG_DIR").and_then(|dir| dir.as_str()),
        config.to_str(),
        "{fragment}"
    );

    let installed = run(&repo, Some(&config), &["hook", "install"], "");
    assert_eq!(installed.status.code(), Some(0));
    let written = fs::read_to_string(repo.join(".claude/settings.local.json")).unwrap();
    // A fresh settings file holds exactly the two entries, hooks first.
    assert_eq!(written, fragment, "the file `hook install` wrote differs");

    // The same entry `mcp print-config` prints for the settings file.
    let mcp = run(&repo, Some(&config), &["mcp", "print-config"], "");
    let stdout = text(&mcp.stdout);
    let settings = stdout
        .split("==> ")
        .nth(1)
        .unwrap()
        .split_once('\n')
        .unwrap()
        .1;
    let mcp_env = parse_strict(settings.trim()).unwrap();
    assert_eq!(mcp_env.get("env"), value.get("env"));
}

const NOT_INJECTED: &str = "no Context Pack was injected into this session";

#[test]
fn u9_k3_the_equals_form_of_config_dir_is_refused_with_its_own_words() {
    let (_guard, root) = short_tempdir();
    let repo = root.join("r");
    repository(&repo);
    let input =
        r#"{"hook_event_name":"SessionStart","session_id":"k3","source":"startup","cwd":"/tmp"}"#;

    let hook = run(
        &repo,
        None,
        &["hook", "session-start", "--config-dir=/c"],
        input,
    );
    assert_eq!(hook.status.code(), Some(0));
    assert_eq!(
        text(&hook.stdout),
        format!(
            "memprove: hook session-start takes --config-dir <DIR> as two arguments, not the \
             --config-dir=<DIR> spelling; {NOT_INJECTED}.\n"
        )
    );
    assert!(hook.stderr.is_empty());
    // The two-argument form is still the accepted one.
    let accepted = run(
        &repo,
        None,
        &["hook", "session-start", "--config-dir", "/c"],
        input,
    );
    assert_eq!(accepted.status.code(), Some(0));
    assert!(!text(&accepted.stdout).contains("spelling"));

    let serve = run(&repo, None, &["mcp", "serve", "--config-dir=/c"], "");
    assert_eq!(serve.status.code(), Some(2));
    let stderr = text(&serve.stderr);
    assert!(
        stderr.starts_with(
            "memprove: error: --config-dir=<DIR> is not accepted; write --config-dir <DIR> as \
             two arguments\n"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("unknown option"), "{stderr}");
    assert!(
        stderr.contains("run `memprove mcp serve --help`"),
        "{stderr}"
    );
    assert!(serve.stdout.is_empty());
}
