//! P4-A (Owner): which facts `checkpoint create --reaffirm-all` reaffirms,
//! and on which runs. Pure: no IO, no clock, no repository. The CLI calls
//! it now; the MCP tool of a later slice reuses it.
//!
//! A fact awaits reaffirmation when it stands (`active`) and its verdict is
//! not `verified` ([`awaits_reaffirmation`]). Without Evidence named on the
//! command line, a fact is reaffirmed only on the latest run, in the current
//! repository state, of each command its Evidence ran, and only when that
//! run passed ([`match_reaffirmation`]): the same commands, never another
//! one. The daemon still checks every reaffirmation as before.
//!
//! P4-B (Owner, T18 SF-1): a run answers only for Evidence of its own kind,
//! a test run for a test run and a command run for a command run, and a
//! fact is reaffirmed only when it cites at least one test run, so the
//! runs chosen never make the daemon refuse the whole request for the kind
//! of its Evidence (a `verified_fact` needs a passed test run, ADR 0012
//! P2-E; the latest test run of an argv answers for its test runs, ADR
//! 0013 P3-L L2).

use crate::{ClaimKind, Standing, Verdict};

impl ClaimKind {
    /// Parses [`ClaimKind::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Fact, Self::OpenItem, Self::NextAction, Self::Decision]
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }
}

impl Standing {
    /// Parses [`Standing::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Active, Self::Superseded, Self::Withdrawn]
            .into_iter()
            .find(|standing| standing.as_str() == value)
    }
}

impl Verdict {
    /// Parses [`Verdict::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|verdict| verdict.as_str() == value)
    }
}

/// Whether a claim awaits reaffirmation: a fact that stands and is not
/// `verified`. A claim the same command already names (supersedes,
/// withdraws, or reaffirms) is left to that flag by the caller.
#[must_use]
pub fn awaits_reaffirmation(kind: ClaimKind, standing: Standing, verdict: Verdict) -> bool {
    kind == ClaimKind::Fact && standing == Standing::Active && verdict != Verdict::Verified
}

/// The kind of a run (P4-B): a test run (`test_result` Evidence) or a
/// command run (`command_result`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RunKind {
    /// A test run.
    Test,
    /// A command run that is not a test run.
    Command,
}

impl RunKind {
    /// The kind of Evidence of [`crate::EvidenceKind::as_str`] spelling
    /// `kind`, when it is a run.
    #[must_use]
    pub fn of_evidence(kind: &str) -> Option<Self> {
        match kind {
            "test_result" => Some(Self::Test),
            "command_result" => Some(Self::Command),
            _ => None,
        }
    }
}

/// One run of the current repository state (one Task, one Workspace, one
/// capture manifest), in the order the runs were recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRun<K, L> {
    /// How the caller names the run (a label, an Evidence ID).
    pub label: L,
    /// Its command: its argv, or a key equal exactly when the argv are
    /// equal element by element; `None` when it is not known.
    pub command: Option<K>,
    /// Its kind; `None` when it is not known (P4-B). A run of an unknown
    /// kind never answers for Evidence, but it is still a run of its
    /// command.
    pub kind: Option<RunKind>,
    /// Whether its result is `PASS`.
    pub passed: bool,
}

/// Why the runs at hand do not answer for the current repository state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateSkip {
    /// The capture manifest of the runs' Snapshot is not known.
    Unrecorded,
    /// The current repository state could not be observed.
    Unobserved,
    /// The current repository state differs from the runs' Snapshot: they
    /// are runs of an earlier state.
    Changed,
    /// The runs are of another Workspace than the one the Checkpoint is
    /// recorded for (P4-B, T18 N-c).
    OtherWorkspace,
}

