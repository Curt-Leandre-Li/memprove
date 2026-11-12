//! What one tool call resolves before any request (ADR 0014 E7.5.2, E7.8):
//! the server's own context store and context, the directory it works in,
//! the Project of the Workspace root, and a reader with the hook's token
//! and data-directory defaults, restricted to the tool's operations. All of
//! it is read again on every call; the one thing kept between calls is
//! whether the client session from `CLAUDE_CODE_SESSION_ID` was used
//! (U1, U-F, SF-1).

use std::path::PathBuf;

use super::{
    Environment,
    failure::{Failure, Ids},
    tools::Tool,
};
use crate::{
    claims::is_uuid_v7,
    context::{
        Context, ContextLookup, ContextStore, claude_code_session_of, client_session_of,
        workspace_root,
    },
    hook::{self, ChoiceError, Reader},
};

/// U1 (A1): which variable held a value the server ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ignored {
    /// `MEMPROVE_CLIENT_SESSION` (Owner condition j).
    ClientSession,
    /// `CLAUDE_CODE_SESSION_ID` (Owner O2).
    ClaudeCodeSession,
}

/// One tool call's local state.
#[derive(Debug)]
pub(super) struct Local {
    /// The environment read for this call.
    pub(super) environment: Environment,
    /// The configuration directory's store, for the directory's files.
    pub(super) base: Option<ContextStore>,
    /// The server's own store: the client session's files when
    /// `MEMPROVE_CLIENT_SESSION` is valid (E5), else (U1, A1) when it is
    /// not set and `CLAUDE_CODE_SESSION_ID` is valid and (U-F, SF-1) that
    /// client session has records or was used before, else the directory's.
    pub(super) own: Option<ContextStore>,
    /// The directory the server works in.
    pub(super) cwd: Option<PathBuf>,
    /// The server's own context there.
    pub(super) lookup: ContextLookup,
}

impl Local {
    /// Reads the stores and the server's own context for `environment`.
    /// The client session (U1, A1, Owner O2) is `MEMPROVE_CLIENT_SESSION`
    /// when it is set, else `CLAUDE_CODE_SESSION_ID`; a value that is set
    /// but refused is ignored, the directory's context is used, and `warn`
    /// is called with the variable (Owner condition j, O2). A refused
    /// `MEMPROVE_CLIENT_SESSION` is not replaced by
    /// `CLAUDE_CODE_SESSION_ID`: it is set, so it is the one that applies.
    ///
    /// U1 (U-F, SF-1, Owner ruling 2026-10-14): `CLAUDE_CODE_SESSION_ID`
    /// is the client session only when that client session has a context
    /// file or a last-Pack record at or above the directory (the hook
    /// writes one), checked on every call, or when an earlier call of this
    /// process already used it (`adopted`, set here, never cleared);
    /// otherwise the directory's context is used, as before A1, with no
    /// warning.
    pub(super) fn resolve(
        environment: Environment,
        adopted: &mut bool,
        warn: &mut dyn FnMut(Ignored),
    ) -> Self {
        let (client, from_claude_code) = match client_session_of(environment.client_session.clone())
        {
            Ok(Some(client)) => (Some(client), false),
            Ok(None) => (
                claude_code_session_of(environment.claude_code_session.clone()).unwrap_or_else(
                    |_| {
                        warn(Ignored::ClaudeCodeSession);
                        None
                    },
                ),
                true,
            ),
            Err(_) => {
                warn(Ignored::ClientSession);
                (None, false)
            }
        };
        let process = &environment.process;
        let base = ContextStore::from_env(
            process.config_dir.clone(),
            process.xdg_config_home.clone(),
            process.home.clone(),
        );
        let cwd = environment.directory();
        let client = client.filter(|client| {
            !from_claude_code
                || *adopted
                || base
                    .as_ref()
                    .zip(cwd.as_ref())
                    .is_some_and(|(base, cwd)| base.for_client(Some(client)).has_records(cwd))
        });
        if from_claude_code && client.is_some() {
            *adopted = true;
        }
        let own = base
            .as_ref()
            .map(|store| store.for_client(client.as_deref()));
        let lookup = match (&own, &cwd) {
            (Some(store), Some(cwd)) => store.lookup(cwd),
            _ => ContextLookup::Unavailable,
        };
        Self {
            environment,
            base,
            own,
            cwd,
            lookup,
        }
    }

