# iOS / TestFlight preparation

The App Store Connect listing is **Cypher Remote**. The installed display
name remains **Cypher**. Bundle ID: `ai.mvp-lab.cypher.ios`; team:
`999875MHT4` (Changrui Chen).

TestFlight/App Store distribution does **not** require a registered iPhone,
UDID, or Development provisioning profile. Do not route cloud-Mac
distribution through a device-development signing setup.

## Current validation boundary

- Release archiving for `generic/platform=iOS` and subsequent automatic
  App Store distribution export have both succeeded on Xcode 26.6 without
  a connected/registered iPhone.
- The exported IPA passed `codesign --verify --deep --strict`; it carries
  the expected team/bundle ID, an iOS Team Store provisioning profile with
  no device list, `beta-reports-active = true`, no debug entitlement, and the
  required-reason privacy manifest.
- The next release is prepared as `0.1.4` / build `1`; the previous
  `0.1.3 (1)` upload remains unchanged. Notification configuration and real-device
  acceptance gate the new release; see [notifications.md](notifications.md).
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

# Build for iPhoneOS, not the simulator. Signing is handled at distribution.
xcodebuild -project apps/ios/Cypher.xcodeproj -scheme Cypher \
  -configuration Release -destination 'generic/platform=iOS' \
  -archivePath "$OUT/Cypher.xcarchive" \
  -disableAutomaticPackageResolution -onlyUsePackageVersionsFromResolvedFile \
  CODE_SIGNING_ALLOWED=NO archive

# Requires the approved team/account and distribution-signing permissions.
# This may create/download Apple-managed signing assets; it does NOT upload.
xcodebuild -exportArchive -archivePath "$OUT/Cypher.xcarchive" \
  -exportOptionsPlist apps/ios/ExportOptions-TestFlight.plist \
  -exportPath "$OUT/export" -allowProvisioningUpdates
```

The unsigned-archive/export path above has been verified with automatic
distribution signing. If a different machine requires a local distribution
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
No E2EE claim is made. `ITSAppUsesNonExemptEncryption` remains unset pending
the export-compliance review; do not fill legal answers blindly.

## Publication boundary

Uploading a build, enabling tester groups/public links, accepting agreements,
and submitting for review are separate actions. Confirm them before execution.
Never put Apple passwords, 2FA codes, certificate private keys, or API tokens
in chat, argv, logs, or git.
