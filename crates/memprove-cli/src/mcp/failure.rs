//! Tool results (ADR 0014 E7.5.3) and tool errors (E7.7): a closed set of
//! failure codes, each with one fixed sentence whose only variable parts are
//! statuses, Problem codes, scopes, operation names, counts, numbers, and
//! budgets (Owner condition u). No sentence holds agent text, a request
//! value, a path, or a token. U2 (S1, N-3, Owner O4): the one exception is
//! the `detail` of a `400 invalid_request` of a write, which memproved
//! writes itself (it names the member and the rule, never the value) and
//! which is relayed only when it is printable ASCII of at most
//! `PROBLEM_DETAIL_MAX_CHARS` characters with no token in it.

use memprove_protocol::{
    phase1::{OperationId, binding_for},
    strict_json::JsonValue,
};

use super::wire::{compact, number, object, optional, text, texts};
use crate::hook::{ReachError, ReadError, ReadFailure};

/// The Task and Session a result concerns (Owner condition s): UUIDs or
/// `null`, the first two members of every `structuredContent`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Ids {
    /// The Task.
    pub(super) task_id: Option<String>,
    /// The Session.
    pub(super) session_id: Option<String>,
}

impl Ids {
    fn members(&self) -> Vec<(&'static str, JsonValue)> {
        vec![
            ("task_id", optional(self.task_id.as_deref())),
            ("session_id", optional(self.session_id.as_deref())),
        ]
    }
}

/// What a `tools/call` returns, in both eras.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ToolResult {
    /// `content[0].text`.
    pub(super) text: String,
    /// `structuredContent`, or for a Pack the second text block.
    pub(super) structured: JsonValue,
    /// T21 (V6-F1): `structured` goes as the compact text of `content[1]`
    /// and the result has no `structuredContent`: the result of `pack` and
    /// `claims`, whose first text block (the Pack) is not that object's
    /// serialization. A client that hands the model `structuredContent` in
    /// place of the text blocks (Claude Code 2.1.290) would drop the Pack.
    pub(super) structured_as_text: bool,
    /// `isError`.
    pub(super) is_error: bool,
    /// U12 T2 (Owner O4): the opening of a whole `pack` Pack, a text block
    /// before `text` (the hook's opening without its `SessionStart` words);
    /// `None` for every other result.
    pub(super) opening: Option<String>,
    /// What the standard-error log says: `ok`, or the failure code.
    pub(super) outcome: &'static str,
}

/// A programming error: the message is answered `-32603`, never a tool
/// result (an operation outside the tool's static set, ADR 0014 E7.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Internal;

