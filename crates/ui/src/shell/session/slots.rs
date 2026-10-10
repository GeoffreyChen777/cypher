//! Session slots: one per open chat tab — creating and disposing them,
//! keeping them in step with the workspace layout, following focus, and
//! routing their state and composer events.

use super::*;

impl Shell {
    // ---- slot lookup ----

    pub(in crate::shell) fn slot_for_tab(&self, tab: &TabKey) -> Option<SlotId> {
        self.tiles
            .slots
            .iter()
            .find(|(_, slot)| slot.tab == *tab)
            .map(|(sid, _)| *sid)
    }

    /// The focused group's active tab's slot.
    pub(in crate::shell) fn focused_slot(&self) -> Option<SlotId> {
        self.slot_for_tab(self.workspace.focused_tab()?)
    }

    /// The slot whose context shows `chat_id` (comment / side-chat routing).
    pub(in crate::shell) fn slot_for_chat(&self, chat_id: &str, cx: &App) -> Option<SlotId> {
        self.slot_for_tab(&TabKey::session(chat_id)).or_else(|| {
            self.tiles
                .slots
                .iter()
                .find(|(_, slot)| slot.state.read(cx).selected_chat.as_deref() == Some(chat_id))
                .map(|(sid, _)| *sid)
        })
    }

    /// The focused tile's chat id ("" for a canvas / empty tile) — the nav
    /// history key.
    pub(in crate::shell) fn focused_chat_key(&self) -> String {
        self.workspace
            .focused_tab()
            .and_then(|tab| tab.chat_id())
            .unwrap_or_default()
            .to_string()
    }

