//! Session navigation and routing. The activity sidebar IS the session
//! list; a click opens (or focuses) the session as a tab in the workspace
//! (docs/workspace-layout.md): the focused tile, or — ⌘-click — a new split
//! to its right. The main state's selection follows the focused tile
//! (`Shell::sync_follow`). The layout persists as `UiSettings.workspace`
//! (and `project_workspaces` per project window), restored at boot landing.

use super::*;
use crate::workspace::{TabKey, Workspace};

/// A saved layout made fit to restore: session tabs survive only while
/// `live` (the chat exists, isn't archived, and is in this window's scope);
/// new-session canvases are dropped — a canvas holds nothing but an unsent
/// project pick, and the boot landing opens a fresh one when no tab is left.
/// Groups emptied by the pruning collapse; tiles that were already empty
/// stay. Pure.
pub(super) fn restore_workspace(mut saved: Workspace, live: impl Fn(&str) -> bool) -> Workspace {
    saved.retain_tabs(|tab| tab.chat_id().is_some_and(&live));
    saved
}

/// Carry the tabs opened before the first chats frame (⌘N, the titlebar
/// `+`) into the restored layout: each opens in its focused group — a
/// canvas under a fresh key of the restored layout's own, returned as
/// `(old, new)` so its slot follows — and the one that had focus stays
/// focused. Pure.
pub(super) fn adopt_presync_tabs(
    restored: &mut Workspace,
    presync: &Workspace,
) -> Vec<(TabKey, TabKey)> {
    let focused = presync.focused_tab();
    let mut renamed = Vec::new();
    let mut focus = None;
    for tab in presync.tabs() {
        let key = match tab {
            TabKey::NewSession(_) => {
                let fresh = restored.new_session_tab();
                renamed.push((tab.clone(), fresh.clone()));
                fresh
            }
            TabKey::Session(_) => tab.clone(),
        };
        if Some(tab) == focused {
            focus = Some(key.clone());
        }
        restored.open(key);
    }
    if let Some((group, index)) = focus.and_then(|tab| restored.find(&tab)) {
        restored.activate(group, index);
    }
    renamed
}

/// How long a chat this window just created (a fork, a promoted side chat)
/// may be missing from the chats frames before its tab counts as deleted.
pub(super) const EXPECTED_CHAT_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// What the chats list says about an open session (or a parked terminal's
/// chat).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionFate {
    Stays,
    /// Archived, or outside this window's project scope: its tab closes (the
    /// chat may come back — terminals stay parked).
    Hidden,
    /// Gone from the list: tab and parked terminals close.
    Deleted,
}

/// Judge `chat_id` against the synced `chats` list. Pure.
///
/// A listed chat is judged on every call (archive / scope changes need no
/// chats frame). A MISSING chat is judged only on a new chats frame
/// (`chats_frame`): pre-sync lists are empty, and an optimistic insert or an
/// unrelated notify says nothing. Even then a chat that is `awaited` — a
/// canvas's just-minted chat with a live pending send, or a fork /
/// promoted side chat created moments ago — stays: its row just hasn't
/// landed yet.
pub(super) fn session_fate(
    chat_id: &str,
    chats: &[cypher_proto::Chat],
    scope: &crate::state::ProjectScope,
    chats_frame: bool,
    awaited: bool,
) -> SessionFate {
    match chats.iter().find(|c| c.id == chat_id) {
        Some(chat) if chat.archived || !scope.chat_visible(chat) => SessionFate::Hidden,
        Some(_) => SessionFate::Stays,
        None if chats_frame && !awaited => SessionFate::Deleted,
        None => SessionFate::Stays,
    }
}

