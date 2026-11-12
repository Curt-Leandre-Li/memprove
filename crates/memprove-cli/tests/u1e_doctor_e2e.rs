//! U1 (E3, ADR 0015): `memprove doctor` with the real binaries: one
//! `PASS`, `WARN`, or `FAIL` line per check, exit 1 exactly when a check
//! fails, `--json` with the same checks, nothing written, and never the
//! token's text. The failures the spec names: a token file of mode 0644, a
//! stopped daemon, and a CLI of another build (a copy of this CLI whose
//! compiled-in build identity is replaced, and re-signed on macOS).

mod u1e_support;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Output,
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use u1e_support::{
    Daemon, bootstrap, cli_binary, cli_in, isolated, json, other_build, repository, short_tempdir,
    string, text,
};

struct Setup {
    _guard: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    control: PathBuf,
    agent: PathBuf,
    config: PathBuf,
    repo: PathBuf,
    project: String,
}

impl Setup {
    fn new() -> (Self, Daemon) {
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
        let registered = json(&cli_in(
            &root,
            &data,
            &control,
            &["project", "register", repo.to_str().unwrap()],
        ));
        let project = string(&registered, "project_id").to_owned();
        let created = json(&cli_in(
            &root,
            &data,
            &control,
            &[
                "token",
                "create",
                "--label",
                "agent",
                "--scope",
                "mcp-read",
                "--scope",
                "mcp-write",
            ],
        ));
        let agent = config.join("token");
        fs::write(&agent, format!("{}\n", string(&created, "secret"))).unwrap();
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o600)).unwrap();
        (
            Self {
                _guard: guard,
                root,
                data,
                control,
                agent,
                config,
                repo,
                project,
            },
            daemon,
        )
    }

    /// `doctor` (with `extra`) run by `binary` in `cwd`, configured as an
    /// installed agent is: data directory, configuration directory, and the
    /// agent's token file in the environment.
    /// U7 P3: with the directory of `binary` on `PATH`, so that `doctor`'s
    /// `path` line passes.
    fn doctor_with(&self, binary: &Path, cwd: &Path, extra: &[&str]) -> Output {
        let path = std::env::join_paths(
            std::iter::once(
                fs::canonicalize(binary)
                    .unwrap()
                    .parent()
                    .unwrap()
                    .to_path_buf(),
            )
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            )),
        )
        .unwrap();
        isolated(binary)
            .current_dir(cwd)
            .env("PATH", path)
            .env("HOME", &self.root)
            .env("MEMPROVE_DATA_DIR", &self.data)
            .env("MEMPROVE_CONFIG_DIR", &self.config)
            .env("MEMPROVE_TOKEN_FILE", &self.agent)
            .arg("doctor")
            .args(extra)
            .output()
            .unwrap()
    }

    fn doctor(&self, cwd: &Path, extra: &[&str]) -> Output {
        self.doctor_with(&cli_binary(), cwd, extra)
    }

    fn agent(&self, words: &[&str]) -> Output {
        isolated(cli_binary())
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("MEMPROVE_DATA_DIR", &self.data)
            .env("MEMPROVE_CONFIG_DIR", &self.config)
            .env("MEMPROVE_TOKEN_FILE", &self.agent)
            .args(words)
            .output()
            .unwrap()
    }

    /// No output of `doctor` holds a token's text.
    fn assert_no_secret(&self, output: &Output) {
        for file in [&self.agent, &self.control] {
            let secret = fs::read_to_string(file).unwrap();
            let secret = secret.trim();
            assert!(!text(&output.stdout).contains(secret));
            assert!(!text(&output.stderr).contains(secret));
        }
    }
}

/// The `(status, check)` of each line before the summary, after checking
/// the summary line counts them.
fn lines(output: &Output) -> Vec<(String, String)> {
    let stdout = text(&output.stdout);
    println!(
        "$ memprove doctor\n{stdout}[exit {:?}]",
        output.status.code()
    );
    let mut rows: Vec<&str> = stdout.lines().collect();
    let summary = rows.pop().unwrap_or_default();
    let parsed: Vec<(String, String)> = rows
        .iter()
        .map(|line| {
            let mut parts = line.split_whitespace();
            let status = parts.next().unwrap().to_owned();
            let check = parts.next().unwrap().trim_end_matches(':').to_owned();
            assert!(
                ["PASS", "WARN", "FAIL"].contains(&status.as_str()),
                "{line}"
            );
            (status, check)
        })
        .collect();
    let count = |status: &str| parsed.iter().filter(|(seen, _)| seen == status).count();
    assert_eq!(
        summary,
        format!(
            "memprove doctor: {} PASS, {} WARN, {} FAIL",
            count("PASS"),
            count("WARN"),
            count("FAIL")
        ),
        "{stdout}"
    );
    parsed
}

