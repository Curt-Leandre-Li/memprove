#!/bin/bash
# The takeover evaluation (U7 H1, Owner O8; ADR 0018,
# docs/takeover-eval.md): a real `claude -p` client takes over a
# Task that memprove holds, in six scenarios, and every run is archived
# for the scoring entry `u7_takeover_eval_e2e` to judge.
#
#   TAKEOVER_EVAL_OUT=<dir> scripts/takeover-eval/run.sh
#   TAKEOVER_EVAL_DRY_RUN=1 scripts/takeover-eval/run.sh
#
# Scenarios: A, a normal takeover (one Session that ended normally after a
# passed test run, a fact, an open item, and a next action); B, A plus a
# Session another client session left open; C, A with one character of a
# fact's text changed in the state database behind the daemon's back; D, a
# Task of more than 70 statements, so that the Pack leaves things out; E
# (U13 E1, Owner O3), A plus a Session of the agent `takeover-eval-parent`
# (an orchestrator) left open, which is the client's own parent: the client
# starts with MEMPROVE_PARENT_SESSION naming it, so ADR 0021's condition (c)
# does not hold and the client must leave that Session open; F (U13 F1, Owner
# O4), A plus a Session of the agent `takeover-eval-left` (a sub_agent) left
# open, dispatched by a Session of the agent `takeover-eval-dispatcher` (an
# orchestrator) that then ended normally: under ADR 0021's condition (b) a
# Session whose dispatcher has ended looks gone however recent its last
# write, so the client must end it as abandoned before its `begin`, without
# waiting.
#
# U10 T2 (Owner O4): every environment of the invocation is built and seeded
# first, its daemon stopped until its client runs; then the clients run, A, C,
# and D first and B last, and each B client starts only once the Session the
# seed left open has not written for 16 minutes (the 15 minutes of ADR 0021's
# rule, plus one), sleeping for what is left and recording the wait. So a run
# of B alone waits about 16 minutes. Nothing shifts a clock or a timestamp.
# U13 E1: E's clients run with B's, last, and wait the same way, so that of
# the rule's three conditions only (c) fails in E. U13 F1: F's clients run
# with A's, C's, and D's, first, and do not wait, so that (b) holds in F only
# by its second clause (the Session that dispatched it has ended).
#
# Every run gets a new environment under /private/tmp/u7-h.*, built from the
# same seed: a Git repository of a small Python project (three commits, five
# pytest tests), a data directory, a memproved of the binaries under test,
# a configuration directory with the agent's token, and the project enabled
# with the binaries' own `hook install --allow-tools`, its memprove rules
# then widened to the whole `memprove` command line (U10 T3). The client then
# runs in the repository with a clean environment: only HOME is the real one
# (the client's login is there), every ANTHROPIC_*, CLAUDE*, and
# MEMPROVE_* variable is left out, and PATH starts with the temporary bin
# directory; it loads only the project's memprove MCP server
# (--mcp-config <repo>/.mcp.json --strict-mcp-config) and, when the client
# knows the option, only the project's settings (--setting-sources
# project,local). Each run's directory under TAKEOVER_EVAL_OUT keeps the
# transcript, the hook's text, `ledger verify` before and after, the Task
# Capsule and `resume --text` before and after, the ledger's Events after,
# the time taken, the exit code, and the model.
#
# Variables:
#   TAKEOVER_EVAL_OUT        the results directory (required unless a dry
#                            run); one subdirectory <scenario>-<run> per run
#   TAKEOVER_EVAL_BIN        the directory of the memprove and memproved
#                            under test (default: target/debug of this
#                            repository)
#   TAKEOVER_EVAL_SCENARIOS  the scenarios, comma-separated (default A,B,C,D)
#   TAKEOVER_EVAL_RUNS       the runs per scenario (default 1)
#   TAKEOVER_EVAL_MODEL      passed to claude as --model when set
#   TAKEOVER_EVAL_DRY_RUN    1: build and seed each scenario once and check
#                            it (C tampered, D omitting), without claude and
#                            without waiting
#   TAKEOVER_EVAL_PYTHON     the Python with pytest for the temporary project
#                            (default /usr/local/bin/python3.12)
#   TAKEOVER_EVAL_CLAUDE     the claude executable (default: from PATH)
#   TAKEOVER_EVAL_TIMEOUT    seconds a client run may take (default 900)
#   TAKEOVER_EVAL_KEEP       1: keep the temporary directories (debugging)
#   TAKEOVER_EVAL_TMP_PREFIX the temporary directories are <it>.XXXXXX (default
#                            /private/tmp/u7-h; one name under /private/tmp)
#   TAKEOVER_EVAL_PROTECT    more directories nothing may point into, colon-
#                            separated, besides ~/.memprove and
#                            ~/.config/memprove (such as a second installation)
#
# Before anything else it checks that nothing of a real installation can be
# read or written (see "Safety" below); a failed check exits 2 and does
# nothing. On exit, also on an error or Ctrl-C, it stops the daemon it
# started by its PID and removes the temporary directory.
set -euo pipefail
umask 077

readonly SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
readonly REPO_TOP="$(cd "$SCRIPT_DIR/../.." && pwd -P)"

# D2's take-over prompt (the second dogfooding round, S3)
# and the sentence O8 adds for the scoring of the last line.
readonly PROMPT='Continue the Task memprove records for this repository: read the injected Context Pack first, query it with the memprove MCP tool `pack` and a `query` at least twice (once for the open items, once for a specific file or test), then do the next step, run the tests with `memprove run`, record a fact that cites that run, and end the Session; read no chat history. The last line of the answer is only `Facts: #<number>[, #<number>…]`, with numbers the ledger has.'

say() { printf 'takeover-eval: %s\n' "$*" >&2; }
die() {
  say "error: $*"
  exit 1
}
refuse() {
  say "refused: $*"
  exit 2
}

