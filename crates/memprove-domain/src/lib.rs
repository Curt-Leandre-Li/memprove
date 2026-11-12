//! Pure memprove domain types and state machines will live in this crate.
//!
//! This crate must remain independent of databases, networks, `Git`, clocks,
//! environment variables, and filesystems.

#![forbid(unsafe_code)]

mod actions;
mod canonical;
mod capsule;
mod identity;
mod ontology;
mod pack;
mod reaffirm;
mod records;
mod recovery;
mod recovery_preview;
mod resumed;
mod role;
mod token_shape;

pub use actions::*;
pub use canonical::{CanonicalJson, MAX_SAFE_INTEGER};
pub use capsule::*;
pub use identity::{
    AgentId, CheckpointId, CorrelationId, DomainError, EventId, EvidenceId, IdempotencyKey,
    OperationId, PrincipalId, ProjectId, RepositorySnapshotId, SessionId, Sha256Digest, TaskId,
    UtcTimestamp, WorkspaceId,
};
pub use ontology::*;
pub use pack::*;
pub use reaffirm::*;
pub use records::{
    AGENT_NAME_RULE, AgentRefusal, AgentRole, DAEMON_EVENT_TYPES, DAEMON_PRODUCER_NAME,
    DAEMON_RESERVED_EVENT_TYPES, EVENT_DIGEST_VERSION, EVENT_ENVELOPE_VERSION, Event,
    EventChainVerifier, EventInput, EventProducer, IdempotencyContext, IdempotencyRecord,
    IdempotencyState, LedgerFailure, LedgerRecordType, LedgerVerification, MigrationRecord,
    PrivacyClass, Project, ProjectProvisioningState, SESSION_CRASHED_ENDED_BY_SCHEMA,
    SESSION_STARTED_AGENT_SCHEMA, Session, SessionAgent, SessionEndedBy, SessionOutcome, Task,
    TaskSessionLink, TaskStatus, TaskTransitionError, Workspace, canonical_request_sha256,
    is_daemon_event_type, is_reserved_producer_name, verify_event_chain,
};
pub use recovery::*;
pub use recovery_preview::*;
pub use resumed::{ResumedFrom, ResumedFromRefusal};
pub use role::{RoleRule, RoleWrite, is_sub_agent, role_refusal};
pub use token_shape::{TOKEN_SECRET_CHARS, holds_token_shape, is_token_shaped, token_candidates};

