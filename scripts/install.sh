#!/bin/bash
# memprove installer (U1 E2, ADR 0015), for macOS and Linux.
#
#   scripts/install.sh [--prefix <DIR>] [--from-binaries <DIR>] [--write-profile | --no-write-profile]
#   scripts/install.sh [--prefix <DIR>] --print-launchd
#
# A fresh install creates <prefix>/bin, data, and config (mode 0700),
# builds the two binaries offline (or takes them from --from-binaries),
# issues the operator's control token with `memproved bootstrap`, starts
# the daemon, issues the agent's mcp-read + mcp-write token, writes
# bin/ct and bin/ensure-daemon.sh, and prints how to enable a project.
# Run again on an existing install, it upgrades: it stops the daemon and
# waits for it to exit, backs up data/ with `memprove backup`, takes
# group's and others' access away from data/projects/, the directories in
# it, and data/tmp/ (made before U2 S2 under a looser umask), keeps the
# old binaries as <name>.<old build>, installs the new ones, starts the
# daemon, and runs `ledger verify` for every Project the registry lists (a
# directory in data/projects it does not list only gets a warning); a
# daemon that was not running before the upgrade is stopped again after
# that. A step that fails
# stops the upgrade and puts the old version back, running or stopped as
# it was. No step uses the network; no token is ever printed.
#
# U17 (M1 O4): the binaries of the default prefix (~/.memprove) need no
# environment variable: `memprove` finds ~/.memprove/config by its
# place. What the shell still needs is <prefix>/bin on PATH, and for another
# prefix the three MEMPROVE_* variables; --write-profile writes those
# lines into the shell's profile between marker comments (replaced on a
# second run), --no-write-profile leaves the profile alone, and with
# neither an interactive run asks once. The second step after this script
# is `memprove init` in the repository.
set -euo pipefail
umask 077

readonly LAUNCHD_LABEL="local.memprove.memproved"
readonly MAX_SOCKET_PATH_BYTES=103

say() { printf '%s\n' "$*"; }
warn() { printf 'install.sh: %s\n' "$*" >&2; }
die() {
  warn "error: $*"
  exit 1
}

usage() {
  cat <<'USAGE'
Usage: scripts/install.sh [--prefix <DIR>] [--from-binaries <DIR>] [--write-profile | --no-write-profile]
       scripts/install.sh [--prefix <DIR>] --print-launchd

Installs memprove under <DIR> (default: ~/.memprove), or upgrades the
installation already there.

Options:
  --prefix <DIR>         installation directory (default: $HOME/.memprove)
  --from-binaries <DIR>  install <DIR>/memprove and <DIR>/memproved
                         instead of building them (they must be one build)
  --write-profile        write the PATH line (and, for a prefix other than
                         $HOME/.memprove, the MEMPROVE_* variables) into
                         the shell profile ($SHELL: ~/.zshrc, ~/.bash_profile
                         or ~/.bashrc, ~/.config/fish/config.fish), between
                         marker comments; without it an interactive run asks
  --no-write-profile     change no profile and ask nothing
  --print-launchd        print a launchd property list that runs the daemon
                         of <DIR>, and change nothing
  -h, --help             print this help

The build runs `cargo build --offline --release -p memprove-cli
-p memprove-daemon` in this repository. On macOS, LIBCLANG_PATH and
DYLD_FALLBACK_LIBRARY_PATH default to /opt/homebrew/opt/llvm/lib; set them
to override. Exit status: 0 success, 1 a step failed (an upgrade put the old
version back), 2 usage error.
USAGE
}

prefix=""
from_binaries=""
print_launchd=0
write_profile=""
while [ $# -gt 0 ]; do
  case "$1" in
    --write-profile) write_profile=yes; shift ;;
    --no-write-profile) write_profile=no; shift ;;
    --prefix) [ $# -ge 2 ] || { warn "--prefix requires a value"; exit 2; }; prefix="$2"; shift 2 ;;
    --prefix=*) prefix="${1#--prefix=}"; shift ;;
    --from-binaries) [ $# -ge 2 ] || { warn "--from-binaries requires a value"; exit 2; }; from_binaries="$2"; shift 2 ;;
    --from-binaries=*) from_binaries="${1#--from-binaries=}"; shift ;;
    --print-launchd) print_launchd=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) warn "unknown argument: $1"; usage >&2; exit 2 ;;
  esac
done

default_prefix=0
if [ -z "$prefix" ]; then
  [ -n "${HOME:-}" ] || { warn "HOME is not set; pass --prefix <DIR>"; exit 2; }
  prefix="$HOME/.memprove"
  default_prefix=1
