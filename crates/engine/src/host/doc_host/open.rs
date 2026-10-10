//! Opening chat docs: load the stored snapshot (or seed a new doc), join the
//! chat room, and register the handle; plus temporary Side Chat docs.

use super::*;

impl DocHost {
    /// Open (or return) the chat's doc handle: load the local snapshot (or init fresh),
    /// start the change-driven task, and join the edge room when configured.
    pub fn open(&self, chat_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        // Every chat syncs through its chat2 room. The registry's `roomGen`
        // stays on the wire (iOS gates on it), but the host no longer reads
        // it: a row still marked gen 1, or no row at all (a chat being born
        // while its CreateChat mint races this open), opens as chat2 too.
        {
            let handles = lock(&self.inner.handles);
            if let Some(handle) = handles.get(chat_id) {
                handle.touch();
                return Ok(handle.clone());
            }
        }
        let stored = self.inner.store.load_snapshot_with_cursor(chat_id)?;
        let mut snapshot_len = 0usize;
        let mut chat2_cursor = 0u64;
        let mut requeue_commands: Vec<SessionCommandEntry> = Vec::new();
        let doc = match stored {
            Some((bytes, cursor, epoch)) if epoch >= crate::host::chat2_host::CHAT2_DOC_EPOCH => {
                snapshot_len = bytes.len();
                chat2_cursor = cursor;
                let raw = loro::LoroDoc::new();
                raw.import(&bytes)
                    .map_err(|e| EngineError::Other(format!("snapshot import failed: {e}")))?;
                SessionDoc::from_doc(raw)
            }
            Some((bytes, _cursor, epoch)) if self.inner.config.edge.is_none() => {
                // Offline/edge-less: adopting would blank a readable
                // transcript with no way to catch up. Keep the old doc
                // read-only-ish; the adopt runs on the next online open.
                tracing::info!(chat = %chat_id, old_epoch = epoch,
                    "chat2 adopt deferred (no edge configured)");
                snapshot_len = bytes.len();
                let raw = loro::LoroDoc::new();
                raw.import(&bytes)
                    .map_err(|e| EngineError::Other(format!("snapshot import failed: {e}")))?;
                SessionDoc::from_doc(raw)
            }
            Some((bytes, _cursor, epoch)) => {
                // Discard-and-adopt: this device's doc predates the
                // chat2 lineage. Keep the old snapshot under a suffixed
                // id for rollback, carry over OUR OWN unresolved
                // commands, and start fresh — the chat2 catch-up
                // (checkpoint + rows) repopulates the transcript. This
                // is the self-repair path: no user action, ever.
                tracing::info!(chat = %chat_id, old_epoch = epoch,
                    "chat2 adopt: discarding pre-chat2 local doc (rollback copy kept)");
                let rollback_id = format!("{chat_id}.pre-chat2");
                // A re-adopt after a mid-catch-up crash reruns this path
                // with a near-empty doc under `chat_id` — the FIRST
                // rollback copy is the real transcript; never overwrite
                // it.
                if matches!(self.inner.store.load_snapshot(&rollback_id), Ok(None)) {
                    let _ = self.inner.store.save_snapshot(&rollback_id, &bytes);
                }
                if let Ok(raw) = {
                    let old = loro::LoroDoc::new();
                    old.import(&bytes).map(|_| old)
                } {
                    let old_doc = SessionDoc::from_doc(raw);
                    if let Ok(commands) = old_doc.read_commands() {
                        requeue_commands = commands
                            .into_iter()
                            .filter(|c| {
                                c.status == SessionCommandStatus::Pending
                                    && c.issued_by == self.inner.config.device_id
                            })
                            .collect();
                    }
                }
                SessionDoc::init(chat_id)?
            }
            None => {
                // Born on chat2 (or a cold reader's first open): stamp
                // the epoch-2 lineage NOW. Plain snapshot saves preserve
                // an existing row's epoch but default a NEW row to 0 —
                // without this stamp, the next open reads "pre-chat2
                // doc" and the adopt DISCARDS everything written
                // since (caught by the restart_resume suite: first-turn
                // transcripts vanished on reopen).
                let doc = SessionDoc::init(chat_id)?;
                if let Ok(snapshot) = doc.export_snapshot() {
                    let _ = self.inner.store.save_snapshot_with_cursor(
                        chat_id,
                        &snapshot,
                        0,
                        crate::host::chat2_host::CHAT2_DOC_EPOCH,
                    );
                }
                doc
            }
        };
        // Replay before installing subscriptions or handing the document to
        // journal recovery. This works offline, not just on a successful dial.
        // Never advance the cloud cursor for local replay.
        for (_, bytes) in self.inner.store.load_outbox(chat_id)? {
            doc.doc()
                .import(&bytes)
                .map_err(|e| EngineError::Other(format!("outbox recovery import failed: {e}")))?;
        }
        let doc = Arc::new(doc);

        let (changed_tx, changed_rx) = watch::channel(0u64);
        let sub = doc.doc().subscribe_root(Arc::new(move |_diff| {
            changed_tx.send_modify(|v| *v = v.wrapping_add(1));
        }));
        // The mirror starts dirty and empty: many opens (command queueing,
        // drains, nudges) never watch the transcript, and the first
        // watch_messages attach materializes it on demand.
        let (messages_tx, _) = watch::channel(Vec::new());
        let (commands_tx, _) = watch::channel(Vec::new());

        let handle = Arc::new(ChatDocHandle {
            preview: OnceLock::new(),
            chat_id: chat_id.to_string(),
            device_id: self.inner.config.device_id.clone(),
            doc: doc.clone(),
            messages_tx,
            commands_tx,
            commands_dirty: AtomicBool::new(true),
            mirror_dirty: AtomicBool::new(true),
            last_access: AtomicI64::new(now_ms()),
            snapshot_bytes: AtomicUsize::new(snapshot_len),
            ephemeral: AtomicBool::new(false),
            checkpointing: Arc::new(AtomicBool::new(false)),
            chat2: Mutex::new(None),
            chat2_pending_local: Mutex::new(Vec::new()),
            chat2_local_sub: Mutex::new(None),
            _sub: sub,
        });
        {
            let mut handles = lock(&self.inner.handles);
            if let Some(existing) = handles.get(chat_id) {
                return Ok(existing.clone()); // racing open — keep the first
            }
            handles.insert(chat_id.to_string(), handle.clone());
        }

        // Edge room join — offline-tolerant AND supervised. `ChatClient` only
        // self-reconnects AFTER a first successful join; a one-shot attempt
        // here (the pre-LRU design) left the doc silently local-only until
        // app restart whenever the dial hit a transient gap — a post-wake
        // network, `Auth::token()` momentarily `None` around a refresh, an
        // edge deploy. The LRU made that dice-roll constant (every reopen),
        // and a watched doc is pinned against eviction, so nothing ever
        // retried: the exact "transcript frozen until restart" report.
        // Retry on the workspace host's capped, jittered backoff; a system
        // wake redials immediately; eviction/purge ends the loop via `weak`.
        if let Some(edge) = &self.inner.config.edge {
            // Subscription BEFORE the dial: every local
            // commit lands in the client when connected, else in the
            // pending buffer the join drains — nothing composed during
            // (or before) the dial is lost to the room.
            self.install_chat2_local_feed(&handle);
            // Re-queue survives the adopt: our own pending commands
            // become fresh entries in the new lineage (the
            // processed_commands ledger still guards double execution).
            // Committed AFTER the local-update subscription above — a
            // commit before it never enters the pending buffer or the
            // client, so the requeued command would sit in the local doc
            // and never reach the room (the host would never see it).
            for command in &requeue_commands {
                let _ = doc.queue_command(command);
            }
            // First contact with the room (cursor 0): everything
            // committed BEFORE the subscription above — SessionDoc::
            // init's container/meta ops, an adopt's fresh doc — is
            // invisible to the push path, yet every later commit
            // causally DEPENDS on it. Rows built on unpushed deps import
            // into peers' loro pending-buffers and never materialize:
            // born-chat2 cross-device runs sat invisible on every other
            // device (host never saw the command, viewers never saw the
            // transcript). Push the doc's full update log as the join's
            // first batch; once acked the cursor moves and this never
            // re-arms.
            if chat2_cursor == 0 {
                match doc
                    .doc()
                    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
                {
                    Ok(bytes) if !bytes.is_empty() => {
                        lock(&handle.chat2_pending_local).push(bytes);
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(chat = %chat_id, error = %err,
                            "chat2 first-contact export failed; peers may stall on missing deps");
                    }
                }
            }
            self.spawn_chat2_join(edge.clone(), &handle, chat2_cursor);
        }
        self.spawn_worker(chat_task(self.clone(), Arc::downgrade(&handle), changed_rx));
        self.evict_over_budget();
        Ok(handle)
    }
}

