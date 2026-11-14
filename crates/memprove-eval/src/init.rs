//! `memprove eval init`: the self-serve setup. It writes what a repository
//! needs to start evaluating (an `evals/` directory with a guide addressed
//! to the coding agent and a first task, a CI workflow, a short note in
//! `AGENTS.md` / `CLAUDE.md`), so that the developer's own agent does the
//! rest.
//!
//! Every file is written only when its text would change, so a second run
//! changes nothing. The files init owns whole (`evals/README.md`,
//! `evals/smoke.json`, the workflow) are never overwritten once they hold
//! something else: the person may have edited them, and they are reported
//! in [`InitReport::kept`]. In `AGENTS.md` / `CLAUDE.md` init owns only the
//! lines between [`templates::BEGIN`] and [`templates::END`], which it
//! replaces in place.

mod detect;
pub mod templates;

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub use detect::{Detected, ProjectKind, detect};

use crate::{
    AgentKind, CommandSpec, EvalError, TASK_SCHEMA, Task,
    json::{Out, string},
};

/// What `init` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitSpec {
    /// The worktree root.
    pub repo_root: PathBuf,
    /// The agent the workflow and the first task are written for.
    pub agent: AgentKind,
    /// Also write `.github/workflows/memprove-eval.yml`.
    pub workflow: bool,
    /// Print what would be written; write nothing.
    pub dry_run: bool,
}

/// What `init` did, for its output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitReport {
    /// Files written or changed, relative to the root (in a dry run: the
    /// files that would be).
    pub written: Vec<PathBuf>,
    /// Files that already said the same, relative to the root.
    pub unchanged: Vec<PathBuf>,
    /// Files init owns whole that exist with other text, left as they are,
    /// relative to the root.
    pub kept: Vec<PathBuf>,
    /// Sentences for the person (for example the secret the workflow
    /// needs).
    pub notes: Vec<String>,
}

/// The directory of the task files, relative to the root.
pub const EVALS_DIR: &str = "evals";
/// The workflow file, relative to the root.
pub const WORKFLOW_PATH: &str = ".github/workflows/memprove-eval.yml";

/// The agent's time limit `init` and `mine` write into a task.
pub const AGENT_TIMEOUT: Duration = Duration::from_secs(1800);
/// The check's time limit `init` and `mine` write into a task.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(1800);
/// A setup command's time limit `init` and `mine` write into a task.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(900);

/// The prompt of the first task, `evals/smoke.json`.
pub const SMOKE_PROMPT: &str =
    "Run the test suite; if anything fails, fix it; otherwise change nothing.";

/// How init treats a file that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ownership {
    /// The whole file is init's, but only while it is absent or the same.
    Whole,
    /// Only the marked block is init's.
    Block,
}

