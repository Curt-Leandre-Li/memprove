//! U1 (E2, with E1, E3, E4; ADR 0015): `scripts/install.sh` end to end,
//! everything in a temporary HOME, XDG directories, and prefix under `/tmp`
//! (the socket path has a length limit); no `MEMPROVE_*` variable is
//! inherited. The acceptance scenario of the U1 task: a fresh install,
//! `doctor` all PASS, a registered Git repository with begin, run,
//! checkpoint, and end, a backup, the data directory emptied and restored,
//! the daemon started, `ledger verify` valid with the same chain head; the
//! same build installed again takes the upgrade path and keeps the data,
//! the head, a backup, and the old binaries; `doctor` fails for a token file
//! of mode 0644, another build of the CLI, and a stopped daemon. Only
//! daemons these tests started are stopped. The tests install the binaries
//! of this build with `--from-binaries`; the release build the script runs
//! without it is covered by the ignored test at the end.

mod u1e_support;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use u1e_support::{
    cli_binary, git, isolated, mode, other_build, repository, short_tempdir, stop_recorded_daemon,
    string, text, workspace_root,
};

/// A temporary HOME and prefix, and the daemon an install there started,
/// stopped when the test ends.
struct Install {
    _guard: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    prefix: PathBuf,
}

impl Install {
    fn new() -> Self {
        let (guard, root) = short_tempdir();
        let home = root.join("h");
        for dir in [
            "h",
            "h/.config",
            "h/.local/share",
            "h/.local/state",
            "h/.cache",
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        Self {
            _guard: guard,
            prefix: root.join("p"),
            home,
            root,
        }
    }

    /// `program` with this test's HOME and XDG directories, and nothing of
    /// memprove's from the parent environment.
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = isolated(program);
        command
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("XDG_STATE_HOME", self.home.join(".local/state"))
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .current_dir(&self.home);
        command
    }

    /// `scripts/install.sh --prefix <prefix> <arguments>`.
    fn install(&self, arguments: &[&str]) -> Output {
        let output = self
            .command("bash")
            .arg(workspace_root().join("scripts/install.sh"))
            .arg("--prefix")
            .arg(&self.prefix)
            .args(arguments)
            .output()
            .unwrap();
        println!(
            "$ install.sh {}\n{}{}[exit {:?}]",
            arguments.join(" "),
            text(&output.stdout),
            text(&output.stderr),
            output.status.code()
        );
        self.assert_no_secret(&output);
        output
    }

    fn install_from(&self, binaries: &Path) -> Output {
        self.install(&["--from-binaries", binaries.to_str().unwrap()])
    }

    fn bin(&self, name: &str) -> PathBuf {
        self.prefix.join("bin").join(name)
    }

    fn data(&self) -> PathBuf {
        self.prefix.join("data")
    }

    /// `<prefix>/bin/ct <words>` in `cwd`.
    fn ct(&self, cwd: &Path, words: &[&str]) -> Output {
        let output = self
            .command(self.bin("ct"))
            .current_dir(cwd)
            .args(words)
            .output()
            .unwrap();
        println!(
            "$ ct {}\n{}{}[exit {:?}]",
            words.join(" "),
            text(&output.stdout),
            text(&output.stderr),
            output.status.code()
        );
        self.assert_no_secret(&output);
        output
    }

    /// `ct <words>` that must exit 0; its standard output as JSON.
    fn ct_json(&self, cwd: &Path, words: &[&str]) -> JsonValue {
        let output = self.ct(cwd, words);
        assert_eq!(output.status.code(), Some(0), "{words:?}");
        parse_strict(text(&output.stdout).trim()).unwrap()
    }

    /// `memprove <words>` as the operator (control token).
    fn operator(&self, words: &[&str]) -> Output {
        self.command(self.bin("memprove"))
            .arg("--data-dir")
            .arg(self.data())
            .arg("--token-file")
            .arg(self.prefix.join("operator-token"))
            .args(words)
            .output()
            .unwrap()
    }

    /// `ledger verify` of `project` as the agent: valid, and the chain head.
    fn ledger(&self, project: &str) -> (bool, String) {
        let report = self.ct_json(&self.home, &["ledger", "verify", "--project", project]);
        let head = match report.get("head_event_sha256") {
            Some(JsonValue::String(head)) => head.clone(),
            other => format!("{other:?}"),
        };
        (report.get("valid") == Some(&JsonValue::Bool(true)), head)
    }

