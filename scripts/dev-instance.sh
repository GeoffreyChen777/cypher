#!/usr/bin/env bash
# Start, check, screenshot and stop one private dev instance of this checkout
# (a headless mock engine plus a UI attached to it) with plain commands:
#
#   CYPHER_DEV_INSTANCE=<name> scripts/dev-instance.sh start
#   CYPHER_DEV_INSTANCE=<name> scripts/dev-instance.sh status
#   CYPHER_DEV_INSTANCE=<name> scripts/dev-instance.sh capture <shot>
#   CYPHER_DEV_INSTANCE=<name> scripts/dev-instance.sh stop all
#
# Everything stays in the instance's own data dirs: each process writes its pid
# to <data-dir>/dev.pid (dev-engine.sh / dev-app.sh), script output goes to
# <data-dir>/logs/dev-*.out next to the app's own logs, and screenshots come
# from the app's dev-capture driver into <ui-dir>/capture/. Stopping reads the
# pid file and checks that the process is this checkout's target/debug/cypher
# with this data dir before signalling it; it never searches for processes.
set -euo pipefail
cd "$(dirname "$0")/.."
root="$(pwd -P)"

usage() {
  cat >&2 <<'EOF'
Usage: CYPHER_DEV_INSTANCE=<name> scripts/dev-instance.sh <command>
  start            build (development + dev-capture), then start the engine
                   and the UI unless they already run
  restart-ui       build, stop this instance's UI, start it again (engine kept)
  status           pids, engine IPC state, log and capture paths
  capture NAME     save the main window to <ui-dir>/capture/NAME.png
  drive LINE...    run dev-capture lines in order ("click 40 60", "key cmd-k",
                   "wait 500", "capture NAME"; crates/ui/src/shell/dev_capture.rs)
  stop [ui|all]    stop the UI (default), or the UI and then the engine
Environment: CYPHER_DEV_MODE=local|dev (default local), CYPHER_DEV_INSTANCE=<name>.
EOF
  exit 2
}

die() { echo "dev-instance: $*" >&2; exit 1; }

mode="${CYPHER_DEV_MODE:-local}"
[[ "$mode" == local || "$mode" == dev ]] || die "CYPHER_DEV_MODE must be local or dev"
instance="${CYPHER_DEV_INSTANCE:-}"
[[ "$instance" =~ ^[A-Za-z0-9._-]*$ ]] || die "CYPHER_DEV_INSTANCE: letters, digits, . _ - only"
# dev-engine.sh and dev-app.sh read the same two variables.
export CYPHER_DEV_INSTANCE="$instance"
unset CYPHER_DEV_MODE

dev_root="$HOME/.cypher-development"
engine_dir="$dev_root/$mode-engine${instance:+-$instance}"
ui_dir="$dev_root/$mode-ui${instance:+-$instance}"
capture_dir="$ui_dir/capture"
features=development,dev-capture
umask 077

build() {
  CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
    cargo build --locked -p cypher --features "$features"
}

# The environment entry CYPHER_DATA_DIR of a process (same user only).
data_dir_of() {
  if [[ -r "/proc/$1/environ" ]]; then
    tr '\0' '\n' <"/proc/$1/environ" | sed -n 's/^CYPHER_DATA_DIR=//p'
  else
    ps eww -o command= -p "$1" | tr ' ' '\n' | sed -n 's/^CYPHER_DATA_DIR=//p'
  fi
}

cwd_of() {
  if [[ -e "/proc/$1/cwd" ]]; then
    readlink "/proc/$1/cwd"
  else
    lsof -a -p "$1" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p'
  fi
}

# Print the pid recorded in $1/dev.pid. Status 0: it is this checkout's
# target/debug/cypher running on data dir $1. 1: no pid file or the process is
# gone. 2: the pid is alive but is something else (left alone).
instance_pid() {
  local dir="$1" pid
  [[ -f "$dir/dev.pid" ]] || return 1
  pid="$(<"$dir/dev.pid")"
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  kill -0 "$pid" 2>/dev/null || return 1
  printf '%s\n' "$pid"
  [[ "$(ps -o command= -p "$pid")" == target/debug/cypher* ]] || return 2
  [[ "$(cwd_of "$pid")" == "$root" ]] || return 2
  [[ "$(data_dir_of "$pid" | head -1)" == "$dir" ]] || return 2
}

engine_listening() {
  CYPHER_DATA_DIR="$engine_dir" target/debug/cypher status --verbose 2>/dev/null \
    | grep -q 'IPC:      listening'
}

start_engine() {
  local pid status=0 waited=0
  pid="$(instance_pid "$engine_dir")" || status=$?
  if [[ $status -eq 0 ]]; then
    echo "engine: already running (pid $pid, $engine_dir)"
    return
  fi
  [[ $status -eq 1 ]] || die "pid $pid in $engine_dir/dev.pid is not this checkout's engine; not starting another"
  mkdir -p "$engine_dir/logs"
  CYPHER_DEV_FEATURES="$features" nohup bash scripts/dev-engine.sh "$mode" \
    >"$engine_dir/logs/dev-engine.out" 2>&1 </dev/null &
  pid=$!
  until engine_listening; do
    kill -0 "$pid" 2>/dev/null || { tail -20 "$engine_dir/logs/dev-engine.out" >&2; die "engine exited; see $engine_dir/logs/dev-engine.out"; }
    ((waited++ < 600)) || die "engine not listening after 600 s; see $engine_dir/logs/"
    sleep 1
  done
  echo "engine: started (pid $pid, $engine_dir)"
}

