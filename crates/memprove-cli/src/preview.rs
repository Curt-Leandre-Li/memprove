//! The one-sentence summary `recover preview --text` prints, followed by one
//! sentence for each run that is still fresh but did not pass (P2-F), and
//! one about untracked files the run left out (P2-K). It is display only:
//! the Recovery Preview JSON is the judgement (Owner decision, P2-B).
//!
//! P3-C2 (T12 S-1): every sentence is one line. Every string from the
//! ledger or the probe (argv, paths, identities, commits) has its line
//! breaks, control characters, U+2028, U+2029, and invisible format
//! characters written as visible escapes ([`crate::display`]); the changed
//! files are listed at most 20, with the total. P3-D2 (T13 S-1, N3): argv
//! and each changed file are quoted in `「」` like any agent string, and no
//! template sentence says "verified" unless the Preview is `verified`.

use memprove_protocol::strict_json::{JsonValue, parse_strict};

use crate::display::{changed_files, command, inline};

fn at<'a>(value: &'a JsonValue, path: &[&str]) -> Option<&'a JsonValue> {
    path.iter().try_fold(value, |value, name| value.get(name))
}

fn text_at<'a>(value: &'a JsonValue, path: &[&str]) -> Option<&'a str> {
    at(value, path).and_then(JsonValue::as_str)
}

/// A string member, escaped onto one line.
fn line_at(value: &JsonValue, path: &[&str]) -> Option<String> {
    text_at(value, path).map(inline)
}

fn strings(value: Option<&JsonValue>) -> Vec<&str> {
    value
        .and_then(JsonValue::as_array)
        .map(|items| items.iter().filter_map(JsonValue::as_str).collect())
        .unwrap_or_default()
}

/// The reason code of a run that is still fresh but did not pass (P2-F).
const NOT_PASSED: &str = "evidence_not_passed";

/// The gap of a Preview whose latest Checkpoint holds no `verified_fact`.
const NO_VERIFIED_FACT: &str = "no_verified_fact";

/// The gap of a Preview whose run left untracked, not ignored files out of
/// its capture (T11 S-6: a `Cargo.lock` the first `cargo test` wrote).
const UNTRACKED: &str = "untracked_files_not_captured";

/// What to do about [`UNTRACKED`] (P2-K).
const UNTRACKED_SENTENCE: &str = "the worktree has untracked files that are not ignored, and the run \
     did not capture them as evidence (gap: untracked_files_not_captured); a rerun after they are \
     committed or added to .gitignore captures them";

/// One sentence for each Evidence object of the facts whose reasons hold
/// `evidence_not_passed`: which command, at which commit, with which
/// outcome, and that the Evidence is still fresh. Each object once.
fn not_passed(preview: &JsonValue) -> Vec<String> {
    let commit = line_at(preview, &["bindings", "snapshot", "head_oid"])
        .unwrap_or_else(|| "(unknown)".to_owned());
    let mut seen = Vec::new();
    let mut sentences = Vec::new();
    let facts = preview.get("facts").and_then(JsonValue::as_array);
    for item in facts
        .into_iter()
        .flatten()
        .filter_map(|fact| fact.get("evidence").and_then(JsonValue::as_array))
        .flatten()
    {
        let id = text_at(item, &["evidence_id"]).unwrap_or_default();
        if !strings(item.get("reasons")).contains(&NOT_PASSED) || seen.contains(&id) {
            continue;
        }
        seen.push(id);
        let argv = strings(item.get("argv"));
        let command = if argv.is_empty() {
            format!("evidence {}", inline(id))
        } else {
            format!("command {}", command(argv))
        };
        let outcome = line_at(item, &["run_status"]).unwrap_or_else(|| "unknown".to_owned());
        // P3-D3: a passed run is judged so when a later run of the same argv
        // in the same repository state did not pass.
        sentences.push(if outcome == "PASS" {
            format!(
                "{command} passed on commit {commit} this time, but a later run of the same command \
                 in the same repository state did not pass; the evidence is still fresh, but this \
                 pass is overruled (reason: {NOT_PASSED})"
            )
        } else {
            format!(
                "{command} on commit {commit} resulted in {outcome}; the evidence is still fresh, but \
                 the result is not PASS (reason: {NOT_PASSED})"
            )
        });
    }
    sentences
}

