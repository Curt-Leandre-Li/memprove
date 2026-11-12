//! The five read tools (ADR 0014 E7.5.5, E7.6): `pack`, `claims`,
//! `context`, `tasks`, and `ledger_verify`. `pack` and `claims` return the
//! bytes `memprove resume --pack` prints, from the same `resume`
//! functions; `context` reads local files only.

use std::io;

use memprove_domain::{PACK_BODY_KEPT, PackError};
use memprove_protocol::{
    phase1::OperationId,
    strict_json::{JsonValue, parse_strict},
};

use super::{
    MCP_PACK_DEFAULT_BUDGET,
    failure::{Failure, Ids, Internal, ToolResult, failed, succeeded},
    local::{Local, choice_failure},
    tools::Tool,
    wire::{number, object, optional, text},
};
use crate::{
    Request,
    claims::is_uuid_v7,
    context::{Context, ContextLookup},
    generate_idempotency_key,
    hook::{self, Basis, ReadError, ReadFailure, Reader},
    resume,
};

/// Tasks per page of `tasks` (Owner condition w: a default call of 20
/// titles of 120 CJK characters stays under the 10,000-token line).
pub(super) const TASKS_PAGE_SIZE: usize = 20;
/// The characters of a Task title that `tasks` and `context` show.
pub(super) const TITLE_SHOWN: usize = PACK_BODY_KEPT;
/// The most run labels `context` lists: the latest ones.
pub(super) const CONTEXT_RUNS_SHOWN: usize = 64;

/// Why a read tool did not succeed.
enum Failed {
    /// A tool error concerning `Ids`.
    Tool(Ids, Failure),
    /// A programming error or a local fault: `-32603`.
    Internal,
}

impl Failed {
    fn of(ids: &Ids) -> impl Fn(Failure) -> Self + '_ {
        move |failure| Self::Tool(ids.clone(), failure)
    }
}

fn finish(result: Result<ToolResult, Failed>) -> Result<ToolResult, Internal> {
    match result {
        Ok(result) => Ok(result),
        Err(Failed::Tool(ids, failure)) => failed(&ids, &failure),
        Err(Failed::Internal) => Err(Internal),
    }
}

/// A Capsule body the Pack cannot read: the daemon's response is not one
/// the CLI accepts (the CLI's exit 4).
const fn bad_capsule() -> Failure {
    Failure::Read(ReadError {
        operation: OperationId::GetTaskCapsule,
        failure: ReadFailure::BadResponse,
    })
}

/// The `task_id` of a `listTasks` item.
fn task_id_of(task: &JsonValue) -> Option<&str> {
    task.get("task_id").and_then(JsonValue::as_str)
}

/// The Project's Tasks numbered as the `task` parameter takes them: by
/// ascending `task_id` (`UUIDv7`, time-ordered), from 1, computed now (ADR
/// 0014 E7.5.2, E7.14 decision 12).
fn numbered(reader: &Reader, project: &str) -> Result<Vec<JsonValue>, Failure> {
    let mut tasks = reader.tasks(project).map_err(Failure::Read)?;
    tasks.retain(|task| task_id_of(task).is_some_and(is_uuid_v7));
    tasks.sort_by(|left, right| task_id_of(left).cmp(&task_id_of(right)));
    Ok(tasks)
}

/// The Task Capsule a `pack` or `claims` call reads, and the Task and
/// Session it concerns.
struct Target {
    ids: Ids,
    body: String,
    /// U1 (A4): the Project the Task was found in, which the last-Pack
    /// record names.
    project_id: String,
    /// U12 T2 (Owner O4): why the Task was chosen, as the opening writes it
    /// after `Context Pack` (the hook's [`hook::basis_clause`], or for a
    /// Task named by number the `task` parameter).
    why: String,
}

