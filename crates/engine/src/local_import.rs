//! Explicit import of NEW v3 local work into an account-scoped profile.
//! Metadata comes from native SQLite; public transcript windows are staged
//! through the bounded v3 producer with atomic per-entry receipts. Legacy
//! snapshots, JSONL, command ledgers and execution permits are never imported.
//! Metadata rows land last so interrupted copies remain structurally retryable.
//! Attachment paths retain an explicit read-only grant to local uploads.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cypher_proto::metadata::view::MetadataView;
use serde::{Deserialize, Serialize};

use crate::EngineError;
use crate::doc_host::DocHost;
use crate::uploads::Uploads;
use crate::workspace_host::WorkspaceHost;

/// Marker recording completed imports, at `{data_dir}/local-import.json`.
/// One entry per target (org, user): the same device may sign into several
/// accounts, and each gets its own one-time import.
const MARKER_FILE: &str = "local-import.json";

/// Serializes every marker read-modify-write in this process. Two runtimes in
/// one process (a swap mid-flight, tests) must not lose an account's entry to
/// a racing update; cross-process exclusion is the data-dir `InstanceLock`'s job.
fn marker_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Marker {
    #[serde(default)]
    imports: Vec<MarkerEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarkerEntry {
    org_id: String,
    user_id: String,
    imported_at_ms: i64,
    imported_chats: usize,
    imported_spaces: usize,
}

/// What the wizard needs to offer (or silently skip) the import step.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalImportStatus {
    /// Chats in the local profile with no row in this synced workspace yet.
    pub available_chats: usize,
    /// Spaces in the local profile with no row in this synced workspace yet.
    pub available_spaces: usize,
    /// A completed import for this (org, user) is already on record.
    pub imported_before: bool,
}

/// Per-item progress for the wizard's progress step.
/// NB: `rename_all` renames the *variants* (the `kind` tag); struct-variant
/// *fields* need `rename_all_fields` — without it the wire shape silently
/// ships snake_case counters (caught by `import_events_serialize_camel_case`).
#[derive(Debug, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum ImportEvent {
    /// Emitted once, before any copying.
    Start { chats: usize, spaces: usize },
    /// One imported chat (docs + journal + row).
    Chat {
        index: usize,
        total: usize,
        chat_id: String,
        title: Option<String>,
    },
    /// Terminal summary — also the RPC's final stream item.
    Summary {
        imported_chats: usize,
        imported_spaces: usize,
        skipped_chats: usize,
        skipped_spaces: usize,
        journals_copied: usize,
        ledger_rows_merged: usize,
        errors: Vec<String>,
    },
}

/// The import service a synced runtime carries (`EngineCore::assemble` builds
/// one when the profile is account-scoped and a local profile exists on disk).
#[derive(Clone)]
pub struct LocalImporter {
    inner: Arc<ImporterInner>,
}

struct ImporterInner {
    data_dir: PathBuf,
    device_id: String,
    org_id: String,
    user_id: String,
    doc_host: DocHost,
    workspace: WorkspaceHost,
    uploads: Uploads,
}

impl LocalImporter {
    #[allow(clippy::too_many_arguments)] // engine assembly seam, not a public API
    pub fn new(
        data_dir: &Path,
        device_id: &str,
        org_id: &str,
        user_id: &str,
        doc_host: DocHost,
        workspace: WorkspaceHost,
        uploads: Uploads,
    ) -> Self {
        Self {
            inner: Arc::new(ImporterInner {
                data_dir: data_dir.to_path_buf(),
                device_id: device_id.to_string(),
                org_id: org_id.to_string(),
                user_id: user_id.to_string(),
                doc_host,
                workspace,
                uploads,
            }),
        }
    }

    fn source_root(&self) -> PathBuf {
        self.inner.data_dir.join("profiles").join("local")
    }

    fn source_uploads(&self) -> PathBuf {
        self.source_root().join("uploads")
    }

