# Architecture

One page on how memprove is built. The agent's manual is `docs/guide.md`.

## The parts

```text
 Claude Code / Codex / Cursor                shell
 ┌──────────────────────────────┐    ┌──────────────────────────┐
 │ SessionStart hook  MCP tools │    │ memprove begin / run / │
 │ (Context Pack)     (10)      │    │ checkpoint / end / …     │
 └──────────────┬───────────────┘    └─────────────┬────────────┘
                │ HTTP/1.1 over a Unix socket (mode 0600), bearer token
                ▼
        memproved  (one process per data directory, file lock)
        ┌─────────────────────────────────────────────────────┐
        │ registry.sqlite: Projects, Workspaces, tokens        │
        │ projects/<id>/state.sqlite: the Project's ledger     │
        └─────────────────────────────────────────────────────┘
```

- **`memproved`** (`crates/memprove-daemon`) is a foreground process
  that serves the control API (`openapi/control-api-v1.yaml`) on
  `<data dir>/memproved.sock`. There is no TCP listener and no network
  access. A token is a 32-byte random secret; the daemon stores only its
  SHA-256 digest and its scopes (`control` for the operator, `mcp-read` and
  `mcp-write` for agents).
- **`memprove`** (`crates/memprove-cli`) is the client: the shell
  commands an agent runs, the SessionStart hook for Claude Code, the MCP
  server (`memprove mcp serve`, stdio), `init`, `doctor`, `backup` and
  `restore`. It keeps one small context file per Workspace (and per client
  session) outside the repository: Project, Workspace, Task, Session.
- **The store** (`crates/memprove-store-sqlite`) owns the SQLite schema
  and its append-only migrations; **the engine** and **the domain**
  (`memprove-engine`, `memprove-domain`) hold the rules; **the
  protocol** (`memprove-protocol`) holds the wire types and their strict
  JSON.

## The ledger

Every write to a Project is an Event in a hash chain: each Event carries the
digest of the one before it, and every record it names (Task, Session,
Checkpoint, Snapshot, Evidence) has its own canonical-JSON digest. `ledger
verify` recomputes all of it and reports the first record that does not
hold (exit 5), so a change made behind the daemon's back is found. A Task
whose records do not hold is `tampered`: none of its documents then says
`verified`.

Statements are claims of a Checkpoint: facts, open items, decisions and the
one next action in force. A later claim supersedes or withdraws an earlier
one; nothing is deleted, and `resume --text --all` still lists what was
replaced. Sessions declare which agent (name, role, parent Session) wrote
each claim, and who ended a Session that was left open.

## Evidence and verdicts

`memprove run -- <command>` runs the command in the caller's shell,
records its result (and, for a test run, how many cases passed) and seals a
Snapshot of the repository state: HEAD, branch, index, tracked-file digests,
untracked files. A fact cites such runs. Reading a Task, the daemon probes
the repository again and gives every fact one of four verdicts by
deterministic rules, never by a model:

| verdict | meaning |
|---|---|
| `verified` | the cited test run passed and the repository is still in the state it ran in |
| `stale` | the state moved on (a commit, an edit, a branch) since the run |
| `partial` (`unconfirmed`) | something needed for the judgement is missing: no passed test, untracked files, a failed later run of the same command |
| `tampered` | a record of the Task does not recompute against the ledger |

The reason codes (`head_changed`, `worktree_changed`, `evidence_not_passed`,
`object_digest_mismatch`, …) say why. A Task is completed only on passed runs
of the current state; `transitionTask` to `completed` refuses otherwise.

## The Context Pack

`resume --pack` renders a Task's Capsule as a fixed set of English lines
(`Pack:`, `Task`, `Integrity:`, `Sessions:`, `Session k …`, `Last passed:`,
`Can:`/`Cannot:`, `Base:`, the claim lines, `Rerun:`, `Omitted:`, `Digest:`)
within a budget of UTF-16 code units (9000 by default), leaving things out
in five deterministic levels when it does not fit, and ends with the SHA-256
of itself. The same ledger, probe and budget give the same bytes. A `query`
selects the statements that match a phrase (an in-memory FTS5 index), best
first.

## The hook and the MCP server

Claude Code's **SessionStart hook** (`memprove hook session-start`) prints
one opening statement (the Task, the current UTC time, the agent the
environment declares, the takeover rule's open item) and the Pack in a
fence, within 8 seconds, and always exits 0; it only reads. The **MCP
server** gives every client ten tools: `pack`, `claims`, `context`, `tasks`
and `ledger_verify` read, `begin`, `checkpoint`, `decide`, `end` and
`complete` write. No tool runs a command: tests are recorded with
`memprove run` in a shell and cited by their labels (`r1`, `r2`, …), so
every piece of Evidence comes from a real run.

## Taking over

A new session reads the Pack first. A Session another agent left open is
ended as abandoned only under the unattended takeover rule (its agent is not
you, it looks gone for 15 minutes or its dispatcher has ended, and it is not
your own parent); otherwise it stays open and an open item records it. The
takeover evaluation (`docs/takeover-eval.md`) runs a real client through six
such scenarios and scores the archive deterministically.
