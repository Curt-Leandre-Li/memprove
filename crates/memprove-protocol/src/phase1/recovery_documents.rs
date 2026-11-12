//! Response validators of the two read-only recovery documents:
//! `RecoveryPreview` (ADR 0012 D6), every member checked against `OpenAPI`
//! (T11 S-3, P3-B), and `TaskCapsule` (ADR 0013 D4/D5).
//!
//! Like every response validator, unknown members are accepted
//! (`DecodeMode::CompatibleResponse`); every known member is checked.

use std::cmp::Ordering;

use super::{
    DecodeMode, JsonValue, TaskState, ValidationResult, checked_object, ensure_unique,
    expect_array, expect_bool, expect_integer, expect_object, expect_string, invalid_value,
    invalid_variant, non_empty_string, non_negative_integer, nullable_uuid, optional,
    positive_integer, required, rfc3339_date_time, schema_version, sha256, string_array, uuid,
    uuid_array, validate_array_items, wire_variant,
};

/// `CapsuleValidation.status` values (ADR 0012 D4).
const VERDICTS: [&str; 4] = ["verified", "stale", "partial", "tampered"];

/// `CapsuleValidation.reasons` values (ADR 0012 D4).
const REASONS: [&str; 11] = [
    "head_changed",
    "index_changed",
    "worktree_changed",
    "object_missing",
    "object_digest_mismatch",
    "ledger_chain_invalid",
    "evidence_stale",
    "evidence_unavailable",
    "evidence_not_passed",
    "project_mismatch",
    "workspace_mismatch",
];

/// `TaskCapsule.damaged_records[].record_type` values (ADR 0013 D7), and
/// `session` (P4-C2, ontology v4): a Session's row or its Task link row.
const CAPSULE_RECORD_TYPES: [&str; 6] = [
    "event",
    "evidence",
    "snapshot",
    "checkpoint",
    "task",
    "session",
];

/// PRD FR-203 outcomes of a command or test Evidence object.
const RUN_STATUSES: [&str; 4] = ["PASS", "FAIL", "NOT_RUN", "UNAVAILABLE"];

/// Kinds of a typed Evidence object.
const EVIDENCE_KINDS: [&str; 5] = [
    "git_head",
    "git_worktree",
    "file_digest",
    "command_result",
    "test_result",
];

/// `CheckpointStatement` classifications.
const CLASSIFICATIONS: [&str; 4] = [
    "verified_fact",
    "user_supplied_context",
    "recommendation",
    "unknown",
];

/// Checkpoint sections.
const SECTIONS: [&str; 3] = ["verified_completed_work", "open_items", "next_action"];

/// `RecoveryPreview.gaps` values.
const PREVIEW_GAPS: [&str; 11] = [
    "no_checkpoint",
    "no_verified_fact",
    "statement_without_evidence",
    "record_integrity",
    "ledger_chain_invalid",
    "workspace_unavailable",
    "workspace_identity",
    "base_commit_unavailable",
    "untracked_files_not_captured",
    "sessions_truncated",
    "events_truncated",
];

/// `TaskCapsule.gaps` values (ADR 0013 D4): the Preview's without the two
/// truncation gaps, and `base_commits_over_limit`.
const CAPSULE_GAPS: [&str; 10] = [
    "no_checkpoint",
    "no_verified_fact",
    "statement_without_evidence",
    "record_integrity",
    "ledger_chain_invalid",
    "workspace_unavailable",
    "workspace_identity",
    "base_commit_unavailable",
    "untracked_files_not_captured",
    "base_commits_over_limit",
];

/// `TaskCapsule.actions[].action`, in their order (ADR 0014 E3 as
/// implemented in P4-D; Owner B3: no `reaffirm`).
const CAPSULE_ACTIONS: [&str; 3] = ["complete_task", "begin_session", "cite_latest_run"];

/// `TaskCapsule.actions[].reasons` values: the Problem codes the writes are
/// refused with (Owner condition a).
const ACTION_REASONS: [&str; 4] = [
    "invalid_request",
    "project_workspace_mismatch",
    "invalid_transition",
    "evidence_stale",
];

/// Most Sessions a Preview lists.
const PREVIEW_SESSION_LIMIT: usize = 20;

/// Most Events after the Checkpoint a Preview lists.
const PREVIEW_EVENT_LIMIT: usize = 50;

type Object = [(String, JsonValue)];

fn one_of(value: &JsonValue, path: &str, allowed: &[&str]) -> ValidationResult {
    let raw = expect_string(value, path)?;
    if allowed.contains(&raw) {
        Ok(())
    } else {
        Err(invalid_variant(path, raw))
    }
}

fn nullable_one_of(value: &JsonValue, path: &str, allowed: &[&str]) -> ValidationResult {
    if matches!(value, JsonValue::Null) {
        Ok(())
    } else {
        one_of(value, path, allowed)
    }
}

fn nullable_string(value: &JsonValue, path: &str) -> ValidationResult {
    if matches!(value, JsonValue::Null) {
        Ok(())
    } else {
        expect_string(value, path).map(|_| ())
    }
}

fn nullable_non_empty_string(value: &JsonValue, path: &str) -> ValidationResult {
    if matches!(value, JsonValue::Null) {
        Ok(())
    } else {
        non_empty_string(value, path)
    }
}

fn nullable_sha256(value: &JsonValue, path: &str) -> ValidationResult {
    if matches!(value, JsonValue::Null) {
        Ok(())
    } else {
        sha256(value, path)
    }
}

fn nullable_date_time(value: &JsonValue, path: &str) -> ValidationResult {
    if matches!(value, JsonValue::Null) {
        Ok(())
    } else {
        rfc3339_date_time(value, path)
    }
}

