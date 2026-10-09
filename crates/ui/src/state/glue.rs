//! gpui glue: the entity's engine-facing actions and the watch pumps that
//! feed it.

use super::*;

impl AppState {
    /// Kick off (or retry) the engine bootstrap: probe → connect-or-embed on
    /// tokio, then attach subscriptions. Safe to call again after `Failed`.
    pub fn bootstrap(
        state: Entity<AppState>,
        data_dir: PathBuf,
        config: EngineBootConfig,
        cx: &mut App,
    ) {
        state.update(cx, |s, cx| {
            s.connection = ConnectionStatus::Connecting;
            s.workspace_scope = None;
            s.auth = None;
            s.data_dir = Some(data_dir);
            cx.notify();
        });
        let boot = Tokio::spawn(cx, EngineHandle::bootstrap(config));
        cx.spawn(async move |cx| {
            let outcome = match boot.await {
                Ok(Ok(handle)) => Ok(handle),
                Ok(Err(err)) => Err(format!("{err:#}")),
                Err(join_err) => Err(join_err.to_string()),
            };
            // NB: at the pinned rev `Entity::update(&mut AsyncApp)` returns the
            // closure's value directly (no Result) — AsyncApp implements
            // AppContext like App does.
            state.update(cx, |s, cx| match outcome {
                Ok(handle) => s.attach_engine(handle, true, cx),
                Err(message) => {
                    tracing::error!(%message, "engine bootstrap failed");
                    s.connection = ConnectionStatus::Failed(message);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Wire the connected engine: mark Ready and start the standing watches.
    /// Methods the engine doesn't serve fail their subscribe and are skipped
    /// gracefully.
    /// `owner`: this state bootstrapped the handle, so it also watches the
    /// deferred engine assembly (and shuts the handle down on failure). A
    /// project window's state only borrows the main window's handle.
    pub(super) fn attach_engine(
        &mut self,
        handle: EngineHandle,
        owner: bool,
        cx: &mut Context<Self>,
    ) {
        let engine_info = handle.engine_info();
        self.workspace_scope = Some(engine_info.workspace_scope);
        self.local_device_id = Some(engine_info.device_id.clone());
        self.engine = Some(handle.clone());
        let mut watch_tasks = Vec::with_capacity(8);
        if owner && let Some(task) = spawn_deferred_engine_watch(cx, handle.clone()) {
            watch_tasks.push(task);
        }
        watch_tasks.extend([
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_SESSIONS,
                AppState::apply_sessions,
            ),
            spawn_chats_watch(cx, handle.clone()),
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_DEVICES,
                AppState::apply_devices,
            ),
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_SPACES,
                AppState::apply_spaces,
            ),
            // Auth frames parse tolerantly — engine and proto tags differ today.
            spawn_watch(
                cx,
                handle.clone(),
                methods::AUTH_STATUS,
                AppState::apply_auth_value,
            ),
            spawn_update_watch(cx, handle.clone()),
            spawn_pi_update_watch(cx, handle.clone()),
            spawn_local_device_probe(cx, handle.clone()),
        ]);
        self.watch_tasks = watch_tasks;
        // EngineInfo is part of the attachment boundary: views must know which
        // data profile they reached before they are allowed to render Ready.
        self.connection = ConnectionStatus::Ready;
        // Re-subscribe the transcript if a chat was already selected (reconnect path).
        self.spawn_transcript_watches(cx);
        cx.notify();
    }

    /// Subscribe the selected chat's transcript + ledger (when this state
    /// owns transcripts and has an engine). Callers drop the old tasks.
    pub(super) fn spawn_transcript_watches(&mut self, cx: &mut Context<Self>) {
        if !self.transcript_watches {
            return;
        }
        if let (Some(chat_id), Some(handle)) = (self.selected_chat.clone(), self.engine.clone()) {
            self.transcript_task =
                Some(spawn_transcript_watch(cx, handle.clone(), chat_id.clone()));
            self.commands_task = Some(spawn_commands_watch(cx, handle, chat_id));
        }
    }

    /// Lists-only mode (`false`): the selection keeps driving the sidebar,
    /// space and seen marks, but this state subscribes no transcript — the
    /// session tiles' contexts own them. Switching drops or re-subscribes
    /// the current selection's watches.
    pub fn set_transcript_watches(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.transcript_watches == on {
            return;
        }
        self.transcript_watches = on;
        self.transcript.clear();
        self.commands.clear();
        self.bump_transcript();
        self.transcript_task = None;
        self.commands_task = None;
        self.spawn_transcript_watches(cx);
        cx.notify();
    }

    #[cfg(test)]
    pub fn transcript_watches(&self) -> bool {
        self.transcript_watches
    }

    /// Select a chat (or clear). Swaps the per-chat doc-transcript subscription:
    /// dropping the old task drops its stream receiver, which cancels the doc
    /// watch server-side. Selecting a chat also lands in its space and marks it
    /// seen (a global-list click must switch the tab strip too).
    pub fn select_chat(&mut self, chat_id: Option<String>, cx: &mut Context<Self>) {
        if self.selected_chat == chat_id {
            // Re-selecting still clears a fresh "completed" badge.
            if let Some(id) = chat_id {
                self.mark_chat_seen(&id, cx);
            }
            return;
        }
        self.selected_chat = chat_id.clone();
        self.auto_selected = true;
        self.transcript.clear();
        self.commands.clear();
        self.bump_transcript();
        self.transcript_task = None;
        self.commands_task = None;
        if let Some(id) = chat_id.as_deref() {
            // A chat implies its project (or the lack of one); `select_chat(None)`
            // (the new-session canvas) keeps the current project pick.
            self.scratch_pending = false;
            if let Some(chat) = self.chats.iter().find(|c| c.id == id) {
                match chat.space_id.clone() {
                    Some(space_id) => {
                        self.selected_space = Some(space_id);
                        self.no_project = false;
                    }
                    None => {
                        self.no_project = true;
                        self.selected_device = Some(chat.device_id.clone());
                    }
                }
            }
            self.mark_chat_seen(id, cx);
        }
        self.spawn_transcript_watches(cx);
        cx.notify();
    }

    /// Select a project; the caller (shell) decides which chat to land on.
    /// `Some` clears a "Don't work in a project" opt-out and re-aims the
    /// device pick at the project's host; `None` IS that opt-out.
    pub fn select_space(&mut self, space_id: Option<String>, cx: &mut Context<Self>) {
        match &space_id {
            Some(id) => {
                self.no_project = false;
                self.scratch_pending = false;
                if let Some(device) = self.space_row(id).map(|s| s.device_id.clone()) {
                    self.selected_device = Some(device);
                }
            }
            None => self.no_project = true,
        }
        if self.selected_space == space_id && space_id.is_some() {
            cx.notify();
            return;
        }
        if space_id.is_some() {
            self.selected_space = space_id;
        }
        cx.notify();
    }

    /// Window-focus liveness sweep: ask the engine to probe every open room
    /// (workspace + chat docs). Fire-and-forget; each room ignores the hint
    /// unless it has been broadcast-quiet ≥30s, so spamming is harmless.
    pub fn probe_sync(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.engine.clone() else {
            return;
        };
        cx.spawn(async move |_, _| {
            let params = serde_json::json!({});
            if let Err(err) = handle.client().call(methods::PROBE_SYNC, params).await {
                tracing::debug!(error = %err, "probe sync failed");
            }
        })
        .detach();
    }

    pub fn report_notification_activity(
        &self,
        mut activity: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(handle) = self.engine.clone() else {
            return;
        };
        let Some(AuthState::SignedIn {
            user,
            org_id: Some(org),
        }) = &self.auth
        else {
            return;
        };
        activity["expectedUserId"] = serde_json::json!(user.id);
        activity["expectedOrgId"] = serde_json::json!(org);
        cx.spawn(async move |_, _| {
            // Old engines/disabled notification services must not affect the UI.
            let _ = handle
                .client()
                .call(methods::NOTIFICATION_ACTIVITY, activity)
                .await;
        })
        .detach();
    }

    /// Synced seen marker: only fires when the chat is currently unseen
    /// (idempotence — no mutate spam), stamps the local row optimistically so
    /// the LWW round-trip is invisible, and fire-and-forgets the mutate.
    ///
    /// A session context stamps its own row and forwards to its parent,
    /// which owns the mutate — one RPC, and the sidebar badge clears at
    /// once instead of after the next mirror copy. Must not run inside the
    /// parent's update.
    pub fn mark_chat_seen(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let stamped = match self.chats.iter_mut().find(|c| c.id == chat_id) {
            Some(chat) if chat.unseen() => {
                chat.last_seen_at = Some(Utc::now());
                cx.notify();
                true
            }
            _ => false,
        };
        if let Some(parent) = self.parent() {
            parent.update(cx, |parent, cx| parent.mark_chat_seen(chat_id, cx));
            return;
        }
        if !stamped {
            return;
        }
        let Some(handle) = self.engine.clone() else {
            return;
        };
        let chat_id = chat_id.to_string();
        cx.spawn(async move |_, _| {
            let params = serde_json::json!({ "op": "markChatSeen", "chatId": chat_id });
            if let Err(err) = handle.client().call(methods::MUTATE, params).await {
                tracing::warn!(chat = %chat_id, error = %err, "markChatSeen failed");
            }
        })
        .detach();
    }
}

/// Observe assembly after an early attach (cloud onboarding or another viewport
/// reaching the embedded engine over IPC). Data subscriptions wait on the same
/// result, but their individual errors are not authoritative: older engines may
/// legitimately omit a watch method. Only the assembly result may fail the
/// whole connection.
fn spawn_deferred_engine_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
) -> Option<Task<()>> {
    let mut deferred = handle.deferred_state()?;
    Some(cx.spawn(async move |this, cx| {
        let Err(failure) = wait_for_deferred_engine(&mut deferred).await else {
            return;
        };
        tracing::error!(error = %failure, "engine assembly failed after attachment");
        // Embedded handles release their IPC listener before exposing Retry;
        // remote handles stop their completed readiness probe.
        handle.shutdown().await;
        this.update(cx, |state, cx| {
            state.connection = ConnectionStatus::Failed(failure);
            cx.notify();
        })
        .ok();
    }))
}

/// Chats watch. Boot selection is the shell's job (it lands on the first
/// restored open tab, device-local state this entity can't see); this task
/// only pumps frames.
fn spawn_chats_watch(cx: &mut Context<AppState>, handle: EngineHandle) -> Task<()> {
    cx.spawn(async move |this, cx| {
        // Resubscribe loop (same contract as the transcript watch): a daemon
        // restart or RPC drop ends the stream, and a bare return here froze
        // the sidebar until app restart — new chats, renames and archives
        // from every device silently stopped arriving.
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        loop {
            let mut rx = match handle
                .client()
                .subscribe(methods::WATCH_CHATS, serde_json::json!({}))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::debug!(error = %err, "chats watch unavailable; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue;
                }
            };
            while let Some(value) = rx.recv().await {
                let parsed: Vec<Chat> = match serde_json::from_value(value) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        tracing::warn!(error = %err, "dropping malformed chats frame");
                        continue;
                    }
                };
                let alive = this.update(cx, |state, cx| {
                    state.apply_chats(parsed);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            tracing::debug!("chats stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

fn spawn_watch<T: DeserializeOwned + 'static>(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    method: &'static str,
    apply: fn(&mut AppState, T),
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        // Resubscribe loop: these are the standing Sessions/Devices/Spaces
        // watches — a daemon restart ended the stream and a bare return froze
        // them for the rest of the app's life (remote Working dots staled out
        // to nothing after 45s, and Idle/Completed transitions from other
        // devices never arrived again — "the session never completes").
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        loop {
            let mut rx = match handle
                .client()
                .subscribe(method, serde_json::json!({}))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::debug!(method, error = %err, "watch unavailable; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue;
                }
            };
            while let Some(value) = rx.recv().await {
                let parsed: T = match serde_json::from_value(value) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        tracing::warn!(method, error = %err, "dropping malformed watch frame");
                        continue;
                    }
                };
                let alive = this.update(cx, |state, cx| {
                    apply(state, parsed);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            tracing::debug!(method, "watch stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

/// Capped exponential backoff for the UpdateStatus watch: 2, 4, 8, 16, then 30s
/// forever. The other standing watches retry at a flat 2s; the update strip is
/// advisory and must not churn the IPC + log every 2s while the stream is
/// unavailable or closes prematurely (the 0.1.0 local-only regression). A
/// stream that delivered a valid frame resets the step, so a healthy engine
/// restart is picked up quickly.
const UPDATE_BACKOFF_SECS: [u64; 5] = [2, 4, 8, 16, 30];

/// Delay for backoff `step` (0-based), capped at the final entry.
pub(super) fn update_backoff_delay(step: usize) -> std::time::Duration {
    std::time::Duration::from_secs(UPDATE_BACKOFF_SECS[step.min(UPDATE_BACKOFF_SECS.len() - 1)])
}

/// UpdateStatus watch. Unlike the other standing watches, the update strip is
/// advisory: a missing or prematurely closed stream must never surface a
/// user-facing error or churn the IPC every 2s forever. On 0.1.0 local-only
/// runtimes had no updater, so the generic watch's flat-2s resubscribe loop
/// spun forever; this one backs off (capped exponential) and keeps the last
/// valid frame on screen while it is unavailable.
fn spawn_update_watch(cx: &mut Context<AppState>, handle: EngineHandle) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let mut backoff_step = 0usize;
        loop {
            let mut rx = match handle
                .client()
                .subscribe(methods::UPDATE_STATUS, serde_json::json!({}))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::debug!(error = %err, "update status unavailable; retrying");
                    let delay = update_backoff_delay(backoff_step);
                    if backoff_step < UPDATE_BACKOFF_SECS.len() - 1 {
                        backoff_step += 1;
                    }
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(delay).await;
                    continue;
                }
            };
            let mut frames = 0usize;
            while let Some(value) = rx.recv().await {
                let parsed: cypher_update::UpdateStatus = match serde_json::from_value(value) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        tracing::warn!(error = %err, "dropping malformed update frame");
                        continue;
                    }
                };
                let alive = this.update(cx, |state, cx| {
                    state.apply_update(parsed);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
                frames += 1;
            }
            // Stream ended (engine restart, RPC drop). A stream that delivered a
            // valid frame resets the backoff; one that closed prematurely keeps
            // backing off so a broken runtime cannot churn every 2s.
            tracing::debug!("update status stream ended; retrying");
            if frames > 0 {
                backoff_step = 0;
            }
            let delay = update_backoff_delay(backoff_step);
            if backoff_step < UPDATE_BACKOFF_SECS.len() - 1 {
                backoff_step += 1;
            }
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(delay).await;
        }
    })
}

/// Pi/package update watch: same advisory backoff discipline as the Cypher
/// release stream. The last valid frame remains visible across engine
/// reconnects so an available update never flickers away.
fn spawn_pi_update_watch(cx: &mut Context<AppState>, handle: EngineHandle) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let mut backoff_step = 0usize;
        loop {
            let mut rx = match handle
                .client()
                .subscribe(methods::PI_UPDATE_STATUS, serde_json::json!({}))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::debug!(error = %err, "Pi update status unavailable; retrying");
                    let delay = update_backoff_delay(backoff_step);
                    if backoff_step < UPDATE_BACKOFF_SECS.len() - 1 {
                        backoff_step += 1;
                    }
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(delay).await;
                    continue;
                }
            };
            let mut frames = 0usize;
            while let Some(value) = rx.recv().await {
                let parsed: cypher_engine::pi_packages::PiUpdateStatus =
                    match serde_json::from_value(value) {
                        Ok(parsed) => parsed,
                        Err(err) => {
                            tracing::warn!(error = %err, "dropping malformed Pi update frame");
                            continue;
                        }
                    };
                let alive = this.update(cx, |state, cx| {
                    state.apply_pi_update(parsed);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
                frames += 1;
            }
            tracing::debug!("Pi update status stream ended; retrying");
            if frames > 0 {
                backoff_step = 0;
            }
            let delay = update_backoff_delay(backoff_step);
            if backoff_step < UPDATE_BACKOFF_SECS.len() - 1 {
                backoff_step += 1;
            }
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(delay).await;
        }
    })
}

/// Best-effort `LocalDevice` probe: fills `local_device_id` for the "This
/// device" badge. Engines that don't serve the method leave it `None`.
fn spawn_local_device_probe(cx: &mut Context<AppState>, handle: EngineHandle) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let Ok(value) = handle
            .client()
            .call("LocalDevice", serde_json::json!({}))
            .await
        else {
            tracing::debug!("LocalDevice unavailable; skipping this-device badge");
            return;
        };
        let id = value
            .get("id")
            .or_else(|| value.get("deviceId"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(id) = id {
            this.update(cx, |state, cx| {
                state.local_device_id = Some(id);
                cx.notify();
            })
            .ok();
        }
    })
}

