//! U3 G4 (Owner O8, O9; ADR 0016, "Query selection") through the real
//! binaries: `memprove resume --pack --query <TEXT>` on a Task of more
//! than 60 statements, superseded and withdrawn ones among them. An
//! A one-word and a two-word query list the statements they match in the rank
//! order an in-memory FTS5 index (`trigram`, `bm25`, ties by number) gives
//! over the Capsule's statements; a superseded match is followed by what
//! supersedes it; the active next action closes the list; no match is a
//! whole Pack with no claim line; a small budget cuts by the levels; a
//! query with a control character or a token
//! is refused without being repeated; the same query gives the same bytes,
//! after a daemon restart too; a Pack without `--query` is the whole
//! Pack; nothing is written to the ledger or recorded for `begin`, and the
//! query is in no file.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;

use memprove_domain::{ClaimKind, PackCapsule, pack_kind_label, render_pack, select_pack};
use memprove_store_sqlite::{SearchEntry, rank_statements_v2};
use p4b_support::{Cli, Setup, sequence, step};
use p4g_support::{files_under, last_packs, level};
use serde_json::Value;

/// A query that appears in no file unless something persisted it.
const MARKER: &str = "budget";
const FILLERS: u64 = 52;

/// A Task of 64 statements: two facts, 52 filler open items (one later
/// withdrawn), an open item superseded twice in turn, a Decision with a
/// rationale, an open item about the budget, and two next actions (the
/// first superseded by the second).
fn query_ledger(setup: &Setup) -> String {
    let agent = &setup.agent;
    step("a Task of 64 statements");
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "U3 G4 query Pack",
    ]);
    let a = agent.test("tests/a.sh");
    let fillers = (1..=FILLERS)
        .map(|number| format!("unrelated record filler {number}"))
        .collect::<Vec<_>>();
    let mut words = vec![
        "checkpoint".to_owned(),
        "create".to_owned(),
        "--fact".to_owned(),
        "The Pack budget counts UTF-16 code units".to_owned(),
        "--evidence".to_owned(),
        a.clone(),
        "--fact".to_owned(),
        "the limit is counted in UTF-16 code units; nine thousand by default".to_owned(),
        "--evidence".to_owned(),
        a,
    ];
    for filler in &fillers {
        words.push("--open".to_owned());
        words.push(filler.clone());
    }
    words.extend(
        [
            "--open",
            "query results ordered by claim number (old)",
            "--open",
            "budget budget budget",
            "--next",
            "the old next step: write the tests first",
        ]
        .map(str::to_owned),
    );
    let words = words.iter().map(String::as_str).collect::<Vec<_>>();
    agent.ok(&words);
    let capsule = agent.capsule();
    let old = number_of(&capsule, "query results ordered by claim number (old)");
    let withdrawn = number_of(&capsule, "unrelated record filler 7");
    agent.ok(&[
        "checkpoint",
        "create",
        "--open",
        "query results ordered by rank (second version)",
        "--supersedes",
        &old.to_string(),
        "--withdraw",
        &withdrawn.to_string(),
        "--next",
        "the old next step: write the tests first",
    ]);
    let second = number_of(
        &agent.capsule(),
        "query results ordered by rank (second version)",
    );
    agent.ok(&[
        "checkpoint",
        "create",
        "--open",
        "query results ordered by bm25 rank, ties by number ascending",
        "--supersedes",
        &second.to_string(),
        "--next",
        "next step: implement the query Pack",
    ]);
    agent.bytes(&[
        "decide",
        "Use the trigram tokenizer",
        "--rationale",
        "the budget search needs substrings",
    ]);
    begun["task_id"].as_str().unwrap().to_owned()
}

fn number_of(capsule: &Value, text: &str) -> u64 {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|claim| claim["text"] == text)
        .map(|claim| claim["number"].as_u64().unwrap())
        .max()
        .unwrap_or_else(|| panic!("no claim {text:?}"))
}

/// The ranking an in-memory FTS5 index gives over the Capsule's
/// statements (the matching of U4 S2, the CLI's since U16), computed here
/// apart from the CLI.
fn reference_matches(capsule: &Value, query: &str) -> Vec<u64> {
    let claims = capsule["claims"].as_array().unwrap();
    let entries = claims
        .iter()
        .map(|claim| {
            let kind = claim["kind"].as_str().unwrap();
            let kind = ClaimKind::ALL
                .into_iter()
                .find(|known| known.as_str() == kind)
                .unwrap_or_else(|| panic!("kind {kind}"));
            SearchEntry {
                number: claim["number"].as_u64().unwrap(),
                text: claim["text"].as_str().unwrap(),
                rationale: claim["rationale"].as_str(),
                kind_label: pack_kind_label(kind),
                active: claim["standing"] == "active",
            }
        })
        .collect::<Vec<_>>();
    rank_statements_v2(&entries, query).unwrap()
}

/// The query Pack, whose ranking this test's reference ranking is.
fn query_pack(agent: &Cli, query: &str, more: &[&str]) -> String {
    let mut words = vec!["resume", "--pack", "--query", query];
    words.extend(more);
    agent.bytes(&words)
}

