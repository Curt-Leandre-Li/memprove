//! P4-G (ADR 0014 E7.15): the write flows `memprove` and `memprove mcp
//! serve` share, moved out of the CLI's `main.rs` with their behaviour
//! unchanged: `begin`'s requests in order, the resolution of a drafted
//! request through the Task Capsule, and the read-back after a Checkpoint.
//!
//! A flow reaches `memproved` through a [`Daemon`]: the CLI's
//! [`Socket`] reports a failed exchange on its standard streams as it always
//! did, the MCP server keeps it for a tool error. What a flow notes on the
//! way, refuses, or produces is data; each front end renders it, the CLI
//! with the same lines in the same order as before.

use std::{
    fmt::{self, Write as _},
    path::Path,
};

use memprove_protocol::{
    phase1::OperationId,
    strict_json::{JsonValue, parse_strict},
};

use crate::{
    BeginSpec, BeginTask, CallError, CapsuleView, Reply, Request, begin_steps,
    claims::{self, CapsuleError, CheckpointReport, ClaimError, ReaffirmAllReport},
    context::{Context, ContextError, ContextLookup, ContextStore, LastPack},
    generate_idempotency_key, json_string, previous_session_check, resume,
};

/// How a flow reaches `memproved`.
pub trait Daemon {
    /// How an exchange failed, as the front end keeps it.
    type Failed;
    /// How a quiet request failed; the port reports nothing itself.
    type Unread: fmt::Display;

    /// Sends `request`; a failure is reported the front end's way.
    ///
    /// # Errors
    ///
    /// A Problem response, or a transport, token, or contract failure.
    fn exchange(&self, request: &Request) -> Result<Reply, Self::Failed>;

    /// Sends `request` quietly; a Problem response is an error too.
    ///
    /// # Errors
    ///
    /// Why no successful response was read.
    fn read(&self, request: &Request) -> Result<Reply, Self::Unread>;

    /// The error of a successful response the flow cannot use, and why.
    fn malformed(operation: OperationId, why: String) -> Self::Unread;

    /// The Problem code of a failed exchange, when the daemon refused it.
    fn problem_code(failed: &Self::Failed) -> Option<String>;
}

/// Why a CLI exchange did not produce a success, already reported on
/// standard error (a Problem body on standard output).
#[derive(Debug)]
pub enum Failed {
    /// A Problem response (exit 1).
    Problem(Reply),
    /// Transport, token, contract, or local failure, with its exit code.
    Other(u8),
}

impl Failed {
    /// The CLI's exit code.
    #[must_use]
    pub const fn code(&self) -> u8 {
        match self {
            Self::Problem(_) => 1,
            Self::Other(code) => *code,
        }
    }
}

/// The exit code when a response is larger than the CLI reads for its
/// operation (P3-C2, T12 S-5): not `4`, the daemon kept the contract.
pub const RESPONSE_TOO_LARGE: u8 = 6;

/// The CLI's way to `memproved`: the socket and the token.
#[derive(Debug, Clone, Copy)]
pub struct Socket<'a> {
    /// The socket.
    pub socket: &'a Path,
    /// The token, when there is one.
    pub token: Option<&'a str>,
}

impl Daemon for Socket<'_> {
    type Failed = Failed;
    type Unread = String;

    /// Sends `request` and reports any failure on standard error. A Problem
    /// body is printed on standard output, as for every command.
    fn exchange(&self, request: &Request) -> Result<Reply, Failed> {
        let operation = request.operation;
        match crate::call(self.socket, request, self.token) {
            Ok(reply) if reply.is_success() => Ok(reply),
            Ok(reply) => {
                println!("{}", reply.body);
                eprintln!("{}", reply.failure_line(operation));
                Err(Failed::Problem(reply))
            }
            Err(error @ CallError::Transport { .. }) => {
                eprintln!("memprove: error: {error}");
                if let Some(key) = &request.idempotency_key {
                    eprintln!("memprove: retry with --idempotency-key {key} to avoid a duplicate");
                }
                Err(Failed::Other(3))
            }
            Err(error @ CallError::InvalidToken) => {
                eprintln!("memprove: error: {error}");
                Err(Failed::Other(2))
            }
            Err(error @ CallError::Contract(_)) => {
                eprintln!("memprove: error: {error}");
                Err(Failed::Other(4))
            }
            Err(error @ CallError::ResponseTooLarge { .. }) => {
                eprintln!("memprove: error: {error}");
                Err(Failed::Other(RESPONSE_TOO_LARGE))
            }
        }
    }

    fn read(&self, request: &Request) -> Result<Reply, String> {
        let reply =
            crate::call(self.socket, request, self.token).map_err(|error| error.to_string())?;
        if reply.is_success() {
            Ok(reply)
        } else {
            Err(format!(
                "{} answered {} {}",
                request.operation,
                reply.status,
                reply.problem_code().unwrap_or_default()
            ))
        }
    }

    fn malformed(_: OperationId, why: String) -> String {
        why
    }

    fn problem_code(failed: &Failed) -> Option<String> {
        match failed {
            Failed::Problem(reply) => reply.problem_code(),
            Failed::Other(_) => None,
        }
    }
}