    /// `ct doctor` in `cwd`, as [`Self::doctor`] runs it: the exit code and
    /// the whole report.
    fn doctor_text(&self, cwd: &Path) -> (Option<i32>, String) {
        let path = std::env::join_paths(std::iter::once(self.prefix.join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let output = self
            .command(self.bin("ct"))
            .current_dir(cwd)
            .env("PATH", path)
            .arg("doctor")
            .output()
            .unwrap();
        self.assert_no_secret(&output);
        (output.status.code(), text(&output.stdout))
    }

    /// `ct doctor` in `cwd`: the exit code and `(status, check)` per line.
    /// U7 P3: with the installation's `bin` on `PATH`, as step 4 of the
    /// next steps puts it there, so that `doctor`'s `path` line passes.
    fn doctor(&self, cwd: &Path) -> (Option<i32>, Vec<(String, String)>) {
        let path = std::env::join_paths(std::iter::once(self.prefix.join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let output = self
            .command(self.bin("ct"))
            .current_dir(cwd)
            .env("PATH", path)
            .arg("doctor")
            .output()
            .unwrap();
        println!(
            "$ ct doctor\n{}{}[exit {:?}]",
            text(&output.stdout),
            text(&output.stderr),
            output.status.code()
        );
        self.assert_no_secret(&output);
        let rows = text(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let status = parts.next()?;
                ["PASS", "WARN", "FAIL"].contains(&status).then(|| {
                    (
                        status.to_owned(),
                        parts.next().unwrap().trim_end_matches(':').to_owned(),
                    )
                })
            })
            .collect();
        (output.status.code(), rows)
    }

    fn alive(&self) -> bool {
        self.command(self.bin("memprove"))
            .arg("--data-dir")
            .arg(self.data())
            .args(["health", "live"])
            .output()
            .unwrap()
            .status
            .success()
    }

    /// No output holds the text of either token.
    fn assert_no_secret(&self, output: &Output) {
        for file in [
            self.prefix.join("operator-token"),
            self.prefix.join("config/token"),
        ] {
            let Ok(secret) = fs::read_to_string(&file) else {
                continue;
            };
            let secret = secret.trim();
            assert!(!secret.is_empty());
            assert!(
                !text(&output.stdout).contains(secret),
                "a token was printed"
            );
            assert!(
                !text(&output.stderr).contains(secret),
                "a token was printed"
            );
        }
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        stop_recorded_daemon(&self.data());
    }
}

fn status_of<'a>(rows: &'a [(String, String)], check: &str) -> &'a str {
    rows.iter().find(|(_, seen)| seen == check).map_or_else(
        || panic!("no {check} line in {rows:?}"),
        |(status, _)| status.as_str(),
    )
}

/// The directory of the binaries of this build.
fn binaries() -> PathBuf {
    cli_binary().parent().unwrap().to_path_buf()
}

/// The build identity `--version` of the installed CLI prints.
fn build_of(binary: &Path) -> String {
    let version = text(&isolated(binary).arg("--version").output().unwrap().stdout);
    version
        .trim()
        .rsplit_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .unwrap()
        .to_owned()
}

/// U10 N9: the guide installed at `guide` is the repository's `source`
/// behind the one line `<!-- installed with build <build> -->`.
fn assert_installed_guide(guide: &Path, build: &str, source: &[u8]) {
    let installed = fs::read(guide).unwrap();
    let comment = format!("<!-- installed with build {build} -->\n");
    assert!(
        installed.starts_with(comment.as_bytes()),
        "the installed guide starts with {:?}",
        String::from_utf8_lossy(&installed[..installed.len().min(80)])
    );
    assert_eq!(&installed[comment.len()..], source);
}

/// Every entry of `dir`, emptied in place.
fn empty(dir: &Path) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            fs::remove_dir_all(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)] // The acceptance scenario, in order.
fn install_doctor_work_backup_restore_upgrade_and_doctor_failures() {
    let setup = Install::new();
    let build = build_of(&cli_binary());

    // A fresh install.
    let fresh = setup.install_from(&binaries());
    assert_eq!(fresh.status.code(), Some(0));
    assert!(text(&fresh.stdout).contains("Next steps:"));
    assert!(text(&fresh.stdout).contains("project register"));
    assert!(text(&fresh.stdout).contains("\"SessionStart\""));
    assert!(text(&fresh.stdout).contains("\"mcpServers\""));
    // U5 P3 (Owner O5): the next steps print the fragment `mcp
    // print-config` prints for this installation's configuration
    // directory, and the first Session is the real Task's (no "set up").
    let printed = setup
        .command(setup.bin("memprove"))
        .env("MEMPROVE_CONFIG_DIR", setup.prefix.join("config"))
        .args(["mcp", "print-config"])
        .output()
        .unwrap();
    assert_eq!(printed.status.code(), Some(0));
    let fragment = text(&printed.stdout);
    assert!(fragment.contains("\"permissions\""), "{fragment}");
    // U5 Z-F round 1 (S1): both parts, each under the line naming its file.
    assert!(
        fragment.starts_with("==> .claude/settings.local.json <==\n")
            && fragment.contains("\n==> .mcp.json <==\n"),
        "{fragment}"
    );
    assert!(
        text(&fresh.stdout).contains(&fragment),
        "the next steps lack the fragment:\n{fragment}"
    );
    assert!(!text(&fresh.stdout).contains("--reason \"set up\""));
    assert!(text(&fresh.stdout).contains("ct hook install"));
    for dir in ["", "bin", "data", "config"] {
        assert_eq!(mode(&setup.prefix.join(dir)), 0o700, "{dir}");
    }
    for file in ["operator-token", "config/token", "config/data-dir"] {
        assert_eq!(mode(&setup.prefix.join(file)), 0o600, "{file}");
    }
    for name in ["memprove", "memproved", "ct", "ensure-daemon.sh"] {
        assert!(setup.bin(name).is_file(), "{name}");
    }
    assert_eq!(build_of(&setup.bin("memprove")), build);
    assert_eq!(build_of(&setup.bin("memproved")), build);
    assert!(setup.alive());
    let tokens: Vec<String> = ["operator-token", "config/token"]
        .iter()
        .map(|file| fs::read_to_string(setup.prefix.join(file)).unwrap())
        .collect();

    // doctor: every check passes.
    let (code, rows) = setup.doctor(&setup.home);
    assert_eq!(code, Some(0));
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|(status, _)| status == "PASS"), "{rows:?}");

    // A temporary Git repository: register, begin, run, checkpoint, end.
    let repo = setup.root.join("r");
    repository(&repo);
    let registered = setup.operator(&["project", "register", repo.to_str().unwrap()]);
    assert_eq!(
        registered.status.code(),
        Some(0),
        "{}",
        text(&registered.stderr)
    );
    let registered = parse_strict(text(&registered.stdout).trim()).unwrap();
    let project = string(&registered, "project_id").to_owned();
    let begun = setup.ct(
        &repo,
        &["begin", "--project", &project, "--new-task", "Install"],
    );
    assert_eq!(begun.status.code(), Some(0));
    let run = setup.ct(
        &repo,
        &[
            "run",
            "--test-format",
            "cargo",
            "--",
            "sh",
            "tests/check.sh",
        ],
    );
    assert_eq!(run.status.code(), Some(0));
    let checkpoint = setup.ct(
        &repo,
        &[
            "checkpoint",
            "create",
            "--fact",
            "check passes",
            "--next",
            "nothing",
        ],
    );
    assert_eq!(checkpoint.status.code(), Some(0));
    let ended = setup.ct(&repo, &["end", "--reason", "installed"]);
    assert_eq!(ended.status.code(), Some(0));
    let before = setup.ledger(&project);
    assert!(before.0);
    assert!(git(&repo, &["status", "--porcelain"]).is_empty());

    // Backup, empty the data directory, restore, start, verify.
    stop_recorded_daemon(&setup.data());
    assert!(!setup.alive());
    let backup = setup.root.join("bk");
    let backed = setup
        .command(setup.bin("memprove"))
        .args(["backup", "--data-dir"])
        .arg(setup.data())
        .arg("--out")
        .arg(&backup)
        .output()
        .unwrap();
    assert_eq!(backed.status.code(), Some(0), "{}", text(&backed.stderr));
    empty(&setup.data());
    let restored = setup
        .command(setup.bin("memprove"))
        .arg("restore")
        .arg("--from")
        .arg(&backup)
        .arg("--data-dir")
        .arg(setup.data())
        .output()
        .unwrap();
    assert_eq!(
        restored.status.code(),
        Some(0),
        "{}",
        text(&restored.stderr)
    );
    let started = setup
        .command(setup.bin("ensure-daemon.sh"))
        .output()
        .unwrap();
    assert_eq!(started.status.code(), Some(0), "{}", text(&started.stderr));
    assert_eq!(setup.ledger(&project), before);

    // The same build again: the upgrade path.
    let upgraded = setup.install_from(&binaries());
    assert_eq!(upgraded.status.code(), Some(0));
    let upgrade_text = text(&upgraded.stdout);
    assert!(
        upgrade_text.contains("Upgrading memprove"),
        "{upgrade_text}"
    );
    assert!(upgrade_text.contains(&format!("Project {project}: valid")));
    assert!(setup.alive());
    assert_eq!(setup.ledger(&project), before);
    for name in ["memprove", "memproved"] {
        let old = setup.bin(&format!("{name}.{build}"));
        assert!(old.is_file(), "{} is missing", old.display());
        assert_eq!(fs::read(&old).unwrap(), fs::read(setup.bin(name)).unwrap());
    }
    let backups: Vec<PathBuf> = fs::read_dir(setup.prefix.join("backups"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    assert!(backups[0].join("SHA256SUMS").is_file());
    assert!(backups[0].join("registry.sqlite").is_file());
    assert!(
        backups[0]
            .join(format!("projects/{project}/state.sqlite"))
            .is_file()
    );
    let tokens_after: Vec<String> = ["operator-token", "config/token"]
        .iter()
        .map(|file| fs::read_to_string(setup.prefix.join(file)).unwrap())
        .collect();
    assert_eq!(tokens, tokens_after, "an upgrade keeps both tokens");
    let (code, rows) = setup.doctor(&setup.home);
    assert_eq!(code, Some(0));
    assert!(rows.iter().all(|(status, _)| status == "PASS"), "{rows:?}");

    // doctor in the Workspace: its context and its ledger pass.
    let (_, rows) = setup.doctor(&repo);
    assert_eq!(status_of(&rows, "workspace"), "PASS");
    assert_eq!(status_of(&rows, "ledger"), "PASS");

    // doctor's failures: a token file of mode 0644 ...
    let agent = setup.prefix.join("config/token");
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o644)).unwrap();
    let (code, rows) = setup.doctor(&setup.home);
    assert_eq!(code, Some(1));
    assert_eq!(status_of(&rows, "token-file"), "FAIL");
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o600)).unwrap();
    // ... a CLI of another build in place of the installed one ...
    let installed = setup.bin("memprove");
    let original = setup.root.join("memprove-installed");
    fs::rename(&installed, &original).unwrap();
    fs::copy(other_build(&setup.root), &installed).unwrap();
    let (code, rows) = setup.doctor(&setup.home);
    assert_eq!(code, Some(1));
    assert_eq!(status_of(&rows, "build"), "FAIL");
    fs::rename(&original, &installed).unwrap();
    // ... and a stopped daemon.
    stop_recorded_daemon(&setup.data());
    let (code, rows) = setup.doctor(&setup.home);
    assert_eq!(code, Some(1));
    assert_eq!(status_of(&rows, "daemon"), "FAIL");
}

