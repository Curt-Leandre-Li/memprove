//! P4-G (ADR 0014 E7.11.2 items 1 and 7, E7.11.3 items 1 to 4 and 6;
//! Owner conditions s, u, v, w) through the real binaries: the five read
//! tools of `memprove mcp serve` against a temporary `memproved`.
//! `pack` and `claims` give the bytes `memprove resume --pack` prints and
//! record the same last-Pack file; `tasks` numbers the Project's Tasks by
//! ascending `task_id`; `ledger_verify` reports a ledger that does not hold
//! as an error; tokens never leak, scopes hold, a stopped or silent daemon
//! is a tool error, a tampered Task still reads, and every default call
//! stays under Claude Code's 10,000-token warning line.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;

use memprove_domain::says_verified;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
};

use p4b_support::{Cli, Setup, claim, commit, git, sequence, step, write};
use p4g_support::{
    Called, Mcp, McpEnv, WARNING_TOKENS, base64, estimated_tokens, files_under, last_packs, level,
};
use serde_json::{Value, json};

/// The agent's configuration, data directory, and token file, in the
/// repository.
fn agent_env(setup: &Setup) -> McpEnv {
    McpEnv::of(
        &setup.agent.config_dir,
        &setup.agent.data_dir,
        &setup.agent.token,
        &setup.repo,
    )
}

/// A test script that fails while `flags/fail-<name>` exists; `flags/` is
/// ignored, so the repository state stays the same.
fn flaky(name: &str) -> String {
    format!(
        "#!/bin/sh\nif [ -f flags/fail-{name} ]; then\n  echo 'test {name} ... FAILED'\n  \
         echo 'test result: FAILED. 0 passed; 1 failed'\n  exit 1\nfi\necho 'test {name} ... ok'\n\
         echo 'test result: ok. 1 passed; 0 failed'\n"
    )
}

/// The text outside every `「…」` of `text`.
fn outside_quotes(text: &str) -> String {
    let mut depth = 0_usize;
    let mut outside = String::new();
    for character in text.chars() {
        match character {
            '「' => depth += 1,
            '」' => depth = depth.saturating_sub(1),
            other if depth == 0 => outside.push(other),
            _ => {}
        }
    }
    outside
}

/// The text of a non-Pack result, read as JSON: it is the serialized
/// `structuredContent` (M26).
fn text_json(called: &Called) -> Value {
    serde_json::from_str(&called.text).unwrap()
}

/// The `Digest: ` value of a Pack.
fn pack_digest(pack: &str) -> &str {
    pack.lines()
        .last()
        .and_then(|line| line.strip_prefix("Digest: "))
        .unwrap_or_else(|| panic!("no digest line:\n{pack}"))
}

/// The ledger of E7.11.2 item 1: three Sessions; facts verified (#4),
/// stale (#1), and partial (#5); a withdrawn fact (#2); an open item (#6);
/// a Decision (#8); two base commits; two runs of one argv (`tests/c.sh`,
/// passed then failed). Returns the Task and the open Session.
fn parity_ledger(setup: &Setup) -> (String, String) {
    let agent = &setup.agent;
    write(&setup.repo.join("tests/c.sh"), &flaky("c"));
    git(&setup.repo, &["add", "."]);
    commit(&setup.repo, "c can fail on a flag");
    step("Session 1 at the first base commit: facts on a and b");
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "P4-G parity",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    let a1 = agent.test("tests/a.sh");
    let b1 = agent.test("tests/b.sh");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "a passes",
        "--evidence",
        &a1,
        "--fact",
        "b passes",
        "--evidence",
        &b1,
        "--next",
        "n1",
    ]);
    agent.bytes(&["end", "--reason", "done"]);
    step("the second base commit");
    write(&setup.repo.join("README"), "fixture, changed\n");
    commit(&setup.repo, "the second base");
    step("Session 2: a and c pass, b's fact is withdrawn, then c fails");
    agent.ok(&["begin"]);
    let a2 = agent.test("tests/a.sh");
    let c2 = agent.test("tests/c.sh");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "a still passes",
        "--evidence",
        &a2,
        "--fact",
        "c passes",
        "--evidence",
        &c2,
        "--open",
        "d is unknown",
        "--withdraw",
        "2",
        "--withdraw-reason",
        "b was flaky",
        "--next",
        "n2",
    ]);
    fs::create_dir_all(setup.repo.join("flags")).unwrap();
    write(&setup.repo.join("flags/fail-c"), "1\n");
    agent.test("tests/c.sh");
    agent.bytes(&["decide", "use plan x", "--rationale", "simpler"]);
    agent.bytes(&["end", "--reason", "done"]);
    step("Session 3 stays open");
    let third = agent.ok(&["begin"]);
    let capsule = agent.capsule();
    for (number, verdict) in [(1, "stale"), (4, "verified"), (5, "partial")] {
        assert_eq!(claim(&capsule, number)["verdict"], verdict, "#{number}");
    }
    assert_eq!(claim(&capsule, 2)["standing"], "withdrawn");
    assert_eq!(claim(&capsule, 8)["kind"], "decision");
    assert_eq!(capsule["sessions"].as_array().unwrap().len(), 3);
    (task, third["session_id"].as_str().unwrap().to_owned())
}

