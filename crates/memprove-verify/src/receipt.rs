//! The receipt of one verification (`receipt.json`, schema
//! `memprove.verify.receipt.v1`) and the summary a person reads.

use std::{fmt::Write as _, path::Path};

use memprove_eval::{json::Out, process::Exit};

use crate::snapshot::Base;

/// The `schema` member of a receipt.
pub const RECEIPT_SCHEMA: &str = "memprove.verify.receipt.v1";

/// What a verification found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every check exits 0 on a clean copy of the work, and nothing leaves
    /// the verdict to a person.
    Pass,
    /// A check (or a setup command) does not exit 0 on a clean copy of the
    /// work.
    Fail,
    /// Every check exits 0, but only with the work's own changes to the
    /// protected paths, or the work adds a skip marker: a person decides.
    NeedsHuman,
}

impl Verdict {
    /// The name in a receipt.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NeedsHuman => "needs_human",
        }
    }

    /// The verdict a receipt names.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        [Self::Pass, Self::Fail, Self::NeedsHuman]
            .into_iter()
            .find(|verdict| verdict.name() == name)
    }

    /// The word a summary opens with.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::NeedsHuman => "NEEDS A PERSON",
        }
    }
}

/// Where the contract of a verification was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractSource {
    /// The base commit: the work cannot have changed it.
    Base,
    /// The work being verified: the base commit has no contract.
    Candidate,
    /// The file `--contract` names.
    File,
}

impl ContractSource {
    /// The name in a receipt.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Candidate => "candidate",
            Self::File => "file",
        }
    }
}

/// One setup or check command, as it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    /// `setup-<n>`, or the check's name.
    pub name: String,
    /// Program and arguments.
    pub argv: Vec<String>,
    /// How it ended; `None` when it could not be started.
    pub exit: Option<Exit>,
    /// Why it could not be started.
    pub error: Option<String>,
    /// Wall-clock time.
    pub duration_ms: u64,
    /// Its time limit.
    pub timeout_secs: u64,
    /// Its log files, relative to the run directory, without `.out` and
    /// `.err`.
    pub log: String,
}

impl CommandResult {
    /// Whether it exited with code 0.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.exit.is_some_and(Exit::success)
    }

    /// How it ended, as the end of a sentence.
    #[must_use]
    pub fn ended(&self) -> String {
        match self.exit {
            Some(Exit::Code(0)) => "passed".to_owned(),
            Some(Exit::Code(code)) => format!("exited with code {code}"),
            Some(Exit::Signal(signal)) => format!("was ended by signal {signal}"),
            Some(Exit::TimedOut) => format!("ran past its limit of {} s", self.timeout_secs),
            None => format!(
                "could not be started: {}",
                self.error.as_deref().unwrap_or("unknown")
            ),
        }
    }

    fn to_out(&self) -> Out {
        let (ended, code, signal) = match self.exit {
            Some(Exit::Code(code)) => ("exited", Some(code), None),
            Some(Exit::Signal(signal)) => ("signal", None, Some(signal)),
            Some(Exit::TimedOut) => ("timed_out", None, None),
            None => ("not_started", None, None),
        };
        Out::Object(vec![
            ("name", Out::str(&self.name)),
            ("argv", Out::strs(&self.argv)),
            ("ended", Out::str(ended)),
            (
                "exit_code",
                Out::opt(code, |code| Out::int(i64::from(code))),
            ),
            (
                "signal",
                Out::opt(signal, |signal| Out::int(i64::from(signal))),
            ),
            ("error", Out::opt(self.error.as_deref(), Out::str)),
            ("duration_ms", Out::uint(self.duration_ms)),
            ("timeout_secs", Out::uint(self.timeout_secs)),
            ("log", Out::str(&self.log)),
        ])
    }
}

/// The commands run on one tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    /// The tree whose files the commands ran on.
    pub tree: String,
    /// The setup commands that ran, in order; the first that fails is the
    /// last.
    pub setup: Vec<CommandResult>,
    /// The checks that ran; none when a setup command failed.
    pub checks: Vec<CommandResult>,
}

impl Stage {
    /// Whether every setup command and every check exited with code 0.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.setup.iter().all(CommandResult::passed)
            && !self.checks.is_empty()
            && self.checks.iter().all(CommandResult::passed)
    }

    /// One sentence for each command that did not exit with code 0.
    #[must_use]
    pub fn failures(&self) -> Vec<String> {
        let sentence = |kind: &str, result: &CommandResult| {
            format!(
                "{kind} `{}` ({}) {}",
                result.name,
                result.argv.join(" "),
                result.ended()
            )
        };
        self.setup
            .iter()
            .filter(|result| !result.passed())
            .map(|result| sentence("setup command", result))
            .chain(
                self.checks
                    .iter()
                    .filter(|result| !result.passed())
                    .map(|result| sentence("check", result)),
            )
            .collect()
    }

    /// The first command that did not exit with code 0.
    #[must_use]
    pub fn first_failure(&self) -> Option<&CommandResult> {
        self.setup
            .iter()
            .chain(&self.checks)
            .find(|result| !result.passed())
    }

    fn commands(&self) -> [(&'static str, Out); 2] {
        let list = |results: &[CommandResult]| {
            Out::Array(results.iter().map(CommandResult::to_out).collect())
        };
        [("setup", list(&self.setup)), ("checks", list(&self.checks))]
    }
}