    /// Land keyboard focus in the focused tile's composer — or on the root
    /// when that tile is empty, so window shortcuts keep dispatching.
    pub(in crate::shell) fn focus_landing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self
            .focused_slot()
            .and_then(|sid| self.tiles.slots.get(&sid))
        {
            Some(slot) => window.focus(&slot.composer.focus_handle(cx), cx),
            None => window.focus(&self.root_focus, cx),
        }
    }

    // ---- slot lifecycle ----

    fn create_slot(&mut self, tab: TabKey, cx: &mut Context<Self>) -> SlotId {
        let sid = self.tiles.next_slot_id;
        self.tiles.next_slot_id += 1;
        let chat_id = tab.chat_id().map(str::to_string);
        let state = AppState::new_session_context(&self.state, chat_id.clone(), cx);
        let popup = self.comment_popup.clone().downgrade();
        let transcript = cx.new(|cx| Transcript::new(state.clone(), popup, cx));
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        let subagents = cx.new(|cx| SubagentsPanel::new(state.clone(), cx));
        // "PaletteSearch" context: ↵ / ⇧↵ / esc stay unbound so they bubble
        // to the bar's frame (`find_key`) as match navigation instead of
        // editing text.
        let find_input = cx.new(|cx| TextInput::with_context("Find in chat…", "PaletteSearch", cx));
        let subscriptions = vec![
            // A tile's sends change the shared pending-send overlay without
            // notifying main, so the shell observes each context itself
            // (never main → contexts: that loops through the mirror).
            cx.observe(&state, move |this: &mut Shell, _, cx| {
                this.on_slot_state_changed(sid, cx);
                cx.notify();
            }),
            cx.subscribe(
                &composer,
                move |this: &mut Shell, _, event: &ComposerEvent, cx| {
                    this.on_composer_event(sid, event, cx);
                },
            ),
            // Session Fork (v1): a settled entry's fork affordance → the shell
            // owns the ForkSession RPC (target host when remote) and the
            // created chat's tab/prefill.
            cx.subscribe(
                &transcript,
                move |this: &mut Shell, _, event: &crate::transcript::TranscriptEvent, cx| {
                    match event {
                        crate::transcript::TranscriptEvent::ForkRequested {
                            chat_id,
                            anchor_message_id,
                        } => {
                            // `fork_session` arms the loading guard itself.
                            this.fork_session(sid, chat_id.clone(), anchor_message_id.clone(), cx);
                        }
                        crate::transcript::TranscriptEvent::RewindRequested {
                            chat_id,
                            anchor_message_id,
                        } => {
                            // Session Rewind: the transcript already took the
                            // user's confirming click; the shell owns the
                            // RewindSession RPC.
                            this.rewind_session(
                                sid,
                                chat_id.clone(),
                                anchor_message_id.clone(),
                                cx,
                            );
                        }
                    }
                },
            ),
            // An inspector row opens the child chat in its own tab (the
            // context stays on this tile's session).
            cx.subscribe(
                &subagents,
                |this: &mut Shell, _, event: &crate::subagents::SubagentsEvent, cx| match event {
                    crate::subagents::SubagentsEvent::OpenChat(chat_id) => {
                        this.open_chat(chat_id.clone(), cx);
                    }
                },
            ),
            cx.subscribe(&find_input, {
                let transcript = transcript.clone();
                move |_: &mut Shell, input, event: &TextInputEvent, cx| {
                    if matches!(event, TextInputEvent::Edited) {
                        let query = input.read(cx).text().to_owned();
                        transcript.update(cx, |t, cx| t.set_find_query(&query, cx));
                    }
                }
            }),
        ];
        // This session's remembered docks, else the most recently used sizes.
        let docks = {
            let settings = self.live_settings(cx);
            chat_id
                .as_ref()
                .and_then(|id| settings.session_docks.get(id).copied())
                .unwrap_or_else(|| SessionDock::seeded_from(settings.latest_session_dock()))
        };
        let mut dock = Dock::new();
        // Never open on the canvas (nothing to host yet).
        dock.open = docks.right_open && chat_id.is_some();
        // A session without a saved draft of its own (never opened, or a
        // fork's prefill landing on a background tab) gets back the one
        // stashed for it.
        if let Some(chat_id) = &chat_id
            && let Some((draft, staged)) = self.closed_tabs.drafts.remove(chat_id)
        {
            composer.update(cx, |composer, cx| {
                composer.seed_draft(chat_id, draft, cx);
                composer.seed_attachments(chat_id, staged, cx);
            });
        }
        // The session's terminals outlived its last tab: re-bind them here.
        let terminal = chat_id
            .as_ref()
            .and_then(|id| self.closed_tabs.terminals.remove(id));
        if let Some(panel) = &terminal {
            let open = docks.terminal_open;
            panel.update(cx, |panel, cx| {
                panel.rebind(state.clone(), cx);
                panel.set_open(open, cx);
            });
        }
        self.tiles.slots.insert(
            sid,
            SessionSlot {
                tab,
                state,
                transcript,
                composer,
                find_input,
                find_focus: cx.focus_handle(),
                find_focus_pending: false,
                subagents,
                file_drag_active: false,
                // Seed with the compact composer stack's rough height so the
                // first frame's clearance isn't zero (the measure corrects it).
                bottom_stack: Rc::new(Cell::new(120.0)),
                // A roomy guess until the first paint measures it, so the
                // first frame never reads as "too narrow for the dock".
                area: Rc::new(Cell::new(gpui::Bounds::new(
                    gpui::point(px(0.0), px(0.0)),
                    gpui::size(px(1000.0), px(800.0)),
                ))),
                terminal,
                // Opened on first render (`render_terminal_container`).
                terminal_open: docks.terminal_open,
                right_fraction: docks.right,
                terminal_fraction: docks.terminal,
                terminal_tween: None,
                terminal_tween_task: None,
                terminal_drag_anchor: None,
                dock,
                diffs: std::collections::HashMap::new(),
                diff_subs: std::collections::HashMap::new(),
                diff_seq: 0,
                files: std::collections::HashMap::new(),
                files_seq: 0,
                side_chats: std::collections::HashMap::new(),
                side_chat_subs: std::collections::HashMap::new(),
                side_chat_seq: 0,
                _subscriptions: subscriptions,
            },
        );
        sid
    }

    /// Drop a slot whose tab closed: stash its unsent draft, dispose its
    /// temporary side chats (engine objects), detach its surfaces' floating
    /// comment state before the entities go, and park its terminals (their
    /// PTYs outlive the tab) — or close them when the chat is gone.
    fn dispose_slot(&mut self, sid: SlotId, cx: &mut Context<Self>) {
        let Some(slot) = self.tiles.slots.remove(&sid) else {
            return;
        };
        if let Some(chat_id) = slot.tab.chat_id() {
            let composer = slot.composer.read(cx);
            let draft = composer.current_draft(cx);
            let staged = composer.staged_attachments();
            if !draft.trim().is_empty() || !staged.is_empty() {
                self.closed_tabs
                    .drafts
                    .insert(chat_id.to_string(), (draft, staged));
            }
        }
        for panel in slot.side_chats.values() {
            panel.update(cx, |panel, cx| panel.dispose(cx));
        }
        for changes in slot.diffs.values() {
            changes.update(cx, |changes, cx| changes.detach(cx));
        }
        slot.transcript
            .update(cx, |t, cx| t.dismiss_comment_ui_and_selection(cx));
        if let Some(terminal) = slot.terminal {
            terminal.update(cx, |terminal, cx| terminal.detach_comment_selection(cx));
            let live_chat = slot
                .tab
                .chat_id()
                .filter(|id| self.state.read(cx).chats.iter().any(|c| c.id == *id));
            match live_chat {
                Some(chat_id) => {
                    // Only the panel keeps the context alive now: stop its
                    // mirror and transcript watches.
                    slot.state.update(cx, |s, _| s.park_session_context());
                    terminal.update(cx, |terminal, cx| terminal.set_open(false, cx));
                    self.closed_tabs
                        .terminals
                        .insert(chat_id.to_string(), terminal);
                }
                None => terminal.update(cx, |terminal, cx| terminal.close_all(cx)),
            }
        }
        if self.menus.right_plus.get() == Some(&sid) {
            self.close_right_plus(cx);
        }
    }

    /// Make the slot set match the workspace: drop the slots of closed
    /// tabs, create one for every visible tab (and the focused one) that has
    /// none yet — background tabs and the tiles hidden behind a zoom get
    /// theirs when first shown, then keep it.
    pub(in crate::shell) fn sync_slots(&mut self, cx: &mut Context<Self>) {
        let tabs: std::collections::HashSet<TabKey> = self.workspace.tabs().cloned().collect();
        let stale: Vec<SlotId> = self
            .tiles
            .slots
            .iter()
            .filter(|(_, slot)| !tabs.contains(&slot.tab))
            .map(|(sid, _)| *sid)
            .collect();
        for sid in stale {
            self.dispose_slot(sid, cx);
        }
        let shown: Vec<TabKey> = self
            .workspace
            .visible_tabs()
            .into_iter()
            .chain(self.workspace.focused_tab())
            .cloned()
            .collect();
        for tab in shown {
            if self.slot_for_tab(&tab).is_none() {
                self.create_slot(tab, cx);
            }
        }
    }

    /// Persist a slot's docks under its session (sizes + open flags; a
    /// canvas has no session to key them by yet).
    pub(in crate::shell) fn remember_slot_docks(&mut self, sid: SlotId, cx: &mut Context<Self>) {
        let Some(slot) = self.tiles.slots.get(&sid) else {
            return;
        };
        let Some(chat_id) = slot.tab.chat_id().map(str::to_string) else {
            return;
        };
        let docks = SessionDock {
            right: slot.right_fraction,
            terminal: slot.terminal_fraction,
            right_open: slot.dock.open,
            terminal_open: slot.terminal_open,
            used_at: 0,
        };
        let now = Utc::now().timestamp_millis();
        self.persist_settings(cx, move |settings| {
            settings.remember_session_dock(&chat_id, docks, now);
        });
    }

    /// Persist the workspace layout (every mutation lands here through
    /// [`Self::workspace_changed`]; split drags call it directly); held until
    /// boot restored the saved one.
    pub(in crate::shell) fn save_layout(&mut self, cx: &mut Context<Self>) {
        if !self.tiles.boot_landed {
            return;
        }
        match self.project_window.clone() {
            // The main window's layout is stamped into the save snapshot.
            None => self.schedule_save(cx),
            Some(project) => {
                let workspace = self.workspace.clone();
                self.persist_settings(cx, move |settings| {
                    settings
                        .project_workspaces
                        .insert(project.clone(), workspace.clone());
                });
            }
        }
    }

    /// Main's selection FOLLOWS the focused tile's session: the sidebar
    /// highlight, nav history, cycle order, space implication and
    /// notification activity all read it. Only runs on a focus change, so
    /// main's own notifies never feed back into the workspace.
    pub(in crate::shell) fn sync_follow(&mut self, cx: &mut Context<Self>) {
        let focused = self.workspace.focused_tab().cloned();
        if focused == self.tiles.followed {
            return;
        }
        self.tiles.followed = focused;
        let chat = self
            .tiles
            .followed
            .as_ref()
            .and_then(|tab| tab.chat_id())
            .map(str::to_string);
        if self.state.read(cx).selected_chat != chat {
            self.state
                .update(cx, |s, cx| s.select_chat(chat.clone(), cx));
        }
        // Route history: a session switch is a navigation. The very first
        // landing off the untouched boot canvas REPLACES that entry —
        // zeron's `/` route redirected into the last-used chat, leaving no
        // dead Back target. Walking history lands here too, but the
        // destination already equals `current()`, so the push dedups.
        if matches!(self.route, Route::Chat) {
            let entry = NavEntry::Chat(chat.unwrap_or_default());
            if self.nav.len() == 1 && *self.nav.current() == NavEntry::Chat(String::new()) {
                self.nav.replace(entry);
            } else {
                self.nav.push(entry);
            }
        }
    }

    /// After any workspace mutation: slots match the tabs, sessions that
    /// left the screen drop their comment UI, main follows the focused
    /// tile, the layout is saved, and the window re-renders.
    pub(in crate::shell) fn workspace_changed(&mut self, cx: &mut Context<Self>) {
        self.sync_slots(cx);
        self.dismiss_hidden_comment_ui(cx);
        self.sync_follow(cx);
        self.save_layout(cx);
        cx.notify();
    }

    /// The slots on screen now vs at the last workspace change: a
    /// transcript that went to the background (tab switch, sidebar pick,
    /// nav, drop, preset, zoom…) takes its comment pill/editor and selection
    /// with it, and the shared popup closes — it must not float at a stale
    /// anchor over the incoming session. Closed slots did this on disposal.
    fn dismiss_hidden_comment_ui(&mut self, cx: &mut Context<Self>) {
        let shown: Vec<SlotId> = self
            .workspace
            .visible_tabs()
            .into_iter()
            .filter_map(|tab| self.slot_for_tab(tab))
            .collect();
        let before = std::mem::replace(&mut self.tiles.shown_slots, shown);
        let hidden: Vec<SlotId> = before
            .into_iter()
            .filter(|sid| !self.tiles.shown_slots.contains(sid))
            .collect();
        if hidden.is_empty() {
            return;
        }
        for sid in hidden {
            if let Some(slot) = self.tiles.slots.get(&sid) {
                slot.transcript
                    .update(cx, |t, cx| t.dismiss_comment_ui_and_selection(cx));
            }
        }
        self.dismiss_comment_popup(cx);
    }

    /// A slot's context changed (its own notify — the shell does not observe
    /// main's contexts through main):
    /// - a canvas whose first send selected a chat becomes that session's
    ///   tab in place (same entities — the send is in flight);
    /// - a session tile whose context moved to ANOTHER chat goes back to its
    ///   own chat, and the other chat opens like a sidebar click (a
    ///   fallback: the subagents inspector asks the shell directly,
    ///   [`crate::subagents::SubagentsEvent`]);
    /// - a session whose chat vanished closes its tab — unless its row is
    ///   still on the way ([`Self::chat_awaited`]): the context re-selects
    ///   it and waits.
    fn on_slot_state_changed(&mut self, sid: SlotId, cx: &mut Context<Self>) {
        let Some(slot) = self.tiles.slots.get(&sid) else {
            return;
        };
        let selected = slot.state.read(cx).selected_chat.clone();
        let slot_state = slot.state.clone();
        match (slot.tab.clone(), selected) {
            (tab @ TabKey::NewSession(_), Some(chat_id)) => {
                let session = TabKey::Session(chat_id);
                let open_elsewhere = self.workspace.contains(&session);
                self.workspace.replace_tab(&tab, session.clone());
                if !open_elsewhere && let Some(slot) = self.tiles.slots.get_mut(&sid) {
                    slot.tab = session;
                }
                self.workspace_changed(cx);
            }
            (TabKey::NewSession(_), None) => {
                // The canvas's project pick is the new-session fallback the
                // next boot restores (main window only; live projects only).
                if !self.is_project_window() {
                    let live = slot_state.read(cx).selected_space_if_live();
                    if live.is_some() && live != self.settings.last_space_id {
                        self.settings.last_space_id = live;
                        self.schedule_save(cx);
                    }
                }
            }
            (TabKey::Session(bound), Some(chat_id)) if bound != chat_id => {
                slot_state.update(cx, |s, cx| s.select_chat(Some(bound), cx));
                self.open_chat(chat_id, cx);
            }
            (TabKey::Session(bound), None) if self.chat_awaited(&bound, cx) => {
                slot_state.update(cx, |s, cx| s.select_chat(Some(bound), cx));
            }
            (tab @ TabKey::Session(_), None) => {
                self.workspace.close(&tab);
                self.workspace_changed(cx);
            }
            (TabKey::Session(_), Some(_)) => {}
        }
    }

    fn on_composer_event(&mut self, sid: SlotId, event: &ComposerEvent, cx: &mut Context<Self>) {
        let Some((composer, transcript)) = self
            .tiles
            .slots
            .get(&sid)
            .map(|slot| (slot.composer.clone(), slot.transcript.clone()))
        else {
            return;
        };
        match event {
            ComposerEvent::OpenAgentSettings { target_device } => {
                let result = self.settings_target.update(cx, |target, cx| {
                    target.select(Some(target_device.clone()), cx)
                });
                match result {
                    Ok(()) => self.open_settings(SettingsSection::Harnesses, cx),
                    Err(error) => {
                        composer.update(cx, |composer, cx| composer.report_error(error, cx))
                    }
                }
            }
            ComposerEvent::OpenGithubSettings { target_device } => {
                let result = self.settings_target.update(cx, |target, cx| {
                    target.select(Some(target_device.clone()), cx)
                });
                match result {
                    Ok(()) => self.open_settings(SettingsSection::Github, cx),
                    Err(error) => {
                        composer.update(cx, |composer, cx| composer.report_error(error, cx))
                    }
                }
            }
            ComposerEvent::OpenProviders {
                intent,
                target_device,
            } => {
                let result = self
                    .settings_target
                    .update(cx, |target, cx| target.select(target_device.clone(), cx));
                match result {
                    Ok(()) => self.open_providers(intent.clone(), cx),
                    Err(error) => {
                        composer.update(cx, |composer, cx| composer.report_error(error, cx))
                    }
                }
            }
            // Every send glides the prompt to the viewport top and reserves
            // the reply's space below it (notes-app parity).
            ComposerEvent::Sent {
                chat_id,
                message_id,
            } => {
                transcript.update(cx, |t, cx| {
                    t.on_own_send(chat_id.clone(), message_id.clone(), cx)
                });
            }
        }
    }
}
