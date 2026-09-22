#!/usr/bin/env bash
# exec a scoped engine; no production processes are stopped or data copied.
set -euo pipefail
cd "$(dirname "$0")/.."
mode="${1:-local}"
case "$mode" in local|dev) ;; *) echo 'Usage: dev-engine.sh [local|dev]' >&2; exit 2;; esac
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
cargo build --locked -p cypher --features development
unset CYPHER_EDGE_TOKEN CYPHER_EDGE_URL CYPHER_WORKOS_CLIENT_ID CYPHER_ENGINE_DATA_DIR CYPHER_ORG_ID CYPHER_USER_ID CYPHER_DEV_ACCESS_TOKEN
export CYPHER_DATA_DIR="$HOME/.cypher-development/$mode-engine"
umask 077
mkdir -p "$CYPHER_DATA_DIR"
if [[ "$mode" == dev ]]; then
  # The hosted development Worker was retired on 2026-09-22; the development
  # Edge is now a local `wrangler dev` (cd edge && npm run dev) unless an
  # endpoint is named explicitly:
  #   CYPHER_DEV_EDGE_URL=https://edge-dev.example.com scripts/dev-engine.sh dev
  # A caller-supplied value wins over the private file.
  dev_edge_override="${CYPHER_DEV_EDGE_URL:-}"
  set -a; source "$HOME/Documents/cypher-development.env"; set +a
  if [[ -n "$dev_edge_override" ]]; then
    export CYPHER_DEV_EDGE_URL="$dev_edge_override"
  elif [[ "${CYPHER_DEV_EDGE_URL:-}" == *cypher-edge-development* ]]; then
    # Stale value in the private file, pointing at the retired Worker. Fall
    # through to the binary's local default rather than dialling a dead host.
    unset CYPHER_DEV_EDGE_URL
  fi
  # The binary reads these names directly (cypher_env::var prefixes CYPHER_).
  : "${CYPHER_DEV_ACCESS_TOKEN:?Missing CYPHER_DEV_ACCESS_TOKEN}"
  # The retired hosted Worker ran AUTH_MODE=dev-locked, where the private
  # 64-hex secret *was* the credential and the Worker answered with the fixed
  # identity dev-user/dev-org. A local `wrangler dev` runs AUTH_MODE=dev, where
  # the bearer is the user id and only a `user@org` form carries an org claim,
  # so the bare secret authenticates as a user with no org and every
  # /registry/dev-org/* route answers 403. Against a loopback Edge, send the
  # identity that Worker used to grant: it maps to orgs/dev-org/dev-user, the
  # directory already on disk. The private secret still goes to a real remote
  # staging endpoint unchanged.
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