start_ui() {
  local pid status=0 waited=0 log="$ui_dir/logs/cypher-headed.log"
  pid="$(instance_pid "$ui_dir")" || status=$?
  if [[ $status -eq 0 ]]; then
    echo "ui: already running (pid $pid, $ui_dir)"
    return
  fi
  [[ $status -eq 1 ]] || die "pid $pid in $ui_dir/dev.pid is not this checkout's UI; not starting another"
  engine_listening || die "no engine listening for $engine_dir; run start"
  if [[ ! -d "$ui_dir" ]]; then
    # A fresh instance opens on the main window, not onboarding. Delete
    # ui-settings.json (with the UI stopped) to see onboarding.
    mkdir -p "$ui_dir"
    printf '{"setupCompleted":true,"piRuntimeSetupVersion":1}\n' >"$ui_dir/ui-settings.json"
  fi
  mkdir -p "$ui_dir/logs" "$capture_dir"
  rm -f "$capture_dir/cmd" "$capture_dir/cmd.tmp"
  CYPHER_CAPTURE_DIR="$capture_dir" nohup bash scripts/dev-app.sh "$mode" \
    >"$ui_dir/logs/dev-app.out" 2>&1 </dev/null &
  pid=$!
  # The driver starts on the main window's first frame, after this launch
  # rotated the headed log.
  until [[ "$(cat "$ui_dir/dev.pid" 2>/dev/null)" == "$pid" ]] \
        && grep -q 'dev capture driver listening' "$log" 2>/dev/null; do
    kill -0 "$pid" 2>/dev/null || { tail -20 "$ui_dir/logs/dev-app.out" >&2; die "UI exited; see $ui_dir/logs/"; }
    ((waited++ < 120)) || die "UI capture driver not up after 60 s; see $log"
    sleep 0.5
  done
  echo "ui: started (pid $pid, $ui_dir)"
  grep -q 'engine daemon detected; connecting' "$log" && echo "ui: attached to the engine over IPC"
}

stop_one() {
  local label="$1" dir="$2" pid status=0 waited=0
  pid="$(instance_pid "$dir")" || status=$?
  if [[ $status -eq 1 ]]; then
    rm -f "$dir/dev.pid"
    echo "$label: not running"
    return
  fi
  [[ $status -eq 0 ]] || die "pid $pid in $dir/dev.pid is not this checkout's $label; left alone"
  kill "$pid"
  while kill -0 "$pid" 2>/dev/null; do
    ((waited++ < 60)) || die "$label pid $pid still running 30 s after SIGTERM"
    sleep 0.5
  done
  rm -f "$dir/dev.pid"
  echo "$label: stopped (pid $pid exited)"
}

drive() {
  local pid status=0 before=0 waited=0 log="$capture_dir/log" out
  [[ $# -gt 0 ]] || usage
  pid="$(instance_pid "$ui_dir")" || status=$?
  [[ $status -eq 0 ]] || die "no UI of this checkout running on $ui_dir; run start"
  [[ ! -e "$capture_dir/cmd" ]] || die "$capture_dir/cmd is still pending"
  [[ -f "$log" ]] && before="$(wc -l <"$log")"
  printf '%s\n' "$@" >"$capture_dir/cmd.tmp"
  # Rename into place so the driver never reads a half-written file.
  mv "$capture_dir/cmd.tmp" "$capture_dir/cmd"
  until tail -n "+$((before + 1))" "$log" 2>/dev/null | grep -qx 'ok done'; do
    kill -0 "$pid" 2>/dev/null || die "UI pid $pid exited"
    ((waited++ < 240)) || die "no result in $log after 120 s"
    sleep 0.5
  done
  out="$(tail -n "+$((before + 1))" "$log")"
  printf '%s\n' "$out"
  ! grep -q '^ERR ' <<<"$out"
}

command="${1:-}"
[[ $# -gt 0 ]] && shift
case "$command" in
  start)
    build
    start_engine
    start_ui
    ;;
  restart-ui)
    build
    stop_one ui "$ui_dir"
    start_ui
    ;;
  status)
    status=0
    pid="$(instance_pid "$engine_dir")" || status=$?
    case $status in 0) echo "engine: pid $pid";; 1) echo "engine: not running";; *) echo "engine: pid file names pid $pid, not this checkout's engine";; esac
    if engine_listening; then echo "engine IPC: listening"; else echo "engine IPC: not listening"; fi
    status=0
    pid="$(instance_pid "$ui_dir")" || status=$?
    case $status in 0) echo "ui: pid $pid";; 1) echo "ui: not running";; *) echo "ui: pid file names pid $pid, not this checkout's UI";; esac
    echo "engine log: $engine_dir/logs/cypher-headless.log (script output: dev-engine.out)"
    echo "ui log: $ui_dir/logs/cypher-headed.log (script output: dev-app.out)"
    echo "captures: $capture_dir"
    ;;
  capture)
    name="${1:-}"
    [[ "$name" =~ ^[A-Za-z0-9._-]+$ ]] || die "capture NAME: letters, digits, . _ - only"
    drive "capture $name"
    echo "$capture_dir/$name.png"
    ;;
  drive)
    drive "$@"
    ;;
  stop)
    what="${1:-ui}"
    [[ "$what" == ui || "$what" == all ]] || usage
    stop_one ui "$ui_dir"
    if [[ "$what" == all ]]; then stop_one engine "$engine_dir"; fi
    ;;
  *)
    usage
    ;;
esac
