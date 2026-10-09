#!/usr/bin/env bash
# exec a scoped engine; no production processes are stopped or data copied.
set -euo pipefail
cd "$(dirname "$0")/.."
mode="${1:-local}"
case "$mode" in local|dev) ;; *) echo 'Usage: dev-engine.sh [local|dev]' >&2; exit 2;; esac
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
cargo build --locked -p cypher --features development
unset CYPHER_EDGE_TOKEN CYPHER_EDGE_URL CYPHER_WORKOS_CLIENT_ID CYPHER_ENGINE_DATA_DIR CYPHER_ORG_ID CYPHER_USER_ID CYPHER_DEV_ACCESS_TOKEN
export CYPHER_DATA_DIR="$HOME/.cypher-development/$mode-engine"
umask 077
mkdir -p "$CYPHER_DATA_DIR"
if [[ "$mode" == dev ]]; then
  # The development Edge is a local `wrangler dev` (cd edge && npm run dev)
  # unless an endpoint is named explicitly:
  #   CYPHER_DEV_EDGE_URL=https://edge-dev.example.com scripts/dev-engine.sh dev
  # A caller-supplied value wins over the private file.
  dev_edge_override="${CYPHER_DEV_EDGE_URL:-}"
  env_file="${CYPHER_DEV_ENV_FILE:-$HOME/Documents/cypher-development.env}"
  [[ -f "$env_file" ]] || { echo "Missing $env_file (set CYPHER_DEV_ENV_FILE to the private development env file)" >&2; exit 1; }
  set -a; source "$env_file"; set +a
  if [[ -n "$dev_edge_override" ]]; then
    export CYPHER_DEV_EDGE_URL="$dev_edge_override"
  elif [[ "${CYPHER_DEV_EDGE_URL:-}" == *cypher-edge-development* ]]; then
    # Stale value in the private file, pointing at the retired Worker. Fall
    # through to the binary's local default rather than dialling a dead host.
    unset CYPHER_DEV_EDGE_URL
  fi
  # The binary reads these names directly (cypher_env::var prefixes CYPHER_).
  : "${CYPHER_DEV_ACCESS_TOKEN:?Missing CYPHER_DEV_ACCESS_TOKEN}"
  # A local `wrangler dev` runs AUTH_MODE=dev, where the bearer is the user id
  # and only a `user@org` form carries an org claim, so the bare secret would
  # get 403 from every /registry/dev-org/* route. Against a loopback Edge, send
  # dev-user@dev-org instead: it maps to the existing orgs/dev-org/dev-user
  # directory. The private secret still goes to a remote staging endpoint.
  if [[ -z "${CYPHER_DEV_EDGE_URL:-}" \
        || "${CYPHER_DEV_EDGE_URL}" =~ ^https?://(localhost|127\.0\.0\.1|\[::1\])(:|/|$) ]]; then
    export CYPHER_DEV_ACCESS_TOKEN=dev-user@dev-org
  fi
  export CYPHER_PROFILE=development
  # The dev Edge preview path is the default only when its independent,
  # private publisher credential is present. Never put this credential in git
  # or pass it to the headed UI process.
  if [[ -n "${CYPHER_DEV_PREVIEW_PUBLISH_TOKEN:-}" ]]; then
    export CYPHER_DEV_STREAM_PREVIEW=1
  fi
else
  export CYPHER_PROFILE=local
fi
export CYPHER_HARNESS="${CYPHER_HARNESS:-mock}"
export CYPHER_AUTO_UPDATE=0
exec target/debug/cypher headless
