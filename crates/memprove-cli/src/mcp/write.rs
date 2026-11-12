//! The five write tools (ADR 0014 E7.5.6): `begin`, `checkpoint`, `decide`,
//! `end`, and `complete`, each as the CLI command of the same name does it,
//! through the flows the CLI shares ([`crate::flow`]). Which Session a
//! write concerns follows rule D (E7.5.2): the server's own context when it
//! names a Session, else the one context file of the Workspace root that
//! does; `session` names a Session of the Pack explicitly. Parameters are
//! numbers and labels only; no tool runs a command (Owner condition t).
//! Results name their Task and Session first (condition s); every sentence
//! the server writes is fixed (condition u).

use std::path::PathBuf;

use memprove_protocol::{
    phase1::OperationId,
    strict_json::{JsonValue, parse_strict},
};

use super::{
    MCP_PACK_DEFAULT_BUDGET,
    failure::{Failure, Ids, Internal, ToolResult, failed, succeeded},
    local::{Local, choice_failure},
    read::{CONTEXT_RUNS_SHOWN, shown_context},
    tools::Tool,
    wire::{number, object, optional, text, texts},
};
use crate::{
    BeginSpec, BeginTask, LAST_EVIDENCE, Reply, Request,
    claims::is_uuid_v7,
    claims::{
        self, CapsuleError, CheckpointDraft, ClaimError, ClaimRef, Draft, ReaffirmAllReport,
        StatementDraft, StatementKind,
    },
    completion_body,
    context::{Context, ContextStore, workspace_root},
    default_completion, end_body_by, ender_of,
    flow::{self, BeginRequest, BeginStop, Daemon, DraftStop, LocalFailure, Unmoved},
    hook::{self, ReadError, ReadFailure, Reader},
};

/// Why a write tool did not succeed.
enum Stop {
    /// A tool error concerning `Ids`.
    Tool(Ids, Failure),
    /// A programming error or a local fault: `-32603`.
    Internal,
}

fn finish(result: Result<ToolResult, Stop>) -> Result<ToolResult, Internal> {
    match result {
        Ok(result) => Ok(result),
        Err(Stop::Tool(ids, failure)) => failed(&ids, &failure),
        Err(Stop::Internal) => Err(Internal),
    }
}

/// A tool error about `ids`.
fn stop(ids: &Ids) -> impl Fn(Failure) -> Stop + '_ {
    move |failure| Stop::Tool(ids.clone(), failure)
}

/// The server's way to `memproved`: the reader of the hook's defaults,
/// restricted to the tool's operations. Nothing is printed; a failure is
/// kept for the tool error.
struct Port<'a>(&'a Reader);

impl Daemon for Port<'_> {
    type Failed = ReadError;
    type Unread = ReadError;

    fn exchange(&self, request: &Request) -> Result<Reply, ReadError> {
        self.0.send(request)
    }

    fn read(&self, request: &Request) -> Result<Reply, ReadError> {
        self.0.send(request)
    }

    fn malformed(operation: OperationId, _: String) -> ReadError {
        bad_response(operation)
    }

    fn problem_code(failed: &ReadError) -> Option<String> {
        match &failed.failure {
            ReadFailure::Refused { code, .. } => code.clone(),
            _ => None,
        }
    }
}

/// A response the server cannot use.
const fn bad_response(operation: OperationId) -> ReadError {
    ReadError {
        operation,
        failure: ReadFailure::BadResponse,
    }
}

/// A context a write uses, and the file it was read from.
#[derive(Debug, Clone)]
struct ContextFile {
    path: PathBuf,
    context: Context,
}

impl ContextFile {
    /// Writes the context back after `change`, when it changed (as the
    /// CLI's `Saver` does for its own file). Whether it was written; a
    /// failure is a warning on standard error.
    fn update(&self, base: &ContextStore, change: impl FnOnce(&mut Context) -> bool) -> bool {
        let mut context = self.context.clone();
        if !change(&mut context) {
            return false;
        }
        match base.save_at(self.path.clone(), &context) {
            Ok(_) => true,
            Err(error) => {
                crate::log_line(format_args!(
                    "memprove: warning: the current context was not updated: {error}"
                ));
                false
            }
        }
    }
}

/// The Session a write concerns: the Task, the Session, and the context
/// file that names it (none when `session` named a Session no context of
/// the root records).
struct Target {
    ids: Ids,
    session_id: String,
    workspace_id: String,
    file: Option<ContextFile>,
}

impl Target {
    /// The context file that names the Session; a write that needs its
    /// runs, Snapshot, or revisions fails without one.
    fn file(&self) -> Result<&ContextFile, Stop> {
        self.file
            .as_ref()
            .ok_or_else(|| Stop::Tool(self.ids.clone(), Failure::NoSessionContext))
    }
}

/// Which context files rule D may take a Session from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// `checkpoint` and `decide`: any context file of the Workspace root.
    Any,
    /// U1 (A2, Owner O3): `end` and `complete`, which write the file back:
    /// the server's own file, or the directory's while no client session's
    /// file of the root names a Session; never another client session's.
    OwnFile,
}

/// Rule D (ADR 0014 E7.5.2): the server's own context when it names a
/// Session, else the one context file of the Workspace root that does.
/// U1 (A2, Owner O3): for `end` and `complete` ([`Scope::OwnFile`]) step 2
/// never takes another client session's file. While a client session's
/// file of the root names a Session (the hook's layout), a server with a
/// client session whose own context names none gets
/// `no_session_context`, and a server with no client session gets
/// `no_client_session`: which client session it serves is not known, and
/// it does not guess. Without such a file (one context file per root, no
/// hook) the directory's file is taken as before.
fn rule_d(local: &Local, tool: &Tool, scope: Scope) -> Result<Target, Stop> {
    let ids = local.ids();
    let fail = stop(&ids);
    let base = local
        .base
        .as_ref()
        .ok_or_else(|| fail(Failure::NoConfigDir))?;
    let own = local
        .own
        .as_ref()
        .ok_or_else(|| fail(Failure::NoConfigDir))?;
    let file = if let Some(context) = local
        .context()
        .filter(|context| context.session_id.is_some())
    {
        ContextFile {
            path: own.path_for(&context.workspace_root),
            context: context.clone(),
        }
    } else {
        let root = local
            .root()
            .ok_or_else(|| fail(Failure::NoSessionContext))?;
        let mut named = live(
            local,
            tool,
            base.context_files_of(&root)
                .into_iter()
                .filter(|(_, context)| context.session_id.is_some())
                .collect(),
        );
        if scope == Scope::OwnFile {
            let directory = base.for_client(None).path_for(&root);
            let own_path = own.path_for(&root);
            let clients = named
                .iter()
                .filter(|(path, _)| *path != directory && *path != own_path)
                .count();
            if clients > 0 {
                return Err(fail(if own.client_session.is_some() {
                    Failure::NoSessionContext
                } else {
                    Failure::NoClientSession
                }));
            }
            named.retain(|(path, _)| *path != own_path);
        }
        match named.len() {
            0 => return Err(fail(Failure::NoSessionContext)),
            1 => {
                let (path, context) = named.remove(0);
                ContextFile { path, context }
            }
            count => return Err(fail(Failure::AmbiguousSessionContext(count))),
        }
    };
    let context = &file.context;
    let ids = Ids {
        task_id: context.task_id.clone(),
        session_id: context.session_id.clone(),
    };
    let session_id = context.session_id.clone().unwrap_or_default();
    let usable = is_uuid_v7(&session_id)
        && is_uuid_v7(&context.workspace_id)
        && context.task_id.as_deref().is_none_or(is_uuid_v7);
    if !usable {
        return Err(Stop::Tool(ids, Failure::ContextUnusable));
    }
    Ok(Target {
        ids,
        session_id,
        workspace_id: context.workspace_id.clone(),
        file: Some(file),
    })
}

