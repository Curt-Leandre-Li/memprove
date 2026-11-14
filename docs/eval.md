# memprove eval

`memprove eval` answers one question: after a change to how a coding agent works in this repository (a new model, a
new prompt, an edited `CLAUDE.md` or `AGENTS.md`, different memory), does the agent still do this repository's real
work? It gives the repository's own tasks to the agent, each in its own Git worktree, and judges each by the exit code
of the task's check command alone. Runs are kept as `run.json`, compared with a baseline committed to the repository,
and gated in CI.

```bash
memprove eval init                      # evals/, a CI workflow, five lines in AGENTS.md / CLAUDE.md
memprove eval mine --verify             # tasks from the history: parent commit, message, the commit's tests
memprove eval run evals/ --agent claude-code
memprove eval report latest
memprove eval baseline latest --write evals/baseline.json
memprove eval gate latest --baseline evals/baseline.json
```

A developer can hand the first two commands to their own coding agent: `init` writes `evals/README.md`, a guide
addressed to that agent, and the agent then adds and checks tasks by itself.

## Why only the exit code

A model that grades another model's work is a second thing to evaluate, costs a second bill, and gives a different
answer the next day. A check command does not: `cargo test -p parser` exits 0 or it does not, today and in a year, on
a laptop and in CI. So a task is judged by its check command and nothing else; an agent's own account of what it did
is recorded but never counted. The price is that a task needs a check that can tell good work from bad, which is what
hidden test patches (below) are for.

## Install and set up

`memprove eval` is part of the `memprove` binary (see the README for installing it); it needs Git and, to run a real
agent, that agent's CLI (`claude` for Claude Code, `codex` for Codex). It does not need the memprove daemon unless
the run reads memory (see "Memory").

```
memprove eval init [--agent claude-code|codex] [--no-workflow] [--dry-run]
```

`init` writes, in the repository root:

- `evals/README.md`: how to write and check task files, addressed to a coding agent.
- `evals/smoke.json`: a first task, with a check guessed from the repository's files (first match wins):
  `Cargo.toml` gives `cargo test`; `package.json` with a `test` script gives `npm test` after `npm ci` (`pnpm` or
  `yarn` when their lock file is there); `pyproject.toml`, `pytest.ini` or `setup.cfg` give `python3 -m pytest`;
  `go.mod` gives `go test ./...`; a `Makefile` with a `test` target gives `make test`. With no match, no smoke task
  is written and init says so.
- `.github/workflows/memprove-eval.yml` (not with `--no-workflow`), see "CI".
- A block of at most five lines between `<!-- memprove-eval:begin -->` and `<!-- memprove-eval:end -->` in
  `AGENTS.md` and/or `CLAUDE.md`, whichever exist (`AGENTS.md` when neither does), telling the agent where the evals
  are and what to run after changing prompts or memory.

Running `init` again changes nothing. It replaces its block in place, and it never overwrites `evals/README.md`,
`evals/smoke.json` or the workflow once they hold something else: it reports them as kept, so edits are safe.
`--dry-run` lists what would be written and writes nothing.

## Tasks

A task is a file `evals/<id>.json`:

```json
{
  "schema": "memprove.eval.task.v1",
  "id": "empty-input",
  "title": "parser accepts empty input",
  "base": "a1b2c3d",
  "prompt": "Make `parse` accept empty input and return an empty document.",
  "setup": [{"argv": ["npm", "ci"], "timeout_secs": 600}],
  "check": {"argv": ["cargo", "test", "-p", "parser"], "timeout_secs": 900},
  "check_patch": "evals/empty-input.check.patch",
  "agent_timeout_secs": 1800,
  "tags": ["parser"]
}
```

| Member | |
|---|---|
| `id` | 1 to 64 of `a-z`, `0-9`, `-`, `_`; the file name when left out |
| `base` | the commit the agent starts from; HEAD at the start of the run when left out |
| `prompt` | what the agent is asked to do |
| `setup` | commands run in the fresh worktree before the agent (installing packages); a failure is `setup_failed` |
| `check` | the acceptance command: exit code 0 is `passed` |
| `check_patch` | a patch, relative to the root, applied after the agent and before the check |
| `agent_timeout_secs` | the agent's limit; past it the agent is killed and the check runs all the same |
| `tags` | free labels |

Commands run without a shell; write `["sh", "-c", "..."]` for one. A member not in the table is an error.

A good task is real work from this repository (a fixed bug, a small feature), takes an agent less than 15 minutes,
and has a narrow check (one package, one test file) that fails before the work and passes after it. The tests that
judge it go in the check patch, so that the agent can neither read nor weaken them: make the change and its tests on
a branch from `base`, then `git diff <base> HEAD -- <test files> > evals/<id>.check.patch`.

Check a new task file with an agent that does nothing:

```bash
memprove eval run evals/<id>.json --agent command -- true
```

The smoke task should come out `passed`, a task for new work `failed`; if a task for new work passes, its check does
not test the work. `patch_failed`, `setup_failed` and `error` mean the file needs fixing.

## Mining tasks from the history

```
memprove eval mine [--limit N] [--range <git-range>] [--out evals] [--verify] [--dry-run]
```

`mine` looks at the non-merge commits of `--range` (the history of HEAD by default), newest first, and turns a commit
into a task when it changed at least one test file and at least one file that is neither a test nor documentation,
and no more than 20 files and 600 lines. A test file is one under a `test`, `tests`, `spec`, `specs` or `__tests__`
directory, or one whose name starts with `test_` or holds `_test.`, `.test.`, `_spec.` or `.spec.`; documentation is
under `docs/` or `doc/`, or a `.md`, `.markdown`, `.rst` or `.adoc` file. The task `evals/mined-<7 hex>.json` has:

