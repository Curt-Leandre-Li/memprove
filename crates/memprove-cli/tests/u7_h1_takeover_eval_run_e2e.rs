//! U7 H1 (Owner O8; ADR 0018, `docs/takeover-eval.md`):
//! `scripts/takeover-eval/run.sh`, the takeover evaluation's runner.
//!
//! Its safety checks run on every `cargo test`: they need only the shell,
//! with a temporary `HOME`, so the "real installations" they guard are
//! directories of that temporary home. A variable that names one, binaries
//! or a results directory inside one, or a results directory inside the
//! repository is refused with exit 2 before anything is written; the
//! inherited `MEMPROVE_*` variables and `PATH` entries inside one are
//! dropped, not refused.
//!
//! The ignored tests run the script for real (they need
//! `/usr/local/bin/python3.12` with pytest, `sqlite3`, and the debug
//! `memproved` next to `memprove`; run them with `-- --ignored`):
//!
//! - `u7_h1_dry_run_prepares_and_checks_the_four_scenarios`: the dry run
//!   builds and seeds A to D and checks C's tampering and D's omission; the
//!   archive says what each seed is.
//! - `u13_e1_the_dry_run_seeds_e_with_the_clients_parent_left_open` (U13
//!   E1): the dry run of A to E; E's seed leaves the Session of the agent
//!   `takeover-eval-parent` open and names it in `seed.json`, and the seeds
//!   of A to D keep their members.
//! - `u13_f1_the_dry_run_seeds_f_with_a_dispatcher_that_ended` (U13 F1): the
//!   dry run of A to F; F's seed leaves open a Session of the agent
//!   `takeover-eval-left` dispatched by a Session of `takeover-eval-dispatcher`
//!   that ended normally, names both in `seed.json`, and its Pack shows the
//!   dispatcher ended; the seeds of A to E keep their members.
//! - `u7_h1_each_run_gets_a_new_environment_and_a_clean_client`: two runs
//!   of A with a stand-in `claude` that records how it was started and runs
//!   the hook as Claude Code would; each run has its own environment of
//!   the same seed, the client gets the standard prompt in the temporary
//!   repository with no `MEMPROVE_*`, `ANTHROPIC_*`, or `CLAUDE*`
//!   variable, the real `HOME`, and the temporary bin directory first on
//!   `PATH`, and every run's archive is complete. Afterwards no temporary
//!   directory and no daemon of the script is left.
//! - `u9_k6_the_b_client_declares_an_agent_and_the_others_declare_none`: A,
//!   B, (U13 E1) E, and (U13 F1) F with the stand-in; only B's, E's, and F's
//!   clients get the agent variables, E's with its parent; B's and E's wait
//!   for their seed, F's does not.

#[path = "u1e_support/mod.rs"]
mod support;

use memprove_domain::says_verified;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use support::{cli_binary, text, workspace_root};

/// The standard take-over prompt (O8): D2's prompt and the sentence for
/// the last line.
const PROMPT: &str = "Continue the Task memprove records for this repository: read the injected Context Pack first, query it with the memprove MCP tool `pack` and a `query` at least twice (once for the open items, once for a specific file or test), then do the next step, run the tests with `memprove run`, record a fact that cites that run, and end the Session; read no chat history. The last line of the answer is only `Facts: #<number>[, #<number>…]`, with numbers the ledger has.";

/// The Python with pytest the temporary project's tests run with.
const PYTHON: &str = "/usr/local/bin/python3.12";

/// U10 T2: the prefix of the temporary directories of the two ignored
/// tests, `TAKEOVER_EVAL_TMP_PREFIX` of the test's own environment when set
/// (a development run that must not touch `/private/tmp/u7-h.*`), else the
/// script's default.
fn tmp_prefix() -> String {
    std::env::var("TAKEOVER_EVAL_TMP_PREFIX").unwrap_or_else(|_| "/private/tmp/u7-h".to_owned())
}

fn script() -> PathBuf {
    workspace_root().join("scripts/takeover-eval/run.sh")
}

/// `run.sh` with only `HOME`, a minimal `PATH`, and `variables`.
fn run_script(home: &Path, variables: &[(&str, &str)]) -> Output {
    let mut command = Command::new("/bin/bash");
    command
        .arg(script())
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .current_dir(home);
    for (name, value) in variables {
        command.env(name, value);
    }
    let output = command.output().unwrap();
    println!(
        "$ run.sh {variables:?}\n{}{}[exit {:?}]",
        text(&output.stdout),
        text(&output.stderr),
        output.status.code()
    );
    output
}

/// A temporary home whose `~/.memprove` and `~/.config/memprove` stand for
/// the real installations, canonical; U22: `~/trial`, protected through
/// `TAKEOVER_EVAL_PROTECT`, stands for a second one.
fn home() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

fn assert_refused(output: &Output, words: &str) {
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("takeover-eval: refused: "), "{stderr}");
    assert!(stderr.contains(words), "{stderr}");
}

#[test]
fn u7_h1_a_variable_naming_a_real_installation_is_refused_before_anything() {
    let (_guard, home) = home();
    let out = home.join("results");
    let trial = home.join("trial");
    for (name, value) in [
        ("TRIAL_CONFIG", trial.join("config")),
        ("SOME_DATA", home.join(".memprove/data")),
        ("XDG_HINT", home.join(".config/memprove")),
    ] {
        let output = run_script(
            &home,
            &[
                (name, value.to_str().unwrap()),
                ("TAKEOVER_EVAL_PROTECT", trial.to_str().unwrap()),
                ("TAKEOVER_EVAL_OUT", out.to_str().unwrap()),
                ("TAKEOVER_EVAL_DRY_RUN", "1"),
            ],
        );
        assert_refused(&output, &format!("the variable {name} points into"));
        assert!(!out.exists(), "the results directory was created");
    }
}