# The temporary directories of the runs are $TMP_PREFIX.XXXXXX. U9 K6: a run
# that must not touch /private/tmp/u7-h.* (a development run) moves the prefix
# with TAKEOVER_EVAL_TMP_PREFIX, which stays one name under /private/tmp.
TMP_PREFIX="${TAKEOVER_EVAL_TMP_PREFIX:-/private/tmp/u7-h}"
case "$TMP_PREFIX" in
  /private/tmp/?* )
    case "${TMP_PREFIX#/private/tmp/}" in
      */* | *..*) refuse "TAKEOVER_EVAL_TMP_PREFIX must be one name under /private/tmp" ;;
    esac
    ;;
  *) refuse "TAKEOVER_EVAL_TMP_PREFIX must be one name under /private/tmp" ;;
esac
readonly TMP_PREFIX

# Whether $1 is $2 or inside it.
inside() {
  case "$1" in
    "$2" | "$2"/*) return 0 ;;
  esac
  return 1
}

# The canonical path of $1, which exists.
canonical() { (cd "$1" && pwd -P); }

# The canonical form of $1, which need not exist: its nearest existing
# ancestor made canonical, the rest appended.
absolute() {
  local path="$1" rest=""
  case "$path" in /*) ;; *) path="$PWD/$path" ;; esac
  while [ ! -d "$path" ]; do
    rest="/$(basename "$path")$rest"
    path="$(dirname "$path")"
  done
  printf '%s%s\n' "$(canonical "$path")" "$rest"
}

# --- Safety: nothing of a real installation is read or written ------------
#
# Checked before any file is written or any program but the shell's own
# runs: a failed check exits 2 and does nothing.

# The real installations no variable, binary, or output may point into,
# as written and in canonical form: the default installation's prefix and
# configuration directory, and (U22) each directory TAKEOVER_EVAL_PROTECT
# names, colon-separated, such as the prefix of a second installation.
PROTECTED=("$HOME/.memprove" "$HOME/.config/memprove")
if [ -n "${TAKEOVER_EVAL_PROTECT:-}" ]; then
  IFS=: read -r -a protect_entries <<<"$TAKEOVER_EVAL_PROTECT"
  for entry in "${protect_entries[@]}"; do
    [ -n "$entry" ] && PROTECTED+=("$entry")
  done
fi
FORBIDDEN=()
for forbidden in "${PROTECTED[@]}"; do
  FORBIDDEN+=("$forbidden")
  canonical_form="$(absolute "$forbidden")"
  [ "$canonical_form" = "$forbidden" ] || FORBIDDEN+=("$canonical_form")
done

# Whether $1 (canonical) is inside a real installation.
forbidden_path() {
  local forbidden
  for forbidden in "${FORBIDDEN[@]}"; do
    inside "$1" "$forbidden" && return 0
  done
  return 1
}

# Every MEMPROVE_* variable is dropped first: none is an input of this
# script (the desktop app exports MEMPROVE_CONFIG_DIR of the trial
# installation and a MEMPROVE_CLIENT_SESSION; D2 F13), and every command
# below gets its directories explicitly.
for name in $(env | sed -n 's/^\(MEMPROVE_[A-Za-z0-9_]*\)=.*/\1/p'); do
  unset "$name"
done

# PATH without any entry inside a real installation.
clean_path=""
IFS=: read -r -a path_entries <<<"$PATH"
for entry in "${path_entries[@]}"; do
  if [ -n "$entry" ] && ! forbidden_path "$entry" && ! forbidden_path "$(absolute "$entry")"; then
    clean_path="${clean_path:+$clean_path:}$entry"
  fi
done
export PATH="$clean_path"

# No variable that is left names a real installation (the script's own
# TAKEOVER_EVAL_* inputs are checked one by one below).
while IFS= read -r line; do
  case "$line" in TAKEOVER_EVAL_*) continue ;; esac
  for forbidden in "${FORBIDDEN[@]}"; do
    case "$line" in
      *"$forbidden"*) refuse "the variable ${line%%=*} points into $forbidden" ;;
    esac
  done
done < <(env)

DRY_RUN="${TAKEOVER_EVAL_DRY_RUN:-0}"
RUNS="${TAKEOVER_EVAL_RUNS:-1}"
SCENARIOS="${TAKEOVER_EVAL_SCENARIOS:-A,B,C,D}"
MODEL="${TAKEOVER_EVAL_MODEL:-}"
PY="${TAKEOVER_EVAL_PYTHON:-/usr/local/bin/python3.12}"
TIMEOUT="${TAKEOVER_EVAL_TIMEOUT:-900}"
KEEP="${TAKEOVER_EVAL_KEEP:-0}"

case "$RUNS" in '' | *[!0-9]* | 0) refuse "TAKEOVER_EVAL_RUNS must be a positive whole number" ;; esac
case "$TIMEOUT" in '' | *[!0-9]* | 0) refuse "TAKEOVER_EVAL_TIMEOUT must be a positive whole number" ;; esac
case "$DRY_RUN" in 0 | 1) ;; *) refuse "TAKEOVER_EVAL_DRY_RUN must be 0 or 1" ;; esac
IFS=, read -r -a SCENARIO_LIST <<<"$SCENARIOS"
[ "${#SCENARIO_LIST[@]}" -gt 0 ] || refuse "TAKEOVER_EVAL_SCENARIOS names no scenario"
for scenario in "${SCENARIO_LIST[@]}"; do
  case "$scenario" in A | B | C | D | E | F) ;; *) refuse "unknown scenario in TAKEOVER_EVAL_SCENARIOS (A, B, C, D, E, F)" ;; esac
done

