//! Memory (docs/design/memory.md): what agents learn and keep between
//! sessions, as small trees of markdown files.
//!
//! Each store is one Loro doc opened through the [`DocHost`] like a chat, so
//! it persists offline and syncs through its own chat2 room; its files live
//! in root containers of their own ([`doc`]). The registry holds a small
//! [`MemoryStore`] row per store. This service resolves which stores a chat
//! sees ([`access`]), answers the `memory`, `search_history` and
//! `read_history` tools ([`tool`]), builds each run's memory section of the
//! prompt ([`prompt`]), holds writes from runs that read untrusted content
//! ([`taint`]), applies the app's edits, and runs consolidation passes
//! ([`passes`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use sha2::{Digest, Sha256};

use cypher_proto::{
    self as p, KIND_MEMORY_STORES, MEMORY_INDEX, MEMORY_PREVIEW_MAX, MemoryAction, MemoryAuthor,
    MemoryChange, MemoryEdit, MemoryFile, MemoryPending, MemoryScope, MemoryStore, MemoryView,
};

use crate::EngineError;
use crate::bots::Bots;
use crate::host::doc_host::{ChatDocHandle, DocHost};
use crate::host::workspace_host::{Fields, WorkspaceHost};
use crate::pi::runtime::PiRuntimePaths;
use crate::util::{lock, new_id};

mod access;
mod doc;
mod passes;
mod prompt;
mod taint;
#[cfg(test)]
mod tests;
mod tool;

use access::{Reader, Root};
use doc::StoreDoc;

/// How long a write waits for a store this device opens for the first time
/// to catch up with its room, so it never writes over a copy it hasn't got.
const FIRST_OPEN_CATCH_UP: Duration = Duration::from_secs(5);

/// The row fields any engine that changes a store's doc rewrites.
const COUNT_FIELDS: [&str; 7] = [
    "id",
    "scope",
    "files",
    "bytes",
    "pending",
    "preview",
    "updatedAt",
];

/// This engine's memory service.
#[derive(Clone)]
pub struct Memory {
    inner: Arc<Inner>,
}

struct Inner {
    workspace: WorkspaceHost,
    doc_host: DocHost,
    bots: Bots,
    /// Identifies the user in user-level store ids (a hash of org and user).
    user_key: String,
    /// `<store>/memory`: the empty folders consolidation passes run in.
    root: PathBuf,
    /// One writer per store at a time on this engine.
    store_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pi_runtime: Mutex<Option<PiRuntimePaths>>,
    passes: passes::PassState,
    /// Runs that read untrusted content, and passes' token use.
    taint: taint::Taint,
    cancel: tokio_util::sync::CancellationToken,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// What a write did, for the tool's answer.
#[derive(Debug, PartialEq)]
enum Written {
    Applied(String),
    Held(String),
}

/// A write in store terms: the file's new text (`None`: removed), and the
/// path it moved from.
#[derive(Debug, Clone, PartialEq)]
struct Planned {
    path: String,
    text: Option<String>,
    from: Option<String>,
}

impl Memory {
    pub fn new(workspace: WorkspaceHost, doc_host: DocHost, bots: Bots, root: PathBuf) -> Self {
        let user_key = user_key(workspace.org_id(), workspace.user_id());
        Self {
            inner: Arc::new(Inner {
                workspace,
                doc_host,
                bots,
                user_key,
                root,
                store_locks: Default::default(),
                pi_runtime: Default::default(),
                passes: Default::default(),
                taint: Default::default(),
                cancel: Default::default(),
                tasks: Default::default(),
            }),
        }
    }

    /// The installed Pi Runtime, for subagent profiles' `memory:`.
    pub fn set_pi_runtime(&self, paths: Option<PiRuntimePaths>) {
        *lock(&self.inner.pi_runtime) = paths;
    }

    /// Start the pass host and move old notebooks in (needs a runtime).
    pub fn start(&self) {
        let memory = self.clone();
        let task = tokio::spawn(async move {
            memory.migrate_notebooks().await;
            memory.run_pass_host().await;
        });
        lock(&self.inner.tasks).push(task);
    }

    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        let tasks: Vec<_> = lock(&self.inner.tasks).drain(..).collect();
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
    }