/// U1 (A3, original #34): the context files of `named` whose Session has
/// not ended. A context file can name a Session that ended elsewhere (a
/// `session end <SESSION_ID>` from another context, an `end` with
/// `abandoned`), and rule D does not count it. Each Session's state is
/// read from `listSessions` of its context's Workspace; a file whose
/// Session the listing does not show as `ended` or `crashed`, or whose
/// listing cannot be read, is counted, as before.
fn live(local: &Local, tool: &Tool, named: Vec<(PathBuf, Context)>) -> Vec<(PathBuf, Context)> {
    if named.is_empty() {
        return named;
    }
    let Ok(reader) = local.reader(tool) else {
        return named;
    };
    let port = Port(&reader);
    named
        .into_iter()
        .filter(|(_, context)| {
            let (Some(session), workspace) =
                (context.session_id.as_deref(), context.workspace_id.as_str())
            else {
                return false;
            };
            if !is_uuid_v7(session) || !is_uuid_v7(workspace) {
                return true;
            }
            let listed = flow::listed(
                &port,
                OperationId::ListSessions,
                &format!("/v1/sessions?workspace_id={workspace}"),
                "session_id",
                session,
            );
            !matches!(
                listed,
                Ok(Some(item)) if matches!(
                    item.get("state").and_then(JsonValue::as_str),
                    Some("ended" | "crashed")
                )
            )
        })
        .collect()
}

/// The Session `session: k` names (ADR 0014 E7.5.2): the k-th of the
/// `sessions` of the Capsule of the Task chosen as for reads (the hook's
/// rule m), and the context file of the root that names it, if any. A
/// `tampered` Capsule resolves no number.
fn by_session(local: &Local, tool: &Tool, asked: i64) -> Result<Target, Stop> {
    let ids = local.ids();
    let fail = stop(&ids);
    let base = local
        .base
        .as_ref()
        .ok_or_else(|| fail(Failure::NoConfigDir))?;
    let cwd = local.cwd.as_ref().ok_or_else(|| fail(Failure::NoContext))?;
    let client = local
        .own
        .as_ref()
        .filter(|own| own.client_session.is_some());
    let chosen = hook::choose_task(client, &base.for_client(None), cwd, || {
        Reader::new(&local.environment.process, base).map(|reader| reader.only(tool.operations))
    })
    .map_err(|error| {
        let (ids, failure) = choice_failure(error, ids.clone());
        Stop::Tool(ids, failure)
    })?;
    let task_ids = Ids {
        task_id: Some(chosen.task_id.clone()),
        session_id: None,
    };
    let capsule = parse_strict(&chosen.body).map_err(|_| {
        Stop::Tool(
            task_ids.clone(),
            Failure::Read(bad_response(OperationId::GetTaskCapsule)),
        )
    })?;
    if capsule.get("status").and_then(JsonValue::as_str) == Some("tampered") {
        return Err(Stop::Tool(task_ids, Failure::CapsuleTampered));
    }
    let sessions = capsule
        .get("sessions")
        .and_then(JsonValue::as_array)
        .unwrap_or_default();
    let session = usize::try_from(asked - 1)
        .ok()
        .and_then(|index| sessions.get(index))
        .ok_or_else(|| {
            Stop::Tool(
                task_ids.clone(),
                Failure::NoSuchSession {
                    number: asked,
                    total: sessions.len(),
                },
            )
        })?;
    let member = |name: &str| {
        session
            .get(name)
            .and_then(JsonValue::as_str)
            .filter(|id| is_uuid_v7(id))
            .map(str::to_owned)
    };
    let (Some(session_id), Some(workspace_id)) = (member("session_id"), member("workspace_id"))
    else {
        return Err(Stop::Tool(
            task_ids,
            Failure::Read(bad_response(OperationId::GetTaskCapsule)),
        ));
    };
    let file = local
        .context()
        .filter(|context| context.session_id.as_deref() == Some(session_id.as_str()))
        .zip(local.own.as_ref())
        .map(|(context, own)| ContextFile {
            path: own.path_for(&context.workspace_root),
            context: context.clone(),
        })
        .or_else(|| {
            base.context_files_of(&chosen.root)
                .into_iter()
                .find(|(_, context)| context.session_id.as_deref() == Some(session_id.as_str()))
                .map(|(path, context)| ContextFile { path, context })
        });
    Ok(Target {
        ids: Ids {
            task_id: Some(chosen.task_id),
            session_id: Some(session_id.clone()),
        },
        session_id,
        workspace_id,
        file,
    })
}

/// The Session the call concerns: `session: k` when given, else rule D
/// over the files `scope` admits.
fn target(local: &Local, tool: &Tool, arguments: &JsonValue, scope: Scope) -> Result<Target, Stop> {
    match arguments.get("session").and_then(JsonValue::as_i64) {
        Some(asked) => by_session(local, tool, asked),
        None => rule_d(local, tool, scope),
    }
}

/// The reader of the call, restricted to `tool`'s operations.
fn reader(local: &Local, tool: &Tool, ids: &Ids) -> Result<Reader, Stop> {
    local.reader(tool).map_err(stop(ids))
}

/// The strings of an array argument.
fn strings(value: Option<&JsonValue>) -> Vec<&str> {
    value
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(JsonValue::as_str)
        .collect()
}

/// The claim numbers of an array argument.
fn numbers(value: Option<&JsonValue>) -> Vec<ClaimRef> {
    value
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(JsonValue::as_i64)
        .filter_map(|claim| u64::try_from(claim).ok())
        .map(ClaimRef::Number)
        .collect()
}

/// One run as the errors list it: `r4 (PASS)`.
fn shown_run(context: &Context, run: &crate::context::RunLabel) -> String {
    let status = run.status.as_deref().unwrap_or("earlier run");
    match context.overruled_by(run) {
        Some(later) => format!("{} ({status}, overruled by {later})", run.label),
        None => format!("{} ({status})", run.label),
    }
}

