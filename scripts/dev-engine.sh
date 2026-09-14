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
  export CYPHER_PROFILE=development
else
  export CYPHER_PROFILE=local
fi
export CYPHER_HARNESS="${CYPHER_HARNESS:-mock}"
export CYPHER_AUTO_UPDATE=0
exec target/debug/cypher headless
