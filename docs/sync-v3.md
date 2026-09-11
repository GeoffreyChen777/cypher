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

Current event vocabulary: command queued, claim/cancel attempted, command resolved, run started,
message created, indexed part put, text appended (part-scoped byte offset
checked), message finished, attachment sealed, run finished. The initial toy
tool/input events have been removed, not retained as compatibility aliases.
Semantic run events carry stable run/message/part identifiers.
No executable closures or arbitrary client-provided SQL are transported.

`commandQueued.command` now carries the full `SessionCommandEntry`, not the
initial text-only command. Its identity/issuer must match the event envelope,
and new entries must be pending and unresolved. Cancellation is issuer-only
and loses the race to acceptance. Only the host resolves commands; applied
requires prior acceptance, and all resolutions are immutable. A rejected
claim cannot start a run or strand an otherwise idle ownership transfer.

The closed command shape descriptor is shared from
`apps/ios/Cypher/Sync/Sync3CommandSchema.json` (bundled on iOS, embedded in Rust,
imported by Edge). Validation happens before decoding can discard unknown
properties or default omitted fields. This preserves model settings, worktrees,
comment prompts, attachment descriptors, input labels and retry identity.
Legacy Loro frontiers are not accepted on this wire. The local normalized
SQLite format is now **7**; earlier prototype formats are rejected without
resetting or migrating their contents. The network version remains **3**.

#### Durable claim/cancel decisions

The user selected **server-ordered durable decisions**, not client-side
guessing/removal of rejected outbox work. `commandClaimAttempted` and
`commandCancelAttempted` are committed attempts. An authorized attempt that
loses an ordinary claim/cancel race is a replayable no-op, not a rejected
batch that parks the chat. Unknown commands, wrong authors and stale ownership
epochs still fail closed. The first winning claim stores `acceptedOpId` with
its run ID; later attempts cannot overwrite either, even when they request
the same run ID. Repeated delivery returns the same committed receipt.

The host must first await its claim's committed decision and match
`acceptedOpId` to its own durable intent. It must not batch a speculative
`runStarted` behind an unconfirmed claim: a losing claim grants no run.
Committed run fencing and a local, non-replayable execution claim are then
required before external effects. The journal now implements that local gate;
normal host-dispatch integration is still a release blocker. The protocol
alone does not grant exactly-once external execution. The previous prototype event names are
rejected rather than maintained as compatibility aliases.

#### Local execution intents and one-shot dispatch

`prepare_execution` atomically stores a scope/epoch-bound intent and its
claim-attempt outbox operation. It never speculates a run start in the same
batch. `advance_execution` checks committed history, not an ACKed-but-unapplied
outbox row. Only the winning operation ID can enqueue the stable run start.
After that start is committed, the journal commits `Claimed` before returning
the sole non-cloneable/non-serializable dispatch permit. A subsequent call or
process restart returns `RecoveryRequired`, including a crash just before the
external call: uncertainty is intentionally not reinterpreted as permission.
Run payloads cannot bypass the run fence by choosing a control plan.

Known completion and terminal outbox operations commit together; the exact
same completion can be retried without another dispatch. Unfinished bounded
producers prevent premature run completion. Missing intent receipts are an
explicit recovery error, not permission to reseed the claim or start.
Private intent rows contain a command hash, not another complete prompt.
Client wrappers wake the transport only after durable local operations.

This gate is not payload-specific host policy: expiry, based-on, artifact
availability and control-target eligibility still need to be checked before
dispatch. Persistent harness process/semantic-turn integration remains open.
Recovery of the same crash-surviving database is not rollback/import of an old
local backup; migration must quarantine uncertain restored intents rather than
treating an old `Prepared` image as evidence that execution never happened.

The normal Engine no longer automatically re-dispatches a fresh crashed
request, a stream that fails before `SessionStarted`, or an accepted steering
request lacking `Steered` confirmation. Reusing a user-message ID only deduped
the transcript, not the external effects; absence of telemetry was not proof
of non-execution. Boot recovery and unconfirmed delivery now retain output and
surface review-required errors. Original harness session references remain
available for explicit continuation. Old auto-resume budget files are left
untouched but no longer consumed or reset. Normal dispatch still requires the
v3 integration above; these changes remove known unsafe retry routes rather
than claiming that integration is complete.

#### Private execution source log

