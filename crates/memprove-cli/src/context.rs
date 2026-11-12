//! The current context (P2-D1): per Workspace directory, the Project,
//! Workspace, agent, Task, Session, and latest Snapshot identities that
//! `memprove begin` and later commands recorded, so that an agent does not
//! repeat them on every command.
//!
//! One file per Workspace root, under the CLI configuration directory, never
//! inside the Workspace (it would be an untracked file and change how
//! Evidence is judged, Owner decision P2-D1). Files are mode 0600 in a 0700
//! directory, hold identities only, never a token, and are rejected when
//! group or others can access them.
//!
//! P4-E (ADR 0014 E5, Owner conditions j and k): with a valid
//! `MEMPROVE_CLIENT_SESSION`, each client session (a Claude Code session,
//! say) of a Workspace root has its own file,
//! `contexts/<sha256(root)>/<sha256(client session)>.json`, and its own
//! last-Pack file next to it; without the variable nothing changes.

use std::{
    ffi::OsString,
    fmt::{self, Write as _},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};

use crate::{json_string, run::sha256};

/// Environment variable naming the CLI configuration directory.
pub const CONFIG_DIR_ENV: &str = "MEMPROVE_CONFIG_DIR";
/// Environment variable naming the client session whose context the CLI
/// uses (P4-E, ADR 0014 E5).
pub const CLIENT_SESSION_ENV: &str = "MEMPROVE_CLIENT_SESSION";
/// The most characters a client session identifier has (Owner condition j).
pub const MAX_CLIENT_SESSION_CHARS: usize = 128;

