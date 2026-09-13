# Cypher 0.1.2 — Update Notifications for Every Install

The update-notification fix release: Cypher now reliably tells *every* desktop
install when a newer release exists — including signed-out and local-only
runtimes that never touch the sync edge — and the UI's update strip is now
honest about the version running in *this* process, not whatever daemon happens
to be attached.

## What's new

- **Update notifications for signed-out / local desktop installs.** On 0.1.0,
  the release checker was gated behind the edge-enabled (signed-in, synced)
  profile. A signed-out or local-only desktop install had no updater at all:
  the UI's `UpdateStatus` stream closed immediately and resubscribed every 2
  seconds forever — no strip, no banner, and a quiet IPC/log churn. The release
  checker now starts for **every** runtime and profile.
- **The public updater is independent of workspace sync and auth.** Release
  endpoints (`{edge}/releases/*`) are public and updates are device-local, so
  the checker no longer requires the workspace/edge connection that auth and
  sync use. Only the *token-change wake* stays edge-gated — it exists solely to
  re-check when authentication recovers after an offline boot. Local-only
  runtimes keep the normal 6h cadence, and the first check still waits out the
  20s boot delay so engine startup is never slowed.
- **`UpdateStatus` stream stays available with capped retry.** The UI's
  `UpdateStatus` watcher now backs off exponentially (2s → 4s → 8s → 16s →
  30s cap) instead of hammering the IPC every 2s when the stream is unavailable
  or closes prematurely, and it keeps the last valid frame on screen while it
  is unavailable. A stream that delivers a valid frame resets the backoff, so a
  healthy engine restart is still picked up quickly.
- **The strip compares against its own process version.** The engine can be a
  different-version daemon than the UI process it serves (app/daemon skew), so
  the engine's `update_available` boolean is no longer trusted blindly: the UI
  shows the strip iff the latest release is newer than the version running in
  *this* process, which fixes both a stale-engine false positive and a missed
  notification when the UI process is older than its attached daemon.
- **Updater shutdown/check race fixed.** The check loop's receivers are now
  created synchronously with the channels, so an immediate `check_now` or
  `shutdown` can never be lost to a loop that hasn't subscribed yet — an
  immediate shutdown on a freshly-spawned updater no longer hangs waiting for
  the 6h loop (covered by a regression test).

## Immediate workaround for 0.1.0 and 0.1.1 installs

If you installed 0.1.0 or 0.1.1 and Cypher is not showing an update banner,
update manually — either:

```sh
/Applications/Cypher.app/Contents/MacOS/cypher update
```

or reinstall the current DMG from <https://edge.letscypher.app> / the
[download page](https://letscypher.app). Local-only desktop installs in 0.1.0
and 0.1.1 had no working update checker; 0.1.2 and newer check automatically
again.

## macOS build note

The macOS build remains **ad-hoc signed and not notarized** unless the CI
signing secrets appear (`MACOS_CERT_P12`/`MACOS_CERT_PASSWORD` for a Developer
ID Application certificate, plus `AC_API_KEY_*` App Store Connect credentials
for notarization). When those secrets are configured, the release pipeline
signs and notarizes automatically; until then Gatekeeper may warn on the
first launch.

## Tags & artifacts

Release tag: `cypher-v0.1.2` — artifacts: `cypher-0.1.2-linux-{x86_64,aarch64}.tar.gz`,
`cypher-0.1.2-macos-arm64.dmg`, `cypher-0.1.2-macos-arm64-app.tar.gz`.

The Linux artifacts build genuinely headless (`--no-default-features`: no
X11/Wayland/GPUI linkage) and run on a clean Ubuntu 24.04 container, verified
in CI by the release smoke test; macOS ships the default headed build.
