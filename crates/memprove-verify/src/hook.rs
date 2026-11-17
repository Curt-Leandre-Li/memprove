//! `memprove verify hook`: one command for two Claude Code hooks. At
//! `SessionStart` it records the commit the session starts at, the base of
//! the session's verifications. At `Stop` it verifies the work as it
//! stands: a verdict of `fail` stops the agent from finishing and tells it
//! what failed, at most [`MAX_BLOCKS`] times in a session; `needs a
//! person` and an error of the verification itself never block, they are
//! shown to the person. A repository without a contract, and work that is
//! the base's tree, are left alone without a word.
//!
//! The hook reads the event's JSON on standard input as untrusted input,
//! prints at most one JSON object, and always exits 0.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use memprove_eval::{
    git,
    json::{self, Out},
};
use sha2::{Digest, Sha256};

use crate::{
    CONTRACT_PATH, VerifyError,
    receipt::Verdict,
    run::{self, Options, Planned},
    state_dir,
};

/// The most bytes of standard input the hook reads; a longer input is
/// ignored.
pub const MAX_INPUT_BYTES: u64 = 64 * 1024;
/// The most times in one client session a `fail` verdict stops the agent
/// from finishing.
pub const MAX_BLOCKS: u32 = 3;
/// The `timeout` of the `SessionStart` handler `init --hook` writes, in
/// seconds.
pub const START_TIMEOUT_SECONDS: u64 = 10;
/// The `timeout` of the `Stop` handler `init --hook` writes, in seconds:
/// the hook runs the contract's commands, which may take long.
pub const STOP_TIMEOUT_SECONDS: u64 = 3600;
/// The arguments after the program in the handlers `init --hook` writes.
pub const HANDLER_ARGS: [&str; 2] = ["verify", "hook"];

