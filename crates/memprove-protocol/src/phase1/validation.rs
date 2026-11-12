use std::{cmp::Ordering, fmt, str::FromStr};

use crate::strict_json::{JsonValue, echoable_name};

use super::{
    registry::binding_for_id,
    types::{
        ClientScope, EventType, OperationId, PrivacyClass, ProblemCode, ProjectKind, SchemaId,
        SessionState, TaskState, TokenStatus,
    },
};

/// Classified failure from a frozen wire binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationErrorKind {
    /// A JSON value had the wrong primitive or container type.
    TypeMismatch {
        /// Expected JSON type.
        expected: &'static str,
        /// Actual JSON type.
        actual: &'static str,
    },
    /// A required object member was absent.
    MissingField(String),
    /// A request object contained a member outside the frozen schema.
    UnknownField(String),
    /// An enum-like field contained an unfrozen value.
    InvalidVariant {
        /// Field containing the invalid value.
        field: String,
        /// Invalid wire spelling.
        value: String,
    },
    /// A field had the right JSON type but violated another constraint.
    InvalidValue {
        /// Field or schema location.
        field: String,
        /// Stable diagnostic reason.
        reason: &'static str,
    },
    /// A required request body was absent.
    MissingBody,
    /// An operation with no request body received one.
    UnexpectedBody,
    /// The operation has no response binding for this status.
    UnsupportedStatus(u16),
    /// The response media type did not match the frozen status binding.
    MediaTypeMismatch {
        /// Expected media type.
        expected: &'static str,
        /// Supplied media type.
        actual: String,
    },
    /// A globally valid Problem pair is not declared for the operation.
    ProblemNotAllowed {
        /// Response status.
        status: u16,
        /// Stable problem code.
        code: String,
    },
}

/// Validation failure with a JSON-path-like location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Classified failure.
    pub kind: ValidationErrorKind,
    /// Location within the body or binding metadata.
    pub path: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "wire validation failed at {}: {:?}",
            self.path, self.kind
        )
    }
}

impl std::error::Error for ValidationError {}

/// Longest `detail` of a `400` Problem, in characters (`maxLength` of `detail` in the contract).
pub const PROBLEM_DETAIL_MAX_CHARS: usize = 512;

impl ValidationError {
    /// The `detail` of the `400` Problem for this request failure: the
    /// location (`$.field`) and the rule it violates.
    ///
    /// It never repeats a value from the request. A member name the schema
    /// does not define is repeated only when [`echoable_name`] allows it.
    #[must_use]
    pub fn detail(&self) -> String {
        let at = display_path(&self.path);
        let subject = if at == "$" {
            "the request body".to_owned()
        } else {
            at.clone()
        };
        match &self.kind {
            ValidationErrorKind::TypeMismatch { expected, actual } => {
                format!("{subject} must be {expected}, not {actual}")
            }
            ValidationErrorKind::MissingField(name) => format!("{at}.{name} is required"),
            ValidationErrorKind::UnknownField(name) => match echoable_name(name) {
                Some(name) => format!("{subject} has an unknown member {name}"),
                None => format!("{subject} has an unknown member"),
            },
            ValidationErrorKind::InvalidVariant { .. } => {
                format!("{subject} is not one of the allowed values")
            }
            ValidationErrorKind::InvalidValue { reason, .. } => format!("{subject}: {reason}"),
            ValidationErrorKind::MissingBody => "the request body is required".to_owned(),
            ValidationErrorKind::UnexpectedBody => {
                "this operation takes no request body".to_owned()
            }
            _ => format!("{subject} does not match the request schema"),
        }
    }
}

/// A validation path as the reader should see it: `$.field` below the body,
/// `$` for the body itself. The validators name a root `$create_token` or
/// `$register_project` after its schema; that name is dropped.
fn display_path(path: &str) -> String {
    let path = path.replace("$.$", "$");
    match path.strip_prefix('$') {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') => {
            let tail = rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');
            format!("${tail}")
        }
        _ => path,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeMode {
    StrictRequest,
    CompatibleResponse,
}

type ValidationResult<T = ()> = Result<T, ValidationError>;

/// Validates an optional request body through its operation binding.
///
/// Request objects are closed: any unknown member is rejected.
///
/// # Errors
///
/// Returns [`ValidationError`] when body presence or content differs from the
/// frozen operation schema.
pub fn validate_request(operation_id: OperationId, body: Option<&JsonValue>) -> ValidationResult {
    let binding = binding_for_id(operation_id);
    match (binding.request_schema, body) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(error(ValidationErrorKind::UnexpectedBody, "$request")),
        (Some(_), None) => Err(error(ValidationErrorKind::MissingBody, "$request")),
        (Some(schema), Some(body)) => validate_schema(schema, body, DecodeMode::StrictRequest),
    }
}

/// Validates a response through its operation, status, media, and schema binding.
///
/// Response objects accept unknown members for forward-compatible clients while
/// still requiring and validating every known frozen member.
///
/// # Errors
///
/// Returns [`ValidationError`] for an unsupported status/media pair, malformed
/// success payload, invalid Problem, or operation-specific Problem mismatch.
pub fn validate_response(
    operation_id: OperationId,
    status: u16,
    media_type: &str,
    body: &JsonValue,
) -> ValidationResult {
    let binding = binding_for_id(operation_id);
    if let Some(success) = binding
        .successes
        .iter()
        .find(|success| success.status == status)
    {
        if success.media_type != media_type {
            return Err(error(
                ValidationErrorKind::MediaTypeMismatch {
                    expected: success.media_type,
                    actual: media_type.to_owned(),
                },
                "$response.media_type",
            ));
        }
        return validate_schema(success.schema, body, DecodeMode::CompatibleResponse);
    }

    let problem_status_exists = binding
        .problems
        .iter()
        .any(|problem| problem.status == status);
    if !problem_status_exists {
        return Err(error(
            ValidationErrorKind::UnsupportedStatus(status),
            "$response.status",
        ));
    }
    if media_type != "application/problem+json" {
        return Err(error(
            ValidationErrorKind::MediaTypeMismatch {
                expected: "application/problem+json",
                actual: media_type.to_owned(),
            },
            "$response.media_type",
        ));
    }
    let code = validate_problem_payload(status, body)?;
    if binding
        .problems
        .iter()
        .any(|problem| problem.status == status && problem.code == code)
    {
        Ok(())
    } else {
        Err(error(
            ValidationErrorKind::ProblemNotAllowed {
                status,
                code: code.as_str().to_owned(),
            },
            "$response.problem",
        ))
    }
}

/// Validates a Problem against the global 22-pair status/code closure.
///
/// # Errors
///
/// Returns [`ValidationError`] for malformed fields, unknown codes, a payload
/// status mismatch, or an illegal status/code cross-pair.
pub fn validate_problem(status: u16, body: &JsonValue) -> ValidationResult {
    validate_problem_payload(status, body).map(|_| ())
}

/// Validates the `detail` rule of the Problems `memproved` emits (P3-A):
/// a `400` carries a non-empty `detail` of at most
/// [`PROBLEM_DETAIL_MAX_CHARS`] characters, and a `401` or `403` carries
/// none, except that a `403 role_forbidden` (U9 R1, ADR 0020) carries one
/// as a `400` does. Other statuses are not constrained.
///
/// [`validate_problem`] stays lenient on `detail` because the frozen Phase 0
/// fixtures hold `400` Problems without one; a daemon response is checked
/// with both.
///
/// # Errors
///
/// Returns [`ValidationError`] for a `400` without a usable `detail` and for
/// a `401` or `403` with one.
pub fn validate_problem_detail(status: u16, body: &JsonValue) -> ValidationResult {
    let object = expect_object(body, "$problem")?;
    let detail = optional(object, "detail");
    let role_forbidden = status == 403
        && optional(object, "code").and_then(JsonValue::as_str)
            == Some(ProblemCode::RoleForbidden.as_str());
    match (status, detail) {
        (403, None) if role_forbidden => Err(error(
            ValidationErrorKind::MissingField("detail".to_owned()),
            "$problem",
        )),
        (400 | 403, Some(detail)) if status == 400 || role_forbidden => {
            let text = expect_string(detail, "$.detail")?;
            if text.is_empty() || text.chars().count() > PROBLEM_DETAIL_MAX_CHARS {
                return Err(invalid_value(
                    "$.detail",
                    "expected 1 through 512 characters",
                ));
            }
            Ok(())
        }
        (400, None) => Err(error(
            ValidationErrorKind::MissingField("detail".to_owned()),
            "$problem",
        )),
        (401 | 403, Some(_)) => Err(invalid_value("$.detail", "must be absent for 401 and 403")),
        _ => Ok(()),
    }
}

/// Validates the exact frozen `WWW-Authenticate` challenge value.
///
/// # Errors
///
/// Returns [`ValidationError`] unless the JSON value is exactly the string
/// `Bearer`.
pub fn validate_www_authenticate(value: &JsonValue) -> ValidationResult {
    match value.as_str() {
        Some("Bearer") => Ok(()),
        Some(other) => Err(invalid_variant("WWW-Authenticate", other)),
        None => Err(type_mismatch("string", value, "$header.WWW-Authenticate")),
    }
}