/// Whether runs sealed in a Snapshot with capture manifest `runs_state`
/// are runs of the current repository state, observed as `current` (both
/// `capture_manifest_digest`s, ADR 0013 D3). `same_workspace` says whether
/// the runs are of the Workspace that `current` observes; when they are
/// not, that is the reason, whatever the manifests say (P4-B).
///
/// # Errors
///
/// Why they are not, or why that cannot be told.
pub fn runs_are_current(
    same_workspace: bool,
    runs_state: Option<&str>,
    current: Option<&str>,
) -> Result<(), StateSkip> {
    match (same_workspace, runs_state, current) {
        (false, _, _) => Err(StateSkip::OtherWorkspace),
        (true, None, _) => Err(StateSkip::Unrecorded),
        (true, _, None) => Err(StateSkip::Unobserved),
        (true, Some(runs_state), Some(current)) if runs_state == current => Ok(()),
        _ => Err(StateSkip::Changed),
    }
}

/// Why a fact is not reaffirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReaffirmSkip<K, L> {
    /// The runs at hand are not runs of the current repository state.
    State(StateSkip),
    /// The fact cites no Evidence, so there is no command to rerun.
    NoEvidence,
    /// The cited Evidence at this 0-based position has no command (it is
    /// not a command or test run, or it cannot be read).
    NotACommand {
        /// Position among the fact's Evidence.
        evidence: usize,
    },
    /// No run of the state has this command.
    NotRun {
        /// The command.
        command: K,
    },
    /// The latest run of the state with this command did not pass.
    NotPassed {
        /// The command.
        command: K,
        /// Its latest run.
        latest: L,
    },
    /// P4-B (T18 SF-1): the state has runs of this command, but none of
    /// the kind of the cited Evidence (a test run cited, only command runs
    /// since, or the other way round).
    NoRunOfKind {
        /// The command.
        command: K,
        /// The kind of the cited Evidence.
        kind: RunKind,
    },
    /// P4-B: the latest run of the state with this command and of the kind
    /// of the cited Evidence did not pass (ADR 0013 P3-L L2 for tests).
    KindNotPassed {
        /// The command.
        command: K,
        /// The kind of the cited Evidence.
        kind: RunKind,
        /// Its latest run of that kind.
        latest: L,
    },
    /// P4-B: none of the cited Evidence is a test run, so no reaffirmation
    /// on the same kinds could hold the passed test run a `verified_fact`
    /// needs.
    NoTest,
}

