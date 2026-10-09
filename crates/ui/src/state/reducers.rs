//! Pure reducers: how engine frames and local actions change [`AppState`].

use super::*;

impl AppState {
    pub fn apply_chats(&mut self, mut chats: Vec<Chat>) {
        sort_chats(&mut chats);
        self.chats = chats;
        self.chats_synced = true;
        self.chats_generation = self.chats_generation.wrapping_add(1);
        self.drop_vanished_chat();
    }

    /// Selected chat vanished (deleted elsewhere): drop selection +
    /// transcript. A chat with a send in flight is kept: a canvas's first
    /// send selects the client-minted id before the row's chats frame lands,
    /// and an unrelated frame in that gap must not drop it (the tile would
    /// close mid-send). The overlay is TTL-bounded, so a send that never
    /// creates the row still lets the selection go. Returns whether it
    /// dropped.
    pub(super) fn drop_vanished_chat(&mut self) -> bool {
        if let Some(selected) = &self.selected_chat
            && !self.chats.iter().any(|c| &c.id == selected)
            && !self.send_pending(selected, Utc::now())
        {
            self.selected_chat = None;
            self.transcript.clear();
            self.commands.clear();
            self.transcript_task = None;
            self.commands_task = None;
            self.bump_transcript();
            return true;
        }
        false
    }

    pub fn apply_sessions(&mut self, sessions: Vec<Session>) {
        self.sessions = sessions;
    }

    /// Project one `WatchSideChatStatus` frame into `sessions` (upsert by
    /// chat id). Temporary side chats never appear in the public
    /// `WatchSessions` stream; this is the ONLY status channel for a fork, and
    /// projecting it into `sessions` makes the reused Transcript/Composer
    /// status logic (`session_for`, `indicator_for`, `run_live`) work
    /// unchanged. `device_id` is the side chat's authoritative host device.
    pub fn apply_side_chat_status(&mut self, status: SideChatStatus, target_device_id: &str) {
        let session = Session {
            chat_id: status.side_chat_id,
            device_id: target_device_id.to_string(),
            status: status.status,
            started_at: status.started_at,
            updated_at: status.updated_at,
            subagents: Vec::new(),
            context_usage: None,
            throughput: None,
        };
        if let Some(existing) = self
            .sessions
            .iter_mut()
            .find(|s| s.chat_id == session.chat_id)
        {
            *existing = session;
        } else {
            self.sessions.push(session);
        }
    }

    /// Build a forked/secondary [`AppState`] for one temporary Side Chat: a
    /// synthetic selected `Chat` row inheriting the parent's
    /// device/space/cwd/branch/checkout/config, plus the targeted
    /// `WatchDocMessages` (transcript) and private `WatchSideChatStatus`
    /// watches — and nothing else. The main state's selection is untouched;
    /// the shared [`EngineHandle`] is cloned, never restarted. The EXISTING
    /// `Transcript` / `Composer` components (which read `selected_chat`,
    /// `transcript`, `pending_echoes` and `sessions`) work unchanged on it.
    ///
    /// No normal `WatchChats`/`WatchSessions`/`WatchSpaces`/`WatchDevices`
    /// watches run in the fork — they would replace the synthetic row/list
    /// state. The remote `targetDeviceId` stays authoritative on the two
    /// watches and on every side-chat RPC.
    ///
    /// The parent chat is read from `main`; when the parent row is missing
    /// (should not happen — the shell's StartSideChat race guard disposes
    /// late starts) the fork still exists but carries no synthetic row, so
    /// the panel renders a degraded empty transcript.
    pub fn new_side_chat_fork(
        main: &Entity<AppState>,
        parent_chat_id: &str,
        side_chat_id: &str,
        target_device_id: &str,
        cx: &mut App,
    ) -> Entity<AppState> {
        let (engine, local, workspace_scope, parent, parent_space, devices) = {
            let m = main.read(cx);
            let parent = m.chats.iter().find(|c| c.id == parent_chat_id).cloned();
            let parent_space = parent
                .as_ref()
                .and_then(|p| p.space_id.as_deref())
                .and_then(|space_id| m.spaces.iter().find(|s| s.id == space_id).cloned());
            (
                m.engine.clone(),
                m.local_device_id.clone(),
                m.workspace_scope,
                parent,
                parent_space,
                m.devices.clone(),
            )
        };
        let fork = cx.new(|_cx| {
            let mut s = AppState::new();
            s.engine = engine.clone();
            s.connection = ConnectionStatus::Ready;
            s.workspace_scope = workspace_scope;
            s.local_device_id = local;
            s.devices = devices;
            s.spaces = parent_space.into_iter().collect();
            if let Some(parent) = parent {
                let synthetic = side_chat_synthetic_row(&parent, side_chat_id, target_device_id);
                s.chats = vec![synthetic];
                s.selected_chat = Some(side_chat_id.to_string());
                s.selected_space = parent.space_id.clone();
                s.selected_device = Some(target_device_id.to_string());
                s.no_project = parent.space_id.is_none();
            }
            s
        });
        // The fork's standing (and only) watches: the targeted transcript
        // watch and the private status watch. No WatchChats/WatchSessions —
        // they would erase the synthetic state.
        fork.update(cx, |s, cx| {
            if let Some(engine) = engine {
                s.transcript_task = Some(spawn_fork_transcript_watch(
                    cx,
                    engine.clone(),
                    side_chat_id.to_string(),
                    target_device_id.to_string(),
                ));
                s.watch_tasks.push(spawn_side_chat_status_watch(
                    cx,
                    engine,
                    side_chat_id.to_string(),
                    target_device_id.to_string(),
                ));
            }
        });
        fork
    }

