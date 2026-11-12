//! U9 R1–R3 (Owner O4; ADR 0020) through the real binaries: what a Session
//! that declared the role `sub_agent` may not write.
//!
//! - R1: `memproved` refuses three writes of such a Session with `403
//!   role_forbidden` and a `detail` of fixed text, and writes nothing (the
//!   Project sequence stays): (i) `proposeDecision`; (ii) `transitionTask`
//!   to `completed` whose optional `session_id` names such a Session,
//!   whatever runs it cites (U9 SF1-A, the Owner's O8 exception: the CLI
//!   and the MCP `complete` send the context's Session; a request without
//!   one is not judged by role, and a `session_id` that is no Session of
//!   the Task is `400`); (iii) a `createCheckpoint` edge, `supersedes` or
//!   `withdraws`, whose target another agent wrote (the author Session's
//!   agent name differs, or the author declared none), unless the edge
//!   supersedes an open item (withdrawing one is refused too, S-R round 1). An `orchestrator`, a `human`, and a Session that
//!   declared no agent are not limited; Sessions of one agent name may
//!   supersede and withdraw each other's statements (`--reaffirm` is a
//!   `supersedes` edge, so the same holds for it); ending another's Session
//!   as abandoned is allowed to every role.
//! - R2: the CLI prints the daemon's `detail` on its failure line and exits
//!   1; the MCP tools' error result is that `detail`, word for word.
//! - R3: the minimal flow of the takeover evaluation's scenario B, by a
//!   `sub_agent` client (`end --abandoned` of the Session another agent
//!   left, `begin`, `run`, `checkpoint --fact` citing its own run, `end`),
//!   through the CLI and through the MCP tools, passes.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;

use std::{
    fmt::Write as _,
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use p4b_support::{Cli, Run, Setup, cli_binary, isolated, step, text};
use p4g_support::{Mcp, McpEnv};
use serde_json::{Value, json};

/// The fixed `detail` of a refused `proposeDecision`.
const DECISION: &str = "a Session declared as sub_agent cannot record a Decision; the orchestrating session \
     or a person does it";
/// The fixed `detail` of a refused `transitionTask` to `completed`.
const COMPLETION: &str = "a Session declared as sub_agent cannot move the Task to completed; the orchestrating \
     session or a person does it, and they can cite any Session's runs";
/// The fixed `detail` of a `session_id` that is no Session of the Task.
const NOT_OF_TASK: &str = "$.session_id must name a Session of the Task";
/// The fixed `detail` of a `session_id` with another target.
const ONLY_COMPLETED: &str = "$.session_id applies only to target_state completed";
/// The fixed `detail` of a refused `createCheckpoint` edge.
const EDGE: &str = "a Session declared as sub_agent can supersede open items and the statements written by \
     Sessions of the same agent name, and can withdraw only statements written by Sessions of the \
     same agent name; superseding another agent's facts, next actions, and Decisions, and \
     withdrawing any statement another agent wrote, is for the orchestrating session or a person";

static KEYS: AtomicU32 = AtomicU32::new(0);

struct Http {
    status: u16,
    body: Value,
}

fn http(socket: &Path, method: &str, target: &str, token: &str, body: Option<&Value>) -> Http {
    let key = body.map(|_| {
        let key = KEYS.fetch_add(1, Ordering::SeqCst);
        format!("u9-r1-idempotency-key-{key:04}")
    });
    http_keyed(socket, method, target, token, body, key.as_deref())
}

/// [`http`] with the `Idempotency-Key` `key` for a request with a body.
fn http_keyed(
    socket: &Path,
    method: &str,
    target: &str,
    token: &str,
    body: Option<&Value>,
    key: Option<&str>,
) -> Http {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: continuity.local\r\nConnection: close\r\n\
         Authorization: Bearer {token}\r\n"
    );
    let body = body.map(Value::to_string);
    if let Some(body) = &body {
        let key = key.expect("a request with a body has an Idempotency-Key");
        write!(
            head,
            "Content-Type: application/json\r\nIdempotency-Key: {key}\r\n\
             Content-Length: {}\r\n",
            body.len()
        )
        .unwrap();
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    if let Some(body) = &body {
        stream.write_all(body.as_bytes()).unwrap();
    }
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    println!("$ {method} {target}\n{head}\n{body}");
    Http {
        status: head[9..12].parse().unwrap(),
        body: serde_json::from_str(body).unwrap_or(Value::Null),
    }
}

fn socket(setup: &Setup) -> PathBuf {
    setup.agent.data_dir.join("memproved.sock")
}

fn token(setup: &Setup) -> String {
    fs::read_to_string(&setup.agent.token)
        .unwrap()
        .trim()
        .to_owned()
}

/// The Project sequence, from a `ledger verify` that must find the ledger
/// valid.
fn ledger_sequence(setup: &Setup) -> u64 {
    let verified = setup
        .agent
        .ok(&["ledger", "verify", "--project", &setup.project_id]);
    assert_eq!(verified["valid"], true, "{verified}");
    verified["project_sequence"].as_u64().unwrap()
}

/// `memprove <words>` of `cli` with the variables `env` set too.
fn run_with(cli: &Cli, env: &[(&str, &str)], words: &[&str]) -> Run {
    let mut command = isolated(cli_binary());
    command
        .current_dir(&cli.cwd)
        .arg("--data-dir")
        .arg(&cli.data_dir)
        .arg("--token-file")
        .arg(&cli.token)
        .args(words)
        .env("MEMPROVE_CONFIG_DIR", &cli.config_dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null());
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.output().unwrap();
    let run = Run {
        code: output.status.code(),
        stdout: text(&output.stdout),
        stderr: text(&output.stderr),
    };
    println!(
        "[{} {env:?}] $ ct {}\n{}{}[exit {}]",
        cli.label,
        words.join(" "),
        run.stdout,
        run.stderr,
        run.code
            .map_or_else(|| "signal".to_owned(), |code| code.to_string())
    );
    run
}

/// `run_with` that must succeed.
fn ok_with(cli: &Cli, env: &[(&str, &str)], words: &[&str]) -> Run {
    let run = run_with(cli, env, words);
    assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
    run
}

/// `begin --project <P>` with `words`; the context it printed.
fn begin(cli: &Cli, project_id: &str, words: &[&str]) -> Value {
    cli.ok(&[&["begin", "--project", project_id][..], words].concat())
}

/// The Task Capsule of `task_id`, read with the control token.
fn capsule_of(setup: &Setup, task_id: &str) -> Value {
    let run = setup.operator.run(&["resume", "--task", task_id], true);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    serde_json::from_str(run.stdout.trim()).unwrap()
}

/// The listed revision of `session_id`, a Session of `task_id`.
fn revision_of(setup: &Setup, task_id: &str, session_id: &str) -> String {
    let capsule = capsule_of(setup, task_id);
    let workspace = capsule["sessions"][0]["workspace_id"].as_str().unwrap();
    let run = setup
        .operator
        .run(&["session", "list", "--workspace", workspace], true);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let listed: Value = serde_json::from_str(run.stdout.trim()).unwrap();
    listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["session_id"] == session_id)
        .unwrap()["revision"]
        .to_string()
}

