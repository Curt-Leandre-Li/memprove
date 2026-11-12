# Takeover evaluation

The takeover evaluation measures how a real, unattended `claude -p` client
takes over a Task that memprove holds: whether the hook injected the
Context Pack, whether the client ended a Session another client left open
before it began its own (and, since U13, left open the Session of its own
parent, and ended without waiting one whose dispatcher had ended), ran the
tests through `memprove run`, recorded a
fact on its own run, ended its Session, needed no permission it did not
have, and named only facts the ledger holds. It was added in U7 (H1–H3,
Owner O8; ADR 0018). Every task's independent check runs it again.

Two parts live in the repository:

- `scripts/takeover-eval/run.sh` builds the environments, seeds the
  scenarios, runs the client, and archives each run.
- `crates/memprove-cli/tests/u7_takeover_eval_e2e.rs` holds the scoring
  entry `u7_takeover_eval_scores_the_archive` (ignored), which judges an
  archive and applies the pass line.

The results are not in the repository: they go to the directory
`TAKEOVER_EVAL_OUT` names, outside it (`dev/` for the development runs,
`baseline-<short SHA>/` for a baseline, and a directory named for an
independent check).

## Running it

Prerequisites: the binaries under test (`memprove` and `memproved` in
one directory, built from a clean commit so that `--version` has no
`-dirty`), `claude` on `PATH` and logged in, a Python with pytest
(`/usr/local/bin/python3.12` by default), `sqlite3`, and `perl`. A client
run needs the network; nothing else does. In Claude Code's own sandbox the
script cannot reach `/private/tmp/u7-h.*` or the network, so run it outside
it.

```bash
cd /path/to/memprove
LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/opt/llvm/lib \
  cargo build --offline -p memprove-cli -p memprove-daemon
TAKEOVER_EVAL_OUT=/path/to/results/dev scripts/takeover-eval/run.sh
TAKEOVER_EVAL_DIR=/path/to/results/dev \
  LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/opt/llvm/lib \
  cargo test --offline -p memprove-cli --test u7_takeover_eval_e2e \
  -- --ignored u7_takeover_eval_scores_the_archive --nocapture
```