elif [ -n "${HOME:-}" ] && [ "${prefix%/}" = "$HOME/.memprove" ]; then
  default_prefix=1
fi
case "$prefix" in
  /*) ;;
  *) prefix="$(pwd -P)/$prefix" ;;
esac
prefix="${prefix%/}"
# The prefix goes into generated scripts, JSON, and XML unescaped.
if ! printf '%s' "$prefix" | LC_ALL=C grep -Eq '^[A-Za-z0-9 ._/@+:,=-]+$'; then
  warn "the prefix may hold only letters, digits, spaces, and . _ / @ + : , = -: $prefix"
  exit 2
fi
socket_bytes=$(printf '%s' "$prefix/data/memproved.sock" | wc -c | tr -d ' ')
if [ "$socket_bytes" -gt "$MAX_SOCKET_PATH_BYTES" ]; then
  warn "the socket path $prefix/data/memproved.sock would be $socket_bytes bytes, above the Unix socket limit of $MAX_SOCKET_PATH_BYTES; choose a shorter prefix"
  exit 2
fi

readonly bin="$prefix/bin" data="$prefix/data" config="$prefix/config"
readonly cli="$bin/memprove" daemon="$bin/memproved"
readonly operator_token="$prefix/operator-token" agent_token="$config/token"
readonly guide="$prefix/share/guide.md"

if [ "$print_launchd" = 1 ]; then
  cat <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$LAUNCHD_LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$daemon</string>
    <string>--data-dir</string>
    <string>$data</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>Umask</key>
  <integer>63</integer>
  <key>StandardOutPath</key>
  <string>$prefix/daemon.log</string>
  <key>StandardErrorPath</key>
  <string>$prefix/daemon.log</string>
</dict>
</plist>
PLIST
  warn "printed only; nothing was installed. Save it as ~/Library/LaunchAgents/$LAUNCHD_LABEL.plist and load it with: launchctl bootstrap gui/\$(id -u) ~/Library/LaunchAgents/$LAUNCHD_LABEL.plist"
  warn "under launchd, stop it with launchctl bootout before an upgrade; memproved exits 0 on SIGTERM, so KeepAlive does not restart it"
  exit 0
fi

script_dir="$(cd "$(dirname "$0")" && pwd -P)"
source_dir="$(dirname "$script_dir")"

# The build identity inside the parentheses of a --version line.
build_of() {
  local line
  line="$("$1" --version 2>/dev/null)" || return 1
  case "$line" in
    *"("*")") line="${line##*(}"; printf '%s\n' "${line%)}" ;;
    *) printf 'unknown\n' ;;
  esac
}

# ---- the new binaries -----------------------------------------------------

if [ -n "$from_binaries" ]; then
  binaries="$(cd "$from_binaries" 2>/dev/null && pwd -P)" || die "--from-binaries $from_binaries is not a directory"
else
  command -v cargo >/dev/null 2>&1 || die "cargo is not on PATH; install Rust, or pass --from-binaries <DIR>"
  if [ "$(uname -s)" = Darwin ]; then
    export LIBCLANG_PATH="${LIBCLANG_PATH:-/opt/homebrew/opt/llvm/lib}"
    export DYLD_FALLBACK_LIBRARY_PATH="${DYLD_FALLBACK_LIBRARY_PATH:-/opt/homebrew/opt/llvm/lib}"
  fi
  say "Building the release binaries (offline) in $source_dir ..."
  (cd "$source_dir" && cargo build --offline --release -p memprove-cli -p memprove-daemon) \
    || die "the release build failed; nothing was installed or changed"
  target_dir="${CARGO_TARGET_DIR:-target}"
  case "$target_dir" in
    /*) ;;
    *) target_dir="$source_dir/$target_dir" ;;
  esac
  binaries="$target_dir/release"
fi
for name in memprove memproved; do
  [ -f "$binaries/$name" ] && [ -x "$binaries/$name" ] || die "$binaries/$name is not an executable file"
done
new_build="$(build_of "$binaries/memprove")" || die "$binaries/memprove --version failed"
new_daemon_build="$(build_of "$binaries/memproved")" || die "$binaries/memproved --version failed"
[ "$new_build" = "$new_daemon_build" ] \
  || die "$binaries/memprove is build $new_build but memproved is build $new_daemon_build; install one build"

# ---- shared steps ---------------------------------------------------------

write_ct() {
  cat >"$bin/ct" <<CT
#!/bin/bash
# Generated by scripts/install.sh: memprove with this installation's data
# directory, configuration directory, and agent token (mcp-read + mcp-write).
PREFIX='$prefix'
export MEMPROVE_DATA_DIR="\$PREFIX/data"
export MEMPROVE_CONFIG_DIR="\$PREFIX/config"
export MEMPROVE_TOKEN_FILE="\$PREFIX/config/token"
# MEMPROVE_TOKEN would take precedence over the token file; drop it.
unset MEMPROVE_TOKEN
exec "\$PREFIX/bin/memprove" "\$@"
CT
  chmod 700 "$bin/ct"
}

write_ensure_daemon() {
  cat >"$bin/ensure-daemon.sh" <<'ENSURE_HEAD'
#!/bin/bash
# Generated by scripts/install.sh: start this installation's memproved in
# the background, detached from this session, unless it already answers.
# Its output is appended to <prefix>/daemon.log.
set -u
ENSURE_HEAD
  cat >>"$bin/ensure-daemon.sh" <<ENSURE_PREFIX
PREFIX='$prefix'
ENSURE_PREFIX
  cat >>"$bin/ensure-daemon.sh" <<'ENSURE_BODY'
DATA="$PREFIX/data"
LOG="$PREFIX/daemon.log"
umask 077
alive() { "$PREFIX/bin/memprove" --data-dir "$DATA" health live >/dev/null 2>&1; }
alive && exit 0
if command -v perl >/dev/null 2>&1; then
  # setsid() + exec: a session of its own, no controlling terminal.
  nohup perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$PREFIX/bin/memproved" --data-dir "$DATA" </dev/null >>"$LOG" 2>&1 &
elif command -v setsid >/dev/null 2>&1; then
  nohup setsid "$PREFIX/bin/memproved" --data-dir "$DATA" </dev/null >>"$LOG" 2>&1 &
else
  nohup "$PREFIX/bin/memproved" --data-dir "$DATA" </dev/null >>"$LOG" 2>&1 &
fi
for _ in $(seq 1 100); do
  alive && exit 0
  sleep 0.1
done
echo "ensure-daemon: memproved did not become live; see $LOG" >&2
exit 1
ENSURE_BODY
  chmod 700 "$bin/ensure-daemon.sh"
}

alive() { "$1" --data-dir "$data" health live >/dev/null 2>&1; }

# The pid memproved recorded in its lock file, when it is a running
# memproved.
daemon_pid() {
  local pid
  pid="$(head -n 1 "$data/memproved.lock" 2>/dev/null | tr -d ' \n')" || return 1
  case "$pid" in '' | *[!0-9]*) return 1 ;; esac
  kill -0 "$pid" 2>/dev/null || return 1
  if command -v ps >/dev/null 2>&1; then
    case "$(ps -p "$pid" -o comm= 2>/dev/null)" in
      *memproved*) ;;
      *) return 1 ;;
    esac
  fi
  printf '%s\n' "$pid"
}

# U19 (the rename to memprove): a daemon of the previous name, continuityd,
# may still hold data/, with its own lock and socket names; the new CLI does
# not see it. Stop it the same way first, so that one daemon runs per data
# directory. U20 (N2): it sets legacy_was_running, so that the upgrade
# counts that daemon as running and leaves memproved running afterwards.
stop_legacy_daemon() {
  local pid
  pid="$(head -n 1 "$data/continuityd.lock" 2>/dev/null | tr -d ' \n')" || return 0
  case "$pid" in '' | *[!0-9]*) return 0 ;; esac
  kill -0 "$pid" 2>/dev/null || return 0
  if command -v ps >/dev/null 2>&1; then
    case "$(ps -p "$pid" -o comm= 2>/dev/null)" in
      *continuityd*) ;;
      *) return 0 ;;
    esac
  fi
  say "stopping the continuityd (pid $pid) of the previous name"
  legacy_was_running=1
  kill -TERM "$pid" || return 1
  for _ in $(seq 1 150); do
    if ! kill -0 "$pid" 2>/dev/null && [ ! -e "$data/continuityd.sock" ]; then
      return 0
    fi
    sleep 0.1
  done
  warn "continuityd (pid $pid) did not exit within 15 seconds"
  return 1
}

# Stops the daemon of data/ with SIGTERM and waits until the process has
# exited and its socket is gone. $1 is a CLI to ask with.
stop_daemon() {
  local pid
  stop_legacy_daemon || return 1
  alive "$1" || return 0
  pid="$(daemon_pid)" || { warn "memproved answers, but its lock file names no running memproved"; return 1; }
  kill -TERM "$pid" || return 1
  for _ in $(seq 1 150); do
    if ! kill -0 "$pid" 2>/dev/null && [ ! -e "$data/memproved.sock" ]; then
      return 0
    fi
    sleep 0.1
  done
  warn "memproved (pid $pid) did not exit within 15 seconds"
  return 1
}

start_daemon() { "$bin/ensure-daemon.sh"; }

# U2 (W-F, SF-1; ADR 0015, "Amendment (U2, W-F)"): memproved creates
# data/projects/, data/projects/<ID>/ and data/tmp/ with mode 0700 since
# U2 S2, but one made before it under umask 022 is 0755, and doctor's
# projects-dir check fails for it. Takes group's and others' bits away
# from such a directory (chmod go-rwx: nothing stricter is loosened, the
# owner's and the special bits stay); a symbolic link is skipped, not
# followed, and no file is touched. Prints one line per directory it
# changed; returns 1 when one could not be changed.
tighten_one() {
  local dir="$1" was now
  [ -d "$dir" ] && [ ! -L "$dir" ] || return 0
  was="$(ls -ld "$dir" | cut -c1-10)" || return 1
  case "${was:4:6}" in
    *[rwxst]*) ;;
    *) return 0 ;;
  esac
  chmod go-rwx "$dir" || return 1
  now="$(ls -ld "$dir" | cut -c1-10)" || return 1
  say "    tightened ${dir#"$prefix"/}: $was -> $now"
}

tighten_or_warn() {
  tighten_one "$1" \
    || warn "could not take group's and others' access away from $1; chmod 700 it while memproved is stopped"
}

# data/projects/, each directory in it (one per Project; find -type d does
# not follow a link), and data/tmp/, in that order.
tighten_data_dirs() {
  local dir
  tighten_or_warn "$data/projects"
  if [ -d "$data/projects" ] && [ ! -L "$data/projects" ]; then
    while IFS= read -r -d '' dir; do
      tighten_or_warn "$dir"
    done < <(find "$data/projects" -mindepth 1 -maxdepth 1 -type d -print0 | LC_ALL=C sort -z)
  fi
  tighten_or_warn "$data/tmp"
}

# U7 K4 (D2 F20): the agent's guide, docs/guide.md of this repository, at
# <prefix>/share/guide.md, so that an agent that only has the
# installation can read how to use it. Written whole under a staging name and
# moved into place; a fresh install and every upgrade bring it to the version of
# the binaries. A guide that cannot be installed is a warning, not a failure:
# the binaries do not need it.
#
# U10 N9 (D3 F28): its first line is `<!-- installed with build <build> -->`,
# the build of the binaries installed with it (`new_build`, what `--version`
# prints in parentheses), so that `doctor` can say whether the guide and the
# binary are the same version; the repository's own guide has no such line.
install_guide() {
  local source="$source_dir/docs/guide.md"
  if [ ! -f "$source" ]; then
    warn "$source does not exist; the guide was not installed at $guide"
    return 0
  fi
  if mkdir -p "$prefix/share" && chmod 700 "$prefix/share" \
    && printf '<!-- installed with build %s -->\n' "$new_build" >"$guide.new" \
    && cat "$source" >>"$guide.new" \
    && chmod 600 "$guide.new" && mv "$guide.new" "$guide"; then
    return 0
  fi
  rm -f "$guide.new"
  warn "the guide could not be installed at $guide"
}

# U17 (M1 O4): the lines the shell needs: PATH always; the three variables
# only for a prefix other than the default, whose config/ the binaries find
# by its place.
profile_lines() {
  local shell_name="$1"
  if [ "$shell_name" = fish ]; then
    printf 'set -gx PATH %s $PATH\n' "$bin"
    if [ "$default_prefix" = 0 ]; then
      printf 'set -gx MEMPROVE_CONFIG_DIR %s\n' "$config"
      printf 'set -gx MEMPROVE_DATA_DIR %s\n' "$data"
      printf 'set -gx MEMPROVE_TOKEN_FILE %s\n' "$agent_token"
    fi
  else
    printf 'export PATH="%s:$PATH"\n' "$bin"
    if [ "$default_prefix" = 0 ]; then
      printf 'export MEMPROVE_CONFIG_DIR="%s"\n' "$config"
      printf 'export MEMPROVE_DATA_DIR="%s"\n' "$data"
      printf 'export MEMPROVE_TOKEN_FILE="%s"\n' "$agent_token"
    fi
  fi
}

# The profile file of the login shell ($SHELL): zsh, bash (macOS reads
# ~/.bash_profile for a login shell, Linux ~/.bashrc), or fish; empty for
# another shell.
profile_file() {
  case "$(basename "${SHELL:-}")" in
    zsh) printf '%s\n' "$HOME/.zshrc" ;;
    bash)
      if [ "$(uname -s)" = Darwin ]; then printf '%s\n' "$HOME/.bash_profile"; else printf '%s\n' "$HOME/.bashrc"; fi ;;
    fish) printf '%s\n' "$HOME/.config/fish/config.fish" ;;
    *) printf '\n' ;;
  esac
}

readonly PROFILE_BEGIN="# >>> memprove (scripts/install.sh) >>>"
readonly PROFILE_END="# <<< memprove <<<"

# Writes the profile block, replacing an earlier one between the markers;
# every other line of the profile stays. Prints what it did.
write_profile_block() {
  local file="$1" shell_name tmp
  shell_name="$(basename "${SHELL:-}")"
  mkdir -p "$(dirname "$file")"
  tmp="$file.memprove.new"
  if [ -f "$file" ]; then
    awk -v begin="$PROFILE_BEGIN" -v end="$PROFILE_END" '
      $0 == begin { skipping = 1; next }
      $0 == end { skipping = 0; next }
      !skipping { print }
    ' "$file" >"$tmp"
  else
    : >"$tmp"
  fi
  if [ -s "$tmp" ] && [ -n "$(tail -c 1 "$tmp")" ]; then printf '\n' >>"$tmp"; fi
  {
    printf '%s\n' "$PROFILE_BEGIN"
    profile_lines "$shell_name"
    printf '%s\n' "$PROFILE_END"
  } >>"$tmp"
  mv "$tmp" "$file"
  say "Wrote the memprove lines to $file (between the markers; a new shell reads them)."
}

# U17 (M1 O4): writes the profile when asked (--write-profile), or when an
# interactive run says yes; says what to put there otherwise.
maybe_write_profile() {
  local file answer
  file="$(profile_file)"
  if [ -z "$file" ]; then
    [ "$write_profile" = yes ] && warn "the login shell $(basename "${SHELL:-unknown}") is not zsh, bash, or fish; put these lines in its profile yourself:"
    profile_lines sh | sed 's/^/     /'
    return 0
  fi
  case "$write_profile" in
    yes) write_profile_block "$file" ;;
    no) say "The profile was left alone (--no-write-profile); the lines for $file:"; profile_lines "$(basename "${SHELL:-}")" | sed 's/^/     /' ;;
    *)
      if [ -t 0 ] && [ -t 1 ]; then
        say "The shell needs these lines in $file:"
        profile_lines "$(basename "${SHELL:-}")" | sed 's/^/     /'
        printf 'Write them there now, between marker comments? [y/N] '
        read -r answer || answer=""
        case "$answer" in
          y|Y|yes|YES) write_profile_block "$file" ;;
          *) say "The profile was left alone; run again with --write-profile to write it." ;;
        esac
      else
        say "The profile was left alone (not interactive); the lines for $file, which --write-profile writes:"
        profile_lines "$(basename "${SHELL:-}")" | sed 's/^/     /'
      fi
      ;;
  esac
}

print_next_steps() {
  local fragment variables
  if [ "$default_prefix" = 1 ]; then
    variables="   (the default prefix: the binaries find $config by its place, so no
   MEMPROVE_* variable is needed)
"
  else
    variables="     export MEMPROVE_CONFIG_DIR=\"$config\"
     export MEMPROVE_DATA_DIR=\"$data\"
     export MEMPROVE_TOKEN_FILE=\"$agent_token\"
"
  fi
  # U5 P3 (Owner O5): the enabling fragment, with this installation's
  # configuration directory in its env; U5 Z-F round 1 (S1): two parts, one
  # for .claude/settings.local.json and one for .mcp.json, each under its
  # "==> <file> <==" line.
  fragment="$(cd "$prefix" && MEMPROVE_CONFIG_DIR="$config" "$cli" mcp print-config 2>/dev/null)" \
    || fragment="(memprove mcp print-config failed)"
  cat <<NEXT

memprove $new_build is installed in $prefix, and memproved is running.

  $bin/memprove, memproved   the binaries ($bin/ct runs memprove as the agent)
  $guide
                                 the agent's guide: the commands, their exit codes, taking over
  $data                           the daemon's data directory
  $config                         contexts, the agent's token (token), data-dir
  $operator_token                 the operator's control token; never give it to an agent

Next steps:

1. Put $bin on PATH (--write-profile writes the line; without the PATH line
   a \`memprove run\` the agent types is not found, and the rule
   Bash(memprove run:*) does not match the absolute path it runs instead;
   \`memprove doctor\` checks it on its \`path\` line):
     export PATH="$bin:\$PATH"
$variables
2. In each project (a Git worktree with a commit), as the operator:
     cd /path/to/project && memprove init
   It runs project register with the operator's token, keeps memprove's
   files out of Evidence (.git/info/exclude), writes the configuration of the
   clients it detects (Claude Code: the SessionStart hook, .mcp.json, and the
   permissions.allow rules, as \`memprove hook install --allow-tools\` does;
   Codex: ~/.codex/config.toml and a note in AGENTS.md; Cursor: .cursor/mcp.json
   and a rule), and runs \`memprove doctor\`. --client <name> picks the
   clients, --dry-run only describes.

Then the first Session, in the project, on the Task you are really going to
do (no separate "set up" Task):
     memprove begin --new-task "<TITLE>"
     # ... edit, commit, memprove run, memprove checkpoint create ...
     memprove end --reason "<what was done>"

Also: the daemon after a reboot (or let launchd do it: --print-launchd):
   $bin/ensure-daemon.sh
   The parts \`memprove init\` (or \`ct hook install --allow-tools\`) writes for
   Claude Code, to put in place by hand if you prefer (each under the "==>"
   line that names its file):
$fragment
   Back up data/ while the daemon is stopped: memprove backup --out <DIR>
   Upgrade: run this script again; it backs up data/ first.
NEXT
}

# ---- upgrade --------------------------------------------------------------

upgrade() {
  local old_build stamp backup_dir old_cli old_daemon was_running=0 token project failed=""
  old_build="$(build_of "$daemon")" || old_build="unknown"
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  old_cli="$cli.$old_build"
  old_daemon="$daemon.$old_build"
  if [ -e "$old_cli" ] || [ -e "$old_daemon" ]; then
    old_cli="$old_cli.$stamp"
    old_daemon="$old_daemon.$stamp"
  fi
  backup_dir="$prefix/backups/$stamp-$old_build"
  # U22: two upgrades within one second (a fast machine, a script that
  # upgrades twice) would otherwise share the names above; the later one
  # takes the first free -2, -3, ... instead.
  local free=2 base_backup="$backup_dir" base_cli="$old_cli" base_daemon="$old_daemon"
  while [ -e "$backup_dir" ]; do
    backup_dir="$base_backup-$free"
    free=$((free + 1))
  done
  free=2
  while [ -e "$old_cli" ] || [ -e "$old_daemon" ]; do
    old_cli="$base_cli-$free"
    old_daemon="$base_daemon-$free"
    free=$((free + 1))
  done
  say "Upgrading memprove in $prefix: build $old_build -> $new_build"

  # Stage the new binaries next to the old ones; nothing changes yet.
  cp "$binaries/memprove" "$bin/.memprove.new"
  cp "$binaries/memproved" "$bin/.memproved.new"
  chmod 700 "$bin/.memprove.new" "$bin/.memproved.new"
  local staged="$bin/.memprove.new"
  "$staged" --version >/dev/null && "$bin/.memproved.new" --version >/dev/null \
    || { rm -f "$bin/.memprove.new" "$bin/.memproved.new"; die "the new binaries do not run here; nothing was changed"; }

  # U20 (N2): a continuityd the rename stopped was running too.
  if alive "$staged" || [ "$legacy_was_running" = 1 ]; then was_running=1; fi
  say "1/5 stopping memproved ..."
  if ! stop_daemon "$staged"; then
    rm -f "$bin/.memprove.new" "$bin/.memproved.new"
    die "memproved could not be stopped; nothing was changed, and the old version still runs"
  fi

  restart_old() {
    if [ "$was_running" = 1 ]; then
      start_daemon >/dev/null 2>&1 || warn "the old memproved did not start again; start it with $bin/ensure-daemon.sh"
    fi
  }

  say "2/5 backing up $data to $backup_dir ..."
  if ! "$staged" backup --data-dir "$data" --out "$backup_dir"; then
    rm -f "$bin/.memprove.new" "$bin/.memproved.new"
    restart_old
    die "the backup failed; nothing was changed, and the old version is in place"
  fi
  # After the backup, which records the modes as they were (a restore of
  # it gives them back), and while the daemon is stopped. A rollback
  # leaves the directories tightened: the old daemon only needs its owner
  # to enter them.
  tighten_data_dirs

  say "3/5 keeping the old binaries as $(basename "$old_cli") and $(basename "$old_daemon") ..."
  swap_failed() {
    [ -e "$cli" ] && [ -e "$old_cli" ] && rm -f "$cli"
    [ -e "$daemon" ] && [ -e "$old_daemon" ] && rm -f "$daemon"
    [ -e "$old_cli" ] && mv "$old_cli" "$cli"
    [ -e "$old_daemon" ] && mv "$old_daemon" "$daemon"
    rm -f "$bin/.memprove.new" "$bin/.memproved.new"
    restart_old
    die "the binaries could not be swapped; the old version is in place, and the backup is $backup_dir"
  }
  mv "$cli" "$old_cli" || swap_failed
  mv "$daemon" "$old_daemon" || swap_failed
  mv "$bin/.memprove.new" "$cli" || swap_failed
  mv "$bin/.memproved.new" "$daemon" || swap_failed
  [ -f "$config/data-dir" ] || printf '%s\n' "$data" >"$config/data-dir"
  if [ "$renamed_from_previous_name" = 1 ]; then
    write_ct
    write_ensure_daemon
  else
    [ -f "$bin/ct" ] || write_ct
    [ -f "$bin/ensure-daemon.sh" ] || write_ensure_daemon
  fi

  rollback() {
    warn "putting the old version back"
    stop_daemon "$cli" || true
    rm -f "$cli" "$daemon"
    mv "$old_cli" "$cli"
    mv "$old_daemon" "$daemon"
    restart_old
  }

  say "4/5 starting memproved $new_build ..."
  if ! start_daemon; then
    rollback
    die "the new memproved did not start (see $prefix/daemon.log); the old version is back, and the backup is $backup_dir"
  fi

  say "5/5 verifying the ledger of every Project ..."
  token="$agent_token"
  [ -f "$token" ] || token="$operator_token"
  # U3 (K7, F-N-2): only a Project the registry lists is verified. No
  # operation lists them, so the registry is asked about each directory
  # with the control token; one it does not list only gets a warning.
  local registry=1 verified=0 answer listed verify_err
  if [ ! -f "$operator_token" ]; then
    registry=0
    warn "no control token at $operator_token; every directory in data/projects is verified"
  fi
  # U3 (X-F SF-1): every entry of data/projects/, a dot-named one too (a
  # glob skips those), in the order the tightening step uses; one that is
  # not a directory, or a link to one, is skipped as before. A name that
  # starts with a dot is never a Project ID, so it gets the stray warning
  # without the control token as well. U4 (Y2, R1-N1): so does any name that
  # is not the shape of a lowercase UUIDv7; only such a name can be a Project.
  local entries=()
  if [ -d "$data/projects" ]; then
    while IFS= read -r -d '' project; do
      entries+=("$project")
    done < <(find "$data/projects/" -mindepth 1 -maxdepth 1 -print0 | LC_ALL=C sort -z)
  fi
  for project in ${entries[@]+"${entries[@]}"}; do
    [ -d "$project" ] || continue
    project="$(basename "$project")"
    if [ "$registry" = 0 ] && ! [[ "$project" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$ ]]; then
      warn "data/projects/$project is not a Project the registry lists; its ledger was not verified, and it was left as it is"
      continue
    fi
    if [ "$registry" = 1 ]; then
      # A Project ID is a lowercase UUIDv7; any other name is not asked about.
      if ! [[ "$project" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$ ]]; then
        answer='"code":"invalid_request"'
      elif answer="$("$cli" --data-dir "$data" --token-file "$operator_token" project get "$project" 2>/dev/null)"; then
        answer=""
      fi
      case "$answer" in
        "") ;;
        *'"code":"project_not_found"'* | *'"code":"invalid_request"'*)
          warn "data/projects/$project is not a Project the registry lists; its ledger was not verified, and it was left as it is"
          continue
          ;;
        *)
          failed="$project"
          break
          ;;
      esac
    fi
    # U5 (H1, Y-V N-2): without the control token the registry could not be
    # asked, so a UUIDv7-shaped directory it does not list reaches
    # `ledger verify`; its `404 project_not_found` marks it as unregistered
    # (a warning, as above), not as a ledger that failed to verify.
    if verify_err="$("$cli" --data-dir "$data" --token-file "$token" ledger verify --project "$project" 2>&1 >/dev/null)"; then
      say "    Project $project: valid"
      verified=$((verified + 1))
    elif [ "$registry" = 0 ] && [[ "$verify_err" == *'404 project_not_found'* ]]; then
      warn "data/projects/$project is not a Project the registry lists; its ledger was not verified, and it was left as it is"
    else
      [ -z "$verify_err" ] || printf '%s\n' "$verify_err" >&2
      failed="$project"
      break
    fi
  done
  if [ -z "$failed" ] && [ "$registry" = 1 ]; then
    listed="$("$cli" --data-dir "$data" --token-file "$operator_token" status 2>/dev/null \
      | sed -n 's/.*"project_count":\([0-9][0-9]*\).*/\1/p')" || listed=""
    if [ -n "$listed" ] && [ "$listed" -gt "$verified" ]; then
      warn "the registry lists $listed Projects, and $verified of them have a directory in data/projects; the others were not verified"
    fi
  fi
  if [ -n "$failed" ]; then
    rollback
    die "ledger verify failed for Project $failed; the old version is back, and the backup is $backup_dir"
  fi
  # U2 (S4, Owner O7): the daemon ends the upgrade as it began it. One
  # that was not running was started only for `ledger verify`.
  if [ "$was_running" = 0 ]; then
    say "    memproved was not running before the upgrade; stopping it again ..."
    stop_daemon "$cli" || warn "memproved $new_build could not be stopped; it is still running"
  fi
  install_guide
  say "Upgraded to $new_build. Backup: $backup_dir. Old binaries: $old_cli, $old_daemon."
  say "The agent's guide, for this version: $guide"
  maybe_write_profile
}

