//! The contract (`.memprove/verify.json`, schema
//! `memprove.verify.contract.v1`): what must hold before work counts as
//! done.

use std::{
    path::{Component, Path},
    time::Duration,
};

use memprove_eval::{
    CommandSpec,
    json::{self, Out, Reader},
    task::{DEFAULT_CHECK_TIMEOUT_SECS, DEFAULT_SETUP_TIMEOUT_SECS},
};

use crate::VerifyError;

/// The `schema` member of a contract.
pub const CONTRACT_SCHEMA: &str = "memprove.verify.contract.v1";
/// The contract file, relative to the repository root.
pub const CONTRACT_PATH: &str = ".memprove/verify.json";

const MEMBERS: &[&str] = &["schema", "checks", "setup", "protected", "skip_markers"];
const CHECK_MEMBERS: &[&str] = &["name", "argv", "timeout_secs"];
const COMMAND_MEMBERS: &[&str] = &["argv", "timeout_secs"];

/// One command that must exit 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Unique in the contract: 1 to 64 of `a-z`, `0-9`, `-`, `_`;
    /// `check-<n>` when the contract names none.
    pub name: String,
    /// The command and its time limit.
    pub command: CommandSpec,
}

/// What must hold before work counts as done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    /// The commands that must exit 0; at least one.
    pub checks: Vec<Check>,
    /// Commands a new working copy needs first (installing packages), in
    /// order.
    pub setup: Vec<CommandSpec>,
    /// The protected paths, as Git glob patterns relative to the
    /// repository root (`tests/**`): what the work should not need to
    /// change to pass.
    pub protected: Vec<String>,
    /// Whether a skip marker added outside the protected paths leaves the
    /// verdict to a person; `true` when the contract does not say.
    pub skip_markers: bool,
}

impl Contract {
    /// Reads a contract from `text` (the contents of `source`, for
    /// messages).
    ///
    /// # Errors
    /// [`VerifyError::Invalid`] naming the member that is wrong.
    pub fn parse(text: &str, source: Option<&Path>) -> Result<Self, VerifyError> {
        let value = json::parse(text, source)?;
        let reader = Reader::new(&value, source, "")?;
        reader.deny_unknown(MEMBERS)?;
        let schema = reader.str("schema")?;
        if schema != CONTRACT_SCHEMA {
            return Err(reader.error(
                "schema",
                &format!("must be {CONTRACT_SCHEMA}, not {schema}"),
            ));
        }
        let items = reader.array("checks")?;
        if items.is_empty() {
            return Err(reader.error("checks", "must name at least one command"));
        }
        let mut checks: Vec<Check> = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let at = format!("checks[{index}]");
            let entry = Reader::new(item, source, &at)?;
            entry.deny_unknown(CHECK_MEMBERS)?;
            let name = entry
                .opt_str("name")?
                .map_or_else(|| format!("check-{}", index + 1), str::to_owned);
            if !valid_name(&name) {
                return Err(entry.error(
                    "name",
                    &format!("must be 1 to 64 of a-z, 0-9, - and _, not {name:?}"),
                ));
            }
            if checks.iter().any(|check| check.name == name) {
                return Err(entry.error("name", &format!("is used twice: {name}")));
            }
            checks.push(Check {
                name,
                command: command(&entry, DEFAULT_CHECK_TIMEOUT_SECS)?,
            });
        }
        let mut setup = Vec::new();
        for (index, item) in reader.array("setup")?.iter().enumerate() {
            let at = format!("setup[{index}]");
            let entry = Reader::new(item, source, &at)?;
            entry.deny_unknown(COMMAND_MEMBERS)?;
            setup.push(command(&entry, DEFAULT_SETUP_TIMEOUT_SECS)?);
        }
        let protected = reader.strs("protected")?;
        if let Some(pattern) = protected.iter().find(|pattern| !valid_pattern(pattern)) {
            return Err(reader.error(
                "protected",
                &format!(
                    "holds {pattern:?}: a pattern is a path relative to the repository root, \
                     without .. and without a leading / or :"
                ),
            ));
        }
        Ok(Self {
            checks,
            setup,
            protected,
            skip_markers: reader.opt_bool("skip_markers")?.unwrap_or(true),
        })
    }

    /// The contract as the text of a contract file.
    #[must_use]
    pub fn render(&self) -> String {
        let command = |spec: &CommandSpec| {
            vec![
                ("argv", Out::strs(&spec.argv)),
                ("timeout_secs", Out::uint(spec.timeout.as_secs())),
            ]
        };
        let mut members = vec![
            ("schema", Out::str(CONTRACT_SCHEMA)),
            (
                "checks",
                Out::Array(
                    self.checks
                        .iter()
                        .map(|check| {
                            let mut members = vec![("name", Out::str(&check.name))];
                            members.extend(command(&check.command));
                            Out::Object(members)
                        })
                        .collect(),
                ),
            ),
            (
                "setup",
                Out::Array(
                    self.setup
                        .iter()
                        .map(|spec| Out::Object(command(spec)))
                        .collect(),
                ),
            ),
            ("protected", Out::strs(&self.protected)),
        ];
        if !self.skip_markers {
            members.push(("skip_markers", Out::bool(false)));
        }
        Out::Object(members).render()
    }
}

