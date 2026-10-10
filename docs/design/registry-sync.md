# Registry sync — the workspace index without a CRDT

The registry is the workspace index behind the sidebar: devices, spaces, chats and session
status. It syncs as a table of rows with per-field last-writer-wins clocks through the
RegistryRoom Durable Object (`reg1/{org}/{user}`), not as a CRDT document.

## Why

The sidebar's data is a keyed set of rows with independently updatable scalar fields — a
replicated table, not a document. Every write already flows through one Durable Object, so
that object can be the authority and keep **current state only**:

- 10,000 sessions ≈ a few MB of rows, bounded forever; history is discarded the moment it
  stops being true.
- The Durable Object cold-starts by reading a SQLite table: no history replay, no wasm, no
  compaction. A CRDT doc here kept growing history that the Durable Object had to replay or
  re-export in wasm under a CPU limit, and that is how such a room wedges.
- Same-field conflicts resolve by last-writer-wins, as a Loro map would, in a few dozen
  lines of auditable code (per-field hybrid logical clocks).

Session docs (transcripts) stay on Loro, because concurrent text has no "newest wins"
answer ([chat2 sync](chat2-sync.md)).

## Topology

```
engine A ── RegistryDoc (rows + pending ops, SQLite-persisted) ── RegistryClient ─┐
                                                                                  ├─ RegistryRoom DO (reg1/{org}/{user})
engine B ── RegistryDoc ── RegistryClient ────────────────────────────────────────┘   rows table + seq counter
```

- **RegistryRoom DO** (`apps/edge/src/registry/registry-room.ts`): authoritative row table in DO SQLite.
  Applies pushed ops with per-field LWW (HLC compare), bumps a monotonic `seq` per batch,
  broadcasts merged rows to every socket. Cursor sync: a client joining with `cursor=N`
  gets only rows with `seq > N`. Pure merge logic lives in `registry-core.ts` (unit-tested;
  mirrored 1:1 in Rust).
- **RegistryDoc** (`crates/doc/src/registry.rs`): the client-side table. Authoritative rows
  (server truth) + a pending-op queue (offline writes, replayed as an overlay for reads).
  Serialized whole into `DocsStore` (`registry1` snapshot row) — offline restarts keep
  full state, cursor, and queue.
- **RegistryClient** (`crates/sync/src/registry.rs`): WS transport — hello/cursor handshake,
  push/ack, rows broadcasts, presence, probe/redial liveness, reconnect with backoff. Fills
  the `RoomStatsSnapshot` that the SyncStatus RPC and `cypher sync` render.

## Wire protocol (JSON text frames)

Client→server:

| frame | shape |
|---|---|
| hello | `{t:"hello", cursor: number\|null, device}` — must be first |
| push | `{t:"push", batch, ops: [Op]}` |
| presence | `{t:"presence", at}` |
| probe | `{t:"probe"}` |

Server→client:

| frame | shape |
|---|---|
| state | `{t:"state", seq, full, rows, gcFloor, presence}` — hello reply; `full=false` ⇒ rows are the `seq > cursor` delta |
| rows | `{t:"rows", seq, rows}` — merged full rows for every touched row, broadcast to all sockets |
| ack | `{t:"ack", batch, seq, applied}` |
| presence | `{t:"presence", device, at}` |
| probe-ok | `{t:"probe-ok", seq}` |

`Op = {kind, id, op: "upsert"|"update"|"delete", set?: {field: value|null}, hlc, clocks?}`
`Row = {kind, id, seq, deleted, delHlc?, fields, clocks}`

## Merge rules (identical in TS, Rust and Swift; shared test vectors)

The vectors are in `protocol/vectors/registry-core-v1.json`; see
[protocol/README.md](../../protocol/README.md).

- HLC strings `"{ms:013}-{counter:06}-{device}"` — lexicographic order = causal order,
  device id breaks ties totally.
- A field set applies iff its clock > the stored clock for that field. `null` deletes the
  field (still a clocked write).
- `update` ops never create or revive rows; `upsert` creates, and revives a tombstone iff
  newer than `delHlc`.
- `delete` tombstones iff newer than `delHlc`; fields and clocks are cleared. Tombstones GC after 30 days (daily alarm); `gcFloor` forces a full resync for
  cursors older than the horizon.
- Re-applying any op is a no-op (`>` compare) — reconnect re-pushes are idempotent by
  construction.

## Cursor + recovery

- `seq` bumps once per accepted batch; every touched row is stamped with it. Delta sync is
  `SELECT * WHERE seq > cursor`.
- Server behind the client (`state.seq < cursor`, e.g. wiped DO storage): the client keeps
  its rows and re-seeds the server from them with **original per-field clocks** (`clocks`
  on the op), so nothing is lost.
- Local-only rows on a full resync re-seed the same way; unpushed writes always live in
  the pending queue and replay over whatever the server returns.

## Presence and operations

- Presence is ephemeral: an in-memory map in the Durable Object, 15 s client beats and a
  45 s freshness window in the host, which falls back to probing a peer over the device
  relay.
- `/registry/:orgId/stats` carries per-device push attribution (`pushOutcomes`), the
  surface for debugging a device whose writes are not landing.
- The daily alarm takes a nightly R2 backup of the row table (seq-monotonic guard) and GCs
  tombstones. `/registry/:orgId/rows` is a repair read, and `POST /registry/:orgId/reset`
  is an operator wipe; the devices re-seed it automatically.
