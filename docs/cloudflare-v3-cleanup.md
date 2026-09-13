# Cloudflare v3 cleanup — 2026-09-13

The user authorized removing withdrawn 0.3.10/0.3.11 release objects and
permanently deleting the v3 `Sync3Room` and `WorkspaceHub` data. This operation
does not publish the replacement 0.3.10 or upload a TestFlight build.

This is the cleanup-time snapshot. The subsequent
[0.3.10 release receipt](releases/0.3.10.md) records the current release
channel, deployed versions, and automation state.

## Production state

- Edge application code is restored from `0f0d0f9`. The only Edge source-tree
  change is the migration history in `edge/wrangler.jsonc`.
- Keep the already-applied `v5-v3-native` entry. The new
  `v6-remove-sync-v3` migration deletes **only** `Sync3Room` and `WorkspaceHub`.
  Do not remove migration history, recreate namespaces, or deploy the archived
  v3 configuration over this state.
- Deployed Edge version: `0fef285b-11a4-4dec-b5fd-fd86853a926a`.
- Cloudflare reports both v3 namespaces absent. `SessionRoom`, `DeviceRoom`,
  `RegistryRoom`, `ChatRoom`, and `PushDevice` retain their exact pre-cleanup
  namespace IDs. No old-room storage wipe or attachment deletion was performed.
- `cypher-blobs`, authentication secrets, and push configuration are retained.
- In `cypher-releases`, the four application artifacts, four checksum files,
  and versioned manifest for each of 0.3.10 and 0.3.11 were deleted: **18
  objects**. All other objects retain their pre-cleanup size/ETag, except the
  explicitly changed `manifest.json` and `latest.txt` pointers.
- Both application pointers now select **0.3.9**. Pi Runtime objects and the
  Runtime channel manifest were not changed.
- Landing was redeployed from the baseline to remove the withdrawn-version
  fallback link; its fallback download is 0.3.9. Landing version:
  `9b0f6415-7b4c-4e28-b32b-52bddecfd7e4`.

## Verification and recovery evidence

- Edge typecheck, **92 unit tests**, **35 workerd tests**, and dry-run build
  passed. Both landing tests passed.
- All 18 removed public URLs return 404. All four public 0.3.9 artifact
  downloads match their manifest's size and SHA-256.
- `/health` returns `{"ok":true,"auth":"workos"}`;
  `python3 scripts/ci/release.py check-deploy` passes for 0.3.9.
- This verifies deployment and retained namespace identities, not a complete
  authenticated desktop-to-iOS synchronization or physical-device APNs test.
- Private **release-file** backup and before/after inventories are on the
  release Mac at
  `~/Documents/cypher-release-backups/20260913-v3-cleanup/`.
  These are not backups of the deleted v3 conversation databases.
- Deployment logs: `/tmp/cypher-cf-cleanup-edge-deploy.log` and
  `/tmp/cypher-cf-cleanup-landing-deploy.log`.
- No desktop/UI/headless process, local chat store, or Apple build was changed.

## Before publishing again

Repository-wide GitHub Actions are disabled for this maintenance. The `deploy`
workflow is also independently disabled. The abnormal historical deployment
run **34749426934** still appears queued: normal/force cancellation returned
409, and deletion returned 403 despite repository administrator access.
Resolve or isolate this task before restoring automation; its workflow checks
out the then-current `main`, not necessarily its original triggering revision.

The v3 GitHub tags/releases were removed separately; the code remains on
`archive-v3`. Installed 0.3.10/0.3.11 clients do not automatically downgrade
when the public channel is 0.3.9. Reusing 0.3.10 for different binaries also
requires manual replacement on clients already reporting that version.