/// The chat one step from `selected` in the sidebar `order`, wrapping at both
/// ends. Pure.
///
/// `order` is the sidebar's own row order ([`AppState::overview_chats`] — the
/// flat recency list the grouped cards are built from), which already hides
/// archived and child chats and restricts to live spaces.
///
/// With nothing selected — the new-session canvas — cycling enters the list at
/// the end it would have wrapped to: the first row going forward, the last
/// going back. A selection that has since left the list (archived from another
/// device mid-cycle) is treated the same way rather than dead-ending.
pub(super) fn cycle_target(
    order: &[String],
    selected: Option<&str>,
    forward: bool,
) -> Option<String> {
    if order.is_empty() {
        return None;
    }
    let at = selected.and_then(|id| order.iter().position(|c| c == id));
    let next = match (at, forward) {
        (Some(at), true) => (at + 1) % order.len(),
        (Some(at), false) => (at + order.len() - 1) % order.len(),
        (None, true) => 0,
        (None, false) => order.len() - 1,
    };
    Some(order[next].clone())
}

impl Shell {
    /// Boot landing, once the first chats frame has synced: the saved
    /// layout comes back ([`restore_workspace`]), and only when it holds no
    /// tab does the old landing run — the most recently active visible chat
    /// opens as a tab (no chats, or the `CYPHER_OPEN_ROUTE=new` pin → a
    /// new-session tab). Anything the user opened before the frame landed
    /// joins the restored layout, focused ([`adopt_presync_tabs`]), and
    /// replaces the landing. A project window also opens the chat its state
    /// selected (the chat moves windows with it) into its restored layout.
    pub(super) fn boot_select_chat(&mut self, cx: &mut Context<Self>) {
        if self.boot_landed {
            return;
        }
        let (landing, pinned) = {
            let state = self.state.read(cx);
            if !state.chats_synced {
                return;
            }
            if self.is_project_window() {
                (state.selected_chat.clone(), false)
            } else if state.auto_selected {
                // Pinned to the canvas (or the user already picked).
                (None, true)
            } else {
                let latest = state
                    .overview_chats(Utc::now())
                    .first()
                    .map(|(_, c)| c.id.clone());
                (latest, false)
            }
        };
        self.boot_landed = true;
        self.prune_persisted_layout(cx);
        let saved = self.saved_workspace.take();
        let presync = self.workspace.tabs().next().is_some();
        if let Some(saved) = saved.filter(|_| !pinned) {
            let live: std::collections::HashSet<String> = {
                let state = self.state.read(cx);
                let scope = state.project_scope();
                state
                    .chats
                    .iter()
                    .filter(|c| !c.archived && scope.chat_visible(c))
                    .map(|c| c.id.clone())
                    .collect()
            };
            let mut restored = restore_workspace(saved, |id| live.contains(id));
            if presync {
                for (old, new) in adopt_presync_tabs(&mut restored, &self.workspace) {
                    if let Some(slot) = self
                        .slot_for_tab(&old)
                        .and_then(|sid| self.slots.get_mut(&sid))
                    {
                        slot.tab = new;
                    }
                }
            }
            self.workspace = restored;
        }
        if presync {
            self.workspace_changed(cx);
            return;
        }
        let restored = self.workspace.tabs().next().is_some();
        match landing {
            Some(chat_id) if !restored || self.is_project_window() => {
                self.workspace.open(TabKey::Session(chat_id));
            }
            None if !restored => {
                self.open_new_session(cx);
                return;
            }
            _ => {}
        }
        self.focus_pending = true;
        self.workspace_changed(cx);
    }

