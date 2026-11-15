//! U23-F1, through the real binaries: `memprove eval ab` runs a task only
//! memory can pass without memory and then with memprove's Pack, read from
//! a running daemon of a default installation; the Pack it gives is, byte
//! for byte, the one the `SessionStart` hook gives a new session but for
//! what it says a reader can do with memprove (U23-F4-3: the takeover hint
//! and the `Can:` lines, with the `Digest:` line written again), and the
//! two runs write nothing to the ledger, the home, or the worktree.

mod u1e_support;

use std::{
    fs,
    io::Write as _,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Output, Stdio},
};

use memprove_domain::Sha256Digest;
use memprove_eval::runner::{self, AGENT_CONFIG_DIR, PACK_FILE};
use memprove_protocol::strict_json::{JsonValue, parse_strict};
use u1e_support::{
    Daemon, bootstrap, cli_binary, git, isolated, json, ledger, repository, short_tempdir, text,
};

/// Answers 42 only when its prompt says so, and keeps its
/// `MEMPROVE_CONFIG_DIR` next to its prompt.
const AGENT: &str = "if grep -q 'the answer is 42' \"$MEMPROVE_EVAL_PROMPT_FILE\"; \
                     then echo 42 > answer.txt; else echo unknown > answer.txt; fi; \
                     printf '%s' \"${MEMPROVE_CONFIG_DIR-unset}\" \
                     > \"$(dirname \"$MEMPROVE_EVAL_PROMPT_FILE\")/config-dir\"";

/// A default-prefix installation under a temporary home with a running
/// daemon, and a repository of one commit.
struct Install {
    _dir: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
    operator: PathBuf,
    repo: PathBuf,
    daemon: Daemon,
}

impl Install {
    fn new() -> Self {
        let (dir, root) = short_tempdir();
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
        let repo = root.join("repo");
        repository(&repo);
        Self {
            _dir: dir,
            home,
            data,
            operator,
            repo,
            daemon,
        }
    }

    /// `memprove <words>` in the repository with only `HOME` and `PATH`
    /// set, and `stdin` on its standard input.
    fn memprove(&self, words: &[&str], stdin: &str) -> String {
        let mut child = isolated(cli_binary())
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
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
            .write_all(stdin.as_bytes())
            .unwrap();
        let output: Output = child.wait_with_output().unwrap();
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

    /// Every file under the home and the repository (but `.git/`, where
    /// runs are kept, and the daemon's data directory), with its contents.
    fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, skip: &[&Path], out: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut entries = fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            entries.sort();
            for path in entries {
                if skip.contains(&path.as_path()) {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, skip, out);
                } else {
                    out.push((path.clone(), fs::read(&path).unwrap_or_default()));
                }
            }
        }
        let mut files = Vec::new();
        let git = self.repo.join(".git");
        for dir in [&self.home, &self.repo] {
            walk(dir, &[self.data.as_path(), git.as_path()], &mut files);
        }
        files
    }
}

/// The Pack in the `SessionStart` hook's output: the text of its
/// ` ```text ` block.
fn pack_of(hook: &str) -> &str {
    let start = hook.find("\n```text\n").expect("a text block") + "\n```text\n".len();
    let end = hook.rfind("```\n").expect("the block's end");
    &hook[start..end]
}

/// The takeover hint's start (`memprove_domain::pack_takeover_hint`).
const TAKEOVER: &str = "; end it before taking over: ";

/// `pack` as `eval` gives it to an agent (U23-F4-3): each `Session ` line
/// cut where its takeover hint starts, no `Can:` line, and the `Digest:`
/// line written for the lines kept (the SHA-256 of every line before it).
fn for_the_agent(pack: &str) -> String {
    let kept: String = pack
        .lines()
        .filter(|line| !line.starts_with("Can: ") && !line.starts_with("Digest: "))
        .map(|line| match line.find(TAKEOVER) {
            Some(at) if line.starts_with("Session ") => &line[..at],
            _ => line,
        })
        .map(|line| line.to_owned() + "\n")
        .collect();
    let digest = Sha256Digest::of_bytes(kept.as_bytes());
    format!("{kept}Digest: sha256:{digest}\n")
}

/// The Pack given (`pack`) is the hook's (`hook`, which has a takeover hint
/// and `Can:` lines) as `eval` gives it to an agent, and still has the
/// Session's line and the fact.
fn assert_given(pack: &str, hook: &str) {
    assert!(pack_of(hook).contains(TAKEOVER), "{hook}");
    assert!(pack_of(hook).contains("\nCan: "), "{hook}");
    assert_eq!(pack, for_the_agent(pack_of(hook)), "{hook}");
    assert!(!pack.contains(TAKEOVER), "{pack}");
    assert!(!pack.contains("session end"), "{pack}");
    assert!(!pack.contains("\nCan: "), "{pack}");
    assert!(pack.contains(" not ended (started "), "{pack}");
    assert!(pack.contains("the answer is 42"), "{pack}");
}

