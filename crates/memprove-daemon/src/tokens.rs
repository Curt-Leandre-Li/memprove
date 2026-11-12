//! `createClientToken`, `listClientTokens`, and `revokeClientToken`.
//!
//! Check order follows the contract: readiness, authentication, authorization,
//! then request validation, then the idempotent registry transaction. A new
//! token's secret appears only in its `201` response; replays, lists, and
//! revocations return metadata without the secret or its verifier.

use memprove_domain::{Sha256Digest, UtcTimestamp};
use memprove_protocol::{
    phase1::{ClientScope, OperationId, ProblemCode, ValidationErrorKind, validate_request},
    strict_json::{JsonValue, echoable_name, parse_strict_bytes},
};
use memprove_store_sqlite::{
    Idempotent, NewClientToken, StoreError, StoredToken, TokenCommand, TokenState,
};
use serde_json::json;

use crate::{
    api::{AppState, MAX_REQUEST_BODY_BYTES, WireResponse, guard},
    auth::{self, generate_secret},
};

/// Default and maximum `listClientTokens` page sizes.
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 500;
/// `Idempotency-Key` length bounds from the contract.
const KEY_MIN_CHARS: usize = 16;
const KEY_MAX_CHARS: usize = 200;

const REVOKE_PREFIX: &str = "/v1/auth/tokens/";
const REVOKE_SUFFIX: &str = "/revoke";

/// Transport inputs of a token mutation.
#[derive(Debug, Clone, Copy)]
pub struct TokenRequest<'a> {
    /// Bearer credential, if any.
    pub bearer: Option<&'a str>,
    /// `Idempotency-Key` header, if present and visible ASCII.
    pub idempotency_key: Option<&'a str>,
    /// `Content-Type` header, if present and visible ASCII.
    pub content_type: Option<&'a str>,
    /// Request body; `None` when it could not be read.
    pub body: Option<&'a [u8]>,
}

fn problem(code: ProblemCode) -> WireResponse {
    WireResponse::problem(code)
}

fn unavailable(action: &str, error: &dyn std::fmt::Display) -> WireResponse {
    eprintln!("memproved: cannot {action}: {error}");
    problem(ProblemCode::ServiceNotReady)
}

/// `POST /v1/auth/tokens`.
#[must_use]
pub fn create_client_token(state: &AppState, request: &TokenRequest<'_>) -> WireResponse {
    let principal = match guard(&state.plane, OperationId::CreateClientToken, request.bearer) {
        Ok(principal) => principal,
        Err(denied) => return denied,
    };
    let key = match idempotency_key(request.idempotency_key) {
        Ok(key) => key,
        Err(detail) => return WireResponse::invalid(detail),
    };
    let (label, scopes) = match decode_create(request) {
        Ok(decoded) => decoded,
        Err(rejected) => return rejected,
    };
    let scope_names: Vec<&str> = scopes.iter().map(|scope| scope.as_str()).collect();
    let digest = request_digest(&json!({
        "operation_id": OperationId::CreateClientToken.as_str(),
        "client_label": label,
        "scopes": scope_names,
    }));
    let secret = match generate_secret() {
        Ok(secret) => secret,
        Err(error) => return unavailable("generate a token secret", &error),
    };
    let verifier = secret.verifier();
    let token_id = uuid::Uuid::now_v7().to_string();
    let now = UtcTimestamp::now();
    let command = TokenCommand {
        principal_token_id: principal.id(),
        idempotency_key: key,
        request_sha256: digest.as_str(),
        now: now.as_str(),
    };
    let token = NewClientToken {
        token_id: &token_id,
        client_label: &label,
        scopes: &scope_names,
        verifier_sha256: &verifier,
        created_at: now.as_str(),
    };
    let outcome = match auth::lock(&state.registry) {
        Ok(mut registry) => registry.create_client_token(&command, &token),
        Err(_) => return problem(ProblemCode::ServiceNotReady),
    };
    match outcome {
        Ok(Idempotent::Created(created)) => WireResponse::json(
            201,
            &json!({
                "schema_version": 1,
                "token_id": created.token_id,
                "client_label": created.client_label,
                "scopes": created.scopes,
                "secret": secret.expose(),
            }),
        ),
        Ok(Idempotent::Replayed(existing)) => summary_response(&existing),
        Err(StoreError::IdempotencyConflict) => problem(ProblemCode::IdempotencyConflict),
        Err(error) => unavailable("create a client token", &error),
    }
}

