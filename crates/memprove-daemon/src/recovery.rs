//! `captureEvidence`, `getEvidence`, `createCheckpoint`, and `getCheckpoint`
//! (ADR 0012, P2-A), the read-only `getRecoveryPreview` (ADR 0012 D6,
//! P2-B), the read-only `getTaskCapsule` (ADR 0013 D4, P3-B), and
//! `proposeDecision` with the claim edges of `createCheckpoint` (ADR 0013
//! D1/D2, P3-C).
//!
//! Check order follows the contract: readiness, authentication,
//! authorization, then request validation, then the engine recovery service.
//! The command whose result is captured ran in the caller's environment;
//! `memproved` only runs its fixed-argv, read-only Git probes.

use memprove_domain::{
    Checkpoint, CheckpointClassification, CheckpointId, CheckpointStatement, ClaimId, Evidence,
    EvidenceCollector, EvidenceId, EvidenceKind, RepositorySnapshot, RepositorySnapshotId,
    RunStatus, SessionId, Sha256Digest, TaskId, TestFormat, UtcTimestamp, WorkspaceId,
    is_reserved_producer_name,
};
use memprove_engine::recovery::{
    CheckpointRequest, CommandRun, DecisionRequest, EvidenceRequest, RecordedCheckpoint,
    RecordedDecision, RecordedEvidence, RecoveryError, StatementRequest, TestDeclaration,
};
use memprove_protocol::{
    phase1::{OperationId, ProblemCode},
    strict_json::{JsonValue, echoable_name},
};
use serde_json::json;

use crate::{
    api::{AppState, WireResponse, guard},
    auth::{SharedRegistry, contains_secret_shape, lock, verifier},
    catalog::{
        command, id_member, invalid_id, mutation, parse_id, path_id, problem, tampered, text,
        unbound_state,
    },
    events::safe_integer,
    tokens::{TokenRequest, is_uuid_v7},
};

const EVIDENCE_PREFIX: &str = "/v1/evidence/";
const CHECKPOINT_PREFIX: &str = "/v1/checkpoints/";
const TASK_PREFIX: &str = "/v1/tasks/";
const PREVIEW_SUFFIX: &str = "/recovery-preview";
const CAPSULE_SUFFIX: &str = "/recovery-capsule";

fn recovery_problem(error: RecoveryError) -> WireResponse {
    problem(match error {
        RecoveryError::InvalidRequest(detail) => return WireResponse::invalid(detail),
        RecoveryError::WorkspaceNotResolved => ProblemCode::WorkspaceNotResolved,
        RecoveryError::ResourceNotFound => ProblemCode::ResourceNotFound,
        RecoveryError::ProjectWorkspaceMismatch => ProblemCode::ProjectWorkspaceMismatch,
        RecoveryError::IdempotencyConflict => ProblemCode::IdempotencyConflict,
        RecoveryError::InvalidTransition => ProblemCode::InvalidTransition,
        RecoveryError::Tampered => return tampered(),
        RecoveryError::UnboundState => return unbound_state(),
        RecoveryError::Unavailable => ProblemCode::ServiceNotReady,
        RecoveryError::RoleForbidden(rule) => return WireResponse::role_forbidden(rule),
    })
}

/// The wire spelling of a digest: `sha256:` and 64 lowercase hex digits.
fn wire_digest(digest: &Sha256Digest) -> String {
    format!("sha256:{digest}")
}

fn digest_of(value: &str) -> Option<Sha256Digest> {
    Sha256Digest::parse(value.strip_prefix("sha256:")?).ok()
}

fn snapshot_body(snapshot: &RepositorySnapshot) -> serde_json::Value {
    json!({
        "snapshot_id": snapshot.snapshot_id(),
        "task_id": snapshot.task_id(),
        "workspace_id": snapshot.workspace_id(),
        "head_oid": snapshot.head_oid(),
        "branch": snapshot.branch(),
        "is_dirty": snapshot.is_dirty(),
        "captured_at": snapshot.captured_at(),
        "capture_manifest_digest": wire_digest(snapshot.capture_manifest_digest()),
        "canonical_digest": wire_digest(snapshot.canonical_digest()),
        "git_head_evidence_id": snapshot.git_head_evidence_id(),
        "git_worktree_evidence_id": snapshot.git_worktree_evidence_id(),
        "command_evidence_ids": snapshot.command_evidence_ids(),
        "test_evidence_ids": snapshot.test_evidence_ids(),
    })
}

