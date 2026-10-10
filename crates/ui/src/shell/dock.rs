//! A session tile's right dock — the surface host (t3code RightPanelTabs):
//! Git diff / Files / Side chat tabs bound to the tile's session, hidden by
//! default, drag-resizable, with its own surface tab strip in its top row.
//! Terminals live only in the tile's bottom dock ([`super::session`]).

use super::session::{SessionSlot, SlotId};
use super::*;

/// One dock surface tab: a git-diff page (each tab its own [`Changes`]
/// viewer — multiple diff panels, user request), a file browser/editor over
/// the chat's checkout ([`FilesPanel`]), or a temporary Side Chat (round
/// 21). `Picker` is the empty state ("Open a surface").
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DockSurface {
    #[default]
    Picker,
    Diff(u64),
    Files(u64),
    SideChat(u64),
}

/// A tile's right dock state. Everything defaults CLOSED (user request —
/// a default-open pane popped open on every session you visited): opening
/// is an explicit act, remembered by the tile for the rest of the app run; a
/// fresh open with no surface tabs lands on the picker.
pub(super) struct Dock {
    pub(super) open: bool,
    /// Which surface tab renders; validated against the live tab list each
    /// frame (a closed tab falls back gracefully).
    pub(super) active: DockSurface,
    /// Ordered surface tabs (drag-reorderable; stale entries are skipped at
    /// read time).
    pub(super) surfaces: Vec<DockSurface>,
    /// Takeover (the dock's expand button): the dock fills the session area
    /// and the conversation column collapses to zero. View state — never
    /// persisted, reset on close.
    pub(super) expanded: bool,
    /// Open/close/expand width tween.
    pub(super) tween: Option<WidthTween>,
    /// In-flight surface-tab drag (slide animation state).
    pub(super) tab_drag: Option<DockTabDragState>,
    /// Surface-tab strip scroll (the strip overflows horizontally, t3
    /// ScrollArea-style; drag drop-math reads the offset back out).
    pub(super) tab_scroll: gpui::ScrollHandle,
}

impl Dock {
    pub(super) fn new() -> Self {
        Self {
            open: false,
            active: DockSurface::Picker,
            surfaces: Vec::new(),
            expanded: false,
            tween: None,
            tab_drag: None,
            tab_scroll: gpui::ScrollHandle::new(),
        }
    }
}

/// Drag marker for a dock's width handle.
pub(super) struct DockResize(pub(super) SlotId);

/// The dragged surface-tab payload (strip reorder).
struct DockTabDrag {
    slot: SlotId,
    from: usize,
    title: SharedString,
}

/// Live drag-over state for the surface-tab strip — the terminal drawer's
/// [`crate::terminal::panel`] DragState, ported: `epoch` keys the 150ms
/// slide-animation restarts as the hovered slot changes.
pub(super) struct DockTabDragState {
    from: usize,
    over: usize,
    epoch: usize,
    prev_over: usize,
}

/// Ghost chip following the pointer while a surface tab drags.
struct SurfaceTabGhost {
    title: SharedString,
}

impl Render for SurfaceTabGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .h(px(RIGHT_TAB_HEIGHT))
            .w(px(112.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .rounded(px(RIGHT_TAB_RADIUS))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border_strong)
            .text_size(px(11.5))
            .text_color(theme.text)
            .opacity(0.85)
            .child(div().truncate().child(self.title.clone()))
    }
}

/// The drag-drop spec for the dock surface tab strip: move the surface at
/// `from` to `to` (both must be in bounds and distinct). Pure so the reorder
/// behaviour — including mixed diff/files/side-chat strips — is testable
/// without a live shell. Returns whether the strip changed.
pub fn reorder_dock_surfaces(tabs: &mut [DockSurface], from: usize, to: usize) -> bool {
    if from < tabs.len() && to < tabs.len() && from != to {
        let surface = tabs[from];
        if from < to {
            tabs.copy_within(from + 1..=to, from);
            tabs[to] = surface;
        } else {
            tabs.copy_within(to..from, to + 1);
            tabs[to] = surface;
        }
        true
    } else {
        false
    }
}

/// The dock's minimum width inside a session area. Narrower than the old
/// right pane's `RIGHT_PANE_MIN` (settings): a tile holds far less than a window, and a
/// half-window tile should keep its chat beside the dock.
pub const DOCK_MIN_WIDTH: f32 = 280.0;

/// Whether a session area this wide is too narrow for the chat column and
/// the dock side by side — the dock then takes the area over (decision 6).
pub fn dock_takes_over(area_width: f32) -> bool {
    area_width < session::CHAT_MIN_WIDTH + DOCK_MIN_WIDTH
}

/// A dock size as a fraction of the session area's `extent` (what a session
/// remembers); `None` before the area is measured.
pub fn fraction_of(size: f32, extent: f32) -> Option<f32> {
    (extent > 0.0)
        .then(|| size / extent)
        .and_then(crate::prefs::clamp_fraction)
}

/// A remembered fraction back in pixels of `extent`; a dock never dragged
/// (`None`) uses the legacy global pixel size.
pub fn preferred_size(fraction: Option<f32>, legacy_px: f32, extent: f32) -> f32 {
    fraction.map_or(legacy_px, |fraction| fraction * extent)
}

/// The terminal dock's resting height in a session area `area_height` tall:
/// the session's fraction (or the legacy height), kept between
/// `TERMINAL_MIN_HEIGHT` and the viewport-relative cap.
pub fn terminal_dock_height(fraction: Option<f32>, legacy_px: f32, area_height: f32) -> f32 {
    clamp_terminal_height(
        preferred_size(fraction, legacy_px, area_height),
        area_height,
    )
}

/// Terminal body kept under the dock's tab bar before the dock gives up its
/// body and shows the bar alone.
pub const TERMINAL_BODY_MIN: f32 = 48.0;

/// The terminal dock's height in a short tile. Priority, top down: the tile
/// header (outside the session area), the composer stack (`stack_h`, never
/// clipped), the terminal, then the transcript (may shrink to nothing). The
/// terminal keeps `height` while it fits under the composer, else takes the
/// room left; with less than a tab bar plus [`TERMINAL_BODY_MIN`] it
/// collapses to its tab bar, and without room for that it hides (0 — still
/// open, it comes back as the tile grows).
pub fn fit_terminal_height(height: f32, area_height: f32, stack_h: f32) -> f32 {
    let room = area_height - stack_h;
    // The tab bar plus the dock's top border.
    let bar = crate::terminal::panel::TAB_BAR_HEIGHT + 1.0;
    if room >= height {
        height
    } else if room >= bar + TERMINAL_BODY_MIN {
        room
    } else if room >= bar {
        bar
    } else {
        0.0
    }
}

