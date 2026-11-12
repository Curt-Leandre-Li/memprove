//! Bearer-token secrets and the registry-backed authentication ports.
//!
//! A token secret is 32 bytes from the operating system's CSPRNG, encoded as
//! 43 unpadded base64url characters; the bearer credential is the secret
//! itself. Only its SHA-256 verifier is persisted. Secrets are never logged and
//! never appear in error messages.

use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use memprove_domain::Sha256Digest;
use memprove_engine::runtime::{
    AuthenticationError, Authenticator, Principal, ProjectCounter, Unavailable,
};
use memprove_store_sqlite::RegistryStore;

/// Registry shared by the authenticator, the status port, and token handlers.
pub type SharedRegistry = Arc<Mutex<RegistryStore>>;

/// Secret length in bytes before encoding.
pub const SECRET_BYTES: usize = 32;
/// Encoded secret length: unpadded base64url of [`SECRET_BYTES`].
pub const SECRET_CHARS: usize = 43;

/// A one-time token secret. `Debug` never prints the value.
pub struct Secret(String);

impl Secret {
    /// The plaintext secret, for the single disclosure to its client.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Lower-case hex SHA-256 verifier of this secret.
    #[must_use]
    pub fn verifier(&self) -> String {
        verifier(&self.0)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

/// The operating system's random source failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RandomUnavailable;

impl fmt::Display for RandomUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the system random source is unavailable")
    }
}

impl std::error::Error for RandomUnavailable {}

/// Generates a fresh secret from the system CSPRNG.
///
/// # Errors
///
/// Returns [`RandomUnavailable`] when the system random source fails.
pub fn generate_secret() -> Result<Secret, RandomUnavailable> {
    let mut bytes = [0_u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| RandomUnavailable)?;
    let secret = base64url(&bytes);
    bytes.fill(0);
    Ok(Secret(secret))
}

/// Lower-case hex SHA-256 of `secret`.
#[must_use]
pub fn verifier(secret: &str) -> String {
    Sha256Digest::of_bytes(secret.as_bytes())
        .as_str()
        .to_owned()
}

/// Whether `value` has the exact shape of a token secret.
#[must_use]
pub fn is_secret_shaped(value: &str) -> bool {
    value.len() == SECRET_CHARS
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// U1 (C4, Owner O6): whether `text` holds a string in the format of a token
/// secret: a maximal run of exactly [`SECRET_CHARS`] characters of `A-Z`,
/// `a-z`, `0-9`, `-`, and `_`; U5 P2 (Owner O4 a): with at least one
/// upper-case letter, one lower-case letter, and one digit
/// ([`memprove_domain::holds_token_shape`]). A longer run (a long
/// identifier, a digest) or a shorter one is not a token; a token with a
/// space, a quote, `=`, or any other character next to it is.
#[must_use]
pub fn contains_secret_shape(text: &str) -> bool {
    memprove_domain::holds_token_shape(text)
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let block = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |block, (index, byte)| {
                block | (u32::from(*byte) << (16 - 8 * index))
            });
        let symbols = chunk.len() + 1;
        for index in 0..symbols {
            let sextet = (block >> (18 - 6 * index)) & 0x3f;
            encoded.push(char::from(ALPHABET[sextet as usize]));
        }
    }
    encoded
}

/// Locks the shared registry; a poisoned lock is treated as unavailable.
///
/// # Errors
///
/// Returns [`Unavailable`] when a previous holder panicked.
pub fn lock(registry: &SharedRegistry) -> Result<MutexGuard<'_, RegistryStore>, Unavailable> {
    registry.lock().map_err(|_| Unavailable)
}

/// Authenticates bearer secrets against active registry tokens.
pub struct RegistryAuthenticator(SharedRegistry);

impl RegistryAuthenticator {
    /// Creates an authenticator over `registry`.
    #[must_use]
    pub fn new(registry: SharedRegistry) -> Self {
        Self(registry)
    }
}

impl Authenticator for RegistryAuthenticator {
    fn authenticate(&self, bearer: Option<&str>) -> Result<Principal, AuthenticationError> {
        let Some(secret) = bearer.filter(|secret| is_secret_shaped(secret)) else {
            return Err(AuthenticationError::Rejected);
        };
        let registry = lock(&self.0).map_err(|Unavailable| AuthenticationError::Unavailable)?;
        match registry.authenticate_token(&verifier(secret)) {
            Ok(Some(token)) => Ok(Principal::new(token.token_id, token.scopes)),
            Ok(None) => Err(AuthenticationError::Rejected),
            Err(error) => {
                eprintln!("memproved: cannot read client tokens: {error}");
                Err(AuthenticationError::Unavailable)
            }
        }
    }
}

/// Reports the registry's `ACTIVE` Project count.
pub struct RegistryProjects(SharedRegistry);

impl RegistryProjects {
    /// Creates the port over `registry`.
    #[must_use]
    pub fn new(registry: SharedRegistry) -> Self {
        Self(registry)
    }
}

impl ProjectCounter for RegistryProjects {
    fn project_count(&self) -> Result<u64, Unavailable> {
        lock(&self.0)?.active_project_count().map_err(|error| {
            eprintln!("memproved: cannot count Projects: {error}");
            Unavailable
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_matches_rfc_4648_vectors() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(&[0xfb, 0xff, 0xbf]), "-_-_");
    }

    #[test]
    fn secrets_are_43_base64url_chars_and_distinct() {
        let first = generate_secret().unwrap();
        let second = generate_secret().unwrap();
        assert!(is_secret_shaped(first.expose()));
        assert_eq!(first.expose().len(), SECRET_CHARS);
        assert_ne!(first.expose(), second.expose());
        assert_eq!(first.verifier().len(), 64);
        assert_ne!(first.verifier(), first.expose());
        assert_eq!(format!("{first:?}"), "Secret(<redacted>)");
    }

    #[test]
    fn verifier_is_hex_sha256() {
        assert_eq!(
            verifier("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn secret_shape_is_strict() {
        assert!(!is_secret_shaped(""));
        assert!(!is_secret_shaped(&"a".repeat(42)));
        assert!(!is_secret_shaped(&"a".repeat(44)));
        assert!(!is_secret_shaped(&format!("{}=", "a".repeat(42))));
        assert!(is_secret_shaped(
            &"a-_".repeat(14).chars().chain(['Z']).collect::<String>()
        ));
    }
}
