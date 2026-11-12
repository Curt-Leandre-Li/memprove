//! P3-B acceptance (ADR 0013 D4): the real `memprove` CLI against the real
//! `memproved` over a temporary Git repository. The operator holds the
//! bootstrap control token; every agent step uses one token that carries
//! only `mcp-read` and `mcp-write`.
//!
//! a. Session 1 at commit X: run the tests (PASS), Checkpoint C1 with a
//!    fact, an open item, and a next action; end Session 1.
//! b. Commit Y.
//! c. Session 2 at Y: run the tests (PASS), Checkpoint C2; Session 2 is
//!    left open.
//! d. Commit Z.
//! e. `memprove resume`: both Checkpoints' statements, numbered, with
//!    their verdicts, the changed files since X and since Y, one rerun for
//!    the shared argv, and Session 2 not ended; `--text` printed.
//! f. Under one N and probe, the latest Checkpoint's statements get the
//!    Preview's Evidence verdicts.
//! g. Two calls in a row: identical bytes; the Event count does not
//!    change.
//! h. The client writes each of the three reserved Event types: 400.
//! i. Daemon stopped, one Evidence row changed with `sqlite3`: after the
//!    restart the whole Capsule is tampered, naming the record.
//!
//! Run with `--nocapture` to print the transcript; token secrets are never
//! printed.

use memprove_domain::says_verified;
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use memprove_domain::CanonicalJson;
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(15);
const AGENT_ID: &str = "018f47a0-0000-7000-8000-00000000a6e1";
const TEST_SCRIPT: &str = "#!/bin/sh
# Three checks over the fixture repository.
set -e
test -f README && echo 'test readme_exists ... ok'
grep -q fixture README && echo 'test readme_content ... ok'
test \"$(git rev-parse --is-inside-work-tree)\" = true && echo 'test inside_worktree ... ok'
echo 'test result: ok. 3 passed; 0 failed'
";

