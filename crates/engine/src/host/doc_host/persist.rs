//! Incremental chat-doc persistence: one [`DocPersister`] per open doc writes
//! only what changed since its last write.
//!
//! The stored doc is a base snapshot plus the Loro updates appended on top of
//! it ([`cypher_sync::DocWrite`]). Every save used to re-export and rewrite the
//! whole snapshot — per debounced local commit, per synced row, per outbox
//! ACK — so one streaming multi-MB chat cost megabytes per second of disk
//! writes for kilobytes of change. Now a save appends the update since the
//! last stored version, and the base is only rewritten when the deltas have
//! grown past it ([`delta_budget`]) or the persister cannot vouch for the
//! stored base (first write after a fresh doc, or another writer replaced it).

use loro::{ExportMode, LoroDoc, VersionVector};

use cypher_sync::{DocWrite, DocsStore, StoreError, StoredDoc};

use super::*;
use crate::host::chat2_host::CHAT2_DOC_EPOCH;

/// Deltas may total at least this much before a compaction, whatever the
/// base size: small docs would otherwise rewrite their base every few commits.
const MIN_DELTA_BUDGET_BYTES: usize = 1024 * 1024;

/// Delta rows before a compaction regardless of size — bounds the per-row
/// overhead and the import work of a cold open.
const MAX_DELTAS: usize = 2048;

/// Delta bytes allowed on top of a base of `base_len`: writing the deltas
/// costs about what one more base rewrite would, so total writes stay within
/// ~2x the actual change volume.
fn delta_budget(base_len: usize) -> usize {
    base_len.max(MIN_DELTA_BUDGET_BYTES)
}

/// What this persister knows the store holds for its doc.
struct StoredBase {
    /// Base generation the stored deltas extend.
    generation: i64,
    /// Doc version covered by base + deltas.
    vv: VersionVector,
    base_len: usize,
    delta_len: usize,
    deltas: usize,
}

pub(crate) struct DocPersister {
    store: Arc<DocsStore>,
    doc_id: String,
    /// `None` until the first write (or an [`Self::adopt`]): the next write is
    /// then a full snapshot. Held across export + write so concurrent savers
    /// (debounced save, chat2 sink callbacks) never interleave.
    base: Mutex<Option<StoredBase>>,
}

impl DocPersister {
    pub(crate) fn new(store: Arc<DocsStore>, doc_id: impl Into<String>) -> Self {
        Self {
            store,
            doc_id: doc_id.into(),
            base: Mutex::new(None),
        }
    }

    /// Open path: the doc was just imported from `stored` and `vv` is its
    /// version right after that import, before anything else touched it.
    pub(crate) fn adopt(&self, stored: &StoredDoc, vv: VersionVector) {
        *lock(&self.base) = Some(StoredBase {
            generation: stored.generation,
            vv,
            base_len: stored.base.len(),
            delta_len: stored.stored_len() - stored.base.len(),
            deltas: stored.deltas.len(),
        });
    }

    /// Persist the doc's current content (cursor and epoch untouched).
    /// Returns the stored size (base + deltas).
    pub(crate) fn save(&self, doc: &LoroDoc) -> Result<usize, StoreError> {
        self.write(doc, None, None, false)
    }

    /// Persist content together with the chat2 room cursor (C2 rule).
    pub(crate) fn save_with_cursor(&self, doc: &LoroDoc, cursor: u64) -> Result<usize, StoreError> {
        self.write(doc, Some(cursor), None, false)
    }

    /// A full snapshot with the chat2 cursor — the epoch-2 lineage stamp for
    /// a born or promoted doc.
    pub(crate) fn save_full_with_cursor(
        &self,
        doc: &LoroDoc,
        cursor: u64,
    ) -> Result<usize, StoreError> {
        self.write(doc, Some(cursor), None, true)
    }