/// The dock's resting width in a session area `area_width` wide: the
/// preferred width, clamped so the chat column keeps its minimum; the
/// whole area in takeover (expanded, or too narrow for both).
pub fn dock_width(area_width: f32, preferred: f32, expanded: bool) -> f32 {
    if expanded || dock_takes_over(area_width) {
        area_width.max(0.0)
    } else {
        let max = (area_width - session::CHAT_MIN_WIDTH).min(RIGHT_PANE_MAX);
        preferred.clamp(DOCK_MIN_WIDTH, max.max(DOCK_MIN_WIDTH))
    }
}

impl Shell {
    // ---- dock geometry ----

    fn dock_target(&self, slot: &SessionSlot) -> f32 {
        if !slot.dock.open {
            return 0.0;
        }
        self.dock_open_width(slot)
    }

    /// The dock's width when open: the session's fraction of its area (the
    /// legacy global width if never dragged), clamped to what the area holds.
    fn dock_open_width(&self, slot: &SessionSlot) -> f32 {
        let area_w = f32::from(slot.area.get().size.width);
        let preferred = preferred_size(slot.right_fraction, self.settings.right_pane_width, area_w);
        dock_width(area_w, preferred, slot.dock.expanded)
    }

    /// The dock's width this frame (mid-tween while opening/closing).
    pub(super) fn dock_width_now(&self, slot: &SessionSlot) -> f32 {
        self.eval_tween(slot.dock.tween, self.dock_target(slot))
    }

    /// Does the slot's space folder have git? Owner-stamped and synced —
    /// gates the Git surface rows with zero RPCs.
    fn slot_git_detected(&self, slot: &SessionSlot, cx: &App) -> bool {
        slot.state.read(cx).selected_space_git()
    }

    /// Files is only meaningful for a session bound to a project checkout
    /// (the engine resolves paths against the chat's verified cwd).
    fn files_available(&self, slot: &SessionSlot, cx: &App) -> bool {
        let state = slot.state.read(cx);
        state.selected_chat.is_some() && crate::files::context_for(state).is_ok()
    }

    // ---- open / close ----

    /// The tile header's dock button, ⌘B (focused tile), and the dock's own
    /// close button. Not gated on git: the dock is a surface HOST, and only
    /// the Git surface rows check git.
    pub(super) fn toggle_dock(&mut self, sid: SlotId, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get(&sid) else {
            return;
        };
        // Never on the new-session canvas (nothing to host yet); closing
        // stays possible.
        if !slot.dock.open && slot.tab.chat_id().is_none() {
            return;
        }
        let from = self.dock_width_now(slot);
        let open = !slot.dock.open;
        let active = self.resolved_dock_active(slot, cx);
        let diff = match active {
            DockSurface::Diff(id) => slot.diffs.get(&id).cloned(),
            _ => None,
        };
        if let Some(slot) = self.slots.get_mut(&sid) {
            slot.dock.open = open;
            if !open {
                // Closing always leaves takeover mode — reopening at full
                // bleed with the conversation gone read as a broken chat.
                slot.dock.expanded = false;
            }
        }
        if let Some(changes) = diff {
            if open {
                // Reopening onto a diff tab revalidates its watch.
                changes.update(cx, |changes, cx| changes.ensure_content(cx));
            } else {
                // The dock is gone: an outgoing diff's selection/comment must
                // not linger over the conversation.
                changes.update(cx, |changes, cx| changes.detach(cx));
            }
        }
        if let Some(slot) = self.slots.get(&sid) {
            let to = self.dock_target(slot);
            if let Some(slot) = self.slots.get_mut(&sid) {
                slot.dock.tween = Some(WidthTween::new(from, to));
            }
        }
        self.remember_slot_docks(sid, cx);
        cx.notify();
    }

    /// Toggle the dock takeover (its expand button, t3code parity): the dock
    /// fills the session area, hiding the conversation column; toggling back
    /// restores the saved width. When the area is too narrow for both the
    /// dock is ALREADY a takeover — the toggle returns to the chat (closes
    /// the dock).
    fn toggle_dock_expand(&mut self, sid: SlotId, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get(&sid) else {
            return;
        };
        if !slot.dock.expanded && dock_takes_over(f32::from(slot.area.get().size.width)) {
            self.toggle_dock(sid, cx);
            return;
        }
        let from = self.dock_width_now(slot);
        if let Some(slot) = self.slots.get_mut(&sid) {
            slot.dock.expanded = !slot.dock.expanded;
        }
        if let Some(slot) = self.slots.get(&sid) {
            let to = self.dock_target(slot);
            if let Some(slot) = self.slots.get_mut(&sid) {
                slot.dock.tween = Some(WidthTween::new(from, to));
            }
        }
        cx.notify();
    }

    pub(super) fn on_dock_drag(
        &mut self,
        event: &gpui::DragMoveEvent<DockResize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sid = event.drag(cx).0;
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        let area = slot.area.get();
        let width = f32::from(area.right()) - f32::from(event.event.position.x);
        let area_w = f32::from(area.size.width);
        slot.dock.tween = None;
        slot.dock.expanded = false;
        // Clamp to what the area can hold beside the chat's minimum; the
        // session remembers the share of its area.
        slot.right_fraction = fraction_of(dock_width(area_w, width, false), area_w);
        self.remember_slot_docks(sid, cx);
        cx.notify();
    }

    // ---- surfaces ----