The v3 journal now persists complete decoded `AgentEvent` values in its
scope-bound SQLite transaction, with stable per-command ordinals and SHA-256
digests. Exact retries/overlap are idempotent; changed events, missing
positions, ahead cursors, corrupt bodies and lossy schema reads fail explicitly.
These raw source rows never enter the replication outbox or wake the network
transport. Reads use indexed cursor pages, not whole-chat replay: at most
32 events / 1 MiB, except one oversized event is returned intact to guarantee
progress. A multi-event append has the same budget; a single large private
tool event is retained without applying the smaller public-wire size limit.

`new_execution_writer` binds the producer to the dispatched run and owner
epoch. `enqueue_execution_frame` verifies the retained source position, live
run, intent and ownership fence in the same transaction as producer metadata
and outbox writes. Losing the publication fence blocks those writes, but a
previously issued permit can still retain late raw observations in its
original local scope. Observations after local completion are explicitly
marked; replay cannot silently reopen that semantic run or issue a permit.
Normal Engine source adoption and persistent-process lifecycle remain open.

The complete native transcript entry/part models, event fold, render privacy
policy and continuation helpers now live in `cypher-proto`. The running-code
path in `engine::sessions` uses that shared fold/privacy implementation; only
its persistence writer is still the legacy document writer. `cargo tree -p
cypher-proto --edges normal` contains no Loro dependency. System message roles
and native `#c` continuation identifiers are accepted by all three v3 readers.
The wire now preserves full rendered parts, device/time/continuation metadata,
message status, tool output/diff references and input questions. Its closed
part descriptor is shared from `Sync3PartSchema.json`. Private tool inputs
cannot enter the log; question IDs are opaque strings, not filesystem IDs.
Part IDs are message-scoped, text replacement requires byte-offset deltas,
question identity is immutable and resolved parts cannot become unresolved.
Runs cannot finish with open messages; finished messages reject late writes.

Each projected message is bounded to 256 KiB / 256 parts. Individually bounded
text deltas may accumulate beyond the 64-KiB single-string wire budget: native
reload validates this stored aggregate separately, without relaxing admission.
Overflow must trigger producer rollover, not truncation or a larger frame.
The bounded producer exists; normal Engine adoption, actual attachment
transfer and normal-client rendering still need integration. An
`attachmentSealed` record alone does not establish file upload or availability.

Messages now retain the immutable committed position of `messageCreated`.
The server assigns it from the log, not from an author-supplied field. Native
replay derives the identical position. Updates retain that position even if
the message's last-update sequence becomes newest; timestamps are never an
ordering authority. Native and DO-local render-window APIs return at most
32 messages / 1 MiB of JSON bodies, oldest-first, using an exclusive creation
cursor to read older pages. Cursor and rows share a read transaction.
The ordered partial index is explicitly selected: an execution-plan test
caught SQLite preferring a kind-only index with a sort, which would have
made window cost grow with history. Byte-limited pages use at most one bounded
row of lookahead and do not skip the first excluded row. These local APIs are
not new remote endpoints, and their view watermark must not replace the
replication cursor. Normal-client use and bounded replay remain unfinished.

### Durable bounded producer

`Journal::new_writer` binds a `sync3::writer::TranscriptWriter` to the account,
room and actor before any callback can enqueue a frame. Cross-scope callbacks
and copied foreign checkpoints fail closed, including when actor IDs match.
The writer consumes the storage-independent native fold
one bounded frame at a time. Callers must retain the full append-only source
and continue while `Progress.more` is true. Text is split on UTF-8 boundaries
with JSON escaping included in the physical message budget. Continuations
retain the logical root and ordered parts; scoped repeated IDs also roll over.
Tools reserve space for late updates, and all children close before the root.
Oversized structured public fields fail with `part_requires_artifact`; they
are neither silently clipped nor evidence that an artifact has been uploaded.

`Journal::enqueue_writer_frame` atomically persists operations and sparse
producer checkpoint rows. `Client::enqueue_writer_frame` additionally wakes
the transport. Only this durable acceptance advances the producer, not a
network send. An ambiguous sink result retains the exact frame for retry;
restart restores offsets, source high-water hashes and finalization progress.
Observed but unwritten text cannot silently disappear on resume. Checkpoints
contain metadata and hashes, not another copy of the transcript or private
tool input. Revision conflicts fail closed. A token append changes four rows:
its operation, producer header, current chunk and current source slot, rather
than rewriting every prior slot. These private producer tables are additive
to native SQLite format 7 and are not replicated wire entities. Private
producer header version 2 includes the scope binding.

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
- [x] Complete command payloads, issuer/identity checks, cancellation races and
  immutable resolution, with shared three-language validation/lifecycle cases.