fn status_of<'a>(rows: &'a [(String, String)], check: &str) -> &'a str {
    rows.iter().find(|(_, seen)| seen == check).map_or_else(
        || panic!("no {check} line in {rows:?}"),
        |(status, _)| status.as_str(),
    )
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered scenario.
fn doctor_passes_a_sound_setup_and_fails_each_broken_part() {
    let (setup, daemon) = Setup::new();

    // Outside every Workspace: every check passes.
    let sound = setup.doctor(&setup.root, &[]);
    setup.assert_no_secret(&sound);
    let rows = lines(&sound);
    assert_eq!(sound.status.code(), Some(0), "{}", text(&sound.stdout));
    assert!(sound.stderr.is_empty(), "{}", text(&sound.stderr));
    for check in [
        "data-dir",
        "socket",
        "daemon",
        "token-file",
        "token",
        "build",
        "config-dir",
        "guide",
        "workspace",
    ] {
        assert_eq!(status_of(&rows, check), "PASS", "{}", text(&sound.stdout));
    }
    assert!(rows.iter().all(|(status, _)| status == "PASS"), "{rows:?}");

    // --json: the same checks.
    let report = parse_strict(text(&setup.doctor(&setup.root, &["--json"]).stdout).trim()).unwrap();
    let checks = report.get("checks").and_then(JsonValue::as_array).unwrap();
    assert_eq!(checks.len(), rows.len());
    for (check, (status, name)) in checks.iter().zip(&rows) {
        assert_eq!(string(check, "check"), name);
        assert_eq!(string(check, "status"), status);
        assert!(!string(check, "detail").is_empty());
    }
    assert_eq!(report.get("fail").and_then(JsonValue::as_i64), Some(0));

    // In the Workspace, after `begin`: its context and its ledger pass; no
    // hook is configured yet, a warning only.
    let begun = setup.agent(&["begin", "--project", &setup.project, "--new-task", "Doctor"]);
    assert_eq!(begun.status.code(), Some(0), "{}", text(&begun.stderr));
    let inside = setup.doctor(&setup.repo, &[]);
    let rows = lines(&inside);
    assert_eq!(inside.status.code(), Some(0), "{}", text(&inside.stdout));
    assert_eq!(
        status_of(&rows, "workspace"),
        "PASS",
        "{}",
        text(&inside.stdout)
    );
    assert_eq!(
        status_of(&rows, "ledger"),
        "PASS",
        "{}",
        text(&inside.stdout)
    );
    assert_eq!(status_of(&rows, "hook"), "WARN", "{}", text(&inside.stdout));
    assert!(text(&inside.stdout).contains(&setup.project));

    // A SessionStart hook that runs a binary that is not there fails.
    let settings = setup.repo.join(".claude/settings.local.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"/nowhere/memprove","args":["hook","session-start"],"timeout":10}]}]}}"#,
    )
    .unwrap();
    let missing = setup.doctor(&setup.repo, &[]);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(status_of(&lines(&missing), "hook"), "FAIL");
    assert!(text(&missing.stdout).contains("/nowhere/memprove"));
    // ... and one that runs this CLI passes, with (U5 P3) the settings'
    // env.MEMPROVE_CONFIG_DIR the configuration directory doctor uses
    // (without it the line is a WARN).
    fs::write(
        &settings,
        format!(
            r#"{{"env":{{"MEMPROVE_CONFIG_DIR":"{}"}},"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}","args":["hook","session-start"],"timeout":10}}]}}]}}}}"#,
            setup.config.display(),
            cli_binary().display()
        ),
    )
    .unwrap();
    let present = setup.doctor(&setup.repo, &[]);
    assert_eq!(
        status_of(&lines(&present), "hook"),
        "PASS",
        "{}",
        text(&present.stdout)
    );
    fs::remove_dir_all(setup.repo.join(".claude")).unwrap();

    // A token file of mode 0644 fails, and the token is never shown.
    fs::set_permissions(&setup.agent, fs::Permissions::from_mode(0o644)).unwrap();
    let open = setup.doctor(&setup.root, &[]);
    setup.assert_no_secret(&open);
    assert_eq!(open.status.code(), Some(1));
    assert_eq!(status_of(&lines(&open), "token-file"), "FAIL");
    assert!(
        text(&open.stdout).contains("0644"),
        "{}",
        text(&open.stdout)
    );
    fs::set_permissions(&setup.agent, fs::Permissions::from_mode(0o600)).unwrap();

    // A CLI of another build fails the build check.
    let other = other_build(&setup.root);
    let mismatch = setup.doctor_with(&other, &setup.root, &[]);
    assert_eq!(
        mismatch.status.code(),
        Some(1),
        "{}",
        text(&mismatch.stdout)
    );
    let rows = lines(&mismatch);
    assert_eq!(
        status_of(&rows, "build"),
        "FAIL",
        "{}",
        text(&mismatch.stdout)
    );
    assert_eq!(status_of(&rows, "daemon"), "PASS");

    // A stopped daemon fails.
    daemon.stop();
    let stopped = setup.doctor(&setup.root, &[]);
    assert_eq!(stopped.status.code(), Some(1));
    let rows = lines(&stopped);
    assert_eq!(
        status_of(&rows, "daemon"),
        "FAIL",
        "{}",
        text(&stopped.stdout)
    );
    assert_eq!(
        status_of(&rows, "socket"),
        "FAIL",
        "{}",
        text(&stopped.stdout)
    );
    let report = parse_strict(text(&setup.doctor(&setup.root, &["--json"]).stdout).trim()).unwrap();
    assert!(report.get("fail").and_then(JsonValue::as_i64) >= Some(1));
}

