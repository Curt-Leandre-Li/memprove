//! U17 (M1 O4), through the real binaries: `memprove init` in a
//! repository registers the Project with the installation's operator
//! token, keeps memprove's files out of Evidence, writes the
//! configuration of the clients it is asked for (or detects), writes the
//! note for the clients without a `SessionStart` hook, and ends with
//! `doctor`'s report; run again it changes nothing; `--dry-run` writes and
//! sends nothing; the default installation (`~/.memprove`) is found by
//! its place, with no `MEMPROVE_*` variable. The Codex and Cursor files
//! are written under a temporary `HOME`, never the real one.

mod u1e_support;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Output,
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use u1e_support::{Daemon, bootstrap, cli_binary, isolated, json, repository, short_tempdir, text};

/// A default-prefix installation under a temporary home: `~/.memprove`
/// with `bin/`, `data/`, `config/` (`data-dir`, `token`) and the operator's
/// token, a running daemon, and a repository with one commit.
struct Install {
    _dir: tempfile::TempDir,
    home: PathBuf,
    prefix: PathBuf,
    repo: PathBuf,
    daemon: Option<Daemon>,
}

impl Install {
    fn new() -> Self {
        let (dir, root) = short_tempdir();
        let home = root.join("home");
        let prefix = home.join(".memprove");
        let (data, config) = (prefix.join("data"), prefix.join("config"));
        for path in [&home, &prefix, &data, &config] {
            fs::create_dir_all(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(config.join("data-dir"), format!("{}\n", data.display())).unwrap();
        let operator = prefix.join("operator-token");
        bootstrap(&data, &operator);
        let daemon = Daemon::start(&data, &root.join("daemon.log"));
        // The agent's token, as scripts/install.sh writes it.
        let created = json(
            &isolated(cli_binary())
                .arg("--data-dir")
                .arg(&data)
                .arg("--token-file")
                .arg(&operator)
                .args([
                    "token",
                    "create",
                    "--label",
                    "agent",
                    "--scope",
                    "mcp-read",
                    "--scope",
                    "mcp-write",
                ])
                .output()
                .unwrap(),
        );
        let secret = created.get("secret").and_then(JsonValue::as_str).unwrap();
        fs::write(config.join("token"), format!("{secret}\n")).unwrap();
        fs::set_permissions(config.join("token"), fs::Permissions::from_mode(0o600)).unwrap();
        let repo = root.join("repo");
        repository(&repo);
        Self {
            _dir: dir,
            home,
            prefix,
            repo,
            daemon: Some(daemon),
        }
    }

    /// `memprove <words>` in `cwd` with only `HOME` set to the temporary
    /// home (the default installation is found by its place) and `PATH`
    /// holding no client.
    fn init_in(&self, cwd: &Path, words: &[&str]) -> Output {
        let mut command = isolated(cli_binary());
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .arg("init")
            .args(words);
        command.output().unwrap()
    }

    fn stop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            daemon.stop();
        }
    }
}

