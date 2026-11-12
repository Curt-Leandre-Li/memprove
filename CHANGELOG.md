# Changelog

memprove is not released yet; this file starts with the work that made the
repository publishable.

## Unreleased

### Added

- `memprove init`: enables a repository in one command for Claude Code,
  Codex and Cursor (registers the Project, keeps memprove's files out of
  Evidence through `.git/info/exclude`, writes the SessionStart hook, the
  MCP server entries and a short note for clients without a hook, then runs
  `doctor`).
- `memprove init --allow-tools`: `init` writes Claude Code's
  `permissions.allow` rules only when asked, and otherwise prints them;
  it also replaces what it wrote under the previous name (the `AGENTS.md`
  note, the Codex table, an unedited `.cursor/rules/continuity.mdc`).
- `TAKEOVER_EVAL_PROTECT` for the takeover evaluation: more directories
  (such as a second installation) that its safety checks guard, besides
  `~/.memprove` and `~/.config/memprove`.
- `scripts/install.sh --write-profile` / `--no-write-profile`; a default
  prefix (`~/.memprove`) needs no environment variable, because the CLI,
  the hook and the MCP server find `~/.memprove/config` on their own.
- `LICENSE` (Apache-2.0), `CONTRIBUTING.md`, a CI workflow for macOS and
  Ubuntu, `README.md`, `docs/guide.md` and `docs/architecture.md`.

### Changed

- The product is named memprove: the crates are `memprove-*`, the binaries
  `memprove` and `memproved`, the MCP server `memprove` (allow rules
  `mcp__memprove`, `Bash(memprove run:*)`), the default installation prefix
  `~/.memprove` with its configuration in `~/.memprove/config`, the daemon's
  socket and lock `memproved.sock` and `memproved.lock`, the environment
  variables `MEMPROVE_*` (`MEMPROVE_CONFIG_DIR`, `MEMPROVE_DATA_DIR`,
  `MEMPROVE_TOKEN_FILE`, `MEMPROVE_TOKEN`, `MEMPROVE_CLIENT_SESSION`,
  `MEMPROVE_AGENT_NAME`, `MEMPROVE_AGENT_ROLE`, `MEMPROVE_PARENT_SESSION`),
  and the Pack's format label `memprove-pack v1`. The ledger's own
  identifiers are unchanged, so an existing ledger reads as before: the
  daemon still writes its Events as the producer `continuityd` and its
  repository probes as the collector `continuityd-git` (a migration's trigger
  enforces the former), the ontology version is
  `continuity.work-state-ontology.v4`, the backup marker is
  `continuity-backup.txt`, and the migrations and the published schemas are as
  they were. `scripts/install.sh` renames an installation of the previous name
  in place: it stops the old `continuityd`, keeps the data and the tokens,
  writes `ct` and `ensure-daemon.sh` again, and leaves `memproved` running
  when `continuityd` was. In a repository enabled under the
  previous name, `memprove hook install` replaces that installation's
  SessionStart handler, `env.CONTINUITY_CONFIG_DIR` and
  `mcpServers.continuity` (with `--allow-tools` also its rules
  `mcp__continuity`, `Bash(continuity run:*)` and `Bash(<path> run:*)`)
  instead of adding its own beside them, and `memprove hook uninstall`
  removes them.
- Every fixed text is English: the Context Pack lines, the hook's
  statements, the MCP tool descriptions and failure sentences, the help
  pages, `resume`, `preview`, `doctor`, the run hints and the daemon's
  error details. The Pack's line structure, omission levels, budget and
  digest algorithm are unchanged; only the digest values moved.
- The last sentence of the hook's and the MCP `pack` tool's opening, and the
  takeover rule in the MCP `begin` and `end` descriptions, name the session
  that records the open item for a Session it leaves open, and when: in its
  first checkpoint (before, the item "is recorded", with no one named).
- The query Pack keeps the ASCII matching path only (terms of three or more
  characters through the trigram index, shorter ones as whole words).
- The takeover evaluation's prompt, seeds and scorer are English.

### Removed

- The semantic projection and its MCP tool, the ontology manifest, the
  client and query profiles, the ContextManifest and the static HTML export,
  with their tests and documentation (the budgets and omission levels they
  carried are plain constants of the Pack now).