fn validate_schema(schema: SchemaId, value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    match schema {
        SchemaId::NoBody => Err(error(ValidationErrorKind::UnexpectedBody, "$request")),
        SchemaId::Health => validate_health(value, mode),
        SchemaId::Status => validate_status(value, mode),
        SchemaId::Version => validate_version(value, mode),
        SchemaId::CreateClientTokenRequest => validate_create_token_request(value, mode),
        SchemaId::TokenCreated => validate_token_created(value, mode),
        SchemaId::TokenSummary => validate_token_summary(value, mode),
        SchemaId::TokenSummaryPage => validate_token_page(value, mode),
        SchemaId::RegisterProjectRequest => validate_register_project(value, mode),
        SchemaId::Project => validate_project(value, mode),
        SchemaId::ResolveWorkspaceRequest => validate_resolve_workspace(value, mode),
        SchemaId::Workspace => validate_workspace(value, mode),
        SchemaId::CreateSessionRequest => validate_create_session(value, mode),
        SchemaId::Session => validate_session(value, mode),
        SchemaId::SessionPage => validate_session_page(value, mode),
        SchemaId::ExpectedRevisionCommand => validate_expected_revision(value, mode),
        SchemaId::EventEnvelope => validate_event_envelope(value, mode, "$"),
        SchemaId::EventBatchRequest => validate_event_batch(value, mode),
        SchemaId::Accepted => validate_accepted(value, mode),
        SchemaId::EventPage => validate_event_page(value, mode),
        SchemaId::EventStream => validate_event_stream(value),
        SchemaId::CreateTaskRequest => validate_create_task(value, mode),
        SchemaId::Task => validate_task(value, mode, "$"),
        SchemaId::TaskPage => validate_task_page(value, mode),
        SchemaId::TaskTransitionRequest => validate_task_transition(value, mode),
        SchemaId::TaskDependencyRequest => validate_task_dependency(value, mode),
        SchemaId::Problem => validate_problem_payload_from_body(value).map(|_| ()),
        SchemaId::CreateClientTokenSuccess => Err(invalid_value(
            "$response",
            "sum type requires a status-specific schema",
        )),
        SchemaId::CaptureEvidenceRequest => validate_capture_evidence(value, mode),
        SchemaId::CreateCheckpointRequest => validate_create_checkpoint(value, mode),
        SchemaId::Resource => validate_resource(value, mode),
        SchemaId::RecoveryPreview => recovery_documents::validate_recovery_preview(value, mode),
        SchemaId::ProjectCommand => validate_project_command(value, mode),
        SchemaId::LedgerVerification => validate_ledger_verification(value, mode),
        SchemaId::TaskCapsule => recovery_documents::validate_task_capsule(value, mode),
        SchemaId::DecisionProposal => validate_decision_proposal(value, mode),
    }
}

/// Artifact types of the frozen `CaptureEvidenceRequest`.
const ARTIFACT_TYPES: [&str; 8] = [
    "git_commit",
    "file_content",
    "file_slice",
    "diff",
    "command_run",
    "test_run",
    "tool_call",
    "human_approval",
];

/// Longest `stdout_tail`/`stderr_tail`, in characters (ADR 0012).
pub const MAX_OUTPUT_TAIL_CHARS: usize = 4096;

/// ADR 0012 amends `CaptureEvidenceRequest` with `task_id` and `session_id`
/// and types `metadata` as `CommandRunMetadata` for `command_run`/`test_run`.
fn validate_capture_evidence(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let path = "$capture_evidence";
    let required_fields = [
        "schema_version",
        "workspace_id",
        "task_id",
        "session_id",
        "artifact_type",
        "snapshot_id",
        "metadata",
    ];
    let allowed = [
        "schema_version",
        "workspace_id",
        "task_id",
        "session_id",
        "artifact_type",
        "snapshot_id",
        "object_id",
        "metadata",
    ];
    let object = checked_object(value, &required_fields, &allowed, mode, path)?;
    schema_version(required(object, "schema_version", path)?)?;
    for field in ["workspace_id", "task_id", "session_id", "snapshot_id"] {
        uuid(required(object, field, path)?, &format!("$.{field}"))?;
    }
    let artifact_type = expect_string(required(object, "artifact_type", path)?, "$.artifact_type")?;
    if !ARTIFACT_TYPES.contains(&artifact_type) {
        return Err(invalid_variant("artifact_type", artifact_type));
    }
    if let Some(object_id) = optional(object, "object_id")
        && !matches!(object_id, JsonValue::Null)
    {
        sha256(object_id, "$.object_id")?;
    }
    let metadata = required(object, "metadata", path)?;
    if matches!(artifact_type, "command_run" | "test_run") {
        validate_command_run_metadata(metadata, mode).map_err(|error| rebase(error, "$.metadata"))
    } else {
        expect_object(metadata, "$.metadata").map(|_| ())
    }
}

fn validate_command_run_metadata(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let path = "$";
    let required_fields = [
        "argv",
        "exit_code",
        "started_at",
        "finished_at",
        "stdout_tail",
        "stderr_tail",
        "stdout_sha256",
        "stderr_sha256",
        "collector",
    ];
    let allowed = [
        "argv",
        "exit_code",
        "started_at",
        "finished_at",
        "stdout_tail",
        "stderr_tail",
        "stdout_sha256",
        "stderr_sha256",
        "collector",
        "test_name",
        "test_case_count",
        "test_format",
        "launch_error",
    ];
    let object = checked_object(value, &required_fields, &allowed, mode, path)?;
    let argv = expect_array(required(object, "argv", path)?, "$.argv")?;
    if argv.is_empty() {
        return Err(invalid_value("$.argv", "expected a non-empty array"));
    }
    for (index, argument) in argv.iter().enumerate() {
        non_empty_string(argument, &format!("$.argv[{index}]"))?;
    }
    int32(required(object, "exit_code", path)?, "$.exit_code")?;
    for field in ["started_at", "finished_at"] {
        let raw = expect_string(required(object, field, path)?, &format!("$.{field}"))?;
        if !(raw.ends_with('Z') && is_rfc3339_date_time(raw)) {
            return Err(invalid_value(
                &format!("$.{field}"),
                "expected an RFC 3339 UTC timestamp",
            ));
        }
    }
    for field in ["stdout_tail", "stderr_tail"] {
        let tail = expect_string(required(object, field, path)?, &format!("$.{field}"))?;
        if tail.chars().count() > MAX_OUTPUT_TAIL_CHARS {
            return Err(invalid_value(
                &format!("$.{field}"),
                "expected at most 4096 characters",
            ));
        }
    }
    for field in ["stdout_sha256", "stderr_sha256"] {
        sha256(required(object, field, path)?, &format!("$.{field}"))?;
    }
    let collector = required(object, "collector", path)?;
    let collector_fields = ["name", "version"];
    let collector_object = checked_object(
        collector,
        &collector_fields,
        &collector_fields,
        mode,
        "$.collector",
    )?;
    client_name(
        required(collector_object, "name", "$.collector")?,
        mode,
        "$.collector.name",
    )?;
    non_empty_string(
        required(collector_object, "version", "$.collector")?,
        "$.collector.version",
    )?;
    if let Some(test_name) = optional(object, "test_name") {
        non_empty_string(test_name, "$.test_name")?;
    }
    // P2-C: a command that could not be started (FR-203 `UNAVAILABLE`).
    if let Some(launch_error) = optional(object, "launch_error") {
        non_empty_string(launch_error, "$.launch_error")?;
    }
    // U9 K2 (Owner O6): the `--test-format` a test run was made with; that
    // it goes with a `test_run` is `memproved`'s check.
    if let Some(format) = optional(object, "test_format") {
        test_format(format)?;
    }
    if let Some(count) = optional(object, "test_case_count") {
        let count_integer = expect_integer(count, "$.test_case_count")?;
        if count_integer.compare_to(0) == Ordering::Less
            || count_integer.compare_to(MAX_SAFE_INTEGER) == Ordering::Greater
        {
            return Err(invalid_value(
                "$.test_case_count",
                "expected 0 through 9007199254740991",
            ));
        }
    }
    Ok(())
}

