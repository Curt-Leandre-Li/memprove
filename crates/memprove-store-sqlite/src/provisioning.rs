use std::{fmt::Write as _, path::Path};

use memprove_domain::{
    IdempotencyContext, Project, ProjectId, ProjectProvisioningState, Sha256Digest, Workspace,
};

use crate::{Idempotent, RegistryStore, StateStore, StoreError, vfs::CapabilityRoot};

/// Recoverable registry-to-state project provisioning saga.
pub struct ProjectProvisioner {
    registry: RegistryStore,
    capability_root: CapabilityRoot,
    #[cfg(test)]
    crash_point: Option<CrashPoint>,
}

/// Where a test stops the saga as if the process had died: no rollback runs.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CrashPoint {
    /// After the registry `PROVISIONING` row, before the state database.
    BeforeStateDatabase,
    /// After the root Workspace, before the Project becomes `ACTIVE`.
    BeforeActivation,
}

impl ProjectProvisioner {
    /// Opens the global registry used by the saga.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the registry cannot be opened or migrated.
    pub fn open(data_dir: &Path) -> Result<Self, StoreError> {
        let capability_root = CapabilityRoot::open(data_dir)?;
        Ok(Self {
            registry: RegistryStore::open_with_root(capability_root.clone())?,
            capability_root,
            #[cfg(test)]
            crash_point: None,
        })
    }

