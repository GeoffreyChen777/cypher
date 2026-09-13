use super::wire::*;
use crate::sync3::Error;
use cypher_proto::metadata::{MetadataRow as RegistryRow, OpKind, RowOp, apply_op};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

pub struct Journal {
    db: Connection,
    scope: Scope,
}
#[derive(Debug, Clone)]
pub struct Pending {
    pub id: String,
    pub request: String,
    pub hash: String,
}
pub struct Window {
    pub next: Option<String>,
    pub rows: Vec<RegistryRow>,
}
impl Journal {
    pub fn open(path: &Path, scope: Scope) -> Result<Self, Error> {
        scope.validate()?;
        let mut db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: u32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if ![0, 3].contains(&version) {
            return Err(error("unsupported_workspace_format"));
        }
        let tables: u64 = db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )?;
        let ours: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='workspace3_meta')", [], |r| r.get(0))?;
        if (version == 0 && tables != 0) || (version == 3 && !ours) {
            return Err(error("unsupported_workspace_format"));
        }
        #[cfg(unix)]
        if path != Path::new(":memory:") {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            for suffix in ["-wal", "-shm"] {
                let mut sidecar = path.as_os_str().to_os_string();
                sidecar.push(suffix);
                match std::fs::set_permissions(
                    std::path::PathBuf::from(sidecar),
                    std::fs::Permissions::from_mode(0o600),
                ) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS workspace3_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),
              scope TEXT NOT NULL,cursor INTEGER NOT NULL DEFAULT 0,clock_ms INTEGER NOT NULL DEFAULT 0,clock_counter INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS workspace3_rows(kind TEXT NOT NULL,id TEXT NOT NULL,seq INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(kind,id));
            CREATE TABLE IF NOT EXISTS workspace3_outbox(ordinal INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,request TEXT NOT NULL,hash TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS workspace3_pending(batch TEXT NOT NULL,position INTEGER NOT NULL,kind TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,
              PRIMARY KEY(batch,position),FOREIGN KEY(batch) REFERENCES workspace3_outbox(id) ON DELETE CASCADE);
            CREATE INDEX IF NOT EXISTS workspace3_pending_row ON workspace3_pending(kind,id);
            PRAGMA foreign_keys=ON;")?;
        let tx = db.transaction()?;
        let encoded = serde_json::to_string(&scope)?;
        tx.execute(
            "INSERT OR IGNORE INTO workspace3_meta(singleton,scope) VALUES(1,?)",
            [&encoded],
        )?;
        let stored: String = tx.query_row(
            "SELECT scope FROM workspace3_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if serde_json::from_str::<Scope>(&stored)? != scope {
            return Err(error("workspace_scope_mismatch"));
        }
        tx.pragma_update(None, "user_version", 3)?;
        tx.commit()?;
        Ok(Self { db, scope })
    }
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn canonical_row(&self, kind: &str, id: &str) -> Result<Option<RegistryRow>, Error> {
        canonical(&self.db, kind, id)
    }
    pub fn cursor(&self) -> Result<u64, Error> {
        Ok(self.db.query_row(
            "SELECT cursor FROM workspace3_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )?)
    }
    /// HLC generation and the immutable outbox frame commit together. No
    /// network access is required to stage an offline metadata edit.
    pub fn mutate(
        &mut self,
        kind: &str,
        id: &str,
        op: OpKind,
        set: Option<BTreeMap<String, Value>>,
        now: u64,
    ) -> Result<String, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let batch = stage(&tx, &self.scope, kind, id, op, set, now)?;
        tx.commit()?;
        Ok(batch)
    }
    /// Domain cascades are one local transaction. Generated domain clocks are
    /// intentionally not imported: this journal assigns/observes durable HLCs.
    pub fn mutate_many(&mut self, operations: &[RowOp], now: u64) -> Result<(), Error> {
        if operations.len() > 4096 {
            return Err(error("workspace_mutation_too_large"));
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for op in operations {
            stage(
                &tx,
                &self.scope,
                &op.kind,
                &op.id,
                op.op,
                op.set.clone(),
                now,
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn pending(&self) -> Result<Option<Pending>, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT id,request,hash FROM workspace3_outbox ORDER BY ordinal LIMIT 1",
                [],
                |r| {
                    Ok(Pending {
                        id: r.get(0)?,
                        request: r.get(1)?,
                        hash: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }
    /* The stage implementation lives below the public journal API. */
}

fn stage(
    tx: &Connection,
    scope: &Scope,
    kind: &str,
    id: &str,
    op: OpKind,
    set: Option<BTreeMap<String, Value>>,
    now: u64,
) -> Result<String, Error> {
    let (ms, counter): (u64, u32) = tx.query_row(
        "SELECT clock_ms,clock_counter FROM workspace3_meta WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (ms, counter) = if now > ms {
        (now, 0)
    } else if counter == 999999 {
        (ms + 1, 0)
    } else {
        (ms, counter + 1)
    };
    if ms >= 10_000_000_000_000 {
        return Err(error("clock_exhausted"));
    }
    let operation = RowOp {
        kind: kind.into(),
        id: id.into(),
        op,
        set,
        hlc: format!("{ms:013}-{counter:06}-{}", scope.actor),
    };
    if scope.endpoint == "local" {
        super::wire::local_operation(&operation, &scope.actor)?;
    } else {
        super::wire::operation(&operation, &scope.actor)?;
    }
    let pending_count: u64 =
        tx.query_row("SELECT COUNT(*) FROM workspace3_pending", [], |r| r.get(0))?;
    if pending_count >= 1024 {
        return Err(error("workspace_outbox_full"));
    }
    let old = view(&tx, kind, id)?;
    let (next, _) = apply_op(old.as_ref(), &operation);
    if next
        .as_ref()
        .is_some_and(|r| serde_json::to_vec(r).map_or(true, |b| b.len() > ROW_BYTES))
    {
        return Err(error("row_too_large"));
    }
    let batch = uuid::Uuid::new_v4().to_string();
    if scope.endpoint == "local" {
        if let Some(mut next) = next {
            let cursor: u64 = tx.query_row(
                "SELECT cursor FROM workspace3_meta WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            if cursor == MAX_SAFE {
                return Err(error("sequence_exhausted"));
            }
            next.seq = cursor + 1;
            put(&tx, &next)?;
            tx.execute(
                "UPDATE workspace3_meta SET cursor=? WHERE singleton=1",
                [next.seq],
            )?;
        }
        tx.execute(
            "UPDATE workspace3_meta SET clock_ms=?,clock_counter=? WHERE singleton=1",
            params![ms, counter],
        )?;
        return Ok(batch);
    }
    let request = serde_json::to_string(
        &json!({ "version": 3, "type": "push", "id": batch, "ops": [&operation] }),
    )?;
    let hash = format!("{:x}", Sha256::digest(request.as_bytes()));
    tx.execute(
        "INSERT INTO workspace3_outbox(id,request,hash) VALUES(?,?,?)",
        params![batch, request, hash],
    )?;
    tx.execute(
        "INSERT INTO workspace3_pending VALUES(?,?,?,?,?)",
        params![batch, 0, kind, id, serde_json::to_string(&operation)?],
    )?;
    tx.execute(
        "UPDATE workspace3_meta SET clock_ms=?,clock_counter=? WHERE singleton=1",
        params![ms, counter],
    )?;
    Ok(batch)
}
impl Journal {
    pub fn apply_page(&mut self, after: u64, page: &Page) -> Result<(), Error> {
        if self.scope.endpoint == "local" {
            return Err(error("local_authority_cannot_join_cloud"));
        }
        page.validate(after)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: u64 = tx.query_row(
            "SELECT cursor FROM workspace3_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if current != after {
            return Err(error("workspace_cursor_changed"));
        }
        for row in &page.rows {
            put(&tx, row)?;
        }
        tx.execute(
            "UPDATE workspace3_meta SET cursor=? WHERE singleton=1",
            [page.next],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// An exact request-byte ACK retires only that batch, after its returned
    /// canonical rows commit. It never advances the contiguous pull cursor.
    pub fn acknowledge(
        &mut self,
        id: &str,
        hash: &str,
        through: u64,
        rows: &[RegistryRow],
    ) -> Result<(), Error> {
        if through > MAX_SAFE || rows.len() > MAX_OPS {
            return Err(error("invalid_workspace_ack"));
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expected: Option<String> = tx
            .query_row("SELECT hash FROM workspace3_outbox WHERE id=?", [id], |r| {
                r.get(0)
            })
            .optional()?;
        if expected.as_deref() != Some(hash) {
            return Err(error("workspace_ack_mismatch"));
        }
        let mut seen = std::collections::HashSet::new();
        for row in rows {
            if row.seq > through || !seen.insert((&row.kind, &row.id)) {
                return Err(error("invalid_workspace_ack"));
            }
            let target: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM workspace3_pending WHERE batch=? AND kind=? AND id=?)",
                params![id, row.kind, row.id],
                |r| r.get(0),
            )?;
            if !target {
                return Err(error("workspace_ack_mismatch"));
            }
            put(&tx, row)?;
        }
        // Missing update targets are legitimate no-ops; otherwise ACK must
        // carry the canonical target before its optimistic overlay is removed.
        let mut query = tx.prepare("SELECT body FROM workspace3_pending WHERE batch=?")?;
        for body in query.query_map([id], |r| r.get::<_, String>(0))? {
            let op: RowOp = serde_json::from_str(&body?)?;
            if !seen.iter().any(|(k, i)| **k == op.kind && **i == op.id)
                && (op.op != OpKind::Update || canonical(&tx, &op.kind, &op.id)?.is_some())
            {
                return Err(error("workspace_ack_missing_row"));
            }
        }
        drop(query);
        tx.execute("DELETE FROM workspace3_outbox WHERE id=?", [id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn row(&self, kind: &str, id: &str) -> Result<Option<RegistryRow>, Error> {
        view(&self.db, kind, id)
    }
    pub fn window(&self, kind: &str, after: &str, limit: usize) -> Result<Window, Error> {
        if !super::wire::kind(kind) || limit == 0 || limit > MAX_ROWS {
            return Err(error("invalid_workspace_window"));
        }
        let mut query = self.db.prepare(
            "SELECT id FROM workspace3_rows WHERE kind=? AND id>?
            UNION SELECT id FROM workspace3_pending WHERE kind=? AND id>? ORDER BY id LIMIT ?",
        )?;
        let ids = query
            .query_map(params![kind, after, kind, after, limit], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut result = Window {
            next: None,
            rows: vec![],
        };
        let mut bytes = 1024;
        for id in ids {
            if let Some(row) = view(&self.db, kind, &id)? {
                let n = serde_json::to_vec(&row)?.len();
                if bytes + n > FRAME_BYTES {
                    break;
                }
                bytes += n;
                result.rows.push(row);
            }
            result.next = Some(id);
        }
        Ok(result)
    }
}
fn canonical(db: &Connection, kind: &str, id: &str) -> Result<Option<RegistryRow>, Error> {
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM workspace3_rows WHERE kind=? AND id=?",
            params![kind, id],
            |r| r.get(0),
        )
        .optional()?;
    body.map(|body| serde_json::from_str(&body).map_err(Error::from))
        .transpose()
}
fn view(db: &Connection, kind: &str, id: &str) -> Result<Option<RegistryRow>, Error> {
    let mut row = canonical(db, kind, id)?;
    let mut query = db.prepare(
        "SELECT p.body FROM workspace3_pending p JOIN workspace3_outbox o ON o.id=p.batch
        WHERE p.kind=? AND p.id=? ORDER BY o.ordinal,p.position",
    )?;
    for body in query.query_map(params![kind, id], |r| r.get::<_, String>(0))? {
        row = apply_op(row.as_ref(), &serde_json::from_str::<RowOp>(&body?)?).0;
    }
    Ok(row)
}
fn put(db: &Connection, incoming: &RegistryRow) -> Result<(), Error> {
    row(incoming)?;
    if let Some(old) = canonical(db, &incoming.kind, &incoming.id)? {
        if old.seq > incoming.seq {
            return Ok(());
        }
        if old.seq == incoming.seq {
            if old != *incoming {
                return Err(error("workspace_row_conflict"));
            }
            return Ok(());
        }
    }
    for c in incoming.clocks.values().chain(incoming.del_hlc.iter()) {
        let (ms, counter) = clock(c)?;
        db.execute("UPDATE workspace3_meta SET clock_ms=?,clock_counter=? WHERE singleton=1 AND (clock_ms<? OR (clock_ms=? AND clock_counter<?))",
            params![ms,counter,ms,ms,counter])?;
    }
    db.execute("INSERT INTO workspace3_rows VALUES(?,?,?,?) ON CONFLICT(kind,id) DO UPDATE SET seq=excluded.seq,body=excluded.body",
        params![incoming.kind,incoming.id,incoming.seq,serde_json::to_string(incoming)?])?;
    Ok(())
}
