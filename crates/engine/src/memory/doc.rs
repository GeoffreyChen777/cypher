//! A memory store's Loro doc (docs/design/memory.md §2).
//!
//! Every container is a ROOT container, which two devices can create at
//! once without conflict (a child container made concurrently under one map
//! key keeps only one side):
//!
//! - `memoryFiles`: path → file metadata (JSON: author, times). A path is a
//!   file while it has an entry here.
//! - `mem:<path>` (with `/` as `|`, root names may not hold `/`): the file's
//!   text, a `LoroText`, so concurrent edits merge by character. Deleting a
//!   file empties its text and drops its entry; recreating it reuses both.
//! - `memoryPending`: id → a held write (JSON [`MemoryPending`]).
//! - `memoryChanges`: the change log, oldest first (JSON [`MemoryChange`]).
//!
//! The chat schema's own `meta` map, written when the doc host first makes
//! the doc, is never read here.

use loro::{Frontiers, LoroDoc, LoroList, LoroMap, LoroText, LoroValue, UpdateOptions};
use serde::{Deserialize, Serialize};

use cypher_proto::{
    MEMORY_CHANGES_MAX, MEMORY_INDEX, MemoryAuthor, MemoryChange, MemoryFile, MemoryPending,
};

use crate::EngineError;

const FILES: &str = "memoryFiles";
const PENDING: &str = "memoryPending";
const CHANGES: &str = "memoryChanges";
const TEXT_PREFIX: &str = "mem:";

/// A file's metadata entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileMeta {
    #[serde(default)]
    author: MemoryAuthor,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    updated_at: i64,
}

fn json<T: Serialize>(value: &T) -> Result<String, EngineError> {
    serde_json::to_string(value).map_err(loro_err)
}

fn loro_err(err: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("memory doc: {err}"))
}

/// The doc's version, hex: what an undo checks out.
pub(super) fn version_hex(doc: &LoroDoc) -> String {
    hex(&doc.oplog_frontiers().encode())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// One store's doc, read and written through its root containers. Callers
/// serialize writes per store and commit through [`StoreDoc::commit`].
pub(super) struct StoreDoc<'a> {
    doc: &'a LoroDoc,
}

impl<'a> StoreDoc<'a> {
    pub(super) fn new(doc: &'a LoroDoc) -> Self {
        Self { doc }
    }

