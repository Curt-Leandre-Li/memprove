//! The server's own check of tool arguments against the tool's
//! `inputSchema` (ADR 0014 E7.5.4): the subset of JSON Schema 2020-12 the
//! ten schemas use, read from the same JSON text `tools/list` sends, so the
//! two never disagree. A violation names the parameter (a schema key),
//! never the value.

use memprove_protocol::strict_json::JsonValue;

use super::{failure::Failure, wire::compact};
use crate::context::is_run_label;

/// The one `pattern` the schemas use: a run label or `last`.
pub(super) const LABEL_PATTERN: &str = "^(r[1-9][0-9]*|last)$";

/// U8 A3 (Owner O4): the pattern of `begin`'s `agent_name`.
pub(super) const AGENT_NAME_PATTERN: &str = "^[A-Za-z0-9_-]{1,64}$";

/// The keywords the validator reads; [`check_schema`] refuses any other.
#[cfg(test)]
const KEYWORDS: [&str; 16] = [
    "type",
    "enum",
    "additionalProperties",
    "properties",
    "required",
    "minLength",
    "maxLength",
    "pattern",
    "minimum",
    "maximum",
    "minItems",
    "maxItems",
    "uniqueItems",
    "items",
    "$ref",
    "description",
];

/// Checks `arguments` (an object) against the tool's `schema`.
///
/// # Errors
///
/// [`Failure::UnknownArgument`] for a member the schema does not name, and
/// [`Failure::InvalidArguments`] naming the first parameter, in schema
/// order, that is missing or does not hold.
pub(super) fn validate(schema: &JsonValue, arguments: &JsonValue) -> Result<(), Failure> {
    let properties = schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .unwrap_or_default();
    let Some(members) = arguments.as_object() else {
        return Err(Failure::UnknownArgument);
    };
    if members
        .iter()
        .any(|(name, _)| !properties.iter().any(|(known, _)| known == name))
    {
        return Err(Failure::UnknownArgument);
    }
    for (name, property) in properties {
        let holds = match arguments.get(name) {
            Some(value) => holds(property, value, schema),
            None => !required(schema).contains(&name.as_str()),
        };
        if !holds {
            return Err(Failure::InvalidArguments(name.clone()));
        }
    }
    Ok(())
}

fn required(schema: &JsonValue) -> Vec<&str> {
    schema
        .get("required")
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(JsonValue::as_str)
        .collect()
}

fn integer(schema: &JsonValue, name: &str) -> Option<i64> {
    schema.get(name).and_then(JsonValue::as_i64)
}

fn count(schema: &JsonValue, name: &str) -> Option<usize> {
    integer(schema, name).and_then(|value| usize::try_from(value).ok())
}

/// Whether `value` holds against `schema`; `root` holds the `$defs`.
fn holds(schema: &JsonValue, value: &JsonValue, root: &JsonValue) -> bool {
    if let Some(reference) = schema.get("$ref").and_then(JsonValue::as_str) {
        return reference
            .strip_prefix("#/$defs/")
            .and_then(|name| root.get("$defs")?.get(name))
            .is_some_and(|definition| holds(definition, value, root));
    }
    match schema.get("type").and_then(JsonValue::as_str) {
        Some("object") => object_holds(schema, value, root),
        Some("array") => array_holds(schema, value, root),
        Some("string") => value
            .as_str()
            .is_some_and(|text| string_holds(schema, text)),
        Some("integer") => value.as_i64().is_some_and(|number| {
            integer(schema, "minimum").is_none_or(|minimum| number >= minimum)
                && integer(schema, "maximum").is_none_or(|maximum| number <= maximum)
        }),
        Some("boolean") => value.as_bool().is_some(),
        _ => false,
    }
}

fn object_holds(schema: &JsonValue, value: &JsonValue, root: &JsonValue) -> bool {
    let Some(members) = value.as_object() else {
        return false;
    };
    let properties = schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .unwrap_or_default();
    members.iter().all(|(name, member)| {
        properties
            .iter()
            .find(|(known, _)| known == name)
            .is_some_and(|(_, property)| holds(property, member, root))
    }) && required(schema)
        .iter()
        .all(|name| members.iter().any(|(member, _)| member == name))
}

fn array_holds(schema: &JsonValue, value: &JsonValue, root: &JsonValue) -> bool {
    let Some(items) = value.as_array() else {
        return false;
    };
    let in_range = count(schema, "minItems").is_none_or(|minimum| items.len() >= minimum)
        && count(schema, "maxItems").is_none_or(|maximum| items.len() <= maximum);
    let unique = schema.get("uniqueItems").and_then(JsonValue::as_bool) != Some(true) || {
        let mut seen = items.iter().map(compact).collect::<Vec<_>>();
        seen.sort();
        seen.windows(2).all(|pair| pair[0] != pair[1])
    };
    in_range
        && unique
        && schema
            .get("items")
            .is_none_or(|item| items.iter().all(|value| holds(item, value, root)))
}