[ -d "${TAKEOVER_EVAL_BIN:-$REPO_TOP/target/debug}" ] || refuse "no binaries directory ${TAKEOVER_EVAL_BIN:-$REPO_TOP/target/debug}"
BIN="$(canonical "${TAKEOVER_EVAL_BIN:-$REPO_TOP/target/debug}")"
forbidden_path "$BIN" && refuse "the binaries under test are inside a real installation ($BIN)"
[ -x "$BIN/memprove" ] && [ -x "$BIN/memproved" ] || refuse "$BIN lacks memprove or memproved"

forbidden_path "$(absolute "$TMP_PREFIX")" && refuse "TAKEOVER_EVAL_TMP_PREFIX is inside a real installation ($TMP_PREFIX)"

OUT=""
if [ -n "${TAKEOVER_EVAL_OUT:-}" ]; then
  OUT="$(absolute "$TAKEOVER_EVAL_OUT")"
  forbidden_path "$OUT" && refuse "the results directory is inside a real installation ($OUT)"
  inside "$OUT" "$REPO_TOP" && refuse "the results directory is inside the repository"
elif [ "$DRY_RUN" = 0 ]; then
  refuse "TAKEOVER_EVAL_OUT is not set"
fi

CLAUDE=""
if [ "$DRY_RUN" = 0 ]; then
  CLAUDE="${TAKEOVER_EVAL_CLAUDE:-$(command -v claude || true)}"
  [ -n "$CLAUDE" ] && [ -x "$CLAUDE" ] || refuse "no claude executable (TAKEOVER_EVAL_CLAUDE)"
  forbidden_path "$(absolute "$CLAUDE")" && refuse "the claude executable is inside a real installation"
fi

# The tools the environments need.
[ -x "$PY" ] || refuse "no Python at $PY (TAKEOVER_EVAL_PYTHON)"
"$PY" -m pytest --version >/dev/null 2>&1 || refuse "$PY has no pytest"
command -v sqlite3 >/dev/null || refuse "sqlite3 is not on PATH"
[ -n "$OUT" ] && mkdir -p "$OUT"

# --- One environment -------------------------------------------------------

ROOT=""
DAEMON_PID=""
CLIENT_PID=""
# U10 T2: every environment built so far and not yet removed.
ROOTS=()

stop_daemon() {
  [ -n "$DAEMON_PID" ] || return 0
  if kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill -TERM "$DAEMON_PID" 2>/dev/null || true
    for _ in $(seq 1 100); do
      kill -0 "$DAEMON_PID" 2>/dev/null || break
      sleep 0.1
    done
    kill -0 "$DAEMON_PID" 2>/dev/null && kill -KILL "$DAEMON_PID" 2>/dev/null || true
  fi
  wait "$DAEMON_PID" 2>/dev/null || true
  DAEMON_PID=""
}

# Removes the environment $1 (unless TAKEOVER_EVAL_KEEP=1).
remove_root() {
  case "$1" in
    "$TMP_PREFIX".*)
      if [ "$KEEP" = 1 ]; then
        say "kept $1"
      else
        rm -rf "$1"
      fi
      ;;
  esac
}

# Stops the client and the daemon of the current environment and removes it.
cleanup() {
  [ -n "$CLIENT_PID" ] && kill -TERM "$CLIENT_PID" 2>/dev/null || true
  CLIENT_PID=""
  stop_daemon
  [ -n "$ROOT" ] && remove_root "$ROOT"
  ROOT=""
}

# On exit: the current environment, then every other one still there.
cleanup_all() {
  local root
  cleanup
  for root in ${ROOTS[@]+"${ROOTS[@]}"}; do
    [ -d "$root" ] && remove_root "$root"
  done
  ROOTS=()
}
trap cleanup_all EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

now_ms() { perl -MTime::HiRes=time -e 'printf "%d\n", time * 1000'; }
utc_now() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# The agent's CLI in the repository, with a temporary HOME and only the
# variables it needs; $CLIENT_SESSION, when set, is its client session.
CLIENT_SESSION=""
agent() {
  (
    cd "$ROOT/repo"
    env -i HOME="$ROOT/home" PATH="$ROOT/bin:$PATH" TMPDIR="$ROOT/tmp" \
      MEMPROVE_CONFIG_DIR="$ROOT/c" GIT_CONFIG_NOSYSTEM=1 \
      ${CLIENT_SESSION:+MEMPROVE_CLIENT_SESSION="$CLIENT_SESSION"} \
      "$ROOT/bin/memprove" "$@"
  )
}

# The operator's CLI: the control token, the data directory by name.
operator() {
  (
    cd "$ROOT/repo"
    env -i HOME="$ROOT/home" PATH="$ROOT/bin:$PATH" TMPDIR="$ROOT/tmp" GIT_CONFIG_NOSYSTEM=1 \
      "$ROOT/bin/memprove" --data-dir "$ROOT/d" --token-file "$ROOT/op-token" "$@"
  )
}

# git in the repository, with fixed identity and dates so that every
# environment of a scenario has the same commits.
GIT_DATE=2026-10-01T08:00:00Z
repo_git() {
  (
    cd "$ROOT/repo"
    env -i HOME="$ROOT/home" PATH="$PATH" GIT_CONFIG_NOSYSTEM=1 \
      GIT_AUTHOR_DATE="$GIT_DATE" GIT_COMMITTER_DATE="$GIT_DATE" \
      git "$@"
  )
}

# A JSON member of the document on standard input: $1 is a Python
# expression over `d`.
json_get() { "$PY" -I -c 'import json, sys; d = json.load(sys.stdin); print(eval(sys.argv[1]))' "$1"; }

start_daemon() {
  env -i HOME="$ROOT/home" PATH="$PATH" TMPDIR="$ROOT/tmp" \
    "$ROOT/bin/memproved" --data-dir "$ROOT/d" </dev/null >>"$ROOT/daemon.log" 2>&1 &
  DAEMON_PID=$!
  for _ in $(seq 1 100); do
    operator health live >/dev/null 2>&1 && return 0
    kill -0 "$DAEMON_PID" 2>/dev/null || break
    sleep 0.1
  done
  die "memproved did not become live (see $ROOT/daemon.log)"
}

