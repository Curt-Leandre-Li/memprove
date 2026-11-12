use std::{fmt, fmt::Write as _, str::FromStr};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::{Uuid, Variant};

/// Validation failures at the domain boundary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DomainError {
    #[error("{kind} must be a UUIDv7")]
    InvalidUuidV7 { kind: &'static str },
    #[error("timestamp must be canonical UTC RFC3339")]
    InvalidTimestamp,
    #[error("SHA-256 digest must be 64 lower-case hexadecimal characters")]
    InvalidDigest,
    #[error("{kind} must be non-empty, contain no control characters, and be at most 255 bytes")]
    InvalidOpaqueValue { kind: &'static str },
    #[error("invalid JSON: {0}")]
    InvalidJson(String),
    #[error("invalid event chain: {0}")]
    InvalidEventChain(String),
}

macro_rules! uuid_v7_id {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("Typed UUIDv7 identity for ", $kind, ".")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generates a time-ordered `UUIDv7`.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7().to_string())
            }

            /// Parses and validates an existing `UUIDv7`.
            ///
            /// # Errors
            ///
            /// Returns [`DomainError::InvalidUuidV7`] for malformed or non-v7 input.
            pub fn parse(value: &str) -> Result<Self, DomainError> {
                let parsed = Uuid::parse_str(value)
                    .map_err(|_| DomainError::InvalidUuidV7 { kind: $kind })?;
                if parsed.get_version_num() != 7
                    || parsed.get_variant() != Variant::RFC4122
                    || value != parsed.hyphenated().to_string()
                {
                    return Err(DomainError::InvalidUuidV7 { kind: $kind });
                }
                Ok(Self(value.to_owned()))
            }

            /// Returns the underlying UUID value.
            #[must_use]
            pub fn as_uuid(&self) -> Uuid {
                Uuid::parse_str(&self.0).expect("typed ID contains a validated UUIDv7")
            }

            /// Returns the canonical lower-case hyphenated spelling.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = DomainError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

uuid_v7_id!(ProjectId, "project_id");
uuid_v7_id!(WorkspaceId, "workspace_id");
uuid_v7_id!(TaskId, "task_id");
uuid_v7_id!(SessionId, "session_id");
uuid_v7_id!(EventId, "event_id");
uuid_v7_id!(AgentId, "agent_id");
uuid_v7_id!(EvidenceId, "evidence_id");
uuid_v7_id!(RepositorySnapshotId, "snapshot_id");
uuid_v7_id!(CheckpointId, "checkpoint_id");
uuid_v7_id!(CorrelationId, "correlation_id");

macro_rules! opaque_id {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("Validated opaque ", $kind, ".")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parses a non-empty opaque namespace value.
            ///
            /// # Errors
            ///
            /// Returns [`DomainError::InvalidOpaqueValue`] when the value violates its boundary.
            pub fn parse(value: &str) -> Result<Self, DomainError> {
                if value.is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
                    return Err(DomainError::InvalidOpaqueValue { kind: $kind });
                }
                Ok(Self(value.to_owned()))
            }

            /// Returns the validated value.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = DomainError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

opaque_id!(PrincipalId, "principal_id");
opaque_id!(OperationId, "operation_id");
opaque_id!(IdempotencyKey, "idempotency_key");

/// A canonical UTC RFC3339 timestamp.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct UtcTimestamp(String);

impl UtcTimestamp {
    /// Parses an exact canonical `Z` timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidTimestamp`] for non-canonical or non-UTC input.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        if !value.ends_with('Z') {
            return Err(DomainError::InvalidTimestamp);
        }
        let parsed =
            DateTime::parse_from_rfc3339(value).map_err(|_| DomainError::InvalidTimestamp)?;
        if parsed.offset().local_minus_utc() != 0 {
            return Err(DomainError::InvalidTimestamp);
        }
        let canonical = parsed
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::AutoSi, true);
        if canonical != value {
            return Err(DomainError::InvalidTimestamp);
        }
        Ok(Self(canonical))
    }

    /// Normalizes any RFC 3339 `Z` timestamp to the canonical form accepted by
    /// [`Self::parse`]: `YYYY-MM-DDTHH:MM:SS[.fff|.ffffff|.fffffffff]Z`, with the
    /// fraction omitted when it is zero and otherwise written with the fewest
    /// of 3, 6, or 9 digits that represent it exactly (`chrono`'s `AutoSi`).
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidTimestamp`] for non-`Z` input, malformed
    /// input, or more than nine fractional digits, which could not be kept
    /// exactly.
    pub fn normalize(value: &str) -> Result<Self, DomainError> {
        if !value.ends_with('Z') {
            return Err(DomainError::InvalidTimestamp);
        }
        if let Some((_, fraction)) = value.split_once('.') {
            let digits = &fraction[..fraction.len() - 1];
            if digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(DomainError::InvalidTimestamp);
            }
        }
        let parsed =
            DateTime::parse_from_rfc3339(value).map_err(|_| DomainError::InvalidTimestamp)?;
        if parsed.offset().local_minus_utc() != 0 {
            return Err(DomainError::InvalidTimestamp);
        }
        Self::parse(
            &parsed
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::AutoSi, true),
        )
    }

    /// Captures the current UTC time in canonical RFC3339 form.
    #[must_use]
    pub fn now() -> Self {
        Self(Utc::now().to_rfc3339_opts(SecondsFormat::AutoSi, true))
    }

    /// Returns the canonical timestamp.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UtcTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for UtcTimestamp {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for UtcTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// A validated lower-case SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    /// Computes SHA-256 over exact bytes.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut encoded = String::with_capacity(64);
        for byte in Sha256::digest(bytes) {
            write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Self(encoded)
    }

    /// Parses a canonical lower-case hexadecimal digest.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidDigest`] for non-canonical input.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DomainError::InvalidDigest);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the canonical hexadecimal spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for Sha256Digest {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}
