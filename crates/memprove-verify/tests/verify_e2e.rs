//! `memprove verify` end to end on a small repository: the six things the
//! first version has to get right. The task of the fixture is to make
//! `src/answer.txt` say 42; its checks are shell scripts, so the tests need
//! no toolchain but `git` and `sh`.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use memprove_eval::git::git;
use memprove_verify::{
    BaseSource, FindingKind, Options, Receipt, Verdict, hook, init,
    run::{self, FAILURE_FILE, PROTECTED_PATCH_FILE, RECEIPT_FILE, SUMMARY_FILE},
    state_dir, verify,
};

/// Runs every script under `tests/`; passes when there is none.
const RUN_ALL: &str =
    "for t in tests/*.sh; do [ -e \"$t\" ] || continue; sh \"$t\" || exit 1; done";

fn contract(check: &str) -> String {
    format!(
        r#"{{
  "schema": "memprove.verify.contract.v1",
  "checks": [{{"name": "tests", "argv": ["sh", "-c", {}], "timeout_secs": 60}}],
  "protected": ["tests/**"]
}}
"#,
        memprove_eval::json::string(check)
    )
}

/// A repository whose one commit holds the task unsolved: the contract,
/// `src/answer.txt` saying 0, and a test that wants a line saying 42 in a
/// file of `src/`. `*.cache` is ignored.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for args in [
        &["init", "--quiet", "-b", "main"][..],
        &["config", "user.email", "verify@example.invalid"],
        &["config", "user.name", "verify"],
        &["config", "commit.gpgsign", "false"],
    ] {
        git(root, args).expect("git");
    }
    write(root, ".memprove/verify.json", &contract(RUN_ALL));
    write(root, ".gitignore", "*.cache\n");
    write(root, "src/answer.txt", "0\n");
    write(root, "tests/answer.sh", "cat src/* | grep -qx 42\n");
    commit(root, "the task");
    dir
}

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, text).expect("write");
}

fn commit(root: &Path, message: &str) {
    git(root, &["add", "--all"]).expect("add");
    git(root, &["commit", "--quiet", "-m", message]).expect("commit");
}

fn verified(root: &Path) -> (Receipt, PathBuf) {
    verify(&Options::new(root).expect("options")).expect("verified")
}

fn kinds(receipt: &Receipt) -> Vec<(FindingKind, &str)> {
    receipt
        .findings
        .iter()
        .map(|finding| (finding.kind, finding.path.as_str()))
        .collect()
}

#[test]
fn work_that_fails_a_check_fails() {
    let repo = fixture();
    write(repo.path(), "src/answer.txt", "41\n");
    let (receipt, run_dir) = verified(repo.path());
    assert_eq!(receipt.verdict, Verdict::Fail);
    assert_eq!(
        receipt.reasons,
        [format!(
            "check `tests` (sh -c {RUN_ALL}) exited with code 1"
        )]
    );
    assert!(receipt.restored.is_none());
    assert_eq!(receipt.base.chosen_by, BaseSource::Head);
    assert_eq!(receipt.diff, (1, 1, 1));
    for file in [
        RECEIPT_FILE,
        SUMMARY_FILE,
        FAILURE_FILE,
        "candidate/check-tests.out",
    ] {
        assert!(run_dir.join(file).is_file(), "{file}");
    }
    let summary = fs::read_to_string(run_dir.join(SUMMARY_FILE)).expect("summary");
    assert!(summary.starts_with("memprove verify: FAIL\n"), "{summary}");
    assert_eq!(
        receipt.libtest_summary(),
        "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
    );
    // Nothing of the repository changed: the index is as it was and the
    // run's files are not in the worktree.
    assert_eq!(
        git(repo.path(), &["status", "--porcelain"]).expect("status"),
        " M src/answer.txt\n"
    );
}

#[test]
fn work_that_is_done_passes_committed_or_not() {
    let repo = fixture();
    write(repo.path(), "src/answer.txt", "42\n");
    let (receipt, _) = verified(repo.path());
    assert_eq!(receipt.verdict, Verdict::Pass, "{:?}", receipt.reasons);
    assert!(receipt.reasons.is_empty() && receipt.findings.is_empty());
    assert!(receipt.restored.is_none(), "no protected path differs");

    // A new file Git does not ignore is part of the work.
    write(repo.path(), "src/answer.txt", "0\n");
    write(repo.path(), "src/more.txt", "42\n");
    assert_eq!(verified(repo.path()).0.verdict, Verdict::Pass);

    // A commit, compared with its parent.
    commit(repo.path(), "the answer");
    let mut options = Options::new(repo.path()).expect("options");
    options.commit = Some("HEAD".to_owned());
    let (receipt, _) = verify(&options).expect("verified");
    assert_eq!(receipt.verdict, Verdict::Pass);
    assert_eq!(receipt.base.chosen_by, BaseSource::Parent);
    assert_eq!(receipt.diff.0, 1);
}

