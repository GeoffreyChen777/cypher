#!/usr/bin/env bash
# Run the iOS unit tests (CypherTests) on an iPhone simulator.
#
#   bash scripts/ci/ios-unit-tests.sh
#
# IOS_SIMULATOR=<name or UDID> picks the simulator; otherwise an available
# iPhone is used, preferring one that is shut down. Keychain tests need the
# ad-hoc signed test host (apps/ios/README.md). Build products go to
# target/ios-unit-tests.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
simulator="${IOS_SIMULATOR:-}"
if [[ -z "$simulator" ]]; then
  simulator="$(xcrun simctl list devices available --json | python3 -c '
import json, sys
devices = json.load(sys.stdin)["devices"]
iphones = [device for runtime in sorted(devices, reverse=True) if ".iOS-" in runtime
           for device in devices[runtime] if device["name"].startswith("iPhone")]
# Prefer a shut-down simulator so a booted one in use stays untouched.
iphones.sort(key=lambda device: device["state"] != "Shutdown")
if not iphones:
    sys.exit("no available iPhone simulator")
print(iphones[0]["udid"])
')"
fi
if [[ "$simulator" =~ ^[0-9A-Fa-f-]{36}$ ]]; then
  destination="platform=iOS Simulator,id=$simulator"
else
  destination="platform=iOS Simulator,name=$simulator"
fi
echo "destination: $destination"
xcodebuild -project apps/ios/Cypher.xcodeproj -scheme Cypher \
  -destination "$destination" -derivedDataPath target/ios-unit-tests \
  -disableAutomaticPackageResolution -onlyUsePackageVersionsFromResolvedFile \
  -only-testing:CypherTests \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- \
  test