    pub(super) fn raw(&self) -> &'a LoroDoc {
        self.doc
    }

    fn files(&self) -> LoroMap {
        self.doc.get_map(FILES)
    }

    fn text(&self, path: &str) -> LoroText {
        self.doc
            .get_text(format!("{TEXT_PREFIX}{}", path.replace('/', "|")))
    }

    fn meta_of(&self, path: &str) -> Option<FileMeta> {
        match self.files().get(path)?.into_value().ok()? {
            LoroValue::String(json) => serde_json::from_str(&json).ok(),
            _ => None,
        }
    }

    /// Every file, `MEMORY.md` first, then by path.
    pub(super) fn files_list(&self) -> Vec<MemoryFile> {
        let mut paths: Vec<String> = Vec::new();
        self.files().for_each(|key, _| paths.push(key.to_string()));
        let mut files: Vec<MemoryFile> = paths
            .into_iter()
            .filter_map(|path| self.file(&path))
            .collect();
        files.sort_by(|a, b| {
            (a.path != MEMORY_INDEX)
                .cmp(&(b.path != MEMORY_INDEX))
                .then(a.path.cmp(&b.path))
        });
        files
    }

    pub(super) fn file(&self, path: &str) -> Option<MemoryFile> {
        let meta = self.meta_of(path)?;
        Some(MemoryFile {
            path: path.to_string(),
            text: self.text(path).to_string(),
            author: meta.author,
            created_at: meta.created_at,
            updated_at: meta.updated_at,
        })
    }

    /// Create or replace `path`'s text, merged as a diff into the text
    /// container so a concurrent edit elsewhere in the file survives.
    pub(super) fn put(
        &self,
        path: &str,
        text: &str,
        author: &MemoryAuthor,
        now: i64,
    ) -> Result<(), EngineError> {
        let created_at = self
            .meta_of(path)
            .map_or(now, |meta| meta.created_at.max(1).min(now));
        let container = self.text(path);
        if container.to_string() != text
            && container
                .update(
                    text,
                    UpdateOptions {
                        timeout_ms: Some(500.0),
                        use_refined_diff: true,
                    },
                )
                .is_err()
        {
            // A diff too slow to compute: replace the whole text.
            let len = container.len_unicode();
            container.delete(0, len).map_err(loro_err)?;
            container.insert(0, text).map_err(loro_err)?;
        }
        let meta = FileMeta {
            author: author.clone(),
            created_at,
            updated_at: now,
        };
        self.files().insert(path, json(&meta)?).map_err(loro_err)?;
        Ok(())
    }

    pub(super) fn remove(&self, path: &str) -> Result<(), EngineError> {
        let container = self.text(path);
        let len = container.len_unicode();
        if len > 0 {
            container.delete(0, len).map_err(loro_err)?;
        }
        self.files().delete(path).map_err(loro_err)?;
        Ok(())
    }

    pub(super) fn pending(&self) -> Vec<MemoryPending> {
        let mut out: Vec<MemoryPending> = Vec::new();
        self.doc.get_map(PENDING).for_each(|_, value| {
            if let Some(LoroValue::String(json)) = value.into_value().ok()
                && let Ok(pending) = serde_json::from_str(&json)
            {
                out.push(pending);
            }
        });
        out.sort_by(|a, b| a.at.cmp(&b.at).then(a.id.cmp(&b.id)));
        out
    }

    pub(super) fn hold(&self, pending: &MemoryPending) -> Result<(), EngineError> {
        self.doc
            .get_map(PENDING)
            .insert(&pending.id, json(pending)?)
            .map_err(loro_err)?;
        Ok(())
    }

    pub(super) fn release(&self, id: &str) -> Result<(), EngineError> {
        self.doc.get_map(PENDING).delete(id).map_err(loro_err)?;
        Ok(())
    }

    fn changes_list(&self) -> LoroList {
        self.doc.get_list(CHANGES)
    }

    /// The change log, newest first.
    pub(super) fn changes(&self) -> Vec<MemoryChange> {
        let list = self.changes_list();
        let mut out: Vec<MemoryChange> = (0..list.len())
            .filter_map(|i| match list.get(i)?.into_value().ok()? {
                LoroValue::String(json) => serde_json::from_str(&json).ok(),
                _ => None,
            })
            .collect();
        out.reverse();
        out
    }

    /// Append to the log, dropping the oldest past [`MEMORY_CHANGES_MAX`].
    pub(super) fn log(&self, change: &MemoryChange) -> Result<(), EngineError> {
        let list = self.changes_list();
        list.push(json(change)?).map_err(loro_err)?;
        let over = list.len().saturating_sub(MEMORY_CHANGES_MAX);
        if over > 0 {
            list.delete(0, over).map_err(loro_err)?;
        }
        Ok(())
    }

    /// Mark logged changes undone (by id).
    pub(super) fn mark_undone(&self, ids: &[String]) -> Result<(), EngineError> {
        let list = self.changes_list();
        for i in 0..list.len() {
            let Some(LoroValue::String(stored)) = list.get(i).and_then(|v| v.into_value().ok())
            else {
                continue;
            };
            let Ok(mut change) = serde_json::from_str::<MemoryChange>(&stored) else {
                continue;
            };
            if ids.contains(&change.id) && !change.undone {
                change.undone = true;
                list.delete(i, 1).map_err(loro_err)?;
                list.insert(i, json(&change)?).map_err(loro_err)?;
            }
        }
        Ok(())
    }

    /// Make this round of edits one commit (and so one sync batch).
    pub(super) fn commit(&self) {
        self.doc.commit();
    }

    /// The files as they were at `version` (from [`version_hex`]).
    pub(super) fn files_at(&self, version: &str) -> Result<Vec<MemoryFile>, EngineError> {
        let bytes = unhex(version).ok_or_else(|| loro_err("bad version"))?;
        let frontiers = Frontiers::decode(&bytes).map_err(loro_err)?;
        let fork = self.doc.fork_at(&frontiers).map_err(loro_err)?;
        Ok(StoreDoc::new(&fork).files_list())
    }
}