- [x] Durable claim/cancel attempts and immutable winning operation identity,
  including same-run competition, receipt replay and transaction rollback.
- [x] Local durable execution intent, committed run fence and one-shot dispatch
  gate; lost permits require reconciliation instead of another dispatch.
- [x] Full rendered-part wire shapes, continuation/status metadata, scoped part
  identity, UTF-8 deltas and atomic per-message byte-budget rejection.
- [x] Immutable committed message order and indexed, row/byte-bounded local
  render windows, including clock skew, late updates, paging and restart.
- [x] Bounded native-fold producer, durable sparse checkpoints, exact frame
  retry, lossless rollover, late structured updates and resumable finalization.
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
- [x] Dev UI rebuilt/restarted and its engine connection verified. Data dirs
  retained; the engine is restarted only after checking its scope and tasks
  when the implemented behavior requires it.
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
Rust writes twelve events; Swift reads the identical projection and writes a
thirteenth command; a fresh Rust client reads that Swift command. Healthy WS
traffic invokes no HTTP repair. A separate room exercises the real bounded
Rust producer with a megabyte-scale escaped Unicode/NFD transcript, producer
restart and late tool resolution. Rust and Swift verify the complete normalized
part digest; Swift uses ordered, bounded window paging. This is not an actual
harness execution test. The writer smoke now obtains its local execution
permit through the real workerd claim and run-start receipts before publishing.

Results:

- Rust proto/sync: **151 passed**, two opt-in legacy live-edge tests ignored.
  Document unit tests: **81 passed**; its integration test also passes.
  Eighteen existing fold tests moved from doc to proto, and two new transcript
  tests cover lossless data roundtrip and non-mutating render-only privacy.
  No normal-client protocol was switched by these extractions.
- Edge: **114 unit + 59 workerd passed**; typecheck and bundle build passed.
- iOS `Cypher` scheme: **187 passed**, including eighteen v3 tests, on an isolated
  iPhone 17 Pro / iOS 26.5 simulator (removed after testing).
- Desktop build passed. Engine unit tests: **147 passed**; UI tests: **672 passed**.
  Latest engine restart: PID `65019`. Before restarting
  this engine, IPC confirmed local-only mode, no active public sessions or
  subagents, and no additional private chat handles; no child process existed.
  Both data directories were preserved. New startup logs are
  `/tmp/cypher-no-blind-retry-dev-{engine,ui}.log`; live IPC confirmed the same
  device ID, chat and session status, and the UI's socket peer.
  The subsequent source-log change rebuilt and restarted only UI PID `2902`,
  preserving that engine; `/tmp/cypher-v3-source-dev-ui.log` and live IPC
  verified its connection to the same development device.
  The prior icon failure was fixed by preferring package-name matches over
  incidental description keywords. The terminal test now checks the already
  documented/implemented `#191919` baseline; terminal rendering was not changed.
- The existing Engine E2E suite now passes **18 tests** with default parallelism
  repeatedly (one paid-provider test remains explicitly ignored). Native stack
  sampling identified synchronous `FSEventStreamStart` inside Spaces reconcile
  blocking the executor, not a need for longer test deadlines. Native watch
  creation/destruction now have a background owner; Diff watchers likewise no
  longer pin entries or join a pending native registration at runtime teardown.
  Kicks are bounded/coalesced, and shutdown cancels listeners and closes handles
  that register late. Four deterministic tests cover these boundaries.
  Parallel E2E duration fell from roughly 90 seconds to under one second on
  this machine. The complete Engine suite passes **325 tests**, with three
  explicitly opt-in provider/live-edge tests ignored. No v3 harness integration
  is implied: that normal execution path still uses the legacy writer.
