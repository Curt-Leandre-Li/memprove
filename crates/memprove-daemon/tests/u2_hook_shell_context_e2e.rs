//! U2 stage W-A (ADR 0014, "Amendment (U2)") through the real binaries:
//! the `SessionStart` hook, `memprove mcp serve`, and the shell of one
//! Claude Code session agree on its context. H1 (Owner O5): the shell's
//! `begin` takes the Project of this client session's last-Pack record
//! when it has no context of its own. H2: the hook replaces a damaged
//! last-Pack record of the client session it exports. H3: the hook writes
//! a client session's record only once it exported that client session;
//! otherwise an injected Pack is the directory's last Pack. H5: the
//! `no_project` sentence says whether this directory has a context.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;

use std::{
    fs,
    io::Write as _,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Stdio,
};

use p4b_support::{Cli, Run, Setup, cli_binary, isolated, step, text};
use p4g_support::{Mcp, McpEnv, files_under};
use serde_json::{Value, json};

/// The server as Claude Code spawns it in `setup`'s repository, with
/// `CLAUDE_CODE_SESSION_ID` set to `claude_code_session`.
fn server(setup: &Setup, claude_code_session: &str) -> Mcp {
    Mcp::start(&McpEnv {
        claude_code_session: Some(claude_code_session.to_owned()),
        ..McpEnv::of(
            &setup.agent.config_dir,
            &setup.agent.data_dir,
            &setup.agent.token,
            &setup.repo,
        )
    })
}

/// The agent's CLI in the shell, as the client session `client` when given.
fn as_client(cli: &Cli, client: Option<&str>, words: &[&str]) -> Run {
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
    if let Some(client) = client {
        command.env("MEMPROVE_CLIENT_SESSION", client);
    }
    let output = command.output().unwrap();
    let run = Run {
        code: output.status.code(),
        stdout: text(&output.stdout),
        stderr: text(&output.stderr),
    };
    println!(
        "[client {}] $ ct {}\n{}{}[exit {:?}]",
        client.unwrap_or("-"),
        words.join(" "),
        run.stdout,
        run.stderr,
        run.code
    );
    run
}

/// The file the hook of `client` exports its client session to, as Claude
/// Code names one per session.
fn env_file(setup: &Setup, client: &str) -> PathBuf {
    setup.root_path.join(format!("claude-env-{client}.sh"))
}

/// What one run of the hook printed.
struct Hooked {
    stdout: String,
    stderr: String,
}

