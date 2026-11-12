//! Narrow tests of P4-H1 (Owner, T20 SF-1 and SF-2): no read of the Project
//! ledger answers `5xx` because a row of it is missing or does not decode;
//! `503 service_not_ready` stays for what a retry can cure.
//!
//! - SF-1: a missing `workspaces` row is read as one that does not decode:
//!   the Capsule and the Preview answer `200` with the probe unavailable,
//!   never `verified`, and name no damaged record; `listTasks` does not
//!   read the root Workspace.
//! - SF-2: `verifyLedger` reports a row that does not decode as its
//!   `first_failure`.
//! - The sweep: every column of the first and the last row of every table
//!   the reads cover, made a value that does not decode (`'x'`: not a UUID,
//!   a time, JSON, or a digest; `-1`: an integer out of range; NULL where
//!   the column allows it), and each of those rows deleted; every read
//!   operation runs against each.
//! - The column sweep: each column made NULL, `0`, or text that is not
//!   UTF-8 in every row of its table at once.
//! - The edge sweep (review of the first P4-H1 commit): the same columns
//!   made a value that decodes as its SQL type but not as what the column
//!   holds (`0`, `2^53`, and `i64::MAX` in an INTEGER column; the empty
//!   text and text whose bytes are not UTF-8 in a TEXT one), each table
//!   emptied, and the state database's binding (`state_metadata`) and
//!   migration record (`schema_migrations`) broken.
//!
//! Every tamper is a writer that bypasses `memproved`: `sqlite3` on the
//! state database with every trigger dropped, foreign keys and CHECK
//! constraints off.

use std::{fmt::Write as _, process::Command};

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{
    Fixture,
    integrity_tests::{Ledger, quote},
    ok,
};
use crate::{
    api::WireResponse,
    catalog::{get_project, list_sessions, list_tasks, resolve_workspace},
    events::{get_event, list_events, verify_ledger},
    recovery::{get_checkpoint, get_evidence, get_recovery_preview, get_task_capsule},
    test_support::{git_worktree, json_request},
};

/// The tables of the Project ledger the read operations cover.
const TABLES: [&str; 11] = [
    "workspaces",
    "tasks",
    "sessions",
    "task_sessions",
    "events",
    "evidence",
    "repository_snapshots",
    "repository_snapshot_evidence",
    "checkpoints",
    "checkpoint_items",
    "checkpoint_item_evidence",
];

/// The sweep's cases run on this many fixtures at once.
const SWEEP_THREADS: usize = 4;