/// Handles one hook event read from `input`; `cwd` is the directory when
/// the event names none. Returns the JSON object to print, if any.
pub fn hook(input: &mut dyn Read, cwd: &Path) -> Option<String> {
    let mut text = String::new();
    let read = input
        .take(MAX_INPUT_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if u64::try_from(read).is_ok_and(|read| read > MAX_INPUT_BYTES) {
        return None;
    }
    let event = json::parse(&text, None).ok()?;
    let member = |name: &str| event.get(name).and_then(json::JsonValue::as_str);
    let dir = member("cwd").map_or_else(|| cwd.to_path_buf(), PathBuf::from);
    let root = git::toplevel(&dir).ok()?;
    let session = member("session_id");
    match member("hook_event_name")? {
        "SessionStart" => {
            record_base(&root, session);
            None
        }
        "Stop" => stop(&root, session).unwrap_or_else(|error| {
            Some(message(&format!(
                "memprove verify could not verify this work: {error}"
            )))
        }),
        _ => None,
    }
}

/// The name of `session`'s files: the session id when it is 1 to 64 of
/// letters, digits, `-` and `_`, else a digest of it; `no-session` without
/// one.
fn session_key(session: Option<&str>) -> String {
    match session {
        None => "no-session".to_owned(),
        Some(id)
            if (1..=64).contains(&id.len())
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_') =>
        {
            id.to_owned()
        }
        Some(id) => crate::hex(&Sha256::digest(id.as_bytes())[..16]),
    }
}

fn session_file(root: &Path, session: Option<&str>, extension: &str) -> Option<PathBuf> {
    let dir = state_dir(root).ok()?.join("sessions");
    fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{}.{extension}", session_key(session))))
}

/// Records `HEAD` as the commit `session` started at, unless one is
/// recorded already (a resumed or compacted session keeps its first).
fn record_base(root: &Path, session: Option<&str>) {
    let (Some(file), Ok(head)) = (
        session_file(root, session, "base"),
        git::commit(root, "HEAD"),
    ) else {
        return;
    };
    if session.is_some() && !file.exists() {
        let _ = fs::write(file, head);
    }
}

/// The commit recorded for `session`.
fn recorded_base(root: &Path, session: Option<&str>) -> Option<String> {
    session?;
    let text = fs::read_to_string(session_file(root, session, "base")?).ok()?;
    Some(text.trim().to_owned()).filter(|commit| !commit.is_empty())
}

fn stop(root: &Path, session: Option<&str>) -> Result<Option<String>, VerifyError> {
    let mut options = Options::new(root)?;
    options.recorded_base = recorded_base(root, session);
    options.session = session.map(str::to_owned);
    let plan = match run::plan(&options)? {
        Planned::NoContract => return Ok(None),
        Planned::Ready(plan) => plan,
    };
    if plan.unchanged() {
        plan.discard();
        return Ok(None);
    }
    let (earlier, fresh) = if let Some(earlier) = run::earlier(root, &plan.key()) {
        plan.discard();
        (earlier, false)
    } else {
        let (_, run_dir) = run::execute(&options, &plan)?;
        let earlier = run::read_run(&run_dir).ok_or_else(|| {
            VerifyError::invalid(Some(&run_dir), "the run directory holds no receipt")
        })?;
        (earlier, true)
    };
    let blocks_file = session_file(root, session, "blocks");
    let blocks = blocks_file
        .as_ref()
        .and_then(|file| fs::read_to_string(file).ok())
        .and_then(|text| text.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let set_blocks = |count: u32| {
        if let Some(file) = &blocks_file {
            let _ = fs::write(file, count.to_string());
        }
    };
    Ok(match earlier.verdict {
        Verdict::Pass => {
            if let Some(file) = &blocks_file {
                let _ = fs::remove_file(file);
            }
            fresh.then(|| message(&earlier.summary))
        }
        Verdict::NeedsHuman => fresh.then(|| message(&earlier.summary)),
        Verdict::Fail if blocks < MAX_BLOCKS => {
            set_blocks(blocks + 1);
            Some(block(&format!(
                "memprove verify: this work does not pass the repository's checks yet \
                 (stop {} of {MAX_BLOCKS} that this hook holds back).\n\n{}\n{}\n\
                 Fix what fails, then finish again. Changing {CONTRACT_PATH} or the protected \
                 paths does not make this pass: the checks are read from the base commit, and \
                 work that passes only with its own changes to protected paths is left to a \
                 person.",
                blocks + 1,
                earlier.summary,
                earlier.failure.as_deref().unwrap_or_default(),
            )))
        }
        Verdict::Fail => {
            set_blocks(blocks.saturating_add(1));
            (fresh || blocks == MAX_BLOCKS).then(|| {
                message(&format!(
                    "memprove verify: this work still does not pass after {MAX_BLOCKS} held \
                     stops; the agent is no longer held back.\n\n{}",
                    earlier.summary
                ))
            })
        }
    })
}

/// A `Stop` decision that keeps the agent working, with `reason` for it.
fn block(reason: &str) -> String {
    Out::Object(vec![
        ("decision", Out::str("block")),
        ("reason", Out::str(reason)),
    ])
    .render()
}

/// A message shown to the person; the agent goes on as it would.
fn message(text: &str) -> String {
    Out::Object(vec![("systemMessage", Out::str(text.trim_end()))]).render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_files_are_named_safely() {
        assert_eq!(session_key(None), "no-session");
        assert_eq!(
            session_key(Some("0191f2a4-7b1c-7d3e-9a55-0c1d2e3f4a5b")),
            "0191f2a4-7b1c-7d3e-9a55-0c1d2e3f4a5b"
        );
        let unsafe_id = session_key(Some("../../etc/passwd"));
        assert_eq!(unsafe_id.len(), 32);
        assert!(unsafe_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(session_key(Some("")).len(), 32);
    }

    #[test]
    fn input_that_is_not_a_hook_event_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        for input in ["", "not json", "{}", r#"{"hook_event_name":"Stop"}"#] {
            assert_eq!(hook(&mut input.as_bytes(), dir.path()), None, "{input}");
        }
        let long = format!(
            r#"{{"hook_event_name":"Stop","pad":"{}"}}"#,
            "x".repeat(70_000)
        );
        assert_eq!(hook(&mut long.as_bytes(), dir.path()), None);
    }
}
