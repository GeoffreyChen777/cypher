#!/usr/bin/env bash
# Separate optimized Dev bundle; simulator token injection uses environment,
# never CLI args, app resources, UserDefaults or a shared production Keychain.
set -euo pipefail
unset CYPHER_DEV_PREVIEW_PUBLISH_TOKEN SIMCTL_CHILD_CYPHER_DEV_PREVIEW_PUBLISH_TOKEN
cd "$(dirname "$0")/.."
export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
sim="${CYPHER_DEV_SIMULATOR:?Set the dedicated development simulator UUID}"
out="$HOME/.cypher-development/ios-build"
xcodebuild -project apps/ios/Cypher.xcodeproj -scheme CypherDev -configuration Development \
  -destination "platform=iOS Simulator,id=$sim" -derivedDataPath "$out" \
  -disableAutomaticPackageResolution -onlyUsePackageVersionsFromResolvedFile \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- build
xcrun simctl install "$sim" "$out/Build/Products/Development-iphonesimulator/Cypher.app"
set -a; source "$HOME/Documents/cypher-development.env"; set +a
unset CYPHER_DEV_PREVIEW_PUBLISH_TOKEN SIMCTL_CHILD_CYPHER_DEV_PREVIEW_PUBLISH_TOKEN
export SIMCTL_CHILD_CYPHER_DEV_ACCESS_TOKEN="$CYPHER_DEV_ACCESS_TOKEN"
xcrun simctl launch "$sim" ai.mvp-lab.cypher.ios.dev "$@"
