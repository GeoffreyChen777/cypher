# Workspace v3 control channel

Implementation status: normal Engine and Swift workspace/remote RPC now use
this endpoint. **This is not a production cutover or
whole-product acceptance claim.**

## Scope and authority

`/workspace3/{org}/ws` is routed to `["workspace3", verifiedOrg, verifiedUser]`.
Client-supplied identity/deadline headers are overwritten by the authenticated
Worker. Existing sockets stop serving business traffic at their bounded auth
deadline (at most five minutes). No legacy registry/device endpoint fallback,
old snapshot import or metadata reseed is part of this protocol.

The hub handles metadata and availability, **not execution ownership**.
Presence expiry, socket replacement, RPC cancellation and demand never close
a v3 execution occupancy or grant a dispatch permit.

## Current-state metadata

Messages are closed JSON envelopes with `version: 3`.

- `hello { user, org, actor, role: host|viewer, after }` →
  `welcome { user, org, connection, leaseMs, through, next, done, rows }`.
  The expected user must equal the authenticated account; clients verify the
  echoed user before applying rows or transmitting any pending metadata.
- `page { after }` → `page { through, next, done, rows }`.
- `push { id, ops }` →
  `pushed { id, requestHash, through, rows }`, plus a `changed { through }`
  hint to connected peers.
- `probe { id }` → `probeOk { id, through }`. Runtime `ping`/`pong` is only
  transport liveness, not a successful business probe.

Rows retain the portable per-field HLC merge rules. New operations cannot
carry legacy clock overrides. Kinds are `devices`, `spaces`, `chats`; session
activity belongs to replaceable presence, not a durable heartbeat row.

Each changed row receives its own unique monotonic sequence. Pages contain
at most 32 rows and fit a 256 KiB envelope; a row is at most 64 KiB. A push
contains at most three operations, each at most 16 KiB, and is transactional.
Tombstones are retained. Cursors ahead of the server are errors, not resets.

`requestHash` is SHA-256 of the exact UTF-8 request frame. The client must
match it against its in-flight request before retiring a persisted batch,
apply the canonical returned rows, and **not advance its contiguous cursor
from an ACK**. Paging advances that cursor. Metadata retries reuse the same
LWW operations; no growing metadata receipt log is needed.

## Replaceable state

- `watch { chats }` replaces this connection's desired conversations.
  `watching { chats }` acknowledges it; hosting devices receive
  `demand { chats }`, derived from current chat metadata.
- `presence { state }` is forwarded with server-assigned `actor`, `role`,
  `connection` and `expiresAt`. It writes no SQLite rows. Clients expire it
  locally; after hub hibernation, a lost presence payload is unknown until
  refreshed, not proof that a task ended.
- `peerClosed { actor, connection }` is generation-specific. A retired
  connection cannot take a replacement connection offline.

There are at most 64 sockets and eight wanted chat IDs per socket. Hibernation
attachments have a hard 2 KiB budget shared with in-flight RPC state; capacity
errors are explicit and must not silently discard another subscription.

## Bounded RPC routing

- Caller: `call { id, target, method, params }`.
- Fragmented caller: `call { id, target, method, params: {}, input: true }`.
- Route receipt: `routed { id, token, window: 2 }`.
- Host: `call { token, from, method, params, window: 2 }`.
- Host result: `reply { token, sequence, done, value }`.
- Caller result: `reply { id, sequence, done, value }`.
- Caller applies a result then sends `ack { token, through }`, where
  `through` is the next expected sequence (first result has sequence zero).
  Host receives `credit { token, through }`.
- Caller cancellation: `cancel { token }`, forwarded only to the bound host.
- Caller input: `input { token, sequence, done, value }`; host receives the
  same token-bound frame. The input window is independently limited to two.
- Host input receipt: `inputAck { token, through }`; caller receives
  `inputCredit { id, token, through }`. Input ends at most at fragment 256.
  A terminal reply can reject input early; nonterminal output cannot start
  before input is complete.

An opaque HMAC-authenticated token binds the two server connection IDs, the
caller request ID and a fresh per-call nonce. The secret survives hibernation;
call/credit state lives in bounded socket attachments, not a durable RPC log.
Reusing a caller ID cannot let an old reply complete a new call.

At most eight calls per caller and eight total across all callers to a host,
two unacknowledged frames per direction per call,
64 KiB params/result values and 256 KiB total frames are accepted. The input
queue is capped at 64 frames / 2 MiB. Clients must bound/reassemble any
application-level fragmentation; oversized results must not be truncated.

Routing is not proof of delivery or execution. Host loss reports
`delivery_unknown`; clients must not automatically resend side-effecting RPC.
Client deadlines/cancellation and disconnect cleanup are required. Ordinary
chat execution continues to use the committed v3 command/intent gate.
Caller closure cancels exactly its original generation's host routes, including
incomplete uploads; replacement calls cannot inherit those cancellations.

### Application codec and native adapter

`crates/rpc/src/workspace3/` implements both directions plus device-addressed
client handles over the existing Hub connection. No device-room socket or
automatic RPC replay is part of the normal runtime.

Each logical JSON value is at most 8 MiB, encoded in 32 KiB raw-byte fragments:
`{ codec: "json-base64-v3", length, offset, end, data }`. `data` is canonical
base64; offsets count raw bytes, not Unicode characters. Nonfinal chunks have
exactly 32 KiB. Decoders reject changed lengths, reordering, duplicates,
unknown fields, invalid base64 and inconsistent end markers. No incomplete
JSON value reaches the application service. An error poisons that call.

