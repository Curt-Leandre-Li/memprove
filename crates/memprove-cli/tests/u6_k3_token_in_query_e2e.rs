//! U6 K3 (U5 N4): `resume --pack --query` refuses, with the refusal it gives
//! a query in the format of a memprove token (U5 C4), a query that holds
//! the whole token the CLI itself holds, wherever it holds it from: before
//! anything is sent, and without repeating the query. The token here has no
//! upper-case letter, so only the new check can see it.
//!
//! No daemon is needed: a request that was sent would fail as "cannot reach
//! memproved" (exit 3), which is how the tests tell a refusal from a
//! query that went on.

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Output};

#[path = "u1e_support/mod.rs"]
mod u1e_support;

use u1e_support::{cli_binary, isolated, text};

const ID: &str = "018f47a0-0000-7000-8000-000000000001";
/// 43 characters of `a-z`, `0-9`, and `-`: not in the token format of C4.
const TOKEN: &str = "cty_u6k3-held-token-0123456789-abcdefghijkl";
/// A string in the token format of C4 (43 characters, three classes).
const SHAPED: &str = "Ab3_cDe-4fGh5iJk6lMn7oPq8rSt9uVw0xYz1AbCd2E";

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

struct Place {
    _guard: tempfile::TempDir,
    root: PathBuf,
}

impl Place {
    fn new() -> Self {
        let (guard, root) = short_tempdir();
        let config = root.join("c");
        fs::create_dir_all(&config).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
        for (name, contents) in [
            ("token", format!("{TOKEN}\n")),
            ("data-dir", format!("{}\n", root.join("d").display())),
        ] {
            let path = config.join(name);
            fs::write(&path, contents).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        Self {
            _guard: guard,
            root,
        }
    }

    /// `memprove resume --task --workspace --pack --query <query>`, the
    /// token coming from `<config>/token` (`from_file`) or `MEMPROVE_TOKEN`.
    fn resume(&self, query: &str, source: Source) -> Output {
        let mut command = isolated(cli_binary());
        command
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("MEMPROVE_CONFIG_DIR", self.root.join("c"));
        match source {
            Source::ConfigFile => {}
            Source::Variable(token) => {
                command.env("MEMPROVE_TOKEN", token);
            }
            Source::TokenFile => {
                command.arg("--token-file").arg(self.root.join("c/token"));
            }
        }
        let words = [
            "resume",
            "--task",
            ID,
            "--workspace",
            ID,
            "--pack",
            "--query",
            query,
        ];
        let output = command.args(words).output().unwrap();
        println!(
            "$ memprove resume ... --query <{} bytes>\n{}{}[exit {:?}]",
            query.len(),
            text(&output.stdout),
            text(&output.stderr),
            output.status.code()
        );
        output
    }
}

#[derive(Clone, Copy)]
enum Source {
    ConfigFile,
    Variable(&'static str),
    TokenFile,
}

#[test]
fn u6_k3_a_query_with_the_held_token_is_refused_as_a_token_shaped_one_is() {
    let place = Place::new();
    // The refusal of a query in the C4 format, which is the one to match.
    let shaped = place.resume(SHAPED, Source::ConfigFile);
    assert_eq!(shaped.status.code(), Some(2));
    assert_eq!(text(&shaped.stdout), "");
    assert!(
        text(&shaped.stderr).contains("nothing was sent"),
        "{}",
        text(&shaped.stderr)
    );

    for query in [
        TOKEN.to_owned(),
        format!("{TOKEN} and more words"),
        format!("words in front {TOKEN} and behind"),
        format!("ends with the token {TOKEN}"),
        format!("{TOKEN}{TOKEN}"),
    ] {
        for source in [
            Source::ConfigFile,
            Source::TokenFile,
            Source::Variable(TOKEN),
        ] {
            let output = place.resume(&query, source);
            assert_eq!(output.status.code(), Some(2), "{query}");
            assert_eq!(text(&output.stdout), "");
            // The same words as for a query in the token format, and neither
            // the query nor the token is repeated.
            assert_eq!(text(&output.stderr), text(&shaped.stderr), "{query}");
            assert!(!text(&output.stderr).contains(TOKEN));
            assert!(!text(&output.stderr).contains("cannot reach"));
        }
    }
}

#[test]
fn u6_k3_a_query_without_the_held_token_goes_on() {
    let place = Place::new();
    for query in [
        "a harmless query",
        // Part of the token, and a different token.
        &TOKEN[..20],
        &TOKEN[4..],
        "cty_u6k3-held-token-0123456789-abcdefghijkx",
    ] {
        let output = place.resume(query, Source::ConfigFile);
        // It was sent (to a socket nobody listens on), not refused.
        assert_eq!(output.status.code(), Some(3), "{query}");
        assert!(text(&output.stderr).contains("cannot reach memproved"));
    }
    // A token another source holds is the one that counts: the file's token
    // in the query is no refusal when MEMPROVE_TOKEN holds a different one.
    let output = place.resume(TOKEN, Source::Variable("another-token-value"));
    assert_eq!(output.status.code(), Some(3));
}
