//! P4-G (ADR 0014 E7, Owner conditions q to y): `memprove mcp serve`, a
//! Model Context Protocol server over standard input and output.
//!
//! JSON-RPC 2.0, one message per line, parsed with the strict JSON parser
//! of `memprove-protocol` and written with [`crate::json_string`]; no other
//! crate (condition q). Both protocol eras are served, decided per request
//! (condition r): the 2026-07-28 revision (`server/discover`, the protocol
//! version in each request's `_meta`, `resultType` in each result) and the
//! `initialize` handshake of 2025-11-25 and 2025-06-18. No protocol session
//! is relied on: every tool call re-reads the context files, the token, and
//! the data directory, and every result names its Task and Session
//! (condition s). Ten tools are listed in a fixed order, the read tools
//! apart from the write tools; no tool runs a command (condition t).
//! Standard output carries protocol messages only, and standard error one
//! log line per message (condition x).

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::{context::workspace_root, hook};

mod failure;
mod local;
mod protocol;
mod read;
mod schema;
mod tools;
mod wire;
mod write;

pub use protocol::{run_stdio, run_stdio_with, serve};

/// The default budget of the `pack` tool, in UTF-16 code units (ADR 0014
/// E7.6, Owner condition w): a default call stays under Claude Code's
/// 10,000-token warning line even for a Pack of CJK text. The CLI and the
/// hook keep [`memprove_domain::PACK_DEFAULT_BUDGET`] (9,000), which the
/// tool's `budget` argument selects.
pub const MCP_PACK_DEFAULT_BUDGET: usize = memprove_domain::PACK_MCP_DEFAULT_BUDGET;

/// The variable Claude Code sets to the project root in a stdio server's
/// environment (ADR 0014 E7.5.2).
pub const PROJECT_DIR_ENV: &str = "CLAUDE_PROJECT_DIR";

/// U1 (A1, Owner O2): the variable Claude Code sets to its session id in a
/// stdio server's environment; without `MEMPROVE_CLIENT_SESSION` the
/// server takes it as its client session.
pub const CLAUDE_CODE_SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

/// What the server reads from its environment, once per tool call (ADR
/// 0014 E7.5.2, E7.8): never at startup and never kept.
#[derive(Debug, Clone, Default)]
pub struct Environment {
    /// What the `SessionStart` hook reads: the configuration and data
    /// directories and the token sources (`CLAUDE_ENV_FILE` is never
    /// written by the server).
    pub process: hook::Environment,
    /// `MEMPROVE_CLIENT_SESSION`.
    pub client_session: Option<OsString>,
    /// U1 (A1): `CLAUDE_CODE_SESSION_ID`.
    pub claude_code_session: Option<OsString>,
    /// `CLAUDE_PROJECT_DIR`.
    pub project_dir: Option<OsString>,
    /// The process's working directory.
    pub cwd: Option<PathBuf>,
}

impl Environment {
    /// The environment of this process, now.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            process: hook::Environment::from_process(),
            client_session: std::env::var_os(crate::context::CLIENT_SESSION_ENV),
            claude_code_session: std::env::var_os(CLAUDE_CODE_SESSION_ENV),
            project_dir: std::env::var_os(PROJECT_DIR_ENV),
            cwd: std::env::current_dir().ok(),
        }
    }

    /// The directory the server works in: `CLAUDE_PROJECT_DIR` when it is
    /// the absolute path of an existing directory, else the working
    /// directory.
    #[must_use]
    pub fn directory(&self) -> Option<PathBuf> {
        self.project_dir
            .as_deref()
            .map(Path::new)
            .filter(|path| path.is_absolute() && path.is_dir())
            .map(Path::to_path_buf)
            .or_else(|| self.cwd.clone())
    }
}

/// The project-scoped configuration file of Claude Code's MCP servers, at
/// the Workspace root (ADR 0014 E7.9); U5 Z-F round 1 (S1): the file `hook
/// install` writes `mcpServers.memprove` into, the second part of `mcp
/// print-config`.
pub const PROJECT_CONFIG_FILE: &str = ".mcp.json";

/// U5 Z-F round 1 (S1): the line that starts each part of `mcp
/// print-config`'s output, naming the file of the Workspace root the part
/// belongs in. No line of an indented JSON value starts with `==> `, so the
/// parts are found by these lines alone.
#[must_use]
pub fn part_header(file: &str) -> String {
    format!("==> {file} <==")
}

