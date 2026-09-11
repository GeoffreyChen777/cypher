# v3 direct cutover and release checklist

User decision: **no legacy protocol compatibility**. This is a release plan,
not a claim that the migration or deployment has happened.

iOS publication scope was separately confirmed: **TestFlight for the existing
tester scope only**. No new tester invitations, public links or App Store
review submissions.

## Non-negotiable cutover properties

- The supported production clients use v3 only. No fallback to chat2, s2,
  registry1, old presence polling or old activity heartbeat.
- A maintenance boundary fences old writes, including already-open sockets;
  rejecting only new HTTP upgrades is insufficient.
- Retain authenticated, account-scoped legacy snapshots and their checksums.
  Do not delete chats, reset namespaces, or regenerate unresolved command IDs.
- Verify all transcript parts, ordering, attachments, tool references and
  command outcomes during conversion. Uncertain execution is quarantined for
  explicit reconciliation, not automatically executed again.
- Availability leases never transfer execution ownership.
- Failure before activation leaves maintenance in effect and retained data
  intact. Failure after v3 writes requires a tested rollback that preserves
  those writes, not merely enabling the old routes.

## Required implementation work before deployment

1. Complete the event model against existing command/transcript features,
   including comments, attachments, side chats, input responses and recovery.
   The portable command DTO now lives in `cypher-proto` rather than the Loro
   document crate. `fixtures/sync3/run-command.json` covers the actual complete
   run payload, now carried by the v3 `commandQueued` wire operation. Shared
   validation and lifecycle vectors cover strict parsing and durable decisions.
   Distinguish a semantic turn from a persistent harness process: a parked
   process can serve multiple turns, so its process ID cannot blindly become
   a single already-finished v3 run ID.
   The native entry/part models, folding, privacy projection and continuation
   helpers are also storage-independent now. Full part/delta operations and
   tools/questions/status/continuation metadata now exist on the wire. The
   actual bounded writer and normal-client rendering are not implemented yet.
   Transcript ordering now derives from authoritative committed positions,
   not timestamps or entity rows' last-update positions. Indexed, bounded
   native render-window reads are available; normal clients still need to
   adopt them rather than loading the complete projection.
   Part IDs are message-scoped (including repeated live-plan IDs), not globally
   unique tool IDs. The existing 256-KiB segment budget is larger than a v3
   operation's 128-KiB limit: use part/delta operations and bounded rollover,
   not oversized whole-message replacements.
   Before any external side effect, the host must obtain a committed owner
   fence and durably record its execution claim. A crash with an uncertain
   claim is recovery work, never an automatic second dispatch.
2. Replace normal Engine and iOS SessionStore sync with v3. Keep local profiles
   functional. Verify actual mock-harness execution, not only event fixtures.
3. Add the account-scoped WorkspaceHub control connection: metadata, demand
   subscriptions, replaceable presence/view leases, and bounded RPC routing.
4. Commit semantic-notification outbox entries with run events. Prove retries,
   dedupe, account changes and authorized real-device delivery.
5. Finish resumable, verified snapshots and pruning, bounded replay and
   slow-reader/backpressure handling.
6. Implement offline/online migration and every crash-boundary test.
7. Make old routes and surviving old sockets reject requests at the cutover
   boundary. Keep migration access separate from normal-client access.

## Release order

1. Complete the acceptance matrix; rerun native and CI suites on the exact
   source revision to be released.
2. Build and validate macOS ARM64, Linux x86_64/ARM64 and iOS artifacts.
   Verify signing/notarization, Runtime compatibility and immutable checksums.
3. Upload the iOS distribution build and verify Apple processing. Existing
   distribution is TestFlight; upload, tester availability, real-device
   acceptance and App Store review are distinct statuses. Do not guess legal
   declarations or accept agreements.
4. Coordinate the maintenance window and capture/verify migration backups
   before enabling the v3-only production configuration.
5. Publish through the existing tag/release workflow and deploy through the
   existing production mutex. Never rename `cypher-edge` or its current DOs.
6. Verify the production Worker version, channel manifests, artifact
   downloads/checksums, new-client command execution, cross-device transcript
   convergence and notifications. Verify that old clients cannot write.
7. Record actual release URLs, deployed version IDs, Apple build processing
   status, migration counts and rollback evidence. CI success alone does not
   establish deployment or TestFlight availability.

The existing production namespace and channels remain unchanged until these
gates are satisfied. Creating a branch, running tests or discovering configured
CI secret names does not count as deployment.
