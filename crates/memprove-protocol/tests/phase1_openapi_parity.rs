use std::{collections::BTreeMap, path::PathBuf, process::Command};

use memprove_protocol::{
    phase1::{OperationId, validate_problem, validate_request, validate_response},
    strict_json::{JsonValue, parse_strict},
};

const CASES: &str = include_str!("../../../tests/phase1/fixtures/openapi-wire-parity-v1.json");

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

fn integer_field(value: &JsonValue, name: &str) -> u16 {
    field(value, name)
        .as_i64()
        .and_then(|number| u16::try_from(number).ok())
        .unwrap_or_else(|| panic!("fixture field {name} must be a u16 integer"))
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root must resolve")
}

fn oracle_results(root: &std::path::Path) -> BTreeMap<String, bool> {
    let output = Command::new("ruby")
        .arg(root.join("tests/phase1/openapi_schema_oracle.rb"))
        .arg(root.join("tests/phase1/fixtures/openapi-wire-parity-v1.json"))
        .current_dir(root)
        .output()
        .expect("canonical OpenAPI oracle must execute");
    assert!(
        output.status.success(),
        "canonical OpenAPI oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let document = parse_strict(
        std::str::from_utf8(&output.stdout).expect("oracle output must be UTF-8 JSON"),
    )
    .expect("oracle output must be strict JSON");
    let validator = field(&document, "validator");
    assert_eq!(string_field(validator, "id"), "sourcemeta-jsonschema");
    assert_eq!(string_field(validator, "version"), "16.1.0");
    assert_eq!(field(validator, "format_assertion").as_bool(), Some(true));
    assert_eq!(field(validator, "network_enabled").as_bool(), Some(false));

    field(&document, "cases")
        .as_array()
        .expect("oracle cases must be an array")
        .iter()
        .map(|record| {
            (
                string_field(record, "case_id").to_owned(),
                field(record, "valid")
                    .as_bool()
                    .expect("oracle validity must be boolean"),
            )
        })
        .collect()
}

#[test]
#[ignore = "requires hermetic Sourcemeta binding; run scripts/phase1/verify_slice1"]
fn rust_wire_validation_matches_the_canonical_openapi_oracle() {
    let root = repository_root();
    let oracle = oracle_results(&root);
    let fixture = parse_strict(CASES).expect("parity fixture must be strict JSON");
    let cases = field(&fixture, "cases")
        .as_array()
        .expect("parity cases must be an array");
    assert_eq!(oracle.len(), cases.len());

    let mut mismatches = Vec::new();
    for record in cases {
        let case_id = string_field(record, "case_id");
        let operation_id = string_field(record, "operation_id")
            .parse::<OperationId>()
            .expect("fixture operation_id must be a Phase 1 operation");
        let body = parse_strict(string_field(record, "payload_raw"))
            .unwrap_or_else(|error| panic!("{case_id}: invalid fixture payload: {error:?}"));
        let rust_valid = match string_field(record, "direction") {
            "request" => validate_request(operation_id, Some(&body)).is_ok(),
            "response" => validate_response(
                operation_id,
                integer_field(record, "status"),
                string_field(record, "media_type"),
                &body,
            )
            .is_ok(),
            "problem" => validate_problem(integer_field(record, "status"), &body).is_ok(),
            direction => panic!("{case_id}: unsupported parity direction {direction}"),
        };
        let oracle_valid = *oracle
            .get(case_id)
            .unwrap_or_else(|| panic!("oracle omitted {case_id}"));
        if rust_valid != oracle_valid {
            mismatches.push(format!(
                "{case_id}: Rust={rust_valid}, OpenAPI_oracle={oracle_valid}"
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "Rust/OpenAPI parity mismatches:\n{}",
        mismatches.join("\n")
    );
}
