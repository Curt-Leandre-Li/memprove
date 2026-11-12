//! Narrow tests of P4-D (ADR 0014 E3: the Task Capsule's `actions`, Owner
//! decisions A1, B3, C1) and P4-F (E6: `createSession`'s `resumed_from`
//! and `capsule.recovered@1`) through the real handlers, store, and Git
//! probes.
//!
//! Read and write agree (Owner condition b): every action read from the
//! Capsule is executed at once under the same ledger; an available one is
//! not refused, an unavailable one is refused with a code it lists. The
//! property test does that over randomly built ledgers.

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{AGENT, Fixture, commit_in, completion, ok, problem, strings};
use crate::{
    catalog::{create_session, end_session, resolve_workspace, transition_task},
    test_support::{git_worktree, json_request},
};

/// Changes a tracked file and commits it: the runs so far are stale.
fn change_and_commit(fixture: &Fixture, message: &str) {
    let readme = fixture.repo.join("README");
    let mut text = std::fs::read_to_string(&readme).unwrap_or_default();
    text.push_str(message);
    text.push('\n');
    std::fs::write(&readme, text).unwrap();
    commit_in(&fixture.repo, message);
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

/// A well-formed `resumed_from` at ledger sequence `sequence`.
fn resumed(sequence: u64) -> Value {
    json!({
        "capsule_digest": digest('c'),
        "project_sequence": sequence,
        "pack_digest": digest('d'),
        "budget": 9000,
    })
}

/// A `createSession` body, with the members of `extra` added.
fn session_request(workspace_id: &str, task_id: Option<&str>, extra: &Value) -> Value {
    let mut body = json!({
        "schema_version": 1, "workspace_id": workspace_id, "agent_id": AGENT,
        "task_id": task_id,
    });
    for (name, value) in extra.as_object().unwrap() {
        body[name] = value.clone();
    }
    body
}

impl Fixture {
    /// A new Session of `task` in the fixture's Workspace that declares a
    /// Pack of ledger sequence `sequence` (P4-F).
    pub(super) fn resumed_session(&self, task: &str, sequence: u64) -> String {
        let body = session_request(
            &self.workspace_id,
            Some(task),
            &json!({ "resumed_from": resumed(sequence) }),
        );
        ok(
            &self.create_session_with(&self.writer, &body),
            OperationId::CreateSession,
            201,
        )["session_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn create_session_with(&self, bearer: &str, body: &Value) -> crate::api::WireResponse {
        create_session(
            self.state(),
            &json_request(Some(bearer), Some(&self.key()), &body.to_string()),
        )
    }

    /// The Project's latest ledger sequence, as a Capsule reads it.
    fn high_water(&self) -> u64 {
        self.capsule(None).1["bindings"]["ledger"]["project_sequence"]
            .as_u64()
            .unwrap()
    }

    fn sessions_of_task(&self) -> usize {
        self.capsule(None).1["sessions"].as_array().unwrap().len()
    }

    /// The action `name` of the Capsule read now with `query`.
    fn action(&self, name: &str, query: Option<&str>) -> Value {
        let (_, capsule) = self.capsule(query);
        capsule["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["action"] == name)
            .unwrap()
            .clone()
    }

    /// A passed (`exit` 0, cases) or failed test run, or a command run.
    fn run(&self, kind: &str, exit_code: i32) -> (String, String) {
        let body = self.run_body(kind, exit_code, (kind == "test_run").then_some(3));
        let recorded = self.capture(&body);
        (
            recorded["evidence_id"].as_str().unwrap().to_owned(),
            body["snapshot_id"].as_str().unwrap().to_owned(),
        )
    }

    fn task_state(&self) -> (String, u64) {
        let (_, capsule) = self.capsule(None);
        (
            capsule["task"]["status"].as_str().unwrap().to_owned(),
            capsule["task"]["revision"].as_u64().unwrap(),
        )
    }

    /// Registers the linked worktree at `path` and returns its Workspace.
    fn resolve(&self, path: &std::path::Path) -> String {
        let resolved = ok(
            &resolve_workspace(
                self.state(),
                &json_request(
                    Some(&self.writer),
                    None,
                    &json!({ "schema_version": 1, "project_id": self.project_id, "path": path })
                        .to_string(),
                ),
            ),
            OperationId::ResolveWorkspace,
            200,
        );
        resolved["workspace_id"].as_str().unwrap().to_owned()
    }

    fn advance(&self, targets: &[&str]) {
        for target in targets {
            let (_, revision) = self.task_state();
            self.step(&self.task_id.clone(), revision, target);
        }
    }

    /// Ends the fixture's own Session, as abandoned (`session.crashed`) or
    /// normally.
    fn end_own_session(&self, abandoned: bool) {
        let mut body = json!({ "schema_version": 1, "expected_revision": 1, "reason": "done" });
        if abandoned {
            body["outcome"] = json!("abandoned");
        }
        ok(
            &end_session(
                self.state(),
                &json_request(Some(&self.writer), Some(&self.key()), &body.to_string()),
                &self.session_id,
            ),
            OperationId::EndSession,
            200,
        );
    }

    /// Moves the Task to `cancelled` from where it is.
    fn cancel(&self) {
        match self.task_state().0.as_str() {
            "in_progress" => self.advance(&["blocked", "cancelled"]),
            "proposed" | "ready" | "blocked" => self.advance(&["cancelled"]),
            _ => {}
        }
    }
}

/// Executes `action` at once, as its `args` say, and checks the outcome
/// against what the read said (Owner condition b). Returns whether it was
/// available.
fn execute_and_check(fixture: &Fixture, action: &Value, label: &str) -> bool {
    let args = &action["args"];
    let available = action["available"].as_bool().unwrap();
    let reasons = strings(&action["reasons"]);
    let (operation, response) = match action["action"].as_str().unwrap() {
        "complete_task" => {
            let mut body = completion(
                args["expected_revision"].as_u64().unwrap(),
                &strings(&args["completion_evidence_ids"]),
            );
            if !args["workspace_id"].is_null() {
                body["workspace_id"] = args["workspace_id"].clone();
            }
            (
                OperationId::TransitionTask,
                transition_task(
                    fixture.state(),
                    &json_request(
                        Some(&fixture.writer),
                        Some(&fixture.key()),
                        &body.to_string(),
                    ),
                    &fixture.task_id,
                ),
            )
        }
        "begin_session" => {
            let workspace = args["workspace_id"]
                .as_str()
                .unwrap_or(&fixture.workspace_id)
                .to_owned();
            let body = session_request(&workspace, Some(&fixture.task_id), &json!({}));
            (
                OperationId::CreateSession,
                fixture.create_session_with(&fixture.writer, &body),
            )
        }
        "cite_latest_run" => {
            let mut body = json!({
                "schema_version": 1,
                "workspace_id": args["workspace_id"].as_str().unwrap_or(&fixture.workspace_id),
                "task_id": fixture.task_id,
                "session_id": fixture.session_id,
                "objective": "cite the latest run",
                "verified_completed_work": [{
                    "classification": "verified_fact", "text": "the latest run passed",
                    "evidence_ids": args["evidence_id"].as_str().map_or_else(Vec::new, |id| vec![id]),
                }],
                "open_items": [],
                "next_action": { "classification": "recommendation", "text": "go on" },
            });
            // Without a run there is no Snapshot to anchor to.
            if let Some(snapshot) = args["snapshot_id"].as_str() {
                body["snapshot_id"] = json!(snapshot);
            }
            (
                OperationId::CreateCheckpoint,
                fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body),
            )
        }
        other => panic!("unknown action {other}"),
    };
    if available {
        assert!(
            (200..300).contains(&response.status),
            "{label}: {} was available but {}",
            action["action"],
            response.body
        );
        response.validate(Some(operation)).unwrap();
    } else {
        response.validate(Some(operation)).unwrap();
        let refused: Value = serde_json::from_str(&response.body).unwrap();
        let code = refused["code"].as_str().unwrap_or("");
        // The reasons are in check order: the write answers the first.
        assert_eq!(
            reasons.first(),
            Some(&code),
            "{label}: {} was unavailable for {reasons:?} but the write answered {} {}",
            action["action"],
            response.status,
            response.body
        );
    }
    available
}

/// Owner A1: a Session is not linked to a `completed` or `cancelled` Task
/// (`409 invalid_transition`), and nothing is written; `begin_session`
/// says so first.
#[test]
fn a1_a_finished_task_takes_no_new_session() {
    let fixture = Fixture::new();
    let (evidence, _) = fixture.run("test_run", 0);
    fixture.advance(&["ready", "in_progress"]);
    let (_, revision) = fixture.task_state();
    ok(
        &fixture.transition_with(
            &fixture.task_id.clone(),
            &fixture.key(),
            &completion(revision, &[&evidence]),
        ),
        OperationId::TransitionTask,
        200,
    );
    let action = fixture.action("begin_session", None);
    assert_eq!(action["available"], false);
    assert_eq!(strings(&action["reasons"]), ["invalid_transition"]);
    let before = (fixture.high_water(), fixture.sessions_of_task());
    let body = session_request(&fixture.workspace_id, Some(&fixture.task_id), &json!({}));
    for bearer in [&fixture.writer, &fixture.control] {
        problem(
            &fixture.create_session_with(bearer, &body),
            OperationId::CreateSession,
            409,
            "invalid_transition",
        );
    }
    assert_eq!((fixture.high_water(), fixture.sessions_of_task()), before);
    // A cancelled Task too.
    let cancelled = fixture.task(Some(&fixture.workspace_id.clone()));
    fixture.step(&cancelled, 1, "cancelled");
    let body = session_request(&fixture.workspace_id, Some(&cancelled), &json!({}));
    problem(
        &fixture.create_session_with(&fixture.writer, &body),
        OperationId::CreateSession,
        409,
        "invalid_transition",
    );
    // A Session without a Task is not affected.
    let body = session_request(&fixture.workspace_id, None, &json!({}));
    ok(
        &fixture.create_session_with(&fixture.writer, &body),
        OperationId::CreateSession,
        201,
    );
    // The late link of the store refuses it as well.
    let mut store = fixture.store();
    let session = memprove_domain::SessionId::parse(&fixture.session_id).unwrap();
    let task = memprove_domain::TaskId::parse(&cancelled).unwrap();
    assert!(matches!(
        store.link_session_to_task(
            &task,
            &session,
            &memprove_domain::UtcTimestamp::parse("2026-10-06T10:00:00Z").unwrap()
        ),
        Err(memprove_store_sqlite::StoreError::InvalidTransition)
    ));
}

/// Owner condition c: the actions do not depend on the caller's token; the
/// whole Capsule is the same under `control` and `mcp-read`.
#[test]
fn every_token_reads_the_same_actions() {
    let fixture = Fixture::new();
    fixture.run("test_run", 0);
    let read = |bearer: &str| {
        let response = fixture.capsule_with(Some(bearer), &fixture.task_id, None);
        ok(&response, OperationId::GetTaskCapsule, 200);
        response.body
    };
    let as_reader = read(&fixture.reader);
    assert_eq!(read(&fixture.control), as_reader);
    let capsule: Value = serde_json::from_str(&as_reader).unwrap();
    for action in capsule["actions"].as_array().unwrap() {
        assert_eq!(action["scope"], "mcp-write");
    }
    // A write token cannot read it at all: the access rule is unchanged.
    let refused = fixture.capsule_with(Some(&fixture.writer), &fixture.task_id, None);
    assert_eq!(refused.status, 403);
}

/// Owner condition d: a `tampered` Task cannot be completed, and the
/// Capsule says so; the write is refused with the listed code.
#[test]
fn a_tampered_task_lists_completion_as_unavailable() {
    let fixture = Fixture::new();
    fixture.run("test_run", 0);
    fixture.advance(&["ready", "in_progress"]);
    let before = fixture.action("complete_task", None);
    assert_eq!(before["available"], true, "{before}");
    let (_, revision) = fixture.task_state();
    // The Task row now disagrees with its last Task Event.
    fixture.tamper(&format!(
        "UPDATE tasks SET description = 'x', revision = {} WHERE task_id = '{}';",
        revision + 5,
        fixture.task_id
    ));
    let (_, capsule) = fixture.capsule(None);
    assert_eq!(capsule["status"], "tampered");
    let action = fixture.action("complete_task", None);
    assert_eq!(action["available"], false);
    assert_eq!(strings(&action["reasons"]), ["evidence_stale"]);
    assert!(!execute_and_check(&fixture, &action, "tampered"));
}

/// The actions of each state the property draws, once each, by hand: an
/// available and an unavailable reading of every action.
#[test]
fn each_action_is_read_as_its_write_answers() {
    let fixture = Fixture::new();
    // No run yet: nothing completes and nothing can be cited.
    for name in ["complete_task", "cite_latest_run"] {
        let action = fixture.action(name, None);
        assert_eq!(action["available"], false, "{action}");
        assert!(!execute_and_check(&fixture, &action, "no run"));
    }
    // A failed test run: still nothing.
    fixture.run("test_run", 1);
    let cite = fixture.action("cite_latest_run", None);
    assert_eq!(strings(&cite["reasons"]), ["invalid_request"]);
    assert!(!execute_and_check(&fixture, &cite, "failed run"));
    // A passed test run while the Task is proposed: citable, but the Task
    // cannot complete from proposed.
    fixture.run("test_run", 0);
    let complete = fixture.action("complete_task", None);
    assert_eq!(strings(&complete["reasons"]), ["invalid_transition"]);
    assert!(!execute_and_check(&fixture, &complete, "proposed"));
    let cite = fixture.action("cite_latest_run", None);
    assert!(execute_and_check(&fixture, &cite, "passed run"));
    let begin = fixture.action("begin_session", None);
    assert!(execute_and_check(&fixture, &begin, "proposed"));
    // A later command run is the latest run: a verified_fact needs a test.
    fixture.run("command_run", 0);
    let cite = fixture.action("cite_latest_run", None);
    assert_eq!(strings(&cite["reasons"]), ["invalid_request"]);
    assert!(!execute_and_check(&fixture, &cite, "command run"));
    // In progress, then a commit makes the runs stale.
    fixture.advance(&["ready", "in_progress"]);
    change_and_commit(&fixture, "move on");
    let complete = fixture.action("complete_task", None);
    assert_eq!(strings(&complete["reasons"]), ["evidence_stale"]);
    assert!(!execute_and_check(&fixture, &complete, "stale"));
    // A fresh passed run: the Task completes.
    fixture.run("test_run", 0);
    let complete = fixture.action("complete_task", None);
    assert_eq!(
        complete["args"]["completion_evidence_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(execute_and_check(&fixture, &complete, "fresh"));
    let begin = fixture.action("begin_session", None);
    assert_eq!(strings(&begin["reasons"]), ["invalid_transition"]);
    assert!(!execute_and_check(&fixture, &begin, "completed"));
}

/// P4-H1 (Owner, T20 N-1): a `completed` or `cancelled` Task takes no new
/// Session, so its latest run can be cited only through a Session of it
/// that has not ended; once none is open, `cite_latest_run` is
/// `invalid_transition`, after `invalid_request` when the run cannot be
/// cited anyway. Each reading is executed at once.
#[test]
fn n1_a_finished_task_cites_only_through_an_open_session() {
    let read_and_execute = |fixture: &Fixture, reasons: &[&str], label: &str| {
        let cite = fixture.action("cite_latest_run", None);
        assert_eq!(strings(&cite["reasons"]), reasons, "{label}: {cite}");
        assert_eq!(
            execute_and_check(fixture, &cite, label),
            reasons.is_empty(),
            "{label}"
        );
    };
    // Completed with a passed test run as the latest run.
    let fixture = Fixture::new();
    fixture.run("test_run", 0);
    fixture.advance(&["ready", "in_progress"]);
    let complete = fixture.action("complete_task", None);
    assert!(execute_and_check(&fixture, &complete, "complete"));
    read_and_execute(&fixture, &[], "completed, its Session open");
    fixture.end_own_session(false);
    read_and_execute(
        &fixture,
        &["invalid_transition"],
        "completed, Session ended",
    );

    // Completed with a command run as the latest run: not citable anyway.
    let fixture = Fixture::new();
    fixture.run("test_run", 0);
    fixture.run("command_run", 0);
    fixture.advance(&["ready", "in_progress"]);
    let complete = fixture.action("complete_task", None);
    assert!(execute_and_check(&fixture, &complete, "complete"));
    read_and_execute(&fixture, &["invalid_request"], "completed, command, open");
    fixture.end_own_session(true);
    read_and_execute(
        &fixture,
        &["invalid_request", "invalid_transition"],
        "completed, command, abandoned",
    );

    // Cancelled with a passed test run of a Session of its own.
    let mut fixture = Fixture::new();
    let cancelled = fixture.task(Some(&fixture.workspace_id.clone()));
    fixture.session_id = fixture.session(&fixture.workspace_id.clone(), Some(&cancelled));
    fixture.task_id = cancelled;
    fixture.run("test_run", 0);
    fixture.cancel();
    assert_eq!(fixture.task_state().0, "cancelled");
    read_and_execute(&fixture, &[], "cancelled, its Session open");
    fixture.end_own_session(true);
    read_and_execute(
        &fixture,
        &["invalid_transition"],
        "cancelled, Session abandoned",
    );

    // Cancelled with no run and no Session at all.
    let mut fixture = Fixture::new();
    fixture.task_id = fixture.task(Some(&fixture.workspace_id.clone()));
    fixture.cancel();
    assert!(
        fixture.capsule(None).1["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    read_and_execute(
        &fixture,
        &["invalid_request", "invalid_transition"],
        "cancelled, no run, no Session",
    );
    // An unfinished Task without an open Session can still begin one.
    let fixture = Fixture::new();
    fixture.run("test_run", 0);
    fixture.end_own_session(false);
    let cite = fixture.action("cite_latest_run", None);
    assert_eq!(cite["available"], true, "{cite}");
}

/// A Task scoped to one Workspace, read from another: `begin_session`
/// there is `project_workspace_mismatch`.
#[test]
fn a_scoped_task_cannot_begin_in_another_workspace() {
    let fixture = Fixture::new();
    let linked = git_worktree(&fixture.repo, &fixture.repos.path().join("linked"), "side");
    let resolved = fixture.resolve(&linked);
    let query = format!("workspace_id={resolved}");
    let action = fixture.action("begin_session", Some(&query));
    assert_eq!(action["args"]["workspace_id"], resolved.as_str());
    assert_eq!(strings(&action["reasons"]), ["project_workspace_mismatch"]);
    assert!(!execute_and_check(&fixture, &action, "other Workspace"));
    let here = fixture.action("begin_session", None);
    assert!(execute_and_check(&fixture, &here, "own Workspace"));
}

/// Owner condition b over random ledgers: runs that pass, fail, or are
/// plain commands, commits between them, Task transitions, a linked
/// Workspace, and a tampered Task row; then (P4-H1, T20 N-1) the Task
/// completed or cancelled, with its Session left open or ended normally or
/// abandoned. Every action is read and executed at once, with the
/// fixture's Session; both outcomes of every action occur.
#[test]
fn property_actions_agree_with_their_writes_on_random_ledgers() {
    let mut seen = std::collections::BTreeSet::new();
    let mut finished = std::collections::BTreeSet::new();
    for seed in 0..20_u64 {
        let mut random = Random(seed.wrapping_mul(0x9e37_79b9) ^ 0x5eed);
        let fixture = Fixture::new();
        let mut query = None;
        for _ in 0..=random.below(4) {
            match random.below(7) {
                0 | 1 => {
                    fixture.run("test_run", 0);
                }
                2 => {
                    fixture.run("test_run", 1);
                }
                3 => {
                    fixture.run("command_run", i32::from(random.below(2) == 0));
                }
                4 => {
                    change_and_commit(&fixture, &format!("commit {seed}"));
                }
                5 => {
                    let (state, _) = fixture.task_state();
                    let next: &[&str] = match (state.as_str(), random.below(3)) {
                        ("proposed", 0) => &["cancelled"],
                        ("proposed", _) => &["ready", "in_progress"],
                        ("ready" | "blocked", _) => &["in_progress"],
                        ("in_progress", 0) => &["blocked"],
                        _ => &[],
                    };
                    fixture.advance(next);
                }
                _ => {
                    if query.is_none() {
                        let linked = git_worktree(
                            &fixture.repo,
                            &fixture.repos.path().join(format!("linked-{seed}")),
                            &format!("side-{seed}"),
                        );
                        query = Some(format!("workspace_id={}", fixture.resolve(&linked)));
                    }
                }
            }
        }
        let tampered = random.below(5) == 0;
        if tampered {
            fixture.advance(match fixture.task_state().0.as_str() {
                "proposed" => &["ready", "in_progress"],
                "ready" => &["in_progress"],
                _ => &[],
            });
            fixture.tamper(&format!(
                "UPDATE tasks SET description = 'x' WHERE task_id = '{}';",
                fixture.task_id
            ));
            let (state, revision) = fixture.task_state();
            let _ = state;
            fixture.tamper(&format!(
                "UPDATE tasks SET revision = {} WHERE task_id = '{}';",
                revision + 3,
                fixture.task_id
            ));
        }
        let finish = if tampered { 0 } else { random.below(3) };
        if finish > 0 {
            let complete = fixture.action("complete_task", query.as_deref());
            if complete["available"] == true {
                assert!(execute_and_check(&fixture, &complete, "finish"));
                seen.insert(("complete_task", true));
            } else {
                fixture.cancel();
            }
        }
        let open = finish < 2;
        if !open {
            fixture.end_own_session(random.below(2) == 0);
        }
        let done = matches!(fixture.task_state().0.as_str(), "completed" | "cancelled");
        // Citing first, then a new Session, then completion: each read just
        // before its own write.
        for name in ["cite_latest_run", "begin_session", "complete_task"] {
            let action = fixture.action(name, query.as_deref());
            let available = execute_and_check(&fixture, &action, &format!("seed {seed}"));
            seen.insert((name, available));
            if name == "cite_latest_run" && done {
                finished.insert((open, available));
            }
        }
    }
    for name in ["cite_latest_run", "begin_session", "complete_task"] {
        for available in [true, false] {
            assert!(
                seen.contains(&(name, available)),
                "{name} {available}: {seen:?}"
            );
        }
    }
    // A finished Task with its Session open and with it ended; without an
    // open Session it is never citable.
    println!("finished Tasks, (Session open, citable): {finished:?}");
    assert!(finished.contains(&(false, false)), "{finished:?}");
    assert!(finished.iter().any(|(open, _)| *open), "{finished:?}");
    assert!(!finished.contains(&(false, true)), "{finished:?}");
}

struct Random(u64);

impl Random {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (value ^ (value >> 31)) % bound
    }
}

/// E6: a well-formed `resumed_from` is recorded as `capsule.recovered@1`
/// after `session.started` and `task.session_linked`, and the Capsule reads
/// it back on the Session, `declared: true`; the ledger verifies.
#[test]
fn a_resumed_session_records_capsule_recovered() {
    let fixture = Fixture::new();
    let sequence = fixture.high_water();
    let body = session_request(
        &fixture.workspace_id,
        Some(&fixture.task_id),
        &json!({ "resumed_from": resumed(sequence) }),
    );
    let created = ok(
        &fixture.create_session_with(&fixture.writer, &body),
        OperationId::CreateSession,
        201,
    );
    let session_id = created["session_id"].as_str().unwrap();
    let events = fixture.events();
    let tail = events[events.len() - 3..]
        .iter()
        .map(|event| event["event_type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        tail,
        [
            "session.started",
            "task.session_linked",
            "capsule.recovered"
        ]
    );
    let recovered = events.last().unwrap();
    assert_eq!(recovered["producer"]["name"], "continuityd");
    assert_eq!(recovered["payload_schema"], "capsule.recovered@1");
    assert_eq!(recovered["session_id"], session_id);
    assert_eq!(
        recovered["payload"],
        json!({
            "task_id": fixture.task_id, "capsule_digest": digest('c'),
            "project_sequence": sequence, "pack_digest": digest('d'), "budget": 9000,
            "declared": true,
        })
    );
    let (_, capsule) = fixture.capsule(None);
    let session = capsule["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["session_id"] == session_id)
        .unwrap();
    assert_eq!(
        session["resumed_from"],
        json!({
            "capsule_digest": digest('c'), "project_sequence": sequence,
            "pack_digest": digest('d'), "budget": 9000, "declared": true,
        })
    );
    assert!(
        capsule["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|other| other["session_id"] != session_id)
            .all(|other| other["resumed_from"].is_null())
    );
    assert!(fixture.ledger_valid());
    // A replay of the same request writes nothing more.
    let high_water = fixture.high_water();
    let key = fixture.key();
    let first = create_session(
        fixture.state(),
        &json_request(Some(&fixture.writer), Some(&key), &body.to_string()),
    );
    let again = create_session(
        fixture.state(),
        &json_request(Some(&fixture.writer), Some(&key), &body.to_string()),
    );
    assert_eq!(first.body, again.body);
    assert_eq!(fixture.high_water(), high_water + 3);
}

/// Owner condition f: `resumed_from` above the high-water mark, without a
/// Task, with a malformed digest, or with a budget out of the Pack's range
/// is `400 invalid_request`; neither the Session nor an Event is written.
#[test]
fn a_malformed_resumed_from_writes_nothing() {
    let fixture = Fixture::new();
    let high_water = fixture.high_water();
    let sessions = fixture.sessions_of_task();
    let mut cases = vec![
        ("above", Some(&fixture.task_id), resumed(high_water + 1)),
        ("no task", None, resumed(high_water)),
    ];
    for (name, value) in [
        ("capsule_digest", json!("sha256:1234")),
        ("pack_digest", json!(digest('A'))),
        ("budget", json!(999)),
        ("budget", json!(1_000_001)),
        ("project_sequence", json!(-1)),
    ] {
        let mut from = resumed(high_water);
        from[name] = value;
        cases.push((name, Some(&fixture.task_id), from));
    }
    let mut extra = resumed(high_water);
    extra["declared"] = json!(true);
    cases.push(("extra member", Some(&fixture.task_id), extra));
    for (label, task, from) in cases {
        let body = session_request(
            &fixture.workspace_id,
            task.map(String::as_str),
            &json!({ "resumed_from": from }),
        );
        let response = fixture.create_session_with(&fixture.writer, &body);
        problem(
            &response,
            OperationId::CreateSession,
            400,
            "invalid_request",
        );
        let detail = serde_json::from_str::<Value>(&response.body).unwrap()["detail"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(detail.contains("resumed_from"), "{label}: {detail}");
        assert_eq!(fixture.high_water(), high_water, "{label}");
        assert_eq!(fixture.sessions_of_task(), sessions, "{label}");
    }
    // At the high-water mark itself it is accepted.
    let body = session_request(
        &fixture.workspace_id,
        Some(&fixture.task_id),
        &json!({ "resumed_from": resumed(high_water) }),
    );
    ok(
        &fixture.create_session_with(&fixture.writer, &body),
        OperationId::CreateSession,
        201,
    );
}