/// The Evidence IDs and labels of run labels (`rN` or `last`), resolved
/// through `context`, each once, in order (as `--evidence` resolves them).
fn evidence_of(
    context: &Context,
    labels: &[&str],
    completion: bool,
) -> Result<(Vec<String>, Vec<String>), Failure> {
    let mut ids = Vec::new();
    let mut shown = Vec::new();
    for label in labels {
        let (id, named) = if *label == LAST_EVIDENCE {
            let id = context
                .evidence_id
                .clone()
                .ok_or(Failure::NoRunToCite(completion))?;
            let named = context
                .runs
                .iter()
                .rfind(|run| run.evidence_id == id)
                .map_or_else(|| LAST_EVIDENCE.to_owned(), |run| run.label.clone());
            (id, named)
        } else {
            let run = context.run(label).ok_or_else(|| Failure::NoSuchRun {
                label: (*label).to_owned(),
                citable: context
                    .runs
                    .iter()
                    .map(|run| shown_run(context, run))
                    .collect(),
            })?;
            (run.evidence_id.clone(), run.label.clone())
        };
        if !is_uuid_v7(&id) {
            return Err(Failure::ContextUnusable);
        }
        if !ids.contains(&id) {
            ids.push(id);
            shown.push(named);
        }
    }
    Ok((ids, shown))
}

/// A tool error for a stop of [`flow::resolve_draft`]; `Ok` with the
/// report when `--reaffirm-all` left nothing to record.
fn draft_stop(ids: &Ids, stop: DraftStop<Port<'_>>) -> Result<Box<ReaffirmAllReport>, Stop> {
    let fail = |failure| Err(Stop::Tool(ids.clone(), failure));
    match stop {
        DraftStop::Nothing(report) => Ok(report),
        DraftStop::Unread(error) => fail(Failure::Read(error)),
        DraftStop::Capsule(CapsuleError::Tampered(_)) => fail(Failure::CapsuleTampered),
        DraftStop::Capsule(_) => fail(Failure::Read(bad_response(OperationId::GetTaskCapsule))),
        DraftStop::Claim(error) => claim_failure(&error).map_or(Err(Stop::Internal), fail),
    }
}

/// The tool error of a reference that does not resolve; `None` for the
/// cases the server's numbers never reach (a programming error).
fn claim_failure(error: &ClaimError) -> Option<Failure> {
    match error {
        ClaimError::NotListed {
            number,
            first,
            last,
        } => Some(Failure::NoSuchClaim {
            number: *number,
            listed: match (first, last) {
                (Some(first), Some(last)) => format!("#{first}–#{last}"),
                _ => "no claim".to_owned(),
            },
        }),
        ClaimError::NotAFact { number, .. } | ClaimError::NotActive { number, .. } => {
            Some(Failure::NotReaffirmable(*number))
        }
        ClaimError::IdNotListed(_)
        | ClaimError::NeedsCapsule(_)
        | ClaimError::ReaffirmNeedsCapsule
        | ClaimError::ReaffirmAllNeedsCapsule
        // U8 A5: the tools have no `--withdraw-session`.
        | ClaimError::NoSuchSession { .. }
        | ClaimError::NothingInForce(_)
        | ClaimError::TooManyInForce { .. }
        | ClaimError::SnapshotRequired(_) => None,
    }
}

/// Builds the request of `draft`: now, or through the Task Capsule when it
/// names numbers (as the CLI's parser and `resolve_draft` do). `Err(Ok)` is
/// `--reaffirm-all` with nothing to record.
fn drafted(
    port: &Port<'_>,
    ids: &Ids,
    operation: OperationId,
    target: &str,
    draft: Draft,
) -> Result<(Request, flow::Resolved), Result<Box<ReaffirmAllReport>, Stop>> {
    let mut request = Request::bare(operation, target.to_owned());
    if draft.needs_capsule() {
        request.pending = Some(draft);
    } else {
        request.body = Some(draft.render(None).map_err(|error| {
            Err(claim_failure(&error)
                .map_or(Stop::Internal, |failure| Stop::Tool(ids.clone(), failure)))
        })?);
    }
    // The warnings of `--reaffirm-all --evidence` name commands; the result
    // carries their codes instead.
    let resolved = flow::resolve_draft(port, &mut request, &mut |_| {})
        .map_err(|stop| draft_stop(ids, stop))?;
    let request = flow::keyed(request).map_err(|_| Err(Stop::Internal))?;
    Ok((request, resolved))
}

/// `begin` (E7.5.6): `memprove begin` in the server's own context.
pub(super) fn begin(
    local: &Local,
    tool: &Tool,
    arguments: &JsonValue,
) -> Result<ToolResult, Internal> {
    finish(begin_call(local, tool, arguments))
}

fn begin_call(local: &Local, tool: &Tool, arguments: &JsonValue) -> Result<ToolResult, Stop> {
    let ids = local.ids();
    let fail = stop(&ids);
    if arguments.get("task").is_some() && arguments.get("new_task").is_some() {
        return Err(fail(Failure::Combination("task", "new_task")));
    }
    let context = local.context();
    // U10 N1 (U9 VN6): the agent is one declaration (inputs, else the
    // server's variables, else the context's), and a part of one is
    // `invalid_arguments` before the tool says there is no Project, no Task,
    // or no context, with or without a context to continue.
    let agent = begin_agent(context, arguments, &server_agent_env()).map_err(&fail)?;
    let own = local
        .own
        .as_ref()
        .ok_or_else(|| fail(Failure::NoConfigDir))?;
    let root = local
        .cwd
        .as_ref()
        .and_then(|cwd| workspace_root(cwd).ok())
        .ok_or_else(|| fail(Failure::NoContext))?;
    if own.check_outside(&root).is_err() {
        return Err(fail(Failure::ConfigDirInWorkspace));
    }
    let root_text = root.to_str().ok_or_else(|| fail(Failure::NoContext))?;
    let last_pack = own.last_pack(&root).ok().flatten();
    let project_id = context
        .map(|context| context.project_id.clone())
        // U1 (U-F, SF-1R): the Project of the last-Pack record, also of a
        // record of no Pack the hook wrote.
        .or_else(|| own.recorded_project(&root))
        .ok_or_else(|| {
            fail(Failure::NoProject {
                directory_context: local.directory_context_only(),
            })
        })?;
    if !is_uuid_v7(&project_id) {
        return Err(fail(Failure::ContextUnusable));
    }
    let reader = reader(local, tool, &ids)?;
    let task = begin_task(
        &reader,
        &ids,
        &project_id,
        context,
        last_pack.as_ref(),
        arguments,
    )?;
    let task_ids = Ids {
        task_id: match &task {
            BeginTask::Existing(task_id) => Some(task_id.clone()),
            BeginTask::New { .. } => None,
        },
        session_id: None,
    };
    let spec = BeginSpec {
        project_id,
        task,
        agent_id: context
            .map(|context| context.agent_id.clone())
            .filter(|agent| is_uuid_v7(agent)),
        agent,
    };
    let request = BeginRequest {
        spec: &spec,
        root: &root,
        root_text,
        store: own,
        previous: context
            .filter(|context| context.workspace_root == root)
            .and_then(|context| {
                context
                    .session_id
                    .as_deref()
                    .map(|session| (session, context.workspace_id.as_str()))
            }),
    };
    let port = Port(&reader);
    let begun = flow::begin(&port, &request, &mut |note| {
        if note.is_warning() {
            crate::log_line(format_args!("{}", note.line(&request)));
        }
    })
    .map_err(|stop| begin_stop(&ids, &task_ids, stop))?;
    begun_result(own, begun)
}

