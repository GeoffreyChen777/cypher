# chat2: session sync over a log relay

Each chat's Loro doc syncs through a ChatRoom Durable Object (`chat2/{chatId}`,
`apps/edge/src/chat/chat-room.ts`) that stores and relays opaque Loro updates as
append-only rows and never parses a doc. The synced transcript stays thin: tool output is
kept as a summary, not in full. Code cites the section labels below (A1, C3, M2, …).

## Why

- **Thin docs.** Tool outputs and diffs dominate a transcript's size, so the doc keeps
  summaries and per-file diff stats; the full text stays in the host's local run journal.
  Small docs keep sync fast on slow links.
- **A relay that cannot wedge.** A Durable Object that materializes docs through loro-wasm
  fails at around 1 MB (CPU limits, wasm heap faults, rejoins that never backfill). The
  ChatRoom only stores rows and checkpoints, so its serving cost does not depend on what
  the doc contains.

Clients keep real Loro docs: offline commit-then-converge and the command ledger as the
outbox work as on any Loro doc. Partial or windowed doc loads are out of scope.

## A. Thin docs

**A1. The fold keeps summaries.** In `crates/doc/src/parts.rs`:
- `output`: code fences stripped, then at most `TOOL_OUTPUT_SUMMARY_MAX_LINES` (5)
  complete lines; a long single line is kept whole.
- `diff`: per-file stats `{path, additions, deletions}` (`diffStats`) instead of inline
  diff text.
- `outputRef`, `outputBytes`, `diffRef` and `diffStats` are serde-additive; older apps
  render the summary as if it were the output.

**A2. Output sidecar (read-only).** Hosts do not upload full outputs. Older chats whose
parts carry an `outputRef` still resolve it: the Worker answers
`GET /blob/{chatId}/{partId}[.diff]` from the `BLOBS` bucket (owner auth through the
Worker's JWT check), and the UI fetches it on expand (`FetchToolBlob`); offline shows the
summary.

**A3. UI.** A tool part renders its summary inline and fetches an existing `outputRef`
when expanded.

## B. The ChatRoom relay

**Storage** (Durable Object SQLite):
- `rows(seq INTEGER PRIMARY KEY, device TEXT, batch_id TEXT UNIQUE, bytes BLOB)` — opaque
  Loro update blobs, at most 1 MB per row (`MAX_ROW_BYTES`); oversized rows are rejected
  at the header.
- The checkpoint and its opaque, client-written frontier as blobs (chunked by `blobs.ts`),
  and `meta` keys `seqFloor`, `checkpointSeq`, `checkpointSize` and `checkpointAt`.
- `tail` and `diff` sidecar routes serve whatever a host published, verbatim; current
  hosts publish neither (C3).

**Protocol** (binary WebSocket frames: 1-byte type + JSON header + raw payload, with no
base64 so payloads cost their own size on slow links):
- `hello{cursor, device}` → `state{seqFloor, headSeq, checkpointSeq, checkpointSize,
  rowCount, rowBytes}` with the checkpoint frontier as its payload, metadata only. The
  client compares that frontier with its local doc: if the doc already includes it, it requests `rows{after: max(cursor,
  checkpointSeq)}`; otherwise it fetches `GET /checkpoint` (HTTP, Range-resumable) and
  then the rows after `checkpointSeq`.
- `rows{after, excludeDevice}` streams rows with `seq > after` from other devices, so a
  device never re-downloads its own writes.
- `push{batchId, bytes}` appends, relays and answers `ack{batchId, seq}`. `batch_id
  UNIQUE` deduplicates reconnect re-pushes; re-importing a Loro update is a no-op, so
  duplicates are safe end to end. Reconnect replay sends one pending batch at a time,
  armed by the previous ACK, and a duplicate `batchId` is acknowledged before quota
  accounting so lost ACKs do not spend the write budget.
- `POST /checkpoint {seqCovered, frontier}` + chunked blob: owner-only and refused below
  `seqFloor` (the floor never moves backwards). Rows with `seq <= seqCovered` are deleted
  once the checkpoint commits.
- Presence: opaque ephemeral frames relayed to live sockets with a 30 s TTL sweep; the
  server keeps no presence state beyond that.
- Validation needs no wasm: auth and ownership, frame shape, the row size cap and
  per-device rate and byte quotas. Malformed content stays inside its owner's room, is
  skipped by client imports and disappears with the next checkpoint.
- Operations: `GET /stats` (headSeq, seqFloor, row bytes and count, checkpoint age) and a
  nightly R2 backup from the daily alarm.

## C. Clients and host duties

**C1. Rust client** (`crates/sync/src/chat_client.rs`): cursor tracking, a pending-push
queue keyed by batch id, the hello → backfill → live loop, the frontier comparison that
skips a checkpoint, and reconnect with backoff and wake-ups. Liveness is layered: a ping
lease, a join deadline and a probe clock.

**C2. Store** (`crates/sync/src/store.rs`): each snapshot row carries its room `cursor`
and doc `epoch`, written in the same transaction as the snapshot bytes so content and
cursor cannot diverge; unacknowledged batches live in the durable `chat_outbox` until
their ACK. iOS writes the cursor and snapshot into one `c2_<id>.loro` file for the same
reason (`DocDisk.swift`).

**C3. Host duties** (`crates/engine/src/host/doc_host/chat2_sync.rs`). Only the chat's
host device posts checkpoints:
- a threshold checkpoint when the room's rows pass 512 KB or 200 rows (`rowBytes` and
  `rowCount` in `state`);
- a bootstrap checkpoint when a room has rows but no checkpoint;
- a seed checkpoint after a server reset, and a compensating one after a rejected push.

Hosts publish no tail or diff sidecar: nothing reads the tail, and remote clients read
working-tree diffs through the device relay. Measuring the Edge's cost:
[Cloudflare billing](../operations/cloudflare-billing.md).

**C4. iOS** (`apps/ios/Cypher/Sync/ChatRoomClient.swift`): the same protocol, with the
framing vectors shared across Rust, TypeScript and Swift
([`protocol/vectors/chat-frames-v1.json`](../../protocol/README.md)).

## M. Doc lineage

**M1. Lineage epoch.** Every chat2 doc carries `meta.epoch = 2`.

**M2. Room generation.** Registry chat rows carry a `roomGen` field (per-field HLC LWW
like every row). Desktop hosts and current iOS builds open every chat as chat2, whatever
the row says. Chats are still created with `roomGen: 2` because iOS builds up to 0.2.0
(24) dial a chat's room only when the row says 2; the field can stop being written once
those builds are gone (TestFlight builds expire 90 days after upload).

**M3. Older local docs.** A device opening a chat whose local doc has `epoch < 2` discards
that doc **after** re-queueing its own unresolved commands as fresh entries, then adopts
the chat2 checkpoint. The old snapshot row is kept under a suffixed doc id; importing it
instead would duplicate every message, because the two Loro histories are unrelated.