/// `memprove hook session-start` for `client`, as Claude Code runs it at
/// `SessionStart`, with `CLAUDE_ENV_FILE` set to `env_file` when given and
/// the token read from `token_file`.
fn hook(setup: &Setup, client: &str, env_file: Option<&Path>, token_file: &Path) -> Hooked {
    let cli = &setup.agent;
    let input = json!({
        "session_id": client,
        "transcript_path": "/nowhere/transcript.jsonl",
        "cwd": setup.repo.to_str().unwrap(),
        "hook_event_name": "SessionStart",
        "source": "startup",
    })
    .to_string();
    let mut command = isolated(cli_binary());
    command
        .args(["hook", "session-start"])
        .current_dir(&setup.repo)
        .env("MEMPROVE_CONFIG_DIR", &cli.config_dir)
        .env("MEMPROVE_DATA_DIR", &cli.data_dir)
        .env("MEMPROVE_TOKEN_FILE", token_file)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("CLAUDE_ENV_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(env_file) = env_file {
        command.env("CLAUDE_ENV_FILE", env_file);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let hooked = Hooked {
        stdout: text(&output.stdout),
        stderr: text(&output.stderr),
    };
    println!(
        "$ memprove hook session-start ({client})\n{}{}",
        hooked.stdout, hooked.stderr
    );
    assert_eq!(output.status.code(), Some(0));
    hooked
}

/// The lowercase hexadecimal SHA-256 of `bytes`, as the context files are
/// named.
fn digest(bytes: &[u8]) -> String {
    memprove_domain::Sha256Digest::of_bytes(bytes).to_string()
}

/// The directory of the client sessions' files of the repository.
fn clients_dir(setup: &Setup) -> PathBuf {
    let root = std::fs::canonicalize(&setup.repo).unwrap();
    setup
        .agent
        .config_dir
        .join("contexts")
        .join(digest(root.as_os_str().as_encoded_bytes()))
}

/// The last-Pack file of the client session `client` at the repository.
fn client_record(setup: &Setup, client: &str) -> PathBuf {
    clients_dir(setup).join(format!("{}.pack.json", digest(client.as_bytes())))
}

/// The directory's own files (context and last-Pack) under `config_dir`,
/// with their bytes.
fn directory_files(config_dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let contexts = config_dir.join("contexts");
    files_under(&contexts)
        .into_iter()
        .filter(|(path, _)| path.parent() == Some(contexts.as_path()))
        .collect()
}

/// The directory's context names a completed Task and no Task is in
/// progress, so the hook injects no Pack. Returns that Task.
fn finished_task(setup: &Setup, title: &str) -> String {
    let agent = &setup.agent;
    let begun = agent.ok(&["begin", "--project", &setup.project_id, "--new-task", title]);
    agent.test("tests/a.sh");
    let checkpoint = agent.run(
        &[
            "checkpoint",
            "create",
            "--fact",
            "a passes",
            "--next",
            "done",
        ],
        true,
    );
    assert_eq!(checkpoint.code, Some(0), "{}", checkpoint.stderr);
    agent.bytes(&["task", "complete"]);
    agent.bytes(&["end", "--reason", "done"]);
    begun["task_id"].as_str().unwrap().to_owned()
}

/// H1 (Owner O5, N-11): in the hook's layout a client session whose only
/// record is the hook's record of no Pack `begin`s a new Task in the shell
/// without `--project`: the Project comes from that record. The server of
/// the same client session then reads the Session from the same file; the
/// directory's files are unchanged. An explicit `--project` still comes
/// first, and a client session with no record still needs `--project`.
#[test]
fn u2_h1_the_shell_begins_in_the_project_of_a_record_of_no_pack() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let config = &agent.config_dir;
    let finished = finished_task(&setup, "u2 h1 done");
    let client = "cc-u2-h1";

    step("the hook injects no Pack and records the client session with the Project");
    let hooked = hook(
        &setup,
        client,
        Some(&env_file(&setup, client)),
        &agent.token,
    );
    assert!(!hooked.stdout.contains("```text"), "{}", hooked.stdout);
    assert_eq!(
        std::fs::read_to_string(client_record(&setup, client)).unwrap(),
        format!(
            "{{\"no_pack\":true,\"project_id\":\"{}\"}}\n",
            setup.project_id
        )
    );
    let directory_before = directory_files(config);

    step("the shell, as that client session, begins a new Task without --project");
    let begun = as_client(agent, Some(client), &["begin", "--new-task", "u2 h1 next"]);
    assert_eq!(begun.code, Some(0), "{}", begun.stderr);
    let begun: Value = serde_json::from_str(begun.stdout.trim()).unwrap();
    assert_eq!(begun["project_id"], setup.project_id.as_str());
    assert_ne!(begun["task_id"], finished.as_str());
    let session = begun["session_id"].as_str().unwrap().to_owned();
    assert_eq!(directory_files(config), directory_before);

    step("the server of the same client session reads that Session");
    let mut mcp = server(&setup, client);
    let shown = mcp.ok("context", &json!({}));
    assert_eq!(shown.structured["context_file"], "client", "{}", shown.line);
    assert_eq!(shown.structured["lookup"], "found");
    assert_eq!(shown.structured["session_id"], session.as_str());
    mcp.finish();
    let ended = as_client(agent, Some(client), &["end", "--reason", "h1"]);
    assert_eq!(ended.code, Some(0), "{}", ended.stderr);

    step("an explicit --project comes before the record");
    let other = "cc-u2-h1-explicit";
    hook(&setup, other, Some(&env_file(&setup, other)), &agent.token);
    let unknown = "01900000-0000-7000-8000-000000000001";
    let refused = as_client(
        agent,
        Some(other),
        &["begin", "--project", unknown, "--new-task", "u2 h1 other"],
    );
    assert_ne!(refused.code, Some(0), "{}", refused.stdout);
    assert!(
        !refused.stderr.contains("requires --project"),
        "{}",
        refused.stderr
    );
    assert!(
        !clients_dir(&setup)
            .join(format!("{}.json", digest(other.as_bytes())))
            .exists()
    );

    step("a client session with no record still needs --project");
    let bare = as_client(
        agent,
        Some("cc-u2-h1-none"),
        &["begin", "--new-task", "u2 h1 none"],
    );
    assert_eq!(bare.code, Some(2), "{}", bare.stdout);
    assert!(
        bare.stderr
            .contains("begin requires --project <PROJECT_ID> the first time in a Workspace"),
        "{}",
        bare.stderr
    );
    assert_eq!(directory_files(config), directory_before);
}