/// The result of a `begin` that started a Session: the context written,
/// the Pack declared, and whether the Task moved (E7.5.6).
fn begun_result(own: &ContextStore, begun: flow::Begun<ReadError>) -> Result<ToolResult, Stop> {
    let context = begun.context;
    let new_ids = Ids {
        task_id: context.task_id.clone(),
        session_id: context.session_id.clone(),
    };
    if own.save(&context).is_err() {
        return Err(Stop::Tool(new_ids, Failure::ContextNotWritten));
    }
    let mut members = vec![
        (
            "context_file",
            text(if own.client_session.is_some() {
                "client"
            } else {
                "directory"
            }),
        ),
        // U11 T1 (Owner O3): the agent the Session declared, in words.
        ("agent", text(&session_agent(&context))),
        (
            "context",
            parse_strict(&context.to_json()).map_err(|_| Stop::Internal)?,
        ),
        (
            "resumed_from",
            begun.resumed.as_ref().map_or(JsonValue::Null, |pack| {
                object([
                    (
                        "project_sequence",
                        number(pack.resumed_from.project_sequence),
                    ),
                    (
                        "pack_digest",
                        text(&format!("sha256:{}", pack.resumed_from.pack_digest)),
                    ),
                ])
            }),
        ),
        ("task_moved", JsonValue::Bool(begun.moved)),
    ];
    if let Err(unmoved) = begun.advanced {
        let code = match &unmoved {
            Unmoved::Failed(error) => Port::problem_code(error),
            Unmoved::Generate(_) => None,
        };
        members.push(("warning", text("task_not_moved")));
        members.push(("code", optional(code.as_deref())));
    }
    Ok(within_budget(&new_ids, &members, &context))
}

/// U11 T1 (Owner O3): the agent of the Session `begin` started, as the
/// context it wrote records it: `agent「<name>」(<role>)`, or `no agent`
/// when the Session declared none. The name and the role were checked
/// before the Session was created (a name of `[A-Za-z0-9_-]`, a role of the
/// closed set), so they need no quoting.
fn session_agent(context: &Context) -> String {
    match (&context.agent_name, &context.agent_role) {
        (Some(name), Some(role)) => format!("agent「{name}」({role})"),
        _ => "no agent".to_owned(),
    }
}