/// Why a tool call did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Failure {
    /// A parameter (a schema key) is missing or does not hold.
    InvalidArguments(String),
    /// A member the `inputSchema` does not name.
    UnknownArgument,
    /// Two parameters that do not go together (the first named), or one
    /// that needs the other.
    Combination(&'static str, &'static str),
    /// No configuration directory.
    NoConfigDir,
    /// The daemon cannot be asked (the hook's defaults, E7.8).
    Reach(ReachError),
    /// A request failed.
    Read(ReadError),
    /// No context of the Workspace root records a Workspace.
    NoContext,
    /// The context files of the root record more than one Workspace.
    AmbiguousWorkspace,
    /// A context file records an ID that is not a `UUIDv7`.
    ContextUnusable,
    /// P4-G: the context a write uses does not record what it needs: the
    /// Task, the Task's revision, or the Session's revision.
    ContextIncomplete(&'static str),
    /// P4-G: the configuration directory is inside the Workspace, so no
    /// context file can be written there.
    ConfigDirInWorkspace,
    /// P4-G: the Session started, but its context file was not written.
    ContextNotWritten,
    /// Neither a context nor the last Pack names the Project. U2 (H5,
    /// N-2): `directory_context` when the server has a client session and
    /// the directory's context covers the directory (only this client
    /// session has no context); the sentence says which.
    NoProject {
        /// Whether the directory's context exists here.
        directory_context: bool,
    },
    /// No context names a Task and none is in progress.
    NoTask,
    /// Several Tasks in progress in the Workspace.
    AmbiguousTask(usize),
    /// More Tasks in progress in the Project than are looked at.
    TooManyTasks(usize),
    /// The Task is completed or cancelled.
    TaskFinished(&'static str),
    /// U1 (A6, original #27): the Task the hook's rule m would read is
    /// completed or cancelled: the context of this client session names it
    /// (`client`), or the directory's does and no other Task is in
    /// progress.
    ChosenTaskFinished {
        /// `completed` or `cancelled`.
        state: &'static str,
        /// Whether this client session's context names it.
        client: bool,
    },
    /// No Task has this number.
    NoSuchTask {
        /// The number asked for.
        number: i64,
        /// How many Tasks the Project lists.
        total: usize,
    },
    /// The Capsule does not list this claim number.
    NoSuchClaim {
        /// The number asked for.
        number: u64,
        /// The numbers it lists (`#1–#N`).
        listed: String,
    },
    /// The Pack does not fit its budget even at level 5.
    OverBudget {
        /// The UTF-16 code units it needs.
        needed: usize,
        /// The budget asked for.
        budget: usize,
    },
    /// `verifyLedger` found a record that does not hold: the report.
    LedgerInvalid(JsonValue),
    /// Rule D: no context of the Workspace root names a Session.
    NoSessionContext,
    /// Rule D: several contexts of the Workspace root name a Session.
    AmbiguousSessionContext(usize),
    /// U1 (A2, Owner O3): `end` or `complete` under rule D in the hook's
    /// layout (a client session's context file of the root names a
    /// Session) on a server with no client session: nothing is sent.
    NoClientSession,
    /// The Capsule lists no Session with this number.
    NoSuchSession {
        /// The number asked for.
        number: i64,
        /// How many Sessions the Capsule lists.
        total: usize,
    },
    /// The Task Capsule is `tampered`: numbers are not resolved.
    CapsuleTampered,
    /// A run label the context does not list.
    NoSuchRun {
        /// The label (it matches the schema's pattern).
        label: String,
        /// The runs that can be cited, each as `r4 (PASS)`.
        citable: Vec<String>,
    },
    /// No run to cite: for a fact (`false`) or for completing the Task
    /// (`true`).
    NoRunToCite(bool),
    /// The latest run of a command did not pass: each as `r2 (FAIL)`.
    FailingRuns(Vec<String>),
    /// U8 K5 (Owner O5): a fact cites a run this context recorded as a
    /// command run (`command_result`); nothing is sent.
    CommandRunCited {
        /// The run's label.
        label: String,
        /// Its argv.
        argv: Vec<String>,
    },
    /// The context's Session is still active.
    SessionActive,
    /// U3 (K1, W-B-1, Owner O3): `complete` read the Session it concerns
    /// before sending `transitionTask`, and it is not active: `ended`,
    /// `crashed`, or (`None`) not listed. Nothing is sent.
    SessionNotActive(Option<&'static str>),
    /// P4-G: the Checkpoint needs the Snapshot of a run in the Session,
    /// and the context records none.
    NoSnapshot,
    /// P4-G: the claim a reaffirmation names is not an active fact.
    NotReaffirmable(u64),
    /// U3 G5 (Owner O8): `pack`'s `query` holds a control character or a
    /// string in the format of a memprove token; the query is not
    /// repeated and nothing is sent. Its code is `invalid_arguments`.
    QueryRefused,
}

impl Failure {
    /// The failure code, or `None` for a programming error.
    fn code(&self) -> Option<&'static str> {
        Some(match self {
            Self::InvalidArguments(_)
            | Self::UnknownArgument
            | Self::Combination(..)
            | Self::QueryRefused => "invalid_arguments",
            Self::NoConfigDir => "no_config_dir",
            Self::Reach(error) => match error {
                ReachError::NoDataDir => "no_data_dir",
                ReachError::DataDirUnreadable | ReachError::DataDirMalformed => "data_dir_unusable",
                ReachError::DaemonNotRunning => "daemon_not_running",
                ReachError::NoToken => "no_token",
                ReachError::TokenUnusable => "token_unusable",
            },
            Self::Read(error) => match error.failure {
                ReadFailure::Unreachable => "daemon_unreachable",
                ReadFailure::InvalidToken => "token_unusable",
                ReadFailure::BadResponse => "bad_response",
                ReadFailure::Refused { .. } => "problem",
                ReadFailure::NotAllowed => return None,
            },
            Self::NoContext => "no_context",
            Self::AmbiguousWorkspace => "ambiguous_workspace",
            Self::ContextUnusable | Self::ContextIncomplete(_) => "context_unusable",
            Self::ConfigDirInWorkspace => "config_dir_in_workspace",
            Self::ContextNotWritten => "context_not_written",
            Self::NoProject { .. } => "no_project",
            Self::NoTask => "no_task",
            Self::AmbiguousTask(_) | Self::TooManyTasks(_) => "ambiguous_task",
            Self::TaskFinished(_) | Self::ChosenTaskFinished { .. } => "task_finished",
            Self::NoSuchTask { .. } => "no_such_task",
            Self::NoSuchClaim { .. } => "no_such_claim",
            Self::OverBudget { .. } => "over_budget",
            Self::LedgerInvalid(_) => "ledger_invalid",
            Self::NoSessionContext => "no_session_context",
            Self::AmbiguousSessionContext(_) => "ambiguous_session_context",
            Self::NoClientSession => "no_client_session",
            Self::NoSuchSession { .. } => "no_such_session",
            Self::CapsuleTampered => "capsule_tampered",
            Self::NoSuchRun { .. } => "no_such_run",
            Self::NoRunToCite(_) => "no_run_to_cite",
            Self::FailingRuns(_) => "failing_runs",
            Self::CommandRunCited { .. } => "command_run_cited",
            Self::SessionActive => "session_active",
            Self::SessionNotActive(_) => "session_not_active",
            Self::NoSnapshot => "no_snapshot",
            Self::NotReaffirmable(_) => "not_reaffirmable",
        })
    }

    /// The members after `failure` in `structuredContent`.
    fn extras(&self) -> Vec<(&'static str, JsonValue)> {
        match self {
            Self::InvalidArguments(parameter) => vec![("parameter", text(parameter))],
            Self::UnknownArgument => vec![("parameter", JsonValue::Null)],
            Self::QueryRefused => vec![("parameter", text("query"))],
            Self::Combination(parameter, _) => vec![("parameter", text(parameter))],
            Self::Read(ReadError {
                operation,
                failure: ReadFailure::Refused { status, code, .. },
            }) => vec![
                ("status", number(status)),
                ("code", optional(code.as_deref())),
                ("operation", text(operation.as_str())),
            ],
            Self::Read(ReadError { operation, .. }) => {
                vec![("operation", text(operation.as_str()))]
            }
            Self::AmbiguousTask(count)
            | Self::TooManyTasks(count)
            | Self::AmbiguousSessionContext(count) => vec![("count", number(count))],
            Self::NoSuchSession {
                number: asked,
                total,
            } => vec![("number", number(asked)), ("total", number(total))],
            Self::NoSuchRun { label, citable } => vec![
                ("label", text(label)),
                ("citable", texts(citable.iter().map(String::as_str))),
            ],
            Self::FailingRuns(runs) => vec![("runs", texts(runs.iter().map(String::as_str)))],
            Self::CommandRunCited { label, .. } => vec![("label", text(label))],
            Self::NotReaffirmable(claim) => vec![("number", number(claim))],
            Self::TaskFinished(state) | Self::ChosenTaskFinished { state, .. } => {
                vec![("state", text(state))]
            }
            Self::NoSuchTask {
                number: asked,
                total,
            } => {
                vec![("number", number(asked)), ("total", number(total))]
            }
            Self::NoSuchClaim {
                number: asked,
                listed,
            } => {
                vec![("number", number(asked)), ("listed", text(listed))]
            }
            Self::OverBudget { needed, budget } => {
                vec![("needed", number(needed)), ("budget", number(budget))]
            }
            Self::LedgerInvalid(report) => vec![("report", report.clone())],
            Self::SessionNotActive(state) => vec![("state", optional(*state))],
            _ => Vec::new(),
        }
    }

    /// The one fixed sentence of `content[0].text`.
    fn sentence(&self) -> String {
        match self {
            Self::InvalidArguments(parameter) => {
                format!("the argument {parameter} does not match the tool's inputSchema; nothing was sent")
            }
            Self::UnknownArgument => {
                "an argument outside the inputSchema is present; nothing was sent".to_owned()
            }
            Self::Combination(first, second) => {
                format!("the arguments {first} and {second} are not allowed together; nothing was sent")
            }
            Self::NoConfigDir => {
                "no configuration directory (none of MEMPROVE_CONFIG_DIR, XDG_CONFIG_HOME, HOME is set)"
                    .to_owned()
            }
            Self::Reach(error) => error.clause().to_owned(),
            Self::Read(error) => read_sentence(error),
            Self::NoContext => {
                format!(
                    "this directory has no memprove context, so the Workspace here cannot be \
                     resolved; {BEGIN_FIRST}"
                )
            }
            Self::AmbiguousWorkspace => {
                "this directory's context file records more than one Workspace".to_owned()
            }
            Self::ContextUnusable => "the ID the context file records is not a UUIDv7".to_owned(),
            Self::ContextIncomplete(what) => {
                format!("the context file records no{what}; nothing was sent")
            }
            Self::ConfigDirInWorkspace => {
                "the configuration directory is inside the Workspace, where the context file cannot \
                 be written; nothing was sent"
                    .to_owned()
            }
            Self::ContextNotWritten => {
                "the Session has begun, but the context file was not written; the session and \
                 abandoned arguments of end end it"
                    .to_owned()
            }
            Self::NoProject { directory_context } => no_project(*directory_context),
            Self::NoTask => {
                "this directory's context names no Task, and this Workspace has no in-progress Task"
                    .to_owned()
            }
            Self::AmbiguousTask(count) => format!(
                "this Workspace has {count} in-progress Tasks, and neither this client session's \
                 context nor this directory's names one of them; the task argument names one"
            ),
            Self::TooManyTasks(count) => format!(
                "this Project has {count} in-progress Tasks, and neither this client session's \
                 context nor this directory's names one of them; the task argument names one"
            ),
            Self::TaskFinished(state) => {
                format!("the Task is {state} and takes no new Session; new_task starts new work")
            }
            Self::ChosenTaskFinished { state, client } => chosen_finished(state, *client),
            Self::NoSuchTask { number, total } => {
                format!("the Project lists {total} Tasks; there is no Task number {number}")
            }
            Self::NoSuchClaim { number, listed } => {
                format!("the Capsule does not list #{number} (it lists {listed}); nothing was sent")
            }
            Self::OverBudget { needed, budget } => format!(
                "the Task's Context Pack does not fit the budget of {budget} UTF-16 code units (it needs {needed})"
            ),
            Self::LedgerInvalid(report) => ledger_sentence(report),
            Self::NoSessionContext
            | Self::AmbiguousSessionContext(_)
            | Self::NoClientSession
            | Self::NoSuchSession { .. }
            | Self::SessionActive
            | Self::SessionNotActive(_)
            | Self::NoSnapshot => self.session_sentence(),
            Self::CapsuleTampered => {
                "the Task's Capsule is judged tampered, so numbers are not resolved; nothing was sent; \
                 memprove ledger verify shows why"
                    .to_owned()
            }
            Self::NoSuchRun { .. }
            | Self::NoRunToCite(_)
            | Self::FailingRuns(_)
            | Self::CommandRunCited { .. } => self.run_sentence(),
            Self::NotReaffirmable(claim) => {
                format!(
                    "#{claim} is not an active fact; only an active fact can be reaffirmed; nothing \
                     was sent"
                )
            }
            Self::QueryRefused => QUERY_REFUSED.to_owned(),
        }
    }

    /// The sentences about the context's Session and the Sessions of the
    /// Capsule (split from [`Self::sentence`] for its length).
    fn session_sentence(&self) -> String {
        match self {
            Self::NoSessionContext => {
                "this directory has no context that records a Session; begin comes first".to_owned()
            }
            Self::AmbiguousSessionContext(count) => {
                format!(
                    "this directory has {count} contexts that record a Session; the session argument \
                     names one"
                )
            }
            Self::NoClientSession => NO_CLIENT_SESSION.to_owned(),
            Self::NoSuchSession { number, total } => {
                format!("the Task's Capsule lists {total} Sessions; there is no Session number {number}")
            }
            Self::SessionActive => {
                "the context's Session is still active, so no new Session was begun; end comes first"
                    .to_owned()
            }
            Self::SessionNotActive(state) => format!(
                "the context's Session is not an active Session (state {}); nothing was sent; \
                 completing the Task needs an active Session: begin, then memprove run on the \
                 command line, then complete",
                state.unwrap_or("not listed")
            ),
            Self::NoSnapshot => "a Checkpoint needs the snapshot a memprove run of this Session \
                 sealed, and the context records none; nothing was sent; memprove run on the \
                 command line comes first"
                .to_owned(),
            _ => unreachable!("not a Session sentence"),
        }
    }
}

impl Failure {
    /// The sentence of a failure about the runs a statement may cite.
    fn run_sentence(&self) -> String {
        match self {
            Self::NoSuchRun { label, citable } => format!(
                "{label} is not a run of the current repository state that can be cited; the runs \
                 that can be cited: {}",
                if citable.is_empty() {
                    "none".to_owned()
                } else {
                    citable.join(", ")
                }
            ),
            Self::NoRunToCite(false) => {
                "a fact needs a passed test run as evidence, and the context records no run".to_owned()
            }
            Self::NoRunToCite(true) => {
                "completing the Task needs a passed run as evidence, and the context records no run; \
                 nothing was sent"
                    .to_owned()
            }
            Self::FailingRuns(runs) => format!(
                "in the current repository state the latest run of a command did not pass ({}); \
                 nothing was sent; a rerun, or the evidence argument naming the runs to cite, \
                 resolves it",
                runs.join(", ")
            ),
            Self::CommandRunCited { label, argv } => {
                crate::run::command_run_cannot_support_fact(label, argv)
            }
            _ => String::new(),
        }
    }
}

/// U3 G5: the sentence of [`Failure::QueryRefused`]: the rule, never the
/// query.
const QUERY_REFUSED: &str = "the argument query holds a control character (Cc) or a string in the \
    format of a memprove token (43 characters of A-Z, a-z, 0-9, -, _ with at least one upper-case \
    letter, one lower-case letter, and one digit); the query is not repeated; nothing was sent";

/// U1 (A7, original #33): the first `begin` in a Workspace, as a command
/// line that runs once its placeholders are filled (`--project` alone is a
/// usage error).
const BEGIN_FIRST: &str = "one memprove begin --project <PROJECT_ID> --new-task <TITLE> on the \
    command line comes first (to continue an existing Task, --task <TASK_ID> replaces --new-task \
    <TITLE>)";

/// U2 (H5, N-2): the sentence of `no_project`: no context in the Workspace
/// at all, or (`directory_context`) the directory's context exists and only
/// this client session has none; both end with the whole `begin` command.
fn no_project(directory_context: bool) -> String {
    if directory_context {
        format!(
            "this directory has a context, but this client session has none and no Pack recorded a \
             Project; memprove context show without MEMPROVE_CLIENT_SESSION shows the Project \
             ID this directory's context records; {BEGIN_FIRST}"
        )
    } else {
        format!("this Workspace has no context and no Pack recorded a Project; {BEGIN_FIRST}")
    }
}

/// U1 (A6, original #27): the sentence of a Task rule m chose that is
/// finished: the Task and its state, and how to read another Task or start
/// one.
fn chosen_finished(state: &str, client: bool) -> String {
    let whose = if client {
        format!("the Task recorded by this client session's context is {state}")
    } else {
        format!(
            "the Task recorded by this directory's context is {state}, and this Workspace has no \
             in-progress Task"
        )
    };
    format!(
        "{whose}; pack and claims read another Task through the task argument, and begin starts a \
         new Task through new_task"
    )
}

/// U1 (A2, Owner O3): the sentence of `no_client_session`.
const NO_CLIENT_SESSION: &str = "this directory has a client session context that records a Session, \
    but this server has no client session (neither MEMPROVE_CLIENT_SESSION nor \
    CLAUDE_CODE_SESSION_ID has a usable value), so the context file to write back is undetermined; \
    nothing was sent; memprove end or memprove task complete in that client session's shell \
    does it";

/// The sentence of a failed request.
fn read_sentence(error: &ReadError) -> String {
    match &error.failure {
        ReadFailure::Unreachable if is_write(error.operation) => {
            "cannot connect to memproved; whether this write took effect is unknown".to_owned()
        }
        ReadFailure::Unreachable | ReadFailure::NotAllowed => {
            "cannot connect to memproved".to_owned()
        }
        ReadFailure::InvalidToken => ReachError::TokenUnusable.clause().to_owned(),
        ReadFailure::BadResponse => "memproved's response is malformed or too large".to_owned(),
        ReadFailure::Refused {
            status,
            code,
            detail,
        } => {
            let sentence =
                problem_sentence(error.operation, *status, code.as_deref(), detail.as_deref());
            // U11 N1 (U10 VN5; Owner O6): the Problem code opens the text,
            // so that a transcript reader finds `role_forbidden` (or the
            // code of any other refusal) in it. A refusal with no code keeps
            // its sentence; every other failure is not a Problem answer.
            match code {
                Some(code) => format!("{code}: {sentence}"),
                None => sentence,
            }
        }
    }
}

/// Whether `operation` writes to the ledger: after its request was sent,
/// whether it took effect is unknown when the exchange failed.
const fn is_write(operation: OperationId) -> bool {
    matches!(
        operation,
        OperationId::CreateTask
            | OperationId::CreateSession
            | OperationId::TransitionTask
            | OperationId::EndSession
            | OperationId::CreateCheckpoint
            | OperationId::ProposeDecision
    )
}

/// The scope a refused `operation` needs, for the `403` sentence: the MCP
/// scope its `x-memprove-access` admits (`mcp-write` for a write).
fn scope_of(operation: OperationId) -> &'static str {
    let scopes = binding_for(operation.as_str())
        .map(|binding| binding.access_rule.scopes)
        .unwrap_or_default();
    ["mcp-write", "mcp-read"]
        .into_iter()
        .find(|scope| scopes.contains(scope))
        .unwrap_or("control")
}