/// Sets up `spec.repo_root` for `memprove eval`. Running it again changes
/// nothing.
///
/// # Errors
/// [`EvalError::Usage`] for the `command` agent with a workflow (the
/// workflow cannot know the program); [`EvalError::Io`] when a file cannot
/// be read or written; [`EvalError::Invalid`] when `AGENTS.md` or
/// `CLAUDE.md` has a begin marker without an end marker.
pub fn init(spec: &InitSpec) -> Result<InitReport, EvalError> {
    if spec.workflow && spec.agent == AgentKind::Command {
        return Err(EvalError::Usage(
            "memprove eval init writes a workflow for claude-code or codex; pass --agent claude-code, --agent codex, or --no-workflow".to_owned(),
        ));
    }
    let root = &spec.repo_root;
    let mut report = InitReport::default();
    let mut files: Vec<(PathBuf, String, Ownership)> = vec![(
        Path::new(EVALS_DIR).join("README.md"),
        templates::readme(),
        Ownership::Whole,
    )];

    match detect(root) {
        Some(found) => {
            files.push((
                Path::new(EVALS_DIR).join("smoke.json"),
                render_task(&smoke_task(&found)),
                Ownership::Whole,
            ));
            report.notes.push(format!(
                "The first task, evals/smoke.json, checks with `{}` (from {}).",
                found.check.join(" "),
                found.marker
            ));
            if !found.also.is_empty() {
                report.notes.push(format!(
                    "Also found {}; {} comes first in the order {}.",
                    found.also.join(", "),
                    found.marker,
                    detect::ORDER
                ));
            }
        }
        None => report.notes.push(format!(
            "No test command was found (looked for {}), so evals/smoke.json was not written; write a first task as evals/README.md says.",
            detect::ORDER
        )),
    }

    if spec.workflow {
        files.push((
            PathBuf::from(WORKFLOW_PATH),
            templates::workflow(spec.agent),
            Ownership::Whole,
        ));
        let (secret, _) = templates::agent_secret(spec.agent);
        report.notes.push(format!(
            "The workflow needs the repository secret {secret} (Settings > Secrets and variables > Actions); it runs by hand or on a pull request labelled `eval`, since a real agent costs money."
        ));
    }

    for name in instruction_files(root) {
        let path = root.join(&name);
        let current = read_optional(&path)?.unwrap_or_default();
        let block = templates::agents_block(spec.agent);
        let text = templates::splice(&current, &block)
            .map_err(|detail| EvalError::invalid(Some(&path), detail))?;
        files.push((name, text, Ownership::Block));
    }

    for (relative, text, ownership) in files {
        let path = root.join(&relative);
        match (read_optional(&path)?, ownership) {
            (Some(current), _) if current == text => report.unchanged.push(relative),
            (Some(_), Ownership::Whole) => {
                report.notes.push(format!(
                    "{} differs from what init writes and was left as it is; remove it and run init again to get the new one.",
                    relative.display()
                ));
                report.kept.push(relative);
            }
            _ => {
                if !spec.dry_run {
                    write(&path, &text)?;
                }
                report.written.push(relative);
            }
        }
    }
    if spec.dry_run {
        report
            .notes
            .push("Dry run: nothing was written.".to_owned());
    }
    Ok(report)
}

/// The repository's test command, guessed from its files (`Cargo.toml`,
/// `package.json`, `pyproject.toml`, `go.mod`, ...); `None` when there is
/// no guess. See [`detect`] for the rules and the setup the command needs.
#[must_use]
pub fn detect_check(repo_root: &Path) -> Option<Vec<String>> {
    detect(repo_root).map(|found| found.check)
}

/// The text of a task file for `task`, as `init` and `mine` write it:
/// members in the order of the format, the optional ones left out when
/// empty. [`Task::source`] is not written.
#[must_use]
pub fn render_task(task: &Task) -> String {
    let command = |spec: &CommandSpec| {
        Out::Object(vec![
            ("argv", Out::strs(&spec.argv)),
            ("timeout_secs", Out::uint(spec.timeout.as_secs())),
        ])
    };
    let mut members = vec![
        ("schema", Out::str(TASK_SCHEMA)),
        ("id", Out::str(&task.id)),
    ];
    if let Some(title) = &task.title {
        members.push(("title", Out::str(title)));
    }
    if let Some(base) = &task.base {
        members.push(("base", Out::str(base)));
    }
    members.push(("prompt", Out::str(&task.prompt)));
    if !task.setup.is_empty() {
        members.push((
            "setup",
            Out::Array(task.setup.iter().map(command).collect()),
        ));
    }
    members.push(("check", command(&task.check)));
    if let Some(patch) = &task.check_patch {
        members.push(("check_patch", Out::Raw(string(&slashed(patch)))));
    }
    members.push((
        "agent_timeout_secs",
        Out::uint(task.agent_timeout.as_secs()),
    ));
    if !task.tags.is_empty() {
        members.push(("tags", Out::strs(&task.tags)));
    }
    Out::Object(members).render()
}