/// U9 K2 (Owner O6): a `test_format` is one of the closed set.
fn test_format(value: &JsonValue) -> ValidationResult {
    let format = expect_string(value, "$.test_format")?;
    if matches!(format, "cargo" | "pytest" | "cases") {
        Ok(())
    } else {
        Err(invalid_value(
            "$.test_format",
            "expected cargo, pytest, or cases",
        ))
    }
}

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// An array of unique claim IDs (ADR 0013 D1): a statement's or a
/// Decision's `supersedes`, a Checkpoint's `withdrawals` (P3-C).
fn claim_id_array(value: &JsonValue, path: &str) -> ValidationResult {
    let values = expect_array(value, path)?;
    ensure_unique(values, path)?;
    for (index, value) in values.iter().enumerate() {
        recovery_documents::claim_id(value, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

/// ADR 0013 D2 (P3-C) amends the frozen `DecisionProposal`: the Task, the
/// recording Session, the decision text in `content`, an optional
/// rationale, the Evidence it rests on, and the claims it supersedes.
fn validate_decision_proposal(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let path = "$decision_proposal";
    let required_fields = ["schema_version", "task_id", "session_id", "content"];
    let allowed = [
        "schema_version",
        "task_id",
        "session_id",
        "content",
        "rationale",
        "evidence_ids",
        "supersedes",
    ];
    let object = checked_object(value, &required_fields, &allowed, mode, path)?;
    schema_version(required(object, "schema_version", path)?)?;
    for field in ["task_id", "session_id"] {
        uuid(required(object, field, path)?, &format!("$.{field}"))?;
    }
    claim_text(required(object, "content", path)?, "$.content")?;
    if let Some(rationale) = optional(object, "rationale")
        && !matches!(rationale, JsonValue::Null)
    {
        claim_text(rationale, "$.rationale")?;
    }
    if let Some(evidence_ids) = optional(object, "evidence_ids") {
        uuid_array(evidence_ids, "$.evidence_ids")?;
    }
    if let Some(supersedes) = optional(object, "supersedes") {
        claim_id_array(supersedes, "$.supersedes")?;
    }
    Ok(())
}

/// ADR 0012 amends `CreateCheckpointRequest` with `task_id`, `session_id`,
/// `snapshot_id`, and the three statement sections; ADR 0013 (P3-C) adds
/// `withdrawals` and each statement's `supersedes`; P3-D2 adds
/// `withdrawal_reason` and the length limit of agent text.
fn validate_create_checkpoint(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let path = "$create_checkpoint";
    let required_fields = [
        "schema_version",
        "workspace_id",
        "objective",
        "task_id",
        "session_id",
        "snapshot_id",
        "next_action",
    ];
    let allowed = [
        "schema_version",
        "workspace_id",
        "objective",
        "task_id",
        "session_id",
        "snapshot_id",
        "verified_completed_work",
        "open_items",
        "next_action",
        "withdrawals",
        "withdrawal_reason",
    ];
    let object = checked_object(value, &required_fields, &allowed, mode, path)?;
    schema_version(required(object, "schema_version", path)?)?;
    for field in ["workspace_id", "task_id", "session_id", "snapshot_id"] {
        uuid(required(object, field, path)?, &format!("$.{field}"))?;
    }
    non_empty_string(required(object, "objective", path)?, "$.objective")?;
    if let Some(withdrawals) = optional(object, "withdrawals") {
        claim_id_array(withdrawals, "$.withdrawals")?;
    }
    if let Some(reason) = optional(object, "withdrawal_reason") {
        claim_text(reason, "$.withdrawal_reason")?;
    }
    for field in ["verified_completed_work", "open_items"] {
        if let Some(statements) = optional(object, field) {
            validate_array_items(statements, &format!("$.{field}"), |item, item_path| {
                validate_statement(item, mode).map_err(|error| rebase(error, item_path))
            })?;
        }
    }
    validate_statement(required(object, "next_action", path)?, mode)
        .map_err(|error| rebase(error, "$.next_action"))
}

fn validate_statement(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let required_fields = ["classification", "text"];
    let allowed = ["classification", "text", "evidence_ids", "supersedes"];
    let object = checked_object(value, &required_fields, &allowed, mode, "$")?;
    let classification =
        expect_string(required(object, "classification", "$")?, "$.classification")?;
    if !matches!(
        classification,
        "verified_fact" | "user_supplied_context" | "recommendation" | "unknown"
    ) {
        return Err(invalid_variant("classification", classification));
    }
    claim_text(required(object, "text", "$")?, "$.text")?;
    if let Some(evidence_ids) = optional(object, "evidence_ids") {
        uuid_array(evidence_ids, "$.evidence_ids")?;
    }
    if let Some(supersedes) = optional(object, "supersedes") {
        claim_id_array(supersedes, "$.supersedes")?;
    }
    Ok(())
}

/// The frozen open `Resource`: every member beyond the three required ones
/// is accepted.
fn validate_resource(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let path = "$resource";
    let fields = ["schema_version", "id", "revision"];
    let object = checked_object(value, &fields, &fields, mode, path)?;
    schema_version(required(object, "schema_version", path)?)?;
    uuid(required(object, "id", path)?, "$.id")?;
    positive_integer(required(object, "revision", path)?, "$.revision")
}

/// The frozen `ProjectCommand`, the `verifyLedger` request (P2-C).
fn validate_project_command(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let path = "$project_command";
    let fields = ["schema_version", "project_id"];
    let object = checked_object(value, &fields, &fields, mode, path)?;
    schema_version(required(object, "schema_version", path)?)?;
    uuid(required(object, "project_id", path)?, "$.project_id")
}

/// `LedgerVerification.first_failure.record_type` values (P2-C; `task`,
/// P2-E).
/// U1 B4 (Owner O5): `workspace` names a missing root Workspace row.
const LEDGER_RECORD_TYPES: [&str; 6] = [
    "event",
    "evidence",
    "snapshot",
    "checkpoint",
    "task",
    "workspace",
];

/// The `verifyLedger` result (P2-C): what was recomputed, the chain head,
/// and the first record that does not hold. `valid` is true exactly when
/// `first_failure` is null.
fn validate_ledger_verification(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let path = "$ledger_verification";
    let fields = [
        "schema_version",
        "project_id",
        "project_sequence",
        "valid",
        "checked",
        "head_event_sha256",
        "first_failure",
    ];
    let object = checked_object(value, &fields, &fields, mode, path)?;
    schema_version(required(object, "schema_version", path)?)?;
    uuid(required(object, "project_id", path)?, "$.project_id")?;
    non_negative_integer(
        required(object, "project_sequence", path)?,
        "$.project_sequence",
    )?;
    let valid = expect_bool(required(object, "valid", path)?, "$.valid")?;
    let checked_fields = ["events", "evidence", "snapshots", "checkpoints", "tasks"];
    let checked = checked_object(
        required(object, "checked", path)?,
        &checked_fields,
        &checked_fields,
        mode,
        "$.checked",
    )?;
    for field in checked_fields {
        non_negative_integer(
            required(checked, field, "$.checked")?,
            &format!("$.checked.{field}"),
        )?;
    }
    let head = required(object, "head_event_sha256", path)?;
    if !matches!(head, JsonValue::Null) {
        sha256(head, "$.head_event_sha256")?;
    }
    let failure = required(object, "first_failure", path)?;
    if matches!(failure, JsonValue::Null) != valid {
        return Err(invalid_value(
            "$.valid",
            "expected valid to be true exactly when first_failure is null",
        ));
    }
    if matches!(failure, JsonValue::Null) {
        return Ok(());
    }
    let failure_fields = ["record_type", "record_id", "project_sequence", "reason"];
    let failure = checked_object(
        failure,
        &failure_fields,
        &failure_fields,
        mode,
        "$.first_failure",
    )?;
    let record_type = expect_string(
        required(failure, "record_type", "$.first_failure")?,
        "$.first_failure.record_type",
    )?;
    if !LEDGER_RECORD_TYPES.contains(&record_type) {
        return Err(invalid_variant("first_failure.record_type", record_type));
    }
    expect_string(
        required(failure, "record_id", "$.first_failure")?,
        "$.first_failure.record_id",
    )?;
    let sequence = required(failure, "project_sequence", "$.first_failure")?;
    if !matches!(sequence, JsonValue::Null) {
        positive_integer(sequence, "$.first_failure.project_sequence")?;
    }
    non_empty_string(
        required(failure, "reason", "$.first_failure")?,
        "$.first_failure.reason",
    )
}

/// A JSON integer in the signed 32-bit range.
fn int32(value: &JsonValue, path: &str) -> ValidationResult {
    let integer = expect_integer(value, path)?;
    let within = if integer.negative {
        MathematicalInteger {
            negative: false,
            ..integer
        }
        .compare_to(2_147_483_648)
            != Ordering::Greater
    } else {
        integer.compare_to(2_147_483_647) != Ordering::Greater
    };
    if within {
        Ok(())
    } else {
        Err(invalid_value(path, "expected a 32-bit signed integer"))
    }
}

/// The `OpenAPI` `Sha256` spelling, `sha256:` and 64 lowercase hex digits.
fn sha256(value: &JsonValue, path: &str) -> ValidationResult {
    let raw = expect_string(value, path)?;
    let valid = raw.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err(invalid_value(path, "expected sha256:<64 lowercase hex>"))
    }
}

fn validate_health(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let object = checked_object(value, &["status"], &["status"], mode, "$health")?;
    exact_string(required(object, "status", "$health")?, "status", "ok")
}

fn validate_status(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = [
        "schema_version",
        "ready",
        "writer_queue_depth",
        "project_count",
    ];
    let object = checked_object(value, &fields, &fields, mode, "$status")?;
    schema_version(required(object, "schema_version", "$status")?)?;
    expect_bool(required(object, "ready", "$status")?, "$.ready")?;
    non_negative_integer(
        required(object, "writer_queue_depth", "$status")?,
        "$.writer_queue_depth",
    )?;
    non_negative_integer(
        required(object, "project_count", "$status")?,
        "$.project_count",
    )
}

fn validate_version(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let required_fields = [
        "schema_version",
        "application_version",
        "api_major",
        "capabilities",
    ];
    let fields = [
        "schema_version",
        "application_version",
        "api_major",
        "capabilities",
        "build",
    ];
    let object = checked_object(value, &required_fields, &fields, mode, "$version")?;
    if let Some(build) = optional(object, "build") {
        build_identity(build, "$.build")?;
    }
    schema_version(required(object, "schema_version", "$version")?)?;
    non_empty_string(
        required(object, "application_version", "$version")?,
        "$.application_version",
    )?;
    exact_integer(required(object, "api_major", "$version")?, "api_major", 1)?;
    string_array(
        required(object, "capabilities", "$version")?,
        "$.capabilities",
        true,
        false,
    )
}

/// U1 (E1, Owner O8): the build identity of the version response,
/// `^(?:[0-9a-f]{12}(?:-dirty)?|unknown)$`.
fn build_identity(value: &JsonValue, path: &str) -> ValidationResult {
    let text = expect_string(value, path)?;
    let commit = text.strip_suffix("-dirty").unwrap_or(text);
    let is_commit = commit.len() == 12
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if text == "unknown" || is_commit {
        Ok(())
    } else {
        Err(invalid_value(
            path,
            "expected 12 lowercase hex characters, optionally followed by -dirty, or unknown",
        ))
    }
}

fn validate_create_token_request(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = ["schema_version", "client_label", "scopes"];
    let object = checked_object(value, &fields, &fields, mode, "$create_token")?;
    schema_version(required(object, "schema_version", "$create_token")?)?;
    client_label(required(object, "client_label", "$create_token")?)?;
    client_scopes(required(object, "scopes", "$create_token")?)
}

fn validate_token_created(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = [
        "schema_version",
        "token_id",
        "client_label",
        "scopes",
        "secret",
    ];
    let object = checked_object(value, &fields, &fields, mode, "$token_created")?;
    schema_version(required(object, "schema_version", "$token_created")?)?;
    uuid(
        required(object, "token_id", "$token_created")?,
        "$.token_id",
    )?;
    client_label(required(object, "client_label", "$token_created")?)?;
    client_scopes(required(object, "scopes", "$token_created")?)?;
    let secret = expect_string(required(object, "secret", "$token_created")?, "$.secret")?;
    if secret.len() == 43
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(invalid_value(
            "$.secret",
            "expected 43 base64url characters",
        ))
    }
}

fn validate_token_summary(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = [
        "schema_version",
        "token_id",
        "client_label",
        "scopes",
        "status",
        "created_at",
        "revoked_at",
    ];
    let object = checked_object(value, &fields, &fields, mode, "$token")?;
    schema_version(required(object, "schema_version", "$token")?)?;
    uuid(required(object, "token_id", "$token")?, "$.token_id")?;
    client_label(required(object, "client_label", "$token")?)?;
    client_scopes(required(object, "scopes", "$token")?)?;
    let status = wire_variant::<TokenStatus>(required(object, "status", "$token")?, "status")?;
    rfc3339_date_time(required(object, "created_at", "$token")?, "$.created_at")?;
    let revoked_at = required(object, "revoked_at", "$token")?;
    match (status, revoked_at) {
        (TokenStatus::Active, JsonValue::Null) => Ok(()),
        (TokenStatus::Revoked, value) => rfc3339_date_time(value, "$.revoked_at"),
        _ => Err(invalid_value(
            "$.revoked_at",
            "must be null for active and a timestamp for revoked",
        )),
    }
}

fn validate_token_page(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = ["schema_version", "items", "next_cursor"];
    let object = checked_object(value, &fields, &fields, mode, "$token_page")?;
    schema_version(required(object, "schema_version", "$token_page")?)?;
    validate_array_items(
        required(object, "items", "$token_page")?,
        "$.items",
        |item, path| validate_token_summary_at(item, mode, path),
    )?;
    nullable_cursor(
        required(object, "next_cursor", "$token_page")?,
        "$.next_cursor",
    )
}

fn validate_token_summary_at(value: &JsonValue, mode: DecodeMode, path: &str) -> ValidationResult {
    validate_token_summary(value, mode).map_err(|error| rebase(error, path))
}

fn validate_register_project(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = ["schema_version", "root"];
    let object = checked_object(value, &fields, &fields, mode, "$register_project")?;
    schema_version(required(object, "schema_version", "$register_project")?)?;
    non_empty_string(required(object, "root", "$register_project")?, "$.root")
}

fn validate_project(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = [
        "schema_version",
        "project_id",
        "root_workspace_id",
        "kind",
        "revision",
    ];
    let object = checked_object(value, &fields, &fields, mode, "$project")?;
    schema_version(required(object, "schema_version", "$project")?)?;
    uuid(required(object, "project_id", "$project")?, "$.project_id")?;
    uuid(
        required(object, "root_workspace_id", "$project")?,
        "$.root_workspace_id",
    )?;
    wire_variant::<ProjectKind>(required(object, "kind", "$project")?, "kind")?;
    positive_integer(required(object, "revision", "$project")?, "$.revision")
}

fn validate_resolve_workspace(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = ["schema_version", "project_id", "path"];
    let object = checked_object(value, &fields, &fields, mode, "$resolve_workspace")?;
    schema_version(required(object, "schema_version", "$resolve_workspace")?)?;
    uuid(
        required(object, "project_id", "$resolve_workspace")?,
        "$.project_id",
    )?;
    non_empty_string(required(object, "path", "$resolve_workspace")?, "$.path")
}

fn validate_workspace(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = ["schema_version", "workspace_id", "project_id", "revision"];
    let object = checked_object(value, &fields, &fields, mode, "$workspace")?;
    schema_version(required(object, "schema_version", "$workspace")?)?;
    uuid(
        required(object, "workspace_id", "$workspace")?,
        "$.workspace_id",
    )?;
    uuid(
        required(object, "project_id", "$workspace")?,
        "$.project_id",
    )?;
    positive_integer(required(object, "revision", "$workspace")?, "$.revision")
}

/// ADR 0014 E6 (P4-F): `resumed_from`, the Pack a Session declares it
/// continues from: both digests `sha256:<64 lowercase hex>`, the ledger
/// sequence an integer from 0, the budget within the Pack's range (1,000
/// through 1,000,000 UTF-16 code units).
fn resumed_from(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    if matches!(value, JsonValue::Null) {
        return Ok(());
    }
    let fields = [
        "capsule_digest",
        "project_sequence",
        "pack_digest",
        "budget",
    ];
    let path = "$.resumed_from";
    let object = checked_object(value, &fields, &fields, mode, path)?;
    sha256(
        required(object, "capsule_digest", path)?,
        "$.resumed_from.capsule_digest",
    )?;
    sha256(
        required(object, "pack_digest", path)?,
        "$.resumed_from.pack_digest",
    )?;
    non_negative_integer(
        required(object, "project_sequence", path)?,
        "$.resumed_from.project_sequence",
    )?;
    let budget_at = "$.resumed_from.budget";
    let budget = expect_integer(required(object, "budget", path)?, budget_at)?;
    if budget.compare_to(1_000) == Ordering::Less
        || budget.compare_to(1_000_000) == Ordering::Greater
    {
        return Err(invalid_value(
            budget_at,
            "expected an integer from 1000 through 1000000 (the Pack's budget range)",
        ));
    }
    Ok(())
}

/// U8 A1 (Owner O4): the `agent` a Session declares, in a `createSession`
/// request and a `Session`: `name` (1 to 64 ASCII letters, digits, `_`, or
/// `-`), `role` (orchestrator, `sub_agent`, or human), and an optional
/// `parent_session_id` (a `UUIDv7`, or `null` in a request).
fn session_agent(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    session_agent_at(value, mode, "$.agent")
}

/// [`session_agent`] at `path` (U9 A7: also `$.ended_by.agent`).
fn session_agent_at(value: &JsonValue, mode: DecodeMode, path: &str) -> ValidationResult {
    let fields = ["name", "role", "parent_session_id"];
    let object = checked_object(value, &fields[..2], &fields, mode, path)?;
    let name_path = format!("{path}.name");
    let name = expect_string(required(object, "name", path)?, &name_path)?;
    if !(1..=64).contains(&name.len())
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(invalid_value(
            &name_path,
            "expected 1 to 64 ASCII letters, digits, _ or -",
        ));
    }
    let role_path = format!("{path}.role");
    let role = expect_string(required(object, "role", path)?, &role_path)?;
    if !matches!(role, "orchestrator" | "sub_agent" | "human") {
        return Err(invalid_variant(
            role_path.strip_prefix("$.").unwrap_or(&role_path),
            role,
        ));
    }
    if let Some(parent) = optional(object, "parent_session_id") {
        let parent_path = format!("{path}.parent_session_id");
        if mode == DecodeMode::StrictRequest {
            nullable_uuid(parent, &parent_path)?;
        } else {
            uuid(parent, &parent_path)?;
        }
    }
    Ok(())
}