#[test]
fn passing_by_weakening_or_deleting_a_test_needs_a_person() {
    let repo = fixture();
    // The answer stays wrong; the test is made to pass.
    write(repo.path(), "tests/answer.sh", "true\n");
    let (receipt, run_dir) = verified(repo.path());
    assert_eq!(receipt.verdict, Verdict::NeedsHuman);
    assert_eq!(
        kinds(&receipt),
        [(FindingKind::ProtectedModified, "tests/answer.sh")]
    );
    assert!(receipt.candidate.passed());
    assert!(!receipt.restored.as_ref().expect("ran again").passed());
    assert!(
        receipt.reasons[0].starts_with(
            "the checks pass only with the work's changes to the protected paths; with those \
             paths as at the base, check `tests`"
        ),
        "{:?}",
        receipt.reasons
    );
    let patch = fs::read_to_string(run_dir.join(PROTECTED_PATCH_FILE)).expect("patch");
    assert!(
        patch.contains("-cat src/* | grep -qx 42\n+true\n"),
        "{patch}"
    );

    // The same with the test deleted.
    fs::remove_file(repo.path().join("tests/answer.sh")).expect("delete");
    let (receipt, _) = verified(repo.path());
    assert_eq!(receipt.verdict, Verdict::NeedsHuman);
    assert_eq!(
        kinds(&receipt),
        [(FindingKind::ProtectedDeleted, "tests/answer.sh")]
    );
}

#[test]
fn honest_work_that_also_touches_tests_passes() {
    let repo = fixture();
    write(repo.path(), "src/answer.txt", "42\n");
    write(repo.path(), "tests/more.sh", "test -s src/answer.txt\n");
    let (receipt, _) = verified(repo.path());
    assert_eq!(receipt.verdict, Verdict::Pass, "{:?}", receipt.reasons);
    assert_eq!(
        kinds(&receipt),
        [(FindingKind::ProtectedAdded, "tests/more.sh")]
    );
    assert!(receipt.restored.as_ref().expect("ran again").passed());
    assert!(
        receipt.notes[0].starts_with("the work changes protected paths, and the checks also pass")
    );
}

#[test]
fn the_work_cannot_weaken_its_own_contract() {
    let repo = fixture();
    write(repo.path(), ".memprove/verify.json", &contract("true"));
    let (receipt, _) = verified(repo.path());
    assert_eq!(receipt.verdict, Verdict::Fail, "the base's check ran");
    assert_eq!(
        kinds(&receipt),
        [(FindingKind::ContractChanged, ".memprove/verify.json")]
    );
    assert_eq!(receipt.candidate.checks[0].argv[2], RUN_ALL);

    // Committing the weaker contract does not help when the base is named.
    let base = git(repo.path(), &["rev-parse", "HEAD"]).expect("head");
    commit(repo.path(), "weaker");
    let mut options = Options::new(repo.path()).expect("options");
    options.base = Some(base.trim().to_owned());
    assert_eq!(verify(&options).expect("verified").0.verdict, Verdict::Fail);

    // --contract is the caller's word, and wins.
    let file = repo.path().join("other.json");
    fs::write(&file, contract("true")).expect("write");
    options.contract_file = Some(file);
    assert_eq!(verify(&options).expect("verified").0.verdict, Verdict::Pass);
}

#[test]
fn leftovers_in_the_worktree_do_not_pass_the_check() {
    let repo = fixture();
    // An ignored file makes the check pass where the agent worked ...
    write(repo.path(), "src/left.cache", "42\n");
    let in_place = Command::new("sh")
        .args(["-c", RUN_ALL])
        .current_dir(repo.path())
        .status()
        .expect("sh");
    assert!(in_place.success());
    // ... but it is not part of the work, so not of the clean copy; with
    // nothing else changed, the tree verified is the base's.
    let (receipt, _) = verified(repo.path());
    assert_eq!(receipt.verdict, Verdict::Fail);
    assert_eq!(receipt.diff, (0, 0, 0));
}