/// U2 (S1, N-3, Owner O4): the clause that relays a `400` detail of a
/// write: `; memproved says: <detail>`, or nothing when the detail is
/// absent, is not printable ASCII (memproved writes its details in it),
/// is longer than `PROBLEM_DETAIL_MAX_CHARS` characters, or holds a string
/// in the format of a memprove token (U5 P2: C4's shape as amended).
fn relayed(detail: Option<&str>) -> String {
    let Some(detail) = detail else {
        return String::new();
    };
    // U5 P2 (Owner O4 c): the one shape rule of C4 as amended.
    let token_shaped = memprove_domain::holds_token_shape(detail);
    let shown = !detail.is_empty()
        && detail.len() <= memprove_protocol::phase1::PROBLEM_DETAIL_MAX_CHARS
        && detail.bytes().all(|byte| matches!(byte, b' '..=b'~'))
        && !token_shaped;
    if shown {
        format!("; memproved says: {detail}")
    } else {
        String::new()
    }
}

/// One sentence per Problem code (ADR 0014 E7.7). The `detail` is relayed
/// only for a `400 invalid_request` of a write (U2, S1); otherwise only
/// whether there was one counts (`evidence_stale`).
fn problem_sentence(
    operation: OperationId,
    status: u16,
    code: Option<&str>,
    detail: Option<&str>,
) -> String {
    let Some(code) = code else {
        return format!("memproved refused the request ({status})");
    };
    match code {
        "invalid_request"
            if matches!(
                operation,
                OperationId::CreateCheckpoint | OperationId::ProposeDecision
            ) =>
        {
            format!(
                "the request is invalid ({status} invalid_request): a fact needs a passed test run; a \
                 pass overruled by a later run of the same command cannot be cited; only a fact can \
                 supersede a fact; withdrawing a fact needs withdraw_reason; a body is at most 4096 \
                 characters; a body holds no token{}",
                relayed(detail)
            )
        }
        "invalid_request" if is_write(operation) => {
            // U10 N4 (U9 VN12): a completion that a daemon older than this
            // server refuses for `session_id` says which build to upgrade to.
            let upgrade = crate::older_daemon_hint(operation, status, detail)
                .map_or_else(String::new, |hint| format!("; {hint}"));
            format!(
                "the request is invalid ({status} invalid_request){}{upgrade}",
                relayed(detail)
            )
        }
        "invalid_request" | "unsupported_scope" => {
            format!("the request is invalid ({status} {code})")
        }
        "authentication_required" => {
            format!("memproved did not accept this token ({status} authentication_required)")
        }
        "policy_denied" => format!(
            "this token lacks the needed permission ({status} policy_denied; {} is needed)",
            scope_of(operation)
        ),
        // U9 R2 (Owner O4; ADR 0020): memproved's reason, word for word;
        // it is a fixed rule text. Nothing is checked locally beforehand.
        "role_forbidden" => role_detail(detail).map_or_else(
            || format!("memproved refused the request ({status} role_forbidden)"),
            str::to_owned,
        ),
        "project_not_found" | "workspace_not_resolved" | "resource_not_found" => {
            format!("memproved cannot find the object the context records ({status} {code})")
        }
        "project_workspace_mismatch" => {
            format!("the Task belongs to another Workspace ({status} project_workspace_mismatch)")
        }
        "revision_conflict" => {
            format!(
                "the revision has changed ({status} revision_conflict); a fresh read, then a retry"
            )
        }
        "invalid_transition" => {
            format!("the state does not allow this operation ({status} invalid_transition)")
        }
        "evidence_stale" if detail.is_some() => format!(
            "the Task's integrity check failed ({status} evidence_stale); a rerun does not help; \
             memprove ledger verify shows which record"
        ),
        "evidence_stale" => format!(
            "the cited evidence no longer holds ({status} evidence_stale): only a run that passed on \
             the same commit and in the same worktree state can be cited; commit, rerun, retry"
        ),
        "service_not_ready" => {
            format!(
                "memproved is temporarily unavailable ({status} service_not_ready); a retry may succeed"
            )
        }
        _ => format!("memproved refused the request ({status} {code})"),
    }
}