/// U9 A7 (Owner O3): who ended a Session as abandoned, in an `endSession`
/// request and a `Session`: an object with an optional `session_id` (a
/// `UUIDv7`, or `null` in a request) and an optional `agent` (as
/// [`session_agent`]); that one of them is given, that it comes with
/// `outcome: abandoned`, and that the Sessions exist is `memproved`'s
/// check.
fn ended_by(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let path = "$.ended_by";
    let object = checked_object(value, &[], &["session_id", "agent"], mode, path)?;
    if let Some(session_id) = optional(object, "session_id") {
        if mode == DecodeMode::StrictRequest {
            nullable_uuid(session_id, "$.ended_by.session_id")?;
        } else {
            uuid(session_id, "$.ended_by.session_id")?;
        }
    }
    if let Some(agent) = optional(object, "agent")
        && !(mode == DecodeMode::StrictRequest && matches!(agent, JsonValue::Null))
    {
        session_agent_at(agent, mode, "$.ended_by.agent")?;
    }
    Ok(())
}

/// ADR 0012 adds the optional, nullable `task_id`; ADR 0014 E6 the
/// optional, nullable `resumed_from`; U8 A1 (Owner O4) the optional,
/// nullable `agent`.
fn validate_create_session(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let required_fields = ["schema_version", "workspace_id", "agent_id"];
    let allowed = [
        "schema_version",
        "workspace_id",
        "agent_id",
        "task_id",
        "resumed_from",
        "agent",
    ];
    let object = checked_object(value, &required_fields, &allowed, mode, "$create_session")?;
    schema_version(required(object, "schema_version", "$create_session")?)?;
    uuid(
        required(object, "workspace_id", "$create_session")?,
        "$.workspace_id",
    )?;
    uuid(
        required(object, "agent_id", "$create_session")?,
        "$.agent_id",
    )?;
    if let Some(task_id) = optional(object, "task_id") {
        nullable_uuid(task_id, "$.task_id")?;
    }
    if let Some(value) = optional(object, "resumed_from") {
        resumed_from(value, mode)?;
    }
    if let Some(value) = optional(object, "agent")
        && !matches!(value, JsonValue::Null)
    {
        session_agent(value, mode)?;
    }
    Ok(())
}