#[test]
fn an_added_skip_marker_needs_a_person() {
    let repo = fixture();
    write(repo.path(), "src/answer.txt", "42\n");
    write(
        repo.path(),
        "src/lib.rs",
        "#[test]\n#[ignore]\nfn slow() {}\n",
    );
    // `cat src/*` still finds the 42.
    let (receipt, _) = verified(repo.path());
    assert_eq!(receipt.verdict, Verdict::NeedsHuman);
    let finding = &receipt.findings[0];
    assert_eq!(
        (
            finding.kind,
            finding.path.as_str(),
            finding.line,
            finding.detail.as_str()
        ),
        (FindingKind::SkipMarker, "src/lib.rs", Some(2), "#[ignore")
    );
    assert_eq!(
        receipt.reasons,
        ["the work adds 1 skip marker outside the protected paths"]
    );
}

#[test]
fn without_a_contract_there_is_nothing_to_verify() {
    let repo = fixture();
    git(repo.path(), &["rm", "--quiet", ".memprove/verify.json"]).expect("rm");
    commit(repo.path(), "no contract");
    let error = verify(&Options::new(repo.path()).expect("options")).expect_err("no contract");
    assert!(
        error
            .to_string()
            .starts_with("no contract: neither the base commit nor the work has"),
        "{error}"
    );
    // A contract that is only in the work is used, and the receipt says so.
    write(repo.path(), ".memprove/verify.json", &contract("true"));
    let (receipt, _) = verified(repo.path());
    assert_eq!(receipt.verdict, Verdict::Pass);
    assert!(
        receipt
            .notes
            .iter()
            .any(|note| note.starts_with("the base commit has no contract"))
    );
}

fn event(name: &str, root: &Path) -> String {
    format!(
        r#"{{"session_id":"s-1","hook_event_name":"{name}","cwd":{}}}"#,
        memprove_eval::json::string(&root.to_string_lossy())
    )
}

fn stop(root: &Path) -> Option<memprove_eval::json::JsonValue> {
    hook::hook(&mut event("Stop", root).as_bytes(), root)
        .map(|output| memprove_eval::json::parse(&output, None).expect("one JSON object"))
}

#[test]
fn the_stop_hook_holds_failing_work_three_times_then_lets_go() {
    let repo = fixture();
    let root = repo.path();
    assert_eq!(
        hook::hook(&mut event("SessionStart", root).as_bytes(), root),
        None
    );
    assert!(
        stop(root).is_none(),
        "the tree is the base's: nothing to verify"
    );

    write(root, "src/answer.txt", "41\n");
    let runs = || {
        fs::read_dir(state_dir(root).expect("state").join("runs"))
            .expect("runs")
            .count()
    };
    for held in 1..=hook::MAX_BLOCKS {
        let output = stop(root).expect("held");
        assert_eq!(
            output.get("decision").and_then(|v| v.as_str()),
            Some("block")
        );
        let reason = output
            .get("reason")
            .and_then(|v| v.as_str())
            .expect("reason");
        assert!(reason.contains(&format!("stop {held} of 3")), "{reason}");
        assert!(reason.contains("memprove verify: FAIL"), "{reason}");
        assert!(reason.contains("`tests` (sh -c"), "{reason}");
        assert_eq!(runs(), 1, "the same tree is not verified again");
    }
    let output = stop(root).expect("told");
    assert!(output.get("decision").is_none(), "no longer held");
    let told = output
        .get("systemMessage")
        .and_then(|v| v.as_str())
        .expect("message");
    assert!(
        told.contains("still does not pass after 3 held stops"),
        "{told}"
    );
    assert!(stop(root).is_none(), "said once");

    // Fixed: verified again, passes, and the count starts over.
    write(root, "src/answer.txt", "42\n");
    let output = stop(root).expect("told");
    let told = output
        .get("systemMessage")
        .and_then(|v| v.as_str())
        .expect("message");
    assert!(told.starts_with("memprove verify: PASS"), "{told}");
    assert_eq!(runs(), 2);
    assert!(stop(root).is_none(), "a tree already verified says nothing");
    write(root, "src/answer.txt", "7\n");
    let output = stop(root).expect("held");
    assert!(
        output
            .get("reason")
            .and_then(|v| v.as_str())
            .is_some_and(|reason| reason.contains("stop 1 of 3"))
    );
}