/// U7 K4 (D2 F20, F11; ADR 0015, Amendment U7): the installation holds the
/// agent's guide at `<prefix>/share/guide.md`, byte for byte the one of
/// the repository, and the next steps name its path and put the `PATH` line
/// in the words of `doctor`'s `path` line. The upgrade brings the guide to the
/// version of the new binaries and names it too.
#[test]
fn u7_k4_the_guide_is_installed_and_the_next_steps_name_it_and_the_path_line() {
    let setup = Install::new();
    let fresh = setup.install_from(&binaries());
    assert_eq!(fresh.status.code(), Some(0));
    let guide = setup.prefix.join("share/guide.md");
    let repository_guide = fs::read(workspace_root().join("docs/guide.md")).unwrap();
    assert!(!repository_guide.is_empty());
    // U10 N9: the repository's guide, behind the line that names the build.
    assert_installed_guide(&guide, &build_of(&setup.bin("memprove")), &repository_guide);
    assert_eq!(mode(&setup.prefix.join("share")), 0o700);
    assert_eq!(mode(&guide), 0o600);
    let steps = text(&fresh.stdout);
    assert!(
        steps.contains(&format!("{}", guide.display())),
        "the next steps do not name the guide:\n{steps}"
    );
    // The PATH line, in doctor's words.
    let bin = setup.prefix.join("bin");
    assert!(
        steps.contains(&format!("export PATH=\"{}:$PATH\"", bin.display())),
        "{steps}"
    );
    let doctor = setup
        .command(setup.bin("ct"))
        .env("PATH", "/usr/bin:/bin")
        .arg("doctor")
        .output()
        .unwrap();
    let doctor = text(&doctor.stdout);
    let line = doctor
        .lines()
        .find(|line| line.starts_with("WARN path:"))
        .unwrap_or_else(|| panic!("no WARN path line:\n{doctor}"));
    for words in [
        "a `memprove run` the agent types is not found",
        "Bash(memprove run:*)",
    ] {
        assert!(line.contains(words), "doctor: {words}");
        assert!(steps.contains(words), "the next steps: {words}");
    }

    // An upgrade brings the guide to the version of the new binaries.
    fs::write(&guide, "stale\n").unwrap();
    let upgrade = setup.install_from(&binaries());
    assert_eq!(upgrade.status.code(), Some(0));
    assert_installed_guide(&guide, &build_of(&setup.bin("memprove")), &repository_guide);
    assert!(
        text(&upgrade.stdout).contains(&format!("{}", guide.display())),
        "{}",
        text(&upgrade.stdout)
    );
}