impl Fixture {
    /// The rows of `sql` on the state database, each a list of its fields.
    pub(super) fn rows(&self, sql: &str) -> Vec<Vec<String>> {
        let output = Command::new("sqlite3")
            .args(["-batch", "-noheader", "-separator", "\u{1f}"])
            .arg(self.database())
            .arg(sql)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{sql}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| line.split('\u{1f}').map(str::to_owned).collect())
            .collect()
    }

    /// The first field of every row of `sql`.
    pub(super) fn column(&self, sql: &str) -> Vec<String> {
        self.rows(sql)
            .into_iter()
            .map(|mut row| row.remove(0))
            .collect()
    }

    /// Runs `sql` with every trigger dropped and foreign keys and CHECK
    /// constraints off: a writer that bypasses `memproved` entirely.
    pub(super) fn corrupt(&self, sql: &str) {
        let output = self.run_tampering(sql);
        assert!(
            output.status.success(),
            "{sql}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// [`Self::corrupt`]; `false`, with the database unchanged, when a
    /// constraint no writer can bypass (`NOT NULL`, `UNIQUE`, a `STRICT`
    /// type) refuses `sql`.
    fn try_corrupt(&self, sql: &str) -> bool {
        self.run_tampering(sql).status.success()
    }

    fn run_tampering(&self, sql: &str) -> std::process::Output {
        let mut drops = String::new();
        for name in self.column("SELECT name FROM sqlite_master WHERE type = 'trigger'") {
            write!(drops, "DROP TRIGGER IF EXISTS {name};").unwrap();
        }
        Command::new("sqlite3")
            .arg(self.database())
            .arg(format!(
                "PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON; \
                 BEGIN; {drops} {sql} COMMIT;"
            ))
            .output()
            .unwrap()
    }
}

/// The identities every read of the sweep names, taken before any tamper.
struct Subjects {
    workspaces: Vec<String>,
    tasks: Vec<String>,
    events: Vec<String>,
    evidence: Vec<String>,
    checkpoints: Vec<String>,
}

impl Subjects {
    fn of(fixture: &Fixture) -> Self {
        Self {
            workspaces: fixture.column("SELECT workspace_id FROM workspaces ORDER BY rowid"),
            tasks: fixture.column("SELECT task_id FROM tasks ORDER BY rowid"),
            events: fixture.column("SELECT event_id FROM events ORDER BY rowid"),
            evidence: fixture.column("SELECT evidence_id FROM evidence ORDER BY rowid"),
            checkpoints: fixture.column("SELECT checkpoint_id FROM checkpoints ORDER BY rowid"),
        }
    }
}

/// One read: its label, its operation, and its response.
type Read = (String, OperationId, WireResponse);

/// The project-wide reads: `getProject`, `listTasks`, `listEvents`,
/// `verifyLedger`, and `listSessions` of every Workspace.
fn project_reads(fixture: &Fixture, subjects: &Subjects) -> Vec<Read> {
    let state = fixture.state();
    let control = Some(fixture.control.as_str());
    let reader = Some(fixture.reader.as_str());
    let project = fixture.project_id.as_str();
    let verify = json!({ "schema_version": 1, "project_id": project }).to_string();
    let key = fixture.key();
    let mut reads = vec![
        (
            "getProject".to_owned(),
            OperationId::GetProject,
            get_project(state, reader, project),
        ),
        (
            "listTasks".to_owned(),
            OperationId::ListTasks,
            list_tasks(
                state,
                reader,
                Some(&format!("project_id={project}&limit=500")),
            ),
        ),
        (
            "listEvents".to_owned(),
            OperationId::ListEvents,
            list_events(
                state,
                control,
                Some(&format!("project_id={project}&limit=500")),
            ),
        ),
        (
            "verifyLedger".to_owned(),
            OperationId::VerifyLedger,
            verify_ledger(state, &json_request(reader, Some(&key), &verify)),
        ),
    ];
    for workspace in &subjects.workspaces {
        let query = format!("workspace_id={workspace}&limit=500");
        reads.push((
            format!("listSessions {workspace}"),
            OperationId::ListSessions,
            list_sessions(state, reader, Some(&query)),
        ));
    }
    reads
}

/// Every read operation of the Project ledger, on every subject: the
/// Capsule and the Preview of every Task, without and with each
/// `workspace_id`, and each Event, Evidence object, and Checkpoint.
fn every_read(fixture: &Fixture, subjects: &Subjects) -> Vec<Read> {
    let state = fixture.state();
    let control = Some(fixture.control.as_str());
    let reader = Some(fixture.reader.as_str());
    let mut reads = project_reads(fixture, subjects);
    for task in &subjects.tasks {
        let asked = std::iter::once(None).chain(
            subjects
                .workspaces
                .iter()
                .map(|workspace| Some(format!("workspace_id={workspace}"))),
        );
        for query in asked {
            let label = query.clone().unwrap_or_default();
            reads.push((
                format!("getTaskCapsule {task} {label}"),
                OperationId::GetTaskCapsule,
                get_task_capsule(state, reader, task, query.as_deref()),
            ));
            reads.push((
                format!("getRecoveryPreview {task} {label}"),
                OperationId::GetRecoveryPreview,
                get_recovery_preview(state, reader, task, query.as_deref()),
            ));
        }
    }
    for event in &subjects.events {
        reads.push((
            format!("getEvent {event}"),
            OperationId::GetEvent,
            get_event(state, control, event),
        ));
    }
    for evidence in &subjects.evidence {
        reads.push((
            format!("getEvidence {evidence}"),
            OperationId::GetEvidence,
            get_evidence(state, control, evidence),
        ));
    }
    for checkpoint in &subjects.checkpoints {
        reads.push((
            format!("getCheckpoint {checkpoint}"),
            OperationId::GetCheckpoint,
            get_checkpoint(state, reader, checkpoint),
        ));
    }
    reads
}

/// The reads of `case` that answer `5xx` or leave their contract; each
/// line names the case, the read, and the response. The ledger check
/// always answers `200`; the Capsule and the Preview answer `200` unless
/// the case removes a Task's own row or its identity (`404`). A state
/// database whose binding or migration record is broken is not read as
/// the Project's at all: every read of it answers `409 capsule_tampered`.
fn failures(case: &str, reads: &[Read]) -> Vec<String> {
    let task_row_gone = case.starts_with("tasks rowid")
        || case.starts_with("tasks.task_id ")
        || case == "tasks emptied";
    let unbound = case.starts_with("state_metadata") || case.starts_with("schema_migrations");
    reads
        .iter()
        .filter_map(|(label, operation, response)| {
            let document = matches!(
                operation,
                OperationId::GetTaskCapsule | OperationId::GetRecoveryPreview
            );
            let tampered =
                unbound && response.status == 409 && response.body.contains("\"capsule_tampered\"");
            let broken = response.status >= 500
                || response.validate(Some(*operation)).is_err()
                || (*operation == OperationId::VerifyLedger && response.status != 200 && !tampered)
                || (document
                    && response.status != 200
                    && !(task_row_gone && response.status == 404)
                    && !tampered);
            broken.then(|| format!("{case} | {label} | {} {}", response.status, response.body))
        })
        .collect()
}

/// The ledger of [`Ledger::new`], a Decision on its Task, a second Task, and
/// a linked worktree Workspace: every kind of row the reads cover.
fn sweep_ledger() -> Ledger {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    fixture.decide("keep the fixture small", &[&ledger.cp3.evidence], &[]);
    fixture.task(None);
    let linked = git_worktree(
        &fixture.repo,
        &fixture.repos.path().join("linked"),
        "linked",
    );
    let body = json!({ "schema_version": 1, "project_id": fixture.project_id, "path": linked });
    ok(
        &resolve_workspace(
            fixture.state(),
            &json_request(Some(&fixture.writer), None, &body.to_string()),
        ),
        OperationId::ResolveWorkspace,
        200,
    );
    ledger
}

/// The values a sweep writes into a column, by its declared type
/// (`INTEGER` or `TEXT`; every table is `STRICT`) and whether it takes NULL.
type Values = fn(&str, bool) -> Vec<&'static str>;

/// The first sweep's values: `'x'` (not a UUID, a time, JSON, or a digest)
/// in a TEXT column, `-1` (out of range) in an INTEGER one, and NULL where
/// the column allows it.
fn undecodable_values(kind: &str, nullable: bool) -> Vec<&'static str> {
    let mut values = vec![if kind == "INTEGER" { "-1" } else { "'x'" }];
    if nullable {
        values.push("NULL");
    }
    values
}