/// `PreviewReasons`: unique reason codes.
fn reasons(value: &JsonValue, path: &str) -> ValidationResult {
    ensure_unique(expect_array(value, path)?, path)?;
    validate_array_items(value, path, |reason, at| one_of(reason, at, &REASONS))
}

/// A verdict at `key` and its `reasons`.
fn verdict(object: &Object, key: &str, path: &str) -> ValidationResult {
    one_of(
        required(object, key, path)?,
        &format!("{path}.{key}"),
        &VERDICTS,
    )?;
    reasons(
        required(object, "reasons", path)?,
        &format!("{path}.reasons"),
    )
}

/// A member every known value of which is checked by `check`.
fn member(
    object: &Object,
    name: &str,
    path: &str,
    check: impl FnOnce(&JsonValue, &str) -> ValidationResult,
) -> ValidationResult {
    check(required(object, name, path)?, &format!("{path}.{name}"))
}

/// An optional member, checked when present.
fn optional_member(
    object: &Object,
    name: &str,
    path: &str,
    check: impl FnOnce(&JsonValue, &str) -> ValidationResult,
) -> ValidationResult {
    optional(object, name).map_or(Ok(()), |value| check(value, &format!("{path}.{name}")))
}

fn max_items(value: &JsonValue, path: &str, limit: usize) -> ValidationResult {
    if expect_array(value, path)?.len() > limit {
        Err(invalid_value(path, "too many items"))
    } else {
        Ok(())
    }
}

/// U9 K2 (Owner O6): the `--test-format` a test run was made with.
fn test_format(value: &JsonValue, path: &str) -> ValidationResult {
    one_of(value, path, &["cargo", "pytest", "cases"])
}

/// A non-empty argv: an array of at least one string.
fn argv(value: &JsonValue, path: &str) -> ValidationResult {
    string_array(value, path, false, true)
}

fn nullable_argv(value: &JsonValue, path: &str) -> ValidationResult {
    if matches!(value, JsonValue::Null) {
        Ok(())
    } else {
        argv(value, path)
    }
}

/// `repo://` paths, unique.
fn repo_paths(value: &JsonValue, path: &str) -> ValidationResult {
    string_array(value, path, true, false)?;
    validate_array_items(value, path, |item, at| {
        if expect_string(item, at)?.starts_with("repo://") {
            Ok(())
        } else {
            Err(invalid_value(at, "expected a repo:// path"))
        }
    })
}

fn gaps(value: &JsonValue, path: &str, allowed: &[&str]) -> ValidationResult {
    string_array(value, path, true, false)?;
    validate_array_items(value, path, |gap, at| one_of(gap, at, allowed))
}

fn task_state(value: &JsonValue, path: &str) -> ValidationResult {
    wire_variant::<TaskState>(value, path.trim_start_matches("$.")).map(|_| ())
}

/// The probe binding shared by both documents: `workspace_id` and
/// `available`, and the observation's members when present.
fn probe_binding(value: &JsonValue, mode: DecodeMode, path: &str) -> ValidationResult {
    let fields = ["workspace_id", "available"];
    let allowed = [
        "workspace_id",
        "available",
        "head_oid",
        "branch",
        "is_dirty",
        "capture_manifest_digest",
        "same_repository",
        "same_workspace",
    ];
    let probe = checked_object(value, &fields, &allowed, mode, path)?;
    member(probe, "workspace_id", path, nullable_uuid)?;
    member(probe, "available", path, |value, at| {
        expect_bool(value, at).map(|_| ())
    })?;
    optional_member(probe, "head_oid", path, |value, at| {
        expect_string(value, at).map(|_| ())
    })?;
    optional_member(probe, "branch", path, nullable_string)?;
    for flag in ["is_dirty", "same_repository", "same_workspace"] {
        optional_member(probe, flag, path, |value, at| {
            expect_bool(value, at).map(|_| ())
        })?;
    }
    optional_member(probe, "capture_manifest_digest", path, sha256)
}

fn ledger_binding(value: &JsonValue, mode: DecodeMode, path: &str) -> ValidationResult {
    let fields = ["project_sequence", "chain_valid"];
    let ledger = checked_object(value, &fields, &fields, mode, path)?;
    member(ledger, "project_sequence", path, non_negative_integer)?;
    member(ledger, "chain_valid", path, |value, at| {
        expect_bool(value, at).map(|_| ())
    })
}

/// P3-C2 (Owner, T12 S-6): a `tampered` document holds no `verified` at any
/// level: no `verdict` or `status` member anywhere in it reads `verified`.
fn no_verified_when_tampered(document: &JsonValue) -> ValidationResult {
    fn walk(value: &JsonValue, path: &str) -> ValidationResult {
        match value {
            JsonValue::Object(members) => {
                for (name, member) in members {
                    let at = format!("{path}.{name}");
                    if matches!(name.as_str(), "verdict" | "status")
                        && member.as_str() == Some("verified")
                    {
                        return Err(invalid_value(
                            &at,
                            "a tampered document holds no verified verdict",
                        ));
                    }
                    walk(member, &at)?;
                }
                Ok(())
            }
            JsonValue::Array(items) => items
                .iter()
                .enumerate()
                .try_for_each(|(index, item)| walk(item, &format!("{path}[{index}]"))),
            _ => Ok(()),
        }
    }
    if document.get("status").and_then(JsonValue::as_str) == Some("tampered") {
        walk(document, "$")
    } else {
        Ok(())
    }
}

