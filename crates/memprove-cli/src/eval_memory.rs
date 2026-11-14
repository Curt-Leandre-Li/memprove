//! `memprove eval`'s memory probe: the statements of the Pack memprove
//! would give an agent that starts in a repository, each with its verdict,
//! read from the daemon without writing anything.
//!
//! The probe finds the Task as the `SessionStart` hook does for a new
//! client session (Owner condition m, without a client session's
//! context: the directory's context, else the only Task in progress in the
//! Workspace), with the same configuration directory, data directory and
//! token, and reads its Capsule, the source of the Pack the hook, `memprove
//! resume --pack` and the MCP server give. It sends only `getTaskCapsule`
//! and `listTasks`; it begins no Session, writes no checkpoint, and writes
//! no context or last-Pack file.

use std::path::Path;

use memprove_domain::{PackCapsule, Verdict, word};
use memprove_eval::{MemoryNote, MemoryProbe};
use memprove_protocol::{
    phase1::OperationId,
    strict_json::{JsonValue, parse_strict},
};

use crate::{
    context::ContextStore,
    hook::{self, ChoiceError, Environment, Reader},
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
    fn notes(&self, repo_root: &Path) -> Result<Vec<MemoryNote>, String> {
        notes_with(&Environment::from_process(), repo_root)
    }
}

/// The statements memprove would give a new session in `repo_root`, as
/// [`DaemonMemory`] reads them, with the configuration, data directory and
/// token that `environment` names (as the `SessionStart` hook reads them).
///
/// # Errors
/// One sentence saying why memory could not be read: no configuration
/// directory, no context for the repository, no daemon or token, no Task
/// in progress, or a Capsule the Pack cannot render.
pub fn notes_with(environment: &Environment, repo_root: &Path) -> Result<Vec<MemoryNote>, String> {
    let not_read = |why: &str| format!("memprove's memory was not read: {why}");
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
    notes_of(&chosen.body)
        .map_err(|why| not_read(&format!("the Capsule of Task {} {why}", chosen.task_id)))
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