    /// Content covering `batch_id` + cursor stored, THEN the batch retired —
    /// one transaction.
    pub(crate) fn acknowledge(
        &self,
        doc: &LoroDoc,
        batch_id: &str,
        cursor: u64,
    ) -> Result<usize, StoreError> {
        self.write(doc, Some(cursor), Some(batch_id), false)
    }

    fn write(
        &self,
        doc: &LoroDoc,
        cursor: Option<u64>,
        retire: Option<&str>,
        force_full: bool,
    ) -> Result<usize, StoreError> {
        let cursor = cursor.map(|cursor| (cursor, CHAT2_DOC_EPOCH));
        let mut base = lock(&self.base);
        // Export's own barrier, made explicit: the version read below must
        // cover any pending auto-commit transaction. Read it BEFORE exporting
        // so ops committed concurrently land in this write or the next one.
        doc.commit();
        let vv = doc.oplog_vv();
        if !force_full && let Some(stored) = base.as_mut() {
            let write = if vv == stored.vv {
                if cursor.is_none() && retire.is_none() {
                    return Ok(stored.base_len + stored.delta_len);
                }
                Some(None)
            } else {
                match doc.export(ExportMode::updates(&stored.vv)) {
                    Ok(delta)
                        if stored.deltas < MAX_DELTAS
                            && stored.delta_len + delta.len() <= delta_budget(stored.base_len) =>
                    {
                        Some(Some(delta))
                    }
                    Ok(_) => None, // over budget: compact below
                    Err(err) => {
                        tracing::warn!(chat = %self.doc_id, error = %err,
                            "delta export failed; rewriting the snapshot");
                        None
                    }
                }
            };
            if let Some(delta) = write {
                let content = match &delta {
                    Some(bytes) => DocWrite::Delta {
                        generation: stored.generation,
                        bytes,
                    },
                    None => DocWrite::Unchanged {
                        generation: stored.generation,
                    },
                };
                if self
                    .store
                    .write_doc(&self.doc_id, content, cursor, retire)?
                    .is_some()
                {
                    if let Some(bytes) = delta {
                        stored.vv = vv;
                        stored.delta_len += bytes.len();
                        stored.deltas += 1;
                    }
                    return Ok(stored.base_len + stored.delta_len);
                }
                tracing::debug!(chat = %self.doc_id, "stored base replaced; rewriting the snapshot");
            }
        }
        let snapshot = doc
            .export(ExportMode::Snapshot)
            .map_err(|e| StoreError::Merge(e.to_string()))?;
        let generation = self
            .store
            .write_doc(&self.doc_id, DocWrite::Full(&snapshot), cursor, retire)?
            .ok_or_else(|| StoreError::Merge("full snapshot write refused".into()))?;
        *base = Some(StoredBase {
            generation,
            vv,
            base_len: snapshot.len(),
            delta_len: 0,
            deltas: 0,
        });
        Ok(snapshot.len())
    }
}