    /// The server's own context, when one applies here.
    pub(super) fn context(&self) -> Option<&Context> {
        self.lookup.context()
    }

    /// The Task and Session of the server's own context: what a result
    /// names when the call concerned no other (Owner condition s).
    pub(super) fn ids(&self) -> Ids {
        Ids {
            task_id: self.context().and_then(|context| context.task_id.clone()),
            session_id: self
                .context()
                .and_then(|context| context.session_id.clone()),
        }
    }

    /// U2 (H5, N-2): whether the server has a client session and the
    /// directory's context covers the directory it works in, so that only
    /// this client session lacks a context (`no_project` says which).
    pub(super) fn directory_context_only(&self) -> bool {
        self.own
            .as_ref()
            .is_some_and(|own| own.client_session.is_some())
            && self
                .base
                .as_ref()
                .zip(self.cwd.as_ref())
                .is_some_and(|(base, cwd)| matches!(base.find(cwd), Ok(Some(_))))
    }

    /// The Workspace root of the directory the server works in.
    pub(super) fn root(&self) -> Option<PathBuf> {
        self.cwd
            .as_ref()
            .map(|cwd| workspace_root(cwd).unwrap_or_else(|_| cwd.clone()))
    }

    /// A reader with the hook's defaults (E7.8), sending only `tool`'s
    /// operations.
    ///
    /// # Errors
    ///
    /// Which default is missing or unusable.
    pub(super) fn reader(&self, tool: &Tool) -> Result<Reader, Failure> {
        let base = self.base.as_ref().ok_or(Failure::NoConfigDir)?;
        Reader::new(&self.environment.process, base)
            .map(|reader| reader.only(tool.operations))
            .map_err(Failure::Reach)
    }

    /// The Project of the Workspace root (E7.5.2): the one every context
    /// file of the root records, else the server's own last-Pack record
    /// (U1, U-F, SF-1R: of a Pack or of no Pack).
    ///
    /// # Errors
    ///
    /// When none or several are recorded, or the ID is not a `UUIDv7`.
    pub(super) fn project(&self) -> Result<String, Failure> {
        let base = self.base.as_ref().ok_or(Failure::NoConfigDir)?;
        let root = self.root().ok_or(Failure::NoContext)?;
        let project = match hook::workspace_of(base, &root) {
            Ok((project, _)) => project,
            Err(ChoiceError::NoContext) => self
                .own
                .as_ref()
                .and_then(|own| own.recorded_project(&root))
                // No context of the root at all (U2, H5).
                .ok_or(Failure::NoProject {
                    directory_context: false,
                })?,
            Err(_) => return Err(Failure::AmbiguousWorkspace),
        };
        if is_uuid_v7(&project) {
            Ok(project)
        } else {
            Err(Failure::ContextUnusable)
        }
    }
}

/// The failure, and the Task it concerns, when Owner condition m chose no
/// Task (E7.5.2: the hook's statement becomes the tool error). U1 (A6): a
/// finished Task is `task_finished` with a sentence that names the Task's
/// state, not `begin`'s sentence about Sessions.
pub(super) fn choice_failure(error: ChoiceError, ids: Ids) -> (Ids, Failure) {
    let finished = |finished: hook::Finished, client: bool| {
        (
            Ids {
                task_id: Some(finished.task_id),
                session_id: None,
            },
            Failure::ChosenTaskFinished {
                state: finished.state,
                client,
            },
        )
    };
    match error {
        ChoiceError::NoContext => (ids, Failure::NoContext),
        ChoiceError::SeveralWorkspaces => (ids, Failure::AmbiguousWorkspace),
        ChoiceError::NotUuid => (ids, Failure::ContextUnusable),
        ChoiceError::Reach(error) => (ids, Failure::Reach(error)),
        ChoiceError::Read(error) => (ids, Failure::Read(error)),
        ChoiceError::ClientTaskFinished(task) => finished(task, true),
        ChoiceError::NoneInProgress(Some(task)) => finished(task, false),
        ChoiceError::NoneInProgress(None) => (ids, Failure::NoTask),
        ChoiceError::Several(_, tasks) => (ids, Failure::AmbiguousTask(tasks.len())),
        ChoiceError::TooMany(_, count) => (ids, Failure::TooManyTasks(count)),
    }
}
