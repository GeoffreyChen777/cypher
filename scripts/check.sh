#!/usr/bin/env bash
# Single entry point for the checks CI runs; each CI step calls one stage.
#
#   bash scripts/check.sh <stage>...
#
# Stages:
#   fmt        cargo fmt --check
#   lint       crate layering, Python byte-compile, shell syntax and ShellCheck,
#              node --check, and clippy over the headless crate set
#   rust       the headless crate tests, the headless build and its CLI tests
#   edge       apps/edge typecheck, unit and workerd tests
#   scripts    script, release and installer tests; documentation links
#   runtime    the Pi runtime suites that need no staged runtime
#   workflows  actionlint and the workflow policy (downloads actionlint)
#   macos      macOS only: icon, workspace clippy (gpui included), UI tests, and the
#              Rust/Swift preview vectors
#   ios        the iOS unit tests on a simulator (needs Xcode 27)
#   all        fmt lint rust edge scripts runtime workflows, plus macos on macOS
#
# CARGO, PYTHON and NODE override the tools used (for example a wrapper that
# serializes heavy builds).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CARGO="${CARGO:-cargo}"
PYTHON="${PYTHON:-python3}"
NODE="${NODE:-node}"

# Crates that build without gpui; the Linux CI job checks exactly this set.
HEADLESS=(-p cypher -p cypher-env -p cypher-proto -p cypher-doc -p cypher-sync
  -p cypher-harness -p cypher-engine -p cypher-rpc -p cypher-update -p cypher-syntax)

run() {
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then echo "::group::$*"; else echo "+ $*"; fi
  "$@"
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then echo "::endgroup::"; fi
}

# each <pathspec> <command...>: run the command once per tracked matching file.
each() {
  local spec="$1"
  shift
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then echo "::group::$* <$spec>"; else echo "+ $* <$spec>"; fi
  git ls-files -z -- "$spec" | xargs -0 -n 1 "$@"
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then echo "::endgroup::"; fi
}

stage_fmt() {
  run "$CARGO" fmt --all --check
}

stage_lint() {
  run "$PYTHON" scripts/ci/check-crate-layers.py
  run "$PYTHON" -W error -m compileall -q scripts
  each 'scripts/*.sh' bash -n
  run sh -n apps/edge/src/install.sh
  each '*.mjs' "$NODE" --check
  run bash scripts/ci/shellcheck.sh
  run "$CARGO" clippy --locked --no-default-features "${HEADLESS[@]}" --all-targets -- -D warnings
}

stage_rust() {
  run "$CARGO" test --locked --no-default-features -p cypher -p cypher-env -p cypher-proto \
    -p cypher-doc -p cypher-syntax -p cypher-harness -p cypher-engine -p cypher-update -p cypher-rpc
  # The registry transport suites need the in-process server, so they run
  # nowhere else: without the feature their targets don't even build.
  run "$CARGO" test --locked -p cypher-sync --features mock-server \
    --lib --test registry_client --test registry_transport
  run "$CARGO" test --locked --no-default-features -p cypher --features development \
    --test development_profile
  run "$CARGO" test --locked -p cypher-engine --features development --lib development_auth_tests
  run "$CARGO" build --locked --no-default-features -p cypher
  run "$PYTHON" scripts/ci/check-production-profile.py "$ROOT/target/debug/cypher"
  run "$PYTHON" scripts/tests/test-linux-cli.py --binary target/debug/cypher
}

stage_edge() {
  [[ -d apps/edge/node_modules ]] || run npm ci --prefix apps/edge
  run npm --prefix apps/edge run typecheck
  run npm --prefix apps/edge test
}

stage_scripts() {
  run "$PYTHON" -m unittest discover -s scripts/ci -p 'test_*.py'
  run "$PYTHON" scripts/ci/check-production-profile.py
  run "$PYTHON" scripts/tests/test-linux-cli.py
  run "$NODE" --test scripts/tests/test-landing.mjs
  run bash scripts/tests/test-check-linux-abi.sh
  run "$PYTHON" scripts/tests/check-doc-links.py
}

stage_runtime() {
  run "$NODE" --test pi-runtime/provider-service.test.mjs
  run "$NODE" --test pi-runtime/patches/pi-agent-squad-cypher-host/cypher-host.test.mjs
  # The extension suites import their .ts sources directly (Node type
  # stripping) and need no staged runtime.
  run "$NODE" --test pi-runtime/extensions/cypher-translation.test.mjs \
    pi-runtime/extensions/cypher-fast-mode.test.mjs pi-runtime/extensions/cypher-codemode.test.mjs
}

stage_workflows() {
  run bash scripts/ci/actionlint.sh
}

stage_macos() {
  [[ "$(uname -s)" == Darwin ]] || { echo "the macos stage needs a macOS host" >&2; return 1; }
  run bash scripts/tests/test-macos-icon.sh
  run "$CARGO" clippy --workspace --all-targets --locked -- -D warnings
  run "$CARGO" clippy --locked -p cypher-ui -p cypher --features cypher/dev-capture -- -D warnings
  run "$CARGO" test --locked -p cypher-ui --lib
  run "$CARGO" test --locked -p cypher-sync --lib preview
  local out
  out="$(mktemp -d)"
  run xcrun swiftc apps/ios/Cypher/Sync/ChatFrames.swift apps/ios/Cypher/Sync/StreamPreview.swift \
    apps/ios/Cypher/Sync/PreviewProjection.swift scripts/tests/stream-preview-vectors.swift \
    -o "$out/preview-vectors"
  run "$out/preview-vectors" crates/sync/tests/fixtures/stream-preview-v1.json \
    crates/sync/tests/fixtures/preview-reducer-v1.json
  rm -rf "$out"
}

stage_ios() {
  run bash scripts/ci/ios-unit-tests.sh
}

stage_all() {
  stage_fmt
  stage_lint
  stage_rust
  stage_edge
  stage_scripts
  stage_runtime
  stage_workflows
  if [[ "$(uname -s)" == Darwin ]]; then stage_macos; fi
}

[[ $# -gt 0 ]] || { sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
for stage in "$@"; do
  case "$stage" in
    fmt | lint | rust | edge | scripts | runtime | workflows | macos | ios | all) "stage_$stage" ;;
    *) echo "unknown stage: $stage (run without arguments for the list)" >&2; exit 2 ;;
  esac
done
