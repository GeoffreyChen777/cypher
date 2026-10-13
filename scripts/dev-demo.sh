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
#   scripts/dev-demo.sh --permissions
#                                 # permission control: under "Permission control
#                                 # demo", a settled deploy (commands run outside
#                                 # the sandbox, blocked, denied, approvals filed
#                                 # under their calls), a chat waiting on a live
#                                 # permission card, one waiting on an ordinary
#                                 # question, and command rules in Settings →
#                                 # Permissions
#   scripts/dev-demo.sh --diffs   # "Diff fences" under "Diff fence demo": an
#                                 # answer with chat-style and git-patch
#                                 # ```diff blocks
#   scripts/dev-demo.sh --changes # turn changes: "Due dates for tasks" under
#                                 # "Turn changes demo", four rounds on a small
#                                 # Git project — each round's answer lists the
#                                 # files it changed (added, modified, deleted,
#                                 # renamed; one round with more files than the
#                                 # card lists, one that changed nothing); click
#                                 # a file for the round's diff
#
# The flags combine (--permissions, --diffs or --changes takes the place of
# the default seed).
# Everything lives under /tmp/cypher-demo-* (--work: /tmp/cypher-demo-work-*,
# --permissions: /tmp/cypher-demo-permissions-*, --diffs:
# /tmp/cypher-demo-diffs-*, --changes: /tmp/cypher-demo-changes-*); re-runs
# reuse it (delete
# those directories to reseed). Quitting the app stops the engine.
set -euo pipefail
cd "$(dirname "$0")/.."

WORK=""
DELAY=""
PERMS=""
DIFFS=""
CHANGES=""
for arg in "$@"; do
  case "$arg" in
    --work) WORK=1 ;;
    --slow) DELAY=350 ;;
    --permissions) PERMS=1 ;;
    --diffs) DIFFS=1 ;;
    --changes) CHANGES=1 ;;
    *) echo 'Usage: dev-demo.sh [--work] [--slow] [--permissions] [--diffs] [--changes]' >&2; exit 2 ;;
  esac
done
DEMO_DIR=/tmp/cypher-demo${WORK:+-work}${PERMS:+-permissions}${DIFFS:+-diffs}${CHANGES:+-changes}
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
run_prompt() { # chat cwd [prompt] [what the agent receives, e.g. with a mock marker]
  local prompt="${3:-Walk me through the streaming pipeline}"
  probe QueueCommand "{\"chatId\":\"$1\",\"command\":{\"kind\":\"run\",\"messageId\":\"$(uuidgen)\",\"request\":{\"prompt\":\"$prompt\",\"model\":null,\"reasoning\":null,\"modelOptions\":{},\"cwd\":\"$2\",\"sandbox\":\"workspace-write\",\"autoApprove\":true,\"resume\":null}${4:+,\"agentPrompt\":\"$4\"}}}" >/dev/null
}

# Wait until `chat` has `n` settled answers and, with `recorded`, the last
# one carries the round's changes (recorded just after the round settles).
wait_round() { # chat n [recorded]
  for _ in $(seq 1 120); do
    probe WatchDocMessages "{\"chatId\":\"$1\"}" --stream 1 2>/dev/null | python3 -c '
import json, sys
n, recorded = int(sys.argv[1]), sys.argv[2] == "recorded"
answers = [e for e in json.load(sys.stdin).get("reset", [])
           if e["role"] == "assistant" and e.get("status") == "complete"]
sys.exit(0 if len(answers) >= n and (not recorded or "turnChanges" in answers[-1]) else 1)
' "$2" "${3:-}" && return 0
    sleep 0.25
  done
  echo "round $2 of $1 did not settle" >&2
  return 1
}