fn spawn_transcript_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    chat_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        // Outer loop: a delta desync (missed frame) resubscribes immediately
        // and the fresh stream's opening reset heals the copy; a subscribe
        // failure, malformed frame, or stream end retries on a delay. Every
        // path re-enters the loop — a return here freezes the transcript
        // with no banner and no heal short of an app restart (this watch and
        // its engine-side room are the ONLY transcript delivery path). The
        // task itself is dropped by select_chat/apply_chats when the chat is
        // deselected or deleted, so retrying can't outlive relevance.
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        'resubscribe: loop {
            let params = serde_json::json!({ "chatId": chat_id });
            let mut rx = match handle
                .client()
                .subscribe(methods::WATCH_DOC_MESSAGES, params)
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::warn!(%chat_id, error = %err, "transcript watch failed; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue 'resubscribe;
                }
            };
            while let Some(value) = rx.recv().await {
                let frame: TranscriptFrame = match serde_json::from_value(value) {
                    Ok(frame) => frame,
                    Err(err) => {
                        // Schema skew (a newer peer's entry shape arriving
                        // through sync): a skipped frame is a silently stale
                        // copy, so resubscribe for a fresh reset — delayed,
                        // in case the reset itself is what can't parse.
                        tracing::warn!(error = %err, "malformed transcript frame; resubscribing");
                        cx.background_executor().timer(RETRY_DELAY).await;
                        continue 'resubscribe;
                    }
                };
                let mut desync = false;
                let alive = this.update(cx, |state, cx| {
                    // Guard against a stale pump racing a newer selection.
                    if state.selected_chat.as_deref() == Some(chat_id.as_str()) {
                        if let Err(err) = state.apply_transcript_frame(frame) {
                            tracing::warn!(%chat_id, error = %err, "resubscribing transcript");
                            desync = true;
                        }
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
                if desync {
                    continue 'resubscribe;
                }
            }
            // Stream ended: engine restart, RPC drop, or chat purge. Retry;
            // the purge case is cleaned up by apply_chats dropping this task.
            tracing::debug!(%chat_id, "transcript stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

/// `WatchDocCommands`: the selected chat's durable command ledger — current
/// value first, then re-sent on every doc change. The UI projects Queued /
/// Retrying / Failed from this, so a Rejected or Expired command is visible
/// even when no session row ever reflects it (the host writes nothing to the
/// transcript for a refused message). Same resubscribe discipline as
/// [`spawn_transcript_watch`]; dropped by `select_chat`/`apply_chats` with
/// the chat.
fn spawn_commands_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    chat_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        'resubscribe: loop {
            let params = serde_json::json!({ "chatId": chat_id });
            let mut rx = match handle
                .client()
                .subscribe(methods::WATCH_DOC_COMMANDS, params)
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::warn!(%chat_id, error = %err, "commands watch failed; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue 'resubscribe;
                }
            };
            while let Some(value) = rx.recv().await {
                let commands: Vec<SessionCommandEntry> = match serde_json::from_value(value) {
                    Ok(commands) => commands,
                    Err(err) => {
                        // Schema skew — resubscribe for a fresh frame.
                        tracing::warn!(error = %err, "malformed commands frame; resubscribing");
                        cx.background_executor().timer(RETRY_DELAY).await;
                        continue 'resubscribe;
                    }
                };
                let alive = this.update(cx, |state, cx| {
                    // Guard against a stale pump racing a newer selection.
                    if state.selected_chat.as_deref() == Some(chat_id.as_str()) {
                        state.apply_commands(commands);
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
            // Stream ended: engine restart, RPC drop, or chat purge. Retry;
            // the purge case is cleaned up by apply_chats dropping this task.
            tracing::debug!(%chat_id, "commands stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

/// `WatchDocMessages` for a Side Chat fork: identical to [`spawn_transcript_watch`]
/// but carries `targetDeviceId` (the side chat is owned by the parent's host
/// device, which may differ from the connected engine's), and the fork's
/// selection never changes so the guard is trivially true. The task dies with
/// the fork (panel close drops the entity).
pub(super) fn spawn_fork_transcript_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    chat_id: String,
    target_device_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        'resubscribe: loop {
            let mut params = serde_json::Map::new();
            params.insert("chatId".into(), serde_json::Value::String(chat_id.clone()));
            if let Some(local) = this.update(cx, |s, _| s.local_device_id.clone()).ok().flatten()
                && target_device_id != local
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target_device_id.clone()),
                );
            }
            let mut rx = match handle
                .client()
                .subscribe(methods::WATCH_DOC_MESSAGES, serde_json::Value::Object(params))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::warn!(%chat_id, error = %err, "side chat transcript watch failed; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue 'resubscribe;
                }
            };
            while let Some(value) = rx.recv().await {
                let frame: TranscriptFrame = match serde_json::from_value(value) {
                    Ok(frame) => frame,
                    Err(err) => {
                        tracing::warn!(error = %err, "malformed side chat transcript frame; resubscribing");
                        cx.background_executor().timer(RETRY_DELAY).await;
                        continue 'resubscribe;
                    }
                };
                let mut desync = false;
                let alive = this.update(cx, |s, cx| {
                    if let Err(err) = s.apply_transcript_frame(frame) {
                        tracing::warn!(%chat_id, error = %err, "side chat transcript desync");
                        desync = true;
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
                if desync {
                    continue 'resubscribe;
                }
            }
            tracing::debug!(%chat_id, "side chat transcript stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

/// `WatchSideChatStatus`: the private per-chat status stream, projected into
/// the fork's `sessions` via [`AppState::apply_side_chat_status`] (`null`
/// until the first transition or after dispose). The stream ends at
/// promotion/dispose; retrying after a clean end would hang, so a closed
/// stream simply stops the fork's status updates (the panel is usually gone
/// by then anyway).
pub(super) fn spawn_side_chat_status_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    side_chat_id: String,
    target_device_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        loop {
            let mut params = serde_json::Map::new();
            params.insert(
                "sideChatId".into(),
                serde_json::Value::String(side_chat_id.clone()),
            );
            if let Some(local) = this.update(cx, |s, _| s.local_device_id.clone()).ok().flatten()
                && target_device_id != local
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target_device_id.clone()),
                );
            }
            let mut rx = match handle
                .client()
                .subscribe(
                    methods::WATCH_SIDE_CHAT_STATUS,
                    serde_json::Value::Object(params),
                )
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::warn!(%side_chat_id, error = %err, "side chat status watch failed; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue;
                }
            };
            while let Some(value) = rx.recv().await {
                if value.is_null() {
                    // First frame / after dispose: no session yet.
                    if this
                        .update(cx, |s, cx| {
                            s.sessions.retain(|s| s.chat_id != side_chat_id);
                            cx.notify();
                        })
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                let status: SideChatStatus = match serde_json::from_value(value) {
                    Ok(status) => status,
                    Err(err) => {
                        tracing::warn!(error = %err, "malformed side chat status frame");
                        continue;
                    }
                };
                let alive = this.update(cx, |s, cx| {
                    s.apply_side_chat_status(status.clone(), &target_device_id);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            // Stream ended: after dispose or promotion the panel is normally
            // already gone; if it somehow outlived the chat, retry.
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

/// The synthetic `Chat` row a Side Chat fork selects (pure — testable without
/// a panel): the side chat inherits the parent's working context
/// (device/space/cwd/branch/checkout/config) so the reused Transcript/Composer
/// read the right values, but is its OWN row (the engine holds the real temp
/// in memory; there is no workspace row until promotion).
pub(super) fn side_chat_synthetic_row(
    parent: &Chat,
    side_chat_id: &str,
    target_device_id: &str,
) -> Chat {
    Chat {
        pinned: false,
        id: side_chat_id.to_string(),
        device_id: target_device_id.to_string(),
        title: None,
        archived: false,
        cwd: parent.cwd.clone(),
        branch: parent.branch.clone(),
        checkout_id: parent.checkout_id.clone(),
        config: parent.config.clone(),
        last_message_preview: None,
        last_message_at: None,
        created_at: chrono::Utc::now(),
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: parent.space_id.clone(),
        last_seen_at: None,
        room_gen: Some(2),
        child: None,
    }
}
