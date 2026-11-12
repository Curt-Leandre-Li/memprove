use std::{collections::BTreeMap, fmt};

use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Number, Value};

use crate::{DomainError, Sha256Digest};

/// Canonical JSON serialized according to RFC 8785 (JCS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalJson {
    text: String,
}

impl CanonicalJson {
    /// Parses JSON, rejects duplicate members, and emits canonical compact bytes.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidJson`] for malformed input or duplicate members.
    pub fn parse(raw: &str) -> Result<Self, DomainError> {
        validate_raw_number_lexemes(raw)?;
        let mut deserializer = serde_json::Deserializer::from_str(raw);
        let value = NoDuplicateValue::deserialize(&mut deserializer)
            .map_err(|error| DomainError::InvalidJson(error.to_string()))?
            .0;
        deserializer
            .end()
            .map_err(|error| DomainError::InvalidJson(error.to_string()))?;
        Self::from_trusted_value(&value)
    }

    /// Parses like [`Self::parse`] and also rejects every integer literal (no
    /// fraction, no exponent) whose magnitude exceeds [`MAX_SAFE_INTEGER`].
    /// RFC 8785 serializes numbers as IEEE-754 doubles, so such a literal would
    /// otherwise be rounded silently and the canonical bytes would no longer
    /// mean what the producer sent.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidJson`] for an unsafe integer literal or
    /// any [`Self::parse`] failure.
    pub fn parse_exact_integers(raw: &str) -> Result<Self, DomainError> {
        reject_unsafe_integer_lexemes(raw)?;
        Self::parse(raw)
    }

    /// Canonicalizes a trusted, internally constructed JSON value.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidJson`] if serialization fails or a retained number is outside
    /// the accepted JCS double domain. Untrusted input must enter through [`Self::parse`] while its
    /// original number lexemes are still available.
    pub(crate) fn from_trusted_value(value: &Value) -> Result<Self, DomainError> {
        validate_number_domain(value)?;
        let text = serde_json_canonicalizer::to_string(value)
            .map_err(|error| DomainError::InvalidJson(error.to_string()))?;
        Ok(Self { text })
    }

    /// Returns the canonical compact JSON.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Hashes only the canonical JSON bytes.
    #[must_use]
    pub fn sha256(&self) -> Sha256Digest {
        Sha256Digest::of_bytes(self.text.as_bytes())
    }
}

impl fmt::Display for CanonicalJson {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

fn validate_number_domain(value: &Value) -> Result<(), DomainError> {
    match value {
        Value::Number(number) => validate_number_lexeme(&number.to_string())?,
        Value::Array(items) => {
            for item in items {
                validate_number_domain(item)?;
            }
        }
        Value::Object(object) => {
            for item in object.values() {
                validate_number_domain(item)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
    Ok(())
}

fn validate_raw_number_lexemes(raw: &str) -> Result<(), DomainError> {
    let bytes = raw.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        match bytes[offset] {
            b'"' => offset = string_end(bytes, offset + 1),
            b'-' | b'0'..=b'9' => {
                let end = number_end(bytes, offset)?;
                validate_number_lexeme(&raw[offset..end])?;
                offset = end;
            }
            _ => offset += 1,
        }
    }
    Ok(())
}

/// Largest integer every IEEE-754 double neighbourhood represents exactly
/// (2^53 − 1).
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn reject_unsafe_integer_lexemes(raw: &str) -> Result<(), DomainError> {
    let bytes = raw.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        match bytes[offset] {
            b'"' => offset = string_end(bytes, offset + 1),
            b'-' | b'0'..=b'9' => {
                let end = number_end(bytes, offset)?;
                let lexeme = &raw[offset..end];
                let digits = lexeme.strip_prefix('-').unwrap_or(lexeme);
                if digits.bytes().all(|byte| byte.is_ascii_digit())
                    && digits
                        .parse::<u64>()
                        .map_or(true, |value| value > MAX_SAFE_INTEGER)
                {
                    return Err(DomainError::InvalidJson(format!(
                        "JSON integer {lexeme} exceeds 2^53 - 1 and cannot be kept exactly"
                    )));
                }
                offset = end;
            }
            _ => offset += 1,
        }
    }
    Ok(())
}

fn string_end(bytes: &[u8], mut offset: usize) -> usize {
    while offset < bytes.len() {
        match bytes[offset] {
            b'\\' => offset = offset.saturating_add(2),
            b'"' => return offset + 1,
            _ => offset += 1,
        }
    }
    offset
}

fn number_end(bytes: &[u8], start: usize) -> Result<usize, DomainError> {
    let mut offset = start;
    if bytes[offset] == b'-' {
        offset += 1;
    }
    match bytes.get(offset) {
        Some(b'0') => {
            offset += 1;
            if matches!(bytes.get(offset), Some(b'0'..=b'9')) {
                return Err(invalid_number_grammar());
            }
        }
        Some(b'1'..=b'9') => {
            offset += 1;
            while matches!(bytes.get(offset), Some(b'0'..=b'9')) {
                offset += 1;
            }
        }
        _ => return Err(invalid_number_grammar()),
    }
    if bytes.get(offset) == Some(&b'.') {
        offset += 1;
        offset = digits_end(bytes, offset)?;
    }
    if matches!(bytes.get(offset), Some(b'e' | b'E')) {
        offset += 1;
        if matches!(bytes.get(offset), Some(b'+' | b'-')) {
            offset += 1;
        }
        offset = digits_end(bytes, offset)?;
    }
    Ok(offset)
}

fn digits_end(bytes: &[u8], mut offset: usize) -> Result<usize, DomainError> {
    if !matches!(bytes.get(offset), Some(b'0'..=b'9')) {
        return Err(invalid_number_grammar());
    }
    while matches!(bytes.get(offset), Some(b'0'..=b'9')) {
        offset += 1;
    }
    Ok(offset)
}

fn invalid_number_grammar() -> DomainError {
    DomainError::InvalidJson("JSON number does not obey the JSON grammar".to_owned())
}

fn validate_number_lexeme(lexeme: &str) -> Result<(), DomainError> {
    let parsed = lexeme.parse::<f64>().map_err(|_| {
        DomainError::InvalidJson(format!("JSON number {lexeme:?} is not an IEEE-754 double"))
    })?;
    if !parsed.is_finite() {
        return Err(DomainError::InvalidJson(format!(
            "JSON number {lexeme:?} is outside the finite IEEE-754 double domain"
        )));
    }
    Ok(())
}

struct NoDuplicateValue(Value);

impl<'de> Deserialize<'de> for NoDuplicateValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateVisitor)
    }
}

struct NoDuplicateVisitor;

impl<'de> Visitor<'de> for NoDuplicateVisitor {
    type Value = NoDuplicateValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object members")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(NoDuplicateValue)
            .ok_or_else(|| E::custom("JSON number must be finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items = Vec::new();
        while let Some(item) = sequence.next_element::<NoDuplicateValue>()? {
            items.push(item.0);
        }
        Ok(NoDuplicateValue(Value::Array(items)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut members = BTreeMap::new();
        while let Some((key, value)) = object.next_entry::<String, NoDuplicateValue>()? {
            if members.insert(key.clone(), value.0).is_some() {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object member {key:?}"
                )));
            }
        }
        Ok(NoDuplicateValue(Value::Object(
            members.into_iter().collect(),
        )))
    }
}