fn cli_binary() -> PathBuf {
    let daemon = PathBuf::from(env!("CARGO_BIN_EXE_memproved"));
    let cli = daemon.with_file_name("memprove");
    assert!(
        cli.exists(),
        "memprove not found next to {}; run `cargo test --workspace`",
        daemon.display()
    );
    cli
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn step(title: &str) {
    println!("\n===== {title} =====");
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    println!("$ git -C {} {}", directory.display(), arguments.join(" "));
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    text(&output.stdout)
}

fn commit(directory: &Path, message: &str) -> String {
    git(
        directory,
        &[
            "-c",
            "user.name=memprove Test",
            "-c",
            "user.email=memprove@example.invalid",
            "commit",
            "--quiet",
            "-am",
            message,
        ],
    );
    git(directory, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn write(path: &Path, contents: &str) {
    println!("$ printf {contents:?} > {}", path.display());
    fs::write(path, contents).unwrap();
}

fn git_repository(path: &Path) {
    fs::create_dir_all(path.join("tests")).unwrap();
    git(path, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(path.join("README"), b"fixture\n").unwrap();
    fs::write(path.join("tests/run.sh"), TEST_SCRIPT).unwrap();
    git(path, &["add", "README", "tests/run.sh"]);
    commit(path, "fixture with a test script");
}

fn sqlite(database: &Path, sql: &str) -> String {
    println!("$ sqlite3 {} {sql:?}", database.display());
    let output = Command::new("sqlite3")
        .arg(database)
        .arg(sql)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    print!("{stdout}");
    stdout
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Clone)]
struct Cli {
    data_dir: PathBuf,
    token: PathBuf,
    /// Working directory of the CLI, and so of the command `run` executes.
    cwd: PathBuf,
    label: &'static str,
}

impl Cli {
    fn with(&self, token: &Path, cwd: &Path, label: &'static str) -> Self {
        Self {
            token: token.to_path_buf(),
            cwd: cwd.to_path_buf(),
            label,
            ..self.clone()
        }
    }

    fn run(&self, words: &[&str], quiet: bool) -> Run {
        let output = isolated(cli_binary())
            .current_dir(&self.cwd)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--token-file")
            .arg(&self.token)
            .env_remove("MEMPROVE_DATA_DIR")
            .env_remove("MEMPROVE_TOKEN")
            .env_remove("MEMPROVE_TOKEN_FILE")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(words)
            .output()
            .unwrap();
        let run = Run {
            code: output.status.code(),
            stdout: text(&output.stdout),
            stderr: text(&output.stderr),
        };
        if !quiet {
            println!(
                "[{}] (cwd {}) $ memprove {}\n{}{}[exit {}]",
                self.label,
                self.cwd.display(),
                words.join(" "),
                run.stdout,
                run.stderr,
                run.code
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string())
            );
        }
        run
    }

    fn ok(&self, words: &[&str]) -> Value {
        let run = self.run(words, false);
        assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
        serde_json::from_str(run.stdout.trim()).unwrap()
    }

    /// `recover preview`: the exact stdout bytes and the parsed document.
    fn preview(&self, task_id: &str, workspace_id: Option<&str>) -> (String, Value) {
        let mut words = vec!["recover", "preview", "--task", task_id];
        if let Some(workspace_id) = workspace_id {
            words.extend(["--workspace", workspace_id]);
        }
        let run = self.run(&words, false);
        assert_eq!(run.code, Some(0), "{}{}", run.stdout, run.stderr);
        let document: Value = serde_json::from_str(run.stdout.trim()).unwrap();
        check_digest(run.stdout.trim(), &document);
        (run.stdout, document)
    }
}

/// The body is JCS, and `preview_digest` is the SHA-256 of the JCS bytes of
/// the document without it.
fn check_digest(body: &str, document: &Value) {
    assert_eq!(CanonicalJson::parse(body).unwrap().as_str(), body);
    let mut without = document.clone();
    without.as_object_mut().unwrap().remove("preview_digest");
    let digest = CanonicalJson::parse(&without.to_string()).unwrap().sha256();
    assert_eq!(document["preview_digest"], format!("sha256:{digest}"));
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap())
        .collect()
}

fn str_of<'a>(value: &'a Value, name: &str) -> &'a str {
    value[name]
        .as_str()
        .unwrap_or_else(|| panic!("{name} is not a string in {value}"))
}

struct Daemon {
    child: Child,
}