if [[ ! -f "$DAEMON_DIR/.demo-seeded" ]]; then
  echo "▸ seeding demo data"
  DEV=$(probe EngineInfo '{}' | python3 -c 'import json,sys;print(json.load(sys.stdin)["deviceId"])')
  if [[ -n "$PERMS" ]]; then
    # A placeholder project, with a deploy script a command rule pins.
    project="$DEMO_DIR-projects/webapp"
    mkdir -p "$project/scripts"
    printf '#!/bin/sh\nrsync -az dist/ deploy@web:/srv/app/\n' >"$project/scripts/deploy.sh"
    chmod +x "$project/scripts/deploy.sh"
    space=$(new_id) story=$(new_id) card=$(new_id) ask=$(new_id)
    create_space "$space" "$project" '"name":"Permission control demo"'
    # Oldest first: the settled story ends up on top.
    create_chat "$ask" "$space" "Tidy up the config loader"
    run_prompt "$ask" "$project" "Tidy up the config loader." "Tidy up the config loader. [mock:ask]"
    sleep 1
    create_chat "$card" "$space" "Is the app on web healthy?"
    run_prompt "$card" "$project" "Is the app on web healthy?" "Is the app on web healthy? [mock:permission-card]"
    sleep 1
    create_chat "$story" "$space" "Deploy the web app"
    run_prompt "$story" "$project" "Restart the app on web, then publish the release." \
      "Restart the app on web, then publish the release. [mock:permissions]"
    # Settings → Permissions: permission control on, with command rules.
    mkdir -p "$DAEMON_DIR/pi-runtime/agent"
    deploy_sha=$(shasum -a 256 "$project/scripts/deploy.sh" | cut -d' ' -f1)
    cat >"$DAEMON_DIR/pi-runtime/agent/permissions.json" <<JSON
{
  "version": 1,
  "enabled": true,
  "approval": { "escalate": "ask" },
  "commands": [
    { "match": ["ssh", "deploy@web", "**"], "decision": "allow", "label": "Check the app on the server web", "createdAt": "2026-10-12T09:10:00Z" },
    { "match": ["./scripts/deploy.sh", "**"], "decision": "allow", "label": "Deploy the web app", "sha256": "$deploy_sha", "createdAt": "2026-10-12T09:12:00Z" },
    { "match": ["git", "push", "**", "--force", "**"], "decision": "ask", "label": "Force-push" },
    { "match": ["kubectl", "delete", "**"], "decision": "deny" }
  ]
}
JSON
  elif [[ -n "$CHANGES" ]]; then
    # A small Python project as its own Git repository; the mock agent's
    # [mock:changes:N] rounds edit it like a real agent's tools would.
    project="$DEMO_DIR-projects/tasks-app"
    rm -rf "$project"
    mkdir -p "$project"
    cp -R scripts/demo/tasks-app/. "$project/"
    find "$project" -name __pycache__ -prune -exec rm -rf {} +
    git -C "$project" init -q -b main
    git -C "$project" add .
    git -C "$project" -c user.name=Demo -c user.email=demo@example.com commit -q -m "tasks 0.4.0"
    space=$(new_id) chat=$(new_id)
    create_space "$space" "$project" '"name":"Turn changes demo","gitDetected":true'
    create_chat "$chat" "$space" "Due dates for tasks"
    prompts=(
      "Add due dates to tasks: tasks add --due fri, and flag overdue ones in the list."
      "Drop the legacy CSV importer, add tests for the due-date parser, and get the changelog ready for 0.5.0."
      "Rename Task to Todo across the project."
      "Anything else worth doing before the release?"
    )
    for n in 1 2 3 4; do
      prompt="${prompts[$((n - 1))]}"
      run_prompt "$chat" "$project" "$prompt" "$prompt [mock:changes:$n]"
      wait_round "$chat" "$n" "$([[ $n -lt 4 ]] && echo recorded)"
    done
  elif [[ -n "$DIFFS" ]]; then
    space=$(new_id) chat=$(new_id)
    create_space "$space" "$PWD" '"name":"Diff fence demo","gitDetected":true'
    create_chat "$chat" "$space" "Diff fences"
    run_prompt "$chat" "$PWD" "Show me the fingerprint changes as diffs." \
      "Show me the fingerprint changes as diffs. [mock:diffs]"
  elif [[ -n "$WORK" ]]; then
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