write_file() { mkdir -p "$(dirname "$ROOT/repo/$1")" && cat >"$ROOT/repo/$1"; }

commit() {
  GIT_DATE="$1"
  shift
  repo_git add -A
  repo_git commit -q -m "$*"
}

# The repository: a small Python project, three commits, five tests.
make_repo() {
  mkdir -p "$ROOT/repo"
  repo_git init -q -b main
  repo_git config user.name "takeover-eval"
  repo_git config user.email "takeover-eval@example.invalid"
  write_file .gitignore <<'EOF'
__pycache__/
.pytest_cache/
EOF
  write_file README.md <<EOF
# calc

Small arithmetic helpers. Tests: \`$PY -m pytest -q\`.
EOF
  write_file calc.py <<'EOF'
"""Small arithmetic helpers."""


def add(a, b):
    return a + b
EOF
  write_file tests/test_calc.py <<'EOF'
import calc


def test_add():
    assert calc.add(2, 3) == 5


def test_add_negative():
    assert calc.add(-2, -3) == -5
EOF
  commit 2026-10-01T09:00:00Z "calc: add"
  cat >>"$ROOT/repo/calc.py" <<'EOF'


def subtract(a, b):
    return a - b
EOF
  cat >>"$ROOT/repo/tests/test_calc.py" <<'EOF'


def test_subtract():
    assert calc.subtract(5, 3) == 2
EOF
  commit 2026-10-01T10:00:00Z "calc: subtract"
  cat >>"$ROOT/repo/calc.py" <<'EOF'


def multiply(a, b):
    return a * b
EOF
  cat >>"$ROOT/repo/tests/test_calc.py" <<'EOF'


def test_multiply():
    assert calc.multiply(4, 3) == 12


def test_multiply_by_zero():
    assert calc.multiply(4, 0) == 0
EOF
  commit 2026-10-01T11:00:00Z "calc: multiply"
}

# Enabling, as "Enabling memprove in a project" says: bootstrap, daemon,
# Project, the agent's token, the exclude file, `hook install
# --allow-tools`; then the operator's own rules for this project (edits,
# git add and commit) and Claude Code's sandbox allowances for the socket
# and the configuration directory (step 9; the user's settings turn the
# sandbox on).
enable() {
  local created secret
  mkdir -p "$ROOT/bin" "$ROOT/d" "$ROOT/c" "$ROOT/home" "$ROOT/tmp"
  chmod 700 "$ROOT/d" "$ROOT/c"
  cp "$BIN/memprove" "$BIN/memproved" "$ROOT/bin/"
  env -i HOME="$ROOT/home" PATH="$PATH" "$ROOT/bin/memproved" bootstrap \
    --data-dir "$ROOT/d" --label eval-operator --output "$ROOT/op-token" >/dev/null
  start_daemon
  PROJECT_ID="$(operator project register "$ROOT/repo" | json_get 'd["project_id"]')"
  # The secret stays in this shell, as in install.sh.
  created="$(operator token create --label eval-agent --scope mcp-read --scope mcp-write)"
  secret="$(printf '%s' "$created" | sed -n 's/.*"secret":"\([A-Za-z0-9_-]*\)".*/\1/p')"
  unset created
  [ "${#secret}" -eq 43 ] || die "the agent token's secret was not in the response"
  printf '%s\n' "$secret" >"$ROOT/c/token"
  unset secret
  chmod 600 "$ROOT/c/token"
  printf '%s\n' "$ROOT/d" >"$ROOT/c/data-dir"
  mkdir -p "$ROOT/repo/.git/info"
  printf '%s\n' .claude/settings.local.json .mcp.json >>"$ROOT/repo/.git/info/exclude"
  agent hook install --allow-tools 2>"$LOG_DIR/hook-install.err" >/dev/null || die "hook install failed (see $LOG_DIR/hook-install.err)"
  "$PY" -I - "$ROOT/repo/.claude/settings.local.json" "$ROOT/d/memproved.sock" "$ROOT/c" "$ROOT/bin" <<'EOF'
import json, sys
path, socket, config, bin_dir = sys.argv[1:]
with open(path, encoding="utf-8") as f:
    settings = json.load(f)
allow = settings.setdefault("permissions", {}).setdefault("allow", [])
# U10 T3 (Owner O4): the unattended rules of the guide ("What to allow"): the
# server's tools and the whole `memprove` command line, by name and by the
# client's absolute path, in place of the smallest rules `hook install
# --allow-tools` wrote (`memprove run` only).
widened = ["mcp__memprove", "Bash(memprove:*)", "Bash(%s/memprove:*)" % bin_dir]
allow[:] = widened + [rule for rule in allow
                      if rule != "mcp__memprove" and not (rule.startswith("Bash(") and rule.endswith(" run:*)"))]
for rule in ["Edit", "Write", "Bash(git add:*)", "Bash(git commit:*)"]:
    if rule not in allow:
        allow.append(rule)
sandbox = settings.setdefault("sandbox", {})
sandbox.setdefault("network", {}).setdefault("allowUnixSockets", []).append(socket)
sandbox.setdefault("filesystem", {}).setdefault("allowWrite", []).append(config)
with open(path, "w", encoding="utf-8") as f:
    json.dump(settings, f, ensure_ascii=False, indent=2)
    f.write("\n")
EOF
}

readonly TITLE="calc: complete the four operations"
readonly FACT_A="calc.add, calc.subtract, and calc.multiply are implemented, and their tests in tests/test_calc.py all pass"
readonly OPEN_A="calc.divide is not implemented yet: a divisor of 0 must raise ZeroDivisionError"
readonly NEXT_A="implement calc.divide, add two tests to tests/test_calc.py (a plain division and a divisor of 0), commit, run the tests with memprove run, then record a fact that cites that run"

# Scenario D's history: statements long enough that the Pack of more than
# 70 of them does not fit the default budget.
fact_d() { printf 'Review record F%s: review round %s confirmed that calc.add, calc.subtract, and calc.multiply behave as the README specifies, that the result types of integer and float inputs are unchanged, that no exception is swallowed, and that the five tests in tests/test_calc.py cover these paths; this round found nothing in the code to change.' "$1" "$1"; }
open_d() { printf 'To do O%s: the documentation suggestion of review round %s, to add to the README a note on how the calc module handles inputs of class %s, including boundary values, type conversion, and the wording of error messages; it does not change behaviour and waits until calc.divide is done, to be committed with the other documentation suggestions.' "$1" "$1" "$1"; }

# Scenario A, the base of every scenario: one Session of client session
# takeover-eval-seed that ran the tests, recorded a fact, an open item,
# and the next action, and ended. D adds its history before the last
# Checkpoint.
seed() {
  local scenario="$1" round k
  local args
  CLIENT_SESSION=takeover-eval-seed
  # U8 K6 (Owner O7): the seeding Session declares its agent, so that the
  # Pack of a later client shows it.
  agent begin --project "$PROJECT_ID" --new-task "$TITLE" \
    --agent-name takeover-eval-seed --agent-role sub_agent >/dev/null 2>"$LOG_DIR/seed.err" || die "seed: begin failed"
  TASK_ID="$(agent context show | sed -n 's/^Task: *\([0-9a-f-]*\).*/\1/p')"
  [ -n "$TASK_ID" ] || die "no Task in the seed's context"
  agent run --test-format pytest -- "$PY" -m pytest -q >/dev/null 2>>"$LOG_DIR/seed.err" || die "seed: the test run failed"
  if [ "$scenario" = D ]; then
    k=0
    for round in 1 2 3 4 5 6 7; do
      args=()
      for _ in 1 2 3 4 5; do
        k=$((k + 1))
        args+=(--fact "$(fact_d "$k")" --evidence r1 --open "$(open_d "$k")")
      done
      agent checkpoint create "${args[@]}" --next "tidy the review records of round $round" >/dev/null 2>>"$LOG_DIR/seed.err" \
        || die "seed: checkpoint $round failed"
    done
  fi
  agent checkpoint create --fact "$FACT_A" --evidence r1 --open "$OPEN_A" --next "$NEXT_A" >/dev/null 2>>"$LOG_DIR/seed.err" \
    || die "seed: checkpoint failed"
  agent end --reason "add, subtract, and multiply are done" >/dev/null 2>>"$LOG_DIR/seed.err" || die "seed: end failed"
  if [ "$scenario" = B ]; then
    # Another client session begins the Task, runs the tests, and leaves
    # its Session open. U10 T2: its agent is `takeover-eval-left`, not the
    # B client's `takeover-eval-other`: under ADR 0021's rule (a) a client
    # does not end a Session of its own agent name, so a seed of the
    # client's name would make leaving it open the right answer.
    CLIENT_SESSION=takeover-eval-left
    agent begin --project "$PROJECT_ID" --task "$TASK_ID" \
      --agent-name takeover-eval-left --agent-role sub_agent >/dev/null 2>>"$LOG_DIR/seed.err" || die "seed: the other begin failed"
    agent run --test-format pytest -- "$PY" -m pytest -q >/dev/null 2>>"$LOG_DIR/seed.err" || die "seed: the other run failed"
  fi
  if [ "$scenario" = E ]; then
    # U13 E1 (Owner O3): an orchestrator begins the Task, runs the tests, and
    # leaves its Session open, as one that dispatched the client and waits
    # for it would. Its Session ID is the client's parent
    # (MEMPROVE_PARENT_SESSION, `parent_session_id` of seed.json), so under
    # ADR 0021's condition (c) the client must not end it.
    CLIENT_SESSION=takeover-eval-parent
    agent begin --project "$PROJECT_ID" --task "$TASK_ID" \
      --agent-name takeover-eval-parent --agent-role orchestrator >/dev/null 2>>"$LOG_DIR/seed.err" || die "seed: the parent's begin failed"
    agent run --test-format pytest -- "$PY" -m pytest -q >/dev/null 2>>"$LOG_DIR/seed.err" || die "seed: the parent's run failed"
    PARENT_SESSION_ID="$(agent context show | sed -n 's/^Session: *\([0-9a-f-]*\).*/\1/p')"
    [ -n "$PARENT_SESSION_ID" ] || die "no Session in the parent's context"
  fi
  if [ "$scenario" = F ]; then
    # U13 F1 (Owner O4): an orchestrator begins the Task and dispatches a
    # sub_agent (`--parent-session`), which begins it, runs the tests, and is
    # left open; then the orchestrator ends its Session normally. The open
    # Session's dispatcher has ended, so under ADR 0021's condition (b) it
    # looks gone although its last write is recent, and the client must end
    # it without waiting. seed.json names both (`dispatcher_session_id`,
    # `left_session_id`).
    CLIENT_SESSION=takeover-eval-dispatcher
    agent begin --project "$PROJECT_ID" --task "$TASK_ID" \
      --agent-name takeover-eval-dispatcher --agent-role orchestrator >/dev/null 2>>"$LOG_DIR/seed.err" || die "seed: the dispatcher's begin failed"
    DISPATCHER_SESSION_ID="$(agent context show | sed -n 's/^Session: *\([0-9a-f-]*\).*/\1/p')"
    [ -n "$DISPATCHER_SESSION_ID" ] || die "no Session in the dispatcher's context"
    CLIENT_SESSION=takeover-eval-left
    agent begin --project "$PROJECT_ID" --task "$TASK_ID" \
      --agent-name takeover-eval-left --agent-role sub_agent --parent-session "$DISPATCHER_SESSION_ID" \
      >/dev/null 2>>"$LOG_DIR/seed.err" || die "seed: the dispatched begin failed"
    agent run --test-format pytest -- "$PY" -m pytest -q >/dev/null 2>>"$LOG_DIR/seed.err" || die "seed: the dispatched run failed"
    LEFT_SESSION_ID="$(agent context show | sed -n 's/^Session: *\([0-9a-f-]*\).*/\1/p')"
    [ -n "$LEFT_SESSION_ID" ] || die "no Session in the dispatched context"
    CLIENT_SESSION=takeover-eval-dispatcher
    agent end --reason "dispatcher done" >/dev/null 2>>"$LOG_DIR/seed.err" || die "seed: the dispatcher's end failed"
  fi
  CLIENT_SESSION=""
}

# Scenario C: one character of the seeded fact's text changes behind the
# daemon's back. The table and column are `checkpoint_items.text` of the
# Project's state database `<data>/projects/<project>/state.sqlite`
# (crates/memprove-store-sqlite/migrations/state/0003_recovery_kernel.sql,
# CREATE TABLE checkpoint_items; a fact is a row of section
# 'verified_completed_work'); the append-only trigger
# checkpoint_items_reject_update (same file) is dropped first, as the
# store's own tamper tests do (crates/memprove-daemon/tests/p4b_support,
# Setup::tamper). The daemon is stopped while sqlite3 writes.
tamper() {
  local database="$ROOT/d/projects/$PROJECT_ID/state.sqlite" changed
  stop_daemon
  changed="$(sqlite3 "$database" "PRAGMA foreign_keys = OFF; DROP TRIGGER IF EXISTS checkpoint_items_reject_update; UPDATE checkpoint_items SET text = replace(text, 'all pass', 'all passed') WHERE section = 'verified_completed_work' AND text LIKE '%all pass%'; SELECT changes();")"
  [ "$changed" = 1 ] || die "the tampering changed $changed rows, not 1"
  start_daemon
}

# Builds and seeds one environment of $1 into $2 (a run's directory), and
# checks what the scenario needs.
prepare() {
  local scenario="$1" dir="$2" level verify_code path
  LOG_DIR="$dir"
  ROOT="$(mktemp -d "$TMP_PREFIX.XXXXXX")"
  ROOT="$(canonical "$ROOT")"
  case "$ROOT" in "$TMP_PREFIX".*) ;; *) refuse "the temporary directory $ROOT is not under $TMP_PREFIX" ;; esac
  ROOTS+=("$ROOT")
  make_repo
  enable
  for path in "$ROOT/d" "$ROOT/c" "$ROOT/c/token" "$ROOT/op-token" "$ROOT/repo"; do
    case "$path" in "$TMP_PREFIX".*) ;; *) refuse "$path is not under $TMP_PREFIX" ;; esac
  done
  seed "$scenario"
  [ "$scenario" = C ] && tamper
  set +e
  operator ledger verify --project "$PROJECT_ID" >"$dir/ledger-verify-before.json" 2>"$dir/ledger-verify-before.err"
  verify_code=$?
  set -e
  agent resume --task "$TASK_ID" >"$dir/capsule-before.json"
  agent resume --task "$TASK_ID" --text >"$dir/resume-text-before.txt"
  agent resume --task "$TASK_ID" --pack >"$dir/pack-before.txt"
  (cd "$ROOT/repo" && env -i HOME="$ROOT/home" PATH="$ROOT/bin:$PATH" MEMPROVE_CONFIG_DIR="$ROOT/c" \
    "$ROOT/bin/memprove" doctor >"$dir/doctor-before.txt" 2>&1) || true
  cp "$ROOT/repo/.claude/settings.local.json" "$dir/settings.local.json"
  cp "$ROOT/repo/.mcp.json" "$dir/mcp.json"
  case "$scenario" in
    C)
      if [ "$verify_code" = 0 ] && [ "$(json_get 'd["status"]' <"$dir/capsule-before.json")" != tampered ]; then
        die "scenario C: the ledger still verifies and the Capsule is not tampered"
      fi
      ;;
    *)
      [ "$verify_code" = 0 ] || die "scenario $scenario: ledger verify exited $verify_code"
      ;;
  esac
  if [ "$scenario" = D ]; then
    level="$(sed -n 's/^Omitted: level \([0-9]*\);.*/\1/p' "$dir/pack-before.txt")"
    [ -n "$level" ] && [ "$level" -ge 1 ] || die "scenario D: the Pack's omission level is ${level:-missing}, not 1 or more"
  fi
  "$PY" -I - "$dir/seed.json" "$scenario" "$PROJECT_ID" "$TASK_ID" "$verify_code" "$ROOT" \
    "${PARENT_SESSION_ID:-}" "${DISPATCHER_SESSION_ID:-}" "${LEFT_SESSION_ID:-}" <<'EOF'