/// U10 N9 (D3 F28; Owner O6; ADR 0015, "Amendment (U10, S-K)"): `doctor`
/// has a `guide` line that compares the installed guide's first line with
/// this binary's build: PASS when they are the same, WARN (exit 0) when the
/// guide is for another build, has no such line (installed before U10), or
/// is gone; an upgrade writes it again. Outside an installation made by the
/// script the line passes and says there is no guide to compare.
#[test]
fn u10_n9_doctor_compares_the_installed_guide_with_the_binary() {
    let setup = Install::new();
    let fresh = setup.install_from(&binaries());
    assert_eq!(fresh.status.code(), Some(0));
    let build = build_of(&setup.bin("memprove"));
    let guide = setup.prefix.join("share/guide.md");
    let line = |setup: &Install| -> (Option<i32>, String) {
        let (code, text) = setup.doctor_text(&setup.home);
        let line = text
            .lines()
            .find(|line| line.contains(" guide: "))
            .unwrap_or_else(|| panic!("no guide line in:\n{text}"))
            .to_owned();
        (code, line)
    };

    // Installed with the binaries: the same build.
    let (code, seen) = line(&setup);
    assert_eq!(code, Some(0));
    assert!(seen.starts_with("PASS guide: "), "{seen}");
    assert!(seen.contains(&build), "{seen}");

    // Another build's guide: a WARN that names both builds and the cure.
    let source = fs::read(workspace_root().join("docs/guide.md")).unwrap();
    let mut other = b"<!-- installed with build 0123456789ab -->\n".to_vec();
    other.extend_from_slice(&source);
    fs::write(&guide, &other).unwrap();
    let (code, seen) = line(&setup);
    assert_eq!(code, Some(0), "a warning is not a failure: {seen}");
    assert!(seen.starts_with("WARN guide: "), "{seen}");
    for words in ["0123456789ab", build.as_str(), "scripts/install.sh"] {
        assert!(seen.contains(words), "{words}: {seen}");
    }

    // A guide without the line (installed before U10).
    fs::write(&guide, &source).unwrap();
    let (code, seen) = line(&setup);
    assert_eq!(code, Some(0));
    assert!(seen.starts_with("WARN guide: "), "{seen}");
    assert!(seen.contains("no build comment"), "{seen}");

    // No guide at all.
    fs::remove_file(&guide).unwrap();
    let (code, seen) = line(&setup);
    assert_eq!(code, Some(0));
    assert!(seen.starts_with("WARN guide: "), "{seen}");
    assert!(seen.contains("no guide"), "{seen}");

    // An upgrade writes it again, and the line passes.
    let upgrade = setup.install_from(&binaries());
    assert_eq!(upgrade.status.code(), Some(0));
    assert_installed_guide(&guide, &build, &source);
    let (_, seen) = line(&setup);
    assert!(seen.starts_with("PASS guide: "), "{seen}");

    // The binary of the build tree is not in an installation.
    let outside = isolated(cli_binary())
        .current_dir(&setup.home)
        .env("HOME", &setup.home)
        .env("MEMPROVE_CONFIG_DIR", setup.prefix.join("config"))
        .arg("doctor")
        .output()
        .unwrap();
    let outside = text(&outside.stdout);
    let seen = outside
        .lines()
        .find(|line| line.contains(" guide: "))
        .unwrap_or_else(|| panic!("no guide line in:\n{outside}"));
    assert!(seen.starts_with("PASS guide: "), "{seen}");
    assert!(seen.contains("not in an installation"), "{seen}");
}

#[test]
fn a_failed_upgrade_puts_the_old_version_back() {
    let setup = Install::new();
    assert_eq!(setup.install_from(&binaries()).status.code(), Some(0));
    let repo = setup.root.join("r");
    repository(&repo);
    let registered = setup.operator(&["project", "register", repo.to_str().unwrap()]);
    let registered = parse_strict(text(&registered.stdout).trim()).unwrap();
    let project = string(&registered, "project_id").to_owned();
    let before = setup.ledger(&project);
    let daemon_before = fs::read(setup.bin("memproved")).unwrap();
    let cli_before = fs::read(setup.bin("memprove")).unwrap();

    // New binaries whose daemon reports the same build but cannot start.
    let broken = setup.root.join("broken");
    fs::create_dir(&broken).unwrap();
    fs::copy(cli_binary(), broken.join("memprove")).unwrap();
    let build = build_of(&cli_binary());
    let script = broken.join("memproved");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'memproved {} ({build})'; exit 0; fi\n\
             echo 'memproved: cannot start' >&2\nexit 1\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();

    let failed = setup.install_from(&broken);
    assert_eq!(failed.status.code(), Some(1));
    assert!(text(&failed.stderr).contains("the old version is back"));
    assert_eq!(fs::read(setup.bin("memproved")).unwrap(), daemon_before);
    assert_eq!(fs::read(setup.bin("memprove")).unwrap(), cli_before);
    assert!(setup.alive(), "the old daemon runs again");
    assert_eq!(setup.ledger(&project), before);
    // The backup made before the swap is kept.
    assert_eq!(
        fs::read_dir(setup.prefix.join("backups")).unwrap().count(),
        1
    );
}