fn string_holds(schema: &JsonValue, text: &str) -> bool {
    let length = text.chars().count();
    let pattern = match schema.get("pattern").and_then(JsonValue::as_str) {
        None => true,
        Some(LABEL_PATTERN) => is_run_label(text) || text == "last",
        Some(AGENT_NAME_PATTERN) => memprove_domain::SessionAgent::is_name(text),
        // A pattern the validator does not know never holds (fail closed);
        // `check_schema` keeps such a pattern out of the schemas.
        Some(_) => false,
    };
    // U4 S2: `enum`, a list of strings, holds the value to one of them.
    let listed = schema.get("enum").is_none_or(|values| {
        values
            .as_array()
            .unwrap_or_default()
            .iter()
            .any(|value| value.as_str() == Some(text))
    });
    count(schema, "minLength").is_none_or(|minimum| length >= minimum)
        && count(schema, "maxLength").is_none_or(|maximum| length <= maximum)
        && pattern
        && listed
}

/// Checks that `schema` is one the validator reads in full (E7.5.4): every
/// object schema with `"additionalProperties": false`, every string with
/// `maxLength`, every array with `uniqueItems`, only known keywords and
/// patterns, and every `$ref` resolvable.
///
/// # Errors
///
/// What does not hold.
#[cfg(test)]
pub(super) fn check_schema(schema: &JsonValue) -> Result<(), String> {
    if schema.get("$schema").is_some() {
        return Err("$schema is not used (2020-12 is the default)".to_owned());
    }
    let definitions = schema
        .get("$defs")
        .and_then(JsonValue::as_object)
        .unwrap_or_default();
    for (_, definition) in definitions {
        check_node(definition, schema)?;
    }
    let mut top = schema.clone();
    if let JsonValue::Object(members) = &mut top {
        members.retain(|(name, _)| name != "$defs");
    }
    check_node(&top, schema)
}