/// PRD FR-203 outcome of a command or test (`PASS`, `FAIL`, `NOT_RUN`, or
/// `UNAVAILABLE`); `null` for repository Evidence.
fn run_status(evidence: &Evidence) -> Option<&'static str> {
    evidence.payload().run_status().map(RunStatus::as_str)
}

fn artifact_type(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::CommandResult => "command_run",
        EvidenceKind::TestResult => "test_run",
        other => other.as_str(),
    }
}

fn evidence_body(record: &RecordedEvidence) -> serde_json::Value {
    let evidence = &record.evidence;
    json!({
        "schema_version": 1,
        "id": evidence.evidence_id(),
        "revision": 1,
        "evidence_id": evidence.evidence_id(),
        "task_id": evidence.task_id(),
        "workspace_id": evidence.workspace_id(),
        "kind": evidence.kind(),
        "artifact_type": artifact_type(evidence.kind()),
        "status": run_status(evidence),
        "subject": evidence.subject(),
        "observed_at": evidence.observed_at(),
        "collector": evidence.collector(),
        "payload": evidence.payload(),
        "payload_digest": wire_digest(evidence.payload_digest()),
        "canonical_digest": wire_digest(evidence.canonical_digest()),
        "snapshot": record.snapshots.first().map(snapshot_body),
        "snapshot_ids": record
            .snapshots
            .iter()
            .map(RepositorySnapshot::snapshot_id)
            .collect::<Vec<_>>(),
    })
}

fn statement_body(statement: &CheckpointStatement) -> serde_json::Value {
    json!({
        "classification": statement.classification().as_str(),
        "text": statement.text(),
        "evidence_ids": statement.evidence_ids(),
    })
}

fn checkpoint_body(record: &RecordedCheckpoint) -> serde_json::Value {
    let checkpoint: &Checkpoint = &record.checkpoint;
    json!({
        "schema_version": 1,
        "id": checkpoint.checkpoint_id(),
        "revision": 1,
        "checkpoint_id": checkpoint.checkpoint_id(),
        "task_id": checkpoint.task_id(),
        "session_id": checkpoint.session_id(),
        "workspace_id": record.snapshot.workspace_id(),
        "snapshot_id": checkpoint.snapshot_id(),
        "project_event_sequence": checkpoint.project_event_sequence(),
        "objective": checkpoint.objective(),
        "verified_completed_work": checkpoint
            .verified_completed_work()
            .iter()
            .map(statement_body)
            .collect::<Vec<_>>(),
        "open_items": checkpoint.open_items().iter().map(statement_body).collect::<Vec<_>>(),
        "next_action": statement_body(checkpoint.next_action()),
        "created_at": checkpoint.created_at(),
        "digest": wire_digest(checkpoint.digest()),
        "snapshot": snapshot_body(&record.snapshot),
    })
}

/// `400` for a `metadata` member the schema allowed but the daemon refuses;
/// `member` is its name under `$.metadata`.
fn bad_metadata(member: &str, rule: &str) -> WireResponse {
    WireResponse::invalid(format!("$.metadata.{member} {rule}"))
}

/// The test part of a `test_run`: `test_name`, `test_case_count`, and (U9 K2,
/// Owner O6) `test_format`, the `--test-format` the caller made the run with
/// (one of the closed set; the wire validator has checked the value). All
/// three are refused on a `command_run`.
fn test_declaration(
    metadata: &JsonValue,
    test_run: bool,
) -> Result<Option<TestDeclaration>, WireResponse> {
    let test_name = text(metadata, "test_name").map(str::to_owned);
    let test_case_count = match metadata.get("test_case_count") {
        None => None,
        Some(value) => Some(safe_integer(value).ok_or_else(|| {
            bad_metadata(
                "test_case_count",
                "must be an integer from 0 through 9007199254740991",
            )
        })?),
    };
    let test_format = match metadata.get("test_format") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .and_then(TestFormat::parse)
                .ok_or_else(|| bad_metadata("test_format", "must be cargo, pytest, or cases"))?,
        ),
    };
    if test_run {
        Ok(Some(TestDeclaration {
            test_name,
            test_case_count,
            test_format,
        }))
    } else if test_name.is_some() || test_case_count.is_some() {
        Err(WireResponse::invalid(
            "$.metadata.test_name and $.metadata.test_case_count apply only to artifact_type test_run",
        ))
    } else if test_format.is_some() {
        Err(WireResponse::invalid(
            "$.metadata.test_format applies only to artifact_type test_run",
        ))
    } else {
        Ok(None)
    }
}