    /// Main window, at boot: forget the dock sizes of sessions that no
    /// longer exist (archived ones may come back) and the layouts of
    /// projects that are gone.
    fn prune_persisted_layout(&mut self, cx: &mut Context<Self>) {
        if self.is_project_window() {
            return;
        }
        let (chats, spaces) = {
            let state = self.state.read(cx);
            let chats: std::collections::HashSet<String> =
                state.chats.iter().map(|c| c.id.clone()).collect();
            let spaces = state.spaces_synced.then(|| {
                state
                    .spaces
                    .iter()
                    .map(|s| s.id.clone())
                    .collect::<std::collections::HashSet<String>>()
            });
            (chats, spaces)
        };
        let before = (
            self.settings.session_docks.len(),
            self.settings.project_workspaces.len(),
        );
        self.settings
            .prune_session_docks(|chat_id| chats.contains(chat_id));
        if let Some(spaces) = spaces {
            self.settings
                .project_workspaces
                .retain(|id, _| spaces.contains(id));
        }
        let after = (
            self.settings.session_docks.len(),
            self.settings.project_workspaces.len(),
        );
        if before != after {
            self.schedule_save(cx);
        }
    }

    /// Open a session from the sidebar: focus its tab if it is open
    /// anywhere, else open it in the focused tile.
    pub(super) fn open_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.open_chat_with(chat_id, false, cx);
    }

    /// [`Self::open_chat`], or with `split` (⌘-click) in a new tile to the
    /// right of the focused one.
    pub(super) fn open_chat_with(&mut self, chat_id: String, split: bool, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        super::workspace_view::route_open(&mut self.workspace, TabKey::Session(chat_id), split);
        self.focus_pending = true;
        self.workspace_changed(cx);
    }

    /// Back/forward landing on a chat route: its tab (reopened if it was
    /// closed and the chat still exists), or for the canvas entry an open
    /// new-session tab (else a fresh one).
    pub(super) fn open_nav_target(&mut self, chat_id: String, cx: &mut Context<Self>) {
        if chat_id.is_empty() {
            let canvas = self
                .workspace
                .tabs()
                .find(|tab| matches!(tab, TabKey::NewSession(_)))
                .cloned();
            match canvas.and_then(|tab| self.workspace.find(&tab)) {
                Some((group, index)) => {
                    self.workspace.activate(group, index);
                    self.focus_pending = true;
                    self.workspace_changed(cx);
                }
                None => self.open_new_session(cx),
            }
            return;
        }
        let exists = self.state.read(cx).chats.iter().any(|c| c.id == chat_id);
        if exists {
            self.open_chat(chat_id, cx);
        }
    }

    /// Chats deleted / archived, or hidden by this window's project scope,
    /// leave the workspace ([`session_fate`]) — background tabs included,
    /// whose slots (and so contexts) may not exist yet. Deleted chats'
    /// parked terminals close, and their stashed drafts go, in the same pass.
    pub(super) fn prune_tabs(&mut self, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        let mut open: Vec<String> = self
            .workspace
            .tabs()
            .filter_map(|tab| tab.chat_id())
            .map(str::to_string)
            .collect();
        open.extend(self.parked_terminals.keys().cloned());
        open.extend(self.closed_drafts.keys().cloned());
        let sending: std::collections::HashSet<String> = open
            .iter()
            .filter(|id| self.chat_sending(id, cx))
            .cloned()
            .collect();
        let (closing, deleted) = {
            let state = self.state.read(cx);
            let generation = state.chats_generation();
            let chats_frame = state.chats_synced && generation != self.seen_chats_generation;
            self.seen_chats_generation = generation;
            // A frame listing an expected chat settles it; stale ones expire.
            self.expected_chats.retain(|id, created| {
                now.duration_since(*created) < EXPECTED_CHAT_TTL
                    && !(chats_frame && state.chats.iter().any(|c| c.id == *id))
            });
            let mut closing = std::collections::HashSet::new();
            let mut deleted = Vec::new();
            for id in &open {
                let awaited = self.expected_chats.contains_key(id)
                    || sending.contains(id)
                    || state.send_pending(id, Utc::now());
                match session_fate(
                    id,
                    &state.chats,
                    state.project_scope(),
                    chats_frame,
                    awaited,
                ) {
                    SessionFate::Stays => {}
                    SessionFate::Hidden => {
                        closing.insert(id.clone());
                    }
                    SessionFate::Deleted => {
                        closing.insert(id.clone());
                        deleted.push(id.clone());
                    }
                }
            }
            (closing, deleted)
        };
        for id in &deleted {
            if let Some(terminal) = self.parked_terminals.remove(id) {
                terminal.update(cx, |terminal, cx| terminal.close_all(cx));
            }
            self.closed_drafts.remove(id);
        }
        if closing.is_empty() {
            return;
        }
        let removed = self
            .workspace
            .retain_tabs(|tab| tab.chat_id().is_none_or(|id| !closing.contains(id)));
        if removed {
            self.workspace_changed(cx);
        }
    }

    /// A chat this window just created: its tab survives chats frames that
    /// don't list it yet (see [`Self::prune_tabs`]).
    pub(super) fn expect_chat(&mut self, chat_id: &str) {
        self.expected_chats
            .insert(chat_id.to_string(), std::time::Instant::now());
    }

    /// Whether `chat_id` is a just-created chat still waiting for its row.
    pub(super) fn chat_expected(&self, chat_id: &str) -> bool {
        self.expected_chats
            .get(chat_id)
            .is_some_and(|created| created.elapsed() < EXPECTED_CHAT_TTL)
    }

    /// Whether the composer of `chat_id`'s tab is still sending: a slow
    /// (remote) canvas send may outlive the pending-send overlay's TTL
    /// before its row lands, and its tab must not close mid-send.
    fn chat_sending(&self, chat_id: &str, cx: &App) -> bool {
        self.slot_for_tab(&TabKey::session(chat_id))
            .and_then(|sid| self.slots.get(&sid))
            .is_some_and(|slot| slot.composer.read(cx).is_sending())
    }

    /// Whether a session missing from the chats list is still expected to
    /// appear: a fork / promoted side chat just created, or a send from its
    /// tab still in flight. Its tab stays (see [`Self::prune_tabs`]).
    pub(super) fn chat_awaited(&self, chat_id: &str, cx: &App) -> bool {
        self.chat_expected(chat_id) || self.chat_sending(chat_id, cx)
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab: step through the sidebar's session rows in
    /// the order they are drawn ([`AppState::overview_chats`]), from the
    /// focused tile's session. Routing is a sidebar click's (focus an open
    /// tab, else open in the focused tile) — one press, one session.
    ///
    /// Chat-scoped chrome, like the panel toggles: gpui dispatches a matched
    /// binding before any `on_key_down`, so an unscoped cycle would fire
    /// underneath Settings (yanking the user off the page mid-record, since
    /// these are the very keys the shortcuts table invites them to press) or
    /// underneath the add-space palette, stranding the overlay over a session
    /// they never picked.
    pub(super) fn cycle_session(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Chat) || self.add_space.is_some() {
            return;
        }
        let selected = self
            .workspace
            .focused_tab()
            .and_then(|tab| tab.chat_id())
            .map(str::to_string);
        let order = self
            .state
            .read(cx)
            .overview_chats(Utc::now())
            .into_iter()
            .map(|(_, chat)| chat.id.clone())
            .collect::<Vec<_>>();
        if let Some(target) = cycle_target(&order, selected.as_deref(), forward) {
            self.open_chat(target, cx);
        }
    }

    /// A new-session tab in the focused tile: its existing one when it has
    /// one, else a fresh canvas. Returns the canvas slot.
    fn open_canvas(&mut self, cx: &mut Context<Self>) -> Option<session::SlotId> {
        self.route = Route::Chat;
        let focused = self.workspace.focused();
        let existing = self.workspace.group(focused).and_then(|group| {
            group
                .tabs()
                .iter()
                .position(|tab| matches!(tab, TabKey::NewSession(_)))
        });
        let tab = match existing {
            Some(index) => {
                self.workspace.activate(focused, index);
                self.workspace.group(focused)?.tabs()[index].clone()
            }
            None => {
                let tab = self.workspace.new_session_tab();
                self.workspace.open(tab.clone());
                tab
            }
        };
        self.focus_pending = true;
        self.sync_slots(cx);
        self.slot_for_tab(&tab)
    }

    /// The global new-session action (shortcut, the titlebar/sidebar `+`,
    /// an empty tile's button): a new-session tab in the focused tile. A
    /// live project pick stands — the sidebar never filters, so there is no
    /// filter to re-home onto. A missing, project-less, or dangling pick on
    /// the canvas is replaced by the last remembered live project, else the
    /// deterministic live-space fallback
    /// (`AppState::first_space_on_picked_device`). With no spaces at all the
    /// selection is left alone — the onboarding canvas blocks here anyway.
    pub(super) fn open_new_session(&mut self, cx: &mut Context<Self>) {
        let Some(sid) = self.open_canvas(cx) else {
            return;
        };
        let Some((state, composer)) = self
            .slots
            .get(&sid)
            .map(|slot| (slot.state.clone(), slot.composer.clone()))
        else {
            return;
        };
        let last_space = self.settings.last_space_id.clone();
        state.update(cx, |s, cx| {
            // A LIVE project pick stands: `selected_space_row` also reads
            // `None` for the explicit no-project opt-out and dangling ids, so
            // a project-less selection is repaired to the last remembered live
            // project (else the deterministic live-space fallback).
            // The generic new session is never a quick chat.
            s.scratch_pending = false;
            let has_live_selection = s.selected_space_row().is_some();
            if !has_live_selection && !s.spaces.is_empty() {
                let target = last_space
                    .filter(|id| s.space_row(id).is_some() && s.project_scope().space_visible(id))
                    .or_else(|| s.first_space_on_picked_device());
                if let Some(id) = target {
                    s.select_space(Some(id), cx);
                }
            }
        });
        // The generic `+` is not a targeted checkout: clear any programmatic
        // pin (sidebar hover add) so an already-pinned canvas reads generically
        // again — the canvas falls back to its ordinary project defaults.
        composer.update(cx, |composer, cx| {
            composer.clear_checkout_target(cx);
        });
        // Routing to the canvas is a navigation (Back returns to the session
        // just left) — `sync_follow` records it.
        self.workspace_changed(cx);
    }

    /// Quick chat: a new-session tab aimed at `device_id` with no project.
    /// The first send asks that device for a throwaway scratch folder and
    /// the session runs there; deleting the chat removes it.
    pub(super) fn start_quick_chat(&mut self, device_id: String, cx: &mut Context<Self>) {
        self.quick_chat = None;
        let Some(sid) = self.open_canvas(cx) else {
            return;
        };
        let Some((state, composer)) = self
            .slots
            .get(&sid)
            .map(|slot| (slot.state.clone(), slot.composer.clone()))
        else {
            return;
        };
        state.update(cx, |s, cx| s.begin_quick_chat(device_id, cx));
        composer.update(cx, |composer, cx| {
            composer.clear_checkout_target(cx);
        });
        self.workspace_changed(cx);
    }

    /// The sidebar's hover add buttons: a new-session tab explicitly targeted
    /// at `space_id`'s checkout (`plan` — a worktree path or the
    /// ordinary/current checkout). The pin rides the tab's composer pickers
    /// so the target is authoritative without a ListRefs round-trip; the
    /// global [`Self::open_new_session`] behavior is unchanged.
    pub(super) fn open_new_session_for(
        &mut self,
        space_id: String,
        plan: crate::pickers::CheckoutPlan,
        cx: &mut Context<Self>,
    ) {
        self.settings.last_space_id = Some(space_id.clone());
        if let Some(composer) = self
            .open_canvas(cx)
            .and_then(|sid| self.slots.get(&sid))
            .map(|slot| slot.composer.clone())
        {
            composer.update(cx, |composer, cx| {
                composer.target_checkout(space_id, plan, cx);
            });
        }
        self.schedule_save(cx);
        self.workspace_changed(cx);
    }
}