/// Removes the last-Pack files of `config_dir`.
fn forget_packs(config_dir: &Path) {
    for (path, _) in last_packs(config_dir) {
        fs::remove_file(path).unwrap();
    }
}

/// E7.11.2 item 1: `pack` at 6000 (the default), 9000, 4000, the minimum,
/// and the maximum gives `resume --pack --budget <b>`'s bytes and the same
/// last-Pack file; `claims` gives `resume --pack --claims`'s bytes; `pack
/// {task}` gives `resume --task <id> --pack --budget 6000`'s bytes. Reads
/// never move the ledger's high-water mark.
/// U1 (A4): the `pack` tool's last-Pack record is `resume --pack`'s with
/// the Task's Project appended, as the hook writes it.
fn with_project(recorded: &[(PathBuf, Vec<u8>)], project_id: &str) -> Vec<(PathBuf, Vec<u8>)> {
    recorded
        .iter()
        .map(|(path, bytes)| {
            let text = String::from_utf8(bytes.clone()).unwrap();
            let record = text.trim_end().strip_suffix('}').unwrap();
            let with = format!("{record},\"project_id\":\"{project_id}\"}}\n");
            (path.clone(), with.into_bytes())
        })
        .collect()
}

#[test]
fn p4g_pack_and_claims_give_the_bytes_resume_prints() {
    let setup = Setup::new(&["a", "b"]);
    let agent = &setup.agent;
    let (task, session) = parity_ledger(&setup);
    let capsule = agent.capsule();
    let before = sequence(&capsule);
    let mut mcp = Mcp::start(&agent_env(&setup));
    for (arguments, budget) in [
        (json!({}), "6000"),
        (json!({"budget": 9000}), "9000"),
        (json!({"budget": 4000}), "4000"),
        (json!({"budget": 1000}), "1000"),
        (json!({"budget": 1_000_000}), "1000000"),
    ] {
        step(&format!(
            "pack {arguments} against resume --pack --budget {budget}"
        ));
        forget_packs(&agent.config_dir);
        let cli = agent.run(&["resume", "--pack", "--budget", budget], true);
        let recorded = last_packs(&agent.config_dir);
        forget_packs(&agent.config_dir);
        let called = mcp.call("pack", &arguments);
        let object = if called.is_error {
            format!(r#""structuredContent":{{"task_id":"{task}","session_id":"{session}","#)
        } else {
            // T21 (V6-F1): a Pack result's object is its second text block.
            format!(r#""text":"{{\"task_id\":\"{task}\",\"session_id\":\"{session}\","#)
        };
        assert!(called.line.contains(&object), "{}", called.line);
        if cli.code == Some(1) {
            assert_eq!(called.failure(), Some("over_budget"), "{}", called.line);
            assert_eq!(called.structured["budget"], budget.parse::<u64>().unwrap());
            assert!(called.structured["needed"].as_u64().unwrap() > 1000);
            assert!(last_packs(&agent.config_dir).is_empty());
            continue;
        }
        assert_eq!(cli.code, Some(0), "{}", cli.stderr);
        assert!(!called.is_error, "{}", called.line);
        assert_eq!(called.text, cli.stdout, "budget {budget}");
        assert_eq!(called.structured["pack_digest"], pack_digest(&cli.stdout));
        assert_eq!(called.structured["budget"], budget.parse::<u64>().unwrap());
        assert_eq!(
            called.structured["capsule_digest"],
            capsule["capsule_digest"]
        );
        assert_eq!(called.structured["project_sequence"], before);
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            last_packs(&agent.config_dir),
            with_project(&recorded, &setup.project_id),
            "the last-Pack file differs"
        );
    }
    step("pack {budget: 9000} is the bare resume --pack");
    let bare = agent.bytes(&["resume", "--pack"]);
    assert_eq!(mcp.ok("pack", &json!({"budget": 9000})).text, bare);

    step("claims [3, 5, 6, 7] against resume --pack --claims 3,5-7");
    let cli = agent.bytes(&["resume", "--pack", "--claims", "3,5-7"]);
    let called = mcp.ok("claims", &json!({"claims": [7, 3, 6, 5]}));
    assert_eq!(called.text, cli);
    assert_eq!(called.structured["claims"], json!([3, 5, 6, 7]));
    let missing = mcp.call("claims", &json!({"claims": [3, 99]}));
    assert_eq!(missing.failure(), Some("no_such_claim"));
    assert_eq!(missing.structured["number"], 99);

    step("pack {task: n} against resume --task <id> --pack --budget 6000");
    let tasks = mcp.ok("tasks", &json!({}));
    let number = tasks.structured["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["task_id"] == task.as_str())
        .unwrap()["number"]
        .as_u64()
        .unwrap();
    let cli = agent.bytes(&["resume", "--task", &task, "--pack", "--budget", "6000"]);
    let called = mcp.ok("pack", &json!({"task": number}));
    assert_eq!(called.text, cli);
    assert_eq!(called.structured["task_id"], task.as_str());
    assert_eq!(called.structured["session_id"], session.as_str());
    let claims_of = mcp.ok("claims", &json!({"task": number, "claims": [1]}));
    assert_eq!(
        claims_of.text,
        agent.bytes(&["resume", "--task", &task, "--pack", "--claims", "1"])
    );
    let none = mcp.call("pack", &json!({"task": number + 5}));
    assert_eq!(none.failure(), Some("no_such_task"));
    assert_eq!(none.structured["total"], number);

    let (stdout, stderr) = mcp.finish();
    assert_eq!(
        sequence(&agent.capsule()),
        before,
        "a read moved the ledger"
    );
    assert!(
        stderr.lines().all(|line| line.starts_with("memprove")),
        "{stderr}"
    );
    assert!(stdout.lines().count() >= 10);
}

/// A Task begun with `title` in this Workspace, with one passed run and a
/// verified fact; its Session stays open. Returns the Task and Session.
fn verified_task(setup: &Setup, title: &str) -> (String, String) {
    let begun = setup
        .agent
        .ok(&["begin", "--project", &setup.project_id, "--new-task", title]);
    setup.agent.test("tests/a.sh");
    setup
        .agent
        .ok(&["checkpoint", "create", "--fact", "a passes", "--next", "n1"]);
    (
        begun["task_id"].as_str().unwrap().to_owned(),
        begun["session_id"].as_str().unwrap().to_owned(),
    )
}

/// The Project's Task IDs as `task list` gives them, sorted.
fn sorted_task_ids(setup: &Setup) -> Vec<String> {
    let listed = setup.agent.ok(&[
        "task",
        "list",
        "--project",
        &setup.project_id,
        "--limit",
        "500",
    ]);
    let mut ids: Vec<String> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["task_id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

/// Pages 1 to `count` of `tasks`, checked page by page; returns the Task
/// IDs in number order. `ids` are the context's Task and Session.
fn numbered_pages(mcp: &mut Mcp, setup: &Setup, ids: (&str, &str), count: usize) -> Vec<String> {
    let mut numbered = Vec::new();
    for page in 1..=count {
        let arguments = if page == 1 {
            json!({})
        } else {
            json!({ "page": page })
        };
        let called = mcp.ok("tasks", &arguments);
        let content = &called.structured;
        assert!(called.line.contains(&format!(
            r#""structuredContent":{{"task_id":"{}","session_id":"{}","project_id":"{}""#,
            ids.0, ids.1, setup.project_id
        )));
        assert_eq!(content["page"], page);
        assert_eq!(content["page_size"], 20);
        let next = if page < count {
            json!(page + 1)
        } else {
            Value::Null
        };
        assert_eq!(content["next_page"], next);
        for item in content["tasks"].as_array().unwrap() {
            assert_eq!(item["number"], numbered.len() + 1);
            assert!(item["title"].as_str().unwrap().starts_with("task "));
            assert_eq!(item["title_chars"], 7);
            numbered.push(item["task_id"].as_str().unwrap().to_owned());
        }
        assert_eq!(&text_json(&called), content);
    }
    numbered
}

/// E7.11.2 item 7: numbers by ascending `task_id`, 20 per page, continuous
/// across pages, unchanged when a Task is added (the new one last).
#[test]
fn p4g_tasks_are_numbered_by_task_id_and_paged() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let (task, session) = verified_task(&setup, "task 00");
    for index in 1..45 {
        let title = format!("task {index:02}");
        agent.run(
            &[
                "task",
                "create",
                "--project",
                &setup.project_id,
                "--title",
                &title,
            ],
            true,
        );
    }
    let listed = sorted_task_ids(&setup);
    assert_eq!(listed.len(), 45);
    let mut mcp = Mcp::start(&agent_env(&setup));
    step("45 Tasks: three pages of 20, 20, and 5");
    assert_eq!(
        numbered_pages(&mut mcp, &setup, (&task, &session), 3),
        listed
    );
    for page in [json!({"page": 4}), json!({"page": 0}), json!({"page": "1"})] {
        let called = mcp.call("tasks", &page);
        assert_eq!(called.failure(), Some("invalid_arguments"), "{page}");
        assert_eq!(called.structured["parameter"], "page");
    }
    step("a new Task is numbered last; the old numbers stay");
    let added = agent.ok(&[
        "task",
        "create",
        "--project",
        &setup.project_id,
        "--title",
        "task 45",
    ])["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let again = numbered_pages(&mut mcp, &setup, (&task, &session), 3);
    assert_eq!(again[..45], listed[..]);
    assert_eq!(again[45], added);
    step("pack {task: 1} is the first Task by task_id");
    assert_eq!(listed[0], task);
    let cli = agent.bytes(&["resume", "--task", &listed[0], "--pack", "--budget", "6000"]);
    assert_eq!(mcp.ok("pack", &json!({"task": 1})).text, cli);
    mcp.finish();
}

/// `context` (local files only) and `ledger_verify` (the CLI's report);
/// `pack` records the last Pack, which `context` then shows.
#[test]
fn p4g_context_and_ledger_verify() {
    let setup = Setup::new(&["a"]);
    let (task, session) = verified_task(&setup, "verify");
    let mut mcp = Mcp::start(&agent_env(&setup));
    step("context: the directory's context, no last Pack yet");
    let context = mcp.ok("context", &json!({}));
    let shown = &context.structured;
    assert_eq!(shown["task_id"], task.as_str());
    assert_eq!(shown["session_id"], session.as_str());
    assert_eq!(shown["context_file"], "directory");
    assert_eq!(shown["lookup"], "found");
    assert_eq!(shown["contexts_with_session"], 1);
    assert_eq!(shown["last_pack"], Value::Null);
    assert_eq!(shown["context"]["task_title"], "verify");
    assert_eq!(shown["context"]["task_title_chars"], 6);
    assert_eq!(shown["context"]["runs_total"], 1);
    assert_eq!(shown["context"]["runs"][0]["label"], "r1");
    assert_eq!(&text_json(&context), shown);

    step("ledger_verify: the CLI's report");
    let report = mcp.ok("ledger_verify", &json!({}));
    let cli = setup.agent.ok(&["ledger", "verify"]);
    assert_eq!(report.structured["report"], cli);
    assert_eq!(cli["valid"], true);

    step("pack records the last Pack, which context shows");
    let pack = mcp.ok("pack", &json!({}));
    assert!(says_verified(&outside_quotes(&pack.text)));
    let after = mcp.ok("context", &json!({}));
    assert_eq!(after.structured["last_pack"]["task_id"], task.as_str());
    assert_eq!(after.structured["last_pack"]["budget"], 6000);
    assert_eq!(
        after.structured["last_pack"]["pack_digest"],
        pack.structured["pack_digest"]
    );
    mcp.finish();
}

/// Asserts that `pack` and `claims [1]` of a tampered Task give the CLI's
/// bytes (or, for `claims`, its refusal) and show no verified outside 「」.
fn tampered_reads(setup: &Setup, mcp: &mut Mcp, claims_listed: bool) {
    let tampered = mcp.ok("pack", &json!({}));
    assert_eq!(
        tampered.text,
        setup.agent.bytes(&["resume", "--pack", "--budget", "6000"])
    );
    assert!(
        !says_verified(&outside_quotes(&tampered.text)),
        "{}",
        tampered.text
    );
    assert!(
        tampered
            .text
            .lines()
            .any(|line| line.starts_with("Integrity: ")
                && line.contains("claim numbers may differ from before")),
        "{}",
        tampered.text
    );
    let claims = mcp.call("claims", &json!({"claims": [1]}));
    let cli = setup
        .agent
        .run(&["resume", "--pack", "--claims", "1"], true);
    if claims_listed {
        assert!(!claims.is_error, "{}", claims.line);
        assert_eq!(claims.text, cli.stdout);
        assert!(claims.text.contains("#1"), "{}", claims.text);
    } else {
        // The Evidence cannot be read: the Capsule lists no claim, as the
        // CLI says (exit 2).
        assert_eq!(claims.failure(), Some("no_such_claim"), "{}", claims.line);
        assert_eq!(cli.code, Some(2));
    }
    assert!(!says_verified(&outside_quotes(&claims.text)));
}

/// E7.11.3 item 4: a Task whose rows were edited behind the daemon still
/// reads through `pack` and `claims`, as `tampered`; `ledger_verify` is an
/// error carrying the report once an Evidence row no longer holds.
#[test]
fn p4g_a_tampered_task_reads_and_ledger_verify_is_an_error() {
    let mut setup = Setup::new(&["a"]);
    let (_, session) = verified_task(&setup, "tampered");
    step("a Session row edited behind the daemon: the Task reads as tampered");
    setup.stop();
    setup.tamper(&format!(
        "UPDATE sessions SET agent_id = 'someone-else' WHERE session_id = '{session}';"
    ));
    setup.start();
    let mut mcp = Mcp::start(&agent_env(&setup));
    tampered_reads(&setup, &mut mcp, true);
    mcp.finish();

    step("an Evidence row edited too: ledger_verify is an error carrying the report");
    setup.stop();
    setup.tamper("UPDATE evidence SET payload = replace(payload, '1 passed', '9 passed');");
    setup.start();
    let mut mcp = Mcp::start(&agent_env(&setup));
    let invalid = mcp.call("ledger_verify", &json!({}));
    assert_eq!(
        invalid.failure(),
        Some("ledger_invalid"),
        "{}",
        invalid.line
    );
    assert_eq!(invalid.structured["report"]["valid"], false);
    let first = &invalid.structured["report"]["first_failure"];
    assert!(first.is_object(), "{}", invalid.line);
    assert_eq!(
        invalid.text,
        format!(
            "the ledger does not verify; first inconsistency: {} {} (ledger sequence none); the reason is in report.first_failure.reason",
            first["record_type"].as_str().unwrap(),
            first["record_id"].as_str().unwrap()
        )
    );
    assert!(!says_verified(&outside_quotes(&invalid.text)));
    let cli = setup.agent.run(&["ledger", "verify"], true);
    assert_eq!(cli.code, Some(5));
    assert_eq!(
        serde_json::from_str::<Value>(cli.stdout.trim()).unwrap(),
        invalid.structured["report"]
    );
    tampered_reads(&setup, &mut mcp, false);
    mcp.finish();
    setup.stop();
}

/// A token holding `scopes`, written to a 0600 file; returns its ID and
/// the file.
fn token(setup: &Setup, label: &str, scopes: &[&str]) -> (String, PathBuf) {
    let mut words = vec!["token", "create", "--label", label];
    for scope in scopes {
        words.extend(["--scope", scope]);
    }
    let created = setup.operator.run(&words, true);
    assert_eq!(created.code, Some(0), "{}", created.stderr);
    let created: Value = serde_json::from_str(created.stdout.trim()).unwrap();
    let path = setup.root_path.join(format!("{label}.token"));
    fs::write(&path, created["secret"].as_str().unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    (created["token_id"].as_str().unwrap().to_owned(), path)
}

/// The read tools, each with a default call.
const READS: [(&str, &str); 5] = [
    ("pack", "{}"),
    ("claims", r#"{"claims":[1]}"#),
    ("tasks", "{}"),
    ("ledger_verify", "{}"),
    ("context", "{}"),
];

/// Every read tool's default call succeeds in `env`; returns what the
/// server wrote.
fn every_read(env: &McpEnv) -> (String, String) {
    let mut mcp = Mcp::start(env);
    for (tool, arguments) in READS {
        mcp.ok(tool, &serde_json::from_str(arguments).unwrap());
    }
    mcp.finish()
}

/// E7.11.3 item 1: the token from `MEMPROVE_TOKEN`,
/// `MEMPROVE_TOKEN_FILE`, and `<config>/token` never appears, plain or
/// base64, on either stream or in a file the server wrote; a token file
/// other users can read is refused without its content shown.
#[test]
fn p4g_the_token_never_leaks() {
    let setup = Setup::new(&["a"]);
    verified_task(&setup, "tokens");
    let secret = fs::read_to_string(&setup.agent.token)
        .unwrap()
        .trim()
        .to_owned();
    let config = setup.agent.config_dir.clone();
    let by_file = agent_env(&setup);
    let by_variable = McpEnv {
        token_file: None,
        token: Some(secret.clone()),
        ..by_file.clone()
    };
    let by_config = McpEnv {
        token_file: None,
        data_dir: None,
        ..by_file.clone()
    };
    fs::write(config.join("token"), format!("{secret}\n")).unwrap();
    fs::set_permissions(config.join("token"), fs::Permissions::from_mode(0o600)).unwrap();
    let data_dir = format!("{}\n", setup.agent.data_dir.display());
    fs::write(config.join("data-dir"), data_dir).unwrap();
    for env in [by_variable, by_file.clone(), by_config] {
        step(&format!(
            "the token from {}",
            if env.token.is_some() {
                "MEMPROVE_TOKEN"
            } else if env.token_file.is_some() {
                "MEMPROVE_TOKEN_FILE"
            } else {
                "<config>/token"
            }
        ));
        let (stdout, stderr) = every_read(&env);
        for needle in [secret.clone(), base64(secret.as_bytes())] {
            assert!(!stdout.contains(&needle), "the token is on standard output");
            assert!(!stderr.contains(&needle), "the token is on standard error");
            for (path, bytes) in files_under(&config) {
                assert!(
                    path == config.join("token")
                        || !String::from_utf8_lossy(&bytes).contains(&needle),
                    "the token is in {}",
                    path.display()
                );
            }
        }
    }
    step("a token file other users can read: its path at most, never its content");
    let loose = setup.root_path.join("loose.token");
    fs::write(&loose, &secret).unwrap();
    fs::set_permissions(&loose, fs::Permissions::from_mode(0o644)).unwrap();
    let mut mcp = Mcp::start(&McpEnv {
        token_file: Some(loose),
        ..by_file
    });
    let called = mcp.call("pack", &json!({}));
    assert_eq!(called.failure(), Some("token_unusable"));
    let (stdout, stderr) = mcp.finish();
    assert!(!stdout.contains(&secret) && !stderr.contains(&secret));
}

/// E7.11.3 item 2: an `mcp-read` token serves every read tool; an
/// `mcp-write`-only or `ingest-only` token gets `403 policy_denied` naming
/// `mcp-read`, a revoked one `401`, none `no_token`; the ledger does not
/// move.
#[test]
fn p4g_scopes_hold_per_operation() {
    let setup = Setup::new(&["a"]);
    verified_task(&setup, "scopes");
    let before = sequence(&setup.agent.capsule());
    let by_file = agent_env(&setup);
    let (_, read_only) = token(&setup, "read-only", &["mcp-read"]);
    let (_, write_only) = token(&setup, "write-only", &["mcp-write"]);
    let (_, ingest_only) = token(&setup, "ingest-only", &["ingest-only"]);
    let (revoked_id, revoked) = token(&setup, "revoked", &["mcp-read"]);
    setup.operator.ok(&["token", "revoke", &revoked_id]);
    let with = |path: &Path| McpEnv {
        token_file: Some(path.to_path_buf()),
        ..by_file.clone()
    };
    step("mcp-read only: every read tool");
    every_read(&with(&read_only));
    for (path, status, code, scope) in [
        (&write_only, 403, "policy_denied", Some("mcp-read")),
        (&ingest_only, 403, "policy_denied", Some("mcp-read")),
        (&revoked, 401, "authentication_required", None),
    ] {
        step(&format!("{code} for {}", path.display()));
        let mut mcp = Mcp::start(&with(path));
        for (tool, arguments) in READS.iter().take(4) {
            let called = mcp.call(tool, &serde_json::from_str(arguments).unwrap());
            assert_eq!(called.failure(), Some("problem"), "{}", called.line);
            assert_eq!(called.structured["status"], status);
            assert_eq!(called.structured["code"], code);
            if let Some(scope) = scope {
                assert!(
                    called.text.ends_with(&format!("{scope} is needed)")),
                    "{}",
                    called.text
                );
            }
        }
        // `context` needs no token.
        mcp.ok("context", &json!({}));
        mcp.finish();
    }
    step("no token at all");
    let mut mcp = Mcp::start(&McpEnv {
        token_file: None,
        ..by_file
    });
    assert_eq!(mcp.call("pack", &json!({})).failure(), Some("no_token"));
    mcp.finish();
    assert_eq!(
        sequence(&setup.agent.capsule()),
        before,
        "a read moved the ledger"
    );
}

/// E7.11.3 item 3: a daemon that accepts and never answers is
/// `daemon_unreachable` after the CLI's timeout; a stopped daemon is
/// `daemon_not_running`; the server keeps serving after either.
#[test]
fn p4g_a_silent_or_stopped_daemon_is_a_tool_error() {
    let mut setup = Setup::new(&["a"]);
    verified_task(&setup, "daemon down");
    let by_file = agent_env(&setup);
    step("a daemon that accepts and never answers");
    let silent = setup.root_path.join("silent");
    fs::create_dir_all(&silent).unwrap();
    let listener = UnixListener::bind(silent.join("memproved.sock")).unwrap();
    let mut mcp = Mcp::start(&McpEnv {
        data_dir: Some(silent),
        ..by_file.clone()
    });
    let called = mcp.call("pack", &json!({}));
    assert_eq!(
        called.failure(),
        Some("daemon_unreachable"),
        "{}",
        called.line
    );
    assert_eq!(called.structured["operation"], "getTaskCapsule");
    mcp.ok("context", &json!({}));
    mcp.finish();
    drop(listener);
    step("no daemon at all");
    setup.stop();
    let mut mcp = Mcp::start(&by_file);
    for (tool, arguments) in READS.iter().take(4) {
        let called = mcp.call(tool, &serde_json::from_str(arguments).unwrap());
        assert_eq!(
            called.failure(),
            Some("daemon_not_running"),
            "{}",
            called.line
        );
    }
    mcp.ok("context", &json!({}));
    mcp.finish();
}

/// `count` facts of 4,096 characters, `per` per Checkpoint, each citing
/// the latest run.
fn long_facts(agent: &Cli, from: usize, count: usize, per: usize) {
    for first in (from..from + count).step_by(per) {
        let mut words = vec!["checkpoint".to_owned(), "create".to_owned()];
        for index in first..first + per {
            words.push("--fact".to_owned());
            words.push(format!("{index:04}{}", "é".repeat(4092)));
        }
        words.extend(["--next".to_owned(), format!("n{first}")]);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let run = agent.run(&words, true);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
    }
}

/// Asserts that the default call's text and its `structuredContent` each
/// stay under the 10,000-token line by E7.6's ceilings.
fn under_the_line(called: &Called) {
    let structured = serde_json::to_string(&called.structured).unwrap();
    for (what, text) in [("text", &called.text), ("structuredContent", &structured)] {
        let tokens = estimated_tokens(text);
        assert!(
            tokens < WARNING_TOKENS,
            "{what} of {} is about {tokens:.0} tokens",
            called.line.chars().take(120).collect::<String>()
        );
    }
}

/// E7.11.3 item 6 (condition w): at every omission level the `pack` text
/// holds at most `budget` UTF-16 code units and its line at most `budget ×
/// 3 + 1,024` bytes; 60 facts of 4,096 Chinese characters give a default
/// `pack` of at most 6,000 units; the maximum budget gives the whole Pack;
/// every other default call, with Task titles of 4,096 characters, stays
/// under the 10,000-token line.
#[test]
fn p4g_every_omission_level_fits_and_default_calls_stay_under_the_line() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "sizes",
    ]);
    step("30 long facts at the first base, 20 changed files, 30 at the second");
    agent.test("tests/a.sh");
    long_facts(agent, 0, 30, 6);
    for index in 0..20 {
        write(
            &setup.repo.join(format!(
                "changed-file-with-a-long-name-number-{index:02}.txt"
            )),
            "x\n",
        );
    }
    git(&setup.repo, &["add", "."]);
    commit(&setup.repo, "twenty files");
    agent.test("tests/a.sh");
    long_facts(agent, 30, 30, 6);
    let mut mcp = Mcp::start(&agent_env(&setup));

    step("the default call, and claims of one long fact");
    let default = mcp.ok("pack", &json!({}));
    assert!(default.text.encode_utf16().count() <= 6000);
    assert!(default.line.len() <= 6000 * 3 + 1024);
    under_the_line(&default);
    under_the_line(&mcp.ok("claims", &json!({"claims": [1]})));

    step("the whole Pack, then every level down to the minimum");
    let full = mcp.ok("pack", &json!({"budget": 1_000_000}));
    assert_eq!(level(&full.text), 0);
    let whole = full.text.encode_utf16().count();
    assert!(whole > 200_000, "{whole}");
    // The first line names the budget: at a budget of one digit fewer the
    // whole Pack is one code unit shorter, so `whole - 2` is the first
    // budget it does not fit.
    let mut budgets = vec![whole, whole - 2, whole - 50];
    let mut budget = whole;
    while budget > 1000 {
        budget = (budget * 4 / 5).max(1000);
        budgets.push(budget);
    }
    let mut levels = BTreeSet::new();
    for budget in budgets {
        let called = mcp.call("pack", &json!({ "budget": budget }));
        if called.is_error {
            assert_eq!(called.failure(), Some("over_budget"), "{}", called.line);
            assert!(budget < 6000, "{budget}");
            continue;
        }
        assert!(called.text.encode_utf16().count() <= budget, "{budget}");
        assert!(called.line.len() <= budget * 3 + 1024, "{budget}");
        levels.insert(level(&called.text));
    }
    assert_eq!(levels, BTreeSet::from([0, 1, 2, 3, 4, 5]), "{levels:?}");

    step("a Task title of 4,096 characters in the context and in tasks");
    agent.bytes(&["end", "--reason", "done"]);
    let long_title = "é".repeat(4096);
    agent.ok(&["begin", "--new-task", &long_title]);
    for index in 0..25 {
        agent.run(
            &[
                "task",
                "create",
                "--project",
                &setup.project_id,
                "--title",
                &format!("{index:02}{}", "é".repeat(4094)),
            ],
            true,
        );
    }
    let context = mcp.ok("context", &json!({}));
    assert_eq!(context.structured["context"]["task_title_chars"], 4096);
    assert_eq!(
        context.structured["context"]["task_title"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        120
    );
    under_the_line(&context);
    let tasks = mcp.ok("tasks", &json!({}));
    assert_eq!(tasks.structured["tasks"].as_array().unwrap().len(), 20);
    under_the_line(&tasks);
    under_the_line(&mcp.ok("ledger_verify", &json!({})));
    let pack = mcp.call("pack", &json!({}));
    if !pack.is_error {
        under_the_line(&pack);
    }
    mcp.finish();
}