/// The typed `CommandRunMetadata` of a `command_run` or `test_run`. Test
/// fields on a `command_run` are refused. `launch_error` marks a command the
/// caller could not start (P2-C); the domain checks its exit code.
fn command_run(
    metadata: &JsonValue,
    test_run: bool,
    registry: &SharedRegistry,
) -> Result<CommandRun, WireResponse> {
    let argv = metadata
        .get("argv")
        .and_then(JsonValue::as_array)
        .and_then(|argv| {
            argv.iter()
                .map(|argument| argument.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| bad_metadata("argv", "must be an array of strings"))?;
    for (index, argument) in argv.iter().enumerate() {
        refuse_token_shape(registry, argument, &format!("$.metadata.argv[{index}]"))?;
    }
    let exit_code = metadata
        .get("exit_code")
        .and_then(exit_code)
        .ok_or_else(|| bad_metadata("exit_code", "must be a 32-bit signed integer"))?;
    let timestamp = |name: &str| {
        text(metadata, name)
            .and_then(|stamp| UtcTimestamp::normalize(stamp).ok())
            .ok_or_else(|| bad_metadata(name, "must be an RFC 3339 UTC timestamp"))
    };
    let collector = metadata
        .get("collector")
        .ok_or_else(|| bad_metadata("collector", "is required"))?;
    let collector_name = text(collector, "name")
        .ok_or_else(|| bad_metadata("collector.name", "must be a string"))?;
    // P2-E: only memproved's own probes collect under its name.
    if is_reserved_producer_name(collector_name) {
        return Err(bad_metadata(
            "collector.name",
            "is a name reserved for continuityd",
        ));
    }
    let collector_version = text(collector, "version")
        .ok_or_else(|| bad_metadata("collector.version", "must be a string"))?;
    let test = test_declaration(metadata, test_run)?;
    let tail = |name: &str| {
        text(metadata, name)
            .map(str::to_owned)
            .ok_or_else(|| bad_metadata(name, "must be a string"))
    };
    let digest = |name: &str| {
        text(metadata, name).and_then(digest_of).ok_or_else(|| {
            bad_metadata(name, "must be sha256: followed by 64 lowercase hex digits")
        })
    };
    Ok(CommandRun {
        argv,
        exit_code,
        started_at: timestamp("started_at")?,
        finished_at: timestamp("finished_at")?,
        stdout_tail: tail("stdout_tail")?,
        stderr_tail: tail("stderr_tail")?,
        stdout_sha256: digest("stdout_sha256")?,
        stderr_sha256: digest("stderr_sha256")?,
        collector: EvidenceCollector {
            name: collector_name.to_owned(),
            version: collector_version.to_owned(),
        },
        test,
        launch_error: match metadata.get("launch_error") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| bad_metadata("launch_error", "must be a string"))?
                    .to_owned(),
            ),
        },
    })
}

fn exit_code(value: &JsonValue) -> Option<i32> {
    match value {
        JsonValue::Number(raw) => raw.parse::<i64>().ok()?.try_into().ok(),
        _ => None,
    }
}

