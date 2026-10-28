//! Session migration: move root chats, with their Cypher child chats, to
//! another project — on this device or another device of the same account.
//! The wire protocol and the order of a move are documented in
//! `cypher_proto::session_migration`.
//!
//! This device plays one of two parts in a migration:
//! - **source** ([`ChatMigrations::run`]), the chats' current host: it holds
//!   their command drains, parks idle runs, streams each chat's Pi session,
//!   run journal and transcript images to the destination, and once the
//!   destination has committed writes the same homes, drops its copies and
//!   lets go. A failure before the commit leaves every chat where it was:
//!   nothing here is deleted until the destination hosts the chat.
//! - **destination** (`receive_*`), the owner of the target project: it
//!   stages the files, installs them — the Pi session header names the
//!   project folder, which Pi resumes in — writes each chat's [`ChatHome`]
//!   and starts hosting.
//!
//! Both ends write the same home. Whichever registry clock wins, the
//! replicas agree, and each end stops or starts hosting the moment its own
//! write lands rather than when the room delivers the other's.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use cypher_doc::{MessagePart, MessageRole};
use cypher_proto::{
    Chat, ChatHome, ChatMigrationEvent, ChatMigrationRequest, HarnessId, MigratedAttachment,
    MigratedChat, MigratedFile, MigratedHome, MigrationBegin, MigrationBegun, MigrationChunk,
    MigrationCommit, MigrationCommitted, SessionStatus, Space,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;

use crate::EngineError;
use crate::git::repos::expand_home;
use crate::host::doc_host::DocHost;
use crate::host::workspace_host::WorkspaceHost;
use crate::session::engine::SessionsEngine;
use crate::session::forks::canonicalize_under_root;
use crate::session::journal::RunJournal;
use crate::uploads::Uploads;
use crate::util::{lock, new_id};

/// Raw bytes per relayed slice; the base64 rides under the device relay's
/// frame cap (the file manager imports in slices of the same size).
const CHUNK_BYTES: usize = 384 * 1024;
/// Transcript images copied with one chat family, in total.
const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
/// Bytes one inbound migration may stage.
const MAX_STAGED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// An inbound migration idle this long belongs to a source that is gone.
const STAGING_TTL: Duration = Duration::from_secs(60 * 60);
/// Deepest child-chat nesting followed.
const MAX_CHILD_DEPTH: usize = 8;

/// The device that owns the destination project, as the source reaches it:
/// this engine itself, or another one over the device relay.
#[async_trait]
pub trait MigrationPeer: Send + Sync {
    /// The destination is this device: journals and images stay put.
    fn is_local(&self) -> bool;
    async fn begin(&self, request: MigrationBegin) -> Result<MigrationBegun, EngineError>;
    async fn write_chunk(&self, chunk: MigrationChunk) -> Result<(), EngineError>;
    async fn commit(&self, commit: MigrationCommit) -> Result<MigrationCommitted, EngineError>;
    /// Best-effort: drop the staged files.
    async fn abort(&self, migration_id: &str);
}

/// One requested chat, as planned before anything moves.
enum Planned {
    Move(Box<Chat>),
    Refuse {
        chat_id: String,
        title: Option<String>,
        reason: String,
    },
}

/// What a `MigrateChats` request resolves to.
pub struct MigrationPlan {
    to_space: Space,
    items: Vec<Planned>,
}

impl MigrationPlan {
    /// The device that will host the moved chats.
    pub fn destination_device(&self) -> &str {
        &self.to_space.device_id
    }
}

/// An inbound migration's staging folder.
struct Inbound {
    to_space_id: String,
    /// The project folder, `~` expanded here.
    cwd: String,
    dir: PathBuf,
    /// Staging key → bytes staged.
    files: HashMap<String, u64>,
    staged: u64,
    touched: Instant,
}

struct Inner {
    sessions: SessionsEngine,
    doc_host: DocHost,
    workspace: WorkspaceHost,
    journal: Arc<RunJournal>,
    uploads: Uploads,
    /// Cypher's Pi session store (`<profile store>/agent-sessions`): moved
    /// sessions install here, and only files under it are ever deleted.
    pi_sessions_root: PathBuf,
    /// Inbound staging folders (`<profile store>/migrations`).
    staging_root: PathBuf,
    /// One outbound migration at a time from this device.
    outbound: AsyncMutex<()>,
    inbound: Mutex<HashMap<String, Inbound>>,
}

/// Session migration, both ends.
#[derive(Clone)]
pub struct ChatMigrations {
    inner: Arc<Inner>,
}

impl ChatMigrations {
    pub fn new(
        sessions: SessionsEngine,
        doc_host: DocHost,
        workspace: WorkspaceHost,
        journal: Arc<RunJournal>,
        uploads: Uploads,
        store_root: &Path,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                sessions,
                doc_host,
                workspace,
                journal,
                uploads,
                pi_sessions_root: store_root.join("agent-sessions"),
                staging_root: store_root.join("migrations"),
                outbound: AsyncMutex::new(()),
                inbound: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn device_id(&self) -> &str {
        self.inner.workspace.device_id()
    }

    // ── source ──────────────────────────────────────────────────────────────

    /// Resolve a request: the destination project and, per requested root
    /// chat, whether it can move. A project move takes every root chat in
    /// it, archived ones too.
    pub fn plan(&self, request: &ChatMigrationRequest) -> Result<MigrationPlan, EngineError> {
        let to_space = self
            .inner
            .workspace
            .space(&request.to_space_id)?
            .ok_or_else(|| {
                EngineError::Other("The destination project no longer exists.".into())
            })?;
        let chats = self.inner.workspace.read_chats()?;
        let ids: Vec<String> = match request.from_space_id.as_deref() {
            Some(from) if from == to_space.id => {
                return Err(EngineError::Other(
                    "Pick a different project to move the sessions to.".into(),
                ));
            }
            Some(from) => {
                let mut roots: Vec<&Chat> = chats
                    .iter()
                    .filter(|c| c.space_id.as_deref() == Some(from) && !c.is_child())
                    .collect();
                roots.sort_by_key(|c| c.created_at);
                roots.into_iter().map(|c| c.id.clone()).collect()
            }
            None => request.chat_ids.clone(),
        };
        let items = ids
            .into_iter()
            .map(|id| match chats.iter().find(|c| c.id == id) {
                None => Planned::Refuse {
                    chat_id: id,
                    title: None,
                    reason: "The session no longer exists.".into(),
                },
                Some(chat) => match self.refusal(chat, &to_space) {
                    Some(reason) => Planned::Refuse {
                        chat_id: chat.id.clone(),
                        title: chat.title.clone(),
                        reason: reason.into(),
                    },
                    None => Planned::Move(Box::new(chat.clone())),
                },
            })
            .collect();
        Ok(MigrationPlan { to_space, items })
    }

    /// Why `chat` can't move to `to`, when it can't.
    fn refusal(&self, chat: &Chat, to: &Space) -> Option<&'static str> {
        if chat.is_child() {
            return Some("Subagent sessions move with their parent session.");
        }
        if self.inner.sessions.is_ephemeral(&chat.id) {
            return Some("Temporary side chats can't be moved.");
        }
        if chat.device_id != self.device_id() {
            return Some("This session is hosted on another device.");
        }
        match chat.space_id.as_deref() {
            None => Some("Quick chats can't be moved to a project."),
            Some(space) if space == to.id => Some("The session is already in this project."),
            Some(_) => None,
        }
    }

    /// Move every planned chat, reporting each as it settles. Chats move one
    /// family at a time; a failed family stays where it was and the rest
    /// carry on.
    pub async fn run(
        &self,
        plan: MigrationPlan,
        peer: Arc<dyn MigrationPeer>,
        events: mpsc::UnboundedSender<ChatMigrationEvent>,
    ) {
        let _one_at_a_time = self.inner.outbound.lock().await;
        let total = plan.items.len();
        let _ = events.send(ChatMigrationEvent::Start { total });
        let (mut moved, mut failed) = (0, 0);
        for (index, item) in plan.items.into_iter().enumerate() {
            let (chat_id, title, error) = match item {
                Planned::Move(chat) => {
                    let result = self.move_family(&chat, &plan.to_space, peer.as_ref()).await;
                    (chat.id, chat.title, result.err().map(|e| e.to_string()))
                }
                Planned::Refuse {
                    chat_id,
                    title,
                    reason,
                } => (chat_id, title, Some(reason)),
            };
            match &error {
                None => {
                    moved += 1;
                    tracing::info!(chat = %chat_id, space = %plan.to_space.id,
                        device = %plan.to_space.device_id, "session moved");
                }
                Some(error) => {
                    failed += 1;
                    tracing::warn!(chat = %chat_id, space = %plan.to_space.id, %error,
                        "session move failed");
                }
            }
            let _ = events.send(ChatMigrationEvent::Chat {
                chat_id,
                title,
                index: index + 1,
                total,
                error,
            });
        }
        let _ = events.send(ChatMigrationEvent::Done { moved, failed });
    }

    /// Move one root chat and its child chats.
    async fn move_family(
        &self,
        root: &Chat,
        to: &Space,
        peer: &dyn MigrationPeer,
    ) -> Result<(), EngineError> {
        let family = self.family(root)?;
        let _holds = Holds::take(&self.inner.doc_host, &family)?;
        for chat in &family {
            let status = self
                .inner
                .sessions
                .session_status(&chat.id)
                .map(|s| s.status);
            if matches!(
                status,
                Some(SessionStatus::Working | SessionStatus::AwaitingInput)
            ) {
                return Err(EngineError::Other(
                    "The session is still running. Move it once the current turn settles.".into(),
                ));
            }
            if self.inner.doc_host.unsettled_commands(&chat.id)? > 0 {
                return Err(EngineError::Other(
                    "The session has a message waiting to run. Move it once it has run.".into(),
                ));
            }
        }
        // A parked run keeps its Pi process attached to the session file.
        for chat in &family {
            if self.inner.sessions.has_run(&chat.id) {
                self.inner.sessions.quiesce_idle_for_fork(&chat.id).await?;
            }
        }

        let migration_id = new_id();
        peer.begin(MigrationBegin {
            migration_id: migration_id.clone(),
            to_space_id: to.id.clone(),
        })
        .await?;
        let (commit, sessions) = match self.send_family(&migration_id, &to.id, &family, peer).await
        {
            Ok(sent) => sent,
            Err(err) => {
                peer.abort(&migration_id).await;
                return Err(err);
            }
        };
        let committed = match peer.commit(commit).await {
            Ok(committed) => committed,
            Err(err) => {
                peer.abort(&migration_id).await;
                // A lost reply can hide a commit that landed: write the
                // chats' homes here again so this device's clock decides.
                self.restore_homes(&family, &sessions);
                return Err(err);
            }
        };
        for moved in &committed.homes {
            if let Err(err) = self
                .inner
                .workspace
                .rehome_chat(&moved.chat_id, &moved.home)
            {
                tracing::warn!(chat = %moved.chat_id, error = %err, "moved chat: home write failed");
            }
        }
        self.drop_source_copies(&family, &sessions, peer.is_local());
        Ok(())
    }

    /// `root` and every child chat below it.
    fn family(&self, root: &Chat) -> Result<Vec<Chat>, EngineError> {
        let chats = self.inner.workspace.read_chats()?;
        let mut family = vec![root.clone()];
        let mut parents: HashSet<String> = HashSet::from([root.id.clone()]);
        for _ in 0..MAX_CHILD_DEPTH {
            let children: Vec<Chat> = chats
                .iter()
                .filter(|c| {
                    c.parent_chat_id().is_some_and(|p| parents.contains(p))
                        && !family.iter().any(|f| f.id == c.id)
                })
                .cloned()
                .collect();
            if children.is_empty() {
                break;
            }
            parents = children.iter().map(|c| c.id.clone()).collect();
            family.extend(children);
        }
        Ok(family)
    }

    /// Stream the family's files to the destination; returns the commit and
    /// the Pi session each chat resumed (by chat id).
    async fn send_family(
        &self,
        migration_id: &str,
        to_space_id: &str,
        family: &[Chat],
        peer: &dyn MigrationPeer,
    ) -> Result<(MigrationCommit, HashMap<String, PathBuf>), EngineError> {
        let mut chats = Vec::with_capacity(family.len());
        let mut sessions = HashMap::new();
        let mut attachments = Vec::new();
        let mut images_seen = HashSet::new();
        let mut image_bytes = 0u64;
        for (i, chat) in family.iter().enumerate() {
            let cwd = chat.cwd.clone().unwrap_or_default();
            let session = match self.session_file(chat, &cwd) {
                Some(path) => {
                    let key = format!("s{i}");
                    send_file(peer, migration_id, &key, &path).await?;
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| format!("{}.jsonl", chat.id));
                    sessions.insert(chat.id.clone(), path);
                    Some(MigratedFile { file: key, name })
                }
                None => None,
            };
            let journal = if peer.is_local() {
                None
            } else {
                let path = self.inner.journal.journal_path(&chat.id);
                if path.is_file() {
                    let key = format!("j{i}");
                    send_file(peer, migration_id, &key, &path).await?;
                    Some(key)
                } else {
                    None
                }
            };
            let mut previous_cwds = vec![cwd.clone()];
            let expanded = expand_home(&cwd);
            if expanded != cwd {
                previous_cwds.push(expanded);
            }
            chats.push(MigratedChat {
                chat_id: chat.id.clone(),
                session,
                journal,
                previous_cwds,
            });
            if peer.is_local() {
                continue;
            }
            for source_path in self.transcript_images(&chat.id) {
                if !images_seen.insert(source_path.clone()) {
                    continue;
                }
                let Some(file) = self.inner.uploads.migratable_image(&source_path) else {
                    continue;
                };
                let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
                if image_bytes + size > MAX_IMAGE_BYTES {
                    tracing::warn!(chat = %chat.id, image = %source_path,
                        "session move: image budget spent, image left behind");
                    continue;
                }
                image_bytes += size;
                let key = format!("a{}", attachments.len());
                send_file(peer, migration_id, &key, &file).await?;
                attachments.push(MigratedAttachment {
                    file: key,
                    source_path,
                });
            }
        }
        Ok((
            MigrationCommit {
                migration_id: migration_id.to_string(),
                to_space_id: to_space_id.to_string(),
                chats,
                attachments,
            },
            sessions,
        ))
    }

    /// The Pi session file the chat's next run would resume, if any.
    fn session_file(&self, chat: &Chat, cwd: &str) -> Option<PathBuf> {
        let pi = chat
            .config
            .as_ref()
            .is_none_or(|config| config.harness == HarnessId::Pi);
        if !pi {
            return None; // a retired harness can't continue anywhere
        }
        let path = PathBuf::from(self.inner.sessions.resumable_session(&chat.id, cwd)?);
        path.is_file().then_some(path)
    }

    /// Image paths the chat's user messages attached.
    fn transcript_images(&self, chat_id: &str) -> Vec<String> {
        let Ok(handle) = self.inner.doc_host.open(chat_id) else {
            return Vec::new();
        };
        let entries = handle.doc().read_entries().unwrap_or_default();
        entries
            .iter()
            .filter(|entry| entry.role == MessageRole::User)
            .flat_map(|entry| entry.parts.iter())
            .filter_map(|part| match part {
                MessagePart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .flat_map(cypher_proto::attachment_refs::paths)
            .filter(|path| cypher_proto::attachment_refs::is_image_path(path))
            .collect()
    }

    /// A commit failed: put every chat's home back as it was.
    fn restore_homes(&self, family: &[Chat], sessions: &HashMap<String, PathBuf>) {
        for chat in family {
            let Some(space_id) = chat.space_id.clone() else {
                continue;
            };
            let cwd = chat.cwd.clone().unwrap_or_default();
            let home = ChatHome {
                device_id: chat.device_id.clone(),
                space_id,
                harness_session_cwd: chat
                    .harness_session_cwd
                    .clone()
                    .unwrap_or_else(|| expand_home(&cwd)),
                harness_session_id: sessions
                    .get(&chat.id)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                cwd,
            };
            if let Err(err) = self.inner.workspace.rehome_chat(&chat.id, &home) {
                tracing::warn!(chat = %chat.id, error = %err, "session move: home restore failed");
                continue;
            }
            // A home write clears the checkout stamps; these never moved.
            if let Some(branch) = chat.branch.as_deref() {
                let _ = self.inner.workspace.set_chat_branch(&chat.id, branch);
            }
            if let Some(checkout) = chat.checkout_id.as_deref() {
                let _ = self.inner.workspace.set_chat_checkout(&chat.id, checkout);
            }
        }
    }

    /// The destination hosts the family now: forget it here and delete the
    /// files it left behind.
    fn drop_source_copies(
        &self,
        family: &[Chat],
        sessions: &HashMap<String, PathBuf>,
        local: bool,
    ) {
        let rows = self.inner.workspace.read_chats().unwrap_or_default();
        for chat in family {
            if !local {
                self.inner.sessions.forget_moved_chat(&chat.id);
                if let Err(err) = self.inner.journal.discard(&chat.id) {
                    tracing::warn!(chat = %chat.id, error = %err, "moved chat: journal delete failed");
                }
            }
            let Some(old) = sessions.get(&chat.id) else {
                continue;
            };
            // A fork shares no file with its source, but a row naming this
            // one (the moved chat itself, should its home write have
            // failed) keeps it.
            let old_id = old.to_string_lossy();
            if rows
                .iter()
                .any(|row| row.harness_session_id.as_deref() == Some(&*old_id))
            {
                continue;
            }
            match canonicalize_under_root(old, &self.inner.pi_sessions_root) {
                Some(path) => {
                    if let Err(err) = std::fs::remove_file(&path)
                        && err.kind() != std::io::ErrorKind::NotFound
                    {
                        tracing::warn!(session = %path.display(), error = %err,
                            "moved chat: old Pi session delete failed");
                    }
                }
                None => tracing::info!(session = %old.display(),
                    "moved chat: old Pi session kept (outside the managed store)"),
            }
        }
    }

    // ── destination ─────────────────────────────────────────────────────────

    /// `BeginChatMigration`: check this device owns the project and open a
    /// staging folder.
    pub fn receive_begin(&self, request: MigrationBegin) -> Result<MigrationBegun, EngineError> {
        valid_key(&request.migration_id)?;
        let space = self.destination_space(&request.to_space_id)?;
        let cwd = expand_home(&space.path);
        if !Path::new(&cwd).is_dir() {
            return Err(EngineError::Other(format!(
                "The project folder {cwd} doesn't exist on the destination device."
            )));
        }
        self.sweep_staging();
        let dir = self.inner.staging_root.join(&request.migration_id);
        std::fs::create_dir_all(&dir)?;
        lock(&self.inner.inbound).insert(
            request.migration_id,
            Inbound {
                to_space_id: space.id,
                cwd: cwd.clone(),
                dir,
                files: HashMap::new(),
                staged: 0,
                touched: Instant::now(),
            },
        );
        Ok(MigrationBegun { cwd })
    }

    /// `WriteChatMigrationChunk`: append one slice. A slice already staged
    /// (a retry after a lost reply) is accepted without writing it twice.
    pub fn receive_chunk(&self, chunk: MigrationChunk) -> Result<(), EngineError> {
        valid_key(&chunk.file)?;
        let bytes = BASE64
            .decode(chunk.data.as_bytes())
            .map_err(|e| EngineError::Other(format!("migration slice is not base64: {e}")))?;
        let mut inbound = lock(&self.inner.inbound);
        let state = inbound
            .get_mut(&chunk.migration_id)
            .ok_or_else(|| EngineError::Other("Unknown or expired migration.".into()))?;
        let have = state.files.get(&chunk.file).copied().unwrap_or(0);
        let end = chunk.offset + bytes.len() as u64;
        if end <= have && chunk.offset < have {
            return Ok(());
        }
        if chunk.offset != have {
            return Err(EngineError::Other("Migration slice out of order.".into()));
        }
        if state.staged + bytes.len() as u64 > MAX_STAGED_BYTES {
            return Err(EngineError::Other(
                "The sessions are too large to move.".into(),
            ));
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(state.dir.join(&chunk.file))?;
        file.write_all(&bytes)?;
        state.files.insert(chunk.file, end);
        state.staged += bytes.len() as u64;
        state.touched = Instant::now();
        Ok(())
    }

    /// `CommitChatMigration`: install the staged files, then host the chats.
    pub async fn receive_commit(
        &self,
        commit: MigrationCommit,
    ) -> Result<MigrationCommitted, EngineError> {
        let state = lock(&self.inner.inbound)
            .remove(&commit.migration_id)
            .ok_or_else(|| EngineError::Other("Unknown or expired migration.".into()))?;
        let result = self.install(&commit, &state).await;
        let _ = std::fs::remove_dir_all(&state.dir);
        result
    }

    /// `AbortChatMigration`.
    pub fn receive_abort(&self, migration_id: &str) {
        if let Some(state) = lock(&self.inner.inbound).remove(migration_id) {
            let _ = std::fs::remove_dir_all(&state.dir);
        }
    }

    async fn install(
        &self,
        commit: &MigrationCommit,
        state: &Inbound,
    ) -> Result<MigrationCommitted, EngineError> {
        if commit.to_space_id != state.to_space_id {
            return Err(EngineError::Other(
                "The migration names another project.".into(),
            ));
        }
        let space = self.destination_space(&commit.to_space_id)?;
        for chat in &commit.chats {
            if self.inner.workspace.chat(&chat.chat_id)?.is_none() {
                return Err(EngineError::Other(
                    "The session hasn't synced to the destination device yet. Try again in a moment."
                        .into(),
                ));
            }
        }
        for key in commit
            .chats
            .iter()
            .flat_map(|c| c.session.iter().map(|s| &s.file).chain(c.journal.iter()))
            .chain(commit.attachments.iter().map(|a| &a.file))
        {
            if !state.files.contains_key(key) {
                return Err(EngineError::Other(format!(
                    "The migration is missing a file ({key})."
                )));
            }
        }

        // Files first: a chat starts hosting here only once all of its
        // family's sessions are in place.
        let sessions: Vec<(String, MigratedFile)> = commit
            .chats
            .iter()
            .filter_map(|c| c.session.clone().map(|s| (c.chat_id.clone(), s)))
            .collect();
        let installed = {
            let dir = state.dir.clone();
            let root = self.inner.pi_sessions_root.clone();
            let cwd = state.cwd.clone();
            tokio::task::spawn_blocking(move || install_sessions(&dir, &root, &cwd, &sessions))
                .await
                .map_err(|e| EngineError::Other(format!("session install: {e}")))??
        };
        for chat in &commit.chats {
            if let Some(key) = &chat.journal
                && let Err(err) = self
                    .inner
                    .journal
                    .adopt(&chat.chat_id, &state.dir.join(key))
            {
                tracing::warn!(chat = %chat.chat_id, error = %err, "moved chat: journal adopt failed");
            }
        }
        for image in &commit.attachments {
            if let Err(err) = self
                .inner
                .uploads
                .adopt_migrated(&image.source_path, &state.dir.join(&image.file))
            {
                tracing::warn!(image = %image.source_path, error = %err,
                    "moved chat: image adopt failed");
            }
        }

        let device_id = self.device_id().to_string();
        let mut homes = Vec::with_capacity(commit.chats.len());
        for chat in &commit.chats {
            let home = ChatHome {
                device_id: device_id.clone(),
                space_id: space.id.clone(),
                cwd: space.path.clone(),
                harness_session_id: installed
                    .get(&chat.chat_id)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                harness_session_cwd: state.cwd.clone(),
            };
            self.inner.sessions.adopt_harness_session(
                &chat.chat_id,
                &home.harness_session_id,
                &home.harness_session_cwd,
            );
            self.inner.doc_host.note_moved_cwd(
                &chat.chat_id,
                chat.previous_cwds.clone(),
                &space.path,
            );
            self.inner.workspace.rehome_chat(&chat.chat_id, &home)?;
            homes.push(MigratedHome {
                chat_id: chat.chat_id.clone(),
                home,
            });
        }
        for home in &homes {
            self.inner.doc_host.kick_drain(&home.chat_id);
        }
        Ok(MigrationCommitted { homes })
    }

    /// The destination project, which this device must own.
    fn destination_space(&self, space_id: &str) -> Result<Space, EngineError> {
        match self.inner.workspace.space(space_id)? {
            Some(space) if space.device_id == self.device_id() => Ok(space),
            Some(_) => Err(EngineError::Other(
                "The destination project is on another device.".into(),
            )),
            None => Err(EngineError::Other(
                "The destination project no longer exists.".into(),
            )),
        }
    }

    /// Drop inbound migrations whose source went quiet, and their folders.
    fn sweep_staging(&self) {
        let live: HashSet<PathBuf> = {
            let mut inbound = lock(&self.inner.inbound);
            inbound.retain(|_, state| {
                let alive = state.touched.elapsed() < STAGING_TTL;
                if !alive {
                    let _ = std::fs::remove_dir_all(&state.dir);
                }
                alive
            });
            inbound.values().map(|s| s.dir.clone()).collect()
        };
        let Ok(entries) = std::fs::read_dir(&self.inner.staging_root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age > STAGING_TTL);
            if stale && !live.contains(&path) {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
}

#[async_trait]
impl MigrationPeer for ChatMigrations {
    fn is_local(&self) -> bool {
        true
    }

    async fn begin(&self, request: MigrationBegin) -> Result<MigrationBegun, EngineError> {
        self.receive_begin(request)
    }

    async fn write_chunk(&self, chunk: MigrationChunk) -> Result<(), EngineError> {
        self.receive_chunk(chunk)
    }

    async fn commit(&self, commit: MigrationCommit) -> Result<MigrationCommitted, EngineError> {
        self.receive_commit(commit).await
    }

    async fn abort(&self, migration_id: &str) {
        self.receive_abort(migration_id);
    }
}

/// Command-drain holds on a chat family, released when dropped — whether
/// the move committed (the chats are hosted elsewhere now) or failed (they
/// drain here again).
struct Holds {
    doc_host: DocHost,
    chat_ids: Vec<String>,
}

impl Holds {
    fn take(doc_host: &DocHost, family: &[Chat]) -> Result<Self, EngineError> {
        let mut holds = Self {
            doc_host: doc_host.clone(),
            chat_ids: Vec::with_capacity(family.len()),
        };
        for chat in family {
            if !doc_host.hold_commands(&chat.id) {
                return Err(EngineError::Other(
                    "The session is already being moved.".into(),
                ));
            }
            holds.chat_ids.push(chat.id.clone());
        }
        Ok(holds)
    }
}

impl Drop for Holds {
    fn drop(&mut self) {
        for chat_id in &self.chat_ids {
            self.doc_host.release_commands(chat_id);
        }
    }
}

/// Stream one file to the destination in relay-sized slices. An empty file
/// still sends one (empty) slice so it exists there.
async fn send_file(
    peer: &dyn MigrationPeer,
    migration_id: &str,
    key: &str,
    path: &Path,
) -> Result<(), EngineError> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut offset = 0u64;
    loop {
        let n = read_full(&mut file, &mut buf)?;
        peer.write_chunk(MigrationChunk {
            migration_id: migration_id.to_string(),
            file: key.to_string(),
            offset,
            data: BASE64.encode(&buf[..n]),
        })
        .await?;
        offset += n as u64;
        if n < CHUNK_BYTES {
            return Ok(());
        }
    }
}

/// Fill `buf` unless the file ends first; returns the bytes read.
fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut read = 0;
    while read < buf.len() {
        match file.read(&mut buf[read..])? {
            0 => break,
            n => read += n,
        }
    }
    Ok(read)
}

/// Staging keys and migration ids become file names.
fn valid_key(key: &str) -> Result<(), EngineError> {
    let ok = !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    if ok {
        Ok(())
    } else {
        Err(EngineError::Other("Invalid migration key.".into()))
    }
}

/// Install each staged Pi session under `root`, pointed at `cwd`. On any
/// failure the sessions installed so far are removed again.
fn install_sessions(
    dir: &Path,
    root: &Path,
    cwd: &str,
    sessions: &[(String, MigratedFile)],
) -> Result<HashMap<String, PathBuf>, EngineError> {
    std::fs::create_dir_all(root)?;
    let mut installed = HashMap::new();
    for (chat_id, file) in sessions {
        let target = session_target(root, &file.name);
        if let Err(err) = rehome_pi_session(&dir.join(&file.file), &target, cwd) {
            for path in installed.values() {
                let _ = std::fs::remove_file(path);
            }
            return Err(err);
        }
        installed.insert(chat_id.clone(), target);
    }
    Ok(installed)
}

/// Where a moved Pi session lands: its own file name when free, else the
/// name with a short suffix (a same-device move, or a file left behind by a
/// move away and back).
fn session_target(root: &Path, name: &str) -> PathBuf {
    let base = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| {
            n.ends_with(".jsonl")
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        })
        .unwrap_or_else(|| format!("{}.jsonl", new_id()));
    let target = root.join(&base);
    if !target.exists() {
        return target;
    }
    let stem = base.trim_end_matches(".jsonl");
    let id8: String = new_id().chars().take(8).collect();
    root.join(format!("{stem}-{id8}.jsonl"))
}

/// Copy a Pi session to `target` with its header pointing at `cwd`. Pi
/// resumes a session in the folder its header names and refuses one whose
/// folder is missing, so a session moving folders must say so — the same
/// rewrite Pi's own `SessionManager.forkFrom` makes. Every other line is
/// copied byte for byte.
pub(crate) fn rehome_pi_session(
    staged: &Path,
    target: &Path,
    cwd: &str,
) -> Result<(), EngineError> {
    let not_pi = || EngineError::Other("The session file is not a Pi session.".into());
    let mut reader = BufReader::new(std::fs::File::open(staged)?);
    let mut header = String::new();
    reader.read_line(&mut header)?;
    let mut value: serde_json::Value =
        serde_json::from_str(header.trim_end()).map_err(|_| not_pi())?;
    let object = value
        .as_object_mut()
        .filter(|o| o.get("type").and_then(serde_json::Value::as_str) == Some("session"))
        .ok_or_else(not_pi)?;
    object.insert("cwd".into(), serde_json::Value::String(cwd.to_string()));
    let part = target.with_extension("jsonl.part");
    {
        let mut out = BufWriter::new(std::fs::File::create(&part)?);
        serde_json::to_writer(&mut out, &value)
            .map_err(|e| EngineError::Other(format!("session header: {e}")))?;
        out.write_all(b"\n")?;
        std::io::copy(&mut reader, &mut out)?;
        out.flush()?;
    }
    std::fs::rename(&part, target)?;
    Ok(())
}

#[cfg(test)]
mod tests;