    /// Every event of every run on this engine (the run task calls it):
    /// what the run read decides whether its memory writes are held.
    pub fn observe(&self, chat_id: &str, event: &cypher_proto::AgentEvent) {
        let Some(reason) = self.inner.taint.observe(chat_id, event) else {
            return;
        };
        // What a subagent read comes back in its answer: its parents' writes
        // wait too, until their own next run.
        if let Ok(Some(chat)) = self.inner.workspace.chat(chat_id) {
            for parent in self.ancestors(&chat) {
                self.inner
                    .taint
                    .mark(&parent, &format!("used a subagent that {reason}"));
            }
        }
    }

    pub(crate) fn store_id(&self, scope: &MemoryScope) -> String {
        scope.store_id(&self.inner.user_key)
    }

    fn row(&self, store_id: &str) -> Option<MemoryStore> {
        self.inner
            .workspace
            .record::<MemoryStore>(KIND_MEMORY_STORES, store_id)
    }

    fn open(&self, store_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        self.inner.doc_host.open(store_id)
    }

    /// The store's doc, given a moment to catch up when this device has
    /// never held a copy, so the write that follows lands on top of it.
    async fn open_for_write(&self, store_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        let handle = self.open(store_id)?;
        let blank = {
            let store = StoreDoc::new(handle.doc().doc());
            store.files_list().is_empty() && store.changes().is_empty()
        };
        if blank && self.inner.doc_host.syncs() {
            let deadline = tokio::time::Instant::now() + FIRST_OPEN_CATCH_UP;
            while !handle.connected() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if handle.connected() {
                // The join imports the room's rows right after connecting.
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
        Ok(handle)
    }

    fn store_lock(&self, store_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.inner.store_locks)
            .entry(store_id.to_string())
            .or_default()
            .clone()
    }

    /// Rewrite the row's counts from the doc, creating the row if needed.
    fn sync_row(&self, scope: &MemoryScope, store_id: &str, store: &StoreDoc) {
        let files = store.files_list();
        let pending = store.pending().len();
        let now = p::now_ms();
        let existing = self.row(store_id);
        let fresh = existing.is_none();
        let mut row = existing.unwrap_or_else(|| {
            MemoryStore::new(
                store_id.to_string(),
                scope.clone(),
                self.inner.workspace.device_id().to_string(),
                now,
            )
        });
        row.files = files.len() as u32;
        row.bytes = files.iter().map(|f| f.text.len() as u64).sum();
        row.pending = pending as u32;
        row.preview = files
            .iter()
            .find(|f| f.path == MEMORY_INDEX)
            .map(|f| p::clip(&f.text, MEMORY_PREVIEW_MAX))
            .unwrap_or_default();
        row.updated_at = now;
        row.clamp();
        let fields = if fresh {
            Fields::All
        } else {
            Fields::Only(&COUNT_FIELDS)
        };
        if let Err(err) =
            self.inner
                .workspace
                .put_record(KIND_MEMORY_STORES, store_id, &row, fields)
        {
            tracing::warn!(store = %store_id, error = %err, "memory row write failed");
        }
    }

    /// Write one row's owner-set or host-set fields (no counts).
    fn put_row_fields(&self, row: &MemoryStore, fields: &[&str]) -> Result<(), EngineError> {
        let mut row = row.clone();
        row.clamp();
        self.inner
            .workspace
            .put_record(KIND_MEMORY_STORES, &row.id, &row, Fields::Only(fields))
    }

    /// The row for `scope`, created (with its defaults) when missing.
    fn ensure_row(&self, scope: &MemoryScope) -> Result<MemoryStore, EngineError> {
        let id = self.store_id(scope);
        if let Some(row) = self.row(&id) {
            return Ok(row);
        }
        let row = MemoryStore::new(
            id.clone(),
            scope.clone(),
            self.inner.workspace.device_id().to_string(),
            p::now_ms(),
        );
        self.inner
            .workspace
            .put_record(KIND_MEMORY_STORES, &id, &row, Fields::All)?;
        Ok(row)
    }

    // ── writing ─────────────────────────────────────────────────────────

    /// Apply (or hold) one planned write to `root`'s store as `reader`.
    async fn write(
        &self,
        reader: &Reader,
        root: &Root,
        command: &str,
        args: &serde_json::Value,
        plan: impl FnOnce(&StoreDoc) -> Result<Planned, EngineError>,
    ) -> Result<Written, EngineError> {
        if !root.write {
            return Err(EngineError::Other(access::read_only_hint(reader, root)));
        }
        let store_id = self.store_id(&root.scope);
        let guard = self.store_lock(&store_id);
        let _held = guard.lock().await;
        let handle = self.open_for_write(&store_id).await?;
        let store = StoreDoc::new(handle.doc().doc());
        let written = self.write_in(reader, root, &store, command, args, plan)?;
        self.inner.doc_host.flush_chat_sync(&store_id);
        Ok(written)
    }

    /// [`Memory::write`] on a store the caller holds the lock of.
    fn write_in(
        &self,
        reader: &Reader,
        root: &Root,
        store: &StoreDoc,
        command: &str,
        args: &serde_json::Value,
        plan: impl FnOnce(&StoreDoc) -> Result<Planned, EngineError>,
    ) -> Result<Written, EngineError> {
        let store_id = self.store_id(&root.scope);
        let planned = plan(store)?;
        check_limits(store, &planned)?;
        if let Some(text) = &planned.text {
            let (_, secrets) = crate::bots::scrub_secrets(text);
            if secrets > 0 {
                return Err(EngineError::Other(
                    "That looks like it holds a secret (a key, token or password). Memory never \
                     stores secrets: leave it out and write the rest."
                        .into(),
                ));
            }
        }
        let shown = format!("{}{}", root.path, planned.path);
        if let Some(reason) = self.taint_reason(reader) {
            // A rename keeps its source in store terms; the other commands
            // keep their arguments, so accepting one re-applies it to the
            // file as it is then.
            let args = match &planned.from {
                Some(from) => serde_json::json!({ "from": from }),
                None => args.clone(),
            };
            let pending = MemoryPending {
                id: new_id(),
                at: p::now_ms(),
                author: reader.author.clone(),
                path: planned.path.clone(),
                command: command.to_string(),
                args,
                reason: reason.clone(),
                preview: planned.text.clone(),
            };
            store.hold(&pending)?;
            store.commit();
            self.sync_row(&root.scope, &store_id, store);
            self.note_activity(
                root,
                &format!("Memory write held for review: {shown}"),
                reader,
            );
            return Ok(Written::Held(format!(
                "Held for review: this run {reason}, so its memory writes wait for the owner's \
                 approval and stay out of later prompts until then. Nothing else to do."
            )));
        }
        let summary = apply(store, &planned, &reader.author)?;
        store.commit();
        self.sync_row(&root.scope, &store_id, store);
        self.note_activity(root, &format!("{summary}: {shown}"), reader);
        Ok(Written::Applied(format!("{summary}: {shown}.")))
    }

    /// Why `reader`'s writes are held: it, or a parent it was spawned
    /// from, read untrusted content this run (a child's own taint reaches
    /// its parents when it happens, [`Memory::observe`]).
    fn taint_reason(&self, reader: &Reader) -> Option<String> {
        let own = self.inner.taint.reason(&reader.chat.id).or_else(|| {
            self.inner
                .bots
                .peer_taint(&reader.chat.id)
                .map(|reason| format!("received another workspace's answer ({reason})"))
        });
        own.or_else(|| {
            self.ancestors(&reader.chat).into_iter().find_map(|parent| {
                self.inner
                    .taint
                    .reason(&parent)
                    .map(|reason| format!("was started by a session that {reason}"))
            })
        })
    }

    /// The chats `chat` was spawned from, nearest first.
    fn ancestors(&self, chat: &cypher_proto::Chat) -> Vec<String> {
        let mut out = Vec::new();
        let mut next = chat.parent_chat_id().map(str::to_string);
        while let Some(id) = next.filter(|id| !out.contains(id) && out.len() < 16) {
            next = self
                .inner
                .workspace
                .chat(&id)
                .ok()
                .flatten()
                .and_then(|c| c.parent_chat_id().map(str::to_string));
            out.push(id);
        }
        out
    }

    /// A workspace store's changes also go to the workspace's Activity.
    fn note_activity(&self, root: &Root, text: &str, reader: &Reader) {
        if let MemoryScope::Workspace { bot_id } = &root.scope {
            self.inner
                .bots
                .note_memory_activity(bot_id, text, Some(&reader.chat.id));
        }
    }

    // ── the app's view and edits ───────────────────────────────────────

    /// `scope`'s store as one frame (an empty one when it has no doc yet).
    fn view(&self, scope: &MemoryScope) -> Result<MemoryView, EngineError> {
        let id = self.store_id(scope);
        let store = self.row(&id).unwrap_or_else(|| {
            MemoryStore::new(
                id.clone(),
                scope.clone(),
                self.inner.workspace.device_id().to_string(),
                0,
            )
        });
        let handle = self.open(&id)?;
        let doc = StoreDoc::new(handle.doc().doc());
        Ok(MemoryView {
            store,
            files: doc.files_list(),
            pending: doc.pending(),
            changes: doc.changes(),
        })
    }

    /// `WatchMemory`: the view now, then again on every doc or row change.
    pub fn watch(
        &self,
        scope: MemoryScope,
    ) -> Result<BoxStream<'static, serde_json::Value>, EngineError> {
        let id = self.store_id(&scope);
        let handle = self.open(&id)?;
        // Holding the doc pins it against eviction while the view is open.
        let doc = handle.doc_arc();
        let (tx, rx) = tokio::sync::watch::channel(0u64);
        let sub = doc.doc().subscribe_root(Arc::new(move |_| {
            tx.send_modify(|v| *v = v.wrapping_add(1));
        }));
        let records = self.inner.workspace.watch_bot_records();
        let memory = self.clone();
        let state = (memory, scope, rx, records, false, sub, doc);
        Ok(futures::stream::unfold(
            state,
            |(memory, scope, mut rx, mut records, emitted, sub, doc)| async move {
                if emitted {
                    tokio::select! {
                        changed = rx.changed() => changed.ok()?,
                        changed = records.changed() => changed.ok()?,
                    }
                    // Let a burst of commits land as one frame.
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    rx.borrow_and_update();
                    records.borrow_and_update();
                }
                let view = memory.view(&scope).ok()?;
                let value = serde_json::to_value(&view).ok()?;
                Some((value, (memory, scope, rx, records, true, sub, doc)))
            },
        )
        .boxed())
    }