/// U8 A1 (Owner O4): a Session that declared an agent has the optional
/// member `agent`; U9 A7 (Owner O3), one ended as abandoned by an ender
/// that reported itself, `ended_by`.
fn validate_session(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = [
        "schema_version",
        "session_id",
        "workspace_id",
        "agent_id",
        "state",
        "revision",
    ];
    let allowed = [
        "schema_version",
        "session_id",
        "workspace_id",
        "agent_id",
        "state",
        "revision",
        "agent",
        "ended_by",
    ];
    let object = checked_object(value, &fields, &allowed, mode, "$session")?;
    if let Some(agent) = optional(object, "agent") {
        session_agent(agent, mode)?;
    }
    if let Some(value) = optional(object, "ended_by") {
        ended_by(value, mode)?;
    }
    schema_version(required(object, "schema_version", "$session")?)?;
    uuid(required(object, "session_id", "$session")?, "$.session_id")?;
    uuid(
        required(object, "workspace_id", "$session")?,
        "$.workspace_id",
    )?;
    uuid(required(object, "agent_id", "$session")?, "$.agent_id")?;
    wire_variant::<SessionState>(required(object, "state", "$session")?, "state")?;
    positive_integer(required(object, "revision", "$session")?, "$.revision")
}

fn validate_session_page(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    validate_page(value, mode, "$session_page", false, |item, _path| {
        validate_session(item, mode)
    })
}

/// The body of `endSession`, the one routed operation bound to this schema
/// (contract schema `EndSessionRequest`): the expected revision, an optional
/// `reason`, (ADR 0013 D6, P3-D) an optional `outcome`, and (U9 A7, Owner
/// O3) an optional, nullable `ended_by`.
fn validate_expected_revision(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let required_fields = ["schema_version", "expected_revision"];
    let allowed = [
        "schema_version",
        "expected_revision",
        "reason",
        "outcome",
        "ended_by",
    ];
    let object = checked_object(value, &required_fields, &allowed, mode, "$revision_command")?;
    schema_version(required(object, "schema_version", "$revision_command")?)?;
    positive_integer(
        required(object, "expected_revision", "$revision_command")?,
        "$.expected_revision",
    )?;
    if let Some(reason) = optional(object, "reason") {
        non_empty_string(reason, "$.reason")?;
    }
    if let Some(outcome) = optional(object, "outcome")
        && !matches!(
            expect_string(outcome, "$.outcome")?,
            "completed" | "abandoned"
        )
    {
        return Err(invalid_value(
            "$.outcome",
            "expected completed or abandoned",
        ));
    }
    if let Some(value) = optional(object, "ended_by")
        && !matches!(value, JsonValue::Null)
    {
        ended_by(value, mode)?;
    }
    Ok(())
}

fn validate_event_envelope(value: &JsonValue, mode: DecodeMode, path: &str) -> ValidationResult {
    let required_fields = [
        "envelope_version",
        "event_id",
        "project_id",
        "workspace_id",
        "event_type",
        "payload_schema",
        "producer",
        "privacy_class",
        "payload",
    ];
    let allowed = [
        "envelope_version",
        "event_id",
        "project_id",
        "workspace_id",
        "session_id",
        "event_type",
        "payload_schema",
        "occurred_at",
        "producer",
        "correlation_id",
        "causation_id",
        "privacy_class",
        "payload",
    ];
    let object = checked_object(value, &required_fields, &allowed, mode, path)?;
    exact_string(
        required(object, "envelope_version", path)?,
        "envelope_version",
        "1.0",
    )?;
    uuid(
        required(object, "event_id", path)?,
        &format!("{path}.event_id"),
    )?;
    uuid(
        required(object, "project_id", path)?,
        &format!("{path}.project_id"),
    )?;
    uuid(
        required(object, "workspace_id", path)?,
        &format!("{path}.workspace_id"),
    )?;
    if let Some(value) = optional(object, "session_id") {
        nullable_uuid(value, &format!("{path}.session_id"))?;
    }
    wire_variant::<EventType>(required(object, "event_type", path)?, "event_type")?;
    payload_schema(
        required(object, "payload_schema", path)?,
        &format!("{path}.payload_schema"),
    )?;
    if let Some(value) = optional(object, "occurred_at") {
        nullable_rfc3339_utc(value, &format!("{path}.occurred_at"))?;
    }
    validate_producer(
        required(object, "producer", path)?,
        mode,
        &format!("{path}.producer"),
    )?;
    for field in ["correlation_id", "causation_id"] {
        if let Some(value) = optional(object, field) {
            nullable_uuid(value, &format!("{path}.{field}"))?;
        }
    }
    wire_variant::<PrivacyClass>(required(object, "privacy_class", path)?, "privacy_class")?;
    expect_object(
        required(object, "payload", path)?,
        &format!("{path}.payload"),
    )?;
    Ok(())
}

fn validate_producer(value: &JsonValue, mode: DecodeMode, path: &str) -> ValidationResult {
    let fields = ["name", "version", "sequence"];
    let object = checked_object(value, &fields, &fields, mode, path)?;
    client_name(
        required(object, "name", path)?,
        mode,
        &format!("{path}.name"),
    )?;
    non_empty_string(
        required(object, "version", path)?,
        &format!("{path}.version"),
    )?;
    let sequence = required(object, "sequence", path)?;
    let sequence_path = format!("{path}.sequence");
    non_negative_integer(sequence, &sequence_path)?;
    // ADR 0010 Q2: `maximum: 9007199254740991`, so the digest stays exact.
    if expect_integer(sequence, &sequence_path)?.compare_to(MAX_PRODUCER_SEQUENCE)
        == Ordering::Greater
    {
        return Err(invalid_value(
            &sequence_path,
            "expected at most 9007199254740991",
        ));
    }
    Ok(())
}

/// `producer.sequence` maximum (2^53 − 1) from the envelope schema.
const MAX_PRODUCER_SEQUENCE: u64 = 9_007_199_254_740_991;

fn validate_event_batch(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = ["events"];
    let object = checked_object(value, &fields, &fields, mode, "$event_batch")?;
    let events = expect_array(required(object, "events", "$event_batch")?, "$.events")?;
    if events.is_empty() || events.len() > 1000 {
        return Err(invalid_value("$.events", "expected 1 through 1000 events"));
    }
    for (index, event) in events.iter().enumerate() {
        validate_event_envelope(event, mode, &format!("$.events[{index}]"))?;
    }
    Ok(())
}

fn validate_accepted(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let required_fields = ["schema_version", "request_id", "status"];
    let allowed = ["schema_version", "request_id", "status", "resource_id"];
    let object = checked_object(value, &required_fields, &allowed, mode, "$accepted")?;
    schema_version(required(object, "schema_version", "$accepted")?)?;
    uuid(required(object, "request_id", "$accepted")?, "$.request_id")?;
    exact_string(
        required(object, "status", "$accepted")?,
        "status",
        "accepted",
    )?;
    if let Some(resource_id) = optional(object, "resource_id") {
        nullable_uuid(resource_id, "$.resource_id")?;
    }
    Ok(())
}

fn validate_event_page(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    validate_page(value, mode, "$event_page", false, |item, path| {
        validate_event_envelope(item, mode, path)
    })
}