    /// Optimistic insert for a promoted Side Chat: the engine has
    /// already created the row (PromoteSideChat is synchronous engine-side),
    /// so this local copy makes the promotion seamless — the sidebar renders
    /// and the chat is selectable immediately, before the next chats frame
    /// replaces it with the authoritative row. Idempotent: a row that already
    /// arrived is left untouched.
    pub fn insert_chat_optimistic(&mut self, chat: Chat) {
        if self.chats.iter().any(|c| c.id == chat.id) {
            return;
        }
        self.chats.push(chat);
        sort_chats(&mut self.chats);
    }

    pub fn apply_spaces(&mut self, mut spaces: Vec<Space>) {
        sort_spaces(&mut spaces);
        self.spaces = spaces;
        self.spaces_synced = true;
        self.heal_space_selection();
    }

    pub(super) fn heal_space_selection(&mut self) {
        // Heal a vanished selection (project deleted elsewhere): fall back to
        // the first project; its chats died with it, so a matching chat
        // selection is healed by the accompanying chats frame (`apply_chats`).
        // The picker lists projects per-device, so healing prefers one on the
        // picked device — a global fallback would silently re-aim the canvas
        // at another machine.
        if let Some(selected) = &self.selected_space
            && !self.spaces.iter().any(|s| &s.id == selected)
        {
            self.selected_space = self.first_space_on_picked_device();
        }
        // First frame with no selection yet: pick the first project so the
        // canvas never boots project-less by accident — unless the user
        // deliberately opted out.
        if self.selected_space.is_none() && !self.no_project {
            self.selected_space = self.first_space_on_picked_device();
        }
    }

