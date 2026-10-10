//! The boot splash.

use super::*;

impl Shell {
    /// Mirror [`AppState::attention_count`] onto the Dock icon (zero when the
    /// setting is off). Written only on change — this runs on every state
    /// notify.
    pub(super) fn sync_dock_badge(&mut self, cx: &mut Context<Self>) {
        // One Dock icon: the main window owns it (the count is app-wide).
        if self.is_project_window() {
            return;
        }
        let count = if self.settings.dock_badge_enabled {
            self.state.read(cx).attention_count(Utc::now())
        } else {
            0
        };
        if self.attention.dock_badge != Some(count) {
            self.attention.dock_badge = Some(count);
            tracing::debug!(count, "dock badge");
            crate::shell::notify::set_badge(count);
        }
    }

    pub(super) fn on_state_changed(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        self.sync_window_scope(cx);
        // App-wide flows (relaunch, runtime switches, capture knobs) are the
        // main window's; a project window only renders its project.
        let main_window = !self.is_project_window();
        if main_window && self.quit_for_relaunch(state, cx) {
            return;
        }
        self.follow_sync_lifecycle(state, main_window, cx);
        self.open_capture_dialogs(state, cx);
        self.ring_session_chimes(state, cx);
        self.sync_dock_badge(cx);
        self.restore_boot_space(state, cx);
        if main_window {
            self.persist_selected_space(state, cx);
        }
        // Boot landing: the most recent session once the first chats frame
        // syncs (manual selection wins).
        self.boot_select_chat(cx);
        // Deleted / archived sessions, and sessions whose project moved to
        // (or out of) this window's scope, leave the workspace.
        self.prune_tabs(cx);
        self.advance_splash(state, cx);
    }

    /// A remotely applied update swapped this app's bundle; the relauncher
    /// is waiting for this process to exit. Quit through the normal path so
    /// the embedded engine flushes before the new bundle opens.
    /// Returns whether it quit.
    fn quit_for_relaunch(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) -> bool {
        if !self.updates.relaunch_quit_sent
            && state
                .read(cx)
                .update
                .as_ref()
                .is_some_and(|update| update.relaunch_pending)
        {
            self.updates.relaunch_quit_sent = true;
            tracing::info!(
                "update applied by the engine; quitting so the relauncher can open the new bundle"
            );
            cx.quit();
            return true;
        }
        false
    }

    /// Follow auth into the sync lifecycle: the next sync step, the in-place
    /// switch, and the move back to a local runtime after a synced sign-out.
    fn follow_sync_lifecycle(
        &mut self,
        state: &Entity<AppState>,
        main_window: bool,
        cx: &mut Context<Self>,
    ) {
        let next_sync_flow = {
            let state = state.read(cx);
            sync_flow_after_auth(self.sync.flow, state.workspace_scope, state.auth.as_ref())
        };
        if main_window && next_sync_flow != self.sync.flow {
            self.sync.flow = next_sync_flow;
            if matches!(
                self.sync.flow,
                SyncFlow::RestartPending { .. } | SyncFlow::SwitchOffer { .. }
            ) {
                self.sync.org = None;
            }
        }
        // The in-place local→synced switch: once the replacement runtime is
        // attached and Ready, kick the import (or finish) from here.
        if main_window {
            self.drive_sync_switch(cx);
        }
        let signed_out_synced = main_window && {
            let state = state.read(cx);
            state.workspace_scope == Some(WorkspaceScope::Synced)
                && matches!(state.auth, Some(AuthState::SignedOut))
        };
        // AuthStatus is shared by every viewport. Whichever viewport owns the
        // embedded runtime drains it; remote viewports request daemon shutdown
        // and all of them independently reattach to the new local runtime.
        if signed_out_synced && self.sync.runtime_change_task.is_none() {
            self.start_local_runtime_transition(false, cx);
        }
    }

    /// Capture knobs that open a dialog once its data has landed.
    fn open_capture_dialogs(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        // Capture knob: the add-space palette needs only the device registry.
        if self.dev.open_dialog.as_deref() == Some("add-space")
            && !state.read(cx).devices.is_empty()
        {
            self.dev.open_dialog = None;
            self.open_add_space(cx);
        }
        // Capture knob: pop the requested dialog once chats have landed.
        if let Some(which) = self.dev.open_dialog.clone()
            && let Some(first) = state.read(cx).chats.first().map(|c| c.id.clone())
        {
            self.dev.open_dialog = None;
            match which.as_str() {
                "rename" => self.open_rename_chat(first, cx),
                "delete" => {
                    self.dialogs.delete_chat = Some(first);
                }
                _ => {}
            }
        }
    }

