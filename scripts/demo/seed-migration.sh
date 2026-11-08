#!/usr/bin/env bash
# Seed the session-migration demo into a running engine (mock sessions):
#
#   "Payments API"   four sessions to move — two settled, one archived, and
#                    "Rotate API keys", still working until you Stop it (a
#                    project move leaves it behind and says why)
#   "Payments v2"    empty: the obvious destination
#   "Docs site"      one session
#   "Build server"   an offline device with its own "payments-api" project:
#                    listed in the palette, but it can't be picked
#
# Right-click a session → Move to Project…, or a project card → Move Sessions
# to Project…
#
#   scripts/demo/seed-migration.sh <engine-data-dir> <projects-dir>
#
# scripts/dev-demo.sh --migration runs it against its own mock engine; it can
# also seed a running dev engine. The offline device needs a development
# build (Mutate seedDevice); without one it is skipped. Re-runs do nothing
# (marker: <engine-data-dir>/.migration-demo-seeded).
set -euo pipefail
cd "$(dirname "$0")/../.."

[[ $# -eq 2 ]] || { echo 'Usage: seed-migration.sh <engine-data-dir> <projects-dir>' >&2; exit 2; }
ENGINE_DIR="$1"
PROJECTS="$2"
if [[ -f "$ENGINE_DIR/.migration-demo-seeded" ]]; then
  echo "▸ migration demo already seeded in $ENGINE_DIR"
  exit 0
fi

[[ -x target/debug/examples/rpc_probe ]] || cargo build -p cypher-rpc --example rpc_probe -q
probe() { ./target/debug/examples/rpc_probe "$ENGINE_DIR" "$@"; }
new_id() { uuidgen | tr 'A-Z' 'a-z'; }
mutate() { probe Mutate "$1" >/dev/null; }
hours_ago_ms() { echo $(( ($(date +%s) - $1 * 3600) * 1000 )); }

DEV=$(probe EngineInfo '{}' | python3 -c 'import json,sys;print(json.load(sys.stdin)["deviceId"])')

create_space() { # id device path name
  mutate "{\"op\":\"createSpace\",\"spaceId\":\"$1\",\"deviceId\":\"$2\",\"path\":\"$3\",\"name\":\"$4\"}"
}
create_chat() { # id space title
  mutate "{\"op\":\"createChat\",\"chatId\":\"$1\",\"spaceId\":\"$2\",\"config\":{\"harness\":\"mock\",\"model\":\"mock-fable-5\",\"reasoning\":null,\"sandbox\":\"workspace-write\"}}"
  mutate "{\"op\":\"renameChat\",\"chatId\":\"$1\",\"title\":\"$3\"}"
}
run_prompt() { # chat cwd prompt [what the agent receives]
  probe QueueCommand "{\"chatId\":\"$1\",\"command\":{\"kind\":\"run\",\"messageId\":\"$(uuidgen)\",\"request\":{\"prompt\":\"$3\",\"model\":null,\"reasoning\":null,\"modelOptions\":{},\"cwd\":\"$2\",\"sandbox\":\"workspace-write\",\"autoApprove\":true,\"resume\":null}${4:+,\"agentPrompt\":\"$4\"}}}" >/dev/null
}
# Wait until `chat` has an answer settled, or (hold) has started one.
wait_answer() { # chat [hold]
  for _ in $(seq 1 120); do
    probe WatchDocMessages "{\"chatId\":\"$1\"}" --stream 1 2>/dev/null | python3 -c '
import json, sys
hold = sys.argv[1] == "hold"
answers = [e for e in json.load(sys.stdin).get("reset", []) if e["role"] == "assistant"]
sys.exit(0 if answers and (hold or answers[-1].get("status") == "complete") else 1)
' "${2:-}" && return 0
    sleep 0.25
  done
  echo "no answer in $1" >&2
  return 1
}
settled() { # space path title prompt age_hours
  local id
  id=$(new_id)
  create_chat "$id" "$1" "$3"
  run_prompt "$id" "$2" "$4"
  wait_answer "$id"
  mutate "{\"op\":\"setChatActivity\",\"chatId\":\"$id\",\"lastMessageAt\":$(hours_ago_ms "$5")}"
  echo "$id"
}

echo "▸ seeding the session-migration demo"
payments="$PROJECTS/payments-api" payments_v2="$PROJECTS/payments-v2" docs="$PROJECTS/docs-site"
mkdir -p "$payments" "$payments_v2" "$docs"
payments_space=$(new_id) v2_space=$(new_id) docs_space=$(new_id)
create_space "$payments_space" "$DEV" "$payments" "Payments API"
create_space "$v2_space" "$DEV" "$payments_v2" "Payments v2"
create_space "$docs_space" "$DEV" "$docs" "Docs site"

# Oldest first, so the sidebar reads newest on top.
settled "$docs_space" "$docs" "Fix broken links" \
  "Find the broken links in the docs and fix them." 30 >/dev/null
archived=$(settled "$payments_space" "$payments" "Audit rate limits" \
  "Audit the rate limits on the public endpoints." 26)
mutate "{\"op\":\"setChatArchived\",\"chatId\":\"$archived\",\"archived\":true}"
settled "$payments_space" "$payments" "Add idempotency keys" \
  "Make POST /charges idempotent with an Idempotency-Key header." 5 >/dev/null
settled "$payments_space" "$payments" "Retry failed webhooks" \
  "Failed webhook deliveries are never retried. Add retries with backoff." 1 >/dev/null

# Still working: the run opens a command that never finishes until Stop.
working=$(new_id)
create_chat "$working" "$payments_space" "Rotate API keys"
run_prompt "$working" "$payments" "Rotate the API signing keys without downtime." \
  "Rotate the API signing keys without downtime. [mock:hold]"
wait_answer "$working" hold

# An offline peer with its own project. Development builds only.
server="demo-build-server"
if probe Mutate "{\"op\":\"seedDevice\",\"deviceId\":\"$server\",\"name\":\"Build server\",\"platform\":\"linux\",\"lastSeenAt\":$(hours_ago_ms 3)}" >/dev/null 2>&1; then
  create_space "$(new_id)" "$server" "/srv/payments-api" "payments-api"
else
  echo "  (offline device skipped: the engine isn't a development build)"
fi

touch "$ENGINE_DIR/.migration-demo-seeded"
echo "▸ seeded: right-click a session or a project card to move sessions"