impl Daemon {
    fn start(cli: &Cli, log: &Path) -> Self {
        let child = isolated(env!("CARGO_BIN_EXE_memproved"))
            .arg("--data-dir")
            .arg(&cli.data_dir)
            .env_remove("MEMPROVE_DATA_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(log).unwrap())
            .spawn()
            .unwrap();
        // U1 (C6): the guard exists before anything below can panic.
        let daemon = Self { child };
        println!(
            "$ memproved --data-dir <DATA> &   # pid {}",
            daemon.child.id()
        );
        let deadline = Instant::now() + TIMEOUT;
        while cli.run(&["health", "ready"], true).code != Some(0) {
            assert!(
                Instant::now() < deadline,
                "memproved never became ready:\n{}",
                fs::read_to_string(log).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(20));
        }
        cli.ok(&["health", "ready"]);
        daemon
    }

    fn signal(mut self, name: &str, number: i32) {
        println!("$ kill -{name} {}", self.child.id());
        let sent = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
        let status = self.child.wait().unwrap();
        println!("memproved exited: {status}");
        if number == 9 {
            assert_eq!(status.signal(), Some(9));
        } else {
            assert_eq!(status.code(), Some(0));
        }
    }

    fn stop(self) {
        self.signal("TERM", 15);
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn event_count(operator: &Cli, project_id: &str) -> usize {
    operator.ok(&["event", "list", "--project", project_id, "--limit", "500"])["items"]
        .as_array()
        .unwrap()
        .len()
}

fn checkpoint_request(
    requests: &Path,
    name: &str,
    ids: (&str, &str, &str, &str),
    evidence_id: &str,
    statements: (&str, &str, &str),
) -> PathBuf {
    let (workspace_id, task_id, session_id, snapshot_id) = ids;
    let (fact, open, next) = statements;
    let request = requests.join(format!("{name}.json"));
    let body = format!(
        r#"{{"schema_version":1,"workspace_id":"{workspace_id}","task_id":"{task_id}",
            "session_id":"{session_id}","snapshot_id":"{snapshot_id}",
            "objective":"Ship the fixture",
            "verified_completed_work":[{{"classification":"verified_fact",
                "text":"{fact}","evidence_ids":["{evidence_id}"]}}],
            "open_items":[{{"classification":"unknown","text":"{open}"}}],
            "next_action":{{"classification":"recommendation","text":"{next}"}}}}"#
    );
    println!("$ cat > {} <<'EOF'\n{body}\nEOF", request.display());
    fs::write(&request, body).unwrap();
    request
}

/// One Session at the current HEAD: run the tests, record a Checkpoint.
/// Returns the Session, the Evidence, the Checkpoint, and the HEAD.
fn session_with_checkpoint(
    agent: &Cli,
    requests: &Path,
    ids: (&str, &str),
    name: &str,
    statements: (&str, &str, &str),
) -> (String, String, String, String) {
    let (workspace_id, task_id) = ids;
    let session = agent.ok(&[
        "session",
        "create",
        "--workspace",
        workspace_id,
        "--agent",
        AGENT_ID,
        "--task",
        task_id,
    ]);
    let session_id = str_of(&session, "session_id").to_owned();
    let passed = agent.ok(&[
        "run",
        "--session",
        &session_id,
        "--workspace",
        workspace_id,
        "--task",
        task_id,
        "--test",
        "--test-cases",
        "3",
        "--test-name",
        "fixture",
        "--",
        "sh",
        "tests/run.sh",
    ]);
    assert_eq!(str_of(&passed, "status"), "PASS");
    let evidence_id = str_of(&passed, "evidence_id").to_owned();
    let snapshot_id = str_of(&passed["snapshot"], "snapshot_id").to_owned();
    let head = str_of(&passed["snapshot"], "head_oid").to_owned();
    let request = checkpoint_request(
        requests,
        name,
        (workspace_id, task_id, &session_id, &snapshot_id),
        &evidence_id,
        statements,
    );
    let checkpoint = agent.ok(&["checkpoint", "create", request.to_str().unwrap()]);
    let checkpoint_id = str_of(&checkpoint, "checkpoint_id").to_owned();
    (session_id, evidence_id, checkpoint_id, head)
}

impl Cli {
    /// `resume`: the exact stdout bytes and the parsed document.
    fn resume(&self, task_id: &str) -> (String, Value) {
        let run = self.run(&["resume", "--task", task_id], false);
        assert_eq!(run.code, Some(0), "{}{}", run.stdout, run.stderr);
        let body = run.stdout.trim();
        let document: Value = serde_json::from_str(body).unwrap();
        assert_eq!(CanonicalJson::parse(body).unwrap().as_str(), body);
        let mut without = document.clone();
        without.as_object_mut().unwrap().remove("capsule_digest");
        let digest = CanonicalJson::parse(&without.to_string()).unwrap().sha256();
        assert_eq!(document["capsule_digest"], format!("sha256:{digest}"));
        (run.stdout, document)
    }

    fn resume_text(&self, task_id: &str) -> String {
        let run = self.run(&["resume", "--task", task_id, "--text", "--all"], false);
        assert_eq!(run.code, Some(0), "{}{}", run.stdout, run.stderr);
        run.stdout
    }
}

fn verdicts(evidence: &Value) -> Vec<(String, String, Vec<String>)> {
    evidence
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                str_of(item, "evidence_id").to_owned(),
                str_of(item, "verdict").to_owned(),
                strings(&item["reasons"])
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            )
        })
        .collect()
}

