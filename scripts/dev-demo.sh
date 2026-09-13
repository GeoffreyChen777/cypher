#!/usr/bin/env bash
# One-command demo: boots a seeded engine daemon + the headed app, offline.
# Made for judging look & feel with real input — no edge, no auth needed.
#
#   scripts/dev-demo.sh            # build, seed demo data, open the app
#   scripts/dev-demo.sh --slow     # pace mock streams (~10s) to watch streaming
#
# Everything lives under /tmp/cypher-demo-*; re-runs reuse it. Ctrl-C cleans up.
set -euo pipefail
cd "$(dirname "$0")/.."

DAEMON_DIR=/tmp/cypher-demo-daemon
UI_DIR=/tmp/cypher-demo-ui
DELAY=""
[[ "${1:-}" == "--slow" ]] && DELAY=350

echo "▸ building (first run takes a few minutes)…"
cargo build -p cypher -q
cargo build -p cypher-rpc --example rpc_probe -q

echo "▸ starting engine daemon with private Unix IPC"
env CYPHER_DATA_DIR="$DAEMON_DIR" CYPHER_HARNESS=mock \
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
  echo "▸ seeding demo chats"
  DEV=$(probe LocalDevice '{}' | python3 -c 'import json,sys;print(json.load(sys.stdin)["deviceId"])')
  # One space per demo folder, created up-front (chats join by space id).
  declare -A SPACES=()
  for project in cypher soccertcg cypher aether; do
    sid=$(uuidgen | tr 'A-Z' 'a-z')
    probe Mutate "{\"op\":\"createSpace\",\"spaceId\":\"$sid\",\"deviceId\":\"$DEV\",\"path\":\"$HOME/github/$project\"}" >/dev/null
    SPACES[$project]="$sid"
  done
  seed() { # title project branch age_hours run
    local id; id=$(uuidgen | tr 'A-Z' 'a-z')
    local sid="${SPACES[$2]}"
    probe Mutate "{\"op\":\"createChat\",\"chatId\":\"$id\",\"spaceId\":\"$sid\",\"config\":{\"harness\":\"mock\",\"model\":\"fable-5\",\"reasoning\":null,\"sandbox\":\"workspace-write\"}}" >/dev/null
    probe Mutate "{\"op\":\"renameChat\",\"chatId\":\"$id\",\"title\":\"$1\"}" >/dev/null
    probe Mutate "{\"op\":\"setChatBranch\",\"chatId\":\"$id\",\"branch\":\"$3\"}" >/dev/null
    if [[ "$5" == run ]]; then
      probe QueueCommand "{\"chatId\":\"$id\",\"command\":{\"kind\":\"run\",\"messageId\":\"$(uuidgen)\",\"request\":{\"prompt\":\"Walk me through the streaming pipeline\",\"model\":null,\"reasoning\":null,\"modelOptions\":{},\"cwd\":\"/tmp\",\"sandbox\":\"workspace-write\",\"autoApprove\":true,\"resume\":null}}}" >/dev/null
      sleep 1
    fi
    probe Mutate "{\"op\":\"setChatActivity\",\"chatId\":\"$id\",\"lastMessageAt\":$(( ($(date +%s) - $4*3600) * 1000 ))}" >/dev/null
  }
  seed "Native Cypher Rust Rewrite"    cypher cypher/main                 0  run
  seed "Rebalance Player Stats Caps"  soccertcg    cypher/rebalance-player-stat-caps  2  run
  seed "Craft Premium TCG Experience" soccertcg    cypher/craft-premium-tcg-exp       26 skip
  seed "Initial Context Exploration"  cypher        cypher/initial-context-exploration 14 skip
  seed "Soccer TCG Repo Creation"     aether       aether/main                       48 skip
  touch "$DAEMON_DIR/.demo-seeded"
fi

echo "▸ opening cypher (composer is live — type into it; --slow shows streaming)"
CYPHER_DATA_DIR="$UI_DIR" CYPHER_ENGINE_DATA_DIR="$DAEMON_DIR" RUST_LOG=warn ./target/debug/cypher