/// `GET /v1/auth/tokens`.
#[must_use]
pub fn list_client_tokens(
    state: &AppState,
    bearer: Option<&str>,
    query: Option<&str>,
) -> WireResponse {
    if let Err(denied) = guard(&state.plane, OperationId::ListClientTokens, bearer) {
        return denied;
    }
    let (limit, cursor) = match parse_list_query(query) {
        Ok(parsed) => parsed,
        Err(detail) => return WireResponse::invalid(detail),
    };
    let page = match auth::lock(&state.registry) {
        Ok(registry) => registry.list_client_tokens(cursor, limit),
        Err(_) => return problem(ProblemCode::ServiceNotReady),
    };
    let page = match page {
        Ok(page) => page,
        Err(error) => return unavailable("list client tokens", &error),
    };
    let Some(items) = page.items.iter().map(summary).collect::<Option<Vec<_>>>() else {
        return unavailable("list client tokens", &"unlisted token state");
    };
    let next_cursor = page
        .has_more
        .then(|| page.items.last().map(|token| token.token_id.clone()))
        .flatten();
    WireResponse::json(
        200,
        &json!({ "schema_version": 1, "items": items, "next_cursor": next_cursor }),
    )
}

/// Extracts `{token_id}` from a `/v1/auth/tokens/{token_id}/revoke` path.
#[must_use]
pub fn revoke_target(path: &str) -> &str {
    path.strip_prefix(REVOKE_PREFIX)
        .and_then(|rest| rest.strip_suffix(REVOKE_SUFFIX))
        .unwrap_or_default()
}

/// `POST /v1/auth/tokens/{token_id}/revoke`.
#[must_use]
pub fn revoke_client_token(
    state: &AppState,
    request: &TokenRequest<'_>,
    target_token_id: &str,
) -> WireResponse {
    let principal = match guard(&state.plane, OperationId::RevokeClientToken, request.bearer) {
        Ok(principal) => principal,
        Err(denied) => return denied,
    };
    let key = match idempotency_key(request.idempotency_key) {
        Ok(key) => key,
        Err(detail) => return WireResponse::invalid(detail),
    };
    match request.body {
        Some([]) => {}
        Some(_) => return WireResponse::invalid("this operation takes no request body"),
        None => return WireResponse::invalid(unreadable_body()),
    }
    if let Err(error) = validate_request(OperationId::RevokeClientToken, None) {
        return WireResponse::invalid(error.detail());
    }
    if !is_uuid_v7(target_token_id) {
        return WireResponse::invalid("path parameter token_id must be a lowercase UUIDv7");
    }
    let digest = request_digest(&json!({
        "operation_id": OperationId::RevokeClientToken.as_str(),
        "token_id": target_token_id,
    }));
    let now = UtcTimestamp::now();
    let command = TokenCommand {
        principal_token_id: principal.id(),
        idempotency_key: key,
        request_sha256: digest.as_str(),
        now: now.as_str(),
    };
    let outcome = match auth::lock(&state.registry) {
        Ok(mut registry) => registry.revoke_client_token(&command, target_token_id),
        Err(_) => return problem(ProblemCode::ServiceNotReady),
    };
    match outcome {
        Ok(Idempotent::Created(token) | Idempotent::Replayed(token)) => summary_response(&token),
        Err(StoreError::NotFound(_)) => problem(ProblemCode::ResourceNotFound),
        Err(StoreError::LastControlToken) => problem(ProblemCode::LastControlToken),
        Err(StoreError::IdempotencyConflict) => problem(ProblemCode::IdempotencyConflict),
        Err(error) => unavailable("revoke a client token", &error),
    }
}