# ---- fresh install --------------------------------------------------------

fresh_started=0
fresh_cleanup() {
  if [ "$fresh_started" = 1 ]; then
    stop_daemon "$cli" || true
  fi
}

fresh_install() {
  local created secret
  say "Installing memprove $new_build in $prefix ..."
  mkdir -p "$prefix" "$bin" "$data" "$config"
  chmod 700 "$prefix" "$bin" "$data" "$config"
  cp "$binaries/memprove" "$cli"
  cp "$binaries/memproved" "$daemon"
  chmod 700 "$cli" "$daemon"
  printf '%s\n' "$data" >"$config/data-dir"
  write_ct
  write_ensure_daemon
  install_guide
  "$daemon" bootstrap --data-dir "$data" --label install-operator --output "$operator_token" \
    || die "memproved bootstrap failed; remove $prefix and run again"
  trap fresh_cleanup EXIT
  start_daemon || die "memproved did not start (see $prefix/daemon.log)"
  fresh_started=1
  # The secret stays in this shell: captured, cut out, and written to the
  # token file by printf (a builtin), never echoed or passed as an argument.
  created="$("$cli" --data-dir "$data" --token-file "$operator_token" \
    token create --label install-agent --scope mcp-read --scope mcp-write)" \
    || die "the agent token could not be created"
  secret="$(printf '%s' "$created" | sed -n 's/.*"secret":"\([A-Za-z0-9_-]*\)".*/\1/p')"
  unset created
  [ "${#secret}" -eq 43 ] || die "the agent token's secret was not in the response"
  printf '%s\n' "$secret" >"$agent_token"
  unset secret
  chmod 600 "$agent_token"
  trap - EXIT
  print_next_steps
  maybe_write_profile
}