/// U1 (A5, original #26): the `begin` result, at most
/// [`MCP_PACK_DEFAULT_BUDGET`] UTF-16 code units. When the whole context
/// does not fit, it is shown as the `context` tool shows it: a Task title
/// over 120 characters cut to its first 120, with `task_title_chars`
/// giving its length, and the latest run labels, fewer until it fits,
/// with `runs_total` giving their number.
fn within_budget(
    ids: &Ids,
    members: &[(&'static str, JsonValue)],
    context: &Context,
) -> ToolResult {
    let whole = succeeded(ids, members.to_vec(), None);
    if whole.text.encode_utf16().count() <= MCP_PACK_DEFAULT_BUDGET {
        return whole;
    }
    let mut result = whole;
    for runs_shown in [CONTEXT_RUNS_SHOWN, 16, 4, 0] {
        let cut = members
            .iter()
            .map(|(name, value)| {
                if *name == "context" {
                    (*name, shown_context(context, runs_shown))
                } else {
                    (*name, value.clone())
                }
            })
            .collect();
        result = succeeded(ids, cut, None);
        if result.text.encode_utf16().count() <= MCP_PACK_DEFAULT_BUDGET {
            break;
        }
    }
    result
}

/// The Task `begin` continues: `task: n` (the n-th Task of the Project),
/// `new_task`, else the context's, else the last Pack's.
/// U10 T6 (Owner O4; D3 F22): the server process's own
/// `MEMPROVE_AGENT_NAME`, `MEMPROVE_AGENT_ROLE`, and
/// `MEMPROVE_PARENT_SESSION` (an empty one is unset), which a client that
/// starts the server with its environment (Claude Code does) passes on.
fn server_agent_env() -> crate::AgentEnv {
    crate::AgentEnv::of(
        std::env::var_os(crate::AGENT_NAME_ENV),
        std::env::var_os(crate::AGENT_ROLE_ENV),
        std::env::var_os(crate::PARENT_SESSION_ENV),
    )
}

/// U8 A3 (Owner O4): the agent `begin` declares: `agent_name`,
/// `agent_role`, and `parent_session` from the arguments, else (when none of
/// the three is given) U10 T6 (Owner O4; D3 F22) the server's environment
/// variables `env` ([`server_agent_env`]), else (when none of those is set
/// either) the context file's agent, whole: the order of `end`'s ender
/// ([`end_ender`]) and of `memprove begin`. Each source is taken as a
/// whole. A name needs a role and a role a name; a parent needs both. Until
/// U10 the server read no variable here, so a client started with
/// `MEMPROVE_AGENT_ROLE=sub_agent` began a Session with no role and R1 did
/// not apply to it (D3 F22).
fn begin_agent(
    context: Option<&Context>,
    arguments: &JsonValue,
    env: &crate::AgentEnv,
) -> Result<Option<memprove_domain::SessionAgent>, Failure> {
    let given = |argument: &str| {
        arguments
            .get(argument)
            .and_then(JsonValue::as_str)
            .map(str::to_owned)
    };
    let mut name = given("agent_name");
    let mut role = given("agent_role");
    let mut parent = given("parent_session");
    // U8 S-F round 1, B1: the declaration is taken as a whole; only without
    // any of the three inputs are the variables read, and only without any
    // of those is the context's remembered agent reused, all of it.
    if name.is_none() && role.is_none() && parent.is_none() {
        name.clone_from(&env.name);
        role.clone_from(&env.role);
        parent.clone_from(&env.parent_session);
    }
    if let Some(context) = context.filter(|_| name.is_none() && role.is_none() && parent.is_none())
    {
        name.clone_from(&context.agent_name);
        role.clone_from(&context.agent_role);
        parent.clone_from(&context.parent_session_id);
    }
    let invalid = |parameter: &str| Failure::InvalidArguments(parameter.to_owned());
    let (name, role) = match (name, role) {
        (None, None) if parent.is_none() => return Ok(None),
        (None, None | Some(_)) => return Err(invalid("agent_name")),
        (Some(_), None) => return Err(invalid("agent_role")),
        (Some(name), Some(role)) => (name, role),
    };
    if !memprove_domain::SessionAgent::is_name(&name) {
        return Err(invalid("agent_name"));
    }
    if memprove_domain::AgentRole::parse(&role).is_none() {
        return Err(invalid("agent_role"));
    }
    memprove_domain::SessionAgent::new(&name, &role, parent.as_deref())
        .map(Some)
        .map_err(|_| invalid("parent_session"))
}

fn begin_task(
    reader: &Reader,
    ids: &Ids,
    project_id: &str,
    context: Option<&Context>,
    last_pack: Option<&crate::context::LastPack>,
    arguments: &JsonValue,
) -> Result<BeginTask, Stop> {
    let fail = stop(ids);
    if let Some(asked) = arguments.get("task").and_then(JsonValue::as_i64) {
        let mut tasks = reader
            .tasks(project_id)
            .map_err(|error| fail(Failure::Read(error)))?
            .iter()
            .filter_map(|task| task.get("task_id").and_then(JsonValue::as_str))
            .filter(|task| is_uuid_v7(task))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        tasks.sort();
        let total = tasks.len();
        return usize::try_from(asked - 1)
            .ok()
            .filter(|index| *index < total)
            .map(|index| BeginTask::Existing(tasks.swap_remove(index)))
            .ok_or_else(|| {
                fail(Failure::NoSuchTask {
                    number: asked,
                    total,
                })
            });
    }
    if let Some(new_task) = arguments.get("new_task") {
        let field = |name: &str| {
            new_task
                .get(name)
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        };
        return Ok(BeginTask::New {
            title: field("title")
                .ok_or_else(|| fail(Failure::InvalidArguments("new_task".into())))?,
            description: field("description"),
        });
    }
    let task_id = context
        .and_then(|context| context.task_id.clone())
        .or_else(|| last_pack.map(|pack| pack.task_id.clone()))
        .ok_or_else(|| fail(Failure::NoTask))?;
    if is_uuid_v7(&task_id) {
        Ok(BeginTask::Existing(task_id))
    } else {
        Err(fail(Failure::ContextUnusable))
    }
}

/// The tool error of a `begin` that started no Session.
fn begin_stop(ids: &Ids, task_ids: &Ids, stop: BeginStop<Port<'_>>) -> Stop {
    match stop {
        BeginStop::Local(LocalFailure::Generate(_)) => Stop::Internal,
        BeginStop::Local(LocalFailure::Missing(operation, _)) => {
            Stop::Tool(ids.clone(), Failure::Read(bad_response(operation)))
        }
        BeginStop::Active(_) => Stop::Tool(ids.clone(), Failure::SessionActive),
        BeginStop::Unconfirmed(_, error) | BeginStop::Failed(error) => {
            Stop::Tool(ids.clone(), Failure::Read(error))
        }
        BeginStop::Finished(_, state) => Stop::Tool(task_ids.clone(), Failure::TaskFinished(state)),
    }
}

/// `checkpoint` (E7.5.6): `memprove checkpoint create` in the Session of
/// rule D, or of `session`.
pub(super) fn checkpoint(
    local: &Local,
    tool: &Tool,
    arguments: &JsonValue,
) -> Result<ToolResult, Internal> {
    finish(checkpoint_call(local, tool, arguments))
}

fn checkpoint_call(local: &Local, tool: &Tool, arguments: &JsonValue) -> Result<ToolResult, Stop> {
    if arguments.get("withdraw_reason").is_some() && arguments.get("withdraw").is_none() {
        return Err(Stop::Tool(
            local.ids(),
            Failure::Combination("withdraw_reason", "withdraw"),
        ));
    }
    let target = target(local, tool, arguments, Scope::Any)?;
    let ids = target.ids.clone();
    let file = target.file()?;
    let draft = checkpoint_draft(&file.context, &target, arguments).map_err(stop(&ids))?;
    let snapshot_id = draft.snapshot_id.clone().unwrap_or_default();
    let reader = reader(local, tool, &ids)?;
    let port = Port(&reader);
    let (request, resolved) = match drafted(
        &port,
        &ids,
        OperationId::CreateCheckpoint,
        "/v1/checkpoints",
        Draft::Checkpoint(draft),
    ) {
        Ok(drafted) => drafted,
        Err(Ok(report)) => return Ok(not_sent(&ids, &snapshot_id, &report)),
        Err(Err(stop)) => return Err(stop),
    };
    let body = request.body.clone().ok_or(Stop::Internal)?;
    let notes = claims::long_statement_notes(&body).len();
    let reply = port
        .exchange(&request)
        .map_err(|error| Stop::Tool(ids.clone(), Failure::Read(error)))?;
    let read = flow::read_back(&port, &body, &reply, &resolved.reaffirmed);
    Ok(recorded(
        &ids,
        &snapshot_id,
        &reply,
        read.as_ref(),
        resolved.reaffirm_all.as_ref(),
        notes,
    ))
}

/// The statements of `facts` and `open_items`.
fn statements(
    context: &Context,
    arguments: &JsonValue,
    name: &str,
    fact: bool,
) -> Result<Vec<StatementDraft>, Failure> {
    arguments
        .get(name)
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .map(|item| {
            let text = item
                .get("text")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| Failure::InvalidArguments(name.to_owned()))?
                .to_owned();
            Ok(StatementDraft {
                kind: if fact {
                    StatementKind::Fact(text)
                } else {
                    StatementKind::Open(text)
                },
                evidence: evidence_of(context, &strings(item.get("evidence")), false)?.0,
                supersedes: numbers(item.get("supersedes")),
            })
        })
        .collect()
}

/// The `CreateCheckpointRequest` draft, from the arguments exactly as the
/// CLI's parser builds it from flags: facts, open items, reaffirmations,
/// then `reaffirm_all`; a fact or reaffirmation without `evidence` cites
/// the latest run (P2-H), an open item cites nothing (P3-L L5).
fn checkpoint_draft(
    context: &Context,
    target: &Target,
    arguments: &JsonValue,
) -> Result<CheckpointDraft, Failure> {
    let mut statements = [
        statements(context, arguments, "facts", true)?,
        statements(context, arguments, "open_items", false)?,
    ]
    .concat();
    for item in arguments
        .get("reaffirm")
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
    {
        let claim = item
            .get("claim")
            .and_then(JsonValue::as_i64)
            .and_then(|claim| u64::try_from(claim).ok())
            .ok_or_else(|| Failure::InvalidArguments("reaffirm".to_owned()))?;
        statements.push(StatementDraft {
            kind: StatementKind::Reaffirm(ClaimRef::Number(claim)),
            evidence: evidence_of(context, &strings(item.get("evidence")), false)?.0,
            supersedes: Vec::new(),
        });
    }
    if let Some(all) = arguments.get("reaffirm_all") {
        statements.push(StatementDraft {
            kind: StatementKind::ReaffirmAll,
            evidence: evidence_of(context, &strings(all.get("evidence")), false)?.0,
            supersedes: Vec::new(),
        });
    }
    for statement in &mut statements {
        if statement.is_fact()
            && statement.evidence.is_empty()
            && statement.kind != StatementKind::ReaffirmAll
        {
            let latest = context
                .evidence_id
                .clone()
                .filter(|id| is_uuid_v7(id))
                .ok_or(Failure::NoRunToCite(false))?;
            statement.evidence.push(latest);
        }
    }
    // U8 K5 (Owner O5): a fact whose every citation is a command run is held
    // back with the one reason, as the CLI does.
    for statement in &statements {
        if matches!(statement.kind, StatementKind::Fact(_))
            && let Some((run, argv)) = context.only_command_runs(&statement.evidence)
        {
            return Err(Failure::CommandRunCited {
                label: run.label.clone(),
                argv: argv.to_vec(),
            });
        }
    }
    let next = arguments.get("next");
    let snapshot_id = context
        .snapshot_id
        .clone()
        .filter(|id| is_uuid_v7(id))
        .ok_or(Failure::NoSnapshot)?;
    Ok(CheckpointDraft {
        workspace_id: target.workspace_id.clone(),
        task_id: context
            .task_id
            .clone()
            .ok_or(Failure::ContextIncomplete(" Task"))?,
        session_id: target.session_id.clone(),
        objective: arguments
            .get("objective")
            .and_then(JsonValue::as_str)
            .map(str::to_owned)
            .or_else(|| context.task_title.clone())
            .filter(|objective| !objective.is_empty())
            .ok_or_else(|| Failure::InvalidArguments("objective".to_owned()))?,
        statements,
        next: next
            .and_then(|next| next.get("text"))
            .and_then(JsonValue::as_str)
            .ok_or_else(|| Failure::InvalidArguments("next".to_owned()))?
            .to_owned(),
        next_supersedes: numbers(next.and_then(|next| next.get("supersedes"))),
        withdrawals: numbers(arguments.get("withdraw")),
        withdrawal_reason: arguments
            .get("withdraw_reason")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
        withdraw_session: None,
        state_runs: context.runs.clone(),
        state_manifest: context.snapshot_manifest.clone(),
        state_workspace: Some(context.workspace_id.clone()),
        snapshot_id: Ok(snapshot_id),
    })
}

/// `{number, by}` items.
fn pairs(items: impl IntoIterator<Item = (i64, i64)>, second: &'static str) -> JsonValue {
    JsonValue::Array(
        items
            .into_iter()
            .map(|(first, then)| object([("number", number(first)), (second, number(then))]))
            .collect(),
    )
}

/// What `--reaffirm-all` did, as codes: each reaffirmed fact with the runs
/// it cites and, read back, the claim that reaffirms it and its verdict;
/// each skipped fact with the codes of its reasons; each warning's code.
fn reaffirm_all_json(report: &ReaffirmAllReport, after: Option<(&str, &str)>) -> JsonValue {
    let outcomes =
        after.and_then(|(capsule, checkpoint_id)| report.outcomes(capsule, checkpoint_id));
    let reaffirmed = report
        .reaffirmed
        .iter()
        .enumerate()
        .map(|(index, (claim, runs))| {
            let outcome = outcomes
                .as_ref()
                .and_then(|outcomes| outcomes.get(index))
                .and_then(Option::as_ref);
            object([
                ("number", number(claim)),
                ("runs", texts(runs.iter().map(String::as_str))),
                (
                    "as",
                    outcome.map_or(JsonValue::Null, |outcome| number(outcome.number)),
                ),
                (
                    "verdict",
                    optional(outcome.map(|outcome| outcome.verdict.as_str())),
                ),
                (
                    "reasons",
                    outcome.map_or(JsonValue::Null, |outcome| {
                        texts(outcome.reasons.iter().map(String::as_str))
                    }),
                ),
            ])
        })
        .collect();
    let skipped = report
        .skip_codes
        .iter()
        .map(|(claim, codes)| {
            object([
                ("number", number(claim)),
                ("reasons", texts(codes.iter().map(|code| code.as_str()))),
            ])
        })
        .collect();
    let warnings = report
        .warned
        .iter()
        .map(|claim| {
            object([
                ("number", number(claim)),
                ("warning", text(claims::WARNING_OTHER_COMMANDS)),
            ])
        })
        .collect();
    object([
        ("reaffirmed", JsonValue::Array(reaffirmed)),
        ("skipped", JsonValue::Array(skipped)),
        ("warnings", JsonValue::Array(warnings)),
    ])
}

/// The result of a Checkpoint `--reaffirm-all` left nothing to record:
/// nothing was sent (P4-A judgment 8), a success.
fn not_sent(ids: &Ids, snapshot_id: &str, report: &ReaffirmAllReport) -> ToolResult {
    let none = || JsonValue::Array(Vec::new());
    succeeded(
        ids,
        vec![
            ("checkpoint_id", JsonValue::Null),
            ("snapshot_id", text(snapshot_id)),
            ("sent", JsonValue::Bool(false)),
            ("new_statements", none()),
            ("superseded", none()),
            ("withdrawn", none()),
            ("reaffirmed", none()),
            ("reaffirm_all", reaffirm_all_json(report, None)),
            ("notes", none()),
            ("report", JsonValue::Null),
        ],
        None,
    )
}

/// The result of a recorded Checkpoint: the numbers it touched, read back
/// (P3-L L8), never a statement text.
fn recorded(
    ids: &Ids,
    snapshot_id: &str,
    reply: &Reply,
    read: Option<&flow::ReadBack<ReadError>>,
    reaffirm_all: Option<&ReaffirmAllReport>,
    notes: usize,
) -> ToolResult {
    let report = read.and_then(|read| read.report.as_ref().ok());
    let none = || JsonValue::Array(Vec::new());
    let after = read.and_then(|read| {
        read.capsule
            .as_deref()
            .ok()
            .map(|capsule| (capsule, read.checkpoint_id.as_str()))
    });
    succeeded(
        ids,
        vec![
            (
                "checkpoint_id",
                optional(flow::member(&reply.body, &["checkpoint_id"]).as_deref()),
            ),
            ("snapshot_id", text(snapshot_id)),
            ("sent", JsonValue::Bool(true)),
            (
                "new_statements",
                report.map_or_else(none, |report| {
                    JsonValue::Array(
                        report
                            .fresh
                            .iter()
                            .map(|(claim, kind)| {
                                object([("number", number(claim)), ("kind", text(kind))])
                            })
                            .collect(),
                    )
                }),
            ),
            (
                "superseded",
                report.map_or_else(none, |report| {
                    pairs(
                        report.superseded.iter().map(|(new, old)| (*old, *new)),
                        "by",
                    )
                }),
            ),
            (
                "withdrawn",
                report.map_or_else(none, |report| {
                    JsonValue::Array(report.withdrawn.iter().map(number).collect())
                }),
            ),
            (
                "reaffirmed",
                report.map_or_else(none, |report| {
                    pairs(report.reaffirmed.iter().copied(), "as")
                }),
            ),
            (
                "reaffirm_all",
                reaffirm_all.map_or(JsonValue::Null, |report| reaffirm_all_json(report, after)),
            ),
            (
                "notes",
                JsonValue::Array(vec![text("long_statement"); notes]),
            ),
            (
                "report",
                text(if report.is_some() {
                    "read"
                } else {
                    "unavailable"
                }),
            ),
        ],
        None,
    )
}

/// `decide` (E7.5.6): `memprove decide` in the Session of rule D, or of
/// `session`.
pub(super) fn decide(
    local: &Local,
    tool: &Tool,
    arguments: &JsonValue,
) -> Result<ToolResult, Internal> {
    finish(decide_call(local, tool, arguments))
}

fn decide_call(local: &Local, tool: &Tool, arguments: &JsonValue) -> Result<ToolResult, Stop> {
    let target = target(local, tool, arguments, Scope::Any)?;
    let ids = target.ids.clone();
    let fail = stop(&ids);
    let task_id = target
        .ids
        .task_id
        .clone()
        .ok_or_else(|| fail(Failure::ContextIncomplete(" Task")))?;
    let labels = strings(arguments.get("evidence"));
    let evidence = if labels.is_empty() {
        Vec::new()
    } else {
        evidence_of(&target.file()?.context, &labels, false)
            .map_err(&fail)?
            .0
    };
    let draft = claims::DecisionDraft {
        task_id: task_id.clone(),
        session_id: target.session_id.clone(),
        content: arguments
            .get("text")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| fail(Failure::InvalidArguments("text".to_owned())))?
            .to_owned(),
        rationale: arguments
            .get("rationale")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
        evidence,
        supersedes: numbers(arguments.get("supersedes")),
    };
    let reader = reader(local, tool, &ids)?;
    let port = Port(&reader);
    let (request, _) = drafted(
        &port,
        &ids,
        OperationId::ProposeDecision,
        "/v1/decisions",
        Draft::Decision(draft),
    )
    .map_err(|stop| stop.map_or_else(|stop| stop, |_| Stop::Internal))?;
    let reply = port
        .exchange(&request)
        .map_err(|error| fail(Failure::Read(error)))?;
    let decision_id = flow::member(&reply.body, &["decision_id"]);
    let capsule = port
        .read(&Request::bare(
            OperationId::GetTaskCapsule,
            format!("/v1/tasks/{task_id}/recovery-capsule"),
        ))
        .ok()
        .and_then(|reply| parse_strict(&reply.body).ok());
    let number_of = |claim_id: &str| {
        capsule.as_ref().and_then(|capsule| {
            capsule
                .get("claims")?
                .as_array()?
                .iter()
                .find(|claim| claim.get("claim_id").and_then(JsonValue::as_str) == Some(claim_id))?
                .get("number")?
                .as_i64()
        })
    };
    let new = decision_id
        .as_deref()
        .and_then(|id| number_of(&format!("decision/{id}")));
    let superseded = parse_strict(&reply.body)
        .ok()
        .and_then(|body| {
            body.get("supersedes")
                .and_then(JsonValue::as_array)
                .map(<[JsonValue]>::to_vec)
        })
        .unwrap_or_default()
        .iter()
        .filter_map(JsonValue::as_str)
        .map(|claim_id| {
            object([
                (
                    "number",
                    number_of(claim_id).map_or(JsonValue::Null, number),
                ),
                ("by", new.map_or(JsonValue::Null, number)),
            ])
        })
        .collect();
    Ok(succeeded(
        &ids,
        vec![
            ("decision_event_id", optional(decision_id.as_deref())),
            ("number", new.map_or(JsonValue::Null, number)),
            ("superseded", JsonValue::Array(superseded)),
        ],
        None,
    ))
}