/// The fixed line the hook writes when it replaces a damaged record.
const REPLACED: &str = "memprove: warning: the last-Pack record of this client session could \
     not be read or is malformed; the record of this SessionStart replaces it";

/// Writes `client`'s last-Pack file as `damage` leaves it.
fn damage(setup: &Setup, client: &str, damage: &str) {
    let record = client_record(setup, client);
    let directory = record.parent().unwrap();
    fs::create_dir_all(directory).unwrap();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    let write = |text: &str, mode: u32| {
        fs::write(&record, text).unwrap();
        fs::set_permissions(&record, fs::Permissions::from_mode(mode)).unwrap();
    };
    let valid = format!(
        "{{\"no_pack\":true,\"project_id\":\"{}\"}}\n",
        setup.project_id
    );
    match damage {
        "not json" => write("{not json", 0o600),
        "no_pack false" => write("{\"no_pack\":false}\n", 0o600),
        "mode 0644" => write(&valid, 0o644),
        "symbolic link" => {
            let target = setup.root_path.join(format!("elsewhere-{client}.json"));
            fs::write(&target, &valid).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            symlink(&target, &record).unwrap();
        }
        other => panic!("unknown damage {other}"),
    }
}

/// The record is a regular file of mode 0600 holding `expected`.
fn assert_record(setup: &Setup, client: &str, expected: &str) {
    let record = client_record(setup, client);
    let metadata = fs::symlink_metadata(&record).unwrap();
    assert!(metadata.is_file(), "{}", record.display());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::read_to_string(&record).unwrap(), expected);
}

/// H2 (N-12, original #149): at `SessionStart` the hook replaces a damaged
/// last-Pack record of the client session it exported (not JSON, a
/// `no_pack` that is not `true`, a group- or world-readable file, a
/// symbolic link) with the record of this run, a record of no Pack or of
/// the Pack it injects, and writes one fixed line on standard error; the
/// server of that client session then `begin`s in the record's Project. A
/// readable record is kept, with no line.
#[test]
fn u2_h2_the_hook_replaces_a_damaged_record_of_the_client_session() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    finished_task(&setup, "u2 h2 done");
    let no_pack = format!(
        "{{\"no_pack\":true,\"project_id\":\"{}\"}}\n",
        setup.project_id
    );

    for (index, kind) in ["not json", "no_pack false", "mode 0644", "symbolic link"]
        .into_iter()
        .enumerate()
    {
        let client = format!("cc-u2-h2-{index}");
        step(&format!(
            "a damaged record ({kind}) is replaced by a record of no Pack"
        ));
        damage(&setup, &client, kind);
        let target = setup.root_path.join(format!("elsewhere-{client}.json"));
        let target_before = fs::read(&target).ok();
        let hooked = hook(
            &setup,
            &client,
            Some(&env_file(&setup, &client)),
            &agent.token,
        );
        assert!(!hooked.stdout.contains("```text"), "{}", hooked.stdout);
        assert_eq!(
            hooked.stderr.matches(REPLACED).count(),
            1,
            "{}",
            hooked.stderr
        );
        assert_record(&setup, &client, &no_pack);
        assert_eq!(
            fs::read(&target).ok(),
            target_before,
            "the link's target is untouched"
        );

        step("the next SessionStart keeps the readable record, with no line");
        let again = hook(
            &setup,
            &client,
            Some(&env_file(&setup, &client)),
            &agent.token,
        );
        assert!(!again.stderr.contains(REPLACED), "{}", again.stderr);
        assert_record(&setup, &client, &no_pack);
    }

    step("a readable record of no Pack without a Project is kept");
    let client = "cc-u2-h2-kept";
    let record = client_record(&setup, client);
    fs::write(&record, "{\"no_pack\":true}\n").unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    let hooked = hook(
        &setup,
        client,
        Some(&env_file(&setup, client)),
        &agent.token,
    );
    assert!(!hooked.stderr.contains(REPLACED), "{}", hooked.stderr);
    assert_record(&setup, client, "{\"no_pack\":true}\n");

    step("the server of a repaired client session begins in the record's Project");
    let mut mcp = server(&setup, "cc-u2-h2-0");
    let begun = mcp.ok("begin", &json!({"new_task": {"title": "u2 h2 next"}}));
    assert_eq!(begun.structured["context_file"], "client", "{}", begun.line);
    mcp.ok("end", &json!({}));
    mcp.finish();

    step("a damaged record is replaced by the Pack the hook injects, with one line");
    agent.ok(&["begin", "--new-task", "u2 h2 live"]);
    agent.bytes(&["end", "--reason", "operator"]);
    let client = "cc-u2-h2-pack";
    damage(&setup, client, "not json");
    let hooked = hook(
        &setup,
        client,
        Some(&env_file(&setup, client)),
        &agent.token,
    );
    assert!(
        hooked.stdout.contains("```text\nPack: "),
        "{}",
        hooked.stdout
    );
    assert_eq!(
        hooked.stderr.matches(REPLACED).count(),
        1,
        "{}",
        hooked.stderr
    );
    let recorded: Value =
        serde_json::from_str(&fs::read_to_string(client_record(&setup, client)).unwrap()).unwrap();
    assert!(recorded.get("pack_digest").is_some(), "{recorded}");
    assert_eq!(recorded["project_id"], setup.project_id.as_str());
}