#[test]
fn u7_h1_inherited_memprove_variables_and_path_entries_are_dropped() {
    let (_guard, home) = home();
    let trial = home.join("trial");
    let trial_bin = trial.join("bin");
    // Each would be refused if it were kept; the scenario error that
    // follows shows they were dropped first.
    let output = run_script(
        &home,
        &[
            (
                "MEMPROVE_CONFIG_DIR",
                trial.join("config").to_str().unwrap(),
            ),
            ("TAKEOVER_EVAL_PROTECT", trial.to_str().unwrap()),
            ("MEMPROVE_CLIENT_SESSION", "desktop-app-session"),
            (
                "PATH",
                &format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", trial_bin.display()),
            ),
            // U13 E1: E is a scenario since U13, so the unknown one is Z.
            ("TAKEOVER_EVAL_SCENARIOS", "A,Z"),
        ],
    );
    assert_refused(&output, "unknown scenario in TAKEOVER_EVAL_SCENARIOS");
}

/// U13 E1 (Owner O3, O6): E is one of the scenarios `TAKEOVER_EVAL_SCENARIOS`
/// may name, and the refusal of an unknown one lists it. A dry run of E alone
/// passes the scenario check and stops at the next one, the binaries
/// directory, which does not exist here.
#[test]
fn u13_e1_scenario_e_is_known_and_the_refusal_lists_it() {
    let (_guard, home) = home();
    let missing = home.join("no-binaries-here");
    let output = run_script(
        &home,
        &[
            ("TAKEOVER_EVAL_DRY_RUN", "1"),
            ("TAKEOVER_EVAL_SCENARIOS", "E"),
            ("TAKEOVER_EVAL_BIN", missing.to_str().unwrap()),
        ],
    );
    assert_refused(&output, "no binaries directory");
    assert!(
        !text(&output.stderr).contains("unknown scenario"),
        "{}",
        text(&output.stderr)
    );
    let output = run_script(&home, &[("TAKEOVER_EVAL_SCENARIOS", "A,Z")]);
    // U13 F1: the list names F too.
    assert_refused(
        &output,
        "unknown scenario in TAKEOVER_EVAL_SCENARIOS (A, B, C, D, E, F)",
    );
}

/// U13 F1 (Owner O4, O6): F is one of the scenarios `TAKEOVER_EVAL_SCENARIOS`
/// may name, alone or with the others, and the refusal of an unknown one lists
/// it. A dry run of F passes the scenario check and stops at the binaries
/// directory, which does not exist here; the default is still `A,B,C,D`.
#[test]
fn u13_f1_scenario_f_is_known_and_the_refusal_lists_it() {
    let (_guard, home) = home();
    let missing = home.join("no-binaries-here");
    for scenarios in ["F", "A,B,C,D,E,F"] {
        let output = run_script(
            &home,
            &[
                ("TAKEOVER_EVAL_DRY_RUN", "1"),
                ("TAKEOVER_EVAL_SCENARIOS", scenarios),
                ("TAKEOVER_EVAL_BIN", missing.to_str().unwrap()),
            ],
        );
        assert_refused(&output, "no binaries directory");
        assert!(
            !text(&output.stderr).contains("unknown scenario"),
            "{scenarios}: {}",
            text(&output.stderr)
        );
    }
    let output = run_script(&home, &[("TAKEOVER_EVAL_SCENARIOS", "F,G")]);
    assert_refused(
        &output,
        "unknown scenario in TAKEOVER_EVAL_SCENARIOS (A, B, C, D, E, F)",
    );
    assert!(
        read(&script()).contains(r#"SCENARIOS="${TAKEOVER_EVAL_SCENARIOS:-A,B,C,D}""#),
        "the default scenarios changed"
    );
}

#[test]
fn u7_h1_binaries_results_or_client_inside_a_real_installation_are_refused() {
    let (_guard, home) = home();
    let installed = home.join(".memprove/bin");
    fs::create_dir_all(&installed).unwrap();
    for name in ["memprove", "memproved", "claude"] {
        let path = installed.join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let binaries = cli_binary().parent().unwrap().to_owned();
    let out = home.join("results");

    let output = run_script(
        &home,
        &[
            ("TAKEOVER_EVAL_BIN", installed.to_str().unwrap()),
            ("TAKEOVER_EVAL_OUT", out.to_str().unwrap()),
        ],
    );
    assert_refused(
        &output,
        "the binaries under test are inside a real installation",
    );

    let inside = home.join(".config/memprove/results");
    let output = run_script(
        &home,
        &[
            ("TAKEOVER_EVAL_BIN", binaries.to_str().unwrap()),
            ("TAKEOVER_EVAL_OUT", inside.to_str().unwrap()),
        ],
    );
    assert_refused(
        &output,
        "the results directory is inside a real installation",
    );
    assert!(!inside.exists(), "the results directory was created");

    let in_repository = workspace_root().join("target/u7-h1-results-must-not-exist");
    let output = run_script(
        &home,
        &[
            ("TAKEOVER_EVAL_BIN", binaries.to_str().unwrap()),
            ("TAKEOVER_EVAL_OUT", in_repository.to_str().unwrap()),
        ],
    );
    assert_refused(&output, "the results directory is inside the repository");
    assert!(!in_repository.exists(), "the results directory was created");

    let output = run_script(
        &home,
        &[
            ("TAKEOVER_EVAL_BIN", binaries.to_str().unwrap()),
            ("TAKEOVER_EVAL_OUT", out.to_str().unwrap()),
            (
                "TAKEOVER_EVAL_CLAUDE",
                installed.join("claude").to_str().unwrap(),
            ),
        ],
    );
    assert_refused(
        &output,
        "the claude executable is inside a real installation",
    );
    assert!(!out.exists(), "the results directory was created");
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn read_json(path: &Path) -> JsonValue {
    parse_strict(read(path).trim()).unwrap_or_else(|error| panic!("{}: {error:?}", path.display()))
}

fn member<'a>(value: &'a JsonValue, name: &str) -> &'a JsonValue {
    value
        .get(name)
        .unwrap_or_else(|| panic!("no member {name}"))
}

fn string_of<'a>(value: &'a JsonValue, name: &str) -> &'a str {
    member(value, name)
        .as_str()
        .unwrap_or_else(|| panic!("{name} is not a string"))
}