/// Gives `request` a random `Idempotency-Key` when it needs one and has none.
///
/// # Errors
///
/// When no key can be generated.
pub fn keyed(mut request: Request) -> Result<Request, String> {
    if request.needs_idempotency_key() && request.idempotency_key.is_none() {
        request.idempotency_key = Some(
            generate_idempotency_key()
                .map_err(|error| format!("cannot generate an Idempotency-Key: {error}"))?,
        );
    }
    Ok(request)
}

/// A string member of a JSON body.
#[must_use]
pub fn member(body: &str, path: &[&str]) -> Option<String> {
    let value = parse_strict(body).ok()?;
    path.iter()
        .try_fold(&value, |value, name| value.get(name))
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

/// A non-negative integer member of a JSON body.
#[must_use]
pub fn number(body: &str, name: &str) -> Option<u64> {
    parse_strict(body)
        .ok()?
        .get(name)
        .and_then(JsonValue::as_i64)
        .and_then(|value| u64::try_from(value).ok())
}

/// Where the context of the current directory is written back.
#[derive(Debug, Clone, Copy)]
pub struct Saver<'a> {
    /// The context store.
    pub store: Option<&'a ContextStore>,
    /// The context of the current directory.
    pub lookup: &'a ContextLookup,
}

impl Saver<'_> {
    /// P4-E: the Session the context names, and whether that context is
    /// the client session's own file.
    #[must_use]
    pub fn current_session(&self) -> Option<resume::CurrentSession<'_>> {
        let session_id = self.lookup.context()?.session_id.as_deref()?;
        Some(resume::CurrentSession {
            session_id,
            client: self
                .store
                .is_some_and(|store| store.client_session.is_some()),
        })
    }

    /// Applies `change` to the current context and writes it, when there is
    /// one and `change` reports a change. A failure is reported, not fatal:
    /// the daemon has already recorded the operation.
    pub fn update(&self, change: impl FnOnce(&mut Context) -> bool) {
        let (Some(store), Some(context)) = (self.store, self.lookup.context()) else {
            return;
        };
        let mut context = context.clone();
        if change(&mut context)
            && let Err(error) = store.save(&context)
        {
            eprintln!("memprove: warning: the current context was not updated: {error}");
        }
    }
}

