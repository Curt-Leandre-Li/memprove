//! U5 Z-F round 1, S2 (Z-V S2; the Owner's S2 decision, 2026-10-19; ADR
//! 0014, "Amendment (U5, Z-F round 1, 2026-10-19)"): with no `--data-dir`,
//! no `--socket`, and no `MEMPROVE_DATA_DIR`, the CLI reads the data
//! directory from `<config>/data-dir`, and with no `--token-file`,
//! `MEMPROVE_TOKEN`, or `MEMPROVE_TOKEN_FILE` the token from
//! `<config>/token`, as the hook and `mcp serve` do; the configuration
//! directory is `MEMPROVE_CONFIG_DIR`, else the default one. An option or
//! variable that is given still wins, with the same output as before. When
//! the fallback finds nothing, the usage error names the file it read.
//! `doctor` reads the same files.
//!
//! Everything runs in temporary directories with a temporary HOME; the
//! daemons are this test's own.

mod u1e_support;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use u1e_support::{Daemon, bootstrap, cli_binary, isolated, short_tempdir, text};

/// A temporary root with a HOME, a configuration directory, and, when
/// started, a daemon whose control token is the configuration directory's
/// `token`.
struct Place {
    _guard: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

impl Place {
    fn new() -> Self {
        let (guard, root) = short_tempdir();
        let home = root.join("h");
        let config = root.join("c");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _guard: guard,
            data: root.join("d"),
            root,
            home,
            config,
        }
    }

    /// Bootstraps and starts the daemon; its control token becomes
    /// `<config>/token` and its data directory `<config>/data-dir`.
    fn start(&self) -> Daemon {
        let token = self.root.join("operator-token");
        bootstrap(&self.data, &token);
        let daemon = Daemon::start(&self.data, &self.root.join("daemon.log"));
        write_owner_only(
            &self.config.join("token"),
            &fs::read_to_string(&token).unwrap(),
        );
        write_owner_only(
            &self.config.join("data-dir"),
            &format!("{}\n", self.data.display()),
        );
        daemon
    }

    /// `memprove <words>` in the root (not a Git worktree), HOME the
    /// temporary one, `MEMPROVE_CONFIG_DIR` set to `config` when given.
    fn command(&self, config: Option<&Path>, words: &[&str]) -> Command {
        let mut command = isolated(cli_binary());
        command
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .args(words);
        if let Some(config) = config {
            command.env("MEMPROVE_CONFIG_DIR", config);
        }
        command
    }

    fn run(&self, config: Option<&Path>, words: &[&str]) -> Output {
        report(words, self.command(config, words).output().unwrap())
    }
}

fn report(words: &[&str], output: Output) -> Output {
    println!(
        "$ memprove {}\n{}{}[exit {:?}]",
        words.join(" "),
        text(&output.stdout),
        text(&output.stderr),
        output.status.code()
    );
    output
}

fn write_owner_only(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn path_text(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// The first line of standard error.
fn first_line(output: &Output) -> String {
    text(&output.stderr)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

const NO_LOCATION: &str = "memprove: error: no daemon location; pass --data-dir <PATH>, \
                           --socket <PATH>, or set MEMPROVE_DATA_DIR";

#[test]
fn u5_zf1_s2_the_cli_finds_the_daemon_and_the_token_through_the_configuration_directory() {
    let place = Place::new();
    let daemon = place.start();

    // Only MEMPROVE_CONFIG_DIR: the data directory and the control token
    // come from <config>/data-dir and <config>/token.
    let status = place.run(Some(&place.config), &["status"]);
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr));
    assert!(
        text(&status.stdout).contains("\"ready\""),
        "{}",
        text(&status.stdout)
    );

    // The token alone from the configuration directory (before: 401).
    let data = path_text(&place.data).to_owned();
    let status = place.run(Some(&place.config), &["--data-dir", &data, "status"]);
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr));

    // No MEMPROVE_CONFIG_DIR: the default directory of HOME.
    let default = place.home.join(".config/memprove");
    fs::create_dir_all(&default).unwrap();
    for name in ["token", "data-dir"] {
        write_owner_only(
            &default.join(name),
            &fs::read_to_string(place.config.join(name)).unwrap(),
        );
    }
    let status = place.run(None, &["status"]);
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr));

    // A given option or variable still wins, and says what it said before:
    // the same output with the configuration directory as with an empty one.
    let empty = place.root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let nowhere = path_text(&place.root.join("nowhere")).to_owned();
    let missing = place.root.join("missing-token");
    for (words, variables) in [
        (
            vec!["--data-dir", nowhere.as_str(), "health", "live"],
            vec![],
        ),
        (vec!["--socket", nowhere.as_str(), "health", "live"], vec![]),
        (
            vec!["health", "live"],
            vec![("MEMPROVE_DATA_DIR", nowhere.clone())],
        ),
        (
            vec!["--data-dir", data.as_str(), "status"],
            vec![("MEMPROVE_TOKEN_FILE", path_text(&missing).to_owned())],
        ),
        (
            vec!["--data-dir", data.as_str(), "status"],
            vec![("MEMPROVE_TOKEN", "not-a-real-token".to_owned())],
        ),
    ] {
        let outputs = [&place.config, &empty].map(|config| {
            let mut command = place.command(Some(config), &words);
            for (name, value) in &variables {
                command.env(name, value);
            }
            report(&words, command.output().unwrap())
        });
        // The daemon's request_id differs between any two requests.
        let shown = |output: &Output| {
            let stdout = text(&output.stdout);
            let stdout = match stdout.split_once("\"request_id\":\"") {
                Some((before, after)) => {
                    format!(
                        "{before}{}",
                        after.split_once('"').map_or("", |(_, rest)| rest)
                    )
                }
                None => stdout,
            };
            (output.status.code(), stdout, text(&output.stderr))
        };
        assert_eq!(
            shown(&outputs[0]),
            shown(&outputs[1]),
            "{words:?} {variables:?}"
        );
        assert_ne!(outputs[0].status.code(), Some(0), "{words:?} {variables:?}");
    }
    daemon.stop();
}

