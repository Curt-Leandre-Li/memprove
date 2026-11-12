//! U8 A2 (Owner O4; the hard condition of `u8-long-task.md` section 4.1):
//! on a real ledger whose Sessions declared no agent, this build's
//! `memproved` serves every Task Capsule byte for byte as the start
//! build does, and `verifyLedger` of the copy it migrated (0008) is the
//! start build's answer, `valid`. U8 A6 (O8 (b)): a Task with withdrawn
//! claims differs only by their `withdrawn_by` and so its `capsule_digest`
//! (and the Pack's first line, which names it, and its last line, the
//! digest of the whole Pack); the `resume --text --all`
//! lines of withdrawn claims gain ` (withdrawn by Session j)`.
//!
//! Ignored by default: it needs a ledger backup and the start build's
//! binaries, which are not in the repository. Run it once with
//!
//! ```text
//! U8_START_BIN_DIR=<dir with memprove and memproved>
//! U8_LEDGER_BACKUP=<directory of a `memprove backup`>
//! U8_AGENT_TOKEN=<file with an mcp-read token of that ledger>
//! U8_WORK_DIR=<empty short directory, e.g. /private/tmp/u8-a-r>
//! cargo test -p memprove-daemon --test u8_a2_replica_digest_e2e -- --ignored
//! ```
//!
//! Both daemons run at the same time over two restored copies, so both
//! probe the same repository state. Each copy is restored with the start
//! build's `restore`; the start build's copy is never opened by this build.

mod p4b_support;

use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use p4b_support::{cli_binary, isolated, step, text};
use serde_json::Value;

fn var(name: &str) -> PathBuf {
    PathBuf::from(env::var_os(name).unwrap_or_else(|| panic!("{name} is not set")))
}

struct Copy {
    cli: PathBuf,
    data_dir: PathBuf,
    config_dir: PathBuf,
    token: PathBuf,
    daemon: Child,
}

impl Copy {
    fn run(&self, words: &[&str]) -> (Option<i32>, String) {
        let output = isolated(&self.cli)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--token-file")
            .arg(&self.token)
            .args(words)
            .env("MEMPROVE_CONFIG_DIR", &self.config_dir)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (output.status.code(), text(&output.stdout))
    }

    fn ok(&self, words: &[&str]) -> String {
        let (code, stdout) = self.run(words);
        assert_eq!(code, Some(0), "{words:?}: {stdout}");
        stdout
    }
}

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.daemon.id().to_string())
            .status();
        let _ = self.daemon.wait();
    }
}