/// The Task of `task: n` (the n-th Task of the Project), else the Task the
/// `SessionStart` hook would choose (Owner condition m, the same function),
/// and its Capsule: judged against the Workspace of the context that
/// applies, as `resume` sends it.
fn target(local: &Local, tool: &Tool, arguments: &JsonValue) -> Result<Target, Failed> {
    let ids = local.ids();
    let fail = Failed::of(&ids);
    if let Some(asked) = arguments.get("task").and_then(JsonValue::as_i64) {
        let project = local.project().map_err(&fail)?;
        let reader = local.reader(tool).map_err(&fail)?;
        let tasks = numbered(&reader, &project).map_err(&fail)?;
        let task_id = usize::try_from(asked - 1)
            .ok()
            .and_then(|index| tasks.get(index))
            .and_then(task_id_of)
            .ok_or_else(|| {
                fail(Failure::NoSuchTask {
                    number: asked,
                    total: tasks.len(),
                })
            })?
            .to_owned();
        let context = local.context();
        let ids = Ids {
            session_id: context
                .filter(|context| context.task_id.as_deref() == Some(task_id.as_str()))
                .and_then(|context| context.session_id.clone()),
            task_id: Some(task_id.clone()),
        };
        let mut target = format!("/v1/tasks/{task_id}/recovery-capsule");
        if let Some(workspace) = context
            .map(|context| context.workspace_id.as_str())
            .filter(|workspace| is_uuid_v7(workspace))
        {
            target.push_str("?workspace_id=");
            target.push_str(workspace);
        }
        let body = reader
            .send(&Request::bare(OperationId::GetTaskCapsule, target))
            .map_err(|error| Failed::Tool(ids.clone(), Failure::Read(error)))?
            .body;
        return Ok(Target {
            ids,
            body,
            project_id: project,
            why: format!(", chosen by the call's task argument (Task number {asked})"),
        });
    }
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
        Failed::Tool(ids, failure)
    })?;
    let why = hook::basis_clause(&chosen.basis);
    let session_id = match chosen.basis {
        Basis::Client | Basis::Directory => chosen.context.and_then(|context| context.session_id),
        Basis::OnlyInProgress(_) => None,
    };
    Ok(Target {
        ids: Ids {
            task_id: Some(chosen.task_id),
            session_id,
        },
        body: chosen.body,
        project_id: chosen.project_id,
        why,
    })
}

/// `pack` (E7.5.5, E7.6): the Pack's bytes as `resume --pack --budget
/// <budget>` prints them, recorded as the last Pack as `resume --pack`
/// records it.
pub(super) fn pack(
    local: &Local,
    tool: &Tool,
    arguments: &JsonValue,
) -> Result<ToolResult, Internal> {
    finish(pack_call(local, tool, arguments))
}

fn pack_call(local: &Local, tool: &Tool, arguments: &JsonValue) -> Result<ToolResult, Failed> {
    let budget = arguments
        .get("budget")
        .and_then(JsonValue::as_i64)
        .and_then(|budget| usize::try_from(budget).ok())
        .unwrap_or(MCP_PACK_DEFAULT_BUDGET);
    // U3 G5 (Owner O8): a query is checked before anything is sent, and
    // never repeated.
    let query = arguments.get("query").and_then(JsonValue::as_str);
    if query.is_some_and(|query| memprove_domain::check_pack_query(query).is_err()) {
        return Err(Failed::Tool(local.ids(), Failure::QueryRefused));
    }
    // U6 K3 (U5 N4): nor may it hold the whole token the server holds, in
    // whatever format; when no reader can be made, the call goes on and
    // fails as it did.
    if let Some(query) = query
        && local
            .reader(tool)
            .is_ok_and(|reader| reader.token_in(query))
    {
        return Err(Failed::Tool(local.ids(), Failure::QueryRefused));
    }
    let Target {
        ids,
        body,
        project_id,
        why,
    } = target(local, tool, arguments)?;
    // U7 P1 (Owner O3): the Session of the server's own context (rule D,
    // step 1) is its own; every other open Session gets the takeover hint.
    let recorded = resume::recorded_pack(local.own.as_ref(), local.cwd.as_deref());
    let own = resume::Own::of(
        local
            .context()
            .and_then(|context| context.session_id.as_deref()),
        recorded.as_ref(),
    );
    let made = if let Some(query) = query {
        // U3 G5: the query Pack `resume --pack --query` prints; never
        // recorded as the last Pack.
        match resume::query_pack(&body, query, budget, own) {
            Ok(pack) => Ok(pack),
            // The domain's length is already the least budget the query
            // Pack fits at (the hook's correction is for the whole Pack).
            Err(resume::QueryPackFailure::Pack(PackError::OverBudget { needed, .. })) => {
                return Err(Failed::Tool(ids, Failure::OverBudget { needed, budget }));
            }
            Err(resume::QueryPackFailure::Pack(error)) => Err(error),
            Err(resume::QueryPackFailure::Refused(_)) => {
                return Err(Failed::Tool(ids, Failure::QueryRefused));
            }
            Err(resume::QueryPackFailure::Index(_)) => return Err(Failed::Internal),
        }
    } else {
        resume::pack(&body, budget, own)
    };
    let text = match made {
        Ok(pack) => pack,
        Err(PackError::OverBudget { needed, .. }) => {
            return Err(Failed::Tool(
                ids,
                Failure::OverBudget {
                    needed: hook::working_budget(&body, needed, own),
                    budget,
                },
            ));
        }
        Err(_) => return Err(Failed::Tool(ids, bad_capsule())),
    };
    // U12 T2 (Owner O4): a whole Pack comes after the opening the hook
    // writes; a query Pack has none.
    let opening = if query.is_none() {
        remember(local, &body, &text, budget, &project_id);
        Some(pack_opening(local, ids.task_id.as_deref(), &why))
    } else {
        None
    };
    Ok(pack_result(&ids, &body, text, budget, opening))
}

