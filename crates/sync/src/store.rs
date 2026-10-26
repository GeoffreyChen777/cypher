//! `DocsStore` — local SQLite persistence for doc snapshots and the
//! processed-command ledger (ARCHITECTURE §2 command plane: entries are marked
//! processed BEFORE execution so a crash can never double-execute a command).
//!
//! A chat doc is stored as a base snapshot plus the Loro updates committed on
//! top of it ([`DocsStore::write_doc`]): rewriting a multi-MB snapshot per
//! streamed commit or synced row was gigabytes of disk writes a day, for a
//! few KB of actual change. Readers that want one blob get the merge.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::now_ms;

/// Errors surfaced by [`DocsStore`].
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("outbox invariant: {0}")]
    Outbox(String),
    #[error("stored doc merge: {0}")]
    Merge(String),
}

/// A doc as stored: the base snapshot and the updates appended since, in
/// commit order. `generation` names the base — see [`DocWrite::Delta`].
pub struct StoredDoc {
    pub base: Vec<u8>,
    pub deltas: Vec<Vec<u8>>,
    pub cursor: u64,
    pub epoch: u32,
    pub generation: i64,
}

impl StoredDoc {
    /// Bytes on disk (base + deltas).
    pub fn stored_len(&self) -> usize {
        self.base.len() + self.deltas.iter().map(Vec::len).sum::<usize>()
    }

    /// One snapshot blob: the base itself, or base + deltas merged through Loro.
    pub fn into_snapshot(self) -> Result<Vec<u8>, StoreError> {
        if self.deltas.is_empty() {
            return Ok(self.base);
        }
        let doc = loro::LoroDoc::new();
        doc.import(&self.base)
            .map_err(|e| StoreError::Merge(e.to_string()))?;
        doc.import_batch(&self.deltas)
            .map_err(|e| StoreError::Merge(e.to_string()))?;
        doc.export(loro::ExportMode::Snapshot)
            .map_err(|e| StoreError::Merge(e.to_string()))
    }
}

/// The content half of one [`DocsStore::write_doc`] call.
pub enum DocWrite<'a> {
    /// Replace the base snapshot and drop every delta (also compaction).
    Full(&'a [u8]),
    /// Append one update on top of the base named `generation`. Refused (the
    /// call returns `Ok(None)`, nothing written) when the base was replaced since:
    /// the update would extend a snapshot it was never computed against.
    Delta { generation: i64, bytes: &'a [u8] },
    /// Content already stored under base `generation`; only the cursor (if any)
    /// and the outbox retirement land. Refused like `Delta` on a replaced base.
    Unchanged { generation: i64 },
}

/// Ordered, append-only migrations. Each entry runs once inside a transaction;
/// `schema_migrations` records what has been applied.
const MIGRATIONS: &[&str] = &[
    // v1 — snapshots + processed-command ledger
    "CREATE TABLE snapshots (
        doc_id   TEXT PRIMARY KEY,
        bytes    BLOB NOT NULL,
        saved_at INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE processed_commands (
        command_id   TEXT PRIMARY KEY,
        processed_at INTEGER NOT NULL
     ) STRICT;",
    // v2 — chat2 room cursor + doc epoch (docs/design/chat2-sync.md C2). The cursor
    // is persisted in the SAME transaction as the snapshot bytes, so content
    // and cursor cannot diverge (restored backups / copied devices simply
    // redownload from their honest cursor). `epoch` marks rebuild lineage
    // (M1/M3): 2 = thin chat2 rebuild; NULL/0 = pre-migration s2 doc.
    "ALTER TABLE snapshots ADD COLUMN cursor INTEGER;
     ALTER TABLE snapshots ADD COLUMN epoch INTEGER;",
    // v3 — durable chat2 outbox. A batch is retired only after its matching ACK.
    "CREATE TABLE chat_outbox (
        doc_id TEXT NOT NULL,
        batch_id TEXT PRIMARY KEY,
        bytes BLOB NOT NULL,
        queued_at INTEGER NOT NULL
     ) STRICT;
     CREATE INDEX chat_outbox_doc ON chat_outbox(doc_id, queued_at);",
    // v4 — incremental persistence. `snapshot_deltas` holds the Loro updates
    // committed on top of a snapshot row (rowid = commit order); `generation` names
    // the base they extend, allocated from `store_counters` so a deleted and
    // re-created row can never reuse one. `registry_items` stores the
    // registry replica one row per item instead of one JSON blob.
    "ALTER TABLE snapshots ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;
     CREATE TABLE snapshot_deltas (
        doc_id TEXT NOT NULL,
        bytes  BLOB NOT NULL
     ) STRICT;
     CREATE INDEX snapshot_deltas_doc ON snapshot_deltas(doc_id);
     CREATE TABLE store_counters (
        name  TEXT PRIMARY KEY,
        value INTEGER NOT NULL
     ) STRICT;
     INSERT INTO store_counters (name, value) VALUES ('snapshot_gen', 0);
     CREATE TABLE registry_items (
        key   TEXT PRIMARY KEY,
        bytes BLOB NOT NULL
     ) STRICT;",
];