fn ok(output: &Output) -> String {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    text(&output.stdout)
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
#[allow(clippy::too_many_lines)] // One scenario, step by step.
fn u17_init_enables_a_repository_for_every_client_and_is_idempotent() {
    let mut install = Install::new();
    let repo = install.repo.clone();
    fs::create_dir_all(install.home.join(".codex")).unwrap();
    fs::create_dir_all(install.home.join(".cursor")).unwrap();
    fs::write(repo.join("AGENTS.md"), "# Agents\n\nBe brief.\n").unwrap();

    // Claude Code is not detected (no ~/.claude, no claude on PATH); it is
    // asked for; Codex and Cursor are detected from the home directory.
    let first = ok(&install.init_in(&repo, &["--client", "claude-code"]));
    println!("{first}");
    assert!(first.contains("configuration directory: "), "{first}");
    assert!(first.contains("(from HOME/.memprove/config)"), "{first}");
    assert!(first.contains("clients: claude-code"), "{first}");
    let project = first
        .lines()
        .find_map(|line| line.strip_prefix("Project "))
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_else(|| panic!("no Project line:\n{first}"))
        .to_owned();
    assert_eq!(project.len(), 36, "{project}");
    assert!(first.contains("\ndoctor:\n"), "{first}");
    assert!(
        first.contains("PASS") && !first.contains("FAIL "),
        "{first}"
    );
    assert!(first.contains("next: memprove begin --new-task"), "{first}");

    // The exclude file holds the two entries (only Claude Code was asked
    // for: with --client the detected ones are not added).
    let exclude = read(&repo.join(".git/info/exclude"));
    for entry in [".claude/settings.local.json", ".mcp.json"] {
        assert_eq!(exclude.matches(entry).count(), 1, "{exclude}");
    }
    assert!(!exclude.contains(".cursor/"), "{exclude}");
    let settings = parse_strict(&read(&repo.join(".claude/settings.local.json"))).unwrap();
    assert!(settings.get("hooks").is_some(), "{settings:?}");
    // U22 (Owner, 2026-11-11): without --allow-tools the rules are named,
    // not written.
    assert!(settings.get("permissions").is_none(), "{settings:?}");
    assert!(
        first.contains("note: permissions.allow was left as it is")
            && first.contains("\"mcp__memprove\"")
            && first.contains("`memprove init --allow-tools` adds them"),
        "{first}"
    );
    let mcp_json = parse_strict(&read(&repo.join(".mcp.json"))).unwrap();
    assert!(
        mcp_json
            .get("mcpServers")
            .and_then(|servers| servers.get("memprove"))
            .is_some()
    );
    assert!(!repo.join(".cursor").exists());
    assert!(!install.home.join(".codex/config.toml").exists());
    // AGENTS.md is a Codex file: not touched for Claude Code alone.
    assert_eq!(read(&repo.join("AGENTS.md")), "# Agents\n\nBe brief.\n");

    // Every client, detected: Codex and Cursor from the home directory,
    // Claude Code asked for again, now with --allow-tools.
    let second = ok(&install.init_in(
        &repo,
        &[
            "--client",
            "claude-code",
            "--client",
            "codex",
            "--client",
            "cursor",
            "--allow-tools",
        ],
    ));
    let settings = parse_strict(&read(&repo.join(".claude/settings.local.json"))).unwrap();
    let allow = settings
        .get("permissions")
        .and_then(|permissions| permissions.get("allow"))
        .and_then(JsonValue::as_array)
        .unwrap();
    assert_eq!(allow.len(), 3, "{allow:?}");
    assert!(!second.contains("note: permissions.allow"), "{second}");
    println!("{second}");
    assert!(
        second.contains("clients: claude-code, codex, cursor"),
        "{second}"
    );
    assert!(
        second.contains(&format!("Project {project} registered")),
        "{second}"
    );
    let exclude = read(&repo.join(".git/info/exclude"));
    for entry in [
        ".claude/settings.local.json",
        ".mcp.json",
        ".cursor/mcp.json",
        ".cursor/rules/memprove.mdc",
    ] {
        assert_eq!(exclude.matches(entry).count(), 1, "{exclude}");
    }
    let codex = read(&install.home.join(".codex/config.toml"));
    assert!(codex.starts_with("[mcp_servers.memprove]\n"), "{codex}");
    assert!(codex.contains("\"--config-dir\""), "{codex}");
    assert!(
        codex.contains(install.prefix.join("config").to_str().unwrap()),
        "{codex}"
    );
    // AGENTS.md is repository content: the first write comes with the commit note, the
    // unchanged second run (below) without it.
    assert!(
        second.contains("note: AGENTS.md is content of the repository: commit it"),
        "{second}"
    );
    let agents = read(&repo.join("AGENTS.md"));
    assert!(
        agents.starts_with("# Agents\n\nBe brief.\n\n<!-- memprove:begin -->\n"),
        "{agents}"
    );
    assert!(agents.contains("MCP tool `pack`"), "{agents}");
    assert!(agents.ends_with("<!-- memprove:end -->\n"), "{agents}");
    let rule = read(&repo.join(".cursor/rules/memprove.mdc"));
    assert!(rule.starts_with("---\nalwaysApply: true\n---\n"), "{rule}");
    let cursor = parse_strict(&read(&repo.join(".cursor/mcp.json"))).unwrap();
    assert!(
        cursor
            .get("mcpServers")
            .and_then(|servers| servers.get("memprove"))
            .is_some()
    );
    // No token in any file init wrote inside the repository.
    let token = read(&install.prefix.join("config/token"));
    for path in [
        ".claude/settings.local.json",
        ".mcp.json",
        ".cursor/mcp.json",
        ".cursor/rules/memprove.mdc",
        "AGENTS.md",
        ".git/info/exclude",
    ] {
        assert!(
            !read(&repo.join(path)).contains(token.trim()),
            "{path} holds the token"
        );
    }
    assert!(!codex.contains(token.trim()));

    // Run again with the same clients: nothing changes, the Project stays.
    let third = ok(&install.init_in(
        &repo,
        &[
            "--client",
            "claude-code",
            "--client",
            "codex",
            "--client",
            "cursor",
            "--allow-tools",
        ],
    ));
    assert!(
        third.contains(&format!("Project {project} registered")),
        "{third}"
    );
    assert!(!third.contains(" written"), "{third}");
    assert_eq!(third.matches("already as asked").count(), 7, "{third}");
    assert!(!third.contains("note: AGENTS.md"), "{third}");
    assert_eq!(read(&repo.join("AGENTS.md")), agents);
    assert_eq!(read(&install.home.join(".codex/config.toml")), codex);

    // Detection without --client: Codex and Cursor (no ~/.claude).
    let detected = ok(&install.init_in(&repo, &["--dry-run"]));
    assert!(detected.contains("clients: codex, cursor"), "{detected}");
    assert!(
        detected.contains("would register the Project"),
        "{detected}"
    );
    assert!(
        detected.contains("doctor is not run in a dry run"),
        "{detected}"
    );
    assert!(
        detected
            .lines()
            .all(|line| !line.ends_with(" written") || line.ends_with("would be written")),
        "{detected}"
    );

    // Usage errors send and write nothing.
    for words in [
        &["extra"][..],
        &["--client", "vim"][..],
        &["--data-dir", "/x"][..],
    ] {
        let refused = install.init_in(&repo, words);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{words:?}: {}",
            text(&refused.stderr)
        );
        assert!(refused.stdout.is_empty(), "{words:?}");
    }
    // Outside a Git worktree: a failure, nothing done.
    let elsewhere = install.home.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let outside = install.init_in(&elsewhere, &["--client", "claude-code"]);
    assert_eq!(outside.status.code(), Some(1), "{}", text(&outside.stderr));
    assert!(text(&outside.stderr).contains("not inside a Git worktree"));
    install.stop();
}