/// The item of a list operation whose `id_field` is `id`, read page by
/// page from `base` (a target with its subject query). `Ok(None)` when it is
/// not listed.
///
/// # Errors
///
/// Why the pages could not be read.
pub fn listed<D: Daemon>(
    daemon: &D,
    operation: OperationId,
    base: &str,
    id_field: &str,
    id: &str,
) -> Result<Option<JsonValue>, D::Unread> {
    let mut cursor: Option<String> = None;
    for _ in 0..100 {
        let mut target = format!("{base}&limit=500");
        if let Some(cursor) = &cursor {
            target.push_str("&cursor=");
            target.push_str(cursor);
        }
        let reply = daemon.read(&Request::bare(operation, target))?;
        let page = parse_strict(&reply.body)
            .map_err(|error| D::malformed(operation, error.to_string()))?;
        let items = page
            .get("items")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| D::malformed(operation, format!("{operation} returned no items")))?;
        if let Some(item) = items
            .iter()
            .find(|item| item.get(id_field).and_then(JsonValue::as_str) == Some(id))
        {
            return Ok(Some(item.clone()));
        }
        match page.get("next_cursor").and_then(JsonValue::as_str) {
            Some(next) => cursor = Some(next.to_owned()),
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// U10 T7 (Owner O4): the Workspace every context of the Workspace root of
/// `cwd` records (the directory's and each client session's), when they
/// agree, as the `SessionStart` hook finds it; `None` otherwise. `session
/// end <ID> --abandoned` without `--revision` lists that Workspace's
/// Sessions when the current context names none.
#[must_use]
pub fn workspace_of_root(
    store: &crate::context::ContextStore,
    cwd: &std::path::Path,
) -> Option<String> {
    let root = crate::context::workspace_root(cwd).ok()?;
    crate::hook::workspace_of(store, &root)
        .ok()
        .map(|(_, workspace)| workspace)
}

/// U11 N4 (U10 VN14; Owner O6): the Project every context of the Workspace
/// root of `cwd` records (the directory's and each client session's), when
/// they agree; `None` otherwise. `task list` without `--project` takes it
/// when the current context names none.
#[must_use]
pub fn project_of_root(
    store: &crate::context::ContextStore,
    cwd: &std::path::Path,
) -> Option<String> {
    let root = crate::context::workspace_root(cwd).ok()?;
    crate::hook::workspace_of(store, &root)
        .ok()
        .map(|(project, _)| project)
}

/// U3 (K1, W-B-1, Owner O3): the state of the Session `session_id` as
/// `listSessions` of the Workspace `workspace_id` lists it (`active`,
/// `ended`, or `crashed`); `Ok(None)` when it is not listed. `task
/// complete` and the MCP `complete` tool read it once before they send
/// `transitionTask`, and send nothing unless it is `active`.
///
/// # Errors
///
/// Why the listing could not be read.
pub fn session_state<D: Daemon>(
    daemon: &D,
    workspace_id: &str,
    session_id: &str,
) -> Result<Option<String>, D::Unread> {
    Ok(listed(
        daemon,
        OperationId::ListSessions,
        &format!("/v1/sessions?workspace_id={workspace_id}"),
        "session_id",
        session_id,
    )?
    .map(|session| {
        session
            .get("state")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_owned()
    }))
}

/// What `begin` knows of a Task: title, revision, state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskDetails {
    title: Option<String>,
    revision: Option<u64>,
    state: Option<String>,
}

impl TaskDetails {
    fn of(item: &JsonValue) -> Self {
        let text = |name: &str| {
            item.get(name)
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        };
        Self {
            title: text("title"),
            revision: item
                .get("revision")
                .and_then(JsonValue::as_i64)
                .and_then(|value| u64::try_from(value).ok()),
            state: text("state"),
        }
    }
}

/// What `begin` is asked to do, resolved by its front end.
#[derive(Debug, Clone, Copy)]
pub struct BeginRequest<'a> {
    /// The Project, the Task, and the agent.
    pub spec: &'a BeginSpec,
    /// The Workspace root.
    pub root: &'a Path,
    /// The Workspace root as UTF-8.
    pub root_text: &'a str,
    /// The store whose last-Pack record of `root` is declared.
    pub store: &'a ContextStore,
    /// The Session the context still names in this Workspace root, and its
    /// Workspace (P2-H: it must have ended).
    pub previous: Option<(&'a str, &'a str)>,
}

/// What `begin` notes on its way; [`BeginNote::line`] is the CLI's line.
#[derive(Debug)]
pub enum BeginNote<'a> {
    /// The context's Session is not listed in its Workspace; it is replaced.
    PreviousNotListed(&'a str),
    /// The Workspace root is not inside a Workspace of the Project.
    NotInWorkspace,
    /// The Task could not be read through `listTasks`.
    TaskUnread(&'a str),
    /// The last-Pack record could not be read.
    LastPackUnreadable(&'a ContextError),
    /// The Session declares the Pack it resumes from.
    Resumed(&'a LastPack),
    /// The last-Pack record was not removed.
    PackKept(&'a ContextError),
}

impl BeginNote<'_> {
    /// Whether the line is a warning (`memprove: warning: …`).
    #[must_use]
    pub const fn is_warning(&self) -> bool {
        !matches!(self, Self::NotInWorkspace | Self::Resumed(_))
    }

    /// The line the CLI prints on standard error.
    #[must_use]
    pub fn line(&self, request: &BeginRequest<'_>) -> String {
        match self {
            Self::PreviousNotListed(previous) => format!(
                "memprove: warning: Session {previous} of the current context is not listed \
                 in its Workspace; it is replaced"
            ),
            Self::NotInWorkspace => format!(
                "memprove: {} is not inside a Workspace of Project {}",
                request.root.display(),
                request.spec.project_id
            ),
            Self::TaskUnread(task_id) => format!(
                "memprove: warning: Task {task_id} could not be read through listTasks; its \
                 state is unknown and it is not moved to in_progress"
            ),
            Self::LastPackUnreadable(error) => format!(
                "memprove: warning: {error}; the Session does not declare the Pack it resumes \
                 from"
            ),
            Self::Resumed(pack) => format!(
                "memprove: the Session declares that it resumes from the Pack of ledger \
                 sequence {} (pack_digest sha256:{})",
                pack.resumed_from.project_sequence, pack.resumed_from.pack_digest
            ),
            Self::PackKept(error) => {
                format!("memprove: warning: the recorded Pack was not removed: {error}")
            }
        }
    }
}

/// A local failure of a flow, between its requests (the CLI's exit 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalFailure {
    /// An `Idempotency-Key` or agent id could not be generated: why.
    Generate(String),
    /// A successful response lacks a member the flow needs.
    Missing(OperationId, &'static str),
}

impl fmt::Display for LocalFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Generate(why) => formatter.write_str(why),
            Self::Missing(operation, member) => {
                write!(formatter, "{operation} returned no {member}")
            }
        }
    }
}