/// The whole doc, for tests that sync two copies.
#[cfg(test)]
pub(super) fn snapshot(doc: &LoroDoc) -> Vec<u8> {
    doc.export(loro::ExportMode::Snapshot).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn author() -> MemoryAuthor {
        MemoryAuthor::Owner
    }

    #[test]
    fn files_read_back_index_first() {
        let doc = LoroDoc::new();
        let store = StoreDoc::new(&doc);
        store.put("/zeta.md", "z", &author(), 5).unwrap();
        store.put(MEMORY_INDEX, "# Index", &author(), 6).unwrap();
        store.put("/alpha.md", "a", &author(), 7).unwrap();
        store.commit();
        let paths: Vec<String> = store.files_list().into_iter().map(|f| f.path).collect();
        assert_eq!(paths, ["/MEMORY.md", "/alpha.md", "/zeta.md"]);
        assert_eq!(store.file("/alpha.md").unwrap().text, "a");
        store.remove("/alpha.md").unwrap();
        assert!(store.file("/alpha.md").is_none());
        // Recreated, it starts empty, not with the old text.
        store.put("/alpha.md", "again", &author(), 8).unwrap();
        assert_eq!(store.file("/alpha.md").unwrap().text, "again");
        assert_eq!(store.file("/alpha.md").unwrap().created_at, 8);
    }

    #[test]
    fn two_devices_editing_one_file_keep_both_edits() {
        let a = LoroDoc::new();
        let b = LoroDoc::new();
        StoreDoc::new(&a)
            .put(MEMORY_INDEX, "- one\n- two\n", &author(), 1)
            .unwrap();
        a.commit();
        b.import(&snapshot(&a)).unwrap();
        StoreDoc::new(&a)
            .put(MEMORY_INDEX, "- one (a)\n- two\n", &author(), 2)
            .unwrap();
        a.commit();
        StoreDoc::new(&b)
            .put(MEMORY_INDEX, "- one\n- two (b)\n", &author(), 3)
            .unwrap();
        b.commit();
        a.import(&snapshot(&b)).unwrap();
        let text = StoreDoc::new(&a).file(MEMORY_INDEX).unwrap().text;
        assert_eq!(text, "- one (a)\n- two (b)\n");
    }

    #[test]
    fn concurrent_creates_of_one_path_merge() {
        let a = LoroDoc::new();
        let b = LoroDoc::new();
        StoreDoc::new(&a)
            .put("/x.md", "from a", &author(), 1)
            .unwrap();
        a.commit();
        StoreDoc::new(&b)
            .put("/x.md", "from b", &author(), 1)
            .unwrap();
        b.commit();
        a.import(&snapshot(&b)).unwrap();
        let text = StoreDoc::new(&a).file("/x.md").unwrap().text;
        assert!(
            text.contains("from a") && text.contains("from b"),
            "both sides kept: {text}"
        );
    }

    #[test]
    fn the_log_is_capped_and_newest_first() {
        let doc = LoroDoc::new();
        let store = StoreDoc::new(&doc);
        for i in 0..(MEMORY_CHANGES_MAX + 5) {
            store
                .log(&MemoryChange {
                    id: format!("c{i}"),
                    at: i as i64,
                    author: author(),
                    path: MEMORY_INDEX.into(),
                    action: cypher_proto::MemoryAction::Edit,
                    summary: String::new(),
                    before: None,
                    owner_text: false,
                    undo: None,
                    undone: false,
                })
                .unwrap();
        }
        let changes = store.changes();
        assert_eq!(changes.len(), MEMORY_CHANGES_MAX);
        assert_eq!(changes[0].id, format!("c{}", MEMORY_CHANGES_MAX + 4));
        store.mark_undone(&["c10".into()]).unwrap();
        assert!(store.changes().iter().any(|c| c.id == "c10" && c.undone));
    }

    #[test]
    fn an_old_version_reads_back() {
        let doc = LoroDoc::new();
        let store = StoreDoc::new(&doc);
        store.put(MEMORY_INDEX, "v1", &author(), 1).unwrap();
        store.commit();
        let before = version_hex(&doc);
        store.put(MEMORY_INDEX, "v2", &author(), 2).unwrap();
        store.put("/new.md", "n", &author(), 2).unwrap();
        store.commit();
        let old = store.files_at(&before).unwrap();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].text, "v1");
        assert_eq!(unhex(&hex(&[0, 255, 16])).unwrap(), vec![0, 255, 16]);
    }
}