/// The claim whose text is `text`.
fn claim_of<'a>(capsule: &'a Value, text: &str) -> &'a Value {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["text"] == text)
        .unwrap_or_else(|| panic!("no claim 「{text}」 in {capsule}"))
}

/// The number of the claim whose text is `text` in `cli`'s Capsule.
fn number(cli: &Cli, text: &str) -> String {
    claim_of(&cli.capsule(), text)["number"].to_string()
}

/// The `standing` of the claim whose text is `text`.
fn standing(cli: &Cli, text: &str) -> String {
    claim_of(&cli.capsule(), text)["standing"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Runs `memprove <words>`, which must be refused with `detail`: exit 1,
/// the daemon's `detail` at the end of the failure line of `operation`,
/// and nothing written.
fn refused(setup: &Setup, cli: &Cli, words: &[&str], operation: &str, detail: &str) {
    let before = ledger_sequence(setup);
    let run = cli.run(words, false);
    assert_eq!(run.code, Some(1), "{words:?}: {}{}", run.stdout, run.stderr);
    assert_eq!(
        run.stderr.lines().last().unwrap_or_default(),
        format!("memprove: {operation} failed: 403 role_forbidden: {detail}"),
        "{words:?}"
    );
    assert_eq!(
        ledger_sequence(setup),
        before,
        "{words:?} wrote to the ledger"
    );
}

/// Runs `memprove <words>`, which must succeed.
fn passes(cli: &Cli, words: &[&str]) {
    let run = cli.run(words, false);
    assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
}

/// A Checkpoint of a fact citing the context's last run that supersedes `target`.
fn fact_over<'a>(fact: &'a str, target: &'a str) -> Vec<&'a str> {
    vec![
        "checkpoint",
        "create",
        "--fact",
        fact,
        "--evidence",
        "last",
        "--supersedes",
        target,
        "--next",
        "next step",
    ]
}

/// A Checkpoint that withdraws `target` (with a reason, which a fact
/// needs).
/// `checkpoint create` with two facts citing the latest run.
fn two_facts<'a>(first: &'a str, second: &'a str) -> Vec<&'a str> {
    vec![
        "checkpoint",
        "create",
        "--fact",
        first,
        "--evidence",
        "last",
        "--fact",
        second,
        "--evidence",
        "last",
        "--next",
        "next step",
    ]
}

/// `checkpoint create --reaffirm <target>` citing the latest run.
fn reaffirm(target: &str) -> Vec<&str> {
    vec![
        "checkpoint",
        "create",
        "--reaffirm",
        target,
        "--evidence",
        "last",
        "--next",
        "next step",
    ]
}

/// `session end <session> --abandoned` at `revision`.
fn end_abandoned<'a>(session: &'a str, revision: &'a str) -> Vec<&'a str> {
    vec![
        "session",
        "end",
        session,
        "--abandoned",
        "--revision",
        revision,
        "--reason",
        "takeover",
    ]
}

fn withdraw(target: &str) -> Vec<&str> {
    vec![
        "checkpoint",
        "create",
        "--next",
        "next step",
        "--withdraw",
        target,
        "--withdraw-reason",
        "no longer holds",
    ]
}

/// Asserts a raw `403 role_forbidden` Problem with `detail`.
fn forbidden(answer: &Http, detail: &str) {
    assert_eq!(answer.status, 403, "{}", answer.body);
    assert_eq!(answer.body["code"], "role_forbidden", "{}", answer.body);
    assert_eq!(answer.body["status"], 403, "{}", answer.body);
    assert_eq!(answer.body["retryable"], false, "{}", answer.body);
    assert_eq!(answer.body["detail"], detail, "{}", answer.body);
}

/// Three Sessions on one Task, each in its own context: an orchestrator
/// `u9-orch` (`lead`), a `sub_agent` `u9-sub` it dispatched (`sub`), and
/// one that declared no agent (`plain`); each has run `tests/a.sh` once
/// (its run, cited as `last`).
struct Three {
    setup: Setup,
    task_id: String,
    lead: Cli,
    lead_session: String,
    sub: Cli,
    sub_session: String,
    plain: Cli,
}

impl Three {
    fn new(title: &str) -> Self {
        let setup = Setup::new(&["a"]);
        let project = setup.project_id.clone();
        let lead = setup.agent.clone();
        let begun = begin(
            &lead,
            &project,
            &[
                "--new-task",
                title,
                "--agent-name",
                "u9-orch",
                "--agent-role",
                "orchestrator",
            ],
        );
        let task_id = begun["task_id"].as_str().unwrap().to_owned();
        let lead_session = begun["session_id"].as_str().unwrap().to_owned();
        lead.test("tests/a.sh");
        let mut three = Self {
            setup,
            task_id,
            lead: lead.clone(),
            lead_session,
            sub: lead.clone(),
            sub_session: String::new(),
            plain: lead.clone(),
        };
        let (sub, sub_session) = three.another("config-sub", "sub", Some(("u9-sub", "sub_agent")));
        let (plain, _) = three.another("config-plain", "plain", None);
        three.sub = sub;
        three.sub_session = sub_session;
        three.plain = plain;
        three
    }

    /// Another Session on the Task in its own context, of `agent` (name,
    /// role; dispatched by the lead) or of none, with its run.
    fn another(
        &self,
        config: &str,
        label: &'static str,
        agent: Option<(&str, &str)>,
    ) -> (Cli, String) {
        let cli = self
            .lead
            .elsewhere(self.setup.root_path.join(config), label);
        let mut words = vec!["--task", self.task_id.as_str()];
        if let Some((name, role)) = agent {
            words.extend([
                "--agent-name",
                name,
                "--agent-role",
                role,
                "--parent-session",
                self.lead_session.as_str(),
            ]);
        }
        let session = begin(&cli, &self.setup.project_id, &words)["session_id"]
            .as_str()
            .unwrap()
            .to_owned();
        cli.test("tests/a.sh");
        (cli, session)
    }
}

#[test]
fn u9_r1_a_sub_agent_may_not_record_a_decision() {
    let three = Three::new("u9 r1 decisions");
    let setup = &three.setup;

    step("(i) the sub_agent's decide is refused, and nothing is written");
    refused(
        setup,
        &three.sub,
        &["decide", "the sub-agent's decision"],
        "proposeDecision",
        DECISION,
    );
    step("the same over the wire: 403 role_forbidden with the fixed detail");
    let before = ledger_sequence(setup);
    let answer = http(
        &socket(setup),
        "POST",
        "/v1/decisions",
        &token(setup),
        Some(&json!({
            "schema_version": 1,
            "task_id": three.task_id,
            "session_id": three.sub_session,
            "content": "the sub-agent's decision",
        })),
    );
    forbidden(&answer, DECISION);
    assert_eq!(ledger_sequence(setup), before);

    step("the orchestrator, a human, and a Session without an agent decide");
    passes(&three.lead, &["decide", "the orchestrator's decision"]);
    passes(&three.plain, &["decide", "the unnamed session's decision"]);
    let (human, _) = three.another("config-human", "human", Some(("owner", "human")));
    passes(&human, &["decide", "the person's decision"]);
    ledger_sequence(setup);
}

/// [`Three`], and the claims the edge tests name: the orchestrator's fact,
/// two open items, next action, and Decision, and a fact of the Session
/// without an agent.
fn with_claims(title: &str) -> Three {
    let three = Three::new(title);
    passes(
        &three.lead,
        &[
            "checkpoint",
            "create",
            "--fact",
            "the orchestrator's fact",
            "--evidence",
            "last",
            "--open",
            "the orchestrator's open item one",
            "--open",
            "the orchestrator's open item two",
            "--next",
            "the orchestrator's next step",
        ],
    );
    passes(&three.lead, &["decide", "the orchestrator's decision"]);
    passes(
        &three.plain,
        &[
            "checkpoint",
            "create",
            "--fact",
            "the unnamed session's fact",
            "--evidence",
            "last",
            "--next",
            "the unnamed session's next step",
        ],
    );
    three
}

#[test]
fn u9_r1_a_sub_agent_may_not_overrule_another_agents_statements() {
    let three = with_claims("u9 r1 refused edges");
    let setup = &three.setup;
    let (lead, sub) = (&three.lead, &three.sub);
    let lead_fact = number(lead, "the orchestrator's fact");
    let lead_next = number(lead, "the orchestrator's next step");
    let lead_decision = number(lead, "the orchestrator's decision");
    let plain_fact = number(lead, "the unnamed session's fact");

    step("(iii) superseding another agent's fact is refused");
    refused(
        setup,
        sub,
        &fact_over("the sub-agent's fact", &lead_fact),
        "createCheckpoint",
        EDGE,
    );
    step("so is --reaffirm of it: a supersedes edge with the same text");
    refused(
        setup,
        sub,
        &[
            "checkpoint",
            "create",
            "--reaffirm",
            &lead_fact,
            "--evidence",
            "last",
            "--next",
            "next step",
        ],
        "createCheckpoint",
        EDGE,
    );
    step("so is superseding its next action, and withdrawing its Decision");
    refused(
        setup,
        sub,
        &[
            "checkpoint",
            "create",
            "--next",
            "the sub-agent's next step",
            "--supersedes",
            &lead_next,
        ],
        "createCheckpoint",
        EDGE,
    );
    refused(
        setup,
        sub,
        &withdraw(&lead_decision),
        "createCheckpoint",
        EDGE,
    );
    step("withdrawing another agent's fact, and a fact of a Session without an agent");
    for target in [&lead_fact, &plain_fact] {
        refused(setup, sub, &withdraw(target), "createCheckpoint", EDGE);
    }
    step("one refused edge refuses the whole Checkpoint, an allowed edge with it");
    let open_one = number(lead, "the orchestrator's open item one");
    refused(
        setup,
        sub,
        &[
            "checkpoint",
            "create",
            "--fact",
            "the sub-agent's fact",
            "--evidence",
            "last",
            "--supersedes",
            &open_one,
            "--next",
            "next step",
            "--supersedes",
            &lead_next,
        ],
        "createCheckpoint",
        EDGE,
    );
    assert_eq!(standing(lead, "the orchestrator's open item one"), "active");
    ledger_sequence(setup);
}

#[test]
fn u9_r1_a_sub_agent_may_overrule_open_items_and_its_own_agents_statements() {
    let three = with_claims("u9 r1 allowed edges");
    let setup = &three.setup;
    let (lead, sub, plain) = (&three.lead, &three.sub, &three.plain);
    let lead_fact = number(lead, "the orchestrator's fact");

    step("an open item of another agent: superseded");
    let open_one = number(lead, "the orchestrator's open item one");
    passes(
        sub,
        &fact_over("the sub-agent finished open item one", &open_one),
    );
    assert_eq!(
        standing(lead, "the orchestrator's open item one"),
        "superseded"
    );

    step("two Sessions of one agent name supersede and withdraw each other's statements");
    passes(
        sub,
        &two_facts(
            "the sub-agent's first session's fact",
            "the sub-agent's first session's second fact",
        ),
    );
    let (again, _) = three.another(
        "config-sub-again",
        "sub-again",
        Some(("u9-sub", "sub_agent")),
    );
    let first_fact = number(lead, "the sub-agent's first session's fact");
    passes(
        &again,
        &fact_over("the sub-agent's second session's fact", &first_fact),
    );
    step("--reaffirm of the same agent's earlier fact");
    let second_fact = number(lead, "the sub-agent's first session's second fact");
    passes(&again, &reaffirm(&second_fact));
    let again_fact = number(lead, "the sub-agent's second session's fact");
    passes(sub, &withdraw(&again_fact));
    assert_eq!(
        standing(lead, "the sub-agent's second session's fact"),
        "withdrawn"
    );

    step("a sub_agent of another name may not supersede u9-sub's fact");
    let (other, other_session) =
        three.another("config-other", "other", Some(("u9-other", "sub_agent")));
    let reaffirmed = number(lead, "the sub-agent's first session's second fact");
    refused(
        setup,
        &other,
        &fact_over("the other-named sub-agent's fact", &reaffirmed),
        "createCheckpoint",
        EDGE,
    );

    step("the orchestrator, a Session without an agent, and a human overrule anyone");
    passes(
        lead,
        &fact_over("the orchestrator supersedes the sub-agent", &reaffirmed),
    );
    passes(
        plain,
        &fact_over(
            "the unnamed session supersedes the orchestrator",
            &lead_fact,
        ),
    );
    let lead_again = number(lead, "the orchestrator supersedes the sub-agent");
    let (human, _) = three.another("config-human", "human", Some(("owner", "human")));
    passes(
        &human,
        &fact_over("the person supersedes the orchestrator", &lead_again),
    );

    step("ending another's Session as abandoned is allowed to a sub_agent");
    let revision = revision_of(setup, &three.task_id, &other_session);
    passes(sub, &end_abandoned(&other_session, &revision));
    ledger_sequence(setup);
}

/// U9 S-R round 1 (the orchestrator's decision on O4 (iii)): the open-item
/// exception is for `supersedes` only; a `sub_agent` withdraws only its own
/// agent name's statements, open items included.
#[test]
fn u9_r1_a_sub_agent_supersedes_but_does_not_withdraw_another_agents_open_item() {
    let three = with_claims("u9 r1 open items");
    let setup = &three.setup;
    let (lead, sub) = (&three.lead, &three.sub);
    let open_two = number(lead, "the orchestrator's open item two");

    step("withdrawing the orchestrator's open item is refused, and nothing is written");
    refused(setup, sub, &withdraw(&open_two), "createCheckpoint", EDGE);
    assert_eq!(standing(lead, "the orchestrator's open item two"), "active");
    step("superseding the same open item passes");
    passes(
        sub,
        &fact_over("the sub-agent finished open item two", &open_two),
    );
    assert_eq!(
        standing(lead, "the orchestrator's open item two"),
        "superseded"
    );

    step("a Session of the same agent name withdraws an open item the first one wrote");
    passes(
        sub,
        &[
            "checkpoint",
            "create",
            "--open",
            "the sub-agent's open item",
            "--next",
            "next step",
        ],
    );
    let (again, _) = three.another(
        "config-sub-again",
        "sub-again",
        Some(("u9-sub", "sub_agent")),
    );
    let own_open = number(lead, "the sub-agent's open item");
    passes(&again, &withdraw(&own_open));
    assert_eq!(standing(lead, "the sub-agent's open item"), "withdrawn");
    ledger_sequence(setup);
}

/// A new test run of `cli` (`tests/a.sh`); its Evidence ID.
fn test_run(cli: &Cli) -> String {
    let (_, evidence) = cli.run_command(&["--test-format", "cargo"], &["sh", "tests/a.sh"]);
    evidence["id"].as_str().unwrap().to_owned()
}

/// `transitionTask` of `task_id` to `target` at its revision over the
/// wire, citing `evidence` (judged in the Workspace of the Task's first
/// Session, as `task complete` sends its context's), with `session_id`
/// when given.
fn transition_over_wire(
    setup: &Setup,
    task_id: &str,
    target: &str,
    evidence: &[&str],
    session_id: Option<&str>,
) -> Http {
    let capsule = capsule_of(setup, task_id);
    let mut body = json!({
        "schema_version": 1,
        "expected_revision": capsule["task"]["revision"],
        "target_state": target,
    });
    if !evidence.is_empty() {
        body["completion_evidence_ids"] = json!(evidence);
        body["workspace_id"] = capsule["sessions"][0]["workspace_id"].clone();
    }
    if let Some(session_id) = session_id {
        body["session_id"] = json!(session_id);
    }
    http(
        &socket(setup),
        "POST",
        &format!("/v1/tasks/{task_id}/transition"),
        &token(setup),
        Some(&body),
    )
}

/// Asserts a raw `400 invalid_request` Problem with `detail`.
fn invalid(answer: &Http, detail: &str) {
    assert_eq!(answer.status, 400, "{}", answer.body);
    assert_eq!(answer.body["code"], "invalid_request", "{}", answer.body);
    assert_eq!(answer.body["detail"], detail, "{}", answer.body);
}

/// Asserts a `200` Task response in the state `completed`.
fn completed(answer: &Http) {
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["state"], "completed", "{}", answer.body);
}

/// A new Task of `three`'s Project, begun in its own context by `judge`
/// (name, role), or by a Session that declared no agent, and a test run on
/// it of a `sub_agent` the lead dispatched: the Task, the judge's Session,
/// and the `sub_agent`'s Evidence ID.
fn task_with_a_sub_agents_run(
    three: &Three,
    title: &str,
    judge: Option<(&str, &str)>,
    labels: (&'static str, &'static str),
) -> (String, String, String) {
    let setup = &three.setup;
    let config = |label: &str| setup.root_path.join(format!("config-{label}"));
    let judging = three.lead.elsewhere(config(labels.0), labels.0);
    let mut words = vec!["--new-task", title];
    if let Some((name, role)) = judge {
        words.extend(["--agent-name", name, "--agent-role", role]);
    }
    let begun = begin(&judging, &setup.project_id, &words);
    let task_id = begun["task_id"].as_str().unwrap().to_owned();
    let judge_session = begun["session_id"].as_str().unwrap().to_owned();
    let sub = three.lead.elsewhere(config(labels.1), labels.1);
    begin(
        &sub,
        &setup.project_id,
        &[
            "--task",
            &task_id,
            "--agent-name",
            "u9-sub",
            "--agent-role",
            "sub_agent",
            "--parent-session",
            &three.lead_session,
        ],
    );
    let sub_run = test_run(&sub);
    (task_id, judge_session, sub_run)
}

#[test]
fn u9_sf1_a_a_sub_agent_completes_no_task_whatever_runs_it_cites() {
    let three = Three::new("u9 sf1-a sub_agent completion");
    let setup = &three.setup;
    let lead_run = test_run(&three.lead);
    let sub_run = test_run(&three.sub);

    step("(ii) the sub_agent's task complete citing the orchestrator's run is refused");
    refused(
        setup,
        &three.sub,
        &["task", "complete", "--evidence", &lead_run],
        "transitionTask",
        COMPLETION,
    );
    step("so are its task complete on its own runs and its task transition --to completed");
    refused(
        setup,
        &three.sub,
        &["task", "complete"],
        "transitionTask",
        COMPLETION,
    );
    refused(
        setup,
        &three.sub,
        &[
            "task",
            "transition",
            "--to",
            "completed",
            "--evidence",
            &lead_run,
        ],
        "transitionTask",
        COMPLETION,
    );

    step("over the wire with the sub_agent's session_id: 403 on anyone's run");
    for evidence in [&lead_run, &sub_run] {
        let before = ledger_sequence(setup);
        let answer = transition_over_wire(
            setup,
            &three.task_id,
            "completed",
            &[evidence],
            Some(&three.sub_session),
        );
        forbidden(&answer, COMPLETION);
        assert_eq!(ledger_sequence(setup), before);
    }

    step("a session_id that is no Session of the Task is 400, nothing written");
    let elsewhere = three
        .lead
        .elsewhere(setup.root_path.join("config-elsewhere"), "elsewhere");
    let foreign = begin(
        &elsewhere,
        &setup.project_id,
        &["--new-task", "u9 sf1-a another task"],
    )["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before = ledger_sequence(setup);
    let answer = transition_over_wire(
        setup,
        &three.task_id,
        "completed",
        &[&lead_run],
        Some(&foreign),
    );
    invalid(&answer, NOT_OF_TASK);
    step("and so is a session_id with a target other than completed");
    let answer = transition_over_wire(
        setup,
        &three.task_id,
        "blocked",
        &[],
        Some(&three.lead_session),
    );
    invalid(&answer, ONLY_COMPLETED);
    assert_eq!(ledger_sequence(setup), before);
    assert_eq!(
        capsule_of(setup, &three.task_id)["task"]["status"],
        "in_progress"
    );

    step("the orchestrator's task complete citing the sub_agent's run completes the Task");
    passes(&three.lead, &["task", "complete", "--evidence", &sub_run]);
    assert_eq!(
        capsule_of(setup, &three.task_id)["task"]["status"],
        "completed"
    );
    ledger_sequence(setup);
}

/// U10 N7 (U9 VN18): a `sub_agent` that writes out the ID of its own
/// context's Task, `task transition <TASK_ID> --to completed`, is still
/// refused: the CLI treats the explicit ID that equals the context's Task as
/// the context's Task and sends the context's Session, so the daemon judges
/// the completion by its role. (An ID other than the context's sends none,
/// by the Owner's rule: `u9_sf2_a_task_transition_without_a_session_is_not_judged_by_role`.)
/// Were the explicit ID read as "another Task", this test would find the
/// Task completed.
#[test]
fn u10_n7_a_sub_agent_naming_its_own_task_completes_nothing() {
    let three = Three::new("u10 n7 explicit task id");
    let setup = &three.setup;
    let lead_run = test_run(&three.lead);
    let sub_run = test_run(&three.sub);

    step("the sub_agent names its own context's Task, on the orchestrator's run and on its own");
    let revision = capsule_of(setup, &three.task_id)["task"]["revision"].to_string();
    for (evidence, extra) in [
        (&lead_run, &[][..]),
        (&sub_run, &[][..]),
        (&sub_run, &["--revision", revision.as_str()][..]),
    ] {
        let mut words = vec![
            "task",
            "transition",
            three.task_id.as_str(),
            "--to",
            "completed",
            "--evidence",
            evidence.as_str(),
        ];
        words.extend_from_slice(extra);
        refused(setup, &three.sub, &words, "transitionTask", COMPLETION);
        assert_eq!(
            capsule_of(setup, &three.task_id)["task"]["status"],
            "in_progress",
            "{words:?} completed the Task"
        );
    }

    step("the same words from the orchestrator's context complete it");
    passes(
        &three.lead,
        &[
            "task",
            "transition",
            three.task_id.as_str(),
            "--to",
            "completed",
            "--evidence",
            sub_run.as_str(),
        ],
    );
    assert_eq!(
        capsule_of(setup, &three.task_id)["task"]["status"],
        "completed"
    );
    ledger_sequence(setup);
}

#[test]
fn u9_sf1_a_others_complete_a_task_on_a_sub_agents_run() {
    let three = Three::new("u9 sf1-a orchestrator completion");
    let setup = &three.setup;
    let sub_run = test_run(&three.sub);

    step("twenty later Sessions: the Preview's view no longer lists the first three");
    for later in 0..20 {
        let cli = three.lead.elsewhere(
            setup.root_path.join(format!("config-later-{later}")),
            "later",
        );
        begin(&cli, &setup.project_id, &["--task", &three.task_id]);
    }
    step("the sub_agent's session_id is still found among the Task's Sessions: 403");
    let before = ledger_sequence(setup);
    forbidden(
        &transition_over_wire(
            setup,
            &three.task_id,
            "completed",
            &[&sub_run],
            Some(&three.sub_session),
        ),
        COMPLETION,
    );
    assert_eq!(ledger_sequence(setup), before);

    step("the orchestrator's session_id on the sub_agent's run: completed");
    completed(&transition_over_wire(
        setup,
        &three.task_id,
        "completed",
        &[&sub_run],
        Some(&three.lead_session),
    ));
    for (title, judge, labels) in [
        (
            "u9 sf1-a human completion",
            Some(("owner", "human")),
            ("human", "sub-human"),
        ),
        (
            "u9 sf1-a plain completion",
            None,
            ("plain-judge", "sub-plain"),
        ),
    ] {
        step(&format!(
            "{title}: the judge's session_id on a sub_agent's run"
        ));
        let (task_id, judge_session, sub_run) =
            task_with_a_sub_agents_run(&three, title, judge, labels);
        completed(&transition_over_wire(
            setup,
            &task_id,
            "completed",
            &[&sub_run],
            Some(&judge_session),
        ));
    }
    step("without session_id no role is checked: a sub_agent's run completes the Task");
    let (task_id, _, sub_run) = task_with_a_sub_agents_run(
        &three,
        "u9 sf1-a no session",
        None,
        ("plain-none", "sub-none"),
    );
    completed(&transition_over_wire(
        setup,
        &task_id,
        "completed",
        &[&sub_run],
        None,
    ));
    ledger_sequence(setup);
}

/// Asserts that the `task transition --to completed` the CLI sent with the
/// `Idempotency-Key` `key` for `task_id` at `revision`, citing `evidence`,
/// carried no `session_id`: the same request without one replays the
/// recorded answer, and with `session` added it is `409
/// idempotency_conflict` (the request digest holds a `session_id` only when
/// one is given); neither writes anything.
fn sent_no_session(
    setup: &Setup,
    task_id: &str,
    revision: &Value,
    evidence: &str,
    key: &str,
    session: &str,
) {
    let capsule = capsule_of(setup, task_id);
    let mut body = json!({
        "schema_version": 1,
        "expected_revision": revision,
        "target_state": "completed",
        "completion_evidence_ids": [evidence],
        "workspace_id": capsule["sessions"][0]["workspace_id"],
    });
    let target = format!("/v1/tasks/{task_id}/transition");
    let before = ledger_sequence(setup);
    let replayed = http_keyed(
        &socket(setup),
        "POST",
        &target,
        &token(setup),
        Some(&body),
        Some(key),
    );
    completed(&replayed);
    assert_eq!(replayed.body["revision"], capsule["task"]["revision"]);
    body["session_id"] = json!(session);
    let conflict = http_keyed(
        &socket(setup),
        "POST",
        &target,
        &token(setup),
        Some(&body),
        Some(key),
    );
    assert_eq!(conflict.status, 409, "{}", conflict.body);
    assert_eq!(
        conflict.body["code"], "idempotency_conflict",
        "{}",
        conflict.body
    );
    assert_eq!(ledger_sequence(setup), before);
}

/// U9 SF2-A (S-F round 1 review, SF-1; ADR 0020, "What it does not do"):
/// the CLI's `task transition --to completed` sends the context's Session
/// only for the context's Task while the context has one. A `sub_agent`
/// that names another Task, or that ended its Session first, sends none,
/// and by the Owner's rule (a request without `session_id` is not judged
/// by role) the Task is completed. This pins the behaviour as it is; it
/// does not change it.
#[test]
fn u9_sf2_a_task_transition_without_a_session_is_not_judged_by_role() {
    let three = Three::new("u9 sf2-a transition without a session");
    let setup = &three.setup;
    let (other_task, _, other_run) = task_with_a_sub_agents_run(
        &three,
        "u9 sf2-a another task",
        None,
        ("other-judge", "other-sub"),
    );

    step("the sub_agent, its Session active, names another Task: completed, no session_id");
    let revision = capsule_of(setup, &other_task)["task"]["revision"].clone();
    let key = "u9-sf2-a-another-task";
    passes(
        &three.sub,
        &[
            "task",
            "transition",
            &other_task,
            "--to",
            "completed",
            "--revision",
            &revision.to_string(),
            "--evidence",
            &other_run,
            "--idempotency-key",
            key,
        ],
    );
    assert_eq!(
        capsule_of(setup, &other_task)["task"]["status"],
        "completed"
    );
    sent_no_session(
        setup,
        &other_task,
        &revision,
        &other_run,
        key,
        &three.sub_session,
    );

    step("the sub_agent ends its Session: the context keeps the Task and the agent");
    let sub_run = test_run(&three.sub);
    passes(&three.sub, &["end", "--reason", "done"]);
    let shown = three.sub.run(&["context", "show"], false);
    assert_eq!(shown.code, Some(0), "{}", shown.stderr);
    let line = |name: &str| {
        shown
            .stdout
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap_or_else(|| panic!("context show lacks {name:?}: {}", shown.stdout))
            .to_owned()
    };
    assert!(
        line("Session:")["Session:".len()..]
            .trim_start()
            .starts_with("none"),
        "{}",
        shown.stdout
    );
    assert!(
        line("Agent name:").contains("u9-sub (role sub_agent"),
        "{}",
        shown.stdout
    );
    assert!(line("Task:").contains(&three.task_id), "{}", shown.stdout);

    step("then its task transition --to completed of the context's Task: completed, no session_id");
    let revision = capsule_of(setup, &three.task_id)["task"]["revision"].clone();
    let key = "u9-sf2-a-after-end";
    passes(
        &three.sub,
        &[
            "task",
            "transition",
            "--to",
            "completed",
            "--evidence",
            &sub_run,
            "--idempotency-key",
            key,
        ],
    );
    assert_eq!(
        capsule_of(setup, &three.task_id)["task"]["status"],
        "completed"
    );
    sent_no_session(
        setup,
        &three.task_id,
        &revision,
        &sub_run,
        key,
        &three.sub_session,
    );
}

#[test]
fn u9_r2_the_mcp_tools_relay_the_daemons_reason() {
    let three = Three::new("u9 r2 mcp");
    let setup = &three.setup;
    passes(
        &three.lead,
        &[
            "checkpoint",
            "create",
            "--fact",
            "the orchestrator's fact",
            "--evidence",
            "last",
            "--next",
            "the orchestrator's next step",
        ],
    );
    let lead_fact: u64 = number(&three.lead, "the orchestrator's fact")
        .parse()
        .unwrap();
    let mut mcp = Mcp::start(&McpEnv::of(
        &three.sub.config_dir,
        &three.sub.data_dir,
        &three.sub.token,
        &setup.repo,
    ));
    let before = ledger_sequence(setup);
    for (tool, arguments, detail) in [
        (
            "decide",
            json!({"text": "the sub-agent's decision"}),
            DECISION,
        ),
        (
            "checkpoint",
            json!({
                "facts": [{"text": "the sub-agent's fact", "evidence": ["last"], "supersedes": [lead_fact]}],
                "next": {"text": "next step"},
            }),
            EDGE,
        ),
        ("complete", json!({}), COMPLETION),
    ] {
        step(&format!("MCP {tool}: refused with the daemon's detail"));
        let called = mcp.call(tool, &arguments);
        assert!(called.is_error, "{}", called.line);
        // U11 N1 (Owner O6): the Problem code first, then the daemon's
        // `detail` word for word.
        assert!(
            called.text.starts_with("role_forbidden: "),
            "{}",
            called.line
        );
        assert_eq!(
            called.text,
            format!("role_forbidden: {detail}"),
            "{}",
            called.line
        );
        assert_eq!(called.structured["failure"], "problem", "{}", called.line);
        assert_eq!(called.structured["status"], 403, "{}", called.line);
        assert_eq!(
            called.structured["code"], "role_forbidden",
            "{}",
            called.line
        );
    }
    mcp.finish();
    assert_eq!(ledger_sequence(setup), before);
}

/// Scenario B's seed, as `scripts/takeover-eval/run.sh` writes it, in one
/// configuration directory with a client session each:
/// `takeover-eval-seed` (a `sub_agent`) ran the tests, recorded a fact, an
/// open item, and the next action, and ended; `takeover-eval-other` (a
/// `sub_agent`) began, ran, and left its Session open. Returns the Task
/// and the open Session.
fn seed_b(setup: &Setup) -> (String, String) {
    let agent = &setup.agent;
    let project = setup.project_id.as_str();
    let seed = [("MEMPROVE_CLIENT_SESSION", "takeover-eval-seed")];
    let begun = ok_with(
        agent,
        &seed,
        &[
            "begin",
            "--project",
            project,
            "--new-task",
            "u9 r3 b",
            "--agent-name",
            "takeover-eval-seed",
            "--agent-role",
            "sub_agent",
        ],
    );
    let begun: Value = serde_json::from_str(begun.stdout.trim()).unwrap();
    let task_id = begun["task_id"].as_str().unwrap().to_owned();
    let test = ["run", "--test-format", "cargo", "--", "sh", "tests/a.sh"];
    ok_with(agent, &seed, &test);
    ok_with(
        agent,
        &seed,
        &[
            "checkpoint",
            "create",
            "--fact",
            "the seed's fact",
            "--evidence",
            "last",
            "--open",
            "the seed's open item",
            "--next",
            "the seed's next step",
        ],
    );
    ok_with(agent, &seed, &["end", "--reason", "seed done"]);
    let other = [("MEMPROVE_CLIENT_SESSION", "takeover-eval-other")];
    let begun = ok_with(
        agent,
        &other,
        &[
            "begin",
            "--project",
            project,
            "--task",
            &task_id,
            "--agent-name",
            "takeover-eval-other",
            "--agent-role",
            "sub_agent",
        ],
    );
    let begun: Value = serde_json::from_str(begun.stdout.trim()).unwrap();
    let other_session = begun["session_id"].as_str().unwrap().to_owned();
    ok_with(agent, &other, &test);
    (task_id, other_session)
}

/// After a B client's flow: the open Session ended abandoned, by the
/// client's agent; the client's fact is active and the seed's open item
/// superseded; no Session is open; the ledger is valid.
fn check_b(setup: &Setup, task_id: &str, other_session: &str) {
    let capsule = capsule_of(setup, task_id);
    let sessions = capsule["sessions"].as_array().unwrap();
    let other = sessions
        .iter()
        .find(|session| session["session_id"] == other_session)
        .unwrap();
    assert_eq!(other["end_state"], "crashed", "{other}");
    assert_eq!(
        other["ended_by"]["agent"],
        json!({"name": "takeover-eval-other", "role": "sub_agent", "parent_session_id": null}),
        "{other}"
    );
    assert!(
        sessions
            .iter()
            .all(|session| session["end_state"] != "open"),
        "{capsule}"
    );
    assert_eq!(claim_of(&capsule, "the taker's fact")["standing"], "active");
    assert_eq!(
        claim_of(&capsule, "the taker's fact")["verdict"],
        "verified"
    );
    assert_eq!(
        claim_of(&capsule, "the seed's open item")["standing"],
        "superseded"
    );
    ledger_sequence(setup);
}

/// The client session of a B client.
const CLIENT: &str = "takeover-eval-client";

#[test]
fn u9_r3_scenario_bs_sub_agent_client_flow_passes_through_the_cli() {
    let setup = Setup::new(&["a"]);
    let (task_id, other_session) = seed_b(&setup);
    // As run.sh sets them for B's client process (Owner O7).
    let env = [
        ("MEMPROVE_AGENT_NAME", "takeover-eval-other"),
        ("MEMPROVE_AGENT_ROLE", "sub_agent"),
        ("MEMPROVE_CLIENT_SESSION", CLIENT),
    ];
    let client = &setup.agent;
    step("end --abandoned the Session the other agent left, at its listed revision");
    let revision = revision_of(&setup, &task_id, &other_session);
    ok_with(
        client,
        &env,
        &[
            "session",
            "end",
            &other_session,
            "--abandoned",
            "--revision",
            &revision,
            "--reason",
            "takeover",
        ],
    );
    step("begin, run, a fact citing the run that completes the seed's open item, end");
    ok_with(
        client,
        &env,
        &["begin", "--project", &setup.project_id, "--task", &task_id],
    );
    let ran = ok_with(
        client,
        &env,
        &["run", "--test-format", "cargo", "--", "sh", "tests/a.sh"],
    );
    assert!(ran.stderr.contains("recorded PASS"), "{}", ran.stderr);
    let capsule = capsule_of(&setup, &task_id);
    let open = claim_of(&capsule, "the seed's open item")["number"].to_string();
    let seed_fact = claim_of(&capsule, "the seed's fact")["number"].to_string();
    step("R1 holds in the flow: the seed's fact is another agent's");
    let before = ledger_sequence(&setup);
    let refused = run_with(
        client,
        &env,
        &fact_over("the taker supersedes the seed's fact", &seed_fact),
    );
    assert_eq!(refused.code, Some(1), "{}", refused.stderr);
    assert_eq!(
        refused.stderr.lines().last().unwrap_or_default(),
        format!("memprove: createCheckpoint failed: 403 role_forbidden: {EDGE}")
    );
    assert_eq!(ledger_sequence(&setup), before);
    ok_with(
        client,
        &env,
        &[
            "checkpoint",
            "create",
            "--fact",
            "the taker's fact",
            "--evidence",
            "last",
            "--supersedes",
            &open,
            "--next",
            "the taker's next step",
        ],
    );
    ok_with(client, &env, &["end", "--reason", "takeover done"]);
    check_b(&setup, &task_id, &other_session);
}

#[test]
fn u9_r3_scenario_bs_sub_agent_client_flow_passes_through_the_mcp_tools() {
    let setup = Setup::new(&["a"]);
    let (task_id, other_session) = seed_b(&setup);
    let client = &setup.agent;
    let mut env = McpEnv::of(
        &client.config_dir,
        &client.data_dir,
        &client.token,
        &setup.repo,
    );
    env.client = Some(CLIENT.to_owned());
    let mut mcp = Mcp::start(&env);
    let agent = [
        ("agent_name", json!("takeover-eval-other")),
        ("agent_role", json!("sub_agent")),
    ];
    step("the Pack, as the hook's would show it; the other agent's Session by its number");
    let pack = mcp.ok("pack", &json!({})).text;
    let session_number: u64 = pack
        .lines()
        .find(|line| line.contains(other_session.as_str()))
        .and_then(|line| line.strip_prefix("Session "))
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_else(|| panic!("no Session line of {other_session} in\n{pack}"))
        .parse()
        .unwrap();
    let mut end = json!({"session": session_number, "abandoned": true, "reason": "takeover"});
    let mut start = json!({});
    for (name, value) in &agent {
        end[name] = value.clone();
        start[name] = value.clone();
    }
    step("end it as abandoned, then begin with the agent's name and role");
    mcp.ok("end", &end);
    mcp.ok("begin", &start);
    step("run in the shell, in the same client session's context");
    ok_with(
        client,
        &[("MEMPROVE_CLIENT_SESSION", CLIENT)],
        &["run", "--test-format", "cargo", "--", "sh", "tests/a.sh"],
    );
    step("a fact citing the run that completes the seed's open item; end");
    let capsule = capsule_of(&setup, &task_id);
    let open = claim_of(&capsule, "the seed's open item")["number"]
        .as_u64()
        .unwrap();
    let seed_fact = claim_of(&capsule, "the seed's fact")["number"]
        .as_u64()
        .unwrap();
    step("R1 holds in the flow: the seed's fact is another agent's");
    let before = ledger_sequence(&setup);
    let refused = mcp.call(
        "checkpoint",
        &json!({
            "facts": [{"text": "the taker supersedes the seed's fact", "evidence": ["last"], "supersedes": [seed_fact]}],
            "next": {"text": "next step"},
        }),
    );
    assert!(refused.is_error, "{}", refused.line);
    assert_eq!(
        refused.text,
        format!("role_forbidden: {EDGE}"),
        "{}",
        refused.line
    );
    assert_eq!(ledger_sequence(&setup), before);
    mcp.ok(
        "checkpoint",
        &json!({
            "facts": [{"text": "the taker's fact", "evidence": ["last"], "supersedes": [open]}],
            "next": {"text": "the taker's next step"},
        }),
    );
    mcp.ok("end", &json!({"reason": "takeover done"}));
    mcp.finish();
    check_b(&setup, &task_id, &other_session);
}
