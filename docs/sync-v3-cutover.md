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
- Persistent harness instances have durable execution occupancy records,
  separate from semantic runs (explicit user decision). These records do not
  expire on heartbeat loss. A finished turn may leave its instance parked;
  ownership transfer waits for verified instance closure and reconciliation.
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
   bounded, durable producer is now implemented and tested through real
   workerd and Swift. Normal Engine writer adoption and client rendering are
   still required. Oversized structured public fields need a verified
   artifact policy; the producer rejects them rather than truncating them.
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
   The user selected server-ordered durable claim/cancel decisions. Winning
   operation identity is now retained on the command; ordinary losing attempts
   commit without poisoning the batch. Host integration must wait for its own
   winning decision before publishing run start and claiming local execution.
   The durable local intent/one-shot gate now exists, with real MockHarness
   API coverage on the experimental path. Normal dispatch, payload policy,
   persistent-process lifecycle and source-journal durability still need
   integration; restoring an old database image is not safe crash-resume proof.
   Normal Engine blind re-dispatch paths have now been removed: neither fresh
   boot recovery, failure before `SessionStarted`, nor an unconfirmed steering
   mailbox delivery automatically starts the request again. The original
   output and session reference remain available for explicit review and a
   new user action. This does not replace the required v3 host integration.
   The v3 SQLite source log now retains full decoded harness events before
   publication, including large private inputs and late results after fence
   loss. Gated producer writes verify source retention and ownership in the
   same transaction; real SIGKILL recovery and bounded pure replay are tested.
   Normal Engine still needs to adopt this source/publisher path in place of
   its JSONL/Loro pipeline.
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

Persistent harness occupancy is now a typed v3 projection entity across Rust,
edge and iOS: semantic `runFinished` leaves the process occupied; explicit
`executionFinished` is required after reconciliation before owner transfer.
Presence expiry cannot release it. Real Engine/iOS lifecycle emission and
authorized production canary verification remain required.

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
