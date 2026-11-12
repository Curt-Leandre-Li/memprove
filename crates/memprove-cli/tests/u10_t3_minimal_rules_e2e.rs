//! U10 T3 (Owner O4; ADR 0018 "Amendment (U10)" Part 1): the takeover
//! evaluation widens its project's rules to the whole `memprove` command
//! line (`scripts/takeover-eval/run.sh`), and the guide calls those the
//! unattended rules; what the CLI writes and prints stays the smallest
//! rules. `mcp print-config` prints, byte for byte, what it printed before
//! U10, and `hook install --allow-tools` writes exactly the three rules
//! `mcp__memprove`, `Bash(memprove run:*)`, and `Bash(<this executable>
//! run:*)`.

#[path = "u1e_support/mod.rs"]
mod support;

use std::fs;

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use support::{cli_binary, git, isolated, short_tempdir, text};

/// `mcp print-config`'s standard output before U10 (at `dfa264b`), with
/// `<EXE>` for this executable and `<CFG>` for the configuration directory.
const PRINTED: &str = r#"==> .claude/settings.local.json <==
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "<EXE>",
            "args": [
              "hook",
              "session-start",
              "--config-dir",
              "<CFG>"
            ],
            "timeout": 10
          }
        ]
      }
    ]
  },
  "env": {
    "MEMPROVE_CONFIG_DIR": "<CFG>"
  },
  "permissions": {
    "allow": [
      "mcp__memprove",
      "Bash(memprove run:*)",
      "Bash(<EXE> run:*)"
    ]
  }
}

==> .mcp.json <==
{
  "mcpServers": {
    "memprove": {
      "type": "stdio",
      "command": "<EXE>",
      "args": [
        "mcp",
        "serve",
        "--config-dir",
        "<CFG>"
      ],
      "env": {
        "MEMPROVE_CONFIG_DIR": "<CFG>"
      }
    }
  }
}
"#;

#[test]
fn u10_t3_the_cli_still_prints_and_writes_the_smallest_rules() {
    let (_guard, root) = short_tempdir();
    let repo = root.join("r");
    let config = root.join("cfg");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&config).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let exe = fs::canonicalize(cli_binary()).unwrap();
    let exe = exe.to_str().unwrap();
    let cfg = config.to_str().unwrap();
    let run = |words: &[&str]| {
        isolated(cli_binary())
            .current_dir(&repo)
            .env("HOME", &root)
            .env("MEMPROVE_CONFIG_DIR", &config)
            .args(words)
            .output()
            .unwrap()
    };
    let printed = run(&["mcp", "print-config"]);
    assert_eq!(printed.status.code(), Some(0), "{}", text(&printed.stderr));
    assert_eq!(
        text(&printed.stdout),
        PRINTED.replace("<EXE>", exe).replace("<CFG>", cfg)
    );
    let installed = run(&["hook", "install", "--allow-tools"]);
    assert_eq!(
        installed.status.code(),
        Some(0),
        "{}",
        text(&installed.stderr)
    );
    let settings =
        parse_strict(&fs::read_to_string(repo.join(".claude/settings.local.json")).unwrap())
            .unwrap();
    let allow: Vec<&str> = settings
        .get("permissions")
        .and_then(|permissions| permissions.get("allow"))
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .map(|rule| rule.as_str().unwrap())
        .collect();
    assert_eq!(
        allow,
        [
            "mcp__memprove".to_owned(),
            "Bash(memprove run:*)".to_owned(),
            format!("Bash({exe} run:*)"),
        ]
    );
}
