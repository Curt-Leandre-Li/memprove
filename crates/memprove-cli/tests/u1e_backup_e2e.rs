//! U1 (E4, ADR 0015): offline `memprove backup --out <dir>` and
//! `memprove restore --from <dir> --data-dir <empty dir>` with the real
//! binaries: refused while the daemon runs, `SHA256SUMS` over every file,
//! no socket, lock, or `tmp/` in the backup, the manifest checked before
//! anything is written, a non-empty target refused, and the modes and the
//! ledger (validity and chain head) the same after a restore.

mod u1e_support;

use std::{
    collections::BTreeMap, fmt::Write as _, fs, os::unix::fs::PermissionsExt as _, path::Path,
};

use sha2::{Digest, Sha256};
use u1e_support::{
    Daemon, bootstrap, cli_binary, cli_in, isolated, json, ledger, mode, repository, short_tempdir,
    string, text,
};

fn memprove(words: &[&str]) -> std::process::Output {
    isolated(cli_binary()).args(words).output().unwrap()
}

fn sha256_hex(path: &Path) -> String {
    Sha256::digest(fs::read(path).unwrap())
        .iter()
        .fold(String::new(), |mut hex, byte| {
            write!(hex, "{byte:02x}").unwrap();
            hex
        })
}

/// Every entry below `root`, relative, with its mode (and, for a file, its
/// SHA-256).
fn tree(root: &Path) -> BTreeMap<String, (u32, Option<String>)> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, (u32, Option<String>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            if path.is_dir() {
                out.insert(relative, (mode(&path), None));
                walk(root, &path, out);
            } else {
                out.insert(relative, (mode(&path), Some(sha256_hex(&path))));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered scenario.
fn backup_and_restore_keep_the_ledger_and_refuse_what_they_must() {
    let (_guard, root) = short_tempdir();
    let data = root.join("d");
    let token = root.join("t");
    let repo = root.join("r");
    repository(&repo);
    bootstrap(&data, &token);
    let daemon = Daemon::start(&data, &root.join("log1"));
    let registered = json(&cli_in(
        &repo,
        &data,
        &token,
        &["project", "register", repo.to_str().unwrap()],
    ));
    let project = string(&registered, "project_id").to_owned();
    let before = ledger(&data, &token, &project);
    assert!(before.0);

    // While the daemon runs, backup is refused and writes nothing.
    let out = root.join("b");
    let refused = memprove(&[
        "--data-dir",
        data.to_str().unwrap(),
        "backup",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr));
    assert!(
        text(&refused.stderr).contains("memproved is running"),
        "{}",
        text(&refused.stderr)
    );
    assert!(!out.exists());

    daemon.stop();
    fs::write(data.join("tmp/scratch"), "temporary\n").unwrap();
    let source = tree(&data);

    // A backup inside the data directory is refused.
    let inside = memprove(&[
        "--data-dir",
        data.to_str().unwrap(),
        "backup",
        "--out",
        data.join("projects/b").to_str().unwrap(),
    ]);
    assert_eq!(inside.status.code(), Some(1));
    assert!(text(&inside.stderr).contains("inside the data directory"));

    let backed = memprove(&[
        "--data-dir",
        data.to_str().unwrap(),
        "backup",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(backed.status.code(), Some(0), "{}", text(&backed.stderr));
    assert_eq!(mode(&out), 0o700);
    for left_out in ["memproved.sock", "memproved.lock", "tmp"] {
        assert!(!out.join(left_out).exists(), "{left_out} was backed up");
    }
    assert!(out.join("registry.sqlite").is_file());
    assert!(
        out.join(format!("projects/{project}/state.sqlite"))
            .is_file()
    );
    // SHA256SUMS: `<sha256>  <path>` for every other file, in path order.
    let sums = fs::read_to_string(out.join("SHA256SUMS")).unwrap();
    let mut listed = Vec::new();
    for line in sums.lines() {
        let (digest, path) = line.split_once("  ").unwrap();
        assert_eq!(sha256_hex(&out.join(path)), digest, "{path}");
        listed.push(path.to_owned());
    }
    let mut sorted = listed.clone();
    sorted.sort();
    assert_eq!(listed, sorted);
    let backed_files: Vec<String> = tree(&out)
        .into_iter()
        .filter(|(path, (_, digest))| digest.is_some() && path != "SHA256SUMS")
        .map(|(path, _)| path)
        .collect();
    assert_eq!(listed, backed_files);
    assert!(listed.contains(&"continuity-backup.txt".to_owned()));
    // The backup holds the same entries with the same modes and contents.
    for (path, entry) in &source {
        if path.starts_with("tmp") || path.starts_with("memproved.") {
            continue;
        }
        assert_eq!(tree(&out).get(path), Some(entry), "{path}");
    }

    // A second backup into the same, now non-empty, directory is refused.
    let again = memprove(&[
        "--data-dir",
        data.to_str().unwrap(),
        "backup",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(again.status.code(), Some(1));
    assert!(text(&again.stderr).contains("is not empty"));

    // Restore into a non-empty directory is refused and changes nothing.
    let busy = root.join("busy");
    fs::create_dir(&busy).unwrap();
    fs::write(busy.join("keep"), "mine\n").unwrap();
    let refused = memprove(&[
        "restore",
        "--from",
        out.to_str().unwrap(),
        "--data-dir",
        busy.to_str().unwrap(),
    ]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(text(&refused.stderr).contains("is not empty"));
    assert_eq!(fs::read_to_string(busy.join("keep")).unwrap(), "mine\n");
    assert_eq!(fs::read_dir(&busy).unwrap().count(), 1);

    // Restore into the emptied data directory: same entries, modes, bytes.
    for entry in fs::read_dir(&data).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            fs::remove_dir_all(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
    }
    let restored = memprove(&[
        "restore",
        "--from",
        out.to_str().unwrap(),
        "--data-dir",
        data.to_str().unwrap(),
    ]);
    assert_eq!(
        restored.status.code(),
        Some(0),
        "{}",
        text(&restored.stderr)
    );
    assert_eq!(mode(&data), 0o700);
    let after = tree(&data);
    for (path, entry) in &source {
        if path.starts_with("tmp") || path.starts_with("memproved.") {
            continue;
        }
        assert_eq!(after.get(path), Some(entry), "{path}");
    }
    assert!(after.keys().all(|path| source.contains_key(path)));

    // The daemon runs on the restored directory; the ledger is the same.
    let daemon = Daemon::start(&data, &root.join("log2"));
    assert_eq!(ledger(&data, &token, &project), before);
    daemon.stop();

    // A changed byte in the backup is found before anything is written.
    let damaged = root.join("damaged");
    let target = root.join("new");
    copy_tree(&out, &damaged);
    let registry = damaged.join("registry.sqlite");
    let mut bytes = fs::read(&registry).unwrap();
    bytes[100] ^= 1;
    fs::write(&registry, bytes).unwrap();
    let refused = memprove(&[
        "restore",
        "--from",
        damaged.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
    ]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(text(&refused.stderr).contains("does not match its SHA-256"));
    assert!(!target.exists(), "nothing is written before the check");

    // So is a file the checksums do not list.
    let extra = root.join("extra");
    copy_tree(&out, &extra);
    fs::write(extra.join("projects/stray"), "x\n").unwrap();
    let refused = memprove(&[
        "restore",
        "--from",
        extra.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
    ]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(text(&refused.stderr).contains("not in SHA256SUMS"));
    assert!(!target.exists());
}

/// `memprove` in `cwd` on `data_dir` with the token `token` and the
/// configuration directory `config`.
fn agent_in(
    cwd: &Path,
    data_dir: &Path,
    token: &Path,
    config: &Path,
    words: &[&str],
) -> std::process::Output {
    isolated(cli_binary())
        .current_dir(cwd)
        .env("MEMPROVE_CONFIG_DIR", config)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--token-file")
        .arg(token)
        .args(words)
        .output()
        .unwrap()
}

/// U1 U-F (SF-2): a restore into another directory, named by a relative
/// path through a symbolically linked parent, gives a data directory a daemon serves:
/// `restore` binds each Project's state database to the new directory, so
/// `ledger verify` is valid with the same chain head and the Task's Capsule
/// and Preview read as before the backup.
#[test]
fn a_restore_into_another_directory_is_bound_to_it() {
    let (_guard, root) = short_tempdir();
    let data = root.join("d");
    let token = root.join("t");
    let config = root.join("c");
    let repo = root.join("r");
    repository(&repo);
    bootstrap(&data, &token);
    let daemon = Daemon::start(&data, &root.join("log1"));
    let registered = json(&cli_in(
        &repo,
        &data,
        &token,
        &["project", "register", repo.to_str().unwrap()],
    ));
    let project = string(&registered, "project_id").to_owned();
    let agent = |data: &Path, words: &[&str]| agent_in(&repo, data, &token, &config, words);
    let begun = json(&agent(
        &data,
        &["begin", "--project", &project, "--new-task", "u1 sf2"],
    ));
    let task = string(&begun, "task_id").to_owned();
    let ran = agent(
        &data,
        &[
            "run",
            "--test-format",
            "cargo",
            "--",
            "sh",
            "tests/check.sh",
        ],
    );
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    json(&agent(
        &data,
        &[
            "checkpoint",
            "create",
            "--fact",
            "check passes",
            "--evidence",
            "r1",
            "--next",
            "restore elsewhere",
        ],
    ));
    json(&agent(&data, &["end"]));
    let before = ledger(&data, &token, &project);
    assert!(before.0);
    let capsule = text(&agent(&data, &["resume", "--task", &task]).stdout);
    let preview = text(&agent(&data, &["recover", "preview", "--task", &task]).stdout);
    assert!(capsule.contains("check passes"), "{capsule}");
    daemon.stop();

    let out = root.join("b");
    let backed = memprove(&[
        "--data-dir",
        data.to_str().unwrap(),
        "backup",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(backed.status.code(), Some(0), "{}", text(&backed.stderr));

    // The target: `ln/n`, relative to `root`, where `ln` is a link to
    // `real`; the new directory is `real/n`. (A link as the last component
    // is refused, as before.)
    fs::create_dir(root.join("real")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("ln")).unwrap();
    let moved = root.join("real/n");
    let restored = isolated(cli_binary())
        .current_dir(&root)
        .args([
            "restore",
            "--from",
            out.to_str().unwrap(),
            "--data-dir",
            "ln/n",
        ])
        .output()
        .unwrap();
    assert_eq!(
        restored.status.code(),
        Some(0),
        "{}",
        text(&restored.stderr)
    );
    assert!(moved.join("registry.sqlite").is_file());

    let daemon = Daemon::start(&moved, &root.join("log2"));
    assert_eq!(ledger(&moved, &token, &project), before);
    let read = agent(&moved, &["resume", "--task", &task]);
    assert_eq!(read.status.code(), Some(0), "{}", text(&read.stderr));
    assert_eq!(text(&read.stdout), capsule);
    let read = agent(&moved, &["recover", "preview", "--task", &task]);
    assert_eq!(read.status.code(), Some(0), "{}", text(&read.stderr));
    assert_eq!(text(&read.stdout), preview);
    daemon.stop();
}

/// A copy of the backup `out` at `to` whose `registry.sqlite` holds
/// `registry` instead, with `SHA256SUMS` recomputed, so that it verifies.
fn backup_with_registry(out: &Path, to: &Path, registry: &[u8]) {
    copy_tree(out, to);
    fs::write(to.join("registry.sqlite"), registry).unwrap();
    let digest = sha256_hex(&to.join("registry.sqlite"));
    let sums = fs::read_to_string(to.join("SHA256SUMS"))
        .unwrap()
        .lines()
        .map(|line| match line.split_once("  ") {
            Some((_, "registry.sqlite")) => format!("{digest}  registry.sqlite\n"),
            _ => format!("{line}\n"),
        })
        .collect::<String>();
    fs::write(to.join("SHA256SUMS"), sums).unwrap();
}

/// U1 U-F (SF-2R, the re-check's N-2): when binding the restored registry
/// fails (the backup verifies, but its `registry.sqlite` is not
/// a database at all, or is one without a `projects` table), `restore` leaves the
/// target exactly as it was: a directory it created is gone, its created
/// parent too; an empty directory that existed is empty again, with its
/// mode, so a retry into it with a good backup succeeds.
#[test]
fn a_restore_that_cannot_bind_leaves_the_target_as_it_was() {
    let (_guard, root) = short_tempdir();
    let data = root.join("d");
    let token = root.join("t");
    bootstrap(&data, &token);
    let out = root.join("b");
    let backed = memprove(&[
        "--data-dir",
        data.to_str().unwrap(),
        "backup",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(backed.status.code(), Some(0), "{}", text(&backed.stderr));
    let empty = root.join("e");
    fs::create_dir(&empty).unwrap();
    fs::set_permissions(&empty, fs::Permissions::from_mode(0o750)).unwrap();
    for (name, registry) in [
        (
            "not-sqlite",
            &b"this is not a SQLite database, not at all\n"[..],
        ),
        ("no-projects", &b""[..]),
    ] {
        let bad = root.join(name);
        backup_with_registry(&out, &bad, registry);
        let created = root.join("p").join("n");
        for (target, existed) in [(&created, false), (&empty, true)] {
            let refused = memprove(&[
                "restore",
                "--from",
                bad.to_str().unwrap(),
                "--data-dir",
                target.to_str().unwrap(),
            ]);
            let stderr = text(&refused.stderr);
            assert_eq!(refused.status.code(), Some(1), "{name}: {stderr}");
            assert!(
                stderr.contains("cannot bind the restored registry"),
                "{name}: {stderr}"
            );
            if existed {
                assert_eq!(tree(target), BTreeMap::new(), "{name}: left in {target:?}");
                assert_eq!(mode(target), 0o750, "{name}");
            } else {
                assert!(!target.exists(), "{name}: {target:?} left");
                assert!(!root.join("p").exists(), "{name}: its parent left");
            }
        }
    }
    let retried = memprove(&[
        "restore",
        "--from",
        out.to_str().unwrap(),
        "--data-dir",
        empty.to_str().unwrap(),
    ]);
    assert_eq!(retried.status.code(), Some(0), "{}", text(&retried.stderr));
    assert!(empty.join("registry.sqlite").is_file());
}

#[test]
fn restore_needs_both_options_and_backup_needs_a_data_directory() {
    for words in [
        &["restore", "--from", "/nowhere"][..],
        &["restore", "--data-dir", "/nowhere"][..],
        &["backup"][..],
        &["backup", "--out", "/nowhere"][..],
        &["backup", "--out", "/a", "--socket", "/s"][..],
    ] {
        let output = memprove(words);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{words:?}: {}",
            text(&output.stderr)
        );
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}