    /// The dock's surface tabs in the STORED (drag-reorderable) order —
    /// `(surface, title)`; entries whose backing entity is gone are skipped.
    fn dock_surface_rows(&self, slot: &SessionSlot, cx: &App) -> Vec<(DockSurface, SharedString)> {
        slot.dock
            .surfaces
            .iter()
            .filter_map(|surface| match surface {
                DockSurface::Diff(id) => slot
                    .diffs
                    .get(id)
                    // Contextual title (user request): the pane's scope
                    // label, or the pinned commit's subject.
                    .map(|changes| (*surface, changes.read(cx).tab_title())),
                DockSurface::Files(id) => slot
                    .files
                    .get(id)
                    .map(|panel| (*surface, panel.read(cx).tab_title())),
                DockSurface::SideChat(id) => slot
                    .side_chats
                    .get(id)
                    .map(|panel| (*surface, panel.read(cx).tab_title())),
                DockSurface::Picker => None,
            })
            .collect()
    }

    /// The surface that actually renders: the stored pick when it still
    /// exists, else the first remaining tab, else the picker — never render
    /// a dead surface.
    fn resolved_dock_active(&self, slot: &SessionSlot, cx: &App) -> DockSurface {
        let picked = slot.dock.active;
        let rows = self.dock_surface_rows(slot, cx);
        let exists = match picked {
            DockSurface::Picker => false,
            surface => rows.iter().any(|(s, _)| *s == surface),
        };
        if exists {
            picked
        } else {
            rows.first().map(|(s, _)| *s).unwrap_or(DockSurface::Picker)
        }
    }

    /// Drag-reorder a surface tab within this dock's strip.
    fn reorder_dock_tabs(&mut self, sid: SlotId, from: usize, to: usize, cx: &mut Context<Self>) {
        if let Some(slot) = self.slots.get_mut(&sid)
            && reorder_dock_surfaces(&mut slot.dock.surfaces, from, to)
        {
            cx.notify();
        }
    }

    /// Track the hovered drop slot mid-drag (the terminal drawer's
    /// `update_drag_over`, ported: epoch bumps restart the slide tween).
    fn update_dock_tab_drag_over(
        &mut self,
        sid: SlotId,
        from: usize,
        over: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        match &mut slot.dock.tab_drag {
            Some(drag) if drag.over != over => {
                drag.prev_over = drag.over;
                drag.over = over;
                drag.epoch += 1;
                cx.notify();
            }
            Some(_) => {}
            None => {
                slot.dock.tab_drag = Some(DockTabDragState {
                    from,
                    over,
                    epoch: 0,
                    prev_over: from,
                });
                cx.notify();
            }
        }
    }

    pub(super) fn set_dock_active(
        &mut self,
        sid: SlotId,
        surface: DockSurface,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.slots.get(&sid) else {
            return;
        };
        // The surface being replaced: an outgoing diff's selection/comment
        // must clear — but ONLY the outgoing ACTIVE one, never a hidden
        // pane's.
        let outgoing = self.resolved_dock_active(slot, cx);
        if let DockSurface::Diff(id) = outgoing
            && outgoing != surface
            && let Some(changes) = slot.diffs.get(&id).cloned()
        {
            changes.update(cx, |changes, cx| changes.detach(cx));
        }
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        slot.dock.active = surface;
        match surface {
            DockSurface::Diff(id) => {
                if let Some(changes) = slot.diffs.get(&id).cloned() {
                    changes.update(cx, |changes, cx| changes.ensure_content(cx));
                }
            }
            DockSurface::Files(id) => {
                if let Some(panel) = slot.files.get(&id).cloned() {
                    panel.update(cx, |panel, cx| panel.ensure_content(cx));
                }
            }
            DockSurface::SideChat(_) | DockSurface::Picker => {}
        }
        cx.notify();
    }

    /// The picker's Files card / the `+` menu's Files row: a fresh file
    /// browser tab over the session's checkout.
    fn add_files_surface(&mut self, sid: SlotId, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        slot.files_seq += 1;
        let id = slot.files_seq;
        let state = slot.state.clone();
        let panel = cx.new(|cx| FilesPanel::new(state, cx));
        slot.files.insert(id, panel);
        slot.dock.surfaces.push(DockSurface::Files(id));
        self.set_dock_active(sid, DockSurface::Files(id), cx);
    }

    /// The picker's Git card / the `+` menu's Git row: every click opens a
    /// FRESH diff tab with its own scope/base selection (multiple diff
    /// panels, user request).
    fn add_diff_surface(&mut self, sid: SlotId, cx: &mut Context<Self>) {
        let Some(state) = self.slots.get(&sid).map(|slot| slot.state.clone()) else {
            return;
        };
        let popup = self.comment_popup.clone().downgrade();
        let changes = cx.new(|cx| Changes::new(state, popup, cx));
        self.register_diff_surface(sid, changes, cx);
    }

    /// A History row click: the commit opens as its own pinned diff tab
    /// (user request).
    fn add_commit_diff_surface(
        &mut self,
        sid: SlotId,
        commit: cypher_proto::GitHistoryCommit,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.slots.get(&sid).map(|slot| slot.state.clone()) else {
            return;
        };
        let popup = self.comment_popup.clone().downgrade();
        let changes = cx.new(|cx| Changes::for_commit(state, popup, commit, cx));
        self.register_diff_surface(sid, changes, cx);
    }

    fn register_diff_surface(
        &mut self,
        sid: SlotId,
        changes: Entity<Changes>,
        cx: &mut Context<Self>,
    ) {
        let sub = cx.subscribe(&changes, move |this: &mut Self, _, event, cx| match event {
            ChangesEvent::OpenCommit(commit) => {
                this.add_commit_diff_surface(sid, commit.clone(), cx);
            }
        });
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        slot.diff_seq += 1;
        let id = slot.diff_seq;
        slot.diffs.insert(id, changes);
        slot.diff_subs.insert(id, sub);
        slot.dock.surfaces.push(DockSurface::Diff(id));
        self.set_dock_active(sid, DockSurface::Diff(id), cx);
    }

    /// A surface tab's ✕. The active fallback happens naturally through
    /// [`Self::resolved_dock_active`] on the next frame.
    fn close_dock_surface(&mut self, sid: SlotId, surface: DockSurface, cx: &mut Context<Self>) {
        if let DockSurface::SideChat(id) = surface {
            self.close_side_chat_by_seq(sid, id, cx);
            return;
        }
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        slot.dock.surfaces.retain(|s| *s != surface);
        match surface {
            DockSurface::Diff(id) => {
                // Dropping the entity tears down its diff watch; detach first
                // so a new pane with the same scope never inherits a stale
                // selection/comment.
                if let Some(changes) = slot.diffs.remove(&id) {
                    changes.update(cx, |changes, cx| changes.detach(cx));
                }
                slot.diff_subs.remove(&id);
            }
            DockSurface::Files(id) => {
                // Dropping the entity drops its editors — unsaved edits go
                // with them (the tab title carries the ● warning).
                slot.files.remove(&id);
            }
            DockSurface::SideChat(_) | DockSurface::Picker => {}
        }
        if slot.dock.active == surface {
            slot.dock.active = DockSurface::Picker;
        }
        cx.notify();
    }

