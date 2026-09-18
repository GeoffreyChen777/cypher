# CI and release operations

## Workflows

- **`ci.yml`**: pull requests, pushes to `main`, and manual runs. Runs workflow
  lint, release/installer regressions, Edge typechecking and unit/workerd tests,
  Linux backend tests (including Engine integration tests), updater Clippy,
  formatting, macOS workspace compilation, and focused setup/runtime/MCP/Unix
  IPC UI regressions. No deployment credentials are available to these jobs.
- **`deploy.yml`**: pushes to `main` and main-only manual runs. Captures a fresh
  `main` SHA once, tests it, checks installer compatibility, then deploys all
  three workers from that same SHA. It deliberately does not use per-push path
  deltas: skipped/pending pushes and `grep -q`/SIGPIPE must not omit changes.
- **`linux.yml`** / **`macos.yml`**: pushes of `cypher-<platform>-v<version>-b<build>`
  build and publish **that platform alone**. Manual runs are **always
  build/validate-only**, even when a tag is selected. The version must match
  `[workspace.package].version` in `Cargo.toml`.
- **`ios.yml`**: pushes of `cypher-ios-v<version>-b<build>` build, verify and
  upload to TestFlight. The tag must match `MARKETING_VERSION` and
  `CURRENT_PROJECT_VERSION` in the Xcode project. Manual runs are
  build/validate-only.
- **`pi-runtime.yml`**: pushes of `pi-runtime-v<version>` publish the Runtime
  channel **alone** (see below). Manual runs are build/validate-only on the same
  terms. The tag must match `dist/pi-runtime/release.json`.

## Independent platform releases

Every platform builds, publishes and is checked for updates on its own. There is
no workflow that releases two platforms together, and no release path that runs
from a developer machine.

| Platform | Tag | Channel | Update check |
| --- | --- | --- | --- |
| Linux | `cypher-linux-v<version>-b<build>` | `releases/linux/*` | client polls `linux/manifest.json` |
| macOS | `cypher-macos-v<version>-b<build>` | `releases/macos/*` | client polls `macos/manifest.json` |
| iOS | `cypher-ios-v<version>-b<build>` | TestFlight | the App Store, not our channel |
| Runtime | `pi-runtime-v<version>` | `releases/runtimes/pi/*` | devices poll `runtimes/pi/manifest.json` |

**Versions.** Linux and macOS are the same Rust binary and share one version,
`[workspace.package].version`. They publish *independently in time*, each
carrying its own **build number**, so `0.3.8 (1)` on Linux and `0.3.8 (2)` on
macOS are a normal, expected state — as is Linux sitting on `0.3.8` while macOS
has moved to `0.3.9`. iOS and the Runtime keep entirely independent version
series, as they already did.

**Artifact names.** Build 1 keeps the historic name (`cypher-0.3.8-linux-x86_64.tar.gz`).
A re-cut changes the bytes, so it must change the immutable object name and
becomes `cypher-0.3.8-b2-linux-x86_64.tar.gz`. Clients read the exact name from
their channel manifest's `roles` map rather than rebuilding it from the version.

**Already-installed clients.** A client released before the split polls the
shared `manifest.json`/`latest.txt` and rebuilds the artifact name from the
version alone. Those pointers are therefore only ever moved to a version that
**every** desktop platform covers **at build 1**, where the historic names
exist. The moment either platform re-cuts a version, the shared channel holds
still rather than pointing at a superseded build; it resumes at the next fully
covered version. The shared channel is never rolled back. This is enforced in
`legacy_value()` and covered by tests.

**Publisher isolation.** A platform publish may write only its own
`<platform>/manifest.json`, `<platform>/latest.txt` and `<platform>/stem.txt`,
plus the shared pointers under the rule above. It never writes another
platform's channel and never republishes the Runtime. A Runtime publish moves
only `runtimes/pi/manifest.json`, and its `minimumCypherVersion` is checked
against the **oldest** published desktop channel, so a Runtime cannot reach a
platform still sitting behind it.

`<platform>/stem.txt` exists for the shell installer: `latest.txt` stays a bare
version so it can be compared numerically, and the artifact stem is published
separately rather than parsed out of JSON in POSIX `sh`.

The Rust toolchain is pinned in `rust-toolchain.toml`. Node is pinned to 24.19.0.
Worker deployments use Wrangler from `edge/package-lock.json`, not a floating
`npx wrangler@4`. Actions are pinned by commit.

`deploy`, `release.publish` and `pi-runtime.publish` share the
**`cypher-production`** concurrency group with `queue: max` and no cancellation
of running jobs. GitHub.com supports
up to 100 pending entries; dispatch order is not a version-order guarantee.
Publication therefore independently rejects version regressions.

