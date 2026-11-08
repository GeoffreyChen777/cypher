//! Registry persistence: the replica is stored one item per row
//! ([`RegistryDoc::to_items`]) and each save writes only the items whose bytes
//! changed since the last save.
//!
//! The replica used to be one JSON blob rewritten on every change — ~1MB for a
//! typical account, saved every few seconds by heartbeats and every other
//! device's row writes. A save now costs the row it touched plus `meta`.

use std::collections::{HashMap, HashSet};

use super::*;

/// Load the replica: the legacy single-blob snapshot when one exists (only a
/// build that predates item storage writes it, so it is the newer copy), else
/// the stored items. `None` when nothing is stored.
pub(crate) fn load_registry(
    store: &DocsStore,
    device_id: &str,
) -> Result<Option<(RegistryDoc, RegistryPersister)>, EngineError> {
    let items = store.load_registry_items()?;
    if let Some(bytes) = store.load_snapshot(REGISTRY_DOC_ID)? {
        let doc = RegistryDoc::from_bytes(&bytes, device_id)
            .map_err(|e| EngineError::Other(format!("registry snapshot load failed: {e}")))?;
        return Ok(Some((doc, RegistryPersister::new(items, true))));
    }
    let doc = RegistryDoc::from_items(items.iter().cloned(), device_id)
        .map_err(|e| EngineError::Other(format!("registry items load failed: {e}")))?;
    Ok(doc.map(|doc| (doc, RegistryPersister::new(items, false))))
}

/// Remembers exactly what the store holds so a save can diff against it.
pub(crate) struct RegistryPersister {
    written: Mutex<Written>,
}

struct Written {
    items: HashMap<String, Vec<u8>>,
    /// A legacy blob row still exists: the next save drops it, and writes
    /// every item even if unchanged.
    legacy_blob: bool,
}

impl RegistryPersister {
    pub(crate) fn new(stored: Vec<(String, Vec<u8>)>, legacy_blob: bool) -> Self {
        Self {
            written: Mutex::new(Written {
                items: stored.into_iter().collect(),
                legacy_blob,
            }),
        }
    }

    pub(super) fn empty() -> Self {
        Self::new(Vec::new(), false)
    }

    /// Write the items that differ from the stored ones and delete the ones
    /// gone. `items` runs under this persister's lock, so two concurrent
    /// saves can't land an older replica after a newer one. The cache moves
    /// only after the transaction commits, so a failed save is retried in
    /// full by the next one.
    pub(super) fn save(
        &self,
        store: &DocsStore,
        items: impl FnOnce() -> Result<Vec<(String, Vec<u8>)>, cypher_doc::DocError>,
    ) -> Result<(), EngineError> {
        let mut written = lock(&self.written);
        let items = items().map_err(|e| EngineError::Other(format!("registry export: {e}")))?;
        let current: HashSet<&str> = items.iter().map(|(key, _)| key.as_str()).collect();
        let deletes: Vec<String> = written
            .items
            .keys()
            .filter(|key| !current.contains(key.as_str()))
            .cloned()
            .collect();
        let upserts: Vec<(String, Vec<u8>)> = items
            .into_iter()
            .filter(|(key, bytes)| {
                written.legacy_blob || written.items.get(key).is_none_or(|old| old != bytes)
            })
            .collect();
        if upserts.is_empty() && deletes.is_empty() && !written.legacy_blob {
            return Ok(());
        }
        store.write_registry_items(
            &upserts,
            &deletes,
            written.legacy_blob.then_some(REGISTRY_DOC_ID),
        )?;
        for key in &deletes {
            written.items.remove(key);
        }
        written.items.extend(upserts);
        written.legacy_blob = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str) -> Device {
        Device {
            id: id.into(),
            name: "mac".into(),
            platform: "macos".into(),
            last_seen_at: None,
            created_at: None,
            version: None,
        }
    }

    fn item_count(store: &DocsStore) -> usize {
        store.load_registry_items().unwrap().len()
    }

    #[test]
    fn legacy_blob_migrates_to_items_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let mut doc = RegistryDoc::new("dev");
        doc.upsert_device(&device("dev")).unwrap();
        doc.settle_pending_locally();
        store
            .save_snapshot(REGISTRY_DOC_ID, &doc.to_bytes().unwrap())
            .unwrap();

        let (doc, persister) = load_registry(&store, "dev").unwrap().unwrap();
        assert_eq!(doc.read_devices().unwrap().len(), 1);
        persister.save(&store, || doc.to_items()).unwrap();
        assert!(store.load_snapshot(REGISTRY_DOC_ID).unwrap().is_none());
        assert_eq!(item_count(&store), 3, "meta + pending + one row");

        let (reloaded, _) = load_registry(&store, "dev").unwrap().unwrap();
        assert_eq!(reloaded.read_all().unwrap(), doc.read_all().unwrap());
    }

    #[test]
    fn saves_write_only_changed_items_and_delete_gone_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let persister = RegistryPersister::empty();
        let mut items = vec![
            ("meta".to_string(), b"m1".to_vec()),
            ("row/chats/a".to_string(), b"a".to_vec()),
            ("row/chats/b".to_string(), b"b".to_vec()),
        ];
        persister.save(&store, || Ok(items.clone())).unwrap();
        assert_eq!(item_count(&store), 3);

        // Change a row behind the persister's back: an unchanged save must
        // not rewrite it (it diffs against what IT wrote).
        store
            .write_registry_items(&[("row/chats/a".into(), b"tampered".to_vec())], &[], None)
            .unwrap();
        persister.save(&store, || Ok(items.clone())).unwrap();
        let stored: HashMap<_, _> = store.load_registry_items().unwrap().into_iter().collect();
        assert_eq!(stored["row/chats/a"], b"tampered");

        items[0].1 = b"m2".to_vec();
        items.pop();
        persister.save(&store, || Ok(items)).unwrap();
        let stored: HashMap<_, _> = store.load_registry_items().unwrap().into_iter().collect();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored["meta"], b"m2");
    }
}