- Normal restart/steering regression suites passed five consecutive runs.
  Tests perform an actual temporary-file effect before a simulated child
  failure with no steering confirmation, proving neither `Steer` nor a `Run`
  routed into that mailbox causes an automatic second harness invocation.
  A later explicit user request is a completion barrier and retains the
  prior error/user entry. The confirmed-steer test observes an actual
  `Steered` event, not merely optimistic `Working`, before interrupting.
  The Side Chat suite also passed five consecutive runs: its status-watch
  test now holds the mock run until the subscriber observes Working, then
  releases completion and observes Idle. A latest-value watch is not an
  event ledger, so an instant mock completion could legitimately coalesce
  the old test's intermediate state.
- Fourteen local execution-gate tests cover ACK/application boundaries,
  prepare/terminal transaction rollback, exact retries, lost permits and
  restart, competing same-actor/same-run intents, cancellation, missing
  receipts, scope/epoch changes, plan validation and unfinished producers.
  An experimental Engine integration test calls the actual `MockHarness` API
  once after the gate, folds/journals its events, rolls over a Unicode response,
  resolves a prior tool, and reloads the settled result without another
  dispatch. Raw private tool input remains in the local test journal and is
  absent from rendered parts. This test does not switch normal SessionStore
  and now uses the v3 SQLite source log instead of the old JSONL journal.
  It reconstructs the exact source fold through bounded raw-event pages,
  retains a post-completion observation without publishing it, and cannot
  reacquire dispatch after reopening.
- Source/publication regressions cover sparse writes, transaction rollback,
  exact overlap, missing positions, scope/epoch loss, mismatched producer
  contexts, schema/corruption rejection and a 2-MiB private tool event.
  A subprocess test performs a simulated external file effect, commits its
  raw observations, then is actually killed with SIGKILL without dropping
  SQLite. Reopening retains the WAL data and requires reconciliation rather
  than issuing another dispatch permit. This is process-crash evidence, not
  a claim to have tested physical power failure.
- The live smoke also hands a private normalized SQLite file from Rust to Swift
  and back, including numeric model options, a host command resolution and an
  ACK that must not skip the cursor. Rust can then re-enqueue its original
  body without creating a false conflict against Swift's persisted receipt.
  Both runtimes also read the immutable-order index from that shared file.
- With 1,000 unrelated historical commands present, one text append makes
  exactly three local row changes: its message, its event and the cursor.
- Eleven producer tests cover multibyte/escaped rollover, private-input removal,
  late tool/question resolution, ambiguous durable acceptance, restart with
  an unwritten suffix, rollback on a later conflicting operation, repeated
  scoped IDs, sparse checkpoint writes, 70-message interrupted finalization,
  invalid checkpoint offsets/revisions, empty terminal output, and account/
  room/actor fencing of callbacks and copied checkpoints.
- Shared negative fixtures prevent non-string roles/outcomes from committing.
  Full-part vectors also prove lossless known tool variants, nested shape
  rejection and removal of private raw tool inputs. Shared lifecycle cases
  cover scoped repeated part IDs, immutable questions, resolved-state
  monotonicity, late writes, unfinished runs and attachment conflicts.
  Budget tests preserve earlier committed text when the next delta exceeds
  the message limit; the Swift case reopens SQLite after every delta.
  Canonical numeric fixtures cover `1.0`, `-0.0`, fractional values and exponents;
  numeric representation changes must not poison the sender's own receipt.
- GitHub CI passed all five jobs for `25cc040`, including Linux backend,
  macOS workspace and the native Swift/workerd/Rust smoke:
  https://github.com/GeoffreyChen777/cypher/actions/runs/34653856079.
  Later implementation commits and the final release still require their own
  CI evidence; this run is not a deployment.

Admission owner/epoch fences are tested at the server and in the pure reducer.
Native journals replay authenticated, already-committed history; they do not
compare a historical author against today's owner. Native transactional tests
still enforce the business transitions, canonical receipts and cursor boundary.

Current limitations are deliberate release blockers, not hidden fallbacks:
normal Engine/SessionStore still use chat2; the v3 namespace is only configured
in local test configurations. No production deployment, data migration,
WorkspaceHub, semantic notification pipeline or checkpoint/pruning was added.
Client projections now use sparse SQLite entity rows: applying one text append
loads its message/run dependencies and writes only the changed message, event
and cursor. Applied event history is still retained; checkpoint/pruning and
normal-client adoption of bounded views remain release blockers. The unreleased initial
whole-projection journal format is rejected explicitly, never silently reset.
The socket authorization deadline is capped by the verified JWT expiry and
five minutes; full control-channel reauthentication/revocation is still pending.