#[test]
fn doctor_writes_nothing_and_takes_only_json() {
    let (setup, daemon) = Setup::new();
    let begun = setup.agent(&["begin", "--project", &setup.project, "--new-task", "Doctor"]);
    assert_eq!(begun.status.code(), Some(0), "{}", text(&begun.stderr));
    let before = snapshot(&setup.root);
    let _ = setup.doctor(&setup.repo, &[]);
    let _ = setup.doctor(&setup.repo, &["--json"]);
    assert_eq!(snapshot(&setup.root), before, "doctor changed a file");
    let wrong = setup.doctor(&setup.root, &["--out", "/x"]);
    assert_eq!(wrong.status.code(), Some(2));
    daemon.stop();
}

/// Every file below `root` but the daemon's log, socket, and lock, with
/// its bytes.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                pending.push(path);
            } else if !["log", "memproved.sock", "memproved.lock"].contains(&name.as_str()) {
                files.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    files.sort();
    files
}

/// A `memproved` started under `umask 022` (through `sh`), killed when
/// dropped.
struct Umask022(std::process::Child);

impl Drop for Umask022 {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// U2 (S2, N-5, original #131; ADR 0015, "Amendment (U2, W-B)"): the
/// daemon creates `projects/` and `projects/<PROJECT_ID>/` with mode 0700
/// whatever its umask, and `doctor` has one `projects-dir` line, right
/// after `data-dir`, that fails when a Project directory is open to group
/// or others.
#[test]
fn u2_s2_project_directories_are_0700_and_doctor_checks_them() {
    let (_guard, root) = short_tempdir();
    let data = root.join("d");
    let control = root.join("c");
    let repo = root.join("r");
    repository(&repo);
    bootstrap(&data, &control);
    let log = fs::File::create(root.join("log")).unwrap();
    let daemon = Umask022(
        isolated("/bin/sh")
            .args(["-c", "umask 022 && exec \"$0\" --data-dir \"$1\""])
            .arg(u1e_support::daemon_binary())
            .arg(&data)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + u1e_support::TIMEOUT;
    while !cli_in(&root, &data, &control, &["health", "ready"])
        .status
        .success()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "memproved never became ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let registered = json(&cli_in(
        &root,
        &data,
        &control,
        &["project", "register", repo.to_str().unwrap()],
    ));
    let project = string(&registered, "project_id").to_owned();
    let projects = data.join("projects");
    let directory = projects.join(&project);
    assert_eq!(
        u1e_support::mode(&projects),
        0o700,
        "projects/ under umask 022"
    );
    assert_eq!(
        u1e_support::mode(&directory),
        0o700,
        "projects/<ID>/ under umask 022"
    );

