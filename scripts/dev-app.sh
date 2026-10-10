#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mode="${1:-local}"
case "$mode" in local|dev) ;; *) echo 'Usage: dev-app.sh [local|dev]' >&2; exit 2;; esac
# CYPHER_DEV_INSTANCE=<name> pairs with the same-named dev-engine.sh instance.
instance="${CYPHER_DEV_INSTANCE:-}"
[[ "$instance" =~ ^[A-Za-z0-9._-]*$ ]] || { echo 'CYPHER_DEV_INSTANCE: letters, digits, . _ - only' >&2; exit 2; }
unset CYPHER_DEV_INSTANCE
unset CYPHER_EDGE_TOKEN CYPHER_EDGE_URL CYPHER_WORKOS_CLIENT_ID CYPHER_DEV_ACCESS_TOKEN
unset CYPHER_DEV_PREVIEW_PUBLISH_TOKEN CYPHER_DEV_STREAM_PREVIEW
export CYPHER_DATA_DIR="$HOME/.cypher-development/$mode-ui${instance:+-$instance}"
export CYPHER_ENGINE_DATA_DIR="$HOME/.cypher-development/$mode-engine${instance:+-$instance}"
export CYPHER_PROFILE=local
# The UI attaches to the existing engine by private IPC. Never embed a new
# engine by accident or pass the cloud secret into the UI environment.
test -d "$CYPHER_ENGINE_DATA_DIR" || { echo 'Start dev-engine.sh first' >&2; exit 1; }
CYPHER_DATA_DIR="$CYPHER_ENGINE_DATA_DIR" target/debug/cypher status --verbose | grep -q 'IPC:      listening' \
  || { echo "No engine listening for $CYPHER_ENGINE_DATA_DIR; start dev-engine.sh first" >&2; exit 1; }
umask 077
mkdir -p "$CYPHER_DATA_DIR"
exec target/debug/cypher