/// Whether `value` is a client session identifier the CLI accepts (Owner
/// condition j): 1 to 128 characters, each an ASCII letter, digit, `_`, or
/// `-`. Nothing else can reach a file name or a shell line.
#[must_use]
pub fn is_client_session(value: &str) -> bool {
    (1..=MAX_CLIENT_SESSION_CHARS).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// U1 (A1, Owner O2): whether `value` is a client session identifier that
/// `memprove mcp serve` takes from `CLAUDE_CODE_SESSION_ID`: 1 to 128
/// characters, each an ASCII letter, digit, `.`, `_`, `:`, or `-`. It is a
/// superset of [`is_client_session`]. Such a value never reaches a file name
/// as it is: a client session's context file is named by the SHA-256 of its
/// bytes ([`ContextStore::path_for`]), so every accepted value gives a name
/// of 64 hexadecimal digits, and two values give the same name only when
/// they are the same value. The `SessionStart` hook names the file of the
/// `session_id` it is given the same way, so for one Claude Code session
/// the hook, the shell, and the server use one file.
#[must_use]
pub fn is_claude_code_session(value: &str) -> bool {
    (1..=MAX_CLIENT_SESSION_CHARS).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

/// U1 (A1): the client session named by the value of
/// `CLAUDE_CODE_SESSION_ID`: `Ok(None)` when it is not set.
///
/// # Errors
///
/// [`InvalidClientSession`] for a value [`is_claude_code_session`] refuses.
pub fn claude_code_session_of(
    value: Option<OsString>,
) -> Result<Option<String>, InvalidClientSession> {
    match value {
        None => Ok(None),
        Some(value) => value
            .into_string()
            .ok()
            .filter(|value| is_claude_code_session(value))
            .map(Some)
            .ok_or(InvalidClientSession),
    }
}

/// A value of [`CLIENT_SESSION_ENV`] that [`is_client_session`] refuses; it
/// is ignored (Owner condition j) and never shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidClientSession;

/// The client session named by the value of [`CLIENT_SESSION_ENV`]:
/// `Ok(None)` when it is not set.
///
/// # Errors
///
/// [`InvalidClientSession`] for a value [`is_client_session`] refuses.
pub fn client_session_of(value: Option<OsString>) -> Result<Option<String>, InvalidClientSession> {
    match value {
        None => Ok(None),
        Some(value) => value
            .into_string()
            .ok()
            .filter(|value| is_client_session(value))
            .map(Some)
            .ok_or(InvalidClientSession),
    }
}

/// Upper bound for one context file (P3-D: room for the run labels of a
/// state with many runs).
const MAX_CONTEXT_BYTES: u64 = 256 * 1024;

/// One run the latest Snapshot of the context seals, under the label an
/// agent cites it by (`--evidence r2`, P3-D). A run is citable while its
/// Evidence is in the latest Snapshot: its own run, or an earlier run of the
/// same repository state carried over into it (ADR 0013 D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLabel {
    /// `r` and a positive number, never reused within one context.
    pub label: String,
    /// The run's Evidence.
    pub evidence_id: String,
    /// Its result (`PASS`, `FAIL`, ...), when this context recorded it.
    pub status: Option<String>,
    /// SHA-256 of its argv, when this context recorded it (P3-D3): runs of
    /// one Snapshot with the same argv are runs of one command in one
    /// repository state.
    pub argv_sha256: Option<String>,
    /// Its Evidence kind (`test_result` or `command_result`), when this
    /// context recorded it (P4-B, T18 SF-1): `--reaffirm-all` matches a run
    /// only to Evidence of its kind. Absent from contexts written before.
    pub kind: Option<String>,
    /// U8 K5 (Owner O5): its argv, kept only for a command run this context
    /// recorded (and only when it is short, [`MAX_STORED_ARGV_BYTES`]), so
    /// that the refusal to cite it as a fact can say how to run it again.
    /// Absent from contexts written before.
    pub argv: Option<Vec<String>>,
}

/// U8 K5: the longest argv, as the JSON of its elements, a context keeps for
/// a command run; a longer one is not kept.
pub const MAX_STORED_ARGV_BYTES: usize = 1024;

/// The digest [`RunLabel::argv_sha256`] holds for `argv`: SHA-256 of its
/// elements, each as a JSON string, joined by commas.
#[must_use]
pub fn argv_digest(argv: &[String]) -> String {
    sha256(
        argv.iter()
            .map(|word| json_string(word))
            .collect::<Vec<_>>()
            .join(",")
            .as_bytes(),
    )
}

/// Whether `value` is spelled as a run label: `r` and a positive decimal
/// number without leading zeros.
#[must_use]
pub fn is_run_label(value: &str) -> bool {
    value.strip_prefix('r').is_some_and(|digits| {
        !digits.is_empty()
            && !digits.starts_with('0')
            && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// Whether `value` looks like a run label without necessarily being one:
/// `r` or `R` and one or more ASCII digits, nothing else (`r0`, `r01`,
/// `R1`). An Evidence ID is a UUID, which never starts with `r` or `R`, so
/// such a value can only be a mistyped label.
#[must_use]
pub fn looks_like_run_label(value: &str) -> bool {
    value.strip_prefix(['r', 'R']).is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// What `memprove begin` and later commands recorded for one Workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// Absolute, canonical Workspace root the context belongs to.
    pub workspace_root: PathBuf,
    /// Project of the Workspace.
    pub project_id: String,
    /// Workspace the root resolved to.
    pub workspace_id: String,
    /// Agent identity Sessions are created under.
    pub agent_id: String,
    /// Task the current or last Session continues.
    pub task_id: Option<String>,
    /// That Task's title, when known (the default Checkpoint objective).
    pub task_title: Option<String>,
    /// That Task's revision as last read (the default `--revision` of
    /// `task transition`).
    pub task_revision: Option<u64>,
    /// That Task's state as last read (`context show`, P2-H).
    pub task_state: Option<String>,
    /// Current Session; cleared when it ends.
    pub session_id: Option<String>,
    /// That Session's revision (the default `--revision` of `end`).
    pub session_revision: Option<u64>,
    /// Snapshot of the latest `memprove run` in the current Session.
    pub snapshot_id: Option<String>,
    /// That Snapshot's `capture_manifest_digest` (P4-A): the repository
    /// state its runs answer for; absent from contexts written before.
    pub snapshot_manifest: Option<String>,
    /// Evidence of that run: the default `--evidence` of `checkpoint create`
    /// and `task complete`, also written `--evidence last` (P2-H).
    pub evidence_id: Option<String>,
    /// That Evidence's result (`PASS`, `FAIL`, ...), as recorded.
    pub evidence_status: Option<String>,
    /// The runs the latest Snapshot seals, by label (P3-D).
    pub runs: Vec<RunLabel>,
    /// The number of the next label.
    pub next_run: u64,
    /// U8 A3 (Owner O4): the agent name the last `begin` declared; absent
    /// from contexts written before U8 and from those of a `begin` that
    /// declared none.
    pub agent_name: Option<String>,
    /// U8 A3: the agent's role, with `agent_name`.
    pub agent_role: Option<String>,
    /// U8 A3: the Session that dispatched the agent, when it named one.
    pub parent_session_id: Option<String>,
}

impl Context {
    /// The context as a JSON document.
    #[must_use]
    pub fn to_json(&self) -> String {
        let text = |value: &Option<String>| value.as_deref().map_or("null".to_owned(), json_string);
        let number = |value: Option<u64>| value.map_or("null".to_owned(), |n| n.to_string());
        let runs = self
            .runs
            .iter()
            .map(|run| {
                // P4-B: `kind` is written only when known, so a run without
                // it reads as before.
                let kind = run
                    .kind
                    .as_deref()
                    .map(|kind| format!(r#","kind":{}"#, json_string(kind)))
                    .unwrap_or_default();
                // U8 K5: `argv` likewise, for a command run only.
                let kind = match &run.argv {
                    Some(argv) => format!(
                        r#"{kind},"argv":[{}]"#,
                        argv.iter()
                            .map(|word| json_string(word))
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                    None => kind,
                };
                format!(
                    r#"{{"label":{},"evidence_id":{},"status":{},"argv_sha256":{}{kind}}}"#,
                    json_string(&run.label),
                    json_string(&run.evidence_id),
                    text(&run.status),
                    text(&run.argv_sha256)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let mut json = String::new();
        write!(
            json,
            r#"{{"schema_version":1,"workspace_root":{},"project_id":{},"workspace_id":{},"agent_id":{},"task_id":{},"task_title":{},"task_revision":{},"task_state":{},"session_id":{},"session_revision":{},"snapshot_id":{},"evidence_id":{},"evidence_status":{},"runs":[{runs}],"next_run":{}}}"#,
            json_string(&self.workspace_root.to_string_lossy()),
            json_string(&self.project_id),
            json_string(&self.workspace_id),
            json_string(&self.agent_id),
            text(&self.task_id),
            text(&self.task_title),
            number(self.task_revision),
            text(&self.task_state),
            text(&self.session_id),
            number(self.session_revision),
            text(&self.snapshot_id),
            text(&self.evidence_id),
            text(&self.evidence_status),
            self.next_run,
        )
        .expect("writing to a String cannot fail");
        // P4-A: written only when known, so a context without it reads as
        // before (`begin` prints none).
        if let Some(manifest) = &self.snapshot_manifest {
            json.pop();
            write!(json, r#","snapshot_manifest":{}}}"#, json_string(manifest))
                .expect("writing to a String cannot fail");
        }
        // U8 A3 (Owner O4): written only when known, so a context of a
        // Session that declared no agent is the bytes it was.
        for (name, value) in [
            ("agent_name", &self.agent_name),
            ("agent_role", &self.agent_role),
            ("parent_session_id", &self.parent_session_id),
        ] {
            if let Some(value) = value {
                json.pop();
                write!(json, r#","{name}":{}}}"#, json_string(value))
                    .expect("writing to a String cannot fail");
            }
        }
        json
    }

    /// U8 A3 (Owner O4): the context's agent as `context show` writes it,
    /// `None` when it records none.
    fn describe_agent(&self) -> Option<String> {
        let name = self.agent_name.as_deref()?;
        let parent = self
            .parent_session_id
            .as_deref()
            .map(|parent| format!(", dispatched by Session {parent}"))
            .unwrap_or_default();
        Some(format!(
            "Agent name:     {name} (role {}{parent})",
            self.agent_role.as_deref().unwrap_or("unknown")
        ))
    }

    /// Records the runs a new Snapshot seals (P3-D): `citable` is every
    /// command and test Evidence of the Snapshot, `subject` the run just
    /// recorded with its `status` and (P3-D3) the digest of its argv. A run
    /// this context already labelled keeps its label; an earlier run carried
    /// over from outside this context, and then the new run, get the next
    /// labels. Runs the Snapshot does not seal are no longer citable and are
    /// dropped. Returns the new run's label.
    pub fn label_runs(
        &mut self,
        citable: &[String],
        subject: &str,
        status: Option<String>,
        argv_sha256: Option<String>,
    ) -> String {
        let mut runs: Vec<RunLabel> = self
            .runs
            .iter()
            .filter(|run| citable.contains(&run.evidence_id))
            .cloned()
            .collect();
        let mut fresh = citable
            .iter()
            .filter(|id| id.as_str() != subject && !runs.iter().any(|run| &run.evidence_id == *id))
            .map(|id| (id.clone(), None, None))
            .collect::<Vec<_>>();
        if !runs.iter().any(|run| run.evidence_id == subject) {
            fresh.push((subject.to_owned(), status, argv_sha256));
        }
        for (evidence_id, status, argv_sha256) in fresh {
            let number = self.next_run.max(1);
            self.next_run = number + 1;
            runs.push(RunLabel {
                label: format!("r{number}"),
                evidence_id,
                status,
                argv_sha256,
                kind: None,
                argv: None,
            });
        }
        self.runs = runs;
        self.runs
            .iter()
            .find(|run| run.evidence_id == subject)
            .map_or_else(String::new, |run| run.label.clone())
    }

    /// P4-B: records the Evidence kind of the run `evidence_id` when this
    /// context labelled it without one (the run just recorded).
    pub fn note_run_kind(&mut self, evidence_id: &str, kind: Option<String>) {
        if let Some(run) = self
            .runs
            .iter_mut()
            .find(|run| run.evidence_id == evidence_id && run.kind.is_none())
        {
            run.kind = kind;
        }
    }

    /// U8 K5: records the argv of the run `evidence_id` when this context
    /// recorded it as a command run (`command_result`) and the argv is short
    /// ([`MAX_STORED_ARGV_BYTES`]); a test run needs none, and a longer one
    /// is not kept.
    pub fn note_run_argv(&mut self, evidence_id: &str, argv: &[String]) {
        let size: usize = argv.iter().map(|word| json_string(word).len() + 1).sum();
        if size > MAX_STORED_ARGV_BYTES {
            return;
        }
        if let Some(run) = self.runs.iter_mut().find(|run| {
            run.evidence_id == evidence_id
                && run.argv.is_none()
                && run.kind.as_deref() == Some("command_result")
        }) {
            run.argv = Some(argv.to_vec());
        }
    }

    /// U8 K5: the run, among the runs this context recorded as command runs
    /// with their argv, that the Evidence `evidence_id` is.
    #[must_use]
    pub fn command_run(&self, evidence_id: &str) -> Option<(&RunLabel, &[String])> {
        self.runs
            .iter()
            .rfind(|run| run.evidence_id == evidence_id)
            .filter(|run| run.kind.as_deref() == Some("command_result"))
            .and_then(|run| Some((run, run.argv.as_deref()?)))
    }

    /// U8 K5 (Owner O5): the first run a fact cites when every Evidence it
    /// cites is a command run this context recorded: such a fact has nothing
    /// that can support it. A fact that cites a test run too, or a run this
    /// context did not record (it may be a test run), or nothing, is the
    /// daemon's to judge; the extra command runs it cites are allowed.
    #[must_use]
    pub fn only_command_runs(&self, cited: &[String]) -> Option<(&RunLabel, &[String])> {
        let runs = cited
            .iter()
            .map(|id| self.command_run(id))
            .collect::<Option<Vec<_>>>()?;
        runs.into_iter().next()
    }

    /// The run a label names, when the latest Snapshot seals it.
    #[must_use]
    pub fn run(&self, label: &str) -> Option<&RunLabel> {
        self.runs.iter().find(|run| run.label == label)
    }

    /// P3-D3: the label of the later run that overrules the passed run
    /// `run`: the latest run this context recorded with the same argv in the
    /// latest Snapshot (so in the same Task, Workspace, and repository
    /// state), when it did not pass. Runs this context did not record have
    /// no known argv; the daemon applies the rule to every run.
    #[must_use]
    pub fn overruled_by(&self, run: &RunLabel) -> Option<&str> {
        let argv = run.argv_sha256.as_ref()?;
        if run.status.as_deref() != Some("PASS") {
            return None;
        }
        self.runs
            .iter()
            .rfind(|other| other.argv_sha256.as_ref() == Some(argv))
            .filter(|latest| latest.status.as_deref() != Some("PASS"))
            .map(|latest| latest.label.as_str())
    }

    /// P3-L L11: the runs `task complete` cites by default, in label order:
    /// the passed runs of the citable list that no later run of the same
    /// argv overrules. Runs without a result recorded by this context and
    /// runs that did not pass are not among them.
    #[must_use]
    pub fn completion_runs(&self) -> Vec<&RunLabel> {
        self.runs
            .iter()
            .filter(|run| run.status.as_deref() == Some("PASS") && self.overruled_by(run).is_none())
            .collect()
    }

    /// P3-L L11 patch: the runs that stand as the latest run of their
    /// command (same argv, the same Snapshot, so the same Task, Workspace,
    /// and repository state) with a result that is not `PASS`, in label
    /// order. A run whose argv this context never recorded is the latest run
    /// of a command of its own. A run without a result recorded here (one
    /// carried over from another Session) is no failure.
    #[must_use]
    pub fn failing_runs(&self) -> Vec<&RunLabel> {
        self.runs
            .iter()
            .enumerate()
            .filter(|(index, run)| {
                run.status.as_deref().is_some_and(|status| status != "PASS")
                    && run.argv_sha256.as_ref().is_none_or(|argv| {
                        self.runs[index + 1..]
                            .iter()
                            .all(|later| later.argv_sha256.as_ref() != Some(argv))
                    })
            })
            .map(|(_, run)| run)
            .collect()
    }

    /// The citable runs as `r1 (PASS), r2 (earlier run)`; P3-D3: a passed
    /// run a later run of the same argv overrules reads `r1 (PASS,
    /// overruled by r2)`.
    #[must_use]
    pub fn citable_runs(&self) -> String {
        self.runs
            .iter()
            .map(|run| {
                let status = run.status.as_deref().unwrap_or("earlier run");
                match self.overruled_by(run) {
                    Some(later) => format!("{} ({status}, overruled by {later})", run.label),
                    None => format!("{} ({status})", run.label),
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// P3-D3: one sentence per overruled passed run among the citable ones.
    #[must_use]
    pub fn overruled_runs(&self) -> Vec<String> {
        self.runs
            .iter()
            .filter_map(|run| {
                self.overruled_by(run).map(|later| {
                    format!(
                        "{} passed, but the later run {later} of the same argv in this repository \
                         state did not pass; {} cannot support a verified fact (cite {later} in \
                         an open item, or run the command again until it passes)",
                        run.label, run.label
                    )
                })
            })
            .collect()
    }

    /// What `memprove context show` prints (P2-H): one line per identity
    /// and the latest run, from this file alone. Never a token: the context
    /// holds none.
    #[must_use]
    pub fn describe(&self) -> String {
        let or_unknown = |value: Option<String>| value.unwrap_or_else(|| "unknown".to_owned());
        let task = match &self.task_id {
            Some(task_id) => format!(
                "{task_id} {} (state {}, revision {})",
                self.task_title
                    .as_deref()
                    .map_or_else(|| "(title unknown)".to_owned(), json_string),
                or_unknown(self.task_state.clone()),
                or_unknown(self.task_revision.map(|n| n.to_string())),
            ),
            None => "none; run `memprove begin --new-task <TITLE>`".to_owned(),
        };
        let session = match &self.session_id {
            Some(session_id) => format!(
                "{session_id} (revision {})",
                or_unknown(self.session_revision.map(|n| n.to_string()))
            ),
            None => "none; run `memprove begin` to start one".to_owned(),
        };
        let evidence = match &self.evidence_id {
            Some(evidence_id) => format!(
                "{evidence_id} ({})",
                or_unknown(self.evidence_status.clone())
            ),
            None => "none".to_owned(),
        };
        [
            format!("Workspace root: {}", self.workspace_root.display()),
            format!("Project:        {}", self.project_id),
            format!("Workspace:      {}", self.workspace_id),
            format!("Agent:          {}", self.agent_id),
        ]
        .into_iter()
        .chain(self.describe_agent())
        .chain([
            format!("Task:           {task}"),
            format!("Session:        {session}"),
            format!(
                "Snapshot:       {}",
                self.snapshot_id.as_deref().unwrap_or("none")
            ),
            format!("Evidence:       {evidence}"),
            format!(
                "Runs:           {}",
                if self.runs.is_empty() {
                    "none".to_owned()
                } else {
                    format!("{} (cite with --evidence <LABEL>)", self.citable_runs())
                }
            ),
        ])
        .collect::<Vec<_>>()
        .join("\n")
    }

    /// Parses a context document.
    ///
    /// # Errors
    ///
    /// Returns a description when the document is not a version 1 context.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let value = parse_strict(text).map_err(|error| format!("not JSON: {error}"))?;
        if value.get("schema_version").and_then(JsonValue::as_i64) != Some(1) {
            return Err("unsupported schema_version".to_owned());
        }
        let required = |name: &str| {
            value
                .get(name)
                .and_then(JsonValue::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{name} is missing"))
        };
        let optional = |name: &str| {
            value
                .get(name)
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        };
        let number = |name: &str| {
            value
                .get(name)
                .and_then(JsonValue::as_i64)
                .and_then(|n| u64::try_from(n).ok())
        };
        Ok(Self {
            workspace_root: PathBuf::from(required("workspace_root")?),
            project_id: required("project_id")?,
            workspace_id: required("workspace_id")?,
            agent_id: required("agent_id")?,
            task_id: optional("task_id"),
            task_title: optional("task_title"),
            task_revision: number("task_revision"),
            // Absent from contexts written before P2-H.
            task_state: optional("task_state"),
            session_id: optional("session_id"),
            session_revision: number("session_revision"),
            snapshot_id: optional("snapshot_id"),
            // Absent from contexts written before P4-A.
            snapshot_manifest: optional("snapshot_manifest"),
            evidence_id: optional("evidence_id"),
            evidence_status: optional("evidence_status"),
            // Absent from contexts written before P3-D.
            runs: runs(&value)?,
            next_run: number("next_run").unwrap_or(1),
            // Absent from contexts written before U8 (A3).
            agent_name: optional("agent_name"),
            agent_role: optional("agent_role"),
            parent_session_id: optional("parent_session_id"),
        })
    }
}

/// The `runs` of a context document; none when absent.
fn runs(value: &JsonValue) -> Result<Vec<RunLabel>, String> {
    let Some(items) = value.get("runs") else {
        return Ok(Vec::new());
    };
    let items = items.as_array().ok_or("runs is not an array")?;
    items
        .iter()
        .map(|item| {
            let text = |name: &str| {
                item.get(name)
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned)
            };
            let label = text("label")
                .filter(|label| is_run_label(label))
                .ok_or("a run label is missing or malformed")?;
            let evidence_id = text("evidence_id")
                .filter(|id| !id.is_empty())
                .ok_or("a run's evidence_id is missing")?;
            Ok(RunLabel {
                label,
                evidence_id,
                status: text("status"),
                argv_sha256: text("argv_sha256"),
                // Absent from contexts written before P4-B.
                kind: text("kind"),
                // Absent from contexts written before U8 K5.
                argv: item
                    .get("argv")
                    .and_then(JsonValue::as_array)
                    .and_then(|argv| {
                        argv.iter()
                            .map(|word| word.as_str().map(str::to_owned))
                            .collect::<Option<Vec<_>>>()
                    }),
            })
        })
        .collect()
}

/// Why a context could not be read or written. Messages name paths, never
/// file contents.
#[derive(Debug)]
pub enum ContextError {
    /// The file or directory could not be read or written.
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// The file grants group or other access, or is not a regular file.
    Insecure {
        /// Context file path.
        path: PathBuf,
    },
    /// The file is not a context document.
    Malformed {
        /// Context file path.
        path: PathBuf,
        /// What is wrong.
        detail: String,
    },
    /// The configuration directory is inside the Workspace.
    InsideWorkspace {
        /// Configuration directory.
        dir: PathBuf,
        /// Workspace root.
        root: PathBuf,
    },
}

impl fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "context {}: {source}", path.display())
            }
            Self::Insecure { path } => write!(
                formatter,
                "context file {} must be a regular file not accessible by group or others \
                 (chmod 600)",
                path.display()
            ),
            Self::Malformed { path, detail } => {
                write!(
                    formatter,
                    "context file {} is malformed: {detail}",
                    path.display()
                )
            }
            Self::InsideWorkspace { dir, root } => write!(
                formatter,
                "the configuration directory {} is inside the Workspace {}; a context file \
                 there would be an untracked file; set {CONFIG_DIR_ENV} to a directory outside it",
                dir.display(),
                root.display()
            ),
        }
    }
}

impl std::error::Error for ContextError {}

/// The directory that holds one context file per Workspace root (and, P4-E,
/// per client session of a Workspace root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextStore {
    /// The CLI configuration directory.
    pub config_dir: PathBuf,
    /// The client session whose context files this store reads and writes
    /// (P4-E, ADR 0014 E5); `None` for the directory's own file, as before.
    /// Always a value [`is_claude_code_session`] accepts (U1, A1): one
    /// [`is_client_session`] accepts, or one `mcp serve` took from
    /// `CLAUDE_CODE_SESSION_ID`.
    pub client_session: Option<String>,
}

/// The context that applies to the current directory.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)] // One lookup per process; boxing buys nothing.
pub enum ContextLookup {
    /// No lookup was made (library callers and tests).
    Unavailable,
    /// The current directory is inside the Workspace of this context.
    Found(Context),
    /// No context covers `cwd`.
    Absent {
        /// The directory searched from.
        cwd: PathBuf,
    },
    /// P4-E: in a client session with no context of its own here, the last
    /// Pack this client session was given (by the `SessionStart` hook or
    /// `resume --pack`): `begin` continues its Task, in its Project when the
    /// file names one. Every other command reads this as [`Self::Absent`].
    Unbegun {
        /// The directory searched from.
        cwd: PathBuf,
        /// The last Pack.
        pack: LastPack,
    },
    /// A context file exists but cannot be used.
    Broken(String),
}

impl ContextLookup {
    /// The context, when one applies.
    #[must_use]
    pub fn context(&self) -> Option<&Context> {
        match self {
            Self::Found(context) => Some(context),
            _ => None,
        }
    }

    /// P4-E: the last Pack of a client session that has no context here
    /// yet ([`Self::Unbegun`]).
    #[must_use]
    pub fn unbegun(&self) -> Option<&LastPack> {
        match self {
            Self::Unbegun { pack, .. } => Some(pack),
            _ => None,
        }
    }

    /// Why the context supplies nothing here, as a clause.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Unavailable => "there is no current context".to_owned(),
            Self::Found(context) => format!(
                "the current context of {} has none",
                context.workspace_root.display()
            ),
            Self::Absent { cwd } | Self::Unbegun { cwd, .. } => format!(
                "{} is not inside a Workspace with a current context",
                cwd.display()
            ),
            Self::Broken(reason) => reason.clone(),
        }
    }

    /// Why `flag` could not be taken from the context.
    #[must_use]
    pub fn missing(&self, command: &str, flag: &str) -> String {
        match self {
            Self::Unavailable => format!("{command} requires {flag}"),
            Self::Found(context) => format!(
                "{command} requires {flag}: the current context of {} has none; pass {flag} \
                 or run `memprove begin`",
                context.workspace_root.display()
            ),
            Self::Absent { cwd } | Self::Unbegun { cwd, .. } => format!(
                "{command} requires {flag}: {} is not inside a Workspace with a current \
                 context; pass {flag} or run `memprove begin` there",
                cwd.display()
            ),
            Self::Broken(reason) => format!("{command} requires {flag}: {reason}"),
        }
    }
}

/// U17 (M1 O4): the configuration directory of the default installation,
/// `scripts/install.sh` with no `--prefix`: `<prefix>/config` under
/// `$HOME/.memprove`. The binaries installed there need no environment
/// variable: this directory is found by its place alone.
pub const INSTALL_CONFIG_SUBDIR: &str = ".memprove/config";

/// U17 (M1 O4): `$HOME/.memprove/config` when `home` is set and that
/// directory exists; `None` otherwise. The one place the resolution of the
/// configuration directory reads the file system.
#[must_use]
pub fn installed_config_dir(home: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let home = home.filter(|home| !home.is_empty())?;
    let dir = PathBuf::from(home).join(INSTALL_CONFIG_SUBDIR);
    dir.is_dir().then_some(dir)
}

impl ContextStore {
    /// The configuration directory from `$MEMPROVE_CONFIG_DIR`, else (U17,
    /// M1 O4) the default installation's `$HOME/.memprove/config` when
    /// that directory exists ([`installed_config_dir`]), else
    /// `$XDG_CONFIG_HOME/memprove`, else `$HOME/.config/memprove`; empty
    /// values are ignored. `None` when none is set.
    #[must_use]
    pub fn from_env(
        config_dir: Option<OsString>,
        xdg_config_home: Option<OsString>,
        home: Option<OsString>,
    ) -> Option<Self> {
        let set = |value: Option<OsString>| value.filter(|value| !value.is_empty());
        let home = set(home);
        let config_dir = set(config_dir)
            .map(PathBuf::from)
            .or_else(|| installed_config_dir(home.as_deref()))
            .or_else(|| set(xdg_config_home).map(|dir| PathBuf::from(dir).join("memprove")))
            .or_else(|| home.map(|dir| PathBuf::from(dir).join(".config/memprove")))?;
        Some(Self {
            config_dir,
            client_session: None,
        })
    }

    /// The same configuration directory for the client session `client`
    /// (P4-E); `None` is the directory's own context. A value
    /// [`is_claude_code_session`] refuses is ignored (U1, A1: the set of
    /// `CLAUDE_CODE_SESSION_ID`, which holds every value
    /// [`is_client_session`] accepts; the file name is a digest either way).
    #[must_use]
    pub fn for_client(&self, client: Option<&str>) -> Self {
        Self {
            config_dir: self.config_dir.clone(),
            client_session: client
                .filter(|client| is_claude_code_session(client))
                .map(str::to_owned),
        }
    }

    fn contexts_dir(&self) -> PathBuf {
        self.config_dir.join("contexts")
    }

    /// The directory of the client sessions' files of the Workspace root
    /// `root` (P4-E): `contexts/<sha256(root)>`.
    fn clients_dir(&self, root: &Path) -> PathBuf {
        self.contexts_dir()
            .join(hex(root.as_os_str().as_encoded_bytes()))
    }

    /// The context file of the Workspace root `root`: the directory's own
    /// file, or (P4-E) the client session's file under
    /// `contexts/<sha256(root)>/`.
    #[must_use]
    pub fn path_for(&self, root: &Path) -> PathBuf {
        match &self.client_session {
            None => {
                let hex = hex(root.as_os_str().as_encoded_bytes());
                self.contexts_dir().join(format!("{hex}.json"))
            }
            Some(client) => self
                .clients_dir(root)
                .join(format!("{}.json", hex(client.as_bytes()))),
        }
    }

    /// Creates the 0700 directory a context or last-Pack file `path` goes
    /// in, and the `contexts` directory above it.
    fn private_directory(&self, path: &Path) -> Result<(), ContextError> {
        let io_error = |path: &Path| {
            let path = path.to_path_buf();
            move |source| ContextError::Io { path, source }
        };
        let contexts = self.contexts_dir();
        let directory = path.parent().unwrap_or(&contexts);
        fs::create_dir_all(directory).map_err(io_error(directory))?;
        for directory in [contexts.as_path(), directory] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .map_err(io_error(directory))?;
        }
        Ok(())
    }

    /// Every context recorded for the Workspace root `root` (P4-E, for the
    /// `SessionStart` hook's Owner condition m): the directory's own and each
    /// client session's, in file-name order. Files that cannot be used are
    /// left out; they are not this caller's to report.
    #[must_use]
    pub fn contexts_of(&self, root: &Path) -> Vec<Context> {
        self.context_files_of(root)
            .into_iter()
            .map(|(_, context)| context)
            .collect()
    }

    /// P4-G (ADR 0014 E7.5.2, rule D): [`ContextStore::contexts_of`] with
    /// the file each context was read from, so that the MCP server can
    /// write back the one file a call used.
    #[must_use]
    pub fn context_files_of(&self, root: &Path) -> Vec<(PathBuf, Context)> {
        let own = self.for_client(None).path_for(root);
        let mut paths = vec![own];
        if let Ok(entries) = fs::read_dir(self.clients_dir(root)) {
            let mut clients = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "json")
                        && !path.to_string_lossy().ends_with(".pack.json")
                })
                .collect::<Vec<_>>();
            clients.sort();
            paths.extend(clients);
        }
        paths
            .into_iter()
            .filter_map(|path| read_context(&path).ok().map(|context| (path, context)))
            .filter(|(_, context)| context.workspace_root == root)
            .collect()
    }

    /// The context of the nearest Workspace root at or above `cwd`.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError`] when `cwd` cannot be resolved or the nearest
    /// context file is unreadable, insecure, malformed, or recorded for
    /// another directory.
    pub fn find(&self, cwd: &Path) -> Result<Option<Context>, ContextError> {
        let cwd = fs::canonicalize(cwd).map_err(|source| ContextError::Io {
            path: cwd.to_path_buf(),
            source,
        })?;
        for directory in cwd.ancestors() {
            let path = self.path_for(directory);
            match fs::symlink_metadata(&path) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(ContextError::Io { path, source }),
            }
            let context = read_context(&path)?;
            if context.workspace_root != directory {
                return Err(ContextError::Malformed {
                    path,
                    detail: format!("recorded for {}", context.workspace_root.display()),
                });
            }
            return Ok(Some(context));
        }
        Ok(None)
    }

    /// The lookup for `cwd`, folding errors into [`ContextLookup::Broken`].
    /// P4-E: in a client session with no context of its own, the last Pack
    /// file of the nearest Workspace root that has one makes it
    /// [`ContextLookup::Unbegun`].
    #[must_use]
    pub fn lookup(&self, cwd: &Path) -> ContextLookup {
        match self.find(cwd) {
            Ok(Some(context)) => ContextLookup::Found(context),
            Ok(None) => {
                let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
                match self.unbegun(&cwd) {
                    Some(pack) => ContextLookup::Unbegun { cwd, pack },
                    None => ContextLookup::Absent { cwd },
                }
            }
            Err(error) => ContextLookup::Broken(error.to_string()),
        }
    }

    /// U1 (U-F, SF-1): whether this store's client session has a context
    /// file or a last-Pack file at or above `cwd` (the places
    /// [`ContextStore::lookup`] reads), whether or not it can be read.
    /// `false` for the directory's store.
    #[must_use]
    pub fn has_records(&self, cwd: &Path) -> bool {
        if self.client_session.is_none() {
            return false;
        }
        let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        cwd.ancestors().any(|directory| {
            fs::symlink_metadata(self.path_for(directory)).is_ok()
                || fs::symlink_metadata(self.last_pack_path(directory)).is_ok()
        })
    }

    /// P4-E: the last Pack of the nearest Workspace root at or above `cwd`
    /// that has one, in a client session only; a file that cannot be read
    /// counts as none (`begin` warns about it when it reads it).
    fn unbegun(&self, cwd: &Path) -> Option<LastPack> {
        self.client_session.as_ref()?;
        cwd.ancestors()
            .find(|directory| fs::symlink_metadata(self.last_pack_path(directory)).is_ok())
            .and_then(|directory| self.last_pack(directory).ok().flatten())
    }

    /// Refuses a configuration directory inside the Workspace `root`.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::InsideWorkspace`] when it is inside.
    pub fn check_outside(&self, root: &Path) -> Result<(), ContextError> {
        if nearest_existing(&self.contexts_dir()).starts_with(root) {
            return Err(ContextError::InsideWorkspace {
                dir: self.config_dir.clone(),
                root: root.to_path_buf(),
            });
        }
        Ok(())
    }

    /// Writes `context` atomically, mode 0600, in a 0700 directory outside
    /// the Workspace.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::InsideWorkspace`] when the configuration
    /// directory is inside the context's Workspace, and [`ContextError::Io`]
    /// when the directory or file cannot be written.
    pub fn save(&self, context: &Context) -> Result<PathBuf, ContextError> {
        self.save_at(self.path_for(&context.workspace_root), context)
    }

    /// P4-G (ADR 0014 E7.5.2): writes `context` to `path`, a context file
    /// of this configuration directory that [`ContextStore::context_files_of`]
    /// named, as [`ContextStore::save`] writes its own file.
    ///
    /// # Errors
    ///
    /// As [`ContextStore::save`].
    pub fn save_at(&self, path: PathBuf, context: &Context) -> Result<PathBuf, ContextError> {
        self.check_outside(&context.workspace_root)?;
        self.private_directory(&path)?;
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .and_then(|mut file| {
                file.write_all(context.to_json().as_bytes())?;
                file.write_all(b"\n")?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&temporary, &path));
        if let Err(source) = written {
            let _ = fs::remove_file(&temporary);
            return Err(ContextError::Io { path, source });
        }
        Ok(path)
    }
}

/// ADR 0014 E6 (P4-F, Owner condition i): the last full Context Pack that
/// `resume --pack` printed in a Workspace, which `begin` sends as
/// `resumed_from` when it continues the same Task. Kept next to the
/// context file, mode 0600; identities and digests only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastPack {
    /// The Task the Pack is of.
    pub task_id: String,
    /// What `resumed_from` declares.
    pub resumed_from: memprove_domain::ResumedFrom,
    /// The Project of the Task, when the writer knew it (P4-E: the
    /// `SessionStart` hook writes it, so that `begin` in a client session
    /// with no context of its own needs no `--project`).
    pub project_id: Option<String>,
}

impl LastPack {
    /// The file's JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        let from = &self.resumed_from;
        // P4-E: `project_id` only when known, so a file without it reads
        // and writes as before.
        let project = self
            .project_id
            .as_deref()
            .map(|project| format!(r#","project_id":{}"#, json_string(project)))
            .unwrap_or_default();
        format!(
            r#"{{"task_id":{},"capsule_digest":"sha256:{}","project_sequence":{},"pack_digest":"sha256:{}","budget":{}{project}}}"#,
            json_string(&self.task_id),
            from.capsule_digest,
            from.project_sequence,
            from.pack_digest,
            from.budget
        )
    }

    /// The `resumed_from` member of a `createSession` body.
    #[must_use]
    pub fn resumed_from_json(&self) -> String {
        let from = &self.resumed_from;
        format!(
            r#"{{"capsule_digest":"sha256:{}","project_sequence":{},"pack_digest":"sha256:{}","budget":{}}}"#,
            from.capsule_digest, from.project_sequence, from.pack_digest, from.budget
        )
    }

    /// Reads the file's JSON: exactly the five members, checked as the
    /// daemon checks `resumed_from`, and (P4-E) optionally `project_id`.
    ///
    /// # Errors
    ///
    /// What is wrong with it.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let value = parse_strict(text).map_err(|error| error.to_string())?;
        let members = value.as_object().ok_or("not a JSON object")?;
        let known = [
            "task_id",
            "capsule_digest",
            "project_sequence",
            "pack_digest",
            "budget",
        ];
        let project = value.get("project_id");
        if members.len() != known.len() + usize::from(project.is_some())
            || members
                .iter()
                .any(|(name, _)| !known.contains(&name.as_str()) && name != "project_id")
        {
            return Err(format!(
                "expected exactly the members {}, and optionally project_id",
                known.join(", ")
            ));
        }
        let project_id = match project {
            None => None,
            Some(project) => Some(
                project
                    .as_str()
                    .filter(|id| crate::claims::is_uuid_v7(id))
                    .ok_or("project_id is not a UUIDv7")?
                    .to_owned(),
            ),
        };
        let text = |name: &str| value.get(name).and_then(JsonValue::as_str);
        let number = |name: &str| {
            value
                .get(name)
                .and_then(JsonValue::as_i64)
                .and_then(|number| u64::try_from(number).ok())
        };
        let task_id = text("task_id")
            .filter(|id| crate::claims::is_uuid_v7(id))
            .ok_or("task_id is not a UUIDv7")?
            .to_owned();
        let resumed_from = memprove_domain::ResumedFrom::new(
            text("capsule_digest"),
            number("project_sequence"),
            text("pack_digest"),
            number("budget"),
        )
        .map_err(|refusal| refusal.0)?;
        Ok(Self {
            task_id,
            resumed_from,
            project_id,
        })
    }
}

/// U1 (U-F, SF-1R): the member that marks a last-Pack file as a record of
/// no Pack.
const NO_PACK: &str = "no_pack";

/// What a last-Pack file records.
enum PackRecord {
    /// A Pack that was given (`resume --pack`, the `pack` tool, or the
    /// `SessionStart` hook).
    Pack(LastPack),
    /// U1 (U-F, SF-1R): no Pack: the `SessionStart` hook exported the
    /// client session but gave it no Pack; with the Project of the
    /// Workspace root when the contexts there named one.
    NoPack { project_id: Option<String> },
}

/// U1 (U-F, SF-1R): `None` when `text` is not a record of no Pack (it has
/// no `no_pack` member); else the Project it names, or what is wrong with
/// it: exactly `"no_pack": true` and optionally a `UUIDv7` `project_id`.
fn no_pack_of(text: &str) -> Option<Result<Option<String>, String>> {
    let value = parse_strict(text).ok()?;
    let members = value.as_object()?;
    if !members.iter().any(|(name, _)| name == NO_PACK) {
        return None;
    }
    let project = value.get("project_id");
    Some(
        if value.get(NO_PACK) != Some(&JsonValue::Bool(true))
            || members.len() != 1 + usize::from(project.is_some())
        {
            Err(format!(
                "a record of no Pack has exactly the member {NO_PACK} (true), and optionally \
                 project_id"
            ))
        } else {
            match project {
                None => Ok(None),
                Some(project) => project
                    .as_str()
                    .filter(|id| crate::claims::is_uuid_v7(id))
                    .map(|id| Some(id.to_owned()))
                    .ok_or_else(|| "project_id is not a UUIDv7".to_owned()),
            }
        },
    )
}

impl ContextStore {
    /// The file of the last Pack of the Workspace root `root`, next to its
    /// context file.
    #[must_use]
    pub fn last_pack_path(&self, root: &Path) -> PathBuf {
        self.path_for(root).with_extension("pack.json")
    }

    /// Writes the last Pack of `root` atomically, mode 0600, in the 0700
    /// directory of the context files, outside the Workspace.
    ///
    /// # Errors
    ///
    /// [`ContextError::InsideWorkspace`] or [`ContextError::Io`].
    pub fn save_last_pack(&self, root: &Path, pack: &LastPack) -> Result<PathBuf, ContextError> {
        self.save_pack_record(root, &pack.to_json())
    }

    /// U1 (U-F, SF-1R): writes the last-Pack file of `root` as a record of
    /// no Pack (`{"no_pack":true}`, with `project_id` when given), as
    /// [`ContextStore::save_last_pack`] writes a Pack. The `SessionStart`
    /// hook writes it for a client session it exported to the shell but
    /// gave no Pack, so that the client session has a record (which
    /// `memprove mcp serve` looks for) without declaring a Pack:
    /// [`ContextStore::last_pack`] reads it as no Pack.
    ///
    /// # Errors
    ///
    /// [`ContextError::InsideWorkspace`] or [`ContextError::Io`].
    pub fn save_no_pack(
        &self,
        root: &Path,
        project_id: Option<&str>,
    ) -> Result<PathBuf, ContextError> {
        let project = project_id
            .map(|project| format!(r#","project_id":{}"#, json_string(project)))
            .unwrap_or_default();
        self.save_pack_record(root, &format!(r#"{{"{NO_PACK}":true{project}}}"#))
    }

    fn save_pack_record(&self, root: &Path, json: &str) -> Result<PathBuf, ContextError> {
        self.check_outside(root)?;
        let path = self.last_pack_path(root);
        self.private_directory(&path)?;
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .and_then(|mut file| {
                file.write_all(json.as_bytes())?;
                file.write_all(b"\n")?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&temporary, &path));
        if let Err(source) = written {
            let _ = fs::remove_file(&temporary);
            return Err(ContextError::Io { path, source });
        }
        Ok(path)
    }

    /// The last Pack of `root`; `None` when there is no file, and (U1, U-F,
    /// SF-1R) when the file records no Pack.
    ///
    /// # Errors
    ///
    /// [`ContextError`] when the file is unreadable, insecure, or malformed.
    pub fn last_pack(&self, root: &Path) -> Result<Option<LastPack>, ContextError> {
        Ok(match self.pack_record(root)? {
            Some(PackRecord::Pack(pack)) => Some(pack),
            Some(PackRecord::NoPack { .. }) | None => None,
        })
    }

    /// U1 (U-F, SF-1R): the Project the last-Pack file of `root` names,
    /// whether it records a Pack or no Pack; `None` when there is no file,
    /// it names none, or it cannot be read.
    #[must_use]
    pub fn recorded_project(&self, root: &Path) -> Option<String> {
        match self.pack_record(root).ok().flatten()? {
            PackRecord::Pack(pack) => pack.project_id,
            PackRecord::NoPack { project_id } => project_id,
        }
    }

    /// U2 (H1, Owner O5): the Project this client session's last-Pack
    /// record names, of a Pack or of no Pack, at the nearest Workspace root
    /// at or above `cwd` that has a last-Pack file (where
    /// [`ContextStore::lookup`] reads one); `None` for the directory's
    /// store, and when there is no file, it names none, or it cannot be
    /// read. Only the shell's `begin` asks for it.
    #[must_use]
    pub fn recorded_project_near(&self, cwd: &Path) -> Option<String> {
        self.client_session.as_ref()?;
        let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        cwd.ancestors()
            .find(|directory| fs::symlink_metadata(self.last_pack_path(directory)).is_ok())
            .and_then(|directory| self.recorded_project(directory))
    }

    /// U2 (H2, N-12): whether the last-Pack file of `root` exists but
    /// cannot be used: it cannot be read, is not a regular file of mode
    /// 0600 or stricter (a symbolic link included), or is neither a record
    /// of a Pack nor a record of no Pack. U3 (K3, V-N-2): a directory there
    /// is not one: no file can replace it, so it is left alone.
    #[must_use]
    pub fn has_damaged_record(&self, root: &Path) -> bool {
        !self.record_is_directory(root)
            && fs::symlink_metadata(self.last_pack_path(root)).is_ok()
            && self.pack_record(root).is_err()
    }

    /// U3 (K3, V-N-2): whether the last-Pack location of `root` is a
    /// directory (the entry itself; a symbolic link is not followed).
    #[must_use]
    pub fn record_is_directory(&self, root: &Path) -> bool {
        fs::symlink_metadata(self.last_pack_path(root)).is_ok_and(|metadata| metadata.is_dir())
    }

    /// The last-Pack file of `root`: a Pack, or (U1, U-F, SF-1R) no Pack.
    fn pack_record(&self, root: &Path) -> Result<Option<PackRecord>, ContextError> {
        let path = self.last_pack_path(root);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(ContextError::Io { path, source }),
        };
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(ContextError::Insecure { path });
        }
        let mut text = String::new();
        File::open(&path)
            .and_then(|file| file.take(MAX_CONTEXT_BYTES).read_to_string(&mut text))
            .map_err(|source| ContextError::Io {
                path: path.clone(),
                source,
            })?;
        let text = text.trim_end();
        match no_pack_of(text) {
            Some(Ok(project_id)) => Ok(Some(PackRecord::NoPack { project_id })),
            Some(Err(detail)) => Err(ContextError::Malformed { path, detail }),
            None => LastPack::from_json(text)
                .map(|pack| Some(PackRecord::Pack(pack)))
                .map_err(|detail| ContextError::Malformed { path, detail }),
        }
    }

    /// Removes the last Pack of `root` once a Session declared it, so that
    /// a later Session does not declare a Pack it was not given.
    ///
    /// # Errors
    ///
    /// [`ContextError::Io`] when it exists and cannot be removed.
    pub fn forget_last_pack(&self, root: &Path) -> Result<(), ContextError> {
        let path = self.last_pack_path(root);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(ContextError::Io { path, source }),
        }
    }
}

/// The lowercase hexadecimal SHA-256 of `bytes`.
fn hex(bytes: &[u8]) -> String {
    let digest = sha256(bytes);
    digest
        .strip_prefix("sha256:")
        .map_or_else(|| digest.clone(), str::to_owned)
}

/// `path`, or its nearest existing ancestor, canonicalized, with the missing
/// components appended.
fn nearest_existing(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        if let Ok(resolved) = fs::canonicalize(current) {
            return missing
                .iter()
                .rev()
                .fold(resolved, |resolved, part| resolved.join(part));
        }
        match (current.parent(), current.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                current = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn read_context(path: &Path) -> Result<Context, ContextError> {
    let io_error = |source| ContextError::Io {
        path: path.to_path_buf(),
        source,
    };
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(ContextError::Insecure {
            path: path.to_path_buf(),
        });
    }
    let mut text = String::new();
    File::open(path)
        .and_then(|file| file.take(MAX_CONTEXT_BYTES).read_to_string(&mut text))
        .map_err(io_error)?;
    Context::from_json(&text).map_err(|detail| ContextError::Malformed {
        path: path.to_path_buf(),
        detail,
    })
}

/// The Workspace root of `cwd`: the top level of its Git worktree, run in
/// the caller's environment, else `cwd` itself; canonical either way.
///
/// # Errors
///
/// Returns the I/O error when `cwd` cannot be resolved.
pub fn workspace_root(cwd: &Path) -> io::Result<PathBuf> {
    let top = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim_end_matches(['\n', '\r']).to_owned())
        .filter(|text| !text.is_empty());
    fs::canonicalize(top.map_or_else(|| cwd.to_path_buf(), PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(root: &Path) -> Context {
        Context {
            workspace_root: root.to_path_buf(),
            project_id: "018f47a0-0000-7000-8000-000000000001".to_owned(),
            workspace_id: "018f47a0-0000-7000-8000-000000000002".to_owned(),
            agent_id: "018f47a0-0000-7000-8000-000000000003".to_owned(),
            task_id: Some("018f47a0-0000-7000-8000-000000000004".to_owned()),
            task_title: Some("Make \"it\" pass".to_owned()),
            task_revision: Some(2),
            task_state: Some("in_progress".to_owned()),
            session_id: Some("018f47a0-0000-7000-8000-000000000005".to_owned()),
            session_revision: Some(1),
            snapshot_id: None,
            snapshot_manifest: None,
            evidence_id: Some("018f47a0-0000-7000-8000-000000000006".to_owned()),
            evidence_status: Some("PASS".to_owned()),
            runs: vec![RunLabel {
                label: "r1".to_owned(),
                evidence_id: "018f47a0-0000-7000-8000-000000000006".to_owned(),
                status: Some("PASS".to_owned()),
                argv_sha256: None,
                kind: None,
                argv: None,
            }],
            next_run: 2,
            agent_name: None,
            agent_role: None,
            parent_session_id: None,
        }
    }

    /// U8 K5 (Owner O5): a context keeps the argv of a command run it
    /// recorded, and only of that: not of a test run, not of a run of
    /// unknown kind, and not one longer than [`MAX_STORED_ARGV_BYTES`]. It
    /// round-trips through the context file, and a file without it reads as
    /// before.
    #[test]
    fn a_command_run_keeps_its_short_argv_and_a_test_run_does_not() {
        let base = tempfile::tempdir().unwrap();
        let mut context = sample(base.path());
        context.runs.clear();
        let word = |text: &str| text.to_owned();
        let command = vec![word("sh"), word("-c"), word("echo \"é\"")];
        let ids: Vec<String> = (0..4)
            .map(|n| format!("018f47a0-0000-7000-8000-00000000001{n}"))
            .collect();
        let cases = [
            (Some("command_result"), command.clone()),
            (Some("test_result"), vec![word("pytest")]),
            (None, vec![word("ls")]),
            (Some("command_result"), vec!["x".repeat(2000)]),
        ];
        for (at, (kind, argv)) in cases.iter().enumerate() {
            context.label_runs(&ids[..=at], &ids[at], Some("PASS".to_owned()), None);
            context.note_run_kind(&ids[at], kind.map(str::to_owned));
            context.note_run_argv(&ids[at], argv);
        }
        let kept: Vec<Option<&[String]>> =
            context.runs.iter().map(|run| run.argv.as_deref()).collect();
        assert_eq!(kept, [Some(&command[..]), None, None, None]);
        // `command_run` finds it by Evidence id only for a command run.
        let first = context.runs[0].evidence_id.clone();
        assert_eq!(
            context
                .command_run(&first)
                .map(|(run, argv)| (run.label.as_str(), argv)),
            Some(("r2", &command[..]))
        );
        assert!(context.command_run(&context.runs[1].evidence_id).is_none());
        assert!(
            context
                .command_run("018f47a0-0000-7000-8000-00000000ffff")
                .is_none()
        );
        // A fact is held back only when every citation is such a run: a test
        // run, a run of unknown kind, or none among them leaves it to the
        // daemon.
        let (command_id, test_id) = (first.clone(), context.runs[1].evidence_id.clone());
        let unknown = "018f47a0-0000-7000-8000-00000000ffff".to_owned();
        let only = |cited: &[&String]| {
            let cited: Vec<String> = cited.iter().map(|id| (*id).clone()).collect();
            context
                .only_command_runs(&cited)
                .map(|(run, _)| run.label.clone())
        };
        assert_eq!(only(&[&command_id]), Some("r2".to_owned()));
        assert_eq!(only(&[&command_id, &command_id]), Some("r2".to_owned()));
        assert_eq!(only(&[&command_id, &test_id]), None);
        assert_eq!(only(&[&test_id, &command_id]), None);
        assert_eq!(only(&[&command_id, &unknown]), None);
        assert_eq!(only(&[&unknown]), None);
        assert_eq!(only(&[]), None);
        let again = Context::from_json(&context.to_json()).unwrap();
        assert_eq!(again.runs, context.runs);
        // A file written before this item has no member.
        let old = context
            .to_json()
            .replace(r#","argv":["sh","-c","echo \"é\""]"#, "");
        assert!(!old.contains("\"argv\":["));
        assert_eq!(Context::from_json(&old).unwrap().runs[0].argv, None);
    }

    /// P4-F (Owner condition i): the last Pack round-trips through its
    /// 0600 file next to the context file, is removed once declared, and a
    /// damaged file is refused, never guessed.
    #[test]
    fn the_last_pack_round_trips_and_a_damaged_one_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(base.path()).unwrap();
        let root = base.join("work");
        fs::create_dir_all(&root).unwrap();
        let store = ContextStore {
            config_dir: base.join("config"),
            client_session: None,
        };
        assert_eq!(store.last_pack(&root).unwrap(), None);
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let text = format!(
            r#"{{"task_id":"018f47a0-0000-7000-8000-000000000004","capsule_digest":"{}","project_sequence":42,"pack_digest":"{}","budget":9000}}"#,
            digest('a'),
            digest('b')
        );
        let pack = LastPack::from_json(&text).unwrap();
        assert_eq!(pack.to_json(), text);
        assert_eq!(
            pack.resumed_from_json(),
            format!(
                r#"{{"capsule_digest":"{}","project_sequence":42,"pack_digest":"{}","budget":9000}}"#,
                digest('a'),
                digest('b')
            )
        );
        let path = store.save_last_pack(&root, &pack).unwrap();
        assert_eq!(path, store.last_pack_path(&root));
        assert!(path.to_string_lossy().ends_with(".pack.json"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(store.last_pack(&root).unwrap(), Some(pack));
        store.forget_last_pack(&root).unwrap();
        assert_eq!(store.last_pack(&root).unwrap(), None);
        store.forget_last_pack(&root).unwrap();
        for damaged in [
            "{}".to_owned(),
            "not json".to_owned(),
            text.replace("9000", "999"),
            text.replace(&digest('a'), "sha256:a"),
            text.replace("018f47a0-0000-7000-8000-000000000004", "task"),
            text.replace('}', r#","extra":1}"#),
        ] {
            assert!(LastPack::from_json(&damaged).is_err(), "{damaged}");
            fs::write(&path, &damaged).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(matches!(
                store.last_pack(&root),
                Err(ContextError::Malformed { .. })
            ));
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            store.last_pack(&root),
            Err(ContextError::Insecure { .. })
        ));
    }

    /// U1 (U-F, SF-1R): a record of no Pack counts as a record of the
    /// client session, reads as no Pack (so `begin` declares none and
    /// `lookup` stays absent), keeps its Project, and is replaced by a
    /// Pack; a damaged one is refused like a damaged Pack.
    #[test]
    fn a_record_of_no_pack_is_a_record_but_no_pack() {
        let base = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(base.path()).unwrap();
        let root = base.join("work");
        fs::create_dir_all(&root).unwrap();
        let store = ContextStore {
            config_dir: base.join("config"),
            client_session: None,
        }
        .for_client(Some("client-x"));
        let project = "018f47a0-0000-7000-8000-000000000001";
        assert!(!store.has_records(&root));
        let path = store.save_no_pack(&root, Some(project)).unwrap();
        assert_eq!(path, store.last_pack_path(&root));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{{\"no_pack\":true,\"project_id\":\"{project}\"}}\n")
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(store.has_records(&root));
        assert_eq!(store.last_pack(&root).unwrap(), None);
        assert_eq!(store.recorded_project(&root).as_deref(), Some(project));
        assert!(matches!(store.lookup(&root), ContextLookup::Absent { .. }));
        store.save_no_pack(&root, None).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"no_pack\":true}\n");
        assert_eq!(store.recorded_project(&root), None);
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let pack = LastPack::from_json(&format!(
            r#"{{"task_id":"018f47a0-0000-7000-8000-000000000004","capsule_digest":"{}","project_sequence":42,"pack_digest":"{}","budget":9000,"project_id":"{project}"}}"#,
            digest('a'),
            digest('b')
        ))
        .unwrap();
        store.save_last_pack(&root, &pack).unwrap();
        assert_eq!(store.last_pack(&root).unwrap(), Some(pack));
        assert_eq!(store.recorded_project(&root).as_deref(), Some(project));
        for damaged in [
            r#"{"no_pack":false}"#.to_owned(),
            r#"{"no_pack":1}"#.to_owned(),
            r#"{"no_pack":true,"project_id":"p"}"#.to_owned(),
            r#"{"no_pack":true,"extra":1}"#.to_owned(),
            format!(r#"{{"no_pack":true,"task_id":"{project}"}}"#),
        ] {
            fs::write(&path, &damaged).unwrap();
            assert!(
                matches!(store.last_pack(&root), Err(ContextError::Malformed { .. })),
                "{damaged}"
            );
            assert_eq!(store.recorded_project(&root), None, "{damaged}");
        }
    }

    /// U2 (H1, H2): a damaged last-Pack record is told apart from a missing
    /// or readable one, and the shell's `begin` finds the Project of the
    /// record at the nearest root above the directory, in a client session
    /// only.
    #[test]
    fn a_damaged_record_is_told_apart_and_the_nearest_record_names_the_project() {
        let base = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(base.path()).unwrap();
        let root = base.join("work");
        let deeper = root.join("src/deeper");
        fs::create_dir_all(&deeper).unwrap();
        let directory = ContextStore {
            config_dir: base.join("config"),
            client_session: None,
        };
        let store = directory.for_client(Some("client-x"));
        let project = "018f47a0-0000-7000-8000-000000000001";
        assert!(!store.has_damaged_record(&root));
        assert_eq!(store.recorded_project_near(&deeper), None);
        let path = store.save_no_pack(&root, Some(project)).unwrap();
        assert!(!store.has_damaged_record(&root));
        assert_eq!(
            store.recorded_project_near(&deeper).as_deref(),
            Some(project)
        );
        directory.save_no_pack(&root, Some(project)).unwrap();
        assert_eq!(directory.recorded_project_near(&deeper), None);
        fs::write(&path, "{not json").unwrap();
        assert!(store.has_damaged_record(&root));
        assert_eq!(store.recorded_project_near(&deeper), None);
        store.save_no_pack(&root, Some(project)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.has_damaged_record(&root));
        fs::remove_file(&path).unwrap();
        let elsewhere = base.join("elsewhere.json");
        fs::write(
            &elsewhere,
            format!("{{\"no_pack\":true,\"project_id\":\"{project}\"}}\n"),
        )
        .unwrap();
        fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert!(store.has_damaged_record(&root));
    }

    /// U1 (A1, Owner O2): `CLAUDE_CODE_SESSION_ID` takes 1 to 128
    /// characters of `[A-Za-z0-9._:-]`, a superset of condition j's set; its
    /// file is named by the digest of its bytes, so a `.` or `:` never
    /// reaches a path, two values never share a file, and the value the
    /// hook was given names the same file for the server as for the shell.
    #[test]
    fn a_claude_code_session_id_names_a_file_by_its_digest() {
        for valid in ["a", "A-z_09", "cc.1:x", "..", ":", &"x".repeat(128)] {
            assert!(is_claude_code_session(valid), "{valid}");
        }
        for invalid in [
            "",
            "a b",
            "a/b",
            "../x",
            "$(x)",
            "a'b",
            "é",
            &"x".repeat(129),
        ] {
            assert!(!is_claude_code_session(invalid), "{invalid}");
        }
        assert_eq!(claude_code_session_of(None), Ok(None));
        assert_eq!(
            claude_code_session_of(Some("cc.1:x".into())),
            Ok(Some("cc.1:x".to_owned()))
        );
        assert_eq!(
            claude_code_session_of(Some("a/b".into())),
            Err(InvalidClientSession)
        );
        let directory = ContextStore {
            config_dir: PathBuf::from("/c"),
            client_session: None,
        };
        let root = Path::new("/w");
        let mut names = Vec::new();
        for value in ["cc.1:x", "cc.1-x", "..", "s-1"] {
            let path = directory.for_client(Some(value)).path_for(root);
            assert_eq!(path.parent().unwrap(), directory.clients_dir(root));
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            assert_eq!(name, format!("{}.json", hex(value.as_bytes())));
            assert!(!names.contains(&name), "{name}");
            names.push(name);
        }
        assert_eq!(directory.for_client(Some("a/b")), directory);
    }

    /// P4-E (Owner conditions j and k): a valid client session has its own
    /// context and last-Pack files under `contexts/<sha256(root)>/`; the
    /// directory's file is untouched, an invalid value is no client session,
    /// and a client session without a context of its own but with a last
    /// Pack is `Unbegun`.
    #[test]
    fn a_client_session_has_its_own_files() {
        for valid in ["a", "A-z_09", &"x".repeat(128)] {
            assert!(is_client_session(valid), "{valid}");
        }
        for invalid in [
            "",
            "a b",
            "a/b",
            "../x",
            "$(x)",
            "a'b",
            "é",
            &"x".repeat(129),
        ] {
            assert!(!is_client_session(invalid), "{invalid}");
        }
        assert_eq!(client_session_of(None), Ok(None));
        assert_eq!(
            client_session_of(Some("s-1".into())),
            Ok(Some("s-1".to_owned()))
        );
        assert_eq!(
            client_session_of(Some("s 1".into())),
            Err(InvalidClientSession)
        );
        let base = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(base.path()).unwrap();
        let root = base.join("work");
        fs::create_dir_all(root.join("src")).unwrap();
        let directory = ContextStore {
            config_dir: base.join("config"),
            client_session: None,
        };
        let client = directory.for_client(Some("s-1"));
        assert_eq!(directory.for_client(Some("a/b")), directory);
        let path = client.save(&sample(&root)).unwrap();
        assert_eq!(path.parent().unwrap(), directory.clients_dir(&root));
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(client.find(&root.join("src")).unwrap(), Some(sample(&root)));
        assert_eq!(directory.find(&root).unwrap(), None);
        assert_eq!(directory.contexts_of(&root), vec![sample(&root)]);
        let other = directory.for_client(Some("s-2"));
        assert!(matches!(other.lookup(&root), ContextLookup::Absent { .. }));
        let text = r#"{"task_id":"018f47a0-0000-7000-8000-000000000004","capsule_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","project_sequence":4,"pack_digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","budget":9000,"project_id":"018f47a0-0000-7000-8000-000000000001"}"#;
        let pack = LastPack::from_json(text).unwrap();
        assert_eq!(pack.to_json(), text);
        other.save_last_pack(&root, &pack).unwrap();
        assert!(
            other
                .last_pack_path(&root)
                .starts_with(directory.clients_dir(&root))
        );
        assert_eq!(other.lookup(&root).unbegun(), Some(&pack));
        // The directory's own lookup never reads a client's last Pack.
        assert!(matches!(
            directory.lookup(&root),
            ContextLookup::Absent { .. }
        ));
    }

    #[test]
    fn a_context_round_trips_and_is_found_from_subdirectories() {
        let base = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(base.path()).unwrap();
        let root = base.join("work");
        fs::create_dir_all(root.join("src/deep")).unwrap();
        let store = ContextStore {
            config_dir: base.join("config"),
            client_session: None,
        };
        let context = sample(&root);
        assert_eq!(Context::from_json(&context.to_json()).unwrap(), context);
        let path = store.save(&context).unwrap();
        assert!(!path.starts_with(&root));
        assert_eq!(store.find(&root).unwrap().as_ref(), Some(&context));
        assert_eq!(
            store.find(&root.join("src/deep")).unwrap(),
            Some(context.clone())
        );
        // A rewrite replaces the file and keeps its mode.
        let mut ended = context;
        ended.session_id = None;
        store.save(&ended).unwrap();
        assert_eq!(store.find(&root).unwrap(), Some(ended));
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.to_lowercase().contains("token"));
    }

    /// P3-D: each run the latest Snapshot seals keeps its label; a carried
    /// run this context never saw, then the new run, take the next numbers;
    /// runs of another state are dropped and their labels never reused.
    #[test]
    fn run_labels_follow_the_latest_snapshot() {
        let mut context = sample(Path::new("/work"));
        context.runs.clear();
        context.next_run = 1;
        let id = |n: u8| format!("018f47a0-0000-7000-8000-0000000001{n:02}");
        assert_eq!(
            context.label_runs(&[id(1)], &id(1), Some("PASS".to_owned()), None),
            "r1"
        );
        assert_eq!(
            context.label_runs(&[id(1), id(2)], &id(2), Some("FAIL".to_owned()), None),
            "r2"
        );
        assert_eq!(context.citable_runs(), "r1 (PASS), r2 (FAIL)");
        // Another state: only the new run; then a carried run from outside.
        assert_eq!(
            context.label_runs(&[id(3)], &id(3), Some("PASS".to_owned()), None),
            "r3"
        );
        assert_eq!(context.citable_runs(), "r3 (PASS)");
        assert_eq!(
            context.label_runs(
                &[id(3), id(9), id(4)],
                &id(4),
                Some("PASS".to_owned()),
                None
            ),
            "r5"
        );
        assert_eq!(
            context.citable_runs(),
            "r3 (PASS), r4 (earlier run), r5 (PASS)"
        );
        assert_eq!(context.run("r4").unwrap().evidence_id, id(9));
        assert!(context.run("r1").is_none());
        let back = Context::from_json(&context.to_json()).unwrap();
        assert_eq!(back, context);
        assert!(context.describe().contains(
            "Runs:           r3 (PASS), r4 (earlier run), r5 (PASS) (cite with --evidence <LABEL>)"
        ));
        let malformed = context.to_json().replace("\"r4\"", "\"x4\"");
        assert!(Context::from_json(&malformed).is_err());
    }

    /// P3-D3: a passed run is marked overruled while the latest run this
    /// context recorded with the same argv in the latest Snapshot did not
    /// pass; another argv, or a later passed run, leaves it citable.
    #[test]
    fn a_later_run_of_the_same_argv_that_did_not_pass_overrules_a_pass() {
        let mut context = sample(Path::new("/work"));
        context.runs.clear();
        context.next_run = 1;
        let id = |n: u8| format!("018f47a0-0000-7000-8000-0000000002{n:02}");
        let check = argv_digest(&["sh".to_owned(), "-c".to_owned(), "test -w lib".to_owned()]);
        let other = argv_digest(&["cargo".to_owned(), "test".to_owned()]);
        assert_ne!(check, other);
        let pass = || Some("PASS".to_owned());
        let fail = || Some("FAIL".to_owned());
        context.label_runs(&[id(1)], &id(1), pass(), Some(check.clone()));
        context.label_runs(&[id(1), id(2)], &id(2), fail(), Some(other.clone()));
        assert_eq!(context.citable_runs(), "r1 (PASS), r2 (FAIL)");
        assert!(context.overruled_runs().is_empty());
        context.label_runs(&[id(1), id(2), id(3)], &id(3), fail(), Some(check.clone()));
        assert_eq!(
            context.citable_runs(),
            "r1 (PASS, overruled by r3), r2 (FAIL), r3 (FAIL)"
        );
        assert_eq!(context.overruled_runs().len(), 1);
        assert!(context.overruled_runs()[0].starts_with("r1 passed, but the later run r3"));
        let back = Context::from_json(&context.to_json()).unwrap();
        assert_eq!(back, context);
        assert!(context.describe().contains("r1 (PASS, overruled by r3)"));
        // A later passed run of the same argv restores the earlier one.
        context.label_runs(&[id(1), id(2), id(3), id(4)], &id(4), pass(), Some(check));
        assert_eq!(
            context.citable_runs(),
            "r1 (PASS), r2 (FAIL), r3 (FAIL), r4 (PASS)"
        );
        // A run whose argv this context never saw marks nothing.
        context.label_runs(&[id(4), id(5), id(6)], &id(6), fail(), None);
        assert_eq!(
            context.citable_runs(),
            "r4 (PASS), r5 (earlier run), r6 (FAIL)"
        );
    }

    /// P2-H: a context written before the Task state and latest Evidence
    /// were recorded still reads, with those fields unknown.
    #[test]
    fn a_context_without_the_p2h_fields_still_reads() {
        let old = r#"{"schema_version":1,"workspace_root":"/w","project_id":"p","workspace_id":"w","agent_id":"a","task_id":"t","task_title":"T","task_revision":2,"session_id":null,"session_revision":null,"snapshot_id":"n"}"#;
        let context = Context::from_json(old).unwrap();
        assert_eq!(context.task_state, None);
        assert_eq!(context.evidence_id, None);
        assert_eq!(context.evidence_status, None);
        assert_eq!(context.snapshot_id.as_deref(), Some("n"));
        // P3-D: nor the run labels.
        assert_eq!((context.runs.len(), context.next_run), (0, 1));
    }

    /// P2-H item 5: `context show` names the Project, Workspace, Task (title,
    /// state, revision), Session, latest Snapshot and Evidence.
    #[test]
    fn context_show_describes_every_identity_and_the_latest_run() {
        let mut context = sample(Path::new("/work"));
        context.snapshot_id = Some("018f47a0-0000-7000-8000-000000000007".to_owned());
        let shown = context.describe();
        for expected in [
            "Workspace root: /work",
            "Project:        018f47a0-0000-7000-8000-000000000001",
            "Workspace:      018f47a0-0000-7000-8000-000000000002",
            "Task:           018f47a0-0000-7000-8000-000000000004 \"Make \\\"it\\\" pass\" \
             (state in_progress, revision 2)",
            "Session:        018f47a0-0000-7000-8000-000000000005 (revision 1)",
            "Snapshot:       018f47a0-0000-7000-8000-000000000007",
            "Evidence:       018f47a0-0000-7000-8000-000000000006 (PASS)",
        ] {
            assert!(shown.contains(expected), "{expected}\n{shown}");
        }
        assert!(!shown.to_lowercase().contains("token"), "{shown}");
        context.session_id = None;
        context.evidence_id = None;
        context.task_state = None;
        let shown = context.describe();
        assert!(shown.contains("Session:        none"), "{shown}");
        assert!(shown.contains("Evidence:       none"), "{shown}");
        assert!(shown.contains("(state unknown, revision 2)"), "{shown}");
    }

    #[test]
    fn a_directory_outside_every_workspace_has_no_context() {
        let base = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(base.path()).unwrap();
        let (root, elsewhere) = (base.join("work"), base.join("elsewhere"));
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        let store = ContextStore {
            config_dir: base.join("config"),
            client_session: None,
        };
        store.save(&sample(&root)).unwrap();
        assert_eq!(store.find(&elsewhere).unwrap(), None);
        let lookup = store.lookup(&elsewhere);
        assert_eq!(
            lookup,
            ContextLookup::Absent {
                cwd: elsewhere.clone()
            }
        );
        let message = lookup.missing("run", "--session");
        assert!(message.contains("is not inside a Workspace"), "{message}");
        assert!(
            message.contains(&elsewhere.display().to_string()),
            "{message}"
        );
    }

    #[test]
    fn context_files_are_private_and_insecure_ones_are_refused() {
        let base = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(base.path()).unwrap();
        let root = base.join("work");
        fs::create_dir_all(&root).unwrap();
        let store = ContextStore {
            config_dir: base.join("config"),
            client_session: None,
        };
        let path = store.save(&sample(&root)).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            store.find(&root),
            Err(ContextError::Insecure { .. })
        ));
        assert!(matches!(store.lookup(&root), ContextLookup::Broken(_)));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, "{\"schema_version\":2}").unwrap();
        assert!(matches!(
            store.find(&root),
            Err(ContextError::Malformed { .. })
        ));
    }

    #[test]
    fn the_configuration_directory_may_not_be_inside_the_workspace() {
        let base = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(base.path()).unwrap();
        let store = ContextStore {
            config_dir: root.join(".config/memprove"),
            client_session: None,
        };
        assert!(matches!(
            store.save(&sample(&root)),
            Err(ContextError::InsideWorkspace { .. })
        ));
        assert!(!root.join(".config").exists());
    }

    #[test]
    fn the_configuration_directory_follows_the_environment() {
        let store = |a: Option<&str>, b: Option<&str>, c: Option<&str>| {
            ContextStore::from_env(a.map(Into::into), b.map(Into::into), c.map(Into::into))
                .map(|store| store.config_dir)
        };
        assert_eq!(store(Some("/c"), Some("/x"), Some("/h")), Some("/c".into()));
        assert_eq!(
            store(Some(""), Some("/x"), Some("/h")),
            Some("/x/memprove".into())
        );
        assert_eq!(
            store(None, None, Some("/h")),
            Some("/h/.config/memprove".into())
        );
        assert_eq!(store(None, None, None), None);
    }
}
