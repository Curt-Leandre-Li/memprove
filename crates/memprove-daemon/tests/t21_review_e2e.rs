//! The T21 fixes (fix round 1) through the real binaries.
//!
//! - V1-S-1: a state database whose last `schema_migrations` row is missing
//!   while its schema shows migration 0007 applied is not read as the
//!   Project's ledger: `ledger verify`, `resume`, and `resume --pack` answer
//!   `409 capsule_tampered` (exit 1), and the read writes nothing (before:
//!   the first read re-applied 0007, inserted a new row 7, and answered
//!   `valid` / `no inconsistency found`).
//! - V1-S-2: `ledger verify` exits 5 with a `task` `first_failure` for a
//!   Task row whose `created_at` is not UTF-8 and for a deleted Task row
//!   whose Task Events remain (before: exit 0, `valid` true).
//! - V1-N-5: a Session whose end the chain records takes no run, and a Task
//!   scoped by its `task.created@1` Event takes no Session from another
//!   Workspace, also after a writer that bypasses `memproved` cleared the
//!   row's `ended_at` or `workspace_id` (before: both writes succeeded).
//! - V1-R-2 (fix round 2): the state database of an `ACTIVE` Project that
//!   was deleted, made 0 bytes, or stripped of every object is not created
//!   again by a read: `ledger verify`, `task list`, `resume`, and `resume
//!   --pack` answer `409 capsule_tampered` (exit 1), and the Project's
//!   directory is unchanged (before: the first read created an empty
//!   ledger, `ledger verify` exited 0 with `valid` true and `task list`
//!   printed no Task).
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use std::{fs, path::Path};

use p4b_support::{Cli, Setup, git, step};
use serde_json::Value;

/// A Task with a verified fact in the Workspace of `setup`; returns the
/// output of `begin`.
fn verified_task(setup: &Setup) -> Value {
    let agent = &setup.agent;
    step("a verified fact");
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "T21 review",
    ]);
    agent.test("tests/a.sh");
    agent.ok(&["checkpoint", "create", "--fact", "a passes", "--next", "n1"]);
    assert_eq!(agent.ok(&["resume"])["status"], "verified");
    begun
}

/// `words` must exit 1 with `409 <code>` on standard error.
fn refused(setup: &Setup, words: &[&str], code: &str) {
    let run = setup.agent.run(words, false);
    assert_eq!(run.code, Some(1), "{words:?}: {}{}", run.stdout, run.stderr);
    assert!(
        run.stderr.contains(&format!("409 {code}")),
        "{words:?}: {}",
        run.stderr
    );
    assert!(!run.stderr.contains("503"), "{words:?}: {}", run.stderr);
}

/// `ledger verify`, which must exit 5; returns its `first_failure`.
fn first_failure(setup: &Setup) -> Value {
    let run = setup.agent.run(&["ledger", "verify"], false);
    assert_eq!(run.code, Some(5), "{}{}", run.stdout, run.stderr);
    let verified: Value = serde_json::from_str(run.stdout.trim()).unwrap();
    assert_eq!(verified["valid"], false);
    verified["first_failure"].clone()
}

#[test]
fn v1_s1_a_missing_last_migration_record_is_drift_and_a_read_writes_nothing() {
    let mut setup = Setup::new(&["a"]);
    verified_task(&setup);
    setup.stop();

    step("the last schema_migrations row (0007) is deleted");
    setup.query("DELETE FROM schema_migrations WHERE migration_id = 7;");
    let record = setup
        .query("SELECT group_concat(migration_id || ':' || applied_at) FROM schema_migrations;");
    let bytes = fs::read(setup.database()).unwrap();
    setup.start();
    refused(&setup, &["ledger", "verify"], "capsule_tampered");
    refused(&setup, &["resume"], "capsule_tampered");
    refused(&setup, &["resume", "--pack"], "capsule_tampered");
    setup.stop();
    assert_eq!(
        setup.query(
            "SELECT group_concat(migration_id || ':' || applied_at) FROM schema_migrations;"
        ),
        record,
        "a read applied a migration"
    );
    assert!(
        fs::read(setup.database()).unwrap() == bytes,
        "a read wrote the state database"
    );
}

/// A change made to the stopped Project's state database.
type ChangeState = fn(&Setup);