/// U9 R2: the `detail` of a `403 role_forbidden`, relayed as the whole
/// sentence when it is one line of at most `PROBLEM_DETAIL_MAX_CHARS`
/// characters without a control character or a string in the format of a
/// memprove token; `None` otherwise.
fn role_detail(detail: Option<&str>) -> Option<&str> {
    detail.filter(|detail| {
        !detail.is_empty()
            && detail.chars().count() <= memprove_protocol::phase1::PROBLEM_DETAIL_MAX_CHARS
            && !detail.chars().any(char::is_control)
            && !memprove_domain::holds_token_shape(detail)
    })
}

/// The sentence of `ledger_invalid`: the first record that does not hold,
/// by type, ID, and sequence. The daemon's `reason` text stays in the
/// report, as a JSON string value: it can quote stored values.
fn ledger_sentence(report: &JsonValue) -> String {
    let failure = report.get("first_failure");
    let field = |name: &str| failure.and_then(|failure| failure.get(name));
    let record_type = field("record_type")
        .and_then(JsonValue::as_str)
        .filter(|kind| kind.bytes().all(|byte| byte.is_ascii_lowercase()))
        .unwrap_or("record");
    let record_id = field("record_id")
        .and_then(JsonValue::as_str)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 64
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        .unwrap_or("(ID not shown)");
    let sequence = field("project_sequence")
        .and_then(JsonValue::as_i64)
        .map_or_else(|| "none".to_owned(), |sequence| sequence.to_string());
    format!(
        "the ledger does not verify; first inconsistency: {record_type} {record_id} (ledger sequence \
         {sequence}); the reason is in report.first_failure.reason"
    )
}