    /// Executes Step A, Step B, close/reopen verification, and Step C.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when any durable saga step fails.
    pub fn provision(
        &mut self,
        proposed: &Project,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Project, StoreError> {
        self.provision_inner(proposed, None, context, request_digest)
    }

    /// Like [`Self::provision`], but also writes the root Workspace into the
    /// state database before the Project becomes `ACTIVE`, so an `ACTIVE`
    /// Project always has its root. `root.project_id` is replaced by the
    /// provisioned Project's identity. Repeating the call after a crash
    /// completes the registration: a crash after the root and before
    /// activation is resumed and activated; a crash before the state database
    /// exists is rolled back without residue (the saga's designed recovery),
    /// so that call fails and the next one provisions afresh. An `ACTIVE`
    /// Project registered by an older build without a root gets one.
    ///
    /// Returns the `ACTIVE` Project and its root Workspace (the first
    /// Workspace registered, which may predate this call).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when any durable saga step fails.
    pub fn provision_with_root(
        &mut self,
        proposed: &Project,
        root: &Workspace,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<(Project, Workspace), StoreError> {
        let project = self.provision_inner(proposed, Some(root), context, request_digest)?;
        // T21 (V1-R-2): the Project is ACTIVE here, so its state database
        // was created while it was PROVISIONING; a registration repeated
        // after that database was deleted or emptied does not create a new,
        // empty ledger.
        let mut state =
            StateStore::open_existing_with_root(self.capability_root.clone(), &project)?;
        let root = state.ensure_root_workspace(&Workspace {
            project_id: project.project_id.clone(),
            ..root.clone()
        })?;
        Ok((project, root))
    }

    fn provision_inner(
        &mut self,
        proposed: &Project,
        root: Option<&Workspace>,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Project, StoreError> {
        self.capability_root
            .verify_identity()
            .map_err(provisioning_root_error)?;
        match self
            .registry
            .register_project_begin(proposed, context, request_digest)
            .map_err(provisioning_root_error)?
        {
            Idempotent::Created(project) => self.initialize_and_finalize(&project, root),
            Idempotent::Replayed(project) => match project.provisioning_state_internal {
                ProjectProvisioningState::Active => {
                    self.capability_root
                        .verify_identity()
                        .map_err(provisioning_root_error)?;
                    Ok(project)
                }
                ProjectProvisioningState::Failed => Err(StoreError::ProvisioningFailed(
                    "registry Project is already FAILED".to_owned(),
                )),
                ProjectProvisioningState::Provisioning => {
                    self.resume_with(&project.project_id, root)
                }
            },
        }
    }

    /// Reconciles one project found in `PROVISIONING` after restart.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the project or state binding cannot be recovered.
    pub fn resume(&mut self, project_id: &ProjectId) -> Result<Project, StoreError> {
        self.resume_with(project_id, None)
    }

    /// Resumes a `PROVISIONING` Project: verifies its state database (a
    /// missing one rolls the Project back), writes `root` if given and
    /// absent, then activates the Project.
    fn resume_with(
        &mut self,
        project_id: &ProjectId,
        root: Option<&Workspace>,
    ) -> Result<Project, StoreError> {
        let project = self.registry.get_project(project_id)?;
        match project.provisioning_state_internal {
            ProjectProvisioningState::Active => return Ok(project),
            ProjectProvisioningState::Failed => {
                return Err(StoreError::ProvisioningFailed(
                    "registry Project is already FAILED".to_owned(),
                ));
            }
            ProjectProvisioningState::Provisioning => {}
        }
        let state_exists = match StateStore::exists_with_root(&self.capability_root, &project) {
            Ok(exists) => exists,
            Err(error) => return self.rollback_incomplete(&project, &error, true),
        };
        if !state_exists {
            return self.rollback_incomplete(
                &project,
                &StoreError::ProvisioningFailed(
                    "state database is missing after registry begin".to_owned(),
                ),
                false,
            );
        }
        if let Err(error) = verify_state_binding_twice(&self.capability_root, &project)
            .and_then(|()| self.write_root(&project, root))
        {
            return self.rollback_incomplete(&project, &error, true);
        }
        #[cfg(test)]
        if self.crash_point == Some(CrashPoint::BeforeActivation) {
            return Err(injected_crash());
        }
        match self.registry.finalize_project(project_id) {
            Ok(active) => match self.capability_root.verify_identity() {
                Ok(()) => Ok(active),
                Err(error) => self.rollback_incomplete(&project, &error, true),
            },
            Err(error) => self.rollback_incomplete(&project, &error, true),
        }
    }

    /// Reads one registry Project.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the project cannot be read.
    pub fn get_project(&self, project_id: &ProjectId) -> Result<Project, StoreError> {
        self.registry.get_project(project_id)
    }

    /// Returns the durable Project count.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the registry query fails.
    pub fn project_count(&self) -> Result<u64, StoreError> {
        self.registry.project_count()
    }

    /// Returns the durable registry idempotency state for a Project.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record cannot be read.
    pub fn idempotency_state_for_project(
        &self,
        project_id: &ProjectId,
    ) -> Result<String, StoreError> {
        self.registry.idempotency_state_for_project(project_id)
    }

    /// Returns the durable registry idempotency state for an exact subject namespace.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record cannot be read.
    pub fn idempotency_state(&self, context: &IdempotencyContext) -> Result<String, StoreError> {
        self.registry.idempotency_state(context)
    }

    /// Returns the data directory used by the registry.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        self.capability_root.display_path()
    }

    fn initialize_and_finalize(
        &mut self,
        project: &Project,
        root: Option<&Workspace>,
    ) -> Result<Project, StoreError> {
        #[cfg(test)]
        if self.crash_point == Some(CrashPoint::BeforeStateDatabase) {
            return Err(injected_crash());
        }
        let state_existed = match StateStore::exists_with_root(&self.capability_root, project) {
            Ok(existed) => existed,
            Err(error) => return self.rollback_created(project, &error, None),
        };
        if let Err(error) = verify_state_binding_twice(&self.capability_root, project)
            .and_then(|()| self.write_root(project, root))
        {
            let cleanup_error = if state_existed {
                None
            } else {
                StateStore::cleanup_new_database_with_root(&self.capability_root, project).err()
            };
            return self.rollback_created(project, &error, cleanup_error);
        }
        #[cfg(test)]
        if self.crash_point == Some(CrashPoint::BeforeActivation) {
            return Err(injected_crash());
        }
        match self.registry.finalize_project(&project.project_id) {
            Ok(active) => match self.capability_root.verify_identity() {
                Ok(()) => Ok(active),
                Err(error) => self.rollback_incomplete(project, &error, true),
            },
            Err(error) => self.rollback_incomplete(project, &error, true),
        }
    }

    /// Writes the root Workspace of a still-`PROVISIONING` Project unless
    /// the state database already has one.
    fn write_root(&self, project: &Project, root: Option<&Workspace>) -> Result<(), StoreError> {
        let Some(root) = root else {
            return Ok(());
        };
        let mut state = StateStore::open_with_root(self.capability_root.clone(), project)?;
        state.ensure_root_workspace(&Workspace {
            project_id: project.project_id.clone(),
            ..root.clone()
        })?;
        Ok(())
    }

    fn rollback_created<T>(
        &mut self,
        project: &Project,
        cause: &StoreError,
        cleanup_error: Option<StoreError>,
    ) -> Result<T, StoreError> {
        let rollback_error = self
            .registry
            .rollback_project_creation(&project.project_id)
            .err();
        let mut reason = cause.to_string();
        if let Some(error) = cleanup_error {
            write!(&mut reason, "; state cleanup failed: {error}")
                .expect("writing to a String cannot fail");
        }
        if let Some(error) = rollback_error {
            write!(&mut reason, "; registry rollback failed: {error}")
                .expect("writing to a String cannot fail");
        }
        Err(StoreError::ProvisioningFailed(reason))
    }

    fn rollback_incomplete<T>(
        &mut self,
        project: &Project,
        cause: &StoreError,
        cleanup_state: bool,
    ) -> Result<T, StoreError> {
        let cleanup_error = cleanup_state
            .then(|| {
                StateStore::cleanup_new_database_with_root(&self.capability_root, project).err()
            })
            .flatten();
        self.rollback_created(project, cause, cleanup_error)
    }
}

fn verify_state_binding_twice(
    capability_root: &CapabilityRoot,
    project: &Project,
) -> Result<(), StoreError> {
    let state = StateStore::open_with_root(capability_root.clone(), project)?;
    drop(state);
    let reopened = StateStore::open_with_root(capability_root.clone(), project)?;
    drop(reopened);
    Ok(())
}

#[cfg(test)]
fn injected_crash() -> StoreError {
    StoreError::ProvisioningFailed("injected crash".to_owned())
}

fn provisioning_root_error(error: StoreError) -> StoreError {
    match error {
        StoreError::CapabilityVfs(message) => {
            StoreError::ProvisioningFailed(format!("capability VFS failure: {message}"))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink, sync::Arc, thread};

    use memprove_domain::{
        IdempotencyKey, OperationId, PrincipalId, ProjectProvisioningState, UtcTimestamp,
    };
    use std::path::Path;
    use tempfile::TempDir;

    use super::*;
    use crate::vfs::CapabilityRoot;

    #[test]
    fn injected_state_vfs_failure_leaves_no_registry_row_or_partial_database() {
        for failure_suffix in ["", "-journal"] {
            let directory = TempDir::new().expect("approved data root");
            let project_id = ProjectId::new();
            let relative_state = format!("projects/{}/state.sqlite", project_id.as_str());
            let project = Project {
                project_id: project_id.clone(),
                repository_common_dir: directory
                    .path()
                    .join("repository.git")
                    .display()
                    .to_string(),
                object_format: "sha1".to_owned(),
                state_db_path: directory.path().join(&relative_state).display().to_string(),
                provisioning_state_internal: ProjectProvisioningState::Provisioning,
                created_at: UtcTimestamp::parse("2026-07-31T01:00:00Z").expect("created_at"),
                updated_at: UtcTimestamp::parse("2026-07-31T01:00:00Z").expect("updated_at"),
                revision: 1,
            };
            let context = IdempotencyContext {
                principal_id: PrincipalId::parse("test-principal").expect("principal"),
                operation_id: OperationId::parse("registerProject").expect("operation"),
                key: IdempotencyKey::parse("vfs-failure").expect("key"),
            };
            let mut provisioner = ProjectProvisioner::open(directory.path()).expect("provisioner");
            let root = CapabilityRoot::open(directory.path()).expect("same capability root");
            root.fail_next_open(&format!("{relative_state}{failure_suffix}"));

            let outcome =
                provisioner.provision(&project, &context, &Sha256Digest::of_bytes(b"request"));
            assert!(
                matches!(&outcome, Err(StoreError::ProvisioningFailed(_))),
                "failure suffix {failure_suffix:?}: {outcome:?}"
            );
            assert_eq!(provisioner.project_count().expect("registry row count"), 0);
            assert!(matches!(
                provisioner.idempotency_state(&context),
                Err(StoreError::NotFound("registry idempotency record"))
            ));
            for suffix in ["", "-journal", "-wal", "-shm"] {
                assert!(
                    !root
                        .exists(&format!("{relative_state}{suffix}"))
                        .expect("database family absence"),
                    "failure suffix {failure_suffix:?}, leftover {suffix:?}"
                );
            }
            assert!(
                !directory
                    .path()
                    .join("projects")
                    .join(project_id.as_str())
                    .exists(),
                "failure suffix {failure_suffix:?} left a partial project directory"
            );
        }
    }

    #[test]
    fn root_swap_between_identity_verification_and_state_creation_rolls_back_everything() {
        let container = TempDir::new().expect("container");
        let approved = container.path().join("approved");
        let original = container.path().join("approved-original");
        fs::create_dir(&approved).expect("approved root");
        let outside = TempDir::new().expect("outside root");
        let project_id = ProjectId::new();
        let relative_state = format!("projects/{}/state.sqlite", project_id.as_str());
        let proposed = Project {
            project_id: project_id.clone(),
            repository_common_dir: approved.join("repository.git").display().to_string(),
            object_format: "sha1".to_owned(),
            state_db_path: approved.join(&relative_state).display().to_string(),
            provisioning_state_internal: ProjectProvisioningState::Provisioning,
            created_at: UtcTimestamp::parse("2026-07-31T01:00:00Z").expect("created_at"),
            updated_at: UtcTimestamp::parse("2026-07-31T01:00:00Z").expect("updated_at"),
            revision: 1,
        };
        let context = IdempotencyContext {
            principal_id: PrincipalId::parse("test-principal").expect("principal"),
            operation_id: OperationId::parse("registerProject").expect("operation"),
            key: IdempotencyKey::parse("root-identity-race").expect("key"),
        };
        let mut provisioner = ProjectProvisioner::open(&approved).expect("provisioner");
        let root = provisioner.capability_root.clone();
        let reached = Arc::new(std::sync::Barrier::new(2));
        let resume = Arc::new(std::sync::Barrier::new(2));
        root.install_root_identity_race_hook(Arc::clone(&reached), Arc::clone(&resume));

        let worker = thread::spawn(move || {
            let result = provisioner.provision(
                &proposed,
                &context,
                &Sha256Digest::of_bytes(b"root identity race"),
            );
            (provisioner, proposed, context, result)
        });
        reached.wait();
        fs::rename(&approved, &original).expect("move approved root after identity verification");
        symlink(outside.path(), &approved).expect("replace approved root with outside symlink");
        resume.wait();

        let (provisioner, proposed, context, result) = worker.join().expect("provision worker");
        assert!(
            matches!(result, Err(StoreError::ProvisioningFailed(_))),
            "identity race was accepted: {result:?}"
        );
        assert_eq!(provisioner.project_count().expect("Project count"), 0);
        assert!(matches!(
            provisioner.idempotency_state(&context),
            Err(StoreError::NotFound("registry idempotency record"))
        ));
        for suffix in ["", "-journal", "-wal", "-shm"] {
            assert!(
                !root
                    .exists(&format!("{relative_state}{suffix}"))
                    .expect("original root residue check"),
                "original root retained state family suffix {suffix:?}"
            );
            assert!(
                !outside
                    .path()
                    .join("projects")
                    .join(proposed.project_id.as_str())
                    .join(format!("state.sqlite{suffix}"))
                    .exists(),
                "outside root received state family suffix {suffix:?}"
            );
        }
        assert!(
            !original
                .join("projects")
                .join(proposed.project_id.as_str())
                .exists(),
            "original capability retained a partial project directory"
        );
    }

    fn proposed_with_root(directory: &Path) -> (Project, Workspace) {
        let project_id = ProjectId::new();
        let now = UtcTimestamp::parse("2026-10-02T01:00:00Z").expect("timestamp");
        let project = Project {
            project_id: project_id.clone(),
            repository_common_dir: directory.join("repository.git").display().to_string(),
            object_format: "sha1".to_owned(),
            state_db_path: directory
                .join(format!("projects/{}/state.sqlite", project_id.as_str()))
                .display()
                .to_string(),
            provisioning_state_internal: ProjectProvisioningState::Provisioning,
            created_at: now.clone(),
            updated_at: now.clone(),
            revision: 1,
        };
        let root = Workspace {
            workspace_id: memprove_domain::WorkspaceId::new(),
            project_id,
            canonical_worktree_path: directory.join("repository").display().to_string(),
            git_dir: directory.join("repository.git").display().to_string(),
            head_commit: "0".repeat(40),
            branch_or_detached_state: "branch:main".to_owned(),
            created_at: now,
            revision: 1,
        };
        (project, root)
    }

    fn register_context(key: &str) -> IdempotencyContext {
        IdempotencyContext {
            principal_id: PrincipalId::parse("test-principal").expect("principal"),
            operation_id: OperationId::parse("registerProject").expect("operation"),
            key: IdempotencyKey::parse(key).expect("key"),
        }
    }

    #[test]
    fn a_crash_between_steps_is_recovered_by_registering_again() {
        for crash in [
            CrashPoint::BeforeStateDatabase,
            CrashPoint::BeforeActivation,
        ] {
            let directory = TempDir::new().expect("data root");
            let (proposed, root) = proposed_with_root(directory.path());
            let digest = Sha256Digest::of_bytes(b"register");
            let mut crashed = ProjectProvisioner::open(directory.path()).expect("provisioner");
            crashed.crash_point = Some(crash);
            assert!(matches!(
                crashed.provision_with_root(&proposed, &root, &register_context("first-key"), &digest),
                Err(StoreError::ProvisioningFailed(reason)) if reason == "injected crash"
            ));
            // The process "died": no rollback ran, the Project is not ACTIVE,
            // and no reader can see it.
            drop(crashed);
            let mut restarted = ProjectProvisioner::open(directory.path()).expect("provisioner");
            let stored = restarted
                .get_project(&proposed.project_id)
                .expect("registry row");
            assert_eq!(
                stored.provisioning_state_internal,
                ProjectProvisioningState::Provisioning,
                "{crash:?}"
            );
            let root_before = (crash == CrashPoint::BeforeActivation).then(|| {
                StateStore::open_with_root(restarted.capability_root.clone(), &stored)
                    .and_then(|state| state.root_workspace())
                    .expect("root written before activation")
                    .expect("root present")
            });

            // A later registration (another key, a fresh proposal) completes it.
            let (again, other_root) = proposed_with_root(directory.path());
            let again = Project {
                repository_common_dir: proposed.repository_common_dir.clone(),
                ..again
            };
            if crash == CrashPoint::BeforeStateDatabase {
                // The saga rolls a Project without a state database back,
                // leaving no residue; the next registration starts afresh.
                assert!(matches!(
                    restarted.provision_with_root(
                        &again,
                        &other_root,
                        &register_context("second-key"),
                        &digest
                    ),
                    Err(StoreError::ProvisioningFailed(_))
                ));
                assert_eq!(restarted.project_count().expect("count"), 0);
            }
            let (project, registered_root) = restarted
                .provision_with_root(&again, &other_root, &register_context("third-key"), &digest)
                .expect("completed registration");
            if crash == CrashPoint::BeforeActivation {
                assert_eq!(
                    project.project_id, proposed.project_id,
                    "resumed, not replaced"
                );
            }
            assert_eq!(
                project.provisioning_state_internal,
                ProjectProvisioningState::Active
            );
            assert_eq!(registered_root.project_id, project.project_id);
            if let Some(root_before) = root_before {
                assert_eq!(registered_root, root_before, "the first root is kept");
            }
            let state = StateStore::open_with_root(restarted.capability_root.clone(), &project)
                .expect("state");
            assert_eq!(state.root_workspace().expect("root"), Some(registered_root));
        }
    }

    #[test]
    fn an_active_project_without_a_root_gets_one_on_registration() {
        let directory = TempDir::new().expect("data root");
        let (proposed, root) = proposed_with_root(directory.path());
        let digest = Sha256Digest::of_bytes(b"register");
        let mut provisioner = ProjectProvisioner::open(directory.path()).expect("provisioner");
        // An older build activated the Project before writing its root.
        let legacy = provisioner
            .provision(&proposed, &register_context("legacy-key"), &digest)
            .expect("legacy provisioning");
        let state = StateStore::open_with_root(provisioner.capability_root.clone(), &legacy)
            .expect("state");
        assert_eq!(state.root_workspace().expect("root query"), None);
        drop(state);
        let (project, registered_root) = provisioner
            .provision_with_root(&proposed, &root, &register_context("legacy-key"), &digest)
            .expect("repair");
        assert_eq!(project.project_id, legacy.project_id);
        assert_eq!(registered_root.workspace_id, root.workspace_id);
    }
}
