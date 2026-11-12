//! P3-A: the `detail` rule of the Problems `memproved` emits, and the
//! diagnostics a request failure turns into.

use memprove_protocol::{
    phase1::{
        OperationId, PROBLEM_DETAIL_MAX_CHARS, ValidationErrorKind, validate_problem,
        validate_problem_detail, validate_request,
    },
    strict_json::{JsonValue, echoable_name, parse_strict},
};

fn problem(status: u16, code: &str, detail: Option<&str>) -> JsonValue {
    let detail = detail.map_or_else(String::new, |detail| {
        format!(r#","detail":{}"#, serde_quote(detail))
    });
    parse_strict(&format!(
        r#"{{"type":"https://continuity.local/problems/x","title":"T","status":{status},"code":"{code}","request_id":"018f0f00-0000-7000-8000-000000000006","retryable":false{detail}}}"#
    ))
    .unwrap()
}

/// JSON string literal for ASCII test text.
fn serde_quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[test]
fn a_400_needs_a_detail_of_one_through_512_characters() {
    let ok =
        |detail: &str| validate_problem_detail(400, &problem(400, "invalid_request", Some(detail)));
    assert!(ok("$.root must be an absolute path").is_ok());
    assert!(ok("x").is_ok());
    assert!(ok(&"x".repeat(PROBLEM_DETAIL_MAX_CHARS)).is_ok());
    assert!(ok("").is_err());
    assert!(ok(&"x".repeat(PROBLEM_DETAIL_MAX_CHARS + 1)).is_err());
    let missing = validate_problem_detail(400, &problem(400, "invalid_request", None));
    assert!(matches!(
        missing.unwrap_err().kind,
        ValidationErrorKind::MissingField(name) if name == "detail"
    ));
    let unsupported = problem(400, "unsupported_scope", None);
    assert!(validate_problem_detail(400, &unsupported).is_err());
}

#[test]
fn a_401_and_a_403_carry_no_detail() {
    for (status, code) in [(401, "authentication_required"), (403, "policy_denied")] {
        assert!(validate_problem_detail(status, &problem(status, code, None)).is_ok());
        assert!(validate_problem_detail(status, &problem(status, code, Some("why"))).is_err());
    }
}

/// U9 R1 (Owner O4; ADR 0020): `403 role_forbidden` is the one `403` with
/// a `detail`, which it needs, as a `400` does; it is a legal pair.
#[test]
fn a_403_role_forbidden_needs_a_detail() {
    let forbidden = |detail: Option<&str>| problem(403, "role_forbidden", detail);
    let ok = |detail: &str| validate_problem_detail(403, &forbidden(Some(detail)));
    assert!(ok("x").is_ok());
    assert!(ok(&"x".repeat(PROBLEM_DETAIL_MAX_CHARS)).is_ok());
    assert!(ok("").is_err());
    assert!(ok(&"x".repeat(PROBLEM_DETAIL_MAX_CHARS + 1)).is_err());
    let missing = validate_problem_detail(403, &forbidden(None));
    assert!(matches!(
        missing.unwrap_err().kind,
        ValidationErrorKind::MissingField(name) if name == "detail"
    ));
    assert!(validate_problem(403, &forbidden(Some("x"))).is_ok());
    assert!(validate_problem(409, &problem(409, "role_forbidden", Some("x"))).is_err());
}

#[test]
fn other_statuses_are_not_constrained() {
    for (status, code) in [(404, "resource_not_found"), (409, "revision_conflict")] {
        assert!(validate_problem_detail(status, &problem(status, code, None)).is_ok());
        assert!(validate_problem_detail(status, &problem(status, code, Some("why"))).is_ok());
    }
}

/// The frozen Phase 0 fixtures hold `400` Problems without a `detail`; the
/// general validator keeps accepting them.
#[test]
fn the_general_validator_still_accepts_a_400_without_a_detail() {
    validate_problem(400, &problem(400, "invalid_request", None)).unwrap();
    validate_problem(400, &problem(400, "invalid_request", Some("why"))).unwrap();
}

#[test]
fn only_plain_names_are_repeated() {
    for plain in ["extra", "schema_version", "a1", &"a".repeat(32)] {
        assert_eq!(echoable_name(plain), Some(plain));
    }
    let token_shaped = "Kq3-xZ_".repeat(7);
    for hidden in [
        "",
        "Extra",
        "with space",
        "dash-ed",
        "dot.ted",
        "ünï",
        &"a".repeat(33),
        token_shaped.as_str(),
    ] {
        assert_eq!(echoable_name(hidden), None, "{hidden:?}");
    }
}

fn request_detail(operation: OperationId, body: &str) -> String {
    let body = parse_strict(body).unwrap();
    validate_request(operation, Some(&body))
        .expect_err("the request is invalid")
        .detail()
}

#[test]
fn a_request_failure_names_the_location_and_the_rule_but_not_the_value() {
    let probe = "PROBE-4f1c";
    let create = OperationId::CreateClientToken;
    let missing = request_detail(create, r#"{"schema_version":1,"scopes":["mcp-read"]}"#);
    assert_eq!(missing, "$.client_label is required");
    let wrong_type = request_detail(
        create,
        &format!(r#"{{"schema_version":1,"client_label":"x","scopes":"{probe}"}}"#),
    );
    assert_eq!(wrong_type, "$.scopes must be array, not string");
    let variant = request_detail(
        create,
        &format!(r#"{{"schema_version":1,"client_label":"x","scopes":["{probe}"]}}"#),
    );
    assert_eq!(variant, "$.scopes[] is not one of the allowed values");
    let unknown = request_detail(
        create,
        r#"{"schema_version":1,"client_label":"x","scopes":["mcp-read"],"extra":1}"#,
    );
    assert_eq!(unknown, "the request body has an unknown member extra");
    let shaped = request_detail(
        create,
        &format!(
            r#"{{"schema_version":1,"client_label":"x","scopes":["mcp-read"],"{}":1}}"#,
            "Zz9-".repeat(11)
        ),
    );
    assert_eq!(shaped, "the request body has an unknown member");
    let range = request_detail(
        create,
        r#"{"schema_version":1,"client_label":"","scopes":["mcp-read"]}"#,
    );
    assert_eq!(range, "$.client_label: expected 1 through 128 characters");
    for detail in [&missing, &wrong_type, &variant, &unknown, &shaped, &range] {
        assert!(!detail.contains(probe), "{detail}");
    }
}

#[test]
fn a_body_that_is_absent_or_forbidden_is_named() {
    let absent = validate_request(OperationId::CreateClientToken, None).unwrap_err();
    assert_eq!(absent.detail(), "the request body is required");
    let body = parse_strict("{}").unwrap();
    let forbidden = validate_request(OperationId::GetStatus, Some(&body)).unwrap_err();
    assert_eq!(forbidden.detail(), "this operation takes no request body");
}

#[test]
fn a_parse_failure_gives_the_rule_and_the_offset_but_not_the_text() {
    let probe = "PROBE-4f1c";
    for (input, rule) in [
        (
            format!(r#"{{"a":"{probe}"#),
            "the input ends inside a value",
        ),
        (
            format!(r#"{{"a":1}} {probe}"#),
            "text follows the first JSON value",
        ),
        (
            format!(r#"{{"a":1,"a":2,"{probe}":3}}"#),
            "the member a appears more than once",
        ),
        (
            format!(r#"{{"{probe}":1,"{probe}":2}}"#),
            "a member appears more than once",
        ),
        (format!(r#"{{"a":{probe}}}"#), "expected"),
    ] {
        let error = parse_strict(&input).unwrap_err();
        let detail = error.detail();
        assert!(
            detail.starts_with("the request body is not valid JSON: "),
            "{detail}"
        );
        assert!(detail.contains(rule), "{detail}");
        assert!(
            detail.contains(&format!("(byte {})", error.offset)),
            "{detail}"
        );
        assert!(!detail.contains(probe), "{detail}");
    }
}
