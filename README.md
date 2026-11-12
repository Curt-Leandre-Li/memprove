# memprove

memprove is a memory and task-state layer for coding agents. Every fact an
agent records is bound to a commit, a repository state and a test run, and a
deterministic rule, not a model, decides whether it is still `verified`,
`stale`, or `unconfirmed`. The record is a hash-chained ledger kept by a
local daemon; a new session (the same agent later, another agent, a person)
reads a compact Context Pack of what holds, what is open and what to do
next, and takes the work over under a rule that says when a Session left
open may be ended.

It is a Rust workspace: `memproved` (the daemon), `memprove` (the
command-line client, the Claude Code SessionStart hook and an MCP server
with ten tools), and the crates they share. Everything runs on your machine
over a Unix socket; nothing leaves it.

## Why not just notes?

Agent memory goes stale silently: a sentence in `CLAUDE.md` or a memory
store says the parser handles empty input, and three commits later it does
not. Tools that audit memory files (stalebrain, true-memory-fragments and
their kin) check sentences against paths, symbol hashes or the commit that
contradicts them; Claude Code's auto-memory and the hosted memory layers
merge and expire entries by timestamps and wording. memprove does not
judge text: a fact is `verified` only while a test run that `memprove
run` recorded passed in the repository state the daemon can still observe.
When HEAD, the index or the worktree moves, the fact is `stale` and the Pack
says which command to run again. When the ledger itself is changed behind
the daemon's back, every verdict of that Task is `tampered` and the word
`verified` appears nowhere. What was superseded or withdrawn stays in the
record, and every statement names the Session, hence the agent, that wrote
it.

## Install in two steps

1. Build and install (macOS with Homebrew LLVM; on Ubuntu install
   `libclang-dev` and skip the two exports):

   ```bash
   export LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib
   export DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/opt/llvm/lib
   cargo build --offline --release -p memprove-cli -p memprove-daemon
   scripts/install.sh --from-binaries target/release
   ```

   The default prefix is `~/.memprove`: binaries in `bin/`, the daemon's
   data in `data/`, the agent's token and the data directory's path in
   `config/`, the operator's token in `operator-token`, the guide in
   `share/`. The daemon is started for you, and the binaries find their
   configuration without any environment variable. Put `~/.memprove/bin`
   on `PATH` (`--write-profile` writes the line into your shell profile).

2. Enable a repository, from inside its Git worktree:

   ```bash
   memprove init
   ```

   It registers the Project, keeps memprove's own files out of your
   Evidence through `.git/info/exclude`, writes the Claude Code hook and MCP
   entry (`.claude/settings.local.json`, `.mcp.json`), the Codex server
   entry (`~/.codex/config.toml`) with a short note in `AGENTS.md`, the
   Cursor entry (`.cursor/mcp.json`) with a rule, for whichever clients it
   detects (`--client` names them), and ends with `memprove doctor`.
   Running it again changes nothing. No token is shown or written into the
   repository.

## One session, five commands

```bash
memprove begin --new-task "parser: handle empty input"     # or a plain `begin` to continue the Task
# ... edit, commit ...
memprove run --test-format cargo -- cargo test              # the run is the Evidence; its label is r1
memprove checkpoint create \
  --fact "the parser accepts empty input and reports it as an empty document" --evidence r1 \
  --open "the error path for a truncated header is untested" \
  --next "write the truncated-header test and run cargo test"
memprove resume --pack                                     # what the next session will read
memprove end --reason "empty input handled"
```

`begin` starts a Session on a Task (and creates the Task with `--new-task`).
`run` runs the command in your shell and records its result together with
the repository state. `checkpoint create` records facts (each citing runs),
open items, decisions and the next action; a fact needs a passed test run.
`resume --pack` prints the Context Pack: the Task's verdict, the Sessions,
the last passed run, what can be done next, every active statement with its
verdict and the commit it is bound to, and what to run again. `end` ends
the Session. In Claude Code the hook injects the Pack at the start of every
session, and the MCP tools do what the commands do, except running
commands: tests are always recorded with `memprove run`.

`memprove --help` lists every command; `docs/guide.md` is the agent's
manual, from install to takeover.

## Taking over

A Pack lists every Session that has not ended. A session that finds one
left by another agent ends it as abandoned only when all three hold: its
agent is not you; it looks gone, because its last write is more than 15
minutes old or the Session that dispatched it has ended; and it is not your
own parent. Otherwise the Session stays open, `begin` as usual, and the
session's first checkpoint records an open item that another Session may
still be working. The hook's opening gives you your name, the current UTC
time and your parent, so the rule needs no read of your environment.
`memproved` records who ended whom, and a Session that declared the
`sub_agent` role cannot record a Decision, complete the Task, or overrule
another agent's facts; the orchestrating session or a person does that. The
takeover evaluation (`docs/takeover-eval.md`) runs a real client through
six scenarios and scores the archive.

## Status

memprove is under active development and not yet published as a release.
The ledger format, the control API (`openapi/control-api-v1.yaml`) and the
Pack's line set are stable; the daemon has no network access by design.
`docs/architecture.md` says how it is built.

## Licence

Apache License, Version 2.0 (`LICENSE`).