/// Import a stored doc into `doc`. `false` when some delta did not apply
/// cleanly — the caller must not [`DocPersister::adopt`] then, so the next
/// write rewrites the base from what memory actually holds.
pub(super) fn import_stored(doc: &LoroDoc, stored: &StoredDoc) -> Result<bool, EngineError> {
    doc.import(&stored.base)
        .map_err(|e| EngineError::Other(format!("snapshot import failed: {e}")))?;
    if stored.deltas.is_empty() {
        return Ok(true);
    }
    match doc.import_batch(&stored.deltas) {
        Ok(status) if status.pending.is_none() => Ok(true),
        Ok(_) => {
            tracing::warn!("stored deltas left pending ops; the next save compacts");
            Ok(false)
        }
        Err(err) => {
            tracing::warn!(error = %err, "stored delta import failed; the next save compacts");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deltas(store: &DocsStore, id: &str) -> usize {
        store.load_doc(id).unwrap().unwrap().deltas.len()
    }

    fn text(store: &DocsStore, id: &str) -> String {
        let doc = LoroDoc::new();
        doc.import(&store.load_snapshot(id).unwrap().unwrap())
            .unwrap();
        doc.get_text("t").to_string()
    }

    fn append(doc: &LoroDoc, s: &str) {
        let t = doc.get_text("t");
        t.insert(t.len_unicode(), s).unwrap();
        doc.commit();
    }

    #[test]
    fn saves_append_deltas_after_one_full_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let persister = DocPersister::new(store.clone(), "c");
        let doc = LoroDoc::new();
        append(&doc, "a");
        persister.save(&doc).unwrap();
        assert_eq!(deltas(&store, "c"), 0, "the first write is the base");
        for s in ["b", "c", "d"] {
            append(&doc, s);
            persister.save(&doc).unwrap();
        }
        assert_eq!(deltas(&store, "c"), 3);
        // Nothing changed: no write at all.
        persister.save(&doc).unwrap();
        assert_eq!(deltas(&store, "c"), 3);
        assert_eq!(text(&store, "c"), "abcd");
    }

    #[test]
    fn cursor_only_and_ack_writes_carry_no_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let persister = DocPersister::new(store.clone(), "c");
        let doc = LoroDoc::new();
        append(&doc, "a");
        persister.save_with_cursor(&doc, 1).unwrap();
        store.enqueue_outbox("c", "b1", b"x").unwrap();
        persister.acknowledge(&doc, "b1", 4).unwrap();
        let stored = store.load_doc("c").unwrap().unwrap();
        assert_eq!(
            (stored.deltas.len(), stored.cursor, stored.epoch),
            (0, 4, CHAT2_DOC_EPOCH)
        );
        assert!(store.load_outbox("c").unwrap().is_empty());
    }

    #[test]
    fn compacts_once_deltas_outgrow_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let persister = DocPersister::new(store.clone(), "c");
        let doc = LoroDoc::new();
        append(&doc, "seed");
        persister.save(&doc).unwrap();
        // Incompressible chunks so the budget is reached on size.
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut chunk = || {
            (0..64 * 1024)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    char::from(b'a' + (x % 26) as u8)
                })
                .collect::<String>()
        };
        let mut compacted = false;
        for _ in 0..40 {
            append(&doc, &chunk());
            persister.save(&doc).unwrap();
            if deltas(&store, "c") == 0 {
                compacted = true;
            }
        }
        assert!(compacted, "deltas past the budget fold back into the base");
        assert_eq!(text(&store, "c"), doc.get_text("t").to_string());
    }

    #[test]
    fn a_replaced_base_falls_back_to_a_full_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let persister = DocPersister::new(store.clone(), "c");
        let doc = LoroDoc::new();
        append(&doc, "a");
        persister.save(&doc).unwrap();
        // Another writer (a second persister, an import) replaces the row.
        let other = LoroDoc::new();
        other.get_text("t").insert(0, "zzz").unwrap();
        other.commit();
        store
            .save_snapshot("c", &other.export(ExportMode::Snapshot).unwrap())
            .unwrap();
        append(&doc, "b");
        persister.save(&doc).unwrap();
        assert_eq!(deltas(&store, "c"), 0);
        assert_eq!(text(&store, "c"), "ab", "never a delta onto a foreign base");
    }

    #[test]
    fn adopted_docs_continue_the_stored_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        {
            let persister = DocPersister::new(store.clone(), "c");
            let doc = LoroDoc::new();
            append(&doc, "a");
            persister.save(&doc).unwrap();
            append(&doc, "b");
            persister.save(&doc).unwrap();
        }
        let stored = store.load_doc("c").unwrap().unwrap();
        let doc = LoroDoc::new();
        assert!(import_stored(&doc, &stored).unwrap());
        let persister = DocPersister::new(store.clone(), "c");
        persister.adopt(&stored, doc.oplog_vv());
        append(&doc, "c");
        persister.save(&doc).unwrap();
        assert_eq!(
            deltas(&store, "c"),
            2,
            "reopen appends instead of rewriting"
        );
        assert_eq!(text(&store, "c"), "abc");
    }
}