/// What a finding is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// The work adds a file at a protected path.
    ProtectedAdded,
    /// The work changes a file at a protected path.
    ProtectedModified,
    /// The work deletes a file at a protected path.
    ProtectedDeleted,
    /// The work changes or deletes the contract; the base commit's was
    /// used.
    ContractChanged,
    /// The work adds a skip marker outside the protected paths.
    SkipMarker,
}

impl FindingKind {
    /// The name in a receipt.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ProtectedAdded => "protected_added",
            Self::ProtectedModified => "protected_modified",
            Self::ProtectedDeleted => "protected_deleted",
            Self::ContractChanged => "contract_changed",
            Self::SkipMarker => "skip_marker",
        }
    }

    const fn phrase(self) -> &'static str {
        match self {
            Self::ProtectedAdded => "protected file added",
            Self::ProtectedModified => "protected file changed",
            Self::ProtectedDeleted => "protected file deleted",
            Self::ContractChanged => "contract changed (the base commit's was used)",
            Self::SkipMarker => "skip marker added",
        }
    }
}

/// Something in the work a person may want to look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// What it is about.
    pub kind: FindingKind,
    /// The file, relative to the repository root.
    pub path: String,
    /// The line in the file, for a skip marker.
    pub line: Option<u64>,
    /// The skip marker; empty otherwise.
    pub detail: String,
}

impl Finding {
    fn sentence(&self) -> String {
        match self.line {
            Some(line) => format!(
                "{}: {}:{line} ({})",
                self.kind.phrase(),
                self.path,
                self.detail
            ),
            None => format!("{}: {}", self.kind.phrase(), self.path),
        }
    }
}

/// One verification: what was verified against what, what ran, and the
/// verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The run's name, also its directory's.
    pub run_id: String,
    /// When it started, UTC.
    pub started_at: String,
    /// When it ended, UTC.
    pub finished_at: String,
    /// The verdict.
    pub verdict: Verdict,
    /// Why the verdict is not `pass`; empty when it is.
    pub reasons: Vec<String>,
    /// What a reader should know that does not change the verdict.
    pub notes: Vec<String>,
    /// The worktree root.
    pub root: String,
    /// `HEAD` when the run started.
    pub head: String,
    /// The client session the hook ran in.
    pub session: Option<String>,
    /// Where the contract was read.
    pub contract_source: ContractSource,
    /// The contract file.
    pub contract_path: String,
    /// SHA-256 of the contract's text, hex.
    pub contract_sha256: String,
    /// The commit the work is compared with.
    pub base: Base,
    /// The commit verified (`--commit`); `None` for the worktree as it
    /// stood.
    pub commit: Option<String>,
    /// Files changed, lines added, lines removed, between the base and the
    /// tree verified.
    pub diff: (u64, u64, u64),
    /// The commands run on the tree verified.
    pub candidate: Stage,
    /// The commands run on that tree with every protected path as at the
    /// base; `None` when no protected path differs or the first run did
    /// not pass.
    pub restored: Option<Stage>,
    /// What a person may want to look at.
    pub findings: Vec<Finding>,
}

