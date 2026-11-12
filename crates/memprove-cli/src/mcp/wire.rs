//! The bytes of the protocol: a bounded line reader for standard input,
//! and a compact JSON writer over [`JsonValue`] with [`json_string`] for
//! every string, so that no message holds a raw line break (ADR 0014 E7.3).

use std::io::{self, BufRead};

use memprove_protocol::strict_json::JsonValue;

use crate::json_string;

/// `value` as compact JSON: no whitespace, members in their order, numbers
/// in their lexical form, every string through [`json_string`].
pub(super) fn compact(value: &JsonValue) -> String {
    let mut out = String::new();
    write(value, &mut out);
    out
}

fn write(value: &JsonValue, out: &mut String) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        JsonValue::Number(number) => out.push_str(number),
        JsonValue::String(text) => out.push_str(&json_string(text)),
        JsonValue::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write(item, out);
            }
            out.push(']');
        }
        JsonValue::Object(members) => {
            out.push('{');
            for (index, (name, member)) in members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&json_string(name));
                out.push(':');
                write(member, out);
            }
            out.push('}');
        }
    }
}

/// A JSON string.
pub(super) fn text(value: &str) -> JsonValue {
    JsonValue::String(value.to_owned())
}

/// A JSON number.
pub(super) fn number(value: impl std::fmt::Display) -> JsonValue {
    JsonValue::Number(value.to_string())
}

/// A JSON string, or `null`.
pub(super) fn optional(value: Option<&str>) -> JsonValue {
    value.map_or(JsonValue::Null, text)
}

/// A JSON object of `members`, in their order.
pub(super) fn object<'a>(members: impl IntoIterator<Item = (&'a str, JsonValue)>) -> JsonValue {
    JsonValue::Object(
        members
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    )
}

/// A JSON array of strings.
pub(super) fn texts<'a>(values: impl IntoIterator<Item = &'a str>) -> JsonValue {
    JsonValue::Array(values.into_iter().map(text).collect())
}

/// One line of standard input.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Line {
    /// Its bytes, without the line feed.
    Text(Vec<u8>),
    /// It was longer than the limit; it was read to its end and dropped.
    TooLong,
}

/// Reads the next line, without its `\n`; a line longer than `limit` bytes
/// is drained to its end and reported as [`Line::TooLong`], never held.
/// `None` at the end of input; a last line without `\n` is a line.
///
/// # Errors
///
/// A read error other than an interruption.
pub(super) fn read_line(input: &mut dyn BufRead, limit: usize) -> io::Result<Option<Line>> {
    let mut line = Vec::new();
    let mut too_long = false;
    let mut started = false;
    loop {
        let buffer = match input.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if buffer.is_empty() {
            return Ok(started.then(|| finish(line, too_long)));
        }
        started = true;
        let end = buffer.iter().position(|byte| *byte == b'\n');
        let chunk = &buffer[..end.unwrap_or(buffer.len())];
        if !too_long {
            if line.len() + chunk.len() > limit {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(chunk);
            }
        }
        let used = end.map_or(buffer.len(), |end| end + 1);
        input.consume(used);
        if end.is_some() {
            return Ok(Some(finish(line, too_long)));
        }
    }
}

fn finish(line: Vec<u8>, too_long: bool) -> Line {
    if too_long {
        Line::TooLong
    } else {
        Line::Text(line)
    }
}

#[cfg(test)]
mod tests {
    use memprove_protocol::strict_json::parse_strict;

    use super::*;

    #[test]
    fn compact_round_trips_and_escapes_every_line_break() {
        for text in [
            r#"{"a":[1,-2,3.50,1e3,12345678901234567890],"b":{"c":null,"d":true,"e":false},"f":"x"}"#,
            "[]",
            "{}",
            r#""  \u0000 \t 「」""#,
        ] {
            let value = parse_strict(text).unwrap();
            let written = compact(&value);
            assert_eq!(parse_strict(&written).unwrap(), value, "{text}");
            assert!(!written.contains('\n'), "{written}");
        }
        let value = text("line one\nline two\r\n");
        assert_eq!(compact(&value), r#""line one\u000aline two\u000d\u000a""#);
        // Numbers keep their lexical form, so an id is echoed byte for byte.
        let id = parse_strict("12345678901234567890").unwrap();
        assert_eq!(compact(&id), "12345678901234567890");
    }

    #[test]
    fn lines_are_bounded_and_drained() {
        let input = b"short\r\n0123456789ABC\nnext\nlast without newline";
        let mut reader = io::BufReader::with_capacity(4, &input[..]);
        let mut lines = Vec::new();
        while let Some(line) = read_line(&mut reader, 10).unwrap() {
            lines.push(line);
        }
        assert_eq!(
            lines,
            [
                Line::Text(b"short\r".to_vec()),
                Line::TooLong,
                Line::Text(b"next".to_vec()),
                Line::TooLong,
            ]
        );
        let mut empty: &[u8] = b"";
        assert_eq!(read_line(&mut empty, 10).unwrap(), None);
        let mut exact: &[u8] = b"0123456789\n";
        assert_eq!(
            read_line(&mut exact, 10).unwrap(),
            Some(Line::Text(b"0123456789".to_vec()))
        );
    }
}
