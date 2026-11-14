//! U23 W3, through the real binaries: the memory probe of `memprove eval`
//! reads the statements memprove would give a new session in a repository,
//! each with its verdict and bound commit, from a running daemon of a
//! default installation (found from `HOME` alone), and writes nothing; with
//! no daemon, or in a repository memprove has no context for, it gives one
//! sentence saying why.

mod u1e_support;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Output,
};

use memprove_cli::{eval_memory::notes_with, hook::Environment};
use memprove_eval::MemoryNote;
use memprove_protocol::strict_json::{JsonValue, parse_strict};
use u1e_support::{
    Daemon, bootstrap, cli_binary, git, isolated, json, ledger, repository, short_tempdir, text,
};

/// A default-prefix installation under a temporary home, as
/// `scripts/install.sh` leaves it, with a running daemon and a repository
/// of one commit.
struct Install {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    data: PathBuf,
    operator: PathBuf,
    repo: PathBuf,
    daemon: Option<Daemon>,
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
            root,
            home,
            data,
            operator,
            repo,
            daemon: Some(daemon),
        }
    }

    /// `memprove <words>` in `cwd` with only `HOME` set.
    fn run_in(&self, cwd: &Path, words: &[&str]) -> String {
        let output: Output = isolated(cli_binary())
            .current_dir(cwd)
            .env("HOME", &self.home)
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

    /// What the `SessionStart` hook would read from a process whose only
    /// variable is `HOME`.
    fn environment(&self) -> Environment {
        Environment {
            home: Some(self.home.clone().into_os_string()),
            ..Environment::default()
        }
    }

    /// Every file under the home and the repository (but `.git/` and the
    /// daemon's data directory), with its contents.
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
                    let bytes = fs::read(&path).unwrap_or_default();
                    out.push((path, bytes));
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

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"]).trim().to_owned()
    }
}

/// Registers the repository, begins a Task and records two facts: `#1`,
/// bound to the first commit, which goes stale when HEAD moves, then `#3`,
/// bound to the new HEAD, which stays verified; each checkpoint has a next
/// action, the second superseding the first. Returns the Project and the
/// two commits.
fn stale_then_verified(install: &Install) -> (String, String, String) {
    let repo = install.repo.clone();
    install.run_in(&repo, &["init", "--client", "claude-code"]);
    let begun = install.run_in(
        &repo,
        &["begin", "--new-task", "parser: handle empty input"],
    );
    let project = parse_strict(begun.trim())
        .unwrap()
        .get("project_id")
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned();

    let first = install.head();
    install.run_in(&repo, &["run", "--test", "--test-cases", "1", "--", "true"]);
    install.run_in(
        &repo,
        &[
            "checkpoint",
            "create",
            "--fact",
            "the parser rejects a truncated header",
            "--evidence",
            "r1",
            "--next",
            "make the parser accept empty input",
        ],
    );
    fs::write(repo.join("parser.txt"), "empty input is accepted\n").unwrap();
    git(&repo, &["add", "parser.txt"]);
    git(
        &repo,
        &["commit", "--quiet", "-m", "parser: accept empty input"],
    );
    let second = install.head();
    install.run_in(&repo, &["run", "--test", "--test-cases", "1", "--", "true"]);
    install.run_in(
        &repo,
        &[
            "checkpoint",
            "create",
            "--fact",
            "the parser accepts empty input",
            "--evidence",
            "r2",
            "--next",
            "write the truncated-header test",
        ],
    );
    (project, first, second)
}

#[test]
fn u23_w3_the_probe_reads_a_verified_and_a_stale_fact_and_writes_nothing() {
    let mut install = Install::new();
    let repo = install.repo.clone();
    let (project, first, second) = stale_then_verified(&install);
    let before = ledger(&install.data, &install.operator, &project);
    let files = install.files();
    let notes = notes_with(&install.environment(), &repo).expect("memory is read");
    // Read twice: the same notes; the ledger did not move, and no file of
    // the home or the repository changed.
    assert_eq!(notes_with(&install.environment(), &repo), Ok(notes.clone()));
    assert_eq!(install.files(), files, "the probe wrote a file");
    assert_eq!(
        ledger(&install.data, &install.operator, &project),
        before,
        "the probe wrote to the ledger"
    );
    // In the Pack's order: facts that are not verified, verified facts, then
    // next actions; the first next action was superseded by the second.
    // A next action cites no Evidence, so memprove holds it unconfirmed.
    assert_eq!(
        notes,
        [
            MemoryNote {
                label: "#1".to_owned(),
                kind: "fact".to_owned(),
                verdict: "stale".to_owned(),
                text: "the parser rejects a truncated header".to_owned(),
                commit: Some(first),
            },
            MemoryNote {
                label: "#3".to_owned(),
                kind: "fact".to_owned(),
                verdict: "verified".to_owned(),
                text: "the parser accepts empty input".to_owned(),
                commit: Some(second),
            },
            MemoryNote {
                label: "#4".to_owned(),
                kind: "next".to_owned(),
                verdict: "unconfirmed".to_owned(),
                text: "write the truncated-header test".to_owned(),
                commit: None,
            },
        ]
    );

    // A repository memprove has no context for.
    let elsewhere = install.root.join("elsewhere");
    repository(&elsewhere);
    let refused = notes_with(&install.environment(), &elsewhere).expect_err("no context");
    assert_eq!(
        refused,
        format!(
            "{} has no memprove context (memprove init or \
             memprove begin there gives it one)",
            elsewhere.display()
        )
    );

    // No daemon.
    if let Some(daemon) = install.daemon.take() {
        daemon.stop();
    }
    let refused = notes_with(&install.environment(), &repo).expect_err("no daemon");
    assert!(
        refused == "memproved is not running (the data directory has no socket)"
            || refused == "cannot connect to memproved",
        "{refused}"
    );
}

#[test]
fn u23_w3_the_probe_says_why_without_a_configuration() {
    let (_dir, root) = short_tempdir();
    let repo = root.join("repo");
    repository(&repo);
    let refused = notes_with(&Environment::default(), &repo).expect_err("no configuration");
    assert_eq!(
        refused,
        "no configuration directory (none of \
         MEMPROVE_CONFIG_DIR, XDG_CONFIG_HOME, HOME is set)"
    );
    // A home with a configuration directory but no installation.
    let home = root.join("home");
    fs::create_dir_all(home.join(".memprove/config")).unwrap();
    let environment = Environment {
        home: Some(home.into_os_string()),
        ..Environment::default()
    };
    let refused = notes_with(&environment, &repo).expect_err("no context");
    assert!(
        refused.ends_with(
            "has no memprove context (memprove init or memprove begin there gives it one)"
        ),
        "{refused}"
    );
}
