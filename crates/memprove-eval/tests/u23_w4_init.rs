//! U23-W4: `memprove eval init` and the test-command guess, on temporary
//! directories.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use memprove_eval::{
    AgentKind,
    init::{
        Detected, InitReport, InitSpec, ProjectKind, WORKFLOW_PATH, detect, detect_check, init,
        templates,
    },
};

fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, text) in files {
        let path = dir.path().join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }
    dir
}

/// Files of a project, the kind it is, its setup lines and its check.
type Case = (
    &'static [(&'static str, &'static str)],
    ProjectKind,
    &'static [&'static str],
    &'static [&'static str],
);

fn words(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|word| (*word).to_owned()).collect()
}

fn spec(root: &Path) -> InitSpec {
    InitSpec {
        repo_root: root.to_path_buf(),
        agent: AgentKind::ClaudeCode,
        workflow: true,
        dry_run: false,
    }
}

/// Every file under `root` with its bytes.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = fs::read(&path).expect("read");
                files.insert(
                    path.strip_prefix(root).expect("inside").to_path_buf(),
                    bytes,
                );
            }
        }
    }
    files
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).expect("read")
}

#[test]
fn the_five_kinds_of_project_are_detected() {
    let cases: [Case; 9] = [
        (
            &[("Cargo.toml", "[package]\nname = \"a\"\n")],
            ProjectKind::Rust,
            &[],
            &["cargo", "test"],
        ),
        (
            &[("package.json", r#"{"scripts": {"test": "jest"}}"#)],
            ProjectKind::Node,
            &["npm install"],
            &["npm", "test"],
        ),
        (
            &[
                ("package.json", r#"{"scripts": {"test": "jest"}}"#),
                ("package-lock.json", "{}"),
            ],
            ProjectKind::Node,
            &["npm ci"],
            &["npm", "test"],
        ),
        (
            &[
                ("package.json", r#"{"scripts": {"test": "vitest"}}"#),
                ("pnpm-lock.yaml", ""),
            ],
            ProjectKind::Node,
            &["pnpm install --frozen-lockfile"],
            &["pnpm", "test"],
        ),
        (
            &[
                ("package.json", r#"{"scripts": {"test": "mocha"}}"#),
                ("yarn.lock", ""),
            ],
            ProjectKind::Node,
            &["yarn install --frozen-lockfile"],
            &["yarn", "test"],
        ),
        (
            &[("pyproject.toml", "[project]\nname = \"a\"\n")],
            ProjectKind::Python,
            &[],
            &["python3", "-m", "pytest"],
        ),
        (
            &[("pytest.ini", "[pytest]\n")],
            ProjectKind::Python,
            &[],
            &["python3", "-m", "pytest"],
        ),
        (
            &[("go.mod", "module example.com/a\n")],
            ProjectKind::Go,
            &[],
            &["go", "test", "./..."],
        ),
        (
            &[("Makefile", "all: build\n\ntest: build\n\t./run-tests\n")],
            ProjectKind::Make,
            &[],
            &["make", "test"],
        ),
    ];
    for (files, kind, setup, check) in cases {
        let dir = dir_with(files);
        let found = detect(dir.path()).unwrap_or_else(|| panic!("{files:?}"));
        assert_eq!(found.kind, kind, "{files:?}");
        let setup_lines: Vec<String> = found.setup.iter().map(|argv| argv.join(" ")).collect();
        assert_eq!(setup_lines, setup, "{files:?}");
        assert_eq!(found.check, words(check), "{files:?}");
        assert_eq!(detect_check(dir.path()), Some(words(check)));
        assert!(found.also.is_empty(), "{files:?}");
    }
}

#[test]
fn nothing_is_guessed_without_a_test_command() {
    for files in [
        &[][..],
        &[(
            "package.json",
            r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#,
        )],
        &[("package.json", r#"{"scripts": {"build": "tsc"}}"#)],
        &[("Makefile", ".PHONY: test\nbuild:\n\tcc a.c\n")],
        &[("README.md", "# a\n")],
    ] {
        let dir = dir_with(files);
        assert_eq!(detect(dir.path()), None, "{files:?}");
    }
}

#[test]
fn the_first_match_wins_and_the_others_are_named() {
    let dir = dir_with(&[
        ("Makefile", "test:\n\tcargo test\n"),
        ("package.json", r#"{"scripts": {"test": "jest"}}"#),
        ("Cargo.toml", "[workspace]\n"),
    ]);
    let found: Detected = detect(dir.path()).expect("detected");
    assert_eq!(found.kind, ProjectKind::Rust);
    assert_eq!(found.also, ["package.json", "Makefile"]);
    let report = init(&spec(dir.path())).expect("init");
    let notes = report.notes.join("\n");
    assert!(
        notes.contains("checks with `cargo test` (from Cargo.toml)"),
        "{notes}"
    );
    assert!(
        notes.contains("Also found package.json, Makefile"),
        "{notes}"
    );
}

#[test]
fn init_in_an_empty_repository_twice_changes_nothing_the_second_time() {
    let dir = dir_with(&[]);
    let first = init(&spec(dir.path())).expect("first");
    let written: Vec<&str> = first
        .written
        .iter()
        .map(|path| path.to_str().expect("utf-8"))
        .collect();
    assert_eq!(written, ["evals/README.md", WORKFLOW_PATH, "AGENTS.md"]);
    assert!(first.unchanged.is_empty());
    let notes = first.notes.join("\n");
    assert!(
        notes.contains("evals/smoke.json was not written"),
        "{notes}"
    );
    assert!(notes.contains("ANTHROPIC_API_KEY"), "{notes}");
    assert_eq!(
        read(dir.path().join("AGENTS.md")),
        templates::agents_block(AgentKind::ClaudeCode)
    );

    let before = snapshot(dir.path());
    let second = init(&spec(dir.path())).expect("second");
    assert_eq!(second.written, Vec::<PathBuf>::new());
    assert_eq!(second.unchanged, first.written);
    assert!(second.kept.is_empty());
    assert_eq!(snapshot(dir.path()), before);
}

#[test]
fn init_writes_a_smoke_task_with_the_detected_command() {
    let dir = dir_with(&[
        ("package.json", r#"{"scripts": {"test": "jest"}}"#),
        ("package-lock.json", "{}"),
    ]);
    let report: InitReport = init(&spec(dir.path())).expect("init");
    assert!(report.written.contains(&PathBuf::from("evals/smoke.json")));
    let smoke = read(dir.path().join("evals/smoke.json"));
    let value = memprove_eval::json::parse(&smoke, None).expect("json");
    let reader = memprove_eval::json::Reader::new(&value, None, "").expect("object");
    reader
        .deny_unknown(&[
            "schema",
            "id",
            "title",
            "prompt",
            "setup",
            "check",
            "agent_timeout_secs",
            "tags",
        ])
        .expect("only task members");
    assert_eq!(
        reader.str("schema").expect("schema"),
        "memprove.eval.task.v1"
    );
    assert_eq!(reader.str("id").expect("id"), "smoke");
    assert_eq!(
        reader.str("prompt").expect("prompt"),
        "Run the test suite; if anything fails, fix it; otherwise change nothing."
    );
    let check = reader.get("check").expect("check");
    let check = memprove_eval::json::Reader::new(check, None, "check").expect("object");
    assert_eq!(check.strs("argv").expect("argv"), ["npm", "test"]);
    let setup = reader.array("setup").expect("setup");
    let first = memprove_eval::json::Reader::new(&setup[0], None, "setup[0]").expect("object");
    assert_eq!(first.strs("argv").expect("argv"), ["npm", "ci"]);

    let again = init(&spec(dir.path())).expect("again");
    assert!(again.written.is_empty(), "{again:?}");
}

#[test]
fn an_existing_agents_file_gets_the_block_appended_then_replaced_in_place() {
    let dir = dir_with(&[("AGENTS.md", "# Rules\n\nRun `make lint` first.\n")]);
    init(&spec(dir.path())).expect("first");
    let block = templates::agents_block(AgentKind::ClaudeCode);
    let agents = read(dir.path().join("AGENTS.md"));
    assert_eq!(
        agents,
        format!("# Rules\n\nRun `make lint` first.\n\n{block}")
    );
    assert!(!dir.path().join("CLAUDE.md").exists());

    let again = init(&spec(dir.path())).expect("second");
    assert!(again.written.is_empty(), "{again:?}");
    assert!(again.unchanged.contains(&PathBuf::from("AGENTS.md")));

    // An older block, with text after it: only the block changes.
    let older = format!(
        "# Rules\n\n{}\nold line\n{}\n\n# More rules\n",
        templates::BEGIN,
        templates::END
    );
    fs::write(dir.path().join("AGENTS.md"), &older).expect("write");
    let replaced = init(&spec(dir.path())).expect("third");
    assert_eq!(replaced.written, [PathBuf::from("AGENTS.md")]);
    assert_eq!(
        read(dir.path().join("AGENTS.md")),
        format!("# Rules\n\n{block}\n# More rules\n")
    );

    // A new agent rewrites the block, and only the block.
    let mut codex = spec(dir.path());
    codex.agent = AgentKind::Codex;
    codex.workflow = false;
    init(&codex).expect("codex");
    let agents = read(dir.path().join("AGENTS.md"));
    assert!(agents.contains("--agent codex"), "{agents}");
    assert_eq!(agents.matches(templates::BEGIN).count(), 1, "{agents}");
}

#[test]
fn claude_md_alone_gets_the_block_and_both_files_get_it_when_both_exist() {
    let only = dir_with(&[("CLAUDE.md", "Be brief.\n")]);
    let report = init(&spec(only.path())).expect("claude only");
    assert!(report.written.contains(&PathBuf::from("CLAUDE.md")));
    assert!(!only.path().join("AGENTS.md").exists());
    assert!(read(only.path().join("CLAUDE.md")).contains(templates::END));

    let both = dir_with(&[("CLAUDE.md", "Be brief.\n"), ("AGENTS.md", "Be kind.\n")]);
    let report = init(&spec(both.path())).expect("both");
    for name in ["AGENTS.md", "CLAUDE.md"] {
        assert!(report.written.contains(&PathBuf::from(name)), "{name}");
        assert!(
            read(both.path().join(name)).contains(templates::BEGIN),
            "{name}"
        );
    }
}

#[test]
fn a_begin_marker_without_an_end_marker_is_refused() {
    let dir = dir_with(&[(
        "AGENTS.md",
        &format!("{}\nhalf a block\n", templates::BEGIN),
    )]);
    let error = init(&spec(dir.path())).expect_err("refused");
    assert!(error.to_string().contains("without"), "{error}");
}

#[test]
fn a_dry_run_writes_nothing_and_says_what_it_would_write() {
    let dir = dir_with(&[
        ("Cargo.toml", "[package]\nname = \"a\"\n"),
        ("AGENTS.md", "x\n"),
    ]);
    let before = snapshot(dir.path());
    let mut dry = spec(dir.path());
    dry.dry_run = true;
    let report = init(&dry).expect("dry run");
    assert_eq!(snapshot(dir.path()), before);
    let written: Vec<&str> = report
        .written
        .iter()
        .map(|path| path.to_str().expect("utf-8"))
        .collect();
    assert_eq!(
        written,
        [
            "evals/README.md",
            "evals/smoke.json",
            WORKFLOW_PATH,
            "AGENTS.md"
        ]
    );
    assert!(report.notes.iter().any(|note| note.starts_with("Dry run")));
}

#[test]
fn no_workflow_leaves_github_alone() {
    let dir = dir_with(&[("go.mod", "module a\n")]);
    let mut spec = spec(dir.path());
    spec.workflow = false;
    let report = init(&spec).expect("init");
    assert!(!dir.path().join(".github").exists());
    assert!(!report.notes.join(" ").contains("secret"));
}

#[test]
fn the_workflow_is_valid_for_actionlint_when_it_is_installed() {
    for agent in [AgentKind::ClaudeCode, AgentKind::Codex] {
        let text = templates::workflow(agent);
        assert!(!text.contains('\t'), "YAML has no tabs");
        for needle in [
            "workflow_dispatch:",
            "fetch-depth: 0",
            "gh release download",
            "--no-memory",
            "memprove eval report latest",
            "memprove eval gate latest --baseline evals/baseline.json",
            "hashFiles('evals/baseline.json')",
            "actions/upload-artifact@v4",
            "contains(github.event.pull_request.labels.*.name, 'eval')",
        ] {
            assert!(text.contains(needle), "{needle}");
        }
        let dir = dir_with(&[(".github/workflows/memprove-eval.yml", &text)]);
        match Command::new("actionlint")
            .arg(".github/workflows/memprove-eval.yml")
            .current_dir(dir.path())
            .output()
        {
            Ok(output) => assert!(
                output.status.success(),
                "actionlint: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(_) => eprintln!("actionlint: NOT RUN (not on PATH)"),
        }
    }
}
