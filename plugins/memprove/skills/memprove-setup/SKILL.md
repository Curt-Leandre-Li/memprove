---
name: memprove-setup
description: Install memprove and set it up in the current Git repository - memory bound to tests (memprove init), agent evals (memprove eval init) and, when the installed release has it, tool rules every agent obeys (memprove policy init). Use when the user asks to install, set up, enable or configure memprove, memprove eval or memprove policy in this repository.
---

# Set memprove up in this repository

memprove is a local memory layer for coding agents: every fact an agent records
is bound to a commit and a passed test run, and a rule, not a model, says
whether it still holds. `memprove eval` checks that an agent still does this
repository's real work; `memprove policy` gives Claude Code, Codex and Cursor
one set of tool rules. Docs: https://memprove.dev/docs/

The user asked you to set it up. Do the steps below in order. Run each command
as a command of its own, read its output, then go on. Every step is safe to run
again. When you are done, report as step 7 says.

## Rules

- Never read, print or copy `~/.memprove/operator-token` or
  `~/.memprove/config/token`, and never put a token into the repository.
- Never run `memprove eval run` or `memprove eval ab` with a real agent
  (`--agent claude-code`, `--agent codex`) unless the user asks for that run:
  it starts another agent and spends their API credits. `--agent command --
  true` runs no agent and is fine.
- Do not commit, push, change the shell profile, use `sudo` or build from
  source unless the user asks.
- If your sandbox blocks a command (no network, a write outside the
  repository, a Unix socket refused), ask the user to approve running it
  outside the sandbox, or give them the command to run themselves. Do not
  work around it.
- If every command runs in a new shell for you, `export` does not carry over:
  where these steps say `memprove`, call `~/.memprove/bin/memprove` by its
  path whenever `memprove` is not on `PATH`.

## 1. Check the repository

```bash
git rev-parse --show-toplevel
git log -1 --oneline
```

Run every later step from the directory the first command prints. If either
command fails, stop and tell the user: memprove needs a Git repository with at
least one commit.

## 2. Find memprove

```bash
memprove --version
```

- `memprove 0.2.0` or later: go to step 4.
- Not found, but `~/.memprove/bin/memprove --version` works: use that path
  (see Rules) and apply the line above to the version it prints.
- `memprove 0.1.x` from `~/.memprove/bin`: step 3 upgrades it in place.
- Found somewhere other than `~/.memprove/bin` (`command -v memprove` says
  where): the user installed it their own way. Ask before step 3, which would
  add a second installation in `~/.memprove`.

## 3. Install or upgrade

Release binaries exist for macOS on Apple silicon (`uname -sm` prints
`Darwin arm64`) and Linux x86_64 with glibc 2.35 or later (Ubuntu 22.04,
Debian 12). On anything else (Intel macOS, Linux arm64, musl, Windows) stop
and tell the user that building from source is described at
https://memprove.dev/docs/guide/install-upgrade-and-backup/ and needs Rust
and libclang.

Make a new temporary directory outside the repository:

```bash
mktemp -d
```

Then, with `<dir>` standing for the directory it printed, download the
installer and run it as a command of its own:

```bash
curl -fsSLo <dir>/install.sh https://memprove.dev/install.sh
sh <dir>/install.sh --no-write-profile
~/.memprove/bin/memprove --version
```

The installer takes the latest release from
https://github.com/memprove/memprove/releases, stops unless the archive's
SHA-256 matches the release's `.sha256` file, installs into `~/.memprove`,
starts the daemon `memproved`, and on an existing installation upgrades it.
`--no-write-profile` leaves the shell profile alone; ask the user before
running it again with `--write-profile`, which adds the `PATH` line so later
sessions find `memprove`.

If it exits with status 1, read its `memprove-install: error:` lines. On a
checksum mismatch nothing was installed: tell the user and do not install
memprove another way. If memprove.dev cannot be reached but github.com can,
install the same release by hand: download
`memprove-<version>-<target>.tar.gz` and its `.sha256` from the latest release
(`<target>` is `aarch64-apple-darwin` or `x86_64-unknown-linux-gnu`), check it
with `shasum -a 256 -c` (Linux: `sha256sum -c`), unpack it, and run
`bash memprove-<version>-<target>/scripts/install.sh --from-binaries
memprove-<version>-<target>/bin --no-write-profile` (the script needs bash).
Stop if the checksum does not match.

