//! `memprove verify init`: writes the contract, with the test command and
//! the protected paths guessed from the repository's files, and with
//! `--hook` adds the two Claude Code hooks to
//! `.claude/settings.local.json`.
//!
//! The contract is written only when there is none: once it exists it is
//! the person's. In the settings file init owns only the handlers that run
//! `verify hook`, which it replaces; every other member stays as it is,
//! and a second run changes nothing.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use memprove_eval::{
    CommandSpec,
    init::{Detected, ProjectKind, detect},
    json::{self, JsonValue},
};

use crate::{
    CONTRACT_PATH, CONTRACT_SCHEMA, Check, Contract, VerifyError,
    hook::{HANDLER_ARGS, START_TIMEOUT_SECONDS, STOP_TIMEOUT_SECONDS},
};

/// The settings file `init --hook` writes, relative to the repository
/// root: the one Claude Code keeps out of version control, since a handler
/// names a program on this computer.
pub const SETTINGS_FILE: &str = ".claude/settings.local.json";
/// The time limit of the check `init` writes.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(1800);
/// The time limit of a setup command `init` writes.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(900);
/// The most bytes of a settings file `init --hook` reads.
const MAX_SETTINGS_BYTES: u64 = 4 << 20;

/// What `init` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitSpec {
    /// The worktree root.
    pub repo_root: PathBuf,
    /// Also add the hooks to [`SETTINGS_FILE`].
    pub hook: bool,
    /// The program the handlers run: this `memprove`.
    pub executable: String,
    /// List the files that would be written; write nothing.
    pub dry_run: bool,
}

/// What `init` did, for its output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitReport {
    /// Files written or changed, relative to the root (in a dry run: the
    /// files that would be).
    pub written: Vec<PathBuf>,
    /// Files that already said the same.
    pub unchanged: Vec<PathBuf>,
    /// The contract, when it exists: left as it is.
    pub kept: Vec<PathBuf>,
    /// Sentences for the person.
    pub notes: Vec<String>,
}

/// Sets up `spec.repo_root` for `memprove verify`.
///
/// # Errors
/// [`VerifyError::Invalid`] when there is no contract and no test command
/// can be guessed, when the existing contract is not valid, or when the
/// settings file is not a JSON object init can add to;
/// [`VerifyError::Io`] when a file cannot be read or written.
pub fn init(spec: &InitSpec) -> Result<InitReport, VerifyError> {
    let root = &spec.repo_root;
    let mut report = InitReport::default();
    let contract_path = root.join(CONTRACT_PATH);
    if contract_path.is_file() {
        let text = fs::read_to_string(&contract_path)
            .map_err(|error| VerifyError::io(&contract_path, error))?;
        Contract::parse(&text, Some(Path::new(CONTRACT_PATH)))?;
        report.kept.push(PathBuf::from(CONTRACT_PATH));
    } else {
        let found = detect(root).ok_or_else(|| {
            VerifyError::invalid(
                None,
                format!(
                    "no test command was recognised here (looked for Cargo.toml, package.json \
                     with a test script, pyproject.toml / pytest.ini / setup.cfg, go.mod, a \
                     Makefile with a test target); write {CONTRACT_PATH} by hand, for example:\n\
                     {{\"schema\": \"{CONTRACT_SCHEMA}\", \"checks\": [{{\"name\": \"tests\", \
                     \"argv\": [\"sh\", \"-c\", \"./run-tests.sh\"]}}], \"protected\": \
                     [\"tests/**\"]}}"
                ),
            )
        })?;
        let contract = guessed(&found);
        report.notes.push(format!(
            "The contract checks with `{}` (from {}) and protects {}; edit {CONTRACT_PATH} to \
             change either.",
            found.check.join(" "),
            found.marker,
            contract.protected.join(", ")
        ));
        if !spec.dry_run {
            write(&contract_path, &contract.render())?;
        }
        report.written.push(PathBuf::from(CONTRACT_PATH));
        report.notes.push(format!(
            "Commit {CONTRACT_PATH}: a verification reads the contract from the base commit, \
             so that the work being verified cannot weaken it."
        ));
    }
    if spec.hook {
        let settings_path = root.join(SETTINGS_FILE);
        let before = read_settings(&settings_path)?;
        let after = with_hooks(before.as_ref(), &spec.executable)
            .map_err(|detail| VerifyError::invalid(Some(Path::new(SETTINGS_FILE)), detail))?;
        if before.as_ref() == Some(&after) {
            report.unchanged.push(PathBuf::from(SETTINGS_FILE));
        } else {
            if !spec.dry_run {
                write(&settings_path, &render(&after))?;
            }
            report.written.push(PathBuf::from(SETTINGS_FILE));
        }
    } else {
        report.notes.push(
            "`memprove verify init --hook` adds the Claude Code hooks: the work is verified \
             each time the agent finishes, and a failing check keeps it working."
                .to_owned(),
        );
    }
    Ok(report)
}