/// ADR 0012 D6: the `RecoveryPreview` document, every member checked
/// against `OpenAPI` (T11 S-3).
#[allow(clippy::too_many_lines)] // Every known member of one document.
pub(super) fn validate_recovery_preview(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let path = "$";
    let fields = [
        "schema_version",
        "ontology_version",
        "task_id",
        "status",
        "reasons",
        "bindings",
        "facts",
        "invalid_evidence_ids",
        "changed_paths",
        "changed_paths_complete",
        "gaps",
        "next_checks",
        "sessions",
        "events_since_checkpoint",
        "preview_digest",
    ];
    let object = checked_object(value, &fields, &fields, mode, path)?;
    schema_version(required(object, "schema_version", path)?)?;
    member(object, "ontology_version", path, non_empty_string)?;
    member(object, "task_id", path, uuid)?;
    verdict(object, "status", path)?;
    let bindings_fields = ["task", "ledger", "checkpoint", "snapshot", "probe"];
    let bindings = checked_object(
        required(object, "bindings", path)?,
        &bindings_fields,
        &bindings_fields,
        mode,
        "$.bindings",
    )?;
    let task_fields = ["task_id", "revision", "status"];
    let task_allowed = ["task_id", "revision", "status", "workspace_id"];
    let task = checked_object(
        required(bindings, "task", "$.bindings")?,
        &task_fields,
        &task_allowed,
        mode,
        "$.bindings.task",
    )?;
    member(task, "task_id", "$.bindings.task", uuid)?;
    member(task, "revision", "$.bindings.task", positive_integer)?;
    member(task, "status", "$.bindings.task", task_state)?;
    optional_member(task, "workspace_id", "$.bindings.task", nullable_uuid)?;
    ledger_binding(
        required(bindings, "ledger", "$.bindings")?,
        mode,
        "$.bindings.ledger",
    )?;
    let checkpoint = required(bindings, "checkpoint", "$.bindings")?;
    if !matches!(checkpoint, JsonValue::Null) {
        let at = "$.bindings.checkpoint";
        let fields = ["checkpoint_id", "digest", "intact"];
        let allowed = [
            "checkpoint_id",
            "digest",
            "intact",
            "project_event_sequence",
            "session_id",
            "objective",
            "created_at",
        ];
        let checkpoint = checked_object(checkpoint, &fields, &allowed, mode, at)?;
        member(checkpoint, "checkpoint_id", at, uuid)?;
        member(checkpoint, "digest", at, nullable_sha256)?;
        member(checkpoint, "intact", at, |value, at| {
            expect_bool(value, at).map(|_| ())
        })?;
        optional_member(
            checkpoint,
            "project_event_sequence",
            at,
            non_negative_integer,
        )?;
        optional_member(checkpoint, "session_id", at, uuid)?;
        optional_member(checkpoint, "objective", at, |value, at| {
            expect_string(value, at).map(|_| ())
        })?;
        optional_member(checkpoint, "created_at", at, rfc3339_date_time)?;
    }
    let snapshot = required(bindings, "snapshot", "$.bindings")?;
    if !matches!(snapshot, JsonValue::Null) {
        let at = "$.bindings.snapshot";
        let fields = [
            "snapshot_id",
            "workspace_id",
            "canonical_digest",
            "capture_manifest_digest",
            "head_oid",
            "branch",
            "is_dirty",
            "captured_at",
        ];
        let snapshot = checked_object(snapshot, &fields, &fields, mode, at)?;
        member(snapshot, "snapshot_id", at, uuid)?;
        member(snapshot, "workspace_id", at, uuid)?;
        member(snapshot, "canonical_digest", at, sha256)?;
        member(snapshot, "capture_manifest_digest", at, sha256)?;
        member(snapshot, "head_oid", at, |value, at| {
            expect_string(value, at).map(|_| ())
        })?;
        member(snapshot, "branch", at, nullable_string)?;
        member(snapshot, "is_dirty", at, |value, at| {
            expect_bool(value, at).map(|_| ())
        })?;
        member(snapshot, "captured_at", at, rfc3339_date_time)?;
    }
    probe_binding(
        required(bindings, "probe", "$.bindings")?,
        mode,
        "$.bindings.probe",
    )?;
    validate_array_items(required(object, "facts", path)?, "$.facts", |fact, at| {
        let fact_fields = [
            "claim",
            "classification",
            "text",
            "propagates",
            "verdict",
            "reasons",
            "evidence",
        ];
        let fact = checked_object(fact, &fact_fields, &fact_fields, mode, at)?;
        let claim_at = format!("{at}.claim");
        let claim_fields = ["checkpoint_id", "section", "ordinal"];
        let claim = checked_object(
            required(fact, "claim", at)?,
            &claim_fields,
            &claim_fields,
            mode,
            &claim_at,
        )?;
        member(claim, "checkpoint_id", &claim_at, uuid)?;
        member(claim, "section", &claim_at, |value, at| {
            one_of(value, at, &SECTIONS)
        })?;
        member(claim, "ordinal", &claim_at, non_negative_integer)?;
        member(fact, "classification", at, |value, at| {
            one_of(value, at, &CLASSIFICATIONS)
        })?;
        member(fact, "text", at, |value, at| {
            expect_string(value, at).map(|_| ())
        })?;
        member(fact, "propagates", at, |value, at| {
            expect_bool(value, at).map(|_| ())
        })?;
        verdict(fact, "verdict", at)?;
        validate_array_items(
            required(fact, "evidence", at)?,
            &format!("{at}.evidence"),
            |item, item_at| {
                let item_fields = [
                    "evidence_id",
                    "kind",
                    "canonical_digest",
                    "run_status",
                    "argv",
                    "verdict",
                    "reasons",
                ];
                let item = checked_object(item, &item_fields, &item_fields, mode, item_at)?;
                member(item, "evidence_id", item_at, uuid)?;
                member(item, "kind", item_at, |value, at| {
                    one_of(value, at, &EVIDENCE_KINDS)
                })?;
                member(item, "canonical_digest", item_at, sha256)?;
                // P2-E: the FR-203 outcome, null for repository Evidence.
                member(item, "run_status", item_at, |value, at| {
                    nullable_one_of(value, at, &RUN_STATUSES)
                })?;
                // P2-F: the stored argv of a command or test, null otherwise.
                member(item, "argv", item_at, nullable_argv)?;
                verdict(item, "verdict", item_at)
            },
        )
    })?;
    uuid_array(
        required(object, "invalid_evidence_ids", path)?,
        "$.invalid_evidence_ids",
    )?;
    member(object, "changed_paths", path, repo_paths)?;
    member(object, "changed_paths_complete", path, |value, at| {
        expect_bool(value, at).map(|_| ())
    })?;
    member(object, "gaps", path, |value, at| {
        gaps(value, at, &PREVIEW_GAPS)
    })?;
    validate_array_items(
        required(object, "next_checks", path)?,
        "$.next_checks",
        |check, at| {
            let fields = ["action", "evidence_id", "argv", "workspace_id", "at_head"];
            let check = checked_object(check, &fields, &fields, mode, at)?;
            member(check, "action", at, |value, at| {
                one_of(value, at, &["rerun"])
            })?;
            member(check, "evidence_id", at, uuid)?;
            member(check, "argv", at, argv)?;
            member(check, "workspace_id", at, nullable_uuid)?;
            member(check, "at_head", at, nullable_string)
        },
    )?;
    let sessions = required(object, "sessions", path)?;
    max_items(sessions, "$.sessions", PREVIEW_SESSION_LIMIT)?;
    validate_array_items(sessions, "$.sessions", |session, at| {
        let fields = ["session_id", "workspace_id", "started_at", "ended_at"];
        let session = checked_object(session, &fields, &fields, mode, at)?;
        member(session, "session_id", at, uuid)?;
        member(session, "workspace_id", at, uuid)?;
        member(session, "started_at", at, rfc3339_date_time)?;
        member(session, "ended_at", at, nullable_date_time)
    })?;
    let events = required(object, "events_since_checkpoint", path)?;
    max_items(events, "$.events_since_checkpoint", PREVIEW_EVENT_LIMIT)?;
    validate_array_items(events, "$.events_since_checkpoint", |event, at| {
        let fields = [
            "project_sequence",
            "event_type",
            "occurred_at",
            "recorded_at",
            "producer",
        ];
        let event = checked_object(event, &fields, &fields, mode, at)?;
        member(event, "project_sequence", at, positive_integer)?;
        member(event, "event_type", at, |value, at| {
            expect_string(value, at).map(|_| ())
        })?;
        member(event, "occurred_at", at, nullable_date_time)?;
        member(event, "recorded_at", at, rfc3339_date_time)?;
        let producer_at = format!("{at}.producer");
        let producer_fields = ["name", "version"];
        let producer = checked_object(
            required(event, "producer", at)?,
            &producer_fields,
            &producer_fields,
            mode,
            &producer_at,
        )?;
        for field in producer_fields {
            member(producer, field, &producer_at, |value, at| {
                expect_string(value, at).map(|_| ())
            })?;
        }
        Ok(())
    })?;
    no_verified_when_tampered(value)?;
    member(object, "preview_digest", path, sha256)
}