## 4. Memory: `memprove init`

```bash
memprove init --dry-run
memprove init
```

`init` registers the repository as a Project with the local daemon, keeps
memprove's own client files out of Git through `.git/info/exclude`, writes the
configuration of the clients it finds on this machine, and ends with
`memprove doctor`: for Claude Code the SessionStart hook in
`.claude/settings.local.json` and the MCP server in `.mcp.json`; for Codex
`[mcp_servers.memprove]` in `~/.codex/config.toml` and a short note in
`AGENTS.md`; for Cursor `.cursor/mcp.json` and a rule in
`.cursor/rules/memprove.mdc`. `--client claude-code` (or `codex`, `cursor`,
repeatable) picks the clients instead.

Before the first memprove Session, doctor's `workspace` and `ledger` lines
are `WARN`: that is expected. A `FAIL` line says what to fix; fix it or tell
the user. `init` prints Claude Code allow rules (`mcp__memprove`,
`Bash(memprove run:*)`) without writing them; `memprove init --allow-tools`
writes them, so ask the user first.

## 5. Evals: `memprove eval init`

```bash
memprove eval init --dry-run
memprove eval init
```

Add `--agent codex` to the second command when the user works with Codex; the
default writes the CI workflow for Claude Code. It writes `evals/README.md`
(how to write a task, addressed to a coding agent), `evals/smoke.json` when it
can guess the test command, `.github/workflows/memprove-eval.yml`, and a note
of under five lines in `AGENTS.md` or `CLAUDE.md`. Files the user has edited
are kept.

If `evals/smoke.json` was written, check the task once without any agent:

```bash
memprove eval run evals/smoke.json --agent command -- true
```

It runs the repository's test command once, in a fresh copy of HEAD. `passed`
is the expected result. `failed`, `setup_failed` or `error` means the guessed
check does not work here (tests that fail at HEAD, or a missing setup step
such as `npm ci`): report it and suggest the fix in `evals/smoke.json`; do not
change the tests. Skip this check if the user wants the setup fast and the
test suite is slow.

Tell the user that the workflow runs only by hand or on a pull request
labelled `eval`, and needs the repository secret `ANTHROPIC_API_KEY` (or
`OPENAI_API_KEY` for Codex). Offer the next step without doing it:
`memprove eval mine --dry-run` lists tasks it could write from the history.

## 6. Tool rules: `memprove policy init`, when the release has it

```bash
memprove policy --help
```

If the first line of the output starts with `memprove policy:`, set it up:

```bash
memprove policy init --dry-run
memprove policy init
memprove policy test
```

`policy init` writes a starter `.memprove/policy.rules` (meant to be
committed) when there is none, and the hook of each client it finds in the
client's local settings. `policy test` checks the `expect` lines of the file
and should pass. Tell the user to read the starter rules; any edit to the
policy file or a hook file needs their approval, so you cannot loosen it
yourself. In Codex the hooks run only after the user trusts this project's
hooks in Codex's `/hooks`.

If the first line is `Usage: memprove ...` (the general help), the installed
release has no `memprove policy` yet: tell the user it comes in a later
release, and go on. Do not install another tool in its place.

## 7. Check and report

```bash
memprove doctor
git status --short
```

Then tell the user, briefly:

- The memprove version and where it is installed. If `~/.memprove/bin` is not
  on their `PATH`, the line to add (`export PATH="$HOME/.memprove/bin:$PATH"`),
  or offer to rerun the installer with `--write-profile`.
- The files `git status --short` lists: repository content to review and
  commit (`evals/`, the workflow, `.memprove/policy.rules`, the notes in
  `AGENTS.md` or `CLAUDE.md`). memprove's client settings are local and
  excluded from Git; the Codex entry is in `~/.codex/config.toml`.
- What they still do themselves: restart the coding agent so it loads the
  hook and the MCP server (Claude Code asks once to approve the `memprove`
  server from `.mcp.json`); add the CI secret; in Codex, trust the hooks in
  `/hooks`.
- Every `FAIL` or unexpected `WARN` line of `doctor`, with what it means.

Offer to commit the repository files; commit only if they say yes. Do not
start a memprove Task as part of the setup: from the next session on, the
hook gives the agent the Context Pack, and the note in `AGENTS.md` or
`CLAUDE.md` says how to use memprove on the real work.