/// `<Workspace root of cwd>/.mcp.json`, where the second part of `mcp
/// print-config` belongs.
///
/// # Errors
///
/// When `cwd` cannot be resolved.
pub fn config_path(cwd: &Path) -> Result<PathBuf, String> {
    workspace_root(cwd)
        .map(|root| root.join(PROJECT_CONFIG_FILE))
        .map_err(|error| format!("cannot resolve the current directory: {error}"))
}

/// What `mcp print-config` prints on standard output (ADR 0014 E7.9,
/// Owner condition y; U5 P3, Owner O5; U5 Z-F round 1, S1, the Owner's O5
/// revision): two parts, each a line [`part_header`] followed by one JSON
/// value indented by two spaces, apart by one empty line. First the part of
/// `.claude/settings.local.json` ([`hook::settings_fragment`]: the
/// `SessionStart` handler, `env.MEMPROVE_CONFIG_DIR` set to `config_dir`,
/// and the suggested `permissions.allow`), then the part of `.mcp.json`
/// ([`hook::mcp_json_fragment`]: the stdio server `memprove`,
/// `executable mcp serve` in exec form, with the same `env`). No token, no
/// data directory, no `timeout` for the server.
#[must_use]
pub fn config_fragment(executable: &str, config_dir: &str) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        part_header(hook::SETTINGS_FILE),
        hook::settings_fragment(executable, config_dir),
        part_header(PROJECT_CONFIG_FILE),
        hook::mcp_json_fragment(executable, config_dir)
    )
}

/// The three statements `mcp print-config` writes on standard error (ADR
/// 0014 E7.9; U5 P3; U5 Z-F round 1, S1): where each part goes (`settings`
/// is the `.claude/settings.local.json` of the Workspace root, `mcp_json`
/// its `.mcp.json`), that Claude Code does not read `mcpServers` from a
/// settings file, and what `hook install` writes; that the second part is
/// also a file for `claude --mcp-config`, and the same entry as a `claude
/// mcp add` command line; and which configuration directory `env` names and
/// what the server and the hook read through it.
#[must_use]
pub fn config_statements(
    settings: &Path,
    mcp_json: &Path,
    executable: &str,
    config_dir: &str,
    source: &str,
) -> [String; 3] {
    [
        format!(
            "memprove: the part under {} belongs in {} and the part under {} in {}, the file \
             Claude Code reads the project's MCP servers from (it does not read mcpServers from \
             a settings file); `memprove hook install` writes the hooks and env members into \
             the first, its permissions.allow too with --allow-tools, and the mcpServers member \
             into the second; nothing was written",
            hook::SETTINGS_FILE,
            settings.display(),
            PROJECT_CONFIG_FILE,
            mcp_json.display()
        ),
        format!(
            "memprove: the {PROJECT_CONFIG_FILE} part is also a file for claude --mcp-config, \
             and its entry also fits ~/.claude.json (local or user scope); the same entry from \
             the command line: claude mcp add --transport stdio --scope project --env {} \
             memprove -- {} mcp serve {} {}",
            shell_word(&format!("{}={config_dir}", hook::CONFIG_DIR_VARIABLE)),
            shell_word(executable),
            crate::CONFIG_DIR_OPTION,
            shell_word(config_dir)
        ),
        format!(
            "memprove: env.{var} is the configuration directory this command uses ({config_dir}, \
             from {source}); the hook, the server, and the CLI read the data directory and the \
             token through <config>/data-dir and <config>/token unless MEMPROVE_DATA_DIR and \
             MEMPROVE_TOKEN_FILE are set; permissions.allow lets an unattended claude -p use \
             the memprove tools and `memprove run`; a token never goes into these files",
            var = hook::CONFIG_DIR_VARIABLE
        ),
    ]
}

