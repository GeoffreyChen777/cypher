#!/usr/bin/env bash
# Single entry point for the checks CI runs; each CI step calls one stage.
#
#   bash scripts/check.sh <stage>...
#
# Stages:
#   fmt          cargo fmt --check
#   lint         crate layering, Python byte-compile, shell syntax and ShellCheck,
#                node --check, and clippy over the headless crate set
#   rust         rust-test and rust-build
#   rust-test    the headless crate tests
#   rust-build   the development-profile suites, the headless build and its CLI
#                tests
#   edge         apps/edge typecheck, unit and workerd tests
#   scripts      script, release and installer tests; documentation links
#   runtime      the Pi runtime suites that need no staged runtime
#   workflows    actionlint and the workflow policy (downloads actionlint)
#   macos        macOS only: macos-lint and macos-test
#   macos-lint   workspace clippy (gpui included)
#   macos-test   icon, UI tests, and the Rust/Swift preview vectors
#   ios          swift-format lint and the iOS unit tests on a simulator, with the
#                Xcode pinned in .xcode-version
#   all          fmt lint rust edge scripts runtime workflows, plus macos on macOS
#
# CI runs rust-test, rust-build, macos-lint and macos-test as separate jobs:
# each builds the workspace with a different feature set, so side by side they
# finish sooner than one after another.
#
# On a Mac, scripts/check-linux.sh runs the Linux-dependent stages in Docker.
#
# CARGO, PYTHON and NODE override the tools used (for example a wrapper that
# serializes heavy builds).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CARGO="${CARGO:-cargo}"
PYTHON="${PYTHON:-python3}"
NODE="${NODE:-node}"

# Crates that build without gpui; the Linux CI jobs lint and test exactly this set.
HEADLESS=(-p cypher -p cypher-env -p cypher-proto -p cypher-doc -p cypher-sync
  -p cypher-net -p cypher-harness -p cypher-engine -p cypher-rpc -p cypher-update -p cypher-syntax
  -p cypher-secrets -p cypher-trust)

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

# Every headless crate in one invocation, so each is compiled once.
# cypher-sync's registry transport suites need its in-process server.
stage_rust_test() {
  run "$CARGO" test --locked --no-default-features "${HEADLESS[@]}" \
    --features cypher-sync/mock-server
}

stage_rust_build() {
  # Both development suites select the same packages and features, so the
  # second reuses the first's build.
  local development=(--locked --no-default-features -p cypher -p cypher-engine
    --features cypher/development)
  run "$CARGO" test "${development[@]}" --test development_profile
  run "$CARGO" test "${development[@]}" --lib development_auth_tests
  run "$CARGO" build --locked --no-default-features -p cypher
  run "$PYTHON" scripts/ci/check-production-profile.py "$ROOT/target/debug/cypher"
  run "$PYTHON" scripts/tests/test-linux-cli.py --binary target/debug/cypher
}

stage_rust() {
  stage_rust_test
  stage_rust_build
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
  # node --test only: the pure suites import their sources directly (Node
  # type stripping) and need neither an install nor a staged runtime.
  run npm --prefix pi-runtime test
  # The workspace tools the engine embeds (crates/harness/src/pi).
  run "$NODE" --test crates/harness/src/pi/bot-tools.test.mjs
}

stage_workflows() {
  run bash scripts/ci/actionlint.sh
}

require_macos() {
  [[ "$(uname -s)" == Darwin ]] || { echo "the $1 stage needs a macOS host" >&2; return 1; }
}

stage_macos_lint() {
  require_macos macos-lint
  run "$CARGO" clippy --workspace --all-targets --locked -- -D warnings
  run "$CARGO" clippy --locked -p cypher-ui -p cypher --features cypher/dev-capture -- -D warnings
}

stage_macos_test() {
  require_macos macos-test
  run bash scripts/tests/test-macos-icon.sh
  # cypher-sync's library holds the Rust side of the preview vectors; testing
  # it with cypher-ui compiles their shared crates once.
  run "$CARGO" test --locked -p cypher-ui -p cypher-sync --lib
  local out
  out="$(mktemp -d)"
  run xcrun swiftc apps/ios/Cypher/Sync/ChatFrames.swift apps/ios/Cypher/Sync/StreamPreview.swift \
    apps/ios/Cypher/Sync/PreviewProjection.swift scripts/tests/stream-preview-vectors.swift \
    -o "$out/preview-vectors"
  run "$out/preview-vectors" protocol/vectors/stream-preview-v1.json \
    protocol/vectors/preview-reducer-v1.json
  run xcrun swiftc -module-cache-path "$out/module-cache" apps/ios/Cypher/Sync/DeviceTrust.swift \
    scripts/tests/device-trust-vectors.swift -o "$out/trust-vectors"
  run "$out/trust-vectors" protocol/vectors/device-trust-v1.json
  run xcrun swiftc -module-cache-path "$out/module-cache" apps/ios/Cypher/Sync/CommandSignature.swift \
    scripts/tests/command-signing-vectors.swift -o "$out/command-vectors"
  run "$out/command-vectors" protocol/vectors/command-signing-v1.json
  rm -rf "$out"
}

stage_macos() {
  stage_macos_lint
  stage_macos_test
}

stage_ios() {
  # The Xcode pinned in .xcode-version, as in CI: swift-format's output and
  # the compiler's verdicts differ between toolchain versions.
  local selected
  selected="$(bash scripts/ci/select-xcode.sh)" || return 1
  export DEVELOPER_DIR="${selected##*DEVELOPER_DIR=}"
  run xcrun swift-format lint --strict --recursive --parallel apps/ios/Cypher apps/ios/CypherTests \
    apps/ios/CypherUITests
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

[[ $# -gt 0 ]] || { awk 'NR > 1 && !/^#/ {exit} NR > 1' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
for stage in "$@"; do
  case "$stage" in
    fmt | lint | rust | rust-test | rust-build | edge | scripts | runtime | workflows | macos | \
      macos-lint | macos-test | ios | all)
      "stage_${stage//-/_}"
      ;;
    *) echo "unknown stage: $stage (run without arguments for the list)" >&2; exit 2 ;;
  esac
done
