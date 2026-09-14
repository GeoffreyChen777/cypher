# iOS / TestFlight preparation

The App Store Connect listing is **Cypher Remote**. The installed display
name remains **Cypher**. Bundle ID: `ai.mvp-lab.cypher.ios`; team:
`999875MHT4` (Changrui Chen).

TestFlight/App Store distribution does **not** require a registered iPhone,
UDID, or Development provisioning profile. Do not route cloud-Mac
distribution through a device-development signing setup.

## Current release: 0.1.6 (6), 2026-09-14

- Companion build for desktop **0.3.11**, source commit `cf499ad` on the
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

## Local archive and App Store export

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
