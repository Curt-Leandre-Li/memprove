//! U6 K1 (U5 ZR1): when no token source gives a token, the CLI does not send
//! the request and take the daemon's bare `401`; it writes on standard error
//! which variables and files it looked at (never a file's contents), sends
//! nothing, and exits as a `401` does (`1`; `125` for `run`, which does not
//! run its command either). The two health operations need no token and still
//! work. Everything shares one path: every command that reaches the daemon
//! with a token goes through it.
//!
//! Everything runs in temporary directories with a temporary HOME. No daemon
//! is needed: the refusal comes before any request, and the anonymous
//! operations are shown to still try the socket.

mod u1e_support;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use u1e_support::{cli_binary, isolated, text};

/// A short temporary directory (the socket path holds at most 103 bytes):
/// under `TMPDIR` when that is short, else under `/tmp`.
fn short_tempdir() -> (tempfile::TempDir, PathBuf) {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .filter(|base| base.is_absolute() && base.as_os_str().len() <= 24)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let dir = tempfile::Builder::new()
        .prefix("u6")
        .tempdir_in(base)
        .unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

const ID: &str = "018f47a0-0000-7000-8000-000000000001";

struct Place {
    _guard: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

impl Place {
    /// A root with a HOME and a configuration directory that holds a
    /// `data-dir` file (naming a directory with no daemon) but no `token`
    /// file.
    fn new() -> Self {
        let (guard, root) = short_tempdir();
        let home = root.join("h");
        let config = root.join("c");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
        let data = root.join("d");
        fs::write(config.join("data-dir"), format!("{}\n", data.display())).unwrap();
        fs::set_permissions(config.join("data-dir"), fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            _guard: guard,
            root,
            home,
            config,
            data,
        }
    }

    fn command(&self, words: &[&str]) -> Command {
        let mut command = isolated(cli_binary());
        command
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("MEMPROVE_CONFIG_DIR", &self.config)
            .args(words);
        command
    }

    fn run(&self, words: &[&str]) -> Output {
        report(words, self.command(words).output().unwrap())
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

/// What the refusal says, for a configuration directory `config`.
fn expected(config: &Path) -> String {
    let config = config.display();
    format!(
        "memprove: error: no token: --token-file was not given; MEMPROVE_TOKEN is not set \
         or is empty; MEMPROVE_TOKEN_FILE is not set or is empty; {config}/token does not \
         exist (the configuration directory is {config}, from MEMPROVE_CONFIG_DIR); nothing \
         was sent. Give one with --token-file <PATH>, MEMPROVE_TOKEN, or MEMPROVE_TOKEN_FILE, \
         or put it in {config}/token (mode 0600)"
    )
}

fn assert_refused(output: &Output, config: &Path, code: i32) {
    assert_eq!(output.status.code(), Some(code), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "", "nothing is printed on stdout");
    assert_eq!(text(&output.stderr).trim_end(), expected(config));
}

#[test]
fn u6_k1_every_command_that_needs_a_token_names_the_places_it_looked() {
    let place = Place::new();
    let marker = place.root.join("ran");
    let marker_text = marker.to_str().unwrap().to_owned();

    for words in [
        vec!["status"],
        vec!["version"],
        vec!["token", "list"],
        vec!["project", "get", ID],
        vec!["session", "list", "--workspace", ID],
        vec!["task", "list", "--project", ID],
        vec!["ledger", "verify", "--project", ID],
        vec!["resume", "--task", ID, "--workspace", ID, "--pack"],
        vec!["begin", "--project", ID, "--new-task", "A task"],
    ] {
        let output = place.run(&words);
        assert_refused(&output, &place.config, 1);
    }

    // `run` neither runs its command nor records anything.
    let words = [
        "run",
        "--session",
        ID,
        "--workspace",
        ID,
        "--task",
        ID,
        "--",
        "touch",
        &marker_text,
    ];
    let output = place.run(&words);
    assert_refused(&output, &place.config, 125);
    assert!(!marker.exists(), "the command was run");

    // The health operations are anonymous: they are sent (here to a socket
    // nobody listens on) without a token, and say so as a transport failure.
    for words in [&["health", "live"][..], &["health", "ready"][..]] {
        let output = place.run(words);
        assert_eq!(output.status.code(), Some(3), "{}", text(&output.stderr));
        assert!(!text(&output.stderr).contains("no token"));
        assert!(text(&output.stderr).contains("memproved.sock"));
    }

    // A token that is given is sent, not judged here.
    let given = place
        .command(&["status"])
        .env("MEMPROVE_TOKEN", "not-a-real-token")
        .output()
        .unwrap();
    assert!(!text(&given.stderr).contains("no token"));
    assert!(text(&given.stderr).contains("memproved.sock"));
}

#[test]
fn u6_k1_the_refusal_names_the_default_directory_and_never_prints_a_file() {
    const SENTINEL: &str = "cty_U6K1SENTINEL0123456789abcdefABCDEF012345";
    let place = Place::new();
    // The data directory is given; no configuration directory is named, so
    // the default one of HOME is read; there is no token file in it.
    let default = place.home.join(".config/memprove");
    fs::create_dir_all(&default).unwrap();
    let data = place.data.to_str().unwrap().to_owned();
    let mut command = isolated(cli_binary());
    command
        .current_dir(&place.root)
        .env("HOME", &place.home)
        .args(["--data-dir", &data, "status"]);
    let output = report(&["status"], command.output().unwrap());
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    let shown = default.display();
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "{shown}/token does not exist (the configuration directory is {shown}, from HOME; \
             MEMPROVE_CONFIG_DIR is not set)"
        )),
        "{stderr}"
    );

    // No configuration directory at all.
    let mut command = isolated(cli_binary());
    command
        .current_dir(&place.root)
        .env_remove("HOME")
        .args(["--data-dir", &data, "status"]);
    let output = report(&["status"], command.output().unwrap());
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains(
            "there is no configuration directory to read a token file from \
             (MEMPROVE_CONFIG_DIR, XDG_CONFIG_HOME, and HOME are all unset)"
        ),
        "{}",
        text(&output.stderr)
    );

    // A file that is there is read, and what it holds is never printed: an
    // unusable one keeps the message it had before K1.
    let token = place.config.join("token");
    fs::write(&token, format!("{SENTINEL}\n")).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
    let output = place.run(&["status"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("chmod 600"));
    assert!(!text(&output.stderr).contains(SENTINEL));
    assert!(!text(&output.stdout).contains(SENTINEL));
}