    /// `EditMemory`: one edit from the app, as the owner.
    pub async fn edit(&self, scope: MemoryScope, edit: MemoryEdit) -> Result<String, EngineError> {
        let root = Root::owner(scope.clone());
        let owner = Reader::owner();
        let shown = |path: &str| path.trim_start_matches('/').to_string();
        match edit {
            MemoryEdit::Write { path, text } => {
                let path = tool::normalize_file(&path)?;
                let written = self
                    .write(&owner, &root, "write", &serde_json::Value::Null, |_| {
                        Ok(Planned {
                            path: path.clone(),
                            text: Some(text),
                            from: None,
                        })
                    })
                    .await?;
                Ok(match written {
                    Written::Applied(_) | Written::Held(_) => format!("Saved {}.", shown(&path)),
                })
            }
            MemoryEdit::Delete { path } => {
                let path = tool::normalize_file(&path)?;
                self.write(&owner, &root, "delete", &serde_json::Value::Null, |store| {
                    store
                        .file(&path)
                        .ok_or_else(|| EngineError::Other(format!("No file {path}.")))?;
                    Ok(Planned {
                        path: path.clone(),
                        text: None,
                        from: None,
                    })
                })
                .await?;
                Ok(format!("Deleted {}.", shown(&path)))
            }
            MemoryEdit::Rename { from, to } => {
                let from = tool::normalize_file(&from)?;
                let to = tool::normalize_file(&to)?;
                self.write(&owner, &root, "rename", &serde_json::Value::Null, |store| {
                    tool::plan_rename(store, &from, &to)
                })
                .await?;
                Ok(format!("Renamed to {}.", shown(&to)))
            }
            MemoryEdit::Accept { id } => self.accept(&scope, &id).await,
            MemoryEdit::Discard { id } => {
                let store_id = self.store_id(&scope);
                let guard = self.store_lock(&store_id);
                let _held = guard.lock().await;
                let handle = self.open(&store_id)?;
                let store = StoreDoc::new(handle.doc().doc());
                if !store.pending().iter().any(|p| p.id == id) {
                    return Err(EngineError::Other(
                        "That write is no longer waiting.".into(),
                    ));
                }
                store.release(&id)?;
                store.commit();
                self.sync_row(&scope, &store_id, &store);
                Ok("Discarded.".into())
            }
            MemoryEdit::Undo { change_id } => self.undo_change(&scope, &change_id).await,
            MemoryEdit::UndoPass { pass_id } => self.undo_pass(&scope, &pass_id).await,
            MemoryEdit::SetConsolidation { enabled, hour } => {
                let mut row = self.ensure_row(&scope)?;
                row.consolidation = p::Consolidation {
                    enabled,
                    hour: hour.min(23),
                };
                self.put_row_fields(&row, &["consolidation"])?;
                Ok(if enabled {
                    format!("Consolidation runs nightly at {:02}:00.", hour.min(23))
                } else {
                    "Consolidation is off.".into()
                })
            }
            MemoryEdit::RunPass => {
                let mut row = self.ensure_row(&scope)?;
                if row.last_pass.as_ref().is_some_and(|p| p.is_running()) {
                    return Err(EngineError::Other("A pass is already running.".into()));
                }
                row.pass_requested_at = Some(p::now_ms());
                self.put_row_fields(&row, &["passRequestedAt"])?;
                self.wake_passes();
                Ok("Consolidation will run shortly on the store's device.".into())
            }
        }
    }