#[test]
#[allow(clippy::too_many_lines)] // One acceptance scenario, a through i, in order.
fn task_capsule_acceptance_through_real_binaries() {
    let root = tempfile::tempdir().unwrap();
    let root_path = fs::canonicalize(root.path()).unwrap();
    let repo = root_path.join("repo");
    let requests = root_path.join("requests");
    fs::create_dir(&requests).unwrap();
    step("setup: repository, daemon, operator, agent token");
    git_repository(&repo);
    let operator = Cli {
        data_dir: root_path.join("data"),
        token: root_path.join("control.token"),
        cwd: repo.clone(),
        label: "operator/control",
    };
    println!(
        "$ memproved bootstrap --data-dir <DATA> --output {}",
        operator.token.display()
    );
    let issued = isolated(env!("CARGO_BIN_EXE_memproved"))
        .arg("bootstrap")
        .arg("--data-dir")
        .arg(&operator.data_dir)
        .arg("--output")
        .arg(&operator.token)
        .env_remove("MEMPROVE_DATA_DIR")
        .output()
        .unwrap();
    assert_eq!(issued.status.code(), Some(0), "{}", text(&issued.stderr));
    println!("[exit 0] (token written to the 0600 file, not printed)");
    let daemon = Daemon::start(&operator, &root_path.join("daemon-1.log"));
    let registered = operator.ok(&["project", "register", repo.to_str().unwrap()]);
    let project_id = str_of(&registered, "project_id").to_owned();
    let workspace_id = str_of(&registered, "root_workspace_id").to_owned();
    let words = [
        "token",
        "create",
        "--label",
        "agent",
        "--scope",
        "mcp-read",
        "--scope",
        "mcp-write",
    ];
    let created = operator.run(&words, true);
    assert_eq!(created.code, Some(0), "{}", created.stderr);
    let created: Value = serde_json::from_str(created.stdout.trim()).unwrap();
    let secret = str_of(&created, "secret").to_owned();
    let mut shown = created.clone();
    shown["secret"] = Value::String("<redacted>".to_owned());
    println!(
        "[operator/control] $ memprove {}\n{shown}\n[exit 0]",
        words.join(" ")
    );
    assert_eq!(strings(&created["scopes"]), ["mcp-read", "mcp-write"]);
    let agent_token = root_path.join("agent.token");
    fs::write(&agent_token, &secret).unwrap();
    fs::set_permissions(&agent_token, fs::Permissions::from_mode(0o600)).unwrap();
    let agent = operator.with(&agent_token, &repo, "agent/mcp-read+mcp-write");
    let task_id = str_of(
        &agent.ok(&[
            "task",
            "create",
            "--project",
            &project_id,
            "--title",
            "Ship the fixture",
        ]),
        "task_id",
    )
    .to_owned();
    let ids = (workspace_id.as_str(), task_id.as_str());

    step("a. Session 1 at X: run the tests, Checkpoint C1, end Session 1");
    let x = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let (session_1, evidence_1, checkpoint_1, head_1) = session_with_checkpoint(
        &agent,
        &requests,
        ids,
        "c1",
        ("tests pass", "docs not reviewed", "update the README"),
    );
    assert_eq!(head_1, x);
    agent.ok(&[
        "session",
        "end",
        &session_1,
        "--revision",
        "1",
        "--reason",
        "C1 recorded",
    ]);

    step("b. commit Y");
    write(&repo.join("README"), "fixture\nsecond line\n");
    let y = commit(&repo, "README: second line");

    step("c. Session 2 at Y: run the tests, Checkpoint C2; Session 2 left open");
    let (session_2, evidence_2, checkpoint_2, head_2) = session_with_checkpoint(
        &agent,
        &requests,
        ids,
        "c2",
        (
            "README updated and tests pass",
            "release notes missing",
            "write the release notes",
        ),
    );
    assert_eq!(head_2, y);

    step("d. commit Z");
    write(&repo.join("README"), "fixture\nsecond line\nthird line\n");
    let z = commit(&repo, "README: third line");

    step("e. memprove resume: every statement of both Checkpoints");
    let (bytes, capsule) = agent.resume(&task_id);
    let claims = capsule["claims"].as_array().unwrap();
    let listed = claims
        .iter()
        .map(|claim| {
            (
                claim["number"].as_u64().unwrap(),
                str_of(claim, "text"),
                str_of(claim, "kind"),
                str_of(claim, "standing"),
                str_of(claim, "verdict"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        listed,
        [
            (1, "tests pass", "fact", "active", "stale"),
            (2, "docs not reviewed", "open_item", "active", "partial"),
            (
                3,
                "update the README",
                "next_action",
                "superseded",
                "partial"
            ),
            (
                4,
                "README updated and tests pass",
                "fact",
                "active",
                "stale"
            ),
            (5, "release notes missing", "open_item", "active", "partial"),
            (
                6,
                "write the release notes",
                "next_action",
                "active",
                "partial"
            ),
        ]
    );
    assert_eq!(claims[0]["checkpoint_id"], checkpoint_1.as_str());
    assert_eq!(claims[3]["checkpoint_id"], checkpoint_2.as_str());
    assert_eq!(claims[0]["session_id"], session_1.as_str());
    assert_eq!(claims[3]["session_id"], session_2.as_str());
    assert_eq!(claims[0]["evidence"][0]["evidence_id"], evidence_1.as_str());
    assert_eq!(claims[0]["evidence"][0]["base_commit"], x.as_str());
    assert_eq!(claims[3]["evidence"][0]["evidence_id"], evidence_2.as_str());
    assert_eq!(claims[3]["evidence"][0]["base_commit"], y.as_str());
    assert_eq!(
        strings(&claims[0]["reasons"]),
        ["head_changed", "index_changed", "evidence_stale"]
    );
    assert_eq!(capsule["status"], "stale");
    assert_eq!(capsule["bindings"]["probe"]["head_oid"], z.as_str());
    assert_eq!(
        capsule["changed_paths_by_base"],
        serde_json::json!([
            { "base_commit": y, "probed": true, "complete": true, "paths": ["repo://README"] },
            { "base_commit": x, "probed": true, "complete": true, "paths": ["repo://README"] },
        ])
    );
    // One rerun for the argv both facts rest on.
    let rerun = capsule["rerun"].as_array().unwrap();
    assert_eq!(rerun.len(), 1);
    assert_eq!(strings(&rerun[0]["argv"]), ["sh", "tests/run.sh"]);
    assert_eq!(rerun[0]["claims"], serde_json::json!([1, 4]));
    assert_eq!(rerun[0]["at_head"], z.as_str());
    let sessions = capsule["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["end_state"], "ended");
    assert_eq!(sessions[1]["end_state"], "open");
    let templates = capsule["continuation"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["class"].as_u64().unwrap(), str_of(item, "template")))
        .collect::<Vec<_>>();
    assert_eq!(
        templates,
        [
            (0, "header"),
            (2, "session_open"),
            (3, "open_item"),
            (3, "open_item"),
            (4, "fact"),
            (4, "fact"),
            (5, "rerun"),
            (8, "next_action"),
            (9, "superseded"),
        ]
    );
    assert_eq!(strings(&capsule["gaps"]), Vec::<&str>::new());
    let mut shown = capsule.clone();
    shown["capsule_digest"] = capsule["capsule_digest"].clone();
    println!(
        "resume JSON:\n{}",
        serde_json::to_string_pretty(&shown).unwrap()
    );
    let text_output = agent.resume_text(&task_id);
    assert!(!says_verified(&text_output), "nothing is verified at Z");
    // P3-C2: statement text is quoted in 「」.
    assert!(text_output.contains("#1 fact stale: 「tests pass」"));
    assert!(text_output.contains("Rerun: 「sh tests/run.sh」"));

    step("f. one N and probe: the latest Checkpoint agrees with the Preview");
    let (_, preview) = agent.preview(&task_id, None);
    let facts = preview["facts"].as_array().unwrap();
    let latest = claims
        .iter()
        .filter(|claim| claim["checkpoint_id"] == checkpoint_2.as_str())
        .collect::<Vec<_>>();
    assert_eq!(latest.len(), facts.len());
    for (claim, fact) in latest.iter().zip(facts) {
        assert_eq!(claim["text"], fact["text"]);
        assert_eq!(verdicts(&claim["evidence"]), verdicts(&fact["evidence"]));
    }
    assert_eq!(
        preview["bindings"]["ledger"]["project_sequence"],
        capsule["bindings"]["ledger"]["project_sequence"]
    );

    step("g. two calls in a row: identical bytes; Event count unchanged");
    let before = event_count(&operator, &project_id);
    let (first, _) = agent.resume(&task_id);
    let (second, _) = agent.resume(&task_id);
    let after = event_count(&operator, &project_id);
    assert_eq!(first, second);
    assert_eq!(first, bytes);
    assert_eq!(before, after);
    println!(
        "identical: {} bytes; events before {before}, after {after}",
        first.trim().len()
    );

    step("h. the client writes each reserved Event type: 400 invalid_request");
    for event_type in ["claim.superseded", "decision.recorded", "session.crashed"] {
        let envelope = requests.join(format!("{event_type}.json"));
        let body = serde_json::json!({
            "envelope_version": "1.0",
            "event_id": memprove_domain::EventId::new(),
            "project_id": project_id,
            "workspace_id": workspace_id,
            "session_id": session_2,
            "event_type": event_type,
            "payload_schema": format!("{event_type}@1"),
            "producer": { "name": "agent-cli", "version": "1.0.0", "sequence": 1 },
            "privacy_class": "internal",
            "payload": { "task_id": task_id },
        });
        fs::write(&envelope, body.to_string()).unwrap();
        let run = agent.run(&["event", "ingest", envelope.to_str().unwrap()], false);
        assert_eq!(run.code, Some(1), "{}{}", run.stdout, run.stderr);
        assert!(run.stderr.contains("invalid_request"), "{}", run.stderr);
    }
    assert_eq!(event_count(&operator, &project_id), before);

    step("i. daemon stopped, one Evidence row changed with sqlite3: tampered");
    daemon.stop();
    let database = operator
        .data_dir
        .join("projects")
        .join(&project_id)
        .join("state.sqlite");
    let tamper = format!(
        "DROP TRIGGER evidence_reject_update; \
         UPDATE evidence SET payload = replace(payload, '3 passed', '9 passed') \
         WHERE evidence_id = '{evidence_1}'; SELECT changes();"
    );
    assert_eq!(sqlite(&database, &tamper).trim(), "1");
    let daemon = Daemon::start(&operator, &root_path.join("daemon-2.log"));
    let (tampered_bytes, tampered) = agent.resume(&task_id);
    assert_eq!(tampered["status"], "tampered");
    assert!(
        tampered["claims"]
            .as_array()
            .unwrap()
            .iter()
            .all(|claim| claim["verdict"] == "tampered")
    );
    assert!(strings(&tampered["gaps"]).contains(&"record_integrity"));
    assert_eq!(
        tampered["damaged_records"],
        serde_json::json!([
            { "record_type": "evidence", "record_id": evidence_1, "reason": "object_digest_mismatch" },
        ])
    );
    // C1 does not recompute, so only C2's statements are listed, and the
    // continuation names the record.
    assert_eq!(tampered["claims"].as_array().unwrap().len(), 3);
    assert_eq!(tampered["continuation"][1]["template"], "damaged_record");
    assert_eq!(agent.resume(&task_id).0, tampered_bytes);
    println!("--text after tampering:\n{}", agent.resume_text(&task_id));
    daemon.stop();
}

fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("MEMPROVE_") {
            command.env_remove(name);
        }
    }
    command
}