/// The success of `pack`: the text, its object (the Capsule's digest and
/// sequence, the Pack's digest, the budget), and (U12 T2) the opening.
fn pack_result(
    ids: &Ids,
    body: &str,
    text: String,
    budget: usize,
    opening: Option<String>,
) -> ToolResult {
    let capsule = parse_strict(body).unwrap_or(JsonValue::Null);
    let sequence = capsule
        .get("bindings")
        .and_then(|bindings| bindings.get("ledger"))
        .and_then(|ledger| ledger.get("project_sequence"))
        .and_then(JsonValue::as_i64)
        .map_or(JsonValue::Null, number);
    let digest = text
        .lines()
        .last()
        .and_then(|line| line.strip_prefix("Digest: "))
        .map(str::to_owned);
    let mut result = succeeded(
        ids,
        vec![
            (
                "capsule_digest",
                optional(capsule.get("capsule_digest").and_then(JsonValue::as_str)),
            ),
            ("project_sequence", sequence),
            ("pack_digest", optional(digest.as_deref())),
            ("budget", number(budget)),
        ],
        Some(text),
    );
    result.opening = opening;
    result
}

/// U12 T2 (Owner O4; ADR 0014 E7.5.5, "Corrected U12 T2"): the opening of a
/// whole Pack of `pack`, the hook's without its `SessionStart` words: the Task
/// and `why` it was chosen, the server's clock, and the agent of the
/// server's process (as `begin` and `end` read it, U10 T6), when it declares
/// a whole one.
fn pack_opening(local: &Local, task_id: Option<&str>, why: &str) -> String {
    let process = &local.environment.process;
    hook::statement_opening(
        task_id.unwrap_or_default(),
        None,
        why,
        &hook::time_sentence(process.now),
        hook::opening_identity(process).as_deref(),
    )
}

/// Records a whole Pack as this context's last Pack, as `resume --pack`
/// records it (E7.6), with the Task's Project (U1 A4).
fn remember(local: &Local, body: &str, pack: &str, budget: usize, project_id: &str) {
    resume::remember_pack(
        local.own.as_ref(),
        local
            .cwd
            .clone()
            .ok_or_else(|| io::Error::other("no working directory")),
        body,
        pack,
        budget,
        Some(project_id),
    );
}

/// `claims` (E7.5.5): the bytes `resume --pack --claims <numbers>` prints;
/// nothing is recorded. U1 (A5): at most [`MCP_PACK_DEFAULT_BUDGET`] UTF-16
/// code units; longer lines are cut the Pack's way, with an `Omitted: ` line.
pub(super) fn claims(
    local: &Local,
    tool: &Tool,
    arguments: &JsonValue,
) -> Result<ToolResult, Internal> {
    finish(claims_call(local, tool, arguments))
}