/// U22 (U20-N1): in a repository and a home that `init` enabled under the
/// previous name, `init` puts its note where the `continuity` note was in
/// AGENTS.md, its Codex table where `[mcp_servers.continuity]` was, and
/// removes the unedited `.cursor/rules/continuity.mdc`; run again, it
/// changes nothing.
#[test]
fn u22_init_replaces_what_it_wrote_under_the_previous_name() {
    let mut install = Install::new();
    let repo = install.repo.clone();
    fs::create_dir_all(install.home.join(".codex")).unwrap();
    fs::create_dir_all(repo.join(".cursor/rules")).unwrap();
    fs::write(
        repo.join("AGENTS.md"),
        "# Agents\n\n<!-- continuity:begin -->\nContinuity keeps this repository's task \
         ledger.\n<!-- continuity:end -->\n\nBe brief.\n",
    )
    .unwrap();
    let old_bin = install.prefix.join("bin/continuity");
    fs::write(
        install.home.join(".codex/config.toml"),
        format!(
            "model = \"x\"\n\n[mcp_servers.continuity]\ncommand = \"{}\"\nargs = [\"mcp\", \
             \"serve\"]\n\n[tui]\na = 1\n",
            old_bin.display()
        ),
    )
    .unwrap();
    let old_rule = "---\nalwaysApply: true\n---\nContinuity keeps this repository's task ledger. \
                    Before changing anything, call the MCP tool `pack` of the server `continuity` \
                    and read the Context Pack it returns; record facts with its `checkpoint` tool, \
                    citing a `continuity run` of the tests, and end your Session with `end`.\n";
    fs::write(repo.join(".cursor/rules/continuity.mdc"), old_rule).unwrap();

    let words = ["--client", "codex", "--client", "cursor"];
    let first = ok(&install.init_in(&repo, &words));
    println!("{first}");
    let agents = read(&repo.join("AGENTS.md"));
    assert!(
        agents.starts_with("# Agents\n\n<!-- memprove:begin -->\n")
            && agents.ends_with("<!-- memprove:end -->\n\nBe brief.\n")
            && !agents.contains("continuity"),
        "{agents}"
    );
    let codex = read(&install.home.join(".codex/config.toml"));
    assert!(
        codex.starts_with("model = \"x\"\n\n[mcp_servers.memprove]\n")
            && codex.ends_with("\n\n[tui]\na = 1\n")
            && !codex.contains("continuity"),
        "{codex}"
    );
    assert!(!repo.join(".cursor/rules/continuity.mdc").exists());
    assert!(first.contains("continuity.mdc removed"), "{first}");
    assert!(repo.join(".cursor/rules/memprove.mdc").is_file());

    let again = ok(&install.init_in(&repo, &words));
    assert!(
        !again.contains(" written") && !again.contains(" removed"),
        "{again}"
    );
    assert_eq!(read(&repo.join("AGENTS.md")), agents);
    assert_eq!(read(&install.home.join(".codex/config.toml")), codex);
    install.stop();
}