/// `path` with `/` between its parts, as task files name paths.
pub(crate) fn slashed(path: &Path) -> String {
    path.iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The setup commands of `found` as task commands.
pub(crate) fn setup_commands(found: &Detected) -> Vec<CommandSpec> {
    found
        .setup
        .iter()
        .map(|argv| CommandSpec {
            argv: argv.clone(),
            timeout: SETUP_TIMEOUT,
        })
        .collect()
}

fn smoke_task(found: &Detected) -> Task {
    Task {
        id: "smoke".to_owned(),
        title: Some("the test suite passes".to_owned()),
        base: None,
        prompt: SMOKE_PROMPT.to_owned(),
        setup: setup_commands(found),
        check: CommandSpec {
            argv: found.check.clone(),
            timeout: CHECK_TIMEOUT,
        },
        check_patch: None,
        agent_timeout: AGENT_TIMEOUT,
        tags: vec!["smoke".to_owned()],
        source: Path::new(EVALS_DIR).join("smoke.json"),
    }
}

/// `AGENTS.md` and/or `CLAUDE.md`, whichever exist; `AGENTS.md` when
/// neither does.
fn instruction_files(root: &Path) -> Vec<PathBuf> {
    let present: Vec<PathBuf> = ["AGENTS.md", "CLAUDE.md"]
        .into_iter()
        .filter(|name| root.join(name).is_file())
        .map(PathBuf::from)
        .collect();
    if present.is_empty() {
        vec![PathBuf::from("AGENTS.md")]
    } else {
        present
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, EvalError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(EvalError::io(path, error)),
    }
}

fn write(path: &Path, text: &str) -> Result<(), EvalError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| EvalError::io(parent, error))?;
    }
    fs::write(path, text).map_err(|error| EvalError::io(path, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(root: &Path) -> InitSpec {
        InitSpec {
            repo_root: root.to_path_buf(),
            agent: AgentKind::ClaudeCode,
            workflow: true,
            dry_run: false,
        }
    }

    #[test]
    fn a_task_renders_its_members_in_the_format_order() {
        let found = Detected {
            kind: ProjectKind::Node,
            marker: "package.json",
            setup: vec![vec!["npm".to_owned(), "ci".to_owned()]],
            check: vec!["npm".to_owned(), "test".to_owned()],
            also: Vec::new(),
        };
        let text = render_task(&smoke_task(&found));
        let order = [
            "\"schema\"",
            "\"id\"",
            "\"title\"",
            "\"prompt\"",
            "\"setup\"",
            "\"check\"",
            "\"agent_timeout_secs\"",
            "\"tags\"",
        ];
        let positions: Vec<usize> = order
            .iter()
            .map(|member| text.find(member).expect(member))
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
        assert!(!text.contains("\"base\""), "{text}");
        assert!(!text.contains("\"check_patch\""), "{text}");
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn the_command_agent_needs_no_workflow() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut spec = spec(dir.path());
        spec.agent = AgentKind::Command;
        assert!(matches!(init(&spec), Err(EvalError::Usage(_))));
        spec.workflow = false;
        let report = init(&spec).expect("init");
        assert!(!dir.path().join(WORKFLOW_PATH).exists());
        assert!(report.written.contains(&PathBuf::from("AGENTS.md")));
    }

    #[test]
    fn an_edited_whole_file_is_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        init(&spec(dir.path())).expect("first");
        let readme = dir.path().join("evals/README.md");
        fs::write(&readme, "my own notes\n").expect("edit");
        let report = init(&spec(dir.path())).expect("second");
        assert_eq!(report.kept, [PathBuf::from("evals/README.md")]);
        assert!(report.written.is_empty(), "{report:?}");
        assert_eq!(fs::read_to_string(&readme).expect("read"), "my own notes\n");
    }

    #[test]
    fn paths_in_task_files_use_slashes() {
        assert_eq!(
            slashed(&Path::new("evals").join("a.check.patch")),
            "evals/a.check.patch"
        );
    }
}
