#!/usr/bin/env bash
# Separate optimized Dev bundle; simulator token injection uses environment,
# never CLI args, app resources, UserDefaults or a shared production Keychain.
set -euo pipefail
unset CYPHER_DEV_PREVIEW_PUBLISH_TOKEN SIMCTL_CHILD_CYPHER_DEV_PREVIEW_PUBLISH_TOKEN
cd "$(dirname "$0")/.."
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
sim="${CYPHER_DEV_SIMULATOR:?Set the dedicated development simulator UUID}"
env_file="${CYPHER_DEV_ENV_FILE:-$HOME/Documents/cypher-development.env}"
[[ -f "$env_file" ]] || { echo "Missing $env_file (set CYPHER_DEV_ENV_FILE to the private development env file)" >&2; exit 1; }
out="$HOME/.cypher-development/ios-build"
xcodebuild -project apps/ios/Cypher.xcodeproj -scheme CypherDev -configuration Development \
  -destination "platform=iOS Simulator,id=$sim" -derivedDataPath "$out" \
  -disableAutomaticPackageResolution -onlyUsePackageVersionsFromResolvedFile \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- build
xcrun simctl install "$sim" "$out/Build/Products/Development-iphonesimulator/Cypher.app"
# The app defaults to a local `wrangler dev` (cd apps/edge && npm run dev), which
# needs no secret. CYPHER_DEV_EDGE_URL names a staging Edge instead; a value
# from the caller wins over the private file, as in dev-engine.sh.
dev_edge_override="${CYPHER_DEV_EDGE_URL:-}"
set -a; source "$env_file"; set +a
unset CYPHER_DEV_PREVIEW_PUBLISH_TOKEN SIMCTL_CHILD_CYPHER_DEV_PREVIEW_PUBLISH_TOKEN
if [[ -n "$dev_edge_override" ]]; then
  CYPHER_DEV_EDGE_URL="$dev_edge_override"
elif [[ "${CYPHER_DEV_EDGE_URL:-}" == *cypher-edge-development* ]]; then
  CYPHER_DEV_EDGE_URL=""  # stale value naming the retired Worker
fi
export SIMCTL_CHILD_CYPHER_DEV_EDGE_URL="${CYPHER_DEV_EDGE_URL:-}"
export SIMCTL_CHILD_CYPHER_DEV_ACCESS_TOKEN="${CYPHER_DEV_ACCESS_TOKEN:-}"
xcrun simctl launch "$sim" ai.mvp-lab.cypher.ios.dev "$@"