/// The `Idempotency-Key` header, or the `detail` of the `400` it earns.
/// The detail never repeats the header value.
pub(crate) fn idempotency_key(value: Option<&str>) -> Result<&str, String> {
    let Some(key) = value else {
        return Err("the Idempotency-Key header is missing or is not visible ASCII".to_owned());
    };
    if (KEY_MIN_CHARS..=KEY_MAX_CHARS).contains(&key.chars().count()) {
        Ok(key)
    } else {
        Err(format!(
            "the Idempotency-Key header must be {KEY_MIN_CHARS} through {KEY_MAX_CHARS} characters"
        ))
    }
}

pub(crate) fn is_json(content_type: Option<&str>) -> bool {
    content_type
        .and_then(|value| value.split(';').next())
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("application/json"))
}

/// `detail` of a body the daemon could not read: the transport failed or it
/// is larger than [`MAX_REQUEST_BODY_BYTES`].
pub(crate) fn unreadable_body() -> String {
    format!("the request body could not be read; it must not exceed {MAX_REQUEST_BODY_BYTES} bytes")
}

/// The strict JSON body of a request: `Content-Type: application/json`, a
/// readable body, and no duplicate member.
///
/// # Errors
///
/// The `400` response that ends the request.
pub(crate) fn body_json(request: &TokenRequest<'_>) -> Result<JsonValue, WireResponse> {
    if !is_json(request.content_type) {
        return Err(WireResponse::invalid(
            "the Content-Type header must be application/json",
        ));
    }
    let Some(bytes) = request.body else {
        return Err(WireResponse::invalid(unreadable_body()));
    };
    parse_strict_bytes(bytes).map_err(|error| WireResponse::invalid(error.detail()))
}

/// Strictly decodes and validates a `CreateClientTokenRequest`, returning the
/// label and the scopes in contract order.
fn decode_create(request: &TokenRequest<'_>) -> Result<(String, Vec<ClientScope>), WireResponse> {
    let body = body_json(request)?;
    validate_request(OperationId::CreateClientToken, Some(&body)).map_err(
        |error| match &error.kind {
            ValidationErrorKind::InvalidVariant { field, .. } if field == "scopes[]" => {
                WireResponse::unsupported_scope(
                    "$.scopes[] holds a scope that is not one of control, ingest-only, \
                     mcp-read, mcp-write, proxy-client",
                )
            }
            _ => WireResponse::invalid(error.detail()),
        },
    )?;
    let unreadable = || WireResponse::invalid("$.client_label or $.scopes is not readable");
    let label = body
        .get("client_label")
        .and_then(JsonValue::as_str)
        .ok_or_else(unreadable)?
        .to_owned();
    let mut scopes = body
        .get("scopes")
        .and_then(JsonValue::as_array)
        .ok_or_else(unreadable)?
        .iter()
        .map(|scope| {
            scope
                .as_str()
                .and_then(|scope| scope.parse::<ClientScope>().ok())
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(unreadable)?;
    scopes.sort_unstable();
    Ok((label, scopes))
}

/// Digest of the canonical typed request: object keys sorted, scopes in
/// contract order, no transport fields.
pub(crate) fn request_digest(canonical: &serde_json::Value) -> Sha256Digest {
    Sha256Digest::of_bytes(canonical.to_string().as_bytes())
}

/// Parses `limit` and `cursor`; unknown, repeated, or malformed parameters
/// are rejected with the `detail` of the `400`.
fn parse_list_query(query: Option<&str>) -> Result<(u32, Option<&str>), String> {
    parse_list_query_for(query, None).map(|list| (list.limit, list.cursor))
}

/// A parsed list query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListQuery<'a> {
    /// Page size, defaulting to 100.
    pub limit: u32,
    /// `UUIDv7` of the last item of the previous page.
    pub cursor: Option<&'a str>,
    /// Value of the required `UUIDv7` subject parameter, when one was named.
    pub subject: Option<&'a str>,
}

/// Parses `limit`, `cursor`, and, when `subject` names one, a required
/// `UUIDv7` subject parameter such as `project_id`. Unknown, repeated, or
/// malformed parameters are rejected.
///
/// # Errors
///
/// The `detail` of the `400`: the parameter and the rule it breaks. It never
/// repeats a parameter value, and repeats a parameter name only when
/// [`echoable_name`] allows it.
pub(crate) fn parse_list_query_for<'a>(
    query: Option<&'a str>,
    subject: Option<&str>,
) -> Result<ListQuery<'a>, String> {
    let uuid = |name: &str| format!("query parameter {name} must be a lowercase UUIDv7");
    let twice = |name: &str| format!("query parameter {name} was given more than once");
    let mut limit = None;
    let mut cursor = None;
    let mut subject_value = None;
    for pair in query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        let Some((name, value)) = pair.split_once('=') else {
            return Err("a query parameter has no value; write name=value".to_owned());
        };
        if name == "limit" {
            if limit.is_some() {
                return Err(twice("limit"));
            }
            let parsed = (!value.is_empty()
                && value.len() <= 3
                && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| value.parse::<u32>().ok())
            .flatten()
            .filter(|limit| (1..=MAX_LIMIT).contains(limit));
            limit = Some(parsed.ok_or_else(|| {
                format!("query parameter limit must be an integer from 1 through {MAX_LIMIT}")
            })?);
        } else if name == "cursor" {
            if cursor.is_some() {
                return Err(twice("cursor"));
            }
            cursor = Some(
                Some(value)
                    .filter(|value| is_uuid_v7(value))
                    .ok_or_else(|| uuid("cursor"))?,
            );
        } else if Some(name) == subject {
            if subject_value.is_some() {
                return Err(twice(name));
            }
            subject_value = Some(
                Some(value)
                    .filter(|value| is_uuid_v7(value))
                    .ok_or_else(|| uuid(name))?,
            );
        } else {
            return Err(match echoable_name(name) {
                Some(name) => format!("unknown query parameter {name}"),
                None => "unknown query parameter".to_owned(),
            });
        }
    }
    if let (Some(name), None) = (subject, subject_value) {
        return Err(format!("query parameter {name} is required"));
    }
    Ok(ListQuery {
        limit: limit.unwrap_or(DEFAULT_LIMIT),
        cursor,
        subject: subject_value,
    })
}

