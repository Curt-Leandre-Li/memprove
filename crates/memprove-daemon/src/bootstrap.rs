//! Offline `memproved bootstrap`: issues the first control token.
//!
//! Bootstrap is not an HTTP route. It runs only while no daemon holds the
//! data directory's instance lock, and only when no active control token
//! exists. Delivery is two-phase so a lost secret never leaves a usable token:
//! the token is recorded as `pending`, the secret is written exactly once
//! (stdout or a new 0600 file), and only then is the token activated. A failed
//! write discards the token; a crash leaves it pending, never authenticating,
//! and the next bootstrap discards it.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use memprove_domain::UtcTimestamp;
use memprove_store_sqlite::{NewClientToken, RegistryStore, StoreError};

use crate::{
    DaemonError,
    auth::{Secret, generate_secret},
    config::BootstrapConfig,
    instance,
};

/// Scopes granted to the bootstrap token.
const BOOTSTRAP_SCOPES: [&str; 1] = ["control"];

/// Non-sensitive result of a successful bootstrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapReport {
    /// Identifier of the new control token.
    pub token_id: String,
    /// Label stored with the token.
    pub client_label: String,
    /// Pending tokens from interrupted earlier bootstraps that were discarded.
    pub discarded_pending: u64,
}

/// Runs bootstrap. Without `config.output()`, the secret is written to
/// `stdout` followed by a newline.
///
/// # Errors
///
/// Returns [`DaemonError`] when the daemon is running, a control token already
/// exists, the registry fails, or the secret cannot be delivered.
pub fn run(
    config: &BootstrapConfig,
    stdout: &mut dyn Write,
) -> Result<BootstrapReport, DaemonError> {
    let daemon = instance::prepare_data_dir(config.daemon())?;
    let _lock = instance::InstanceLock::acquire(daemon.data_dir())?;
    let mut registry = RegistryStore::open(daemon.data_dir()).map_err(DaemonError::Registry)?;

    let secret = generate_secret().map_err(|error| DaemonError::Bootstrap(error.to_string()))?;
    let mut output = match config.output() {
        Some(path) => Some(create_secret_file(path)?),
        None => None,
    };
    let verifier = secret.verifier();
    let token_id = uuid::Uuid::now_v7().to_string();
    let now = UtcTimestamp::now();
    let token = NewClientToken {
        token_id: &token_id,
        client_label: config.label(),
        scopes: &BOOTSTRAP_SCOPES,
        verifier_sha256: &verifier,
        created_at: now.as_str(),
    };
    let discarded_pending = match registry.begin_bootstrap_token(&token) {
        Ok(discarded) => discarded,
        Err(error) => {
            if let Some(path) = config.output() {
                let _ = fs::remove_file(path);
            }
            return Err(match error {
                StoreError::ControlTokenExists => DaemonError::Bootstrap(
                    "an active control token already exists; bootstrap only issues the first \
                     one (create more with `memprove token create`)"
                        .to_owned(),
                ),
                other => DaemonError::Registry(other),
            });
        }
    };

    let delivered = match output.as_mut() {
        Some(file) => deliver(file, &secret).and_then(|()| file.sync_all()),
        None => deliver(stdout, &secret),
    };
    drop(secret);
    let finished = UtcTimestamp::now();
    if let Err(error) = delivered {
        if let Some(path) = config.output() {
            let _ = fs::remove_file(path);
        }
        let discard = registry.discard_bootstrap_token(&token_id, finished.as_str());
        return Err(DaemonError::Bootstrap(format!(
            "cannot write the token secret ({error}); the token was {}",
            if discard.is_ok() {
                "discarded"
            } else {
                "left pending and will be discarded by the next bootstrap"
            }
        )));
    }
    registry
        .activate_bootstrap_token(&token_id, finished.as_str())
        .map_err(|error| {
            DaemonError::Bootstrap(format!(
                "the secret was written but the token could not be activated ({error}); it \
                 will not authenticate; run bootstrap again"
            ))
        })?;
    Ok(BootstrapReport {
        token_id,
        client_label: config.label().to_owned(),
        discarded_pending,
    })
}

fn create_secret_file(path: &Path) -> Result<File, DaemonError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| {
            DaemonError::Bootstrap(format!(
                "cannot create token file {} ({error}); it must not already exist",
                path.display()
            ))
        })
}

fn deliver(sink: &mut dyn Write, secret: &Secret) -> io::Result<()> {
    sink.write_all(secret.expose().as_bytes())?;
    sink.write_all(b"\n")?;
    sink.flush()
}
