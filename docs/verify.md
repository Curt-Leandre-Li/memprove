# memprove verify

`memprove verify` answers one question: an agent says the work is done; is it? The repository says what done means in
a contract, `.memprove/verify.json`: the commands that must exit 0 and the paths the work should not need to change to
pass, its tests. A verification runs those commands on a clean copy of the work and judges by their exit codes alone,
then looks at whether the pass was bought by changing the tests. Every verification writes a receipt.

```bash
memprove verify init --hook     # the contract, and the Claude Code hooks
git add .memprove/verify.json && git commit -m "verify: add the contract"
memprove verify run             # verify the working tree as it stands
memprove verify show            # the last run's summary again
```

It is part of the `memprove` binary and needs only Git: no daemon, no token, no model.

## The contract

```json
{
  "schema": "memprove.verify.contract.v1",
  "checks": [
    {"name": "tests", "argv": ["cargo", "test"], "timeout_secs": 1800}
  ],
  "setup": [],
  "protected": ["tests/**", "**/tests/**"]
}
```

| member | meaning |
|---|---|
| `checks` | the commands that must exit 0, at least one. `argv` runs without a shell (write `["sh", "-c", "..."]` for one); `name` (1 to 64 of `a-z`, `0-9`, `-`, `_`; `check-<n>` when absent) names its log files; `timeout_secs` defaults to 900 |
| `setup` | commands a new copy needs first, in order, such as `npm ci`; `timeout_secs` defaults to 600. One that fails ends the run as `fail` |
| `protected` | Git glob patterns, relative to the repository root (`*` stays within a directory, `**` crosses them): what the work should not need to change to pass |
| `skip_markers` | `false` turns the skip-marker rule off; `true` when absent |

`memprove verify init` writes it with the test command and the protected paths guessed from the repository's files
(`Cargo.toml`, `package.json` with a `test` script, `pyproject.toml` / `pytest.ini` / `setup.cfg`, `go.mod`, a
`Makefile` with a `test` target), and leaves a contract that exists as it is.

**The contract is read from the base commit**, not from the work: an agent that rewrites the check to `true` is still
judged by the check the base has, and the receipt says the contract was changed. A changed contract counts once it is
in the base. Only when the base has no contract is the work's own used, and the receipt says that too. `--contract
<FILE>` names another file; it is the caller's word and wins.

## What a run does

1. **The work.** The working tree as it stands: every tracked file as it is on disk and every new file Git does not
   ignore. What Git ignores (build output, caches, leftovers) is not part of the work. `--commit <REV>` verifies a
   commit's files instead. The repository's index, branches and files are not touched.
2. **The base.** `--base <REV>`; else, in the hook, the commit the client session started at; else the merge base
   with the branch's upstream; else `HEAD` (only uncommitted changes are compared, and the receipt says so). In CI,
   pass `--base origin/main`.
3. **The clean copy.** The work's files are written to a new directory under the system's temporary directory, a
   Git repository of its own with one commit, and the setup commands and the checks run there. A check passes when it
   exits 0; nothing else judges.
4. **The second run.** When the work adds, changes or deletes a protected path, the checks run again on a copy with
   every protected path exactly as at the base.
5. **Skip markers.** The lines the work adds outside the protected paths, in source files (`.rs`, `.py`, `.js`,
   `.jsx`, `.mjs`, `.cjs`, `.ts`, `.tsx`, `.go`, `.java`, `.kt`), are searched for a fixed list of texts that take
   a test out of a run: `#[ignore`, `@pytest.mark.skip`, `@pytest.mark.xfail`, `@unittest.skip`, `pytest.skip(`,
   `it.skip(`, `test.skip(`, `describe.skip(`, `it.only(`, `test.only(`, `describe.only(`, `xit(`, `xtest(`,
   `xdescribe(`, `t.Skip(`, `t.Skipf(`, `t.SkipNow(`, `@Disabled`, `@Ignore`.

| verdict | when | exit code of `run` |
|---|---|---|
| `pass` | every check exits 0 on the clean copy; they also do with the protected paths as at the base (or none differs); no skip marker was added | 0 |
| `fail` | a check or a setup command does not exit 0 on the clean copy | 1 |
| `needs_human` | every check exits 0, but not with the protected paths as at the base, or the work adds a skip marker | 4 |

`needs_human` is not an accusation. A change of behaviour needs its tests changed, and a rule cannot tell that from a
test weakened to pass; so a person looks, and the run directory's `protected.diff` holds exactly what to look at.
Exit code 2 is a usage error and 3 means the run could not be done (no contract, not a repository, `git` failed).

Setup commands and checks get `MEMPROVE_VERIFY_RUN` (the run's name), `MEMPROVE_VERIFY_STAGE` (`candidate`, or
`restored` for the second run) and `MEMPROVE_VERIFY_CACHE`, a directory that outlives the run
(`<git common dir>/memprove-verify/cache`). A clean copy has no build output, so a large project rebuilds each time;
a contract may point its build at the cache, for example
`["sh", "-c", "CARGO_TARGET_DIR=$MEMPROVE_VERIFY_CACHE/target cargo test"]`.

## The receipt

Each run writes `<git common dir>/memprove-verify/runs/<run id>/`, never into the worktree:

- `receipt.json` (schema `memprove.verify.receipt.v1`): the verdict and its reasons, the base commit and how it was
  chosen, the tree verified, the contract's source and SHA-256, every command with its exit code, duration and log
  files, the second run when there was one, and the findings (protected files added, changed or deleted, the
  contract changed, skip markers with file and line).
- `summary.txt`: what `run` prints; `memprove verify show [<RUN_DIR> | latest]` prints it again, `--format json` the
  receipt.
- `candidate/` and `restored/`: each command's standard output and standard error.
- `failure.txt` (verdict `fail`): the end of the first failing command's output. `protected.diff`: the work's
  changes to the protected paths.

`memprove run --test-format cargo -- memprove verify run` records the verification as test Evidence in memprove's
ledger: `run` ends with a `test result:` line that counts one case per check.

## The Claude Code hooks

`memprove verify init --hook` adds two handlers that run `memprove verify hook` to `.claude/settings.local.json`
(handlers of it already there are replaced; everything else in the file stays):

- **SessionStart** records the commit the session starts at. It is the base of the session's verifications, so work
  the agent commits during the session is still compared.
- **Stop** verifies the working tree each time the agent finishes. `fail` keeps the agent working: it is told which
  command failed and the end of its output, at most 3 times in a session; after that the person is told and the
  agent is let go. `needs_human` and an error of the verification itself never hold the agent; the person sees the
  summary. A tree that was already verified is not verified again, and a tree that is the base's is left alone, so
  stops that change no file cost nothing after the first.

The Stop handler's time limit is 3600 seconds. Each verified stop runs the checks on a clean copy, so for a project
with slow tests use the hook for unattended runs, or point the contract at the cache.

## Limits

- It finds work that fails, and passes that depend on changed tests or added skip markers. It does not find a wrong
  answer the tests accept, such as an expected value written into the code.
- The skip-marker list is fixed and textual: it misses other ways to skip, and it flags a marker a person would
  accept.
- Only Claude Code has a hook; with any other agent, run `memprove verify run` when it says it is done, or in CI.
- Submodules are not checked out in the clean copy.
- An agent that can write to `<git common dir>/memprove-verify/` or to the settings file can remove the hook or its
  records; keeping it from doing so is a matter for the permission rules, not for this command.