/// The files of the directory of `database` and their bytes; `None` when
/// the directory does not exist.
fn files_beside(database: &Path) -> Option<Vec<(String, Vec<u8>)>> {
    let mut files = fs::read_dir(database.parent()?)
        .ok()?
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read(&path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    files.sort();
    Some(files)
}

#[test]
fn v1_r2_a_deleted_or_emptied_state_database_is_not_created_again() {
    let mut setup = Setup::new(&["a"]);
    verified_task(&setup);
    let project = setup.project_id.clone();
    setup.stop();
    let pristine = setup.save("pristine");
    let cases: [(&str, ChangeState); 3] = [
        ("the state database is deleted", |setup| {
            fs::remove_file(setup.database()).unwrap();
        }),
        ("the state database is made 0 bytes", |setup| {
            fs::write(setup.database(), b"").unwrap();
        }),
        ("every table of the state database is dropped", |setup| {
            let tables = setup.query(
                "SELECT group_concat('DROP TABLE IF EXISTS \"' || name || '\";', ' ') \
                 FROM sqlite_master WHERE type = 'table';",
            );
            setup.tamper(&tables);
            assert_eq!(
                setup.query("SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%';"),
                "0"
            );
        }),
    ];
    for (case, change) in cases {
        step(case);
        change(&setup);
        let before = files_beside(&setup.database());
        setup.start();
        refused(&setup, &["ledger", "verify"], "capsule_tampered");
        refused(
            &setup,
            &["task", "list", "--project", &project],
            "capsule_tampered",
        );
        refused(&setup, &["resume"], "capsule_tampered");
        refused(&setup, &["resume", "--pack"], "capsule_tampered");
        setup.stop();
        assert!(
            files_beside(&setup.database()) == before,
            "{case}: a read created or wrote the state database"
        );
        setup.restore(&pristine);
    }
}

#[test]
fn v1_s2_ledger_verify_names_an_undecodable_or_missing_task_row() {
    let mut setup = Setup::new(&["a"]);
    let begun = verified_task(&setup);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    setup.stop();
    let pristine = setup.save("pristine");

    step("the Task row's created_at is not UTF-8");
    setup.tamper(&format!(
        "UPDATE tasks SET created_at = CAST(X'FF41' AS TEXT) WHERE task_id = '{task}';"
    ));
    setup.start();
    let failure = first_failure(&setup);
    assert_eq!(failure["record_type"], "task", "{failure}");
    assert_eq!(failure["record_id"], task.as_str(), "{failure}");
    assert_eq!(setup.agent.ok(&["resume"])["status"], "tampered");
    setup.stop();
    setup.restore(&pristine);

    step("the Task row is deleted; its Task Events remain");
    let created = setup
        .query(&format!(
            "SELECT project_sequence FROM events WHERE event_type = 'task.created' \
             AND json_extract(payload, '$.task_id') = '{task}';"
        ))
        .parse::<u64>()
        .unwrap();
    setup.tamper(&format!("DELETE FROM tasks WHERE task_id = '{task}';"));
    setup.start();
    let failure = first_failure(&setup);
    assert_eq!(failure["record_type"], "task", "{failure}");
    assert_eq!(failure["record_id"], task.as_str(), "{failure}");
    assert_eq!(failure["project_sequence"], created, "{failure}");
    setup.stop();
}

#[test]
fn v1_n5_writes_read_the_session_end_and_task_scope_from_the_chain() {
    let mut setup = Setup::new(&["a"]);
    let begun = verified_task(&setup);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    let session = begun["session_id"].as_str().unwrap().to_owned();
    let workspace = begun["workspace_id"].as_str().unwrap().to_owned();
    setup.agent.ok(&["end", "--reason", "done"]);

    step("a Task scoped to the root Workspace; a linked worktree");
    let scoped = setup.agent.ok(&[
        "task",
        "create",
        "--project",
        &setup.project_id,
        "--title",
        "scoped",
        "--workspace",
        &workspace,
    ]);
    let scoped = scoped["task_id"].as_str().unwrap().to_owned();
    let linked = setup.root_path.join("linked");
    git(
        &setup.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    let elsewhere = Cli {
        cwd: linked,
        label: "agent/linked",
        ..setup.agent.clone()
    };
    let project = setup.project_id.clone();
    let begin_elsewhere = ["begin", "--project", &project, "--task", &scoped];
    let run = elsewhere.run(&begin_elsewhere, false);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(
        run.stderr.contains("409 project_workspace_mismatch"),
        "{}",
        run.stderr
    );
    setup.stop();

    step("the ended Session's row and the scoped Task's row are rewritten");
    setup.tamper(&format!(
        "UPDATE sessions SET ended_at = NULL WHERE session_id = '{session}'; \
         UPDATE tasks SET workspace_id = NULL WHERE task_id = '{scoped}';"
    ));
    setup.start();
    let before = setup.query("SELECT max(project_sequence) FROM events;");
    // `run` runs the command, then the write of its result is refused:
    // exit 125, "not recorded" (docs/guide.md).
    let run = setup.agent.run(
        &[
            "run",
            "--session",
            &session,
            "--task",
            &task,
            "--workspace",
            &workspace,
            "--test-format",
            "cargo",
            "--",
            "sh",
            "tests/a.sh",
        ],
        false,
    );
    assert_eq!(run.code, Some(125), "{}", run.stderr);
    assert!(
        run.stderr
            .contains("captureEvidence failed: 409 invalid_transition"),
        "{}",
        run.stderr
    );
    let run = elsewhere.run(&begin_elsewhere, false);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(
        run.stderr.contains("409 project_workspace_mismatch"),
        "{}",
        run.stderr
    );
    setup.stop();
    assert_eq!(
        setup.query("SELECT max(project_sequence) FROM events;"),
        before,
        "a refused write appended an Event"
    );
}
