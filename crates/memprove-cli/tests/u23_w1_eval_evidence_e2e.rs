//! U23 W1, through the real binaries: `memprove run --test-format cargo --
//! memprove eval run ...` records an evaluation as test Evidence with one
//! case per trial, and a fact that cites it is verified.

mod u1e_support;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use memprove_cli::{eval_memory::notes_with, hook::Environment};
use memprove_protocol::strict_json::JsonValue;
use u1e_support::{
    Daemon, bootstrap, cli_binary, git, isolated, json, repository, short_tempdir, text,
};

/// `memprove <words>` in `repo` with only `HOME` and `PATH` set; its
/// standard output.
fn memprove(home: &Path, repo: &Path, words: &[&str]) -> String {
    let output = isolated(cli_binary())
        .current_dir(repo)
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .args(words)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "memprove {}\n{}{}",
        words.join(" "),
        text(&output.stdout),
        text(&output.stderr)
    );
    text(&output.stdout)
}

/// A default-prefix installation under `root/home` with a running daemon
/// and an agent token; returns the home and the daemon.
fn install(root: &Path) -> (PathBuf, Daemon) {
    let home = root.join("home");
    let prefix = home.join(".memprove");
    let (data, config) = (prefix.join("data"), prefix.join("config"));
    for path in [&home, &prefix, &data, &config] {
        fs::create_dir_all(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(config.join("data-dir"), format!("{}\n", data.display())).unwrap();
    let operator = prefix.join("operator-token");
    bootstrap(&data, &operator);
    let daemon = Daemon::start(&data, &root.join("daemon.log"));
    let created = json(
        &isolated(cli_binary())
            .arg("--data-dir")
            .arg(&data)
            .arg("--token-file")
            .arg(&operator)
            .args(["token", "create", "--label", "agent"])
            .args(["--scope", "mcp-read", "--scope", "mcp-write"])
            .output()
            .unwrap(),
    );
    let secret = created.get("secret").and_then(JsonValue::as_str).unwrap();
    fs::write(config.join("token"), format!("{secret}\n")).unwrap();
    fs::set_permissions(config.join("token"), fs::Permissions::from_mode(0o600)).unwrap();
    (home, daemon)
}

#[test]
fn u23_w1_an_evaluation_is_test_evidence_a_fact_can_cite() {
    let (_dir, root) = short_tempdir();
    let (home, daemon) = install(&root);
    let repo = root.join("repo");
    repository(&repo);
    fs::create_dir_all(repo.join("evals")).unwrap();
    for id in ["one", "two"] {
        fs::write(
            repo.join("evals").join(format!("{id}.json")),
            format!(
                r#"{{"schema": "memprove.eval.task.v1", "prompt": "write done.txt",
                    "check": {{"argv": ["sh", "-c", "test -e done.txt"], "timeout_secs": 20}},
                    "tags": ["{id}"]}}"#
            ),
        )
        .unwrap();
    }
    git(&repo, &["add", "evals"]);
    git(&repo, &["commit", "--quiet", "-m", "evals"]);
    memprove(&home, &repo, &["init", "--client", "claude-code"]);
    memprove(&home, &repo, &["begin", "--new-task", "evaluate the agent"]);

    let cli = cli_binary();
    let cli = cli.to_str().unwrap();
    let recorded = memprove(
        &home,
        &repo,
        &[
            "run",
            "--test-format",
            "cargo",
            "--",
            cli,
            "eval",
            "run",
            "evals",
            "--no-memory",
            "--agent",
            "command",
            "--",
            "sh",
            "-c",
            "touch done.txt",
        ],
    );
    assert!(
        recorded.contains("test result: ok. 2 passed; 0 failed;"),
        "{recorded}"
    );
    memprove(
        &home,
        &repo,
        &[
            "checkpoint",
            "create",
            "--fact",
            "the agent passes both evals",
            "--evidence",
            "r1",
            "--next",
            "compare a second model",
        ],
    );
    let environment = Environment {
        home: Some(home.clone().into_os_string()),
        ..Environment::default()
    };
    let notes = notes_with(&environment, &repo).expect("memory is read");
    let fact = notes
        .iter()
        .find(|note| note.text == "the agent passes both evals")
        .expect("the fact");
    assert_eq!(
        (fact.kind.as_str(), fact.verdict.as_str()),
        ("fact", "verified")
    );
    daemon.stop();
}