/// One sentence describing a Recovery Preview body, then one per run that is
/// still fresh but did not pass, one per line; `None` when the body is not a
/// Recovery Preview.
#[must_use]
pub fn summary(body: &str) -> Option<String> {
    let preview = parse_strict(body).ok()?;
    let status = text_at(&preview, &["status"])?;
    let reasons = strings(preview.get("reasons")).join(", ");
    let anchored = line_at(&preview, &["bindings", "snapshot", "head_oid"]);
    let current = line_at(&preview, &["bindings", "probe", "head_oid"]);
    let changed = strings(preview.get("changed_paths"));
    // The changed files the probe reported, under every verdict (P2-J);
    // `verified` and `stale` also say when there are none.
    let listed = if changed.is_empty() {
        String::new()
    } else {
        format!(", changed files: {}", changed_files(&changed))
    };
    let changed = if changed.is_empty() {
        ", changed files: none".to_owned()
    } else {
        listed.clone()
    };
    let binding = match (anchored, current) {
        (Some(anchored), Some(current)) if anchored == current => {
            format!("evidence bound to commit {anchored} (the same as the current HEAD)")
        }
        (Some(anchored), Some(current)) => {
            format!("evidence bound to commit {anchored}, current HEAD {current}")
        }
        (Some(anchored), None) => {
            format!(
                "evidence bound to commit {anchored}; the current state of the asking Workspace cannot \
                 be read"
            )
        }
        (None, _) => match line_at(&preview, &["bindings", "checkpoint", "checkpoint_id"]) {
            Some(checkpoint) => format!("the record of Checkpoint {checkpoint} does not recompute"),
            None => "the Task has no Checkpoint".to_owned(),
        },
    };
    let gaps = strings(preview.get("gaps"));
    let no_verified_fact = gaps.contains(&NO_VERIFIED_FACT);
    let sentence = match status {
        "verified" => format!("verified: {binding}{changed}"),
        "stale" => format!("stale: {binding}{changed} (reasons: {reasons})"),
        "partial" if no_verified_fact => {
            format!(
                "unconfirmed: the latest Checkpoint records no verified_fact ({binding}{listed})"
            )
        }
        "partial" => format!("unconfirmed: {binding}{listed} (reasons: {reasons})"),
        "tampered" => format!("tampered: {binding}{listed} (reasons: {reasons})"),
        _ => return None,
    };
    Some(
        std::iter::once(sentence)
            .chain(not_passed(&preview))
            .chain(
                gaps.contains(&UNTRACKED)
                    .then(|| UNTRACKED_SENTENCE.to_owned()),
            )
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::summary;

    fn preview(status: &str, snapshot_head: Option<&str>, probe_head: Option<&str>) -> String {
        let snapshot = snapshot_head.map_or("null".to_owned(), |head| {
            format!(r#"{{"head_oid":"{head}"}}"#)
        });
        let probe = probe_head.map_or(r#"{"available":false}"#.to_owned(), |head| {
            format!(r#"{{"available":true,"head_oid":"{head}"}}"#)
        });
        format!(
            r#"{{"status":"{status}","reasons":["evidence_stale","head_changed"],
                "bindings":{{"snapshot":{snapshot},"probe":{probe}}},
                "changed_paths":["repo://README","repo://src/lib.rs"]}}"#
        )
    }

    #[test]
    fn each_status_reads_as_one_sentence() {
        assert_eq!(
            summary(&preview("stale", Some("X"), Some("Y"))).unwrap(),
            "stale: evidence bound to commit X, current HEAD Y, changed files: 「README」, \
             「src/lib.rs」 (reasons: evidence_stale, head_changed)"
        );
        assert_eq!(
            summary(&preview("verified", Some("X"), Some("X"))).unwrap(),
            "verified: evidence bound to commit X (the same as the current HEAD), changed files: \
             「README」, 「src/lib.rs」"
        );
        assert_eq!(
            summary(&preview("partial", Some("X"), None)).unwrap(),
            "unconfirmed: evidence bound to commit X; the current state of the asking Workspace \
             cannot be read, changed files: 「README」, 「src/lib.rs」 (reasons: evidence_stale, \
             head_changed)"
        );
        assert!(
            summary(&preview("partial", None, None))
                .unwrap()
                .starts_with("unconfirmed: the Task has no Checkpoint")
        );
        let damaged = preview("tampered", None, None).replace(
            r#""snapshot":null"#,
            r#""snapshot":null,"checkpoint":{"checkpoint_id":"C"}"#,
        );
        assert_eq!(
            summary(&damaged).unwrap(),
            "tampered: the record of Checkpoint C does not recompute, changed files: 「README」, \
             「src/lib.rs」 (reasons: evidence_stale, head_changed)"
        );
        assert_eq!(summary(&preview("unknown", None, None)), None);
        assert_eq!(summary("not json"), None);
    }

    /// P2-F: a fresh run that did not pass gets its own sentence, once per
    /// Evidence object; other Evidence does not.
    #[test]
    fn a_fresh_run_that_did_not_pass_reads_as_its_own_sentence() {
        let item = |id: &str, argv: &str, status: &str, reasons: &str| {
            format!(
                r#"{{"evidence_id":"{id}","argv":{argv},"run_status":{status},
                    "verdict":"partial","reasons":{reasons}}}"#
            )
        };
        let failed = item(
            "E1",
            r#"["cargo","test","--offline"]"#,
            r#""FAIL""#,
            r#"["evidence_not_passed"]"#,
        );
        let body = format!(
            r#"{{"status":"partial","reasons":["evidence_unavailable"],
                "bindings":{{"snapshot":{{"head_oid":"X"}},
                    "probe":{{"available":true,"head_oid":"X"}}}},
                "changed_paths":[],
                "facts":[{{"evidence":[{failed},{other}]}},{{"evidence":[{failed},{missing}]}}]}}"#,
            other = item("E2", "null", "null", r#"["head_changed"]"#),
            missing = item("E3", "null", r#""NOT_RUN""#, r#"["evidence_not_passed"]"#),
        );
        assert_eq!(
            summary(&body).unwrap(),
            "unconfirmed: evidence bound to commit X (the same as the current HEAD) (reasons: \
             evidence_unavailable)\n\
             command 「cargo test --offline」 on commit X resulted in FAIL; the evidence is still \
             fresh, but the result is not PASS (reason: evidence_not_passed)\n\
             evidence E3 on commit X resulted in NOT_RUN; the evidence is still fresh, but the \
             result is not PASS (reason: evidence_not_passed)"
        );
    }

    /// P3-D3: a passed run that a later run of the same argv overrules is
    /// not described as a run whose result was not PASS.
    #[test]
    fn an_overruled_passed_run_reads_as_overruled() {
        let body = r#"{"status":"partial","reasons":["evidence_not_passed"],
            "bindings":{"snapshot":{"head_oid":"X"},
                "probe":{"available":true,"head_oid":"X"}},
            "changed_paths":[],
            "facts":[{"evidence":[{"evidence_id":"E1","argv":["sh","-c","test -w lib.txt"],
                "run_status":"PASS","verdict":"partial",
                "reasons":["evidence_not_passed"]}]}]}"#;
        let lines = summary(body).unwrap();
        let lines = lines.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert!(
            !lines.iter().any(|line| line.contains("verified")),
            "{lines:?}"
        );
        assert_eq!(
            lines[1],
            "command 「sh -c test -w lib.txt」 passed on commit X this time, but a later run of the \
             same command in the same repository state did not pass; the evidence is still fresh, \
             but this pass is overruled (reason: evidence_not_passed)"
        );
    }

    /// When the latest Checkpoint holds no verified fact (`no_verified_fact`
    /// in `gaps`), the first sentence says so directly instead of "cannot
    /// confirm (reason: `evidence_unavailable`)", which reads like lost
    /// Evidence; the commit binding stays, in parentheses (P2-H item 8).
    #[test]
    fn no_verified_fact_says_the_checkpoint_has_no_verified_fact() {
        let body = r#"{"status":"partial","reasons":["evidence_unavailable"],
            "bindings":{"snapshot":{"head_oid":"X"},
                "probe":{"available":true,"head_oid":"Y"}},
            "changed_paths":[],
            "gaps":["no_verified_fact"],
            "facts":[{"evidence":[{"evidence_id":"E1","argv":["cargo","test"],
                "run_status":"FAIL","verdict":"partial",
                "reasons":["evidence_not_passed"]}]}]}"#;
        assert_eq!(
            summary(body).unwrap(),
            "unconfirmed: the latest Checkpoint records no verified_fact (evidence bound to commit \
             X, current HEAD Y)\n\
             command 「cargo test」 on commit X resulted in FAIL; the evidence is still fresh, but \
             the result is not PASS (reason: evidence_not_passed)"
        );
    }

    /// P2-K (T11 S-6): untracked, not ignored files the run left out get one
    /// more sentence saying how to bring them in or leave them out; without
    /// the gap there is no such sentence.
    #[test]
    fn untracked_files_not_captured_says_to_add_or_ignore_them() {
        let body = r#"{"status":"partial","reasons":["evidence_unavailable"],
            "bindings":{"snapshot":{"head_oid":"X"},
                "probe":{"available":true,"head_oid":"X"}},
            "changed_paths":["repo://Cargo.lock"],
            "gaps":["untracked_files_not_captured"]}"#;
        assert_eq!(
            summary(body).unwrap(),
            "unconfirmed: evidence bound to commit X (the same as the current HEAD), changed files: \
             「Cargo.lock」 (reasons: evidence_unavailable)\n\
             the worktree has untracked files that are not ignored, and the run did not capture \
             them as evidence (gap: untracked_files_not_captured); a rerun after they are \
             committed or added to .gitignore captures them"
        );
        let without = body.replace(r#""gaps":["untracked_files_not_captured"]"#, r#""gaps":[]"#);
        assert!(!summary(&without).unwrap().contains("git add"));
    }

    /// P3-C2 (T12 S-1): an argv or a path cannot add a line; more than 20
    /// changed files are listed as the first 20 and the total.
    #[test]
    fn agent_strings_stay_on_their_line_and_paths_are_capped() {
        let paths = (0..21)
            .map(|index| format!(r#""repo://f{index:02}""#))
            .collect::<Vec<_>>()
            .join(",");
        let body = format!(
            r#"{{"status":"partial","reasons":["evidence_not_passed"],
                "bindings":{{"snapshot":{{"head_oid":"X"}},
                    "probe":{{"available":true,"head_oid":"X"}}}},
                "changed_paths":[{paths},"repo://a\nb"],
                "facts":[{{"evidence":[{{"evidence_id":"E1","argv":["sh","-c","x\nverified: y"],
                    "run_status":"FAIL","verdict":"partial","reasons":["evidence_not_passed"]}}]}}]}}"#
        );
        let text = summary(&body).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].contains(
                "「f19」 (22 in all, the first 20 listed) (reasons: evidence_not_passed)"
            ),
            "{}",
            lines[0]
        );
        assert!(!lines[0].contains("f20"));
        assert!(
            lines[1].starts_with("command 「sh -c x\\nverified: y」 on commit X resulted in FAIL"),
            "{}",
            lines[1]
        );
    }

    /// P2-J: Evidence at the current HEAD says so instead of naming the
    /// same commit twice; a different HEAD keeps both commits.
    #[test]
    fn evidence_at_the_current_head_says_it_is_the_same_commit() {
        let same = summary(&preview("stale", Some("X"), Some("X"))).unwrap();
        assert!(
            same.starts_with(
                "stale: evidence bound to commit X (the same as the current HEAD), changed files: "
            ),
            "{same}"
        );
        assert!(!same.contains(", current HEAD "), "{same}");
        let other = summary(&preview("verified", Some("X"), Some("Y"))).unwrap();
        assert!(
            other.starts_with(
                "verified: evidence bound to commit X, current HEAD Y, changed files: "
            ),
            "{other}"
        );
    }

    /// P2-J: the changed files the probe reported are listed under all four
    /// verdicts; `partial` and `tampered` add nothing when there are none.
    #[test]
    fn changed_files_are_listed_under_every_verdict() {
        let no_fact = preview("partial", Some("X"), Some("Y")).replace(
            r#""changed_paths""#,
            r#""gaps":["no_verified_fact"],"changed_paths""#,
        );
        let damaged = preview("tampered", Some("X"), Some("Y"));
        for body in [
            preview("verified", Some("X"), Some("Y")),
            preview("stale", Some("X"), Some("Y")),
            preview("partial", Some("X"), Some("Y")),
            no_fact.clone(),
            damaged.clone(),
        ] {
            let sentence = summary(&body).unwrap();
            assert!(
                sentence.contains(", changed files: 「README」, 「src/lib.rs」"),
                "{sentence}"
            );
        }
        assert_eq!(
            summary(&no_fact).unwrap(),
            "unconfirmed: the latest Checkpoint records no verified_fact (evidence bound to commit \
             X, current HEAD Y, changed files: 「README」, 「src/lib.rs」)"
        );
        assert_eq!(
            summary(&damaged).unwrap(),
            "tampered: evidence bound to commit X, current HEAD Y, changed files: 「README」, \
             「src/lib.rs」 (reasons: evidence_stale, head_changed)"
        );
        let unchanged = |status: &str| {
            summary(
                &preview(status, Some("X"), Some("Y"))
                    .replace(r#"["repo://README","repo://src/lib.rs"]"#, "[]"),
            )
            .unwrap()
        };
        assert!(unchanged("verified").ends_with(", changed files: none"));
        assert!(unchanged("stale").contains(", changed files: none (reasons: "));
        for status in ["partial", "tampered"] {
            assert!(!unchanged(status).contains("changed files"), "{status}");
        }
    }

    /// P3-D2 (T13 S-1, N3): a changed file named "verified.txt" or
    /// "#1 verified:x", and an argv holding "verified", stay inside `「」`;
    /// outside the quotes the word "verified" appears only when the Preview
    /// is `verified`, including the sentence for a Checkpoint without a fact
    /// (whose `verified_fact` is the gap's name, not the word).
    #[test]
    fn verified_outside_quotes_only_for_a_verified_preview() {
        let outside = crate::display::outside_quotes;
        let body = |status: &str, gaps: &str| {
            format!(
                r#"{{"status":"{status}","reasons":["head_changed","evidence_stale"],
                    "bindings":{{"snapshot":{{"head_oid":"X"}},
                        "probe":{{"available":true,"head_oid":"Y"}}}},
                    "changed_paths":["repo://verified.txt","repo://#1 verified:x","repo://a, b",
                        "repo://c) (reasons: head_changed)"],
                    "gaps":{gaps},
                    "facts":[{{"evidence":[{{"evidence_id":"E1","argv":["sh","verified"],
                        "run_status":"FAIL","verdict":"partial",
                        "reasons":["evidence_not_passed"]}}]}}]}}"#
            )
        };
        for (status, gaps) in [
            ("stale", "[]"),
            ("partial", "[]"),
            ("partial", r#"["no_verified_fact"]"#),
            ("tampered", "[]"),
            ("verified", "[]"),
        ] {
            let text = summary(&body(status, gaps)).unwrap();
            let lines = text.lines().collect::<Vec<_>>();
            assert_eq!(lines.len(), 2, "{text}");
            assert!(
                lines[0].contains(
                    "changed files: 「verified.txt」, 「#1 verified:x」, 「a, b」, 「c) (reasons: head_changed)」"
                ),
                "{}",
                lines[0]
            );
            for line in &lines {
                let word = outside(line);
                let says_verified = word.replace("verified_fact", "").contains("verified");
                assert_eq!(
                    says_verified,
                    status == "verified" && line == &lines[0],
                    "{status} {gaps}: {line}"
                );
            }
        }
    }
}