import json, sys
path, scenario, project, task, verify_code, root, parent, dispatcher, left = sys.argv[1:]
seed = {"scenario": scenario, "project_id": project, "task_id": task,
        "ledger_verify_before_exit": int(verify_code), "root": root}
# U13 E1: E's seed names the Session it left open, the client's parent; the
# seeds of A to D keep their members.
if scenario == "E":
    seed["parent_session_id"] = parent
# U13 F1: F's seed names the dispatcher's Session, which ended, and the
# Session it dispatched, which it left open.
if scenario == "F":
    seed["dispatcher_session_id"] = dispatcher
    seed["left_session_id"] = left
with open(path, "w", encoding="utf-8") as f:
    json.dump(seed, f, ensure_ascii=False, indent=2)
    f.write("\n")
EOF
}

# What the run left: the ledger, the Capsule, the Events, the hook's text.
collect() {
  local dir="$1" code cursor page
  set +e
  operator ledger verify --project "$PROJECT_ID" >"$dir/ledger-verify-after.json" 2>"$dir/ledger-verify-after.err"
  code=$?
  set -e
  printf '%s\n' "$code" >"$dir/ledger-verify-after.exit"
  agent resume --task "$TASK_ID" >"$dir/capsule-after.json" 2>"$dir/capsule-after.err" || true
  agent resume --task "$TASK_ID" --text >"$dir/resume-text-after.txt" 2>>"$dir/capsule-after.err" || true
  # Every Event of the Project, page by page.
  : >"$dir/events-after.jsonl"
  cursor=""
  while :; do
    if [ -n "$cursor" ]; then
      page="$(operator event list --project "$PROJECT_ID" --limit 500 --cursor "$cursor")" || break
    else
      page="$(operator event list --project "$PROJECT_ID" --limit 500)" || break
    fi
    printf '%s\n' "$page" >>"$dir/events-after.jsonl"
    cursor="$(printf '%s' "$page" | json_get 'd.get("next_cursor") or ""')"
    [ -n "$cursor" ] || break
  done
  "$PY" -I - "$dir/transcript.jsonl" "$dir/hook.txt" <<'EOF'
import json, sys
transcript, hook = sys.argv[1:]
text = ""
with open(transcript, encoding="utf-8") as f:
    for line in f:
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if event.get("type") == "system" and event.get("subtype") == "hook_response":
            output = event.get("stdout") or event.get("output") or ""
            if output.startswith("memprove："):
                text = output
with open(hook, "w", encoding="utf-8") as f:
    f.write(text)
EOF
}

