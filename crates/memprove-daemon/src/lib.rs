//! `memproved`: the single local writer and Local Control API server.
//!
//! It owns the data directory, a single-instance lock, `registry.sqlite`, and
//! HTTP/1.1 over a 0600 Unix socket serving health, status, version, the
//! client-token operations, the Project/Workspace/Task/Session operations,
//! the Event operations, and the Evidence/Checkpoint operations, with graceful
//! `SIGTERM`/`SIGINT` shutdown. Bearer tokens are verified against the
//! registry; `memproved bootstrap` issues the first control token offline.
//! No TCP listener is opened.

#![forbid(unsafe_code)]

#[cfg(test)]
mod access_tests;
pub mod adapters;
pub mod api;
pub mod auth;
pub mod bootstrap;
pub mod catalog;
pub mod config;
pub mod events;
pub mod instance;
mod overruled;
#[cfg(test)]
mod overruled_bound_tests;
pub mod recovery;
#[cfg(test)]
mod recovery_tests;
pub mod socket;
#[cfg(test)]
mod test_support;
pub mod tokens;
pub mod writer;

use std::{
    fmt, io,
    sync::{Arc, Mutex},
};

use memprove_engine::{
    catalog::Catalog, events::EventLedger, recovery::Recovery, runtime::ControlPlane,
};
use memprove_store_sqlite::{RegistryStore, StoreError};
use tokio::signal::unix::{SignalKind, signal};

pub use config::DaemonConfig;

/// U1 (E1): the build identity `build.rs` compiled in: the first 12
/// characters of the commit, with `-dirty` when the worktree had changes,
/// or `unknown` (see `crates/shared/build_id.rs`). The version response
/// carries it as `build` (Owner O8).
pub const BUILD_ID: &str = env!("MEMPROVE_BUILD_ID");

/// U1 (E1): what `memproved --version` prints, without the line break.
pub const VERSION_LINE: &str = concat!(
    "memproved ",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("MEMPROVE_BUILD_ID"),
    ")"
);

/// Fatal daemon failure.
#[derive(Debug)]
pub enum DaemonError {
    /// Data directory or single-instance lock failure.
    Instance(instance::InstanceError),
    /// Socket preparation failure.
    Socket(socket::SocketError),
    /// The registry could not be opened, migrated, or written.
    Registry(StoreError),
    /// Offline bootstrap was refused or could not deliver the secret.
    Bootstrap(String),
    /// Runtime, signal, or serving failure.
    Io {
        /// Failed step.
        action: &'static str,
        /// Underlying error.
        source: io::Error,
    },
}

impl fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Instance(error) => error.fmt(formatter),
            Self::Socket(error) => error.fmt(formatter),
            Self::Registry(error) => write!(formatter, "registry failure: {error}"),
            Self::Bootstrap(reason) => write!(formatter, "bootstrap failed: {reason}"),
            Self::Io { action, source } => write!(formatter, "cannot {action}: {source}"),
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Instance(error) => Some(error),
            Self::Socket(error) => Some(error),
            Self::Registry(error) => Some(error),
            Self::Bootstrap(_) => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

impl From<instance::InstanceError> for DaemonError {
    fn from(error: instance::InstanceError) -> Self {
        Self::Instance(error)
    }
}

impl From<socket::SocketError> for DaemonError {
    fn from(error: socket::SocketError) -> Self {
        Self::Socket(error)
    }
}

fn io_error(action: &'static str) -> impl FnOnce(io::Error) -> DaemonError {
    move |source| DaemonError::Io { action, source }
}

/// Runs the daemon in the foreground until `SIGTERM` or `SIGINT`.
///
/// Order matters: the instance lock is taken before the registry is opened
/// and before the socket path is touched, so a stale socket is only ever
/// replaced by the lock holder and a registry migration failure stops startup
/// before anything is served. On a graceful stop the socket file is removed
/// before the lock is released.
///
/// # Errors
///
/// Returns [`DaemonError`] when the data directory, lock, socket, runtime, or
/// signal handlers cannot be set up, or when serving fails.
pub fn run(config: &DaemonConfig) -> Result<(), DaemonError> {
    let config = instance::prepare_data_dir(config)?;
    let lock = instance::InstanceLock::acquire(config.data_dir())?;
    let registry = RegistryStore::open(config.data_dir()).map_err(DaemonError::Registry)?;
    let registry = Arc::new(Mutex::new(registry));
    let writers = Arc::new(writer::WriterQueue::default());
    let store = Arc::new(
        adapters::StoreCatalog::open(
            config.data_dir(),
            Arc::clone(&registry),
            Arc::clone(&writers),
        )
        .map_err(DaemonError::Registry)?,
    );
    let catalog = Catalog::new(Box::new(Arc::clone(&store)), Box::new(adapters::RepoGit));
    let events = EventLedger::new(Box::new(Arc::clone(&store)));
    let recovery = Recovery::new(Box::new(store));
    let mut bound = socket::bind(&lock, &config.socket_path())?;
    if bound.removed_stale {
        eprintln!(
            "memproved: removed stale socket left by an unclean exit: {}",
            bound.path().display()
        );
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(io_error("start the async runtime"))?;
    let plane = ControlPlane::new(
        Box::new(auth::RegistryAuthenticator::new(Arc::clone(&registry))),
        Box::new(auth::RegistryProjects::new(Arc::clone(&registry))),
        env!("CARGO_PKG_VERSION"),
        api::routed_capabilities(),
    )
    .with_build(BUILD_ID)
    .with_writer_queue(writers);
    let state = Arc::new(api::AppState {
        plane,
        registry,
        catalog,
        events,
        recovery,
    });
    runtime.block_on(serve(&mut bound, state))?;
    drop(runtime);
    drop(bound);
    drop(lock);
    eprintln!("memproved: stopped");
    Ok(())
}

async fn serve(
    bound: &mut socket::BoundSocket,
    state: Arc<api::AppState>,
) -> Result<(), DaemonError> {
    let listener = bound.take_listener();
    listener
        .set_nonblocking(true)
        .map_err(io_error("configure the socket"))?;
    let listener =
        tokio::net::UnixListener::from_std(listener).map_err(io_error("register the socket"))?;
    let mut terminate =
        signal(SignalKind::terminate()).map_err(io_error("install the SIGTERM handler"))?;
    let mut interrupt =
        signal(SignalKind::interrupt()).map_err(io_error("install the SIGINT handler"))?;

    state.plane.readiness().mark_ready();
    eprintln!(
        "memproved: ready (pid {}, socket {})",
        std::process::id(),
        bound.path().display()
    );

    let draining = Arc::clone(&state);
    axum::serve(listener, api::router(state))
        .with_graceful_shutdown(async move {
            let received = tokio::select! {
                _ = terminate.recv() => "SIGTERM",
                _ = interrupt.recv() => "SIGINT",
            };
            draining.plane.readiness().mark_draining();
            eprintln!("memproved: received {received}, shutting down");
        })
        .await
        .map_err(io_error("serve the Local Control API"))
}
