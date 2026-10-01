//! Session slots (docs/workspace-layout.md): the per-session UI behind one
//! workspace tab — chat (transcript + status strip + composer), the bottom
//! terminal dock, and the right dock's surfaces ([`super::dock`]).
//!
//! Every slot renders from its own session context
//! ([`AppState::new_session_context`]): a secondary state pinned to the
//! tab's session, so `Transcript`, `Composer`, `Changes`, `FilesPanel` and
//! `TerminalPanel` keep reading `selected_chat` unchanged. A slot exists only
//! for a tab in the workspace: it is created when the tab is first ACTIVE in
//! its group (a restored layout doesn't spin up a context per background
//! tab) and dropped when the tab closes (dropping the context kills its
//! watches).
//!
//! Dock sizes are fractions of the slot's session area, remembered per
//! session in `UiSettings.session_docks` (docs/workspace-layout.md,
//! decision 7).

use std::cell::Cell;
use std::rc::Rc;

use super::dock::{Dock, DockSurface, terminal_dock_height};
use super::*;
use crate::settings::SessionDock;
use crate::workspace::TabKey;

/// Stable slot identity: survives a canvas tab becoming its session
/// (`TabKey::NewSession` → `TabKey::Session`), so async replies and element
/// listeners can hold it.
pub(super) type SlotId = u64;

/// Round-21 Side Chats cap: at most this many temporary side chat tabs per
/// chat (they are host-memory engine objects; a runaway count would leak
/// docs and streams). Further offers are ignored.
const MAX_SIDE_CHATS_PER_CHAT: usize = 8;

/// Below this chat-column width (beside the dock's minimum) the dock takes
/// the session area over (decision 6).
pub(super) const CHAT_MIN_WIDTH: f32 = 320.0;

/// The dividers between the chat, terminal and dock stop this far short of
/// the tile edges (user request: lines that don't run edge to edge).
pub(super) const DIVIDER_INSET: f32 = 12.0;

/// Drag marker for a slot's terminal-dock height handle.
pub(super) struct TerminalResize(pub(super) SlotId);

/// Find-bar button tooltip (the step/close glyphs carry their key equivalent
/// so the bar teaches ↵ / ⇧↵ / esc without spelling them on the chrome).
pub(super) struct FindTooltip(pub(super) SharedString);

impl Render for FindTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        motion::fade_quick(
            "find-tooltip",
            div()
                .px(px(8.0))
                .py(px(5.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.surface_raised)
                .text_size(px(11.0))
                .text_color(theme.text_muted)
                .child(self.0.clone()),
        )
    }
}

/// The per-session UI of one workspace tab.
pub(super) struct SessionSlot {
    /// The workspace tab this slot renders (re-keyed in place when a canvas
    /// sends its first message).
    pub(super) tab: TabKey,
    /// The session context (see the module docs).
    pub(super) state: Entity<AppState>,
    pub(super) transcript: Entity<Transcript>,
    pub(super) composer: Entity<Composer>,
    /// In-chat find (⌘F): the query field and the frame that owns its keys.
    /// Whether the BAR is on screen is the transcript's state
    /// ([`Transcript::find_open`]) — this pair is just the chrome, so a chat
    /// switch closing find over there closes the bar here with no second
    /// flag to keep in step.
    pub(super) find_input: Entity<ComposerInput>,
    pub(super) find_focus: gpui::FocusHandle,
    /// Focus lands in the field on the render after ⌘F (the element has to
    /// exist first — the palette flows do the same).
    pub(super) find_focus_pending: bool,
    /// Session-level subagents chrome (current chat's live subagent runs): a
    /// compact trigger on the status strip's right edge with an upward
    /// inspector popover. Renders Empty without records, so the fixed-height
    /// status strip (and the measured bottom stack) never shifts.
    pub(super) subagents: Entity<SubagentsPanel>,
    /// External file drag hovering the conversation column — shows the
    /// "Drop images to attach" veil over the whole chat area; a drop stages
    /// the files in the composer.
    pub(super) file_drag_active: bool,
    /// Measured height of the bottom chrome stack (status strip + composer)
    /// the full-height transcript scrolls under — written by a
    /// paint-time canvas each frame, read the NEXT frame for the fade inset,
    /// the transcript's bottom clearance, and the jump pill's anchor (the
    /// same one-frame lag every fade here rides).
    pub(super) bottom_stack: Rc<Cell<f32>>,
    /// The session area's window bounds (below the tile header), measured at
    /// paint like `bottom_stack`: sizes the docks and drives their drags.
    pub(super) area: Rc<Cell<gpui::Bounds<Pixels>>>,
    /// Lazy bottom terminal dock: no entity (and no RPC) until first opened.
    pub(super) terminal: Option<Entity<TerminalPanel>>,
    /// Terminal dock open (remembered per session with the sizes below).
    pub(super) terminal_open: bool,
    /// Right dock width / terminal dock height as fractions of the session
    /// area; `None` = never dragged (the legacy global pixel size).
    pub(super) right_fraction: Option<f32>,
    pub(super) terminal_fraction: Option<f32>,
    pub(super) terminal_tween: Option<WidthTween>,
    /// Clears the height tween once it completes (so a closed panel unmounts).
    pub(super) terminal_tween_task: Option<Task<()>>,
    /// Height-drag anchor: (pointer y, height) at mouse-down on the handle.
    pub(super) terminal_drag_anchor: Option<(f32, f32)>,
    /// The right dock (surface host) of this session.
    pub(super) dock: Dock,
    /// Diff surfaces by id — each tab its own [`Changes`] viewer with its own
    /// scope/base pick and diff watch (multiple diff panels, user request).
    pub(super) diffs: std::collections::HashMap<u64, Entity<Changes>>,
    /// Event hookups for [`Self::diffs`] (History rows opening commit tabs).
    pub(super) diff_subs: std::collections::HashMap<u64, Subscription>,
    pub(super) diff_seq: u64,
    /// Files surfaces by id — each tab its own tree + editor over the
    /// chat's checkout.
    pub(super) files: std::collections::HashMap<u64, Entity<FilesPanel>>,
    pub(super) files_seq: u64,
    /// Temporary Side Chat tabs (round 21): one [`SideChatPanel`] per open
    /// side chat, keyed by a slot-minted sequence id. Owned here so the
    /// shell can promote/close/dispose them and re-render their tabs.
    ///
    /// [`SideChatPanel`]: crate::side_chats::SideChatPanel
    pub(super) side_chats: std::collections::HashMap<u64, Entity<crate::side_chats::SideChatPanel>>,
    pub(super) side_chat_subs: std::collections::HashMap<u64, Subscription>,
    pub(super) side_chat_seq: u64,
    /// Context observation, composer / transcript / find-field events.
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    // ---- slot lookup ----

    pub(super) fn slot_for_tab(&self, tab: &TabKey) -> Option<SlotId> {
        self.slots
            .iter()
            .find(|(_, slot)| slot.tab == *tab)
            .map(|(sid, _)| *sid)
    }

    /// The focused group's active tab's slot.
    pub(super) fn focused_slot(&self) -> Option<SlotId> {
        self.slot_for_tab(self.workspace.focused_tab()?)
    }

    /// The slot whose context shows `chat_id` (comment / side-chat routing).
    pub(super) fn slot_for_chat(&self, chat_id: &str, cx: &App) -> Option<SlotId> {
        self.slot_for_tab(&TabKey::session(chat_id)).or_else(|| {
            self.slots
                .iter()
                .find(|(_, slot)| slot.state.read(cx).selected_chat.as_deref() == Some(chat_id))
                .map(|(sid, _)| *sid)
        })
    }

    /// The focused tile's chat id ("" for a canvas / empty tile) — the nav
    /// history key.
    pub(super) fn focused_chat_key(&self) -> String {
        self.workspace
            .focused_tab()
            .and_then(|tab| tab.chat_id())
            .unwrap_or_default()
            .to_string()
    }

