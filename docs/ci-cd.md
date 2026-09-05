# CI and release operations

## Workflows

- **`ci.yml`**: pull requests, pushes to `main`, and manual runs. Runs workflow
  lint, release/installer regressions, Edge typechecking and unit/workerd tests,
  Linux backend tests (including Engine integration tests), updater Clippy,
  formatting, and macOS workspace compilation. No deployment credentials are
  available to these jobs. UI unit tests are not currently part of this gate.
- **`deploy.yml`**: pushes to `main` and main-only manual runs. Captures a fresh
  `main` SHA once, tests it, checks installer compatibility, then deploys all
  three workers from that same SHA. It deliberately does not use per-push path
  deltas: skipped/pending pushes and `grep -q`/SIGPIPE must not omit changes.
- **`release.yml`**: pushes of `cypher-v<version>` build and publish. Manual runs
  are **always build/validate-only**, even when a tag is selected. The tag must
  match `[workspace.package].version` in `Cargo.toml`.

The Rust toolchain is pinned in `rust-toolchain.toml`. Node is pinned to 24.19.0.
Worker deployments use Wrangler from `edge/package-lock.json`, not a floating
`npx wrangler@4`. Actions are pinned by commit.

`deploy` and `release.publish` share the **`cypher-production`** concurrency
group with `queue: max` and no cancellation of running jobs. GitHub.com supports
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

## First deployment after the checksum migration

The current embedded installer requires standalone `.sha256` files. Do **not**
deploy it against an older channel that lacks those files.

1. Prepare a new application version; do not reuse the already released `0.2.2`.
2. Configure the deployment credential in GitHub Settings.
3. Publish a new matching `cypher-v<version>` tag and wait for release success.
4. Run `deploy` on `main` again.

The deployment gate reads the public application manifest and `latest.txt`,
requires them to agree, and verifies that both Linux archives and their matching
checksum sidecars exist. Until ready, deployment fails and the existing workers
remain in place. This is intentional: the installer and release workflows are
not made into a new download protocol or migrated to a different storage model.

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