/// `end` (E7.5.6): `memprove end` of the Session of rule D; with
/// `session` and `abandoned`, `memprove end --abandoned` of another
/// agent's Session. The context file naming the Session is cleared.
pub(super) fn end(
    local: &Local,
    tool: &Tool,
    arguments: &JsonValue,
) -> Result<ToolResult, Internal> {
    finish(end_call(local, tool, arguments))
}

fn end_call(local: &Local, tool: &Tool, arguments: &JsonValue) -> Result<ToolResult, Stop> {
    let abandoned = arguments.get("abandoned").and_then(JsonValue::as_bool) == Some(true);
    match (arguments.get("session").is_some(), abandoned) {
        (true, false) => {
            return Err(Stop::Tool(
                local.ids(),
                Failure::Combination("session", "abandoned"),
            ));
        }
        (false, true) => {
            return Err(Stop::Tool(
                local.ids(),
                Failure::Combination("abandoned", "session"),
            ));
        }
        _ => {}
    }
    // U10 N2 (U9 VN7): the agent inputs are the ender's, which only an
    // abandoned end records (the wire refuses an `ended_by` otherwise), so
    // `end` without `abandoned` does not take them, and says so rather than
    // ignore them. U10 N1 (U9 VN6): with `abandoned`, a part of an agent (in
    // the inputs or in the server's variables) is `invalid_arguments` before
    // the context is looked for.
    if abandoned {
        end_ender(local, arguments, "").map_err(|failure| Stop::Tool(local.ids(), failure))?;
    } else if let Some(input) = AGENT_INPUTS
        .into_iter()
        .find(|input| arguments.get(input).is_some())
    {
        return Err(Stop::Tool(
            local.ids(),
            Failure::Combination(input, "abandoned"),
        ));
    }
    let target = target(local, tool, arguments, Scope::OwnFile)?;
    let ids = target.ids.clone();
    let fail = stop(&ids);
    let reader = reader(local, tool, &ids)?;
    let port = Port(&reader);
    let revision = if abandoned {
        let listed = flow::listed(
            &port,
            OperationId::ListSessions,
            &format!("/v1/sessions?workspace_id={}", target.workspace_id),
            "session_id",
            &target.session_id,
        )
        .map_err(|error| fail(Failure::Read(error)))?
        .ok_or_else(|| fail(Failure::Read(bad_response(OperationId::ListSessions))))?;
        listed
            .get("revision")
            .and_then(JsonValue::as_i64)
            .and_then(|revision| u64::try_from(revision).ok())
            .ok_or_else(|| fail(Failure::Read(bad_response(OperationId::ListSessions))))?
    } else {
        target
            .file()?
            .context
            .session_revision
            .ok_or_else(|| fail(Failure::ContextIncomplete(" Session revision")))?
    };
    let ended_by = if abandoned {
        end_ender(local, arguments, &target.session_id).map_err(&fail)?
    } else {
        None
    };
    let mut request = Request::bare(
        OperationId::EndSession,
        format!("/v1/sessions/{}/end", target.session_id),
    );
    request.body = Some(end_body_by(
        revision,
        arguments.get("reason").and_then(JsonValue::as_str),
        abandoned,
        ended_by.as_ref(),
    ));
    let request = flow::keyed(request).map_err(|_| Stop::Internal)?;
    port.exchange(&request)
        .map_err(|error| fail(Failure::Read(error)))?;
    let cleared = match (&target.file, local.base.as_ref()) {
        (Some(file), Some(base)) => file.update(base, |context| {
            let current = context.session_id.as_deref() == Some(target.session_id.as_str());
            if current {
                context.session_id = None;
                context.session_revision = None;
            }
            current
        }),
        _ => false,
    };
    Ok(succeeded(
        &ids,
        vec![
            (
                "outcome",
                text(if abandoned { "abandoned" } else { "completed" }),
            ),
            ("context_cleared", JsonValue::Bool(cleared)),
        ],
        None,
    ))
}

