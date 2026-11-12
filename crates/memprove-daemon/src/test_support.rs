//! In-process fixtures shared by the daemon's unit tests.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use memprove_engine::{
    catalog::{Catalog, GitResolver},
    events::EventLedger,
    recovery::Recovery,
    runtime::ControlPlane,
};
use memprove_protocol::strict_json::{JsonValue, parse_strict};
use memprove_store_sqlite::{NewClientToken, RegistryStore};
use serde_json::json;

use crate::{
    adapters::{RepoGit, StoreCatalog},
    api::{AppState, WireResponse, routed_capabilities},
    auth::{RegistryAuthenticator, RegistryProjects, generate_secret},
    tokens::{TokenRequest, create_client_token, list_client_tokens, revoke_client_token},
    writer::WriterQueue,
};

/// An `AppState` over a temporary data directory.
pub(crate) struct Harness {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) state: AppState,
    pub(crate) writers: Arc<WriterQueue>,
    keys: AtomicU32,
}

impl Harness {
    pub(crate) fn new(ready: bool) -> Self {
        Self::with_git(ready, Box::new(RepoGit))
    }

    pub(crate) fn with_git(ready: bool, git: Box<dyn GitResolver>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = fs::canonicalize(dir.path()).unwrap();
        let registry = Arc::new(Mutex::new(RegistryStore::open(&data_dir).unwrap()));
        let writers = Arc::new(WriterQueue::default());
        let plane = ControlPlane::new(
            Box::new(RegistryAuthenticator::new(Arc::clone(&registry))),
            Box::new(RegistryProjects::new(Arc::clone(&registry))),
            "0.1.0",
            routed_capabilities(),
        )
        .with_writer_queue(Arc::clone(&writers) as _);
        if ready {
            plane.readiness().mark_ready();
        }
        let store = Arc::new(
            StoreCatalog::open(&data_dir, Arc::clone(&registry), Arc::clone(&writers)).unwrap(),
        );
        Self {
            dir,
            state: AppState {
                plane,
                registry,
                catalog: Catalog::new(Box::new(Arc::clone(&store)), git),
                events: EventLedger::new(Box::new(Arc::clone(&store))),
                recovery: Recovery::new(Box::new(store)),
            },
            writers,
            keys: AtomicU32::new(0),
        }
    }

    /// Splits the fixture into its data directory guard and state.
    pub(crate) fn into_parts(self) -> (tempfile::TempDir, AppState) {
        (self.dir, self.state)
    }

    /// Issues an active bootstrap control token the way `memproved
    /// bootstrap` does and returns its secret.
    pub(crate) fn bootstrap(&self) -> String {
        let secret = generate_secret().unwrap();
        let token_id = uuid::Uuid::now_v7().to_string();
        let verifier = secret.verifier();
        let mut registry = self.state.registry.lock().unwrap();
        registry
            .begin_bootstrap_token(&NewClientToken {
                token_id: &token_id,
                client_label: "bootstrap-control",
                scopes: &["control"],
                verifier_sha256: &verifier,
                created_at: "2026-10-02T00:00:00Z",
            })
            .unwrap();
        registry
            .activate_bootstrap_token(&token_id, "2026-10-02T00:00:00Z")
            .unwrap();
        secret.expose().to_owned()
    }

    pub(crate) fn fresh_key(&self) -> String {
        let key = self.keys.fetch_add(1, Ordering::SeqCst) + 1;
        format!("test-key-{key:08}")
    }

    pub(crate) fn create_with(
        &self,
        bearer: Option<&str>,
        key: Option<&str>,
        body: &str,
    ) -> WireResponse {
        create_client_token(&self.state, &json_request(bearer, key, body))
    }

    pub(crate) fn create(&self, bearer: &str, label: &str, scopes: &[&str]) -> WireResponse {
        let body = json!({ "schema_version": 1, "client_label": label, "scopes": scopes });
        self.create_with(Some(bearer), Some(&self.fresh_key()), &body.to_string())
    }

    /// Creates a token and returns `(token_id, secret)`.
    pub(crate) fn issue(&self, bearer: &str, label: &str, scopes: &[&str]) -> (String, String) {
        let response = self.create(bearer, label, scopes);
        assert_eq!(response.status, 201, "{}", response.body);
        let body = parse_strict(&response.body).unwrap();
        (field(&body, "token_id"), field(&body, "secret"))
    }

    pub(crate) fn list(&self, bearer: Option<&str>, query: Option<&str>) -> WireResponse {
        list_client_tokens(&self.state, bearer, query)
    }

    pub(crate) fn revoke_with(
        &self,
        bearer: Option<&str>,
        key: Option<&str>,
        target: &str,
    ) -> WireResponse {
        revoke_client_token(
            &self.state,
            &TokenRequest {
                bearer,
                idempotency_key: key,
                content_type: None,
                body: Some(b""),
            },
            target,
        )
    }

    pub(crate) fn revoke(&self, bearer: &str, target: &str) -> WireResponse {
        self.revoke_with(Some(bearer), Some(&self.fresh_key()), target)
    }

    pub(crate) fn id_of(&self, secret: &str) -> String {
        let registry = self.state.registry.lock().unwrap();
        registry
            .authenticate_token(&crate::auth::verifier(secret))
            .unwrap()
            .unwrap()
            .token_id
    }
}

/// A JSON mutation request.
pub(crate) fn json_request<'a>(
    bearer: Option<&'a str>,
    key: Option<&'a str>,
    body: &'a str,
) -> TokenRequest<'a> {
    TokenRequest {
        bearer,
        idempotency_key: key,
        content_type: Some("application/json"),
        body: Some(body.as_bytes()),
    }
}

/// A string member of a JSON object.
pub(crate) fn field(body: &JsonValue, name: &str) -> String {
    body.get(name)
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned()
}

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Adds a linked worktree of `repository` on a new branch at `path` with the
/// real `git worktree add`, and returns its canonical path.
pub(crate) fn git_worktree(repository: &Path, path: &Path, branch: &str) -> PathBuf {
    git(
        repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            path.to_str().unwrap(),
        ],
    );
    fs::canonicalize(path).unwrap()
}

/// Creates a committed Git repository under `parent/name` and returns its
/// canonical path.
pub(crate) fn git_repository(parent: &Path, name: &str) -> PathBuf {
    let repository = parent.join(name);
    fs::create_dir_all(&repository).unwrap();
    git(&repository, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repository.join("README"), b"fixture\n").unwrap();
    git(&repository, &["add", "README"]);
    git(
        &repository,
        &[
            "-c",
            "user.name=memprove Test",
            "-c",
            "user.email=memprove@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
    fs::canonicalize(repository).unwrap()
}