fn claim_lines(pack: &str) -> Vec<u64> {
    pack.lines()
        .filter_map(|line| line.strip_prefix('#'))
        .map(|rest| rest.split(' ').next().unwrap().parse().unwrap())
        .collect()
}

fn line_of(pack: &str, number: u64) -> &str {
    pack.lines()
        .find(|line| line.starts_with(&format!("#{number} ")))
        .unwrap_or_else(|| panic!("no #{number}:\n{pack}"))
}

fn query_line(matches: &[u64], query: &str) -> String {
    let mut line = format!("Query: 「{query}」; {} matches", matches.len());
    if !matches.is_empty() {
        line.push_str(", by rank: ");
        line.push_str(
            &matches
                .iter()
                .map(|number| format!("#{number}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    line
}

/// The Pack's digest line holds the SHA-256 of the lines before it.
fn assert_whole(pack: &str) {
    let body = pack.strip_suffix('\n').unwrap();
    let (before, last) = body.rsplit_once('\n').unwrap();
    let digest = memprove_domain::Sha256Digest::of_bytes(format!("{before}\n").as_bytes());
    assert_eq!(last, format!("Digest: sha256:{digest}"), "{pack}");
    for line in body.lines() {
        assert!(
            memprove_domain::PACK_LINE_PREFIXES
                .iter()
                .any(|prefix| line.starts_with(prefix)),
            "{line}"
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One scenario, step by step.
fn u3g4_resume_pack_query_selects_by_rank() {
    let mut setup = Setup::new(&["a"]);
    let task = query_ledger(&setup);
    let agent = setup.agent.clone();
    let capsule = agent.capsule();
    let claims = capsule["claims"].as_array().unwrap();
    assert!(claims.len() >= 60, "{}", claims.len());
    let of = |standing: &str| {
        claims
            .iter()
            .filter(|claim| claim["standing"] == standing)
            .count()
    };
    assert!(of("superseded") >= 3 && of("withdrawn") >= 1);
    let before = sequence(&capsule);
    for (path, _) in last_packs(&agent.config_dir) {
        std::fs::remove_file(path).unwrap();
    }

    step("English: budget, ranked by bm25");
    let next = number_of(&capsule, "next step: implement the query Pack");
    let triple = number_of(&capsule, "budget budget budget");
    let fact = number_of(&capsule, "The Pack budget counts UTF-16 code units");
    let decision = number_of(&capsule, "Use the trigram tokenizer");
    let matches = reference_matches(&capsule, "budget");
    assert_eq!(matches.first(), Some(&triple), "{matches:?}");
    let mut sorted = matches.clone();
    sorted.sort_unstable();
    let mut expected = vec![fact, triple, decision];
    expected.sort_unstable();
    assert_eq!(sorted, expected);
    let pack = query_pack(&agent, MARKER, &[]);
    println!("{pack}");
    assert_whole(&pack);
    let lines = pack.lines().collect::<Vec<_>>();
    assert!(lines[1].starts_with("Ontology: "), "{pack}");
    assert_eq!(lines[2], query_line(&matches, "budget"));
    assert!(lines[3].starts_with("Task「U3 G4 query Pack」"), "{pack}");
    let mut listed = matches.clone();
    listed.push(next);
    assert_eq!(claim_lines(&pack), listed);
    assert!(!pack.contains("\nRerun: ") && !pack.contains("\nSuperseded and withdrawn: "));

    step("ordered: the superseded matches with what supersedes them");
    let old = number_of(&capsule, "query results ordered by claim number (old)");
    let second = number_of(&capsule, "query results ordered by rank (second version)");
    let current = number_of(
        &capsule,
        "query results ordered by bm25 rank, ties by number ascending",
    );
    let matches = reference_matches(&capsule, "ordered");
    let mut sorted = matches.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, [old, second, current]);
    let pack = query_pack(&agent, "ordered", &[]);
    println!("{pack}");
    assert_whole(&pack);
    assert_eq!(
        pack.lines().nth(2).unwrap(),
        query_line(&matches, "ordered")
    );
    // Each match, then what supersedes it, in turn, unless listed already.
    let mut expected = Vec::new();
    for number in &matches {
        let chain: &[u64] = if *number == old {
            &[old, second, current]
        } else if *number == second {
            &[second, current]
        } else {
            &[current]
        };
        for each in chain {
            if !expected.contains(each) {
                expected.push(*each);
            }
        }
    }
    expected.push(next);
    assert_eq!(claim_lines(&pack), expected);
    assert!(
        line_of(&pack, old).starts_with(&format!("#{old} open item, superseded by #{second}: ")),
        "{pack}"
    );
    assert!(
        line_of(&pack, second)
            .starts_with(&format!("#{second} open item, superseded by #{current}: ")),
        "{pack}"
    );
    // Only the oldest matches: what supersedes it follows, in turn.
    let pack = query_pack(&agent, "(old)", &[]);
    assert_eq!(reference_matches(&capsule, "(old)"), [old]);
    assert_eq!(claim_lines(&pack), [old, second, current, next]);

    step("a two-character word matches a whole word only; a withdrawn match shows its standing");
    let pack = query_pack(&agent, "by", &[]);
    let budget_fact = number_of(
        &capsule,
        "the limit is counted in UTF-16 code units; nine thousand by default",
    );
    let mut by = reference_matches(&capsule, "by");
    by.sort_unstable();
    let mut expected = [old, second, current, budget_fact];
    expected.sort_unstable();
    assert_eq!(by, expected);
    assert!(claim_lines(&pack).contains(&budget_fact), "{pack}");
    assert!(!claim_lines(&pack).contains(&triple), "{pack}");
    let withdrawn = number_of(&capsule, "unrelated record filler 7");
    let pack = query_pack(&agent, "filler 7", &[]);
    assert!(
        line_of(&pack, withdrawn).starts_with(&format!("#{withdrawn} open item, withdrawn: ")),
        "{pack}"
    );

    step("no match: a whole Pack with no claim line");
    let pack = query_pack(&agent, "nothing like this", &[]);
    println!("{pack}");
    assert_whole(&pack);
    assert_eq!(
        pack.lines().nth(2).unwrap(),
        "Query: 「nothing like this」; 0 matches"
    );
    assert!(claim_lines(&pack).is_empty(), "{pack}");
    assert!(!pack.contains("\nBase: "), "{pack}");
    assert!(pack.contains("\nSessions: "), "{pack}");

    step("over budget: the levels cut, and the query line keeps every match");
    let pack = query_pack(&agent, "filler", &["--budget", "2400"]);
    assert_whole(&pack);
    assert!(pack.encode_utf16().count() <= 2400);
    assert_eq!(level(&pack), 5, "{pack}");
    let matches = reference_matches(&capsule, "filler");
    assert_eq!(matches.len(), usize::try_from(FILLERS).unwrap());
    let listed = pack
        .lines()
        .find(|line| line.starts_with("Query: "))
        .unwrap_or_else(|| panic!("{pack}"));
    assert!(
        listed.contains(&format!("; {} matches", matches.len())),
        "{listed}"
    );
    // Level 5 lists fewer claims than matched, and the next action stays.
    assert!(claim_lines(&pack).len() < matches.len(), "{pack}");
    assert!(claim_lines(&pack).contains(&next), "{pack}");

    step("refused: a control character, a token; never repeated");
    let token = "u3G4_tokenShaped-0123456789abcdefghijklmnop";
    assert_eq!(token.len(), 43);
    for query in [
        "secret\u{7}query".to_owned(),
        format!("secret {token}"),
        "s".repeat(257),
        String::new(),
    ] {
        let run = agent.run(&["resume", "--pack", "--query", &query], true);
        assert_eq!(run.code, Some(2), "{query:?}: {}", run.stderr);
        assert!(run.stdout.is_empty());
        assert!(!run.stderr.contains("secret") && !run.stderr.contains(token));
        assert!(run.stderr.contains("--query"), "{}", run.stderr);
    }
    for words in [
        &["resume", "--query", "budget"][..],
        &["resume", "--text", "--query", "budget"],
        &["resume", "--pack", "--claims", "1", "--query", "budget"],
    ] {
        let run = agent.run(words, true);
        assert_eq!(run.code, Some(2), "{words:?}");
        assert!(run.stdout.is_empty());
    }

    step("the same query, the same bytes, after a restart too");
    let first = query_pack(&agent, "ordered budget", &[]);
    assert_eq!(query_pack(&agent, "ordered budget", &[]), first);
    setup.stop();
    setup.start();
    assert_eq!(query_pack(&agent, "ordered budget", &[]), first);

    step("without --query: the whole Pack, no Query line");
    let plain = agent.bytes(&["resume", "--pack"]);
    assert!(!plain.contains("Query: "));
    let body = agent.bytes(&["resume"]);
    // U7 P1 (Owner O3, amended 2026-10-21): read as the agent reads it,
    // whose own Session is the one its context names (its open line has no
    // takeover hint).
    let shown = agent.bytes(&["context", "show"]);
    let own = shown
        .lines()
        .find_map(|line| line.strip_prefix("Session:"))
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|session| *session != "none");
    let reference = render_pack(
        &select_pack(
            &PackCapsule::from_json(body.trim_end())
                .unwrap()
                .with_own_session(own),
            9_000,
        )
        .unwrap(),
    );
    assert_eq!(plain, reference);

    step("nothing written, nothing recorded for begin, the query in no file");
    for (path, _) in last_packs(&agent.config_dir) {
        std::fs::remove_file(path).unwrap();
    }
    query_pack(&agent, MARKER, &[]);
    assert!(last_packs(&agent.config_dir).is_empty());
    let after = agent.capsule();
    assert_eq!(sequence(&after), before, "reads write nothing");
    assert_eq!(after["task"]["task_id"], task.as_str());
    let unique = "the unique query marker zqxjv";
    let pack = query_pack(&agent, unique, &[]);
    assert!(pack.contains(unique));
    for (path, bytes) in files_under(&setup.root_path) {
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains(unique), "{}", path.display());
    }
}