/// The run directories `eval ab` printed, without memory first.
fn run_dirs(printed: &str) -> Vec<PathBuf> {
    printed
        .lines()
        .filter_map(|line| line.strip_prefix("run directory: "))
        .map(PathBuf::from)
        .collect()
}

fn trial_file(run_dir: &Path, name: &str) -> String {
    let run = runner::read_run(run_dir).unwrap();
    fs::read_to_string(run_dir.join(&run.results[0].logs).join(name)).unwrap()
}

#[test]
fn u23_f1_eval_ab_gives_the_pack_a_new_session_gets_and_it_fixes_the_task() {
    let install = Install::new();
    let repo = install.repo.clone();
    fs::create_dir_all(repo.join("evals")).unwrap();
    fs::write(
        repo.join("evals/answer.json"),
        r#"{"schema": "memprove.eval.task.v1", "prompt": "Write the answer to answer.txt.",
            "check": {"argv": ["sh", "-c", "test \"$(cat answer.txt)\" = 42"]}}"#,
    )
    .unwrap();
    git(&repo, &["add", "evals"]);
    git(&repo, &["commit", "--quiet", "-m", "evals"]);
    install.memprove(&["init", "--client", "claude-code"], "");
    let begun = install.memprove(&["begin", "--new-task", "keep the answer file right"], "");
    let project = parse_strict(begun.trim())
        .unwrap()
        .get("project_id")
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned();
    install.memprove(&["run", "--test", "--test-cases", "1", "--", "true"], "");
    install.memprove(
        &[
            "checkpoint",
            "create",
            "--fact",
            "the answer is 42",
            "--evidence",
            "r1",
            "--next",
            "check the answer file",
        ],
        "",
    );

    let ledger_before = ledger(&install.data, &install.operator, &project);
    let files = install.files();
    let printed = install.memprove(
        &[
            "eval", "ab", "evals", "--agent", "command", "--", "sh", "-c", AGENT,
        ],
        "",
    );
    // The two runs wrote nothing to the ledger, the home, or the worktree.
    assert_eq!(
        ledger(&install.data, &install.operator, &project),
        ledger_before,
        "eval ab wrote to the ledger"
    );
    assert_eq!(install.files(), files, "eval ab wrote a file");

    assert!(printed.starts_with("without memory: run "), "{printed}");
    assert!(
        printed.contains(": 0 of 1 trials passed, cost n/a\n"),
        "{printed}"
    );
    assert!(
        printed.contains(": 1 of 1 trials passed, cost n/a, the agent given memprove's Pack\n"),
        "{printed}"
    );
    assert!(
        printed.contains(
            "**0.0% → 100.0% (+100.0 pp)** on the 1 task in both · 0 regressed · 1 fixed"
        ),
        "{printed}"
    );
    let dirs = run_dirs(&printed);
    let [without, with] = dirs.as_slice() else {
        panic!("two run directories: {printed}");
    };

    // The Pack given is the one the hook gives a new session here, without
    // what it says a reader can do with memprove (U23-F4-3): the hint on how
    // to end the Session `begin` opened, which named the program that wrote
    // it, and the `Can:` lines.
    let hook = install.memprove(
        &["hook", "session-start"],
        &format!(
            r#"{{"hook_event_name":"SessionStart","session_id":"f1-client","source":"startup","cwd":"{}"}}"#,
            repo.display()
        ),
    );
    let pack = fs::read_to_string(with.join(PACK_FILE)).unwrap();
    assert_given(&pack, &hook);
    let given = runner::read_run(with).unwrap().memory_given.unwrap();
    assert_eq!(given.bytes, pack.len() as u64);
    assert_eq!(runner::read_run(without).unwrap().memory_given, None);
    assert!(!without.join(PACK_FILE).exists());

    // What each arm's agent read, and where its memprove looked.
    assert_eq!(
        trial_file(without, "prompt.md"),
        "Write the answer to answer.txt."
    );
    let prompt = trial_file(with, "prompt.md");
    assert!(
        prompt.contains(&format!("```text\n{pack}```\n")),
        "{prompt}"
    );
    assert!(prompt.ends_with("\nTask:\n\nWrite the answer to answer.txt."));
    for dir in [without, with] {
        assert_eq!(
            trial_file(dir, "config-dir"),
            dir.join(AGENT_CONFIG_DIR).to_string_lossy()
        );
    }
    install.daemon.stop();
}