fn validate_event_stream(value: &JsonValue) -> ValidationResult {
    let stream = expect_string(value, "$event_stream")?;
    let Some(rest) = stream.strip_prefix("id: ") else {
        return Err(invalid_value("$event_stream", "missing id prefix"));
    };
    let Some((id, rest)) = rest.split_once('\n') else {
        return Err(invalid_value("$event_stream", "missing id terminator"));
    };
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_value("$event_stream", "event id must be decimal"));
    }
    let Some(rest) = rest.strip_prefix("event: continuity") else {
        return Err(invalid_value(
            "$event_stream",
            "invalid event or data prefix",
        ));
    };
    let mut rest_chars = rest.chars();
    if rest_chars.next().is_none() {
        return Err(invalid_value(
            "$event_stream",
            "invalid event or data prefix",
        ));
    }
    let Some(rest) = rest_chars.as_str().strip_prefix("event\ndata: ") else {
        return Err(invalid_value(
            "$event_stream",
            "invalid event or data prefix",
        ));
    };
    let Some(data) = rest.strip_suffix("\n\n") else {
        return Err(invalid_value("$event_stream", "missing final blank line"));
    };
    if !data.starts_with('{') || !data.ends_with('}') || data.contains(['\r', '\n']) {
        return Err(invalid_value(
            "$event_stream",
            "data must match the frozen single-line object-shaped pattern",
        ));
    }
    Ok(())
}

fn validate_create_task(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let required_fields = ["schema_version", "project_id", "title"];
    let allowed = [
        "schema_version",
        "project_id",
        "workspace_id",
        "title",
        "description",
    ];
    let object = checked_object(value, &required_fields, &allowed, mode, "$create_task")?;
    schema_version(required(object, "schema_version", "$create_task")?)?;
    uuid(
        required(object, "project_id", "$create_task")?,
        "$.project_id",
    )?;
    if let Some(workspace_id) = optional(object, "workspace_id") {
        nullable_uuid(workspace_id, "$.workspace_id")?;
    }
    non_empty_string(required(object, "title", "$create_task")?, "$.title")?;
    if let Some(description) = optional(object, "description") {
        expect_string(description, "$.description")?;
    }
    Ok(())
}

fn validate_task(value: &JsonValue, mode: DecodeMode, path: &str) -> ValidationResult {
    let required_fields = [
        "schema_version",
        "task_id",
        "project_id",
        "title",
        "state",
        "revision",
        "dependency_ids",
        "completion_evidence_ids",
    ];
    let allowed = [
        "schema_version",
        "task_id",
        "project_id",
        "workspace_id",
        "title",
        "state",
        "revision",
        "dependency_ids",
        "completion_evidence_ids",
    ];
    let object = checked_object(value, &required_fields, &allowed, mode, path)?;
    schema_version(required(object, "schema_version", path)?)?;
    uuid(
        required(object, "task_id", path)?,
        &format!("{path}.task_id"),
    )?;
    uuid(
        required(object, "project_id", path)?,
        &format!("{path}.project_id"),
    )?;
    if let Some(workspace_id) = optional(object, "workspace_id") {
        nullable_uuid(workspace_id, &format!("{path}.workspace_id"))?;
    }
    non_empty_string(required(object, "title", path)?, &format!("{path}.title"))?;
    wire_variant::<TaskState>(required(object, "state", path)?, "state")?;
    positive_integer(
        required(object, "revision", path)?,
        &format!("{path}.revision"),
    )?;
    uuid_array(
        required(object, "dependency_ids", path)?,
        &format!("{path}.dependency_ids"),
    )?;
    uuid_array(
        required(object, "completion_evidence_ids", path)?,
        &format!("{path}.completion_evidence_ids"),
    )
}

fn validate_task_page(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    validate_page(value, mode, "$task_page", false, |item, path| {
        validate_task(item, mode, path)
    })
}

fn validate_task_transition(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let required_fields = ["schema_version", "expected_revision", "target_state"];
    let allowed = [
        "schema_version",
        "expected_revision",
        "target_state",
        "completion_evidence_ids",
        "force_reason",
        "workspace_id",
        "session_id",
    ];
    let object = checked_object(value, &required_fields, &allowed, mode, "$task_transition")?;
    schema_version(required(object, "schema_version", "$task_transition")?)?;
    positive_integer(
        required(object, "expected_revision", "$task_transition")?,
        "$.expected_revision",
    )?;
    wire_variant::<TaskState>(
        required(object, "target_state", "$task_transition")?,
        "target_state",
    )?;
    if let Some(ids) = optional(object, "completion_evidence_ids") {
        uuid_array(ids, "$.completion_evidence_ids")?;
    }
    if let Some(reason) = optional(object, "force_reason") {
        non_empty_string(reason, "$.force_reason")?;
    }
    // P2-C: the Workspace whose probe judges completion Evidence.
    if let Some(workspace_id) = optional(object, "workspace_id") {
        uuid(workspace_id, "$.workspace_id")?;
    }
    // U9 SF1-A (the Owner's O8 exception to R1): the asking Session, by
    // whose declared role a completion is judged.
    if let Some(session_id) = optional(object, "session_id") {
        uuid(session_id, "$.session_id")?;
    }
    Ok(())
}

fn validate_task_dependency(value: &JsonValue, mode: DecodeMode) -> ValidationResult {
    let fields = ["schema_version", "expected_revision", "dependency_task_id"];
    let object = checked_object(value, &fields, &fields, mode, "$task_dependency")?;
    schema_version(required(object, "schema_version", "$task_dependency")?)?;
    positive_integer(
        required(object, "expected_revision", "$task_dependency")?,
        "$.expected_revision",
    )?;
    uuid(
        required(object, "dependency_task_id", "$task_dependency")?,
        "$.dependency_task_id",
    )
}

fn validate_page(
    value: &JsonValue,
    mode: DecodeMode,
    path: &str,
    cursor_required: bool,
    mut validate_item: impl FnMut(&JsonValue, &str) -> ValidationResult,
) -> ValidationResult {
    let required_fields = if cursor_required {
        &[
            "schema_version",
            "snapshot_sequence",
            "items",
            "next_cursor",
        ][..]
    } else {
        &["schema_version", "snapshot_sequence", "items"][..]
    };
    let allowed = [
        "schema_version",
        "snapshot_sequence",
        "items",
        "next_cursor",
    ];
    let object = checked_object(value, required_fields, &allowed, mode, path)?;
    schema_version(required(object, "schema_version", path)?)?;
    non_negative_integer(
        required(object, "snapshot_sequence", path)?,
        &format!("{path}.snapshot_sequence"),
    )?;
    let items = expect_array(required(object, "items", path)?, &format!("{path}.items"))?;
    for (index, item) in items.iter().enumerate() {
        validate_item(item, &format!("{path}.items[{index}]"))?;
    }
    if let Some(cursor) = optional(object, "next_cursor") {
        nullable_cursor(cursor, &format!("{path}.next_cursor"))?;
    }
    Ok(())
}

fn validate_problem_payload(status: u16, body: &JsonValue) -> ValidationResult<ProblemCode> {
    let code = validate_problem_payload_from_body(body)?;
    let object = expect_object(body, "$problem")?;
    let payload_status = expect_integer(required(object, "status", "$problem")?, "$.status")?;
    if payload_status.compare_to(u64::from(status)) != Ordering::Equal {
        return Err(invalid_value("$.status", "does not match HTTP status"));
    }
    if code.status() != status {
        return Err(invalid_value("$.code", "illegal status/code pair"));
    }
    Ok(code)
}

fn validate_problem_payload_from_body(body: &JsonValue) -> ValidationResult<ProblemCode> {
    let required_fields = ["type", "title", "status", "code", "request_id", "retryable"];
    let allowed = [
        "type",
        "title",
        "status",
        "detail",
        "code",
        "request_id",
        "retryable",
    ];
    let object = checked_object(
        body,
        &required_fields,
        &allowed,
        DecodeMode::CompatibleResponse,
        "$problem",
    )?;
    non_empty_string(required(object, "type", "$problem")?, "$.type")?;
    non_empty_string(required(object, "title", "$problem")?, "$.title")?;
    let status = expect_integer(required(object, "status", "$problem")?, "$.status")?;
    if status.compare_to(400) == Ordering::Less || status.compare_to(599) == Ordering::Greater {
        return Err(invalid_value(
            "$.status",
            "expected integer from 400 through 599",
        ));
    }
    if let Some(detail) = optional(object, "detail") {
        expect_string(detail, "$.detail")?;
    }
    let code = wire_variant::<ProblemCode>(required(object, "code", "$problem")?, "code")?;
    uuid(required(object, "request_id", "$problem")?, "$.request_id")?;
    expect_bool(required(object, "retryable", "$problem")?, "$.retryable")?;
    Ok(code)
}

fn checked_object<'a>(
    value: &'a JsonValue,
    required_fields: &[&str],
    allowed: &[&str],
    mode: DecodeMode,
    path: &str,
) -> ValidationResult<&'a [(String, JsonValue)]> {
    let object = expect_object(value, path)?;
    for field in required_fields {
        if !object.iter().any(|(name, _)| name == field) {
            return Err(error(
                ValidationErrorKind::MissingField((*field).to_owned()),
                path,
            ));
        }
    }
    if mode == DecodeMode::StrictRequest
        && let Some((unknown, _)) = object
            .iter()
            .find(|(name, _)| !allowed.contains(&name.as_str()))
    {
        return Err(error(
            ValidationErrorKind::UnknownField(unknown.clone()),
            path,
        ));
    }
    Ok(object)
}