/// How a cell of H3's matrix gives the hook `CLAUDE_ENV_FILE`.
#[derive(Clone, Copy, Debug)]
enum Export {
    /// A file the hook appends the `export` line to.
    Written,
    /// A directory: the `export` line cannot be written.
    Failed,
    /// No `CLAUDE_ENV_FILE`.
    Unset,
}

/// The client session the shell of `client` has after the hook: the value
/// of the last `export MEMPROVE_CLIENT_SESSION=` line of its
/// `CLAUDE_ENV_FILE`, as Claude Code's Bash tool sources it.
fn exported_client(env_file: &Path) -> Option<String> {
    let text = fs::read_to_string(env_file).ok()?;
    let line = text
        .lines()
        .rev()
        .find(|line| line.starts_with("export MEMPROVE_CLIENT_SESSION="))?;
    Some(
        line.trim_start_matches("export MEMPROVE_CLIENT_SESSION=")
            .trim_matches('\'')
            .to_owned(),
    )
}

/// What one cell of H3's matrix needs: the setup, the Task the
/// directory's context names, its last-Pack file, a path that is a
/// directory, and a token file that does not exist.
struct Matrix<'a> {
    setup: &'a Setup,
    task: String,
    directory_record: PathBuf,
    not_a_file: PathBuf,
    no_token: PathBuf,
}