impl DocHost {
    /// Open (or return) a temporary Side Chat doc: a FRESH
    /// in-memory [`SessionDoc`] with no snapshot load/save, no chat2/edge
    /// room, no maintenance and no LRU eviction — host-memory only until
    /// promotion. The handle is registered in the same map so `WatchDocMessages`
    /// (which routes through [`Self::open`]) streams its transcript, and
    /// `purge_chat` tears it down. The worker is the standard `chat_task`,
    /// which skips persistence for ephemeral handles.
    pub fn open_ephemeral(&self, chat_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        {
            let handles = lock(&self.inner.handles);
            if let Some(handle) = handles.get(chat_id) {
                handle.touch();
                return Ok(handle.clone());
            }
        }
        let doc = Arc::new(SessionDoc::init(chat_id)?);
        let (changed_tx, changed_rx) = watch::channel(0u64);
        let sub = doc.doc().subscribe_root(Arc::new(move |_diff| {
            changed_tx.send_modify(|v| *v = v.wrapping_add(1));
        }));
        let (messages_tx, _) = watch::channel(Vec::new());
        let (commands_tx, _) = watch::channel(Vec::new());
        let handle = Arc::new(ChatDocHandle {
            preview: OnceLock::new(),
            chat_id: chat_id.to_string(),
            device_id: self.inner.config.device_id.clone(),
            doc: doc.clone(),
            messages_tx,
            commands_tx,
            commands_dirty: AtomicBool::new(true),
            mirror_dirty: AtomicBool::new(true),
            last_access: AtomicI64::new(now_ms()),
            snapshot_bytes: AtomicUsize::new(0),
            ephemeral: AtomicBool::new(true),
            checkpointing: Arc::new(AtomicBool::new(false)),
            chat2: Mutex::new(None),
            chat2_pending_local: Mutex::new(Vec::new()),
            chat2_local_sub: Mutex::new(None),
            _sub: sub,
        });
        {
            let mut handles = lock(&self.inner.handles);
            if let Some(existing) = handles.get(chat_id) {
                return Ok(existing.clone()); // racing open — keep the first
            }
            handles.insert(chat_id.to_string(), handle.clone());
        }
        self.spawn_worker(chat_task(self.clone(), Arc::downgrade(&handle), changed_rx));
        Ok(handle)
    }
}
