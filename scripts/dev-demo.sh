#!/usr/bin/env bash
# One-command demo: boots a seeded mock engine + the headed app, offline.
# Made for judging look & feel with real input — no Edge, no auth needed.
#
#   scripts/dev-demo.sh           # build, seed demo chats, open the app
#   scripts/dev-demo.sh --slow    # pace mock replies (~10s) to watch streaming
#   scripts/dev-demo.sh --work    # every reply opens with eight thoughts around
#                                 # ten commands (CYPHER_MOCK_WORK=1); open
#                                 # "Thinking between commands" under "Work run
#                                 # demo" to see the run fold into one
#                                 # "Ran 10 commands · 8 thoughts · 1 failed" row
#
# The flags combine. Everything lives under /tmp/cypher-demo-* (--work:
# /tmp/cypher-demo-work-*); re-runs reuse it (delete those directories to
# reseed). Quitting the app stops the engine.
set -euo pipefail
cd "$(dirname "$0")/.."

WORK=""
DELAY=""
for arg in "$@"; do
  case "$arg" in
    --work) WORK=1 ;;
    --slow) DELAY=350 ;;
    *) echo 'Usage: dev-demo.sh [--work] [--slow]' >&2; exit 2 ;;
  esac
done
DEMO_DIR=/tmp/cypher-demo${WORK:+-work}
DAEMON_DIR=$DEMO_DIR-daemon
UI_DIR=$DEMO_DIR-ui

# Hermetic: no inherited profile, Edge credentials or engine socket.
for var in $(compgen -e | grep '^CYPHER_' || true); do unset "$var"; done
export CYPHER_PROFILE=local CYPHER_AUTO_UPDATE=0

echo "▸ building (first run takes a few minutes)…"
cargo build -p cypher -q
cargo build -p cypher-rpc --example rpc_probe -q

echo "▸ starting the mock engine with private Unix IPC"
env CYPHER_DATA_DIR="$DAEMON_DIR" CYPHER_HARNESS=mock \
  ${WORK:+CYPHER_MOCK_WORK=1} ${DELAY:+CYPHER_MOCK_DELAY_MS=$DELAY} RUST_LOG=warn \
  ./target/debug/cypher headless &
DAEMON_PID=$!
trap 'kill $DAEMON_PID 2>/dev/null || true' EXIT
probe() { ./target/debug/examples/rpc_probe "$DAEMON_DIR" "$@"; }
for _ in $(seq 1 40); do
  probe EngineReady '{}' >/dev/null 2>&1 && break
  sleep 0.25
done

new_id() { uuidgen | tr 'A-Z' 'a-z'; }
mutate() { probe Mutate "$1" >/dev/null; }
create_space() { # id path [extra json fields]
  mutate "{\"op\":\"createSpace\",\"spaceId\":\"$1\",\"deviceId\":\"$DEV\",\"path\":\"$2\"${3:+,$3}}"
}
create_chat() { # id space title
  mutate "{\"op\":\"createChat\",\"chatId\":\"$1\",\"spaceId\":\"$2\",\"config\":{\"harness\":\"mock\",\"model\":\"mock-fable-5\",\"reasoning\":null,\"sandbox\":\"workspace-write\"}}"
  mutate "{\"op\":\"renameChat\",\"chatId\":\"$1\",\"title\":\"$3\"}"
}
run_prompt() { # chat cwd
  probe QueueCommand "{\"chatId\":\"$1\",\"command\":{\"kind\":\"run\",\"messageId\":\"$(uuidgen)\",\"request\":{\"prompt\":\"Walk me through the streaming pipeline\",\"model\":null,\"reasoning\":null,\"modelOptions\":{},\"cwd\":\"$2\",\"sandbox\":\"workspace-write\",\"autoApprove\":true,\"resume\":null}}}" >/dev/null
}

if [[ ! -f "$DAEMON_DIR/.demo-seeded" ]]; then
  echo "▸ seeding demo data"
  DEV=$(probe EngineInfo '{}' | python3 -c 'import json,sys;print(json.load(sys.stdin)["deviceId"])')
  if [[ -n "$WORK" ]]; then
    # The scripted commands read this repo's files, so the project is this checkout.
    space=$(new_id) chat=$(new_id)
    create_space "$space" "$PWD" '"name":"Work run demo","gitDetected":true'
    create_chat "$chat" "$space" "Thinking between commands"
    run_prompt "$chat" "$PWD"
  else
    # This checkout plus two placeholder projects under the demo directory.
    mkdir -p "$DEMO_DIR-projects/soccertcg" "$DEMO_DIR-projects/aether"
    cypher_space=$(new_id) soccer_space=$(new_id) aether_space=$(new_id)
    create_space "$cypher_space" "$PWD"
    create_space "$soccer_space" "$DEMO_DIR-projects/soccertcg"
    create_space "$aether_space" "$DEMO_DIR-projects/aether"
    seed() { # space path title branch age_hours run|skip
      local id; id=$(new_id)
      create_chat "$id" "$1" "$3"
      mutate "{\"op\":\"setChatBranch\",\"chatId\":\"$id\",\"branch\":\"$4\"}"
      if [[ "$6" == run ]]; then
        run_prompt "$id" "$2"
        sleep 1
      fi
      mutate "{\"op\":\"setChatActivity\",\"chatId\":\"$id\",\"lastMessageAt\":$(( ($(date +%s) - $5*3600) * 1000 ))}"
    }
    seed "$cypher_space" "$PWD" "Native Cypher Rust Rewrite" cypher/main 0 run
    seed "$soccer_space" "$DEMO_DIR-projects/soccertcg" "Rebalance Player Stats Caps" cypher/rebalance-player-stat-caps 2 run
    seed "$soccer_space" "$DEMO_DIR-projects/soccertcg" "Craft Premium TCG Experience" cypher/craft-premium-tcg-exp 26 skip
    seed "$cypher_space" "$PWD" "Initial Context Exploration" cypher/initial-context-exploration 14 skip
    seed "$aether_space" "$DEMO_DIR-projects/aether" "Soccer TCG Repo Creation" aether/main 48 skip
  fi
  touch "$DAEMON_DIR/.demo-seeded"
fi

echo "▸ opening cypher (composer is live — type into it; --slow shows streaming)"
CYPHER_DATA_DIR="$UI_DIR" CYPHER_ENGINE_DATA_DIR="$DAEMON_DIR" RUST_LOG=warn ./target/debug/cypher