    /// Apply a held write as its author, then drop it: one critical
    /// section, so two accepts of one write apply it once.
    async fn accept(&self, scope: &MemoryScope, id: &str) -> Result<String, EngineError> {
        let store_id = self.store_id(scope);
        let guard = self.store_lock(&store_id);
        let _held = guard.lock().await;
        let handle = self.open(&store_id)?;
        let store = StoreDoc::new(handle.doc().doc());
        let pending = store
            .pending()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| EngineError::Other("That write is no longer waiting.".into()))?;
        let root = Root::owner(scope.clone());
        let reader = Reader::accepting(pending.author.clone());
        let result = self.write_in(
            &reader,
            &root,
            &store,
            &pending.command,
            &pending.args,
            |store| match pending.command.as_str() {
                "rename" => {
                    let from = pending.args["from"].as_str().unwrap_or_default();
                    tool::plan_rename(store, from, &pending.path)
                }
                "str_replace" | "insert" | "delete" => {
                    tool::plan_command(store, &pending.command, &pending.path, &pending.args)
                }
                _ => Ok(Planned {
                    path: pending.path.clone(),
                    text: pending.preview.clone(),
                    from: None,
                }),
            },
        );
        // Accepted or impossible now (the file changed since): either way it
        // no longer waits.
        store.release(id)?;
        store.commit();
        self.sync_row(scope, &store_id, &store);
        self.inner.doc_host.flush_chat_sync(&store_id);
        match result {
            Ok(_) => Ok(format!(
                "Accepted: {}.",
                pending.path.trim_start_matches('/')
            )),
            Err(err) => Err(EngineError::Other(format!(
                "It can't be applied any more ({err}), so it was dropped."
            ))),
        }
    }

    async fn undo_change(
        &self,
        scope: &MemoryScope,
        change_id: &str,
    ) -> Result<String, EngineError> {
        let store_id = self.store_id(scope);
        let guard = self.store_lock(&store_id);
        let _held = guard.lock().await;
        let handle = self.open(&store_id)?;
        let store = StoreDoc::new(handle.doc().doc());
        let change = store
            .changes()
            .into_iter()
            .find(|c| c.id == change_id)
            .ok_or_else(|| EngineError::Other("That change is no longer in the log.".into()))?;
        if change.undone {
            return Err(EngineError::Other("That change was already undone.".into()));
        }
        let version = change
            .undo
            .as_deref()
            .ok_or_else(|| EngineError::Other("That change can't be undone.".into()))?;
        let before = store.files_at(version)?;
        let was = |path: &str| before.iter().find(|f| f.path == path).cloned();
        let mut plans = vec![Planned {
            path: change.path.clone(),
            text: was(&change.path).map(|f| f.text),
            from: None,
        }];
        if let MemoryAction::Rename { from } = &change.action {
            plans = vec![
                Planned {
                    path: change.path.clone(),
                    text: None,
                    from: None,
                },
                Planned {
                    path: from.clone(),
                    text: was(from).map(|f| f.text),
                    from: None,
                },
            ];
        }
        let undo_id = new_id();
        let undo_version = doc::version_hex(handle.doc().doc());
        for plan in &plans {
            // Back as it was, by whoever wrote it then.
            let author = was(&plan.path).map_or(MemoryAuthor::Owner, |f| f.author);
            restore(&store, plan, &author)?;
        }
        store.log(&MemoryChange {
            id: undo_id,
            at: p::now_ms(),
            author: MemoryAuthor::Owner,
            path: change.path.clone(),
            action: MemoryAction::Undo {
                change_id: change.id.clone(),
            },
            summary: format!("Undid \"{}\"", change.summary),
            before: None,
            owner_text: false,
            undo: Some(undo_version),
            undone: false,
        })?;
        store.mark_undone(std::slice::from_ref(&change.id))?;
        store.commit();
        self.sync_row(scope, &store_id, &store);
        Ok(format!("Undid: {}.", change.summary))
    }

    /// Put the whole store back as it was before pass `pass_id`.
    async fn undo_pass(&self, scope: &MemoryScope, pass_id: &str) -> Result<String, EngineError> {
        let mut row = self
            .row(&self.store_id(scope))
            .ok_or_else(|| EngineError::Other("This memory has no passes.".into()))?;
        let pass = row
            .last_pass
            .clone()
            .filter(|p| p.id == pass_id)
            .ok_or_else(|| EngineError::Other("Only the latest pass can be undone.".into()))?;
        if pass.is_running() {
            return Err(EngineError::Other(
                "The pass is still running; undo it when it ends.".into(),
            ));
        }
        let version = pass
            .undo
            .clone()
            .ok_or_else(|| EngineError::Other("That pass can't be undone.".into()))?;
        let store_id = self.store_id(scope);
        let guard = self.store_lock(&store_id);
        let _held = guard.lock().await;
        let handle = self.open(&store_id)?;
        let store = StoreDoc::new(handle.doc().doc());
        let before = store.files_at(&version)?;
        // Only what the pass changed goes back: a session's write that
        // landed while it ran stays.
        let mine: Vec<MemoryChange> = store
            .changes()
            .into_iter()
            .filter(|c| matches!(&c.author, MemoryAuthor::Consolidation { pass_id: id } if id == pass_id))
            .collect();
        let mut touched: Vec<String> = Vec::new();
        for change in &mine {
            touched.push(change.path.clone());
            if let MemoryAction::Rename { from } = &change.action {
                touched.push(from.clone());
            }
        }
        touched.sort();
        touched.dedup();
        let undo_version = doc::version_hex(handle.doc().doc());
        let mut restored = 0;
        for path in &touched {
            let then = before.iter().find(|f| &f.path == path);
            let now = store.file(path);
            match (then, now) {
                (None, Some(_)) => store.remove(path)?,
                (Some(then), now) if now.as_ref().is_none_or(|n| n.text != then.text) => {
                    store.put(path, &then.text, &then.author, p::now_ms())?
                }
                _ => continue,
            }
            restored += 1;
        }
        let undone: Vec<String> = mine.into_iter().map(|c| c.id).collect();
        store.mark_undone(&undone)?;
        store.log(&MemoryChange {
            id: new_id(),
            at: p::now_ms(),
            author: MemoryAuthor::Owner,
            path: MEMORY_INDEX.into(),
            action: MemoryAction::Undo {
                change_id: pass_id.to_string(),
            },
            summary: "Undid the consolidation pass".into(),
            before: None,
            owner_text: false,
            undo: Some(undo_version),
            undone: false,
        })?;
        store.commit();
        self.sync_row(scope, &store_id, &store);
        row.last_pass = row.last_pass.map(|mut p| {
            p.outcome = p::PassOutcome::Undone;
            p
        });
        self.put_row_fields(&row, &["lastPass"])?;
        Ok(format!(
            "Put memory back as it was before the pass ({restored} file{} restored).",
            if restored == 1 { "" } else { "s" }
        ))
    }

    // ── the notebook, moved in ──────────────────────────────────────────

    /// Fold each workspace's notebook (hosted here) into its store's
    /// `MEMORY.md`, once, and drop the note rows. Runs at start.
    pub async fn migrate_notebooks(&self) {
        let workspace = &self.inner.workspace;
        let Ok(bots) = workspace.read_bots() else {
            return;
        };
        let records = workspace.read_bot_records();
        for bot in bots
            .iter()
            .filter(|b| b.host_device_id == workspace.device_id())
        {
            let mut notes: Vec<&p::BotNote> = records
                .notes
                .iter()
                .filter(|n| n.bot_id == bot.id)
                .collect();
            if notes.is_empty() {
                continue;
            }
            notes.sort_by_key(|n| std::cmp::Reverse(n.updated_at.max(n.created_at)));
            let scope = MemoryScope::Workspace {
                bot_id: bot.id.clone(),
            };
            match self.fold_notes(&scope, &notes).await {
                Ok(()) => {
                    let ids: Vec<String> = notes.iter().map(|n| n.id.clone()).collect();
                    workspace.delete_records(p::KIND_BOT_NOTES, &ids);
                    tracing::info!(bot = %bot.id, notes = ids.len(), "notebook moved into memory");
                }
                Err(err) => {
                    tracing::warn!(bot = %bot.id, error = %err, "notebook migration failed");
                }
            }
        }
    }

    async fn fold_notes(
        &self,
        scope: &MemoryScope,
        notes: &[&p::BotNote],
    ) -> Result<(), EngineError> {
        let store_id = self.store_id(scope);
        let guard = self.store_lock(&store_id);
        let _held = guard.lock().await;
        let handle = self.open_for_write(&store_id).await?;
        let store = StoreDoc::new(handle.doc().doc());
        // Notes an older device wrote after an earlier move join the same
        // section; the rows go once they are in.
        let lines: Vec<String> = notes
            .iter()
            .map(|note| {
                let who = match &note.source {
                    p::NoteSource::Owner => "you".to_string(),
                    p::NoteSource::Bot { title, .. } => title
                        .as_deref()
                        .filter(|t| !t.trim().is_empty())
                        .map_or_else(|| "the workspace".to_string(), |t| format!("“{t}”")),
                };
                let day =
                    chrono::DateTime::from_timestamp_millis(note.updated_at.max(note.created_at))
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_default();
                let text = note.text.split_whitespace().collect::<Vec<_>>().join(" ");
                // Memory never holds secrets, a note's included.
                let (text, _) = crate::bots::scrub_secrets(&text);
                format!("- {text} _({who}, {day})_")
            })
            .collect();
        let existing = store.file(MEMORY_INDEX).map(|f| f.text).unwrap_or_default();
        let mut text = String::new();
        if !existing.trim().is_empty() {
            text.push_str(existing.trim_end());
            text.push_str("\n\n");
        }
        if !existing.contains("## From the notebook") {
            text.push_str("## From the notebook\n\n");
        }
        text.push_str(&lines.join("\n"));
        text.push('\n');
        let planned = Planned {
            path: MEMORY_INDEX.into(),
            text: Some(text),
            from: None,
        };
        apply(&store, &planned, &MemoryAuthor::Notebook)?;
        store.commit();
        self.sync_row(scope, &store_id, &store);
        Ok(())
    }

    fn wake_passes(&self) {
        self.inner.passes.wake.notify_one();
    }
}