    let doctor = || {
        isolated(cli_binary())
            .current_dir(&root)
            .env("HOME", &root)
            .env("MEMPROVE_DATA_DIR", &data)
            .env("MEMPROVE_TOKEN_FILE", &control)
            .arg("doctor")
            .output()
            .unwrap()
    };
    let sound = doctor();
    let rows = lines(&sound);
    assert_eq!(rows[0].1, "data-dir");
    assert_eq!(rows[1], ("PASS".to_owned(), "projects-dir".to_owned()));
    assert_eq!(
        rows.iter()
            .filter(|(_, check)| check == "projects-dir")
            .count(),
        1
    );

    // A Project directory group can enter fails; so does projects/ itself.
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o750)).unwrap();
    let open = doctor();
    assert_eq!(open.status.code(), Some(1), "{}", text(&open.stdout));
    assert_eq!(status_of(&lines(&open), "projects-dir"), "FAIL");
    assert!(
        text(&open.stdout).contains(&format!("{} has mode 0750", directory.display())),
        "{}",
        text(&open.stdout)
    );
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&projects, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(status_of(&lines(&doctor()), "projects-dir"), "FAIL");
    fs::set_permissions(&projects, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(status_of(&lines(&doctor()), "projects-dir"), "PASS");
    drop(daemon);
}

/// U3 (K6, V-N-7, original #69; ADR 0015, "Amendment (U3, X-K)"):
/// memproved creates `tmp/` of the data directory with mode 0700 whatever
/// its umask, and `doctor`'s one `projects-dir` line checks `tmp/` too: it
/// names `tmp/` when it passes, and fails when group or others can enter
/// it.
#[test]
fn u3_k6_tmp_is_0700_and_doctor_checks_it() {
    let (_guard, root) = short_tempdir();
    let data = root.join("d");
    let control = root.join("c");
    let repo = root.join("r");
    repository(&repo);
    bootstrap(&data, &control);
    let log = fs::File::create(root.join("log")).unwrap();
    let daemon = Umask022(
        isolated("/bin/sh")
            .args(["-c", "umask 022 && exec \"$0\" --data-dir \"$1\""])
            .arg(u1e_support::daemon_binary())
            .arg(&data)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + u1e_support::TIMEOUT;
    while !cli_in(&root, &data, &control, &["health", "ready"])
        .status
        .success()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "memproved never became ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let tmp = data.join("tmp");
    let projects = data.join("projects");
    assert_eq!(u1e_support::mode(&tmp), 0o700, "tmp/ under umask 022");
    let doctor = || {
        isolated(cli_binary())
            .current_dir(&root)
            .env("HOME", &root)
            .env("MEMPROVE_DATA_DIR", &data)
            .env("MEMPROVE_TOKEN_FILE", &control)
            .arg("doctor")
            .output()
            .unwrap()
    };
    let line = |output: &Output| {
        text(&output.stdout)
            .lines()
            .find(|line| line.contains(" projects-dir: "))
            .unwrap_or_default()
            .to_owned()
    };

    // No Project yet: one line, naming tmp/.
    let none = doctor();
    assert_eq!(
        line(&none),
        format!(
            "PASS projects-dir: {} does not exist yet: no Project is registered; {} has mode \
             0700 or stricter",
            projects.display(),
            tmp.display()
        )
    );
    let registered = json(&cli_in(
        &root,
        &data,
        &control,
        &["project", "register", repo.to_str().unwrap()],
    ));
    string(&registered, "project_id");
    let sound = doctor();
    assert_eq!(sound.status.code(), Some(0), "{}", text(&sound.stdout));
    assert_eq!(
        line(&sound),
        format!(
            "PASS projects-dir: {} and its 1 Project directory, and {}, have mode 0700 or \
             stricter",
            projects.display(),
            tmp.display()
        )
    );
    assert_eq!(
        text(&sound.stdout)
            .lines()
            .filter(|line| line.contains("projects-dir"))
            .count(),
        1
    );

    // tmp/ that group or others can enter fails.
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
    let open = doctor();
    assert_eq!(open.status.code(), Some(1), "{}", text(&open.stdout));
    assert_eq!(
        line(&open),
        format!(
            "FAIL projects-dir: {} has mode 0755; group or others can enter it (chmod 700)",
            tmp.display()
        )
    );
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(status_of(&lines(&doctor()), "projects-dir"), "PASS");
    drop(daemon);
}
