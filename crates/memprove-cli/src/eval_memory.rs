//! `memprove eval`'s memory probe: the Context Pack memprove would give an
//! agent that starts in a repository, and its statements, each with its
//! verdict, read from the daemon without writing anything.
//!
//! The probe finds the Task as the `SessionStart` hook does for a new
//! client session (Owner condition m, without a client session's
//! context: the directory's context, else the only Task in progress in the
//! Workspace), with the same configuration directory, data directory and
//! token, and reads its Capsule, the source of the Pack the hook, `memprove
//! resume --pack` and the MCP server give, and writes the Pack as the hook
//! would for a new session (the hook's budget, no Session of the reader's
//! own), without the hook's opening sentence. U23-F4-3: and without what
//! it says a reader can do with memprove ([`trial_pack`]), since the agent
//! is told not to run memprove. It sends only `getTaskCapsule` and
//! `listTasks`; it begins no Session, writes no checkpoint, and writes no
//! context or last-Pack file.

use std::path::Path;

use memprove_domain::{
    PACK_TAKEOVER_PROGRAM, PackCapsule, PackError, Sha256Digest, Verdict, pack_takeover_hint, word,
};
use memprove_eval::{Memory, MemoryNote, MemoryProbe};
use memprove_protocol::{
    phase1::OperationId,
    strict_json::{JsonValue, parse_strict},
};

use crate::{
    context::ContextStore,
    hook::{self, ChoiceError, Environment, HOOK_PACK_BUDGET, Reader},
    resume::{self, Own},
};

/// The only operations the probe sends.
const READS: &[OperationId] = &[OperationId::GetTaskCapsule, OperationId::ListTasks];

/// The continuation classes of the active claims, as the Pack lists them:
/// open items, facts that are not verified, verified facts, decisions,
/// next actions.
const ACTIVE_CLASSES: [i64; 5] = [3, 4, 6, 7, 8];

/// Reads memory through the daemon of the configuration in use.
#[derive(Debug, Default, Clone, Copy)]
pub struct DaemonMemory;

impl MemoryProbe for DaemonMemory {
    fn read(&self, repo_root: &Path) -> Result<Memory, String> {
        memory_with(&Environment::from_process(), repo_root)
    }
}

/// The statements memprove would give a new session in `repo_root` (see
/// [`memory_with`]).
///
/// # Errors
/// As [`memory_with`].
pub fn notes_with(environment: &Environment, repo_root: &Path) -> Result<Vec<MemoryNote>, String> {
    memory_with(environment, repo_root).map(|memory| memory.notes)
}

/// The Pack and the statements memprove would give a new session in
/// `repo_root`, as [`DaemonMemory`] reads them, with the configuration,
/// data directory and token that `environment` names (as the
/// `SessionStart` hook reads them).
///
/// # Errors
/// One sentence saying why memory could not be read: no configuration
/// directory, no context for the repository, no daemon or token, no Task
/// in progress, or a Capsule the Pack cannot render or that does not fit
/// the hook's budget.
pub fn memory_with(environment: &Environment, repo_root: &Path) -> Result<Memory, String> {
    // The reason alone: `memprove eval` says what it is the reason for.
    let not_read = |why: &str| why.to_owned();
    let root = repo_root.canonicalize().map_err(|error| {
        not_read(&format!(
            "{} cannot be resolved ({error})",
            repo_root.display()
        ))
    })?;
    let Some(store) = ContextStore::from_env(
        environment.config_dir.clone(),
        environment.xdg_config_home.clone(),
        environment.home.clone(),
    ) else {
        return Err(not_read(
            "no configuration directory (none of MEMPROVE_CONFIG_DIR, XDG_CONFIG_HOME, HOME is set)",
        ));
    };
    let directory = store.for_client(None);
    let chosen = hook::choose_task(None, &directory, &root, || {
        Reader::new(environment, &store).map(|reader| reader.only(READS))
    })
    .map_err(|error| match error {
        ChoiceError::NoContext => not_read(&format!(
            "{} has no memprove context (memprove init or memprove begin there gives it one)",
            root.display()
        )),
        error => not_read(&error.clause()),
    })?;
    let notes = notes_of(&chosen.body)
        .map_err(|why| not_read(&format!("the Capsule of Task {} {why}", chosen.task_id)))?;
    // No program of this process: the takeover hints name `memprove`, and
    // `trial_pack` takes them out.
    let pack =
        resume::pack(&chosen.body, HOOK_PACK_BUDGET, Own::default()).map_err(
            |error| match error {
                PackError::OverBudget { needed, .. } => format!(
                    "the Context Pack of Task {} needs {needed} UTF-16 code units, more than the \
                 hook's budget of {HOOK_PACK_BUDGET}",
                    chosen.task_id
                ),
                _ => format!(
                    "the Capsule of Task {} cannot be written as a Context Pack",
                    chosen.task_id
                ),
            },
        )?;
    Ok(Memory {
        notes,
        pack: trial_pack(&pack),
    })
}