/// SQLite-backed store under a data directory (`{data_dir}/docs.sqlite3`).
///
/// Holds warm-open doc snapshots (the DO room is authoritative; these make
/// cold starts instant and offline restarts possible) and the command ledger
/// that gives command execution mark-BEFORE-execute idempotence.
pub struct DocsStore {
    conn: Mutex<Connection>,
}

impl DocsStore {
    /// Open (creating directory, database, and schema as needed).
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref();
        std::fs::create_dir_all(data_dir)?;
        let mut conn = Connection::open(data_dir.join("docs.sqlite3"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Latest saved snapshot for `doc_id`, if any (deltas merged in).
    pub fn load_snapshot(&self, doc_id: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.load_doc(doc_id)?
            .map(StoredDoc::into_snapshot)
            .transpose()
    }

    /// The stored doc as base + deltas, for writers that continue appending.
    pub fn load_doc(&self, doc_id: &str) -> Result<Option<StoredDoc>, StoreError> {
        let mut conn = self.conn();
        // One read transaction: a writer can't compact between the two reads.
        let tx = conn.transaction()?;
        let Some((base, cursor, epoch, generation)) = tx
            .query_row(
                "SELECT bytes, cursor, epoch, generation FROM snapshots WHERE doc_id = ?1",
                params![doc_id],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                        row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u32,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        let deltas = {
            let mut stmt =
                tx.prepare("SELECT bytes FROM snapshot_deltas WHERE doc_id = ?1 ORDER BY rowid")?;
            stmt.query_map(params![doc_id], |row| row.get(0))?
                .collect::<Result<Vec<Vec<u8>>, _>>()?
        };
        tx.commit()?;
        Ok(Some(StoredDoc {
            base,
            deltas,
            cursor,
            epoch,
            generation,
        }))
    }

    /// Save (upsert) the snapshot for `doc_id`.
    pub fn save_snapshot(&self, doc_id: &str, bytes: &[u8]) -> Result<(), StoreError> {
        self.write_doc(doc_id, DocWrite::Full(bytes), None, None)?;
        Ok(())
    }

    /// The one doc write: content (full base, appended delta, or nothing),
    /// optionally the chat2 `(cursor, epoch)` and the retirement of an outbox
    /// batch — ONE transaction, so bytes and cursor can never disagree (the
    /// C2 rule) and a retired batch is always covered by stored content.
    ///
    /// Returns the base generation now stored, or `None` when a `Delta` /
    /// `Unchanged` write named a base that was replaced (nothing written; the
    /// caller rewrites in full). A cursor-less write keeps the row's cursor
    /// and epoch; a new row gets none.
    pub fn write_doc(
        &self,
        doc_id: &str,
        write: DocWrite<'_>,
        cursor: Option<(u64, u32)>,
        retire_outbox: Option<&str>,
    ) -> Result<Option<i64>, StoreError> {
        let cursor = cursor
            .map(|(cursor, epoch)| {
                i64::try_from(cursor)
                    .map(|cursor| (cursor, epoch as i64))
                    .map_err(|_| StoreError::Outbox("cursor overflow".into()))
            })
            .transpose()?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if let Some(batch_id) = retire_outbox {
            let owner: Option<String> = tx
                .query_row(
                    "SELECT doc_id FROM chat_outbox WHERE batch_id=?1",
                    [batch_id],
                    |r| r.get(0),
                )
                .optional()?;
            if owner.as_deref() != Some(doc_id) {
                return Err(StoreError::Outbox(
                    "ACK does not name this document's queued batch".into(),
                ));
            }
        }
        let generation = match write {
            DocWrite::Full(bytes) => {
                let generation: i64 = tx.query_row(
                    "UPDATE store_counters SET value = value + 1 WHERE name = 'snapshot_gen' RETURNING value",
                    [],
                    |row| row.get(0),
                )?;
                match cursor {
                    Some((cursor, epoch)) => tx.execute(
                        "INSERT INTO snapshots (doc_id, bytes, saved_at, cursor, epoch, generation)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT(doc_id) DO UPDATE SET bytes = excluded.bytes,
                             saved_at = excluded.saved_at, cursor = excluded.cursor,
                             epoch = excluded.epoch, generation = excluded.generation",
                        params![doc_id, bytes, now_ms(), cursor, epoch, generation],
                    )?,
                    None => tx.execute(
                        "INSERT INTO snapshots (doc_id, bytes, saved_at, generation) VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(doc_id) DO UPDATE SET bytes = excluded.bytes,
                             saved_at = excluded.saved_at, generation = excluded.generation",
                        params![doc_id, bytes, now_ms(), generation],
                    )?,
                };
                tx.execute(
                    "DELETE FROM snapshot_deltas WHERE doc_id = ?1",
                    params![doc_id],
                )?;
                generation
            }
            DocWrite::Delta { generation, bytes } => {
                if !base_is(&tx, doc_id, generation)? {
                    return Ok(None);
                }
                tx.execute(
                    "INSERT INTO snapshot_deltas (doc_id, bytes) VALUES (?1, ?2)",
                    params![doc_id, bytes],
                )?;
                touch_row(&tx, doc_id, cursor)?;
                generation
            }
            DocWrite::Unchanged { generation } => {
                if !base_is(&tx, doc_id, generation)? {
                    return Ok(None);
                }
                if cursor.is_some() {
                    touch_row(&tx, doc_id, cursor)?;
                }
                generation
            }
        };
        if let Some(batch_id) = retire_outbox {
            tx.execute(
                "DELETE FROM chat_outbox WHERE doc_id=?1 AND batch_id=?2",
                params![doc_id, batch_id],
            )?;
        }
        tx.commit()?;
        Ok(Some(generation))
    }

    /// Save the snapshot together with its chat2 room cursor and doc epoch —
    /// ONE transaction, so bytes and cursor can never disagree (the C2 rule;
    /// a divergent pair is exactly the restored-backup redownload bug).
    pub fn save_snapshot_with_cursor(
        &self,
        doc_id: &str,
        bytes: &[u8],
        cursor: u64,
        epoch: u32,
    ) -> Result<(), StoreError> {
        self.write_doc(doc_id, DocWrite::Full(bytes), Some((cursor, epoch)), None)?;
        Ok(())
    }

    /// Snapshot (deltas merged in) + chat2 cursor + epoch. Pre-migration rows
    /// (or rows written by [`Self::save_snapshot`]) read back as `(bytes, 0, 0)`.
    pub fn load_snapshot_with_cursor(
        &self,
        doc_id: &str,
    ) -> Result<Option<(Vec<u8>, u64, u32)>, StoreError> {
        let Some(stored) = self.load_doc(doc_id)? else {
            return Ok(None);
        };
        let (cursor, epoch) = (stored.cursor, stored.epoch);
        Ok(Some((stored.into_snapshot()?, cursor, epoch)))
    }

    /// Delete the snapshot row (and its deltas) for `doc_id`. Missing rows
    /// are a no-op.
    pub fn delete_snapshot(&self, doc_id: &str) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM snapshots WHERE doc_id = ?1", params![doc_id])?;
        tx.execute(
            "DELETE FROM snapshot_deltas WHERE doc_id = ?1",
            params![doc_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every stored registry item (`key → bytes`).
    pub fn load_registry_items(&self) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT key, bytes FROM registry_items")?;
        Ok(stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Upsert and delete registry items in one transaction. `drop_snapshot`
    /// names a legacy single-blob snapshot row this write supersedes.
    pub fn write_registry_items(
        &self,
        upserts: &[(String, Vec<u8>)],
        deletes: &[String],
        drop_snapshot: Option<&str>,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut put = tx.prepare(
                "INSERT INTO registry_items (key, bytes) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET bytes = excluded.bytes",
            )?;
            for (key, bytes) in upserts {
                put.execute(params![key, bytes])?;
            }
            let mut del = tx.prepare("DELETE FROM registry_items WHERE key = ?1")?;
            for key in deletes {
                del.execute(params![key])?;
            }
        }
        if let Some(doc_id) = drop_snapshot {
            tx.execute("DELETE FROM snapshots WHERE doc_id = ?1", params![doc_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Whether a snapshot row exists for `doc_id` — presence only, no blob read.
    pub fn has_snapshot(&self, doc_id: &str) -> Result<bool, StoreError> {
        let hit = self
            .conn()
            .query_row(
                "SELECT 1 FROM snapshots WHERE doc_id = ?1",
                params![doc_id],
                |_| Ok(()),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    pub fn load_outbox(&self, doc_id: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        let conn = self.conn();
        // rowid is insertion order even when clocks go backwards or several
        // commits land in the same millisecond. UUID sorting is not causal order.
        let mut stmt = conn
            .prepare("SELECT batch_id, bytes FROM chat_outbox WHERE doc_id = ?1 ORDER BY rowid")?;
        Ok(stmt
            .query_map(params![doc_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn enqueue_outbox(
        &self,
        doc_id: &str,
        batch_id: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let old: Option<(String, Vec<u8>)> = tx
            .query_row(
                "SELECT doc_id, bytes FROM chat_outbox WHERE batch_id=?1",
                [batch_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((owner, payload)) = old {
            if owner != doc_id || payload != bytes {
                return Err(StoreError::Outbox(
                    "batch ID reused with different content".into(),
                ));
            }
        } else {
            tx.execute(
                "INSERT INTO chat_outbox(doc_id,batch_id,bytes,queued_at) VALUES(?1,?2,?3,?4)",
                params![doc_id, batch_id, bytes, now_ms()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// ACK is one SQLite transaction: a snapshot covering this batch survives
    /// before replay evidence is removed. Later batches are never deleted.
    pub fn acknowledge_outbox(
        &self,
        doc_id: &str,
        batch_id: &str,
        snapshot: &[u8],
        cursor: u64,
        epoch: u32,
    ) -> Result<(), StoreError> {
        self.write_doc(
            doc_id,
            DocWrite::Full(snapshot),
            Some((cursor, epoch)),
            Some(batch_id),
        )?;
        Ok(())
    }

    pub fn update_outbox(&self, batch_id: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let changed = self.conn().execute(
            "UPDATE chat_outbox SET bytes=?2 WHERE batch_id=?1",
            params![batch_id, bytes],
        )?;
        if changed == 0 {
            return Err(StoreError::Outbox("batch is not pending".into()));
        }
        Ok(())
    }

    #[cfg(test)]
    fn retire_outbox(&self, batch_id: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "DELETE FROM chat_outbox WHERE batch_id = ?1",
            params![batch_id],
        )?;
        Ok(())
    }

    /// The full command ledger — profile-import reads the source's claims so
    /// imported pending commands can never re-execute under the new profile.
    pub fn processed_commands(&self) -> Result<Vec<(String, i64)>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT command_id, processed_at FROM processed_commands")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Merge foreign ledger claims (profile import). Existing claims win;
    /// returns how many rows were newly inserted.
    pub fn import_processed_commands(&self, rows: &[(String, i64)]) -> Result<usize, StoreError> {
        let mut inserted = 0;
        let conn = self.conn();
        for (command_id, processed_at) in rows {
            inserted += conn.execute(
                "INSERT OR IGNORE INTO processed_commands (command_id, processed_at) VALUES (?1, ?2)",
                params![command_id, processed_at],
            )?;
        }
        Ok(inserted)
    }

    /// Whether `command_id` has already been claimed for execution.
    pub fn is_processed(&self, command_id: &str) -> Result<bool, StoreError> {
        let hit = self
            .conn()
            .query_row(
                "SELECT 1 FROM processed_commands WHERE command_id = ?1",
                params![command_id],
                |_| Ok(()),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    /// Claim `command_id` for execution — call BEFORE executing (ledger rule:
    /// a crash mid-execution must never re-run the command). Returns `true`
    /// if this call claimed it, `false` if it was already processed.
    pub fn mark_processed(&self, command_id: &str) -> Result<bool, StoreError> {
        let changed = self.conn().execute(
            "INSERT OR IGNORE INTO processed_commands (command_id, processed_at) VALUES (?1, ?2)",
            params![command_id, now_ms()],
        )?;
        Ok(changed > 0)
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A poisoned lock only means another thread panicked mid-query; the
        // connection itself is still usable.
        crate::lock(&self.conn)
    }
}

/// Whether `doc_id`'s stored base is `generation`.
fn base_is(tx: &Transaction<'_>, doc_id: &str, generation: i64) -> Result<bool, StoreError> {
    let stored: Option<i64> = tx
        .query_row(
            "SELECT generation FROM snapshots WHERE doc_id = ?1",
            params![doc_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(stored == Some(generation))
}

/// Stamp `saved_at` and, when given, the chat2 cursor + epoch.
fn touch_row(
    tx: &Transaction<'_>,
    doc_id: &str,
    cursor: Option<(i64, i64)>,
) -> Result<(), StoreError> {
    match cursor {
        Some((cursor, epoch)) => tx.execute(
            "UPDATE snapshots SET saved_at = ?2, cursor = ?3, epoch = ?4 WHERE doc_id = ?1",
            params![doc_id, now_ms(), cursor, epoch],
        )?,
        None => tx.execute(
            "UPDATE snapshots SET saved_at = ?2 WHERE doc_id = ?1",
            params![doc_id, now_ms()],
        )?,
    };
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
         ) STRICT",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let version = index as i64 + 1;
        if version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, now_ms()],
        )?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_roundtrip_and_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();

        assert_eq!(store.load_snapshot("chat-1").unwrap(), None);
        store.save_snapshot("chat-1", b"v1").unwrap();
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"v1"[..])
        );
        store.save_snapshot("chat-1", b"v2-longer-bytes").unwrap();
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"v2-longer-bytes"[..])
        );
        // Distinct docs do not collide.
        store.save_snapshot("chat-2", b"other").unwrap();
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"v2-longer-bytes"[..])
        );
    }

    #[test]
    fn cursor_rides_the_snapshot_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();

        // Plain saves (pre-chat2 path) read back with cursor/epoch 0.
        store.save_snapshot("chat-1", b"v1").unwrap();
        assert_eq!(
            store.load_snapshot_with_cursor("chat-1").unwrap(),
            Some((b"v1".to_vec(), 0, 0))
        );
        // Cursor and bytes land together; a re-save moves both.
        store
            .save_snapshot_with_cursor("chat-1", b"v2", 41, 2)
            .unwrap();
        assert_eq!(
            store.load_snapshot_with_cursor("chat-1").unwrap(),
            Some((b"v2".to_vec(), 41, 2))
        );
        // Plain load still works for cursor-written rows.
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"v2"[..])
        );
        // A plain save (legacy caller) clears nothing — cursor persists…
        store.save_snapshot("chat-1", b"v3").unwrap();
        let (bytes, cursor, epoch) = store.load_snapshot_with_cursor("chat-1").unwrap().unwrap();
        assert_eq!((bytes.as_slice(), cursor, epoch), (&b"v3"[..], 41, 2));
    }

    #[test]
    fn processed_ledger_claims_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();

        assert!(!store.is_processed("cmd-1").unwrap());
        assert!(store.mark_processed("cmd-1").unwrap(), "first mark claims");
        assert!(store.is_processed("cmd-1").unwrap());
        assert!(
            !store.mark_processed("cmd-1").unwrap(),
            "second mark must not re-claim"
        );
    }

    #[test]
    fn reopen_preserves_data_and_migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = DocsStore::open(dir.path()).unwrap();
            store.save_snapshot("chat-1", b"persisted").unwrap();
            store.mark_processed("cmd-1").unwrap();
        }
        let store = DocsStore::open(dir.path()).unwrap(); // re-runs migrate()
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"persisted"[..])
        );
        assert!(store.is_processed("cmd-1").unwrap());
        assert!(!store.mark_processed("cmd-1").unwrap());
    }

    #[test]
    fn outbox_roundtrips_in_order_and_retires_only_matching_batch() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        store.enqueue_outbox("chat-1", "b1", b"one").unwrap();
        store.enqueue_outbox("chat-1", "b2", b"two").unwrap();
        store.enqueue_outbox("chat-2", "other", b"x").unwrap();
        assert_eq!(
            store.load_outbox("chat-1").unwrap(),
            vec![
                ("b1".into(), b"one".to_vec()),
                ("b2".into(), b"two".to_vec())
            ]
        );
        store.retire_outbox("b1").unwrap();
        assert_eq!(
            store.load_outbox("chat-1").unwrap(),
            vec![("b2".into(), b"two".to_vec())]
        );
        drop(store);
        let reopened = DocsStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.load_outbox("chat-1").unwrap(),
            vec![("b2".into(), b"two".to_vec())]
        );
    }

    #[test]
    fn outbox_is_immutable_and_order_does_not_depend_on_uuid_or_clock() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        store.enqueue_outbox("c", "z-first", b"a").unwrap();
        store.enqueue_outbox("c", "a-second", b"b").unwrap();
        store
            .conn()
            .execute("UPDATE chat_outbox SET queued_at=0", [])
            .unwrap();
        store.enqueue_outbox("c", "z-first", b"a").unwrap();
        assert!(store.enqueue_outbox("c", "z-first", b"mutated").is_err());
        assert!(
            store
                .enqueue_outbox("different-chat", "z-first", b"a")
                .is_err()
        );
        assert_eq!(
            store.load_outbox("c").unwrap(),
            vec![
                ("z-first".into(), b"a".to_vec()),
                ("a-second".into(), b"b".to_vec())
            ]
        );
    }

    #[test]
    fn failed_ack_rolls_back_snapshot_and_keeps_all_pending_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        store.save_snapshot_with_cursor("c", b"old", 1, 2).unwrap();
        store.enqueue_outbox("c", "b1", b"delta-1").unwrap();
        store.enqueue_outbox("c", "b2", b"delta-2").unwrap();
        store.conn().execute_batch("CREATE TRIGGER fail_ack BEFORE DELETE ON chat_outbox BEGIN SELECT RAISE(ABORT,'injected disk failure'); END;").unwrap();
        assert!(store.acknowledge_outbox("c", "b1", b"new", 2, 2).is_err());
        assert_eq!(
            store.load_snapshot_with_cursor("c").unwrap(),
            Some((b"old".to_vec(), 1, 2))
        );
        assert_eq!(store.load_outbox("c").unwrap().len(), 2);
        store.conn().execute_batch("DROP TRIGGER fail_ack").unwrap();
        assert!(
            store
                .acknowledge_outbox("other", "b1", b"wrong", 99, 2)
                .is_err()
        );
        assert!(store.load_snapshot("other").unwrap().is_none());
        store.acknowledge_outbox("c", "b1", b"new", 2, 2).unwrap();
        drop(store);
        let reopened = DocsStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.load_snapshot_with_cursor("c").unwrap(),
            Some((b"new".to_vec(), 2, 2))
        );
        assert_eq!(
            reopened.load_outbox("c").unwrap(),
            vec![("b2".into(), b"delta-2".to_vec())]
        );
    }

    fn text_doc(text: &str) -> loro::LoroDoc {
        let doc = loro::LoroDoc::new();
        doc.get_text("t").insert(0, text).unwrap();
        doc.commit();
        doc
    }

    fn append(doc: &loro::LoroDoc, text: &str) -> loro::VersionVector {
        let before = doc.oplog_vv();
        let t = doc.get_text("t");
        t.insert(t.len_unicode(), text).unwrap();
        doc.commit();
        before
    }

    fn read_text(bytes: &[u8]) -> String {
        let doc = loro::LoroDoc::new();
        doc.import(bytes).unwrap();
        doc.get_text("t").to_string()
    }

    #[test]
    fn deltas_append_on_their_base_and_reads_merge_them() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let doc = text_doc("base");
        let snapshot = doc.export(loro::ExportMode::Snapshot).unwrap();
        let generation = store
            .write_doc("c", DocWrite::Full(&snapshot), Some((3, 2)), None)
            .unwrap()
            .unwrap();
        for chunk in [" one", " two"] {
            let from = append(&doc, chunk);
            let delta = doc.export(loro::ExportMode::updates(&from)).unwrap();
            let written = store
                .write_doc(
                    "c",
                    DocWrite::Delta {
                        generation,
                        bytes: &delta,
                    },
                    None,
                    None,
                )
                .unwrap();
            assert_eq!(
                written,
                Some(generation),
                "a delta keeps its base generation"
            );
        }
        let stored = store.load_doc("c").unwrap().unwrap();
        assert_eq!(
            (stored.deltas.len(), stored.cursor, stored.epoch),
            (2, 3, 2)
        );
        assert_eq!(read_text(&stored.into_snapshot().unwrap()), "base one two");
        let (bytes, cursor, epoch) = store.load_snapshot_with_cursor("c").unwrap().unwrap();
        assert_eq!(
            (read_text(&bytes).as_str(), cursor, epoch),
            ("base one two", 3, 2)
        );
        assert_eq!(
            read_text(&store.load_snapshot("c").unwrap().unwrap()),
            "base one two"
        );
    }

