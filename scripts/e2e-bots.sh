#!/usr/bin/env bash
# Two-user bot network smoke (docs/design/agent-bots.md §11): a local Edge
# (wrangler dev, dev auth), two headless engines signed in as two different
# users, and the cypher-rpc bot_network_driver example: invite, redeem, a
# question answered across both BotRooms, and a revoke.
#
# It starts its own Edge on its own port and its own data directories, and
# stops everything it started.
#
# Usage: scripts/e2e-bots.sh
# Env:   CYPHER_E2E_BOTS_PORT (default 27681), CYPHER_E2E_KEEP_LOGS=1 to keep logs.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
EDGE_PORT="${CYPHER_E2E_BOTS_PORT:-27681}"
EDGE_URL="http://127.0.0.1:${EDGE_PORT}"
WORK="$(mktemp -d /tmp/cypher-e2e-bots.XXXXXX)"
A_DIR="$WORK/alice"
B_DIR="$WORK/bob"

EDGE_PID=""
A_PID=""
B_PID=""
STATUS=1

cleanup() {
  for pid in "$A_PID" "$B_PID"; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done
  [[ -n "$EDGE_PID" ]] && kill -- -"$EDGE_PID" 2>/dev/null || true
  sleep 1
  for pid in "$A_PID" "$B_PID"; do
    [[ -n "$pid" ]] && kill -9 "$pid" 2>/dev/null || true
  done
  [[ -n "$EDGE_PID" ]] && kill -9 -- -"$EDGE_PID" 2>/dev/null || true
  if [[ "$STATUS" -ne 0 ]]; then
    echo "--- engine A log (tail) ---"; tail -n 40 "$WORK/engine-a.log" 2>/dev/null || true
    echo "--- engine B log (tail) ---"; tail -n 40 "$WORK/engine-b.log" 2>/dev/null || true
    echo "--- edge log (tail) ---"; tail -n 40 "$WORK/edge.log" 2>/dev/null || true
  fi
  if [[ "${CYPHER_E2E_KEEP_LOGS:-0}" == "1" ]]; then
    echo "logs kept in $WORK"
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

wait_for() { # wait_for <description> <timeout_s> <command...>
  local what="$1" timeout="$2"; shift 2
  local waited=0
  until "$@" >/dev/null 2>&1; do
    sleep 1
    waited=$((waited + 1))
    if [[ "$waited" -ge "$timeout" ]]; then
      echo "FAIL: timed out waiting for $what" >&2
      exit 1
    fi
  done
}

if curl -sf -m 2 "$EDGE_URL/health" >/dev/null 2>&1; then
  echo "FAIL: something already listens on :$EDGE_PORT; pick another CYPHER_E2E_BOTS_PORT" >&2
  exit 1
fi
echo "edge: starting wrangler dev on :$EDGE_PORT"
set -m
bash -c "cd '$ROOT/apps/edge' && exec npx wrangler dev --port '$EDGE_PORT' --var AUTH_MODE:dev --persist-to '$WORK/wrangler'" \
  >"$WORK/edge.log" 2>&1 &
EDGE_PID=$!
set +m
wait_for "edge /health" 90 curl -sf -m 3 "$EDGE_URL/health"

echo "build: cypher + bot_network_driver"
(cd "$ROOT" && cargo build -q -p cypher)
(cd "$ROOT" && cargo build -q -p cypher-rpc --example bot_network_driver)
CYPHER="$ROOT/target/debug/cypher"
DRIVER="$ROOT/target/debug/examples/bot_network_driver"

mkdir -p "$A_DIR" "$B_DIR"
start_engine() { # start_engine <data_dir> <token> <org> <log>
  CYPHER_DATA_DIR="$1" CYPHER_EDGE_URL="$EDGE_URL" CYPHER_EDGE_TOKEN="$2" CYPHER_ORG_ID="$3" \
    CYPHER_HARNESS=mock CYPHER_AUTO_UPDATE=0 RUST_LOG=info \
    "$CYPHER" headless >"$4" 2>&1 &
}
start_engine "$A_DIR" "alice@org-a" "org-a" "$WORK/engine-a.log"; A_PID=$!
start_engine "$B_DIR" "bob@org-b" "org-b" "$WORK/engine-b.log"; B_PID=$!
wait_for "engine A IPC" 60 env CYPHER_DATA_DIR="$A_DIR" "$CYPHER" sync
wait_for "engine B IPC" 60 env CYPHER_DATA_DIR="$B_DIR" "$CYPHER" sync
echo "engines: alice pid=$A_PID  bob pid=$B_PID"

"$DRIVER" "$A_DIR" "$B_DIR"
STATUS=$?
exit "$STATUS"