/// `POST /v1/evidence/capture`.
#[must_use]
pub fn capture_evidence(state: &AppState, request: &TokenRequest<'_>) -> WireResponse {
    let operation = OperationId::CaptureEvidence;
    let (principal, key, body) = match mutation(state, operation, request) {
        Ok(decoded) => decoded,
        Err(denied) => return denied,
    };
    let test_run = match text(&body, "artifact_type") {
        Some("command_run") => false,
        Some("test_run") => true,
        _ => {
            return WireResponse::invalid("$.artifact_type must be command_run or test_run");
        }
    };
    if !matches!(body.get("object_id"), None | Some(JsonValue::Null)) {
        return WireResponse::invalid("$.object_id must be absent or null");
    }
    let workspace_id = match id_member(&body, "workspace_id", WorkspaceId::parse) {
        Ok(workspace_id) => workspace_id,
        Err(invalid) => return invalid,
    };
    let task_id = match id_member(&body, "task_id", TaskId::parse) {
        Ok(task_id) => task_id,
        Err(invalid) => return invalid,
    };
    let session_id = match id_member(&body, "session_id", SessionId::parse) {
        Ok(session_id) => session_id,
        Err(invalid) => return invalid,
    };
    let snapshot_id = match id_member(&body, "snapshot_id", RepositorySnapshotId::parse) {
        Ok(snapshot_id) => snapshot_id,
        Err(invalid) => return invalid,
    };
    let run = match body.get("metadata") {
        Some(metadata) => match command_run(metadata, test_run, &state.registry) {
            Ok(run) => run,
            Err(invalid) => return invalid,
        },
        None => return WireResponse::invalid("$.metadata is required"),
    };
    let canonical = json!({
        "operation_id": operation.as_str(),
        "workspace_id": workspace_id,
        "task_id": task_id,
        "session_id": session_id,
        "snapshot_id": snapshot_id,
        "artifact_type": if test_run { "test_run" } else { "command_run" },
        "argv": run.argv,
        "exit_code": run.exit_code,
        "started_at": run.started_at,
        "finished_at": run.finished_at,
        "stdout_tail": run.stdout_tail,
        "stderr_tail": run.stderr_tail,
        "stdout_sha256": run.stdout_sha256,
        "stderr_sha256": run.stderr_sha256,
        "collector": run.collector,
        "test_name": run.test.as_ref().map(|test| test.test_name.clone()),
        "test_case_count": run.test.as_ref().map(|test| test.test_case_count),
    });
    // Present only when given, so the digest of every earlier request stays
    // the same and replays across the upgrade still match.
    let mut canonical = canonical;
    if let Some(launch_error) = &run.launch_error {
        canonical["launch_error"] = json!(launch_error);
    }
    if let Some(format) = run.test.as_ref().and_then(|test| test.test_format) {
        canonical["test_format"] = json!(format.as_str());
    }
    let command = match command(&principal, operation, &key, &canonical) {
        Ok(command) => command,
        Err(invalid) => return invalid,
    };
    let request = EvidenceRequest {
        workspace_id,
        task_id,
        session_id,
        snapshot_id,
        run,
    };
    match state.recovery.capture_evidence(request, &command) {
        Ok(record) => WireResponse::json(201, &evidence_body(&record)),
        Err(error) => recovery_problem(error),
    }
}

/// Extracts `{evidence_id}` from a `/v1/evidence/{evidence_id}` path.
#[must_use]
pub fn evidence_target(path: &str) -> &str {
    path.strip_prefix(EVIDENCE_PREFIX).unwrap_or_default()
}

/// `GET /v1/evidence/{evidence_id}`.
#[must_use]
pub fn get_evidence(state: &AppState, bearer: Option<&str>, evidence_id: &str) -> WireResponse {
    if let Err(denied) = guard(&state.plane, OperationId::GetEvidence, bearer) {
        return denied;
    }
    let evidence_id = match path_id(evidence_id, "evidence_id", EvidenceId::parse) {
        Ok(evidence_id) => evidence_id,
        Err(invalid) => return invalid,
    };
    match state.recovery.get_evidence(&evidence_id) {
        Ok(record) => WireResponse::json(200, &evidence_body(&record)),
        Err(error) => recovery_problem(error),
    }
}

