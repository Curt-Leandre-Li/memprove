//! The ten tools (ADR 0014 E7.5): names, descriptions, input schemas, and
//! annotations in the fixed order of `tools/list`, and the closed set of
//! daemon operations each tool may send (E7.5.1). The list is built once
//! and is byte-identical in both protocol eras and on every call.

use std::sync::OnceLock;

use memprove_protocol::{
    phase1::OperationId,
    strict_json::{JsonValue, parse_strict},
};

use super::{
    failure::{Internal, ToolResult, failed},
    local::Local,
    read, schema,
    wire::{object, text},
    write,
};

/// Whether a tool reads or writes the ledger (Owner condition t).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// Reads only; a token with `mcp-read` serves it.
    Read,
    /// Writes; its writes need `mcp-write`.
    Write,
}

/// One tool.
#[derive(Debug)]
pub(super) struct Tool {
    /// Its name; Claude Code shows it as `mcp__memprove__<name>`.
    pub(super) name: &'static str,
    /// Read or write.
    pub(super) kind: Kind,
    /// Its description: declarative sentences only (condition y).
    pub(super) description: &'static str,
    /// Its `inputSchema`, JSON Schema 2020-12, as compact JSON text.
    pub(super) schema: &'static str,
    /// The only daemon operations it may send; any other is refused before
    /// anything is sent (ADR 0014 E7.5.1).
    pub(super) operations: &'static [OperationId],
}

/// U10 SF2 (final baseline B-3): ADR 0021's takeover rule, in the words
/// that end the descriptions of `begin`'s `agent_name` and `end`'s
/// `abandoned` (Owner O8 (d) lets `tools/list` differ in those two). The
/// rule is the guide's "Which open Session to end": the same three
/// conditions, as statements (U11 N6), held to this text word for word and
/// to the guide point by point by the tests; a client that loads these two
/// tools reads it here. A literal, so that `concat!`
/// puts it in both compact schemas; it holds no `"` and no `\`. U11 T1, T2
/// (Owner O3, O4; ADR 0021 "Amendment (U11)"): (a)'s "you" is the agent the
/// hook's or pack's opening names (`You are agent「…」`), else the `agent_name` declared,
/// and (b)'s current time is the one the opening states (`The current UTC time is …`), else
/// the client's own clock; the conditions and the 15 minutes are unchanged.
/// U11 S-F round 1 (ADR 0021 "Amendment (U11, S-F round 1, 2026-11-04)"):
/// (c)'s parent is the one the opening gives (`parent Session …`), else the
/// `parent_session` declared, and an opening that names the client without
/// one means it has none; (c) no longer points at `MEMPROVE_PARENT_SESSION`.
macro_rules! takeover_rule {
    () => {
        "In an unattended takeover (ADR 0021; an interactive reader asks the person instead), \
         an open Session another agent left, whose Session k line in the Pack gives its last \
         write (last wrote to the ledger at) and ends with the takeover hint (end it before \
         taking over), is ended with end {session:k, abandoned:true} before begin when all \
         three hold: (a) its agent is not you: its name differs from your name, the one the \
         hook's or pack's opening gives you (You are agent「…」) or, when it gives none, the \
         agent_name you declare; or it has no name and you have one; (b) its last write is \
         more than 15 minutes older than the current UTC time, the one the hook's or pack's \
         opening gives (The current UTC time is …) or your own clock, or the Session that \
         dispatched it (dispatched by Session j) has ended, or the operator's prompt says the \
         previous run is gone; (c) it is not your own parent: the Session the hook's or pack's \
         opening gives you (parent Session …) or, when the opening names no agent, the \
         parent_session you declare; when the opening names you without a parent Session, you \
         have no parent. No record shows whether a client is still alive; the three conditions \
         decide it. When one does not hold, that Session stays open: begin runs as usual, and \
         this session records an open item another Session (Session k) may still be working, \
         not ended in its first checkpoint."
    };
}