/// The three inputs of an agent declaration of `begin` and `end` (U8 A3,
/// U9 A7), in the order the tools' schemas list them.
const AGENT_INPUTS: [&str; 3] = ["agent_name", "agent_role", "parent_session"];

/// U9 A7 (Owner O3): who the `end` tool reports as the ender of the
/// abandoned Session `target`, filled in as `memprove end --abandoned`
/// fills it ([`ender_of`]): the Session of the server's own context when it
/// is not `target`, and the agent of `agent_name`, `agent_role`, and
/// `parent_session` when any of them is given (whole, as `begin` takes
/// them), else of the server's own environment variables, else of its
/// context file (not when that context names `target`).
fn end_ender(
    local: &Local,
    arguments: &JsonValue,
    target: &str,
) -> Result<Option<memprove_domain::SessionEndedBy>, Failure> {
    let given = AGENT_INPUTS
        .into_iter()
        .any(|input| arguments.get(input).is_some());
    let env = if given {
        crate::AgentEnv::default()
    } else {
        crate::AgentEnv::of(
            std::env::var_os(crate::AGENT_NAME_ENV),
            std::env::var_os(crate::AGENT_ROLE_ENV),
            std::env::var_os(crate::PARENT_SESSION_ENV),
        )
    };
    let filled = ender_of(target, local.context(), &env)
        .map_err(|_| Failure::InvalidArguments("agent_name".to_owned()))?;
    if !given {
        return Ok(filled);
    }
    let session_id = filled.and_then(|ended_by| ended_by.session_id);
    let agent = begin_agent(None, arguments, &crate::AgentEnv::default())?;
    Ok((session_id.is_some() || agent.is_some())
        .then_some(memprove_domain::SessionEndedBy { session_id, agent }))
}