    /// Session chimes (herdr semantics, `sound::sound_for_transition`): a
    /// question rings whenever a session flips to AwaitingInput, a
    /// completion rings on the Working→Idle edge — for ANY session on any
    /// device. A row's first appearance only seeds the baseline, so boot
    /// (restored rows) and fresh sends stay silent. Desktop banners
    /// (`notify::post`) ride the SAME edges and gates behind their own
    /// settings flag — one detector, two outputs, so the banner can never
    /// fire where the chime wouldn't.
    ///
    /// STALENESS-GATED like the dot (`effective_indicator`): raw row
    /// statuses include the past, and a dead turn's stale Working row must
    /// not turn a late-synced Idle into a "done" chime. The chime judges by
    /// the dot's clock.
    ///
    /// SEND-PENDING-GATED too (`AppState::send_pending`): until the host
    /// acks a queued send, the done-chime stays quiet for that chat while
    /// the baseline keeps tracking silently, so a phantom Working→Idle
    /// never fires later. The question chime is NOT gated: an instant
    /// AwaitingInput ack should still ring.
    ///
    /// WINDOW-SCOPED: each window rings for the sessions it lists, so a
    /// project open in its own window rings once, from there. Rows outside
    /// the scope still track their baseline silently — a project that
    /// returns to the main window never replays an edge it already rang.
    fn ring_session_chimes(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        let now = Utc::now();
        type Ping = (
            String,
            cypher_proto::SessionStatus,
            bool,
            Option<String>,
            bool,
        );
        let sessions: Vec<Ping> = {
            let state = state.read(cx);
            let scope = state.project_scope();
            state
                .sessions
                .iter()
                .map(|s| {
                    use cypher_proto::view::Indicator;
                    let status = match cypher_proto::view::effective_indicator(Some(s), now) {
                        Indicator::Working => cypher_proto::SessionStatus::Working,
                        Indicator::AwaitingInput => cypher_proto::SessionStatus::AwaitingInput,
                        Indicator::Errored => cypher_proto::SessionStatus::Errored,
                        Indicator::None => cypher_proto::SessionStatus::Idle,
                    };
                    let send_pending = state.send_pending(&s.chat_id, now);
                    let chat = state.chats.iter().find(|c| c.id == s.chat_id);
                    let title = chat.and_then(|c| c.title.clone());
                    // Rows without a chat yet belong to the main window.
                    let in_scope = chat.map_or(scope.only.is_none(), |c| scope.chat_visible(c));
                    (s.chat_id.clone(), status, send_pending, title, in_scope)
                })
                .collect()
        };
        let (sound_enabled, notifications_enabled, background_only) = self.chime_settings(cx);
        // Background-only banners: `active_window()` is app-level (any
        // Cypher window being key), so a ping for a *background chat* in a
        // focused app still stays a chime — you're already looking at
        // Cypher; the sidebar dot carries the rest.
        let app_focused = cx.active_window().is_some();
        for (chat_id, status, send_pending, title, in_scope) in sessions {
            let prev = self.attention.sound_prev.insert(chat_id, status);
            if in_scope
                && let Some(prev) = prev
                && let Some(sound) = crate::kit::sound::sound_for_transition(prev, status)
                && !(send_pending && sound == crate::kit::sound::Sound::Done)
            {
                if sound_enabled {
                    crate::kit::sound::play(sound);
                }
                if notifications_enabled && !(background_only && app_focused) {
                    let title = title.unwrap_or_else(|| "New session".into());
                    let body = match sound {
                        crate::kit::sound::Sound::Done => "Run finished",
                        crate::kit::sound::Sound::Request => "Waiting on your input",
                    };
                    crate::shell::notify::post(&title, body);
                }
            }
        }
    }

    /// Boot: restore the last selected space once the first spaces frame
    /// lands (a still-existing row wins over the auto-selected first one;
    /// the boot-auto-selected chat's own space wins over both — selecting a
    /// chat implies its space, which `select_chat` already applied).
    fn restore_boot_space(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        if !self.space_boot_applied && !state.read(cx).spaces.is_empty() {
            self.space_boot_applied = true;
            if state.read(cx).selected_chat.is_none() {
                // Restore the last selected project (unless the user opted
                // out of projects); a still-existing row wins over the
                // auto-selected first one. The sidebar never filters — the
                // canvas defaults are the only target.
                let exists = |id: &String| state.read(cx).space_row(id).is_some();
                let target = if !state.read(cx).no_project {
                    self.settings.last_space_id.clone().filter(&exists)
                } else {
                    None
                };
                if target.is_some() {
                    state.update(cx, |s, cx| s.select_space(target.clone(), cx));
                    // A boot canvas tile opened before the spaces frame
                    // copied main's then-empty pick: aim it too.
                    let canvases: Vec<Entity<AppState>> = self
                        .tiles
                        .slots
                        .values()
                        .filter(|slot| matches!(slot.tab, crate::workspace::TabKey::NewSession(_)))
                        .map(|slot| slot.state.clone())
                        .collect();
                    for canvas in canvases {
                        canvas.update(cx, |s, cx| s.select_space(target.clone(), cx));
                    }
                }
            }
        }
    }

    /// Persist the selected space (the new-tab fallback) — only when it
    /// resolves to a LIVE Space. A dangling id (space deleted elsewhere)
    /// must never overwrite the remembered one, or the next boot would
    /// restore a dead project.
    fn persist_selected_space(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        let state = state.read(cx);
        let live = state.selected_space_if_live();
        if live.is_some() && live != self.settings.last_space_id {
            self.settings.last_space_id = live;
            self.schedule_save(cx);
        }
    }

    /// Fade the boot splash out once connected (and drop it on a failed
    /// connection so the gate card shows at once).
    fn advance_splash(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        match state.read(cx).connection {
            ConnectionStatus::Ready => {
                if self.splash.phase == SplashPhase::Visible {
                    self.splash.phase = SplashPhase::FadingOut;
                    self.splash.task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(SPLASH_OUT.total() + Duration::from_millis(30))
                            .await;
                        this.update(cx, |shell, cx| {
                            shell.splash.phase = SplashPhase::Gone;
                            cx.notify();
                        })
                        .ok();
                    }));
                }
            }
            // Reveal the gate card immediately; the splash never returns mid-session.
            ConnectionStatus::Failed(_) => self.splash.phase = SplashPhase::Gone,
            ConnectionStatus::Connecting => {}
        }
    }
}