# U10 T2 (Owner O4): the seconds a B client waits for, at least: the
# Session the seed left open must not have written for this long (ADR 0021's
# 15 minutes, plus one).
readonly SEED_AGE=960

# The seconds since the latest write of a Session that was open in the
# Capsule before the run (`last_recorded_at`, else `started_at`), by this
# machine's clock; the ledger is read as it is.
seed_age() {
  "$PY" -I - "$1/capsule-before.json" <<'EOF'
import datetime, json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    capsule = json.load(f)
times = [s.get("last_recorded_at") or s["started_at"]
         for s in capsule["sessions"] if s["end_state"] == "open"]
if not times:
    sys.exit("no Session was open before the run")
latest = max(datetime.datetime.fromisoformat(t.replace("Z", "+00:00")) for t in times)
print(int((datetime.datetime.now(datetime.timezone.utc) - latest).total_seconds()))
EOF
}

# U10 T3: whether the client knows --setting-sources (its --help lists it).
SETTING_SOURCES=""
setting_sources() {
  if [ -z "$SETTING_SOURCES" ]; then
    if "$CLAUDE" --help 2>/dev/null | grep -q -- '--setting-sources'; then
      SETTING_SOURCES="project,local"
    else
      SETTING_SOURCES="-"
    fi
  fi
}