/// Every tool, in the order of `tools/list`.
pub(super) const TOOLS: [Tool; 10] = [
    Tool {
        name: "pack",
        kind: Kind::Read,
        description: "Returns the Context Pack of a Task: the bytes `memprove resume --pack \
            --budget <budget>` prints, whose last line is the Pack's digest. The default budget \
            is 6000 UTF-16 code units; the hook and the CLI use 9000, which `budget` selects. \
            Without `task` the Task is chosen as the SessionStart hook chooses it: the context \
            of this server's client session, else the directory's context, else the only Task \
            in progress in the Workspace. A whole Pack is recorded as this context's last Pack, \
            which the next `begin` declares as resumed_from. Without query, the first text \
            block is the opening the SessionStart hook writes, without its \
            SessionStart words: the Task and why it was chosen, this server's current UTC time \
            (The current UTC time is …), and, when this server's process has MEMPROVE_AGENT_NAME and \
            MEMPROVE_AGENT_ROLE, the agent it names (You are agent「…」, with parent Session when \
            MEMPROVE_PARENT_SESSION is set); the Pack is the next block. Reading needs \
            mcp-read.",
        schema: r#"{"type":"object","additionalProperties":false,"properties":{"task":{"type":"integer","minimum":1,"description":"The Task's number as the tasks tool lists it."},"budget":{"type":"integer","minimum":1000,"maximum":1000000,"description":"UTF-16 code units; default 6000."},"query":{"type":"string","minLength":1,"maxLength":256,"description":"Text with no control character and no memprove token. The Pack then lists only the statements that match it, best first (SQLite FTS5, trigram tokenizer, bm25), each followed by the statements that supersede it, then the current next action: the bytes memprove resume --pack --query <query> --budget <budget> prints. A word of one or two characters matches only as a whole word; a word that is a kind label (fact, open_item, decision, next_action) matches the statements of that kind; the statements every word matches come first and, within that, the current ones before the superseded and withdrawn ones. Nothing is recorded as the last Pack then."}}}"#,
        operations: &[OperationId::GetTaskCapsule, OperationId::ListTasks],
    },
    Tool {
        name: "claims",
        kind: Kind::Read,
        description: "Returns the full Pack lines of the claims named, superseded and withdrawn \
            ones too, after the base lines they reference: the bytes `memprove resume --pack \
            --claims <numbers>` prints when they fit in 6000 UTF-16 code units, the Pack's \
            default budget. Longer lines are cut as the Pack cuts them (changed files counted, \
            bodies shortened to 120 characters, then omitted, then claims not listed), and a \
            last Omitted: line says what was cut; that command prints them whole. Reading needs \
            mcp-read.",
        schema: r#"{"type":"object","additionalProperties":false,"required":["claims"],"properties":{"task":{"type":"integer","minimum":1},"claims":{"type":"array","minItems":1,"maxItems":500,"uniqueItems":true,"items":{"type":"integer","minimum":1}}}}"#,
        operations: &[OperationId::GetTaskCapsule, OperationId::ListTasks],
    },
    Tool {
        name: "context",
        kind: Kind::Read,
        description: "Returns the context this server reads and writes for this Workspace root, \
            from local files only and without a token: which context file is in use, the \
            Project, Workspace, agent, Task (title, state and revision as last seen, not a \
            fresh read), Session, the latest run, and the run labels that can be cited. A Task \
            title over 120 characters is cut to its first 120, with task_title_chars giving its \
            length, and at most the latest 64 run labels are listed, with runs_total giving \
            their number. `contexts_with_session` counts the context files of this root that \
            name an active Session, which the write tools use when this server's own context \
            names none.",
        schema: r#"{"type":"object","additionalProperties":false}"#,
        operations: &[],
    },
    Tool {
        name: "tasks",
        kind: Kind::Read,
        description: "Lists the Tasks of the context's Project with the numbers the `task` \
            parameter takes: the n-th Task by ascending task_id, 20 per page. A title over 120 \
            characters is cut to its first 120, with title_chars giving its length. A number \
            keeps naming the same Task when Tasks are added. Reading needs mcp-read.",
        schema: r#"{"type":"object","additionalProperties":false,"properties":{"page":{"type":"integer","minimum":1}}}"#,
        operations: &[OperationId::ListTasks],
    },
    Tool {
        name: "ledger_verify",
        kind: Kind::Read,
        description: "Recomputes the Project's Event chain and every Evidence, Snapshot, and \
            Checkpoint digest, writes nothing, and returns the report `memprove ledger \
            verify` prints. A ledger that does not hold is an error result naming the first \
            record that does not hold. Reading needs mcp-read.",
        schema: r#"{"type":"object","additionalProperties":false}"#,
        operations: &[OperationId::VerifyLedger],
    },
    Tool {
        name: "begin",
        kind: Kind::Write,
        description: "Starts a Session on the Task and records it as the context of this \
            server, as `memprove begin` does: it checks that the context's Session has ended, \
            resolves the Workspace, creates the Task for `new_task`, refuses a completed or \
            cancelled Task, declares this context's last Pack as resumed_from when it is of the \
            same Task, moves a proposed or ready Task to in_progress, and writes the context \
            file. The Task is `task` or `new_task` when one is given, else the context's, else \
            the last Pack's. The Project comes from the context or the last Pack; a Workspace \
            with neither needs one `memprove begin --project <PROJECT_ID> --new-task <TITLE>` \
            (or `--task <TASK_ID>`) from the command line first. Writing needs mcp-write.",
        schema: concat!(
            r#"{"type":"object","additionalProperties":false,"properties":{"task":{"type":"integer","minimum":1},"new_task":{"type":"object","additionalProperties":false,"required":["title"],"properties":{"title":{"type":"string","minLength":1,"maxLength":4096},"description":{"type":"string","minLength":1,"maxLength":4096}}},"agent_name":{"type":"string","minLength":1,"maxLength":64,"pattern":"^[A-Za-z0-9_-]{1,64}$","description":"When the hook's or pack's opening names you, the server uses the agent it was started with, and begin and end need no agent input. U8: the Session's agent name, as begin --agent-name; it needs agent_role. The three inputs agent_name, agent_role, and parent_session are one declaration: give the name and the role together (the parent when the agent was dispatched by a Session), or none of the three to use the server's environment (MEMPROVE_AGENT_NAME, MEMPROVE_AGENT_ROLE, MEMPROVE_PARENT_SESSION, which a client passes on to the server it starts, so a client started as a sub_agent begins a sub_agent Session), else to continue the context's agent, whole; a name without a role, a role without a name, or a parent alone, in the inputs or in the environment, is invalid_arguments, told before a missing Project, Task, or context is. "#,
            takeover_rule!(),
            r#""},"agent_role":{"type":"string","enum":["orchestrator","sub_agent","human"],"maxLength":12,"description":"U8: the agent's role, as begin --agent-role; it needs agent_name, and the three are one declaration (see agent_name: all together, or none to use the server's environment, else the context's agent, else invalid_arguments)."},"parent_session":{"type":"string","minLength":36,"maxLength":36,"description":"U8: the Session ID that dispatched this agent, as begin --parent-session; it needs agent_name and agent_role, and the three are one declaration (all together, or none to use the server's environment, else the context's agent, else invalid_arguments)."}}}"#
        ),
        operations: &[
            OperationId::ListSessions,
            OperationId::ResolveWorkspace,
            OperationId::CreateTask,
            OperationId::ListTasks,
            OperationId::CreateSession,
            OperationId::TransitionTask,
        ],
    },
    Tool {
        name: "checkpoint",
        kind: Kind::Write,
        description: "Records a Checkpoint of the Task in the Session, as `memprove \
            checkpoint create` does: facts (a fact needs a passed test run and cites the latest \
            run when `evidence` is omitted), open items (they cite nothing unless `evidence` is \
            given), reaffirmations by claim number, `reaffirm_all` (every fact that awaits one, \
            on the latest runs of the same commands, or on `evidence`), withdrawals by claim \
            number with a reason, and the next action, which replaces the one in force. \
            Statements are recorded in the order facts, open items, reaffirmations, then \
            reaffirm_all's expansion; the new numbers follow that order. Claim numbers are the \
            Pack's #n; run labels are the rN `memprove run` printed in this Session, or last. \
            A tampered Task Capsule or an unlisted number sends nothing. A Session whose agent \
            is a sub_agent may supersede only open items and the statements of Sessions of its \
            own agent name, and withdraw only the latter; the daemon refuses any other target \
            with 403 role_forbidden, and the orchestrator or a person writes it. Writing needs \
            mcp-write.",
        schema: concat!(
            r#"{"type":"object","additionalProperties":false,"required":["next"],"properties":{"#,
            r##""facts":{"type":"array","maxItems":64,"uniqueItems":true,"items":{"$ref":"#/$defs/statement"}},"##,
            r##""open_items":{"type":"array","maxItems":64,"uniqueItems":true,"items":{"$ref":"#/$defs/statement"}},"##,
            r##""reaffirm":{"type":"array","maxItems":64,"uniqueItems":true,"items":{"type":"object","additionalProperties":false,"required":["claim"],"properties":{"claim":{"type":"integer","minimum":1},"evidence":{"$ref":"#/$defs/labels"}}}},"##,
            r##""reaffirm_all":{"type":"object","additionalProperties":false,"properties":{"evidence":{"$ref":"#/$defs/labels"}}},"##,
            r##""next":{"type":"object","additionalProperties":false,"required":["text"],"properties":{"text":{"type":"string","minLength":1,"maxLength":4096},"supersedes":{"$ref":"#/$defs/claims"}}},"##,
            r##""withdraw":{"$ref":"#/$defs/claims"},"##,
            r#""withdraw_reason":{"type":"string","minLength":1,"maxLength":4096},"#,
            r#""objective":{"type":"string","minLength":1,"maxLength":4096},"#,
            r#""session":{"type":"integer","minimum":1}},"#,
            r##""$defs":{"##,
            r##""statement":{"type":"object","additionalProperties":false,"required":["text"],"properties":{"text":{"type":"string","minLength":1,"maxLength":4096},"evidence":{"$ref":"#/$defs/labels"},"supersedes":{"$ref":"#/$defs/claims"}}},"##,
            r#""labels":{"type":"array","minItems":1,"maxItems":64,"uniqueItems":true,"items":{"type":"string","maxLength":21,"pattern":"^(r[1-9][0-9]*|last)$"}},"#,
            r#""claims":{"type":"array","minItems":1,"maxItems":500,"uniqueItems":true,"items":{"type":"integer","minimum":1}}}}"#
        ),
        operations: &[
            OperationId::GetTaskCapsule,
            OperationId::CreateCheckpoint,
            OperationId::ListTasks,
            OperationId::ListSessions,
        ],
    },
    Tool {
        name: "decide",
        kind: Kind::Write,
        description: "Records a Decision of the Task in the Session, as `memprove decide` \
            does: the text, an optional rationale, optional cited runs (none by default), and \
            the claim numbers it supersedes (an earlier Decision too; a fact can be superseded \
            only by a fact). Its verdict follows a fact's rule and never counts toward the \
            Task's verdict. A Session whose agent is a sub_agent records no Decision: the daemon \
            answers 403 role_forbidden, and the orchestrator or a person records it. Writing \
            needs mcp-write.",
        schema: concat!(
            r#"{"type":"object","additionalProperties":false,"required":["text"],"properties":{"#,
            r#""text":{"type":"string","minLength":1,"maxLength":4096},"#,
            r#""rationale":{"type":"string","minLength":1,"maxLength":4096},"#,
            r#""evidence":{"type":"array","minItems":1,"maxItems":64,"uniqueItems":true,"items":{"type":"string","maxLength":21,"pattern":"^(r[1-9][0-9]*|last)$"}},"#,
            r#""supersedes":{"type":"array","minItems":1,"maxItems":500,"uniqueItems":true,"items":{"type":"integer","minimum":1}},"#,
            r#""session":{"type":"integer","minimum":1}}}"#
        ),
        operations: &[
            OperationId::GetTaskCapsule,
            OperationId::ProposeDecision,
            OperationId::ListTasks,
            OperationId::ListSessions,
        ],
    },
    Tool {
        name: "end",
        kind: Kind::Write,
        description: "Ends the Session, as `memprove end` does, and clears it from the \
            context file that names it. `abandoned` records session.crashed instead of \
            session.ended and is for a Session another agent left; `session` names such a \
            Session by its Pack number and requires `abandoned`. The reason is recorded in the \
            Event and readable only with a control token. Writing needs mcp-write.",
        schema: concat!(
            r#"{"type":"object","additionalProperties":false,"properties":{"reason":{"type":"string","minLength":1,"maxLength":4096},"abandoned":{"type":"boolean","description":"When the hook's or pack's opening names you, the server uses the agent it was started with, and begin and end need no agent input. True records session.crashed for a Session another agent left; it requires session. The agent inputs agent_name, agent_role, and parent_session describe the agent that ends it and are taken only with abandoned: given without it, they are invalid_arguments and nothing is sent. "#,
            takeover_rule!(),
            r#""},"session":{"type":"integer","minimum":1},"agent_name":{"type":"string","minLength":1,"maxLength":64,"pattern":"^[A-Za-z0-9_-]{1,64}$","description":"U9: with abandoned (without it, invalid_arguments), the agent that ends the Session, as begin's agent_name; it needs agent_role. The three inputs agent_name, agent_role, and parent_session are one declaration: give the name and the role together (the parent when the agent was dispatched by a Session), or none of the three to use the server's environment, else its context's agent, whole; a name without a role, a role without a name, or a parent alone is invalid_arguments, told before a missing context is."},"agent_role":{"type":"string","enum":["orchestrator","sub_agent","human"],"maxLength":12,"description":"U9: that agent's role, as begin's agent_role; it needs agent_name, and the three are one declaration (all together, or none to use the server's environment or its context's agent, else invalid_arguments)."},"parent_session":{"type":"string","minLength":36,"maxLength":36,"description":"U9: the Session ID that dispatched that agent, as begin's parent_session; it needs agent_name and agent_role, and the three are one declaration (all together, or none to use the server's environment or its context's agent, else invalid_arguments)."}}}"#
        ),
        operations: &[
            OperationId::EndSession,
            OperationId::GetTaskCapsule,
            OperationId::ListSessions,
            OperationId::ListTasks,
        ],
    },
    Tool {
        name: "complete",
        kind: Kind::Write,
        description: "Moves the Task to completed, as `memprove task complete` does: without \
            `evidence` it cites every passed run of the current repository state and refuses \
            locally while the latest run of some command did not pass; with `evidence` it \
            cites those runs as given. The daemon accepts only passed runs of this Task that \
            still hold in this Workspace and answers 409 evidence_stale otherwise; a tampered \
            Task is never completed. A Session whose agent is a sub_agent completes no Task: \
            the daemon answers 403 role_forbidden, whatever runs it cites, and the \
            orchestrator or a person completes it (here on their own context's runs; \
            `memprove task complete --evidence <EVIDENCE_ID>` cites any Session's run). \
            Writing needs mcp-write.",
        schema: concat!(
            r#"{"type":"object","additionalProperties":false,"properties":{"#,
            r#""evidence":{"type":"array","minItems":1,"maxItems":64,"uniqueItems":true,"items":{"type":"string","maxLength":21,"pattern":"^(r[1-9][0-9]*|last)$"}},"#,
            r#""session":{"type":"integer","minimum":1}}}"#
        ),
        operations: &[
            OperationId::TransitionTask,
            OperationId::GetTaskCapsule,
            OperationId::ListTasks,
            OperationId::ListSessions,
        ],
    },
];