#[cfg(test)]
fn check_node(node: &JsonValue, root: &JsonValue) -> Result<(), String> {
    let members = node.as_object().ok_or("a schema is not an object")?;
    for (name, _) in members {
        if !KEYWORDS.contains(&name.as_str()) {
            return Err(format!("unknown keyword {name}"));
        }
    }
    if let Some(reference) = node.get("$ref").and_then(JsonValue::as_str) {
        let name = reference
            .strip_prefix("#/$defs/")
            .ok_or("a $ref outside $defs")?;
        root.get("$defs")
            .and_then(|definitions| definitions.get(name))
            .ok_or_else(|| format!("{reference} does not resolve"))?;
        return Ok(());
    }
    match node.get("type").and_then(JsonValue::as_str) {
        Some("object") => {
            if node.get("additionalProperties") != Some(&JsonValue::Bool(false)) {
                return Err("an object without additionalProperties false".to_owned());
            }
            for (_, property) in node
                .get("properties")
                .and_then(JsonValue::as_object)
                .unwrap_or_default()
            {
                check_node(property, root)?;
            }
            Ok(())
        }
        Some("array") => {
            if node.get("uniqueItems") != Some(&JsonValue::Bool(true)) {
                return Err("an array without uniqueItems".to_owned());
            }
            check_node(node.get("items").ok_or("an array without items")?, root)
        }
        Some("string") => {
            if node.get("maxLength").is_none() {
                return Err("a string without maxLength".to_owned());
            }
            if let Some(values) = node.get("enum") {
                let values = values.as_array().ok_or("an enum that is not an array")?;
                if values.is_empty() || values.iter().any(|value| value.as_str().is_none()) {
                    return Err("an enum that is not a list of strings".to_owned());
                }
            }
            match node.get("pattern").and_then(JsonValue::as_str) {
                None | Some(LABEL_PATTERN | AGENT_NAME_PATTERN) => Ok(()),
                Some(other) => Err(format!("unknown pattern {other}")),
            }
        }
        Some("integer" | "boolean") => Ok(()),
        _ => Err("a schema without a known type".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use memprove_protocol::strict_json::parse_strict;

    use super::*;
    use crate::mcp::tools::find;

    fn check(tool: &str, arguments: &str) -> Result<(), String> {
        let schema = find(tool).unwrap().input_schema();
        validate(&schema, &parse_strict(arguments).unwrap()).map_err(|failure| match failure {
            Failure::InvalidArguments(parameter) => parameter,
            Failure::UnknownArgument => "<unknown>".to_owned(),
            _ => unreachable!(),
        })
    }

    /// U16: `pack` takes `task`, `budget`, and `query`, nothing else; none
    /// is required.
    #[test]
    fn pack_takes_task_budget_and_query_only() {
        let schema = find("pack").unwrap().input_schema();
        let properties = schema
            .get("properties")
            .and_then(JsonValue::as_object)
            .unwrap();
        let names = properties
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["task", "budget", "query"]);
        assert!(schema.get("required").is_none());
        for arguments in [r#"{"manifest":true}"#, r#"{"query_profile":"search.v2"}"#] {
            assert_eq!(check("pack", arguments), Err("<unknown>".to_owned()));
        }
    }

    /// U3 G5: `pack` takes an optional string `query` of 1 through 256
    /// characters, with `budget` and `task`.
    #[test]
    fn pack_takes_a_query_string() {
        assert_eq!(check("pack", r#"{"query":"budget"}"#), Ok(()));
        assert_eq!(
            check("pack", r#"{"query":"budget","budget":1000,"task":2}"#),
            Ok(())
        );
        let longest = format!(r#"{{"query":"{}"}}"#, "é".repeat(256));
        assert_eq!(check("pack", &longest), Ok(()));
        let longer = format!(r#"{{"query":"{}"}}"#, "é".repeat(257));
        for arguments in [
            longer.as_str(),
            r#"{"query":""}"#,
            r#"{"query":3}"#,
            r#"{"query":null}"#,
            r#"{"query":["x"]}"#,
        ] {
            assert_eq!(
                check("pack", arguments),
                Err("query".to_owned()),
                "{arguments}"
            );
        }
    }

    #[test]
    fn arguments_are_checked_against_the_listed_schema() {
        assert_eq!(check("pack", "{}"), Ok(()));
        assert_eq!(check("pack", r#"{"task":3,"budget":1000}"#), Ok(()));
        assert_eq!(check("pack", r#"{"budget":999}"#), Err("budget".to_owned()));
        assert_eq!(
            check("pack", r#"{"budget":1000001}"#),
            Err("budget".to_owned())
        );
        assert_eq!(check("pack", r#"{"task":0}"#), Err("task".to_owned()));
        assert_eq!(check("pack", r#"{"task":1.0}"#), Err("task".to_owned()));
        assert_eq!(check("pack", r#"{"task":"3"}"#), Err("task".to_owned()));
        assert_eq!(
            check("pack", r#"{"task":"018f47a0-0000-7000-8000-000000000004"}"#),
            Err("task".to_owned())
        );
        assert_eq!(check("pack", r#"{"bogus":1}"#), Err("<unknown>".to_owned()));
        assert_eq!(check("claims", "{}"), Err("claims".to_owned()));
        assert_eq!(
            check("claims", r#"{"claims":[]}"#),
            Err("claims".to_owned())
        );
        assert_eq!(
            check("claims", r#"{"claims":[3,3]}"#),
            Err("claims".to_owned())
        );
        assert_eq!(check("claims", r#"{"claims":[3,5,6,7]}"#), Ok(()));
        assert_eq!(
            check("context", r#"{"x":null}"#),
            Err("<unknown>".to_owned())
        );
        assert_eq!(check("tasks", r#"{"page":2}"#), Ok(()));
        // Write tools: labels, nested objects, $defs.
        assert_eq!(check("checkpoint", "{}"), Err("next".to_owned()));
        let good = r#"{"facts":[{"text":"t","evidence":["r1","last"],"supersedes":[2]}],"withdraw":[3],"withdraw_reason":"w","next":{"text":"n"}}"#;
        assert_eq!(check("checkpoint", good), Ok(()));
        for (bad, parameter) in [
            (
                r#"{"facts":[{"text":"t","evidence":["r0"]}],"next":{"text":"n"}}"#,
                "facts",
            ),
            (
                r#"{"facts":[{"text":"t","evidence":["R1"]}],"next":{"text":"n"}}"#,
                "facts",
            ),
            (
                r#"{"facts":[{"text":"t","bogus":1}],"next":{"text":"n"}}"#,
                "facts",
            ),
            (r#"{"facts":[{"text":""}],"next":{"text":"n"}}"#, "facts"),
            (r#"{"next":{}}"#, "next"),
            (r#"{"next":{"text":"n"},"withdraw":[0]}"#, "withdraw"),
        ] {
            assert_eq!(check("checkpoint", bad), Err(parameter.to_owned()), "{bad}");
        }
        let long = "é".repeat(4096);
        assert_eq!(check("decide", &format!(r#"{{"text":"{long}"}}"#)), Ok(()));
        assert_eq!(
            check("decide", &format!(r#"{{"text":"{long}x"}}"#)),
            Err("text".to_owned())
        );
        assert_eq!(
            check("end", r#"{"abandoned":"yes"}"#),
            Err("abandoned".to_owned())
        );
        assert_eq!(check("complete", r#"{"evidence":["r12"]}"#), Ok(()));
    }

    #[test]
    fn an_unknown_pattern_or_keyword_is_refused() {
        let schema = parse_strict(
            r#"{"type":"object","additionalProperties":false,"properties":{"a":{"type":"string","maxLength":3,"pattern":"^x$"}}}"#,
        )
        .unwrap();
        assert!(check_schema(&schema).is_err());
        let arguments = parse_strict(r#"{"a":"x"}"#).unwrap();
        assert!(validate(&schema, &arguments).is_err());
        let loose = parse_strict(r#"{"type":"object","properties":{}}"#).unwrap();
        assert!(check_schema(&loose).is_err());
        let format = parse_strict(
            r#"{"type":"object","additionalProperties":false,"properties":{"a":{"type":"string","maxLength":3,"format":"uuid"}}}"#,
        )
        .unwrap();
        assert!(check_schema(&format).is_err());
    }
}