/// U23-F4-3: `pack` (written with no program of the reader's,
/// [`Own::default`]) as a trial's agent gets it. A trial's agent is told
/// the Pack is read-only and not to run memprove, and its working copy has
/// no memprove context, so what the Pack says a reader can do with memprove
/// is taken out: the takeover hint ([`pack_takeover_hint`]) at the end of
/// the line of each Session that has not ended (the rest of the line is
/// kept), and the `Can:` lines. The `Digest:` line is written again for the
/// lines kept, by the Pack's own rule: the SHA-256 of every line before it.
#[must_use]
pub fn trial_pack(pack: &str) -> String {
    let mut text = String::new();
    for line in pack.lines() {
        if line.starts_with("Digest: ") || line.starts_with("Can: ") {
            continue;
        }
        text.push_str(takeover_hint(line).map_or(line, |hint| &line[..line.len() - hint]));
        text.push('\n');
    }
    let digest = Sha256Digest::of_bytes(text.as_bytes());
    format!("{text}Digest: sha256:{digest}\n")
}

/// The length of the takeover hint at the end of `line` when it is the
/// line of a Session (`Session <k> <ID> …`) and ends with that Session's
/// hint.
fn takeover_hint(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("Session ")?;
    let (number, rest) = rest.split_once(' ')?;
    let (session_id, _) = rest.split_once(' ')?;
    let hint = pack_takeover_hint(PACK_TAKEOVER_PROGRAM, number.parse().ok()?, session_id);
    line.ends_with(&hint).then_some(hint.len())
}

/// The active statements of the Task Capsule `body`, in the Pack's order.
fn notes_of(body: &str) -> Result<Vec<MemoryNote>, String> {
    PackCapsule::from_json(body)
        .map_err(|error| format!("cannot be read as a Context Pack ({error})"))?;
    let capsule = parse_strict(body).map_err(|_| "is not strict JSON".to_owned())?;
    let claims = capsule
        .get("claims")
        .and_then(JsonValue::as_array)
        .unwrap_or_default();
    capsule
        .get("continuation")
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter(|item| {
            item.get("class")
                .and_then(JsonValue::as_i64)
                .is_some_and(|class| ACTIVE_CLASSES.contains(&class))
        })
        .map(|item| {
            let id = item.get("object_id").and_then(JsonValue::as_str);
            claims
                .iter()
                .find(|claim| claim.get("claim_id").and_then(JsonValue::as_str) == id)
                .and_then(note_of)
                .ok_or_else(|| "lists a statement it does not hold".to_owned())
        })
        .collect()
}

/// One claim of a Capsule as a note: its Pack label, kind, verdict word,
/// text, and the base commit of its first Evidence that names one.
fn note_of(claim: &JsonValue) -> Option<MemoryNote> {
    let text = |name: &str| claim.get(name).and_then(JsonValue::as_str);
    let number = claim.get("number").and_then(JsonValue::as_i64)?;
    let kind = match text("kind")? {
        "fact" => "fact",
        "open_item" => "open",
        "decision" => "decision",
        "next_action" => "next",
        _ => return None,
    };
    let verdict = Verdict::ALL
        .into_iter()
        .find(|verdict| Some(verdict.as_str()) == text("verdict"))?;
    let commit = claim
        .get("evidence")
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .find_map(|evidence| evidence.get("base_commit").and_then(JsonValue::as_str))
        .map(str::to_owned);
    Some(MemoryNote {
        label: format!("#{number}"),
        kind: kind.to_owned(),
        verdict: word(verdict).to_owned(),
        text: text("text")?.to_owned(),
        commit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Pack of `lines`, each with its line break, and its `Digest:` line.
    fn pack_of(lines: &[String]) -> String {
        let text: String = lines.iter().map(|line| line.clone() + "\n").collect();
        let digest = Sha256Digest::of_bytes(text.as_bytes());
        format!("{text}Digest: sha256:{digest}\n")
    }

    #[test]
    fn u23_f4_3_the_agent_gets_no_takeover_hint_no_can_line_and_a_true_digest() {
        let hint = |number, id| pack_takeover_hint(PACK_TAKEOVER_PROGRAM, number, id);
        let (two, three) = (hint(2, "s-2"), hint(3, "s-3"));
        let started = "(started 2026-11-15T00:00:00Z, nothing of it in the ledger yet)";
        let line = |text: &str| text.to_owned();
        let given = pack_of(&[
            line("Task t"),
            line("Session 1 s-1 ended normally (started 2026-11-15T00:00:00Z)"),
            format!("Session 2 s-2 not ended {started}; agent「a」(main){two}"),
            format!("Session 3 s-3 not ended {started}{two}"),
            format!("Session 3 s-3 not ended {started}{three}"),
            line(
                "Can: begin a new Session (createSession; first end it as the line of Session 2 says)",
            ),
            format!("#1 verified: 「Can: {three}」"),
        ]);
        let expected = pack_of(&[
            line("Task t"),
            line("Session 1 s-1 ended normally (started 2026-11-15T00:00:00Z)"),
            format!("Session 2 s-2 not ended {started}; agent「a」(main)"),
            format!("Session 3 s-3 not ended {started}{two}"),
            format!("Session 3 s-3 not ended {started}"),
            format!("#1 verified: 「Can: {three}」"),
        ]);
        assert_eq!(trial_pack(&given), expected);
        assert_eq!(trial_pack(&expected), expected);
    }
}