/// The server's `instructions`, identical in both eras (ADR 0014 E7.10).
pub(super) const INSTRUCTIONS: &str = "memprove keeps a local ledger of each Task's facts, \
    open items, decisions, next action, and evidence. Read tools need a token with mcp-read and \
    write tools one with mcp-write; the token is read from the server's environment and never \
    from a parameter. Parameters are numbers and labels: claim numbers are the #n of the Pack, \
    run labels are the rN that `memprove run` prints in a shell, Session numbers are the Session k \
    of the Pack, Task numbers come from the tasks tool. No tool runs a command; tests are \
    recorded with `memprove run` in a shell and their labels are then cited here. Every \
    result names the Task and the Session it concerns. The pack tool's `query` returns a Pack \
    of only the statements that match it, best first, each followed by the statements that \
    supersede it, and the current next action: the context of one question. In a session the memprove \
    SessionStart hook started, `begin` and `end` work through these tools and in the shell \
    alike: both use the context of that client session, so each finds the Session the other \
    began.";

/// The tool named `name`.
pub(super) fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.name == name)
}

impl Tool {
    /// The parsed `inputSchema`.
    ///
    /// # Panics
    ///
    /// Never: the schemas are constants that a unit test parses.
    pub(super) fn input_schema(&self) -> JsonValue {
        parse_strict(self.schema).expect("every inputSchema is valid JSON")
    }