/// The user's key in store ids: a hash of org and user, so it is stable
/// across the user's devices and says nothing about them.
fn user_key(org: &str, user: &str) -> String {
    let digest = Sha256::digest(format!("{org}\u{0}{user}").as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Refuse a write past the store's limits (said, never cut silently).
fn check_limits(store: &StoreDoc, planned: &Planned) -> Result<(), EngineError> {
    let Some(text) = &planned.text else {
        return Ok(());
    };
    if text.len() > p::MEMORY_FILE_MAX {
        return Err(EngineError::Other(format!(
            "{} would be {} KB; a memory file holds at most {} KB. Split it into topic files.",
            planned.path,
            text.len() / 1024,
            p::MEMORY_FILE_MAX / 1024
        )));
    }
    let files = store.files_list();
    let others: usize = files
        .iter()
        .filter(|f| f.path != planned.path && Some(&f.path) != planned.from.as_ref())
        .map(|f| f.text.len())
        .sum();
    if others + text.len() > p::MEMORY_STORE_MAX {
        return Err(EngineError::Other(format!(
            "This memory would hold {} KB; it holds at most {} KB. Remove or merge what is stale \
             first.",
            (others + text.len()) / 1024,
            p::MEMORY_STORE_MAX / 1024
        )));
    }
    let new_file = !files.iter().any(|f| f.path == planned.path);
    if new_file && planned.from.is_none() && files.len() >= p::MEMORY_FILES_MAX {
        return Err(EngineError::Other(format!(
            "This memory already has {} files, its most. Merge some first.",
            p::MEMORY_FILES_MAX
        )));
    }
    Ok(())
}

/// Apply `planned` and log it; the log line's summary.
fn apply(
    store: &StoreDoc,
    planned: &Planned,
    author: &MemoryAuthor,
) -> Result<String, EngineError> {
    let now = p::now_ms();
    let version = doc::version_hex(store.raw());
    let old = store.file(&planned.path);
    let moved = planned.from.as_ref().and_then(|from| store.file(from));
    let (action, summary, before, owner_text) = match (&planned.text, &planned.from) {
        (Some(text), Some(from)) => {
            let moved = moved.ok_or_else(|| EngineError::Other(format!("No file {from}.")))?;
            store.remove(from)?;
            store.put(&planned.path, text, author, now)?;
            (
                MemoryAction::Rename { from: from.clone() },
                "Renamed".to_string(),
                None,
                moved.author == MemoryAuthor::Owner && *author != MemoryAuthor::Owner,
            )
        }
        (Some(text), None) => {
            let summary = match &old {
                None => "Created".to_string(),
                Some(old) => line_summary(&old.text, text),
            };
            store.put(&planned.path, text, author, now)?;
            (
                if old.is_some() {
                    MemoryAction::Edit
                } else {
                    MemoryAction::Create
                },
                summary,
                old.as_ref().map(|f| f.text.clone()),
                old.as_ref().is_some_and(|f| {
                    f.author == MemoryAuthor::Owner && *author != MemoryAuthor::Owner
                }),
            )
        }
        (None, _) => {
            let old =
                old.ok_or_else(|| EngineError::Other(format!("No file {}.", planned.path)))?;
            store.remove(&planned.path)?;
            (
                MemoryAction::Delete,
                "Deleted".to_string(),
                Some(old.text.clone()),
                old.author == MemoryAuthor::Owner && *author != MemoryAuthor::Owner,
            )
        }
    };
    store.log(&MemoryChange {
        id: new_id(),
        at: now,
        author: author.clone(),
        path: planned.path.clone(),
        action,
        summary: summary.clone(),
        before: before.map(|text| p::clip(&text, 4_000)),
        owner_text,
        undo: Some(version),
        undone: false,
    })?;
    Ok(summary)
}

/// Write back what a file was (`None`: it didn't exist), unlogged.
fn restore(store: &StoreDoc, planned: &Planned, author: &MemoryAuthor) -> Result<(), EngineError> {
    match &planned.text {
        Some(text) => store.put(&planned.path, text, author, p::now_ms()),
        None if store.file(&planned.path).is_some() => store.remove(&planned.path),
        None => Ok(()),
    }
}

/// "Added 3 lines", "Removed 1 line", "Changed 2 lines".
fn line_summary(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let kept = old_lines.iter().filter(|l| new_lines.contains(l)).count();
    let removed = old_lines.len() - kept;
    let added = new_lines.iter().filter(|l| !old_lines.contains(l)).count();
    let n = |count: usize, what: &str| {
        format!("{count} line{}{what}", if count == 1 { "" } else { "s" })
    };
    match (added, removed) {
        (0, 0) => "Updated".into(),
        (a, 0) => format!("Added {}", n(a, "")),
        (0, r) => format!("Removed {}", n(r, "")),
        (a, r) if a == r => format!("Changed {}", n(a, "")),
        (a, r) => format!("Added {}, removed {r}", n(a, "")),
    }
}

/// Files listed in prompts and views: path and size.
fn size_label(file: &MemoryFile) -> String {
    let bytes = file.text.len();
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}