/// U1 (C4, Owner O6): `400` when `value`, the text of the member `at`, holds
/// a string in the format of a token secret. The detail is fixed text that
/// names the member; the text is not repeated, and nothing has been written.
/// U3 (K5, Owner O4): also a Task's title and description, a Checkpoint's
/// objective, an end's reason, and each element of an Evidence's argv.
///
/// U5 P2 (Owner O4): the format is a maximal run of 43 characters of `A-Z`,
/// `a-z`, `0-9`, `-`, and `_` with at least one upper-case letter, one
/// lower-case letter, and one digit (a); and (b) every maximal run of 43
/// such characters, whatever classes it holds, whose SHA-256 is the
/// verifier of a token in `registry` (one this daemon issued, in any state)
/// is refused too. One detail for both: it does not say which held. A
/// registry that cannot be read is `503 service_not_ready`, as the
/// authentication of the same request would be.
pub(crate) fn refuse_token_shape(
    registry: &SharedRegistry,
    value: &str,
    at: &str,
) -> Result<(), WireResponse> {
    let refused = || {
        WireResponse::invalid(format!(
            "{at} holds a string in the format of a memprove token (43 characters of A-Z, \
             a-z, 0-9, '-', and '_', with at least one upper-case letter, one lower-case \
             letter, and one digit) or a token this memproved issued; nothing was recorded"
        ))
    };
    if contains_secret_shape(value) {
        return Err(refused());
    }
    let mut candidates = memprove_domain::token_candidates(value).peekable();
    if candidates.peek().is_none() {
        return Ok(());
    }
    let unavailable = || WireResponse::problem(ProblemCode::ServiceNotReady);
    let registry = lock(registry).map_err(|_| unavailable())?;
    for candidate in candidates {
        match registry.token_verifier_issued(&verifier(candidate)) {
            Ok(true) => return Err(refused()),
            Ok(false) => {}
            Err(error) => {
                eprintln!("memproved: cannot read client tokens: {error}");
                return Err(unavailable());
            }
        }
    }
    Ok(())
}