/// ADR 0013 D1: `<checkpoint_id>/<section>/<ordinal>` or
/// `decision/<event_id>`.
pub(super) fn claim_id(value: &JsonValue, path: &str) -> ValidationResult {
    let raw = expect_string(value, path)?;
    let invalid = || invalid_value(path, "expected a claim ID");
    if let Some(event_id) = raw.strip_prefix("decision/") {
        return uuid(&JsonValue::String(event_id.to_owned()), path).map_err(|_| invalid());
    }
    let mut parts = raw.split('/');
    let (Some(checkpoint_id), Some(section), Some(ordinal), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    uuid(&JsonValue::String(checkpoint_id.to_owned()), path).map_err(|_| invalid())?;
    let canonical = !ordinal.is_empty()
        && ordinal.bytes().all(|byte| byte.is_ascii_digit())
        && (ordinal.len() == 1 || !ordinal.starts_with('0'));
    if SECTIONS.contains(&section) && canonical {
        Ok(())
    } else {
        Err(invalid())
    }
}

/// An array of unique positive integers, at least one when `non_empty`.
fn numbers(value: &JsonValue, path: &str, non_empty: bool) -> ValidationResult {
    let values = expect_array(value, path)?;
    if non_empty && values.is_empty() {
        return Err(invalid_value(path, "expected a non-empty array"));
    }
    ensure_unique(values, path)?;
    validate_array_items(value, path, positive_integer)
}

fn uuids(value: &JsonValue, path: &str, non_empty: bool) -> ValidationResult {
    if non_empty && expect_array(value, path)?.is_empty() {
        return Err(invalid_value(path, "expected a non-empty array"));
    }
    validate_array_items(value, path, uuid)
}

/// The `args` members each template requires (ADR 0013 D5).
fn template_args(template: &str) -> Option<&'static [&'static str]> {
    Some(match template {
        "header" => &[
            "title",
            "task_status",
            "project_sequence",
            "head_oid",
            "withdrawn_facts",
        ],
        "ledger_chain_invalid" => &[],
        "damaged_record" => &["record_type"],
        "session_open" | "session_crashed" => &["workspace_id", "started_at"],
        "open_item" | "next_action" => &["number", "text"],
        "fact" => &["number", "text", "base_commits", "head_oid"],
        "rerun" => &[
            "argv",
            "evidence_ids",
            "numbers",
            "workspace_id",
            "head_oid",
        ],
        "reaffirm" => &[
            "argv",
            "evidence_ids",
            "numbers",
            "verified_evidence_id",
            "verified_snapshot_id",
        ],
        "decision" => &["number", "text", "rationale"],
        "superseded" => &["number", "text", "superseded_by"],
        "withdrawn" => &["number", "text", "superseded_by", "withdrawal_reason"],
        _ => return None,
    })
}