/// Human-readable boundary description used by workspace smoke tests.
pub const CRATE_PURPOSE: &str = "pure domain objects, invariants, and state machines";

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp(value: &str) -> UtcTimestamp {
        UtcTimestamp::parse(value).expect("valid fixture timestamp")
    }

    #[test]
    fn generated_ids_are_uuid_v7_and_non_v7_values_are_rejected() {
        let id = ProjectId::new();

        assert_eq!(id.as_uuid().get_version_num(), 7);
        assert_eq!(ProjectId::parse(id.as_str()).expect("round trip"), id);
        assert!(ProjectId::parse("550e8400-e29b-41d4-a716-446655440000").is_err());
    }

    #[test]
    fn uuid_v7_ids_reject_noncanonical_spellings_at_every_serde_shape() {
        let canonical = ProjectId::new().to_string();
        let invalid_spellings = [
            canonical.to_uppercase(),
            canonical.replace('-', ""),
            format!("{{{canonical}}}"),
            format!("urn:uuid:{canonical}"),
            format!(" {canonical}"),
            format!("{canonical} "),
            "550e8400-e29b-41d4-a716-446655440000".to_owned(),
            "not-a-uuid".to_owned(),
        ];

        macro_rules! assert_canonical_uuid_boundary {
            ($type:ty) => {{
                #[derive(serde::Deserialize)]
                struct NestedUuid {
                    id: $type,
                }

                let parsed = <$type>::parse(&canonical).expect("canonical UUIDv7");
                assert_eq!(parsed.as_str(), canonical);
                let encoded = serde_json::to_string(&parsed).expect("serialize canonical ID");
                assert_eq!(encoded, format!(r#""{canonical}""#));
                let nested: NestedUuid = serde_json::from_value(serde_json::json!({
                    "id": canonical,
                }))
                .expect("nested canonical ID");
                assert_eq!(nested.id.as_str(), canonical);
                assert_eq!(
                    serde_json::to_string(
                        &serde_json::from_str::<$type>(&encoded).expect("deserialize canonical ID")
                    )
                    .expect("reserialize canonical ID"),
                    encoded,
                );

                for invalid in &invalid_spellings {
                    let encoded_invalid =
                        serde_json::to_string(invalid).expect("encode invalid spelling");
                    assert!(
                        <$type>::parse(invalid).is_err(),
                        "{} direct parser accepted {invalid:?}",
                        stringify!($type),
                    );
                    assert!(
                        serde_json::from_str::<$type>(&encoded_invalid).is_err(),
                        "{} direct Deserialize accepted {invalid:?}",
                        stringify!($type),
                    );
                    assert!(
                        serde_json::from_str::<Option<$type>>(&encoded_invalid).is_err(),
                        "{} Option accepted {invalid:?}",
                        stringify!($type),
                    );
                    assert!(
                        serde_json::from_value::<Vec<$type>>(serde_json::json!([invalid])).is_err(),
                        "{} Vec accepted {invalid:?}",
                        stringify!($type),
                    );
                    assert!(
                        serde_json::from_value::<std::collections::BTreeMap<String, $type>>(
                            serde_json::json!({"id": invalid})
                        )
                        .is_err(),
                        "{} Map accepted {invalid:?}",
                        stringify!($type),
                    );
                    assert!(
                        serde_json::from_value::<NestedUuid>(serde_json::json!({"id": invalid}))
                            .is_err(),
                        "{} nested DTO accepted {invalid:?}",
                        stringify!($type),
                    );
                }
            }};
        }

        assert_canonical_uuid_boundary!(ProjectId);
        assert_canonical_uuid_boundary!(WorkspaceId);
        assert_canonical_uuid_boundary!(TaskId);
        assert_canonical_uuid_boundary!(SessionId);
        assert_canonical_uuid_boundary!(EventId);
        assert_canonical_uuid_boundary!(AgentId);
    }

    #[test]
    fn uuid_v7_ids_reject_non_rfc_4122_variants() {
        // Version nibble 7 with each non-RFC 4122 variant: NCS (0xxx),
        // Microsoft (110x), and future (111x).
        let non_rfc = [
            "018f0f5e-7b6e-7cc0-0123-456789abcdef",
            "018f0f5e-7b6e-7cc0-c123-456789abcdef",
            "018f0f5e-7b6e-7cc0-e123-456789abcdef",
        ];
        for value in non_rfc {
            let parsed = uuid::Uuid::parse_str(value).expect("UUID fixture");
            assert_eq!(parsed.get_version_num(), 7);
            assert_ne!(parsed.get_variant(), uuid::Variant::RFC4122);
        }

        macro_rules! assert_rfc_variant_boundary {
            ($type:ty) => {{
                #[derive(serde::Deserialize)]
                #[allow(dead_code)] // Rejection occurs before the field can be observed.
                struct NestedUuid {
                    id: $type,
                }

                for value in non_rfc {
                    assert!(
                        <$type>::parse(value).is_err(),
                        "{} accepted non-RFC variant {value}",
                        stringify!($type),
                    );
                    assert!(
                        serde_json::from_value::<NestedUuid>(serde_json::json!({ "id": value }))
                            .is_err(),
                        "{} nested serde accepted non-RFC variant {value}",
                        stringify!($type),
                    );
                }
                for variant in ['8', '9', 'a', 'b'] {
                    let value = format!("018f0f5e-7b6e-7cc0-{variant}123-456789abcdef");
                    assert!(<$type>::parse(&value).is_ok(), "{value}");
                }
            }};
        }

        assert_rfc_variant_boundary!(ProjectId);
        assert_rfc_variant_boundary!(WorkspaceId);
        assert_rfc_variant_boundary!(TaskId);
        assert_rfc_variant_boundary!(SessionId);
        assert_rfc_variant_boundary!(EventId);
        assert_rfc_variant_boundary!(AgentId);
        assert_rfc_variant_boundary!(EvidenceId);
        assert_rfc_variant_boundary!(RepositorySnapshotId);
        assert_rfc_variant_boundary!(CheckpointId);
    }

    #[test]
    fn validated_newtype_deserialization_matches_constructor_boundaries() {
        macro_rules! assert_boundary {
            ($type:ty, $valid:expr, [$($invalid:expr),+ $(,)?]) => {{
                let valid = $valid;
                let encoded = serde_json::to_string(valid).expect("encode fixture string");
                assert_eq!(
                    serde_json::from_str::<$type>(&encoded).is_ok(),
                    <$type>::parse(valid).is_ok(),
                    "valid boundary mismatch for {}",
                    stringify!($type),
                );

                $(
                    let invalid = $invalid;
                    let encoded =
                        serde_json::to_string(invalid).expect("encode invalid fixture string");
                    assert_eq!(
                        serde_json::from_str::<$type>(&encoded).is_ok(),
                        <$type>::parse(invalid).is_ok(),
                        "invalid boundary mismatch for {} and {invalid:?}",
                        stringify!($type),
                    );
                )+
            }};
        }

        let uuid_v7 = ProjectId::new().to_string();
        let uuid_v4 = "550e8400-e29b-41d4-a716-446655440000";
        assert_boundary!(ProjectId, &uuid_v7, [uuid_v4, "not-a-uuid"]);
        assert_boundary!(WorkspaceId, &uuid_v7, [uuid_v4, "not-a-uuid"]);
        assert_boundary!(TaskId, &uuid_v7, [uuid_v4, "not-a-uuid"]);
        assert_boundary!(SessionId, &uuid_v7, [uuid_v4, "not-a-uuid"]);
        assert_boundary!(EventId, &uuid_v7, [uuid_v4, "not-a-uuid"]);
        assert_boundary!(AgentId, &uuid_v7, [uuid_v4, "not-a-uuid"]);

        let oversized = "x".repeat(256);
        assert_boundary!(PrincipalId, "principal-a", ["", "line\nbreak", &oversized]);
        assert_boundary!(OperationId, "createTask", ["", "line\nbreak", &oversized]);
        assert_boundary!(IdempotencyKey, "request-1", ["", "line\nbreak", &oversized]);
        assert_boundary!(
            UtcTimestamp,
            "2026-07-31T01:02:03.456Z",
            [
                "2026-07-31T09:02:03.456+08:00",
                "2026-07-31T01:02:03.000Z",
                "not-a-timestamp",
            ]
        );
        assert_boundary!(
            Sha256Digest,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            [
                "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789",
                "abc",
            ]
        );
    }

    #[test]
    fn utc_timestamp_accepts_only_canonical_utc_rfc3339() {
        assert_eq!(
            timestamp("2026-07-31T01:02:03.456Z").as_str(),
            "2026-07-31T01:02:03.456Z"
        );
        assert!(UtcTimestamp::parse("2026-07-31T09:02:03.456+08:00").is_err());
        assert!(UtcTimestamp::parse("2026-07-31 01:02:03Z").is_err());
        assert!(UtcTimestamp::parse("2026-07-31T01:02:03.000Z").is_err());
    }

    #[test]
    fn canonical_json_is_member_order_independent_and_rejects_duplicates() {
        let left =
            CanonicalJson::parse(r#"{"z":1,"nested":{"b":2,"a":1},"a":3}"#).expect("valid JSON");
        let right =
            CanonicalJson::parse(r#"{"a":3,"nested":{"a":1,"b":2},"z":1}"#).expect("valid JSON");

        assert_eq!(left.as_str(), r#"{"a":3,"nested":{"a":1,"b":2},"z":1}"#);
        assert_eq!(left, right);
        assert_eq!(left.sha256(), right.sha256());
        let ordered_array = CanonicalJson::parse(r"[1,2,3]").expect("ordered array");
        let permuted_array = CanonicalJson::parse(r"[3,2,1]").expect("permuted array");
        assert_ne!(
            ordered_array.as_str().as_bytes(),
            permuted_array.as_str().as_bytes()
        );
        assert!(CanonicalJson::parse(r#"{"a":1,"a":2}"#).is_err());
        assert!(CanonicalJson::parse(r#"{"outer":{"a":1,"a":2}}"#).is_err());
    }

    #[test]
    fn canonical_json_matches_rfc8785_number_and_string_serialization() {
        let input = r#"{
            "numbers":[333333333.33333329,1E30,4.50,2e-3,0.000000000000000000000000001],
            "string":"\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/",
            "literals":[null,true,false]
        }"#;
        let expected = r#"{"literals":[null,true,false],"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],"string":"€$\u000f\nA'B\"\\\\\"/"}"#;

        assert_eq!(
            CanonicalJson::parse(input).expect("RFC example").as_str(),
            expected
        );
    }

    #[test]
    fn canonical_json_normalizes_equivalent_numbers_and_exponent_thresholds() {
        let integer = CanonicalJson::parse("1").expect("integer");
        let decimal = CanonicalJson::parse("1.0").expect("decimal");
        assert_eq!(integer, decimal);
        assert_eq!(
            CanonicalJson::parse("-0.0")
                .expect("negative zero")
                .as_str(),
            "0"
        );
        assert_eq!(
            CanonicalJson::parse("[1e-7,1e-6,1e20,1e21]")
                .expect("exponent thresholds")
                .as_str(),
            "[1e-7,0.000001,100000000000000000000,1e+21]"
        );
        assert_eq!(
            CanonicalJson::parse("1.7976931348623157e308")
                .expect("largest finite double")
                .as_str(),
            "1.7976931348623157e+308"
        );
    }

    #[test]
    fn canonical_json_accepts_finite_binary64_rounding_inputs() {
        let exact_double_boundary =
            CanonicalJson::parse("9007199254740992.0").expect("exact 2^53 decimal");
        let exact_integer_boundary =
            CanonicalJson::parse("9007199254740992").expect("exact 2^53 integer");
        assert_eq!(exact_double_boundary.as_str(), "9007199254740992");
        assert_eq!(exact_integer_boundary, exact_double_boundary);

        let rounded_integer =
            CanonicalJson::parse("9007199254740993").expect("binary64 rounding input");
        let rounded_decimal =
            CanonicalJson::parse("9007199254740993.0").expect("decimal binary64 rounding input");
        assert_eq!(rounded_integer.as_str(), "9007199254740992");
        assert_eq!(rounded_integer, exact_double_boundary);
        assert_eq!(rounded_integer, rounded_decimal);
        assert_eq!(rounded_integer.sha256(), exact_double_boundary.sha256());

        let one = CanonicalJson::parse("1").expect("integer one");
        assert_eq!(one, CanonicalJson::parse("1.0").expect("decimal one"));
        assert_eq!(one, CanonicalJson::parse("1e0").expect("exponent one"));
        assert_eq!(one, CanonicalJson::parse("1.00e+0").expect("expanded one"));
        assert_eq!(
            CanonicalJson::parse("-0")
                .expect("negative integer zero")
                .as_str(),
            "0"
        );
        assert_eq!(
            CanonicalJson::parse("-0.0")
                .expect("negative decimal zero")
                .as_str(),
            "0"
        );
        assert_eq!(
            CanonicalJson::parse("0.1")
                .expect("ordinary decimal")
                .as_str(),
            "0.1"
        );
        assert_eq!(
            CanonicalJson::parse("0.10000000000000001").expect("rounded decimal"),
            CanonicalJson::parse("0.1").expect("short decimal")
        );

        assert_eq!(
            CanonicalJson::parse("1.7976931348623157e308")
                .expect("maximum finite double")
                .as_str(),
            "1.7976931348623157e+308"
        );
        assert_eq!(
            CanonicalJson::parse("5e-324")
                .expect("minimum positive binary64")
                .as_str(),
            "5e-324"
        );

        let nested = CanonicalJson::parse(
            r#"{"z":[9007199254740993.0,1e0,0.10000000000000001],"a":{"n":-0.0}}"#,
        )
        .expect("nested binary64 numbers");
        let reordered = CanonicalJson::parse(r#"{"a":{"n":0},"z":[9007199254740992,1.00e+0,0.1]}"#)
            .expect("member-order perturbation");
        assert_eq!(nested, reordered);
        assert_eq!(nested.sha256(), reordered.sha256());
        for _ in 0..8 {
            assert_eq!(
                CanonicalJson::parse(nested.as_str()).expect("repeated canonicalization"),
                nested
            );
        }
    }

    #[test]
    fn canonical_json_sorts_object_members_by_utf16_code_units() {
        let input = r#"{
            "\u20ac":"Euro Sign","\r":"Carriage Return","\ufb33":"Hebrew Letter",
            "1":"One","\ud83d\ude00":"Emoji","\u0080":"Control","\u00f6":"Latin"
        }"#;
        let expected = "{\"\\r\":\"Carriage Return\",\"1\":\"One\",\"\u{80}\":\"Control\",\"ö\":\"Latin\",\"€\":\"Euro Sign\",\"😀\":\"Emoji\",\"\u{fb33}\":\"Hebrew Letter\"}";

        assert_eq!(
            CanonicalJson::parse(input)
                .expect("sorting example")
                .as_str(),
            expected
        );
    }

    #[test]
    fn canonical_json_rejects_non_finite_and_invalid_number_inputs() {
        for input in ["1e400", "NaN", "Infinity", "-Infinity"] {
            assert!(CanonicalJson::parse(input).is_err(), "accepted {input:?}");
        }
        for input in ["+1", "01", "1.", ".1", "1e", "--1"] {
            assert!(CanonicalJson::parse(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn canonical_json_nested_perturbations_repeat_identically() {
        let left =
            CanonicalJson::parse(r#" { "z":[{"b":1.0,"a":"line\nquote\"slash\\"}], "a":-0.0 } "#)
                .expect("left");
        let right = CanonicalJson::parse(r#"{"a":0,"z":[{"a":"line\nquote\"slash\\","b":1}]}"#)
            .expect("right");

        assert_eq!(left, right);
        assert_eq!(left.sha256(), right.sha256());
        for _ in 0..8 {
            assert_eq!(
                CanonicalJson::parse(left.as_str()).expect("repeat canonicalization"),
                left
            );
        }
    }

    #[test]
    fn sha256_digest_is_lowercase_hex_and_rejects_noncanonical_text() {
        let digest = Sha256Digest::of_bytes(b"memprove");

        assert_eq!(digest.as_str().len(), 64);
        assert!(digest.as_str().bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            Sha256Digest::parse(digest.as_str()).expect("round trip"),
            digest
        );
        assert!(Sha256Digest::parse(&digest.as_str().to_uppercase()).is_err());
        assert!(Sha256Digest::parse("abc").is_err());
    }

    fn event_input(session_id: &SessionId, event_type: &str, payload: &str) -> EventInput {
        EventInput {
            event_id: EventId::new(),
            project_id: ProjectId::parse("018f47a0-0000-7000-8000-00000000a001").unwrap(),
            workspace_id: WorkspaceId::parse("018f47a0-0000-7000-8000-00000000b001").unwrap(),
            session_id: Some(session_id.clone()),
            event_type: event_type.to_owned(),
            payload_schema: format!("{event_type}@1"),
            payload: CanonicalJson::parse(payload).expect("payload"),
            occurred_at: Some(timestamp("2026-07-31T01:02:03Z")),
            recorded_at: timestamp("2026-07-31T01:02:04Z"),
            producer: EventProducer {
                name: "test".to_owned(),
                version: "1".to_owned(),
                sequence: 0,
            },
            correlation_id: None,
            causation_id: None,
            privacy_class: PrivacyClass::Internal,
        }
    }

    #[test]
    fn event_payload_order_does_not_change_digest_and_chain_breaks_are_detected() {
        let session_id = SessionId::new();
        let first = Event::new(
            event_input(&session_id, "session.started", r#"{"b":2,"a":1}"#),
            Some(1),
            1,
            None,
        )
        .expect("first event");
        let same_fields_different_payload_order = Event::new(
            EventInput {
                payload: CanonicalJson::parse(r#"{"a":1,"b":2}"#).expect("payload"),
                ..first.input().clone()
            },
            Some(1),
            1,
            None,
        )
        .expect("equivalent event");
        let second = Event::new(
            event_input(&session_id, "turn.started", r#"{"turn":1}"#),
            Some(2),
            2,
            Some(first.event_sha256().clone()),
        )
        .expect("second event");

        assert_eq!(
            first.event_sha256(),
            same_fields_different_payload_order.event_sha256()
        );
        assert_eq!(first.payload_sha256(), &first.payload().sha256());
        verify_event_chain([&first, &second]).expect("valid event chain");

        let broken = Event::new(
            second.input().clone(),
            second.sequence(),
            second.project_sequence(),
            Some(Sha256Digest::of_bytes(b"wrong")),
        )
        .expect("individually valid event with broken link");
        assert!(verify_event_chain([&first, &broken]).is_err());
    }

    #[test]
    fn exact_integer_parsing_rejects_values_rfc_8785_would_round() {
        let safe =
            CanonicalJson::parse_exact_integers(r#"{"n":9007199254740991,"m":-9007199254740991}"#)
                .expect("safe integers");
        assert_eq!(
            safe.as_str(),
            r#"{"m":-9007199254740991,"n":9007199254740991}"#
        );
        for raw in [
            r#"{"n":9007199254740992}"#,
            r#"{"n":-9007199254740993}"#,
            r#"{"n":[18446744073709551617]}"#,
        ] {
            assert!(CanonicalJson::parse_exact_integers(raw).is_err(), "{raw}");
        }
        // Lexemes inside strings are text, not numbers.
        assert!(CanonicalJson::parse_exact_integers(r#"{"s":"9007199254740993"}"#).is_ok());
        // Fractions and exponents keep RFC 8785 double semantics.
        assert_eq!(
            CanonicalJson::parse_exact_integers(r#"{"n":1e300,"f":0.5}"#)
                .unwrap()
                .as_str(),
            r#"{"f":0.5,"n":1e+300}"#
        );
        // The lenient parser still rounds silently; this is why ingest uses the exact one.
        assert_eq!(
            CanonicalJson::parse(r#"{"n":9007199254740993}"#)
                .unwrap()
                .as_str(),
            r#"{"n":9007199254740992}"#
        );
    }

    #[test]
    fn timestamps_normalize_to_the_canonical_fraction_rule() {
        for (raw, canonical) in [
            ("2026-10-02T08:00:00Z", "2026-10-02T08:00:00Z"),
            ("2026-10-02T08:00:00.000Z", "2026-10-02T08:00:00Z"),
            ("2026-10-02T08:00:00.5Z", "2026-10-02T08:00:00.500Z"),
            ("2026-10-02T08:00:00.120000Z", "2026-10-02T08:00:00.120Z"),
            ("2026-10-02T08:00:00.000123Z", "2026-10-02T08:00:00.000123Z"),
            (
                "2026-10-02T08:00:00.0000001Z",
                "2026-10-02T08:00:00.000000100Z",
            ),
            ("2026-10-02t08:00:00Z", "2026-10-02T08:00:00Z"),
        ] {
            let normalized = UtcTimestamp::normalize(raw).expect(raw);
            assert_eq!(normalized.as_str(), canonical, "{raw}");
            assert_eq!(UtcTimestamp::parse(canonical).unwrap(), normalized);
        }
        for raw in [
            "2026-10-02T08:00:00+00:00",
            "2026-10-02T08:00:00.1234567891Z",
            "2026-10-02T08:00:00.Z",
            "2026-10-02 08:00:00",
            "not a time",
        ] {
            assert!(UtcTimestamp::normalize(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn canonical_request_digest_contains_only_request_body() {
        let body = CanonicalJson::parse(r#"{"title":"one"}"#).expect("body");
        let first = IdempotencyContext {
            principal_id: PrincipalId::parse("test-principal-a").expect("principal"),
            operation_id: OperationId::parse("createTask").expect("operation"),
            key: IdempotencyKey::parse("same-key").expect("key"),
        };
        let second = IdempotencyContext {
            principal_id: PrincipalId::parse("test-principal-b").expect("principal"),
            operation_id: OperationId::parse("createSession").expect("operation"),
            key: IdempotencyKey::parse("other-key").expect("key"),
        };

        assert_ne!(first, second);
        assert_eq!(canonical_request_sha256(&body), body.sha256());
    }
}