fn expect_object<'a>(
    value: &'a JsonValue,
    path: &str,
) -> ValidationResult<&'a [(String, JsonValue)]> {
    value
        .as_object()
        .ok_or_else(|| type_mismatch("object", value, path))
}

fn expect_array<'a>(value: &'a JsonValue, path: &str) -> ValidationResult<&'a [JsonValue]> {
    value
        .as_array()
        .ok_or_else(|| type_mismatch("array", value, path))
}

fn expect_string<'a>(value: &'a JsonValue, path: &str) -> ValidationResult<&'a str> {
    value
        .as_str()
        .ok_or_else(|| type_mismatch("string", value, path))
}

fn expect_bool(value: &JsonValue, path: &str) -> ValidationResult<bool> {
    value
        .as_bool()
        .ok_or_else(|| type_mismatch("boolean", value, path))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppendedZeroes {
    Finite(usize),
    BeyondUsize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MathematicalInteger {
    negative: bool,
    significant: String,
    appended_zeroes: AppendedZeroes,
}

impl MathematicalInteger {
    fn parse(raw: &str) -> Option<Self> {
        let (negative, unsigned) = match raw.strip_prefix('-') {
            Some(unsigned) => (true, unsigned),
            None => (false, raw),
        };
        let exponent_index = unsigned.find(['e', 'E']);
        let (mantissa, exponent) = exponent_index.map_or((unsigned, None), |index| {
            (&unsigned[..index], Some(&unsigned[index + 1..]))
        });
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || whole.len() > 1 && whole.starts_with('0')
            || (!fraction.is_empty() && !fraction.bytes().all(|byte| byte.is_ascii_digit()))
            || mantissa.ends_with('.')
        {
            return None;
        }
        let (exponent_negative, exponent_magnitude) = parse_exponent(exponent)?;

        let mut coefficient = String::with_capacity(whole.len().checked_add(fraction.len())?);
        coefficient.push_str(whole);
        coefficient.push_str(fraction);
        if coefficient.bytes().all(|byte| byte == b'0') {
            return Some(Self {
                negative: false,
                significant: "0".to_owned(),
                appended_zeroes: AppendedZeroes::Finite(0),
            });
        }

        let appended_zeroes = if exponent_negative {
            let magnitude = exponent_magnitude?;
            let discarded = fraction.len().checked_add(magnitude)?;
            discard_integral_zeroes(&mut coefficient, discarded)?;
            AppendedZeroes::Finite(0)
        } else {
            match exponent_magnitude {
                Some(magnitude) if magnitude < fraction.len() => {
                    discard_integral_zeroes(&mut coefficient, fraction.len() - magnitude)?;
                    AppendedZeroes::Finite(0)
                }
                Some(magnitude) => AppendedZeroes::Finite(magnitude - fraction.len()),
                None => AppendedZeroes::BeyondUsize,
            }
        };

        let first_significant = coefficient.bytes().position(|byte| byte != b'0')?;
        coefficient.drain(..first_significant);
        Some(Self {
            negative,
            significant: coefficient,
            appended_zeroes,
        })
    }

    fn compare_to(&self, bound: u64) -> Ordering {
        if self.negative {
            return Ordering::Less;
        }
        if self.significant == "0" {
            return 0_u64.cmp(&bound);
        }
        let AppendedZeroes::Finite(appended_zeroes) = self.appended_zeroes else {
            return Ordering::Greater;
        };
        let Some(digits) = self.significant.len().checked_add(appended_zeroes) else {
            return Ordering::Greater;
        };
        let bound = bound.to_string();
        match digits.cmp(&bound.len()) {
            Ordering::Equal => {
                let mut decimal = self.significant.clone();
                decimal.extend(std::iter::repeat_n('0', appended_zeroes));
                decimal.as_str().cmp(bound.as_str())
            }
            ordering => ordering,
        }
    }
}

fn parse_exponent(exponent: Option<&str>) -> Option<(bool, Option<usize>)> {
    let Some(exponent) = exponent else {
        return Some((false, Some(0)));
    };
    let (negative, digits) = match exponent.as_bytes().first() {
        Some(b'-') => (true, &exponent[1..]),
        Some(b'+') => (false, &exponent[1..]),
        _ => (false, exponent),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let magnitude = digits.bytes().try_fold(0_usize, |value, digit| {
        value
            .checked_mul(10)?
            .checked_add(usize::from(digit - b'0'))
    });
    Some((negative && magnitude != Some(0), magnitude))
}

fn discard_integral_zeroes(coefficient: &mut String, discarded: usize) -> Option<()> {
    let retained = coefficient.len().checked_sub(discarded)?;
    if coefficient.as_bytes()[retained..]
        .iter()
        .any(|byte| *byte != b'0')
    {
        return None;
    }
    coefficient.truncate(retained);
    Some(())
}

fn expect_integer(value: &JsonValue, path: &str) -> ValidationResult<MathematicalInteger> {
    let JsonValue::Number(raw) = value else {
        return Err(type_mismatch("integer", value, path));
    };
    MathematicalInteger::parse(raw).ok_or_else(|| type_mismatch("integer", value, path))
}

fn required<'a>(
    object: &'a [(String, JsonValue)],
    name: &str,
    path: &str,
) -> ValidationResult<&'a JsonValue> {
    object
        .iter()
        .find_map(|(key, value)| (key == name).then_some(value))
        .ok_or_else(|| error(ValidationErrorKind::MissingField(name.to_owned()), path))
}

fn optional<'a>(object: &'a [(String, JsonValue)], name: &str) -> Option<&'a JsonValue> {
    object
        .iter()
        .find_map(|(key, value)| (key == name).then_some(value))
}

fn schema_version(value: &JsonValue) -> ValidationResult {
    exact_integer(value, "schema_version", 1)
}

fn exact_integer(value: &JsonValue, field: &str, expected: u64) -> ValidationResult {
    let actual = expect_integer(value, &format!("$.{field}"))?;
    if actual.compare_to(expected) == Ordering::Equal {
        Ok(())
    } else {
        Err(invalid_value(
            &format!("$.{field}"),
            "unexpected integer constant",
        ))
    }
}

fn non_negative_integer(value: &JsonValue, path: &str) -> ValidationResult {
    if expect_integer(value, path)?.compare_to(0) == Ordering::Less {
        Err(invalid_value(path, "expected a non-negative integer"))
    } else {
        Ok(())
    }
}

fn positive_integer(value: &JsonValue, path: &str) -> ValidationResult {
    if expect_integer(value, path)?.compare_to(1) == Ordering::Less {
        Err(invalid_value(path, "expected a positive integer"))
    } else {
        Ok(())
    }
}

/// P3-D2 (Owner): the most characters (Unicode scalar values, JSON Schema's
/// `maxLength`) an agent may write in one statement text, a Decision's
/// content or rationale, or a withdrawal reason.
pub const CLAIM_TEXT_MAX_CHARS: usize = 4096;

/// A non-empty string of at most [`CLAIM_TEXT_MAX_CHARS`] characters.
fn claim_text(value: &JsonValue, path: &str) -> ValidationResult {
    let text = expect_string(value, path)?;
    if text.is_empty() {
        Err(invalid_value(path, "expected a non-empty string"))
    } else if text.chars().count() > CLAIM_TEXT_MAX_CHARS {
        Err(invalid_value(
            path,
            "expected at most 4096 characters (P3-D2 limit for agent text)",
        ))
    } else {
        Ok(())
    }
}

fn non_empty_string(value: &JsonValue, path: &str) -> ValidationResult {
    if expect_string(value, path)?.is_empty() {
        Err(invalid_value(path, "expected a non-empty string"))
    } else {
        Ok(())
    }
}

/// P2-K (T11 S-4): a producer or collector name a client submits holds only
/// ASCII letters, digits, `.`, `_`, and `-` (`^[A-Za-z0-9._-]+$`), so no
/// look-alike of `memproved` (leading space, Cyrillic letter, zero-width
/// character) is accepted. A response returns a stored name as recorded: an
/// Event from before P2-K may carry another.
fn client_name(value: &JsonValue, mode: DecodeMode, path: &str) -> ValidationResult {
    non_empty_string(value, path)?;
    let admissible = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
    if mode == DecodeMode::StrictRequest && !expect_string(value, path)?.bytes().all(admissible) {
        return Err(invalid_value(
            path,
            "expected only ASCII letters, digits, '.', '_', and '-'",
        ));
    }
    Ok(())
}