fn claims_call(local: &Local, tool: &Tool, arguments: &JsonValue) -> Result<ToolResult, Failed> {
    let mut numbers = arguments
        .get("claims")
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(JsonValue::as_i64)
        .filter_map(|claim| u64::try_from(claim).ok())
        .collect::<Vec<_>>();
    numbers.sort_unstable();
    let Target { ids, body, .. } = target(local, tool, arguments)?;
    let ranges = numbers
        .iter()
        .map(|claim| (*claim, *claim))
        .collect::<Vec<_>>();
    match resume::pack_claims_within(&body, &ranges, MCP_PACK_DEFAULT_BUDGET) {
        Ok(lines) => Ok(succeeded(
            &ids,
            vec![(
                "claims",
                JsonValue::Array(numbers.into_iter().map(number).collect()),
            )],
            Some(lines),
        )),
        Err(PackError::NoSuchClaim { number, listed }) => {
            Err(Failed::Tool(ids, Failure::NoSuchClaim { number, listed }))
        }
        Err(PackError::OverBudget { needed, budget }) => {
            Err(Failed::Tool(ids, Failure::OverBudget { needed, budget }))
        }
        Err(_) => Err(Failed::Tool(ids, bad_capsule())),
    }
}

/// The first [`TITLE_SHOWN`] characters of `title` (never half a
/// character) and its length in characters.
fn cut(title: &str) -> (JsonValue, JsonValue) {
    let shown = title.chars().take(TITLE_SHOWN).collect::<String>();
    (text(&shown), number(title.chars().count()))
}

/// The context as `context` shows it: [`Context::to_json`], with a long
/// Task title cut and only the latest `runs_shown` run labels (Owner
/// condition w); U1 (A5): the `begin` tool shows it so when its result
/// would be over the Pack's default budget.
pub(super) fn shown_context(context: &Context, runs_shown: usize) -> JsonValue {
    let Ok(JsonValue::Object(members)) = parse_strict(&context.to_json()) else {
        return JsonValue::Null;
    };
    let mut shown = Vec::new();
    for (name, value) in members {
        match name.as_str() {
            "task_title" => {
                let (title, chars) = value
                    .as_str()
                    .map_or((JsonValue::Null, JsonValue::Null), cut);
                shown.push((name, title));
                shown.push(("task_title_chars".to_owned(), chars));
            }
            "runs" => {
                let runs = value.as_array().unwrap_or_default();
                let from = runs.len().saturating_sub(runs_shown);
                shown.push((name, JsonValue::Array(runs[from..].to_vec())));
                shown.push(("runs_total".to_owned(), number(runs.len())));
            }
            _ => shown.push((name, value)),
        }
    }
    JsonValue::Object(shown)
}

/// `context` (E7.5.5): local files only, no token, no daemon.
pub(super) fn context(local: &Local) -> Result<ToolResult, Internal> {
    finish(context_call(local))
}

fn context_call(local: &Local) -> Result<ToolResult, Failed> {
    let ids = local.ids();
    let fail = Failed::of(&ids);
    let (Some(base), Some(own)) = (local.base.as_ref(), local.own.as_ref()) else {
        return Err(fail(Failure::NoConfigDir));
    };
    let root = local.root().ok_or_else(|| fail(Failure::NoContext))?;
    let lookup = match &local.lookup {
        ContextLookup::Found(_) => "found",
        ContextLookup::Unbegun { .. } => "unbegun",
        ContextLookup::Broken(_) => "broken",
        ContextLookup::Absent { .. } | ContextLookup::Unavailable => "absent",
    };
    let last_pack = match own.last_pack(&root) {
        Ok(Some(pack)) => object([
            ("task_id", text(&pack.task_id)),
            ("project_id", optional(pack.project_id.as_deref())),
            (
                "project_sequence",
                number(pack.resumed_from.project_sequence),
            ),
            (
                "pack_digest",
                text(&format!("sha256:{}", pack.resumed_from.pack_digest)),
            ),
            ("budget", number(pack.resumed_from.budget)),
        ]),
        Ok(None) | Err(_) => JsonValue::Null,
    };
    let with_session = base
        .contexts_of(&root)
        .iter()
        .filter(|context| context.session_id.is_some())
        .count();
    Ok(succeeded(
        &ids,
        vec![
            (
                "context_file",
                text(if own.client_session.is_some() {
                    "client"
                } else {
                    "directory"
                }),
            ),
            ("lookup", text(lookup)),
            (
                "context",
                local.context().map_or(JsonValue::Null, |context| {
                    shown_context(context, CONTEXT_RUNS_SHOWN)
                }),
            ),
            ("last_pack", last_pack),
            ("contexts_with_session", number(with_session)),
        ],
        None,
    ))
}

