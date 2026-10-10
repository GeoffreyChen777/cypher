//! The chat2 room side of [`DocHost`]: the local-update feed, the
//! supervised room join, and the host's checkpoint duties.

use super::*;

impl DocHost {
    /// Feed local commits to the chat2 client through a channel.
    ///
    /// Rule for every Loro hook in this host: the hook only forwards data;
    /// it takes no engine lock and does no I/O. Loro runs subscriptions
    /// synchronously inside commit and export, on whichever thread caused
    /// them, and while a hook runs Loro parks every OTHER thread that tries
    /// to commit to the same doc (a 10ms spin in `loro-internal`
    /// `utils/subscription.rs`). A lock taken inside a hook therefore turns
    /// ordinary contention into a cross-thread stall, and a hook triggered by
    /// the client's own sink (export on ACK) into a self-deadlock. That is
    /// how a headless hang once started: one blocked ACK, then the
    /// agent run parked on the emitter guard, then every runtime worker.
    /// The pump task below does the client/pending routing under the client
    /// lock, outside any Loro hook.
    pub(super) fn install_chat2_local_feed(&self, handle: &Arc<ChatDocHandle>) {
        let (feed_tx, mut feed_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, bool)>();
        let sub = handle
            .doc
            .doc()
            .subscribe_local_update(Box::new(move |bytes: &Vec<u8>| {
                // The deferral mark is read here, inside the commit on the
                // committing thread — a thread-local, so no lock.
                let deferred = cypher_doc::local_commit_is_deferrable();
                // `false` unsubscribes once the pump is gone (handle evicted).
                feed_tx.send((bytes.clone(), deferred)).is_ok()
            }));
        *lock(&handle.chat2_local_sub) = Some(sub);
        let weak = Arc::downgrade(handle);
        self.spawn_worker(async move {
            while let Some((bytes, deferred)) = feed_rx.recv().await {
                let Some(handle) = weak.upgrade() else { return };
                handle.route_local_update(bytes, deferred);
            }
        });
    }

