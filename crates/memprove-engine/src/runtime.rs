//! Minimal runtime services behind the Local Control API health surface.
//!
//! Authentication, the Project count, and the writer-queue depth are ports
//! implemented by the daemon. Without a writer-queue port the depth is 0.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Process readiness shared between the lifecycle owner and request handlers.
#[derive(Debug, Default)]
pub struct Readiness(AtomicBool);

impl Readiness {
    /// Creates a not-ready flag.
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Marks the runtime ready to serve protected operations.
    pub fn mark_ready(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Marks the runtime as draining; protected operations become not ready.
    pub fn mark_draining(&self) {
        self.0.store(false, Ordering::SeqCst);
    }

    /// Reports whether the runtime currently accepts protected operations.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// An authenticated client and the scopes granted to its token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    id: String,
    scopes: Vec<String>,
}

impl Principal {
    /// Creates a principal with stable identity `id` holding exactly `scopes`.
    #[must_use]
    pub fn new(id: String, scopes: Vec<String>) -> Self {
        Self { id, scopes }
    }

    /// Stable identity of the authenticated client (its token id).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Reports whether the principal holds `scope`.
    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|held| held == scope)
    }
}

/// A backing store the runtime depends on cannot be read; protected
/// operations fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unavailable;

/// Why a bearer credential did not yield a [`Principal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationError {
    /// Missing, unknown, pending, discarded, or revoked credential. The cause
    /// is deliberately not distinguished.
    Rejected,
    /// The credential store cannot be read.
    Unavailable,
}

/// Port that turns a bearer credential into a [`Principal`].
pub trait Authenticator: Send + Sync {
    /// Authenticates the bearer credential, if any was presented.
    ///
    /// # Errors
    ///
    /// Returns [`AuthenticationError::Rejected`] for a missing or unusable
    /// credential and [`AuthenticationError::Unavailable`] when the credential
    /// store cannot be consulted.
    fn authenticate(&self, bearer: Option<&str>) -> Result<Principal, AuthenticationError>;
}

/// Port reporting the number of registered Projects.
pub trait ProjectCounter: Send + Sync {
    /// Counts registered Projects.
    ///
    /// # Errors
    ///
    /// Returns [`Unavailable`] when the registry cannot be read.
    fn project_count(&self) -> Result<u64, Unavailable>;
}

/// Port reporting how many mutations currently hold or wait for a Project
/// writer lane.
pub trait WriterQueueDepth: Send + Sync {
    /// Mutations admitted to a writer lane and not yet finished.
    fn depth(&self) -> u64;
}

/// Runtime status without secrets or absolute paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeStatus {
    /// Whether protected operations are currently served.
    pub ready: bool,
    /// Mutations holding or waiting for a Project writer lane.
    pub writer_queue_depth: u64,
    /// Registered Projects.
    pub project_count: u64,
}

/// Version and contract capabilities served by this binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    /// Application version string.
    pub application_version: &'static str,
    /// Major version of the Local Control API.
    pub api_major: u32,
    /// Capability identifiers of the routed operations.
    pub capabilities: Vec<&'static str>,
    /// U1 (E1, Owner O8): the build identity of the binary, when it reports
    /// one: `<12 hex>[-dirty]` or `unknown`.
    pub build: Option<&'static str>,
}

/// Local Control API major version served by this crate.
pub const API_MAJOR: u32 = 1;

/// Application service backing the health, status, and version operations.
pub struct ControlPlane {
    readiness: Readiness,
    authenticator: Box<dyn Authenticator>,
    projects: Box<dyn ProjectCounter>,
    writer_queue: Option<Arc<dyn WriterQueueDepth>>,
    application_version: &'static str,
    capabilities: Vec<&'static str>,
    build: Option<&'static str>,
}

impl ControlPlane {
    /// Creates a not-yet-ready control plane.
    #[must_use]
    pub fn new(
        authenticator: Box<dyn Authenticator>,
        projects: Box<dyn ProjectCounter>,
        application_version: &'static str,
        capabilities: Vec<&'static str>,
    ) -> Self {
        Self {
            readiness: Readiness::new(),
            authenticator,
            projects,
            writer_queue: None,
            application_version,
            capabilities,
            build: None,
        }
    }

