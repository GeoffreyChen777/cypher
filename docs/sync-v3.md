# Sync v3 — implementation contract

Status: **in development; not production enabled**.

Release decision (2026-09-11): the user explicitly selected a **direct cutover,
without old-protocol compatibility**, and authorized production deployment and
new client releases after completion. Work is isolated on
`sync-v3-direct-cutover`; do not push incomplete changes to `main` (it deploys).
Old protocol writers must be refused after cutover, not dual-written or used as
a fallback. Retaining and verifying migration backups remains mandatory.
The iOS release target is confirmed as TestFlight for existing testers only;
App Store submission and expanding the testing audience are not authorized.

This is the new protocol selected on 2026-09-11, not a change to chat2's polling
intervals. Existing rooms, databases, and production migrations stay intact.
Do not advertise v3 to normal clients until the complete acceptance matrix below
passes. Completing the protocol core alone is NOT completing the rollout.

## Authority and delivery

- One account-scoped WorkspaceHub control stream; direct, demand-driven chat
  streams. No token-by-token cross-DO gateway forwarding.
- SQLite outbox before local acknowledgement. Stable operation IDs survive
  process death and transport replacement.
- Server ACK means **committed**, not executed and not locally applied.
- Ordered event application and the applied cursor commit in one local
  transaction. ACKs never skip unread events.
- A resumed stream is bounded by an immutable `through` high-water mark.
  Concurrent later writes belong to the next page/live stream.
- A command is queued → accepted by host → running → finished. External side
  effects cannot in general be made exactly-once. An uncertain execution is
  reconciled explicitly, never blindly replayed.
- Stable chat execution owner, with an epoch fence. Availability leases do not
  transfer ownership. Old epochs and unknown protocol versions fail closed.
- IDs are scoped by verified account and room. Operation-ID reuse with a
  different body is an error, not a second write.
- Reliable event, command and notification outboxes are distinct from lossy,
  replaceable presence/view leases. No durable heartbeat rows.
- A healthy WS uses no parallel HTTP sync. Business ACK/cursor deadlines, not
  transport pongs, trigger one coordinated repair.
- No implicit cloud reseed after server rollback. Snapshot/cursor incompatibility
  produces an explicit recovery condition.

## Wire core

JSON v3 envelopes are capped at 256 KiB, with at most 64 operations per batch.
Integers on the wire are nonnegative JavaScript-safe integers. UTF-8 byte
budgets, not character counts, apply on every platform.

`hello` identifies actor and applied cursor. `push` carries stable operations.
`pull` reads bounded pages; `probe` returns the committed head. `state`, `page`,
`ack`, and structured `error` are server replies. Transport generation is
client-local and must fence callbacks after cancellation.

An operation has `id`, `actor`, `ownerEpoch` and a typed `event`. Transcript
events are host-authored; command enqueue is multi-device. An account member
is trusted within its account; an actor label is not a cryptographic device
identity. User/organization authorization remains mandatory at the Worker.

Initial event vocabulary: command queued/accepted, run started, message created,
text appended (byte offset checked), tool started/finished, input requested,
run finished. Semantic run events carry stable run/message/part identifiers.
No executable closures or arbitrary client-provided SQL are transported.

## Deployment and migration boundaries

- New DO class/namespace; never rename `cypher-edge` or existing DO bindings.
- Experimental endpoints must be explicitly enabled and use separate v3 room
  identities. They must not reinterpret chat2 payloads.
- Migration requires a retained legacy snapshot, frozen write epoch, content
  verification and reconciliation of unacknowledged commands. Old writers
  receive upgrade-required rather than silently writing a frozen lineage.
- Normal release/production deployment, real-data migration and physical-device
  APNs validation require separate approval.
- A rollback after v3 writes must preserve/convert those writes; flipping a
  version number back is not rollback.

## Acceptance ledger

Each item requires evidence. Unchecked items are not implemented/verified.

- [x] Shared golden projection and invalid-operation vectors (Rust/TypeScript/Swift).
- [x] Real DO SQLite transaction: append + dedupe + receipt; rollback,
  reconstructed-log replay and conflicting ID.
- [x] Bounded cursor resume, immutable page ceiling, gap and epoch rejection.
- [x] Host ownership fence and reducer validation.
- [x] Rust durable outbox and transactional cursor/reducer.
- [x] Rust transport: lost ACK repair, healthy-live zero HTTP, pongs cannot
  hide business timeout, semantic epoch conflict parks without a retry storm.
