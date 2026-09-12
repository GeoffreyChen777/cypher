import {
  applyOperation, byteLength, canonical, isId, MAX_BATCH_OPS, MAX_FRAME_BYTES, MAX_MESSAGE_BYTES,
  reject, safeInteger, validateOperation, VERSION,
  type EntityKind, type Operation, type Projection, type ProjectionStore, type Reply, type Row,
} from "./sync3-protocol";

/** Real DO SQLite, one bounded materialized entity per row, not a whole-chat
 * JSON rewrite on each token. All append/dedupe/reducer writes share a txn. */
export class Sync3Log implements ProjectionStore {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS v3_meta (
      singleton INTEGER PRIMARY KEY CHECK(singleton=1), account TEXT NOT NULL,
      epoch INTEGER NOT NULL, owner TEXT NOT NULL, owner_epoch INTEGER NOT NULL, head INTEGER NOT NULL)`);
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS v3_events (
      seq INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE, operation TEXT NOT NULL)`);
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS v3_entities (
      kind TEXT NOT NULL, id TEXT NOT NULL, body TEXT NOT NULL, run_id TEXT, created_seq INTEGER,
      PRIMARY KEY(kind,id))`);
    storage.sql.exec("CREATE INDEX IF NOT EXISTS v3_entity_run ON v3_entities(kind,run_id)");
    storage.sql.exec("CREATE UNIQUE INDEX IF NOT EXISTS v3_message_order ON v3_entities(created_seq) WHERE kind='messages'");
  }
  initialize(account: string, owner: string): void {
    if (!account || !isId(owner)) reject("invalid_owner");
    this.storage.transactionSync(() => {
      const existing = this.meta();
      if (existing) {
        if (existing.account !== account) reject("forbidden");
        if (existing.owner !== owner) reject("already_initialized");
        return;
      }
      this.storage.sql.exec("INSERT INTO v3_meta VALUES(1,?,1,?,1,0)", account, owner);
    });
  }
  meta(): { account: string; epoch: number; owner: string; owner_epoch: number; head: number } | undefined {
    return this.storage.sql.exec<{ account: string; epoch: number; owner: string; owner_epoch: number; head: number }>(
      "SELECT account,epoch,owner,owner_epoch,head FROM v3_meta WHERE singleton=1").toArray()[0];
  }
  state(): Extract<Reply, { type: "state" }> {
    const m = this.meta();
    if (!m) reject("not_initialized");
    return { type: "state", version: VERSION, epoch: m.epoch, owner: m.owner, ownerEpoch: m.owner_epoch, head: m.head };
  }
  get<K extends EntityKind>(kind: K, id: string): Projection[K][string] | undefined {
    const row = this.storage.sql.exec<{ body: string }>(
      "SELECT body FROM v3_entities WHERE kind=? AND id=?", kind, id).toArray()[0];
    return row ? JSON.parse(row.body) : undefined;
  }
  set<K extends EntityKind>(kind: K, id: string, value: Projection[K][string]): void {
    const body = JSON.stringify(value);
    // Explicit failure rather than approaching workerd's ~2 MiB SQLite cell
    // limit. Transcript chunk rollover is a client duty, not silent truncation.
    if (byteLength(body) > MAX_FRAME_BYTES * 4) reject("entity_too_large");
    const runId = (value as { runId?: string | null }).runId ?? null;
    const createdSeq = (value as { createdSeq?: number }).createdSeq ?? null;
    this.storage.sql.exec("INSERT INTO v3_entities(kind,id,body,run_id,created_seq) VALUES(?,?,?,?,?) ON CONFLICT(kind,id) DO UPDATE SET body=excluded.body,run_id=excluded.run_id",
      kind, id, body, runId, createdSeq);
  }
  hasAcceptedRun(runId: string): boolean {
    return this.storage.sql.exec("SELECT 1 FROM v3_entities WHERE kind='commands' AND run_id=? AND json_extract(body,'$.command.status') IN ('pending','applied') LIMIT 1", runId).toArray().length > 0;
  }
  hasOpenMessage(runId: string): boolean {
    return this.storage.sql.exec("SELECT 1 FROM v3_entities WHERE kind='messages' AND run_id=? AND json_extract(body,'$.entry.status')='streaming' LIMIT 1", runId).toArray().length > 0;
  }
  hasExecution(commandId?: string): boolean {
    return (commandId === undefined
      ? this.storage.sql.exec("SELECT 1 FROM v3_entities WHERE kind='executions' AND json_extract(body,'$.closed')=0 LIMIT 1")
      : this.storage.sql.exec("SELECT 1 FROM v3_entities WHERE kind='executions' AND json_extract(body,'$.commandId')=? LIMIT 1", commandId)).toArray().length > 0;
  }
  hasUnresolvedExecution(): boolean {
    const pending = this.storage.sql.exec(`SELECT 1 FROM v3_entities c
      LEFT JOIN v3_entities r ON r.kind='runs' AND r.id=c.run_id
      WHERE c.kind='commands' AND json_extract(c.body,'$.acceptedOpId') IS NOT NULL AND
      (json_extract(c.body,'$.command.status')='pending' OR
       (json_extract(c.body,'$.command.status')='applied' AND
        (r.id IS NULL OR json_extract(r.body,'$.outcome') IS NULL))) LIMIT 1`).toArray();
    return pending.length > 0 || this.storage.sql.exec(
      "SELECT 1 FROM v3_entities WHERE kind='runs' AND json_extract(body,'$.outcome') IS NULL LIMIT 1").toArray().length > 0;
  }
  /** Local bounded projection read; not a new remotely exposed endpoint. */
  messageWindow(before?: number, limit = 32): { through: number; messages: Projection["messages"][string][] } {
    if (!safeInteger(limit) || limit < 1 || limit > 32 || (before !== undefined && !safeInteger(before))) reject("invalid_window");
    return this.storage.transactionSync(() => {
      const through = this.state().head, messages: Projection["messages"][string][] = [];
      let used = 0;
      for (const row of this.storage.sql.exec<{ id: string; body: string; created_seq: number }>(
        "SELECT id,body,created_seq FROM v3_entities INDEXED BY v3_message_order WHERE kind='messages' AND created_seq<? ORDER BY created_seq DESC LIMIT ?",
        before ?? Number.MAX_SAFE_INTEGER + 1, limit)) {
        const size = byteLength(row.body);
        if (size > MAX_MESSAGE_BYTES) reject("message_too_large");
        if (used + size > 1024 * 1024) break;
        const message = JSON.parse(row.body) as Projection["messages"][string];
        if (row.created_seq <= 0 || row.created_seq > through || message.createdSeq !== row.created_seq || message.entry.id !== row.id) reject("invalid_projection");
        used += size; messages.push(message);
      }
      return { through, messages: messages.reverse() };
    });
  }
  append(operations: Operation[], actor?: string): Extract<Reply, { type: "ack" }> {
    if (!operations.length || operations.length > MAX_BATCH_OPS) reject("invalid_batch");
    if (byteLength(JSON.stringify({ type: "push", version: 3, operations })) > MAX_FRAME_BYTES) reject("frame_too_large");
    operations.forEach(op => {
      validateOperation(op);
      if (actor && op.actor !== actor) reject("actor_mismatch");
    });
    return this.storage.transactionSync(() => {
      const m = this.meta();
      if (!m) reject("not_initialized");
      const receipts: { id: string; seq: number }[] = [];
      for (const op of operations) {
        const body = canonical(op);
        const existing = this.storage.sql.exec<{ seq: number; operation: string }>(
          "SELECT seq,operation FROM v3_events WHERE id=?", op.id).toArray()[0];
        if (existing) {
          if (existing.operation !== body) reject("operation_id_conflict");
          // A committed old-epoch retry is still acknowledged. It does not
          // re-execute or mutate the new owner's state.
          receipts.push({ id: op.id, seq: existing.seq });
          continue;
        }
        if (!safeInteger(m.head + 1)) reject("sequence_exhausted");
        applyOperation(this, op, m.owner, m.owner_epoch, m.head + 1);
        this.storage.sql.exec("INSERT INTO v3_events(seq,id,operation) VALUES(?,?,?)", ++m.head, op.id, body);
        receipts.push({ id: op.id, seq: m.head });
      }
      this.storage.sql.exec("UPDATE v3_meta SET head=? WHERE singleton=1", m.head);
      return { type: "ack", version: VERSION, epoch: m.epoch, receipts };
    });
  }
  page(epoch: number, after: number, through: number): Extract<Reply, { type: "page" }> {
    const m = this.meta();
    if (!m) reject("not_initialized");
    if (epoch !== m.epoch) reject("epoch_mismatch");
    if (![after, through].every(safeInteger) || after > through || through > m.head) reject("invalid_cursor");
    const rows: Row[] = [];
    let next = after, used = 1024;
    for (const row of this.storage.sql.exec<{ seq: number; operation: string }>(
      "SELECT seq,operation FROM v3_events WHERE seq>? AND seq<=? ORDER BY seq LIMIT 64", after, through)) {
      const size = byteLength(row.operation) + 64;
      if (used + size > MAX_FRAME_BYTES) break;
      if (row.seq !== next + 1) reject("log_gap");
      rows.push({ seq: row.seq, operation: JSON.parse(row.operation) });
      next = row.seq; used += size;
    }
    if (next === after && next < through) reject("log_gap");
    return { type: "page", version: VERSION, epoch, through, next, rows, done: next === through };
  }
  /** Explicit idle ownership transfer only; never called by presence expiry. */
  transferOwner(expectedEpoch: number, owner: string): void {
    if (!isId(owner)) reject("invalid_owner");
    this.storage.transactionSync(() => {
      const m = this.meta();
      if (!m || m.owner_epoch !== expectedEpoch) reject("stale_owner_epoch");
      if (!safeInteger(expectedEpoch + 1)) reject("sequence_exhausted");
      const uncertain = this.storage.sql.exec(`SELECT 1 FROM v3_entities c
        LEFT JOIN v3_entities r ON r.kind='runs' AND r.id=c.run_id
        WHERE c.kind='commands' AND c.run_id IS NOT NULL
        AND json_extract(c.body,'$.command.status') IN ('pending','applied')
        AND (r.id IS NULL OR json_extract(r.body,'$.outcome') IS NULL) LIMIT 1`).toArray();
      const live = this.storage.sql.exec("SELECT 1 FROM v3_entities WHERE kind='runs' AND json_extract(body,'$.outcome') IS NULL LIMIT 1").toArray();
      const open = this.storage.sql.exec("SELECT 1 FROM v3_entities WHERE kind='messages' AND json_extract(body,'$.entry.status')='streaming' LIMIT 1").toArray();
      if (uncertain.length || live.length || open.length || this.hasExecution()) reject("execution_unresolved");
      this.storage.sql.exec("UPDATE v3_meta SET owner=?,owner_epoch=? WHERE singleton=1", owner, expectedEpoch + 1);
    });
  }
}