/// One statement of a Checkpoint; `at` is its location, for the `400`.
fn statement(
    value: &JsonValue,
    at: &str,
    registry: &SharedRegistry,
) -> Result<StatementRequest, WireResponse> {
    let classification = match text(value, "classification") {
        Some("verified_fact") => CheckpointClassification::VerifiedFact,
        Some("user_supplied_context") => CheckpointClassification::UserSuppliedContext,
        Some("recommendation") => CheckpointClassification::Recommendation,
        Some("unknown") => CheckpointClassification::Unknown,
        _ => {
            return Err(WireResponse::invalid(format!(
                "{at}.classification must be verified_fact, user_supplied_context, recommendation, or unknown"
            )));
        }
    };
    let evidence_ids = match value.get("evidence_ids") {
        None => Vec::new(),
        Some(ids) => ids
            .as_array()
            .and_then(|ids| {
                ids.iter()
                    .map(|id| id.as_str().and_then(|id| parse_id(id, EvidenceId::parse)))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| invalid_id(&format!("every {at}.evidence_ids item")))?,
    };
    let statement_text = text(value, "text")
        .ok_or_else(|| WireResponse::invalid(format!("{at}.text must be a string")))?;
    refuse_token_shape(registry, statement_text, &format!("{at}.text"))?;
    Ok(StatementRequest {
        classification,
        text: statement_text.to_owned(),
        evidence_ids,
        supersedes: claim_ids(value, "supersedes", at)?,
    })
}

/// The optional array of claim IDs `name` of `value` (ADR 0013 D1, P3-C);
/// `at` is the location of `value`.
fn claim_ids(value: &JsonValue, name: &str, at: &str) -> Result<Vec<ClaimId>, WireResponse> {
    match value.get(name) {
        None => Ok(Vec::new()),
        Some(ids) => ids
            .as_array()
            .and_then(|ids| {
                ids.iter()
                    .map(|id| id.as_str().and_then(ClaimId::parse))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| {
                WireResponse::invalid(format!(
                    "every {at}.{name} item must be a claim ID: \
                     <checkpoint_id>/<section>/<ordinal> or decision/<event_id>"
                ))
            }),
    }
}

fn claim_wires(ids: &[ClaimId]) -> Vec<String> {
    ids.iter().map(ClaimId::wire).collect()
}

fn statements(
    body: &JsonValue,
    name: &str,
    registry: &SharedRegistry,
) -> Result<Vec<StatementRequest>, WireResponse> {
    match body.get(name) {
        None => Ok(Vec::new()),
        Some(items) => items
            .as_array()
            .ok_or_else(|| WireResponse::invalid(format!("$.{name} must be an array")))?
            .iter()
            .enumerate()
            .map(|(index, item)| statement(item, &format!("$.{name}[{index}]"), registry))
            .collect(),
    }
}

fn statement_canonical(statement: &StatementRequest) -> serde_json::Value {
    let mut canonical = json!({
        "classification": statement.classification.as_str(),
        "text": statement.text,
        "evidence_ids": statement.evidence_ids,
    });
    // P3-C: present only when given, so the digest of every earlier request
    // stays the same and replays across the upgrade still match.
    if !statement.supersedes.is_empty() {
        canonical["supersedes"] = json!(claim_wires(&statement.supersedes));
    }
    canonical
}

/// `POST /v1/checkpoints`.
#[must_use]
pub fn create_checkpoint(state: &AppState, request: &TokenRequest<'_>) -> WireResponse {
    let operation = OperationId::CreateCheckpoint;
    let (principal, key, body) = match mutation(state, operation, request) {
        Ok(decoded) => decoded,
        Err(denied) => return denied,
    };
    let workspace_id = match id_member(&body, "workspace_id", WorkspaceId::parse) {
        Ok(workspace_id) => workspace_id,
        Err(invalid) => return invalid,
    };
    let task_id = match id_member(&body, "task_id", TaskId::parse) {
        Ok(task_id) => task_id,
        Err(invalid) => return invalid,
    };
    let session_id = match id_member(&body, "session_id", SessionId::parse) {
        Ok(session_id) => session_id,
        Err(invalid) => return invalid,
    };
    let snapshot_id = match id_member(&body, "snapshot_id", RepositorySnapshotId::parse) {
        Ok(snapshot_id) => snapshot_id,
        Err(invalid) => return invalid,
    };
    let Some(objective) = text(&body, "objective").map(str::to_owned) else {
        return WireResponse::invalid("$.objective must be a string");
    };
    if let Err(refused) = refuse_token_shape(&state.registry, &objective, "$.objective") {
        return refused;
    }
    let verified_completed_work =
        match statements(&body, "verified_completed_work", &state.registry) {
            Ok(statements) => statements,
            Err(invalid) => return invalid,
        };
    let open_items = match statements(&body, "open_items", &state.registry) {
        Ok(statements) => statements,
        Err(invalid) => return invalid,
    };
    let next_action = match body.get("next_action") {
        Some(next) => match statement(next, "$.next_action", &state.registry) {
            Ok(next) => next,
            Err(invalid) => return invalid,
        },
        None => return WireResponse::invalid("$.next_action is required"),
    };
    let withdrawals = match claim_ids(&body, "withdrawals", "$") {
        Ok(withdrawals) => withdrawals,
        Err(invalid) => return invalid,
    };
    // P3-D2 (Owner, T13 N2): why the withdrawn claims no longer hold; a
    // withdrawn fact needs it (checked against the ledger by the store).
    let withdrawal_reason = text(&body, "withdrawal_reason").map(str::to_owned);
    if let Some(reason) = &withdrawal_reason
        && let Err(refused) = refuse_token_shape(&state.registry, reason, "$.withdrawal_reason")
    {
        return refused;
    }
    if withdrawal_reason.is_some() && withdrawals.is_empty() {
        return WireResponse::invalid(
            "$.withdrawal_reason is given without $.withdrawals; it says why the withdrawn \
             claims no longer hold",
        );
    }
    let mut canonical = json!({
        "operation_id": operation.as_str(),
        "workspace_id": workspace_id,
        "task_id": task_id,
        "session_id": session_id,
        "snapshot_id": snapshot_id,
        "objective": objective,
        "verified_completed_work": verified_completed_work
            .iter()
            .map(statement_canonical)
            .collect::<Vec<_>>(),
        "open_items": open_items.iter().map(statement_canonical).collect::<Vec<_>>(),
        "next_action": statement_canonical(&next_action),
    });
    if !withdrawals.is_empty() {
        canonical["withdrawals"] = json!(claim_wires(&withdrawals));
    }
    // P3-D2: present only when given, so earlier requests keep their digest.
    if let Some(reason) = &withdrawal_reason {
        canonical["withdrawal_reason"] = json!(reason);
    }
    let command = match command(&principal, operation, &key, &canonical) {
        Ok(command) => command,
        Err(invalid) => return invalid,
    };
    let request = CheckpointRequest {
        workspace_id,
        task_id,
        session_id,
        snapshot_id,
        objective,
        verified_completed_work,
        open_items,
        next_action,
        withdrawals,
        withdrawal_reason,
    };
    match state.recovery.create_checkpoint(request, &command) {
        Ok(record) => WireResponse::json(201, &checkpoint_body(&record)),
        Err(error) => recovery_problem(error),
    }
}

fn decision_body(record: &RecordedDecision) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "id": record.decision_id,
        "revision": 1,
        "decision_id": record.decision_id,
        "claim_id": ClaimId::Decision(record.decision_id.clone()).wire(),
        "task_id": record.task_id,
        "session_id": record.session_id,
        "project_sequence": record.project_sequence,
        "content": record.payload.text,
        "rationale": record.payload.rationale,
        "evidence_ids": record.payload.evidence_ids,
        "supersedes": claim_wires(&record.supersedes),
        "recorded_at": record.recorded_at,
    })
}