    /// The `annotations` of the tool (ADR 0014 E7.5.1).
    fn annotations(&self) -> JsonValue {
        let flag = JsonValue::Bool;
        match self.kind {
            Kind::Read => object([("readOnlyHint", flag(true)), ("openWorldHint", flag(false))]),
            Kind::Write => object([
                ("readOnlyHint", flag(false)),
                ("destructiveHint", flag(false)),
                ("idempotentHint", flag(false)),
                ("openWorldHint", flag(false)),
            ]),
        }
    }

    /// The tool's entry of `tools/list`.
    fn entry(&self) -> JsonValue {
        object([
            ("name", text(self.name)),
            ("description", text(self.description)),
            ("inputSchema", self.input_schema()),
            ("annotations", self.annotations()),
        ])
    }
}

/// The `tools` array of `tools/list`, built once.
pub(super) fn list() -> &'static JsonValue {
    static LIST: OnceLock<JsonValue> = OnceLock::new();
    LIST.get_or_init(|| JsonValue::Array(TOOLS.iter().map(Tool::entry).collect()))
}

/// Calls `tool` with `arguments` (an object, or none) in `local`: the
/// arguments are checked against the tool's `inputSchema` first, then the
/// tool runs. `Err` is a programming error (`-32603`).
pub(super) fn call(
    tool: &'static Tool,
    arguments: Option<&JsonValue>,
    local: &Local,
) -> Result<ToolResult, Internal> {
    let empty = JsonValue::Object(Vec::new());
    let arguments = arguments.unwrap_or(&empty);
    if let Err(failure) = schema::validate(&tool.input_schema(), arguments) {
        return failed(&local.ids(), &failure);
    }
    match tool.name {
        "pack" => read::pack(local, tool, arguments),
        "claims" => read::claims(local, tool, arguments),
        "context" => read::context(local),
        "tasks" => read::tasks(local, tool, arguments),
        "ledger_verify" => read::ledger_verify(local, tool),
        "begin" => write::begin(local, tool, arguments),
        "checkpoint" => write::checkpoint(local, tool, arguments),
        "decide" => write::decide(local, tool, arguments),
        "end" => write::end(local, tool, arguments),
        "complete" => write::complete(local, tool, arguments),
        _ => Err(Internal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::wire::compact;

    /// The `x-memprove-access` scopes of every operation in `OpenAPI`.
    fn openapi_scopes() -> Vec<(String, String, Vec<String>)> {
        let openapi = include_str!("../../../../openapi/control-api-v1.yaml");
        let mut found = Vec::new();
        let mut operation = None;
        for line in openapi.lines() {
            let line = line.trim();
            if let Some(id) = line.strip_prefix("operationId: ") {
                operation = Some(id.trim().to_owned());
            } else if let Some(rule) = line.strip_prefix("x-memprove-access: ")
                && let Some(id) = operation.take()
            {
                let kind = rule
                    .split("kind: ")
                    .nth(1)
                    .and_then(|rest| rest.split(',').next())
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                let scopes = rule
                    .split('[')
                    .nth(1)
                    .and_then(|rest| rest.split(']').next())
                    .unwrap_or_default()
                    .split(',')
                    .map(str::trim)
                    .filter(|scope| !scope.is_empty())
                    .map(str::to_owned)
                    .collect();
                found.push((id, kind, scopes));
            }
        }
        found
    }

    /// Every operation of a read tool admits `mcp-read`; every operation of
    /// a write tool admits `mcp-read` or `mcp-write`, its writes
    /// `mcp-write`; none is `control`-only (ADR 0014 E7.5.1, E7.8).
    #[test]
    fn the_static_operation_table_matches_openapi_access() {
        let openapi = openapi_scopes();
        assert!(openapi.len() >= 30, "{openapi:?}");
        let scopes = |operation: OperationId| {
            let (_, kind, scopes) = openapi
                .iter()
                .find(|(id, _, _)| id == operation.as_str())
                .unwrap_or_else(|| panic!("{operation} is not in OpenAPI"));
            assert_eq!(kind, "AnyOf", "{operation}");
            // The protocol crate's registry says the same.
            let binding = memprove_protocol::phase1::binding_for(operation.as_str()).unwrap();
            assert_eq!(
                binding.access_rule.scopes,
                scopes.iter().map(String::as_str).collect::<Vec<_>>(),
                "{operation}"
            );
            scopes.clone()
        };
        for tool in &TOOLS {
            let mut writes = 0;
            for operation in tool.operations {
                let scopes = scopes(*operation);
                assert!(scopes.iter().any(|scope| scope != "control"), "{operation}");
                let read = scopes.iter().any(|scope| scope == "mcp-read");
                let write = scopes.iter().any(|scope| scope == "mcp-write");
                match tool.kind {
                    Kind::Read => assert!(read, "{}: {operation}", tool.name),
                    Kind::Write => {
                        assert!(read || write, "{}: {operation}", tool.name);
                        writes += usize::from(write);
                    }
                }
            }
            if tool.kind == Kind::Write {
                assert!(writes > 0, "{} writes nothing", tool.name);
            }
        }
        let names: Vec<&str> = TOOLS.iter().map(|tool| tool.name).collect();
        assert_eq!(
            names,
            [
                "pack",
                "claims",
                "context",
                "tasks",
                "ledger_verify",
                "begin",
                "checkpoint",
                "decide",
                "end",
                "complete"
            ]
        );
    }

    /// U2 (H4, N-1, original #127): the last sentence of `instructions`
    /// agrees with A1 and SF-1R: in the hook's layout `begin` and `end` go
    /// through the tools or the shell, which share the client session's
    /// context.
    /// The tool table of `docs/guide.md` ("The MCP server"): for each row, the
    /// tool's name, its kind, and the daemon operations it names.
    fn contract_tool_rows() -> Vec<(String, String, Vec<String>, String)> {
        let contract = include_str!("../../../../docs/guide.md");
        let ticked = |cell: &str| {
            cell.trim()
                .strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'))
                .map(str::to_owned)
        };
        let mut rows = Vec::new();
        for line in contract.lines() {
            let cells: Vec<&str> = line.trim().trim_matches('|').split('|').collect();
            if cells.len() != 4 {
                continue;
            }
            let Some(name) = ticked(cells[0]) else {
                continue;
            };
            let operations = cells[2]
                .split(',')
                .filter_map(ticked)
                .collect::<Vec<String>>();
            rows.push((
                name,
                cells[1].trim().to_owned(),
                operations,
                cells[3].trim().to_owned(),
            ));
        }
        rows
    }

    /// U6 K4 (X1 inventory, section 3): the tool table of the guide
    /// names the tools the server registers, in the order of `tools/list`,
    /// with their read or write kind and the daemon operations each may
    /// send; a table that drifts from the code fails here.
    #[test]
    fn the_tool_table_of_mcp_contract_matches_the_registered_tools() {
        let rows = contract_tool_rows();
        let registered: Vec<(String, String, Vec<String>)> = TOOLS
            .iter()
            .map(|tool| {
                let kind = match tool.kind {
                    Kind::Read => "read",
                    Kind::Write => "write",
                };
                let mut operations: Vec<String> = tool
                    .operations
                    .iter()
                    .map(|operation| operation.as_str().to_owned())
                    .collect();
                operations.sort();
                (tool.name.to_owned(), kind.to_owned(), operations)
            })
            .collect();
        let scopes: Vec<(String, String)> = rows
            .iter()
            .map(|(name, _, _, scope)| (name.clone(), scope.clone()))
            .collect();
        let documented: Vec<(String, String, Vec<String>)> = rows
            .into_iter()
            .map(|(name, kind, mut operations, _)| {
                operations.sort();
                (name, kind, operations)
            })
            .collect();
        assert_eq!(
            documented.iter().map(|row| &row.0).collect::<Vec<_>>(),
            registered.iter().map(|row| &row.0).collect::<Vec<_>>(),
            "the tool names of docs/guide.md differ from the registered ones"
        );
        assert_eq!(documented, registered);
        // U7 K5 (U6 V-N3): the Token scope column is what the registry
        // implies: a read tool that calls the daemon needs `mcp-read`, one
        // that only reads local files needs none, a write tool needs
        // `mcp-write` for its writes.
        let implied: Vec<(String, String)> = TOOLS
            .iter()
            .map(|tool| {
                let scope = match (tool.kind, tool.operations.is_empty()) {
                    (Kind::Read, true) => "none",
                    (Kind::Read, false) => "mcp-read",
                    (Kind::Write, _) => "mcp-write",
                };
                (tool.name.to_owned(), scope.to_owned())
            })
            .collect();
        assert_eq!(
            scopes, implied,
            "the token scope column of docs/guide.md differs from the registered tools"
        );
    }

    #[test]
    fn the_instructions_say_begin_and_end_go_either_way_in_the_hooks_layout() {
        assert!(
            INSTRUCTIONS.ends_with(
                "In a session the memprove SessionStart hook started, `begin` and `end` work \
                 through these tools and in the shell alike: both use the context of that \
                 client session, so each finds the Session the other began."
            ),
            "{INSTRUCTIONS}"
        );
        assert!(
            !INSTRUCTIONS.contains("are run in the shell"),
            "{INSTRUCTIONS}"
        );
    }

    /// U4 Y-F (Y-V SF-1), U16: `query`'s description states the short-word
    /// matching (the one of U4's `search.v2`, the only matching since U16).
    #[test]
    fn the_query_description_states_the_short_word_matching() {
        // The schema's text, which `tools/list` sends as it is; no
        // description in it has an escape.
        let schema = find("pack").unwrap().schema;
        let start = schema.find(r#""query":{"#).unwrap();
        let description = &schema[start..];
        assert!(
            !description.contains("Words of one or two characters match as substrings."),
            "{description}"
        );
        assert!(!description.contains("search.v"), "{description}");
        assert!(
            description.contains(
                "A word of one or two characters matches only as a whole word; a word that is a \
                 kind label (fact, open_item, decision, next_action) matches the statements of \
                 that kind"
            ),
            "{description}"
        );
    }

    /// U9 K5 (U8 V-N15): the descriptions of `begin`'s and `end`'s three
    /// agent inputs say the rule of U8 S-F round 1 (B1) once, in each of
    /// them: they are one declaration, given whole or not at all (then the
    /// context's, or for `end` the server's environment and then its
    /// context's, agent is used whole), and a part alone is
    /// `invalid_arguments`.
    #[test]
    fn begin_and_end_say_their_agent_inputs_are_one_declaration() {
        for tool in ["begin", "end"] {
            let schema = find(tool).unwrap().input_schema();
            for input in ["agent_name", "agent_role", "parent_session"] {
                let description = schema
                    .get("properties")
                    .and_then(|properties| properties.get(input))
                    .and_then(|property| property.get("description"))
                    .and_then(JsonValue::as_str)
                    .unwrap_or_else(|| panic!("{tool}.{input} has no description"));
                assert!(
                    description.contains("one declaration")
                        && description.contains("invalid_arguments")
                        && description.contains("all together, or none") == (input != "agent_name"),
                    "{tool}.{input}: {description}"
                );
            }
            let name = schema
                .get("properties")
                .and_then(|properties| properties.get("agent_name"))
                .and_then(|property| property.get("description"))
                .and_then(JsonValue::as_str)
                .unwrap();
            assert!(
                name.contains("a name without a role, a role without a name, or a parent alone"),
                "{tool}: {name}"
            );
            // U10 N1: and that this comes before a missing Project, Task,
            // or context.
            assert!(
                name.contains("invalid_arguments, told before a missing"),
                "{tool}: {name}"
            );
        }
        // The contract is wrapped by hand; its words are what is compared.
    }

    /// U10 N2 (U9 VN7): `end` says that its agent inputs are taken only with
    /// `abandoned` and are `invalid_arguments` without it, in the
    /// description of `abandoned` and of `agent_name`, and the contract says
    /// so too.
    #[test]
    fn u10_n2_end_says_its_agent_inputs_need_abandoned() {
        let schema = find("end").unwrap().input_schema();
        let description = |input: &str| {
            schema
                .get("properties")
                .and_then(|properties| properties.get(input))
                .and_then(|property| property.get("description"))
                .and_then(JsonValue::as_str)
                .unwrap_or_else(|| panic!("end.{input} has no description"))
                .to_owned()
        };
        let abandoned = description("abandoned");
        assert!(
            abandoned.contains("taken only with abandoned")
                && abandoned.contains("invalid_arguments and nothing is sent"),
            "{abandoned}"
        );
        assert!(description("agent_name").contains("(without it, invalid_arguments)"));
    }

    /// U10 T6 (Owner O4; D3 F22): `begin`'s three agent inputs say that,
    /// when none is given, the server's environment comes first, and
    /// `agent_name` names the three variables.
    #[test]
    fn u10_t6_begin_says_it_reads_the_servers_environment() {
        let schema = find("begin").unwrap().input_schema();
        for input in ["agent_name", "agent_role", "parent_session"] {
            let description = schema
                .get("properties")
                .and_then(|properties| properties.get(input))
                .and_then(|property| property.get("description"))
                .and_then(JsonValue::as_str)
                .unwrap();
            assert!(
                description.contains("the server's environment"),
                "begin.{input}: {description}"
            );
        }
        let name = schema
            .get("properties")
            .and_then(|properties| properties.get("agent_name"))
            .and_then(|property| property.get("description"))
            .and_then(JsonValue::as_str)
            .unwrap();
        for variable in [
            "MEMPROVE_AGENT_NAME",
            "MEMPROVE_AGENT_ROLE",
            "MEMPROVE_PARENT_SESSION",
        ] {
            assert!(name.contains(variable), "{name}");
        }
    }

    /// U11 N5 (U10 VN16): the rule of ADR 0021 as the descriptions of
    /// `begin`'s `agent_name` and `end`'s `abandoned` state it, whole. A
    /// test constant of its own, not the macro's text: both descriptions
    /// must end with exactly these words, so that a change to one word, a
    /// dropped sentence, or a branch turned around (U10 S-F round 2's M2,
    /// M4, M8, M11) is a failure, not a point the test never looked for.
    const TAKEOVER_RULE: &str = "In an unattended takeover (ADR 0021; an interactive reader \
        asks the person instead), an open Session another agent left, whose Session k line in the \
        Pack gives its last write (last wrote to the ledger at) and ends with the takeover hint \
        (end it before taking over), is ended with end {session:k, abandoned:true} before begin \
        when all three hold: (a) its agent is not you: its name differs from your name, the one \
        the hook's or pack's opening gives you (You are agent「…」) or, when it gives none, the \
        agent_name you declare; or it has no name and you have one; (b) its last write is more \
        than 15 minutes older than the current UTC time, the one the hook's or pack's opening \
        gives (The current UTC time is …) or your own clock, or the Session that dispatched it \
        (dispatched by Session j) has ended, or the operator's prompt says the previous run is \
        gone; (c) it is not your own parent: the Session the hook's or pack's opening gives you \
        (parent Session …) or, when the opening names no agent, the parent_session you declare; \
        when the opening names you without a parent Session, you have no parent. No record shows \
        whether a client is still alive; the three conditions decide it. When one does not hold, \
        that Session stays open: begin runs as usual, and this session records an open item \
        another Session (Session k) may still be working, not ended in its first checkpoint.";

    /// U11 N5: the points of the rule, each as the descriptions put it and
    /// as the guide's "Which open Session to end" puts it (the guide with its
    /// backticks and line breaks taken out). The guide states the same three
    /// conditions in its own sentences (U11 N6), so the two columns differ in
    /// words but not in what they require.
    const RULE_POINTS: [(&str, &str); 15] = [
        (
            "In an unattended takeover (ADR 0021;",
            "In an unattended takeover, end such a Session as abandoned only when all three hold",
        ),
        (
            "an interactive reader asks the person instead",
            "an interactive reader asks the person",
        ),
        (
            "is ended with end {session:k, abandoned:true} before begin when all three hold",
            "end such a Session as abandoned only when all three hold",
        ),
        (
            "(a) its agent is not you: its name differs from your name",
            "(a) its agent is not you: its name differs from your name",
        ),
        (
            "the one the hook's or pack's opening gives you (You are agent「…」) or, when it gives none, the \
             agent_name you declare",
            "the one the hook's or pack's opening gives you (You are agent「<name>」(<role>)) or, \
             when it gives none, the name you declare (agent_name)",
        ),
        (
            "or it has no name and you have one",
            "or it has no name and you have one",
        ),
        (
            "(b) its last write is more than 15 minutes older than the current UTC time",
            "is more than 15 minutes old",
        ),
        (
            "the one the hook's or pack's opening gives (The current UTC time is …) or your own clock",
            "the current time in the hook's or pack's opening (The current UTC time is <UTC>) or your own clock",
        ),
        (
            "or the Session that dispatched it (dispatched by Session j) has ended",
            "or the Session that dispatched it (dispatched by Session <j>) has ended",
        ),
        (
            "or the operator's prompt says the previous run is gone",
            "or the operator's prompt says the previous run is gone",
        ),
        (
            "(c) it is not your own parent: the Session the hook's or pack's opening gives you (parent Session …) \
             or, when the opening names no agent, the parent_session you declare",
            "(c) it is not your own parent: the Session the hook's or pack's opening gives you \
             (, parent Session <ID>) or, when the opening names no agent, the one you \
             declare (--parent-session or parent_session, or MEMPROVE_PARENT_SESSION)",
        ),
        (
            "when the opening names you without a parent Session, you have no parent",
            "when the opening names you without a parent Session, you have no parent",
        ),
        (
            "No record shows whether a client is still alive; the three conditions decide it.",
            "No record shows whether a client is still alive; the three conditions decide it.",
        ),
        (
            "When one does not hold, that Session stays open: begin runs as usual",
            "When one does not hold, do not end it: begin as usual",
        ),
        (
            "this session records an open item another Session (Session k) may still be working, not ended in its first checkpoint",
            "record an open item another Session (Session k) may still be working, not ended",
        ),
    ];

    /// `text` with whitespace runs made single spaces and backticks taken out.
    fn flat(text: &str) -> String {
        text.replace('`', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The takeover rule that ends the description of `input` of `tool`,
    /// after the opening sentence both descriptions begin with (U11 T1).
    fn described_rule(tool: &str, input: &str) -> String {
        let schema = find(tool).unwrap().input_schema();
        let description = schema
            .get("properties")
            .and_then(|properties| properties.get(input))
            .and_then(|property| property.get("description"))
            .and_then(JsonValue::as_str)
            .unwrap_or_else(|| panic!("{tool}.{input} has no description"));
        // U12 T3 (Owner O5): a statement, no longer "give no agent input".
        let opening = "When the hook's or pack's opening names you, the server uses the agent it was \
                       started with, and begin and end need no agent input. ";
        let rest = description.strip_prefix(opening).unwrap_or_else(|| {
            panic!("{tool}.{input} does not open with the sentence: {description}")
        });
        let start = rest
            .find("In an unattended takeover (ADR 0021")
            .unwrap_or_else(|| panic!("{tool}.{input} states no takeover rule: {description}"));
        rest[start..].to_owned()
    }

    /// U10 SF2 (final baseline B-3; the orchestrator's decision in the
    /// ledger), whole since U11 N5 (U10 VN16): the descriptions of `begin`'s
    /// `agent_name` and `end`'s `abandoned` end with ADR 0021's takeover
    /// rule, word for word [`TAKEOVER_RULE`], in statements only (ADR 0014
    /// E7 condition y): it is for an unattended takeover; an open Session
    /// another agent left is ended as abandoned before `begin` when all
    /// three conditions hold (its agent is not you; its last write is more
    /// than 15 minutes old, or its dispatcher ended, or the operator says
    /// the previous run is gone; it is not your parent); otherwise `begin`
    /// runs as usual and an open item is recorded. The contract says where
    /// the rule is stated.
    #[test]
    fn u10_sf2_begin_and_end_state_the_takeover_rule() {
        for (tool, input) in [("begin", "agent_name"), ("end", "abandoned")] {
            let rule = described_rule(tool, input);
            assert_eq!(rule, TAKEOVER_RULE, "{tool}.{input} states another rule");
            assert!(
                !rule.contains(['?', '？', '!', '！']),
                "{tool}.{input}: {rule}"
            );
            // U11 T1 (Owner O3): (a) no longer points at the variable.
            let condition_a = &rule[rule.find("(a) ").unwrap()..rule.find("(b) ").unwrap()];
            assert!(
                !condition_a.contains("MEMPROVE_AGENT_NAME"),
                "{tool}.{input}: {condition_a}"
            );
            for word in ["must", "Must", "MUST", "should", "Should"] {
                assert!(
                    !rule
                        .split(|character: char| !character.is_ascii_alphabetic())
                        .any(|found| found == word),
                    "{tool}.{input} says {word}: {rule}"
                );
            }
        }
        assert_eq!(
            described_rule("begin", "agent_name"),
            described_rule("end", "abandoned"),
            "begin and end state different rules"
        );
    }

    /// U12 T2 (Owner O4): `pack`'s description says that a call without
    /// `query` returns the hook's opening, from this server's
    /// clock and environment, before the Pack; and the takeover rule's (a),
    /// (b), and (c) take the name, the time, and the parent from the hook's or
    /// the pack's opening.
    #[test]
    fn u12_t2_pack_states_its_opening_and_the_rule_reads_it() {
        let description = find("pack").unwrap().description;
        assert!(
            description.contains(
                "Without query, the first text block is the opening the \
                 SessionStart hook writes, without its SessionStart words: the Task and why it \
                 was chosen, this server's current UTC time (The current UTC time is …), and, when this \
                 server's process has MEMPROVE_AGENT_NAME and MEMPROVE_AGENT_ROLE, the \
                 agent it names (You are agent「…」, with parent Session when MEMPROVE_PARENT_SESSION is \
                 set); the Pack is the next block."
            ),
            "{description}"
        );
        let rule = flat(TAKEOVER_RULE);
        assert_eq!(
            rule.matches("the hook's or pack's opening").count(),
            3,
            "{rule}"
        );
        assert!(!rule.contains("the hook's opening"), "{rule}");
    }

    /// U12 T3 (Owner O5; U11-VN3): the descriptions of `begin`'s
    /// `agent_name` and `end`'s `abandoned` open with a statement (ADR 0014
    /// E7 condition y), not with the imperative "give no agent input" of
    /// U11, and no sentence of them starts with an imperative.
    #[test]
    fn u12_t3_the_agent_descriptions_open_with_a_statement() {
        for (tool, input) in [("begin", "agent_name"), ("end", "abandoned")] {
            let schema = find(tool).unwrap().input_schema();
            let description = schema
                .get("properties")
                .and_then(|properties| properties.get(input))
                .and_then(|property| property.get("description"))
                .and_then(JsonValue::as_str)
                .unwrap();
            assert!(
                description.starts_with(
                    "When the hook's or pack's opening names you, the server uses the agent it was \
                     started with, and begin and end need no agent input. "
                ),
                "{tool}.{input}: {description}"
            );
            for words in ["give no", "Give no", "do not", "Do not", "don't", "Don't"] {
                assert!(
                    !description.contains(words),
                    "{tool}.{input} says {words:?}: {description}"
                );
            }
            for sentence in description.split(". ") {
                let first = sentence.split_whitespace().next().unwrap_or_default();
                assert!(
                    ![
                        "Give", "Pass", "Use", "Call", "Run", "Send", "Set", "Provide"
                    ]
                    .contains(&first),
                    "{tool}.{input}: an imperative: {sentence}"
                );
            }
        }
    }

    /// U11 S-F round 1 (final baseline `36df8a8` B-2: the client ran `echo
    /// "PARENT=$MEMPROVE_PARENT_SESSION"` to settle condition (c) and was
    /// refused; ADR 0021 "Amendment (U11, S-F round 1, 2026-11-04)"): the
    /// rule part of both descriptions takes the parent of (c) from the hook's
    /// opening (`parent Session …`), else the `parent_session` declared, and an opening
    /// that names the client without a parent means it has none; it no longer
    /// points at `MEMPROVE_PARENT_SESSION`, which a takeover client cannot
    /// read.
    #[test]
    fn u11_sf1_condition_c_takes_the_parent_from_the_hook_opening() {
        for (tool, input) in [("begin", "agent_name"), ("end", "abandoned")] {
            let rule = described_rule(tool, input);
            assert!(
                !rule.contains("MEMPROVE_PARENT_SESSION"),
                "{tool}.{input} points at the variable: {rule}"
            );
            let condition_c = &rule[rule.find("(c) ").unwrap()..rule.find("No record").unwrap()];
            for words in [
                "the Session the hook's or pack's opening gives you (parent Session …)",
                "when the opening names no agent, the parent_session you declare",
                "when the opening names you without a parent Session, you have no parent",
            ] {
                assert!(
                    condition_c.contains(words),
                    "{tool}.{input}: (c) lacks {words:?}: {condition_c}"
                );
            }
        }
    }

    /// U11 N5 (U10 VN16): each point of [`TAKEOVER_RULE`] has its counterpart
    /// in the guide's "Which open Session to end", and the guide's section
    /// holds nothing the rule lacks as a condition: three conditions
    /// `(a)`, `(b)`, `(c)`, the 15 minutes, and the open item.
    #[test]
    fn u11_n5_the_rule_corresponds_to_the_guide_point_by_point() {
        let guide = flat(include_str!("../../../../docs/guide.md"));
        let section = guide
            .split("**Which open Session to end.**")
            .nth(1)
            .expect("the guide has no \"Which open Session to end\"")
            .split("**When a rule is missing,**")
            .next()
            .unwrap();
        let rule = flat(TAKEOVER_RULE);
        for (in_rule, in_guide) in RULE_POINTS {
            let (in_rule, in_guide) = (flat(in_rule), flat(in_guide));
            assert!(rule.contains(&in_rule), "the rule lacks {in_rule:?}");
            assert!(
                section.contains(&in_guide),
                "the guide's section lacks {in_guide:?}, the counterpart of {in_rule:?}"
            );
        }
        for (in_rule, in_guide) in [
            ("(a) its agent", "- (a) its agent"),
            ("(b) its last write", "- (b) it looks gone"),
            ("(c) it is not", "- (c) it is not"),
        ] {
            assert_eq!(rule.matches(in_rule).count(), 1, "{in_rule}");
            assert_eq!(section.matches(in_guide).count(), 1, "{in_guide}");
        }
        assert_eq!(rule.matches("15 minutes").count(), 1);
    }

    /// U11 N6 (U10 VN17): the guide, ADR 0021, and the contract describe the
    /// two descriptions as stating the same three conditions in statements
    /// (the guide states them as a procedure, in imperatives, so they are
    /// not the same words), and none of them says "in the same words" of
    /// the rule any more.
    #[test]
    fn u11_n6_the_documents_do_not_call_the_descriptions_the_same_words() {
        let guide = flat(include_str!("../../../../docs/guide.md"));
        for (name, text) in [("guide", &guide)] {
            assert!(
                text.contains("the same three conditions, as statements"),
                "{name} does not say the descriptions state the same three conditions as statements"
            );
            for old in [
                "in the same words (U10 SF2)",
                "in the same words and in statements",
                "in the same words, in statements only",
                "in the same words: in an unattended takeover",
            ] {
                assert!(!text.contains(old), "{name} still says {old:?}");
            }
        }
    }

    /// U11 N3 (U10 VN12): the guide's "Which open Session to end" says how
    /// a reader sees from the Pack that the Session that dispatched another
    /// has ended: every Session that has not ended has a `Session` line, so
    /// one with no line, or whose line says it ended, has ended. The
    /// sentences follow the Pack's rule for which Sessions get a line (the
    /// latest ended one and the latest three abandoned, too). U17: ADR 0021
    /// keeps the words of the Chinese Pack and is no longer checked here.
    #[test]
    fn u11_n3_the_documents_say_how_the_pack_shows_a_dispatcher_has_ended() {
        let guide = flat(include_str!("../../../../docs/guide.md"));
        for words in [
            "every Session that has not ended has a Session line",
            "a Session with no Session line, or whose line says it ended, has ended",
            "the latest three that ended abandoned",
            "dispatched by Session <j>",
            "dispatched by unlisted Session <id>",
        ] {
            assert!(guide.contains(words), "the guide lacks {words:?}");
        }
    }

    /// U13 N2: the guide's "Unattended takeover" states the two negative
    /// cases the takeover evaluation runs, each as a statement and pointing
    /// at its scenario: a parent that has not ended is left open (E), and a
    /// dispatcher that has ended makes the 15 minutes needless (F).
    #[test]
    fn u13_n2_the_guide_states_both_negative_cases() {
        let guide = flat(include_str!("../../../../docs/guide.md"));
        let takeover = guide
            .split("## Unattended takeover")
            .nth(1)
            .expect("the guide has no Unattended takeover")
            .split("## One Session, start to end")
            .next()
            .unwrap();
        for words in [
            "in scenario E (docs/takeover-eval.md): the Session left open is the \
             client's own parent, so (c) does not hold, and the client leaves it open and runs \
             begin as usual",
            "in scenario F (docs/takeover-eval.md): the Session left open was \
             dispatched by a Session that has ended",
            "so (b) holds although its last write is less than 15 minutes old, and the client \
             ends it as abandoned before its begin without waiting for the 15 minutes",
        ] {
            let words = flat(words);
            assert!(takeover.contains(&words), "the guide lacks {words:?}");
        }
    }

    /// U12 N2 (U11-FN1): the guide no longer says a takeover client is
    /// refused every read of its environment. It says what the evaluation's
    /// allow rules refuse: a command that expands a variable or calls
    /// `printenv`, `env`, or `date` (the `identity-time` class of ADR 0018
    /// "Amendment (U12)"), and that the name, the time, and the parent the
    /// opening gives need no such read. Both places that spoke of the read,
    /// "The agent's environment" and "Unattended takeover", say so.
    #[test]
    fn u12_n2_the_guide_says_what_the_allow_rules_refuse_not_every_read() {
        let guide = flat(include_str!("../../../../docs/guide.md"));
        let between = |from: &str, to: &str| {
            guide
                .split(from)
                .nth(1)
                .unwrap_or_else(|| panic!("the guide has no {from:?}"))
                .split(to)
                .next()
                .unwrap()
                .to_owned()
        };
        let environment = between(
            "### The agent's environment",
            "### Keep memprove's files out of Evidence",
        );
        let takeover = between("## Unattended takeover", "## One Session, start to end");
        for old in [
            "a takeover client is refused every read of its environment",
            "a takeover client often may not: printenv is not a memprove command",
        ] {
            assert!(!guide.contains(old), "the guide still says {old:?}");
        }
        for (name, section) in [
            ("The agent's environment", &environment),
            ("Unattended takeover", &takeover),
        ] {
            for words in [
                "under the evaluation's allow rules a command that expands a variable or calls \
                 printenv, env, or date is refused",
                "need no such read",
            ] {
                let words = flat(words);
                assert!(section.contains(&words), "{name} lacks {words:?}");
            }
        }
    }

    #[test]
    fn the_list_is_built_once_and_every_schema_is_strict() {
        assert_eq!(compact(list()), compact(list()));
        assert!(std::ptr::eq(list(), list()));
        for tool in &TOOLS {
            let schema = tool.input_schema();
            assert_eq!(
                compact(&schema),
                tool.schema,
                "{} is not compact",
                tool.name
            );
            schema::check_schema(&schema).unwrap_or_else(|why| panic!("{}: {why}", tool.name));
            assert!(
                tool.name.len() <= 128
                    && tool
                        .name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte)),
                "{}",
                tool.name
            );
        }
    }
}