#[test]
fn the_hook_compares_with_where_the_session_started() {
    let repo = fixture();
    let root = repo.path();
    hook::hook(&mut event("SessionStart", root).as_bytes(), root);
    // The agent weakens the test and commits: HEAD is its own commit now.
    write(root, "tests/answer.sh", "true\n");
    commit(root, "agent: make it pass");
    let output = stop(root).expect("told");
    assert!(
        output.get("decision").is_none(),
        "a person decides; the agent is not held"
    );
    let told = output
        .get("systemMessage")
        .and_then(|v| v.as_str())
        .expect("message");
    assert!(
        told.starts_with("memprove verify: NEEDS A PERSON"),
        "{told}"
    );
    assert!(told.contains("where the client session started"), "{told}");
    assert!(
        told.contains("protected file changed: tests/answer.sh"),
        "{told}"
    );

    // Without the hook's record the same work, committed, compares with
    // HEAD and shows nothing; the receipt says what the base was.
    let (receipt, _) = verified(root);
    assert_eq!(receipt.base.chosen_by, BaseSource::Head);
    assert!(
        receipt
            .notes
            .iter()
            .any(|note| note.starts_with("the base is HEAD"))
    );
}

#[test]
fn the_hook_says_nothing_without_a_contract_or_a_repository() {
    let repo = fixture();
    let root = repo.path();
    git(root, &["rm", "--quiet", ".memprove/verify.json"]).expect("rm");
    commit(root, "no contract");
    write(root, "src/answer.txt", "41\n");
    assert!(stop(root).is_none());
    let plain = tempfile::tempdir().expect("tempdir");
    assert!(stop(plain.path()).is_none());
}

#[test]
fn init_writes_a_contract_that_verifies_and_hooks_that_stay_put() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for args in [
        &["init", "--quiet", "-b", "main"][..],
        &["config", "user.email", "verify@example.invalid"],
        &["config", "user.name", "verify"],
        &["config", "commit.gpgsign", "false"],
    ] {
        git(root, args).expect("git");
    }
    let spec = init::InitSpec {
        repo_root: root.to_path_buf(),
        hook: true,
        executable: "/opt/memprove".to_owned(),
        dry_run: true,
    };
    let error = init::init(&spec).expect_err("nothing to guess from");
    assert!(
        error
            .to_string()
            .starts_with("no test command was recognised here")
    );

    write(root, "Makefile", "test:\n\ttest -f ok\n");
    write(root, "ok", "");
    commit(root, "a project");
    let report = init::init(&spec).expect("dry run");
    assert_eq!(report.written.len(), 2);
    assert!(!root.join(".memprove").exists() && !root.join(".claude").exists());

    let spec = init::InitSpec {
        dry_run: false,
        ..spec
    };
    let report = init::init(&spec).expect("init");
    assert_eq!(
        report.written,
        [
            PathBuf::from(".memprove/verify.json"),
            PathBuf::from(init::SETTINGS_FILE)
        ]
    );
    let settings = fs::read_to_string(root.join(init::SETTINGS_FILE)).expect("settings");
    assert_eq!(settings.matches("\"/opt/memprove\"").count(), 2);
    let again = init::init(&spec).expect("again");
    assert!(again.written.is_empty());
    assert_eq!(again.kept, [PathBuf::from(".memprove/verify.json")]);
    assert_eq!(again.unchanged, [PathBuf::from(init::SETTINGS_FILE)]);

    // The contract init wrote verifies this repository.
    let (receipt, _) = verified(root);
    assert_eq!(receipt.verdict, Verdict::Pass, "{:?}", receipt.reasons);
    assert_eq!(receipt.candidate.checks[0].argv, ["make", "test"]);
    assert!(run::earlier(root, "no-such-key").is_none());
}

#[test]
fn a_long_list_of_findings_is_cut_in_the_summary_not_in_the_receipt() {
    let repo = fixture();
    write(repo.path(), "src/answer.txt", "42\n");
    for index in 0..20 {
        write(repo.path(), &format!("tests/extra-{index:02}.sh"), "true\n");
    }
    // A marker in a document is not a skipped test.
    write(
        repo.path(),
        "docs/testing.md",
        "Mark a slow test with `#[ignore]`.\n",
    );
    let (receipt, run_dir) = verified(repo.path());
    assert_eq!(receipt.verdict, Verdict::Pass, "{:?}", receipt.reasons);
    assert_eq!(receipt.findings.len(), 20);
    let summary = receipt.summary(&run_dir);
    assert_eq!(summary.matches("\n  finding ").count(), 13, "{summary}");
    assert!(
        summary.contains("finding   and 8 more, listed in the receipt\n"),
        "{summary}"
    );
    assert_eq!(receipt.to_json().matches("\"protected_added\"").count(), 20);
}