/// U2 (S4, N-7, original #133, Owner O7; ADR 0015, "Amendment (U2,
/// W-B)"): an upgrade leaves the daemon as it found it. One that was
/// running runs the new build afterwards; one that was not running is
/// started for `ledger verify` and stopped again, its socket gone; and a
/// failed upgrade of a stopped daemon leaves the old one stopped.
#[test]
fn u2_s4_an_upgrade_leaves_the_daemon_running_or_stopped_as_it_was() {
    let setup = Install::new();
    assert_eq!(setup.install_from(&binaries()).status.code(), Some(0));
    let repo = setup.root.join("r");
    repository(&repo);
    let registered = setup.operator(&["project", "register", repo.to_str().unwrap()]);
    let registered = parse_strict(text(&registered.stdout).trim()).unwrap();
    let project = string(&registered, "project_id").to_owned();
    let before = setup.ledger(&project);

    // It was running: it runs afterwards.
    assert!(setup.alive());
    let running = setup.install_from(&binaries());
    assert_eq!(running.status.code(), Some(0));
    assert!(text(&running.stdout).contains(&format!("Project {project}: valid")));
    assert!(
        setup.alive(),
        "a daemon that was running runs after the upgrade"
    );
    assert_eq!(setup.ledger(&project), before);

    // It was not running: verified, then stopped again.
    stop_recorded_daemon(&setup.data());
    assert!(!setup.alive());
    let stopped = setup.install_from(&binaries());
    assert_eq!(stopped.status.code(), Some(0));
    let output = text(&stopped.stdout);
    assert!(
        output.contains(&format!("Project {project}: valid")),
        "{output}"
    );
    assert!(
        output.contains("memproved was not running before the upgrade; stopping it again"),
        "{output}"
    );
    assert!(
        !setup.alive(),
        "a daemon that was not running is stopped again"
    );
    assert!(!setup.data().join("memproved.sock").exists());
    assert_eq!(
        fs::read_dir(setup.prefix.join("backups")).unwrap().count(),
        2
    );

    // A failed upgrade of a stopped daemon puts the old version back,
    // stopped: new binaries whose daemon reports this build but cannot start.
    let broken = setup.root.join("broken");
    fs::create_dir(&broken).unwrap();
    fs::copy(cli_binary(), broken.join("memprove")).unwrap();
    let script = broken.join("memproved");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'memproved {} ({})'; exit 0; fi\n\
             echo 'memproved: cannot start' >&2\nexit 1\n",
            env!("CARGO_PKG_VERSION"),
            build_of(&cli_binary())
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let failed = setup.install_from(&broken);
    assert_eq!(failed.status.code(), Some(1));
    assert!(text(&failed.stderr).contains("the old version is back"));
    assert!(!setup.alive(), "the old daemon stays stopped, as it was");

    // The data is as it was: started again, the ledger is the same.
    let started = setup
        .command(setup.bin("ensure-daemon.sh"))
        .output()
        .unwrap();
    assert_eq!(started.status.code(), Some(0), "{}", text(&started.stderr));
    assert_eq!(setup.ledger(&project), before);
}