    pub(super) fn close_right_plus(&mut self, cx: &mut Context<Self>) {
        if self.right_plus.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.right_plus);
        }
        cx.notify();
    }

    /// The dock column: its surface tab strip row, then the ACTIVE surface
    /// — the Diff page (its options row + the lazy [`Changes`] viewer), the
    /// Files page, a side chat, or the "Open a surface" picker when no tabs
    /// exist. `None` while fully closed.
    pub(super) fn render_dock(
        &mut self,
        sid: SlotId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let slot = self.slots.get(&sid)?;
        let target = self.dock_target(slot);
        let tween = slot.dock.tween;
        let width = self.eval_tween(tween, target);
        if width <= 0.0 && target <= 0.0 {
            return None;
        }
        let area_w = f32::from(slot.area.get().size.width);
        let takeover = slot.dock.expanded || dock_takes_over(area_w);
        // The open width, even mid-close: the clipped inner keeps it so the
        // content never reflows while the container tweens.
        let open_w = self.dock_open_width(slot);
        let active = self.resolved_dock_active(slot, cx);
        let theme = Theme::of(cx).clone();
        // The focused tile's rail floats over the dock's top-right corner;
        // the surfaces' header buttons step left of it.
        let header_right = if self.workspace.focused_tab() == Some(&slot.tab) {
            RAIL_WIDTH
        } else {
            8.0
        };
        let content: AnyElement = if slot.dock.open {
            match active {
                DockSurface::Diff(id) if slot.diffs.contains_key(&id) => {
                    let changes = slot.diffs.get(&id).cloned().expect("checked");
                    // Idempotent — also covers a persisted-open pane on boot.
                    changes.update(cx, |changes, cx| changes.ensure_content(cx));
                    // The diff options (scope dropdown, ref selector,
                    // fold-all) sit in their own row under the dock's
                    // surface tab strip (user request).
                    let controls =
                        changes.update(cx, |changes, cx| changes.render_header_controls(cx));
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex_none()
                                .h(px(36.0))
                                .pl(px(8.0))
                                .pr(px(header_right))
                                .child(controls),
                        )
                        // Full-width opaque diff-row tints must end before
                        // the rounded card's bottom arcs (GPUI clips rects,
                        // not descendant pixels to the parent's radius).
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .pb(px(PANEL_CORNER_RADIUS))
                                .child(changes),
                        )
                        .into_any_element()
                }
                DockSurface::Files(id) if slot.files.contains_key(&id) => {
                    let panel = slot.files.get(&id).cloned().expect("checked");
                    panel.update(cx, |panel, cx| panel.ensure_content(cx));
                    // Same two-row shape as the diff pane: the surface's own
                    // controls row under the tab strip, then the body.
                    let controls = panel.update(cx, |panel, cx| panel.render_header_controls(cx));
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex_none()
                                .h(px(36.0))
                                .pl(px(8.0))
                                .pr(px(header_right))
                                .child(controls),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .pb(px(PANEL_CORNER_RADIUS))
                                .child(panel),
                        )
                        .into_any_element()
                }
                DockSurface::SideChat(id) => {
                    if let Some(panel) = slot.side_chats.get(&id) {
                        panel.update(cx, |panel, cx| panel.set_header_right(header_right, cx));
                        panel.clone().into_any_element()
                    } else {
                        self.render_surface_picker(sid, cx)
                    }
                }
                _ => self.render_surface_picker(sid, cx),
            }
        } else {
            gpui::Empty.into_any_element()
        };
        let slot = self.slots.get(&sid)?;
        let is_side_chat = slot.dock.open
            && matches!(
                active,
                DockSurface::SideChat(id) if slot.side_chats.contains_key(&id)
            );
        let panel_bg = crate::chat_style::panel_background(
            crate::chat_style::settings(cx),
            &theme,
            is_side_chat,
        );
        // The dock paints the selected surface's background; child viewports
        // stay transparent.
        let panel_bg = match active {
            DockSurface::Diff(_) => {
                let t = crate::surface_style::theme(crate::surface_style::Region::Git, cx);
                t.regions.git_background.unwrap_or(panel_bg)
            }
            _ => panel_bg,
        };
        // Surfaces switch from the session's vertical rail in the tile's
        // top-right corner ([`Self::render_session_rail`]), which also toggles the dock.
        let panel = div()
            .size_full()
            .flex()
            .flex_row()
            .relative()
            .bg(panel_bg)
            .child(div().flex_1().min_w_0().h_full().child(content))
            // A divider line on the chat side, stopping short of the tile's
            // top and bottom edges.
            .when(!takeover, |el| {
                el.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(super::session::DIVIDER_INSET))
                        .bottom(px(super::session::DIVIDER_INSET))
                        .w(px(1.0))
                        .bg(theme.border),
                )
            });
        // The resize grabber floats over the dock's left edge (inside the
        // width-clipped container — a negative inset was clipped into
        // unreachability, user-reported dead resize). Takeover has no drag
        // width — the handle would fight the area-derived target.
        let handle = (!takeover).then(|| {
            self.resize_handle(("dock-resize", sid), move || DockResize(sid), |_, _| {}, cx)
                // Double-click: back to the default width, for this session.
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                        if event.click_count == 2
                            && let Some(slot) = this.slots.get_mut(&sid)
                        {
                            let area_w = f32::from(slot.area.get().size.width);
                            slot.right_fraction = fraction_of(RIGHT_PANE_DEFAULT, area_w);
                            this.remember_slot_docks(sid, cx);
                            cx.notify();
                        }
                    }),
                )
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(0.0))
        });
        Some(
            div()
                .h_full()
                .flex_none()
                .overflow_hidden()
                .w(px(width))
                .child(
                    div()
                        .h_full()
                        .w(px(open_w.max(width)))
                        .relative()
                        .child(panel)
                        .children(handle),
                )
                .into_any_element(),
        )
    }

    /// The right pane's empty state: the "Open a surface" heading over a
    /// compact vertical list of surface rows (icon + label) — the Capy
    /// arrangement (user request): the old two-card grid clipped in narrow
    /// panes and wasted short ones.
    fn render_surface_picker(&mut self, sid: SlotId, cx: &mut Context<Self>) -> AnyElement {
        let (files_available, git) = match self.slots.get(&sid) {
            Some(slot) => (
                self.files_available(slot, cx),
                self.slot_git_detected(slot, cx),
            ),
            None => (false, false),
        };
        let theme = Theme::of(cx).clone();
        let text = theme.text;
        let muted = theme.text_muted;
        let border = theme.border;
        let border_strong = theme.border_strong;
        let row = |id: &'static str, icon_path: &'static str, title: &'static str| {
            div()
                .id(id)
                .w_full()
                .h(px(44.0))
                .px(px(14.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(border)
                .bg(crate::theme::ink(0.02))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .cursor_pointer()
                .hover(move |s| s.bg(crate::theme::ink(0.05)).border_color(border_strong))
                .child(icon(icon_path).size(px(15.0)).flex_none().text_color(muted))
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(text)
                        .child(SharedString::from(title)),
                )
        };
        // Centered by auto margins, which collapse when the list outgrows a
        // short dock: it then starts at the top and scrolls, never painting
        // up over the dock's header row.
        div()
            .id(("dock-picker", sid))
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p(px(16.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(280.0))
                    .m_auto()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .text_center()
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(text)
                            .child(SharedString::from("Open a surface")),
                    )
                    .child(
                        div()
                            .mt(px(4.0))
                            .text_center()
                            .text_size(px(11.5))
                            .text_color(muted)
                            .child(SharedString::from("Choose what to show in the side panel.")),
                    )
                    .child(
                        div()
                            .mt(px(16.0))
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            // Files needs a session with a project checkout
                            // to browse (same gate as the `+` menu row).
                            .when(files_available, |el| {
                                el.child(
                                    row("surface-card-files", icons::FOLDER_WITH_FILES, "Files")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.add_files_surface(sid, cx);
                                        })),
                                )
                            })
                            // Git only where there IS git — the dock itself
                            // doesn't gate on it (side chats work anywhere).
                            .when(git, |el| {
                                el.child(
                                    row("surface-card-git", icons::GIT_BRANCH, "Git").on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.add_diff_surface(sid, cx);
                                        }),
                                    ),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    /// Tabs and their toolbar sit on the active pane's background, so they
    /// must use its text colors too (e.g. a dark terminal in a light app).
    fn dock_header_theme(&self, active: DockSurface, cx: &App) -> Theme {
        match active {
            DockSurface::Diff(_) => {
                let mut theme = crate::surface_style::theme(crate::surface_style::Region::Git, cx);
                theme.surface = theme.regions.git_background.unwrap_or(theme.surface);
                theme
            }
            DockSurface::SideChat(_) => {
                let mut theme = crate::chat_style::theme(cx);
                if theme.text != Theme::of(cx).text || theme.bg != Theme::of(cx).bg {
                    theme.text_muted = theme.bg.blend(theme.text.opacity(0.72));
                }
                theme.surface = crate::chat_style::panel_background(
                    crate::chat_style::settings(cx),
                    Theme::of(cx),
                    true,
                );
                theme
            }
            _ => Theme::of(cx).clone(),
        }
    }

    /// The dock's surface strip: one chip per surface tab (icon · title ·
    /// ✕) plus the `+` menu — the t3code RightPanelTabs bar, in the dock's
    /// top row; the diff options sit in the pane below.
    /// The session's vertical rail in the tile's top-right corner (user
    /// request; `top` clears native caption buttons), one
    /// raised card: the tile actions (terminal, dock, split, zoom), a divider,
    /// then the dock's surfaces (icon-only, titles on hover) and the `+`
    /// menu; the dock-expand toggle sits under the card while the dock is
    /// open. Surfaces reorder by dragging; middle-click or the hover ✕ closes.
    /// Open the dock if it is closed (a rail surface or `+` pick shows its
    /// surface straight away).
    fn ensure_dock_open(&mut self, sid: SlotId, cx: &mut Context<Self>) {
        if self.slots.get(&sid).is_some_and(|slot| !slot.dock.open) {
            self.toggle_dock(sid, cx);
        }
    }

    pub(super) fn render_session_rail(
        &mut self,
        sid: SlotId,
        top: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        /// Uniform button slot — the drag mechanics (drop-index quantisation
        /// + slide offsets) assume equal heights.
        const BUTTON: f32 = 28.0;
        const GAP: f32 = 4.0;
        const SLOT: f32 = BUTTON + GAP;

        let has_drag = cx.has_active_drag();
        let Some(slot) = self.slots.get_mut(&sid) else {
            return Empty.into_any_element();
        };
        // Heal drag state if the pointer was released outside the rail.
        if slot.dock.tab_drag.is_some() && !has_drag {
            slot.dock.tab_drag = None;
        }
        let Some(slot) = self.slots.get(&sid) else {
            return Empty.into_any_element();
        };
        let rows = self.dock_surface_rows(slot, cx);
        let count = rows.len();
        let dock_open = slot.dock.open;
        // No dock on the new-session canvas (nothing to host yet).
        let has_dock = slot.tab.chat_id().is_some();
        let terminal_open = slot.terminal_open;
        let group = self.workspace.find(&slot.tab).map(|(group, _)| group);
        let zoomed = group.is_some() && self.workspace.zoomed() == group;
        let can_zoom = zoomed || self.workspace.group_count() > 1;
        let active = self.resolved_dock_active(slot, cx);
        let theme = self.dock_header_theme(active, cx);
        let files_available = self.files_available(slot, cx);
        let git = self.slot_git_detected(slot, cx);
        let tab_scroll = slot.dock.tab_scroll.clone();
        let drag = slot
            .dock
            .tab_drag
            .as_ref()
            .map(|d| (d.from, d.over, d.epoch, d.prev_over));

        // The column IS the scroller (a short tile can overflow it); drop
        // math runs in content coordinates (viewport y plus the scrolled-off
        // height).
        let scroll_for_drag = tab_scroll.clone();
        let mut tabs = div()
            .id(("dock-rail-tabs", sid))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(GAP))
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&tab_scroll)
            .on_drag_move::<DockTabDrag>(cx.listener(
                move |this, event: &gpui::DragMoveEvent<DockTabDrag>, _, cx| {
                    let payload = event.drag(cx);
                    if payload.slot != sid {
                        return;
                    }
                    let from = payload.from;
                    let rel_y = f32::from(event.event.position.y)
                        - f32::from(event.bounds.top())
                        - f32::from(scroll_for_drag.offset().y);
                    let over = crate::terminal::panel::drop_index(rel_y, SLOT, count);
                    this.update_dock_tab_drag_over(sid, from, over, cx);
                },
            ))
            .on_drop::<DockTabDrag>(cx.listener(move |this, payload: &DockTabDrag, _, cx| {
                let Some(slot) = this.slots.get_mut(&sid) else {
                    return;
                };
                let drag = slot.dock.tab_drag.take();
                if payload.slot != sid {
                    cx.notify();
                    return;
                }
                let to = drag.map(|d| d.over).unwrap_or(payload.from);
                this.reorder_dock_tabs(sid, payload.from, to, cx);
            }));
        for (ix, (surface, title)) in rows.into_iter().enumerate() {
            let is_active = dock_open && surface == active;
            let icon_path = match surface {
                DockSurface::Diff(_) => icons::GIT_BRANCH,
                DockSurface::Files(_) => icons::FOLDER_WITH_FILES,
                _ => icons::CHAT_ROUND_LINE,
            };
            let group: SharedString = format!("dock-rail-tab-{sid}-{ix}").into();
            let ghost_title = title.clone();
            // A diff tab's title is its scope ("Working tree", a commit
            // subject) — name the surface too, the icon alone is ambiguous.
            let tooltip: SharedString = match surface {
                DockSurface::Diff(_) => format!("Git · {title}").into(),
                _ => title.clone(),
            };
            let button = div()
                .id(("dock-rail-tab", ix))
                .group(group.clone())
                .size(px(BUTTON))
                .flex_none()
                .relative()
                .rounded(px(RIGHT_TAB_RADIUS + 2.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                // Not `.occlude()`: a BlockMouse hitbox would end the hit
                // test before the scrolling column behind the buttons.
                .block_mouse_except_scroll()
                .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                    window.prevent_default()
                })
                // No fill, active or not (user request): the icon's tone
                // marks the active surface, and hover lifts an idle one.
                .tooltip(move |_, cx| {
                    cx.new(|_| super::session::FindTooltip(tooltip.clone()))
                        .into()
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.ensure_dock_open(sid, cx);
                    this.set_dock_active(sid, surface, cx);
                }))
                // Middle-click closes, like every tab strip.
                .on_mouse_down(
                    gpui::MouseButton::Middle,
                    cx.listener(move |this, _, _, cx| {
                        this.close_dock_surface(sid, surface, cx);
                    }),
                )
                .on_drag(
                    DockTabDrag {
                        slot: sid,
                        from: ix,
                        title: ghost_title,
                    },
                    |payload, _point, _, cx| {
                        let title = payload.title.clone();
                        cx.stop_propagation();
                        cx.new(|_| SurfaceTabGhost { title })
                    },
                )
                .child(
                    icon(icon_path)
                        .size(px(15.0))
                        .text_color(if is_active {
                            theme.text
                        } else {
                            theme.text_muted.opacity(0.55)
                        })
                        .when(!is_active, |el| {
                            el.group_hover(group.clone(), |s| s.text_color(theme.text_muted))
                        }),
                )
                // Hover ✕ badge in the corner — closes this surface.
                .child(
                    div()
                        .id(("dock-rail-close", ix))
                        // Inside the button: the scrolling column clips
                        // anything past its edge.
                        .absolute()
                        .top(px(1.0))
                        .right(px(1.0))
                        .size(px(11.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme.surface_raised)
                        .border_1()
                        .border_color(theme.border_strong)
                        .opacity(0.0)
                        .group_hover(group.clone(), |s| s.opacity(1.0))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_dock_surface(sid, surface, cx);
                        }))
                        .child(
                            icon(icons::CLOSE)
                                .size(px(7.0))
                                .text_color(theme.text_muted),
                        ),
                );
            // Sliding transform while a sibling drags over (the terminal
            // drawer's recipe, vertical): animate 150ms between committed
            // offsets; the dragged button leaves an invisible spacer — the
            // ghost carries it.
            let wrapped: AnyElement = match drag {
                Some((from, over, epoch, prev_over)) if ix != from => {
                    let target = crate::terminal::panel::slide_offset(ix, from, over) * SLOT;
                    let start = crate::terminal::panel::slide_offset(ix, from, prev_over) * SLOT;
                    div()
                        .relative()
                        .child(button.with_animation(
                            ("dock-rail-slide", (ix as u64) | ((epoch as u64) << 32)),
                            TAB_SLIDE.animation(),
                            move |el, t| el.top(px(motion::lerp(start, target, t))),
                        ))
                        .into_any_element()
                }
                Some((from, ..)) if ix == from => {
                    div().size(px(BUTTON)).flex_none().into_any_element()
                }
                _ => button.into_any_element(),
            };
            tabs = tabs.child(wrapped);
        }
        // The `+` — a small menu offering the two surfaces (t3 "Add panel
        // surface"); mirrors the picker cards. Right-aligned: the rail hugs
        // the dock's right edge.
        let plus_open = self.right_plus.get() == Some(&sid);
        let plus_group: SharedString = format!("right-surface-add-{sid}").into();
        let mut plus = div()
            .id(("right-surface-add", sid))
            .size(px(BUTTON))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(RIGHT_TAB_RADIUS + 2.0))
            .cursor_pointer()
            .group(plus_group.clone())
            // No tooltip over its own open menu.
            .when(!plus_open, |el| {
                el.tooltip(|_, cx| {
                    cx.new(|_| super::session::FindTooltip("Open a surface".into()))
                        .into()
                })
            })
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.right_plus.note_trigger_press();
                }),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if this.right_plus.take_press_was_open() {
                    this.close_right_plus(cx);
                } else {
                    this.right_plus.open(sid);
                    cx.notify();
                }
            }))
            .child(
                icon(icons::PLUS)
                    .size(px(15.0))
                    .text_color(if plus_open {
                        theme.text
                    } else {
                        theme.text_muted.opacity(0.55)
                    })
                    .group_hover(plus_group.clone(), |s| s.text_color(theme.text_muted)),
            );
        if plus_open {
            let theme = Theme::of(cx).clone();
            let closing = self.right_plus.closing_since();
            let menu = popover::popover_card(&theme)
                .w(px(168.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_right_plus(cx)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .when(files_available, |el| {
                            el.child(
                                popover::menu_row(&theme, false, "right-plus-files")
                                    .id("right-plus-files-row")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.ensure_dock_open(sid, cx);
                                        this.add_files_surface(sid, cx);
                                        this.close_right_plus(cx);
                                    }))
                                    .child(
                                        icon(icons::FOLDER_WITH_FILES)
                                            .size(px(13.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child(SharedString::from("Files")),
                            )
                        })
                        // Git only where there IS git — a non-git project's
                        // diff surface would open a dead pane (same gate as
                        // the empty-surface picker card).
                        .when(git, |el| {
                            el.child(
                                popover::menu_row(&theme, false, "right-plus-diff")
                                    .id("right-plus-diff-row")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.ensure_dock_open(sid, cx);
                                        this.add_diff_surface(sid, cx);
                                        this.close_right_plus(cx);
                                    }))
                                    .child(
                                        icon(icons::GIT_BRANCH)
                                            .size(px(13.0))
                                            .text_color(theme.text_muted),
                                    )
                                    // "Git", not "Git diff" — the surface hosts
                                    // history and per-commit views too (user
                                    // request; matches the picker card).
                                    .child(SharedString::from("Git")),
                            )
                        }),
                )
                .into_any_element();
            plus = plus.relative().child(popover::anchored_menu_below_end(
                "right-plus-menu",
                menu,
                closing,
            ));
        }
        let raised_bg = Theme::of(cx).surface_raised;
        let (on, off) = (theme.text, theme.text_muted.opacity(0.55));
        let hover = theme.text_muted;
        let tone = RailTone { on, off, hover };
        // The tile actions — moved here from the tab row (user request).
        let mut actions = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(GAP))
            .child(rail_button(
                format!("rail-terminal-{sid}").into(),
                icons::TERMINAL,
                terminal_open,
                "Terminal",
                tone,
                cx.listener(move |this, _, window, cx| this.toggle_terminal(sid, window, cx)),
            ));
        if has_dock {
            actions = actions.child(rail_button(
                format!("rail-dock-{sid}").into(),
                icons::SIDEBAR_MINIMALISTIC,
                dock_open,
                "Side panel",
                tone,
                cx.listener(move |this, _, _, cx| this.toggle_dock(sid, cx)),
            ));
        }
        if let Some(group) = group {
            actions = actions.child(rail_button(
                format!("rail-split-{sid}").into(),
                icons::DIFF_SPLIT,
                false,
                "Split right",
                tone,
                cx.listener(move |this, _, _, cx| {
                    this.workspace.focus(group);
                    this.split_focused(crate::workspace::Edge::Right, cx);
                }),
            ));
            if can_zoom {
                actions = actions.child(rail_button(
                    format!("rail-zoom-{sid}").into(),
                    icons::EXPAND_ARROWS,
                    zoomed,
                    if zoomed { "Restore tiles" } else { "Zoom tile" },
                    tone,
                    cx.listener(move |this, _, _, cx| {
                        this.workspace.toggle_zoom(group);
                        this.workspace_changed(cx);
                    }),
                ));
            }
        }
        let divider = div()
            .flex_none()
            .w(px(BUTTON - 10.0))
            .h(px(1.0))
            .my(px(2.0))
            .bg(theme.border);
        let card = div()
            .flex_none()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(GAP))
            .p(px(3.0))
            .rounded(px(RIGHT_TAB_RADIUS + 5.0))
            .bg(raised_bg)
            .shadow_sm()
            .child(actions)
            // The dock's surfaces + `+` (not on a canvas — no dock there).
            .when(has_dock, |el| {
                el.child(divider)
                    .when(count > 0, |el| el.child(tabs))
                    .child(plus)
            });
        let expand = dock_open.then(|| {
            header_icon_button(
                ("dock-expand", sid),
                icons::EXPAND_ARROWS,
                &theme,
                cx.listener(move |this, _, _, cx| this.toggle_dock_expand(sid, cx)),
            )
        });
        div()
            .flex_none()
            .w(px(RAIL_WIDTH))
            .h_full()
            .pt(px(top))
            .pb(px(RAIL_MARGIN))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(GAP))
            .child(card)
            .child(div().flex_1())
            .children(expand)
            .into_any_element()
    }
}