/// The contract for a project whose test command was guessed.
fn guessed(found: &Detected) -> Contract {
    let protected: &[&str] = match found.kind {
        ProjectKind::Rust => &["tests/**", "**/tests/**"],
        ProjectKind::Node => &[
            "test/**",
            "tests/**",
            "**/__tests__/**",
            "**/*.test.*",
            "**/*.spec.*",
        ],
        ProjectKind::Python => &["tests/**", "**/test_*.py", "**/*_test.py", "**/conftest.py"],
        ProjectKind::Go => &["**/*_test.go", "**/testdata/**"],
        ProjectKind::Make => &["test/**", "tests/**"],
    };
    Contract {
        checks: vec![Check {
            name: "tests".to_owned(),
            command: CommandSpec {
                argv: found.check.clone(),
                timeout: CHECK_TIMEOUT,
            },
        }],
        setup: found
            .setup
            .iter()
            .map(|argv| CommandSpec {
                argv: argv.clone(),
                timeout: SETUP_TIMEOUT,
            })
            .collect(),
        protected: protected
            .iter()
            .map(|pattern| (*pattern).to_owned())
            .collect(),
        skip_markers: true,
    }
}

fn write(path: &Path, text: &str) -> Result<(), VerifyError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|error| VerifyError::io(dir, error))?;
    }
    let partial = path.with_extension("json.partial");
    fs::write(&partial, text)
        .and_then(|()| fs::rename(&partial, path))
        .map_err(|error| VerifyError::io(path, error))
}

fn read_settings(path: &Path) -> Result<Option<JsonValue>, VerifyError> {
    let Ok(meta) = fs::metadata(path) else {
        return Ok(None);
    };
    if meta.len() > MAX_SETTINGS_BYTES {
        return Err(VerifyError::invalid(
            Some(Path::new(SETTINGS_FILE)),
            "is too large to edit",
        ));
    }
    let text = fs::read_to_string(path).map_err(|error| VerifyError::io(path, error))?;
    json::parse(&text, Some(Path::new(SETTINGS_FILE))).map(Some)
}

fn string(text: &str) -> JsonValue {
    JsonValue::String(text.to_owned())
}

/// The handler group init writes under one event.
fn own_group(executable: &str, timeout: u64) -> JsonValue {
    JsonValue::Object(vec![(
        "hooks".to_owned(),
        JsonValue::Array(vec![JsonValue::Object(vec![
            ("type".to_owned(), string("command")),
            ("command".to_owned(), string(executable)),
            (
                "args".to_owned(),
                JsonValue::Array(HANDLER_ARGS.iter().map(|word| string(word)).collect()),
            ),
            ("timeout".to_owned(), JsonValue::Number(timeout.to_string())),
        ])]),
    )])
}

/// Whether `handler` runs `<program> verify hook`.
fn is_own(handler: &JsonValue) -> bool {
    handler
        .get("args")
        .and_then(JsonValue::as_array)
        .is_some_and(|args| {
            args.iter()
                .map(JsonValue::as_str)
                .eq(HANDLER_ARGS.iter().copied().map(Some))
        })
}

/// `settings` (an object, or nothing yet) with one `verify hook` handler
/// under `hooks.SessionStart` and one under `hooks.Stop`, run by
/// `executable`: handlers of `verify hook` already there are replaced,
/// everything else stays in place.
fn with_hooks(settings: Option<&JsonValue>, executable: &str) -> Result<JsonValue, String> {
    let mut members = match settings {
        None => Vec::new(),
        Some(value) => value.as_object().ok_or("must be a JSON object")?.to_vec(),
    };
    let mut hooks = match members.iter().find(|(name, _)| name == "hooks") {
        None => Vec::new(),
        Some((_, value)) => value
            .as_object()
            .ok_or("`hooks` must be an object")?
            .to_vec(),
    };
    for (event, timeout) in [
        ("SessionStart", START_TIMEOUT_SECONDS),
        ("Stop", STOP_TIMEOUT_SECONDS),
    ] {
        let groups = match hooks.iter().find(|(name, _)| name == event) {
            None => &[][..],
            Some((_, value)) => value
                .as_array()
                .ok_or_else(|| format!("`hooks.{event}` must be an array"))?,
        };
        let mut kept: Vec<JsonValue> = groups.iter().filter_map(without_own).collect();
        kept.push(own_group(executable, timeout));
        set(&mut hooks, event, JsonValue::Array(kept));
    }
    set(&mut members, "hooks", JsonValue::Object(hooks));
    Ok(JsonValue::Object(members))
}

