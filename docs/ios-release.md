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

Export compliance is declared in `Cypher/Info.plist` as
`ITSAppUsesNonExemptEncryption = NO`: the app uses only the encryption iOS
provides (TLS through `URLSession`, plus CryptoKit SHA-256 and `SecRandom` for
the sign-in PKCE verifier), so App Store Connect doesn't ask per build. Adding
encryption of the app's own (for example end-to-end encrypted chats) means
revisiting that answer.

iOS has its own version series, independent of the desktop app and the Runtime.
The required secrets are listed in [ci-cd.md](ci-cd.md#ios--testflight-secrets);
the only ones not already configured are the Apple **Distribution** identity and
its provisioning profile.

A `workflow_dispatch` run builds and verifies but never uploads.

To diagnose a failing CI archive, run the same `xcodebuild archive` and
`-exportArchive` steps as `.github/workflows/ios.yml`. A local export is not a
release and must not be uploaded by hand; inspect it with
`scripts/ci/ios-verify.py`, never by the mere existence of an archive or IPA.

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
No E2EE claim is made; see the export-compliance note above for the
`ITSAppUsesNonExemptEncryption` declaration.

## Publication boundary

Uploading a build, enabling tester groups/public links, accepting agreements,
and submitting for review are separate actions. Confirm them before execution.
Never put Apple passwords, 2FA codes, certificate private keys, or API tokens
in chat, argv, logs, or git.
