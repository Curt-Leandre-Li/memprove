//! The response validators of `RecoveryPreview` (every member, T11 S-3) and
//! `TaskCapsule` (ADR 0013 D4/D5), and the `getTaskCapsule` binding.

use std::collections::BTreeSet;

use memprove_protocol::phase1::{
    OperationId, ProblemCode, binding_for, validate_request, validate_response,
};
use memprove_protocol::strict_json::parse_strict;

const ID: &str = "018f47a0-0000-7000-8000-000000000001";
const OTHER: &str = "018f47a0-0000-7000-8000-000000000002";
const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const X: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const Y: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A Preview with every optional member present.
fn preview() -> String {
    format!(
        r#"{{"schema_version":1,"ontology_version":"continuity.work-state-ontology.v3",
        "task_id":"{ID}","status":"stale","reasons":["head_changed","evidence_stale"],
        "bindings":{{
            "task":{{"task_id":"{ID}","revision":2,"status":"in_progress","workspace_id":null}},
            "ledger":{{"project_sequence":7,"chain_valid":true}},
            "checkpoint":{{"checkpoint_id":"{ID}","digest":"{DIGEST}","intact":true,
                "project_event_sequence":5,"session_id":"{ID}","objective":"o",
                "created_at":"2026-10-03T10:00:00Z"}},
            "snapshot":{{"snapshot_id":"{ID}","workspace_id":"{ID}","canonical_digest":"{DIGEST}",
                "capture_manifest_digest":"{DIGEST}","head_oid":"{X}","branch":"main",
                "is_dirty":false,"captured_at":"2026-10-03T10:00:00Z"}},
            "probe":{{"workspace_id":"{ID}","available":true,"head_oid":"{Y}","branch":null,
                "is_dirty":false,"capture_manifest_digest":"{DIGEST}","same_repository":true,
                "same_workspace":true}}}},
        "facts":[{{"claim":{{"checkpoint_id":"{ID}","section":"open_items","ordinal":0}},
            "classification":"unknown","text":"t","propagates":false,"verdict":"stale",
            "reasons":["head_changed","evidence_stale"],
            "evidence":[{{"evidence_id":"{ID}","kind":"test_result","canonical_digest":"{DIGEST}",
                "run_status":"PASS","argv":["sh","t"],"verdict":"stale",
                "reasons":["head_changed"]}}]}}],
        "invalid_evidence_ids":[],"changed_paths":["repo://a"],"changed_paths_complete":true,
        "gaps":["no_verified_fact"],
        "next_checks":[{{"action":"rerun","evidence_id":"{ID}","argv":["sh","t"],
            "workspace_id":"{ID}","at_head":"{Y}"}}],
        "sessions":[{{"session_id":"{ID}","workspace_id":"{ID}",
            "started_at":"2026-10-03T10:00:00Z","ended_at":null}}],
        "events_since_checkpoint":[{{"project_sequence":7,"event_type":"session.ended",
            "occurred_at":null,"recorded_at":"2026-10-03T10:00:00Z",
            "producer":{{"name":"continuityd","version":"0.1.0"}}}}],
        "preview_digest":"{DIGEST}"}}"#
    )
}

fn valid(op: OperationId, text: &str) {
    validate_response(op, 200, "application/json", &parse_strict(text).unwrap())
        .unwrap_or_else(|error| panic!("{error:?}\n{text}"));
}

fn invalid(op: OperationId, text: &str) {
    let parsed = parse_strict(text).unwrap_or_else(|error| panic!("{error:?}\n{text}"));
    assert!(
        validate_response(op, 200, "application/json", &parsed).is_err(),
        "accepted:\n{text}"
    );
}