/// The edge sweep's values, which decode as their SQL type but not as what
/// the column holds: `0` (below every revision and Project sequence),
/// `2^53` (above the largest safe integer), and `i64::MAX` in an INTEGER
/// column; the empty text and text whose bytes are not UTF-8 in a TEXT one.
fn edge_values(kind: &str, _nullable: bool) -> Vec<&'static str> {
    if kind == "INTEGER" {
        vec!["0", "9007199254740992", "9223372036854775807"]
    } else {
        vec!["''", "CAST(X'FF' AS TEXT)"]
    }
}

/// The tamper cases of `table`: each column of its first and last row made
/// each of `values`, and, with `delete_rows`, each of those rows deleted.
fn table_cases(
    fixture: &Fixture,
    table: &str,
    values: Values,
    delete_rows: bool,
) -> Vec<(String, String)> {
    let columns = fixture.rows(&format!(
        "SELECT name, type, \"notnull\" FROM pragma_table_info('{table}')"
    ));
    let rowids = fixture.column(&format!("SELECT rowid FROM {table} ORDER BY rowid"));
    let mut picked = vec![rowids.first().unwrap().clone()];
    if rowids.len() > 1 {
        picked.push(rowids.last().unwrap().clone());
    }
    let mut cases = Vec::new();
    for rowid in &picked {
        for column in &columns {
            let (name, kind, not_null) = (&column[0], &column[1], &column[2]);
            for value in values(kind, not_null == "0") {
                cases.push((
                    format!("{table}.{name} = {value} at rowid {rowid}"),
                    format!("UPDATE {table} SET {name} = {value} WHERE rowid = {rowid};"),
                ));
            }
        }
        if delete_rows {
            cases.push((
                format!("{table} rowid {rowid} deleted"),
                format!("DELETE FROM {table} WHERE rowid = {rowid};"),
            ));
        }
    }
    cases
}