    #[test]
    fn a_replaced_base_refuses_stale_deltas_and_full_writes_compact() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let doc = text_doc("a");
        let snapshot = doc.export(loro::ExportMode::Snapshot).unwrap();
        let generation = store
            .write_doc("c", DocWrite::Full(&snapshot), None, None)
            .unwrap()
            .unwrap();
        let from = append(&doc, "b");
        let delta = doc.export(loro::ExportMode::updates(&from)).unwrap();
        store
            .write_doc(
                "c",
                DocWrite::Delta {
                    generation,
                    bytes: &delta,
                },
                None,
                None,
            )
            .unwrap();
        // Another writer replaces the base: the old generation is dead.
        store.save_snapshot("c", &snapshot).unwrap();
        let stored = store.load_doc("c").unwrap().unwrap();
        assert!(stored.deltas.is_empty(), "a full write drops the deltas");
        assert_ne!(stored.generation, generation);
        assert_eq!(
            store
                .write_doc(
                    "c",
                    DocWrite::Delta {
                        generation,
                        bytes: &delta
                    },
                    Some((9, 2)),
                    None
                )
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .write_doc("c", DocWrite::Unchanged { generation }, Some((9, 2)), None)
                .unwrap(),
            None
        );
        let stored = store.load_doc("c").unwrap().unwrap();
        assert!(stored.deltas.is_empty());
        assert_eq!(stored.cursor, 0, "a refused write moves nothing");
        // Delete then re-create never reuses a generation.
        store.delete_snapshot("c").unwrap();
        let regen = store
            .write_doc("c", DocWrite::Full(&snapshot), None, None)
            .unwrap()
            .unwrap();
        assert!(regen > stored.generation);
    }

    #[test]
    fn delete_drops_deltas_with_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let doc = text_doc("a");
        let snapshot = doc.export(loro::ExportMode::Snapshot).unwrap();
        let generation = store
            .write_doc("c", DocWrite::Full(&snapshot), None, None)
            .unwrap()
            .unwrap();
        let from = append(&doc, "b");
        let delta = doc.export(loro::ExportMode::updates(&from)).unwrap();
        store
            .write_doc(
                "c",
                DocWrite::Delta {
                    generation,
                    bytes: &delta,
                },
                None,
                None,
            )
            .unwrap();
        store.delete_snapshot("c").unwrap();
        assert!(store.load_doc("c").unwrap().is_none());
        let orphans: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM snapshot_deltas", [], |r| r.get(0))
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn unchanged_ack_moves_cursor_and_retires_only_its_batch() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let generation = store
            .write_doc("c", DocWrite::Full(b"base"), Some((1, 2)), None)
            .unwrap()
            .unwrap();
        store.enqueue_outbox("c", "b1", b"one").unwrap();
        store.enqueue_outbox("c", "b2", b"two").unwrap();
        assert!(
            store
                .write_doc(
                    "c",
                    DocWrite::Unchanged { generation },
                    Some((5, 2)),
                    Some("nope")
                )
                .is_err()
        );
        store
            .write_doc(
                "c",
                DocWrite::Unchanged { generation },
                Some((5, 2)),
                Some("b1"),
            )
            .unwrap();
        let stored = store.load_doc("c").unwrap().unwrap();
        assert_eq!((stored.base.as_slice(), stored.cursor), (&b"base"[..], 5));
        assert_eq!(
            store.load_outbox("c").unwrap(),
            vec![("b2".into(), b"two".to_vec())]
        );
    }

    #[test]
    fn registry_items_upsert_delete_and_supersede_the_legacy_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        store.save_snapshot("registry1", b"{legacy}").unwrap();
        store
            .write_registry_items(
                &[
                    ("meta".into(), b"m1".to_vec()),
                    ("row:a".into(), b"a1".to_vec()),
                ],
                &[],
                Some("registry1"),
            )
            .unwrap();
        assert!(store.load_snapshot("registry1").unwrap().is_none());
        store
            .write_registry_items(&[("meta".into(), b"m2".to_vec())], &["row:a".into()], None)
            .unwrap();
        drop(store);
        let store = DocsStore::open(dir.path()).unwrap();
        assert_eq!(
            store.load_registry_items().unwrap(),
            vec![("meta".into(), b"m2".to_vec())]
        );
    }
}