/// `complete` (E7.5.6): `memprove task complete` on the context of rule
/// D, or of `session`.
pub(super) fn complete(
    local: &Local,
    tool: &Tool,
    arguments: &JsonValue,
) -> Result<ToolResult, Internal> {
    finish(complete_call(local, tool, arguments))
}

fn complete_call(local: &Local, tool: &Tool, arguments: &JsonValue) -> Result<ToolResult, Stop> {
    let target = target(local, tool, arguments, Scope::OwnFile)?;
    let ids = target.ids.clone();
    let fail = stop(&ids);
    let file = target.file()?;
    let context = &file.context;
    let task_id = context
        .task_id
        .clone()
        .ok_or_else(|| fail(Failure::ContextIncomplete(" Task")))?;
    let revision = context
        .task_revision
        .ok_or_else(|| fail(Failure::ContextIncomplete(" Task revision")))?;
    let reader = reader(local, tool, &ids)?;
    let port = Port(&reader);
    // U3 (K1, Owner O3) and U4 (Y1, N-3): the Session is read once, before
    // the Evidence is resolved, so that the refusal of an Session that is
    // not active wins over "no run to cite" and the like. When the listing
    // shows it not active (or does not list it), or cannot be read at all
    // (a Problem, no daemon), nothing is sent.
    match flow::session_state(&port, &target.workspace_id, &target.session_id) {
        Ok(Some(state)) if state == "active" => {}
        Ok(Some(state)) if state == "ended" => {
            return Err(fail(Failure::SessionNotActive(Some("ended"))));
        }
        Ok(Some(state)) if state == "crashed" => {
            return Err(fail(Failure::SessionNotActive(Some("crashed"))));
        }
        Ok(_) => return Err(fail(Failure::SessionNotActive(None))),
        // A Task judged tampered refuses its listing (`409
        // capsule_tampered`): the sentence the other tools give a tampered
        // Task, which says nothing was sent.
        Err(ReadError {
            failure: ReadFailure::Refused { ref code, .. },
            ..
        }) if code.as_deref() == Some("capsule_tampered") => {
            return Err(fail(Failure::CapsuleTampered));
        }
        Err(error) => return Err(fail(Failure::Read(error))),
    }
    let labels = strings(arguments.get("evidence"));
    let (evidence, cited) = if labels.is_empty() {
        let runs = default_completion(context).map_err(|failing| {
            fail(Failure::FailingRuns(
                failing.iter().map(|run| shown_run(context, run)).collect(),
            ))
        })?;
        if runs.is_empty() {
            evidence_of(context, &[LAST_EVIDENCE], true).map_err(&fail)?
        } else {
            let mut evidence = Vec::new();
            for run in &runs {
                if !is_uuid_v7(&run.evidence_id) {
                    return Err(fail(Failure::ContextUnusable));
                }
                evidence.push(run.evidence_id.clone());
            }
            (evidence, runs.iter().map(|run| run.label.clone()).collect())
        }
    } else {
        evidence_of(context, &labels, true).map_err(&fail)?
    };
    let mut request = Request::bare(
        OperationId::TransitionTask,
        format!("/v1/tasks/{task_id}/transition"),
    );
    // U9 SF1-A (the Owner's O8 exception to R1): the context's Session
    // asks, and the daemon judges the completion by its declared role.
    request.body = Some(completion_body(
        revision,
        &evidence,
        &target.workspace_id,
        Some(&target.session_id),
    ));
    let request = flow::keyed(request).map_err(|_| Stop::Internal)?;
    let reply = port
        .exchange(&request)
        .map_err(|error| fail(Failure::Read(error)))?;
    let state = flow::member(&reply.body, &["state"]);
    let revision = flow::number(&reply.body, "revision");
    if let Some(base) = local.base.as_ref() {
        file.update(base, |context| {
            let current = context.task_id.as_deref() == Some(task_id.as_str());
            if current {
                context.task_revision = revision;
                context.task_state.clone_from(&state);
            }
            current
        });
    }
    Ok(succeeded(
        &ids,
        vec![
            ("state", optional(state.as_deref())),
            ("revision", revision.map_or(JsonValue::Null, number)),
            ("cited", texts(cited.iter().map(String::as_str))),
        ],
        None,
    ))
}