/// Matches the contract's lower-case `UUIDv7` pattern.
pub(crate) fn is_uuid_v7(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => *byte == b'7',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        })
}

fn summary(token: &StoredToken) -> Option<serde_json::Value> {
    let status = match token.state {
        TokenState::Active => "active",
        TokenState::Revoked => "revoked",
        TokenState::Pending | TokenState::Discarded => return None,
    };
    Some(json!({
        "schema_version": 1,
        "token_id": token.token_id,
        "client_label": token.client_label,
        "scopes": token.scopes,
        "status": status,
        "created_at": token.created_at,
        "revoked_at": token.revoked_at,
    }))
}

fn summary_response(token: &StoredToken) -> WireResponse {
    match summary(token) {
        Some(body) => WireResponse::json(200, &body),
        None => unavailable("summarize a client token", &"unlisted token state"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_query_parsing() {
        let id = "018f47a0-0000-7000-8000-000000000001";
        assert_eq!(parse_list_query(None), Ok((100, None)));
        assert_eq!(parse_list_query(Some("")), Ok((100, None)));
        assert_eq!(parse_list_query(Some("limit=1")), Ok((1, None)));
        assert_eq!(parse_list_query(Some("limit=500")), Ok((500, None)));
        assert_eq!(
            parse_list_query(Some(&format!("cursor={id}&limit=2"))),
            Ok((2, Some(id)))
        );
        for (bad, detail) in [
            (
                "limit=0",
                "query parameter limit must be an integer from 1 through 500",
            ),
            (
                "limit=501",
                "query parameter limit must be an integer from 1 through 500",
            ),
            (
                "limit=-1",
                "query parameter limit must be an integer from 1 through 500",
            ),
            (
                "limit=+1",
                "query parameter limit must be an integer from 1 through 500",
            ),
            (
                "limit=",
                "query parameter limit must be an integer from 1 through 500",
            ),
            (
                "limit=1&limit=2",
                "query parameter limit was given more than once",
            ),
            (
                "cursor=",
                "query parameter cursor must be a lowercase UUIDv7",
            ),
            (
                "cursor=abc",
                "query parameter cursor must be a lowercase UUIDv7",
            ),
            ("other=1", "unknown query parameter other"),
            ("limit", "a query parameter has no value; write name=value"),
        ] {
            assert_eq!(parse_list_query(Some(bad)), Err(detail.to_owned()), "{bad}");
        }
    }

    #[test]
    fn a_query_parameter_name_is_repeated_only_when_it_is_plain() {
        let secret = "k".repeat(43);
        let detail = parse_list_query(Some(&format!("{secret}=1"))).unwrap_err();
        assert_eq!(detail, "unknown query parameter");
        assert!(!detail.contains(&secret));
        let detail = parse_list_query(Some("limit=abc")).unwrap_err();
        assert!(!detail.contains("abc"), "{detail}");
    }

    #[test]
    fn subject_list_query_parsing() {
        let id = "018f47a0-0000-7000-8000-000000000001";
        let cursor = "018f47a0-0000-7000-8000-000000000002";
        assert_eq!(
            parse_list_query_for(Some(&format!("project_id={id}")), Some("project_id")),
            Ok(ListQuery {
                limit: 100,
                cursor: None,
                subject: Some(id)
            })
        );
        assert_eq!(
            parse_list_query_for(
                Some(&format!("limit=2&cursor={cursor}&workspace_id={id}")),
                Some("workspace_id")
            ),
            Ok(ListQuery {
                limit: 2,
                cursor: Some(cursor),
                subject: Some(id)
            })
        );
        for (bad, detail) in [
            (String::new(), "query parameter project_id is required"),
            (
                "limit=1".to_owned(),
                "query parameter project_id is required",
            ),
            (
                "project_id=".to_owned(),
                "query parameter project_id must be a lowercase UUIDv7",
            ),
            (
                "project_id=abc".to_owned(),
                "query parameter project_id must be a lowercase UUIDv7",
            ),
            (
                format!("project_id={id}&project_id={id}"),
                "query parameter project_id was given more than once",
            ),
            (
                format!("project_id={id}&workspace_id={id}"),
                "unknown query parameter workspace_id",
            ),
            (
                format!("project_id={id}&limit=0"),
                "query parameter limit must be an integer from 1 through 500",
            ),
            (
                format!("project_id={id}&cursor=x"),
                "query parameter cursor must be a lowercase UUIDv7",
            ),
        ] {
            assert_eq!(
                parse_list_query_for(Some(&bad), Some("project_id")),
                Err(detail.to_owned()),
                "{bad}"
            );
        }
        assert_eq!(
            parse_list_query_for(Some(&format!("project_id={id}")), None),
            Err("unknown query parameter project_id".to_owned())
        );
    }

    #[test]
    fn uuid_v7_pattern() {
        assert!(is_uuid_v7("018f47a0-0000-7000-8000-000000000001"));
        assert!(!is_uuid_v7("018f47a0-0000-4000-8000-000000000001"));
        assert!(!is_uuid_v7("018F47A0-0000-7000-8000-000000000001"));
        assert!(!is_uuid_v7("018f47a0-0000-7000-c000-000000000001"));
        assert!(!is_uuid_v7(""));
    }

    #[test]
    fn revoke_target_extraction() {
        assert_eq!(revoke_target("/v1/auth/tokens/abc/revoke"), "abc");
        assert_eq!(revoke_target("/v1/auth/tokens/abc"), "");
    }

    #[test]
    fn idempotency_key_bounds() {
        assert!(idempotency_key(None).unwrap_err().contains("missing"));
        for length in [15, 201] {
            let detail = idempotency_key(Some(&"k".repeat(length))).unwrap_err();
            assert_eq!(
                detail,
                "the Idempotency-Key header must be 16 through 200 characters"
            );
        }
        assert!(idempotency_key(Some(&"k".repeat(16))).is_ok());
        assert!(idempotency_key(Some(&"k".repeat(200))).is_ok());
    }

    #[test]
    fn content_type_essence() {
        assert!(is_json(Some("application/json")));
        assert!(is_json(Some("Application/JSON; charset=utf-8")));
        assert!(!is_json(Some("text/plain")));
        assert!(!is_json(None));
    }
}