# One client run in the environment: the prompt, a clean environment, a
# time limit. The limit is an alarm that survives the exec (SIGALRM ends
# the client, exit 142), so no watchdog process outlives the run.
client() {
  local dir="$1" scenario="$2" run="$3" started finished code age="" waited=""
  # U9 K6 (Owner O7): in B the client declares an agent, so that the end of
  # the Session another client left open (`memprove end --abandoned`, in the
  # client's shell, or through the MCP tool) records who ended it. The
  # variables are the client's own: a server it starts reads them only if the
  # client passes its environment on, which the evaluation records
  # (`old_session_ended_by`) instead of assuming.
  local agent_env=() parent=""
  # U13 F1 (Owner O4): F's client is B's agent, with no parent.
  case "$scenario" in
    B | F) agent_env=(MEMPROVE_AGENT_NAME=takeover-eval-other MEMPROVE_AGENT_ROLE=sub_agent) ;;
  esac
  # U13 E1 (Owner O3): E's client is the same agent, dispatched by the
  # Session E's seed left open: its parent, which the hook's opening names.
  if [ "$scenario" = E ]; then
    parent="$(json_get 'd.get("parent_session_id") or ""' <"$dir/seed.json")"
    [ -n "$parent" ] || die "scenario E: the seed names no parent Session"
    agent_env=(MEMPROVE_AGENT_NAME=takeover-eval-other MEMPROVE_AGENT_ROLE=sub_agent
      MEMPROVE_PARENT_SESSION="$parent")
  fi
  # U10 T2: B's client waits until the Session the seed left open is old.
  # U13 E1: so does E's, so that of ADR 0021's three conditions only (c) fails.
  if [ "$scenario" = B ] || [ "$scenario" = E ]; then
    age="$(seed_age "$dir")" || die "scenario $scenario: cannot read the open Session's last write"
    waited=0
    if [ "$age" -lt "$SEED_AGE" ]; then
      waited=$((SEED_AGE - age))
      say "scenario $scenario run $run: the open Session last wrote ${age}s ago; waiting ${waited}s"
      sleep "$waited"
    fi
    age="$(seed_age "$dir")"
    [ "$age" -ge "$SEED_AGE" ] || die "scenario $scenario: the open Session is only ${age}s old"
  fi
  # U13 F1 (Owner O4): F's client starts at once; run.json records how old the
  # open Session is (`seed_age_seconds`) and no wait (`seed_wait_seconds`
  # null).
  if [ "$scenario" = F ]; then
    age="$(seed_age "$dir")" || die "scenario F: cannot read the open Session's last write"
  fi
  # U10 T3: only the project's memprove server, only the project's settings.
  setting_sources
  local sources=()
  [ "$SETTING_SOURCES" != "-" ] && sources=(--setting-sources "$SETTING_SOURCES")
  printf '%s scenario=%s run=%s\n' "$(utc_now)" "$scenario" "$run" >>"$OUT/claude-invocations.log"
  started="$(now_ms)"
  (
    cd "$ROOT/repo"
    exec perl -e 'alarm shift; exec @ARGV or die "exec: $!"' "$TIMEOUT" \
      env -i HOME="$HOME" USER="${USER:-}" LOGNAME="${LOGNAME:-}" SHELL="${SHELL:-/bin/zsh}" \
      TMPDIR="${TMPDIR:-/tmp}" __CF_USER_TEXT_ENCODING="${__CF_USER_TEXT_ENCODING:-}" \
      ${agent_env[@]+"${agent_env[@]}"} \
      PATH="$ROOT/bin:$PATH" \
      "$CLAUDE" -p "$PROMPT" --output-format stream-json --verbose \
      --mcp-config "$ROOT/repo/.mcp.json" --strict-mcp-config ${sources[@]+"${sources[@]}"} \
      ${MODEL:+--model "$MODEL"}
  ) </dev/null >"$dir/transcript.jsonl" 2>"$dir/claude-stderr.txt" &
  CLIENT_PID=$!
  set +e
  wait "$CLIENT_PID"
  code=$?
  set -e
  CLIENT_PID=""
  finished="$(now_ms)"
  "$PY" -I - "$dir/run.json" "$scenario" "$run" "${MODEL:-}" "$code" "$((finished - started))" \
    "$("$CLAUDE" --version 2>/dev/null | head -n 1)" "$("$ROOT/bin/memprove" --version)" \
    "$("$ROOT/bin/memproved" --version)" "$started" "$waited" "$age" "$SETTING_SOURCES" "$parent" <<'EOF'