/// `POST /v1/decisions` (ADR 0013 D2, P3-C): records a Decision with its
/// `decision.recorded` Event and one `claim.superseded` Event per
/// `supersedes` item, in one transaction. There is no accept step.
#[must_use]
pub fn propose_decision(state: &AppState, request: &TokenRequest<'_>) -> WireResponse {
    let operation = OperationId::ProposeDecision;
    let (principal, key, body) = match mutation(state, operation, request) {
        Ok(decoded) => decoded,
        Err(denied) => return denied,
    };
    let task_id = match id_member(&body, "task_id", TaskId::parse) {
        Ok(task_id) => task_id,
        Err(invalid) => return invalid,
    };
    let session_id = match id_member(&body, "session_id", SessionId::parse) {
        Ok(session_id) => session_id,
        Err(invalid) => return invalid,
    };
    let Some(content) = text(&body, "content").map(str::to_owned) else {
        return WireResponse::invalid("$.content must be a string");
    };
    if let Err(refused) = refuse_token_shape(&state.registry, &content, "$.content") {
        return refused;
    }
    let rationale = match body.get("rationale") {
        None | Some(JsonValue::Null) => None,
        Some(value) => match value.as_str() {
            Some(rationale) => Some(rationale.to_owned()),
            None => return WireResponse::invalid("$.rationale must be a string or null"),
        },
    };
    if let Some(rationale) = &rationale
        && let Err(refused) = refuse_token_shape(&state.registry, rationale, "$.rationale")
    {
        return refused;
    }
    let evidence_ids = match body.get("evidence_ids") {
        None => Vec::new(),
        Some(ids) => match ids.as_array().and_then(|ids| {
            ids.iter()
                .map(|id| id.as_str().and_then(|id| parse_id(id, EvidenceId::parse)))
                .collect::<Option<Vec<_>>>()
        }) {
            Some(ids) => ids,
            None => return invalid_id("every $.evidence_ids item"),
        },
    };
    let supersedes = match claim_ids(&body, "supersedes", "$") {
        Ok(supersedes) => supersedes,
        Err(invalid) => return invalid,
    };
    let canonical = json!({
        "operation_id": operation.as_str(),
        "task_id": task_id,
        "session_id": session_id,
        "content": content,
        "rationale": rationale,
        "evidence_ids": evidence_ids,
        "supersedes": claim_wires(&supersedes),
    });
    let command = match command(&principal, operation, &key, &canonical) {
        Ok(command) => command,
        Err(invalid) => return invalid,
    };
    let request = DecisionRequest {
        task_id,
        session_id,
        text: content,
        rationale,
        evidence_ids,
        supersedes,
    };
    match state.recovery.propose_decision(request, &command) {
        Ok(record) => WireResponse::json(201, &decision_body(&record)),
        Err(error) => recovery_problem(error),
    }
}

/// Extracts `{checkpoint_id}` from a `/v1/checkpoints/{checkpoint_id}` path.
#[must_use]
pub fn checkpoint_target(path: &str) -> &str {
    path.strip_prefix(CHECKPOINT_PREFIX).unwrap_or_default()
}

/// `GET /v1/checkpoints/{checkpoint_id}`.
#[must_use]
pub fn get_checkpoint(state: &AppState, bearer: Option<&str>, checkpoint_id: &str) -> WireResponse {
    if let Err(denied) = guard(&state.plane, OperationId::GetCheckpoint, bearer) {
        return denied;
    }
    let checkpoint_id = match path_id(checkpoint_id, "checkpoint_id", CheckpointId::parse) {
        Ok(checkpoint_id) => checkpoint_id,
        Err(invalid) => return invalid,
    };
    match state.recovery.get_checkpoint(&checkpoint_id) {
        Ok(record) => WireResponse::json(200, &checkpoint_body(&record)),
        Err(error) => recovery_problem(error),
    }
}