- [x] iOS durable state/outbox, account/epoch isolation, transactional projection
  and client destruction/cancellation.
- [x] Swift aggregate HTTP repair deadline and cancellation fencing at hello,
  page and ACK commit boundaries; TLS-only remote repair and no redirects.
- [ ] Complete Swift network fault parity (slow sends, socket/probe faults,
  background/foreground re-entry).
- [ ] Engine/UI integration with actual commands and transcript projection.
- [ ] WorkspaceHub control stream, presence/view leases and demand subscriptions.
- [ ] Transactional semantic-notification outbox and delivery dedupe.
- [ ] Typed-event snapshot/checkpoint, bounded catch-up, slow-network backpressure.
- [ ] Legacy-to-v3 migration, crash at every boundary, post-write rollback.
- [x] Runtime-real Rust ↔ workerd ↔ Swift live convergence and client restart.
- [ ] Full process-kill/hibernation/network fault matrix across all runtimes.
- [x] Desktop and iOS local build/test regression suites.
- [ ] Release CI and target-platform artifact validation on the final revision.
- [x] Dev UI rebuilt/restarted; existing headless engine and data dirs retained,
  new UI socket verified against the existing engine socket.
- [ ] Authorized production canary and physical iOS notification verification.

## Cost acceptance (same workload, not just fewer delivered messages)

Healthy-live HTTP rows = 0; per-peer HTTP presence polling = 0; standalone
activity HTTP heartbeat = 0; token-rotation release checks = 0. Track HTTP,
DO WS messages, inter-DO RPC, CPU, SQL reads/writes and R2 separately. Verify
convergence and notification correctness before comparing daily totals.

## Reproducible verification (2026-09-11)

```sh
cargo test -p cypher-proto -p cypher-sync --features cypher-sync/mock-server
cd edge && npm run typecheck && npm run test:unit && npm run test:workerd
# From repo root, on macOS with Xcode and installed edge/node_modules:
python3 scripts/ci/sync3-smoke.py
```

The smoke owns a loopback-only workerd process group and temporary databases.
Rust writes ten events; Swift reads the identical projection and writes an
eleventh command; a fresh Rust client reads that Swift command. Healthy WS
traffic invokes no HTTP repair. This is not an actual harness execution test.

Results:

- Rust proto/sync: **86 passed**, two opt-in legacy live-edge tests ignored.
  Document-model tests: **99 passed** after extracting the full command DTO
  into `cypher-proto`; no normal-client protocol was switched by that extraction.
- Edge: **102 unit + 44 workerd passed**; typecheck and bundle build passed.
- iOS `Cypher` scheme: **180 passed**, including eleven v3 tests, on an isolated
  iPhone 17 Pro / iOS 26.5 simulator (removed after testing).
- Desktop build passed. Engine tests: **143 passed**; UI tests: **672 passed**.
  The prior icon failure was fixed by preferring package-name matches over
  incidental description keywords. The terminal test now checks the already
  documented/implemented `#191919` baseline; terminal rendering was not changed.
- The live smoke also hands a private normalized SQLite file from Rust to Swift
  and back, including pending numeric input answers and an ACK that must not
  skip the cursor. Both directions pass.
- With 1,000 unrelated historical commands present, one text append makes
  exactly three local row changes: its message, its event and the cursor.
- Shared negative fixtures prevent non-string roles/outcomes from committing.
  Canonical numeric fixtures cover `1.0`, `-0.0`, fractional values and exponents;
  numeric representation changes must not poison the sender's own receipt.
- GitHub CI passed all five jobs for `43ecaaa`, including Linux backend,
  macOS workspace and the native Swift/workerd/Rust smoke:
  https://github.com/GeoffreyChen777/cypher/actions/runs/34606348029.
  Later implementation commits and the final release still require their own
  CI evidence; this run is not a deployment.

Current limitations are deliberate release blockers, not hidden fallbacks:
normal Engine/SessionStore still use chat2; the v3 namespace is only configured
in local test configurations. No production deployment, data migration,
WorkspaceHub, semantic notification pipeline or checkpoint/pruning was added.
Client projections now use sparse SQLite entity rows: applying one text append
loads its message/run dependencies and writes only the changed message, event
and cursor. Applied event history is still retained; checkpoint/pruning and
bounded large-history views remain release blockers. The unreleased initial
whole-projection journal format is rejected explicitly, never silently reset.
The socket authorization deadline is capped by the verified JWT expiry and
five minutes; full control-channel reauthentication/revocation is still pending.