/// For a fact whose cited Evidence ran `cited` (each Evidence's command and
/// kind, in citation order), the runs to reaffirm it on: for each distinct
/// command and kind, in first-cited order, the latest run of `runs` with
/// that command and kind, when it passed and the latest run of that
/// command of any kind passed too (ADR 0013 P3-D3: that run answers for
/// the command in its state). Otherwise every reason it is skipped, in
/// citation order, and [`ReaffirmSkip::NoTest`] last when no cited Evidence
/// is a test run. A run whose command or kind is not known never matches.
///
/// # Errors
///
/// The reasons, never empty, when the fact is skipped.
pub fn match_reaffirmation<K: PartialEq + Clone, L: Clone>(
    cited: &[Option<(K, RunKind)>],
    runs: &[StateRun<K, L>],
) -> Result<Vec<L>, Vec<ReaffirmSkip<K, L>>> {
    if cited.is_empty() {
        return Err(vec![ReaffirmSkip::NoEvidence]);
    }
    let mut commands: Vec<(&K, RunKind)> = Vec::new();
    let mut skipped = Vec::new();
    for (evidence, command) in cited.iter().enumerate() {
        match command {
            None => skipped.push(ReaffirmSkip::NotACommand { evidence }),
            Some((command, kind)) if !commands.contains(&(command, *kind)) => {
                commands.push((command, *kind));
            }
            Some(_) => {}
        }
    }
    let mut chosen = Vec::new();
    for (command, kind) in &commands {
        let of_command = |run: &&StateRun<K, L>| run.command.as_ref() == Some(*command);
        let latest = runs.iter().rfind(of_command);
        let latest_of_kind = runs
            .iter()
            .filter(of_command)
            .rfind(|run| run.kind == Some(*kind));
        match (latest, latest_of_kind) {
            (None, _) => skipped.push(ReaffirmSkip::NotRun {
                command: (*command).clone(),
            }),
            (Some(run), _) if !run.passed => skipped.push(ReaffirmSkip::NotPassed {
                command: (*command).clone(),
                latest: run.label.clone(),
            }),
            (Some(_), None) => skipped.push(ReaffirmSkip::NoRunOfKind {
                command: (*command).clone(),
                kind: *kind,
            }),
            (Some(_), Some(run)) if !run.passed => skipped.push(ReaffirmSkip::KindNotPassed {
                command: (*command).clone(),
                kind: *kind,
                latest: run.label.clone(),
            }),
            (Some(_), Some(run)) => chosen.push(run.label.clone()),
        }
    }
    if !commands.is_empty() && commands.iter().all(|(_, kind)| *kind != RunKind::Test) {
        skipped.push(ReaffirmSkip::NoTest);
    }
    if skipped.is_empty() {
        Ok(chosen)
    } else {
        Err(skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: RunKind = RunKind::Test;
    const C: RunKind = RunKind::Command;

    fn run(
        label: &'static str,
        command: Option<&'static str>,
        passed: bool,
    ) -> StateRun<&'static str, &'static str> {
        kinded(label, command, Some(T), passed)
    }

    fn kinded(
        label: &'static str,
        command: Option<&'static str>,
        kind: Option<RunKind>,
        passed: bool,
    ) -> StateRun<&'static str, &'static str> {
        StateRun {
            label,
            command,
            kind,
            passed,
        }
    }

    fn test(command: &'static str) -> (&'static str, RunKind) {
        (command, T)
    }

    #[test]
    fn only_an_active_fact_that_is_not_verified_awaits() {
        for kind in [
            ClaimKind::Fact,
            ClaimKind::OpenItem,
            ClaimKind::NextAction,
            ClaimKind::Decision,
        ] {
            for standing in [Standing::Active, Standing::Superseded, Standing::Withdrawn] {
                for verdict in Verdict::ALL {
                    assert_eq!(
                        awaits_reaffirmation(kind, standing, verdict),
                        kind == ClaimKind::Fact
                            && standing == Standing::Active
                            && verdict != Verdict::Verified,
                        "{kind:?} {standing:?} {verdict:?}"
                    );
                }
            }
            assert_eq!(ClaimKind::parse(kind.as_str()), Some(kind));
        }
        for standing in [Standing::Active, Standing::Superseded, Standing::Withdrawn] {
            assert_eq!(Standing::parse(standing.as_str()), Some(standing));
        }
        for verdict in Verdict::ALL {
            assert_eq!(Verdict::parse(verdict.as_str()), Some(verdict));
        }
        assert_eq!(ClaimKind::parse("Fact"), None);
        assert_eq!(Standing::parse(""), None);
        assert_eq!(Verdict::parse("unknown"), None);
        assert_eq!(RunKind::of_evidence("test_result"), Some(T));
        assert_eq!(RunKind::of_evidence("command_result"), Some(C));
        assert_eq!(RunKind::of_evidence("git_head"), None);
    }

    #[test]
    fn only_runs_of_the_current_state_answer() {
        assert_eq!(runs_are_current(true, Some("m"), Some("m")), Ok(()));
        assert_eq!(
            runs_are_current(true, Some("m"), Some("n")),
            Err(StateSkip::Changed)
        );
        assert_eq!(
            runs_are_current(true, None, Some("m")),
            Err(StateSkip::Unrecorded)
        );
        assert_eq!(
            runs_are_current(true, None, None),
            Err(StateSkip::Unrecorded)
        );
        assert_eq!(
            runs_are_current(true, Some("m"), None),
            Err(StateSkip::Unobserved)
        );
        // P4-B (T18 N-c): runs of another Workspace say so first.
        for (runs_state, current) in [(Some("m"), Some("m")), (Some("m"), Some("n")), (None, None)]
        {
            assert_eq!(
                runs_are_current(false, runs_state, current),
                Err(StateSkip::OtherWorkspace)
            );
        }
    }

    #[test]
    fn the_latest_run_of_each_command_must_pass() {
        let runs = [
            run("r1", Some("test"), true),
            run("r2", Some("lint"), true),
            run("r3", Some("test"), true),
            run("r4", None, true),
            run("r5", Some("build"), true),
            run("r6", Some("build"), false),
        ];
        assert_eq!(
            match_reaffirmation(&[Some(test("test"))], &runs),
            Ok(vec!["r3"])
        );
        assert_eq!(
            match_reaffirmation(
                &[Some(test("lint")), Some(test("test")), Some(test("lint"))],
                &runs
            ),
            Ok(vec!["r2", "r3"])
        );
        assert_eq!(
            match_reaffirmation(&[Some(test("build"))], &runs),
            Err(vec![ReaffirmSkip::NotPassed {
                command: "build",
                latest: "r6"
            }])
        );
        assert_eq!(
            match_reaffirmation(&[Some(test("test")), Some(test("doc"))], &runs),
            Err(vec![ReaffirmSkip::NotRun { command: "doc" }])
        );
        assert_eq!(
            match_reaffirmation(&[None, Some(test("doc")), Some(test("build"))], &runs),
            Err(vec![
                ReaffirmSkip::NotACommand { evidence: 0 },
                ReaffirmSkip::NotRun { command: "doc" },
                ReaffirmSkip::NotPassed {
                    command: "build",
                    latest: "r6"
                },
            ])
        );
        assert_eq!(
            match_reaffirmation::<&str, &str>(&[], &runs),
            Err(vec![ReaffirmSkip::NoEvidence])
        );
        // A later pass restores the command.
        let restored = [
            run("r1", Some("build"), false),
            run("r2", Some("build"), true),
        ];
        assert_eq!(
            match_reaffirmation(&[Some(test("build"))], &restored),
            Ok(vec!["r2"])
        );
    }

    /// P4-B (T18 SF-1): a run answers only for Evidence of its own kind,
    /// and a reaffirmation needs a test run.
    #[test]
    fn a_run_answers_only_for_evidence_of_its_kind() {
        // A test run, then a passed command run of the same argv: the test
        // run is chosen (the daemon's rules hold: the latest run passed,
        // and so did the latest test run).
        let runs = [
            kinded("r1", Some("a"), Some(T), true),
            kinded("r2", Some("a"), Some(C), true),
        ];
        assert_eq!(
            match_reaffirmation(&[Some(test("a"))], &runs),
            Ok(vec!["r1"])
        );
        // Only a command run: the test Evidence has no run of its kind.
        let commands = [kinded("r1", Some("a"), Some(C), true)];
        assert_eq!(
            match_reaffirmation(&[Some(test("a"))], &commands),
            Err(vec![ReaffirmSkip::NoRunOfKind {
                command: "a",
                kind: T
            }])
        );
        // A run of unknown kind (an older context) answers for nothing.
        let unknown = [kinded("r1", Some("a"), None, true)];
        assert_eq!(
            match_reaffirmation(&[Some(test("a"))], &unknown),
            Err(vec![ReaffirmSkip::NoRunOfKind {
                command: "a",
                kind: T
            }])
        );
        // Test PASS, test FAIL, command PASS (T18 TC): the latest test run
        // did not pass.
        let failed = [
            kinded("r1", Some("a"), Some(T), true),
            kinded("r2", Some("a"), Some(T), false),
            kinded("r3", Some("a"), Some(C), true),
        ];
        assert_eq!(
            match_reaffirmation(&[Some(test("a"))], &failed),
            Err(vec![ReaffirmSkip::KindNotPassed {
                command: "a",
                kind: T,
                latest: "r2"
            }])
        );
        // Test PASS, command FAIL: the latest run of the argv did not pass.
        let overruled = [
            kinded("r1", Some("a"), Some(T), true),
            kinded("r2", Some("a"), Some(C), false),
        ];
        assert_eq!(
            match_reaffirmation(&[Some(test("a"))], &overruled),
            Err(vec![ReaffirmSkip::NotPassed {
                command: "a",
                latest: "r2"
            }])
        );
        // A test and a command of one argv: one run of each kind.
        let both = [
            kinded("r1", Some("a"), Some(C), true),
            kinded("r2", Some("a"), Some(T), true),
        ];
        assert_eq!(
            match_reaffirmation(&[Some(test("a")), Some(("a", C))], &both),
            Ok(vec!["r2", "r1"])
        );
        // Evidence without a test run: never reaffirmed.
        assert_eq!(
            match_reaffirmation(&[Some(("a", C))], &both),
            Err(vec![ReaffirmSkip::NoTest])
        );
    }

    /// Property: whatever the runs, a reaffirmation cites one run per
    /// distinct command and kind of the fact's Evidence, each with that
    /// command and kind, the latest such run, a passed one, and the latest
    /// run of its command of any kind passed; at least one is a test run;
    /// nothing else.
    #[test]
    fn a_reaffirmation_cites_exactly_the_commands_of_the_evidence() {
        let commands = ["a", "b", "c"];
        // Every sequence of up to four runs over three commands, unknown
        // included, of both kinds or none, each passed or not.
        let mut sequences: Vec<Vec<StateRun<&str, usize>>> = vec![Vec::new()];
        for _ in 0..4 {
            let mut longer = Vec::new();
            for sequence in &sequences {
                for command in [None, Some("a"), Some("b"), Some("c")] {
                    for kind in [None, Some(T), Some(C)] {
                        for passed in [false, true] {
                            let mut next = sequence.clone();
                            next.push(StateRun {
                                label: sequence.len(),
                                command,
                                kind,
                                passed,
                            });
                            longer.push(next);
                        }
                    }
                }
            }
            sequences.extend(longer);
        }
        let cited_sets: Vec<Vec<Option<(&str, RunKind)>>> = vec![
            vec![Some(test("a"))],
            vec![Some(test("a")), Some(("b", C))],
            vec![Some(test("b")), Some(test("a")), Some(test("b"))],
            vec![Some(test("a")), Some(("a", C)), Some(test("c"))],
            vec![Some(("c", C))],
            vec![Some(test("c")), None],
        ];
        let mut reaffirmed = 0;
        for runs in &sequences {
            for cited in &cited_sets {
                let Ok(chosen) = match_reaffirmation(cited, runs) else {
                    continue;
                };
                reaffirmed += 1;
                let mut wanted: Vec<(&str, RunKind)> = Vec::new();
                for item in cited.iter().flatten() {
                    if !wanted.contains(item) {
                        wanted.push(*item);
                    }
                }
                assert!(cited.iter().all(Option::is_some));
                assert!(wanted.iter().any(|(_, kind)| *kind == T));
                let got = chosen
                    .iter()
                    .map(|label| (runs[*label].command.unwrap(), runs[*label].kind.unwrap()))
                    .collect::<Vec<_>>();
                assert_eq!(got, wanted, "{cited:?} {runs:?}");
                for label in &chosen {
                    let run = &runs[*label];
                    assert!(run.passed);
                    let later = runs[label + 1..]
                        .iter()
                        .filter(|later| later.command == run.command)
                        .collect::<Vec<_>>();
                    assert!(later.iter().all(|later| later.kind != run.kind));
                    assert!(later.last().is_none_or(|latest| latest.passed));
                }
            }
            for command in commands {
                let latest = runs.iter().rfind(|run| run.command == Some(command));
                let latest_test = runs
                    .iter()
                    .rfind(|run| run.command == Some(command) && run.kind == Some(T));
                let result = match_reaffirmation(&[Some(test(command))], runs);
                assert_eq!(
                    result.is_ok(),
                    latest.is_some_and(|run| run.passed)
                        && latest_test.is_some_and(|run| run.passed)
                );
            }
        }
        assert!(reaffirmed > 1000, "{reaffirmed}");
    }
}