/// Extracts `{task_id}` from a `/v1/tasks/{task_id}/recovery-preview` path.
#[must_use]
pub fn recovery_preview_target(path: &str) -> &str {
    path.strip_prefix(TASK_PREFIX)
        .and_then(|rest| rest.strip_suffix(PREVIEW_SUFFIX))
        .unwrap_or_default()
}

/// The optional `workspace_id` query parameter; anything else, a repeat, or
/// a malformed value is `400 invalid_request`.
fn preview_query(query: Option<&str>) -> Result<Option<WorkspaceId>, WireResponse> {
    let mut workspace_id = None;
    for pair in query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        let Some((name, value)) = pair.split_once('=') else {
            return Err(WireResponse::invalid(
                "a query parameter has no value; write name=value",
            ));
        };
        if name != "workspace_id" {
            return Err(WireResponse::invalid(match echoable_name(name) {
                Some(name) => format!("unknown query parameter {name}"),
                None => "unknown query parameter".to_owned(),
            }));
        }
        if workspace_id.is_some() {
            return Err(WireResponse::invalid(
                "query parameter workspace_id was given more than once",
            ));
        }
        if !is_uuid_v7(value) {
            return Err(invalid_id("query parameter workspace_id"));
        }
        workspace_id = Some(
            parse_id(value, WorkspaceId::parse)
                .ok_or_else(|| invalid_id("query parameter workspace_id"))?,
        );
    }
    Ok(workspace_id)
}

/// `GET /v1/tasks/{task_id}/recovery-preview`: the JCS bytes of the
/// Preview, unchanged. Reads only; writes nothing and appends no Event.
#[must_use]
pub fn get_recovery_preview(
    state: &AppState,
    bearer: Option<&str>,
    task_id: &str,
    query: Option<&str>,
) -> WireResponse {
    if let Err(denied) = guard(&state.plane, OperationId::GetRecoveryPreview, bearer) {
        return denied;
    }
    let task_id = match path_id(task_id, "task_id", TaskId::parse) {
        Ok(task_id) => task_id,
        Err(invalid) => return invalid,
    };
    let workspace_id = match preview_query(query) {
        Ok(workspace_id) => workspace_id,
        Err(invalid) => return invalid,
    };
    match state
        .recovery
        .recovery_preview(&task_id, workspace_id.as_ref())
    {
        Ok(preview) => WireResponse {
            status: 200,
            media_type: "application/json",
            body: preview.as_str().to_owned(),
            bearer_challenge: false,
        },
        Err(error) => recovery_problem(error),
    }
}

/// Extracts `{task_id}` from a `/v1/tasks/{task_id}/recovery-capsule` path.
#[must_use]
pub fn task_capsule_target(path: &str) -> &str {
    path.strip_prefix(TASK_PREFIX)
        .and_then(|rest| rest.strip_suffix(CAPSULE_SUFFIX))
        .unwrap_or_default()
}

/// `GET /v1/tasks/{task_id}/recovery-capsule` (ADR 0013 D4): the JCS bytes
/// of the Task Capsule, unchanged. Reads only; writes nothing and appends no
/// Event. The query is the Preview's: an optional `workspace_id`.
#[must_use]
pub fn get_task_capsule(
    state: &AppState,
    bearer: Option<&str>,
    task_id: &str,
    query: Option<&str>,
) -> WireResponse {
    if let Err(denied) = guard(&state.plane, OperationId::GetTaskCapsule, bearer) {
        return denied;
    }
    let task_id = match path_id(task_id, "task_id", TaskId::parse) {
        Ok(task_id) => task_id,
        Err(invalid) => return invalid,
    };
    let workspace_id = match preview_query(query) {
        Ok(workspace_id) => workspace_id,
        Err(invalid) => return invalid,
    };
    match state.recovery.task_capsule(&task_id, workspace_id.as_ref()) {
        Ok(capsule) => WireResponse {
            status: 200,
            media_type: "application/json",
            body: capsule.as_str().to_owned(),
            bearer_challenge: false,
        },
        Err(error) => recovery_problem(error),
    }
}