- `base`: the commit's parent;
- `prompt`: the commit's subject and body, without trailers such as `Co-Authored-By:`;
- `check_patch`: `evals/mined-<7 hex>.check.patch`, the binary diff of the commit's test files against the parent;
- `check`: the guessed test command, narrowed when the rules allow: for Rust, `cargo test -p <package>` for the
  packages that hold the test files, plus `--test <name>` when they are all integration tests of one package; for
  Go, `go test` with the test files' package directories; for Python, `python3 -m pytest` with the test files;
  `setup` comes from the same guess.

A task file that exists is never overwritten, and counts towards `--limit` (10 by default), so running `mine` again
changes nothing. With `--verify`, each candidate is tried in two worktrees before it is written: setup and check must
fail on the parent with the check patch applied, and pass on the commit. Candidates that do not are reported with the
reason and the directory of their logs (under `.git/memprove-eval/mine/`) and are not written. A Rust repository
builds from scratch in each worktree; setting `CARGO_TARGET_DIR` to one directory for the whole run saves most of it.

Mined tasks are a start, not a finished suite: read each prompt (a commit message says what changed, not always what
was asked), and add the setup the check needs that the guess cannot know (a service, a binary another package builds).

## Running, reporting, gating

```
memprove eval run      [<task file or dir>...] [--agent claude-code|codex|command] [--model M]
                       [--agent-arg A]... [--trials N] [--jobs N] [--out-root DIR]
                       [--keep-worktrees] [--no-memory] [-- <program> <args>...]
memprove eval report   [<run dir>|latest] [--baseline FILE] [--format markdown|json]
memprove eval baseline [<run dir>|latest] [--write FILE]
memprove eval gate     [<run dir>|latest] --baseline FILE [--max-drop <percentage points>] [--allow-regressions]
```

`run` reads the tasks (`evals/` by default; `baseline.json` is not a task) and runs every trial in its own detached
worktree at the task's base: setup, the agent with the prompt, then the check patch and the check. Each trial ends as
one of `passed`, `failed`, `check_timeout`, `setup_failed`, `patch_failed`, or `error` (the trial could not be run:
not the agent's fault, counted as not passed and reported apart). The run directory
`<git common dir>/memprove-eval/runs/<run id>/` holds `run.json` and each trial's agent and check output and the
agent's diff; it is inside `.git`, so it is never an untracked file. `latest` is the run directory with the greatest
name. `--agent command -- <program> <args>...` runs any program as the agent, with `{prompt_file}` and `{workdir}`
replaced in its arguments.

`report` prints a run as Markdown (for a pull request comment or the job summary) or as JSON: the pass rate, cost
and tokens as the agent reported them, one row per task with each trial's outcome and, against a baseline, its change
(`regressed`, `worse`, `fixed`, `better`, `same`, `new`, `missing`), and every trial that did not pass with its reason
and logs. `baseline --write` keeps a run's per-task results (passed out of trials) as `evals/baseline.json`, to
commit. `gate` exits 1 when the pass rate is more than `--max-drop` points below the baseline's, when a task that
passed every trial in the baseline now passes none (unless `--allow-regressions`), or when a baseline task did not
run.

Exit codes of every `eval` command: 0 done (all trials passed, the gate passed), 1 some trial did not pass or the gate
failed, 2 usage error, 3 the command could not be done (a file, `git`, the agent would not start).

## CI

The workflow `init` writes runs by hand (`workflow_dispatch`) or on a pull request labelled `eval`, not on every push,
since a real agent costs money. It checks out the whole history (tasks start from earlier commits), downloads the
memprove release it names from GitHub, installs the agent's CLI with npm, runs `memprove eval run evals/ --agent <agent>
--no-memory`, adds `memprove eval report` to the job summary, gates against `evals/baseline.json` when that file
exists, and uploads the run directory as an artifact. It needs the repository secret `ANTHROPIC_API_KEY` (Claude Code)
or `OPENAI_API_KEY` (Codex). Add to it what the checks need, such as a toolchain or a service; `init` leaves an edited
workflow alone.

## Memory

memprove records what an agent learned as facts bound to Evidence; an evaluation is Evidence too. Run it under
`memprove run`:

```bash
memprove run --test-format cargo -- memprove eval run evals/
```

`eval run` ends with a line in libtest's format (`test result: ok. 5 passed; 0 failed; ...`, one case per trial), so
memprove records the run as test Evidence that a fact can cite ("the agent passes 5 of 5 evals with this CLAUDE.md"),
and the fact goes stale when HEAD moves, like any other.

Without `--no-memory`, `eval run` also reads the statements memprove would give an agent starting in the repository
(the Pack's facts, open items, decisions and next action, each with its verdict) and keeps them in `run.json`. When a
trial fails, the report lists the statements the agent was given that memprove no longer held as `verified` at the
time (stale, unconfirmed or tampered): the first place to look when an agent that used to pass starts failing.

## Limits of v0

- Claude Code (`claude -p`), Codex (`codex exec`) and any program as `command`; not Cursor, which has no
  non-interactive CLI yet.
- Cost and tokens are what the agent reports about itself; there is no price table.
- The agent's worktree shares the repository's object store, so an agent that looks can find later commits,
  including the commit a mined task was made from. Tasks measure an agent setup against itself over time; they are
  not a sealed benchmark.
- No model grades the work, no hosted service, no web page.