#[cfg(test)]
mod cycle_tests {
    use super::*;

    fn order(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn chat(id: &str, space_id: Option<&str>) -> cypher_proto::Chat {
        cypher_proto::Chat {
            id: id.into(),
            created_at: Utc::now(),
            space_id: space_id.map(Into::into),
            ..crate::test_fixtures::chat()
        }
    }

    #[test]
    fn steps_forward_and_back_through_the_list() {
        let list = order(&["a", "b", "c"]);
        assert_eq!(cycle_target(&list, Some("a"), true).as_deref(), Some("b"));
        assert_eq!(cycle_target(&list, Some("b"), true).as_deref(), Some("c"));
        assert_eq!(cycle_target(&list, Some("c"), false).as_deref(), Some("b"));
        assert_eq!(cycle_target(&list, Some("b"), false).as_deref(), Some("a"));
    }

    #[test]
    fn wraps_at_both_ends() {
        let list = order(&["a", "b", "c"]);
        assert_eq!(cycle_target(&list, Some("c"), true).as_deref(), Some("a"));
        assert_eq!(cycle_target(&list, Some("a"), false).as_deref(), Some("c"));
    }

    #[test]
    fn a_single_session_cycles_to_itself() {
        // Not a no-op by accident: with one row both directions must resolve,
        // so the shortcut never looks broken by dead-ending on `None`.
        let list = order(&["only"]);
        assert_eq!(
            cycle_target(&list, Some("only"), true).as_deref(),
            Some("only")
        );
        assert_eq!(
            cycle_target(&list, Some("only"), false).as_deref(),
            Some("only")
        );
    }

    #[test]
    fn no_selection_enters_the_list_from_the_matching_end() {
        let list = order(&["a", "b", "c"]);
        assert_eq!(cycle_target(&list, None, true).as_deref(), Some("a"));
        assert_eq!(cycle_target(&list, None, false).as_deref(), Some("c"));
        assert_eq!(
            cycle_target(&list, Some("gone"), true).as_deref(),
            Some("a")
        );
        assert_eq!(
            cycle_target(&list, Some("gone"), false).as_deref(),
            Some("c")
        );
    }

    #[test]
    fn restore_prunes_dead_sessions_and_canvases() {
        use crate::workspace::{Edge, TabKey, Workspace};
        let mut saved = Workspace::new();
        let left = saved.open(TabKey::session("a"));
        saved.open(TabKey::session("gone"));
        let canvas = saved.new_session_tab();
        saved.open(canvas);
        let right = saved
            .open_split(TabKey::session("archived"), left, Edge::Right)
            .unwrap();
        saved.split_group(right, Edge::Bottom);
        let empty = saved.focused();
        saved.open_in(empty, TabKey::session("b"));
        saved.activate(left, 1); // "gone" was the active tab
        let restored = restore_workspace(saved, |id| id == "a" || id == "b");
        let shape: Vec<Vec<TabKey>> = restored
            .groups_in_reading_order()
            .into_iter()
            .map(|id| restored.group(id).unwrap().tabs().to_vec())
            .collect();
        // The archived session's group collapsed; the canvas is dropped.
        assert_eq!(
            shape,
            vec![vec![TabKey::session("a")], vec![TabKey::session("b")]]
        );
        let first = restored.groups_in_reading_order()[0];
        assert_eq!(
            restored.group(first).unwrap().active_tab(),
            Some(&TabKey::session("a"))
        );
        // Nothing live: an empty (single-group) workspace — the boot
        // landing takes over.
        let mut only_dead = Workspace::new();
        only_dead.open(TabKey::session("gone"));
        let restored = restore_workspace(only_dead, |_| false);
        assert_eq!(restored.tabs().count(), 0);
        assert_eq!(restored.group_count(), 1);
    }

    #[test]
    fn presync_tabs_join_the_restored_layout() {
        use crate::workspace::{Edge, TabKey, Workspace};
        // The saved layout: two tiles, the right one focused.
        let mut saved = Workspace::new();
        let left = saved.open(TabKey::session("a"));
        saved
            .open_split(TabKey::session("b"), left, Edge::Right)
            .unwrap();
        let mut restored = restore_workspace(saved, |_| true);
        let taken = restored.new_session_tab();
        // ⌘N before the first chats frame: a canvas in the boot workspace,
        // whose key may collide with one the restored layout mints.
        let mut presync = Workspace::new();
        let canvas = presync.new_session_tab();
        presync.open(canvas.clone());
        let renamed = adopt_presync_tabs(&mut restored, &presync);
        assert_eq!(renamed.len(), 1);
        let (old, new) = &renamed[0];
        assert_eq!(old, &canvas);
        assert!(matches!(new, TabKey::NewSession(_)));
        assert_ne!(new, &taken);
        // The saved tabs survive; the canvas joined the focused tile, focused.
        assert!(restored.contains(&TabKey::session("a")));
        assert!(restored.contains(&TabKey::session("b")));
        assert_eq!(restored.group_count(), 2);
        assert_eq!(restored.focused_tab(), Some(new));
        let focused = restored.group(restored.focused()).unwrap();
        assert_eq!(focused.tabs(), &[TabKey::session("b"), new.clone()]);
    }

    #[test]
    fn a_background_session_leaves_when_its_chat_goes() {
        use crate::state::ProjectScope;
        let scope = ProjectScope::default();
        let mut archived = chat("archived", None);
        archived.archived = true;
        let chats = [chat("live", None), archived, chat("elsewhere", Some("p"))];
        let fate = |id: &str, scope: &ProjectScope, frame: bool, awaited: bool| {
            session_fate(id, &chats, scope, frame, awaited)
        };
        assert_eq!(fate("live", &scope, true, false), SessionFate::Stays);
        // Archived / out of scope: judged on any notify, no frame needed.
        assert_eq!(fate("archived", &scope, false, false), SessionFate::Hidden);
        let other_window = ProjectScope {
            only: Some("q".into()),
            ..ProjectScope::default()
        };
        assert_eq!(
            fate("elsewhere", &other_window, false, false),
            SessionFate::Hidden
        );
        // Missing: only a new chats frame deletes it…
        assert_eq!(fate("gone", &scope, false, false), SessionFate::Stays);
        assert_eq!(fate("gone", &scope, true, false), SessionFate::Deleted);
        // …and never a chat whose row is still on its way (a live pending
        // send, a fork / promoted side chat just created).
        assert_eq!(fate("minted", &scope, true, true), SessionFate::Stays);
        // Awaiting doesn't shield an archived chat.
        assert_eq!(fate("archived", &scope, true, true), SessionFate::Hidden);
    }

    #[test]
    fn an_empty_list_has_nothing_to_select() {
        assert_eq!(cycle_target(&[], None, true), None);
        assert_eq!(cycle_target(&[], Some("a"), true), None);
    }

    #[test]
    fn the_order_comes_from_the_sidebar_overview() {
        // `cycle_target` walks the ids in order; `overview_chats` is what the
        // sidebar draws (and already hides archived/child chats). A selected
        // row that is no longer in the list (e.g. archived from another
        // device) is treated like no selection rather than dead-ending.
        let list = order(&["mine", "theirs", "loose"]);
        assert_eq!(
            cycle_target(&list, Some("mine"), true).as_deref(),
            Some("theirs")
        );
        // Archived/child exclusion lives in `overview_chats`, so the pure
        // helper just receives the filtered order.
        let filtered = [chat("mine", None), chat("theirs", None)];
        let ids: Vec<String> = filtered.iter().map(|c| c.id.clone()).collect();
        assert_eq!(cycle_target(&ids, None, false).as_deref(), Some("theirs"));
    }
}
