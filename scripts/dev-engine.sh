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
  set -a; source "$HOME/Documents/cypher-development.env"; set +a
  # The private file uses explicit CYPHER_DEV_* names; the binary's config
  # loader intentionally reads the generic names below only inside this
  # script, so they never leak into the headed UI launcher.
  export EDGE_URL="${CYPHER_DEV_EDGE_URL:?Missing CYPHER_DEV_EDGE_URL}"
  export DEV_ACCESS_TOKEN="${CYPHER_DEV_ACCESS_TOKEN:?Missing CYPHER_DEV_ACCESS_TOKEN}"
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