impl Matrix<'_> {
    /// Runs the hook for a new client session of the cell and checks the
    /// files it wrote; returns the client session and the shell's
    /// `MEMPROVE_CLIENT_SESSION`.
    fn hook(&self, pack: bool, export: Export) -> (String, Option<String>) {
        let setup = self.setup;
        let client = format!("cc-u2-h3-{}-{export:?}", if pack { "pack" } else { "none" });
        step(&format!("the hook: Pack {pack}, export {export:?}"));
        let _ = fs::remove_file(&self.directory_record);
        let env = match export {
            Export::Written => Some(env_file(setup, &client)),
            Export::Failed => Some(self.not_a_file.clone()),
            Export::Unset => None,
        };
        let token = if pack {
            setup.agent.token.clone()
        } else {
            self.no_token.clone()
        };
        let hooked = hook(setup, &client, env.as_deref(), &token);
        assert_eq!(
            hooked.stdout.contains("```text\nPack: "),
            pack,
            "{}",
            hooked.stdout
        );
        let shell = env.as_deref().and_then(exported_client);
        let exported = matches!(export, Export::Written);
        assert_eq!(shell.as_deref(), exported.then_some(client.as_str()));

        step("the hook's files: the client session's only after an export");
        let client_files = files_under(&clients_dir(setup))
            .into_iter()
            .map(|(path, _)| path)
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&digest(client.as_bytes()))
            })
            .collect::<Vec<_>>();
        if exported {
            assert_eq!(client_files, vec![client_record(setup, &client)]);
        } else {
            assert_eq!(client_files, Vec::<PathBuf>::new());
        }
        let directory_pack = fs::read_to_string(&self.directory_record).ok();
        if pack && !exported {
            let recorded: Value = serde_json::from_str(directory_pack.as_deref().unwrap()).unwrap();
            assert_eq!(recorded["task_id"], self.task.as_str());
            assert_eq!(recorded["project_id"], setup.project_id.as_str());
        } else {
            assert_eq!(directory_pack, None);
        }
        (client, shell)
    }

    /// The server of `client` and the shell with `shell` resolve the same
    /// context: the same Task before `begin`, the Session the server's
    /// `begin` starts, and its end.
    fn agree(&self, client: &str, shell: Option<&str>, pack: bool) {
        let agent = &self.setup.agent;
        let task = self.task.as_str();
        step("before begin: the server and the shell name the same Task");
        let mut mcp = server(self.setup, client);
        let shown = mcp.ok("context", &json!({}));
        let expected_file = if shell.is_some() {
            "client"
        } else {
            "directory"
        };
        assert_eq!(
            shown.structured["context_file"], expected_file,
            "{}",
            shown.line
        );
        let shell_context = as_client(agent, shell, &["context", "show"]);
        if shell_context.code == Some(0) {
            assert!(
                shell_context.stdout.contains(task),
                "{}",
                shell_context.stdout
            );
            assert_eq!(shown.structured["task_id"], task, "{}", shown.line);
        } else {
            assert_eq!(shown.structured["task_id"], Value::Null, "{}", shown.line);
        }

        step("the server's begin starts the Session the shell's context names");
        let begun = mcp.ok("begin", &json!({"task": 1}));
        assert_eq!(
            begun.structured["context_file"], expected_file,
            "{}",
            begun.line
        );
        assert_eq!(begun.structured["task_id"], task);
        assert_eq!(
            !begun.structured["resumed_from"].is_null(),
            pack,
            "{}",
            begun.line
        );
        let session = begun.structured["session_id"].as_str().unwrap().to_owned();
        let shell_context = as_client(agent, shell, &["context", "show"]);
        assert_eq!(shell_context.code, Some(0), "{}", shell_context.stderr);
        assert!(
            shell_context.stdout.contains(&session),
            "{}",
            shell_context.stdout
        );

        step("the shell's end ends it for the server too");
        let ended = as_client(agent, shell, &["end", "--reason", "h3"]);
        assert_eq!(ended.code, Some(0), "{}", ended.stderr);
        let shown = mcp.ok("context", &json!({}));
        assert_eq!(
            shown.structured["session_id"],
            Value::Null,
            "{}",
            shown.line
        );
        mcp.finish();
    }
}

/// H3 (N-14, original #151): the hook writes the record of a client
/// session only once it has exported that client session to the shell.
/// For every outcome of the hook, a Pack or none, by an export that was
/// written, failed, or had no `CLAUDE_ENV_FILE`, the server given the
/// client session as `CLAUDE_CODE_SESSION_ID` and the shell with what the
/// hook exported resolve the same context: the same Task before `begin`,
/// the Session the server's `begin` starts is the one the shell's
/// `context show` names, and the shell's `end` ends it for both. Without
/// an export, an injected Pack is recorded as the directory's last Pack and
/// no file of the client session is written; with one, as the client
/// session's.
#[test]
fn u2_h3_the_server_and_the_shell_resolve_one_context_for_every_hook_outcome() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u2 h3",
    ]);
    agent.bytes(&["end", "--reason", "operator"]);
    let root = fs::canonicalize(&setup.repo).unwrap();
    let matrix = Matrix {
        setup: &setup,
        task: begun["task_id"].as_str().unwrap().to_owned(),
        directory_record: agent.config_dir.join("contexts").join(format!(
            "{}.pack.json",
            digest(root.as_os_str().as_encoded_bytes())
        )),
        not_a_file: setup.root_path.join("env-is-a-directory"),
        no_token: setup.root_path.join("no-such-token"),
    };
    fs::create_dir_all(&matrix.not_a_file).unwrap();
    for pack in [true, false] {
        for export in [Export::Written, Export::Failed, Export::Unset] {
            let (client, shell) = matrix.hook(pack, export);
            matrix.agree(&client, shell.as_deref(), pack);
        }
    }
}