    /// U1 (E1, Owner O8): reports `build` as the binary's build identity in
    /// the version response.
    #[must_use]
    pub fn with_build(mut self, build: &'static str) -> Self {
        self.build = Some(build);
        self
    }

    /// Reports `writer_queue_depth` from `queue`.
    #[must_use]
    pub fn with_writer_queue(mut self, queue: Arc<dyn WriterQueueDepth>) -> Self {
        self.writer_queue = Some(queue);
        self
    }

    /// Shared readiness flag.
    #[must_use]
    pub fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// Authenticates a bearer credential through the installed port.
    ///
    /// # Errors
    ///
    /// Propagates [`AuthenticationError`] from the authenticator.
    pub fn authenticate(&self, bearer: Option<&str>) -> Result<Principal, AuthenticationError> {
        self.authenticator.authenticate(bearer)
    }

    /// Current runtime status with the registry's Project count.
    ///
    /// # Errors
    ///
    /// Returns [`Unavailable`] when the Project count cannot be read.
    pub fn status(&self) -> Result<RuntimeStatus, Unavailable> {
        Ok(RuntimeStatus {
            ready: self.readiness.is_ready(),
            writer_queue_depth: self.writer_queue.as_ref().map_or(0, |queue| queue.depth()),
            project_count: self.projects.project_count()?,
        })
    }

    /// Version and capabilities of this binary.
    #[must_use]
    pub fn version(&self) -> VersionInfo {
        VersionInfo {
            application_version: self.application_version,
            api_major: API_MAJOR,
            capabilities: self.capabilities.clone(),
            build: self.build,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoCredentials;

    impl Authenticator for NoCredentials {
        fn authenticate(&self, _bearer: Option<&str>) -> Result<Principal, AuthenticationError> {
            Err(AuthenticationError::Rejected)
        }
    }

    struct FixedProjects(Result<u64, Unavailable>);

    impl ProjectCounter for FixedProjects {
        fn project_count(&self) -> Result<u64, Unavailable> {
            self.0
        }
    }

    #[test]
    fn readiness_transitions() {
        let readiness = Readiness::new();
        assert!(!readiness.is_ready());
        readiness.mark_ready();
        assert!(readiness.is_ready());
        readiness.mark_draining();
        assert!(!readiness.is_ready());
    }

    #[test]
    fn principal_reports_identity_and_scopes() {
        let principal = Principal::new("t1".to_owned(), vec!["control".to_owned()]);
        assert_eq!(principal.id(), "t1");
        assert!(principal.has_scope("control"));
        assert!(!principal.has_scope("mcp-read"));
    }

    #[test]
    fn status_tracks_readiness_and_reports_the_project_count() {
        let plane = ControlPlane::new(
            Box::new(NoCredentials),
            Box::new(FixedProjects(Ok(3))),
            "0.1.0",
            vec!["healthLive"],
        );
        assert_eq!(plane.authenticate(None), Err(AuthenticationError::Rejected));
        assert!(!plane.status().unwrap().ready);
        plane.readiness().mark_ready();
        assert_eq!(
            plane.status(),
            Ok(RuntimeStatus {
                ready: true,
                writer_queue_depth: 0,
                project_count: 3
            })
        );
        let version = plane.version();
        assert_eq!(version.api_major, 1);
        assert_eq!(version.capabilities, vec!["healthLive"]);
    }

    #[test]
    fn status_fails_closed_when_the_registry_is_unavailable() {
        let plane = ControlPlane::new(
            Box::new(NoCredentials),
            Box::new(FixedProjects(Err(Unavailable))),
            "0.1.0",
            vec![],
        );
        assert_eq!(plane.status(), Err(Unavailable));
    }

    struct FixedDepth(u64);

    impl WriterQueueDepth for FixedDepth {
        fn depth(&self) -> u64 {
            self.0
        }
    }

    #[test]
    fn status_reports_the_writer_queue_port() {
        let plane = ControlPlane::new(
            Box::new(NoCredentials),
            Box::new(FixedProjects(Ok(1))),
            "0.1.0",
            vec![],
        )
        .with_writer_queue(Arc::new(FixedDepth(2)));
        assert_eq!(plane.status().unwrap().writer_queue_depth, 2);
    }
}
