# iOS / TestFlight preparation

The App Store Connect listing is **Cypher Remote**. The installed display
name remains **Cypher**. Bundle ID: `ai.mvp-lab.cypher.ios`; team:
`999875MHT4` (Changrui Chen).

TestFlight/App Store distribution does **not** require a registered iPhone,
UDID, or Development provisioning profile. Do not route cloud-Mac
distribution through a device-development signing setup.

## How a build ships

Releases run in GitHub Actions (`.github/workflows/ios.yml`). There is no local
archive/upload path any more: a developer Mac is not expected to hold the
distribution private key, and one did not.

1. Set the iOS version and build in the Xcode project. They must agree across
   every build configuration; `release.py ios-context` refuses otherwise.
2. Push `cypher-ios-v<version>-b<build>`. The tag must equal
   `MARKETING_VERSION` and `CURRENT_PROJECT_VERSION`; a mismatched build number
   would upload a package Apple rejects as a duplicate.
3. CI imports the signing identity into an ephemeral keychain, archives,
   exports, verifies the package (`scripts/ci/ios-verify.py`) and uploads to
   TestFlight.

iOS has its own version series, independent of the desktop app and the Runtime.
The required secrets are listed in [ci-cd.md](ci-cd.md#ios--testflight-secrets);
the only ones not already configured are the Apple **Distribution** identity and
its provisioning profile.

A `workflow_dispatch` run builds and verifies but never uploads.

## Release: 0.2.0 (19), 2026-09-26

- Covers the iOS commits since build 18: `6af3ef7` (faded edges on the
  composer chip row), `1966d85` (notification settings surface Badges turned
  off), `da22453` (the shown session is marked seen as activity lands, so
  badges clear on every device) and `b22e0ca` (ASCII wordmark boot splash).
- Release build for `generic/platform=iOS Simulator` succeeded locally with
  `CODE_SIGNING_ALLOWED=NO`; `release.py ios-context` accepts the tag.
- Upload is left to `.github/workflows/ios.yml` via the
  `cypher-ios-v0.2.0-b19` tag. Export compliance, tester groups, public links
  and review submission remain separate explicit actions.

## Release: 0.2.0 (18), 2026-09-25

- Build 17 never reached TestFlight: CI's Xcode 26.3 gave up type-checking
  `HomeView.body` ("unable to type-check this expression in reasonable
  time"), which Xcode 26.6 locally compiled in 550 ms. Build 18 splits that
  body, and the similarly long `SessionView` and `NewSessionView` bodies, into
  separately type-checked layers; each now checks in under 150 ms.
- Same content as build 17 below. A Release device build and the 208
  `CypherTests` pass locally; `release.py ios-context` accepts the tag.
- Upload is left to `.github/workflows/ios.yml` via the
  `cypher-ios-v0.2.0-b18` tag. Export compliance, tester groups, public links
  and review submission remain separate explicit actions.

## Release: 0.2.0 (17), 2026-09-25

- Covers the iOS commits since build 16: `88bc3a5` (Home tab flicker fix and
  simpler session rows), `6d9d11d` (no coding ligatures in code text),
  `9ff524e` (turn scrubber on the transcript's trailing edge) and `2224cf1`
  (six desktop session features brought to iOS). `a6d2d80` is a demo-only
  flag.
- Release build for `generic/platform=iOS Simulator` succeeded locally with
  `CODE_SIGNING_ALLOWED=NO`; `release.py ios-context` accepts the tag.
- Tagged `cypher-ios-v0.2.0-b17`; the archive failed on CI (see build 18).

## Release: 0.2.0 (16), 2026-09-24

- Covers the iOS commits since build 15: `3bc0672` (attach any file type),
  `b44bc1e` (native inset-grouped project and session lists) and `fbfd1b6`
  (no multi-second loader when opening a session).
- Release build for `generic/platform=iOS Simulator` succeeded locally with
  `CODE_SIGNING_ALLOWED=NO`; `release.py ios-context` accepts the tag.
- Upload is left to `.github/workflows/ios.yml` via the
  `cypher-ios-v0.2.0-b16` tag. Export compliance, tester groups, public links
  and review submission remain separate explicit actions.

## Release: 0.2.0 (14), 2026-09-19

- Fixes the notification-tap crash reported twice through TestFlight feedback
  by the same tester: `betaFeedbackCrashSubmissions` on 2026-09-10 (0.1.5 b2)
  and 2026-09-19 (0.2.0 b13), both `EXC_CRASH (SIGABRT)` from an
  `NSAssertionHandler` failure in `-[UIApplication
  _performBlockAfterCATransactionCommitSynchronizes:]`, frame 6
  `@objc closure #1 in PushAppDelegate.userNotificationCenter(_:didReceive:)`.
- Cause: both `UNUserNotificationCenterDelegate` methods were `nonisolated`
  `async`, so the bridged ObjC thunk invoked UIKit's completion handler from
  the cooperative pool. The early `guard ... else { return }` returned off-main
  too, so every tap crashed, not only ones carrying a parsable payload.
- Fix: both methods are `@MainActor`, with `@preconcurrency` on the delegate
  conformance. Because `NotificationController` is `@MainActor`, the now-direct
  calls only compile while the isolation holds — a revert breaks the build.
- `CypherTests/PushDelegateIsolationTests.swift` drives the real ObjC selectors
  from a background thread and asserts the completion handler returns on the
  main thread. Verified to fail against the pre-fix code (`Optional(false)`)
  and pass after. 186 Development simulator unit tests pass, 1 skipped.
- No live tap test on device or simulator: the dev edge reports
  `notifications/settings -> available:false`, so the Dev app cannot request
  iOS notification authorization and `simctl push` has nothing to display.
- Upload is left to `.github/workflows/ios.yml` via the
  `cypher-ios-v0.2.0-b14` tag. Export compliance, tester groups, public links
  and review submission remain separate explicit actions.

## Attempted build: 0.2.0 (11), 2026-09-18

- Covers the three model-picker commits that landed after build 10:
  `29c77e4` (group models by provider), `39a9197` (`-mock-providers` dev flag),
  `54ac505` (center loading/empty states).
- Build number bumped to 11 in `project.pbxproj` and `Development.xcconfig`.
- Release build for `generic/platform=iOS Simulator` **succeeded** with
  `CODE_SIGNING_ALLOWED=NO`, including `builtin-validationUtility
  -validate-for-store`. Product Info.plist reports `ai.mvp-lab.cypher.ios`,
  0.2.0, build 11.
- **Archive failed; nothing was uploaded.** This Mac has no distribution
  signing identity: `security find-identity -p codesigning` returns 0 valid
  identities across every keychain in the search list, and the archive stops
  with `No signing certificate "iOS Distribution" found: ... matching team ID
  "999875MHT4" with a private key`.
- The two App Store provisioning profiles are present and valid to
  2027-09-08; only the certificate + private key are missing. No App Store
  Connect API key (`AuthKey_*.p8`) or fastlane config exists here either, so
  the upload step has no credentials regardless of signing.
- Note: Xcode 27.0 is installed at `/Applications/Xcode.app`, but
  `xcode-select -p` points at `/Library/Developer/CommandLineTools`; builds
  require `DEVELOPER_DIR` (as the runbook already sets).

## Prepared build: 0.2.0 (10), 2026-09-17

- Companion to the iOS notification-navigation fix and composer hardening
  (`b6a751e`): a tap now opens its session from any screen and never replays
  on the way back mid-pop; the expanded composer clips its editor and tints
  its glass.
- 181 Debug simulator unit tests passed (ad-hoc signed; the two keychain
  fixtures need signing). Distribution archives do not use that override.
- Signed archive/export verified: arm64, `ai.mvp-lab.cypher.ios`, version
  0.2.0/build 10, production APNs, `get-task-allow=false`,
  `beta-reports-active=true`, strict deep signature.
- **Not uploaded.** No export-compliance declaration, tester/group change or
  review submission was made.
- Evidence: `/tmp/cypher-ios-distribution.uPQ5mL/` (archive, export, unpacked payload, logs).

## Prepared build: 0.2.0 (9), 2026-09-16

- Companion to the iOS notification-tap crash fix (`1dbe430`).
- Signed archive/export verified: arm64, `ai.mvp-lab.cypher.ios`, version 0.2.0/build 9, production APNs, `get-task-allow=false`, strict deep signature.
- App Store Connect build `fd9e04ac-fdba-48bc-9b6b-49d185085b4d` is **VALID**.
- `usesNonExemptEncryption=false` submitted for this build (same TLS/PKCE-only behavior as 0.2.0 (8)).
- Internal TestFlight state: **IN_BETA_TESTING**. External state: **READY_FOR_BETA_SUBMISSION**.
- No public-link, tester-group, or App Store review submission was made.
- Evidence: `/tmp/cypher-ios-distribution.xHGJaU/` and `/tmp/cypher-ios-0209-build.json`.

## Prepared build: 0.2.0 (8), 2026-09-15

- Companion build for desktop **0.3.12**, source commit `4d9ff35`.
- Signed archive/export verified: arm64, `ai.mvp-lab.cypher.ios`, version 0.2.0/build 8, production APNs, `get-task-allow=false`, strict deep signature.
- App Store Connect build `2a2f7437-0033-468e-b0d9-b07c08168f85` is **VALID**.
- `usesNonExemptEncryption=false` submitted for this build (same TLS/PKCE-only behavior as 0.1.6).
- Internal TestFlight state: **IN_BETA_TESTING**. External state: **READY_FOR_BETA_SUBMISSION**.
- No public-link, tester-group, or App Store review submission was made.
- Evidence: `/tmp/cypher-ios-020.gtUTbk/` and `/tmp/cypher-ios-020-build.json`.

## Prepared build: 0.1.6 (7), 2026-09-14

- Companion build for desktop **0.3.11**, source commit `b86f867`.
- Only the iOS build number changed from build 6; the desktop/engine cost
  optimizations are not compiled into this Swift client.
- Signed archive and local export succeeded. Exported IPA verified as arm64,
  `ai.mvp-lab.cypher.ios`, version 0.1.6/build 6, with production APNs,
  `get-task-allow=false`, and a valid strict deep signature.
- Upload and Apple processing verification are pending. No new export-compliance
  declaration, tester/group modification or review submission has been made.
- Evidence: `/tmp/cypher-ios-0311.PLiJcL/` and `/tmp/cypher-ios-0311-*.log`.

## Previous release: 0.1.6 (5), 2026-09-13

- Companion build for desktop **0.3.10**, source commit `cf499ad` on the
  restored main branch; application code is the pre-v3 baseline.
- **169 Release simulator tests passed** with `ENABLE_TESTABILITY=YES` for
  the test invocation only. Distribution archives do not use that override.
- An unsigned archive exported successfully but lost `aps-environment` in
  the signed IPA. It was **not uploaded**. Re-archiving with the existing
  Apple Distribution identity/profile preserved the production push entitlement.
- The final IPA passed strict deep signature verification, arm64/version/build,
  distribution profile, no-debug-entitlement, production APNs, icon and privacy
  manifest checks. Xcode reported **Upload succeeded**.
- App Store Connect build `93f7a3f1-4706-4cbe-8cd6-c7d43f98e7ca` is **VALID**.
  With explicit user confirmation, `usesNonExemptEncryption=false` was submitted
  for this build. Final internal state: **IN_BETA_TESTING**; external state:
  **READY_FOR_BETA_SUBMISSION**. No tester/group changes or review submission
  were performed; these states do not establish physical-device acceptance.
- Evidence is under `/tmp/cypher-restored-0310/` on the release Mac, with
  `/tmp/cypher-restored-0310-ios-upload.log` containing the upload receipt.

## Earlier validation

- Release archiving for `generic/platform=iOS` and subsequent automatic
  App Store distribution export have both succeeded on Xcode 26.6 without
  a connected/registered iPhone.
- The exported IPA passed `codesign --verify --deep --strict`; it carries
  the expected team/bundle ID, an iOS Team Store provisioning profile with
  no device list, `beta-reports-active = true`, no debug entitlement, and the
  required-reason privacy manifest.
- The next release is prepared as `0.1.5` / build `2`, replacing the composer's
  SwiftUI focus/notification bridge with an owned native editor and delegate.
  Build `0.1.5 (1)` was uploaded successfully, but the user reported the toolbar
  still failed to expand on focus on a physical phone. Build 2 passes Release
  simulator unit/real-tap tests; the affected phone still needs to confirm it.
  The previous
  `0.1.4 (3)` upload reported success on 2026-09-08; that is upload acceptance,
  not proof of Apple processing completion or real-device acceptance.
  The new release includes unread-session icon badges, quiet reconnect status
  and distinct steer bubbles; see [notifications.md](notifications.md).
- `ExportOptions-TestFlight.plist` requests **local export**, not upload.
  Version/build auto-management is disabled.
- With explicit approval, `0.1.3 (1)` was uploaded on 2026-09-07 using a
  separate, private upload-options file. Xcode reported **Upload succeeded**
  and **Uploaded package is processing**. No tester invitations, external
  testing/public links, or App Store review submissions were performed.
- Apple post-upload processing, export-compliance answers, App Store Connect
  privacy disclosures and real-device/remote execution acceptance still need
  separate verification. Upload acceptance is not TestFlight readiness.

## Local archive and App Store export (reference only)

**Publication no longer runs from a developer machine.** The commands below are
kept because they are what CI performs and are useful for diagnosing a failing
build locally; a local export is not a release and must not be uploaded by hand.

Run from the repository root. Keep output directories private and outside git.
Use a fresh archive/export path for each attempt.

```sh
export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
umask 077
OUT="$(mktemp -d /tmp/cypher-ios-distribution.XXXXXX)"

# Build for iPhoneOS with the verified, existing distribution profile.
# An unsigned archive can export without preserving the app's push entitlement.
xcodebuild -project apps/ios/Cypher.xcodeproj -scheme Cypher \
  -configuration Release -destination 'generic/platform=iOS' \
  -archivePath "$OUT/Cypher.xcarchive" \
  -disableAutomaticPackageResolution -onlyUsePackageVersionsFromResolvedFile \
  CODE_SIGN_STYLE=Manual 'CODE_SIGN_IDENTITY=Apple Distribution' \
  'PROVISIONING_PROFILE_SPECIFIER=Cypher iOS App Store Distribution 2026' archive

# Requires the approved team/account and distribution-signing permissions.
# This may create/download Apple-managed signing assets; it does NOT upload.
xcodebuild -exportArchive -archivePath "$OUT/Cypher.xcarchive" \
  -exportOptionsPlist apps/ios/ExportOptions-TestFlight.plist \
  -exportPath "$OUT/export" -allowProvisioningUpdates
```

The signed-archive/export path preserves the production APNs entitlement.
If a different machine requires a local distribution
certificate/profile, resolve that specific distribution requirement; do not
ask for a physical phone merely to satisfy a Development profile.

Before upload, inspect the exported IPA: bundle ID/version/build, iPhoneOS
architecture, production entitlements (`get-task-allow` false/absent),
distribution profile/team/expiry, privacy resources, icons, and signature.
Do not use the existence of an archive or IPA alone as proof of correctness.

## Privacy and compliance

`Cypher/PrivacyInfo.xcprivacy` declares the source-verified required reasons:

| Category | Reason | Evidence / use |
| --- | --- | --- |
| UserDefaults | `CA92.1` | `@AppStorage` in AppModel/Home/NewSession; app-private preferences |
| File timestamp | `C617.1` | `DocDisk.prune` reads local Application Support cache modification dates |

The locked Loro/Markdown dependencies currently provide no bundled privacy
manifests. The arm64 Release executable imports `_fstat` and `_clock_gettime`;
an import alone does not establish every API's purpose or prove that a
System Boot Time reason is required. Review dependencies as part of final
distribution validation and respond to any Apple validation findings.

This required-reason manifest does **not** declare “no data collected.”
Account identity, synced chats/projects and uploaded images cross the device
boundary. Review collection/linkage/purpose disclosures against the actual
Edge/WorkOS/remote-provider behavior before completing App Store Connect
privacy answers.

Networking uses Apple's URLSession/TLS; the PKCE helper uses CryptoKit SHA-256.
No E2EE claim is made. `ITSAppUsesNonExemptEncryption` remains unset in the
binary. The user approved the non-exempt-encryption declaration for build
0.1.6 (5) in App Store Connect; do not automatically extend that declaration
to future builds with different encryption behavior.

## Publication boundary

Uploading a build, enabling tester groups/public links, accepting agreements,
and submitting for review are separate actions. Confirm them before execution.
Never put Apple passwords, 2FA codes, certificate private keys, or API tokens
in chat, argv, logs, or git.