/// The `begin` command `no_project` gives (U1, A7), unchanged.
const BEGIN_FIRST: &str = "one memprove begin --project <PROJECT_ID> --new-task <TITLE> on the \
    command line comes first (to continue an existing Task, --task <TASK_ID> replaces --new-task \
    <TITLE>)";

/// H5 (N-2, original #128): `no_project` tells the two cases apart. With no
/// context in the Workspace at all, the sentence is the one before; when
/// the directory's context exists but this client session has neither a
/// context nor a record naming the Project, the sentence says so and where
/// the directory's Project ID is shown. Both end with the whole `begin`
/// command (A7).
#[test]
fn u2_h5_no_project_says_whether_the_directory_has_a_context() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let env = |client: Option<&str>| McpEnv {
        client: client.map(str::to_owned),
        ..McpEnv::of(
            &agent.config_dir,
            &agent.data_dir,
            &agent.token,
            &setup.repo,
        )
    };

    step("no context in the Workspace at all: the sentence as before");
    let mut mcp = Mcp::start(&env(Some("cc-u2-h5")));
    let none = mcp.call("begin", &json!({}));
    assert_eq!(none.failure(), Some("no_project"), "{}", none.line);
    assert_eq!(
        none.text,
        format!("this Workspace has no context and no Pack recorded a Project; {BEGIN_FIRST}")
    );
    mcp.finish();

    step("the directory's context exists, this client session has none");
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u2 h5",
    ]);
    agent.bytes(&["end", "--reason", "operator"]);
    let mut mcp = Mcp::start(&env(Some("cc-u2-h5")));
    let client = mcp.call("begin", &json!({}));
    assert_eq!(client.failure(), Some("no_project"), "{}", client.line);
    assert_eq!(
        client.text,
        format!(
            "this directory has a context, but this client session has none and no Pack recorded a \
             Project; memprove context show without MEMPROVE_CLIENT_SESSION shows the Project \
             ID this directory's context records; {BEGIN_FIRST}"
        )
    );
    assert_eq!(client.structured["failure"], "no_project");
    mcp.finish();

    step("a server with no client session uses the directory's context and begins");
    let mut mcp = Mcp::start(&env(None));
    let begun = mcp.ok("begin", &json!({}));
    assert_eq!(begun.structured["context_file"], "directory");
    mcp.ok("end", &json!({}));
    mcp.finish();
}