/// The classes a template may take (ADR 0013 D5): `fact` is class 4 when
/// not verified and class 6 when verified.
fn template_classes(template: &str) -> &'static [u64] {
    match template {
        "header" => &[0],
        "ledger_chain_invalid" | "damaged_record" => &[1],
        "session_open" | "session_crashed" => &[2],
        "open_item" => &[3],
        "fact" => &[4, 6],
        "rerun" | "reaffirm" => &[5],
        "decision" => &[7],
        "next_action" => &[8],
        _ => &[9],
    }
}

const ARG_MEMBERS: [&str; 19] = [
    "title",
    "task_status",
    "project_sequence",
    "head_oid",
    "record_type",
    "workspace_id",
    "started_at",
    "number",
    "text",
    "base_commits",
    "argv",
    "evidence_ids",
    "numbers",
    "verified_evidence_id",
    "verified_snapshot_id",
    "rationale",
    "superseded_by",
    "withdrawn_facts",
    "withdrawal_reason",
];

/// Each known member of a continuation item's `args`.
fn arg(name: &str, value: &JsonValue, at: &str) -> ValidationResult {
    match name {
        "title" => expect_string(value, at).map(|_| ()),
        "task_status" => task_state(value, at),
        "project_sequence" | "withdrawn_facts" => non_negative_integer(value, at),
        "head_oid" | "rationale" => nullable_string(value, at),
        "withdrawal_reason" => nullable_non_empty_string(value, at),
        "record_type" => one_of(value, at, &CAPSULE_RECORD_TYPES),
        "workspace_id" => nullable_uuid(value, at),
        "started_at" => rfc3339_date_time(value, at),
        "number" => positive_integer(value, at),
        "text" => non_empty_string(value, at),
        "base_commits" => {
            string_array(value, at, false, false)?;
            validate_array_items(value, at, non_empty_string)
        }
        "argv" => argv(value, at),
        "evidence_ids" => uuids(value, at, true),
        "numbers" => numbers(value, at, true),
        "verified_evidence_id" | "verified_snapshot_id" => uuid(value, at),
        "superseded_by" => numbers(value, at, false),
        _ => Ok(()),
    }
}

fn continuation_item(
    item: &JsonValue,
    mode: DecodeMode,
    at: &str,
    previous_class: &mut u64,
) -> ValidationResult {
    let fields = [
        "class",
        "template",
        "object_id",
        "session_id",
        "verdict",
        "reasons",
        "standing",
        "args",
    ];
    let item = checked_object(item, &fields, &fields, mode, at)?;
    let class_at = format!("{at}.class");
    let class_value = required(item, "class", at)?;
    let class = expect_integer(class_value, &class_at)?;
    if class.compare_to(9) == Ordering::Greater || class.negative {
        return Err(invalid_value(
            &class_at,
            "expected a class from 0 through 9",
        ));
    }
    let class = (0..=9)
        .find(|candidate| class.compare_to(*candidate) == Ordering::Equal)
        .ok_or_else(|| invalid_value(&class_at, "expected a class from 0 through 9"))?;
    if class < *previous_class {
        return Err(invalid_value(&class_at, "classes must not decrease"));
    }
    *previous_class = class;
    let template_at = format!("{at}.template");
    let template = expect_string(required(item, "template", at)?, &template_at)?;
    let required_args =
        template_args(template).ok_or_else(|| invalid_variant(&template_at, template))?;
    if !template_classes(template).contains(&class) {
        return Err(invalid_value(
            &class_at,
            "the template does not take this class",
        ));
    }
    member(item, "object_id", at, non_empty_string)?;
    member(item, "session_id", at, nullable_uuid)?;
    let verdict_value = required(item, "verdict", at)?;
    nullable_one_of(verdict_value, &format!("{at}.verdict"), &VERDICTS)?;
    // D5: "verified" only on a verified item, and a fact's class says which.
    if template == "fact" && (class == 6) != (verdict_value.as_str() == Some("verified")) {
        return Err(invalid_value(
            &class_at,
            "a fact is class 6 exactly when its verdict is verified",
        ));
    }
    member(item, "reasons", at, reasons)?;
    member(item, "standing", at, |value, at| {
        nullable_one_of(value, at, &["active", "superseded", "withdrawn"])
    })?;
    let args_at = format!("{at}.args");
    let args = checked_object(
        required(item, "args", at)?,
        required_args,
        &ARG_MEMBERS,
        mode,
        &args_at,
    )?;
    for (name, value) in args {
        if ARG_MEMBERS.contains(&name.as_str()) {
            arg(name, value, &format!("{args_at}.{name}"))?;
        }
    }
    Ok(())
}

