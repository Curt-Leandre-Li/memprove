use std::collections::BTreeSet;

use memprove_protocol::phase1::{
    AccessDecision, ClientScope, EventType, MappingState, OperationId, PHASE1_BINDING_COUNT,
    PrivacyClass, ProblemCode, ProjectKind, SessionState, TaskState, TokenStatus,
    ValidationErrorKind, WireOperation, all_bindings, binding_for, operations, validate_problem,
    validate_request, validate_response, validate_www_authenticate,
};
use memprove_protocol::strict_json::{JsonErrorKind, JsonValue, parse_strict};

const FIXTURES: &str =
    include_str!("../../../tests/phase0/fixtures/phase1-entry-contract-v1/fixtures.json");
const OPENAPI: &str = include_str!("../../../openapi/control-api-v1.yaml");
const EVENT_SCHEMA: &str = include_str!("../../../schemas/events/event-envelope-v1.json");
const FIXTURE_MANIFEST: &str =
    include_str!("../../../tests/phase0/fixtures/phase1-entry-contract-v1/manifest.json");

fn object(value: &JsonValue) -> &[(String, JsonValue)] {
    value.as_object().expect("fixture value must be an object")
}

fn field<'a>(value: &'a JsonValue, name: &str) -> &'a JsonValue {
    value
        .get(name)
        .unwrap_or_else(|| panic!("fixture object must contain {name}"))
}

fn string_field<'a>(value: &'a JsonValue, name: &str) -> &'a str {
    field(value, name)
        .as_str()
        .unwrap_or_else(|| panic!("fixture field {name} must be a string"))
}

fn optional_string_field<'a>(value: &'a JsonValue, name: &str) -> Option<&'a str> {
    match field(value, name) {
        JsonValue::Null => None,
        value => Some(
            value
                .as_str()
                .unwrap_or_else(|| panic!("fixture field {name} must be string or null")),
        ),
    }
}

fn bool_field(value: &JsonValue, name: &str) -> bool {
    field(value, name)
        .as_bool()
        .unwrap_or_else(|| panic!("fixture field {name} must be a boolean"))
}

fn operation_field(value: &JsonValue) -> OperationId {
    string_field(value, "operation_id")
        .parse()
        .expect("fixture operation_id must have a Phase 1 binding")
}

fn assert_operation<O: WireOperation>() {}

fn inline_yaml_enum(prefix: &str) -> Vec<&str> {
    let line = OPENAPI
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("OpenAPI must contain inline enum after {prefix}"));
    line.split_once('[')
        .and_then(|(_, values)| values.split_once(']').map(|(values, _)| values))
        .expect("inline enum must use square brackets")
        .split(',')
        .map(str::trim)
        .collect()
}

#[test]
fn all_twenty_three_operation_markers_compile_as_wire_bindings() {
    assert_operation::<operations::HealthLive>();
    assert_operation::<operations::HealthReady>();
    assert_operation::<operations::GetStatus>();
    assert_operation::<operations::GetVersion>();
    assert_operation::<operations::CreateClientToken>();
    assert_operation::<operations::ListClientTokens>();
    assert_operation::<operations::RevokeClientToken>();
    assert_operation::<operations::RegisterProject>();
    assert_operation::<operations::GetProject>();
    assert_operation::<operations::ResolveWorkspace>();
    assert_operation::<operations::CreateSession>();
    assert_operation::<operations::ListSessions>();
    assert_operation::<operations::EndSession>();
    assert_operation::<operations::IngestEvent>();
    assert_operation::<operations::ListEvents>();
    assert_operation::<operations::IngestEventBatch>();
    assert_operation::<operations::StreamEvents>();
    assert_operation::<operations::GetEvent>();
    assert_operation::<operations::CreateTask>();
    assert_operation::<operations::ListTasks>();
    assert_operation::<operations::GetNextTask>();
    assert_operation::<operations::TransitionTask>();
    assert_operation::<operations::AddTaskDependency>();
}

#[test]
fn operation_registry_is_unique_closed_and_default_deny() {
    let bindings = all_bindings();
    // The 23 frozen Phase 1 operations, then the four Phase 2 operations
    // routed by ADR 0012 (P2-A), getRecoveryPreview (D6, P2-B),
    // verifyLedger (P2-C), getTaskCapsule (ADR 0013 D4, P3-B), and
    // proposeDecision (ADR 0013 D2, P3-C).
    assert_eq!(bindings.len(), 31);
    assert_eq!(PHASE1_BINDING_COUNT, 23);
    assert_eq!(
        bindings[PHASE1_BINDING_COUNT..]
            .iter()
            .map(|binding| binding.operation_id.as_str())
            .collect::<Vec<_>>(),
        [
            "captureEvidence",
            "getEvidence",
            "createCheckpoint",
            "getCheckpoint",
            "getRecoveryPreview",
            "verifyLedger",
            "getTaskCapsule",
            "proposeDecision"
        ]
    );

    let operation_ids = bindings
        .iter()
        .map(|binding| binding.operation_id.as_str())
        .collect::<BTreeSet<_>>();
    let method_paths = bindings
        .iter()
        .map(|binding| (binding.method, binding.path))
        .collect::<BTreeSet<_>>();

    assert_eq!(operation_ids.len(), 31, "operationId values must be unique");
    assert_eq!(method_paths.len(), 31, "method/path pairs must be unique");

    for binding in bindings {
        assert_eq!(
            binding.operation_id.as_str(),
            binding.capability_id.as_str()
        );
        assert_eq!(
            binding.authorize(MappingState::MissingOrUnapproved, true),
            AccessDecision::Denied,
            "{} must fail closed while its mapping is absent or unapproved",
            binding.operation_id
        );
    }
    assert!(binding_for("notARealOperation").is_none());
}

/// Problem codes added to `OpenAPI` v1 after the Phase 0 manifest was frozen.
/// The frozen manifest stays byte-identical; each amendment names its ADR.
const POST_FREEZE_PROBLEM_AMENDMENTS: &[(&str, u16, ProblemCode)] = &[
    // ADR 0010 Q3: an Event naming an unknown Session.
    ("ingestEvent", 404, ProblemCode::ResourceNotFound),
    // T7 (Owner decision 2026-10-02): an Event for a Session that is not active.
    ("ingestEvent", 409, ProblemCode::InvalidTransition),
    // ADR 0012: createSession names an unknown Task.
    ("createSession", 404, ProblemCode::ResourceNotFound),
    // ADR 0012: createSession names a Task scoped to another Workspace.
    ("createSession", 409, ProblemCode::ProjectWorkspaceMismatch),
    // P4-D (Owner A1): createSession names a completed or cancelled Task.
    ("createSession", 409, ProblemCode::InvalidTransition),
    // P2-C: a completion's workspace_id is unknown or of another Project.
    ("transitionTask", 404, ProblemCode::WorkspaceNotResolved),
    ("transitionTask", 409, ProblemCode::ProjectWorkspaceMismatch),
    // P4-H1 (Owner, T20 SF-1/SF-2): a stored record the operation reads is
    // missing or does not decode; no retry cures it, so it is not 503.
    ("getProject", 409, ProblemCode::CapsuleTampered),
    ("resolveWorkspace", 409, ProblemCode::CapsuleTampered),
    ("listSessions", 409, ProblemCode::CapsuleTampered),
    ("listEvents", 409, ProblemCode::CapsuleTampered),
    ("getEvent", 409, ProblemCode::CapsuleTampered),
    ("createTask", 409, ProblemCode::CapsuleTampered),
    ("listTasks", 409, ProblemCode::CapsuleTampered),
    // U9 R1 (Owner O4; ADR 0020): a completion whose `session_id` names a
    // `sub_agent` Session, whatever runs it cites (S-F round 1, SF1-A).
    ("transitionTask", 403, ProblemCode::RoleForbidden),
];