    /// Optimistic local echo of a `setChatConfig` mutate: stamp the row now so
    /// the chips update on click; the next chats watch frame carries the same
    /// value once the engine applies the LWW write.
    pub fn apply_chat_config(&mut self, chat_id: &str, config: cypher_proto::ChatConfig) {
        if let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) {
            chat.config = Some(config);
        }
    }

    /// [`Self::apply_chat_config`] from a view: a session context stamps its
    /// parent too, or the next mirror copy would revert the chips until the
    /// engine's chats frame lands. Must not run inside the parent's update.
    pub fn set_chat_config_optimistic(
        &mut self,
        chat_id: &str,
        config: cypher_proto::ChatConfig,
        cx: &mut Context<Self>,
    ) {
        if let Some(parent) = self.parent() {
            parent.update(cx, |parent, cx| {
                parent.set_chat_config_optimistic(chat_id, config.clone(), cx);
            });
        }
        self.apply_chat_config(chat_id, config);
        cx.notify();
    }

    pub fn apply_devices(&mut self, devices: Vec<Device>) {
        self.devices = devices;
    }

    /// First project on the composer's picked device (falling back through
    /// the local device, then any project at all — better a cross-device
    /// project than a surprise project-less canvas). Display order.
    ///
    /// Public: the deterministic live-space fallback for the new-session
    /// canvas (and the healing of a vanished selection).
    pub fn first_space_on_picked_device(&self) -> Option<String> {
        let device = self
            .selected_device
            .as_deref()
            .or(self.local_device_id.as_deref());
        let sorted = self.spaces_sorted();
        device
            .and_then(|d| sorted.iter().find(|s| s.device_id == d).copied())
            .or_else(|| sorted.first().copied())
            .map(|s| s.id.clone())
    }

    pub fn apply_update(&mut self, status: cypher_update::UpdateStatus) {
        self.update = Some(status);
    }

    pub fn apply_pi_update(&mut self, status: cypher_engine::pi_packages::PiUpdateStatus) {
        self.pi_update = Some(status);
    }

    pub fn apply_auth(&mut self, auth: AuthState) {
        self.auth = Some(auth);
    }

    /// Tolerant AuthStatus frame reducer (see [`parse_auth_state`]).
    pub fn apply_auth_value(&mut self, value: serde_json::Value) {
        match parse_auth_state(&value) {
            Some(auth) => self.apply_auth(auth),
            None => tracing::warn!("dropping unrecognized AuthStatus frame"),
        }
    }

    /// The signed-in user, if the engine reports one.
    pub fn auth_user(&self) -> Option<&cypher_proto::UserProfile> {
        match self.auth.as_ref()? {
            AuthState::SignedIn { user, .. } | AuthState::NeedsOrganization { user } => Some(user),
            AuthState::SignedOut => None,
        }
    }

    #[cfg(test)]
    pub fn apply_transcript(&mut self, entries: Vec<SessionMessageEntry>) {
        // Doc frames supersede optimistic echoes carrying the same id.
        if let Some(chat_id) = self.selected_chat.as_deref()
            && let Some(echoes) = self.echoes.get_mut(chat_id)
        {
            echoes.retain(|echo| !entries.iter().any(|e| e.id == echo.id));
        }
        self.transcript = entries;
        self.bump_transcript();
        self.ack_pending_send_from_transcript();
    }

    /// Apply a `WatchDocMessages` delta frame in place. `Err` = this copy has
    /// diverged; the watch task resubscribes for a fresh reset.
    pub fn apply_transcript_frame(
        &mut self,
        frame: TranscriptFrame,
    ) -> Result<(), TranscriptDesync> {
        // Bumped even on a desync: the partially applied copy renders.
        self.bump_transcript();
        cypher_doc::apply_transcript_frame(&mut self.transcript, frame)?;
        if let Some(chat_id) = self.selected_chat.as_deref()
            && let Some(echoes) = self.echoes.get_mut(chat_id)
        {
            let transcript = &self.transcript;
            echoes.retain(|echo| !transcript.iter().any(|e| e.id == echo.id));
        }
        self.ack_pending_send_from_transcript();
        Ok(())
    }

    /// Add an optimistic user echo (composer send path).
    pub fn push_echo(&mut self, chat_id: &str, entry: SessionMessageEntry) {
        let echoes = self.echoes.entry(chat_id.to_string()).or_default();
        if !echoes.iter().any(|e| e.id == entry.id) {
            echoes.push(entry);
        }
        self.bump_transcript();
    }

    /// Mark a message id as sent via Steer (see [`Self::steer_message_ids`]).
    pub fn mark_steer(&mut self, message_id: &str) {
        self.local_steers.insert(message_id.to_string());
        self.bump_transcript();
    }

    /// User messages of the selected chat that were steers: the explicit
    /// message ids on the ledger's Steer commands (the iOS join), plus this
    /// device's own not-yet-synced steers. Old messages without a matching
    /// id stay plain prompts.
    pub fn steer_message_ids(&self) -> HashSet<String> {
        self.commands
            .iter()
            .filter_map(|c| match &c.payload {
                SessionCommandPayload::Steer {
                    message_id: Some(id),
                    ..
                } if !id.is_empty() => Some(id.clone()),
                _ => None,
            })
            .chain(self.local_steers.iter().cloned())
            .collect()
    }

    /// Drop an echo (send failed — the prompt returns to the draft).
    pub fn remove_echo(&mut self, chat_id: &str, message_id: &str) {
        if let Some(echoes) = self.echoes.get_mut(chat_id) {
            echoes.retain(|e| e.id != message_id);
        }
        self.bump_transcript();
    }

    /// Composer send fired: overlay the chat as Working until the host writes
    /// the user message back into the transcript (or the TTL lapses). A remote
    /// send has no live session row until the host drains the queued command —
    /// that gap read as "no live run" and flashed the Completed dot, and any
    /// phantom Working→Idle edge in it rang the done-chime on send (user
    /// report 2026-08-05).
    pub fn begin_pending_send(&mut self, chat_id: &str, message_id: &str, now: DateTime<Utc>) {
        self.pending_sends.borrow_mut().insert(
            chat_id.to_string(),
            PendingSend {
                message_id: message_id.to_string(),
                started: now,
            },
        );
    }

    /// Send failed — drop the overlay so the dot tells the truth again. Only
    /// removes the overlay this message started: a quick resend must not lose
    /// its own overlay to the first send's failure cleanup.
    pub fn end_pending_send(&mut self, chat_id: &str, message_id: &str) {
        let mut pending = self.pending_sends.borrow_mut();
        if pending
            .get(chat_id)
            .is_some_and(|p| p.message_id == message_id)
        {
            pending.remove(chat_id);
        }
    }

    /// Is a send still in flight for this chat (unacked, inside the TTL)?
    pub fn send_pending(&self, chat_id: &str, now: DateTime<Utc>) -> bool {
        self.pending_sends.borrow().get(chat_id).is_some_and(|p| {
            now.signed_duration_since(p.started).num_milliseconds() <= PENDING_SEND_TTL_MS
        })
    }

    /// When the in-flight send (if any, inside the TTL) was fired — the
    /// elapsed-timer base while the overlay reads as Working. The session
    /// row's `started_at` still belongs to the PREVIOUS turn during this
    /// window, and showing it made a fresh send open at the old turn's
    /// half-hour mark.
    pub fn pending_send_started(&self, chat_id: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.pending_sends
            .borrow()
            .get(chat_id)
            .filter(|p| {
                now.signed_duration_since(p.started).num_milliseconds() <= PENDING_SEND_TTL_MS
            })
            .map(|p| p.started)
    }

    /// The host executed the queued command iff the sent message's id showed
    /// up in the transcript (it writes the message before — causally with —
    /// the Working status; sessions.rs dispatch paths).
    fn ack_pending_send_from_transcript(&mut self) {
        let Some(chat_id) = self.selected_chat.as_deref() else {
            return;
        };
        let mut pending = self.pending_sends.borrow_mut();
        if pending
            .get(chat_id)
            .is_some_and(|p| self.transcript.iter().any(|e| e.id == p.message_id))
        {
            pending.remove(chat_id);
        }
    }

    /// Unconfirmed echoes for the selected chat, in send order.
    pub fn pending_echoes(&self) -> &[SessionMessageEntry] {
        self.selected_chat
            .as_deref()
            .and_then(|id| self.echoes.get(id))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Fold one `WatchDocCommands` frame (the current ledger) into state.
    /// The ledger is the durable truth: a Rejected/Expired command ends the
    /// optimistic send-in-flight overlay so the sidebar dot stops reading
    /// "Working" for a message the host refused, and the composer's failed
    /// row + Retry take over.
    pub fn apply_commands(&mut self, commands: Vec<SessionCommandEntry>) {
        self.commands = commands;
        self.bump_transcript();
        let Some(chat_id) = self.selected_chat.as_deref() else {
            return;
        };
        let mut pending = self.pending_sends.borrow_mut();
        if pending.get(chat_id).is_some_and(|p| {
            command_send_status(&self.commands, &p.message_id) == Some(CommandSendStatus::Failed)
        }) {
            pending.remove(chat_id);
        }
    }

    /// Projected status of the selected chat's message command, or `None`
    /// when no Run/Steer command exists for it.
    pub fn command_status_for(&self, message_id: &str) -> Option<CommandSendStatus> {
        command_send_status(&self.commands, message_id)
    }

    /// The retry-able failures of the selected chat's ledger (composer row).
    pub fn failed_commands(&self) -> Vec<FailedCommand> {
        failed_commands(&self.commands)
    }

    /// Whether an optimistic echo is still genuinely in flight. A message
    /// whose latest attempt FAILED renders at full opacity (the failed row
    /// explains it) instead of the 0.65 sending veil forever.
    pub fn echo_pending(&self, message_id: &str) -> bool {
        self.command_status_for(message_id) != Some(CommandSendStatus::Failed)
    }

    pub fn begin_upload_progress(
        &mut self,
        chat_id: &str,
        total_bytes: u64,
        completed_bytes: Arc<std::sync::atomic::AtomicU64>,
    ) {
        self.upload_progress = Some(UploadProgress {
            chat_id: chat_id.to_string(),
            total_bytes: total_bytes.max(1),
            completed_bytes,
        });
    }

    /// Retire the upload trailer when `chat_id`'s send leaves the streaming
    /// stage — success or failure. Scoped by chat so a finishing send never
    /// cancels a LATER upload that has already claimed the slot.
    pub fn end_upload_progress(&mut self, chat_id: &str) {
        if self
            .upload_progress
            .as_ref()
            .is_some_and(|p| p.chat_id == chat_id)
        {
            self.upload_progress = None;
        }
    }

    /// Percent uploaded for `chat_id`'s in-flight send, or `None` when this
    /// chat has no upload streaming right now.
    pub fn upload_progress_percent(&self, chat_id: &str) -> Option<u8> {
        let progress = self.upload_progress.as_ref()?;
        if progress.chat_id != chat_id {
            return None;
        }
        let completed = progress
            .completed_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
            .min(progress.total_bytes);
        Some(((completed.saturating_mul(100)) / progress.total_bytes) as u8)
    }
}