fn capsule_claim(claim: &JsonValue, mode: DecodeMode, at: &str) -> ValidationResult {
    let fields = [
        "number",
        "claim_id",
        "kind",
        "classification",
        "text",
        "rationale",
        "session_id",
        "checkpoint_id",
        "project_sequence",
        "standing",
        "withdrawal_reason",
        "superseded_by",
        "propagates",
        "damaged",
        "verdict",
        "reasons",
        "evidence",
        "grounding_digest",
    ];
    // U8 A6 (Owner O4): a withdrawn claim's optional `withdrawn_by`.
    let allowed = [&fields[..], &["withdrawn_by"]].concat();
    let claim = checked_object(claim, &fields, &allowed, mode, at)?;
    member(claim, "number", at, positive_integer)?;
    member(claim, "claim_id", at, claim_id)?;
    member(claim, "kind", at, |value, at| {
        one_of(value, at, &["fact", "open_item", "next_action", "decision"])
    })?;
    member(claim, "classification", at, |value, at| {
        nullable_one_of(value, at, &CLASSIFICATIONS)
    })?;
    member(claim, "text", at, non_empty_string)?;
    member(claim, "rationale", at, nullable_string)?;
    member(claim, "session_id", at, uuid)?;
    member(claim, "checkpoint_id", at, nullable_uuid)?;
    member(claim, "project_sequence", at, positive_integer)?;
    member(claim, "standing", at, |value, at| {
        one_of(value, at, &["active", "superseded", "withdrawn"])
    })?;
    // P3-D2: only a withdrawn claim carries a reason.
    let reason_at = format!("{at}.withdrawal_reason");
    let reason = required(claim, "withdrawal_reason", at)?;
    nullable_non_empty_string(reason, &reason_at)?;
    if !matches!(reason, JsonValue::Null)
        && required(claim, "standing", at)?.as_str() != Some("withdrawn")
    {
        return Err(invalid_value(
            &reason_at,
            "only a withdrawn claim carries a withdrawal reason",
        ));
    }
    withdrawn_by(claim, mode, at)?;
    validate_array_items(
        required(claim, "superseded_by", at)?,
        &format!("{at}.superseded_by"),
        |by, by_at| {
            let by_fields = ["number", "claim_id"];
            let by = checked_object(by, &by_fields, &by_fields, mode, by_at)?;
            member(by, "number", by_at, positive_integer)?;
            member(by, "claim_id", by_at, claim_id)
        },
    )?;
    for flag in ["propagates", "damaged"] {
        member(claim, flag, at, |value, at| {
            expect_bool(value, at).map(|_| ())
        })?;
    }
    verdict(claim, "verdict", at)?;
    validate_array_items(
        required(claim, "evidence", at)?,
        &format!("{at}.evidence"),
        |item, item_at| {
            let item_fields = [
                "evidence_id",
                "kind",
                "canonical_digest",
                "run_status",
                "argv",
                "snapshot_id",
                "base_commit",
                "verdict",
                "reasons",
            ];
            // U9 K2 (Owner O6): the optional `test_format` of a test run
            // that recorded one.
            let allowed = [&item_fields[..], &["test_format"]].concat();
            let item = checked_object(item, &item_fields, &allowed, mode, item_at)?;
            optional_member(item, "test_format", item_at, test_format)?;
            member(item, "evidence_id", item_at, uuid)?;
            member(item, "kind", item_at, |value, at| {
                nullable_one_of(value, at, &EVIDENCE_KINDS)
            })?;
            member(item, "canonical_digest", item_at, nullable_sha256)?;
            member(item, "run_status", item_at, |value, at| {
                nullable_one_of(value, at, &RUN_STATUSES)
            })?;
            member(item, "argv", item_at, nullable_argv)?;
            member(item, "snapshot_id", item_at, nullable_uuid)?;
            member(item, "base_commit", item_at, nullable_string)?;
            verdict(item, "verdict", item_at)
        },
    )?;
    member(claim, "grounding_digest", at, sha256)
}

/// U8 A6 (Owner O4): a claim's optional `withdrawn_by`: only on a withdrawn
/// claim, `null` or `{checkpoint_id, session_id}`.
fn withdrawn_by(claim: &Object, mode: DecodeMode, at: &str) -> ValidationResult {
    let Some(by) = optional(claim, "withdrawn_by") else {
        return Ok(());
    };
    let by_at = format!("{at}.withdrawn_by");
    if required(claim, "standing", at)?.as_str() != Some("withdrawn") {
        return Err(invalid_value(
            &by_at,
            "only a withdrawn claim names what withdrew it",
        ));
    }
    if matches!(by, JsonValue::Null) {
        return Ok(());
    }
    let by_fields = ["checkpoint_id", "session_id"];
    let by = checked_object(by, &by_fields, &by_fields, mode, &by_at)?;
    member(by, "checkpoint_id", &by_at, uuid)?;
    member(by, "session_id", &by_at, uuid)
}

/// U8 A2 (Owner O4): a Session's `agent` as the Capsule shows it:
/// `{name, role, parent_session_id}`, the name 1 to 64 ASCII letters,
/// digits, `_`, or `-`, the role one of three, the parent a `UUIDv7` or
/// `null`.
fn session_agent(value: &JsonValue, mode: DecodeMode, at: &str) -> ValidationResult {
    let fields = ["name", "role", "parent_session_id"];
    let agent = checked_object(value, &fields, &fields, mode, at)?;
    member(agent, "name", at, |value, at| {
        let name = expect_string(value, at)?;
        if (1..=64).contains(&name.len())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            Ok(())
        } else {
            Err(invalid_value(
                at,
                "expected 1 to 64 ASCII letters, digits, _ or -",
            ))
        }
    })?;
    member(agent, "role", at, |value, at| {
        one_of(value, at, &["orchestrator", "sub_agent", "human"])
    })?;
    member(agent, "parent_session_id", at, nullable_uuid)
}

/// U9 A7 (Owner O3): who ended an abandoned Session, as the Capsule shows
/// it: `session_id` (a `UUIDv7`) and `agent` (as [`session_agent`]), one
/// or both, and nothing else.
fn session_ended_by(value: &JsonValue, mode: DecodeMode, at: &str) -> ValidationResult {
    let ended_by = checked_object(value, &[], &["session_id", "agent"], mode, at)?;
    if ended_by.is_empty() {
        return Err(invalid_value(at, "expected session_id, agent, or both"));
    }
    optional_member(ended_by, "session_id", at, uuid)?;
    optional_member(ended_by, "agent", at, |value, at| {
        session_agent(value, mode, at)
    })
}

