//! P2-B acceptance scenario (ADR 0012 D6): the real `memprove` CLI against
//! the real `memproved` over a temporary Git repository. The operator holds
//! the bootstrap control token; every agent step uses one token that carries
//! only `mcp-read` and `mcp-write`.
//!
//! The work happens in a linked worktree W1 of the registered repository, so
//! that W1 can later be deleted while the repository survives:
//!
//! a. Session at commit X, `memprove run` the tests (PASS), Checkpoint
//!    "tests pass" citing the Evidence, end the Session.
//! b. Repository unchanged: verified.
//! c. Change file Z and commit Y: stale, with X, Y, and Z.
//! d. Back at X, change the worktree only: stale, `worktree_changed`.
//! e. Ask from a linked worktree W2 at Y with `workspace_id`: compared with
//!    W2's HEAD.
//! f. Delete W1 and ask without `workspace_id`: partial.
//! g. New Session at Y in W2, rerun the tests, new Checkpoint: verified.
//! h. Daemon stopped, one Evidence row changed with `sqlite3`: tampered.
//! i. Two calls in a row: identical bytes and digest; the Event count does
//!    not change.
//! j. `SIGKILL` and restart: the same Preview.
//!
//! Run with `--nocapture` to print the transcript; token secrets are never
//! printed. `memprove` is taken from the same target directory as
//! `memproved`, so run this through `cargo test --workspace`.

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

    fn at(&self, cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
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

    fn summary(&self, task_id: &str, workspace_id: Option<&str>) -> String {
        let mut words = vec!["recover", "preview", "--task", task_id, "--text"];
        if let Some(workspace_id) = workspace_id {
            words.extend(["--workspace", workspace_id]);
        }
        let run = self.run(&words, false);
        assert_eq!(run.code, Some(0), "{}{}", run.stdout, run.stderr);
        run.stdout.trim().to_owned()
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

    fn kill(self) {
        self.signal("KILL", 9);
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

/// One agent Session continuing `task_id` in `workspace_id`: run the tests,
/// record a Checkpoint "tests pass" citing them, end the Session. Returns the
/// Evidence, Checkpoint, and Snapshot HEAD.
fn checkpointed_run(
    agent: &Cli,
    requests: &Path,
    workspace_id: &str,
    task_id: &str,
    name: &str,
) -> (String, String, String) {
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
    let request = requests.join(format!("{name}.json"));
    let body = format!(
        r#"{{"schema_version":1,"workspace_id":"{workspace_id}","task_id":"{task_id}",
            "session_id":"{session_id}","snapshot_id":"{snapshot_id}",
            "objective":"Make the fixture tests pass",
            "verified_completed_work":[{{"classification":"verified_fact",
                "text":"tests pass","evidence_ids":["{evidence_id}"]}}],
            "open_items":[],
            "next_action":{{"classification":"recommendation",
                "text":"rerun tests/run.sh after the next change"}}}}"#
    );
    println!("$ cat > {} <<'EOF'\n{body}\nEOF", request.display());
    fs::write(&request, body).unwrap();
    let checkpoint = agent.ok(&["checkpoint", "create", request.to_str().unwrap()]);
    let checkpoint_id = str_of(&checkpoint, "checkpoint_id").to_owned();
    agent.ok(&[
        "session",
        "end",
        &session_id,
        "--revision",
        "1",
        "--reason",
        "tests pass; checkpoint recorded",
    ]);
    (evidence_id, checkpoint_id, head)
}

#[test]
#[allow(clippy::too_many_lines)] // One acceptance scenario, a through j, in order.
fn recovery_preview_acceptance_scenario_through_real_binaries() {
    let root = tempfile::tempdir().unwrap();
    let root_path = fs::canonicalize(root.path()).unwrap();
    let repo = root_path.join("repo");
    let w1 = root_path.join("w1");
    let w2 = root_path.join("w2");
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
    let repo_arg = repo.to_str().unwrap();
    let project_id = str_of(
        &operator.ok(&["project", "register", repo_arg]),
        "project_id",
    )
    .to_owned();
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
    let agent = operator.with(&agent_token, &w1, "agent/mcp-read+mcp-write");

    // The agent works in the linked worktree W1 on branch `work`.
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "work",
            w1.to_str().unwrap(),
        ],
    );
    let w1_id = str_of(
        &agent.ok(&[
            "workspace",
            "resolve",
            "--project",
            &project_id,
            w1.to_str().unwrap(),
        ]),
        "workspace_id",
    )
    .to_owned();
    let task_id = str_of(
        &agent.ok(&[
            "task",
            "create",
            "--project",
            &project_id,
            "--title",
            "Make the fixture tests pass",
        ]),
        "task_id",
    )
    .to_owned();

    step("a. Session at X, memprove run the tests (PASS), Checkpoint, end Session");
    let x = git(&w1, &["rev-parse", "HEAD"]).trim().to_owned();
    let (evidence_x, checkpoint_x, anchored) =
        checkpointed_run(&agent, &requests, &w1_id, &task_id, "checkpoint-x");
    assert_eq!(anchored, x);

    step("b. repository unchanged: verified");
    let (_, preview_b) = agent.preview(&task_id, None);
    assert_eq!(preview_b["status"], "verified");
    assert_eq!(
        preview_b["bindings"]["checkpoint"]["checkpoint_id"],
        checkpoint_x.as_str()
    );
    assert_eq!(preview_b["bindings"]["snapshot"]["head_oid"], x.as_str());
    assert_eq!(preview_b["bindings"]["probe"]["head_oid"], x.as_str());
    assert_eq!(
        preview_b["bindings"]["probe"]["workspace_id"],
        w1_id.as_str()
    );
    assert_eq!(preview_b["facts"][0]["text"], "tests pass");
    assert_eq!(
        preview_b["facts"][0]["evidence"][0]["evidence_id"],
        evidence_x.as_str()
    );
    assert_eq!(preview_b["facts"][0]["verdict"], "verified");
    assert_eq!(strings(&preview_b["changed_paths"]), Vec::<&str>::new());
    println!("--text: {}", agent.summary(&task_id, None));

    step("c. change README (Z) and commit Y: stale with X, Y, Z");
    write(&w1.join("README"), "fixture v2\n");
    let y = commit(&w1, "change README");
    let (c_bytes, preview_c) = agent.preview(&task_id, None);
    assert_eq!(preview_c["status"], "stale");
    assert_eq!(preview_c["bindings"]["snapshot"]["head_oid"], x.as_str());
    assert_eq!(preview_c["bindings"]["probe"]["head_oid"], y.as_str());
    assert_eq!(strings(&preview_c["changed_paths"]), ["repo://README"]);
    assert!(strings(&preview_c["reasons"]).contains(&"head_changed"));
    assert!(strings(&preview_c["reasons"]).contains(&"evidence_stale"));
    assert_eq!(preview_c["facts"][0]["verdict"], "stale");
    assert_eq!(preview_c["next_checks"][0]["at_head"], y.as_str());
    assert_eq!(
        strings(&preview_c["next_checks"][0]["argv"]),
        ["sh", "tests/run.sh"]
    );
    let c_text = agent.summary(&task_id, None);
    assert_eq!(
        c_text,
        format!(
            "stale: evidence bound to commit {x}, current HEAD {y}, changed files: 「README」 \
             (reasons: head_changed, index_changed, evidence_stale)"
        )
    );
    println!("C_PREVIEW_JSON={}", c_bytes.trim());
    println!("C_PREVIEW_TEXT={c_text}");

    step("d. back at X (Y kept on branch y), change the worktree only: stale, worktree_changed");
    git(&w1, &["branch", "y"]);
    git(&w1, &["reset", "--quiet", "--hard", &x]);
    let (_, back) = agent.preview(&task_id, None);
    assert_eq!(back["status"], "verified");
    write(&w1.join("README"), "fixture, edited but not committed\n");
    let (_, preview_d) = agent.preview(&task_id, None);
    assert_eq!(preview_d["status"], "stale");
    assert_eq!(
        strings(&preview_d["reasons"]),
        ["worktree_changed", "evidence_stale"]
    );
    assert_eq!(preview_d["bindings"]["probe"]["head_oid"], x.as_str());
    assert_eq!(preview_d["bindings"]["probe"]["is_dirty"], true);
    assert_eq!(strings(&preview_d["changed_paths"]), ["repo://README"]);
    println!("--text: {}", agent.summary(&task_id, None));
    git(&w1, &["checkout", "--quiet", "--", "README"]);

    step("e. ask from linked worktree W2 (branch y at Y) with workspace_id");
    git(
        &repo,
        &["worktree", "add", "--quiet", w2.to_str().unwrap(), "y"],
    );
    let w2_id = str_of(
        &agent.ok(&[
            "workspace",
            "resolve",
            "--project",
            &project_id,
            w2.to_str().unwrap(),
        ]),
        "workspace_id",
    )
    .to_owned();
    let (_, preview_e) = agent.preview(&task_id, Some(&w2_id));
    assert_eq!(preview_e["status"], "stale");
    assert_eq!(
        preview_e["bindings"]["probe"]["workspace_id"],
        w2_id.as_str()
    );
    assert_eq!(preview_e["bindings"]["probe"]["head_oid"], y.as_str());
    assert_eq!(preview_e["bindings"]["probe"]["branch"], "y");
    assert_eq!(preview_e["bindings"]["snapshot"]["head_oid"], x.as_str());
    assert_eq!(strings(&preview_e["changed_paths"]), ["repo://README"]);
    assert_eq!(preview_e["next_checks"][0]["workspace_id"], w2_id.as_str());
    println!("--text: {}", agent.summary(&task_id, Some(&w2_id)));
    let (_, own) = agent.preview(&task_id, None);
    assert_eq!(own["status"], "verified", "W1 is still clean at X");

    step("f. delete W1 and ask without workspace_id: partial");
    println!("$ rm -rf {}", w1.display());
    fs::remove_dir_all(&w1).unwrap();
    let (_, preview_f) = agent.at(&repo).preview(&task_id, None);
    assert_eq!(preview_f["status"], "partial");
    assert_eq!(strings(&preview_f["reasons"]), ["evidence_unavailable"]);
    assert_eq!(
        preview_f["bindings"]["probe"]["workspace_id"],
        w1_id.as_str()
    );
    assert_eq!(preview_f["bindings"]["probe"]["available"], false);
    assert!(strings(&preview_f["gaps"]).contains(&"workspace_unavailable"));
    assert_eq!(preview_f["next_checks"][0]["at_head"], Value::Null);
    println!("--text: {}", agent.at(&repo).summary(&task_id, None));

    step("g. new Session at Y in W2, rerun the tests, new Checkpoint: verified");
    let agent_w2 = agent.at(&w2);
    let (evidence_y, checkpoint_y, anchored) =
        checkpointed_run(&agent_w2, &requests, &w2_id, &task_id, "checkpoint-y");
    assert_eq!(anchored, y);
    let (g_bytes, preview_g) = agent_w2.preview(&task_id, None);
    assert_eq!(preview_g["status"], "verified");
    assert_eq!(
        preview_g["bindings"]["checkpoint"]["checkpoint_id"],
        checkpoint_y.as_str()
    );
    assert_eq!(preview_g["bindings"]["snapshot"]["head_oid"], y.as_str());
    assert_eq!(
        preview_g["bindings"]["probe"]["workspace_id"],
        w2_id.as_str()
    );
    assert_eq!(
        preview_g["facts"][0]["evidence"][0]["evidence_id"],
        evidence_y.as_str()
    );
    assert_eq!(preview_g["sessions"].as_array().unwrap().len(), 2);
    println!("--text: {}", agent_w2.summary(&task_id, None));

    step("h. daemon stopped, one Evidence row changed with sqlite3: tampered");
    daemon.stop();
    let database = operator
        .data_dir
        .join("projects")
        .join(&project_id)
        .join("state.sqlite");
    let trigger = sqlite(
        &database,
        "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'evidence_reject_update';",
    );
    let tamper = format!(
        "DROP TRIGGER evidence_reject_update; \
         UPDATE evidence SET payload = replace(payload, '3 passed', '9 passed') \
         WHERE evidence_id = '{evidence_y}'; SELECT changes();"
    );
    assert_eq!(sqlite(&database, &tamper).trim(), "1");
    let daemon = Daemon::start(&operator, &root_path.join("daemon-2.log"));
    let (tampered_bytes, preview_h) = agent_w2.preview(&task_id, None);
    assert_eq!(preview_h["status"], "tampered");
    assert_eq!(strings(&preview_h["reasons"]), ["object_digest_mismatch"]);
    assert_eq!(
        strings(&preview_h["invalid_evidence_ids"]),
        [evidence_y.as_str()]
    );
    assert!(strings(&preview_h["gaps"]).contains(&"record_integrity"));
    assert_eq!(preview_h["bindings"]["checkpoint"]["intact"], false);
    assert_eq!(agent_w2.preview(&task_id, None).0, tampered_bytes);
    println!("--text: {}", agent_w2.summary(&task_id, None));

    step("h'. daemon stopped, the row and trigger restored: the g Preview again");
    daemon.stop();
    let restore = format!(
        "UPDATE evidence SET payload = replace(payload, '9 passed', '3 passed') \
         WHERE evidence_id = '{evidence_y}'; {}; SELECT changes();",
        trigger.trim().trim_end_matches(';')
    );
    sqlite(&database, &restore);
    let daemon = Daemon::start(&operator, &root_path.join("daemon-3.log"));
    let (restored, _) = agent_w2.preview(&task_id, None);
    assert_eq!(restored, g_bytes, "same N and probe as in g");

    step("i. two calls in a row: identical bytes and digest; Event count unchanged");
    let before = event_count(&operator, &project_id);
    let (first, first_doc) = agent_w2.preview(&task_id, None);
    let (second, second_doc) = agent_w2.preview(&task_id, None);
    let after = event_count(&operator, &project_id);
    assert_eq!(first, second);
    assert_eq!(first_doc["preview_digest"], second_doc["preview_digest"]);
    assert_eq!(before, after);
    assert_eq!(
        first_doc["bindings"]["ledger"]["project_sequence"].as_u64(),
        Some(u64::try_from(before).unwrap())
    );
    println!(
        "identical: {} bytes, preview_digest {}; events before {before}, after {after}",
        first.trim().len(),
        first_doc["preview_digest"]
    );

    step("j. SIGKILL and restart: the same Preview");
    daemon.kill();
    let daemon = Daemon::start(&operator, &root_path.join("daemon-4.log"));
    let (rebooted, _) = agent_w2.preview(&task_id, None);
    assert_eq!(rebooted, first);
    assert_eq!(event_count(&operator, &project_id), before);
    println!("after SIGKILL and restart: identical bytes");
    daemon.stop();
}

/// A command for a binary under test with no inherited `MEMPROVE_*`
/// variable, so the test does not depend on the caller's environment
/// (P2-H); the test sets the ones it needs afterwards.
fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("MEMPROVE_") {
            command.env_remove(name);
        }
    }
    command
}