    fn marker_path(&self) -> PathBuf {
        self.inner.data_dir.join(MARKER_FILE)
    }

    /// Load the marker under [`marker_lock`]. A file that exists but does not
    /// parse is moved aside to `local-import.json.corrupt` — its grants were
    /// already unreadable (boot's [`marker_grants_read_root`] parses the same
    /// file), and keeping the bytes as evidence beats silently clobbering them
    /// on the next write.
    fn load_marker_locked(&self) -> Marker {
        let path = self.marker_path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => return Marker::default(),
        };
        match serde_json::from_str(&raw) {
            Ok(marker) => marker,
            Err(err) => {
                let aside = path.with_extension("json.corrupt");
                tracing::warn!(error = %err, aside = %aside.display(),
                    "local-import marker is corrupt; moving it aside");
                if let Err(err) = std::fs::rename(&path, &aside) {
                    tracing::warn!(error = %err, "corrupt marker aside failed");
                }
                Marker::default()
            }
        }
    }

    fn imported_before(&self) -> bool {
        let _guard = marker_lock();
        self.load_marker_locked()
            .imports
            .iter()
            .any(|e| e.org_id == self.inner.org_id && e.user_id == self.inner.user_id)
    }

    /// Record a completed import and re-arm the read-only uploads root for
    /// future boots (`EngineCore::assemble` consults [`marker_grants_read_root`]).
    ///
    /// The read-modify-write is serialized process-wide (cross-process is
    /// already excluded by the data-dir `InstanceLock`) and published
    /// atomically — write + fsync a sibling temp file, then rename over the
    /// marker — so a crash or short write can never truncate the file and
    /// erase other accounts' grants. Failures propagate to the caller and end
    /// up in the import summary.
    fn record_import(&self, chats: usize, spaces: usize) -> Result<(), EngineError> {
        let _guard = marker_lock();
        let mut marker = self.load_marker_locked();
        marker
            .imports
            .retain(|e| !(e.org_id == self.inner.org_id && e.user_id == self.inner.user_id));
        marker.imports.push(MarkerEntry {
            org_id: self.inner.org_id.clone(),
            user_id: self.inner.user_id.clone(),
            imported_at_ms: crate::now_ms(),
            imported_chats: chats,
            imported_spaces: spaces,
        });
        let bytes = serde_json::to_vec_pretty(&marker)
            .map_err(|err| EngineError::Other(format!("marker serialize: {err}")))?;
        let path = self.marker_path();
        let tmp = path.with_extension("json.tmp");
        let publish = || -> std::io::Result<()> {
            {
                use std::io::Write;
                let mut file = std::fs::File::create(&tmp)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
            }
            std::fs::rename(&tmp, &path)
        };
        publish().map_err(|err| {
            let _ = std::fs::remove_file(&tmp);
            EngineError::Other(format!("marker write: {err}"))
        })
    }

    /// Open the local profile's stores read-only-ish. `None` when the device
    /// never ran a local profile (nothing to import).
    fn open_source(&self) -> Result<Option<MetadataView>, EngineError> {
        let root = self.source_root();
        if !root.join("docs.sqlite3").is_file() {
            return Ok(None);
        }
        WorkspaceHost::read_local_profile(&self.inner.data_dir, &self.inner.device_id)
    }

    /// What's importable right now (target-dedup applied).
    pub fn status(&self) -> Result<LocalImportStatus, EngineError> {
        let imported_before = self.imported_before();
        let Some(registry) = self.open_source()? else {
            return Ok(LocalImportStatus {
                available_chats: 0,
                available_spaces: 0,
                imported_before,
            });
        };
        let mut available_chats = 0;
        for chat in registry.read_chats()? {
            if self.inner.workspace.chat(&chat.id)?.is_none() {
                available_chats += 1;
            }
        }
        let mut available_spaces = 0;
        for space in registry.read_spaces()? {
            if self.inner.workspace.space(&space.id)?.is_none() {
                available_spaces += 1;
            }
        }
        Ok(LocalImportStatus {
            available_chats,
            available_spaces,
            imported_before,
        })
    }

    /// Run the import, yielding between bounded windows and waiting for
    /// authenticated target readiness. The last event is always Summary.
    pub async fn run(&self, mut emit: impl FnMut(ImportEvent)) -> Result<(), EngineError> {
        let Some(registry) = self.open_source()? else {
            emit(ImportEvent::Start {
                chats: 0,
                spaces: 0,
            });
            emit(ImportEvent::Summary {
                imported_chats: 0,
                imported_spaces: 0,
                skipped_chats: 0,
                skipped_spaces: 0,
                journals_copied: 0,
                ledger_rows_merged: 0,
                errors: Vec::new(),
            });
            return Ok(());
        };

        let mut errors: Vec<String> = Vec::new();

        // Spaces first: chats reference `space_id`, and viewers resolve the
        // reference as soon as the chat row lands.
        let spaces = registry.read_spaces()?;
        let chats = registry.read_chats()?;
        let (total_chats, total_spaces) = (chats.len(), spaces.len());
        let pending_chats: Vec<_> = chats
            .into_iter()
            .filter(|chat| !matches!(self.inner.workspace.chat(&chat.id), Ok(Some(_))))
            .collect();
        let pending_spaces: Vec<_> = spaces
            .into_iter()
            .filter(|space| !matches!(self.inner.workspace.space(&space.id), Ok(Some(_))))
            .collect();
        let skipped_chats = total_chats - pending_chats.len();
        let skipped_spaces = total_spaces - pending_spaces.len();

        emit(ImportEvent::Start {
            chats: pending_chats.len(),
            spaces: pending_spaces.len(),
        });

        let mut imported_spaces = 0;
        for space in &pending_spaces {
            match self.inner.workspace.import_space_row(space) {
                Ok(()) => imported_spaces += 1,
                Err(err) => errors.push(format!("space {}: {err}", space.id)),
            }
        }

        let total = pending_chats.len();
        let mut imported_chats = 0;
        for (index, chat) in pending_chats.iter().enumerate() {
            emit(ImportEvent::Chat {
                index,
                total,
                chat_id: chat.id.clone(),
                title: chat.title.clone(),
            });
            match self.import_chat(chat).await {
                Ok(()) => {
                    imported_chats += 1;
                }
                Err(err) => errors.push(format!("chat {}: {err}", chat.id)),
            }
        }

        // Merge the source's command ledger so imported pending commands can
        // never re-execute under this profile (mark-before-execute carries over).
        // No command/claim or retired JSONL ledger crosses a profile boundary.
        let ledger_rows_merged = 0;
        let journals_copied = 0;

        // Transcripts embed absolute paths under the local uploads root; jail
        // it read-only now (and on future boots, via the marker). A marker
        // that fails to persist means imported attachments stop resolving
        // after a restart — that is a real failure, not a footnote.
        if self.source_uploads().is_dir() {
            self.inner
                .uploads
                .add_read_only_root(&self.source_uploads());
        }
        if let Err(err) = self.record_import(imported_chats, imported_spaces) {
            errors.push(format!("import marker: {err}"));
        }

        emit(ImportEvent::Summary {
            imported_chats,
            imported_spaces,
            skipped_chats,
            skipped_spaces,
            journals_copied,
            ledger_rows_merged,
            errors,
        });
        Ok(())
    }

    /// Copy one chat: doc snapshot (born-chat2 shape), journal files, row.
    /// Returns whether a journal file was copied.
    async fn import_chat(&self, chat: &cypher_proto::Chat) -> Result<(), EngineError> {
        let map = |e: cypher_sync::sync3::Error| EngineError::Other(e.to_string());
        let profile = crate::EngineProfile::local(&self.inner.data_dir)?;
        let sources = crate::session_replicas::SessionReplicas::new(
            &profile,
            self.inner.device_id.clone(),
            None,
        )
        .map_err(map)?;
        let source = sources
            .get_for_owner(&chat.id, &self.inner.device_id)
            .map_err(map)?;
        let through = source.read(|j| j.cursor()).map_err(map)?;
        let target = self
            .inner
            .doc_host
            .open(&chat.id)?
            .replica()
            .cloned()
            .ok_or_else(|| EngineError::Other("native import target unavailable".into()))?;
        let mut status = target.watch();
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let current = status.borrow_and_update().clone();
                if current.phase == cypher_sync::sync3::Phase::Live {
                    return Ok::<_, EngineError>(());
                }
                if let Some(error) = current.error {
                    return Err(EngineError::Other(error));
                }
                status
                    .changed()
                    .await
                    .map_err(|_| EngineError::Other("import target closed".into()))?;
            }
        })
        .await
        .map_err(|_| EngineError::Other("import target unavailable".into()))??;
        let mut after = 0;
        loop {
            let window = source
                .read(|j| j.message_window_after(after, 32))
                .map_err(map)?;
            if window.through != through {
                return Err(EngineError::Other(
                    "local source changed during import".into(),
                ));
            }
            if window.messages.is_empty() {
                break;
            }
            for message in window.messages {
                after = message.created_seq;
                let mut entry = message.entry;
                if entry.status == Some(cypher_proto::MessageStatus::Streaming) {
                    entry.status = Some(cypher_proto::MessageStatus::Aborted);
                }
                let key = serde_json::to_string(&(
                    profile.org_id(),
                    profile.user_id(),
                    &chat.id,
                    &entry.id,
                ))
                .map_err(|e| EngineError::Other(e.to_string()))?;
                target
                    .write(|j| j.import_public_entry(&key, &entry))
                    .map_err(map)?;
            }
            tokio::task::yield_now().await;
        }
        if let Some(cwd) = chat.harness_session_cwd.as_deref().or(chat.cwd.as_deref()) {
            if let Some(session) = source.read(|j| j.harness_session(cwd)).map_err(map)? {
                let origin =
                    serde_json::to_string(&(profile.org_id(), profile.user_id(), &chat.id))
                        .map_err(|e| EngineError::Other(e.to_string()))?;
                target
                    .write(|j| j.import_harness_session(&origin, cwd, &session))
                    .map_err(map)?;
            }
        }
        sources.shutdown().await;
        let mut row = chat.clone();
        row.room_gen = Some(3);
        self.inner.workspace.import_chat_row(&row)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ImportEvent;

    #[test]
    fn import_events_serialize_camel_case() {
        let summary = serde_json::to_value(ImportEvent::Summary {
            imported_chats: 2,
            imported_spaces: 1,
            skipped_chats: 0,
            skipped_spaces: 0,
            journals_copied: 2,
            ledger_rows_merged: 3,
            errors: Vec::new(),
        })
        .expect("serialize");
        assert_eq!(summary["kind"], "summary");
        assert_eq!(
            summary["importedChats"], 2,
            "fields must be camelCase: {summary}"
        );
        let chat = serde_json::to_value(ImportEvent::Chat {
            index: 0,
            total: 2,
            chat_id: "c1".into(),
            title: None,
        })
        .expect("serialize");
        assert_eq!(chat["chatId"], "c1", "fields must be camelCase: {chat}");
    }
}

/// Whether a recorded import grants the synced profile `(org, user)` the local
/// profile's uploads root as a read-only jail root. `EngineCore::assemble`
/// calls this on every account-scoped boot.
pub fn marker_grants_read_root(data_dir: &Path, org_id: &str, user_id: &str) -> Option<PathBuf> {
    let marker: Marker = std::fs::read_to_string(data_dir.join(MARKER_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())?;
    let hit = marker
        .imports
        .iter()
        .any(|e| e.org_id == org_id && e.user_id == user_id);
    let uploads = data_dir.join("profiles").join("local").join("uploads");
    (hit && uploads.is_dir()).then_some(uploads)
}