/// `tasks` (E7.5.5): the Project's Tasks with their numbers, a page at a
/// time.
pub(super) fn tasks(
    local: &Local,
    tool: &Tool,
    arguments: &JsonValue,
) -> Result<ToolResult, Internal> {
    finish(tasks_call(local, tool, arguments))
}

fn tasks_call(local: &Local, tool: &Tool, arguments: &JsonValue) -> Result<ToolResult, Failed> {
    let ids = local.ids();
    let fail = Failed::of(&ids);
    let page = arguments
        .get("page")
        .and_then(JsonValue::as_i64)
        .map_or(Some(1), |page| usize::try_from(page).ok())
        .ok_or_else(|| fail(Failure::InvalidArguments("page".to_owned())))?;
    let project = local.project().map_err(&fail)?;
    let reader = local.reader(tool).map_err(&fail)?;
    let tasks = numbered(&reader, &project).map_err(&fail)?;
    let pages = tasks.len().div_ceil(TASKS_PAGE_SIZE).max(1);
    if page > pages {
        return Err(fail(Failure::InvalidArguments("page".to_owned())));
    }
    let listed = tasks
        .iter()
        .enumerate()
        .skip((page - 1) * TASKS_PAGE_SIZE)
        .take(TASKS_PAGE_SIZE)
        .map(|(index, task)| {
            let member = |name: &str| task.get(name).cloned().unwrap_or(JsonValue::Null);
            let (title, chars) = task
                .get("title")
                .and_then(JsonValue::as_str)
                .map_or((JsonValue::Null, JsonValue::Null), cut);
            object([
                ("number", number(index + 1)),
                ("task_id", member("task_id")),
                ("title", title),
                ("title_chars", chars),
                ("state", member("state")),
                ("revision", member("revision")),
                ("workspace_id", member("workspace_id")),
            ])
        })
        .collect();
    Ok(succeeded(
        &ids,
        vec![
            ("project_id", text(&project)),
            ("page", number(page)),
            ("page_size", number(TASKS_PAGE_SIZE)),
            ("total", number(tasks.len())),
            ("tasks", JsonValue::Array(listed)),
            (
                "next_page",
                if page < pages {
                    number(page + 1)
                } else {
                    JsonValue::Null
                },
            ),
        ],
        None,
    ))
}

/// `ledger_verify` (E7.5.5): the report `memprove ledger verify` prints;
/// a ledger that does not hold is a tool error that carries the report
/// (E7.14 decision 7).
pub(super) fn ledger_verify(local: &Local, tool: &Tool) -> Result<ToolResult, Internal> {
    finish(ledger_call(local, tool))
}

fn ledger_call(local: &Local, tool: &Tool) -> Result<ToolResult, Failed> {
    let ids = local.ids();
    let fail = Failed::of(&ids);
    let project = local.project().map_err(&fail)?;
    let reader = local.reader(tool).map_err(&fail)?;
    let mut request = Request::bare(
        OperationId::VerifyLedger,
        "/v1/verification/ledger".to_owned(),
    );
    request.body = Some(format!(
        r#"{{"schema_version":1,"project_id":{}}}"#,
        crate::json_string(&project)
    ));
    request.idempotency_key = Some(generate_idempotency_key().map_err(|_| Failed::Internal)?);
    let reply = reader
        .send(&request)
        .map_err(|error| fail(Failure::Read(error)))?;
    let report = parse_strict(&reply.body).map_err(|_| {
        fail(Failure::Read(ReadError {
            operation: OperationId::VerifyLedger,
            failure: ReadFailure::BadResponse,
        }))
    })?;
    if report.get("valid").and_then(JsonValue::as_bool) == Some(true) {
        Ok(succeeded(&ids, vec![("report", report)], None))
    } else {
        Err(fail(Failure::LedgerInvalid(report)))
    }
}