/// Problem codes added to `OpenAPI` v1 after the Phase 0 manifest froze its
/// status/code registry, each with its only status.
const POST_FREEZE_PROBLEM_CODES: &[(u16, ProblemCode)] = &[
    // U9 R1 (Owner O4; ADR 0020).
    (403, ProblemCode::RoleForbidden),
];

/// `AccessRule` scopes changed in `OpenAPI` v1 after the Phase 0 manifest was
/// frozen. The frozen manifest and its access fixtures stay byte-identical
/// (frozen: `[control]`); ADR 0012 lets agents read with `mcp-read` and write
/// with `mcp-write`.
const POST_FREEZE_ACCESS_AMENDMENTS: &[(&str, &[&str])] = &[
    ("getProject", &["control", "mcp-read"]),
    ("listSessions", &["control", "mcp-read"]),
    ("listTasks", &["control", "mcp-read"]),
    ("resolveWorkspace", &["control", "mcp-write"]),
    ("createSession", &["control", "mcp-write"]),
    ("endSession", &["control", "mcp-write"]),
    ("ingestEvent", &["control", "mcp-write"]),
    ("createTask", &["control", "mcp-write"]),
    ("transitionTask", &["control", "mcp-write"]),
];

/// The scopes the frozen manifest records for `operation_id`, with any
/// post-freeze amendment applied.
fn amended_scopes<'a>(operation_id: &str, frozen: Vec<&'a str>) -> Vec<&'a str> {
    match POST_FREEZE_ACCESS_AMENDMENTS
        .iter()
        .find(|(amended, _)| *amended == operation_id)
    {
        Some((_, scopes)) => {
            assert_eq!(frozen, ["control"], "{operation_id}: frozen scopes changed");
            scopes.to_vec()
        }
        None => frozen,
    }
}

#[test]
fn post_freeze_access_amendments_match_openapi() {
    for (operation_id, scopes) in POST_FREEZE_ACCESS_AMENDMENTS {
        let operation = OPENAPI
            .split(&format!("operationId: {operation_id}\n"))
            .nth(1)
            .expect("amended operation");
        let operation = &operation[..operation.find("responses:").expect("operation body")];
        assert!(
            operation.contains(&format!(
                "x-memprove-access: {{ kind: AnyOf, scopes: [{}] }}",
                scopes.join(", ")
            )),
            "{operation_id}: OpenAPI access drift"
        );
        assert_eq!(
            binding_for(operation_id).unwrap().access_rule.scopes,
            *scopes
        );
    }
}