/// Whether `name` can name a check: 1 to 64 of `a-z`, `0-9`, `-`, `_` (it
/// names the check's log files).
#[must_use]
pub fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

/// Whether `pattern` can be a protected pattern: a relative path without
/// `..`, not starting with `/` or `:` (Git's pathspec magic), on one line.
fn valid_pattern(pattern: &str) -> bool {
    !pattern.is_empty()
        && !pattern.starts_with([':', '/'])
        && !pattern.contains(['\0', '\n'])
        && Path::new(pattern)
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn command(entry: &Reader<'_>, default_timeout: u64) -> Result<CommandSpec, VerifyError> {
    let argv = entry.strs("argv")?;
    if argv.first().is_none_or(String::is_empty) {
        return Err(entry.error("argv", "must name a program: a non-empty array of strings"));
    }
    let timeout = entry.opt_u64("timeout_secs")?.unwrap_or(default_timeout);
    if timeout == 0 {
        return Err(entry.error("timeout_secs", "must be at least 1"));
    }
    Ok(CommandSpec {
        argv,
        timeout: Duration::from_secs(timeout),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{
  "schema": "memprove.verify.contract.v1",
  "checks": [
    {"name": "tests", "argv": ["cargo", "test"], "timeout_secs": 1200},
    {"argv": ["cargo", "fmt", "--check"]}
  ],
  "setup": [{"argv": ["npm", "ci"]}],
  "protected": ["tests/**", "**/*_test.go"],
  "skip_markers": false
}"#;

    #[test]
    fn reads_every_member_and_fills_defaults() {
        let contract = Contract::parse(FULL, None).expect("valid");
        assert_eq!(contract.checks.len(), 2);
        assert_eq!(contract.checks[0].name, "tests");
        assert_eq!(
            contract.checks[0].command.timeout,
            Duration::from_secs(1200)
        );
        assert_eq!(contract.checks[1].name, "check-2");
        assert_eq!(
            contract.checks[1].command.timeout,
            Duration::from_secs(DEFAULT_CHECK_TIMEOUT_SECS)
        );
        assert_eq!(
            contract.setup[0].timeout,
            Duration::from_secs(DEFAULT_SETUP_TIMEOUT_SECS)
        );
        assert_eq!(contract.protected, ["tests/**", "**/*_test.go"]);
        assert!(!contract.skip_markers);
    }

    #[test]
    fn a_rendered_contract_reads_back_the_same() {
        let contract = Contract::parse(FULL, None).expect("valid");
        let again = Contract::parse(&contract.render(), None).expect("round trip");
        assert_eq!(again, contract);
        let minimal = Contract::parse(
            r#"{"schema":"memprove.verify.contract.v1","checks":[{"argv":["true"]}]}"#,
            None,
        )
        .expect("minimal");
        assert!(minimal.skip_markers && minimal.protected.is_empty());
        assert!(!minimal.render().contains("skip_markers"));
    }

    #[test]
    fn refuses_what_is_wrong_and_names_the_member() {
        let path = Path::new(CONTRACT_PATH);
        let refused = |text: &str| {
            Contract::parse(text, Some(path))
                .expect_err("refused")
                .to_string()
        };
        assert_eq!(
            refused(r#"{"schema":"memprove.verify.contract.v1","checks":[]}"#),
            ".memprove/verify.json: `checks` must name at least one command"
        );
        assert_eq!(
            refused(r#"{"schema":"x","checks":[{"argv":["true"]}]}"#),
            ".memprove/verify.json: `schema` must be memprove.verify.contract.v1, not x"
        );
        assert!(
            refused(
                r#"{"schema":"memprove.verify.contract.v1","checks":[{"name":"a","argv":["true"]},{"name":"a","argv":["true"]}]}"#
            )
            .ends_with("`checks[1].name` is used twice: a")
        );
        assert!(
            refused(
                r#"{"schema":"memprove.verify.contract.v1","checks":[{"name":"A b","argv":["true"]}]}"#
            )
            .contains("`checks[0].name` must be 1 to 64")
        );
        assert!(
            refused(r#"{"schema":"memprove.verify.contract.v1","checks":[{"argv":[]}]}"#)
                .contains("`checks[0].argv` must name a program")
        );
        for pattern in ["../x", "/abs", ":(glob)x", ""] {
            let text = format!(
                r#"{{"schema":"memprove.verify.contract.v1","checks":[{{"argv":["true"]}}],"protected":[{}]}}"#,
                json::string(pattern)
            );
            assert!(refused(&text).contains("`protected` holds"), "{pattern}");
        }
        assert!(
            refused(
                r#"{"schema":"memprove.verify.contract.v1","checks":[{"argv":["true"]}],"base":"main"}"#
            )
            .ends_with("`base` is not a known member")
        );
    }
}