/// U3 K2 (V-N-1, original #63; ADR 0014, "Amendment (U3, X-K)"): a client
/// session that already has a record or a context from an earlier
/// `SessionStart` whose export worked keeps getting its Pack recorded in
/// its own file when a later `SessionStart` cannot export: the shell still
/// has the earlier export, and the server with `CLAUDE_CODE_SESSION_ID`
/// finds the client session's records, so both use the client session's
/// files. The directory's last Pack is not written.
#[test]
fn u3_k2_a_client_session_with_records_keeps_its_pack_when_the_export_fails() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u3 k2",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    agent.bytes(&["end", "--reason", "operator"]);
    let client = "cc-u3-k2";
    let root = fs::canonicalize(&setup.repo).unwrap();
    let directory_record = agent.config_dir.join("contexts").join(format!(
        "{}.pack.json",
        digest(root.as_os_str().as_encoded_bytes())
    ));

    step("the first SessionStart exports the client session and records its Pack");
    let first = hook(
        &setup,
        client,
        Some(&env_file(&setup, client)),
        &agent.token,
    );
    assert!(first.stdout.contains("```text\nPack: "), "{}", first.stdout);
    let shell = exported_client(&env_file(&setup, client));
    assert_eq!(shell.as_deref(), Some(client));
    assert!(client_record(&setup, client).is_file());

    step("the shell of that client session begins and ends: it has a context file");
    let begun = as_client(agent, shell.as_deref(), &["begin", "--task", &task]);
    assert_eq!(begun.code, Some(0), "{}", begun.stderr);
    let ended = as_client(agent, shell.as_deref(), &["end", "--reason", "k2"]);
    assert_eq!(ended.code, Some(0), "{}", ended.stderr);
    assert!(!client_record(&setup, client).exists(), "begin declared it");

    step("the next SessionStart cannot export: the Pack is still the client session's");
    let not_a_file = setup.root_path.join("env-is-a-directory");
    fs::create_dir_all(&not_a_file).unwrap();
    let second = hook(&setup, client, Some(&not_a_file), &agent.token);
    assert!(
        second.stdout.contains("```text\nPack: "),
        "{}",
        second.stdout
    );
    assert!(
        second.stderr.contains("was not written to CLAUDE_ENV_FILE"),
        "{}",
        second.stderr
    );
    let recorded: Value = serde_json::from_str(
        &fs::read_to_string(client_record(&setup, client)).unwrap_or_default(),
    )
    .unwrap_or(Value::Null);
    assert_eq!(recorded["task_id"], task.as_str(), "{recorded}");
    assert!(!directory_record.exists(), "the directory's last Pack");

    step("the server and the shell (with the earlier export) use the same files");
    let mut mcp = server(&setup, client);
    let shown = mcp.ok("context", &json!({}));
    assert_eq!(shown.structured["context_file"], "client", "{}", shown.line);
    let begun = mcp.ok("begin", &json!({"task": 1}));
    assert_eq!(begun.structured["context_file"], "client", "{}", begun.line);
    assert!(
        !begun.structured["resumed_from"].is_null(),
        "the Pack this SessionStart injected is declared: {}",
        begun.line
    );
    let session = begun.structured["session_id"].as_str().unwrap().to_owned();
    let shown = as_client(agent, shell.as_deref(), &["context", "show"]);
    assert_eq!(shown.code, Some(0), "{}", shown.stderr);
    assert!(shown.stdout.contains(&session), "{}", shown.stdout);
    let ended = as_client(agent, shell.as_deref(), &["end", "--reason", "k2"]);
    assert_eq!(ended.code, Some(0), "{}", ended.stderr);
    let shown = mcp.ok("context", &json!({}));
    assert_eq!(
        shown.structured["session_id"],
        Value::Null,
        "{}",
        shown.line
    );
    mcp.finish();
}

/// U3 K3 (V-N-2, original #64; ADR 0014, "Amendment (U3, X-K)"): when the
/// client session's last-Pack location is a directory, the hook neither
/// tries to replace it nor writes a line saying it could not: it is not a
/// damaged record (H2) it can replace, and an injected Pack is not recorded
/// there. Standard output is the statement and the Pack, standard error
/// is empty, the directory stays, and the next `SessionStart` is the same.
#[test]
fn u3_k3_a_record_that_is_a_directory_is_neither_replaced_nor_reported() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u3 k3",
    ]);
    agent.bytes(&["end", "--reason", "operator"]);
    let client = "cc-u3-k3";
    let record = client_record(&setup, client);
    fs::create_dir_all(&record).unwrap();
    for directory in [clients_dir(&setup), record.clone()] {
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for run in 1..=2 {
        step(&format!("SessionStart {run}: the record is a directory"));
        let hooked = hook(
            &setup,
            client,
            Some(&env_file(&setup, client)),
            &agent.token,
        );
        assert!(
            hooked.stdout.contains("```text\nPack: "),
            "{}",
            hooked.stdout
        );
        assert_eq!(hooked.stderr, "", "no REPLACED line and no failure line");
        assert!(fs::symlink_metadata(&record).unwrap().is_dir());
        assert_eq!(
            exported_client(&env_file(&setup, client)).as_deref(),
            Some(client)
        );
    }
}