/// The cases of `table` that change one column in every row: NULL where
/// the column allows it, `0` in an INTEGER column, and text whose bytes
/// are not UTF-8 (distinct per row) in a TEXT one. A bulk read meets
/// these in rows of every kind, not only the first and the last.
fn column_cases(fixture: &Fixture, table: &str) -> Vec<(String, String)> {
    let columns = fixture.rows(&format!(
        "SELECT name, type, \"notnull\" FROM pragma_table_info('{table}')"
    ));
    let mut cases = Vec::new();
    for column in &columns {
        let (name, kind, not_null) = (&column[0], &column[1], &column[2]);
        let mut values = vec![if kind == "INTEGER" {
            "0"
        } else {
            "CAST(X'FF' || rowid AS TEXT)"
        }];
        if not_null == "0" {
            values.push("NULL");
        }
        for value in values {
            cases.push((
                format!("{table}.{name} = {value} in every row"),
                format!("UPDATE {table} SET {name} = {value};"),
            ));
        }
    }
    cases
}

/// The first sweep: [`undecodable_values`] and deleted rows.
fn row_cases(fixture: &Fixture) -> Vec<(String, String)> {
    TABLES
        .iter()
        .flat_map(|table| table_cases(fixture, table, undecodable_values, true))
        .collect()
}

/// The edge sweep: [`edge_values`], every table emptied, and the state
/// database's binding and migration record broken.
fn edge_cases(fixture: &Fixture) -> Vec<(String, String)> {
    let mut cases = TABLES
        .iter()
        .flat_map(|table| table_cases(fixture, table, edge_values, false))
        .collect::<Vec<_>>();
    for table in TABLES {
        cases.push((format!("{table} emptied"), format!("DELETE FROM {table};")));
    }
    for (case, sql) in [
        ("state_metadata emptied", "DELETE FROM state_metadata;"),
        (
            "state_metadata.project_id not UTF-8",
            "UPDATE state_metadata SET project_id = CAST(X'FF' AS TEXT);",
        ),
        (
            "state_metadata.project_id another Project",
            "UPDATE state_metadata SET project_id = '01a00000-0000-7000-8000-000000000000';",
        ),
        (
            "schema_migrations last checksum x",
            "UPDATE schema_migrations SET migration_checksum = 'x'
             WHERE migration_id = (SELECT MAX(migration_id) FROM schema_migrations);",
        ),
        (
            "schema_migrations.migration_checksum not UTF-8",
            "UPDATE schema_migrations SET migration_checksum = CAST(X'FF' AS TEXT)
             WHERE migration_id = 1;",
        ),
    ] {
        cases.push((case.to_owned(), sql.to_owned()));
    }
    cases
}