/// A tool error for `failure`, with `ids` first.
///
/// # Errors
///
/// [`Internal`] for a programming error (`-32603`).
pub(super) fn failed(ids: &Ids, failure: &Failure) -> Result<ToolResult, Internal> {
    let code = failure.code().ok_or(Internal)?;
    let mut members = ids.members();
    members.push(("failure", text(code)));
    members.extend(failure.extras());
    Ok(ToolResult {
        text: failure.sentence(),
        structured: object(members),
        structured_as_text: false,
        is_error: true,
        outcome: code,
        opening: None,
    })
}

/// A success: `ids` first, then `members`. Without a Pack, the text is the
/// compact `structuredContent` (M26). T21 (V6-F1): with one (`pack`,
/// `claims`), the first text block is the Pack's bytes, the second the
/// compact object, and there is no `structuredContent`.
pub(super) fn succeeded(
    ids: &Ids,
    members: Vec<(&'static str, JsonValue)>,
    pack: Option<String>,
) -> ToolResult {
    let mut all = ids.members();
    all.extend(members);
    let structured = object(all);
    let structured_as_text = pack.is_some();
    ToolResult {
        text: pack.unwrap_or_else(|| compact(&structured)),
        structured,
        structured_as_text,
        is_error: false,
        outcome: "ok",
        opening: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(operation: OperationId, status: u16, code: &str, detail: Option<&str>) -> Failure {
        Failure::Read(ReadError {
            operation,
            failure: ReadFailure::Refused {
                status,
                code: Some(code.to_owned()),
                detail: detail.map(str::to_owned),
            },
        })
    }

    #[test]
    fn every_failure_maps_to_one_code_and_one_fixed_sentence() {
        let ids = Ids {
            task_id: Some("018f47a0-0000-7000-8000-000000000004".to_owned()),
            session_id: None,
        };
        let result = failed(
            &ids,
            &refused(OperationId::GetTaskCapsule, 403, "policy_denied", None),
        )
        .unwrap();
        assert!(result.is_error);
        assert_eq!(
            compact(&result.structured),
            r#"{"task_id":"018f47a0-0000-7000-8000-000000000004","session_id":null,"failure":"problem","status":403,"code":"policy_denied","operation":"getTaskCapsule"}"#
        );
        assert_eq!(
            result.text,
            "policy_denied: this token lacks the needed permission (403 policy_denied; mcp-read is needed)"
        );
        let write = failed(
            &ids,
            &refused(OperationId::CreateCheckpoint, 403, "policy_denied", None),
        )
        .unwrap();
        assert!(
            write.text.ends_with("mcp-write is needed)"),
            "{}",
            write.text
        );
        let stale = |detail| {
            failed(
                &ids,
                &refused(OperationId::TransitionTask, 409, "evidence_stale", detail),
            )
            .unwrap()
            .text
        };
        assert!(stale(Some("d")).contains("integrity check failed"));
        assert!(stale(None).contains("commit, rerun, retry"));
        assert_eq!(
            failed(
                &ids,
                &refused(OperationId::ListTasks, 400, "invalid_request", Some("d"))
            )
            .unwrap()
            .text,
            "invalid_request: the request is invalid (400 invalid_request)"
        );
        assert!(
            failed(
                &ids,
                &refused(OperationId::CreateCheckpoint, 400, "invalid_request", None)
            )
            .unwrap()
            .text
            .contains("only a fact can supersede a fact")
        );
        // A programming error is never a tool result.
        let outside = Failure::Read(ReadError {
            operation: OperationId::GetStatus,
            failure: ReadFailure::NotAllowed,
        });
        assert_eq!(failed(&ids, &outside), Err(Internal));
    }

    /// U10 N4 (U9 VN12): the sentence of a `transitionTask` that a daemon
    /// older than this server refuses for `session_id` ends with the upgrade
    /// hint, and no other `400` of a write does.
    #[test]
    fn u10_n4_an_unknown_session_id_ends_with_the_upgrade_hint() {
        let ids = Ids::default();
        let sentence = |operation, status, detail: &str| {
            let failure = refused(operation, status, "invalid_request", Some(detail));
            match failed(&ids, &failure) {
                Ok(result) => result.text,
                Err(Internal) => panic!("a refusal is a tool result"),
            }
        };
        let hint = format!(
            "the daemon is older than this build (it does not know session_id); memproved {} knows it",
            crate::BUILD_ID
        );
        let unknown = "the request body has an unknown member session_id";
        let given = sentence(OperationId::TransitionTask, 400, unknown);
        assert_eq!(
            given,
            format!(
                "invalid_request: the request is invalid (400 invalid_request); memproved says: {unknown}; {hint}"
            )
        );
        for (operation, detail) in [
            (
                OperationId::TransitionTask,
                "$.session_id must name a Session of the Task",
            ),
            (OperationId::CreateSession, unknown),
        ] {
            let other = sentence(operation, 400, detail);
            assert!(!other.contains("the daemon is older"), "{other}");
        }
    }

    /// U9 R2 (Owner O4): a `403 role_forbidden` is memproved's `detail`,
    /// word for word; a detail that cannot be shown leaves the fixed
    /// sentence.
    #[test]
    fn a_role_forbidden_is_the_daemons_reason_word_for_word() {
        let ids = Ids::default();
        for rule in [
            memprove_domain::RoleRule::Decision,
            memprove_domain::RoleRule::Completion,
            memprove_domain::RoleRule::Edge,
        ] {
            let result = failed(
                &ids,
                &refused(
                    OperationId::ProposeDecision,
                    403,
                    "role_forbidden",
                    Some(rule.detail()),
                ),
            )
            .unwrap();
            assert!(result.is_error);
            // U11 N1 (Owner O6): the Problem code first, then memproved's
            // reason word for word.
            assert_eq!(result.text, format!("role_forbidden: {}", rule.detail()));
            assert_eq!(
                result.structured.get("code").and_then(JsonValue::as_str),
                Some("role_forbidden")
            );
        }
        let token = "u9R2_tokenShaped-0123456789abcdefghijklmnop";
        for detail in [None, Some(""), Some("a\nb"), Some(token)] {
            assert_eq!(
                failed(
                    &ids,
                    &refused(OperationId::CreateCheckpoint, 403, "role_forbidden", detail)
                )
                .unwrap()
                .text,
                "role_forbidden: memproved refused the request (403 role_forbidden)"
            );
        }
    }

    /// U2 (S1, N-3, Owner O4): a `400 invalid_request` of a write relays
    /// memproved's `detail` after the fixed sentence, which for
    /// `createCheckpoint` and `proposeDecision` also says that a body holds
    /// no token; a detail that is not printable ASCII, is too long, or holds
    /// a token-shaped run is not relayed; a read relays none.
    #[test]
    fn a_400_of_a_write_relays_the_detail_and_says_a_body_holds_no_token() {
        let ids = Ids::default();
        let member = "$.verified_completed_work[0].text holds a string in the format of a \
                      memprove token (43 characters of A-Z, a-z, 0-9, '-', and '_'); nothing \
                      was recorded";
        let sentence = |operation: OperationId, detail: Option<&str>| {
            failed(&ids, &refused(operation, 400, "invalid_request", detail))
                .unwrap()
                .text
        };
        assert_eq!(
            sentence(OperationId::CreateCheckpoint, Some(member)),
            format!(
                "invalid_request: the request is invalid (400 invalid_request): a fact needs a passed \
                 test run; a pass overruled by a later run of the same command cannot be cited; only a \
                 fact can supersede a fact; withdrawing a fact needs withdraw_reason; a body is at most \
                 4096 characters; a body holds no token; memproved says: {member}"
            )
        );
        assert!(
            sentence(
                OperationId::ProposeDecision,
                Some("$.content must be a string")
            )
            .ends_with("a body holds no token; memproved says: $.content must be a string")
        );
        assert_eq!(
            sentence(
                OperationId::CreateSession,
                Some("$.task_id must be a UUIDv7")
            ),
            "invalid_request: the request is invalid (400 invalid_request); memproved says: $.task_id must be a UUIDv7"
        );
        assert_eq!(
            sentence(OperationId::CreateSession, None),
            "invalid_request: the request is invalid (400 invalid_request)"
        );
        let token = "u2S1-tokenShaped_0123456789abcdefghijklmnop";
        for unshown in [
            format!("$.content holds {token}"),
            "two\nlines".to_owned(),
            "non-ASCII vérifié".to_owned(),
            "x".repeat(memprove_protocol::phase1::PROBLEM_DETAIL_MAX_CHARS + 1),
            String::new(),
        ] {
            let text = sentence(OperationId::ProposeDecision, Some(&unshown));
            assert!(text.ends_with("a body holds no token"), "{text}");
        }
        assert_eq!(
            sentence(OperationId::ListTasks, Some("$.limit must be 1 to 500")),
            "invalid_request: the request is invalid (400 invalid_request)"
        );
    }

    /// U11 N1 (U10 VN5; Owner O6): a tool error that memproved's Problem
    /// caused starts with the Problem code and a colon, so that a
    /// transcript reader finds `role_forbidden` (or any other code) in the
    /// text; a failure that did not come from a Problem answer, and a
    /// Problem answer with no code, keep the sentence they had.
    #[test]
    fn u11_n1_a_problem_result_starts_with_its_code() {
        let ids = Ids::default();
        for (operation, status, code) in [
            (OperationId::CreateCheckpoint, 403, "role_forbidden"),
            (OperationId::CreateCheckpoint, 403, "policy_denied"),
            (OperationId::TransitionTask, 409, "evidence_stale"),
            (OperationId::EndSession, 409, "revision_conflict"),
            (OperationId::CreateSession, 409, "invalid_transition"),
            (OperationId::CreateSession, 400, "invalid_request"),
            (OperationId::ListTasks, 404, "resource_not_found"),
            (OperationId::GetTaskCapsule, 503, "service_not_ready"),
            (OperationId::GetTaskCapsule, 418, "some_other_code"),
        ] {
            for detail in [None, Some("a fixed reason")] {
                let text = failed(&ids, &refused(operation, status, code, detail))
                    .unwrap()
                    .text;
                assert!(
                    text.starts_with(&format!("{code}: ")),
                    "{code} {detail:?}: {text}"
                );
                assert_eq!(text.lines().count(), 1, "{text}");
            }
        }
        // A Problem answer with no code, and failures of the server itself.
        let no_code = Failure::Read(ReadError {
            operation: OperationId::GetTaskCapsule,
            failure: ReadFailure::Refused {
                status: 500,
                code: None,
                detail: None,
            },
        });
        assert_eq!(
            failed(&ids, &no_code).unwrap().text,
            "memproved refused the request (500)"
        );
        for failure in [
            Failure::InvalidArguments("task".to_owned()),
            Failure::UnknownArgument,
            Failure::QueryRefused,
            Failure::NoContext,
            Failure::SessionActive,
            Failure::Read(ReadError {
                operation: OperationId::CreateCheckpoint,
                failure: ReadFailure::Unreachable,
            }),
        ] {
            let text = failed(&ids, &failure).unwrap().text;
            // U17: the prefix is `<code>: `; a sentence that opens with a
            // code-shaped word and a colon would read as one.
            let prefixed = text
                .split_once(": ")
                .is_some_and(|(head, _)| head.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
            assert!(!prefixed, "{failure:?}: {text}");
        }
    }

    /// Every sentence is one line and holds no `?` or `!`; every failure
    /// has its code.
    #[test]
    fn every_sentence_is_one_statement_line() {
        let ids = Ids::default();
        for failure in [
            Failure::InvalidArguments("task".to_owned()),
            Failure::UnknownArgument,
            Failure::NoConfigDir,
            Failure::Reach(ReachError::NoDataDir),
            Failure::Reach(ReachError::DataDirMalformed),
            Failure::Reach(ReachError::DaemonNotRunning),
            Failure::Reach(ReachError::NoToken),
            Failure::Reach(ReachError::TokenUnusable),
            Failure::NoContext,
            Failure::AmbiguousWorkspace,
            Failure::ContextUnusable,
            Failure::NoProject {
                directory_context: false,
            },
            Failure::NoProject {
                directory_context: true,
            },
            Failure::NoTask,
            Failure::AmbiguousTask(2),
            Failure::TooManyTasks(21),
            Failure::TaskFinished("completed"),
            Failure::ChosenTaskFinished {
                state: "cancelled",
                client: true,
            },
            Failure::ChosenTaskFinished {
                state: "completed",
                client: false,
            },
            Failure::NoSuchTask {
                number: 9,
                total: 3,
            },
            Failure::NoSuchClaim {
                number: 9,
                listed: "#1–#3".to_owned(),
            },
            Failure::OverBudget {
                needed: 1200,
                budget: 1000,
            },
            Failure::LedgerInvalid(JsonValue::Null),
            Failure::Combination("task", "new_task"),
            Failure::ContextIncomplete(" Task revision"),
            Failure::ConfigDirInWorkspace,
            Failure::ContextNotWritten,
            Failure::NoSessionContext,
            Failure::AmbiguousSessionContext(2),
            Failure::NoClientSession,
            Failure::NoSuchSession {
                number: 9,
                total: 3,
            },
            Failure::CapsuleTampered,
            Failure::NoSuchRun {
                label: "r9".to_owned(),
                citable: vec!["r1 (PASS)".to_owned()],
            },
            Failure::NoRunToCite(false),
            Failure::NoRunToCite(true),
            Failure::FailingRuns(vec!["r2 (FAIL)".to_owned()]),
            Failure::SessionActive,
            Failure::SessionNotActive(Some("ended")),
            Failure::SessionNotActive(None),
            Failure::NoSnapshot,
            Failure::NotReaffirmable(3),
            Failure::QueryRefused,
        ] {
            let result = failed(&ids, &failure).unwrap();
            assert_eq!(result.text.lines().count(), 1, "{failure:?}");
            assert!(!result.text.contains(['?', '？', '!', '！']), "{failure:?}");
            assert_eq!(
                result.structured.get("failure").and_then(JsonValue::as_str),
                failure.code(),
                "{failure:?}"
            );
        }
    }

    /// U1 (A7, original #33): the sentences of `no_context` and
    /// `no_project` give a `begin` command line that runs as it is once its
    /// placeholders are filled: `--project` with `--new-task` (or
    /// `--task`), which `begin` needs the first time in a Workspace.
    #[test]
    fn no_context_and_no_project_give_a_whole_begin_command() {
        let command = "memprove begin --project <PROJECT_ID> --new-task <TITLE>";
        for failure in [
            Failure::NoContext,
            Failure::NoProject {
                directory_context: false,
            },
            Failure::NoProject {
                directory_context: true,
            },
        ] {
            let sentence = failure.sentence();
            assert!(sentence.contains(command), "{sentence}");
            assert!(sentence.contains("--task <TASK_ID>"), "{sentence}");
            assert_eq!(sentence.lines().count(), 1);
        }
    }

    #[test]
    fn the_ledger_sentence_names_type_id_and_sequence_only() {
        let report = memprove_protocol::strict_json::parse_strict(
            r#"{"first_failure":{"record_type":"task","record_id":"018f47a0-0000-7000-8000-000000000004","project_sequence":null,"reason":"status verified「x」 at revision 2"}}"#,
        )
        .unwrap();
        let sentence = ledger_sentence(&report);
        assert_eq!(
            sentence,
            "the ledger does not verify; first inconsistency: task \
             018f47a0-0000-7000-8000-000000000004 (ledger sequence none); the reason is in \
             report.first_failure.reason"
        );
        assert!(!sentence.contains("verified"));
        let odd = memprove_protocol::strict_json::parse_strict(
            r#"{"first_failure":{"record_type":"event","record_id":"a b","project_sequence":7,"reason":"r"}}"#,
        )
        .unwrap();
        assert!(ledger_sentence(&odd).contains("event (ID not shown) (ledger sequence 7)"));
    }
}