/// The rail's margin around its card (room for the card's shadow too).
pub(super) const RAIL_MARGIN: f32 = 6.0;

/// The rail's full width: its card (a 28px button + the card's 3px
/// padding each side) plus the margin on both sides.
pub(super) const RAIL_WIDTH: f32 = 28.0 + 6.0 + 2.0 * RAIL_MARGIN;

/// Icon tones for the rail's buttons: the active one reads in full text
/// colour, idle ones lighter, lifting on hover — no fills (user request).
#[derive(Clone, Copy)]
struct RailTone {
    on: gpui::Hsla,
    off: gpui::Hsla,
    hover: gpui::Hsla,
}

/// One rail action button (terminal / dock / split / zoom).
fn rail_button(
    id: SharedString,
    icon_path: &'static str,
    on: bool,
    tip: &'static str,
    tone: RailTone,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.clone())
        .group(id.clone())
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(RIGHT_TAB_RADIUS + 2.0))
        .cursor_pointer()
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .tooltip(move |_, cx| cx.new(|_| super::session::FindTooltip(tip.into())).into())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(
            icon(icon_path)
                .size(px(15.0))
                .text_color(if on { tone.on } else { tone.off })
                .when(!on, |el| {
                    el.group_hover(id, move |s| s.text_color(tone.hover))
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_surface_reorder_moves_mixed_strips() {
        // The surface strip mixes diffs, files, and side chats;
        // the drag-drop reorder must move them as one ordered list.
        let mut tabs = vec![
            DockSurface::Diff(1),
            DockSurface::Files(2),
            DockSurface::SideChat(3),
            DockSurface::Diff(4),
            DockSurface::SideChat(5),
        ];
        // Drag the first side chat (index 2) to the end.
        assert!(reorder_dock_surfaces(&mut tabs, 2, 4));
        assert_eq!(
            tabs,
            vec![
                DockSurface::Diff(1),
                DockSurface::Files(2),
                DockSurface::Diff(4),
                DockSurface::SideChat(5),
                DockSurface::SideChat(3),
            ]
        );
        // Drag it back to the front.
        assert!(reorder_dock_surfaces(&mut tabs, 4, 0));
        assert_eq!(tabs[0], DockSurface::SideChat(3));
        assert_eq!(tabs[1], DockSurface::Diff(1));
        // Out-of-bounds and same-index drags are no-ops (no notify).
        let before = tabs.clone();
        assert!(!reorder_dock_surfaces(&mut tabs, 9, 1));
        assert!(!reorder_dock_surfaces(&mut tabs, 1, 1));
        assert!(!reorder_dock_surfaces(&mut tabs, 1, 9));
        assert_eq!(tabs, before);
    }

    #[test]
    fn a_new_dock_is_closed_on_the_picker() {
        let dock = Dock::new();
        assert!(!dock.open && !dock.expanded);
        assert_eq!(dock.active, DockSurface::Picker);
        assert!(dock.surfaces.is_empty());
    }

    #[test]
    fn dock_width_keeps_the_chat_minimum() {
        let min_area = session::CHAT_MIN_WIDTH + DOCK_MIN_WIDTH;
        // Roomy: the preferred width stands.
        assert_eq!(dock_width(1400.0, 520.0, false), 520.0);
        // Narrower: capped so the chat keeps its minimum.
        assert_eq!(
            dock_width(min_area + 40.0, 520.0, false),
            DOCK_MIN_WIDTH + 40.0
        );
        // Below the absolute minimum the preference is raised.
        assert_eq!(dock_width(1400.0, 100.0, false), DOCK_MIN_WIDTH);
        // Expanded: the whole area.
        assert_eq!(dock_width(1400.0, 520.0, true), 1400.0);
    }

    #[test]
    fn dock_sizes_are_fractions_of_the_session_area() {
        // A remembered share scales with the tile…
        assert_eq!(fraction_of(500.0, 1000.0), Some(0.5));
        assert_eq!(preferred_size(Some(0.5), 520.0, 1600.0), 800.0);
        assert_eq!(
            dock_width(1600.0, preferred_size(Some(0.5), 520.0, 1600.0), false),
            800.0
        );
        // …never dragged, the legacy global width applies.
        assert_eq!(preferred_size(None, 520.0, 1600.0), 520.0);
        // Unmeasured areas remember nothing; absurd shares are clamped.
        assert_eq!(fraction_of(500.0, 0.0), None);
        assert_eq!(
            fraction_of(5000.0, 1000.0),
            Some(crate::prefs::DOCK_FRACTION_MAX)
        );
        // The chat column keeps its minimum; the dock keeps its own.
        let area = 1000.0;
        let wide = preferred_size(Some(0.9), 520.0, area);
        assert_eq!(
            dock_width(area, wide, false),
            area - session::CHAT_MIN_WIDTH
        );
        let slim = preferred_size(Some(0.1), 520.0, area);
        assert_eq!(dock_width(area, slim, false), DOCK_MIN_WIDTH);
    }

    #[test]
    fn terminal_height_is_a_clamped_fraction() {
        assert_eq!(terminal_dock_height(Some(0.4), 280.0, 1000.0), 400.0);
        assert_eq!(terminal_dock_height(None, 280.0, 1000.0), 280.0);
        // Never below the minimum, never above the viewport-relative cap.
        assert_eq!(
            terminal_dock_height(Some(0.05), 280.0, 1000.0),
            crate::prefs::TERMINAL_MIN_HEIGHT
        );
        assert_eq!(
            terminal_dock_height(Some(0.95), 280.0, 1000.0),
            1000.0 * crate::prefs::TERMINAL_MAX_VH
        );
    }

    #[test]
    fn a_short_tile_keeps_its_composer_above_the_terminal() {
        let bar = crate::terminal::panel::TAB_BAR_HEIGHT + 1.0;
        // Roomy: the terminal keeps its height; the transcript gets the rest.
        assert_eq!(fit_terminal_height(280.0, 800.0, 200.0), 280.0);
        assert_eq!(fit_terminal_height(165.0, 365.0, 200.0), 165.0);
        // Short: it shrinks to the room under the 200px composer stack (the
        // transcript is already down to nothing)…
        assert_eq!(fit_terminal_height(165.0, 330.0, 200.0), 130.0);
        assert_eq!(
            fit_terminal_height(165.0, 200.0 + bar + TERMINAL_BODY_MIN, 200.0),
            bar + TERMINAL_BODY_MIN
        );
        // …then collapses to its tab bar…
        assert_eq!(
            fit_terminal_height(165.0, 200.0 + bar + TERMINAL_BODY_MIN - 1.0, 200.0),
            bar
        );
        assert_eq!(fit_terminal_height(165.0, 200.0 + bar, 200.0), bar);
        // …then hides; the composer is never the one clipped.
        assert_eq!(fit_terminal_height(165.0, 200.0 + bar - 1.0, 200.0), 0.0);
        assert_eq!(fit_terminal_height(165.0, 150.0, 200.0), 0.0);
        for area in [120.0, 230.0, 260.0, 330.0, 500.0] {
            let terminal = fit_terminal_height(165.0, area, 200.0);
            assert!(area - terminal >= 200.0_f32.min(area), "{area}: {terminal}");
        }
    }

    #[test]
    fn a_narrow_area_hands_the_dock_the_whole_tile() {
        let min_area = session::CHAT_MIN_WIDTH + DOCK_MIN_WIDTH;
        assert!(dock_takes_over(min_area - 1.0));
        assert!(!dock_takes_over(min_area));
        assert_eq!(dock_width(min_area - 1.0, 520.0, false), min_area - 1.0);
        // A half-window tile (~700px) keeps its chat beside the dock.
        assert!(!dock_takes_over(700.0));
        assert!(dock_width(700.0, 520.0, false) <= 700.0 - session::CHAT_MIN_WIDTH);
    }
}