    /// Land keyboard focus in the focused tile's composer — or on the root
    /// when that tile is empty, so window shortcuts keep dispatching.
    pub(super) fn focus_landing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.focused_slot().and_then(|sid| self.slots.get(&sid)) {
            Some(slot) => window.focus(&slot.composer.focus_handle(cx), cx),
            None => window.focus(&self.root_focus, cx),
        }
    }

    // ---- slot lifecycle ----

    fn create_slot(&mut self, tab: TabKey, cx: &mut Context<Self>) -> SlotId {
        let sid = self.next_slot_id;
        self.next_slot_id += 1;
        let chat_id = tab.chat_id().map(str::to_string);
        let state = AppState::new_session_context(&self.state, chat_id.clone(), cx);
        let popup = self.comment_popup.clone().downgrade();
        let transcript = cx.new(|cx| Transcript::new(state.clone(), popup, cx));
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        let subagents = cx.new(|cx| SubagentsPanel::new(state.clone(), cx));
        // "PaletteSearch" context: ↵ / ⇧↵ / esc stay unbound so they bubble
        // to the bar's frame (`find_key`) as match navigation instead of
        // editing text.
        let find_input =
            cx.new(|cx| ComposerInput::with_context("Find in chat…", "PaletteSearch", cx));
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
                move |_: &mut Shell, input, event: &ComposerInputEvent, cx| {
                    if matches!(event, ComposerInputEvent::Edited) {
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
            && let Some((draft, staged)) = self.closed_drafts.remove(chat_id)
        {
            composer.update(cx, |composer, cx| {
                composer.seed_draft(chat_id, draft, cx);
                composer.seed_attachments(chat_id, staged, cx);
            });
        }
        // The session's terminals outlived its last tab: re-bind them here.
        let terminal = chat_id
            .as_ref()
            .and_then(|id| self.parked_terminals.remove(id));
        if let Some(panel) = &terminal {
            let open = docks.terminal_open;
            panel.update(cx, |panel, cx| {
                panel.rebind(state.clone(), cx);
                panel.set_open(open, cx);
            });
        }
        self.slots.insert(
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
        let Some(slot) = self.slots.remove(&sid) else {
            return;
        };
        if let Some(chat_id) = slot.tab.chat_id() {
            let composer = slot.composer.read(cx);
            let draft = composer.current_draft(cx);
            let staged = composer.staged_attachments();
            if !draft.trim().is_empty() || !staged.is_empty() {
                self.closed_drafts
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
                    self.parked_terminals.insert(chat_id.to_string(), terminal);
                }
                None => terminal.update(cx, |terminal, cx| terminal.close_all(cx)),
            }
        }
        if self.right_plus.get() == Some(&sid) {
            self.close_right_plus(cx);
        }
    }

    /// Make the slot set match the workspace: drop the slots of closed
    /// tabs, create one for every visible tab (and the focused one) that has
    /// none yet — background tabs and the tiles hidden behind a zoom get
    /// theirs when first shown, then keep it.
    pub(super) fn sync_slots(&mut self, cx: &mut Context<Self>) {
        let tabs: std::collections::HashSet<TabKey> = self.workspace.tabs().cloned().collect();
        let stale: Vec<SlotId> = self
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
    pub(super) fn remember_slot_docks(&mut self, sid: SlotId, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get(&sid) else {
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
    pub(super) fn save_layout(&mut self, cx: &mut Context<Self>) {
        if !self.boot_landed {
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
    pub(super) fn sync_follow(&mut self, cx: &mut Context<Self>) {
        let focused = self.workspace.focused_tab().cloned();
        if focused == self.followed {
            return;
        }
        self.followed = focused;
        let chat = self
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
    pub(super) fn workspace_changed(&mut self, cx: &mut Context<Self>) {
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
        let before = std::mem::replace(&mut self.shown_slots, shown);
        let hidden: Vec<SlotId> = before
            .into_iter()
            .filter(|sid| !self.shown_slots.contains(sid))
            .collect();
        if hidden.is_empty() {
            return;
        }
        for sid in hidden {
            if let Some(slot) = self.slots.get(&sid) {
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
        let Some(slot) = self.slots.get(&sid) else {
            return;
        };
        let selected = slot.state.read(cx).selected_chat.clone();
        let slot_state = slot.state.clone();
        match (slot.tab.clone(), selected) {
            (tab @ TabKey::NewSession(_), Some(chat_id)) => {
                let session = TabKey::Session(chat_id);
                let open_elsewhere = self.workspace.contains(&session);
                self.workspace.replace_tab(&tab, session.clone());
                if !open_elsewhere && let Some(slot) = self.slots.get_mut(&sid) {
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

    // ---- per-slot render ----

    /// The session area inside a tile (below its header): the chat column
    /// and the right dock side by side, over the terminal dock spanning the
    /// whole width ("Git and files always open on the right, terminal
    /// always at the bottom" — a dock takeover never hides the terminal).
    /// The area's bounds are measured at paint for the docks' sizing and
    /// drags.
    pub(super) fn render_session(
        &mut self,
        sid: SlotId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(slot) = self.slots.get(&sid) else {
            return Empty.into_any_element();
        };
        let area = slot.area.clone();
        let size = area.get().size;
        // MessageRail width gate (hide below 48rem of chat width), the
        // transcript's bottom clearance (the chrome stack floating over its
        // bottom — the terminal dock sits below the chat column, not over
        // it) and the chat column's size (a tile resize re-anchors the list).
        let chat_width = (f32::from(size.width) - self.dock_width_now(slot)).max(0.0);
        let term_h = self.eval_tween(slot.terminal_tween, self.terminal_target(slot));
        let chat_height = (f32::from(size.height) - term_h).max(0.0);
        let stack_h = slot.bottom_stack.get();
        slot.transcript.update(cx, |t, cx| {
            t.set_rail_enabled(rail::rail_visible(chat_width), cx);
            t.set_bottom_clearance(stack_h, cx);
            t.set_viewport_size(chat_width, chat_height, cx);
        });
        let chat = self.render_session_chat(sid, chat_height, window, cx);
        let dock = self.render_dock(sid, cx);
        let terminal = self.render_terminal_container(sid, cx);
        // The session rail floats over the tile's top-right corner
        // (`render_group`), outside this measured area, so it does not
        // shrink the column.
        div()
            .id(("session", sid))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(
                gpui::canvas(move |bounds, _, _| area.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_row()
                    .child(chat)
                    .children(dock),
            )
            .child(terminal)
            .into_any_element()
    }

    // ---- temporary Side Chats (round 21) ----

    /// User-facing notice text for a failed `StartSideChat` RPC.
    ///
    /// `unknown method: StartSideChat` means the device hosting the parent
    /// session still runs a Cypher engine older than the Side Chat feature
    /// — the message says so and how to fix it. Every other failure gets
    /// the generic "Could not open Side Chat: …" (the full RPC error,
    /// e.g. an engine-side `Failed` message).
    pub(super) fn side_chat_start_error_text(err: &cypher_rpc::RpcError) -> String {
        if let cypher_rpc::RpcError::UnknownMethod(method) = err
            && method == methods::START_SIDE_CHAT
        {
            "Side Chat requires a newer Cypher engine on the device hosting \
             this session. Update that device or use a session hosted on \
             this device."
                .to_string()
        } else {
            format!("Could not open Side Chat: {err}")
        }
    }

    /// Session Fork idempotence: should the `(sourceChatId, anchorMessageId)`
    /// → requestId mapping survive this RPC outcome? Errors / lost replies
    /// keep it (the retry must reuse the SAME target id so the engine returns
    /// the created chat); a definitive reply (Created or typed Unavailable)
    /// drops it.
    pub(super) fn fork_request_id_retained(
        result: &Result<cypher_proto::SessionForkResponse, cypher_rpc::RpcError>,
    ) -> bool {
        result.is_err()
    }

    /// User-facing notice text for a failed `ForkSession` RPC.
    /// `unknown method: ForkSession` means the device hosting the source
    /// chat runs an engine too old for Session Fork — the message says so
    /// and how to fix it. Every other failure gets the generic "Could not
    /// fork: …" (the full RPC error).
    pub(super) fn fork_session_error_text(err: &cypher_rpc::RpcError) -> String {
        if let cypher_rpc::RpcError::UnknownMethod(method) = err
            && method == methods::FORK_SESSION
        {
            "Session Fork requires a newer Cypher engine on the device \
             hosting this session. Update that device or use a session \
             hosted on this device."
                .to_string()
        } else {
            format!("Could not fork: {err}")
        }
    }

    /// `ForkSession` for a settled transcript entry (Session Fork v1): mint
    /// a NEW durable root Pi chat on the source chat's host device
    /// (relay-forwarded when the source is remote). The reply's composer
    /// prefill is seeded into the fork's new tab (in the source tab's group);
    /// that tab is FOCUSED only when the user is still on the source chat —
    /// a late reply (user switched away) opens it in the background, never
    /// yanks the focus. Engine/Unavailable failures surface as a desktop
    /// notice AND the in-app sidebar notice strip. The transcript's in-flight
    /// marker (spinner + double-click guard) is begun here and ended on
    /// every settle path.
    ///
    /// Idempotence: the request id (the client-minted target chat id) is
    /// cached per `(sourceChatId, anchorMessageId)` and REUSED across RPC
    /// errors / lost replies — a retry hits the engine with the same id and
    /// returns the already-created chat instead of minting a twin. The cache
    /// entry is dropped once the RPC settles definitively (Created or typed
    /// Unavailable).
    pub(super) fn fork_session(
        &mut self,
        sid: SlotId,
        chat_id: String,
        anchor_message_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(slot_transcript) = self.slots.get(&sid).map(|s| s.transcript.clone()) else {
            return;
        };
        slot_transcript.update(cx, |t, cx| {
            t.begin_fork(chat_id.clone(), anchor_message_id.clone());
            cx.notify();
        });
        // Unwind the in-flight marker on every settle path (the RPC result
        // or an offline pre-flight failure).
        let settle = {
            let transcript = slot_transcript.clone();
            let chat_id = chat_id.clone();
            let anchor_message_id = anchor_message_id.clone();
            move |cx: &mut Context<Shell>| {
                transcript.update(cx, |t, cx| {
                    t.end_fork(chat_id.clone(), anchor_message_id.clone());
                    cx.notify();
                });
            }
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            tracing::warn!(%chat_id, "ForkSession skipped: engine offline");
            let notice = "Cannot fork: the engine is not connected.";
            crate::notify::post("Fork", notice);
            self.sidebar_notice = Some(notice.into());
            settle(cx);
            cx.notify();
            return;
        };
        // The request id is the client-minted TARGET chat id — reuse the
        // cached id for this (source, anchor) so a lost-reply retry returns
        // the SAME chat (idempotent); mint + remember it on first click.
        let key = (chat_id.clone(), anchor_message_id.clone());
        let request_id = self.fork_request_ids.get(&key).cloned().unwrap_or_else(|| {
            let id = uuid::Uuid::new_v4().to_string();
            self.fork_request_ids.insert(key, id.clone());
            id
        });
        let mut params = serde_json::Map::new();
        params.insert("requestId".into(), serde_json::Value::String(request_id));
        params.insert(
            "sourceChatId".into(),
            serde_json::Value::String(chat_id.clone()),
        );
        params.insert(
            "anchorMessageId".into(),
            serde_json::Value::String(anchor_message_id.clone()),
        );
        {
            let state = self.state.read(cx);
            if let (Some(chat), Some(local)) = (
                state.chats.iter().find(|c| c.id == chat_id),
                state.local_device_id.clone(),
            ) && chat.device_id != local
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(chat.device_id.clone()),
                );
            }
        }
        let params = serde_json::Value::Object(params);
        let weak = cx.weak_entity();
        let state = self.state.clone();
        cx.spawn(async move |_this, cx| {
            let value = engine.client().call(methods::FORK_SESSION, params).await;
            // Always clear the in-flight marker once the RPC settles.
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |_shell, cx| {
                    settle(cx);
                    cx.notify();
                });
            }
            let result: Result<cypher_proto::SessionForkResponse, cypher_rpc::RpcError> = value
                .and_then(|v| {
                    serde_json::from_value(v)
                        .map_err(|e| cypher_rpc::RpcError::BadParams(e.to_string()))
                });
            // The (source, anchor) → requestId mapping is RETAINED on errors /
            // lost replies (the retry must reuse the SAME target id so the
            // engine returns the created chat) and DROPPED on a definitive
            // reply (Created / typed Unavailable) — the settle arms below
            // honor `retained`.
            let retained = Self::fork_request_id_retained(&result);
            let response = match result {
                Ok(response) => response,
                Err(err) => {
                    // `retained` is true here — nothing removes the mapping.
                    tracing::warn!(%chat_id, error = %err, "ForkSession failed");
                    let notice = Self::fork_session_error_text(&err);
                    crate::notify::post("Fork", &notice);
                    if let Some(shell) = weak.upgrade() {
                        shell.update(cx, |shell, cx| {
                            shell.sidebar_notice = Some(notice.clone().into());
                            cx.notify();
                        });
                    }
                    return;
                }
            };
            match response {
                cypher_proto::SessionForkResponse::Created(created) => {
                    // Insert the new chat first: the fork tab's context
                    // mirrors main's list when it is created.
                    let fork_id = created.chat.id.clone();
                    let title = created
                        .chat
                        .title
                        .clone()
                        .unwrap_or_else(|| "Fork".to_string());
                    state.update(cx, |state, cx| {
                        state.insert_chat_optimistic(created.chat.clone());
                        cx.notify();
                    });
                    if let Some(shell) = weak.upgrade() {
                        shell.update(cx, |shell, cx| {
                            // Definitive reply: this fork is settled, drop the
                            // idempotence mapping (a fresh future fork mints a
                            // fresh id). Errors/lost replies retain it.
                            if !retained {
                                shell
                                    .fork_request_ids
                                    .remove(&(chat_id.clone(), anchor_message_id.clone()));
                            }
                            // Its row may trail the next chats frame.
                            shell.expect_chat(&fork_id);
                            // The fork opens as a tab in the source tab's
                            // group, its prefill seeded into the new tile's
                            // composer. Focused only when the user is still
                            // on the source — a late reply (user moved on)
                            // opens it in the background, never yanks focus.
                            let source = crate::workspace::TabKey::session(chat_id.clone());
                            let fork = crate::workspace::TabKey::session(fork_id.clone());
                            let on_source = shell.workspace.focused_tab() == Some(&source);
                            let before = shell.workspace.focused();
                            let group = shell
                                .workspace
                                .find(&source)
                                .map(|(group, _)| group)
                                .unwrap_or(before);
                            let shown = shell
                                .workspace
                                .group(group)
                                .and_then(|g| g.active_tab().cloned());
                            shell.workspace.open_in(group, fork.clone());
                            if !on_source {
                                // Background: the group keeps showing what it
                                // showed, and focus stays where it was.
                                if let Some((g, index)) =
                                    shown.and_then(|tab| shell.workspace.find(&tab))
                                {
                                    shell.workspace.activate(g, index);
                                }
                                shell.workspace.focus(before);
                            } else {
                                shell.focus_pending = true;
                            }
                            shell.sync_slots(cx);
                            if let Some(text) = created.composer_text {
                                match shell
                                    .slot_for_tab(&fork)
                                    .and_then(|sid| shell.slots.get(&sid))
                                    .map(|slot| slot.composer.clone())
                                {
                                    Some(composer) => composer.update(cx, |composer, cx| {
                                        composer.seed_draft(&fork_id, text, cx);
                                    }),
                                    // A background tab gets its slot when
                                    // first shown; the prefill waits with
                                    // the closed-tab drafts.
                                    None => {
                                        shell
                                            .closed_drafts
                                            .insert(fork_id.clone(), (text, Vec::new()));
                                    }
                                }
                            }
                            let notice = format!("Fork created: {title}");
                            crate::notify::post("Fork", &notice);
                            shell.workspace_changed(cx);
                        });
                    }
                }
                cypher_proto::SessionForkResponse::Unavailable(unavailable) => {
                    // Definitive refusal: drop the idempotence mapping too.
                    if let Some(shell) = weak.upgrade() {
                        shell.update(cx, |shell, cx| {
                            if !retained {
                                shell
                                    .fork_request_ids
                                    .remove(&(chat_id.clone(), anchor_message_id.clone()));
                            }
                            cx.notify();
                        });
                    }
                    let notice = format!("Fork unavailable: {}", unavailable.message);
                    crate::notify::post("Fork", &notice);
                    if let Some(shell) = weak.upgrade() {
                        shell.update(cx, |shell, cx| {
                            shell.sidebar_notice = Some(notice.clone().into());
                            cx.notify();
                        });
                    }
                }
            }
        })
        .detach();
    }

    /// User-facing notice text for a failed `RewindSession` RPC, mirroring
    /// [`Self::fork_session_error_text`]: an `unknown method` reply means the
    /// device hosting the chat runs an engine without Session Rewind.
    pub(super) fn rewind_session_error_text(err: &cypher_rpc::RpcError) -> String {
        if let cypher_rpc::RpcError::UnknownMethod(method) = err
            && method == methods::REWIND_SESSION
        {
            "Restarting a conversation from a message requires a newer Cypher \
             engine on the device hosting this session. Update that device or \
             use a session hosted on this device."
                .to_string()
        } else {
            format!("Could not restart the conversation: {err}")
        }
    }

    /// `RewindSession` for a settled transcript entry: restart the
    /// conversation at that anchor INSIDE the same chat — the engine deletes
    /// everything after the boundary and re-points this chat's Pi session at
    /// a truncated copy. No chat is created and the selection never moves;
    /// the transcript shrinks through the doc watch the UI is already on.
    ///
    /// The confirming click happened in the transcript (the affordance arms
    /// first), so this call is the point of no return. A USER anchor hands
    /// its text back for the composer — seeded only when the composer is
    /// empty, so a draft in progress is never clobbered.
    ///
    /// Deliberately NOT retried under an idempotence key: a lost reply leaves
    /// the engine's truncation in place (the doc watch shows it), and a blind
    /// retry would cut at the next boundary instead.
    pub(super) fn rewind_session(
        &mut self,
        sid: SlotId,
        chat_id: String,
        anchor_message_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some((slot_transcript, composer)) = self
            .slots
            .get(&sid)
            .map(|s| (s.transcript.clone(), s.composer.clone()))
        else {
            return;
        };
        slot_transcript.update(cx, |t, cx| {
            t.begin_rewind(chat_id.clone(), anchor_message_id.clone());
            cx.notify();
        });
        let settle = {
            let transcript = slot_transcript.clone();
            let chat_id = chat_id.clone();
            let anchor_message_id = anchor_message_id.clone();
            move |cx: &mut Context<Shell>| {
                transcript.update(cx, |t, cx| {
                    t.end_rewind(chat_id.clone(), anchor_message_id.clone());
                    cx.notify();
                });
            }
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            tracing::warn!(%chat_id, "RewindSession skipped: engine offline");
            let notice = "Cannot restart the conversation: the engine is not connected.";
            crate::notify::post("Restart", notice);
            self.sidebar_notice = Some(notice.into());
            settle(cx);
            cx.notify();
            return;
        };
        let mut params = serde_json::Map::new();
        params.insert("chatId".into(), serde_json::Value::String(chat_id.clone()));
        params.insert(
            "anchorMessageId".into(),
            serde_json::Value::String(anchor_message_id.clone()),
        );
        {
            let state = self.state.read(cx);
            if let (Some(chat), Some(local)) = (
                state.chats.iter().find(|c| c.id == chat_id),
                state.local_device_id.clone(),
            ) && chat.device_id != local
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(chat.device_id.clone()),
                );
            }
        }
        let params = serde_json::Value::Object(params);
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let value = engine.client().call(methods::REWIND_SESSION, params).await;
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |_shell, cx| {
                    settle(cx);
                    cx.notify();
                });
            }
            let result: Result<cypher_proto::SessionRewindResponse, cypher_rpc::RpcError> = value
                .and_then(|v| {
                    serde_json::from_value(v)
                        .map_err(|e| cypher_rpc::RpcError::BadParams(e.to_string()))
                });
            let notice = match result {
                Ok(cypher_proto::SessionRewindResponse::Rewound(rewound)) => {
                    if let Some(text) = rewound.composer_text {
                        composer.update(cx, |composer, cx| {
                            if composer.current_draft(cx).trim().is_empty() {
                                composer.seed_draft(&chat_id, text, cx);
                            }
                        });
                    }
                    let removed = rewound.removed_message_ids.len();
                    if removed == 1 {
                        "Conversation restarted — 1 message removed".to_string()
                    } else {
                        format!("Conversation restarted — {removed} messages removed")
                    }
                }
                Ok(cypher_proto::SessionRewindResponse::Unavailable(unavailable)) => {
                    format!("Cannot restart here: {}", unavailable.message)
                }
                Err(err) => {
                    tracing::warn!(%chat_id, error = %err, "RewindSession failed");
                    Self::rewind_session_error_text(&err)
                }
            };
            crate::notify::post("Restart", &notice);
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |shell, cx| {
                    shell.sidebar_notice = Some(notice.clone().into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// `StartSideChat` for a settled selection: mint the temporary chat on
    /// the engine (relay-forwarded when the parent chat is remote — the
    /// parent's host device owns the side chat), then open its right-pane
    /// tab. `selected_text` is the settled quote IN FULL (the engine
    /// validates it — empty/oversized are rejected there — and injects it
    /// into the first send). Capped at [`MAX_SIDE_CHATS_PER_CHAT`] per chat
    /// as a UX guard (the ENGINE enforces the global cap authoritatively).
    /// Start/cap/offline failures surface as a desktop notice AND the
    /// in-app sidebar notice strip, never a silent return.
    pub(super) fn open_side_chat(
        &mut self,
        sid: SlotId,
        parent_chat_id: String,
        source: cypher_proto::SideChatSource,
        selected_text: String,
        origin: Option<cypher_proto::agent_prompt::AgentQuote>,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.slots.get(&sid) else {
            return;
        };
        let open = slot
            .dock
            .surfaces
            .iter()
            .filter(|s| matches!(s, DockSurface::SideChat(_)))
            .count();
        if open >= MAX_SIDE_CHATS_PER_CHAT {
            tracing::warn!(%parent_chat_id, "Side Chat tab cap reached per chat");
            let notice = "Too many side chats open for this chat (max 8).";
            crate::notify::post("Side Chat", notice);
            self.sidebar_notice = Some(notice.into());
            cx.notify();
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            tracing::warn!(%parent_chat_id, "StartSideChat skipped: engine offline");
            let notice = "Cannot open a side chat: engine is not connected.";
            crate::notify::post("Side Chat", notice);
            self.sidebar_notice = Some(notice.into());
            cx.notify();
            return;
        };
        // Remote parent: the side chat is owned by the PARENT'S host device
        // (relay-forwarded). The reply's `targetDeviceId` then rides every
        // subsequent side-chat RPC.
        let mut params = serde_json::Map::new();
        params.insert(
            "parentChatId".into(),
            serde_json::Value::String(parent_chat_id.clone()),
        );
        params.insert(
            "source".into(),
            serde_json::to_value(&source).unwrap_or_default(),
        );
        params.insert(
            "selectedText".into(),
            serde_json::Value::String(selected_text.clone()),
        );
        // What the selection stands for in the agent's own words, when it
        // was taken from a displayed translation.
        if let Some(origin) = &origin {
            params.insert(
                "origin".into(),
                serde_json::to_value(origin).unwrap_or_default(),
            );
        }
        {
            let state = self.state.read(cx);
            if let (Some(chat), Some(local)) = (
                state.chats.iter().find(|c| c.id == parent_chat_id),
                state.local_device_id.clone(),
            ) && chat.device_id != local
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(chat.device_id.clone()),
                );
            }
        }
        let params = serde_json::Value::Object(params);
        let weak = cx.weak_entity();
        let quote = selected_text;
        cx.spawn(async move |_this, cx| {
            let value = engine.client().call(methods::START_SIDE_CHAT, params).await;
            let created: cypher_proto::SideChatCreated = match value.and_then(|v| {
                serde_json::from_value(v)
                    .map_err(|e| cypher_rpc::RpcError::BadParams(e.to_string()))
            }) {
                Ok(created) => created,
                Err(err) => {
                    tracing::warn!(%parent_chat_id, error = %err, "StartSideChat failed");
                    let notice = Self::side_chat_start_error_text(&err);
                    crate::notify::post("Side Chat", &notice);
                    if let Some(shell) = weak.upgrade() {
                        shell.update(cx, |shell, cx| {
                            shell.sidebar_notice = Some(notice.clone().into());
                            cx.notify();
                        });
                    }
                    return;
                }
            };
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |shell, cx| {
                    shell.register_side_chat_tab(sid, created, source.clone(), quote.clone(), cx)
                });
            }
        })
        .detach();
    }

    /// A successful `StartSideChat` lands here: build the panel, subscribe to
    /// its events, and open the tab.
    ///
    /// Race guard (round-21 audit): the START round-trip may outlive the
    /// parent's tile (closed, or its context moved to another chat). A side
    /// chat belongs to its source parent's dock — attaching it anywhere else
    /// would mis-scope the tab, so the created temp is disposed immediately
    /// and nothing is opened.
    fn register_side_chat_tab(
        &mut self,
        sid: SlotId,
        created: cypher_proto::SideChatCreated,
        source: cypher_proto::SideChatSource,
        selected_text: String,
        cx: &mut Context<Self>,
    ) {
        let parent_chat_id = created.parent_chat_id.clone();
        let slot_chat = self
            .slots
            .get(&sid)
            .and_then(|slot| slot.state.read(cx).selected_chat.clone());
        if slot_chat.as_deref() != Some(parent_chat_id.as_str()) {
            tracing::warn!(
                parent = %parent_chat_id,
                switched_to = ?slot_chat,
                side_chat = %created.side_chat_id,
                "StartSideChat returned after the user switched away; disposing the temp"
            );
            let mut params = serde_json::Map::new();
            params.insert(
                "sideChatId".into(),
                serde_json::Value::String(created.side_chat_id.clone()),
            );
            let state = self.state.read(cx);
            if let Some(local) = state.local_device_id.clone()
                && created.target_device_id != local
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(created.target_device_id.clone()),
                );
            }
            let engine = self.state.read(cx).engine().cloned();
            if let Some(engine) = engine {
                cx.spawn(async move |_, _| {
                    let _ = engine
                        .client()
                        .call(
                            methods::DISPOSE_SIDE_CHAT,
                            serde_json::Value::Object(params),
                        )
                        .await;
                })
                .detach();
            }
            return;
        }
        let Some(slot_state) = self.slots.get(&sid).map(|slot| slot.state.clone()) else {
            return;
        };
        let panel = cx.new(|cx| {
            crate::side_chats::SideChatPanel::new(
                slot_state,
                parent_chat_id.clone(),
                created.side_chat_id.clone(),
                created.target_device_id.clone(),
                source,
                selected_text,
                cx,
            )
        });
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        slot.side_chat_seq += 1;
        let id = slot.side_chat_seq;
        let sub = cx.subscribe(&panel, move |this: &mut Self, _, event, cx| match event {
            crate::side_chats::SideChatEvent::Promoted {
                chat_id,
                side_chat_id,
            } => {
                this.promote_side_chat(sid, chat_id.clone(), side_chat_id.clone(), cx);
            }
            crate::side_chats::SideChatEvent::Close { side_chat_id } => {
                this.close_side_chat_tab(sid, side_chat_id.clone(), cx);
            }
        });
        slot.side_chats.insert(id, panel);
        slot.side_chat_subs.insert(id, sub);
        slot.dock.surfaces.push(DockSurface::SideChat(id));
        self.set_dock_active(sid, DockSurface::SideChat(id), cx);
        // Opening a side chat implies the dock is showing it — at its NORMAL
        // width (never a takeover/expanded dock: the conversation stays
        // visible beside the side chat).
        if let Some(slot) = self.slots.get_mut(&sid) {
            slot.dock.expanded = false;
            slot.dock.open = true;
        }
        cx.notify();
    }

    /// Expand: the side chat is now a normal root chat. Capture the panel's
    /// fork + composer + parent BEFORE the tab closes (close drops the
    /// panel) — the promoted row inherits the parent's device/space/cwd/
    /// config, and the fork's transcript/echoes/draft/staged attachments
    /// ride into the promoted chat's NEW TAB (same group as the parent's)
    /// so the switch is seamless (no blank flash, no lost draft). Promotion
    /// only opens the tab AFTER the RPC succeeded — this handler runs on
    /// that.
    fn promote_side_chat(
        &mut self,
        sid: SlotId,
        chat_id: String,
        side_chat_id: String,
        cx: &mut Context<Self>,
    ) {
        let handoff = self
            .slots
            .get(&sid)
            .and_then(|slot| {
                slot.side_chats
                    .values()
                    .find(|p| p.read(cx).side_chat_id == side_chat_id)
            })
            .map(|p| {
                let panel = p.read(cx);
                let parent_chat_id = panel.parent_chat_id().to_string();
                let fork = panel.fork();
                let composer = panel.composer();
                let fork_transcript = fork.read(cx).transcript.clone();
                let fork_echoes = fork.read(cx).pending_echoes().to_vec();
                let draft = composer.read(cx).current_draft(cx);
                let staged = composer.read(cx).staged_attachments();
                let config = panel.picked_config(cx);
                (
                    parent_chat_id,
                    fork_transcript,
                    fork_echoes,
                    draft,
                    staged,
                    config,
                )
            });
        self.close_side_chat_tab(sid, side_chat_id, cx);
        let (parent_chat_id, fork_transcript, fork_echoes, draft, staged, picked_config) =
            handoff.unwrap_or_default();
        // Optimistic insert: the engine already created the row
        // (PromoteSideChat is synchronous engine-side), so the sidebar
        // renders and the new tab's context resolves it before the next
        // chats frame replaces it with the authoritative row.
        self.state.update(cx, |s, cx| {
            if let Some(parent) = s.chats.iter().find(|c| c.id == parent_chat_id).cloned()
                && !s.chats.iter().any(|c| c.id == chat_id)
            {
                s.insert_chat_optimistic(cypher_proto::Chat {
                    pinned: false,
                    id: chat_id.clone(),
                    device_id: parent.device_id.clone(),
                    title: None,
                    archived: false,
                    cwd: parent.cwd.clone(),
                    branch: parent.branch.clone(),
                    checkout_id: parent.checkout_id.clone(),
                    // The side chat's own model/traits picks (persisted by
                    // the panel's promote), else the inherited config.
                    config: picked_config.or_else(|| parent.config.clone()),
                    last_message_preview: None,
                    last_message_at: None,
                    created_at: chrono::Utc::now(),
                    harness_session_id: None,
                    harness_session_cwd: None,
                    space_id: parent.space_id.clone(),
                    last_seen_at: None,
                    room_gen: Some(2),
                    child: None,
                });
                cx.notify();
            }
        });
        // Its row may trail the next chats frame.
        self.expect_chat(&chat_id);
        let tab = crate::workspace::TabKey::session(chat_id.clone());
        let group = self
            .slots
            .get(&sid)
            .and_then(|slot| self.workspace.find(&slot.tab))
            .map(|(group, _)| group)
            .unwrap_or(self.workspace.focused());
        self.workspace.open_in(group, tab.clone());
        self.focus_pending = true;
        self.sync_slots(cx);
        if let Some(slot) = self.slot_for_tab(&tab).and_then(|id| self.slots.get(&id)) {
            // Seed the new tile's transcript from the fork so there is no
            // blank flash while the promoted chat's doc watch reset lands
            // (same content — the doc watch diff is a no-op), and carry any
            // unconfirmed optimistic echoes over.
            slot.state.update(cx, |s, cx| {
                s.set_transcript(fork_transcript);
                for echo in fork_echoes {
                    s.push_echo(&chat_id, echo);
                }
                cx.notify();
            });
            // Hand the side composer's unsent draft + staged attachments off
            // to the new tile's composer (keyed by the promoted chat id).
            slot.composer.update(cx, |composer, cx| {
                composer.seed_draft(&chat_id, draft, cx);
                composer.seed_attachments(&chat_id, staged, cx);
            });
        }
        self.workspace_changed(cx);
    }

    /// Remove a side chat tab by its panel's side-chat id (event path):
    /// dispose (no-op after promotion) and drop the panel (its transcript /
    /// status tasks die with it).
    fn close_side_chat_tab(&mut self, sid: SlotId, side_chat_id: String, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get(&sid) else {
            return;
        };
        if let Some(id) = slot
            .side_chats
            .iter()
            .find(|(_, p)| p.read(cx).side_chat_id == side_chat_id)
            .map(|(id, _)| *id)
        {
            self.close_side_chat_by_seq(sid, id, cx);
        }
    }

    /// Remove a side chat tab by its slot-minted sequence id (tab-strip ✕
    /// path). A side chat lives in its PARENT tile's dock — the slot that
    /// opened it — so a hidden tab (another tab active in that group) is
    /// removed from that dock, never from whichever tile is focused.
    pub(super) fn close_side_chat_by_seq(&mut self, sid: SlotId, id: u64, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        let Some(panel) = slot.side_chats.remove(&id) else {
            return;
        };
        slot.side_chat_subs.remove(&id);
        slot.dock
            .surfaces
            .retain(|s| *s != DockSurface::SideChat(id));
        if slot.dock.active == DockSurface::SideChat(id) {
            slot.dock.active = DockSurface::Picker;
        }
        panel.update(cx, |panel, cx| panel.dispose(cx));
        cx.notify();
    }

    fn terminal_panel(
        &mut self,
        sid: SlotId,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalPanel>> {
        let popup = self.comment_popup.clone().downgrade();
        let slot = self.slots.get_mut(&sid)?;
        if let Some(terminal) = &slot.terminal {
            return Some(terminal.clone());
        }
        let state = slot.state.clone();
        let terminal = cx.new(|cx| TerminalPanel::new(state, popup, cx));
        slot.terminal = Some(terminal.clone());
        Some(terminal)
    }

    /// The dock's resting height: the session's fraction of its area (the
    /// legacy global height if never dragged), clamped to this tile's
    /// session area (a short tile never loses its composer to it).
    fn terminal_height(&self, slot: &SessionSlot) -> f32 {
        let area_h = f32::from(slot.area.get().size.height);
        let height = terminal_dock_height(
            slot.terminal_fraction,
            self.settings.terminal_height,
            area_h,
        );
        super::dock::fit_terminal_height(height, area_h, slot.bottom_stack.get())
    }

    fn terminal_target(&self, slot: &SessionSlot) -> f32 {
        if slot.terminal_open {
            self.terminal_height(slot)
        } else {
            0.0
        }
    }

    /// Cmd/Ctrl+J and the header button (feature-inventory §1.10). Height
    /// animates 200 ms; closing detaches (PTYs stay alive), opening restores.
    /// The flag is per session tile (zeron `sessionPanels`).
    pub(super) fn toggle_terminal(
        &mut self,
        sid: SlotId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.slots.get(&sid) else {
            return;
        };
        let from = self.terminal_target(slot);
        let Some(panel) = self.terminal_panel(sid, cx) else {
            return;
        };
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        slot.terminal_open = !slot.terminal_open;
        let open = slot.terminal_open;
        let composer = slot.composer.clone();
        let to = self
            .slots
            .get(&sid)
            .map_or(0.0, |slot| self.terminal_target(slot));
        if let Some(slot) = self.slots.get_mut(&sid) {
            slot.terminal_tween = Some(WidthTween::new(from, to));
        }
        panel.update(cx, |panel, cx| panel.set_open(open, cx));
        if open {
            // Opening lands keyboard focus IN the shell — typing goes straight
            // to the prompt, no click needed (zeron terminal-panel.tsx: the
            // visible+active effect calls `terminal.focus()` on every open).
            // The handle is focusable before the panel's first paint; once the
            // terminal body mounts with `track_focus` it receives the keys.
            window.focus(&panel.read(cx).focus_handle(), cx);
        } else {
            // Hiding the panel removes the (likely focused) terminal view;
            // with nothing focused, window key bindings stop dispatching, so
            // hand focus to the composer. (Cmd+J is a pure toggle — a second
            // press closes even while the terminal is focused, as in zeron's
            // `useHotkey(toggleShortcut, ... setOpenScoped(!open))`.)
            window.focus(&composer.focus_handle(cx), cx);
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(RESIZE.total().mul_f32(motion::speed_scale()) + Duration::from_millis(30))
                .await;
            this.update(cx, |shell, cx| {
                if let Some(slot) = shell.slots.get_mut(&sid) {
                    slot.terminal_tween = None;
                }
                cx.notify();
            })
            .ok();
        });
        if let Some(slot) = self.slots.get_mut(&sid) {
            slot.terminal_tween_task = Some(task);
        }
        self.remember_slot_docks(sid, cx);
        cx.notify();
    }

    pub(super) fn on_terminal_drag(
        &mut self,
        event: &gpui::DragMoveEvent<TerminalResize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sid = event.drag(cx).0;
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        let Some((anchor_y, anchor_h)) = slot.terminal_drag_anchor else {
            return;
        };
        let dy = anchor_y - f32::from(event.event.position.y);
        let area_h = f32::from(slot.area.get().size.height);
        slot.terminal_tween = None; // live drag tracks the pointer
        let height = clamp_terminal_height(anchor_h + dy, area_h);
        slot.terminal_fraction = super::dock::fraction_of(height, area_h);
        self.remember_slot_docks(sid, cx);
        cx.notify();
    }

    /// A tile's chat column (the old main outlet): transcript underlay with
    /// its edge fade, find bar, jump pill, status strip and composer — all
    /// bound to the slot's session. `chat_height` is the column's height
    /// (the session area above the terminal dock).
    fn render_session_chat(
        &mut self,
        sid: SlotId,
        chat_height: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme_owned = crate::chat_style::theme(cx);
        let theme = &theme_owned;
        let faint = theme.text_faint;
        let Some(slot) = self.slots.get(&sid) else {
            return Empty.into_any_element();
        };
        let (transcript, composer, slot_state) = (
            slot.transcript.clone(),
            slot.composer.clone(),
            slot.state.clone(),
        );
        let has_selection = slot_state.read(cx).selected_chat.is_some();
        let has_spaces = !slot_state.read(cx).spaces.is_empty();
        // The measured status strip + composer stack floating over the
        // column's bottom (last frame's — see `bottom_stack`).
        let measured = slot.bottom_stack.clone();
        let stack_h = measured.get();
        // Canvas content centers in the room ABOVE that stack (never behind
        // the composer); short tiles shed the helper line, then the
        // wordmark.
        let canvas_room = chat_height - stack_h;
        let show_wordmark = canvas_room >= 110.0;
        let show_helper = canvas_room >= 150.0;
        let space_name: SharedString = slot_state
            .read(cx)
            .selected_space_row()
            .map(|s| s.display_name().to_string())
            .unwrap_or_default()
            .into();

        // Content outlet: selected chat → transcript; nothing selected → the
        // "Send a message to start" canvas with a watermark; no spaces at all
        // → the onboarding card. The composer sits below the first two
        // (new-chat mode mints the chat id on first send).
        let outlet: AnyElement = if has_selection {
            transcript.clone().into_any_element()
        } else if !has_spaces {
            // Onboarding (first boot / after the destructive wipe / a
            // project-less opt-out with nothing to work in): no folders to
            // target yet — one clear affordance, regardless of `no_project`
            // (an unselected canvas with zero spaces has nothing to send to).
            let _ = faint;
            div()
                .size_full()
                .pb(px(stack_h))
                .overflow_hidden()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(motion::fade_in(
                    "no-spaces-canvas",
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .child(
                            div()
                                .font_family(theme.font_sans.clone())
                                .text_size(px(21.0))
                                .font_weight(gpui::FontWeight::NORMAL)
                                .text_color(theme.text.opacity(0.18))
                                .child(SharedString::from("Let's Cypher")),
                        )
                        .child(
                            div()
                                .mt(px(24.0))
                                .text_size(px(16.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(SharedString::from("Add a project to get started")),
                        )
                        .child(
                            div()
                                .mt(px(6.0))
                                .text_size(px(13.0))
                                .text_color(theme.text_muted.opacity(0.7))
                                .child(SharedString::from(
                                    "A project is a folder on one of your devices.",
                                )),
                        )
                        .child(
                            popover::btn_primary(&theme_owned, "Add a project")
                                .id("onboarding-add-space")
                                .mt(px(20.0))
                                .on_click(cx.listener(|this, _, _, cx| this.open_add_space(cx))),
                        ),
                ))
                .into_any_element()
        } else {
            // New-chat canvas: the Cypher wordmark over the target selectors
            // (device + project — moved up from the composer footer) and the
            // helper line.
            let helper: SharedString = if space_name.is_empty() {
                "Send a message to start a new session.".into()
            } else {
                format!("Send a message to start a session in {space_name}.").into()
            };
            let pickers = composer.read(cx).pickers().clone();
            let selectors = pickers.update(cx, |p, cx| p.render_target_selectors(cx));
            div()
                .size_full()
                .pb(px(stack_h))
                .overflow_hidden()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(motion::fade_in(
                    "new-chat-canvas",
                    div()
                        .max_w_full()
                        .px(px(12.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .when(show_wordmark, |el| {
                            el.child(
                                div()
                                    .mb(px(16.0))
                                    .font_family(theme.font_sans.clone())
                                    .text_size(px(21.0))
                                    .font_weight(gpui::FontWeight::NORMAL)
                                    .text_color(theme.text.opacity(0.32))
                                    .child(SharedString::from("Let's Cypher")),
                            )
                        })
                        .child(selectors)
                        .when(show_helper, |el| {
                            el.child(
                                div()
                                    .mt(px(12.0))
                                    .text_size(px(14.0))
                                    .text_center()
                                    .text_color(theme.text_muted.opacity(0.6))
                                    .child(helper),
                            )
                        }),
                ))
                .into_any_element()
        };

        let status = self.render_status_strip(sid, cx);
        // File dropzone over the ENTIRE conversation column (transcript +
        // composer, not just the pill): dragging OS files anywhere across the
        // chat area shows the "Drop images to attach" veil; a drop stages the
        // files in the composer. `has_active_drag` gates the veil so a drag
        // that left the window (FileDrop Exited) can't strand it.
        let Some(slot) = self.slots.get(&sid) else {
            return Empty.into_any_element();
        };
        let file_drag_active = slot.file_drag_active && cx.has_active_drag();
        div()
            .id("chat-dropzone")
            // Background belongs to the rounded outer card. A rectangular
            // child fill would cover its corners despite overflow_hidden.
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            // The terminal dock yields to the composer stack
            // (`fit_terminal_height`); only an area shorter than the stack
            // itself clips it — from the top, so the input row and buttons
            // stay on screen and nothing paints over the dock below.
            .justify_end()
            .overflow_hidden()
            .on_drag_move::<gpui::ExternalPaths>(cx.listener(
                move |this, e: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                    let inside = e.bounds.contains(&e.event.position);
                    if let Some(slot) = this.slots.get_mut(&sid)
                        && slot.file_drag_active != inside
                    {
                        slot.file_drag_active = inside;
                        cx.notify();
                    }
                },
            ))
            .on_drop(
                cx.listener(move |this, paths: &gpui::ExternalPaths, _, cx| {
                    let Some(slot) = this.slots.get_mut(&sid) else {
                        return;
                    };
                    slot.file_drag_active = false;
                    let paths = paths.paths().to_vec();
                    slot.composer
                        .update(cx, |composer, cx| composer.add_paths(paths, cx));
                    cx.notify();
                }),
            )
            .child(
                // Full-height underlay: the transcript viewport spans the
                // whole column, scrolling under a small top band and the
                // composer stack below. The per-glyph EdgeFade (glass-safe,
                // same as the sidebar's) spans the full column with
                // ASYMMETRIC bands sized to the chrome: content is opaque at
                // the chrome's inner edge and fades to zero at the window
                // edge — visible mid-fade through the glass chrome it slides
                // under. Always on (the resting paddings keep pinned content
                // out of the bands, and gating on measured scroll state left
                // the top unfaded for one frame on session switch — user
                // report). The jump pill floats outside the fade scope,
                // anchored above the measured stack.
                {
                    // The terminal dock lives below the whole chat column
                    // (see `render_session`), so only the status strip +
                    // composer overlap the transcript. Opaque from the
                    // composer PILL's top (the reserved status strip above
                    // it is empty air), zero at the underlay's bottom edge.
                    let bottom_band = (stack_h - Theme::STATUS_STRIP_HEIGHT).max(1.0);
                    div()
                        .absolute()
                        .inset_0()
                        .child(
                            crate::edge_fade::edge_faded(
                                Theme::TRANSCRIPT_FADE_BAND,
                                true,
                                true,
                                div().size_full().child(outlet),
                            )
                            // The tile header is a normal row above the
                            // viewport: content is fully faded at its
                            // bottom edge and opaque one band below.
                            .band_top(crate::transcript::TOP_CHROME_PX)
                            .band_bottom(bottom_band),
                        )
                        .children(self.render_jump_to_bottom(sid, stack_h, cx))
                        // The find bar floats in the same layer, at the top
                        // — outside the fade scope, over the transcript.
                        .children(self.render_find_bar(sid, window, cx))
                },
            )
            // The glass chrome stack, floating over the transcript's bottom:
            // reserved status strip (h-6, the WorkingIndicator — the composer
            // below never shifts) and composer. A paint-time
            // canvas measures the stack for next frame's fade inset and
            // transcript clearance. The flex_1 spacer has no id/listeners, so
            // pointer + wheel events over it fall through to the list below.
            .child(div().flex_1().min_h_0())
            .child({
                div()
                    .flex_none()
                    .relative()
                    .flex()
                    .flex_col()
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| measured.set(f32::from(bounds.size.height)),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    // The session-level subagents trigger lives INSIDE the
                    // status strip (right edge, next to the composer); the
                    // inspector it opens is a floating layer and never
                    // participates in this stack's measurement.
                    .child(status)
                    // A SELECTED chat keeps its composer even with zero live
                    // spaces (a selected project-less / unavailable-project
                    // chat still needs its input row); an unselected canvas
                    // with no spaces shows onboarding instead, so the
                    // composer only mounts where there is something to send
                    // into.
                    .when(has_selection || has_spaces, |el| el.child(composer.clone()))
            })
            .when(file_drag_active, |el| {
                el.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(12.0))
                        .bg(theme.scrim().opacity(0.4 / 0.6))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(13.0))
                        .text_color(theme.text)
                        .child("Drop images to attach"),
                )
            })
            .into_any_element()
    }

    // ---- in-chat find (⌘F) ----

    /// ⌘F (and Edit → Find in Chat). Opens the find bar over the open
    /// conversation, or — when it is already open — just puts the caret back
    /// in the field with the previous query intact, the way every find bar
    /// behaves. There is nothing to search on the new-chat canvas or in
    /// Settings, so both are no-ops rather than an empty bar.
    pub(super) fn open_find(&mut self, sid: SlotId, cx: &mut Context<Self>) {
        let showing_setup = self.showing_setup();
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        if !matches!(self.route, Route::Chat)
            || showing_setup
            || slot.state.read(cx).selected_chat.is_none()
        {
            return;
        }
        let query = slot.find_input.read(cx).text().to_owned();
        slot.transcript.update(cx, |transcript, cx| {
            transcript.open_find(cx);
            // Re-opening with a retained query must re-run it: the index was
            // dropped when find closed.
            transcript.set_find_query(&query, cx);
        });
        slot.find_focus_pending = true;
        cx.notify();
    }

    /// Close the bar and hand the keyboard back to the composer — where it
    /// was before ⌘F, and the only place in the chat route that wants it.
    fn close_find(&mut self, sid: SlotId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        slot.transcript
            .update(cx, |transcript, cx| transcript.close_find(cx));
        slot.find_focus_pending = false;
        window.focus(&slot.composer.focus_handle(cx), cx);
        cx.notify();
    }

    fn step_find(&mut self, sid: SlotId, delta: isize, cx: &mut Context<Self>) {
        if let Some(slot) = self.slots.get(&sid) {
            slot.transcript
                .update(cx, |transcript, cx| transcript.step_find(delta, cx));
        }
    }

    /// Find-bar keys, bubbling from the focused field ("PaletteSearch" leaves
    /// ↵/⇧↵/↑↓/esc unbound exactly so they arrive here).
    fn find_key(
        &mut self,
        sid: SlotId,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => self.close_find(sid, window, cx),
            "enter" => {
                let delta = if event.keystroke.modifiers.shift {
                    -1
                } else {
                    1
                };
                self.step_find(sid, delta, cx);
            }
            "up" => self.step_find(sid, -1, cx),
            "down" => self.step_find(sid, 1, cx),
            _ => {}
        }
    }

    /// The find bar: a floating pill in the conversation column's top-right,
    /// below the titlebar and clear of the message rail (which hugs the left
    /// edge). Rendered by the SHELL rather than inside the transcript for the
    /// same reason as the jump pill — the transcript outlet sits inside the
    /// EdgeFade scope, which would fade the bar out against the top band.
    fn render_find_bar(
        &mut self,
        sid: SlotId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let slot = self.slots.get_mut(&sid)?;
        if !slot.transcript.read(cx).find_open() {
            return None;
        }
        if std::mem::take(&mut slot.find_focus_pending) {
            let handle = slot.find_input.focus_handle(cx);
            window.focus(&handle, cx);
        }
        let (find_input, find_focus) = (slot.find_input.clone(), slot.find_focus.clone());
        let theme = Theme::of(cx).clone();
        let (position, total) = slot.transcript.read(cx).find_status();
        let typed = !find_input.read(cx).text().trim().is_empty();
        // Empty field reads as "nothing asked for yet", not "nothing found".
        let counter: SharedString = match (typed, total) {
            (false, _) => "".into(),
            (true, 0) => "No results".into(),
            (true, total) => format!("{position} of {total}").into(),
        };
        let has_matches = total > 0;
        let step_button = |shell_key: &'static str,
                           glyph: &'static str,
                           tooltip: &'static str,
                           delta: isize,
                           cx: &mut Context<Self>| {
            div()
                .id(shell_key)
                .size(px(22.0))
                .flex_none()
                .rounded(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .when(has_matches, |el| {
                    // Hover-fade keys are global: suffix the slot so two
                    // tiles' find bars never fade together.
                    let fade_key = format!("{shell_key}-{sid}");
                    el.cursor_pointer()
                        .bg(motion::hover_blend(
                            &fade_key,
                            gpui::transparent_black(),
                            theme.element_hover,
                        ))
                        .on_hover(motion::hover_listener(fade_key))
                        .on_click(cx.listener(move |this, _, _, cx| this.step_find(sid, delta, cx)))
                })
                .child(
                    icon(glyph)
                        .size(px(12.0))
                        .text_color(if has_matches {
                            theme.text_muted
                        } else {
                            theme.text_faint.opacity(0.5)
                        })
                        .flex_none(),
                )
                .tooltip(move |_, cx| cx.new(|_| FindTooltip(tooltip.into())).into())
        };
        let bar = div()
            .id("find-bar")
            .h(px(34.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pl(px(10.0))
            .pr(px(5.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .shadow_md()
            .track_focus(&find_focus)
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    this.find_key(sid, event, window, cx)
                }),
            )
            .child(
                icon(icons::MAGNIFER)
                    .size(px(13.0))
                    .flex_none()
                    .text_color(theme.text_faint),
            )
            .child(
                div()
                    .w(px(190.0))
                    .flex_none()
                    .text_size(px(13.0))
                    .child(find_input.clone()),
            )
            .child(
                div()
                    // Fixed width so stepping through matches ("9 of 12" →
                    // "10 of 12") never nudges the buttons under the cursor.
                    .w(px(64.0))
                    .flex_none()
                    .text_size(px(11.5))
                    .text_color(theme.text_faint)
                    .truncate()
                    .child(counter),
            )
            .child(div().w(px(1.0)).h(px(16.0)).flex_none().bg(theme.border))
            .child(step_button(
                "find-prev",
                icons::ARROW_UP,
                "Previous match (⇧↵)",
                -1,
                cx,
            ))
            .child(step_button(
                "find-next",
                icons::ARROW_DOWN,
                "Next match (↵)",
                1,
                cx,
            ))
            .child(
                div()
                    .id("find-close")
                    .size(px(22.0))
                    .flex_none()
                    .rounded(px(5.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .bg(motion::hover_blend(
                        &format!("find-close-{sid}"),
                        gpui::transparent_black(),
                        theme.element_hover,
                    ))
                    .on_hover(motion::hover_listener(format!("find-close-{sid}")))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.close_find(sid, window, cx)),
                    )
                    .child(
                        icon(icons::CLOSE)
                            .size(px(11.0))
                            .flex_none()
                            .text_color(theme.text_muted),
                    )
                    .tooltip(|_, cx| cx.new(|_| FindTooltip("Close (esc)".into())).into()),
            );
        Some(
            div()
                .absolute()
                .top(px(8.0))
                .right(px(14.0))
                .child(motion::dialog_in("find-bar-in", bar))
                .into_any_element(),
        )
    }

    /// The "↓ Scroll to bottom" pill (round-9 §3): a LABELED rounded-full
    /// chip — down-arrow glyph + 13px label on a near-opaque raised surface
    /// with a hairline — horizontally centered over the transcript column and
    /// floating a small gap above the composer. It hangs 14px below the
    /// conversation region (through the reserved h-6 status strip, whose
    /// content is left-aligned) so its bottom edge sits ~10px above the pill.
    /// Shown past the transcript's 320px threshold; 180ms fade + 2px rise in.
    /// `stack_h` is the measured bottom chrome stack the full-height
    /// transcript scrolls under — the pill anchors just above it (the -14
    /// carries the old status-strip overlap).
    fn render_jump_to_bottom(
        &mut self,
        sid: SlotId,
        stack_h: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let transcript = self.slots.get(&sid)?.transcript.clone();
        if !transcript.read(cx).jump_button_shown() {
            return None;
        }
        let theme = Theme::of(cx);
        // Per-slot hover-fade key (the fade store is global).
        let jump_key = format!("jump-pill-{sid}");
        Some(
            div()
                .absolute()
                .bottom(px(stack_h - 14.0))
                .left_0()
                .right(px(10.0))
                .flex()
                .justify_center()
                .child(motion::dialog_in(
                    "jump-to-bottom",
                    div()
                        .id("jump-to-bottom-btn")
                        .h(px(30.0))
                        .rounded_full()
                        .border_1()
                        .border_color(theme.border)
                        .shadow_md()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .pl(px(11.0))
                        .pr(px(13.0))
                        .cursor_pointer()
                        // Hover must BRIGHTEN the opaque pill, never replace it
                        // with a translucent wash (a 10%-alpha bg here made the
                        // pill go see-through on hover — user-reported), and it
                        // fades over the CSS transition-colors 150ms, not snaps.
                        .bg(motion::hover_blend(
                            &jump_key,
                            theme.surface_raised,
                            theme.surface_raised_hover,
                        ))
                        .on_hover(motion::hover_listener(jump_key.clone()))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            transcript.update(cx, |transcript, cx| transcript.jump_to_bottom(cx));
                        }))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from("↓")),
                        )
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(theme.text)
                                .child(SharedString::from("Scroll to bottom")),
                        ),
                ))
                .into_any_element(),
        )
    }

    /// Terminal panel dock along the session area's bottom (under the chat
    /// column AND the right dock): a 5px height-drag handle
    /// over the panel, the whole container height-animated 200 ms on toggle.
    fn render_terminal_container(&mut self, sid: SlotId, cx: &mut Context<Self>) -> AnyElement {
        let Some(slot) = self.slots.get(&sid) else {
            return gpui::Empty.into_any_element();
        };
        let target = self.terminal_target(slot);
        let tween = slot.terminal_tween;
        if target <= 0.0 && tween.is_none() {
            return gpui::Empty.into_any_element();
        }
        // Defensive: an open flag needs its entity (and set_open) even if
        // toggle_terminal never created one.
        if slot.terminal_open
            && slot.terminal.is_none()
            && let Some(panel) = self.terminal_panel(sid, cx)
        {
            panel.update(cx, |panel, cx| panel.set_open(true, cx));
        }
        let Some(slot) = self.slots.get(&sid) else {
            return gpui::Empty.into_any_element();
        };
        let Some(panel) = slot.terminal.clone() else {
            return gpui::Empty.into_any_element();
        };
        let handle_hover = Theme::of(cx).border_strong;
        let height = self.terminal_height(slot);

        let handle = div()
            .id("terminal-resize")
            .h(px(5.0))
            .w_full()
            .flex_none()
            .cursor_row_resize()
            .hover(move |s| s.bg(handle_hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, _| {
                    let Some(height) = this.slots.get(&sid).map(|s| this.terminal_height(s)) else {
                        return;
                    };
                    if let Some(slot) = this.slots.get_mut(&sid) {
                        slot.terminal_drag_anchor = Some((f32::from(event.position.y), height));
                    }
                }),
            )
            .on_drag(
                TerminalResize(sid),
                |_, _point: Point<gpui::Pixels>, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| DragGhost)
                },
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                    if event.click_count == 2
                        && let Some(slot) = this.slots.get_mut(&sid)
                    {
                        let area_h = f32::from(slot.area.get().size.height);
                        slot.terminal_fraction =
                            super::dock::fraction_of(TERMINAL_DEFAULT_HEIGHT, area_h);
                        this.remember_slot_docks(sid, cx);
                        cx.notify();
                    }
                }),
            );

        // Fixed-height inner clipped by the animated container: content never
        // reflows mid-transition (same trick as the side panes). The handle
        // FLOATS over the panel's top edge (painted after, so it wins hit
        // testing) instead of stacking above it — stacked, its 5px read as
        // dead air between the seam and the tab bar (user report).
        let inner = div()
            .h(px(height))
            .w_full()
            .relative()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(panel))
            .child(handle.absolute().top_0().left_0().right_0());

        // A divider line above the terminal separates it from the chat,
        // stopping short of the tile edges; painted after the panel so its
        // fill doesn't cover it.
        div()
            .w_full()
            .flex_none()
            .overflow_hidden()
            .relative()
            .h(px(self.eval_tween(tween, target)))
            .child(inner)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(DIVIDER_INSET))
                    .right(px(DIVIDER_INSET))
                    .h(px(1.0))
                    .bg(Theme::of(cx).border),
            )
            .into_any_element()
    }

    /// Working indicator strip: gradient spinner + rotating flavour word (7s,
    /// seeded per chat) + elapsed, staleness-gated via [`Indicator`]; falls back
    /// to a "Sending…" bridge and then the engine mode line. The strip's right
    /// side hosts the session-level subagents trigger — `[left status][flex
    /// spacer][Subagents accessory]` — so the left status and the right
    /// accessory can coexist. Both outer accessories align with the points
    /// where the composer pill's rounded top corners finish and become flat,
    /// pulling them slightly inward from the pill's outer edges.
    fn render_status_strip(&mut self, sid: SlotId, cx: &mut Context<Self>) -> AnyElement {
        let Some(slot) = self.slots.get(&sid) else {
            return Empty.into_any_element();
        };
        let (slot_state, composer, subagents) = (
            slot.state.clone(),
            slot.composer.clone(),
            slot.subagents.clone(),
        );
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let state = slot_state.read(cx);

        // Aligned with the composer column: the pill starts after the
        // composer's 16px column padding, then its 26px top-corner radius
        // ends. Inset both accessories by their sum so the left/right trigger
        // boundaries land exactly on those corner endpoints.
        let accessory_inset = Theme::SPACE_LG + crate::composer::PILL_RADIUS;
        let wide = crate::chat_style::settings(cx).wide;
        let strip = div()
            .h(px(Theme::STATUS_STRIP_HEIGHT))
            .flex_none()
            .w_full()
            .when(!wide, |el| el.max_w(px(crate::chat_style::COMPOSER_WIDTH)))
            .mx_auto()
            .flex()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .px(px(accessory_inset + if wide { 32.0 } else { 0.0 }))
            .text_size(px(11.0));

        let left: AnyElement = match state.selected_chat.as_deref() {
            None => Empty.into_any_element(),
            Some(chat_id) => {
                let indicator = state.indicator_for(chat_id, now);
                // Timer base: the freshest of the session row's turn start and
                // the in-flight send. During the send→ack window the row (if
                // any) still carries the PREVIOUS turn's start, and using it
                // opened the timer at the old turn's elapsed instead of 0:00.
                let started = state
                    .session_for(chat_id)
                    .and_then(|s| s.started_at)
                    .into_iter()
                    .chain(state.pending_send_started(chat_id, now))
                    .max();
                let _ = started;
                let sending = composer.read(cx).is_sending();

                // Unused here since the Working loader moved into the transcript
                // (its trailer computes its own elapsed).
                match indicator {
                    // The working loader lives in the TRANSCRIPT now, under the
                    // streaming reply (user request) — the strip stays empty
                    // (its reserved height still steadies the composer).
                    Indicator::Working => Empty.into_any_element(),
                    // No label: the QuestionPanel right below IS the
                    // awaiting-input surface — a strip caption above it was
                    // redundant (user request).
                    Indicator::AwaitingInput => Empty.into_any_element(),
                    Indicator::Errored => div()
                        .text_color(theme.danger)
                        .child(SharedString::from("Run failed"))
                        .into_any_element(),
                    Indicator::None if sending => div()
                        .flex()
                        .items_center()
                        .gap(px(Theme::SPACE_SM))
                        .child(loaders::gradient_spinner(
                            "sending-indicator",
                            &theme,
                            2.5,
                            cx.entity_id(),
                            cx,
                        ))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from("Sending…")),
                        )
                        .into_any_element(),
                    Indicator::None => Empty.into_any_element(),
                }
            }
        };

        // Pending-comments indicator pinned to the strip's upper-left; the
        // session status sits centered; the Subagents trigger stays right.
        let comments_trigger =
            composer.update(cx, |composer, cx| composer.render_comments_trigger(cx));
        strip
            .child(comments_trigger)
            .child(div().flex_1())
            .child(left)
            // Flex spacer keeps the center status centered and the subagents
            // trigger pinned to the composer-aligned right edge; when the
            // trigger renders Empty (no records) the strip is just the left
            // content.
            .child(div().flex_1())
            .child(subagents)
            .into_any_element()
    }
}