/// `group` without its `verify hook` handlers; `None` when those were all
/// it had.
fn without_own(group: &JsonValue) -> Option<JsonValue> {
    let Some(handlers) = group.get("hooks").and_then(JsonValue::as_array) else {
        return Some(group.clone());
    };
    if !handlers.iter().any(is_own) {
        return Some(group.clone());
    }
    let rest: Vec<JsonValue> = handlers
        .iter()
        .filter(|handler| !is_own(handler))
        .cloned()
        .collect();
    if rest.is_empty() {
        return None;
    }
    let mut members = group.as_object()?.to_vec();
    set(&mut members, "hooks", JsonValue::Array(rest));
    Some(JsonValue::Object(members))
}

/// Sets `name` in `members`, in place when it is there, else at the end.
fn set(members: &mut Vec<(String, JsonValue)>, name: &str, value: JsonValue) {
    match members.iter_mut().find(|(member, _)| member == name) {
        Some((_, slot)) => *slot = value,
        None => members.push((name.to_owned(), value)),
    }
}

/// `value` as JSON text, two-space indented, members in their order,
/// ending with a newline.
fn render(value: &JsonValue) -> String {
    let mut text = String::new();
    render_into(value, 0, &mut text);
    text.push('\n');
    text
}

fn render_into(value: &JsonValue, depth: usize, text: &mut String) {
    let indent = |text: &mut String, depth: usize| {
        text.push('\n');
        text.push_str(&"  ".repeat(depth));
    };
    match value {
        JsonValue::Null => text.push_str("null"),
        JsonValue::Bool(value) => text.push_str(if *value { "true" } else { "false" }),
        JsonValue::Number(number) => text.push_str(number),
        JsonValue::String(value) => text.push_str(&json::string(value)),
        JsonValue::Array(items) if items.is_empty() => text.push_str("[]"),
        JsonValue::Object(members) if members.is_empty() => text.push_str("{}"),
        JsonValue::Array(items) => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                indent(text, depth + 1);
                render_into(item, depth + 1, text);
            }
            indent(text, depth);
            text.push(']');
        }
        JsonValue::Object(members) => {
            text.push('{');
            for (index, (name, member)) in members.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                indent(text, depth + 1);
                text.push_str(&json::string(name));
                text.push_str(": ");
                render_into(member, depth + 1, text);
            }
            indent(text, depth);
            text.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> JsonValue {
        json::parse(text, None).expect("json")
    }

    #[test]
    fn adds_both_handlers_to_nothing() {
        let settings = with_hooks(None, "/bin/memprove").expect("added");
        assert_eq!(
            render(&settings),
            r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/bin/memprove",
            "args": [
              "verify",
              "hook"
            ],
            "timeout": 10
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/bin/memprove",
            "args": [
              "verify",
              "hook"
            ],
            "timeout": 3600
          }
        ]
      }
    ]
  }
}
"#
        );
        assert_eq!(parse(&render(&settings)), settings);
    }

    #[test]
    fn keeps_what_is_there_and_replaces_its_own() {
        let before = parse(
            r#"{
  "env": {"A": "1"},
  "hooks": {
    "SessionStart": [
      {"hooks": [{"type": "command", "command": "/a/memprove", "args": ["hook", "session-start"], "timeout": 10}]},
      {"hooks": [{"type": "command", "command": "/old/memprove", "args": ["verify", "hook"]}]}
    ],
    "Stop": [
      {"hooks": [
        {"type": "command", "command": "notify.sh"},
        {"type": "command", "command": "/old/memprove", "args": ["verify", "hook"]}
      ]}
    ],
    "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "x"}]}]
  },
  "permissions": {"allow": ["Bash(ls:*)"]}
}"#,
        );
        let after = with_hooks(Some(&before), "/new/memprove").expect("edited");
        let text = render(&after);
        assert!(!text.contains("/old/memprove"));
        assert_eq!(text.matches("/new/memprove").count(), 2);
        for kept in [
            r#""A": "1""#,
            "session-start",
            "notify.sh",
            "PreToolUse",
            "Bash(ls:*)",
        ] {
            assert!(text.contains(kept), "{kept}");
        }
        let names: Vec<&str> = after
            .as_object()
            .expect("object")
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, ["env", "hooks", "permissions"]);
        assert_eq!(
            with_hooks(Some(&after), "/new/memprove").expect("again"),
            after,
            "a second run changes nothing"
        );
    }

    #[test]
    fn refuses_settings_it_cannot_add_to() {
        assert_eq!(
            with_hooks(Some(&parse("[]")), "m").expect_err("array"),
            "must be a JSON object"
        );
        assert_eq!(
            with_hooks(Some(&parse(r#"{"hooks": {"Stop": {}}}"#)), "m").expect_err("object"),
            "`hooks.Stop` must be an array"
        );
    }
}