impl Receipt {
    /// The receipt as the text of `receipt.json`.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut candidate = vec![
            ("tree", Out::str(&self.candidate.tree)),
            ("commit", Out::opt(self.commit.as_deref(), Out::str)),
            ("files", Out::uint(self.diff.0)),
            ("insertions", Out::uint(self.diff.1)),
            ("deletions", Out::uint(self.diff.2)),
        ];
        candidate.extend(self.candidate.commands());
        let restored = Out::opt(self.restored.as_ref(), |stage| {
            let mut members = vec![("tree", Out::str(&stage.tree))];
            members.extend(stage.commands());
            Out::Object(members)
        });
        Out::Object(vec![
            ("schema", Out::str(RECEIPT_SCHEMA)),
            ("run_id", Out::str(&self.run_id)),
            ("started_at", Out::str(&self.started_at)),
            ("finished_at", Out::str(&self.finished_at)),
            ("verdict", Out::str(self.verdict.name())),
            ("reasons", Out::strs(&self.reasons)),
            ("notes", Out::strs(&self.notes)),
            (
                "repository",
                Out::Object(vec![
                    ("root", Out::str(&self.root)),
                    ("head", Out::str(&self.head)),
                ]),
            ),
            ("session", Out::opt(self.session.as_deref(), Out::str)),
            (
                "contract",
                Out::Object(vec![
                    ("source", Out::str(self.contract_source.name())),
                    ("path", Out::str(&self.contract_path)),
                    ("sha256", Out::str(&self.contract_sha256)),
                ]),
            ),
            (
                "base",
                Out::Object(vec![
                    ("commit", Out::str(&self.base.commit)),
                    ("chosen_by", Out::str(self.base.chosen_by.name())),
                ]),
            ),
            ("candidate", Out::Object(candidate)),
            ("restored", restored),
            (
                "findings",
                Out::Array(
                    self.findings
                        .iter()
                        .map(|finding| {
                            Out::Object(vec![
                                ("kind", Out::str(finding.kind.name())),
                                ("path", Out::str(&finding.path)),
                                ("line", Out::opt(finding.line, Out::uint)),
                                ("detail", Out::str(&finding.detail)),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
        .render()
    }

    /// What a person reads: the verdict, what was compared, each command
    /// and how it ended, the findings, and where the receipt is
    /// (`run_dir`).
    #[must_use]
    pub fn summary(&self, run_dir: &Path) -> String {
        let mut text = format!("memprove verify: {}\n", self.verdict.label());
        let mut row = |label: &str, value: &str| {
            writeln!(text, "  {label:<9} {value}").expect("writing to a String cannot fail");
        };
        for reason in &self.reasons {
            row("why", reason);
        }
        row(
            "base",
            &format!(
                "{} ({})",
                short(&self.base.commit),
                self.base.chosen_by.phrase()
            ),
        );
        let what = self.commit.as_deref().map_or_else(
            || "the working tree".to_owned(),
            |commit| format!("commit {}", short(commit)),
        );
        row(
            "verified",
            &format!(
                "{what}, tree {}: {} file{} changed, +{} -{}",
                short(&self.candidate.tree),
                self.diff.0,
                if self.diff.0 == 1 { "" } else { "s" },
                self.diff.1,
                self.diff.2
            ),
        );
        let contract = match self.contract_source {
            ContractSource::Base => format!("{} as at the base", self.contract_path),
            ContractSource::Candidate => {
                format!("{} as in the work verified", self.contract_path)
            }
            ContractSource::File => self.contract_path.clone(),
        };
        row("contract", &contract);
        let commands = |stage: &Stage| -> Vec<String> {
            stage
                .setup
                .iter()
                .chain(&stage.checks)
                .map(|result| {
                    format!(
                        "{}: {} in {} ({})",
                        result.name,
                        result.ended(),
                        seconds(result.duration_ms),
                        result.argv.join(" ")
                    )
                })
                .collect()
        };
        for line in commands(&self.candidate) {
            row("ran", &line);
        }
        if let Some(restored) = &self.restored {
            row("again", "with every protected path as at the base");
            for line in commands(restored) {
                row("ran", &line);
            }
        }
        for finding in self.findings.iter().take(SUMMARY_FINDINGS) {
            row("finding", &finding.sentence());
        }
        if let Some(more) = self.findings.len().checked_sub(SUMMARY_FINDINGS + 1) {
            row(
                "finding",
                &format!("and {} more, listed in the receipt", more + 1),
            );
        }
        for note in &self.notes {
            row("note", note);
        }
        row("receipt", &run_dir.join("receipt.json").to_string_lossy());
        text
    }

    /// A line in the format of libtest's `test result:` line, counting one
    /// case per check that ran, so that `memprove run --test-format cargo
    /// -- memprove verify run` records the verification as test Evidence.
    #[must_use]
    pub fn libtest_summary(&self) -> String {
        let checks = || {
            self.candidate
                .checks
                .iter()
                .chain(self.restored.iter().flat_map(|stage| &stage.checks))
        };
        let passed = checks().filter(|check| check.passed()).count();
        let failed = checks().count() - passed;
        let millis: u64 = checks().map(|check| check.duration_ms).sum();
        let word = if failed == 0 { "ok" } else { "FAILED" };
        format!(
            "test result: {word}. {passed} passed; {failed} failed; 0 ignored; 0 measured; 0 filtered out; finished in {}.00s",
            millis / 1000
        )
    }
}

/// The most findings a summary lists; the receipt lists every one.
const SUMMARY_FINDINGS: usize = 12;

/// The first 12 characters of an object name.
fn short(object: &str) -> &str {
    object.get(..12).unwrap_or(object)
}

/// `millis` as `41.2 s`.
fn seconds(millis: u64) -> String {
    format!("{}.{} s", millis / 1000, millis % 1000 / 100)
}