fn exact_string(value: &JsonValue, field: &str, expected: &'static str) -> ValidationResult {
    let actual = expect_string(value, &format!("$.{field}"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(invalid_variant(field, actual))
    }
}

fn wire_variant<T>(value: &JsonValue, field: &str) -> ValidationResult<T>
where
    T: FromStr,
{
    let raw = expect_string(value, &format!("$.{field}"))?;
    raw.parse().map_err(|_| invalid_variant(field, raw))
}

fn client_label(value: &JsonValue) -> ValidationResult {
    let label = expect_string(value, "$.client_label")?;
    if label.is_empty() || label.chars().count() > 128 {
        return Err(invalid_value(
            "$.client_label",
            "expected 1 through 128 characters",
        ));
    }
    if label.chars().any(char::is_control) {
        return Err(invalid_value(
            "$.client_label",
            "control characters are forbidden",
        ));
    }
    Ok(())
}

fn client_scopes(value: &JsonValue) -> ValidationResult {
    let values = expect_array(value, "$.scopes")?;
    if values.is_empty() {
        return Err(invalid_value("$.scopes", "expected at least one scope"));
    }
    ensure_unique(values, "$.scopes")?;
    for value in values {
        wire_variant::<ClientScope>(value, "scopes[]")?;
    }
    Ok(())
}

fn string_array(value: &JsonValue, path: &str, unique: bool, non_empty: bool) -> ValidationResult {
    let values = expect_array(value, path)?;
    if non_empty && values.is_empty() {
        return Err(invalid_value(path, "expected a non-empty array"));
    }
    if unique {
        ensure_unique(values, path)?;
    }
    for (index, value) in values.iter().enumerate() {
        expect_string(value, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

fn uuid_array(value: &JsonValue, path: &str) -> ValidationResult {
    let values = expect_array(value, path)?;
    ensure_unique(values, path)?;
    for (index, value) in values.iter().enumerate() {
        uuid(value, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

fn validate_array_items(
    value: &JsonValue,
    path: &str,
    mut validate: impl FnMut(&JsonValue, &str) -> ValidationResult,
) -> ValidationResult {
    let values = expect_array(value, path)?;
    for (index, item) in values.iter().enumerate() {
        validate(item, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

fn ensure_unique(values: &[JsonValue], path: &str) -> ValidationResult {
    for (index, value) in values.iter().enumerate() {
        if values[..index].contains(value) {
            return Err(invalid_value(path, "array items must be unique"));
        }
    }
    Ok(())
}

fn uuid(value: &JsonValue, path: &str) -> ValidationResult {
    let raw = expect_string(value, path)?;
    let bytes = raw.as_bytes();
    let valid = bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[14] == b'7'
        && bytes[18] == b'-'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        && bytes[23] == b'-'
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23)
                || byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
        });
    if valid {
        Ok(())
    } else {
        Err(invalid_value(path, "expected lowercase UUIDv7"))
    }
}

fn nullable_uuid(value: &JsonValue, path: &str) -> ValidationResult {
    if matches!(value, JsonValue::Null) {
        Ok(())
    } else {
        uuid(value, path)
    }
}

fn is_rfc3339_date_time(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b't'))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return false;
    }

    let Some(year) = decimal_component(bytes, 0, 4) else {
        return false;
    };
    let Some(month) = decimal_component(bytes, 5, 2) else {
        return false;
    };
    let Some(day) = decimal_component(bytes, 8, 2) else {
        return false;
    };
    let Some(hour) = decimal_component(bytes, 11, 2) else {
        return false;
    };
    let Some(minute) = decimal_component(bytes, 14, 2) else {
        return false;
    };
    let Some(second) = decimal_component(bytes, 17, 2) else {
        return false;
    };
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return false;
    }

    let mut timezone_index = 19;
    if bytes.get(timezone_index) == Some(&b'.') {
        timezone_index += 1;
        let fraction_start = timezone_index;
        while bytes.get(timezone_index).is_some_and(u8::is_ascii_digit) {
            timezone_index += 1;
        }
        if timezone_index == fraction_start {
            return false;
        }
    }

    let offset_minutes = match &bytes[timezone_index..] {
        [b'Z' | b'z'] => Some(0),
        [
            sign @ (b'+' | b'-'),
            hour_tens,
            hour_ones,
            b':',
            minute_tens,
            minute_ones,
        ] => {
            let Some(offset_hour) = decimal_pair(*hour_tens, *hour_ones) else {
                return false;
            };
            let Some(offset_minute) = decimal_pair(*minute_tens, *minute_ones) else {
                return false;
            };
            if offset_hour > 23 || offset_minute > 59 {
                None
            } else {
                let magnitude = i32::from(offset_hour) * 60 + i32::from(offset_minute);
                Some(if *sign == b'+' { magnitude } else { -magnitude })
            }
        }
        _ => None,
    };
    offset_minutes.is_some_and(|offset| {
        second < 60 || leap_second_is_at_utc_boundary(year, month, day, hour, minute, offset)
    })
}

fn leap_second_is_at_utc_boundary(
    year: u32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    offset_minutes: i32,
) -> bool {
    let Ok(local_minutes) = i32::try_from(hour * 60 + minute) else {
        return false;
    };
    let utc_minutes = local_minutes - offset_minutes;
    let (utc_minutes, day_shift) = if utc_minutes < 0 {
        (utc_minutes + 24 * 60, -1)
    } else if utc_minutes >= 24 * 60 {
        (utc_minutes - 24 * 60, 1)
    } else {
        (utc_minutes, 0)
    };
    if utc_minutes != 23 * 60 + 59 {
        return false;
    }

    shifted_date(year, month, day, day_shift)
        .is_some_and(|(_, month, day)| (month == 6 && day == 30) || (month == 12 && day == 31))
}

fn shifted_date(
    mut year: u32,
    mut month: u32,
    mut day: u32,
    day_shift: i32,
) -> Option<(u32, u32, u32)> {
    match day_shift {
        -1 if day > 1 => day -= 1,
        -1 => {
            if month > 1 {
                month -= 1;
            } else {
                year = year.checked_sub(1)?;
                month = 12;
            }
            day = days_in_month(year, month);
        }
        0 => {}
        1 if day < days_in_month(year, month) => day += 1,
        1 => {
            day = 1;
            if month < 12 {
                month += 1;
            } else {
                year = year.checked_add(1)?;
                month = 1;
            }
        }
        _ => return None,
    }
    Some((year, month, day))
}

fn decimal_component(bytes: &[u8], start: usize, width: usize) -> Option<u32> {
    bytes
        .get(start..start.checked_add(width)?)?
        .iter()
        .try_fold(0_u32, |value, byte| {
            byte.is_ascii_digit()
                .then(|| value * 10 + u32::from(*byte - b'0'))
        })
}

fn decimal_pair(tens: u8, ones: u8) -> Option<u8> {
    (tens.is_ascii_digit() && ones.is_ascii_digit()).then(|| (tens - b'0') * 10 + (ones - b'0'))
}

const fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

const fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn rfc3339_date_time(value: &JsonValue, path: &str) -> ValidationResult {
    let raw = expect_string(value, path)?;
    if is_rfc3339_date_time(raw) {
        Ok(())
    } else {
        Err(invalid_value(path, "expected an RFC 3339 date-time"))
    }
}

fn nullable_rfc3339_utc(value: &JsonValue, path: &str) -> ValidationResult {
    if matches!(value, JsonValue::Null) {
        Ok(())
    } else {
        let raw = expect_string(value, path)?;
        if raw.ends_with('Z') && is_rfc3339_date_time(raw) {
            Ok(())
        } else {
            Err(invalid_value(path, "expected an RFC 3339 UTC timestamp"))
        }
    }
}

fn nullable_cursor(value: &JsonValue, path: &str) -> ValidationResult {
    if matches!(value, JsonValue::Null) {
        Ok(())
    } else {
        non_empty_string(value, path)
    }
}

/// `^[a-z][a-z0-9._]+@[1-9][0-9]*$`. P2-C admits `_` so that the
/// `<event_type>@1` schema of `task.session_linked` is valid.
fn payload_schema(value: &JsonValue, path: &str) -> ValidationResult {
    let raw = expect_string(value, path)?;
    let Some((name, version)) = raw.rsplit_once('@') else {
        return Err(invalid_value(path, "expected name@positive-version"));
    };
    let name_valid = name.len() >= 2
        && name.as_bytes()[0].is_ascii_lowercase()
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'_'
        });
    let version_valid = !version.is_empty()
        && !version.starts_with('0')
        && version.bytes().all(|byte| byte.is_ascii_digit());
    if name_valid && version_valid {
        Ok(())
    } else {
        Err(invalid_value(path, "expected name@positive-version"))
    }
}

fn type_mismatch(expected: &'static str, value: &JsonValue, path: &str) -> ValidationError {
    error(
        ValidationErrorKind::TypeMismatch {
            expected,
            actual: value.type_name(),
        },
        path,
    )
}

fn invalid_variant(field: &str, value: &str) -> ValidationError {
    error(
        ValidationErrorKind::InvalidVariant {
            field: field.to_owned(),
            value: value.to_owned(),
        },
        &format!("$.{field}"),
    )
}

fn invalid_value(field: &str, reason: &'static str) -> ValidationError {
    error(
        ValidationErrorKind::InvalidValue {
            field: field.to_owned(),
            reason,
        },
        field,
    )
}

fn error(kind: ValidationErrorKind, path: &str) -> ValidationError {
    ValidationError {
        kind,
        path: path.to_owned(),
    }
}

fn rebase(mut error: ValidationError, path: &str) -> ValidationError {
    error.path = format!(
        "{path}{}",
        error.path.strip_prefix('$').unwrap_or(&error.path)
    );
    error
}

#[path = "recovery_documents.rs"]
mod recovery_documents;