#[test]
fn u5_zf1_s2_the_usage_error_names_the_configuration_directory_it_read() {
    const SENTINEL: &str = "cty_ZF1S2SENTINEL0123456789abcdefABCDEF012345";
    let place = Place::new();
    write_owner_only(&place.config.join("token"), &format!("{SENTINEL}\n"));
    let config = path_text(&place.config).to_owned();

    // No data-dir file.
    let refused = place.run(Some(&place.config), &["status"]);
    assert_eq!(refused.status.code(), Some(2));
    assert_eq!(
        first_line(&refused),
        format!(
            "{NO_LOCATION}; {config}/data-dir does not exist (the configuration directory is \
             {config}, from MEMPROVE_CONFIG_DIR)"
        )
    );
    assert!(!text(&refused.stderr).contains(SENTINEL));

    // Not one absolute path.
    write_owner_only(&place.config.join("data-dir"), "relative/dir\n");
    let refused = place.run(Some(&place.config), &["status"]);
    assert_eq!(refused.status.code(), Some(2));
    assert_eq!(
        first_line(&refused),
        format!(
            "{NO_LOCATION}; {config}/data-dir is not one absolute path on one line (the \
             configuration directory is {config}, from MEMPROVE_CONFIG_DIR)"
        )
    );

    // The default directory, from HOME.
    let default = place.home.join(".config/memprove");
    let refused = place.run(None, &["status"]);
    assert_eq!(
        first_line(&refused),
        format!(
            "{NO_LOCATION}; {0}/data-dir does not exist (the configuration directory is {0}, \
             from HOME; MEMPROVE_CONFIG_DIR is not set)",
            default.display()
        )
    );

    // No configuration directory at all.
    let mut command = place.command(None, &["status"]);
    command.env_remove("HOME");
    let refused = report(&["status"], command.output().unwrap());
    assert_eq!(
        first_line(&refused),
        format!(
            "{NO_LOCATION}; there is no configuration directory to read a data-dir file from \
             (MEMPROVE_CONFIG_DIR, XDG_CONFIG_HOME, and HOME are all unset)"
        )
    );

    // An empty --data-dir is given, and is refused as before.
    let refused = place.run(Some(&place.config), &["--data-dir", "", "status"]);
    assert_eq!(refused.status.code(), Some(2));
    assert_eq!(first_line(&refused), NO_LOCATION);
}

#[test]
fn u5_zf1_s2_doctor_reads_the_same_files() {
    let place = Place::new();
    let daemon = place.start();
    let config = path_text(&place.config).to_owned();
    let rows = |output: &Output| -> Vec<(String, String, String)> {
        text(&output.stdout)
            .lines()
            .filter_map(|line| {
                let (status, rest) = line.split_once(' ')?;
                let (check, detail) = rest.split_once(':')?;
                Some((
                    status.to_owned(),
                    check.trim().to_owned(),
                    detail.trim().to_owned(),
                ))
            })
            .collect()
    };
    let line = |rows: &[(String, String, String)], check: &str| {
        rows.iter().find(|(_, seen, _)| seen == check).map_or_else(
            || panic!("no {check} line in {rows:?}"),
            |(status, _, detail)| (status.clone(), detail.clone()),
        )
    };

    let doctor = place.run(Some(&place.config), &["doctor"]);
    let found = rows(&doctor);
    let (status, detail) = line(&found, "data-dir");
    assert_eq!(status, "PASS", "{detail}");
    assert_eq!(
        detail,
        format!(
            "{} has mode 0700 (named by {config}/data-dir)",
            place.data.display()
        )
    );
    assert_eq!(line(&found, "daemon").0, "PASS", "{found:?}");
    let (status, detail) = line(&found, "token-file");
    assert_eq!(status, "PASS", "{detail}");
    assert_eq!(
        detail,
        format!("{config}/token has mode 0600 and holds one token")
    );

    fs::remove_file(place.config.join("data-dir")).unwrap();
    fs::remove_file(place.config.join("token")).unwrap();
    let doctor = place.run(Some(&place.config), &["doctor"]);
    assert_eq!(doctor.status.code(), Some(1));
    let found = rows(&doctor);
    let (status, detail) = line(&found, "data-dir");
    assert_eq!(status, "FAIL", "{detail}");
    assert_eq!(
        detail,
        format!(
            "no data directory: pass --data-dir <PATH> or set MEMPROVE_DATA_DIR; \
             {config}/data-dir does not exist (the configuration directory is {config}, from \
             MEMPROVE_CONFIG_DIR)"
        )
    );
    let (status, detail) = line(&found, "token-file");
    assert_eq!(status, "FAIL", "{detail}");
    assert!(
        detail.ends_with(&format!(
            "; {config}/token does not exist (the configuration directory is {config}, from \
             MEMPROVE_CONFIG_DIR)"
        )),
        "{detail}"
    );
    daemon.stop();
}