Actionlint 1.7.12 does not yet recognize GitHub's `concurrency.queue`. The lint
wrapper exempts only that unknown-key diagnostic, and `workflow_policy.py`
strictly verifies the permitted queue/group/cancellation fields. Other syntax
and expression diagnostics remain fatal.

## Required credentials and honest deployment status

Configure **`CLOUDFLARE_API_TOKEN`** through repository **Settings → Secrets and
variables → Actions**. Restrict it to the configured account and
`letscypher.app` zone. It needs the permissions used by the existing workers:
Workers Scripts edit, Workers Routes edit, Workers R2 Storage edit, and the zone
read access required to resolve the configured zone routes.

The account ID remains the one in the committed Wrangler configurations. The
release bucket is `cypher-releases`; the publisher does not create buckets or
rename workers/Durable Objects.

Missing credentials fail the deployment/release preflight with **NOT DEPLOYED**
or **NOT PUBLISHED**. A green test workflow does not mean a successful deployment.
Neither scripts nor CI read local Wrangler credentials or upload local secrets.

GitHub provides `github.token` to the publish step with `contents: write`;
build/test jobs have only `contents: read`. Developer ID signing and Apple
notarization remain optional and use the existing `MACOS_CERT_*` and `AC_API_*`
secrets. Without these, the macOS package is ad-hoc signed, not notarized.

### iOS / TestFlight secrets

`ios.yml` needs an iOS **distribution** identity, which is a different
certificate from the macOS Developer ID one. Configure in **Settings → Secrets
and variables → Actions**:

| Secret | What it is |
| --- | --- |
| `IOS_DIST_CERT_P12` | base64 of the **Apple Distribution** `.p12` (certificate **and** private key) |
| `IOS_DIST_CERT_PASSWORD` | the password set when exporting that `.p12` |
| `IOS_PROVISIONING_PROFILE` | base64 of `Cypher iOS App Store Distribution 2026.mobileprovision` |

`AC_API_KEY_P8`, `AC_API_KEY_ID` and `AC_API_ISSUER_ID` are **already**
configured for macOS notarization and are reused for the TestFlight upload; the
same key authenticates both. No new App Store Connect key is needed.

The macOS `MACOS_CERT_P12` is a Developer ID Application certificate and cannot
sign an iOS App Store build. Export the Apple Distribution identity from
Keychain Access on the Mac that holds its private key:

```sh
# On the Mac that already has the key (Keychain Access > My Certificates >
# "Apple Distribution: ..." > Export as .p12), then:
base64 -i dist.p12 | pbcopy        # paste into IOS_DIST_CERT_P12
base64 -i profile.mobileprovision | pbcopy  # paste into IOS_PROVISIONING_PROFILE
```

The workflow imports the identity into an **ephemeral keychain** on the runner,
verifies it really is an Apple Distribution identity, archives, exports, and
runs `scripts/ci/ios-verify.py` over the resulting package before any upload:
bundle id, version/build against the tag, arm64, production APNs,
`get-task-allow` absent, `beta-reports-active`, a distribution profile with no
device list, the privacy manifest, and a strict deep signature. The existence of
an IPA is never taken as proof of correctness.

Upload is where `ios.yml` stops. Export-compliance answers, tester groups,
public links and App Store review submission remain separate, explicit actions.

## First deployment after the checksum migration

The current embedded installer requires standalone `.sha256` files. Do **not**
deploy it against an older channel that lacks those files.

1. Prepare a new application version; do not reuse the already released `0.2.2`.
2. Configure the deployment credential in GitHub Settings.
3. Publish a matching `cypher-linux-v<version>-b<build>` tag and wait for it to
   succeed. The installer resolves the Linux channel, so that is the one the
   deployment gate requires.
4. Run `deploy` on `main` again.

The deployment gate resolves the channel exactly as `install.sh` does: it reads
`linux/manifest.json`, `linux/latest.txt` and `linux/stem.txt` when the Linux
channel exists, and falls back to the shared `manifest.json`/`latest.txt` before
the first per-platform release. It requires them to agree and verifies that both
Linux archives for the published build and their matching checksum sidecars
exist. Deployment is therefore not blocked by a per-platform channel that has
not been published yet. Until ready, deployment fails and the existing workers
remain in place. This is intentional: the installer and release workflows are
not made into a new download protocol or migrated to a different storage model.

The guided Linux installer additionally declares `MINIMUM_SETUP_VERSION=0.3.3`.
Publish a client with the `setup` command before deploying this installer. The
deployment gate reads that floor from the installer source; the installer also
checks the channel version and probes `cypher setup --help` before activation.
Do not weaken the gate to deploy it against the existing 0.3.2 channel.