/// U2 (W-F, SF-1 of the W-V review; ADR 0015, "Amendment (U2, W-F)"): an
/// install whose data directories were made before S2 under umask 022
/// (`projects/`, `projects/<ID>/`, `tmp/` of mode 0755) fails `doctor`'s
/// `projects-dir`; an upgrade takes group's and others' access away from
/// them after the backup, which records them as they were, and touches no
/// file. Afterwards `doctor` passes, the ledger is the same, and a second
/// upgrade has nothing to change.
#[test]
fn u2_f1_an_upgrade_tightens_data_directories_made_before_s2() {
    let setup = Install::new();
    assert_eq!(setup.install_from(&binaries()).status.code(), Some(0));
    let repo = setup.root.join("r");
    repository(&repo);
    let registered = setup.operator(&["project", "register", repo.to_str().unwrap()]);
    let registered = parse_strict(text(&registered.stdout).trim()).unwrap();
    let project = string(&registered, "project_id").to_owned();
    let before = setup.ledger(&project);

    // As an install made before S2 under umask 022 left them.
    let data = setup.data();
    let directories = [
        data.join("projects"),
        data.join("projects").join(&project),
        data.join("tmp"),
    ];
    for dir in &directories {
        assert!(dir.is_dir(), "{} is missing", dir.display());
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let files = [
        data.join("registry.sqlite"),
        data.join("projects").join(&project).join("state.sqlite"),
    ];
    let file_modes: Vec<u32> = files.iter().map(|file| mode(file)).collect();
    let (code, rows) = setup.doctor(&setup.home);
    assert_eq!(code, Some(1));
    assert_eq!(status_of(&rows, "projects-dir"), "FAIL");

    let upgraded = setup.install_from(&binaries());
    assert_eq!(upgraded.status.code(), Some(0));
    let output = text(&upgraded.stdout);
    for relative in [
        "data/projects".to_owned(),
        format!("data/projects/{project}"),
        "data/tmp".to_owned(),
    ] {
        assert!(
            output.contains(&format!(
                "    tightened {relative}: drwxr-xr-x -> drwx------\n"
            )),
            "{output}"
        );
    }
    for dir in &directories {
        assert_eq!(mode(dir), 0o700, "{}", dir.display());
    }
    let after: Vec<u32> = files.iter().map(|file| mode(file)).collect();
    assert_eq!(after, file_modes, "no file's mode changes");
    assert!(setup.alive());
    assert_eq!(setup.ledger(&project), before);
    let (code, rows) = setup.doctor(&setup.home);
    assert_eq!(code, Some(0), "{rows:?}");
    assert_eq!(status_of(&rows, "projects-dir"), "PASS");
    assert!(rows.iter().all(|(status, _)| status == "PASS"), "{rows:?}");

    // The backup, taken before, records the modes as they were.
    let backups: Vec<PathBuf> = fs::read_dir(setup.prefix.join("backups"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    let manifest = fs::read_to_string(backups[0].join("continuity-backup.txt")).unwrap();
    for line in [
        "dir 755 projects".to_owned(),
        format!("dir 755 projects/{project}"),
    ] {
        assert!(manifest.lines().any(|seen| seen == line), "{manifest}");
    }

    // A second upgrade finds nothing to tighten.
    let again = setup.install_from(&binaries());
    assert_eq!(again.status.code(), Some(0));
    assert!(!text(&again.stdout).contains("tightened"));
    assert_eq!(setup.ledger(&project), before);
}

#[test]
fn print_launchd_prints_a_plist_and_changes_nothing() {
    let setup = Install::new();
    let output = setup.install(&["--print-launchd"]);
    assert_eq!(output.status.code(), Some(0));
    let plist = text(&output.stdout);
    assert!(plist.starts_with("<?xml"));
    assert!(plist.contains(&format!(
        "<string>{}</string>",
        setup.prefix.join("bin/memproved").display()
    )));
    assert!(plist.contains(&format!("<string>{}</string>", setup.data().display())));
    assert!(!setup.prefix.exists(), "nothing is installed");
}

#[test]
fn install_refuses_bad_arguments_and_a_partial_prefix() {
    let setup = Install::new();
    for arguments in [&["--bogus"][..], &["--from-binaries"][..]] {
        assert_eq!(setup.install(arguments).status.code(), Some(2));
    }
    let quoted = setup
        .command("bash")
        .arg(workspace_root().join("scripts/install.sh"))
        .arg("--prefix")
        .arg(setup.root.join("a'b"))
        .output()
        .unwrap();
    assert_eq!(quoted.status.code(), Some(2));
    let long = setup
        .command("bash")
        .arg(workspace_root().join("scripts/install.sh"))
        .arg("--prefix")
        .arg(setup.root.join("x".repeat(100)))
        .output()
        .unwrap();
    assert_eq!(long.status.code(), Some(2));
    assert!(text(&long.stderr).contains("socket"));
    fs::create_dir_all(setup.prefix.join("data")).unwrap();
    fs::write(setup.prefix.join("data/registry.sqlite"), "x").unwrap();
    let partial = setup.install_from(&binaries());
    assert_eq!(partial.status.code(), Some(1));
    assert!(text(&partial.stderr).contains("partial installation"));
}

/// The release build `install.sh` runs without `--from-binaries`, in a
/// target directory of its own (`target/u1e-release`, inside the ignored
/// `target/`) so it never waits on the build directory of the `cargo test`
/// that runs it. Slow (a full offline release build); run it with
/// `cargo test -p memprove-cli --test u1e_install_e2e -- --ignored`.
#[test]
#[ignore = "runs a full offline release build; see the doc comment"]
fn a_fresh_install_builds_the_release_binaries_offline() {
    let setup = Install::new();
    let target = workspace_root().join("target/u1e-release");
    let mut command = setup.command("bash");
    command
        .arg(workspace_root().join("scripts/install.sh"))
        .arg("--prefix")
        .arg(&setup.prefix)
        .env("CARGO_TARGET_DIR", &target)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    // The offline crate cache stays the caller's: CARGO_HOME, else the
    // parent's ~/.cargo (only cargo itself reads and locks it).
    let cargo_home = std::env::var_os("CARGO_HOME").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cargo"),
        PathBuf::from,
    );
    command.env("CARGO_HOME", cargo_home);
    for name in ["LIBCLANG_PATH", "DYLD_FALLBACK_LIBRARY_PATH", "RUSTUP_HOME"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let output = command.output().unwrap();
    println!("{}{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output.stdout).contains("Building the release binaries"));
    assert_eq!(
        build_of(&setup.bin("memprove")),
        build_of(&target.join("release/memprove"))
    );
    let (code, rows) = setup.doctor(&setup.home);
    assert_eq!(code, Some(0));
    assert!(rows.iter().all(|(status, _)| status == "PASS"), "{rows:?}");
}

/// U3 (K7, F-N-2, original #76; ADR 0015, "Amendment (U3, X-K)"): step 5 of
/// an upgrade verifies the ledger of the Projects the registry lists, each
/// confirmed with the control token; a directory in `data/projects/` the
/// registry does not list (a stray name, or a `UUIDv7` nobody registered)
/// gets a warning and is left as it is, and the upgrade does not roll back.
#[test]
fn u3_k7_an_upgrade_verifies_registered_projects_and_warns_about_strays() {
    let setup = Install::new();
    assert_eq!(setup.install_from(&binaries()).status.code(), Some(0));
    let repo = setup.root.join("r");
    repository(&repo);
    let registered = setup.operator(&["project", "register", repo.to_str().unwrap()]);
    let registered = parse_strict(text(&registered.stdout).trim()).unwrap();
    let project = string(&registered, "project_id").to_owned();
    let before = setup.ledger(&project);
    let projects = setup.data().join("projects");
    // U3 (X-F SF-1): a name that starts with a dot is a stray as well.
    let strays = [
        "not-a-project",
        "01a14000-0000-7000-8000-000000000000",
        ".hidden-stray",
    ];
    for stray in strays {
        fs::create_dir(projects.join(stray)).unwrap();
        fs::set_permissions(projects.join(stray), fs::Permissions::from_mode(0o700)).unwrap();
    }

    let upgraded = setup.install_from(&binaries());
    let (stdout, stderr) = (text(&upgraded.stdout), text(&upgraded.stderr));
    assert_eq!(upgraded.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!("    Project {project}: valid\n")),
        "{stdout}"
    );
    for stray in strays {
        assert!(
            stderr.contains(&format!(
                "install.sh: data/projects/{stray} is not a Project the registry lists; its \
                 ledger was not verified, and it was left as it is\n"
            )),
            "{stderr}"
        );
        assert!(projects.join(stray).is_dir());
        assert!(!stdout.contains(&format!("Project {stray}")), "{stdout}");
    }
    assert!(!stderr.contains("putting the old version back"), "{stderr}");
    assert!(stdout.contains("Upgraded to "), "{stdout}");
    assert!(setup.alive());
    assert_eq!(setup.ledger(&project), before);
}

/// U3 (X-F SF-1), widened by U4 (Y2, R1-N1): without the control token file
/// step 5 verifies every directory in `data/projects/` whose name has the
/// shape of a `UUIDv7` as before; any other name (one that starts with a dot,
/// or any foreign name) is not a Project ID, so it gets the stray warning,
/// is not verified, and the upgrade goes on.
#[test]
fn u3_xf1_an_upgrade_without_the_control_token_warns_about_a_dot_named_stray() {
    let setup = Install::new();
    assert_eq!(setup.install_from(&binaries()).status.code(), Some(0));
    let repo = setup.root.join("r");
    repository(&repo);
    let registered = setup.operator(&["project", "register", repo.to_str().unwrap()]);
    let registered = parse_strict(text(&registered.stdout).trim()).unwrap();
    let project = string(&registered, "project_id").to_owned();
    let before = setup.ledger(&project);
    let stray = setup.data().join("projects").join(".hidden-stray");
    fs::create_dir(&stray).unwrap();
    fs::set_permissions(&stray, fs::Permissions::from_mode(0o700)).unwrap();
    // U4 (Y2): names that do not start with a dot but are not UUIDv7 either.
    let foreign = [
        "not-a-project",
        "backup.old",
        "01A14000-0000-7000-8000-000000000000",
    ];
    for name in foreign {
        let directory = setup.data().join("projects").join(name);
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let operator_token = setup.prefix.join("operator-token");
    let moved = setup.root.join("operator-token.away");
    fs::rename(&operator_token, &moved).unwrap();

    let upgraded = setup.install_from(&binaries());
    fs::rename(&moved, &operator_token).unwrap();
    let (stdout, stderr) = (text(&upgraded.stdout), text(&upgraded.stderr));
    assert_eq!(upgraded.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stderr.contains(&format!(
            "install.sh: no control token at {}; every directory in data/projects is verified\n",
            operator_token.display()
        )),
        "{stderr}"
    );
    assert!(
        stdout.contains(&format!("    Project {project}: valid\n")),
        "{stdout}"
    );
    assert!(
        stderr.contains(
            "install.sh: data/projects/.hidden-stray is not a Project the registry lists; its \
             ledger was not verified, and it was left as it is\n"
        ),
        "{stderr}"
    );
    assert!(stray.is_dir());
    assert!(!stdout.contains("Project .hidden-stray"), "{stdout}");
    for name in foreign {
        assert!(
            stderr.contains(&format!(
                "install.sh: data/projects/{name} is not a Project the registry lists; its \
                 ledger was not verified, and it was left as it is\n"
            )),
            "{name}: {stderr}"
        );
        assert!(setup.data().join("projects").join(name).is_dir());
        assert!(!stdout.contains(&format!("Project {name}")), "{stdout}");
    }
    assert!(!stderr.contains("putting the old version back"), "{stderr}");
    assert!(stdout.contains("Upgraded to "), "{stdout}");
    assert!(setup.alive());
    assert_eq!(setup.ledger(&project), before);
}

/// U5 (H1, Y-V N-2): without the control token file the registry cannot be
/// asked, so a directory shaped like a `UUIDv7` is still given to
/// `ledger verify`; when that answers `404 project_not_found` the directory
/// is one the registry does not list, so it gets the stray warning, is left
/// as it is, and the upgrade goes on instead of rolling back. A Project the
/// registry lists is verified as before.
#[test]
fn u5_h1_an_upgrade_without_the_control_token_warns_about_an_unlisted_uuid_directory() {
    let setup = Install::new();
    assert_eq!(setup.install_from(&binaries()).status.code(), Some(0));
    let repo = setup.root.join("r");
    repository(&repo);
    let registered = setup.operator(&["project", "register", repo.to_str().unwrap()]);
    let registered = parse_strict(text(&registered.stdout).trim()).unwrap();
    let project = string(&registered, "project_id").to_owned();
    let before = setup.ledger(&project);
    let unlisted = "01a14000-0000-7000-8000-000000000001";
    let directory = setup.data().join("projects").join(unlisted);
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let operator_token = setup.prefix.join("operator-token");
    let moved = setup.root.join("operator-token.away");
    fs::rename(&operator_token, &moved).unwrap();

    let upgraded = setup.install_from(&binaries());
    fs::rename(&moved, &operator_token).unwrap();
    let (stdout, stderr) = (text(&upgraded.stdout), text(&upgraded.stderr));
    assert_eq!(upgraded.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!("    Project {project}: valid\n")),
        "{stdout}"
    );
    assert!(
        stderr.contains(&format!(
            "install.sh: data/projects/{unlisted} is not a Project the registry lists; its \
             ledger was not verified, and it was left as it is\n"
        )),
        "{stderr}"
    );
    assert!(directory.is_dir());
    assert!(!stdout.contains(&format!("Project {unlisted}")), "{stdout}");
    assert!(!stderr.contains("putting the old version back"), "{stderr}");
    assert!(stdout.contains("Upgraded to "), "{stdout}");
    assert!(setup.alive());
    assert_eq!(setup.ledger(&project), before);
}

/// U19 (T5; the rename to memprove): an installation made under the
/// previous name holds `bin/continuity` and `bin/continuityd`, a `ct` and an
/// `ensure-daemon.sh` that run them, and the lock and socket of `continuityd`.
/// `install.sh` gives the binaries their new names, drops the stale lock and
/// socket, upgrades them as usual, and writes `ct` and `ensure-daemon.sh`
/// again for the new names; `ensure-daemon.sh` then starts the daemon and
/// `doctor` passes.
#[test]
fn u19_t5_an_installation_of_the_previous_name_is_renamed_in_place() {
    let setup = Install::new();
    let build = build_of(&cli_binary());
    assert_eq!(setup.install_from(&binaries()).status.code(), Some(0));
    let repo = setup.root.join("r");
    repository(&repo);
    let registered = setup.operator(&["project", "register", repo.to_str().unwrap()]);
    let registered = parse_strict(text(&registered.stdout).trim()).unwrap();
    let project = string(&registered, "project_id").to_owned();
    let before = setup.ledger(&project);

    // The installation as the previous name left it: the binaries, ct and
    // ensure-daemon.sh under the old names, a stale lock and socket.
    stop_recorded_daemon(&setup.data());
    fs::rename(setup.bin("memprove"), setup.bin("continuity")).unwrap();
    fs::rename(setup.bin("memproved"), setup.bin("continuityd")).unwrap();
    fs::write(
        setup.bin("ct"),
        "#!/bin/bash\nexport CONTINUITY_DATA_DIR=\"$PREFIX/data\"\nexec \"$PREFIX/bin/continuity\" \"$@\"\n",
    )
    .unwrap();
    fs::write(
        setup.bin("ensure-daemon.sh"),
        "#!/bin/bash\nexec \"$PREFIX/bin/continuityd\" --data-dir \"$PREFIX/data\"\n",
    )
    .unwrap();
    fs::write(setup.data().join("continuityd.lock"), "99999999\n").unwrap();
    fs::write(setup.data().join("continuityd.sock"), "").unwrap();

    let renamed = setup.install_from(&binaries());
    assert_eq!(renamed.status.code(), Some(0), "{}", text(&renamed.stderr));
    let stdout = text(&renamed.stdout);
    assert!(
        stdout.contains("Renaming the installation in ")
            && stdout.contains(" from continuity to memprove ..."),
        "{stdout}"
    );
    assert!(stdout.contains("Upgraded to "), "{stdout}");
    for name in ["memprove", "memproved", "ct", "ensure-daemon.sh"] {
        assert!(setup.bin(name).is_file(), "{name}");
    }
    assert_eq!(build_of(&setup.bin("memprove")), build);
    assert_eq!(build_of(&setup.bin("memproved")), build);
    // The old binaries are kept under their build, as in any upgrade.
    assert!(setup.bin(&format!("memprove.{build}")).is_file());
    assert!(setup.bin(&format!("memproved.{build}")).is_file());
    for name in ["continuity", "continuityd"] {
        assert!(!setup.bin(name).exists(), "bin/{name} is still there");
    }
    for name in ["continuityd.lock", "continuityd.sock"] {
        assert!(
            !setup.data().join(name).exists(),
            "data/{name} is still there"
        );
    }
    for name in ["ct", "ensure-daemon.sh"] {
        let script = fs::read_to_string(setup.bin(name)).unwrap();
        assert!(
            !script.contains("continuityd") && !script.contains("CONTINUITY_"),
            "{name} still runs the previous name:\n{script}"
        );
    }
    let ct = fs::read_to_string(setup.bin("ct")).unwrap();
    assert!(
        ct.contains("MEMPROVE_DATA_DIR") && ct.contains("/bin/memprove\""),
        "{ct}"
    );
    assert!(
        fs::read_to_string(setup.bin("ensure-daemon.sh"))
            .unwrap()
            .contains("memproved"),
    );

    // The daemon was not running: it is stopped again, as after any upgrade;
    // the new ensure-daemon.sh starts it, the ledger is the same, and doctor
    // passes.
    assert!(!setup.alive());
    let started = setup
        .command(setup.bin("ensure-daemon.sh"))
        .output()
        .unwrap();
    assert_eq!(started.status.code(), Some(0), "{}", text(&started.stderr));
    assert!(setup.alive());
    assert_eq!(setup.ledger(&project), before);
    let (code, rows) = setup.doctor(&setup.home);
    assert_eq!(code, Some(0));
    assert!(rows.iter().all(|(status, _)| status == "PASS"), "{rows:?}");
}

/// U19 (T5): an installation left with one binary under each name (an
/// interrupted renaming) is finished the same way, with the daemon that was
/// running left running; one that holds a binary under both names is refused
/// untouched.
#[test]
fn u19_t5_an_interrupted_renaming_is_finished_and_two_names_are_refused() {
    let setup = Install::new();
    assert_eq!(setup.install_from(&binaries()).status.code(), Some(0));
    let repo = setup.root.join("r");
    repository(&repo);
    let registered = setup.operator(&["project", "register", repo.to_str().unwrap()]);
    let registered = parse_strict(text(&registered.stdout).trim()).unwrap();
    let project = string(&registered, "project_id").to_owned();
    let before = setup.ledger(&project);
    assert!(setup.alive());

    // The CLI still under the old name, the daemon already under the new one.
    fs::rename(setup.bin("memprove"), setup.bin("continuity")).unwrap();
    let finished = setup.install_from(&binaries());
    assert_eq!(
        finished.status.code(),
        Some(0),
        "{}",
        text(&finished.stderr)
    );
    assert!(text(&finished.stdout).contains("Renaming the installation in "));
    assert!(setup.bin("memprove").is_file());
    assert!(!setup.bin("continuity").exists());
    assert!(setup.alive(), "a daemon that was running runs afterwards");
    assert_eq!(setup.ledger(&project), before);

    // A binary under both names is not guessed about.
    fs::copy(setup.bin("memprove"), setup.bin("continuity")).unwrap();
    let refused = setup.install_from(&binaries());
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused.stderr).contains("holds both bin/continuity and bin/memprove"),
        "{}",
        text(&refused.stderr)
    );
    assert!(setup.bin("continuity").is_file() && setup.bin("memprove").is_file());
    assert!(setup.alive(), "nothing was changed");
    assert_eq!(setup.ledger(&project), before);
}

/// U20 (N2; found when `~/.continuity` was upgraded on 2026-11-09): a
/// `continuityd` of the previous name that is running when `install.sh`
/// renames the installation is stopped for the rename, and its successor
/// `memproved` runs afterwards, as a daemon that was running does after any
/// upgrade. Before, the rename counted the daemon it had just stopped as not
/// running and left the installation with none.
#[test]
fn u20_n2_a_running_daemon_of_the_previous_name_runs_again_after_the_rename() {
    let setup = Install::new();
    assert_eq!(setup.install_from(&binaries()).status.code(), Some(0));
    let repo = setup.root.join("r");
    repository(&repo);
    let registered = setup.operator(&["project", "register", repo.to_str().unwrap()]);
    let registered = parse_strict(text(&registered.stdout).trim()).unwrap();
    let project = string(&registered, "project_id").to_owned();
    let before = setup.ledger(&project);

    // The installation under the previous name with its daemon running as
    // continuityd, its pid in continuityd.lock. It is started detached, so
    // that once stopped it is not left a zombie of this test.
    stop_recorded_daemon(&setup.data());
    fs::rename(setup.bin("memprove"), setup.bin("continuity")).unwrap();
    fs::rename(setup.bin("memproved"), setup.bin("continuityd")).unwrap();
    let started = setup
        .command("bash")
        .args([
            "-c",
            "nohup \"$1\" --data-dir \"$2\" </dev/null >/dev/null 2>&1 &",
            "bash",
        ])
        .arg(setup.bin("continuityd"))
        .arg(setup.data())
        .status()
        .unwrap();
    assert!(started.success());
    let live = || {
        setup
            .command(setup.bin("continuity"))
            .arg("--data-dir")
            .arg(setup.data())
            .args(["health", "live"])
            .output()
            .unwrap()
            .status
            .success()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !live() {
        assert!(
            std::time::Instant::now() < deadline,
            "continuityd did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let pid = fs::read_to_string(setup.data().join("memproved.lock")).unwrap();
    fs::write(setup.data().join("continuityd.lock"), &pid).unwrap();

    let renamed = setup.install_from(&binaries());
    assert_eq!(renamed.status.code(), Some(0), "{}", text(&renamed.stderr));
    let stdout = text(&renamed.stdout);
    assert!(
        stdout.contains(&format!(
            "stopping the continuityd (pid {}) of the previous name",
            pid.trim()
        )),
        "{stdout}"
    );
    assert!(
        !stdout.contains("was not running before the upgrade"),
        "{stdout}"
    );
    assert!(setup.alive(), "the daemon that was running runs afterwards");
    assert!(!setup.bin("continuityd").exists());
    assert_eq!(setup.ledger(&project), before);
}
