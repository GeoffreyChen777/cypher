#!/usr/bin/env bash
# Demo: thinking between tool calls folds into one transcript row.
#
# Boots an offline mock engine whose every reply opens with eight thoughts
# around ten commands (CYPHER_MOCK_WORK=1), seeds a "Work run demo" project
# with one such reply, and opens the app on it. That run used to read as
# sixteen alternating "Thought" / "Ran N commands" rows; it is now one
# "Ran 10 commands · 8 thoughts · 1 failed" row above the answer.
#
#   scripts/dev-demo-work.sh          # build, seed the demo project, open the app
#   scripts/dev-demo-work.sh --slow   # pace replies (~15s): send any prompt to
#                                     # watch the run stream open, then fold
#
# Everything lives under /tmp/cypher-demo-work-*; re-runs reuse it (delete
# those directories to reseed). Quitting the app stops the engine.
set -euo pipefail
cd "$(dirname "$0")/.."

DAEMON_DIR=/tmp/cypher-demo-work-daemon
UI_DIR=/tmp/cypher-demo-work-ui
DELAY=""
[[ "${1:-}" == "--slow" ]] && DELAY=350

# Hermetic: no inherited profile, Edge credentials or engine socket.
for var in $(compgen -e | grep '^CYPHER_' || true); do unset "$var"; done
export CYPHER_PROFILE=local CYPHER_AUTO_UPDATE=0

echo "▸ building (first run takes a few minutes)…"
cargo build -p cypher -q
cargo build -p cypher-rpc --example rpc_probe -q

echo "▸ starting the mock engine with private Unix IPC"
env CYPHER_DATA_DIR="$DAEMON_DIR" CYPHER_HARNESS=mock CYPHER_MOCK_WORK=1 \
  ${DELAY:+CYPHER_MOCK_DELAY_MS=$DELAY} RUST_LOG=warn \
  ./target/debug/cypher headless &
DAEMON_PID=$!
trap 'kill $DAEMON_PID 2>/dev/null || true' EXIT
probe() { ./target/debug/examples/rpc_probe "$DAEMON_DIR" "$@"; }
for _ in $(seq 1 40); do
  probe EngineReady '{}' >/dev/null 2>&1 && break
  sleep 0.25
done

if [[ ! -f "$DAEMON_DIR/.demo-seeded" ]]; then
  echo "▸ seeding the demo project"
  DEV=$(probe LocalDevice '{}' | python3 -c 'import json,sys;print(json.load(sys.stdin)["deviceId"])')
  SPACE=$(uuidgen | tr 'A-Z' 'a-z')
  CHAT=$(uuidgen | tr 'A-Z' 'a-z')
  # The scripted commands read this repo's files, so the project is this checkout.
  probe Mutate "{\"op\":\"createSpace\",\"spaceId\":\"$SPACE\",\"deviceId\":\"$DEV\",\"path\":\"$PWD\",\"name\":\"Work run demo\",\"gitDetected\":true}" >/dev/null
  probe Mutate "{\"op\":\"createChat\",\"chatId\":\"$CHAT\",\"spaceId\":\"$SPACE\",\"config\":{\"harness\":\"mock\",\"model\":\"mock-fable-5\",\"reasoning\":null,\"sandbox\":\"workspace-write\"}}" >/dev/null
  probe Mutate "{\"op\":\"renameChat\",\"chatId\":\"$CHAT\",\"title\":\"Thinking between commands\"}" >/dev/null
  probe QueueCommand "{\"chatId\":\"$CHAT\",\"command\":{\"kind\":\"run\",\"messageId\":\"$(uuidgen)\",\"request\":{\"prompt\":\"Walk me through the streaming pipeline\",\"model\":null,\"reasoning\":null,\"modelOptions\":{},\"cwd\":\"$PWD\",\"sandbox\":\"workspace-write\",\"autoApprove\":true,\"resume\":null}}}" >/dev/null
  touch "$DAEMON_DIR/.demo-seeded"
fi

echo "▸ opening cypher: open \"Thinking between commands\" under Work run demo"
CYPHER_DATA_DIR="$UI_DIR" CYPHER_ENGINE_DATA_DIR="$DAEMON_DIR" RUST_LOG=warn ./target/debug/cypher