/// ADR 0014 E6 (P4-F): a Session's `resumed_from`, null or the Pack it
/// declared it continues from, `declared: true`.
fn session_resumed_from(value: &JsonValue, mode: DecodeMode, at: &str) -> ValidationResult {
    if matches!(value, JsonValue::Null) {
        return Ok(());
    }
    let fields = [
        "capsule_digest",
        "project_sequence",
        "pack_digest",
        "budget",
        "declared",
    ];
    let from = checked_object(value, &fields, &fields, mode, at)?;
    member(from, "capsule_digest", at, sha256)?;
    member(from, "pack_digest", at, sha256)?;
    member(from, "project_sequence", at, non_negative_integer)?;
    member(from, "budget", at, |value, at| {
        let budget = expect_integer(value, at)?;
        if budget.compare_to(1_000) == Ordering::Less
            || budget.compare_to(1_000_000) == Ordering::Greater
        {
            return Err(invalid_value(
                at,
                "expected a Pack budget, 1000 through 1000000",
            ));
        }
        Ok(())
    })?;
    member(from, "declared", at, |value, at| {
        if expect_bool(value, at)? {
            Ok(())
        } else {
            Err(invalid_value(
                at,
                "expected true: the Session's own declaration",
            ))
        }
    })
}

/// The `args` members of each action (ADR 0014 E3 as implemented, P4-D).
fn action_args(action: &str, args: &JsonValue, mode: DecodeMode, at: &str) -> ValidationResult {
    match action {
        "complete_task" => {
            let fields = [
                "operation",
                "target_state",
                "expected_revision",
                "completion_evidence_ids",
                "workspace_id",
            ];
            let args = checked_object(args, &fields, &fields, mode, at)?;
            member(args, "operation", at, |value, at| {
                one_of(value, at, &["transitionTask"])
            })?;
            member(args, "target_state", at, |value, at| {
                one_of(value, at, &["completed"])
            })?;
            member(args, "expected_revision", at, positive_integer)?;
            member(args, "completion_evidence_ids", at, |value, at| {
                ensure_unique(expect_array(value, at)?, at)?;
                uuids(value, at, false)
            })?;
            member(args, "workspace_id", at, nullable_uuid)
        }
        "begin_session" => {
            let fields = [
                "operation",
                "task_id",
                "workspace_id",
                "task_status",
                "open_session_ids",
            ];
            let args = checked_object(args, &fields, &fields, mode, at)?;
            member(args, "operation", at, |value, at| {
                one_of(value, at, &["createSession"])
            })?;
            member(args, "task_id", at, uuid)?;
            member(args, "workspace_id", at, nullable_uuid)?;
            member(args, "task_status", at, task_state)?;
            member(args, "open_session_ids", at, |value, at| {
                ensure_unique(expect_array(value, at)?, at)?;
                uuids(value, at, false)
            })
        }
        _ => {
            let fields = [
                "operation",
                "evidence_id",
                "snapshot_id",
                "workspace_id",
                "kind",
                "run_status",
                "argv",
                "overruled_by",
            ];
            // U9 K2 (Owner O6): the optional `test_format` of a latest run
            // that is a test run that recorded one.
            let allowed = [&fields[..], &["test_format"]].concat();
            let args = checked_object(args, &fields, &allowed, mode, at)?;
            optional_member(args, "test_format", at, test_format)?;
            member(args, "operation", at, |value, at| {
                one_of(value, at, &["createCheckpoint"])
            })?;
            for name in ["evidence_id", "snapshot_id", "workspace_id", "overruled_by"] {
                member(args, name, at, nullable_uuid)?;
            }
            member(args, "kind", at, |value, at| {
                nullable_one_of(value, at, &["command_result", "test_result"])
            })?;
            member(args, "run_status", at, |value, at| {
                nullable_one_of(value, at, &RUN_STATUSES)
            })?;
            member(args, "argv", at, nullable_argv)
        }
    }
}

/// ADR 0014 E3 (P4-D): `actions[]`, the three actions in their order;
/// `available` exactly when `reasons` is empty; the scope a write needs.
fn capsule_actions(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let items = expect_array(value, "$.actions")?;
    if items.len() != CAPSULE_ACTIONS.len() {
        return Err(invalid_value(
            "$.actions",
            "expected complete_task, begin_session, and cite_latest_run",
        ));
    }
    for (index, (item, expected)) in items.iter().zip(CAPSULE_ACTIONS).enumerate() {
        let at = format!("$.actions[{index}]");
        let fields = ["action", "available", "reasons", "scope", "args"];
        let action = checked_object(item, &fields, &fields, mode, &at)?;
        member(action, "action", &at, |value, at| {
            one_of(value, at, &[expected])
        })?;
        let reasons = required(action, "reasons", &at)?;
        member(action, "reasons", &at, |value, at| {
            ensure_unique(expect_array(value, at)?, at)?;
            validate_array_items(value, at, |reason, at| one_of(reason, at, &ACTION_REASONS))
        })?;
        let available = expect_bool(required(action, "available", &at)?, &at)?;
        if available != expect_array(reasons, &at)?.is_empty() {
            return Err(invalid_value(
                &format!("{at}.available"),
                "an action is available exactly when it lists no reason",
            ));
        }
        member(action, "scope", &at, |value, at| {
            one_of(value, at, &["mcp-write"])
        })?;
        action_args(
            expected,
            required(action, "args", &at)?,
            mode,
            &format!("{at}.args"),
        )?;
    }
    Ok(())
}