/// Restores the backup into `<work>/<name>/data` with the start build and
/// starts `daemon` on it.
fn copy(work: &Path, name: &str, cli: PathBuf, daemon: &Path) -> Copy {
    let root = work.join(name);
    let data_dir = root.join("data");
    let config_dir = root.join("config");
    fs::create_dir_all(&config_dir).unwrap();
    fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let restored = isolated(var("U8_START_BIN_DIR").join("memprove"))
        .arg("restore")
        .arg("--from")
        .arg(var("U8_LEDGER_BACKUP"))
        .arg("--data-dir")
        .arg(&data_dir)
        .output()
        .unwrap();
    assert_eq!(
        restored.status.code(),
        Some(0),
        "{}",
        text(&restored.stderr)
    );
    let token = root.join("agent-token");
    fs::copy(var("U8_AGENT_TOKEN"), &token).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let child = isolated(daemon)
        .arg("--data-dir")
        .arg(&data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(root.join("daemon.log")).unwrap())
        .spawn()
        .unwrap();
    let copy = Copy {
        cli,
        data_dir,
        config_dir,
        token,
        daemon: child,
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while copy.run(&["health", "ready"]).0 != Some(0) {
        assert!(
            Instant::now() < deadline,
            "{name}: memproved never became ready"
        );
        thread::sleep(Duration::from_millis(50));
    }
    copy
}

fn project_id(copy: &Copy) -> String {
    let mut projects = fs::read_dir(copy.data_dir.join("projects"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(projects.len(), 1, "{projects:?}");
    projects.pop().unwrap()
}

#[test]
#[ignore = "needs a ledger backup and the start build; see the module"]
fn u8_a2_unnamed_sessions_keep_every_capsule_byte_for_byte() {
    let work = var("U8_WORK_DIR");
    let start_bin = var("U8_START_BIN_DIR");
    let start = copy(
        &work,
        "start",
        start_bin.join("memprove"),
        &start_bin.join("memproved"),
    );
    let head = copy(
        &work,
        "head",
        cli_binary(),
        Path::new(env!("CARGO_BIN_EXE_memproved")),
    );
    let project = project_id(&start);
    assert_eq!(project_id(&head), project);

    step("verifyLedger: the migrated copy answers as the start build does");
    let verify = ["ledger", "verify", "--project", project.as_str()];
    let (expected, migrated) = (start.ok(&verify), head.ok(&verify));
    println!("{migrated}");
    assert_eq!(migrated, expected);
    let verified: Value = serde_json::from_str(migrated.trim()).unwrap();
    assert_eq!(verified["valid"], true);

    step("every Task Capsule: byte for byte, except U8 A6's withdrawn_by");
    let listed: Value = serde_json::from_str(
        start
            .ok(&["task", "list", "--project", &project, "--limit", "100"])
            .trim(),
    )
    .unwrap();
    let tasks = listed["items"].as_array().unwrap();
    assert!(!tasks.is_empty());
    let mut changed = 0;
    for task in tasks {
        let task_id = task["task_id"].as_str().unwrap();
        let (withdrawn, differs) = compare(&start, &head, task_id);
        if differs {
            changed += 1;
            println!("{task_id}: {withdrawn} withdrawn claims, only withdrawn_by differs");
        }
    }
    println!("{changed} of {} Capsules differ", tasks.len());
}

/// Compares the Task's Capsule, Pack, and `resume --text` lines of both
/// builds; returns how many withdrawn claims the Capsule has and whether
/// its bytes differ. U8 A6 (Owner O4, O8 (b)): they may differ only by the
/// `withdrawn_by` of withdrawn claims and the `capsule_digest` (and so the
/// Pack's first line, which names it, and its `Digest: ` line); `resume --text`
/// is the same, and
/// `resume --text --all` differs only on withdrawn lines, which gain
/// ` (withdrawn by Session j)`.
fn compare(start: &Copy, head: &Copy, task_id: &str) -> (usize, bool) {
    let words = ["resume", "--task", task_id];
    let (before, after) = (start.ok(&words), head.ok(&words));
    let (old, mut new): (Value, Value) = (
        serde_json::from_str(before.trim()).unwrap(),
        serde_json::from_str(after.trim()).unwrap(),
    );
    println!(
        "{task_id} {} {}",
        old["capsule_digest"], new["capsule_digest"]
    );
    let mut withdrawn = 0;
    for claim in new["claims"].as_array_mut().unwrap() {
        let by = claim.as_object_mut().unwrap().remove("withdrawn_by");
        if claim["standing"] == "withdrawn" {
            withdrawn += 1;
            let by = by.expect("a withdrawn claim names what withdrew it");
            assert!(by["session_id"].is_string(), "{by}");
        } else {
            assert!(by.is_none(), "{claim}");
        }
    }
    new["capsule_digest"] = old["capsule_digest"].clone();
    assert_eq!(new, old, "Task {task_id}");
    assert_eq!(withdrawn == 0, before == after, "Task {task_id}");
    let pack = ["resume", "--task", task_id, "--pack"];
    let (old_pack, new_pack) = (start.ok(&pack), head.ok(&pack));
    // The first line names the capsule_digest, and the last (`Digest: `) is
    // the digest of the whole Pack, so both move with it; every other line
    // is the same.
    let inner = |pack: &str| {
        let lines = pack.lines().collect::<Vec<_>>();
        assert!(lines.last().unwrap().starts_with("Digest: sha256:"));
        lines[1..lines.len() - 1]
            .iter()
            .map(|line| (*line).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(inner(&old_pack), inner(&new_pack), "Task {task_id}");
    assert_eq!(withdrawn == 0, old_pack == new_pack, "Task {task_id}");
    let text = ["resume", "--task", task_id, "--text"];
    assert_eq!(start.ok(&text), head.ok(&text), "Task {task_id}");
    let all = ["resume", "--task", task_id, "--text", "--all"];
    let (old_all, new_all) = (start.ok(&all), head.ok(&all));
    for (old, new) in old_all.lines().zip(new_all.lines()) {
        if old != new {
            assert!(old.contains(", withdrawn: "), "{old}");
            let suffix = new.strip_prefix(old).unwrap_or_else(|| panic!("{new}"));
            assert!(
                suffix.starts_with(" (withdrawn by ") && suffix.ends_with(')'),
                "{new}"
            );
        }
    }
    (withdrawn, before != after)
}