import json, sys, datetime
(path, scenario, run, model, code, duration, claude, cli, daemon, started,
 waited, age, sources, parent) = sys.argv[1:]
with open(path, "w", encoding="utf-8") as f:
    json.dump({"scenario": scenario, "run": int(run),
               "model_requested": model or None, "exit_code": int(code),
               "duration_ms": int(duration),
               "started_at": datetime.datetime.fromtimestamp(int(started) / 1000, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
               "claude_version": claude, "memprove_version": cli,
               "memproved_version": daemon,
               # U10 T2: B's wait and the open Session's age at the start
               # (U13 E1: E's too; U13 F1: F's age, and no wait).
               "seed_wait_seconds": int(waited) if waited else None,
               "seed_age_seconds": int(age) if age else None,
               # U13 E1: the client's parent in E, null elsewhere.
               "parent_session_id": parent or None,
               # U10 T3: the client loaded only the project's server and,
               # when it knows the option, only these setting sources.
               "strict_mcp_config": True,
               "setting_sources": None if sources == "-" else sources},
              f, ensure_ascii=False, indent=2)
    f.write("\n")
EOF
}

# --- Main ------------------------------------------------------------------

say "binaries: $BIN ($("$BIN/memprove" --version))"
if [ -n "$OUT" ]; then
  printf '%s\n' "$PROMPT" >"$OUT/prompt.txt"
fi
# A dry run builds, seeds, and checks each scenario once, and waits for
# nothing.
if [ "$DRY_RUN" = 1 ]; then
  for scenario in "${SCENARIO_LIST[@]}"; do
    if [ -n "$OUT" ]; then
      dir="$OUT/dry-$scenario"
      [ -e "$dir" ] && refuse "$dir already exists"
      mkdir -p "$dir"
    else
      dir="$(mktemp -d "$TMP_PREFIX-dry.XXXXXX")"
    fi
    prepare "$scenario" "$dir"
    say "dry run: scenario $scenario prepared and checked"
    cleanup
    [ -z "$OUT" ] && rm -rf "$dir"
  done
  say "done"
  exit 0
fi

# U10 T2 (Owner O4): every environment first, seeded and checked, its daemon
# stopped until its client runs.
PLAN=()
for scenario in "${SCENARIO_LIST[@]}"; do
  for run in $(seq 1 "$RUNS"); do
    [ -e "$OUT/$scenario-$run" ] && refuse "$OUT/$scenario-$run already exists; choose another TAKEOVER_EVAL_OUT"
    PLAN+=("$scenario $run")
  done
done
for entry in "${PLAN[@]}"; do
  scenario="${entry% *}"
  run="${entry#* }"
  dir="$OUT/$scenario-$run"
  mkdir -p "$dir"
  prepare "$scenario" "$dir"
  stop_daemon
  ROOT=""
  say "scenario $scenario run $run: seeded"
done

# Then the clients: A, C, D, and (U13 F1) F first, B and (U13 E1) E last
# (each of their clients waits until the Session the seed left open is
# SEED_AGE seconds old; F's does not wait).
for pass in first last; do
  for entry in "${PLAN[@]}"; do
    scenario="${entry% *}"
    run="${entry#* }"
    case "$pass:$scenario" in first:B | first:E | last:A | last:C | last:D | last:F) continue ;; esac
    dir="$OUT/$scenario-$run"
    ROOT="$(json_get 'd["root"]' <"$dir/seed.json")"
    case "$ROOT" in "$TMP_PREFIX".*) ;; *) refuse "the seed of $dir names a root outside $TMP_PREFIX" ;; esac
    PROJECT_ID="$(json_get 'd["project_id"]' <"$dir/seed.json")"
    TASK_ID="$(json_get 'd["task_id"]' <"$dir/seed.json")"
    LOG_DIR="$dir"
    start_daemon
    say "scenario $scenario run $run: client starts"
    client "$dir" "$scenario" "$run"
    collect "$dir"
    say "scenario $scenario run $run: done (exit $(json_get 'd["exit_code"]' <"$dir/run.json"))"
    cleanup
  done
done
say "done"