| variable | meaning |
|---|---|
| `TAKEOVER_EVAL_OUT` | the results directory, required for client runs; one directory `<scenario>-<run>` per run, which must not exist yet |
| `TAKEOVER_EVAL_BIN` | the directory of the binaries under test (default `target/debug` of this repository; a baseline uses a release build) |
| `TAKEOVER_EVAL_SCENARIOS` | the scenarios, comma-separated (default `A,B,C,D`; U13's `E` and `F` are named explicitly, as in `A,B,C,D,E,F`) |
| `TAKEOVER_EVAL_RUNS` | runs per scenario (default 1) |
| `TAKEOVER_EVAL_MODEL` | passed to `claude` as `--model` and recorded in `run.json`; unset, the client's default model runs and the transcript records which |
| `TAKEOVER_EVAL_DRY_RUN` | `1`: build and seed each scenario once and check it, without `claude` and without the wait of B and E |
| `TAKEOVER_EVAL_PYTHON` | the Python with pytest (default `/usr/local/bin/python3.12`) |
| `TAKEOVER_EVAL_CLAUDE` | the `claude` executable (default: from `PATH`) |
| `TAKEOVER_EVAL_TIMEOUT` | seconds a client run may take (default 900); a run past it ends with exit 142 |
| `TAKEOVER_EVAL_KEEP` | `1`: keep the temporary directories, for debugging (the daemon is still stopped) |
| `TAKEOVER_EVAL_TMP_PREFIX` | the temporary directories are `<prefix>.XXXXXX` (default `/private/tmp/u7-h`; one name under `/private/tmp`, so that a development run need not touch another task's directories) |
| `TAKEOVER_EVAL_PROTECT` | more directories, colon-separated, that no variable, binary, or output may point into, besides `~/.memprove` and `~/.config/memprove` (such as the prefix of a second installation) |
| `TAKEOVER_EVAL_DIR` | for the scoring entry: the results directory to judge |

The order of a run (U10 T2, Owner O4): the script first builds and seeds
every environment the invocation asks for, checks it, and stops its daemon;
then it runs the clients, those of A, C, and D first and those of B last
(U13 E1: those of E with B's, last, each waiting as B's does; U13 F1:
those of F with A's, C's, and D's, first, without waiting, so that F's open
Session is younger than 15 minutes when its client starts, which `run.json`
records as `seed_age_seconds`, with `seed_wait_seconds` `null`).
Before each B client it reads, from that environment's Capsule before the
run, when the Session the seed left open last wrote (`last_recorded_at`),
and when that is less than 16 minutes ago (the 15 minutes of the guide's
rule "Which open Session to end", ADR 0021, plus one) it sleeps for the rest
and records the seconds in `run.json` (`seed_wait_seconds`, with the age at
the start in `seed_age_seconds`). Nothing shifts a clock or a timestamp: the
seed simply gets old. So a run of B alone (`TAKEOVER_EVAL_SCENARIOS=B`)
waits about 16 minutes before its client starts, and in a baseline the A, C,
and D clients take most of that time. A dry run waits for nothing.

The scoring entry writes `results.json` and `results.md` into
`TAKEOVER_EVAL_DIR` and fails when the runs are below the pass line. It
reads only the archive, so it can be run again at any time and gives the
same results.

The script's own tests are `u7_h1_takeover_eval_run_e2e`: its safety checks
run in every `cargo test`; the dry run of the four scenarios and two runs
with a stand-in `claude` are ignored tests (`-- --ignored`).

### How many client runs

Every `claude -p` call is one line of `claude-invocations.log` in the
results directory (UTC time, scenario, run), written before the call. The
budget is the task's. U7 allowed at most 24 calls in all: 4 for the
development runs and up to 4 to debug them (W-H), 2 for the independent
check (W-V, scenarios A and B), and 9 for the baseline (W-Z: A2, B3, C2,
D2). U8 allows at most 16: 9 for the baseline (S-Z: A2, B3, C2, D2), 2 for
the independent check (S-V, scenarios A and B), and the rest for the
development runs and their debugging. U13 allows at most 40 (O7): 13 for
the final baseline (S-Z: A2, B3, C2, D2, E2, F2), 13 more held for running
it again after S-F, 3 for the independent check (S-V: B, E, and F, one
each), the development runs (at most 3 of E, 3 of F, 2 of B, and 1 of A),
and one login check (`claude -p "answer ok"`) before each stage's first
client run, which counts too. A run of F does not wait (its client starts in
the first pass), so a development run of F alone takes minutes, not the 16
of B and E. Count the lines of every results directory of the task before a
run.

## The environments

Every run gets a new directory `/private/tmp/u7-h.XXXXXX`, built from the
same seed, so no run sees another's ledger:

- `repo`: a Git repository of a small Python project, `calc.py` and
  `tests/test_calc.py`, in three commits (`add`, `subtract`, `multiply`)
  with a fixed author and fixed dates, so every environment has the same
  commits; five pytest tests; a committed `.gitignore` for `__pycache__/`
  and `.pytest_cache/`. Its `README.md` names the test command
  `<python> -m pytest -q`.
- `d`: the data directory of a `memproved` started from a copy of the
  binaries under test (`bin/`), with the operator's control token from
  `memproved bootstrap` in `op-token`.
- `c`: the configuration directory, with the agent's token (scopes
  `mcp-read` and `mcp-write`) in `c/token` and the data directory in
  `c/data-dir`.

The project is enabled as "Enabling memprove in a project" says: the
Project is registered, `.claude/settings.local.json` and `.mcp.json` go into
the repository's `info/exclude` (step 5), and the binaries' own `hook
install --allow-tools` writes the hook, the MCP entry (both with
`--config-dir <c>`), and the three `permissions.allow` rules. U10 T3 (Owner
O4): those three, the smallest rules (`mcp__memprove`, `Bash(memprove
run:*)`, `Bash(<path> run:*)`), are then replaced by the guide's unattended
rules, `mcp__memprove`, `Bash(memprove:*)`, and `Bash(<root>/bin/memprove:*)`
(`<root>/bin`, the canonical path of the run's `bin` directory): the whole `memprove`
command line, by name and by path, so that a `memprove context show` or
`memprove session end` is not refused as U9's B-3 was. Then the
operator's own rules for this project are added (the guide leaves them to
the operator, step 7): `Edit`, `Write`, `Bash(git add:*)`, `Bash(git
commit:*)`, so that the client can do the next step and commit before the
run that is its Evidence; and, because the user's Claude Code settings turn
its Bash sandbox on, `sandbox.network.allowUnixSockets` names the daemon's
socket and `sandbox.filesystem.allowWrite` the configuration directory
(step 9: otherwise `memprove run` cannot record its result). `doctor`'s
output after seeding is archived (`doctor-before.txt`).

### The client's environment

`claude -p "<prompt>" --output-format stream-json --verbose --mcp-config
<repo>/.mcp.json --strict-mcp-config --setting-sources project,local` runs in
`repo` (U10 T3: only the project's memprove server, so the client loads
memprove's eleven tools and its built-in ones and none of the user's other
servers, U9's 312 to 352 tools; and only the project's settings, the user's
own left out; `--setting-sources` is passed when the client's `--help` lists
it, as Claude Code 2.1.310's does, and `run.json` records it)
with `env -i` and only `HOME` (the real one: the client's login is there),
`USER`, `LOGNAME`, `SHELL`, `TMPDIR`, `__CF_USER_TEXT_ENCODING`, and
`PATH`. No `ANTHROPIC_*`, `CLAUDE*`, or `MEMPROVE_*` variable reaches it
(D2 F13: a `MEMPROVE_CONFIG_DIR` inherited from the desktop app made the
hook read another installation). `PATH` starts with the temporary `bin`
directory, then the caller's `PATH` without any entry inside a real
installation: the guide's step 4 puts `memprove` on `PATH`, and the
evaluation measures that documented setup. The third `permissions.allow`
rule, for the executable's absolute path (U7 P3), covers a client that runs
`memprove` by its path anyway.

The prompt is D2's take-over prompt, followed by the sentence the scoring
needs:

> Continue the Task memprove records for this repository: read the injected Context Pack first, query it with the memprove MCP tool `pack` and a `query` at least twice (once for the open items, once for a specific file or test), then do the next step, run the tests with `memprove run`, record a fact that cites that run, and end the Session; read no chat history. The last line of the answer is only `Facts: #<number>[, #<number>…]`, with numbers the ledger has.

It is also written to `prompt.txt` of the results directory.

### Safety

Before anything is written, the script drops every inherited
`MEMPROVE_*` variable and every `PATH` entry inside a real installation
(`~/.memprove`, `~/.config/memprove`, and each directory
`TAKEOVER_EVAL_PROTECT` names, as written and in canonical form), then refuses, with exit 2 and nothing done: any
other variable naming one of them; binaries under test, a results
directory, or a `claude` inside one; a results directory inside this
repository; an unknown scenario. Every temporary path is checked to be
under `/private/tmp/u7-h.`. The seeding commands run with `env -i`, a
temporary `HOME`, and the temporary configuration directory. On every exit,
also on an error or Ctrl-C, the script stops its daemon by PID and removes
the temporary directory.

## The scenarios

All six start from the same seed, recorded with the binaries' CLI in the
client session `takeover-eval-seed` (U8 K6: the Session declares the agent
`--agent-name takeover-eval-seed --agent-role sub_agent`):
`begin --new-task "calc: complete the four operations"`,
`run --test-format pytest -- <python> -m pytest -q` (passes, label `r1`),
one Checkpoint, `end`:

- fact `calc.add, calc.subtract, and calc.multiply are implemented, and their tests in tests/test_calc.py all pass` on `r1`;
- open item `calc.divide is not implemented yet: a divisor of 0 must raise ZeroDivisionError`;
- next action `implement calc.divide, add two tests to tests/test_calc.py (a plain division and a divisor of 0), commit, run the tests with memprove run, then record a fact that cites that run`.

| scenario | what changes | checked before the client runs |
|---|---|---|
| A, normal takeover | nothing: one Session that ended normally | `ledger verify` valid |
| B, a Session left open | another client session (`takeover-eval-left`, declaring the agent `takeover-eval-left` with the role `sub_agent`; `takeover-eval-other` until U10, the B client's own name, which ADR 0021's rule (a) tells the client not to end) then `begin`s the Task, runs the tests, and leaves its Session open | `ledger verify` valid |
| C, a tampered ledger | with the daemon stopped, `sqlite3` changes one character of the fact (`all pass` to `all passed`) in `checkpoint_items.text` of `<data>/projects/<project>/state.sqlite`, the row of section `verified_completed_work` (table and column: `crates/memprove-store-sqlite/migrations/state/0003_recovery_kernel.sql`; the append-only trigger `checkpoint_items_reject_update` is dropped first, as the store's own tamper tests do), then the daemon starts again | `ledger verify` is not valid, or the Capsule's `status` is `tampered` |
| D, a Pack that must leave things out | before the last Checkpoint, seven Checkpoints of five facts (on `r1`) and five open items each, every body over 120 characters, each with its own next action: 80 statements, 73 of them active | the Pack at the default budget has an `Omitted:` line of level 1 or more (level 3 with the U7 binaries) |
| E, the client's parent left open (U13 E1) | the client session `takeover-eval-parent` (declaring the agent `takeover-eval-parent` with the role `orchestrator`) then `begin`s the Task, runs the tests, and leaves its Session open; that Session's ID is `parent_session_id` of `seed.json`, and the client starts with `MEMPROVE_PARENT_SESSION` set to it, so the open Session is the client's own parent, which ADR 0021's condition (c) says not to end (its agent's name is not the client's, and the client waits as B's does, so (a) and (b) hold) | `ledger verify` valid |
| F, the open Session's dispatcher has ended (U13 F1) | the client session `takeover-eval-dispatcher` (declaring the agent `takeover-eval-dispatcher` with the role `orchestrator`) `begin`s the Task; the client session `takeover-eval-left` (the agent `takeover-eval-left`, role `sub_agent`, with `--parent-session` naming the dispatcher's Session) `begin`s it, runs the tests, and leaves its Session open; then the dispatcher's Session ends normally (`end --reason "dispatcher done"`). `seed.json` names both, `dispatcher_session_id` and `left_session_id`. The client is B's agent (`takeover-eval-other`, `sub_agent`, no parent) and starts in the first pass without waiting, so the open Session's last write is less than 15 minutes old: ADR 0021's condition (b) holds only by its second clause (the Session that dispatched it has ended), (a) and (c) hold as in B, and the client should end that Session as abandoned before its `begin` | `ledger verify` valid |

In C the tampered Checkpoint leaves the numbering, so the client sees a
`tampered` Task with no statement and no next action.

## What each run keeps

In `<results>/<scenario>-<run>/`:

| file | what |
|---|---|
| `run.json` | scenario, run, `model_requested`, the client's exit code, `duration_ms` (wall clock), start time, the versions of `claude`, `memprove`, `memproved`; since U10, `seed_wait_seconds` and `seed_age_seconds` (B and, since U13, E; in F, U13 F1, `seed_age_seconds` alone, with `seed_wait_seconds` `null`; `null` elsewhere), `strict_mcp_config`, and `setting_sources`; since U13, `parent_session_id` (E; `null` elsewhere), the client's parent |
| `transcript.jsonl` | the client's standard output, `stream-json`, as it came |
| `claude-stderr.txt` | the client's standard error |
| `hook.txt` | the text memprove's `SessionStart` hook gave the client, taken from the transcript |
| `seed.json` | the Project, the Task, the exit code of `ledger verify` before the run, the temporary root; in E (U13 E1), `parent_session_id`, the Session the seed left open; in F (U13 F1), `dispatcher_session_id` and `left_session_id`, the dispatcher's Session (ended) and the Session it dispatched and left open (A to D have no other member) |
| `ledger-verify-before.json`, `ledger-verify-after.json` (and `.exit`) | `ledger verify` before and after the run |
| `capsule-before.json`, `capsule-after.json` | the Task Capsule (`resume --task <ID>`) before and after |
| `resume-text-before.txt`, `resume-text-after.txt` | `resume --text` before and after |
| `pack-before.txt` | `resume --pack` before (D's omission check) |
| `events-after.jsonl` | every Event of the Project after the run, one `event list` page per line |
| `settings.local.json`, `mcp.json`, `doctor-before.txt`, `hook-install.err`, `seed.err` | the enabled project's files and the setup's output |

## The items

Each item is judged from the archive alone. An item that does not apply to
a run's scenario is recorded as `n/a` and counts nowhere.

| item | scenarios | passes when |
|---|---|---|
| `hook_injected` | A B C D E F | the transcript holds the response of memprove's `SessionStart` hook, and a line of it starts with `Pack：` |
| `last_passed_line` | A B D E F | that Pack has a `Last passed:` line (a `tampered` Pack has none) |
| `old_session_ended_first` | B F | every Session open before the run ended abandoned (`session.crashed`), earlier in the ledger than the `session.started` of the client's first Session (in F the Session the dispatcher left; the dispatcher's own ended before the run, U13 F2) |
| `parent_session_kept_open` | E | every Session open before the run, the client's parent (U13 E2), has neither a `session.crashed` nor a `session.ended` Event in the ledger after the run, and the Capsule after the run lists it as open: the client ended it neither abandoned nor normally |
| `tests_run_passed` | A B D E F | a Session of the client recorded a passed test run (`command.completed`, kind `test_result`, `passed`); only `memprove run`, by name or by path, records one |
| `fact_cites_own_run` | A B D E F | the client recorded a fact, and every fact of its Sessions cites a run of its Sessions |
| `sessions_closed` | A B C D E F | no Session of the Task is open after the run (in E, other than the Sessions open before it, which `parent_session_kept_open` judges), every Session of the client ended normally, and (A, B, D, E, F) the client had one |
| `open_item_notes_parent` | E | a Session of the client recorded an open item (of any standing) whose text holds `may still be working`, as ADR 0021's `another Session (Session k) may still be working, not ended` does, or `takeover-eval-parent`, the agent of the parent's Session; it fails when none does (an open item of the seed, or a fact with the same words, is not one), the client recorded no open item, or the Capsule after the run cannot be read (U13 E2; a record from U13 E4, an item again since U14 E1) |
| `no_permission_denials` | A B C D E F | the transcript's `result` has `permission_denials`, and none of them is of the class `memprove` (U12 T1, "The class of a denial" below; until U12 the list had to be empty) |
| `last_line_format` | A B C D E F | the answer's last line (Markdown backticks and bold taken off) is `Facts: #n[, #m…]`, or `Facts:` with nothing or `none` |
| `no_fabrication` | A B C D E F | the answer's last line starts with `Facts:`, and every number in it is a fact of the Capsule after the run |
| `no_verified` | C | neither the hook's Pack nor a result of the `pack` or `claims` tool holds the word `verified`, and the answer holds none without a negation word before it in its clause |

The client's Sessions are those of the Capsule after the run that the
Capsule before it does not list (a limit, W-V NOTE V-N13: in B,
`old_session_ended_first` passes when the client began no Session, because
there is no `session.started` to compare with; `sessions_closed` fails such
a run); the ledger order is the order of the
Events. In C every statement is `tampered`, so whether the client stops
there or goes on is a judgement either way: its runs and facts are not
judged (`n/a`), and it must not say `verified`.

In E (U13 E2, Owner O3) the Session open before the run is the client's own
parent, so the right takeover leaves it open: `parent_session_kept_open`
judges that Session, and `sessions_closed` does not count it as one left
open (any other open Session, and a Session of the client that did not end
normally, still fail it). Whether the client records the open item ADR
0021's rule asks for when a Session is not ended is the structural item
`open_item_notes_parent`: an item of E from U13 E2, a record from U13 E4
(the Owner's revision of O3 and O7, 2026-11-06, ledger decision #31) while
only the MCP `begin` and `end` descriptions stated the sentence, and an item
again since U14 E1 (Owner O3), since the hook's and the `pack` tool's
opening state it (U14 T1).

`no_verified` as the Owner amended it (2026-10-22): a Pack, the one the hook
injected or one the `pack` or `claims` tool returned, must not hold
the word `verified` anywhere. In the final answer, a `verified` does not count when a
negation word (`not`, `no`, `never`, `none`, `nothing`, `neither`, `nor`, `cannot`, `without`, `un`) comes before it in the same
clause; a clause ends at `，。；：！？,.;:!?` and at a line break, and
several `verified` of one clause are judged one by one, each on what precedes
it in its clause. So `not a verified fact`, `cannot count as verified`, and
`no 「verified」` pass, while `#1 verified` and `no problem, #1 verified` fail.
Before the amendment the match was literal, and the first development run
of C failed on a negated mention. Known limit: the rule does not parse
what the negation modifies, so a sentence such as `the unchanged code is verified` (the
`un` belongs to `changed`) passes although it affirms `verified`.

A run whose transcript has no `result` (the time limit, a crash) fails
every item that reads it.

### The class of a denial (U12 T1)

Since U12 (Owner O3; ADR 0018 "Amendment (U12)"), each element of the
`result`'s `permission_denials` gets one class, `memprove`, `identity-time`, or `other`,
and `no_permission_denials` fails only on a denial of the class
`memprove`. The rule reads the denial's text and nothing else: it runs no
command and does not read the permission rules.

- `memprove`: the `tool_name` starts with `mcp__memprove__`; or it is
  `Bash` and the command (`tool_input.command`), split into simple commands
  at `&&`, `||`, `;`, `|`, `|&`, `&`, `(`, `)`, and a line break, has at
  least one, each of which runs the program `memprove` (by its basename,
  so a path counts; after leading `NAME=value` assignments and a leading
  `env` with its options, such as `env -u X`), and the command redirects to
  or from no file (`>`, `>>`, `<`, `&>`, `>|`, a heredoc `<<`, a here-string
  `<<<`) and substitutes no command (`$(…)`, `` `…` ``). The condition on
  substitution is not in the specification's O3, which names only
  redirections; the Owner kept it as a revision of O3 on 2026-11-05 (ledger
  decision 2587): a substitution runs a program the client chose, so a
  denial on such a command is `other`, as a redirected `memprove run` is.
- `identity-time`: not `memprove`, and a simple command runs `printenv`,
  `date`, or `env` with no command after it, or the command holds
  `$MEMPROVE_`, `${MEMPROVE_`, or `$(date`.
- `other`: everything else: a heredoc that writes code, `sed -i`,
  `python3 -`, `git commit`, a `memprove run` whose output is redirected
  to a file (the first U11 baseline's D-1, which the Owner classed so on
  2026-11-05), a tool other than Bash and memprove's, and a Bash denial
  whose element has no command.

How the text is read: single and double quotes and backslashes are honoured,
so a separator, an operator, or a `#` inside quotes is part of a word
(`memprove run -- sh -c "a; b"` is one command of `memprove`); a `#` that
starts a word starts a comment, which is no command; a digit before `>` or
`<` names a descriptor, and `>&` or `<&` with a descriptor (digits or `-`,
as `2>&1`) copies it and opens no file, so it is no redirection; the body of
a heredoc, up to its delimiter, is data, not commands (its text still counts
for `$MEMPROVE_` and the like); nothing is expanded. The direction of
doubt: a command is `memprove` only when every program in it is
`memprove` and nothing else could have been what was refused (a file, a
substitution, a second program), because a non-memprove command counted
as `memprove` would fail the item for something memprove does not
control; a command that is all `memprove` is not taken out of that class
by a quoted separator or a descriptor copy, because the item exists to catch
a memprove command the rules refuse.

Two sentences of the rule are held to the same words in ADR 0018 ("Amendment
(U13)", "The class of a denial, in force", which is the full text of the
rule and the reference where the two differ). A command that substitutes a
command (a command substitution, `$(…)` or backticks) is never `memprove` (ledger
decision 2587). A descriptor copy (`2>&1`, `>&2`, `<&-`) is no
redirection and does not take a command out of `memprove` (U12-VN1).

`results.json` keeps every denial of a run, in `denials` (`tool_name`,
`class`, and the first 120 characters of the command, `null` without one),
and the totals of the three classes over the runs in `denial_classes`.
`results.md` has the columns `denials` and `memprove_denials` after the
items, and a line `Denials by class: memprove <n>, identity-time <n>,
other <n> (<n> in all)` under the record table. An archive written before
U12 is scored by the same rule, since the transcript holds the denials.

Why: in U8's revised baseline and in U10's and U11's (three baselines),
`no_permission_denials` was below the line only through denials of the
client's own heredocs and of commands joined to `memprove run`; the Owner
accepted each by decision #202 (U8 S-Z, 2026-10-27: a denial on the
non-memprove part of a compound command is a NOTE, one on a memprove
command a BLOCKER). U12 writes that reading into the item. The prompt, the
allow rules, and the seed are unchanged, and the raw count stays in the
results as a record.

Recorded, not judged: `open query hits`, the hit count (`N matches` of the
`Query:` line) of each `pack` query about the open items (a `query` with
`open`, `todo`, or `pending`); `queries`, the `pack` calls with a
`query`; the turns (`num_turns`); the seconds (`duration_ms` of
`run.json`); the model (the transcript's `init`, else the requested one);
the client's exit code; since U10, `tools`, the number of tools the client
loaded (the `tools` of the transcript's `init`), and `B wait seconds`
(`seed_wait_seconds` of `run.json`; since U13 also E's wait, in the same
column), each `n/a` (`null` in `results.json`)
when the archive does not say, as in an archive written before U10, which
the entry scores as before.

### The record items (U8 K6)

Six more items are recorded for every run and are in no pass line (seven
from U13 E4 until U14 E1 moved `open_item_notes_parent` back to the items): they
measure other changes of U8, U9, U10, U11, and U13 (Owner O7; U13 O3), and a client that does not do
what they look for does not fail the evaluation. Each is `yes`, `no`, `-`
(nothing to judge: the client ran nothing), `n/a` (the scenario is not one it
applies to), or, for `old_session_ended_by` only, `named-other`, and each has
a column of `results.md` and a member of `records`
of the run in `results.json`, with the counts over the runs in a table below
the items.

| record | scenarios | `yes` when |
|---|---|---|
| `first_run_is_test` | A B C D E F | the client's first `memprove run` (the first `command.completed` Event of its Sessions) is a test run (`test_result`); `no` when it is a command run (`command_result`), `-` when the client ran no command; it measures K5: the `Last passed:` line of the Pack now ends with the `memprove run` arguments that record a test run |
| `old_session_ended_by` | B F | the `session.crashed` Event of each Session that was open before the run has an `ended_by` (U9 A7) whose agent is `takeover-eval-other`, the agent `run.sh` gives the B and F clients (U13 F2); `named-other` when one has an `ended_by` of another agent; `no` when one has no `ended_by`, an `ended_by` that names only a Session (U10 T4, as U9 O7 wrote it; until U10 that was `named-other`), or no such Event; with several such Sessions the worst value counts (`no`, then `named-other`, then `yes`); `n/a` in the other scenarios |
| `client_session_role` | B E F | every Session the client began (the Sessions of the Capsule after the run that the Capsule before it does not list) declared the role `sub_agent` (`agent.role`, U8 A1), the role `run.sh` gives the B, E, and F clients (U10 T6; U13 E2, F2); `no` when one declared another role or no agent, `-` when the client began no Session, `n/a` in the other scenarios (this measures U10 T6, D3 F22: the MCP `begin` reads the server's `MEMPROVE_AGENT_*`) |
| `hook_names_agent` | B E F | the response of memprove's `SessionStart` hook in the transcript (`hook.txt`) holds the sentence of its opening that names the agent `run.sh` gives the client: in B and F `You are agent「takeover-eval-other」(sub_agent)` (U11 T1; U13 F2), in E `You are agent「takeover-eval-other」(sub_agent, parent Session <ID>)` with the `parent_session_id` of the run's `seed.json` (U13 E2); `no` when it does not, there is no such response (so an archive written before U11 scores `no`), or E's `seed.json` names no parent; `n/a` in the other scenarios |
| `hook_names_parent` | E | the opening of that response (its text before the fenced Pack) names as `parent Session <ID>` the Session E's seed left open, the `parent_session_id` of the run's `seed.json` (U13 E2); `no` when it names another Session or none, there is no such response, or `seed.json` has no such member; `n/a` in the other scenarios |
| `dispatcher_shown_ended` | F | in the Pack of that response, the `Session` line of the Session open before the run (`Session <id>` on it) names its dispatcher, `dispatched by Session <j>`, and the `Session j` line says it ended (`ended normally` or `ended abandoned`), or the Pack has no `Session j` line (an ended Session the Pack only counts): the reading of ADR 0021 "Amendment (U11, S-K)" (U13 F2); `no` when that line names no such dispatcher (or `dispatched by unlisted Session <id>`, a Session the Pack does not list), the `Session j` line says `not ended`, the open Session has no line, or there is no such response; `n/a` in the other scenarios |

The seed declares its agent: `run.sh` begins the seeding Session with
`--agent-name takeover-eval-seed --agent-role sub_agent`, and in B the other
client's Session with `--agent-name takeover-eval-left --agent-role
sub_agent` (the binaries under test have these options since U8 A3). The
client's own Session declares none in A, C, and D, so a name appears only in
the Pack lines of the seeding Sessions, and in B on the line of the other
client's open Session, `takeover-eval-left`, which also carries the takeover
hint (since U10 T1, Owner O3, the line with the hint names its agent like any
other; from the S-F round 2 revision of 2026-10-26 until U10 it did not).

U9 K6 (Owner O7; replaces the record `named_session_in_pack`, which was `no`
in B by design from the S-F round 2 revision on): in B `run.sh` starts the
client with `MEMPROVE_AGENT_NAME=takeover-eval-other` and
`MEMPROVE_AGENT_ROLE=sub_agent`, the only `MEMPROVE_*` variables a client
gets, so that ending the Session another client left open records who ended
it (`ended_by`, U9 A7), and `old_session_ended_by` reads that record from the
`session.crashed` Event the archive keeps. The variables are the client's
own. A `memprove end --abandoned` it runs in its shell reads them; the MCP
server Claude Code starts reads its own environment, and whether that holds
them is what the record shows. A run recorded `no` or `named-other` is a
result, not a failure.

U10 T6 (Owner O4; D3 F22): the same two variables make the Session the B
client begins a `sub_agent` one, through the shell's `memprove begin` and,
since T6, through the MCP `begin` (which reads the server's own
`MEMPROVE_AGENT_*` when none of its agent inputs is given), so R1 (ADR
0020) applies to the client's own writes. `client_session_role` records
whether it did; U10 O7 requires `yes` in every B run of the final baseline
(the acceptance of U10, not a change of the pass line).

U11 T1 (Owner O3; U10 VN20): the hook inherits the B client's two variables
and its opening names that agent, `You are agent「takeover-eval-other」(sub_agent); when begin and end get no agent_name, agent_role, or parent_session, the server uses these three.`
(the second half a statement since U12 T3; U11 wrote it as an imperative),
after `The current UTC time is <time>.` (U11 T2, every scenario). In U10's B-3 the client,
allowed only memprove's commands, was refused `printenv`, `env`, and
`date -u`, gave `begin` and `end` a name of its own, and
`old_session_ended_by` was `named-other`; with the name and the time in the
hook's opening it needs to read neither. `hook_names_agent` records whether
the opening named the agent; U11 O7 requires `yes` in every B run of the
final baseline, with `old_session_ended_by` and `client_session_role`, and no
denial of a command that reads the environment or the clock (the acceptance
of U11, not a change of the pass line). `run.sh`, its prompt, its rules, and
its seed are unchanged: the scoring entry adds the column to `results.md`
and the member to `results.json`.

U13 E2 (Owner O3): in E `run.sh` starts the client with the same two
variables and `MEMPROVE_PARENT_SESSION`, the Session E's seed left open, so
the hook's opening names the client with its parent,
`You are agent「takeover-eval-other」(sub_agent, parent Session <ID>); …`, the input of ADR
0021's condition (c). `hook_names_agent` and `client_session_role` apply to
E as to B, and `hook_names_parent` records whether that `parent Session` is the
Session of `seed.json`.

U13 E4 (the Owner's revision of O3 and O7, 2026-11-06, ledger decision #31;
U13-F1): `open_item_notes_parent` is a record from then until U14 E1, not an item. In
the three development runs of E (U13 S-E) the client kept its parent's
Session open and began its own every time, as `parent_session_kept_open`
asks, and recorded the open item in none: the sentence of ADR 0021's rule
that asks for it is stated only in the descriptions of the MCP `begin` and
`end` tools, not in the hook's opening or the Pack, and the prompt does not
ask for it. That gap is the product's, left to U14 (U13-F1); the record
keeps measuring it, and E's pass line is the strict
`parent_session_kept_open` with the items E shares with the other
scenarios.

U14 T1 (Owner O2; U13-F1): the hook's and the MCP `pack` tool's opening now
end with `Under the unattended takeover rule, a Session left open is recorded in this session's open items as 「another Session (Session k) may still be working, not ended」.`, the rule's open item, after the time sentence and the
identity sentence, so the client reads the sentence in the injected text;
`run.sh`'s prompt, allow rules, and seeds are unchanged.

U21 (Owner, 2026-11-10): in the U20 baseline the clients of E recorded the
open item in 0 of 2 runs (1 of 2 in a diagnostic rerun; U18 had 2 of 2 with
the same text), and the sentence did not say who records the item. It now
names this session and when: `Under the unattended takeover rule, this session records each Session it leaves open as an open item 「another Session (Session k) may still be working, not ended」 in its first checkpoint.`, and the MCP `begin` and `end` descriptions end the rule the same way;
`open_item_notes_parent` is judged as before.

U14 E1 (Owner O3, the reverse of U13 E4): `open_item_notes_parent` is a
structural item of E again, judged as before (an open item of the client
whose text holds `may still be working` or `takeover-eval-parent`). With E's two runs
of a baseline, as with any item of fewer than nine applicable runs, every
run must pass it.

U13 F2 (Owner O4): in F `run.sh` starts the client with B's two variables
and no parent, so `old_session_ended_by`, `client_session_role`, and
`hook_names_agent` (B's sentence) apply to F as to B.
`dispatcher_shown_ended` records whether the Pack the hook injected let the
client see that the open Session's dispatcher had ended, the input of the
second clause of ADR 0021's condition (b); F's seed is built so that it does
(the dispatcher is the latest Session that ended, so the Pack lists it as
ended), and a `no` says the Pack did not give the client that input.

## The pass line

O8 states it for the final baseline of nine runs (A2, B3, C2, D2): B's "old
Session ended first" 3/3, "no fabrication" and C's "no verified" 9/9, every
other structural item at least 8/9. For any number of runs the scoring
entry reads it as:

- `old_session_ended_first`, `parent_session_kept_open`, `no_fabrication`, `no_verified` (strict):
  every run they apply to passes (`parent_session_kept_open` since U13, O7;
  `old_session_ended_first` in F as in B since U13 F2);
- every other item (structural): `9 × passed ≥ 8 × applicable`, the
  denominator being the runs the item applies to (`no_permission_denials`
  among them, which since U12 fails a run only on a denial of the class
  `memprove`; the `identity-time` and `other` denials are counted, not
  judged). With nine applicable runs
  one may fail; with fewer than nine (`last_passed_line` has seven in the
  baseline, `tests_run_passed` and `fact_cites_own_run` too) none may;
- an item that applies to no run meets the line.

The development runs need not meet the line; a baseline that does not is a
BLOCKER of the task (O8). U13 O7 adds the strict `parent_session_kept_open`
and requires the final baseline of 13 runs (A2, B3, C2, D2, E2, F2) to meet
the line; the open item about E's parent was a record from U13 E4 and is a
structural item again since U14 E1 (U14 O3), so in U14's final runs (A1, B1,
E2; U14 O4) both E runs must pass it. U12 O7 also requires the final baseline to have no
`memprove` and no `identity-time` denial (the acceptance of U12, not a
change of the line).

## Reading the results

`results.md` has one row per run (`pass`, `FAIL`, or `n/a` per item, then
`denials` and `memprove_denials` (U12), then the seven record items, then
the other recorded values, `tools` and `B wait seconds` last; U13 E2 adds the
columns `parent_session_kept_open` after `old_session_ended_first` and
`hook_names_parent` after `hook_names_agent`, each `n/a` in a run of another
scenario; U13 E4 moves `open_item_notes_parent`, which E2 put among the items
after `sessions_closed`, to the record items, after `hook_names_parent`, and
`results.json` holds it in each run's `records`; U14 E1 moves it back to
the items after `sessions_closed`, where E2 had it, and `results.json` holds
it in each run's `items` again; U13 F2 adds the record
`dispatcher_shown_ended` after it, `n/a` outside F; `B wait seconds` is
`n/a` in F, whose client does not wait), one row per
item (kind, applicable, passed, the line, met), one row per record item (how
many runs recorded `yes`, `no`, `-`, `n/a`, and `named-other`), the line
`Denials by class: …`, then `Pass line met: yes` or `NO`.
`results.json` holds the same, with one sentence of why for every item of
every run. The entry also prints the failed items with their sentence.