/// ADR 0013 D4/D5: the `TaskCapsule` document, every member checked.
#[allow(clippy::too_many_lines)] // Every known member of one document.
pub(super) fn validate_task_capsule(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let path = "$";
    let fields = [
        "schema_version",
        "ontology_version",
        "task",
        "status",
        "reasons",
        "bindings",
        "sessions",
        "claims",
        "rerun",
        "changed_paths_by_base",
        "gaps",
        "damaged_records",
        "continuation",
        "omitted",
        "capsule_digest",
        "actions",
    ];
    let object = checked_object(value, &fields, &fields, mode, path)?;
    schema_version(required(object, "schema_version", path)?)?;
    member(object, "ontology_version", path, non_empty_string)?;
    capsule_actions(required(object, "actions", path)?, mode)?;
    let task_fields = ["task_id", "title", "revision", "status", "workspace_id"];
    let task = checked_object(
        required(object, "task", path)?,
        &task_fields,
        &task_fields,
        mode,
        "$.task",
    )?;
    member(task, "task_id", "$.task", uuid)?;
    member(task, "title", "$.task", |value, at| {
        expect_string(value, at).map(|_| ())
    })?;
    member(task, "revision", "$.task", positive_integer)?;
    member(task, "status", "$.task", task_state)?;
    member(task, "workspace_id", "$.task", nullable_uuid)?;
    verdict(object, "status", path)?;
    let bindings_fields = ["ledger", "probe"];
    let bindings = checked_object(
        required(object, "bindings", path)?,
        &bindings_fields,
        &bindings_fields,
        mode,
        "$.bindings",
    )?;
    ledger_binding(
        required(bindings, "ledger", "$.bindings")?,
        mode,
        "$.bindings.ledger",
    )?;
    probe_binding(
        required(bindings, "probe", "$.bindings")?,
        mode,
        "$.bindings.probe",
    )?;
    validate_array_items(
        required(object, "sessions", path)?,
        "$.sessions",
        |session, at| {
            let fields = [
                "session_id",
                "workspace_id",
                "started_at",
                "ended_at",
                "end_state",
                "last_recorded_at",
                "resumed_from",
            ];
            // U8 A2 (Owner O4): the optional `agent` of a Session that
            // declared one; U9 A7 (Owner O3): the optional `ended_by` of
            // one ended as abandoned by an ender that reported itself.
            let allowed = [&fields[..], &["agent", "ended_by"]].concat();
            let session = checked_object(session, &fields, &allowed, mode, at)?;
            member(session, "resumed_from", at, |value, at| {
                session_resumed_from(value, mode, at)
            })?;
            optional_member(session, "agent", at, |value, at| {
                session_agent(value, mode, at)
            })?;
            optional_member(session, "ended_by", at, |value, at| {
                session_ended_by(value, mode, at)
            })?;
            member(session, "session_id", at, uuid)?;
            member(session, "workspace_id", at, uuid)?;
            member(session, "started_at", at, rfc3339_date_time)?;
            member(session, "ended_at", at, nullable_date_time)?;
            member(session, "end_state", at, |value, at| {
                one_of(value, at, &["open", "ended", "crashed"])
            })?;
            // P3-L L7: when the ledger recorded the Session's latest Event.
            member(session, "last_recorded_at", at, nullable_date_time)
        },
    )?;
    let claims = required(object, "claims", path)?;
    validate_array_items(claims, "$.claims", |claim, at| {
        capsule_claim(claim, mode, at)
    })?;
    // Numbers are 1, 2, 3, … in order (ADR 0013 D1, Owner revision a).
    for (index, claim) in expect_array(claims, "$.claims")?.iter().enumerate() {
        let at = format!("$.claims[{index}].number");
        let number = expect_integer(required(expect_object(claim, &at)?, "number", &at)?, &at)?;
        if number.compare_to(u64::try_from(index + 1).unwrap_or(u64::MAX)) != Ordering::Equal {
            return Err(invalid_value(
                &at,
                "claims are numbered 1, 2, 3, … in order",
            ));
        }
    }
    validate_array_items(required(object, "rerun", path)?, "$.rerun", |rerun, at| {
        let fields = [
            "argv",
            "evidence_ids",
            "claims",
            "verdict",
            "reasons",
            "workspace_id",
            "at_head",
        ];
        let rerun = checked_object(rerun, &fields, &fields, mode, at)?;
        member(rerun, "argv", at, argv)?;
        member(rerun, "evidence_ids", at, |value, at| {
            ensure_unique(expect_array(value, at)?, at)?;
            uuids(value, at, true)
        })?;
        member(rerun, "claims", at, |value, at| numbers(value, at, true))?;
        verdict(rerun, "verdict", at)?;
        member(rerun, "workspace_id", at, nullable_uuid)?;
        member(rerun, "at_head", at, nullable_string)
    })?;
    validate_array_items(
        required(object, "changed_paths_by_base", path)?,
        "$.changed_paths_by_base",
        |change, at| {
            let fields = ["base_commit", "probed", "complete", "paths"];
            let change = checked_object(change, &fields, &fields, mode, at)?;
            member(change, "base_commit", at, non_empty_string)?;
            for flag in ["probed", "complete"] {
                member(change, flag, at, |value, at| {
                    expect_bool(value, at).map(|_| ())
                })?;
            }
            member(change, "paths", at, repo_paths)
        },
    )?;
    member(object, "gaps", path, |value, at| {
        gaps(value, at, &CAPSULE_GAPS)
    })?;
    validate_array_items(
        required(object, "damaged_records", path)?,
        "$.damaged_records",
        |record, at| {
            let fields = ["record_type", "record_id", "reason"];
            let record = checked_object(record, &fields, &fields, mode, at)?;
            member(record, "record_type", at, |value, at| {
                one_of(value, at, &CAPSULE_RECORD_TYPES)
            })?;
            member(record, "record_id", at, non_empty_string)?;
            member(record, "reason", at, |value, at| {
                one_of(value, at, &["object_missing", "object_digest_mismatch"])
            })
        },
    )?;
    let continuation = required(object, "continuation", path)?;
    if expect_array(continuation, "$.continuation")?.is_empty() {
        return Err(invalid_value(
            "$.continuation",
            "expected a non-empty array",
        ));
    }
    let mut previous_class = 0;
    validate_array_items(continuation, "$.continuation", |item, at| {
        continuation_item(item, mode, at, &mut previous_class)
    })?;
    let omitted = required(object, "omitted", path)?;
    max_items(omitted, "$.omitted", 0)?;
    no_verified_when_tampered(value)?;
    member(object, "capsule_digest", path, sha256)
}
