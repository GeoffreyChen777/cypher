#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.local/node-v24.19.0/bin:$PATH"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
export DYLD_FALLBACK_LIBRARY_PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/lib${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"
cargo test --locked -p cypher-sync --lib preview
(cd edge && npm run typecheck && npx vitest run src/stream-preview.test.ts)
out=$(mktemp -d /tmp/cypher-preview-tests.XXXXXX)
trap 'rm -rf "$out"' EXIT
xcrun swiftc apps/ios/Cypher/Sync/ChatFrames.swift apps/ios/Cypher/Sync/StreamPreview.swift apps/ios/Cypher/Sync/PreviewProjection.swift \
  scripts/tests/stream-preview-vectors.swift -o "$out/preview-vectors"
"$out/preview-vectors" edge/src/fixtures/stream-preview-v1.json edge/src/fixtures/preview-reducer-v1.json