/// `word` as one POSIX shell word: unchanged when every character is one a
/// shell takes literally, else in single quotes (a `'` written `'\''`), so
/// that the printed command line names the same file.
fn shell_word(word: &str) -> String {
    let literal = !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._+-@%:,=".contains(&byte));
    if literal {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memprove_protocol::strict_json::{JsonValue, parse_strict};

    /// U5 P3 (Owner O5); U5 Z-F round 1 (S1): two parts, the settings
    /// part (hook, env, rules) and the `.mcp.json` part (the server with
    /// the same env), each under its header line; a path that needs
    /// escaping stays one JSON string.
    #[test]
    fn the_fragment_holds_the_settings_part_and_the_mcp_json_part() {
        let fragment = config_fragment("/opt/bin/memprove", "/home/a/.config/memprove");
        assert_eq!(
            fragment,
            r#"==> .claude/settings.local.json <==
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/opt/bin/memprove",
            "args": [
              "hook",
              "session-start",
              "--config-dir",
              "/home/a/.config/memprove"
            ],
            "timeout": 10
          }
        ]
      }
    ]
  },
  "env": {
    "MEMPROVE_CONFIG_DIR": "/home/a/.config/memprove"
  },
  "permissions": {
    "allow": [
      "mcp__memprove",
      "Bash(memprove run:*)",
      "Bash(/opt/bin/memprove run:*)"
    ]
  }
}

==> .mcp.json <==
{
  "mcpServers": {
    "memprove": {
      "type": "stdio",
      "command": "/opt/bin/memprove",
      "args": [
        "mcp",
        "serve",
        "--config-dir",
        "/home/a/.config/memprove"
      ],
      "env": {
        "MEMPROVE_CONFIG_DIR": "/home/a/.config/memprove"
      }
    }
  }
}
"#
        );
        let odd = config_fragment("/a \"b\"\\c/memprove", "/c \"d\"");
        let (settings, mcp_json) = odd
            .strip_prefix("==> .claude/settings.local.json <==\n")
            .and_then(|rest| rest.split_once("\n==> .mcp.json <==\n"))
            .unwrap();
        assert!(parse_strict(settings).unwrap().get("mcpServers").is_none());
        let parsed = parse_strict(mcp_json).unwrap();
        let entry = parsed
            .get("mcpServers")
            .and_then(|servers| servers.get("memprove"))
            .unwrap();
        assert_eq!(
            entry.get("command").and_then(JsonValue::as_str),
            Some("/a \"b\"\\c/memprove")
        );
        assert_eq!(
            entry
                .get("env")
                .and_then(|env| env.get("MEMPROVE_CONFIG_DIR"))
                .and_then(JsonValue::as_str),
            Some("/c \"d\"")
        );
        assert!(entry.get("timeout").is_none());
    }

    #[test]
    fn the_statements_name_the_files_the_command_line_and_the_directory() {
        let [goes, command, reads] = config_statements(
            Path::new("/w/project/.claude/settings.local.json"),
            Path::new("/w/project/.mcp.json"),
            "/opt/bin/memprove",
            "/c/config",
            "MEMPROVE_CONFIG_DIR",
        );
        assert_eq!(
            goes,
            "memprove: the part under .claude/settings.local.json belongs in \
             /w/project/.claude/settings.local.json and the part under .mcp.json in \
             /w/project/.mcp.json, the file Claude Code reads the project's MCP servers from (it \
             does not read mcpServers from a settings file); `memprove hook install` writes the \
             hooks and env members into the first, its permissions.allow too with --allow-tools, \
             and the mcpServers member into the second; nothing was written"
        );
        assert!(
            command.ends_with(
                "claude mcp add --transport stdio --scope project --env \
                 MEMPROVE_CONFIG_DIR=/c/config memprove -- /opt/bin/memprove mcp serve \
                 --config-dir /c/config"
            ),
            "{command}"
        );
        assert!(
            command
                .starts_with("memprove: the .mcp.json part is also a file for claude --mcp-config"),
            "{command}"
        );
        assert!(
            reads.contains("(/c/config, from MEMPROVE_CONFIG_DIR)"),
            "{reads}"
        );
        assert!(
            reads.ends_with("a token never goes into these files"),
            "{reads}"
        );
        for line in [&goes, &command, &reads] {
            assert_eq!(line.lines().count(), 1);
            assert!(!line.contains(['?', '!']));
        }
    }

    #[test]
    fn a_path_a_shell_would_split_is_quoted() {
        assert_eq!(
            shell_word("/usr/local/bin/memprove"),
            "/usr/local/bin/memprove"
        );
        assert_eq!(
            shell_word("/Users/a b/Application Support/memprove"),
            "'/Users/a b/Application Support/memprove'"
        );
        assert_eq!(shell_word("/x/it's/memprove"), r"'/x/it'\''s/memprove'");
        assert_eq!(shell_word("/x/$(id)/memprove"), "'/x/$(id)/memprove'");
        assert_eq!(shell_word(""), "''");
    }
}