/// Runs every `slice`-th case of `cases`, from `offset`, on a fixture of
/// its own; returns the number of cases run and every broken read. The
/// worktrees are moved away first, so that each probe is unavailable at
/// once: a probe reads no row of the ledger but the Workspace's, which the
/// `sf1_*` test covers with the probe running.
fn sweep_slice(
    offset: usize,
    slice: usize,
    cases: fn(&Fixture) -> Vec<(String, String)>,
) -> (usize, Vec<String>) {
    let ledger = sweep_ledger();
    let fixture = &ledger.fixture;
    let subjects = Subjects::of(fixture);
    let pristine = std::fs::read(fixture.database()).unwrap();
    let mut broken = failures("untouched", &every_read(fixture, &subjects));
    for name in ["repo", "linked"] {
        let path = fixture.repos.path().join(name);
        std::fs::rename(&path, path.with_extension("away")).unwrap();
    }
    let cases = cases(fixture);
    let mut count = 0;
    for (case, sql) in cases.iter().skip(offset).step_by(slice) {
        std::fs::write(fixture.database(), &pristine).unwrap();
        if !fixture.try_corrupt(sql) {
            continue;
        }
        broken.extend(failures(case, &every_read(fixture, &subjects)));
        count += 1;
    }
    (count, broken)
}

/// The column sweep: [`column_cases`] of every table.
fn every_row_cases(fixture: &Fixture) -> Vec<(String, String)> {
    TABLES
        .iter()
        .flat_map(|table| column_cases(fixture, table))
        .collect()
}