# U19 (the rename to memprove): an installation made under the previous name
# has bin/continuity and bin/continuityd, a ct and an ensure-daemon.sh that
# run them, and perhaps a running continuityd. Stop that daemon, give the old
# binaries the new names (their build is still read from --version), drop its
# stale lock and socket, and let the upgrade below replace them; ct and
# ensure-daemon.sh are then written again for the new names, and (U20 N2) a
# continuityd that was running is followed by a running memproved. An installation
# left with one binary under each name (an interrupted renaming) is finished
# the same way; one that holds a binary under both names is left alone.
renamed_from_previous_name=0
legacy_was_running=0
if [ -e "$bin/continuityd" ] || [ -e "$bin/continuity" ]; then
  say "Renaming the installation in $prefix from continuity to memprove ..."
  stop_legacy_daemon || die "the continuityd of the previous name could not be stopped; nothing was changed"
  if [ -e "$bin/continuityd" ]; then
    [ -e "$daemon" ] && die "$prefix holds both bin/continuityd and bin/memproved; remove one of them"
    mv "$bin/continuityd" "$daemon" || die "bin/continuityd could not be renamed"
  fi
  if [ -e "$bin/continuity" ]; then
    [ -e "$cli" ] && die "$prefix holds both bin/continuity and bin/memprove; remove one of them"
    mv "$bin/continuity" "$cli" || die "bin/continuity could not be renamed"
  fi
  rm -f "$data/continuityd.lock" "$data/continuityd.sock"
  renamed_from_previous_name=1
fi

if [ -e "$daemon" ]; then
  [ -x "$cli" ] || die "$prefix holds memproved but not memprove; repair or remove it"
  upgrade
elif [ -n "$(ls -A "$bin" 2>/dev/null)" ] || [ -n "$(ls -A "$data" 2>/dev/null)" ]; then
  die "$prefix holds a partial installation (no $daemon); remove it or choose another --prefix"
else
  fresh_install
fi
