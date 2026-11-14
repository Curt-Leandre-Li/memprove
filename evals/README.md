# evals/: tasks for `memprove eval`

For the coding agent that adds or fixes evals. Each `*.json` file here other
than `baseline.json` is one task: a commit to start from, a prompt for a
coding agent, and a check command whose exit code alone decides the result.

## A task file: `evals/<id>.json`

```json
{
  "schema": "memprove.eval.task.v1",
  "id": "empty-input",
  "title": "parser accepts empty input",
  "base": "a1b2c3d",
  "prompt": "Make `parse` accept empty input and return an empty document.",
  "setup": [{"argv": ["npm", "ci"], "timeout_secs": 600}],
  "check": {"argv": ["npm", "test", "--", "parser"], "timeout_secs": 900},
  "check_patch": "evals/empty-input.check.patch",
  "agent_timeout_secs": 1800,
  "tags": ["parser"]
}
```

- `id`: 1 to 64 of `a-z`, `0-9`, `-`, `_`; the file name when left out.
- `base` (HEAD when left out), `title`, `setup`, `check_patch` and `tags`
  may be left out; a member not shown here is an error.
- `argv` runs without a shell; write `["sh", "-c", "..."]` for one.
- `check_patch` (relative to the repository root) is applied after the
  agent, before the check: put the tests that judge the task there, so that
  the agent can neither see nor edit them.

## Which tasks

Real work from this repository, such as a fixed bug or a small feature: a
check that fails before the work and passes after it, less than 15 minutes
for an agent, a narrow check (one package, one test file).
`memprove eval mine --verify` writes such tasks from the history (parent
commit as `base`, message as `prompt`, the commit's test changes as
`check_patch`), keeping those whose check fails on the parent and passes on
the commit. By hand: make the change and its tests on a branch from `base`,
then `git diff <base> HEAD -- <test files> > evals/<id>.check.patch`.

## Check a task file before using it

    memprove eval run evals/<id>.json --agent command -- true

runs the task with an agent that does nothing: `smoke` should come out
`passed`, a task for new work `failed` (if it passes, its check does not test
the work); `patch_failed`, `setup_failed` or `error` mean the file is wrong.

## Run an agent, keep a baseline

    memprove eval run evals/ --agent claude-code     # or --agent codex
    memprove eval report latest
    memprove eval baseline latest --write evals/baseline.json   # commit it
    memprove eval gate latest --baseline evals/baseline.json

Run the gate after changing the model, a prompt, CLAUDE.md, AGENTS.md or
memory. A real agent costs money: start with a few tasks and `--trials 3`.