    /// chat2 relay join (docs/design/chat2-sync.md C3): deadline on every dial,
    /// capped jittered backoff, wake redial. With pull-first transport the
    /// client is returned local-first while HTTPS/WS convergence continues;
    /// `server_known` remains the gate for any server-truth recovery action.
    pub(super) fn spawn_chat2_join(
        &self,
        edge: EdgeConfig,
        handle: &Arc<ChatDocHandle>,
        cursor: u64,
    ) {
        if let Some(mut options) = edge.preview.clone() {
            if !self.preview_is_host(&handle.chat_id) {
                options.publisher_token = None;
            }
            let preview = handle
                .preview
                .get_or_init(|| {
                    cypher_sync::preview_link::PreviewLink::new(&handle.chat_id, options)
                })
                .clone();
            let weak = Arc::downgrade(handle);
            preview.on_change(Arc::new(move || {
                if let Some(handle) = weak.upgrade() {
                    handle.publish_messages_if_watched();
                }
            }));
            if preview.options().publisher_token.is_some() {
                handle
                    .doc
                    .set_preview_hook(Arc::new(move |entry, parts, complete| {
                        preview.stage(entry, parts, complete)
                    }));
            }
        }
        let preview = handle.preview.get().cloned();
        let chat = handle.chat_id.clone();
        let doc = handle.doc.clone();
        let store = self.inner.store.clone();
        let http = self.inner.http.clone();
        let device = self.inner.config.device_id.clone();
        let weak = Arc::downgrade(handle);
        let host = self.clone();
        let mut token_changes = edge.token_changes();
        self.spawn_worker(async move {
            let sink = Arc::new(crate::chat2_host::EngineChatSink::new(&doc, store, chat.clone()).with_preview(preview));
            // The sink holds only a Weak doc ref (a strong one made every
            // chat2 handle read as perma-pinned — LRU eviction dead); this
            // task's own strong ref dies when the join resolves.
            drop(doc);
            let fetcher = Arc::new(crate::chat2_host::EdgeCheckpointFetcher::new(
                http.clone(),
                edge.clone(),
                chat.clone(),
            ));
            let transport = Arc::new(crate::chat2_host::EdgeChatTransport::new(
                http,
                edge.clone(),
                chat.clone(),
                device.clone(),
            ));
            let url = edge.room_url(format!("/chat2/{chat}/ws"));
            let mut wake = cypher_sync::wake::subscribe();
            let mut backoff = crate::workspace_host::JOIN_RETRY_BASE;
            loop {
                if weak.upgrade().is_none() {
                    return; // evicted or purged while dialing
                }
                let dial = tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    cypher_sync::ChatClient::connect_via_transport(
                        url.clone(),
                        sink.clone(),
                        fetcher.clone(),
                        &device,
                        cursor,
                        transport.clone(),
                    ),
                )
                .await;
                match dial {
                    Ok(Ok(client)) => {
                        if edge.bearer().await.is_none() {
                            return;
                        }
                        let Some(handle) = weak.upgrade() else {
                            return; // evicted mid-dial: drop leaves the room
                        };
                        let mut events = client.events();
                        let mut lifecycle_events = client.events();
                        {
                            // Store + drain under ONE client-lock critical
                            // section: the subscription pushes to the buffer
                            // while holding this same lock, so every commit
                            // is either drained here or enqueued directly
                            // after — never dropped between.
                            let mut client_slot = lock(&handle.chat2);
                            let pending: Vec<Vec<u8>> =
                                std::mem::take(&mut *lock(&handle.chat2_pending_local));
                            for update in pending {
                                client.enqueue_update(update);
                            }
                            *client_slot = Some(client);
                        }
                        tracing::info!(chat = %chat, "chat2 room joined (converged)");
                        // Bootstrap heal: a room with NO checkpoint can't
                        // cover its rows' causal deps for cold readers — a
                        // first contact whose init batch never went up (every reader parks every row on missing
                        // deps, transcript invisible forever), or a host
                        // whose WS pushes strand. The checkpoint is the
                        // universal patch: full doc over plain HTTP. Checked
                        // once, shortly after join (an idle chat never hits
                        // the quiesce tick, so the tick can't be the only
                        // trigger).
                        if host.is_host(&chat) {
                            let host = host.clone();
                            let weak = weak.clone();
                            host.clone().spawn_worker(async move {
                                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                                let Some(handle) = weak.upgrade() else { return };
                                let no_checkpoint = lock(&handle.chat2).as_ref().is_some_and(|c| {
                                    let stats = c.stats();
                                    // Pull-first construction is intentionally
                                    // local-first. Never bootstrap-heal from
                                    // placeholder zero stats before HTTP/WS
                                    // has supplied authoritative room state.
                                    stats.server_known && stats.checkpoint_size == 0
                                });
                                let has_content = handle
                                    .doc
                                    .read_entries()
                                    .map(|e| !e.is_empty())
                                    .unwrap_or(false);
                                if no_checkpoint && has_content {
                                    tracing::info!(chat = %handle.chat_id,
                                        "chat2 room has rows but no checkpoint; posting bootstrap checkpoint");
                                    host.spawn_chat2_checkpoint(&handle, "bootstrap");
                                }
                            });
                        }
                        // Host recovery duties (C3): a wiped room needs a
                        // seed checkpoint or fresh readers see only
                        // post-reset rows; rejected pushes reach peers only
                        // through a checkpoint. Watcher dies with the handle.
                        if host.is_host(&chat) {
                            let host = host.clone();
                            let weak = weak.clone();
                            let chat = chat.clone();
                            host.clone().spawn_worker(async move {
                                use cypher_sync::chat_client::ChatEvent;
                                loop {
                                    match events.recv().await {
                                        Ok(ChatEvent::ServerReset) => {
                                            let Some(handle) = weak.upgrade() else { return };
                                            tracing::warn!(chat = %chat, "chat2 room reset; posting seed checkpoint");
                                            host.spawn_chat2_checkpoint(&handle, "server-reset");
                                        }
                                        Ok(ChatEvent::PushRejected) => {
                                            let Some(handle) = weak.upgrade() else { return };
                                            tracing::warn!(chat = %chat, "chat2 push rejected; compensating via checkpoint");
                                            host.spawn_chat2_checkpoint(&handle, "push-rejected");
                                        }
                                        Ok(_) => {}
                                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                                    }
                                }
                            });
                        }
                        drop(handle);
                        if token_changes.is_none() {
                            return;
                        }
                        loop {
                            tokio::select! {
                                event = lifecycle_events.recv() => match event {
                                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                                },
                                _ = crate::workspace_host::token_changed(&mut token_changes) => {
                                    if edge.bearer().await.is_none() {
                                        if let Some(handle) = weak.upgrade() {
                                            lock(&handle.chat2).take();
                                            lock(&handle.chat2_local_sub).take();
                                        }
                                        tracing::info!(chat = %chat,
                                            "chat2 credentials removed; leaving room");
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(chat = %chat, error = %err,
                            backoff_ms = backoff.as_millis() as u64,
                            "chat2 join failed; retrying");
                    }
                    Err(_) => {
                        tracing::warn!(chat = %chat,
                            backoff_ms = backoff.as_millis() as u64,
                            "chat2 join timed out; retrying");
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(backoff + crate::workspace_host::join_retry_jitter()) => {
                        backoff = (backoff * 2).min(crate::workspace_host::JOIN_RETRY_CAP);
                    }
                    _ = wake.recv() => {
                        backoff = crate::workspace_host::JOIN_RETRY_BASE;
                    }
                    _ = crate::workspace_host::token_changed(&mut token_changes) => {
                        backoff = crate::workspace_host::JOIN_RETRY_BASE;
                    }
                }
            }
        });
    }

    /// chat2 host duties on the doc-quiesce tick (docs/design/chat2-sync.md C3):
    /// threshold checkpoint -- when the room's row log passes 512KB or 200
    /// rows, post a full checkpoint so cold readers load one compact blob
    /// instead of replaying the log (the alert-shaped growth bound).
    ///
    /// It used to publish a last-64 transcript "tail" sidecar here too, for an
    /// iOS fallback that native chat2 support made unnecessary. No client has
    /// ever read it -- no iOS build, no desktop build back to 0.3.18, and none
    /// in production (209 uploads, zero reads, in a 30-minute capture) -- yet
    /// it was 18% of the Durable Object bill. The Edge still serves the route.
    pub(super) async fn chat2_maintenance(&self, handle: &Arc<ChatDocHandle>) {
        let stats = match &*lock(&handle.chat2) {
            Some(client) => client.stats(),
            None => return,
        };
        if self.inner.config.edge.is_none() {
            return;
        }
        let chat_id = handle.chat_id.clone();
        // Only the workspace owner can publish chat sidecars/checkpoints;
        // non-host replicas would pay a guaranteed 403 and cannot change the
        // authoritative document anyway.
        if !self.is_host(&chat_id) {
            return;
        }
        // Threshold checkpoint (rowBytes > 512KB || rows > 200), one in
        // flight at a time.
        if stats.row_bytes <= 512 * 1024 && stats.row_count <= 200 {
            return;
        }
        self.spawn_chat2_checkpoint(handle, "threshold");
    }

    /// POST a full checkpoint for a chat2 room (one in flight per handle).
    /// Callers: the quiesce-tick threshold above, and the client recovery
    /// events (`ServerReset` — a wiped room needs a seed checkpoint or every
    /// fresh reader sees only post-reset rows; `PushRejected` — the rejected
    /// ops reach peers only through a checkpoint).
    pub(super) fn spawn_chat2_checkpoint(&self, handle: &Arc<ChatDocHandle>, reason: &'static str) {
        use base64::Engine as _;
        let Some(edge) = self.inner.config.edge.clone() else {
            return;
        };
        let stats = match &*lock(&handle.chat2) {
            Some(client) => client.stats(),
            None => return,
        };
        let chat_id = handle.chat_id.clone();
        if handle
            .checkpointing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let in_flight = handle.checkpointing.clone();
        let Ok(snapshot) = handle.doc.export_snapshot() else {
            in_flight.store(false, Ordering::Release);
            return;
        };
        let frontier = handle.doc.doc().oplog_vv().encode();
        let seq_covered = stats.cursor;
        let http = self.inner.http.clone();
        let weak_note = Arc::downgrade(handle);
        self.spawn_worker(async move {
            let Some(bearer) = edge.bearer().await else {
                in_flight.store(false, Ordering::Release);
                return;
            };
            let url = format!(
                "{}/chat2/{}/checkpoint?seqCovered={}",
                edge.url.trim_end_matches('/'),
                chat_id,
                seq_covered
            );
            let size = snapshot.len() as u64;
            match http
                .post(&url)
                .bearer_auth(&bearer)
                .header(
                    "x-chat2-frontier",
                    base64::engine::general_purpose::STANDARD.encode(&frontier),
                )
                .body(snapshot)
                .send()
                .await
            {
                Ok(res) if res.status().is_success() => {
                    tracing::info!(chat = %chat_id, seq_covered, reason, "chat2 checkpoint posted");
                    if let Some(handle) = weak_note.upgrade()
                        && let Some(client) = &*lock(&handle.chat2)
                    {
                        client.note_checkpoint(seq_covered, size);
                    }
                }
                Ok(res) => {
                    tracing::warn!(chat = %chat_id, status = res.status().as_u16(),
                        "chat2 checkpoint rejected");
                }
                Err(err) => {
                    tracing::warn!(chat = %chat_id, error = %err, "chat2 checkpoint POST failed");
                }
            }
            in_flight.store(false, Ordering::Release);
        });
    }
}