/// The temporary root a run's environment had, from its `seed.json`; it
/// must be gone, and no process may still run from it.
fn assert_root_gone(run: &Path) -> String {
    let seed = read_json(&run.join("seed.json"));
    let root = string_of(&seed, "root").to_owned();
    assert!(root.starts_with(&format!("{}.", tmp_prefix())), "{root}");
    assert!(!Path::new(&root).exists(), "{root} was not removed");
    let pgrep = Command::new("pgrep").args(["-f", &root]).output().unwrap();
    assert!(
        text(&pgrep.stdout).trim().is_empty(),
        "a process of {root} still runs"
    );
    root
}

/// The scenario's Pack before the client: its line that starts with
/// `prefix`, if any.
fn pack_line(run: &Path, prefix: &str) -> Option<String> {
    read(&run.join("pack-before.txt"))
        .lines()
        .find(|line| line.starts_with(prefix))
        .map(str::to_owned)
}

#[test]
#[ignore = "needs /usr/local/bin/python3.12 with pytest, sqlite3, and the debug memproved; see the module documentation"]
fn u7_h1_dry_run_prepares_and_checks_the_four_scenarios() {
    let (_guard, home) = home();
    let out = home.join("results");
    let binaries = cli_binary().parent().unwrap().to_owned();
    let output = run_script(
        &home,
        &[
            ("TAKEOVER_EVAL_DRY_RUN", "1"),
            ("TAKEOVER_EVAL_BIN", binaries.to_str().unwrap()),
            ("TAKEOVER_EVAL_OUT", out.to_str().unwrap()),
            ("TAKEOVER_EVAL_PYTHON", PYTHON),
            ("TAKEOVER_EVAL_TMP_PREFIX", &tmp_prefix()),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    for scenario in ["A", "B", "C", "D"] {
        let run = out.join(format!("dry-{scenario}"));
        let root = assert_root_gone(&run);
        // U10 T3 (Owner O4): the unattended rules, the whole command line by
        // name and by the client's path, in place of `memprove run` only.
        let settings = read_json(&run.join("settings.local.json"));
        let allow: Vec<&str> = member(member(&settings, "permissions"), "allow")
            .as_array()
            .unwrap()
            .iter()
            .map(|rule| rule.as_str().unwrap())
            .collect();
        let by_path = format!("Bash({root}/bin/memprove:*)");
        assert_eq!(
            allow[..3],
            ["mcp__memprove", "Bash(memprove:*)", by_path.as_str()],
            "{allow:?}"
        );
        assert!(
            !allow.iter().any(|rule| rule.ends_with(" run:*)")),
            "{allow:?}"
        );
        let verify = read_json(&run.join("ledger-verify-before.json"));
        let capsule = read_json(&run.join("capsule-before.json"));
        let status = string_of(&capsule, "status");
        if scenario == "C" {
            // One character of the fact changed behind the daemon's back.
            assert_eq!(member(&verify, "valid"), &JsonValue::Bool(false));
            assert_eq!(status, "tampered");
            assert!(!says_verified(&read(&run.join("pack-before.txt"))));
            continue;
        }
        assert_eq!(
            member(&verify, "valid"),
            &JsonValue::Bool(true),
            "{scenario}"
        );
        assert_eq!(status, "verified", "{scenario}");
        let last_passed = pack_line(&run, "Last passed: ").unwrap();
        assert!(
            last_passed.starts_with(&format!("Last passed: 「{PYTHON} -m pytest -q」")),
            "{last_passed}"
        );
        // U8 K5: the line ends with the arguments that record a pytest
        // test run.
        assert!(
            last_passed.ends_with("; --test --test-format pytest)"),
            "{last_passed}"
        );
        // U8 K6: the seeding Session declared its agent, and the Pack shows it.
        let pack = read(&run.join("pack-before.txt"));
        assert!(
            pack.lines().any(|line| line.starts_with("Session 1 ")
                && line.contains("; agent「takeover-eval-seed」(sub_agent)")),
            "{pack}"
        );
        let open: Vec<String> = read(&run.join("pack-before.txt"))
            .lines()
            .filter(|line| line.starts_with("Session ") && line.contains(" not ended ("))
            .map(str::to_owned)
            .collect();
        if scenario == "B" {
            // Another client session's Session is open, with the hint.
            assert_eq!(open.len(), 1, "{open:?}");
            assert!(open[0].contains("end it before taking over"), "{}", open[0]);
            // U10 T1 (Owner O3): the line with the hint names the agent of
            // that Session (U10 T2: `takeover-eval-left`, not the client's
            // name); U10 T7 (Owner O4): the hint is the command that
            // ends it, with its ID and number (the run's bin directory is on
            // the agent's PATH, so the program is `memprove`).
            let id = open[0].split(' ').nth(2).unwrap();
            assert!(
                open[0].ends_with(&format!(
                    "; agent「takeover-eval-left」(sub_agent); end it before taking over: memprove \
                     session end {id} --abandoned --reason <why> (MCP: end {{session:2, \
                     abandoned:true}})"
                )),
                "{}",
                open[0]
            );
        } else {
            assert!(open.is_empty(), "{open:?}");
        }
        let claims = member(&capsule, "claims").as_array().unwrap().len();
        let omission = pack_line(&run, "Omitted: level ").unwrap();
        if scenario == "D" {
            assert!(claims >= 70, "{claims}");
            assert!(!omission.starts_with("Omitted: level 0;"), "{omission}");
        } else {
            assert_eq!(claims, 3, "{scenario}");
            assert!(omission.starts_with("Omitted: level 0;"), "{omission}");
        }
    }
}

/// The members of a `seed.json` of scenarios A to D, as `run.sh` has written
/// them since U9 (the U12 baseline `8301416`'s archives have these and no
/// other); U13 E1 adds none to them.
const SEED_MEMBERS: [&str; 5] = [
    "ledger_verify_before_exit",
    "project_id",
    "root",
    "scenario",
    "task_id",
];

/// The member names of a JSON object, sorted.
fn member_names(value: &JsonValue) -> Vec<&str> {
    let mut names: Vec<&str> = value
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    names.sort_unstable();
    names
}

/// U13 E1 (Owner O3, O6): the dry run of A to E. E's seed is A's, then a
/// Session of the client session `takeover-eval-parent` (agent
/// `takeover-eval-parent`, role `orchestrator`) that begins the Task, runs
/// the tests, and is left open; its Session ID is `parent_session_id` of
/// `seed.json`, the parent `run.sh` gives E's client. The seeds of A to D
/// keep the members they had.
#[test]
#[ignore = "needs /usr/local/bin/python3.12 with pytest, sqlite3, and the debug memproved; see the module documentation"]
fn u13_e1_the_dry_run_seeds_e_with_the_clients_parent_left_open() {
    let (_guard, home) = home();
    let out = home.join("results");
    let binaries = cli_binary().parent().unwrap().to_owned();
    let output = run_script(
        &home,
        &[
            ("TAKEOVER_EVAL_DRY_RUN", "1"),
            ("TAKEOVER_EVAL_SCENARIOS", "A,B,C,D,E"),
            ("TAKEOVER_EVAL_BIN", binaries.to_str().unwrap()),
            ("TAKEOVER_EVAL_OUT", out.to_str().unwrap()),
            ("TAKEOVER_EVAL_PYTHON", PYTHON),
            ("TAKEOVER_EVAL_TMP_PREFIX", &tmp_prefix()),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    for scenario in ["A", "B", "C", "D"] {
        let run = out.join(format!("dry-{scenario}"));
        assert_root_gone(&run);
        let seed = read_json(&run.join("seed.json"));
        assert_eq!(member_names(&seed), SEED_MEMBERS, "{scenario}");
    }
    let run = out.join("dry-E");
    assert_root_gone(&run);
    let seed = read_json(&run.join("seed.json"));
    let mut expected = SEED_MEMBERS.to_vec();
    expected.push("parent_session_id");
    expected.sort_unstable();
    assert_eq!(member_names(&seed), expected);
    assert_eq!(string_of(&seed, "scenario"), "E");
    let parent = string_of(&seed, "parent_session_id");
    let verify = read_json(&run.join("ledger-verify-before.json"));
    assert_eq!(member(&verify, "valid"), &JsonValue::Bool(true));
    let capsule = read_json(&run.join("capsule-before.json"));
    assert_eq!(string_of(&capsule, "status"), "verified");
    let sessions = member(&capsule, "sessions").as_array().unwrap();
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    // The seeding Session ended; the parent's is open, an orchestrator with
    // no parent of its own.
    assert_eq!(string_of(&sessions[0], "end_state"), "ended");
    let agent = member(&sessions[0], "agent");
    assert_eq!(string_of(agent, "name"), "takeover-eval-seed");
    let open = &sessions[1];
    assert_eq!(string_of(open, "session_id"), parent);
    assert!(
        !["ended", "crashed"].contains(&string_of(open, "end_state")),
        "{open:?}"
    );
    let agent = member(open, "agent");
    assert_eq!(string_of(agent, "name"), "takeover-eval-parent");
    assert_eq!(string_of(agent, "role"), "orchestrator");
    assert_eq!(member(agent, "parent_session_id"), &JsonValue::Null);
    // The Pack shows it as open, named, with the takeover hint: what E's
    // client must leave alone because it is its parent (ADR 0021 (c)).
    let pack = read(&run.join("pack-before.txt"));
    let line = pack
        .lines()
        .find(|line| line.starts_with("Session 2 ") && line.contains(" not ended ("))
        .unwrap_or_else(|| panic!("{pack}"));
    assert!(
        line.starts_with(&format!("Session 2 {parent} not ended (")),
        "{line}"
    );
    assert!(
        line.ends_with(&format!(
            "; agent「takeover-eval-parent」(orchestrator); end it before taking over: memprove \
             session end {parent} --abandoned --reason <why> (MCP: end {{session:2, \
             abandoned:true}})"
        )),
        "{line}"
    );
    assert!(pack_line(&run, "Last passed: ").is_some(), "{pack}");
    assert_eq!(member(&capsule, "claims").as_array().unwrap().len(), 3);
    assert!(pack_line(&run, "Omitted: level 0;").is_some(), "{pack}");
}

/// U13 F1 (Owner O4, O6): the dry run of A to F. F's seed is A's, then a
/// Session of the client session `takeover-eval-dispatcher` (agent
/// `takeover-eval-dispatcher`, role `orchestrator`) begins the Task, a Session
/// of the client session `takeover-eval-left` (agent `takeover-eval-left`,
/// role `sub_agent`, dispatched by the first) begins it, runs the tests, and is
/// left open, and the dispatcher's Session ends normally; `seed.json` names
/// both (`dispatcher_session_id`, `left_session_id`). The Pack shows the left
/// Session's line with `dispatched by Session <j>` and the dispatcher's line as ended, the
/// reading of ADR 0021 "Amendment (U11, S-K)" under which (b) holds although
/// the left Session's last write is recent. The seeds of A to E keep their
/// members.
#[test]
#[ignore = "needs /usr/local/bin/python3.12 with pytest, sqlite3, and the debug memproved; see the module documentation"]
fn u13_f1_the_dry_run_seeds_f_with_a_dispatcher_that_ended() {
    let (_guard, home) = home();
    let out = home.join("results");
    let binaries = cli_binary().parent().unwrap().to_owned();
    let output = run_script(
        &home,
        &[
            ("TAKEOVER_EVAL_DRY_RUN", "1"),
            ("TAKEOVER_EVAL_SCENARIOS", "A,B,C,D,E,F"),
            ("TAKEOVER_EVAL_BIN", binaries.to_str().unwrap()),
            ("TAKEOVER_EVAL_OUT", out.to_str().unwrap()),
            ("TAKEOVER_EVAL_PYTHON", PYTHON),
            ("TAKEOVER_EVAL_TMP_PREFIX", &tmp_prefix()),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    // A to E keep the members they had (E's parent since U13 E1).
    for scenario in ["A", "B", "C", "D", "E"] {
        let run = out.join(format!("dry-{scenario}"));
        assert_root_gone(&run);
        let mut expected = SEED_MEMBERS.to_vec();
        if scenario == "E" {
            expected.push("parent_session_id");
            expected.sort_unstable();
        }
        let seed = read_json(&run.join("seed.json"));
        assert_eq!(member_names(&seed), expected, "{scenario}");
    }
    let run = out.join("dry-F");
    assert_root_gone(&run);
    let seed = read_json(&run.join("seed.json"));
    let mut expected = SEED_MEMBERS.to_vec();
    expected.extend(["dispatcher_session_id", "left_session_id"]);
    expected.sort_unstable();
    assert_eq!(member_names(&seed), expected);
    assert_eq!(string_of(&seed, "scenario"), "F");
    let dispatcher = string_of(&seed, "dispatcher_session_id");
    let left = string_of(&seed, "left_session_id");
    assert_ne!(dispatcher, left);
    let verify = read_json(&run.join("ledger-verify-before.json"));
    assert_eq!(member(&verify, "valid"), &JsonValue::Bool(true));
    let capsule = read_json(&run.join("capsule-before.json"));
    assert_eq!(string_of(&capsule, "status"), "verified");
    assert_f_sessions(&capsule, dispatcher, left);
    assert_f_pack(&run, dispatcher, left);
    assert_eq!(member(&capsule, "claims").as_array().unwrap().len(), 3);
    assert!(pack_line(&run, "Last passed: ").is_some());
    assert!(pack_line(&run, "Omitted: level 0;").is_some());
}

/// U13 F1: the Sessions of F's Capsule before the run. The seeding Session
/// ended; the dispatcher's ended normally (not abandoned), an orchestrator
/// with no parent; the left Session is open, a `sub_agent` the dispatcher
/// dispatched.
fn assert_f_sessions(capsule: &JsonValue, dispatcher: &str, left: &str) {
    let sessions = member(capsule, "sessions").as_array().unwrap();
    assert_eq!(sessions.len(), 3, "{sessions:?}");
    assert_eq!(string_of(&sessions[0], "end_state"), "ended");
    assert_eq!(
        string_of(member(&sessions[0], "agent"), "name"),
        "takeover-eval-seed"
    );
    let ended = &sessions[1];
    assert_eq!(string_of(ended, "session_id"), dispatcher);
    assert_eq!(string_of(ended, "end_state"), "ended", "{ended:?}");
    let agent = member(ended, "agent");
    assert_eq!(string_of(agent, "name"), "takeover-eval-dispatcher");
    assert_eq!(string_of(agent, "role"), "orchestrator");
    assert_eq!(member(agent, "parent_session_id"), &JsonValue::Null);
    let open = &sessions[2];
    assert_eq!(string_of(open, "session_id"), left);
    assert!(
        !["ended", "crashed"].contains(&string_of(open, "end_state")),
        "{open:?}"
    );
    let agent = member(open, "agent");
    assert_eq!(string_of(agent, "name"), "takeover-eval-left");
    assert_eq!(string_of(agent, "role"), "sub_agent");
    assert_eq!(string_of(agent, "parent_session_id"), dispatcher);
}

/// U13 F1: F's Pack before the run. The left Session's line names its
/// dispatcher, Session 2, and carries the takeover hint; the dispatcher's line
/// says it ended; no other Session is open.
fn assert_f_pack(run: &Path, dispatcher: &str, left: &str) {
    let pack = read(&run.join("pack-before.txt"));
    let line = pack
        .lines()
        .find(|line| line.starts_with("Session 3 "))
        .unwrap_or_else(|| panic!("{pack}"));
    assert!(
        line.starts_with(&format!("Session 3 {left} not ended (")),
        "{line}"
    );
    assert!(
        line.ends_with(&format!(
            "; agent「takeover-eval-left」(sub_agent, dispatched by Session 2); end it before taking over: \
             memprove session end {left} --abandoned --reason <why> (MCP: end {{session:3, \
             abandoned:true}})"
        )),
        "{line}"
    );
    let line = pack
        .lines()
        .find(|line| line.starts_with("Session 2 "))
        .unwrap_or_else(|| panic!("{pack}"));
    assert!(
        line.starts_with(&format!("Session 2 {dispatcher} ended normally (")),
        "{line}"
    );
    assert!(
        line.ends_with("; agent「takeover-eval-dispatcher」(orchestrator)"),
        "{line}"
    );
    assert_eq!(
        pack.lines()
            .filter(|line| line.starts_with("Session ") && line.contains(" not ended ("))
            .count(),
        1,
        "{pack}"
    );
}

/// A stand-in for `claude`: it records its working directory, arguments,
/// and environment in `$HOME`, runs the project's `SessionStart` hook as
/// Claude Code does, and prints a transcript with the hook's response, an
/// `init` line, and a `result`.
fn stand_in_claude(home: &Path) -> PathBuf {
    let path = home.join("claude");
    fs::write(
        &path,
        format!(
            r#"#!{PYTHON}
import json, os, subprocess, sys
if sys.argv[1:] == ["--version"]:
    print("0.0.0 (stand-in claude)")
    sys.exit(0)
if sys.argv[1:] == ["--help"]:
    print("  --setting-sources <sources>  Comma-separated list of setting sources")
    sys.exit(0)
home = os.environ["HOME"]
number = len([n for n in os.listdir(home) if n.startswith("claude-call-")]) + 1
with open(os.path.join(home, "claude-call-%d.json" % number), "w") as f:
    json.dump({{"cwd": os.getcwd(), "argv": sys.argv[1:], "env": dict(os.environ)}}, f)
with open(".claude/settings.local.json") as f:
    settings = json.load(f)
hook = settings["hooks"]["SessionStart"][0]["hooks"][0]
session = "stand-in-%d" % number
env = dict(os.environ, CLAUDE_ENV_FILE=os.path.join(home, "claude-env-%d.sh" % number))
env.update(settings.get("env", {{}}))
start = {{"session_id": session, "transcript_path": "/nowhere", "cwd": os.getcwd(),
         "hook_event_name": "SessionStart", "source": "startup"}}
out = subprocess.run([hook["command"]] + hook["args"], input=json.dumps(start),
                     capture_output=True, text=True, env=env).stdout
for event in [
    {{"type": "system", "subtype": "hook_response", "hook_name": "SessionStart:startup",
     "output": out, "stdout": out, "stderr": "", "exit_code": 0}},
    {{"type": "system", "subtype": "init", "model": "stand-in-model", "session_id": session}},
    {{"type": "result", "subtype": "success", "num_turns": 1, "duration_ms": 1,
     "permission_denials": [], "result": "No change.\nFacts: #1", "session_id": session}},
]:
    print(json.dumps(event, ensure_ascii=False))
"#
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// What the stand-in recorded for its `number`-th start.
fn client_call(home: &Path, number: usize) -> JsonValue {
    read_json(&home.join(format!("claude-call-{number}.json")))
}

#[test]
#[ignore = "needs /usr/local/bin/python3.12 with pytest, sqlite3, and the debug memproved; see the module documentation"]
fn u7_h1_each_run_gets_a_new_environment_and_a_clean_client() {
    let (_guard, home) = home();
    let out = home.join("results");
    let binaries = cli_binary().parent().unwrap().to_owned();
    let claude = stand_in_claude(&home);
    let output = run_script(
        &home,
        &[
            ("TAKEOVER_EVAL_BIN", binaries.to_str().unwrap()),
            ("TAKEOVER_EVAL_OUT", out.to_str().unwrap()),
            ("TAKEOVER_EVAL_PYTHON", PYTHON),
            ("TAKEOVER_EVAL_CLAUDE", claude.to_str().unwrap()),
            ("TAKEOVER_EVAL_SCENARIOS", "A"),
            ("TAKEOVER_EVAL_RUNS", "2"),
            ("TAKEOVER_EVAL_TMP_PREFIX", &tmp_prefix()),
            ("TAKEOVER_EVAL_MODEL", "stand-in-model-x"),
            // What the desktop app passes down; none may reach the client.
            ("MEMPROVE_CONFIG_DIR", "/nowhere/config"),
            ("MEMPROVE_CLIENT_SESSION", "desktop-app-session"),
            ("ANTHROPIC_API_KEY", "not-a-key"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "parent-session"),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));

    let log = read(&out.join("claude-invocations.log"));
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(lines[0].ends_with(" scenario=A run=1"), "{log}");
    assert!(lines[1].ends_with(" scenario=A run=2"), "{log}");
    assert_eq!(read(&out.join("prompt.txt")), format!("{PROMPT}\n"));

    let mut roots = Vec::new();
    let mut heads = Vec::new();
    for number in 1..=2 {
        let run = out.join(format!("A-{number}"));
        let root = assert_root_gone(&run);
        assert_client(&home, number, &root);
        assert_archive(&run, number);
        heads.push(
            read(&run.join("pack-before.txt"))
                .lines()
                .next()
                .unwrap()
                .split("; ")
                .find(|part| part.starts_with("probed HEAD "))
                .unwrap()
                .to_owned(),
        );
        roots.push(root);
    }
    assert_ne!(roots[0], roots[1], "the two runs shared an environment");
    assert_eq!(heads[0], heads[1], "the two runs had different seeds");
}

/// The stand-in's `number`-th start: the standard prompt in the temporary
/// repository, a clean environment, the temporary bin directory first.
fn assert_client(home: &Path, number: usize, root: &str) {
    let call = client_call(home, number);
    assert_eq!(string_of(&call, "cwd"), format!("{root}/repo"));
    let argv: Vec<&str> = member(&call, "argv")
        .as_array()
        .unwrap()
        .iter()
        .map(|word| word.as_str().unwrap())
        .collect();
    // U10 T3 (Owner O4): only the project's memprove server, only the
    // project's settings (the stand-in's --help lists --setting-sources).
    let mcp_config = format!("{root}/repo/.mcp.json");
    assert_eq!(
        argv,
        [
            "-p",
            PROMPT,
            "--output-format",
            "stream-json",
            "--verbose",
            "--mcp-config",
            mcp_config.as_str(),
            "--strict-mcp-config",
            "--setting-sources",
            "project,local",
            "--model",
            "stand-in-model-x"
        ]
    );
    let env = member(&call, "env").as_object().unwrap();
    for (name, value) in env {
        assert!(
            !name.starts_with("MEMPROVE_")
                && !name.starts_with("ANTHROPIC_")
                && !name.starts_with("CLAUDE"),
            "{name} reached the client"
        );
        let value = value.as_str().unwrap();
        assert!(!value.contains("desktop-app-session"), "{name}={value}");
    }
    let home_value = env
        .iter()
        .find(|(name, _)| name == "HOME")
        .unwrap()
        .1
        .as_str();
    assert_eq!(home_value, home.to_str());
    let path = env
        .iter()
        .find(|(name, _)| name == "PATH")
        .unwrap()
        .1
        .as_str()
        .unwrap();
    assert!(path.starts_with(&format!("{root}/bin:")), "{path}");
}

/// A run's archive: the transcript, the hook's text, the ledger before and
/// after, the Capsule, the Events, and `run.json`.
fn assert_archive(run: &Path, number: usize) {
    let hook = read(&run.join("hook.txt"));
    assert!(
        hook.starts_with("memprove: below is the Context Pack of Task "),
        "{hook}"
    );
    assert!(hook.contains("\nPack: memprove-pack v1; "), "{hook}");
    assert!(hook.contains("\nLast passed: "), "{hook}");
    assert_eq!(
        read(&run.join("transcript.jsonl")).lines().count(),
        3,
        "the transcript is the client's standard output"
    );
    for name in ["ledger-verify-before.json", "ledger-verify-after.json"] {
        assert_eq!(
            member(&read_json(&run.join(name)), "valid"),
            &JsonValue::Bool(true)
        );
    }
    assert_eq!(read(&run.join("ledger-verify-after.exit")), "0\n");
    let capsule = read_json(&run.join("capsule-after.json"));
    assert_eq!(member(&capsule, "claims").as_array().unwrap().len(), 3);
    assert!(
        read(&run.join("resume-text-after.txt"))
            .starts_with("Task「calc: complete the four operations」")
    );
    let events = read(&run.join("events-after.jsonl"));
    let page = parse_strict(events.lines().next().unwrap()).unwrap();
    assert!(member(&page, "items").as_array().unwrap().len() >= 8);
    let meta = read_json(&run.join("run.json"));
    assert_eq!(string_of(&meta, "scenario"), "A");
    assert_eq!(
        member(&meta, "run").as_i64(),
        Some(i64::try_from(number).unwrap())
    );
    assert_eq!(string_of(&meta, "model_requested"), "stand-in-model-x");
    assert_eq!(member(&meta, "exit_code").as_i64(), Some(0));
    assert!(member(&meta, "duration_ms").as_i64().unwrap() > 0);
    assert_eq!(
        string_of(&meta, "claude_version"),
        "0.0.0 (stand-in claude)"
    );
    assert!(string_of(&meta, "memprove_version").starts_with("memprove "));
}

/// U9 K6 (Owner O7): the client of B is started with
/// `MEMPROVE_AGENT_NAME=takeover-eval-other` and
/// `MEMPROVE_AGENT_ROLE=sub_agent`, and with no other `MEMPROVE_*`
/// variable; the clients of the other scenarios get none. The script's
/// temporary directories are moved off `/private/tmp/u7-h` with
/// `TAKEOVER_EVAL_TMP_PREFIX`, which this test uses so that it touches
/// nothing of the U7 evaluation's (U10: the test's own
/// `TAKEOVER_EVAL_TMP_PREFIX`, when set, with `-k<pid>`). U10 T2: B's client
/// starts last and only once the seed's open Session is 16 minutes old, so
/// this test takes about 16 minutes; `run.json` of B records the wait.
/// U13 E1 (Owner O3): the client of E is started with the same two variables
/// and `MEMPROVE_PARENT_SESSION`, the Session the seed left open
/// (`parent_session_id` of its `seed.json`), and runs after B, so its seed is
/// old by then too; the hook's opening names that parent. U13 F1 (Owner O4):
/// the client of F is started with B's two variables and no parent, in the
/// first pass with A's, without waiting: its seed's open Session is younger
/// than `SEED_AGE` when it starts, and the hook's Pack shows that Session's
/// dispatcher ended.
#[test]
#[ignore = "needs /usr/local/bin/python3.12 with pytest, sqlite3, and the debug memproved; see the module documentation"]
fn u9_k6_the_b_client_declares_an_agent_and_the_others_declare_none() {
    let (_guard, home) = home();
    let out = home.join("results");
    let binaries = cli_binary().parent().unwrap().to_owned();
    let claude = stand_in_claude(&home);
    let prefix = std::env::var("TAKEOVER_EVAL_TMP_PREFIX").map_or_else(
        |_| format!("/private/tmp/u9-s-k-h{}", std::process::id()),
        |prefix| format!("{prefix}-k{}", std::process::id()),
    );
    let output = run_script(
        &home,
        &[
            ("TAKEOVER_EVAL_BIN", binaries.to_str().unwrap()),
            ("TAKEOVER_EVAL_OUT", out.to_str().unwrap()),
            ("TAKEOVER_EVAL_PYTHON", PYTHON),
            ("TAKEOVER_EVAL_CLAUDE", claude.to_str().unwrap()),
            ("TAKEOVER_EVAL_SCENARIOS", "A,B,E,F"),
            ("TAKEOVER_EVAL_TMP_PREFIX", &prefix),
            ("MEMPROVE_AGENT_NAME", "from-the-desktop-app"),
            ("MEMPROVE_CLIENT_SESSION", "desktop-app-session"),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let declared = |number: usize| -> Vec<(String, String)> {
        let call = client_call(&home, number);
        let mut found: Vec<(String, String)> = member(&call, "env")
            .as_object()
            .unwrap()
            .iter()
            .filter(|(name, _)| name.starts_with("MEMPROVE_"))
            .map(|(name, value)| (name.clone(), value.as_str().unwrap().to_owned()))
            .collect();
        found.sort();
        found
    };
    // A and (U13 F1) F first, then B and E (the order of
    // TAKEOVER_EVAL_SCENARIOS in each pass).
    assert_eq!(declared(1), Vec::<(String, String)>::new());
    let named = [
        (
            "MEMPROVE_AGENT_NAME".to_owned(),
            "takeover-eval-other".to_owned(),
        ),
        ("MEMPROVE_AGENT_ROLE".to_owned(), "sub_agent".to_owned()),
    ];
    // U13 F1: F's client is B's agent, with no parent.
    assert_eq!(declared(2), named);
    assert_eq!(declared(3), named);
    // U13 E1: E's client also has its parent, the Session E's seed left open.
    let parent = string_of(
        &read_json(&out.join("E-1").join("seed.json")),
        "parent_session_id",
    )
    .to_owned();
    let mut with_parent = named.to_vec();
    with_parent.push(("MEMPROVE_PARENT_SESSION".to_owned(), parent.clone()));
    assert_eq!(declared(4), with_parent);
    // U10 T2: B waited until the open Session was 16 minutes old; A did not.
    // U13 E1: so did E, whose seed is as old once B's wait is over.
    let meta = |name: &str| read_json(&out.join(name).join("run.json"));
    let number = |value: &JsonValue, name: &str| member(value, name).as_i64();
    for waited in ["B-1", "E-1"] {
        assert!(
            number(&meta(waited), "seed_age_seconds").unwrap() >= 960,
            "{waited}"
        );
        assert!(
            number(&meta(waited), "seed_wait_seconds").is_some(),
            "{waited}"
        );
    }
    assert_eq!(member(&meta("A-1"), "seed_wait_seconds"), &JsonValue::Null);
    // U13 F1: F did not wait; `run.json` has its seed's age, younger than
    // SEED_AGE, and no wait.
    assert_eq!(member(&meta("F-1"), "seed_wait_seconds"), &JsonValue::Null);
    let age = number(&meta("F-1"), "seed_age_seconds").unwrap();
    assert!((0..960).contains(&age), "{age}");
    // U13 E1: `run.json` names E's parent, and no parent in A, B, and F.
    assert_eq!(string_of(&meta("E-1"), "parent_session_id"), parent);
    for other in ["A-1", "B-1", "F-1"] {
        assert_eq!(
            member(&meta(other), "parent_session_id"),
            &JsonValue::Null,
            "{other}"
        );
    }
    // The hook's opening names the client, and in E its parent too.
    let hook = |name: &str| read(&out.join(name).join("hook.txt"));
    assert!(
        hook("B-1").contains("You are agent「takeover-eval-other」(sub_agent); "),
        "{}",
        hook("B-1")
    );
    assert!(
        hook("E-1").contains(&format!(
            "You are agent「takeover-eval-other」(sub_agent, parent Session {parent}); "
        )),
        "{}",
        hook("E-1")
    );
    assert_f_hook(&out.join("F-1"));
    // Neither the inherited variables nor the temporary directories remain.
    for scenario in ["A", "B", "E", "F"] {
        let run = out.join(format!("{scenario}-1"));
        let root = string_of(&read_json(&run.join("seed.json")), "root").to_owned();
        assert!(root.starts_with(&format!("{prefix}.")), "{root}");
        assert!(!Path::new(&root).exists(), "{root} was not removed");
    }
}

/// U13 F1: the hook's response to F's client names it as B's does, and its
/// Pack shows the open Session's dispatcher, Session 2, ended.
fn assert_f_hook(run: &Path) {
    let hook = read(&run.join("hook.txt"));
    let seed = read_json(&run.join("seed.json"));
    let dispatcher = string_of(&seed, "dispatcher_session_id");
    let left = string_of(&seed, "left_session_id");
    assert!(
        hook.contains("You are agent「takeover-eval-other」(sub_agent); "),
        "{hook}"
    );
    assert!(
        hook.lines().any(
            |line| line.starts_with(&format!("Session 3 {left} not ended ("))
                && line.contains(
                    "; agent「takeover-eval-left」(sub_agent, dispatched by Session 2); "
                )
        ),
        "{hook}"
    );
    assert!(
        hook.lines()
            .any(|line| line.starts_with(&format!("Session 2 {dispatcher} ended normally ("))),
        "{hook}"
    );
}

/// `TAKEOVER_EVAL_TMP_PREFIX` is one name under `/private/tmp`; any other is
/// refused before anything is written.
#[test]
fn u9_k6_the_temporary_prefix_is_one_name_under_private_tmp() {
    for bad in [
        "/tmp/x",
        "/private/tmp/a/b",
        "/private/tmp/..",
        "relative",
        "/private/tmp/",
    ] {
        let (_guard, home) = home();
        let output = run_script(&home, &[("TAKEOVER_EVAL_TMP_PREFIX", bad)]);
        assert_refused(
            &output,
            "TAKEOVER_EVAL_TMP_PREFIX must be one name under /private/tmp",
        );
    }
}