Normal requests use the input codec uniformly. Results encode one private-RPC
envelope per logical value (`id: 0`, exactly one of `ok`, `err`, `item`, `done`).
The routing `done` flag agrees with that envelope's terminal status. Consumers
ACK after bounded assembly/delivery; queues and credit waits are bounded.
Oversized results explicitly fail rather than truncate. Null results/items
are distinct from absent fields.

Native request/initial-response deadlines are bounded, and partial replies
and credit waits have deadlines. Dropping an idle subscription cancels even
without another item. Disconnects terminate calls with uncertainty; metadata
reconnect may replay its safe outbox, never a transient RPC. Normal host
services are weakly referenced and reject IPC-only methods and foreign target
IDs. Cancel/host loss never releases a conversation's execution occupancy.

## Evidence

`edge/test/workerd/workspace3.workerd.test.ts` covers real SQLite rollback and
paging, exact push-ACK hash, account isolation, zero durable heartbeat/RPC
writes, demand, hibernation reconstruction, route forgery/stale nonce rejection,
credit-window and call-capacity enforcement, uncertain host loss and
already-open socket authentication expiry. `sync3-routes.test.ts` checks the
outer Worker account/org/header boundary.

Remaining integration: retiring legacy transport fixtures and dependencies,
bounded read leases, semantic-notification integration, and the
whole-product cutover gates in `sync-v3-progress.md`.

## Native library

`crates/sync/src/workspace3/` provides:

- An endpoint/org/user/actor-bound SQLite journal with transactional HLC
  allocation, canonical rows, bounded optimistic reads and immutable pending
  request bytes. Page rows/cursor and ACK rows/outbox retirement commit
  atomically. It rejects old SQLite formats, account rebinding, server rewind
  and implicit reseed.
- An explicit `endpoint: "local"` authority commits without a cloud outbox;
  the database cannot later be joined to the cloud under another scope.
- A single reconnect actor with account/endpoint validation before pending
  uploads, separate page/push/probe deadlines, generation-scoped transient
  sends, replaceable desired presence/demand, bounded event/control queues,
  and joined retirement. It exposes RPC frames/credits; application RPC
  adapters still need integration and must not ignore delivery uncertainty.
- v3 handshakes use Authorization headers. Legacy query-token URL providers
  are not a v3 transport fallback.

The journal currently caps the offline metadata outbox at 1024 operations
and returns an explicit capacity error instead of silently discarding edits.
Normal workspace domain mutations commit all operations and HLC allocation
in one local transaction, including multi-row cascades. Normal activity is
replaceable presence; it does not append a metadata row or heartbeat WAL.
Metadata arrivals update only the bounded set of changed render-cache rows.
Device availability is derived from live leases, not saved last-seen dates.
Device tombstones park background metadata upload; explicit fresh sign-in
may rejoin. Wanted conversations wake owning hosts through a weak callback
installed after runtime assembly. Presence is not an execution permit.
The domain model reuses portable LWW types currently located in `cypher-doc`;
that remaining dependency must move out before the legacy crate is removed.

## Swift library

`Workspace3Wire.swift`, `Workspace3Journal.swift`, `Workspace3Client.swift`
and `Workspace3RPCCodec.swift` implement strict workspace envelopes, the same
SQLite schema, a single header-authenticated connection, replaceable presence/
read interest, and the portable RPC fragment codec. `AppConfig` provides the
captured scope and header-only request. Normal `WorkspaceStore`,
`WorkspaceRemote`, session interest and attachments share `Workspace3Context`;
there is no device-room socket or implicit retry in that path.

The SQLite cursor/rows, HLC/outbox and canonical ACK/receipt retirement each
commit atomically. Rewinds, scope rebinding and legacy formats are errors.
Rust compares decoded scope identity rather than JSON key order, allowing
both languages to open a fresh journal written by the other. Bounded indexed
windows include pending overlays; each pending op is applied incrementally.

The Swift connection checks the exact expected endpoint and echoed account
before uploading. It has bounded queues, independent handshake/page/push/
probe deadlines, bounded transient-send age, generation fencing, and joined
reader/timer shutdown. Reliable-frame overflow reconnects from the journal
instead of silently dropping data. Semantic conflicts park the connection.

Swift RPC admission includes connection waiters, not only routed calls.
Closing one remote handle cancels its own waiting work without retiring the
shared context. Account invalidation fences requests and retires the context.
Sidebar mutations commit through SQLite; failed creates return no ID and
surface an error. Parent/space cascades are locally atomic. Runtime sessions
are derived from current host leases and matching chat ownership, never a
durable heartbeat row or a saved last-seen timestamp.

The local smoke script exercises Rust↔Swift SQLite creation/reopen/receipt
retirement, bidirectional Unicode RPC fragments, and two Swift peers against
actual workerd, including wrong-account pending-data retention.

## Notification coordinator

`/workspace3/{org}/notifications/{settings|activity|event|register|unregister}`
routes to the same verified account Hub, with a mandatory captured-user
header. Coordinator policy reads native chat/space rows; semantic-event and
activity acknowledgement writes are transactional, and notification alarms
do not trigger legacy registry backup/reseed. Revocation-only push capabilities
retain their separate `/notifications/revoke` endpoint.

The obsolete RegistryRoom production binding is deleted by the next migration.
The production entry point exports only WorkspaceHub, Sync3Room, PushDevice
and APNsSender. Legacy source files still used by fixtures are not runtime
fallbacks. Notification activity lease storage and guaranteed semantic-event
handoff still require the broader cost/delivery audit; moving the endpoint
alone is not proof of those gates.

`workspace3_smoke` exercises independent host/viewer journals through actual
workerd, including offline metadata upload, hash-bound ACK, demand, RPC
credits, account mismatch with retained unsent data, generation retirement
and restart. It is included in `scripts/ci/sync3-smoke.py`.