## Publication transaction and retries

`scripts/ci/release.py` is the single implementation used for validation,
readiness checking and publication.

Before **any remote write**, it verifies:

- Exact application and Runtime artifact set, with one Runtime per supported
  platform: macOS ARM64, Linux x86_64, Linux ARM64.
- Numeric versions, the pinned Runtime spec/release definition, and minimum app
  compatibility.
- Actual file sizes and SHA-256 values, safe archive entries/links, and agreement
  between `runtime.json` inside each archive and its external metadata.
- Existing R2 application/Runtime channel versions and GitHub latest release;
  older releases cannot replace newer ones.
- Existing immutable objects and GitHub assets; a different digest under the
  same name is an error, not an overwrite.
- The tag still resolves to the checked-out build commit.

The publish order is:

1. Create/resume a **private GitHub draft**, upload and verify its assets.
2. Upload missing immutable R2 artifacts, `.sha256` files, and versioned manifests.
3. Recheck that the channel was not changed while uploading.
4. Update Runtime manifest, application manifest, then `latest.txt`; read each
   back to verify it.
5. Make the GitHub release public **last**.

Network failure may leave unreferenced immutable objects or a private draft, but
does not expose a new public GitHub release early. A retry with the same artifacts
reuses matching objects and can finish an interrupted pointer update.

Only zero-byte GitHub `starter` assets left by an interrupted upload in a private
draft owned by the **same release-plan fingerprint** are automatically removed.
User-authored drafts, unrelated assets and public assets are never overwritten.

For a failed release, prefer GitHub's **re-run failed jobs** so the successful
build artifacts are reused. Rebuilding a signed binary can change bytes; if
objects already exist with different hashes, bump the version instead. There is
no force/rollback/overwrite switch.

The production mutex serializes the supported CI writers. Manual simultaneous
R2/channel writes are outside this protocol; do not mix them with a running
publish. The publisher detects changed pointers before promotion, but does not
claim a distributed transaction across independent manual actors.

## Runtime versioning and reproducibility

`dist/pi-runtime/release.json` pins the bundle revision, minimum Cypher version
and Node version. The minimum version does not automatically increase on every
application release. Bump the Runtime revision when changing bundle contents or
its compatibility requirements.

### Publishing a Runtime without an application release

Devices poll `releases/runtimes/pi/manifest.json` on their own and install a
newer Runtime whose `minimumCypherVersion` their client already satisfies, so a
curated-package refresh does not need an application version to reach the fleet.
Commit the new pin, then push `pi-runtime-v<version>` matching
`dist/pi-runtime/release.json`.

`release.py publish-runtime` is the application publisher minus the application:
same artifact validation across all three platforms, same immutability and
rollback refusals, same production lock — but its plan contains only the Runtime
archives and `runtimes/pi/manifests/<version>.json`, and the only pointer it may
move is `runtimes/pi/manifest.json`. It reads `manifest.json` and `latest.txt`
to confirm they are unchanged before promotion and **never writes them**, needs
no `contents: write`, and publishes no GitHub release. A Runtime whose
`minimumCypherVersion` exceeds the **oldest** published desktop channel is
refused: a client on the platform still sitting behind it could not load it.

Keep the repository pin ahead of the channel. The Runtime now ships from exactly
one workflow — an application release no longer republishes it — so cutting a
Runtime from a commit whose `dist/pi-runtime/release.json` is older than the
published Runtime is refused as a rollback; bump the pin on `main` rather than
reverting it.

`PI_RUNTIME_VERSION` remains a local packaging override; tagged publication must
match the committed release definition. Runtime tarballs normalize owners and
timestamps so rebuilding unchanged content does not change the archive hash.

Runtime smoke testing uses real RPC startup, a loopback-only provider fixture
and isolated settings. It checks the model and required commands, then repeats
startup with an intentionally broken extension and requires failure. It never
uses `pi --help` as a plugin-health check, sends an LLM prompt, uses system Pi,
or requires real provider credentials. Linux additionally runs this test on
Ubuntu 20.04 / glibc 2.31.

## Local checks

```sh
bash scripts/ci/actionlint.sh
python3 -m unittest discover -s scripts/ci -p 'test_*.py' -v
python3 scripts/test-linux-cli.py
node scripts/ci/pi-runtime-smoke.mjs /path/to/extracted/runtime
python3 scripts/ci/release.py validate \
  --dist /path/to/artifacts --version 0.2.3 --out /tmp/release-plan
python3 scripts/ci/release.py check-deploy
```

Only the `publish` subcommand performs remote writes. It requires the tagged
push Actions context and explicit credentials; local validation/readiness
checks are read-only.
