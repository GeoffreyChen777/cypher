# chat2: dumb-relay session sync + thin docs

Status: CURRENT — in production since cypher 0.1.4 (ChatRoom DO, `edge/src/chat-room.ts`). Origin: 2026-08-09 investigation (whale-doc dissection + t3code comparison).
Prior art: `docs/registry-sync.md` (the same argument, applied to the workspace index).

## Why

Two compounding problems, one measured root cause:

1. **Session docs got fat.** The tool-output/diff caps (c951c3e, 2026-08-07) bound each
   *part*, not the *session*. Dissection of chat `1b65e93d` ("ACP Model Traits
   Integration Polish"): 1,079,986-byte snapshot, 10 messages, 426 tool parts —
   **917 KB (85%) is capped tool output**, history overhead 1.00× (pure payload, not
   oplog bloat). Every agentic session now reaches the ~1 MB wasm-wedge zone in a day.
2. **The SessionRoom DO wedges at that size.** Every incident class of Aug 4–5 (wasm
   heap poison, use-after-free doc wrappers, replay CPU-limit death, silent shallow
   exports, import penalty box) exists because the DO materializes the doc through
   loro-wasm. At 1 MB docs these fire routinely: the fresh-device symptom is
   join-OK → 3 silent rejoins → `REJECTED 3` (penalty box) → no backfill, forever.

Priority (product decision): **flawless sync on ~1.2 Mbps links beats tool-output
transparency.** Small docs first, unwedgeable serving second; neither substitutes for
the other (stripping shrinks bytes, the relay makes serving them instant and reliable).

## Non-goals

- Replacing Loro. Clients keep real Loro docs: same schema, same offline
  commit-then-converge, same command-ledger-as-outbox. Only the DO stops parsing bytes.
- Windowed/partial doc loads. Out of scope; the host-published tail sidecar covers
  first-paint latency.
- Preserving full tool outputs inside the synced doc. They move to a lazy sidecar.

---

## Workstream A — thin docs (ship first, independently)

Keeps session docs small: the doc carries summaries, full payloads live elsewhere.

**A1. Fold strips outputs/diffs to summaries.** In `crates/doc/src/parts.rs`:
- `output`: keep first non-empty line, ≤160 chars (t3code ships 84 and users cope).
  Add additive fields `outputRef: Option<String>` (sidecar key) and
  `outputBytes: Option<u64>` so the UI can render "Show full output (12 KB)".
- `diff`: replace inline `ToolDiff` text with per-file stats `{path, additions,
  deletions}` (t3's shape) + `diffRef`. Kill `TOOL_DIFF_DOC_CAP` usage — the 32 KB/edit
  inline diff is a bigger bomb than outputs, currently unexercised only because the
  claude harness emits none.
- Old readers: both fields are already serde-additive; old app versions render the
  summary as if it were the output. Acceptable.

**A2. Output sidecar.** *[PARKED 2026-08-10 (v0.1.30): product call — no R2
uploads for now. The fold keeps small outputs inline (≤160 chars,
fence-stripped) and summarizes big ones; full text survives only in the
host's run journal. The machinery below (blob routes, `sidecar_payload`,
`apply_sidecar_refs`, `upload_tool_sidecar`, UI upgrade path) is built,
tested, and dormant — reintroduction is re-adding one call site in
`sessions.rs`. M1's rebuild still returns sidecar payloads; the C3 cutover
must decide their fate (upload or drop) before flipping `roomGen`.]* Full (still 4 KiB-capped at the harness boundary) outputs and
diffs go to R2 through a Worker route (no DO involvement — the `BLOBS` bucket already
exists): `PUT/GET /blob/{chatId}/{partId}` , owner-auth via the existing Worker JWT
check. Host uploads are debounced/batched per commit tick, fire-and-forget (a lost
upload degrades to "full output unavailable", never blocks the doc). UI fetches on
expand; offline shows the summary + a greyed affordance.

**A3. UI.** Tool-part expansion fetches `outputRef` lazily; render summary inline.

---

## Workstream B — chat2 edge room (dumb authenticated log relay)

New DO class `ChatRoom`, room name `chat2/{chatId}`, modeled line-for-line on
`RegistryRoom` (registry-room.ts, 425 LOC), not on SessionRoom. **No loro-wasm import
anywhere in the class.** The TS schema mirror (`edge/src/session-doc/`) and the wasm
bundling alias have since been removed with the s2 routes.

**Storage** (DO SQLite):
- `rows(seq INTEGER PRIMARY KEY, device TEXT, batch_id TEXT UNIQUE, bytes BLOB)` —
  opaque Loro update blobs. Per-row cap 1 MB (post-strip updates are KB-scale;
  oversized rows are rejected at the header, matching today's discipline).
- checkpoint blob via `blobs.ts` chunking + `meta`: `owner`, `seqFloor`,
  `checkpointFrontier BLOB` (opaque, client-written), `checkpointSize`,
  `checkpointSeq`, `tailDirty`.
- Sidecars: `tail` and `diff` blobs become **host-published** (`PUT /tail`,
  `PUT /diff`), served verbatim. The DO never materializes anything.

**Protocol** (binary WS frames: 1-byte type + JSON header + raw payload; no
loro-protocol, no base64 — 33% base64 overhead matters at 1.2 Mbps):
- `hello{cursor, device}` → `state{seqFloor, headSeq, checkpointSeq,
  checkpointFrontier, checkpointSize}` — metadata only, then:
- **Client-side precision** (replaces the server VV diff): client compares
  `checkpointFrontier` against its local doc frontiers.
  Included → skip the checkpoint, request `rows{after: max(cursor, checkpointSeq)}`.
  Not included → `GET /checkpoint` (HTTP, **Range-resumable** — a stored blob can
  resume at byte N; today's export-per-join cannot), then rows after `checkpointSeq`.
- `rows{after, excludeDevice}` → server streams rows `seq > after AND device !=
  excludeDevice` — you never re-download your own writes (matters exactly on the
  reconnect-after-offline-work path).
- `push{batchId, bytes}` → append + relay + `ack{batchId, seq}`. `batch_id UNIQUE`
  dedupes reconnect re-pushes server-side (client keeps a pending-unacked queue,
  registry-style; Loro re-import is a no-op so duplicates are safe end-to-end).
- `POST /checkpoint {seqCovered, frontier}` + chunked blob: owner-only, guarded by
  `seqCovered >= seqFloor` (floor-monotonic — the dumb replacement for the VV-monotonic
  R2 guard). Rows `seq <= seqCovered` deleted after commit.
- Presence: relay opaque ephemeral frames to live sockets with a 30 s TTL sweep —
  broadcast only, no EphemeralStore on the server.
- Validation kept (all wasm-free): auth/owner, frame shape, row size cap, per-device
  rate/byte quotas. Semantic garbage is contained per-user (owner-only rooms), skipped
  by client imports (malformed-entry philosophy), and erased by the next checkpoint.
- Reconnect replay is head-serialized: one pending batch is sent at a time and the
  next is armed by its ACK. A duplicate `batchId` is acknowledged before quota
  accounting, so lost ACKs do not consume the write budget.
- Ops: `GET /stats` (headSeq, seqFloor, rowBytes, checkpoint age), nightly
  seq-monotonic R2 backup (registry pattern), tombstone-free — rows are the log.

---

## Workstream C — Rust client + host duties

**C1. `crates/sync/src/chat_client.rs`** modeled on `registry.rs` (764 LOC): cursor
tracking, pending-push queue with batch ids, hello/backfill/live loop, frontier
comparison for checkpoint skip, reconnect/backoff/wake plumbing reused from the
existing supervisor machinery. The layered liveness model (ping lease, join deadline,
probe clock) carries over — those lessons are transport-level, not CRDT-level.

**C2. Store migration** (`crates/sync/src/store.rs` MIGRATIONS — append entry #N):
`ALTER TABLE snapshots ADD COLUMN cursor INTEGER; ADD COLUMN epoch INTEGER` — cursor
persisted **in the same transaction** as the snapshot bytes, so content and cursor
cannot diverge (this kills the restored-backup/copied-device redownload cases at the
root).

**C3. Host duties** (`doc_host.rs`):
- Checkpoint policy: post a full checkpoint when server `rowBytes > 512 KB` or
  `rows > 200` (from `/stats` piggybacked on hello) — thresholds cheap to tune later.
  History trim: shallow checkpoint only at a frontier older than RETAIN_DAYS, same
  aged-frontier discipline as today (the ws4 live-frontier lesson: an offline device's
  concurrent ops must never land behind a shallow root).
- Sidecars: hosts publish neither a tail nor a diff sidecar. Nothing read the tail once
  iOS spoke chat2 natively, and it had grown to 18% of the Durable Object bill; the
  route remains for a future instant-open reader (`docs/local-edge.md`). Remote
  clients read working-tree diffs through the device relay.
- Non-host owner devices may checkpoint as fallback if floor lag exceeds a high-water
  mark (any device holds the full doc; ~20 lines, ships later if ever needed — hosts
  must be online to execute commands anyway, so lag is bounded in practice).

**C4. iOS** (`apps/ios/Cypher/Sync/`): `ChatRoomClient.swift`, modeled on
`RegistryClient.swift`. Framing test vectors are shared across Rust/TS/Swift (registry
precedent).

---

## Doc lineage

**M1. Lineage epoch.** Every chat2 doc carries `meta.epoch = 2` (thin docs: summaries
in the doc, full payloads in the A2 sidecar or the host's run journal). The one-time
epoch rebuild that converted fat s2 docs during the cutover was removed after 0.3.41,
together with the s2 rooms themselves.

**M2. Room generation.** Registry chat rows carry a `roomGen` field (per-field HLC LWW
like everything else). Every chat is created with `roomGen: 2`; iOS connects a chat's
room only when the row says 2. Desktop hosts open every chat as chat2, including rows
that still say 1 or have no row yet.

**M3. Rebuild-vs-lineage on other devices.** A device opening a chat whose local doc
has `epoch < 2` discards that doc **after** re-queueing any of its own unresolved
commands as fresh entries, then adopts the chat2 checkpoint. The old snapshot row is
kept under a suffixed doc id. Importing it instead would duplicate every message,
because the two Loro histories are unrelated.