/// T11 S-3: every member `OpenAPI` defines for `RecoveryPreview` is checked.
#[test]
#[allow(clippy::too_many_lines)] // Every invalid variant of one document.
fn every_recovery_preview_member_is_checked() {
    let op = OperationId::GetRecoveryPreview;
    let document = preview();
    valid(op, &document);
    // Nullable bindings and an unavailable probe with no observation.
    valid(
        op,
        &document
            .replacen(
                &format!(
                    r#""checkpoint":{{"checkpoint_id":"{ID}","digest":"{DIGEST}","intact":true,
                "project_event_sequence":5,"session_id":"{ID}","objective":"o",
                "created_at":"2026-10-03T10:00:00Z"}}"#
                ),
                r#""checkpoint":null"#,
                1,
            )
            .replacen(
                r#""is_dirty":false,"captured_at""#,
                r#""is_dirty":true,"captured_at""#,
                1,
            ),
    );
    let cases = [
        // bindings.task
        (r#""revision":2"#, r#""revision":0"#),
        (r#""status":"in_progress""#, r#""status":"working""#),
        (r#""workspace_id":null}"#, r#""workspace_id":"x"}"#),
        // bindings.checkpoint
        (r#""intact":true"#, r#""intact":"yes""#),
        (
            r#""project_event_sequence":5"#,
            r#""project_event_sequence":-1"#,
        ),
        (r#""objective":"o""#, r#""objective":1"#),
        (
            r#""created_at":"2026-10-03T10:00:00Z"}"#,
            r#""created_at":"yesterday"}"#,
        ),
        // bindings.snapshot
        (r#""branch":"main""#, r#""branch":1"#),
        (
            r#""is_dirty":false,"captured_at""#,
            r#""is_dirty":0,"captured_at""#,
        ),
        // bindings.probe
        (r#""same_repository":true"#, r#""same_repository":"true""#),
        (
            r#""capture_manifest_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","same_repository""#,
            r#""capture_manifest_digest":"x","same_repository""#,
        ),
        // facts
        (r#""section":"open_items""#, r#""section":"items""#),
        (r#""ordinal":0"#, r#""ordinal":-1"#),
        (
            r#""classification":"unknown""#,
            r#""classification":"fact""#,
        ),
        (r#""propagates":false"#, r#""propagates":null"#),
        (r#""kind":"test_result""#, r#""kind":"test""#),
        // gaps
        (
            r#""gaps":["no_verified_fact"]"#,
            r#""gaps":["unknown_gap"]"#,
        ),
        (
            r#""gaps":["no_verified_fact"]"#,
            r#""gaps":["no_verified_fact","no_verified_fact"]"#,
        ),
        (
            r#""gaps":["no_verified_fact"]"#,
            r#""gaps":["base_commits_over_limit"]"#,
        ),
        // next_checks
        (r#""action":"rerun""#, r#""action":"run""#),
        (
            r#""evidence_id":"018f47a0-0000-7000-8000-000000000001","argv":["sh","t"]"#,
            r#""evidence_id":"018f47a0-0000-7000-8000-000000000001","argv":[]"#,
        ),
        (r#""at_head":"bbbb"#, r#""at_head":1,"x":"bbbb"#),
        // sessions
        (r#""ended_at":null"#, r#""ended_at":"later""#),
        // events
        (
            r#""project_sequence":7,"event_type""#,
            r#""project_sequence":0,"event_type""#,
        ),
        (r#""occurred_at":null"#, r#""occurred_at":5"#),
        (r#""name":"continuityd""#, r#""name":1"#),
    ];
    for (from, to) in cases {
        assert!(document.contains(from), "{from}");
        let changed = document.replacen(from, to, 1);
        let parsed = parse_strict(&changed).unwrap_or_else(|error| panic!("{error:?}: {to}"));
        assert!(
            validate_response(op, 200, "application/json", &parsed).is_err(),
            "accepted: {from} -> {to}"
        );
    }
    // A missing required member of each nested object (renamed, since a
    // response may carry unknown members).
    for missing in [
        r#""propagates""#,
        r#""kind""#,
        r#""intact""#,
        r#""captured_at""#,
        r#""at_head""#,
        r#""end_state""#,
    ] {
        if document.contains(missing) {
            invalid(op, &document.replacen(missing, r#""renamed""#, 1));
        }
    }
    // At most 20 Sessions and 50 Events.
    let session = format!(
        r#"{{"session_id":"{ID}","workspace_id":"{ID}","started_at":"2026-10-03T10:00:00Z","ended_at":null}}"#
    );
    let extra = vec![session; 20].join(",");
    let crowded = document.replacen(r#""sessions":["#, &format!(r#""sessions":[{extra},"#), 1);
    assert_ne!(crowded, document);
    invalid(op, &crowded);
}

/// A Capsule with every kind of member: two claims, one rerun, one base, one
/// damaged record, and a continuation over several classes.
#[allow(clippy::too_many_lines)] // One whole document.
fn capsule() -> String {
    format!(
        r#"{{"schema_version":1,"ontology_version":"continuity.work-state-ontology.v3",
        "task":{{"task_id":"{ID}","title":"T","revision":2,"status":"in_progress","workspace_id":null}},
        "status":"stale","reasons":["object_missing"],
        "bindings":{{"ledger":{{"project_sequence":9,"chain_valid":true}},
            "probe":{{"workspace_id":"{ID}","available":true,"head_oid":"{Y}","branch":"main",
                "is_dirty":false,"capture_manifest_digest":"{DIGEST}","same_repository":true,
                "same_workspace":true}}}},
        "sessions":[{{"session_id":"{ID}","workspace_id":"{ID}",
            "started_at":"2026-10-03T10:00:00Z","ended_at":null,"end_state":"open",
            "last_recorded_at":"2026-10-03T10:05:00Z",
            "resumed_from":{{"capsule_digest":"{DIGEST}","project_sequence":7,
                "pack_digest":"{DIGEST}","budget":9000,"declared":true}}}}],
        "claims":[
            {{"number":1,"claim_id":"{ID}/verified_completed_work/0","kind":"fact",
              "classification":"verified_fact","text":"F","rationale":null,"session_id":"{ID}",
              "checkpoint_id":"{ID}","project_sequence":3,"standing":"superseded",
              "withdrawal_reason":null,
              "superseded_by":[{{"number":2,"claim_id":"decision/{OTHER}"}}],
              "propagates":true,"damaged":false,"verdict":"tampered",
              "reasons":["object_missing"],
              "evidence":[{{"evidence_id":"{ID}","kind":"test_result","canonical_digest":"{DIGEST}",
                  "run_status":"PASS","argv":["sh","t"],"snapshot_id":"{ID}","base_commit":"{X}",
                  "verdict":"stale","reasons":["head_changed"]}}],
              "grounding_digest":"{DIGEST}"}},
            {{"number":2,"claim_id":"decision/{OTHER}","kind":"decision",
              "classification":null,"text":"D","rationale":"r","session_id":"{ID}",
              "checkpoint_id":null,"project_sequence":5,"standing":"active",
              "withdrawal_reason":null,"superseded_by":[],
              "propagates":false,"damaged":true,"verdict":"tampered","reasons":["object_missing"],
              "evidence":[{{"evidence_id":"{OTHER}","kind":null,"canonical_digest":null,
                  "run_status":null,"argv":null,"snapshot_id":null,"base_commit":null,
                  "verdict":"tampered","reasons":["object_missing"]}}],
              "grounding_digest":"{DIGEST}"}}],
        "rerun":[{{"argv":["sh","t"],"evidence_ids":["{ID}"],"claims":[1],"verdict":"stale",
            "reasons":["head_changed"],"workspace_id":"{ID}","at_head":"{Y}"}}],
        "changed_paths_by_base":[{{"base_commit":"{X}","probed":true,"complete":true,
            "paths":["repo://src/lib.rs"]}}],
        "gaps":["record_integrity","base_commits_over_limit"],
        "damaged_records":[{{"record_type":"evidence","record_id":"{OTHER}","reason":"object_missing"}}],
        "continuation":[
            {{"class":0,"template":"header","object_id":"{ID}","session_id":null,
              "verdict":"tampered","reasons":["object_missing"],"standing":null,
              "args":{{"title":"T","task_status":"in_progress","project_sequence":9,"head_oid":"{Y}",
                  "withdrawn_facts":1}}}},
            {{"class":1,"template":"damaged_record","object_id":"{OTHER}","session_id":null,
              "verdict":"tampered","reasons":["object_missing"],"standing":null,
              "args":{{"record_type":"evidence"}}}},
            {{"class":2,"template":"session_open","object_id":"{ID}","session_id":"{ID}",
              "verdict":null,"reasons":[],"standing":null,
              "args":{{"workspace_id":"{ID}","started_at":"2026-10-03T10:00:00Z"}}}},
            {{"class":5,"template":"rerun","object_id":"{ID}","session_id":null,
              "verdict":"stale","reasons":["head_changed"],"standing":null,
              "args":{{"argv":["sh","t"],"evidence_ids":["{ID}"],"numbers":[1],
                  "workspace_id":"{ID}","head_oid":"{Y}"}}}},
            {{"class":5,"template":"reaffirm","object_id":"{ID}","session_id":null,
              "verdict":"stale","reasons":["head_changed"],"standing":null,
              "args":{{"argv":["sh","t"],"evidence_ids":["{ID}"],"numbers":[1],
                  "verified_evidence_id":"{OTHER}","verified_snapshot_id":"{OTHER}"}}}},
            {{"class":6,"template":"fact","object_id":"{ID}/open_items/1","session_id":"{ID}",
              "verdict":"verified","reasons":[],"standing":"active",
              "args":{{"number":1,"text":"F","base_commits":["{X}"],"head_oid":"{Y}"}}}},
            {{"class":7,"template":"decision","object_id":"decision/{OTHER}","session_id":"{ID}",
              "verdict":"tampered","reasons":["object_missing"],"standing":"active",
              "args":{{"number":2,"text":"D","rationale":"r"}}}},
            {{"class":9,"template":"superseded","object_id":"{ID}/verified_completed_work/0",
              "session_id":"{ID}","verdict":"tampered","reasons":["object_missing"],
              "standing":"superseded","args":{{"number":1,"text":"F","superseded_by":[2]}}}},
            {{"class":9,"template":"withdrawn","object_id":"{ID}/open_items/2",
              "session_id":"{ID}","verdict":"tampered","reasons":["object_missing"],
              "standing":"withdrawn","args":{{"number":3,"text":"W","superseded_by":[],
                  "withdrawal_reason":"flaky"}}}}],
        "omitted":[],"capsule_digest":"{DIGEST}",{ACTIONS}}}"#
    )
}

/// ADR 0014 E3 (P4-D): the three actions, in their order.
const ACTIONS: &str = r#""actions":[
    {"action":"complete_task","available":false,
     "reasons":["invalid_transition","evidence_stale"],"scope":"mcp-write",
     "args":{"operation":"transitionTask","target_state":"completed","expected_revision":2,
         "completion_evidence_ids":[],"workspace_id":"018f47a0-0000-7000-8000-000000000001"}},
    {"action":"begin_session","available":true,"reasons":[],"scope":"mcp-write",
     "args":{"operation":"createSession","task_id":"018f47a0-0000-7000-8000-000000000001",
         "workspace_id":null,"task_status":"in_progress",
         "open_session_ids":["018f47a0-0000-7000-8000-000000000001"]}},
    {"action":"cite_latest_run","available":true,"reasons":[],"scope":"mcp-write",
     "args":{"operation":"createCheckpoint","evidence_id":"018f47a0-0000-7000-8000-000000000001",
         "snapshot_id":"018f47a0-0000-7000-8000-000000000001",
         "workspace_id":"018f47a0-0000-7000-8000-000000000001","kind":"test_result",
         "run_status":"PASS","argv":["sh","t"],"overruled_by":null}}]"#;

/// ADR 0013 D4: the `getTaskCapsule` binding mirrors `getRecoveryPreview`.
#[test]
fn task_capsule_binding_is_read_only_and_mirrors_the_preview() {
    let op = OperationId::GetTaskCapsule;
    let binding = binding_for("getTaskCapsule").unwrap();
    assert_eq!(
        (binding.method, binding.path),
        ("GET", "/v1/tasks/{task_id}/recovery-capsule")
    );
    assert_eq!(binding.access_rule.scopes, ["control", "mcp-read"]);
    assert!(!binding.request_body_required());
    validate_request(op, None).unwrap();
    assert!(validate_request(op, Some(&parse_strict("{}").unwrap())).is_err());
    assert_eq!(
        binding.problem_contracts().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            (400, ProblemCode::InvalidRequest),
            (401, ProblemCode::AuthenticationRequired),
            (403, ProblemCode::PolicyDenied),
            (404, ProblemCode::ResourceNotFound),
            (404, ProblemCode::WorkspaceNotResolved),
            (409, ProblemCode::ProjectWorkspaceMismatch),
            // P4-H1 (Owner, T20 SF-1/SF-2): a stored value that does not
            // decode where the view cannot name it as a damaged record.
            (409, ProblemCode::CapsuleTampered),
            (503, ProblemCode::ServiceNotReady),
        ])
    );
}

/// ADR 0013 D4/D5: every member of `TaskCapsule` is checked.
#[test]
#[allow(clippy::too_many_lines)] // Every invalid variant of one document.
fn every_task_capsule_member_is_checked() {
    let op = OperationId::GetTaskCapsule;
    let document = capsule();
    valid(op, &document);
    // An unavailable probe, no claims, a header alone.
    let minimal = format!(
        r#"{{"schema_version":1,"ontology_version":"v3",
        "task":{{"task_id":"{ID}","title":"","revision":1,"status":"proposed","workspace_id":"{ID}"}},
        "status":"partial","reasons":["evidence_unavailable"],
        "bindings":{{"ledger":{{"project_sequence":0,"chain_valid":true}},
            "probe":{{"workspace_id":null,"available":false}}}},
        "sessions":[],"claims":[],"rerun":[],"changed_paths_by_base":[],
        "gaps":["no_checkpoint","no_verified_fact","workspace_unavailable"],
        "damaged_records":[],
        "continuation":[{{"class":0,"template":"header","object_id":"{ID}","session_id":null,
            "verdict":"partial","reasons":["evidence_unavailable"],"standing":null,
            "args":{{"title":"","task_status":"proposed","project_sequence":0,"head_oid":null,
                "withdrawn_facts":0}}}}],
        "omitted":[],"capsule_digest":"{DIGEST}",{ACTIONS}}}"#
    );
    valid(op, &minimal);
    let cases = [
        // task
        (r#""title":"T""#, r#""title":1"#),
        (r#""revision":2"#, r#""revision":0"#),
        (
            r#""status":"in_progress","workspace_id""#,
            r#""status":"open","workspace_id""#,
        ),
        (r#""workspace_id":null}"#, r#""workspace_id":"x"}"#),
        // top verdict
        (r#""status":"stale""#, r#""status":"broken""#),
        (
            r#""reasons":["object_missing"],"#,
            r#""reasons":["moved"],"#,
        ),
        // bindings
        (r#""chain_valid":true"#, r#""chain_valid":1"#),
        (r#""same_workspace":true"#, r#""same_workspace":"y""#),
        // sessions
        (r#""end_state":"open""#, r#""end_state":"closed""#),
        (
            r#""ended_at":null,"end_state""#,
            r#""ended_at":"x","end_state""#,
        ),
        // P3-L L7
        (
            r#""last_recorded_at":"2026-10-03T10:05:00Z""#,
            r#""last_recorded_at":"x""#,
        ),
        (
            r#""last_recorded_at":"2026-10-03T10:05:00Z""#,
            r#""last_recorded_at":5"#,
        ),
        // claims
        (r#""number":1,"claim_id""#, r#""number":0,"claim_id""#),
        (
            r#""number":2,"claim_id":"decision/018f47a0-0000-7000-8000-000000000002","kind""#,
            r#""number":3,"claim_id":"decision/018f47a0-0000-7000-8000-000000000002","kind""#,
        ),
        (
            r#""claim_id":"018f47a0-0000-7000-8000-000000000001/verified_completed_work/0""#,
            r#""claim_id":"018f47a0-0000-7000-8000-000000000001/verified_completed_work/01""#,
        ),
        (
            r#""claim_id":"decision/018f47a0-0000-7000-8000-000000000002","kind""#,
            r#""claim_id":"decision/x","kind""#,
        ),
        (r#""kind":"fact""#, r#""kind":"claim""#),
        (
            r#""classification":"verified_fact""#,
            r#""classification":"fact""#,
        ),
        (r#""text":"F","rationale""#, r#""text":"","rationale""#),
        (
            r#""rationale":null,"session_id""#,
            r#""rationale":1,"session_id""#,
        ),
        (r#""checkpoint_id":null"#, r#""checkpoint_id":"x""#),
        (r#""project_sequence":3"#, r#""project_sequence":0"#),
        (r#""standing":"superseded","#, r#""standing":"gone","#),
        (
            r#""superseded_by":[{"number":2"#,
            r#""superseded_by":[{"number":0"#,
        ),
        (r#""propagates":true"#, r#""propagates":"true""#),
        (r#""damaged":true"#, r#""damaged":null"#),
        (
            r#""kind":null,"canonical_digest":null"#,
            r#""kind":"x","canonical_digest":null"#,
        ),
        (
            r#""kind":null,"canonical_digest":null"#,
            r#""kind":null,"canonical_digest":"x""#,
        ),
        (r#""run_status":"PASS","#, r#""run_status":"OK","#),
        (
            r#""run_status":null,"argv":null"#,
            r#""run_status":null,"argv":[]"#,
        ),
        (r#""snapshot_id":null"#, r#""snapshot_id":"x""#),
        (r#""base_commit":null"#, r#""base_commit":1"#),
        (
            r#""grounding_digest":"sha256:aaaa"#,
            r#""grounding_digest":"md5:aaaa"#,
        ),
        // rerun
        (r#""claims":[1]"#, r#""claims":[]"#),
        (
            r#""evidence_ids":["018f47a0-0000-7000-8000-000000000001"],"claims""#,
            r#""evidence_ids":[],"claims""#,
        ),
        (r#""at_head":"bbbb"#, r#""at_head":1,"x":"bbbb"#),
        // changed_paths_by_base
        (r#""probed":true"#, r#""probed":"true""#),
        (
            r#""paths":["repo://src/lib.rs"]"#,
            r#""paths":["src/lib.rs"]"#,
        ),
        (
            r#"[{"base_commit":"aaaa"#,
            r#"[{"base_commit":"","x":"aaaa"#,
        ),
        // gaps
        (
            r#""gaps":["record_integrity""#,
            r#""gaps":["sessions_truncated""#,
        ),
        (
            r#""gaps":["record_integrity""#,
            r#""gaps":["base_commits_over_limit""#,
        ),
        // damaged_records
        (
            r#""record_type":"evidence","record_id""#,
            r#""record_type":"row","record_id""#,
        ),
        (r#""reason":"object_missing""#, r#""reason":"head_changed""#),
        // continuation
        (
            r#""class":0,"template":"header""#,
            r#""class":1,"template":"header""#,
        ),
        (
            r#""class":2,"template":"session_open""#,
            r#""class":0,"template":"session_open""#,
        ),
        (
            r#""class":5,"template":"rerun""#,
            r#""class":5,"template":"retry""#,
        ),
        (
            r#""class":6,"template":"fact""#,
            r#""class":4,"template":"fact""#,
        ),
        (r#""class":9,"template""#, r#""class":10,"template""#),
        (
            r#""template":"decision","object_id":"decision"#,
            r#""template":"decision","object_id":"","x":"decision"#,
        ),
        (
            r#""verdict":null,"reasons":[]"#,
            r#""verdict":"fine","reasons":[]"#,
        ),
        (
            r#""reasons":[],"standing":null"#,
            r#""reasons":[],"standing":"x""#,
        ),
        (r#""args":{"record_type":"evidence"}"#, r#""args":{}"#),
        (
            r#""args":{"record_type":"evidence"}"#,
            r#""args":{"record_type":"row"}"#,
        ),
        (r#""task_status":"in_progress""#, r#""task_status":"busy""#),
        (r#""numbers":[1],"#, r#""numbers":[0],"#),
        (
            r#""verified_evidence_id":"018f"#,
            r#""verified_evidence_id":"x","y":"018f"#,
        ),
        (r#""superseded_by":[2]"#, r#""superseded_by":["2"]"#),
        (
            r#""started_at":"2026-10-03T10:00:00Z"}}"#,
            r#""started_at":"now"}}"#,
        ),
        // P3-D2: the withdrawal reason and the withdrawn fact count.
        (
            r#""withdrawal_reason":"flaky""#,
            r#""withdrawal_reason":"""#,
        ),
        (r#""withdrawal_reason":"flaky""#, r#""withdrawal_reason":1"#),
        (r#""withdrawn_facts":1"#, r#""withdrawn_facts":-1"#),
        (r#""withdrawn_facts":1"#, r#""withdrawn_facts":"1""#),
        (
            r#""withdrawal_reason":null,"superseded_by":[]"#,
            r#""withdrawal_reason":"not withdrawn","superseded_by":[]"#,
        ),
        // P4-F: a Session's resumed_from.
        (r#""budget":9000"#, r#""budget":999"#),
        (r#""declared":true"#, r#""declared":false"#),
        (r#""project_sequence":7,"#, r#""project_sequence":-7,"#),
        (
            r#""pack_digest":"sha256:aaaa"#,
            r#""pack_digest":"md5:aaaa"#,
        ),
        // P4-D: actions.
        (r#""action":"complete_task""#, r#""action":"reaffirm""#),
        (r#""available":false"#, r#""available":true"#),
        (
            r#""available":true,"reasons":[],"scope""#,
            r#""available":false,"reasons":[],"scope""#,
        ),
        (
            r#""reasons":["invalid_transition","evidence_stale"]"#,
            r#""reasons":["head_changed"]"#,
        ),
        (
            r#""reasons":["invalid_transition","evidence_stale"]"#,
            r#""reasons":["evidence_stale","evidence_stale"]"#,
        ),
        (r#""scope":"mcp-write""#, r#""scope":"mcp-read""#),
        (r#""target_state":"completed""#, r#""target_state":"ready""#),
        (r#""expected_revision":2"#, r#""expected_revision":0"#),
        (
            r#""open_session_ids":["018f"#,
            r#""open_session_ids":["x","018f"#,
        ),
        (r#""overruled_by":null"#, r#""overruled_by":"x""#),
        (
            r#""run_status":"PASS","argv":["sh","t"],"overruled_by""#,
            r#""run_status":"OK","argv":["sh","t"],"overruled_by""#,
        ),
        // omitted is empty in v1
        (r#""omitted":[]"#, r#""omitted":["x"]"#),
        (
            r#""capsule_digest":"sha256:aaaa"#,
            r#""capsule_digest":"aaaa"#,
        ),
    ];
    for (from, to) in cases {
        assert!(document.contains(from), "{from}");
        let changed = document.replacen(from, to, 1);
        let parsed = parse_strict(&changed).unwrap_or_else(|error| panic!("{error:?}: {to}"));
        assert!(
            validate_response(op, 200, "application/json", &parsed).is_err(),
            "accepted: {from} -> {to}"
        );
    }
    // A verified fact in class 4 is refused, as a fact not verified in 6.
    invalid(
        op,
        &document.replacen(
            r#""class":6,"template":"fact","object_id":"018f47a0-0000-7000-8000-000000000001/open_items/1","session_id":"018f47a0-0000-7000-8000-000000000001",
              "verdict":"verified""#,
            r#""class":6,"template":"fact","object_id":"018f47a0-0000-7000-8000-000000000001/open_items/1","session_id":"018f47a0-0000-7000-8000-000000000001",
              "verdict":"stale""#,
            1,
        ),
    );
    // Classes never decrease.
    let swapped = document.replacen(
        r#""class":7,"template":"decision""#,
        r#""class":3,"template":"open_item""#,
        1,
    );
    invalid(op, &swapped);
    // A required member missing from a claim, a Session, an item, or its
    // args (renamed, since a response may carry unknown members).
    for (missing, renamed) in [
        (r#""grounding_digest""#, r#""renamed""#),
        (r#""rationale""#, r#""renamed""#),
        (r#""end_state""#, r#""renamed""#),
        (r#""last_recorded_at""#, r#""renamed""#),
        (r#""template""#, r#""renamed""#),
        (
            r#""project_sequence":9,"head_oid""#,
            r#""project_sequence":9,"renamed""#,
        ),
        (r#""superseded_by":[2]"#, r#""renamed":[2]"#),
        (r#""withdrawal_reason":null,"#, r#""renamed":null,"#),
        (r#""withdrawn_facts":1"#, r#""renamed":1"#),
        (r#""withdrawal_reason":"flaky""#, r#""renamed":"flaky""#),
        (r#""resumed_from""#, r#""renamed""#),
        (r#""actions""#, r#""renamed""#),
        (r#""open_session_ids""#, r#""renamed""#),
    ] {
        assert!(document.contains(missing), "{missing}");
        invalid(op, &document.replacen(missing, renamed, 1));
    }
    // P4-D: exactly the three actions, in their order.
    let third = document
        .find(
            r#",
    {"action":"cite_latest_run""#,
        )
        .unwrap();
    let end = document.rfind(']').unwrap();
    let mut two = document.clone();
    two.replace_range(third..end, "");
    invalid(op, &two);
    let swapped = document
        .replacen(
            r#""action":"complete_task""#,
            r#""action":"placeholder""#,
            1,
        )
        .replacen(
            r#""action":"begin_session""#,
            r#""action":"complete_task""#,
            1,
        )
        .replacen(
            r#""action":"placeholder""#,
            r#""action":"begin_session""#,
            1,
        );
    invalid(op, &swapped);
    // An empty continuation is refused: the header is always there.
    let header_start = minimal.find(r#""continuation":["#).unwrap();
    let header_end = minimal.find(r#""omitted""#).unwrap();
    let mut empty = minimal.clone();
    empty.replace_range(header_start..header_end, r#""continuation":[],"#);
    invalid(op, &empty);
}

/// P3-C2 (Owner, T12 S-6): a `tampered` Preview or Capsule holds no
/// `verified` at any level; any other status may.
#[test]
fn a_tampered_document_holds_no_verified() {
    let capsule_op = OperationId::GetTaskCapsule;
    let document = capsule();
    let verified_item = r#"{"class":6,"template":"fact","object_id":"018f47a0-0000-7000-8000-000000000001/open_items/1","session_id":"018f47a0-0000-7000-8000-000000000001",
              "verdict":"verified","reasons":[],"standing":"active",
              "args":{"number":1,"text":"F","base_commits":["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],"head_oid":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}},
            "#;
    assert!(document.contains(verified_item));
    let tampered = document.replacen(r#""status":"stale""#, r#""status":"tampered""#, 1);
    invalid(capsule_op, &tampered);
    valid(capsule_op, &tampered.replacen(verified_item, "", 1));
    // Evidence inside a claim counts too.
    let evidence_verified = tampered.replacen(verified_item, "", 1).replacen(
        r#""verdict":"stale","reasons":["head_changed"]}],"#,
        r#""verdict":"verified","reasons":[]}],"#,
        1,
    );
    assert_ne!(evidence_verified, tampered.replacen(verified_item, "", 1));
    invalid(capsule_op, &evidence_verified);

    let preview_op = OperationId::GetRecoveryPreview;
    let stale = preview();
    let verified_evidence = stale.replacen(
        r#""verdict":"stale",
                "reasons":["head_changed"]}]"#,
        r#""verdict":"verified",
                "reasons":[]}]"#,
        1,
    );
    assert_ne!(verified_evidence, stale);
    valid(preview_op, &verified_evidence);
    let tampered = |text: &str| {
        text.replacen(
            r#""status":"stale","reasons":["head_changed","evidence_stale"]"#,
            r#""status":"tampered","reasons":["object_digest_mismatch"]"#,
            1,
        )
    };
    assert_ne!(tampered(&stale), stale);
    valid(preview_op, &tampered(&stale));
    invalid(preview_op, &tampered(&verified_evidence));
}