/// Why `begin` started no Session.
#[derive(Debug)]
pub enum BeginStop<D: Daemon> {
    /// A local failure (the CLI's exit 2).
    Local(LocalFailure),
    /// The context's Session is still active: the refusal (exit 1).
    Active(String),
    /// Whether the context's Session ended could not be read: the refusal
    /// and why (exit 1).
    Unconfirmed(String, D::Unread),
    /// The Task is completed or cancelled: the refusal and the state (exit
    /// 1).
    Finished(String, &'static str),
    /// An exchange failed; the port reported it.
    Failed(D::Failed),
}

/// Why `begin` did not move the Task to `in_progress` after the Session
/// started.
#[derive(Debug)]
pub enum Unmoved<F> {
    /// No `Idempotency-Key` could be generated: why (the CLI's exit 2).
    Generate(String),
    /// The transition failed; the port reported it.
    Failed(F),
}

/// The Session `begin` started, and the context to record.
#[derive(Debug)]
pub struct Begun<F> {
    /// The context of the new Session.
    pub context: Context,
    /// The last Pack the Session declared as `resumed_from`.
    pub resumed: Option<LastPack>,
    /// Whether a transition moved the Task.
    pub moved: bool,
    /// Whether every transition `begin` needed was made.
    pub advanced: Result<(), Unmoved<F>>,
}

/// A request with a body.
fn post(operation: OperationId, target: &str, body: String) -> Result<Request, LocalFailure> {
    let mut request = Request::bare(operation, target.to_owned());
    request.body = Some(body);
    keyed(request).map_err(LocalFailure::Generate)
}

/// `memprove begin` after its local checks: refuses while the context's
/// Session is still active, resolves the Workspace, creates the Task when
/// asked (refusing a finished one), creates a Session continuing it
/// (declaring the last Pack of the same Task, P4-F), and moves the Task to
/// `in_progress`. Nothing is recorded locally: the front end records the
/// returned context.
///
/// # Errors
///
/// Why no Session was started.
pub fn begin<D: Daemon>(
    daemon: &D,
    request: &BeginRequest<'_>,
    notes: &mut dyn FnMut(BeginNote<'_>),
) -> Result<Begun<D::Failed>, BeginStop<D>> {
    check_previous(daemon, request, notes)?;
    let workspace_id = resolve_workspace(daemon, request, notes)?;
    let (task_id, mut task) = begin_task(daemon, request.spec, notes)?;
    // P2-H: a finished Task takes no new Session.
    let steps = begin_steps(&task_id, task.state.as_deref()).map_err(|refusal| {
        let state = if task.state.as_deref() == Some("completed") {
            "completed"
        } else {
            "cancelled"
        };
        BeginStop::Finished(refusal, state)
    })?;
    let agent_id = match request.spec.agent_id.clone() {
        Some(agent_id) => agent_id,
        None => crate::run::uuid_v7().map_err(|error| {
            BeginStop::Local(LocalFailure::Generate(format!(
                "cannot generate an agent id: {error}"
            )))
        })?,
    };
    let (session_id, session_revision, resumed) =
        create_session(daemon, request, &workspace_id, &agent_id, &task_id, notes)?;
    let (moved, advanced) = advance(daemon, &task_id, steps, &mut task);
    Ok(Begun {
        context: Context {
            workspace_root: request.root.to_path_buf(),
            project_id: request.spec.project_id.clone(),
            workspace_id,
            agent_id,
            task_id: Some(task_id),
            task_title: task.title,
            task_revision: task.revision,
            task_state: task.state,
            session_id: Some(session_id),
            session_revision,
            snapshot_id: None,
            snapshot_manifest: None,
            evidence_id: None,
            evidence_status: None,
            runs: Vec::new(),
            next_run: 1,
            // U8 A3 (Owner O4): the context remembers the agent declared.
            agent_name: request.spec.agent.as_ref().map(|agent| agent.name.clone()),
            agent_role: request
                .spec
                .agent
                .as_ref()
                .map(|agent| agent.role.as_str().to_owned()),
            parent_session_id: request
                .spec
                .agent
                .as_ref()
                .and_then(|agent| agent.parent_session_id.as_ref())
                .map(ToString::to_string),
        },
        resumed,
        moved,
        advanced,
    })
}

/// P2-H: a Session the context still names must have ended; it is not
/// ended for the caller.
fn check_previous<D: Daemon>(
    daemon: &D,
    request: &BeginRequest<'_>,
    notes: &mut dyn FnMut(BeginNote<'_>),
) -> Result<(), BeginStop<D>> {
    let Some((previous, workspace_id)) = request.previous else {
        return Ok(());
    };
    let found = listed(
        daemon,
        OperationId::ListSessions,
        &format!("/v1/sessions?workspace_id={workspace_id}"),
        "session_id",
        previous,
    );
    match found {
        Ok(Some(session)) => {
            let state = session
                .get("state")
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            previous_session_check(previous, Ok(Some(state))).map_err(BeginStop::Active)
        }
        Ok(None) => {
            notes(BeginNote::PreviousNotListed(previous));
            Ok(())
        }
        Err(unread) => {
            let refusal = previous_session_check(previous, Err(unread.to_string()))
                .err()
                .unwrap_or_default();
            Err(BeginStop::Unconfirmed(refusal, unread))
        }
    }
}

/// `resolveWorkspace` of the Workspace root in the Project.
fn resolve_workspace<D: Daemon>(
    daemon: &D,
    request: &BeginRequest<'_>,
    notes: &mut dyn FnMut(BeginNote<'_>),
) -> Result<String, BeginStop<D>> {
    let mut resolve = Request::bare(
        OperationId::ResolveWorkspace,
        "/v1/workspaces/resolve".to_owned(),
    );
    resolve.body = Some(format!(
        r#"{{"schema_version":1,"project_id":{},"path":{}}}"#,
        json_string(&request.spec.project_id),
        json_string(request.root_text)
    ));
    let workspace = daemon.exchange(&resolve).map_err(|failed| {
        if D::problem_code(&failed).as_deref() == Some("workspace_not_resolved") {
            notes(BeginNote::NotInWorkspace);
        }
        BeginStop::Failed(failed)
    })?;
    member(&workspace.body, &["workspace_id"]).ok_or(BeginStop::Local(LocalFailure::Missing(
        OperationId::ResolveWorkspace,
        "workspace_id",
    )))
}

/// The Task `begin` continues: a new one, or an existing one read through
/// `listTasks` (unread, `createSession` still checks it).
fn begin_task<D: Daemon>(
    daemon: &D,
    spec: &BeginSpec,
    notes: &mut dyn FnMut(BeginNote<'_>),
) -> Result<(String, TaskDetails), BeginStop<D>> {
    match &spec.task {
        BeginTask::New { title, description } => {
            let mut body = format!(
                r#"{{"schema_version":1,"project_id":{},"title":{}"#,
                json_string(&spec.project_id),
                json_string(title)
            );
            if let Some(description) = description {
                write!(body, r#","description":{}"#, json_string(description))
                    .expect("writing to a String cannot fail");
            }
            body.push('}');
            let request =
                post(OperationId::CreateTask, "/v1/tasks", body).map_err(BeginStop::Local)?;
            let reply = daemon.exchange(&request).map_err(BeginStop::Failed)?;
            let task_id = member(&reply.body, &["task_id"]).ok_or(BeginStop::Local(
                LocalFailure::Missing(OperationId::CreateTask, "task_id"),
            ))?;
            let mut details =
                TaskDetails::of(&parse_strict(&reply.body).unwrap_or(JsonValue::Null));
            details.title = Some(title.clone());
            Ok((task_id, details))
        }
        BeginTask::Existing(task_id) => {
            let item = listed(
                daemon,
                OperationId::ListTasks,
                &format!("/v1/tasks?project_id={}", spec.project_id),
                "task_id",
                task_id,
            );
            let details = if let Ok(Some(item)) = item {
                TaskDetails::of(&item)
            } else {
                notes(BeginNote::TaskUnread(task_id));
                TaskDetails {
                    title: None,
                    revision: None,
                    state: None,
                }
            };
            Ok((task_id.clone(), details))
        }
    }
}

/// `createSession`, declaring the last Pack of the same Task (P4-F, ADR
/// 0014 E6, Owner condition i), which is then forgotten: a later Session
/// does not declare a Pack it was not given. Returns the Session, its
/// revision, and the Pack declared.
fn create_session<D: Daemon>(
    daemon: &D,
    request: &BeginRequest<'_>,
    workspace_id: &str,
    agent_id: &str,
    task_id: &str,
    notes: &mut dyn FnMut(BeginNote<'_>),
) -> Result<(String, Option<u64>, Option<LastPack>), BeginStop<D>> {
    let resumed = match request.store.last_pack(request.root) {
        Ok(Some(pack)) if pack.task_id == task_id => Some(pack),
        Ok(_) => None,
        Err(error) => {
            notes(BeginNote::LastPackUnreadable(&error));
            None
        }
    };
    let session = post(
        OperationId::CreateSession,
        "/v1/sessions",
        format!(
            r#"{{"schema_version":1,"workspace_id":{},"agent_id":{},"task_id":{}{}{}}}"#,
            json_string(workspace_id),
            json_string(agent_id),
            json_string(task_id),
            resumed.as_ref().map_or_else(String::new, |pack| format!(
                r#","resumed_from":{}"#,
                pack.resumed_from_json()
            )),
            // U8 A3 (Owner O4): only when the Session declares an agent.
            crate::agent_member(request.spec.agent.as_ref()),
        ),
    )
    .map_err(BeginStop::Local)?;
    let session = daemon.exchange(&session).map_err(BeginStop::Failed)?;
    let session_id = member(&session.body, &["session_id"]).ok_or(BeginStop::Local(
        LocalFailure::Missing(OperationId::CreateSession, "session_id"),
    ))?;
    if let Some(pack) = &resumed {
        notes(BeginNote::Resumed(pack));
        if let Err(error) = request.store.forget_last_pack(request.root) {
            notes(BeginNote::PackKept(&error));
        }
    }
    Ok((session_id, number(&session.body, "revision"), resumed))
}

/// P2-H: moves a proposed or ready Task to `in_progress`, one edge at a
/// time; stops at the first failure. Returns whether a transition was made
/// and whether all were.
fn advance<D: Daemon>(
    daemon: &D,
    task_id: &str,
    steps: &[&str],
    task: &mut TaskDetails,
) -> (bool, Result<(), Unmoved<D::Failed>>) {
    let mut moved = false;
    for next in steps {
        let Some(revision) = task.revision else {
            break;
        };
        let request = match post(
            OperationId::TransitionTask,
            &format!("/v1/tasks/{task_id}/transition"),
            format!(
                r#"{{"schema_version":1,"expected_revision":{revision},"target_state":{}}}"#,
                json_string(next)
            ),
        ) {
            Ok(request) => request,
            Err(error) => return (moved, Err(Unmoved::Generate(error.to_string()))),
        };
        match daemon.exchange(&request) {
            Ok(reply) => {
                moved = true;
                task.revision = number(&reply.body, "revision");
                task.state = member(&reply.body, &["state"]);
            }
            Err(failed) => return (moved, Err(Unmoved::Failed(failed))),
        }
    }
    (moved, Ok(()))
}

/// What resolving a draft found out besides the body: which facts of the
/// request are reaffirmations (P3-L L8), and what `--reaffirm-all` did
/// (P4-A).
#[derive(Debug, Default)]
pub struct Resolved {
    /// For each fact of the request, whether it is a reaffirmation.
    pub reaffirmed: Vec<bool>,
    /// What `--reaffirm-all` did, when the draft had one.
    pub reaffirm_all: Option<ReaffirmAllReport>,
}

/// Why a drafted request was not sent.
#[derive(Debug)]
pub enum DraftStop<D: Daemon> {
    /// The Task Capsule could not be read; the port reported the exchange.
    Unread(D::Failed),
    /// The Capsule cannot resolve claim numbers.
    Capsule(CapsuleError),
    /// A reference does not resolve.
    Claim(ClaimError),
    /// `--reaffirm-all` found nothing to reaffirm and the request records
    /// nothing else: its report.
    Nothing(Box<ReaffirmAllReport>),
}

/// Builds the body of a drafted request (P3-C): claim numbers,
/// `--reaffirm`, and `--reaffirm-all` resolve through the Task Capsule read
/// now. P4-A: when `--reaffirm-all` leaves nothing to record, nothing is
/// sent. `warn` gets each warning of `--reaffirm-all --evidence` before the
/// body is built.
///
/// # Errors
///
/// Why nothing is sent.
pub fn resolve_draft<D: Daemon>(
    daemon: &D,
    request: &mut Request,
    warn: &mut dyn FnMut(&str),
) -> Result<Resolved, DraftStop<D>> {
    let Some(mut draft) = request.pending.take() else {
        return Ok(Resolved::default());
    };
    let mut target = format!("/v1/tasks/{}/recovery-capsule", draft.task_id());
    // P4-A: --reaffirm-all compares runs with the state of this Workspace.
    if let Some(workspace) = draft.probe_workspace() {
        write!(target, "?workspace_id={workspace}").expect("writing to a String cannot fail");
    }
    let capsule = Request {
        operation: OperationId::GetTaskCapsule,
        target,
        body: None,
        idempotency_key: None,
        summary: false,
        all_items: false,
        capsule_view: CapsuleView::Plain,
        pending: None,
    };
    let reply = daemon.exchange(&capsule).map_err(DraftStop::Unread)?;
    let claims = claims::CapsuleClaims::from_body(&reply.body).map_err(DraftStop::Capsule)?;
    let reaffirm_all = draft
        .expand_reaffirm_all(&claims)
        .map_err(DraftStop::Claim)?;
    // P4-A judgment 8, Owner supplement of P4-B: a --next that differs
    // from the next action in force is recorded all the same.
    if let Some(report) = &reaffirm_all
        && !draft.records_anything()
        && !draft.next_differs(&claims)
    {
        return Err(DraftStop::Nothing(Box::new(report.clone())));
    }
    // P4-B (T18 N-a): with --evidence, facts reaffirmed on runs of other
    // commands than their Evidence ran; the request is sent as given.
    for warning in reaffirm_all.iter().flat_map(|report| &report.warnings) {
        warn(warning);
    }
    request.body = Some(draft.render(Some(&claims)).map_err(DraftStop::Claim)?);
    Ok(Resolved {
        reaffirmed: draft.reaffirmed_facts(),
        reaffirm_all,
    })
}

/// What reading the Task Capsule after a Checkpoint found (P3-L L8).
#[derive(Debug)]
pub struct ReadBack<U> {
    /// The Checkpoint recorded.
    pub checkpoint_id: String,
    /// The Task Capsule after the write, or why it was not read.
    pub capsule: Result<String, U>,
    /// What the Checkpoint did, or why that is not known.
    pub report: Result<CheckpointReport, String>,
}

/// P3-L L8: after a Checkpoint was recorded (`reply`, from the request
/// `body`), reads the Task Capsule and what the Checkpoint did. `None` when
/// the reply names no Checkpoint or Task.
pub fn read_back<D: Daemon>(
    daemon: &D,
    body: &str,
    reply: &Reply,
    reaffirmed: &[bool],
) -> Option<ReadBack<D::Unread>> {
    let (Some(checkpoint_id), Some(task_id)) = (
        member(&reply.body, &["checkpoint_id"]),
        member(&reply.body, &["task_id"]),
    ) else {
        return None;
    };
    let capsule = daemon
        .read(&Request::bare(
            OperationId::GetTaskCapsule,
            format!("/v1/tasks/{task_id}/recovery-capsule"),
        ))
        .map(|read| read.body);
    let report = match &capsule {
        Ok(capsule) => CheckpointReport::read(capsule, body, &checkpoint_id, reaffirmed),
        Err(error) => Err(error.to_string()),
    };
    Some(ReadBack {
        checkpoint_id,
        capsule,
        report,
    })
}