#[test]
fn registry_contract_sets_exactly_match_the_external_manifest() {
    let manifest = parse_strict(FIXTURE_MANIFEST).expect("fixture manifest must be strict JSON");
    let fixture_document = parse_strict(FIXTURES).expect("fixture set must be strict JSON");
    let fixture_cases = field(&fixture_document, "cases")
        .as_array()
        .expect("fixture cases must be an array");
    let operations = field(&manifest, "operations")
        .as_array()
        .expect("manifest operations must be an array");
    assert_eq!(operations.len(), 23);

    let mut manifest_problem_pairs = 0_usize;
    for operation in operations {
        let operation_id = string_field(operation, "operation_id");
        let binding = binding_for(operation_id).expect("manifest operation must be bound");
        assert_eq!(binding.method, string_field(operation, "method"));
        assert_eq!(binding.path, string_field(operation, "path"));
        assert_eq!(
            binding.request_body_required(),
            string_field(field(operation, "request"), "body") == "required"
        );

        let access = field(operation, "access");
        assert_eq!(
            binding.access_rule.kind.as_str(),
            string_field(access, "kind")
        );
        let scopes = field(access, "scopes")
            .as_array()
            .expect("access scopes must be an array")
            .iter()
            .map(|scope| scope.as_str().expect("scope must be a string"))
            .collect::<Vec<_>>();
        assert_eq!(
            binding.access_rule.scopes,
            amended_scopes(operation_id, scopes)
        );

        let mut expected_problems = BTreeSet::new();
        for (status, codes) in object(field(operation, "problem_codes")) {
            let status = status
                .parse::<u16>()
                .expect("problem status must be numeric");
            for code in codes.as_array().expect("problem codes must be an array") {
                expected_problems.insert((
                    status,
                    code.as_str()
                        .expect("problem code must be a string")
                        .parse::<ProblemCode>()
                        .expect("problem code must be frozen"),
                ));
            }
        }
        manifest_problem_pairs += expected_problems.len();
        for (amended_operation, status, code) in POST_FREEZE_PROBLEM_AMENDMENTS {
            if *amended_operation == operation_id {
                assert!(
                    expected_problems.insert((*status, *code)),
                    "{operation_id}: amendment {status} {code:?} is already frozen"
                );
            }
        }
        assert_eq!(
            binding.problem_contracts().collect::<BTreeSet<_>>(),
            expected_problems,
            "{operation_id}: Problem set drift"
        );

        let expected_successes = fixture_cases
            .iter()
            .filter(|case| {
                optional_string_field(case, "operation_id") == Some(operation_id)
                    && optional_string_field(case, "direction") == Some("response")
                    && optional_string_field(case, "media_type")
                        .is_some_and(|media_type| media_type != "application/problem+json")
            })
            .map(|case| {
                (
                    optional_string_field(case, "status")
                        .expect("success status must exist")
                        .parse::<u16>()
                        .expect("success status must be numeric"),
                    optional_string_field(case, "media_type")
                        .expect("success media type must exist"),
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            binding.success_contracts().collect::<BTreeSet<_>>(),
            expected_successes,
            "{operation_id}: success set drift"
        );
    }
    assert_eq!(manifest_problem_pairs, 125);
}

#[test]
fn all_483_external_fixtures_execute_through_real_bindings() {
    let document = parse_strict(FIXTURES).expect("fixture document must be strict JSON");
    let cases = field(&document, "cases")
        .as_array()
        .expect("fixture cases must be an array");
    assert_eq!(cases.len(), 483);

    let mut exercised = 0_usize;
    for case in cases {
        let case_id = string_field(case, "case_id");
        let kind = string_field(case, "kind");
        let payload_raw = string_field(case, "payload_raw");
        let parse_expectation = string_field(case, "json_parse_expectation");
        let schema_expectation = string_field(case, "schema_expectation");
        let parsed = parse_strict(payload_raw);

        if parse_expectation == "rejected_duplicate_member" {
            let Err(error) = parsed else {
                panic!("{case_id}: duplicate-member fixture unexpectedly parsed");
            };
            assert!(
                matches!(error.kind, JsonErrorKind::DuplicateMember(_)),
                "{case_id}: expected duplicate-member rejection, got {error:?}"
            );
            exercised += 1;
            continue;
        }

        assert_eq!(parse_expectation, "accepted", "{case_id}");
        let payload = parsed.unwrap_or_else(|error| panic!("{case_id}: {error:?}"));

        let validation = match kind {
            "request_body" => validate_request(operation_field(case), Some(&payload)),
            "request_body_absent" => validate_request(operation_field(case), None),
            "primary_success" | "problem" | "behavior" => validate_response(
                operation_field(case),
                optional_string_field(case, "status")
                    .expect("response fixture must contain status")
                    .parse()
                    .expect("response fixture status must be numeric"),
                optional_string_field(case, "media_type")
                    .expect("response fixture must contain media_type"),
                &payload,
            ),
            "problem_status_code_pair" => validate_problem(
                optional_string_field(case, "status")
                    .expect("problem fixture must contain status")
                    .parse()
                    .expect("problem fixture status must be numeric"),
                &payload,
            ),
            "www_authenticate_header" => validate_www_authenticate(&payload),
            "access" => {
                let binding = binding_for(string_field(case, "operation_id"))
                    .expect("access fixture operation must have a binding");
                let fixture_rule = field(&payload, "access_rule");
                assert_eq!(
                    string_field(fixture_rule, "kind"),
                    binding.access_rule.kind.as_str(),
                    "{case_id}: AccessRule kind drift"
                );
                let fixture_scopes = field(fixture_rule, "scopes")
                    .as_array()
                    .expect("access fixture scopes must be an array")
                    .iter()
                    .map(|scope| scope.as_str().expect("scope must be a string"))
                    .collect::<Vec<_>>();
                assert_eq!(
                    amended_scopes(string_field(case, "operation_id"), fixture_scopes),
                    binding.access_rule.scopes,
                    "{case_id}"
                );

                let mapping_state = match string_field(&payload, "mapping_state") {
                    "COMPLETE_AND_APPROVED" => MappingState::CompleteAndApproved,
                    "MISSING_OR_UNAPPROVED" => MappingState::MissingOrUnapproved,
                    other => panic!("{case_id}: unknown fixture mapping state {other}"),
                };
                let actual = binding.authorize(
                    mapping_state,
                    bool_field(&payload, "request_satisfies_rule"),
                );
                let expected = match string_field(&payload, "expected_decision") {
                    "ALLOWED" => AccessDecision::Allowed,
                    "DENIED" => AccessDecision::Denied,
                    other => panic!("{case_id}: unknown fixture access decision {other}"),
                };
                assert_eq!(actual, expected, "{case_id}");
                Ok(())
            }
            other => panic!("{case_id}: unsupported fixture kind {other}"),
        };

        match schema_expectation {
            "valid" | "not_applicable" => {
                validation.unwrap_or_else(|error| panic!("{case_id}: {error:?}"));
            }
            "invalid" => assert!(
                validation.is_err(),
                "{case_id}: invalid fixture passed binding validation"
            ),
            other => panic!("{case_id}: unknown schema expectation {other}"),
        }
        exercised += 1;
    }

    assert_eq!(exercised, 483);
    assert!(!object(&document).is_empty());
}

#[test]
fn request_decoder_rejects_unknown_fields_but_response_decoder_is_compatible() {
    let request = parse_strict(
        r#"{"schema_version":1,"project_id":"018f0f00-0000-7000-8000-000000000001","title":"test","future_field":true}"#,
    )
    .unwrap();
    let error = validate_request(OperationId::CreateTask, Some(&request)).unwrap_err();
    assert!(matches!(error.kind, ValidationErrorKind::UnknownField(_)));

    let response = parse_strict(
        r#"{"schema_version":1,"task_id":"018f0f00-0000-7000-8000-000000000005","project_id":"018f0f00-0000-7000-8000-000000000001","title":"test","state":"ready","revision":1,"dependency_ids":[],"completion_evidence_ids":[],"future_field":{"nested":true}}"#,
    )
    .unwrap();
    validate_response(OperationId::CreateTask, 201, "application/json", &response).unwrap();
}

#[test]
fn duplicate_members_are_rejected_at_every_object_depth() {
    let top = parse_strict(r#"{"schema_version":1,"schema_version":1}"#).unwrap_err();
    assert!(matches!(top.kind, JsonErrorKind::DuplicateMember(ref key) if key == "schema_version"));

    let nested = parse_strict(
        r#"{"events":[{"producer":{"name":"x","name":"x","version":"1","sequence":0}}]}"#,
    )
    .unwrap_err();
    assert!(matches!(nested.kind, JsonErrorKind::DuplicateMember(ref key) if key == "name"));
}

#[test]
fn task_state_exactly_matches_the_frozen_six_state_enum() {
    let states = TaskState::ALL.map(TaskState::as_str);
    assert_eq!(
        states,
        [
            "proposed",
            "ready",
            "in_progress",
            "blocked",
            "completed",
            "cancelled"
        ]
    );

    for state in states {
        assert_eq!(state.parse::<TaskState>().unwrap().as_str(), state);
    }
    for invalid in ["active", "paused", "archived", "done", "READY"] {
        assert!(invalid.parse::<TaskState>().is_err(), "accepted {invalid}");
    }

    let invalid_request =
        parse_strict(r#"{"schema_version":1,"expected_revision":1,"target_state":"paused"}"#)
            .unwrap();
    let error = validate_request(OperationId::TransitionTask, Some(&invalid_request)).unwrap_err();
    assert!(matches!(
        error.kind,
        ValidationErrorKind::InvalidVariant { .. }
    ));
}

#[test]
fn problem_status_code_pairs_and_operation_problem_sets_are_closed() {
    let legal = parse_strict(
        r#"{"type":"urn:memprove:problem:invalid_request","title":"Invalid Request","status":400,"code":"invalid_request","request_id":"018f0f00-0000-7000-8000-000000000006","retryable":false}"#,
    )
    .unwrap();
    validate_problem(400, &legal).unwrap();

    let crossed = parse_strict(
        r#"{"type":"urn:memprove:problem:authentication_required","title":"Authentication Required","status":400,"code":"authentication_required","request_id":"018f0f00-0000-7000-8000-000000000006","retryable":false}"#,
    )
    .unwrap();
    assert!(validate_problem(400, &crossed).is_err());

    let globally_legal_but_not_for_health = parse_strict(
        r#"{"type":"urn:memprove:problem:invalid_request","title":"Invalid Request","status":400,"code":"invalid_request","request_id":"018f0f00-0000-7000-8000-000000000006","retryable":false}"#,
    )
    .unwrap();
    assert!(
        validate_response(
            OperationId::HealthReady,
            400,
            "application/problem+json",
            &globally_legal_but_not_for_health
        )
        .is_err()
    );
}

#[test]
fn cursor_uuid_and_expected_revision_contracts_are_enforced() {
    let valid_page = parse_strict(
        r#"{"schema_version":1,"snapshot_sequence":0,"items":[],"next_cursor":"opaque-cursor"}"#,
    )
    .unwrap();
    validate_response(OperationId::ListTasks, 200, "application/json", &valid_page).unwrap();

    let invalid_cursor =
        parse_strict(r#"{"schema_version":1,"snapshot_sequence":0,"items":[],"next_cursor":42}"#)
            .unwrap();
    assert!(
        validate_response(
            OperationId::ListTasks,
            200,
            "application/json",
            &invalid_cursor
        )
        .is_err()
    );

    let invalid_revision =
        parse_strict(r#"{"schema_version":1,"expected_revision":0,"target_state":"ready"}"#)
            .unwrap();
    assert!(validate_request(OperationId::TransitionTask, Some(&invalid_revision)).is_err());

    let invalid_uuid =
        parse_strict(r#"{"schema_version":1,"project_id":"not-a-uuid","title":"test"}"#).unwrap();
    assert!(validate_request(OperationId::CreateTask, Some(&invalid_uuid)).is_err());
}

#[test]
fn www_authenticate_is_the_exact_frozen_bearer_challenge() {
    validate_www_authenticate(&JsonValue::String("Bearer".to_owned())).unwrap();
    for invalid in [
        JsonValue::String("Basic".to_owned()),
        JsonValue::String("bearer".to_owned()),
        JsonValue::Null,
    ] {
        assert!(validate_www_authenticate(&invalid).is_err());
    }
}

#[test]
fn rust_registry_and_frozen_openapi_operation_and_task_enums_match() {
    let openapi_operation_ids = OPENAPI
        .lines()
        .filter_map(|line| line.trim().strip_prefix("operationId: "))
        .filter(|operation_id| binding_for(operation_id).is_some())
        .collect::<BTreeSet<_>>();
    let rust_operation_ids = all_bindings()
        .iter()
        .map(|binding| binding.operation_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(openapi_operation_ids, rust_operation_ids);

    let openapi_task_states = OPENAPI
        .lines()
        .find_map(|line| line.trim().strip_prefix("TaskState: { enum: ["))
        .and_then(|line| line.strip_suffix("] }"))
        .expect("TaskState must use an inline enum")
        .split(',')
        .map(|state| state.trim().trim_matches('"'))
        .collect::<Vec<_>>();
    assert_eq!(openapi_task_states, TaskState::ALL.map(TaskState::as_str));
}

#[test]
fn every_phase_one_wire_enum_matches_its_authoritative_contract() {
    assert_eq!(
        inline_yaml_enum("enum: [control, ingest-only"),
        ClientScope::ALL.map(ClientScope::as_str)
    );
    assert_eq!(
        inline_yaml_enum("status: { enum: [active, revoked"),
        TokenStatus::ALL.map(TokenStatus::as_str)
    );
    assert_eq!(
        inline_yaml_enum("kind: { enum: [git, directory"),
        ProjectKind::ALL.map(ProjectKind::as_str)
    );
    assert_eq!(
        inline_yaml_enum("state: { enum: [active, ended, crashed"),
        SessionState::ALL.map(SessionState::as_str)
    );

    let event_schema = parse_strict(EVENT_SCHEMA).expect("event schema must be strict JSON");
    let properties = field(&event_schema, "properties");
    let event_types = field(field(properties, "event_type"), "enum")
        .as_array()
        .expect("event_type enum must be an array")
        .iter()
        .map(|value| value.as_str().expect("event type must be a string"))
        .collect::<Vec<_>>();
    let privacy_classes = field(field(properties, "privacy_class"), "enum")
        .as_array()
        .expect("privacy_class enum must be an array")
        .iter()
        .map(|value| value.as_str().expect("privacy class must be a string"))
        .collect::<Vec<_>>();
    assert_eq!(event_types, EventType::ALL.map(EventType::as_str));
    assert_eq!(privacy_classes, PrivacyClass::ALL.map(PrivacyClass::as_str));

    let manifest = parse_strict(FIXTURE_MANIFEST).expect("fixture manifest must be strict JSON");
    let statuses = field(field(&manifest, "problem_status_code_registry"), "statuses");
    let mut manifest_pairs = BTreeSet::new();
    for (status, codes) in object(statuses) {
        let status = status
            .parse::<u16>()
            .expect("problem status must be numeric");
        for code in codes
            .as_array()
            .expect("problem code list must be an array")
        {
            manifest_pairs.insert((
                status,
                code.as_str().expect("problem code must be a string"),
            ));
        }
    }
    let rust_pairs = ProblemCode::ALL
        .iter()
        .map(|code| (code.status(), code.as_str()))
        .collect::<BTreeSet<_>>();
    assert_eq!(manifest_pairs.len(), 22);
    for (status, code) in POST_FREEZE_PROBLEM_CODES {
        assert!(
            manifest_pairs.insert((*status, code.as_str())),
            "{code:?} is already frozen"
        );
    }
    assert_eq!(manifest_pairs, rust_pairs);
}

#[test]
fn adr_0010_amendments_match_openapi_schema_and_validators() {
    let ingest = OPENAPI
        .split("operationId: ingestEvent\n")
        .nth(1)
        .expect("ingestEvent operation");
    let ingest = &ingest[..ingest.find("requestBody:").expect("ingestEvent body")];
    assert!(
        ingest
            .contains(r#""404": [project_not_found, workspace_not_resolved, resource_not_found]"#),
        "OpenAPI ingestEvent must list 404 resource_not_found"
    );
    let batch = OPENAPI
        .split("operationId: ingestEventBatch\n")
        .nth(1)
        .expect("ingestEventBatch operation");
    assert!(batch.contains(r#""404": [project_not_found, workspace_not_resolved]"#));
    assert!(EVENT_SCHEMA.contains(
        r#""sequence": { "type": "integer", "minimum": 0, "maximum": 9007199254740991 }"#
    ));
    assert!(
        binding_for("ingestEvent")
            .unwrap()
            .problem_contracts()
            .any(|pair| pair == (404, ProblemCode::ResourceNotFound))
    );
    assert!(
        !binding_for("ingestEventBatch")
            .unwrap()
            .problem_contracts()
            .any(|pair| pair == (404, ProblemCode::ResourceNotFound))
    );

    let envelope = |sequence: &str| {
        parse_strict(&format!(
            r#"{{"envelope_version":"1.0","event_id":"018f0f00-0000-7000-8000-000000000001",
               "project_id":"018f0f00-0000-7000-8000-000000000002",
               "workspace_id":"018f0f00-0000-7000-8000-000000000003",
               "event_type":"turn.started","payload_schema":"turn.started@1",
               "producer":{{"name":"hook","version":"1","sequence":{sequence}}},
               "privacy_class":"internal","payload":{{}}}}"#
        ))
        .unwrap()
    };
    for accepted in ["0", "9007199254740991", "9007199254740991.0"] {
        validate_request(OperationId::IngestEvent, Some(&envelope(accepted)))
            .unwrap_or_else(|error| panic!("{accepted}: {error}"));
    }
    for rejected in ["9007199254740992", "1e16", "-1"] {
        assert!(
            validate_request(OperationId::IngestEvent, Some(&envelope(rejected))).is_err(),
            "{rejected}"
        );
    }
}

#[test]
fn t7_amendments_match_openapi_and_bindings() {
    let ingest = OPENAPI
        .split("operationId: ingestEvent\n")
        .nth(1)
        .expect("ingestEvent operation");
    let ingest = &ingest[..ingest.find("requestBody:").expect("ingestEvent body")];
    assert!(
        ingest.contains(
            r#""409": [idempotency_conflict, project_workspace_mismatch, invalid_transition]"#
        ),
        "OpenAPI ingestEvent must list 409 invalid_transition"
    );
    let batch = OPENAPI
        .split("operationId: ingestEventBatch\n")
        .nth(1)
        .expect("ingestEventBatch operation");
    let batch = &batch[..batch.find("requestBody:").expect("ingestEventBatch body")];
    assert!(!batch.contains("invalid_transition"));
    let resolve = OPENAPI
        .split("operationId: resolveWorkspace\n")
        .nth(1)
        .expect("resolveWorkspace operation");
    let resolve = &resolve[..resolve.find("requestBody:").expect("resolveWorkspace body")];
    assert!(
        resolve.contains("ADR 0011"),
        "resolveWorkspace must cite ADR 0011"
    );
    assert!(
        !resolve.contains("IdempotencyKey"),
        "resolveWorkspace still has no Idempotency-Key"
    );
    assert!(
        binding_for("ingestEvent")
            .unwrap()
            .problem_contracts()
            .any(|pair| pair == (409, ProblemCode::InvalidTransition))
    );
    assert!(
        !binding_for("ingestEventBatch")
            .unwrap()
            .problem_contracts()
            .any(|pair| pair == (409, ProblemCode::InvalidTransition))
    );
}

/// ADR 0012 routes four Phase 2 operations (P2-A) and adds
/// `getRecoveryPreview` (D6, P2-B); their bindings mirror the access and
/// Problem annotations added to `OpenAPI` in the same change.
#[test]
fn p2a_operations_match_openapi_annotations() {
    for binding in &all_bindings()[PHASE1_BINDING_COUNT..] {
        let operation_id = binding.operation_id.as_str();
        let operation = OPENAPI
            .split(&format!("operationId: {operation_id}\n"))
            .nth(1)
            .expect("Phase 2 operation in OpenAPI");
        let operation = &operation[..operation.find("responses:").expect("operation body")];
        assert!(
            operation.contains(&format!(
                "x-memprove-access: {{ kind: AnyOf, scopes: [{}] }}",
                binding.access_rule.scopes.join(", ")
            )),
            "{operation_id}: access drift"
        );
        let annotated = operation
            .lines()
            .skip_while(|line| !line.contains("x-memprove-problem-codes:"))
            .skip(1)
            .map_while(|line| {
                let (status, codes) = line.trim().split_once(": [")?;
                let status = status.trim_matches('"').parse::<u16>().ok()?;
                Some(
                    codes
                        .trim_end_matches(']')
                        .split(", ")
                        .map(|code| (status, code.parse::<ProblemCode>().unwrap()))
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            binding.problem_contracts().collect::<BTreeSet<_>>(),
            annotated,
            "{operation_id}: Problem drift"
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)] // Every amended request schema, valid and invalid.
fn p2a_request_schemas_are_enforced() {
    let ids = r#""workspace_id":"018f47a0-0000-7000-8000-000000000001",
        "task_id":"018f47a0-0000-7000-8000-000000000002",
        "session_id":"018f47a0-0000-7000-8000-000000000003",
        "snapshot_id":"018f47a0-0000-7000-8000-000000000004""#;
    let digest = format!("sha256:{}", "0".repeat(64));
    let capture = |artifact: &str, metadata: &str| {
        parse_strict(&format!(
            r#"{{"schema_version":1,{ids},"artifact_type":"{artifact}","metadata":{metadata}}}"#
        ))
        .unwrap()
    };
    let run = |extra: &str| {
        format!(
            r#"{{"argv":["cargo","test"],"exit_code":0,"started_at":"2026-10-02T10:00:00Z",
                "finished_at":"2026-10-02T10:00:01.250Z","stdout_tail":"ok","stderr_tail":"",
                "stdout_sha256":"{digest}","stderr_sha256":"{digest}",
                "collector":{{"name":"memprove-cli","version":"0.1.0"}}{extra}}}"#
        )
    };
    let op = OperationId::CaptureEvidence;
    for valid in [
        capture("command_run", &run("")),
        capture(
            "test_run",
            &run(r#","test_name":"unit","test_case_count":0"#),
        ),
        capture("diff", "{}"),
    ] {
        validate_request(op, Some(&valid)).unwrap();
    }
    for invalid in [
        capture("command_run", "{}"),
        capture("command_run", &run(r#","extra":1"#)),
        capture("test_run", &run(r#","test_case_count":-1"#)),
        capture("test_run", &run(r#","test_case_count":9007199254740992"#)),
        capture("unknown", "{}"),
        capture(
            "command_run",
            &run("").replace(r#""exit_code":0"#, r#""exit_code":2147483648"#),
        ),
        capture(
            "command_run",
            &run("").replace("10:00:00Z", "10:00:00+01:00"),
        ),
        capture(
            "command_run",
            &run("").replace(
                r#""stdout_tail":"ok""#,
                &format!(r#""stdout_tail":"{}""#, "x".repeat(4097)),
            ),
        ),
        parse_strict(
            r#"{"schema_version":1,"workspace_id":"018f47a0-0000-7000-8000-000000000001",
                "artifact_type":"diff","snapshot_id":"018f47a0-0000-7000-8000-000000000004",
                "metadata":{}}"#,
        )
        .unwrap(),
    ] {
        assert!(validate_request(op, Some(&invalid)).is_err(), "{invalid:?}");
    }
    let negative = capture(
        "command_run",
        &run("").replace(r#""exit_code":0"#, r#""exit_code":-2147483648"#),
    );
    validate_request(op, Some(&negative)).unwrap();

    let op = OperationId::CreateCheckpoint;
    let checkpoint = |sections: &str| {
        parse_strict(&format!(
            r#"{{"schema_version":1,{ids},"objective":"ship",{sections}}}"#
        ))
        .unwrap()
    };
    validate_request(
        op,
        Some(&checkpoint(
            r#""verified_completed_work":[{"classification":"verified_fact","text":"tests pass",
                "evidence_ids":["018f47a0-0000-7000-8000-000000000005"]}],
               "next_action":{"classification":"recommendation","text":"merge"}"#,
        )),
    )
    .unwrap();
    for invalid in [
        checkpoint(r#""open_items":[]"#),
        checkpoint(r#""next_action":{"classification":"guess","text":"x"}"#),
        checkpoint(r#""next_action":{"classification":"unknown","text":""}"#),
        checkpoint(
            r#""next_action":{"classification":"unknown","text":"x",
                "evidence_ids":["018f47a0-0000-7000-8000-000000000005","018f47a0-0000-7000-8000-000000000005"]}"#,
        ),
    ] {
        assert!(validate_request(op, Some(&invalid)).is_err(), "{invalid:?}");
    }
    let resource =
        parse_strict(r#"{"schema_version":1,"id":"018f47a0-0000-7000-8000-000000000005","revision":1,"anything":[1]}"#)
            .unwrap();
    validate_response(
        OperationId::GetCheckpoint,
        200,
        "application/json",
        &resource,
    )
    .unwrap();
    validate_response(
        OperationId::CaptureEvidence,
        201,
        "application/json",
        &resource,
    )
    .unwrap();
}

/// ADR 0012 D6 (P2-B): the `getRecoveryPreview` binding and its document.
#[test]
#[allow(clippy::too_many_lines)] // The binding, then every valid and invalid document.
fn recovery_preview_binding_and_document_are_enforced() {
    let op = OperationId::GetRecoveryPreview;
    let binding = binding_for("getRecoveryPreview").unwrap();
    assert_eq!(
        (binding.method, binding.path),
        ("GET", "/v1/tasks/{task_id}/recovery-preview")
    );
    assert_eq!(binding.access_rule.scopes, ["control", "mcp-read"]);
    assert!(!binding.request_body_required());
    validate_request(op, None).unwrap();
    assert!(validate_request(op, Some(&parse_strict("{}").unwrap())).is_err());

    let id = "018f47a0-0000-7000-8000-000000000001";
    let digest = format!("sha256:{}", "a".repeat(64));
    // `outcome` holds the members run_status and argv (P2-E, P2-F).
    let text = |status: &str, reasons: &str, paths: &str, outcome: &str| {
        format!(
            r#"{{"schema_version":1,"ontology_version":"continuity.work-state-ontology.v3",
                "task_id":"{id}","status":"{status}","reasons":{reasons},
                "bindings":{{"task":{{"task_id":"{id}","revision":1,"status":"in_progress"}},
                    "ledger":{{"project_sequence":5,"chain_valid":true}},
                    "checkpoint":null,"snapshot":null,
                    "probe":{{"workspace_id":null,"available":false}}}},
                "facts":[{{"claim":{{"checkpoint_id":"{id}","section":"verified_completed_work",
                        "ordinal":0}},
                    "classification":"verified_fact","text":"tests pass","propagates":true,
                    "verdict":"{status}","reasons":{reasons},
                    "evidence":[{{"evidence_id":"{id}","kind":"test_result",
                        "canonical_digest":"{digest}",
                        {outcome},"verdict":"stale","reasons":["head_changed"]}}]}}],
                "invalid_evidence_ids":["{id}"],"changed_paths":{paths},
                "changed_paths_complete":true,"gaps":[],"next_checks":[],"sessions":[],
                "events_since_checkpoint":[],"preview_digest":"{digest}"}}"#
        )
    };
    let document = |status: &str, reasons: &str, paths: &str| {
        parse_strict(&text(
            status,
            reasons,
            paths,
            r#""run_status":"PASS","argv":["sh","t"]"#,
        ))
        .unwrap()
    };
    validate_response(
        op,
        200,
        "application/json",
        &document(
            "stale",
            r#"["head_changed","evidence_stale"]"#,
            r#"["repo://README"]"#,
        ),
    )
    .unwrap();
    // P2-F: evidence_not_passed is a reason, at the top, on a fact, and on
    // an Evidence object.
    let not_passed = text(
        "partial",
        r#"["evidence_not_passed"]"#,
        "[]",
        r#""run_status":"FAIL","argv":["sh","t"]"#,
    )
    .replace(
        r#""verdict":"stale","reasons":["head_changed"]"#,
        r#""verdict":"partial","reasons":["evidence_not_passed"]"#,
    );
    validate_response(
        op,
        200,
        "application/json",
        &parse_strict(&not_passed).unwrap(),
    )
    .unwrap();
    // P2-E: run_status is required, an FR-203 outcome or null. P2-F: argv
    // is required, a non-empty array of strings or null.
    let with_outcome = |value: &str| parse_strict(&text("stale", "[]", "[]", value)).unwrap();
    for valid in [
        r#""run_status":null,"argv":null"#,
        r#""run_status":"FAIL","argv":["cargo","test"]"#,
    ] {
        validate_response(op, 200, "application/json", &with_outcome(valid)).unwrap();
    }
    for invalid in [
        with_outcome(r#""run_status":"PASSED","argv":null"#),
        with_outcome(r#""run_status_missing":"PASS","argv":null"#),
        with_outcome(r#""run_status":"PASS""#),
        with_outcome(r#""run_status":"PASS","argv":[]"#),
        with_outcome(r#""run_status":"PASS","argv":"sh t""#),
        document("fresh", "[]", "[]"),
        document("stale", r#"["moved"]"#, "[]"),
        document("stale", r#"["head_changed","head_changed"]"#, "[]"),
        document("stale", "[]", r#"["README"]"#),
        parse_strict(r#"{"schema_version":1}"#).unwrap(),
    ] {
        assert!(
            validate_response(op, 200, "application/json", &invalid).is_err(),
            "{invalid:?}"
        );
    }
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

/// P2-C: `verifyLedger` keeps its frozen method, path, `ProjectCommand`
/// body, and required `Idempotency-Key`; its `202 Accepted` is replaced by
/// `200 LedgerVerification` (Owner decision 2026-10-02).
#[test]
#[allow(clippy::too_many_lines)] // The binding, then every valid and invalid document.
fn verify_ledger_binding_and_document_are_enforced() {
    let op = OperationId::VerifyLedger;
    let binding = binding_for("verifyLedger").unwrap();
    assert_eq!(
        (binding.method, binding.path),
        ("POST", "/v1/verification/ledger")
    );
    assert_eq!(binding.access_rule.scopes, ["control", "mcp-read"]);
    assert!(binding.request_body_required());
    assert_eq!(
        binding.success_contracts().collect::<Vec<_>>(),
        [(200, "application/json")]
    );
    let operation = OPENAPI
        .split("operationId: verifyLedger\n")
        .nth(1)
        .expect("verifyLedger operation");
    let operation = &operation[..operation.find("/v1/conflicts:").expect("next path")];
    assert!(
        operation.contains(r##"parameters: [{ $ref: "#/components/parameters/IdempotencyKey" }]"##)
    );
    assert!(operation.contains(r##"schema: { $ref: "#/components/schemas/ProjectCommand" }"##));
    assert!(operation.contains(r##"schema: { $ref: "#/components/schemas/LedgerVerification" }"##));
    assert!(!operation.contains("responses/Accepted"));

    let id = "018f47a0-0000-7000-8000-000000000001";
    validate_request(
        op,
        Some(&parse_strict(&format!(r#"{{"schema_version":1,"project_id":"{id}"}}"#)).unwrap()),
    )
    .unwrap();
    for invalid in [
        r#"{"schema_version":1}"#.to_owned(),
        r#"{"schema_version":1,"project_id":"nope"}"#.to_owned(),
        format!(r#"{{"schema_version":1,"project_id":"{id}","extra":1}}"#),
    ] {
        assert!(
            validate_request(op, Some(&parse_strict(&invalid).unwrap())).is_err(),
            "{invalid}"
        );
    }

    let digest = format!("sha256:{}", "a".repeat(64));
    let document = |valid: bool, head: &str, failure: &str| {
        parse_strict(&format!(
            r#"{{"schema_version":1,"project_id":"{id}","project_sequence":7,"valid":{valid},
                "checked":{{"events":7,"evidence":4,"snapshots":2,"checkpoints":1,"tasks":3}},
                "head_event_sha256":{head},"first_failure":{failure}}}"#
        ))
        .unwrap()
    };
    let event_failure = r#"{"record_type":"event","record_id":"x","project_sequence":3,
        "reason":"stored Event digest mismatch"}"#;
    let evidence_failure = r#"{"record_type":"evidence","record_id":"y","project_sequence":null,
        "reason":"Evidence digest validation"}"#;
    // P2-E: a Task row that disagrees with its last Task Event.
    let task_failure = r#"{"record_type":"task","record_id":"z","project_sequence":null,
        "reason":"status completed disagrees with its last Task Event"}"#;
    for valid in [
        document(true, &format!(r#""{digest}""#), "null"),
        document(true, "null", "null"),
        document(false, &format!(r#""{digest}""#), event_failure),
        document(false, "null", evidence_failure),
        document(false, "null", task_failure),
    ] {
        validate_response(op, 200, "application/json", &valid).unwrap();
    }
    for invalid in [
        document(false, "null", "null"),
        document(true, "null", event_failure),
        // P2-E: checked.tasks is required.
        parse_strict(&format!(
            r#"{{"schema_version":1,"project_id":"{id}","project_sequence":7,"valid":true,
                "checked":{{"events":7,"evidence":4,"snapshots":2,"checkpoints":1}},
                "head_event_sha256":null,"first_failure":null}}"#
        ))
        .unwrap(),
        document(true, r#""abc""#, "null"),
        document(false, "null", &event_failure.replace("event", "row")),
        document(
            false,
            "null",
            &event_failure.replace(r#""project_sequence":3"#, r#""project_sequence":0"#),
        ),
        document(
            false,
            "null",
            &event_failure.replace("stored Event digest mismatch", ""),
        ),
    ] {
        assert!(
            validate_response(op, 200, "application/json", &invalid).is_err(),
            "{invalid:?}"
        );
    }
    assert!(
        validate_response(op, 202, "application/json", &document(true, "null", "null")).is_err()
    );
    // P4-H1 (review of the first P4-H1 commit): a state database that is
    // not bound to the Project is `409 capsule_tampered`.
    assert_eq!(
        binding.problem_contracts().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            (400, ProblemCode::InvalidRequest),
            (401, ProblemCode::AuthenticationRequired),
            (403, ProblemCode::PolicyDenied),
            (404, ProblemCode::ProjectNotFound),
            (409, ProblemCode::CapsuleTampered),
            (503, ProblemCode::ServiceNotReady),
        ])
    );
}

/// P2-C: `CommandRunMetadata.launch_error` records a command that could not
/// be started; it must be a non-empty string.
#[test]
fn command_run_metadata_accepts_a_launch_error() {
    let digest = format!("sha256:{}", "0".repeat(64));
    let capture = |extra: &str| {
        parse_strict(&format!(
            r#"{{"schema_version":1,"workspace_id":"018f47a0-0000-7000-8000-000000000001",
                "task_id":"018f47a0-0000-7000-8000-000000000002",
                "session_id":"018f47a0-0000-7000-8000-000000000003",
                "snapshot_id":"018f47a0-0000-7000-8000-000000000004","artifact_type":"test_run",
                "metadata":{{"argv":["missing-tool"],"exit_code":-1,
                    "started_at":"2026-10-02T10:00:00Z","finished_at":"2026-10-02T10:00:00Z",
                    "stdout_tail":"","stderr_tail":"","stdout_sha256":"{digest}",
                    "stderr_sha256":"{digest}","collector":{{"name":"memprove-cli","version":"0.1.0"}}{extra}}}}}"#
        ))
        .unwrap()
    };
    let op = OperationId::CaptureEvidence;
    validate_request(
        op,
        Some(&capture(r#","launch_error":"No such file or directory""#)),
    )
    .unwrap();
    validate_request(op, Some(&capture(""))).unwrap();
    for invalid in [
        r#","launch_error":"""#,
        r#","launch_error":null"#,
        r#","launch_error":2"#,
    ] {
        assert!(
            validate_request(op, Some(&capture(invalid))).is_err(),
            "{invalid}"
        );
    }
}

/// P2-C (ADR 0012 D7): the envelope enum gains `memproved`'s four Task
/// and Workspace records, and `payload_schema` admits `_` so that each has
/// the `<event_type>@1` schema; an Event without a Session lists as `null`.
#[test]
fn p2c_event_types_and_payload_schema_match_the_envelope_schema() {
    for added in [
        "task.created",
        "task.transitioned",
        "task.session_linked",
        "workspace.registered",
    ] {
        assert!(EVENT_SCHEMA.contains(&format!("\"{added}\"")), "{added}");
        assert_eq!(added.parse::<EventType>().unwrap().as_str(), added);
    }
    assert!(EVENT_SCHEMA.contains(r#""pattern": "^[a-z][a-z0-9._]+@[1-9][0-9]*$""#));
    let envelope = |session: &str, schema: &str| {
        parse_strict(&format!(
            r#"{{"envelope_version":"1.0","event_id":"018f0f00-0000-7000-8000-000000000001",
               "project_id":"018f0f00-0000-7000-8000-000000000002",
               "workspace_id":"018f0f00-0000-7000-8000-000000000003",
               "session_id":{session},"event_type":"task.session_linked",
               "payload_schema":"{schema}",
               "producer":{{"name":"continuityd","version":"0.1.0","sequence":4}},
               "privacy_class":"internal","payload":{{}}}}"#
        ))
        .unwrap()
    };
    let session = r#""018f0f00-0000-7000-8000-000000000004""#;
    validate_request(
        OperationId::IngestEvent,
        Some(&envelope(session, "task.session_linked@1")),
    )
    .unwrap();
    validate_request(
        OperationId::IngestEvent,
        Some(&envelope("null", "task.created@1")),
    )
    .unwrap();
    for invalid in [
        "Task.created@1",
        "_task@1",
        "task.created@0",
        "task-created@1",
    ] {
        assert!(
            validate_request(OperationId::IngestEvent, Some(&envelope(session, invalid))).is_err(),
            "{invalid}"
        );
    }
}

/// P2-K (T11 S-4): a producer or collector name a client submits holds only
/// ASCII letters, digits, `.`, `_`, and `-`; look-alikes of `memproved`
/// are refused before any handler sees them. A response returns a stored
/// name as recorded, so an Event recorded before P2-K still reads back.
#[test]
fn p2k_client_names_are_ascii_letters_digits_and_three_symbols() {
    assert!(OPENAPI.contains(r#"pattern: "^[A-Za-z0-9._-]+$""#));
    assert!(OPENAPI.contains(r##"schema: { $ref: "#/components/schemas/ClientEventEnvelope" }"##));
    assert!(OPENAPI.contains(r##"items: { $ref: "#/components/schemas/ClientEventEnvelope" }"##));
    assert!(OPENAPI.contains(r##"name: { $ref: "#/components/schemas/ClientName" }"##));
    let envelope = |name: &str| {
        parse_strict(&format!(
            r#"{{"envelope_version":"1.0","event_id":"018f0f00-0000-7000-8000-000000000001",
               "project_id":"018f0f00-0000-7000-8000-000000000002",
               "workspace_id":"018f0f00-0000-7000-8000-000000000003",
               "session_id":"018f0f00-0000-7000-8000-000000000004",
               "event_type":"turn.started","payload_schema":"turn.started@1",
               "producer":{{"name":{},"version":"1","sequence":0}},
               "privacy_class":"internal","payload":{{}}}}"#,
            serde_json_string(name)
        ))
        .unwrap()
    };
    let digest = format!("sha256:{}", "0".repeat(64));
    let capture = |name: &str| {
        parse_strict(&format!(
            r#"{{"schema_version":1,"workspace_id":"018f47a0-0000-7000-8000-000000000001",
                "task_id":"018f47a0-0000-7000-8000-000000000002",
                "session_id":"018f47a0-0000-7000-8000-000000000003",
                "snapshot_id":"018f47a0-0000-7000-8000-000000000004","artifact_type":"test_run",
                "metadata":{{"argv":["cargo","test"],"exit_code":0,
                    "started_at":"2026-10-02T10:00:00Z","finished_at":"2026-10-02T10:00:00Z",
                    "stdout_tail":"","stderr_tail":"","stdout_sha256":"{digest}",
                    "stderr_sha256":"{digest}","collector":{{"name":{},"version":"0.1.0"}}}}}}"#,
            serde_json_string(name)
        ))
        .unwrap()
    };
    let batch = |name: &str| {
        let event = envelope(name);
        parse_strict(&format!(r#"{{"events":[{}]}}"#, compact(&event))).unwrap()
    };
    for accepted in ["hook", "memprove-cli", "agent.x_2-B", "memprove", "0"] {
        validate_request(OperationId::IngestEvent, Some(&envelope(accepted)))
            .unwrap_or_else(|error| panic!("{accepted}: {error}"));
        validate_request(OperationId::IngestEventBatch, Some(&batch(accepted)))
            .unwrap_or_else(|error| panic!("{accepted}: {error}"));
        validate_request(OperationId::CaptureEvidence, Some(&capture(accepted)))
            .unwrap_or_else(|error| panic!("{accepted}: {error}"));
    }
    // T11 S-4's look-alikes that passed before P2-K, and other characters.
    for refused in [
        " continuityd",
        "\tmemproved",
        "\u{a0}continuityd",
        "c\u{43e}ntinuityd",
        "continuit\u{443}d",
        "\u{ff43}\u{ff4f}\u{ff4e}\u{ff54}\u{ff49}\u{ff4e}\u{ff55}\u{ff49}\u{ff54}\u{ff59}\u{ff44}",
        "\u{217d}ontinuityd",
        "\u{200b}memproved",
        "contin\u{200d}uityd",
        "continu\u{301}ityd",
        "cont\u{131}nuityd",
        "memproved ",
        "memproved\n",
        "memproved/1",
        "agent x",
        "agent:x",
        "",
    ] {
        assert!(
            validate_request(OperationId::IngestEvent, Some(&envelope(refused))).is_err(),
            "{refused:?}"
        );
        assert!(
            validate_request(OperationId::IngestEventBatch, Some(&batch(refused))).is_err(),
            "{refused:?}"
        );
        assert!(
            validate_request(OperationId::CaptureEvidence, Some(&capture(refused))).is_err(),
            "{refused:?}"
        );
    }
    validate_response(
        OperationId::GetEvent,
        200,
        "application/json",
        &envelope(" recorded before P2-K"),
    )
    .unwrap();
}

fn serde_json_string(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            character if u32::from(character) < 0x20 => {
                write!(out, "\\u{:04x}", u32::from(character)).unwrap();
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

fn compact(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_owned(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Number(lexeme) => lexeme.clone(),
        JsonValue::String(text) => serde_json_string(text),
        JsonValue::Array(items) => format!(
            "[{}]",
            items.iter().map(compact).collect::<Vec<_>>().join(",")
        ),
        JsonValue::Object(members) => format!(
            "{{{}}}",
            members
                .iter()
                .map(|(name, item)| format!("{}:{}", serde_json_string(name), compact(item)))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

/// ADR 0013 P3-C: `proposeDecision` is bound as amended (task, Session,
/// content, optional rationale, Evidence, and supersedes; control or
/// mcp-write), statements take `supersedes` and a Checkpoint `withdrawals`
/// (claim IDs, unique), and the envelope enum admits `claim.superseded`, so
/// `listEvents` and `getEvent` responses holding one validate.
#[test]
#[allow(clippy::too_many_lines)] // One binding, two request schemas, two responses.
fn p3c_decision_binding_claim_edges_and_the_envelope_enum() {
    const ID: &str = "018f0f00-0000-7000-8000-000000000001";
    let op = OperationId::ProposeDecision;
    let binding = binding_for("proposeDecision").unwrap();
    assert_eq!((binding.method, binding.path), ("POST", "/v1/decisions"));
    assert_eq!(binding.access_rule.scopes, ["control", "mcp-write"]);
    assert_eq!(
        binding.problem_contracts().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            (400, ProblemCode::InvalidRequest),
            (401, ProblemCode::AuthenticationRequired),
            (403, ProblemCode::PolicyDenied),
            // U9 R1 (Owner O4; ADR 0020).
            (403, ProblemCode::RoleForbidden),
            (404, ProblemCode::ResourceNotFound),
            (409, ProblemCode::IdempotencyConflict),
            (409, ProblemCode::InvalidTransition),
            (503, ProblemCode::ServiceNotReady),
        ])
    );
    assert!(OPENAPI.contains(
        "      operationId: proposeDecision\n      tags: [decisions]\n      description:"
    ));
    assert!(OPENAPI.contains("required: [schema_version, task_id, session_id, content]"));
    let claim = format!("{ID}/open_items/0");
    let decision = |extra: &str| {
        parse_strict(&format!(
            r#"{{"schema_version":1,"task_id":"{ID}","session_id":"{ID}","content":"use JCS"{extra}}}"#
        ))
        .unwrap()
    };
    for extra in [
        String::new(),
        r#","rationale":null"#.to_owned(),
        format!(
            r#","rationale":"why","evidence_ids":["{ID}"],"supersedes":["{claim}","decision/{ID}"]"#
        ),
    ] {
        validate_request(op, Some(&decision(&extra))).unwrap();
    }
    for extra in [
        r#","rationale":"""#.to_owned(),
        r#","project_id":"018f0f00-0000-7000-8000-000000000001""#.to_owned(),
        r#","decision_key":"k""#.to_owned(),
        r#","supersedes":["claim 1"]"#.to_owned(),
        format!(r#","supersedes":["{claim}","{claim}"]"#),
        format!(r#","supersedes":["{ID}/open_items/01"]"#),
        format!(r#","supersedes":["{ID}/decisions/0"]"#),
        r#","evidence_ids":["x"]"#.to_owned(),
    ] {
        assert!(
            validate_request(op, Some(&decision(&extra))).is_err(),
            "{extra}"
        );
    }
    assert!(
        validate_request(
            op,
            Some(
                &parse_strict(&format!(
                    r#"{{"schema_version":1,"task_id":"{ID}","session_id":"{ID}","content":""}}"#
                ))
                .unwrap()
            )
        )
        .is_err()
    );

    let checkpoint = |statement_extra: &str, extra: &str| {
        parse_strict(&format!(
            r#"{{"schema_version":1,"workspace_id":"{ID}","objective":"o","task_id":"{ID}",
               "session_id":"{ID}","snapshot_id":"{ID}",
               "open_items":[{{"classification":"unknown","text":"t"{statement_extra}}}],
               "next_action":{{"classification":"unknown","text":"n"}}{extra}}}"#
        ))
        .unwrap()
    };
    let op = OperationId::CreateCheckpoint;
    validate_request(
        op,
        Some(&checkpoint(
            &format!(r#","supersedes":["{claim}"]"#),
            &format!(r#","withdrawals":["decision/{ID}"]"#),
        )),
    )
    .unwrap();
    validate_request(
        op,
        Some(&checkpoint(r#","supersedes":[]"#, r#","withdrawals":[]"#)),
    )
    .unwrap();
    for (statement_extra, extra) in [
        (r#","supersedes":"x""#.to_owned(), String::new()),
        (r#","supersedes":["x"]"#.to_owned(), String::new()),
        (String::new(), r#","withdrawals":["x"]"#.to_owned()),
        (
            String::new(),
            format!(r#","withdrawals":["{claim}","{claim}"]"#),
        ),
        (r#","withdraws":[]"#.to_owned(), String::new()),
    ] {
        assert!(
            validate_request(op, Some(&checkpoint(&statement_extra, &extra))).is_err(),
            "{statement_extra} {extra}"
        );
    }

    // The envelope enum: claim.superseded is listed and parses.
    assert!(EVENT_SCHEMA.contains(r#""claim.superseded""#));
    assert_eq!(
        "claim.superseded".parse::<EventType>().unwrap().as_str(),
        "claim.superseded"
    );
    let event = format!(
        r#"{{"envelope_version":"1.0","event_id":"{ID}","project_id":"{ID}",
           "workspace_id":"{ID}","session_id":"{ID}","event_type":"claim.superseded",
           "payload_schema":"claim.superseded@1","occurred_at":"2026-10-03T10:00:00Z",
           "recorded_at":"2026-10-03T10:00:00Z",
           "producer":{{"name":"continuityd","version":"0.1.0","sequence":9}},
           "correlation_id":null,"causation_id":null,"privacy_class":"internal",
           "payload":{{"task_id":"{ID}","relation":"withdraws","target":"{claim}","by":null}},
           "project_sequence":9,"sequence":3,
           "payload_sha256":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#
    );
    let page = format!(
        r#"{{"schema_version":1,"snapshot_sequence":9,"items":[{event}],"next_cursor":null}}"#
    );
    validate_response(
        OperationId::ListEvents,
        200,
        "application/json",
        &parse_strict(&page).unwrap(),
    )
    .unwrap();
    validate_response(
        OperationId::GetEvent,
        200,
        "application/json",
        &parse_strict(&event).unwrap(),
    )
    .unwrap();
}

/// U1 (E1, Owner O8): the version response may carry one optional member,
/// `build`, the identity `memproved --version` prints; `OpenAPI` and the
/// response validator agree on it, and a response without it stays valid.
#[test]
fn u1e_version_build_member_matches_openapi() {
    let version_schema = OPENAPI
        .split("\n    Version:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n    ClientScope:\n").next())
        .expect("OpenAPI defines the Version schema");
    assert!(
        version_schema
            .contains("required: [schema_version, application_version, api_major, capabilities]"),
        "build stays optional: {version_schema}"
    );
    assert!(
        version_schema.contains(
            r#"build: { type: string, pattern: "^(?:[0-9a-f]{12}(?:-dirty)?|unknown)$" }"#
        ),
        "{version_schema}"
    );
    let response = |build: &str| {
        parse_strict(&format!(
            r#"{{"schema_version":1,"application_version":"0.1.0","api_major":1,"capabilities":["getVersion"]{build}}}"#
        ))
        .unwrap()
    };
    let validate = |body: &JsonValue| {
        validate_response(OperationId::GetVersion, 200, "application/json", body)
    };
    for build in [
        "",
        r#","build":"0123456789ab""#,
        r#","build":"0123456789ab-dirty""#,
        r#","build":"unknown""#,
    ] {
        validate(&response(build)).unwrap_or_else(|error| panic!("{build}: {error}"));
    }
    for build in [
        r#","build":"0123456789AB""#,
        r#","build":"0123456789a""#,
        r#","build":"0123456789abc""#,
        r#","build":"0123456789ab-clean""#,
        r#","build":"""#,
        r#","build":12"#,
        r#","build":null"#,
    ] {
        assert!(validate(&response(build)).is_err(), "{build}");
    }
}