/// Runs `cases` on [`SWEEP_THREADS`] fixtures; asserts that at least
/// `minimum` cases ran and that no read broke.
fn sweep(cases: fn(&Fixture) -> Vec<(String, String)>, minimum: usize) {
    let results = std::thread::scope(|scope| {
        (0..SWEEP_THREADS)
            .map(|offset| scope.spawn(move || sweep_slice(offset, SWEEP_THREADS, cases)))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    let count = results.iter().map(|(count, _)| count).sum::<usize>();
    let broken = results
        .into_iter()
        .flat_map(|(_, broken)| broken)
        .collect::<Vec<_>>();
    assert!(count > minimum, "{count} cases");
    assert!(
        broken.is_empty(),
        "{} broken reads in {count} cases:\n{}",
        broken.len(),
        broken.join("\n")
    );
}

/// P4-H1 item 3: no read of the Project ledger answers `5xx`, or leaves its
/// contract, for any column of any covered table that does not decode or
/// any covered row that is missing.
#[test]
fn no_read_answers_5xx_for_a_row_that_is_missing_or_does_not_decode() {
    sweep(row_cases, 200);
}

/// P4-H1 item 3 (review of the first P4-H1 commit): nor for a value that
/// decodes as its SQL type but not as what its column holds (a zero
/// revision or Project sequence, text that is not UTF-8, empty text), an
/// emptied table, or a state database whose binding or migration record is
/// broken.
#[test]
fn no_read_answers_5xx_for_an_edge_value_an_emptied_table_or_a_broken_binding() {
    sweep(edge_cases, 300);
}

/// P4-H1 item 3 (review of the first P4-H1 commit): nor when one column
/// holds NULL, `0`, or text that is not UTF-8 in every row of its table.
#[test]
fn no_read_answers_5xx_for_a_column_that_does_not_decode_in_every_row() {
    sweep(every_row_cases, 100);
}

/// The Capsule of the fixture's Task, asked with `query`.
fn capsule_of(fixture: &Fixture, query: Option<&str>) -> Value {
    ok(
        &fixture.capsule_with(Some(&fixture.reader), &fixture.task_id, query),
        OperationId::GetTaskCapsule,
        200,
    )
}

/// The Preview of the fixture's Task, asked with `query`.
fn preview_of(fixture: &Fixture, query: Option<&str>) -> Value {
    ok(
        &fixture.preview_with(Some(&fixture.reader), &fixture.task_id, query),
        OperationId::GetRecoveryPreview,
        200,
    )
}

/// The parts of the recovery documents a `workspaces` row decides: the
/// verdicts, the probes, and the damaged records.
fn probe_outcome(capsule: &Value, preview: &Value) -> Value {
    json!({
        "capsule_status": capsule["status"],
        "capsule_probe": capsule["bindings"]["probe"],
        "damaged_records": capsule["damaged_records"],
        "preview_status": preview["status"],
        "preview_probe": preview["bindings"]["probe"],
    })
}

/// T20 SF-1: a missing `workspaces` row gives what a row that does not
/// decode gives, with and without `workspace_id`: `200`, the probe
/// unavailable, never `verified`, no damaged record; `listTasks` answers.
#[test]
fn sf1_a_missing_workspace_row_reads_as_one_that_does_not_decode() {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let workspace = fixture.workspace_id.clone();
    let asked = format!("workspace_id={workspace}");
    let pristine = std::fs::read(fixture.database()).unwrap();
    let outcome = |fixture: &Fixture| {
        [None, Some(asked.as_str())]
            .map(|query| probe_outcome(&capsule_of(fixture, query), &preview_of(fixture, query)))
    };

    fixture.corrupt(&format!(
        "UPDATE workspaces SET created_at = 'not a time' WHERE workspace_id = {};",
        quote(&workspace)
    ));
    let undecodable = outcome(fixture);
    std::fs::write(fixture.database(), &pristine).unwrap();
    fixture.corrupt(&format!(
        "DELETE FROM workspaces WHERE workspace_id = {};",
        quote(&workspace)
    ));
    let missing = outcome(fixture);
    assert_eq!(missing, undecodable);
    for document in &missing {
        assert_eq!(document["capsule_probe"]["available"], false, "{document}");
        assert_eq!(
            document["capsule_probe"]["workspace_id"],
            workspace.as_str()
        );
        assert_eq!(document["preview_probe"]["available"], false, "{document}");
        assert_ne!(document["capsule_status"], "verified", "{document}");
        assert_ne!(document["preview_status"], "verified", "{document}");
        assert_eq!(document["damaged_records"], json!([]), "{document}");
    }
    let tasks = ok(
        &list_tasks(
            fixture.state(),
            Some(&fixture.reader),
            Some(&format!("project_id={}", fixture.project_id)),
        ),
        OperationId::ListTasks,
        200,
    );
    assert_eq!(tasks["items"][0]["task_id"], fixture.task_id.as_str());
}

/// T20 SF-2: `verifyLedger` on a statement `evidence_id` of the latest
/// Checkpoint that does not decode answers `200`, `valid` false, with that
/// Checkpoint as its first failure, never `503`.
#[test]
fn sf2_a_statement_evidence_id_that_does_not_decode_is_the_first_failure() {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    fixture.corrupt(&format!(
        "UPDATE checkpoint_item_evidence SET evidence_id = 'x' WHERE checkpoint_id = {};",
        quote(&ledger.cp3.checkpoint)
    ));
    let verified = ok(
        &fixture.verify_with(
            Some(&fixture.reader),
            Some(&fixture.key()),
            &fixture.project_id,
        ),
        OperationId::VerifyLedger,
        200,
    );
    assert_eq!(verified["valid"], false, "{verified}");
    assert_eq!(
        verified["first_failure"],
        json!({
            "record_type": "checkpoint",
            "record_id": ledger.cp3.checkpoint,
            "project_sequence": null,
            "reason": "evidence_id must be a UUIDv7",
        })
    );
}

/// The Problem of a read whose record is missing or does not decode:
/// `409 capsule_tampered`, not retryable, with the fixed detail.
fn assert_capsule_tampered(case: &str, operation: OperationId, response: &WireResponse) {
    assert_eq!(response.status, 409, "{case}: {}", response.body);
    response.validate(Some(operation)).unwrap();
    let body: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(body["code"], "capsule_tampered", "{case}");
    assert_eq!(body["retryable"], false, "{case}");
    assert_eq!(body["detail"], crate::catalog::TAMPERED_RECORD, "{case}");
}

/// One case of [`a_record_that_does_not_decode_is_capsule_tampered_not_retryable`]:
/// its name, the tamper, the operation, and the read.
type DamagedRead<'a> = (
    &'static str,
    String,
    OperationId,
    Box<dyn Fn() -> WireResponse + 'a>,
);

/// P4-H1 item 3: a single record, or a row of a list page, that is missing
/// or does not decode is `409 capsule_tampered`, which no retry cures; the
/// rest of the Project still reads.
#[test]
fn a_record_that_does_not_decode_is_capsule_tampered_not_retryable() {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let state = fixture.state();
    let (control, reader) = (
        Some(fixture.control.as_str()),
        Some(fixture.reader.as_str()),
    );
    let project = fixture.project_id.clone();
    let event = fixture.column("SELECT event_id FROM events ORDER BY rowid LIMIT 1")[0].clone();
    let pristine = std::fs::read(fixture.database()).unwrap();
    let cases: [DamagedRead<'_>; 7] = [
        (
            "root Workspace row deleted",
            format!(
                "DELETE FROM workspaces WHERE workspace_id = {};",
                quote(&fixture.workspace_id)
            ),
            OperationId::GetProject,
            Box::new(|| get_project(state, reader, &project)),
        ),
        (
            "an Event payload",
            format!(
                "UPDATE events SET payload = '{{}}' WHERE event_id = {};",
                quote(&event)
            ),
            OperationId::GetEvent,
            Box::new(|| get_event(state, control, &event)),
        ),
        (
            "an Event payload, listed",
            format!(
                "UPDATE events SET payload = '{{}}' WHERE event_id = {};",
                quote(&event)
            ),
            OperationId::ListEvents,
            Box::new(|| list_events(state, control, Some(&format!("project_id={project}")))),
        ),
        (
            "an Evidence time",
            format!(
                "UPDATE evidence SET observed_at = 'x' WHERE evidence_id = {};",
                quote(&ledger.cp1.evidence)
            ),
            OperationId::GetEvidence,
            Box::new(|| get_evidence(state, control, &ledger.cp1.evidence)),
        ),
        (
            "a Checkpoint time",
            format!(
                "UPDATE checkpoints SET created_at = 'x' WHERE checkpoint_id = {};",
                quote(&ledger.cp1.checkpoint)
            ),
            OperationId::GetCheckpoint,
            Box::new(|| get_checkpoint(state, reader, &ledger.cp1.checkpoint)),
        ),
        (
            "a Task status",
            format!(
                "UPDATE tasks SET status = 'x' WHERE task_id = {};",
                quote(&fixture.task_id)
            ),
            OperationId::ListTasks,
            Box::new(|| list_tasks(state, reader, Some(&format!("project_id={project}")))),
        ),
        (
            "a Session agent",
            format!(
                "UPDATE sessions SET agent_id = 'x' WHERE session_id = {};",
                quote(&fixture.session_id)
            ),
            OperationId::ListSessions,
            Box::new(|| {
                let query = format!("workspace_id={}", fixture.workspace_id);
                list_sessions(state, reader, Some(&query))
            }),
        ),
    ];
    for (case, sql, operation, read) in &cases {
        std::fs::write(fixture.database(), &pristine).unwrap();
        assert_eq!(read().status, 200, "{case}: untouched");
        fixture.corrupt(sql);
        assert_capsule_tampered(case, *operation, &read());
    }
}
