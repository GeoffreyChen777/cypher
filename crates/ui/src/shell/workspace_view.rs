//! The workspace column (docs/design/workspace-layout.md): the tree of splits whose
//! leaves are tile cards. Each tile has a session tab bar and shows its
//! active tab's session area ([`super::session`]); split gaps carry drag
//! handles; the titlebar cluster's Layout popover applies presets. Session
//! tabs (and sidebar rows) drag onto a tab bar (join / reorder), a tile's
//! centre (join) or its edge (split).

use super::*;
use crate::workspace::{Axis, Drop, Edge, GroupId, Node, Preset, TabKey, Workspace};

/// Gap between tiles — the split handle lives in it.
pub(super) const SPLIT_GAP: f32 = 8.0;
/// A split handle never shrinks a tile below this.
const MIN_TILE_PX: f32 = 200.0;
/// Tile header (session tab bar) height: a top-row tile's header sits in the
/// titlebar band (the workspace is inset `PANEL_EDGE_INSET` from the top).
/// The tile's tab row matches the terminal dock's tab bar (row and chip
/// heights), so both strips in a tile read as one system.
pub(super) const TILE_HEADER_HEIGHT: f32 = crate::terminal::panel::TAB_BAR_HEIGHT;
const TILE_TAB_HEIGHT: f32 = crate::terminal::panel::TAB_HEIGHT;
const TILE_TAB_MAX_WIDTH: f32 = 200.0;
/// Browser-style tabs shrink evenly down to this (icon + a few letters),
/// then the strip scrolls.
const TILE_TAB_MIN_WIDTH: f32 = 84.0;
/// The tab strip's edge fades while tabs hide past an edge.
const TILE_TAB_FADE: f32 = 24.0;

/// The outer band of a tile body, per side, that splits instead of joins.
const DROP_EDGE_BAND: f32 = 0.25;

/// Right inset for the tab row. The rail floats over the body and does
/// not take column width; the tabs still stop where its card begins,
/// unless a Windows caption inset already clears that corner (the rail
/// starts below the header there).
fn tile_header_right_inset(touch: Touch, rail: bool, is_windows: bool) -> f32 {
    let base = if touch.top && touch.right {
        titlebar_right_padding(is_windows, 4.0)
    } else {
        4.0
    };
    let rail_top = if touch.top && touch.right && is_windows {
        Theme::TITLEBAR_HEIGHT
    } else {
        super::dock::RAIL_MARGIN
    };
    // 8px: the rail's top padding plus the card's own padding. Past the
    // header, the buttons no longer cover the tab row.
    if rail && rail_top + 8.0 < TILE_HEADER_HEIGHT {
        base + super::dock::RAIL_WIDTH
    } else {
        base
    }
}

/// A dragged session tab — from a tile's tab bar or a sidebar row.
pub(super) struct TabDrag {
    tab: TabKey,
    title: SharedString,
    harness: Option<cypher_proto::HarnessId>,
}

impl TabDrag {
    pub(super) fn new(
        tab: TabKey,
        title: SharedString,
        harness: Option<cypher_proto::HarnessId>,
    ) -> Self {
        Self {
            tab,
            title,
            harness,
        }
    }

    /// The `on_drag` constructor: the ghost chip. Stops propagation so an
    /// enclosing draggable (a project card) doesn't start its own drag.
    pub(super) fn ghost(
        &self,
        offset: Point<Pixels>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Entity<TabDragGhost> {
        cx.stop_propagation();
        let (title, harness) = (self.title.clone(), self.harness);
        cx.new(|_| TabDragGhost {
            title,
            harness,
            offset,
        })
    }
}

/// Ghost chip following the pointer while a session tab drags. gpui places
/// the ghost at the pointer minus the grab offset within the source; the
/// chip is padded back so the pointer sits near its leading edge even when
/// the source is a wide sidebar row.
pub(super) struct TabDragGhost {
    title: SharedString,
    harness: Option<cypher_proto::HarnessId>,
    offset: Point<Pixels>,
}

impl Render for TabDragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let (x, y) = (f32::from(self.offset.x), f32::from(self.offset.y));
        div()
            .pl(px(x - x.min(20.0)))
            .pt(px(y - y.min(TILE_TAB_HEIGHT / 2.0)))
            .child(
                div()
                    .h(px(TILE_TAB_HEIGHT))
                    .max_w(px(TILE_TAB_MAX_WIDTH))
                    .pl(px(8.0))
                    .pr(px(10.0))
                    .rounded(px(6.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .bg(theme.surface_raised)
                    .border_1()
                    .border_color(theme.border_strong)
                    .shadow_md()
                    .opacity(0.92)
                    .when_some(
                        self.harness.map(crate::pickers::harness_brand_icon),
                        |el, (path, tint)| {
                            el.child(
                                icon(path)
                                    .size(px(13.0))
                                    .flex_none()
                                    .text_color(tint.unwrap_or(theme.text_muted)),
                            )
                        },
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(self.title.clone()),
                    ),
            )
    }
}

/// A live session-tab drag: the tile (and drop) under the pointer, if any.
pub(super) struct TabDropState {
    hover: Option<(GroupId, Drop)>,
}

/// Where a dragged tab lands in a group: a body drop zone, or a tab-bar
/// position (`usize::MAX` appends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Zone(Drop),
    At(usize),
}

/// The drop zone of a tile body under `pointer`: the nearest side whose
/// outer [`DROP_EDGE_BAND`] holds it, else the centre. `None` outside.
/// Pure — unit-tested.
pub fn drop_zone(bounds: gpui::Bounds<Pixels>, pointer: Point<Pixels>) -> Option<Drop> {
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    if w <= 0.0 || h <= 0.0 || !bounds.contains(&pointer) {
        return None;
    }
    let fx = (f32::from(pointer.x) - f32::from(bounds.origin.x)) / w;
    let fy = (f32::from(pointer.y) - f32::from(bounds.origin.y)) / h;
    let (edge, distance) = [
        (Edge::Left, fx),
        (Edge::Right, 1.0 - fx),
        (Edge::Top, fy),
        (Edge::Bottom, 1.0 - fy),
    ]
    .into_iter()
    .fold((Edge::Left, f32::INFINITY), |best, side| {
        if side.1 < best.1 { side } else { best }
    });
    Some(if distance < DROP_EDGE_BAND {
        Drop::Edge(edge)
    } else {
        Drop::Center
    })
}

/// What dropping `tab` on `target`'s `zone` does, `None` when nothing would
/// change: an empty tile always takes the tab in place; its own tile's
/// centre, or an edge of a tile holding only it, is a no-op. Pure.
pub fn effective_drop(
    workspace: &Workspace,
    tab: &TabKey,
    target: GroupId,
    zone: Drop,
) -> Option<Drop> {
    let group = workspace.group(target)?;
    if group.is_empty() {
        return Some(Drop::Center);
    }
    let own = group.tabs().contains(tab);
    match zone {
        Drop::Center if own => None,
        Drop::Edge(_) if own && group.tabs().len() == 1 => None,
        zone => Some(zone),
    }
}

/// Apply a tab drop: an open tab moves (its slot follows it — slots are
/// keyed by tab), a closed session opens there. Focuses the destination.
/// Returns the tab's group.
pub fn route_drop(
    workspace: &mut Workspace,
    tab: TabKey,
    target: GroupId,
    placement: Placement,
) -> Option<GroupId> {
    workspace.group(target)?;
    let placement = match placement {
        Placement::Zone(zone) => {
            Placement::Zone(effective_drop(workspace, &tab, target, zone).unwrap_or(Drop::Center))
        }
        at => at,
    };
    if workspace.contains(&tab) {
        match placement {
            Placement::Zone(drop) => workspace.move_tab(&tab, target, drop),
            Placement::At(index) => workspace.move_to_index(&tab, target, index),
        };
    } else {
        match placement {
            Placement::Zone(Drop::Center) => {
                workspace.open_in(target, tab.clone());
            }
            Placement::Zone(Drop::Edge(edge)) => {
                workspace.open_split(tab.clone(), target, edge);
            }
            Placement::At(index) => {
                let group = workspace.open_in(target, tab.clone());
                workspace.move_to_index(&tab, group, index);
            }
        }
    }
    workspace.find(&tab).map(|(group, _)| group)
}

/// The translucent preview of a drop's resulting area over a tile body.
fn drop_zone_overlay(drop: Drop, accent: gpui::Hsla) -> gpui::Div {
    let half = gpui::relative(0.5);
    let zone = div().absolute().p(px(4.0));
    let zone = match drop {
        Drop::Center => zone.inset_0(),
        Drop::Edge(Edge::Left) => zone.left_0().top_0().bottom_0().w(half),
        Drop::Edge(Edge::Right) => zone.right_0().top_0().bottom_0().w(half),
        Drop::Edge(Edge::Top) => zone.top_0().left_0().right_0().h(half),
        Drop::Edge(Edge::Bottom) => zone.bottom_0().left_0().right_0().h(half),
    };
    zone.child(
        div()
            .size_full()
            .rounded(px(8.0))
            .bg(accent.opacity(0.12))
            .border_1()
            .border_color(accent.opacity(0.45)),
    )
}

/// Drag marker for a split handle: the split's tree path and the boundary
/// (between children `boundary` and `boundary + 1`) it moves.
pub(super) struct SplitResize {
    path: Vec<usize>,
    boundary: usize,
}

/// Which window edges a tile touches (titlebar band, traffic lights,
/// Windows caption controls).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Touch {
    pub(super) top: bool,
    pub(super) left: bool,
    pub(super) right: bool,
}

impl Touch {
    pub(super) const ALL: Self = Self {
        top: true,
        left: true,
        right: true,
    };

    /// Child `index` of `count` in a split along `axis`.
    pub(super) fn child(self, axis: Axis, index: usize, count: usize) -> Self {
        let first = index == 0;
        let last = index + 1 == count;
        match axis {
            Axis::Horizontal => Self {
                top: self.top,
                left: self.left && first,
                right: self.right && last,
            },
            Axis::Vertical => Self {
                top: self.top && first,
                left: self.left,
                right: self.right,
            },
        }
    }
}

/// Sidebar click / Ctrl+Tab routing: a session open anywhere is activated
/// and focused where it is; otherwise it opens in the focused group, or —
/// `split` (⌘-click) — in a new group to the right of it. An EMPTY focused
/// group takes the tab in place even for a split (a split would leave a dead
/// tile beside it). Returns the tab's group.
pub fn route_open(workspace: &mut Workspace, tab: TabKey, split: bool) -> GroupId {
    if let Some((group, index)) = workspace.find(&tab) {
        workspace.activate(group, index);
        return group;
    }
    let focused = workspace.focused();
    let focused_empty = workspace.group(focused).is_some_and(|g| g.is_empty());
    if split
        && !focused_empty
        && let Some(group) = workspace.open_split(tab.clone(), focused, Edge::Right)
    {
        return group;
    }
    workspace.open(tab)
}

/// A split handle drag, as a fraction of the split's content size: how far
/// `pointer` (window px along the split axis) is from the current centre of
/// the gap after child `boundary`. `origin`/`extent` are the split
/// container's measured position and size; `SPLIT_GAP` sits between
/// children. Pure — unit-tested.
pub fn split_drag_delta(
    fractions: &[f32],
    boundary: usize,
    origin: f32,
    extent: f32,
    pointer: f32,
) -> f32 {
    if boundary + 1 >= fractions.len() {
        return 0.0;
    }
    let gaps = (fractions.len() - 1) as f32;
    let content = extent - gaps * SPLIT_GAP;
    if content <= 1.0 {
        return 0.0;
    }
    let before: f32 = fractions[..=boundary].iter().sum();
    let centre = origin + before * content + boundary as f32 * SPLIT_GAP + SPLIT_GAP / 2.0;
    (pointer - centre) / content
}

/// The minimum fraction keeping a tile at least [`MIN_TILE_PX`] in a split
/// `extent` px long with `count` children.
pub fn min_tile_fraction(extent: f32, count: usize) -> f32 {
    let content = extent - count.saturating_sub(1) as f32 * SPLIT_GAP;
    if content <= 1.0 {
        0.0
    } else {
        (MIN_TILE_PX / content).min(0.5)
    }
}

pub(super) fn preset_label(preset: Preset) -> &'static str {
    match preset {
        Preset::Single => "Single",
        Preset::Columns2 => "2 columns",
        Preset::Rows2 => "2 rows",
        Preset::Columns3 => "3 columns",
        Preset::Rows3 => "3 rows",
        Preset::Grid2x2 => "2 × 2 grid",
        Preset::Grid3x3 => "3 × 3 grid",
        Preset::TwoStackedPlusOne => "Two stacked + one",
        Preset::OnePlusTwoStacked => "One + two stacked",
    }
}

/// A tiny drawn preview of a preset: its tiles as outlined cells.
fn preset_glyph(preset: Preset, color: gpui::Hsla) -> AnyElement {
    let cell = move || {
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .rounded(px(1.5))
            .border_1()
            .border_color(color)
    };
    let row = |n: usize| {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .gap(px(1.5))
            .children((0..n).map(|_| cell()))
    };
    let col = |n: usize| {
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(1.5))
            .children((0..n).map(|_| cell()))
    };
    let frame = div()
        .w(px(18.0))
        .h(px(13.0))
        .flex_none()
        .flex()
        .gap(px(1.5));
    match preset {
        Preset::Single => frame.child(cell()),
        Preset::Columns2 => frame.flex_row().child(cell()).child(cell()),
        Preset::Columns3 => frame.flex_row().child(cell()).child(cell()).child(cell()),
        Preset::Rows2 => frame.flex_col().child(cell()).child(cell()),
        Preset::Rows3 => frame.flex_col().child(cell()).child(cell()).child(cell()),
        Preset::Grid2x2 => frame.flex_col().child(row(2)).child(row(2)),
        Preset::Grid3x3 => frame.flex_col().child(row(3)).child(row(3)).child(row(3)),
        Preset::TwoStackedPlusOne => frame.flex_row().child(col(2)).child(cell()),
        Preset::OnePlusTwoStacked => frame.flex_row().child(cell()).child(col(2)),
    }
    .into_any_element()
}

impl Shell {
    // ---- workspace actions ----

    /// Focus a tile (mouse down anywhere in it). Keyboard focus stays with
    /// whatever the click lands on.
    pub(super) fn focus_group(&mut self, group: GroupId, cx: &mut Context<Self>) {
        if self.workspace.focused() != group && self.workspace.focus(group) {
            self.workspace_changed(cx);
        }
    }

    fn activate_tab(&mut self, group: GroupId, index: usize, cx: &mut Context<Self>) {
        if self.workspace.activate(group, index) {
            self.focus_pending = true;
            self.workspace_changed(cx);
        }
    }

    /// Close a tab (its slot goes with it).
    pub(super) fn close_tab(&mut self, tab: &TabKey, cx: &mut Context<Self>) {
        if self.workspace.close(tab) {
            self.focus_pending = true;
            self.workspace_changed(cx);
        }
    }

    /// Split the focused tile with a new empty one and focus it — the next
    /// sidebar pick opens there.
    /// Chat-scoped like the other tile verbs: a rebindable key pressed on
    /// the Settings → Shortcuts page must not yank the user off it.
    pub(super) fn split_focused(&mut self, edge: Edge, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Chat) {
            return;
        }
        let focused = self.workspace.focused();
        if self.workspace.split_group(focused, edge).is_some() {
            self.focus_pending = true;
            self.workspace_changed(cx);
        }
    }

    pub(super) fn focus_neighbour(&mut self, edge: Edge, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Chat) || self.workspace.zoomed().is_some() {
            return;
        }
        if let Some(group) = self.workspace.neighbour(self.workspace.focused(), edge) {
            self.workspace.focus(group);
            self.focus_pending = true;
            self.workspace_changed(cx);
        }
    }

    /// ⌘1…⌘9: focus the `index`th tile in reading order (a zoomed tile
    /// unzooms first so the target is on screen).
    pub(super) fn focus_tile(&mut self, index: usize, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Chat) {
            return;
        }
        let Some(&group) = self.workspace.groups_in_reading_order().get(index) else {
            return;
        };
        if let Some(zoomed) = self.workspace.zoomed()
            && zoomed != group
        {
            self.workspace.toggle_zoom(zoomed);
        }
        self.workspace.focus(group);
        self.focus_pending = true;
        self.workspace_changed(cx);
    }

    pub(super) fn close_focused_tab(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Chat) {
            return;
        }
        if let Some(tab) = self.workspace.focused_tab().cloned() {
            self.close_tab(&tab, cx);
        }
    }

    pub(super) fn toggle_zoom_focused(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Chat) {
            return;
        }
        self.workspace.toggle_zoom(self.workspace.focused());
        self.workspace_changed(cx);
    }

    /// "Close split": the focused tile goes, its tabs join the neighbour
    /// that inherits its space.
    fn close_focused_group(&mut self, cx: &mut Context<Self>) {
        if self.workspace.close_group(self.workspace.focused()) {
            self.focus_pending = true;
            self.workspace_changed(cx);
        }
    }

    /// Presets only move tabs (no slot is created or dropped); empty groups
    /// show the empty-tile picker.
    fn apply_layout_preset(&mut self, preset: Preset, cx: &mut Context<Self>) {
        self.workspace.apply_preset(preset);
        self.workspace_changed(cx);
    }

    /// View → Layout: a preset from anywhere lands back on the workspace.
    pub(super) fn apply_layout_from_menu(&mut self, preset: Preset, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        self.apply_layout_preset(preset, cx);
    }

    fn close_layout_menu(&mut self, cx: &mut Context<Self>) {
        if self.layout_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.layout_menu);
        }
        cx.notify();
    }

    pub(super) fn on_split_drag(
        &mut self,
        event: &gpui::DragMoveEvent<SplitResize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = event.drag(cx);
        let (path, boundary) = (drag.path.clone(), drag.boundary);
        let Some(bounds) = self.split_bounds.borrow().get(&path).copied() else {
            return;
        };
        let Some(Node::Split { axis, children }) = self.workspace.root().at(&path) else {
            return;
        };
        let fractions: Vec<f32> = children.iter().map(|(_, f)| *f).collect();
        let (origin, extent, pointer) = match axis {
            Axis::Horizontal => (
                f32::from(bounds.origin.x),
                f32::from(bounds.size.width),
                f32::from(event.event.position.x),
            ),
            Axis::Vertical => (
                f32::from(bounds.origin.y),
                f32::from(bounds.size.height),
                f32::from(event.event.position.y),
            ),
        };
        let delta = split_drag_delta(&fractions, boundary, origin, extent, pointer);
        let min = min_tile_fraction(extent, fractions.len());
        if self.workspace.resize(&path, boundary, delta, min) {
            self.save_layout(cx);
            cx.notify();
        }
    }

    /// A session tab drags over the window: track the tile body and drop
    /// zone under the pointer (the drop catchers mount on the next render).
    fn on_tab_drag_move(
        &mut self,
        event: &gpui::DragMoveEvent<TabDrag>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pointer = event.event.position;
        let tab = event.drag(cx).tab.clone();
        let hit = self
            .tile_bounds
            .borrow()
            .iter()
            .find_map(|(group, bounds)| drop_zone(*bounds, pointer).map(|zone| (*group, zone)));
        let hover = hit.and_then(|(group, zone)| {
            effective_drop(&self.workspace, &tab, group, zone).map(|drop| (group, drop))
        });
        if self.tab_drop.as_ref().map(|state| state.hover) != Some(hover) {
            self.tab_drop = Some(TabDropState { hover });
            cx.notify();
        }
    }

    /// Drop a session tab: route it into `group`, then focus it there.
    fn drop_tab(
        &mut self,
        drag: &TabDrag,
        group: GroupId,
        placement: Placement,
        cx: &mut Context<Self>,
    ) {
        self.tab_drop = None;
        self.route = Route::Chat;
        route_drop(&mut self.workspace, drag.tab.clone(), group, placement);
        self.focus_pending = true;
        self.workspace_changed(cx);
    }

    /// A drop on a tile body: the zone under the pointer at release.
    fn drop_tab_on_tile(
        &mut self,
        drag: &TabDrag,
        group: GroupId,
        pointer: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let zone = self
            .tile_bounds
            .borrow()
            .get(&group)
            .and_then(|bounds| drop_zone(*bounds, pointer))
            .unwrap_or(Drop::Center);
        self.drop_tab(drag, group, Placement::Zone(zone), cx);
    }

    // ---- rendering ----

    /// The workspace column: the zoomed tile alone, else the whole tree.
    pub(super) fn render_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // A drag that ended off every drop target leaves no catchers
        // behind; tile bodies re-measure at paint.
        if self.tab_drop.is_some() && !cx.has_active_drag() {
            self.tab_drop = None;
        }
        self.tile_bounds.borrow_mut().clear();
        let workspace = &self.workspace;
        self.tile_tab_scroll
            .retain(|group, _| workspace.group(*group).is_some());
        self.split_bounds
            .borrow_mut()
            .retain(|path, _| matches!(workspace.root().at(path), Some(Node::Split { .. })));
        let body = match self.workspace.zoomed() {
            Some(group) => self.render_group(group, Touch::ALL, window, cx),
            None => {
                let root = self.workspace.root().clone();
                self.render_node(&root, Vec::new(), Touch::ALL, window, cx)
            }
        };
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            // No `h_full`: the row stretches this column, and 100% height
            // plus the vertical margins overflowed the window by the
            // bottom inset (the bottom tiles ran into the window edge).
            // The card insets the old chat card had: 8px on the window
            // edges, 4px on the sidebar seam.
            .ml(px(4.0))
            .mt(px(PANEL_EDGE_INSET))
            .mb(px(PANEL_EDGE_INSET))
            .mr(px(PANEL_EDGE_INSET))
            .flex()
            .on_drag_move(cx.listener(Self::on_tab_drag_move))
            .child(body)
            .into_any_element()
    }

    fn render_node(
        &mut self,
        node: &Node,
        path: Vec<usize>,
        touch: Touch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            Node::Group(group) => self.render_group(*group, touch, window, cx),
            Node::Split { axis, children } => {
                let axis = *axis;
                let count = children.len();
                let measured = self.split_bounds.clone();
                let key = path.clone();
                let mut split = div()
                    .size_full()
                    .relative()
                    .flex()
                    .when(axis == Axis::Vertical, |el| el.flex_col())
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| {
                                measured.borrow_mut().insert(key.clone(), bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    );
                for (index, (child, fraction)) in children.iter().enumerate() {
                    if index > 0 {
                        split = split.child(self.render_split_handle(&path, axis, index - 1, cx));
                    }
                    let mut child_path = path.clone();
                    child_path.push(index);
                    let element = self.render_node(
                        child,
                        child_path,
                        touch.child(axis, index, count),
                        window,
                        cx,
                    );
                    // Basis 0 + grow by fraction: the free space (the split
                    // minus its fixed gaps) divides exactly by the fractions.
                    split = split.child(
                        div()
                            .flex_basis(px(0.0))
                            .flex_grow(*fraction)
                            .flex_shrink(1.0)
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .child(element),
                    );
                }
                split.into_any_element()
            }
        }
    }

    /// A split gap's drag handle (double-click equalizes the split).
    fn render_split_handle(
        &mut self,
        path: &[usize],
        axis: Axis,
        boundary: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = Theme::of(cx).border_strong;
        let id = SharedString::from(format!("split-handle-{path:?}-{boundary}"));
        let equalize_path = path.to_vec();
        let drag_path = path.to_vec();
        div()
            .id(id)
            .flex_none()
            .when(axis == Axis::Horizontal, |el| {
                el.w(px(SPLIT_GAP)).h_full().cursor_col_resize()
            })
            .when(axis == Axis::Vertical, |el| {
                el.h(px(SPLIT_GAP)).w_full().cursor_row_resize()
            })
            .child(
                div()
                    .size_full()
                    .rounded(px(2.0))
                    .hover(move |s| s.bg(hover.opacity(0.5))),
            )
            .on_drag(
                SplitResize {
                    path: drag_path,
                    boundary,
                },
                |_, _point: Point<Pixels>, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| DragGhost)
                },
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                    if event.click_count == 2 && this.workspace.equalize(&equalize_path) {
                        this.save_layout(cx);
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }

    /// One tile card: its session tab bar, then the active session's area
    /// (or the empty-tile picker). Mouse down anywhere in it focuses it.
    fn render_group(
        &mut self,
        group: GroupId,
        touch: Touch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let focused = self.workspace.focused() == group;
        let several = self.workspace.group_count() > 1 && self.workspace.zoomed().is_none();
        let active = self
            .workspace
            .group(group)
            .and_then(|g| g.active_tab().cloned());
        let sid = active.as_ref().and_then(|tab| self.slot_for_tab(tab));
        // Only the focused tile shows its rail (user request). It floats over
        // the tile, out of flow, so the entrance is a fade — growing its
        // width used to shove the transcript (and the dock) left.
        let rail_shown = sid.is_some() && focused;
        let header = self.render_tile_header(group, touch, focused, rail_shown, cx);
        let rail = sid.filter(|_| focused).map(|sid| {
            let top = if touch.top && touch.right && cfg!(target_os = "windows") {
                Theme::TITLEBAR_HEIGHT
            } else {
                super::dock::RAIL_MARGIN
            };
            if self.rail_focus.0 != Some(group) {
                self.rail_focus = (Some(group), self.rail_focus.1.wrapping_add(1));
            }
            let epoch = self.rail_focus.1;
            let rail = self.render_session_rail(sid, top, cx);
            // Absolute, fixed width: the fade cannot change anyone's layout.
            // A top-right Windows tile starts the buttons under the caption
            // controls (`top` above). The tab row keeps its own right inset.
            let wrapper = div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .w(px(super::dock::RAIL_WIDTH))
                .child(rail);
            if motion::reduced_motion(cx) {
                wrapper.into_any_element()
            } else {
                wrapper
                    .with_animation(
                        SharedString::from(format!("tile-rail-in-{}-{epoch}", group.0)),
                        motion::TAB_SLIDE.animation(),
                        |el, t| el.opacity(t),
                    )
                    .into_any_element()
            }
        });
        let body = match sid {
            Some(sid) => self.render_session(sid, window, cx),
            None => self.render_empty_tile(group, cx),
        };
        let measured = self.tile_bounds.clone();
        // While a session tab drags: a catcher over the body takes the drop
        // (and blocks the session's own hover effects) and previews it.
        let catcher = (self.tab_drop.is_some() && cx.has_active_drag()).then(|| {
            let hover = self
                .tab_drop
                .as_ref()
                .and_then(|state| state.hover)
                .filter(|(target, _)| *target == group)
                .map(|(_, drop)| drop);
            div()
                .id(("tile-drop", group.0))
                .absolute()
                .inset_0()
                .occlude()
                .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| {
                    this.drop_tab_on_tile(drag, group, window.mouse_position(), cx);
                }))
                .when_some(hover, |el, drop| {
                    el.child(motion::fade_quick(
                        SharedString::from(format!("tile-drop-zone-{}-{drop:?}", group.0)),
                        drop_zone_overlay(drop, theme.accent),
                    ))
                })
        });
        let card_bg = crate::appearance::chat_style::panel_background(
            crate::appearance::chat_style::settings(cx),
            Theme::of(cx),
            true,
        );
        div()
            .id(("tile", group.0))
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_row()
            .rounded(px(PANEL_CORNER_RADIUS))
            .bg(card_bg)
            .shadow_sm()
            .overflow_hidden()
            // The focused tile reads apart only when there is more than one
            // — kept quiet on purpose.
            .border_1()
            .border_color(if several && focused {
                theme.border
            } else {
                gpui::transparent_black()
            })
            .relative()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .flex()
                            .child(body)
                            .child(
                                gpui::canvas(
                                    move |bounds, _, _| {
                                        measured.borrow_mut().insert(group, bounds);
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .inset_0(),
                            )
                            .children(catcher),
                    ),
            )
            .children(rail)
            // Mouse down anywhere in the tile focuses it. A capture listener
            // on the card itself misses presses on occluding children (tab
            // chips, header and dock buttons block the hit test behind
            // them), so it lives on a topmost pass-through layer: a Normal
            // hitbox blocks nothing below it, and floating layers painted
            // later (popovers, menus) still shadow it.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                        this.focus_group(group, cx);
                    })),
            )
            .into_any_element()
    }

    /// The tile header: session tabs (scrolling when they overflow; the tile
    /// actions sit on the session rail). A top-row tile's header is the window's drag region
    /// and clears the traffic lights / caption controls at the corners.
    fn render_tile_header(
        &mut self,
        group: GroupId,
        touch: Touch,
        focused: bool,
        rail: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tabs: Vec<TabKey> = self
            .workspace
            .group(group)
            .map(|g| g.tabs().to_vec())
            .unwrap_or_default();
        let active_index = self.workspace.group(group).map_or(0, |g| g.active());
        // Browser-style strip: tabs shrink evenly to their minimum, then the
        // row scrolls. A newly active tab (opened, activated, or a tab count
        // change) is scrolled fully into view once.
        let (scroll, scrolled_to) = self
            .tile_tab_scroll
            .entry(group)
            .or_insert_with(|| (gpui::ScrollHandle::new(), None));
        let scroll = scroll.clone();
        let reveal = tabs.get(active_index).map(|tab| (tab.clone(), tabs.len()));
        if *scrolled_to != reveal {
            *scrolled_to = reveal;
            scroll.scroll_to_item(active_index);
        }
        let mut strip = div()
            .id(("tile-tabs", group.0))
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .overflow_x_scroll()
            .track_scroll(&scroll)
            // Past the last tab: join at the end.
            .on_drop(cx.listener(move |this, drag: &TabDrag, _, cx| {
                this.drop_tab(drag, group, Placement::At(usize::MAX), cx);
            }));
        for (index, tab) in tabs.iter().enumerate() {
            strip = strip.child(self.render_tile_tab(
                group,
                index,
                tab,
                index == active_index,
                focused,
                cx,
            ));
        }
        // The strip region takes what the buttons leave (they never shrink)
        // and clips there; tabs hidden past an edge fade out.
        let strip = div().flex_1().min_w_0().h_full().overflow_hidden().child(
            crate::kit::edge_fade::edge_faded(TILE_TAB_FADE, false, false, strip)
                .fade_left(true)
                .fade_right(true)
                .fade_overflow_x(&scroll),
        );

        // The top-left tile clears the window-control cluster (and the
        // traffic lights) while the sidebar is collapsed: the cluster
        // overlays the window's top-left, and the tile starts at the
        // sidebar's (animating) right edge plus the 4px seam.
        // Same inset as the terminal tab bar below, so the first tabs line up.
        let mut left = 6.0;
        if touch.top && touch.left {
            let sidebar_now = self.eval_tween(self.sidebar_tween, self.sidebar_target());
            let plus_inset = 26.0 * self.titlebar_plus_alpha();
            // + the cluster's Layout button slot.
            let cluster_end = self.title_bar_content_start() + plus_inset + 26.0;
            let tile_left = sidebar_now + 4.0;
            left = (cluster_end - tile_left).max(left);
        }
        let right = tile_header_right_inset(touch, rail, cfg!(target_os = "windows"));
        // The tile actions live on the session's right-edge rail now.
        let header = div()
            .flex_none()
            .h(px(TILE_HEADER_HEIGHT))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pl(px(left))
            .pr(px(right))
            .child(strip);
        if touch.top {
            self.titlebar_drag_region(("tile-header", group.0), header, cx)
                .into_any_element()
        } else {
            header.into_any_element()
        }
    }

    fn render_tile_tab(
        &mut self,
        group: GroupId,
        index: usize,
        tab: &TabKey,
        active: bool,
        tile_focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let (title, harness, status, target): (
            SharedString,
            Option<cypher_proto::HarnessId>,
            ChatIndicator,
            Option<SharedString>,
        ) = {
            let state = self.state.read(cx);
            match tab
                .chat_id()
                .and_then(|id| state.chats.iter().find(|c| c.id == id))
            {
                Some(chat) => {
                    let folder = chat
                        .space_id
                        .as_deref()
                        .and_then(|id| state.space_row(id))
                        .map(|s| s.display_name().to_string())
                        .unwrap_or_else(|| "~".to_string());
                    let device = state
                        .device_name(&chat.device_id)
                        .unwrap_or("Unknown device");
                    (
                        transcript::single_line(
                            &chat.title.clone().unwrap_or_else(|| "New session".into()),
                        )
                        .into(),
                        chat.config.as_ref().map(|c| c.harness),
                        state.display_status_for(chat, now),
                        Some(format!("{folder} @ {device}").into()),
                    )
                }
                None => ("New session".into(), None, ChatIndicator::Idle, None),
            }
        };
        let drag = TabDrag::new(tab.clone(), title.clone(), harness);
        let accent = theme.accent;
        let hover_group: SharedString = format!("tile-tab-{}-{index}", group.0).into();
        let close_tab = tab.clone();
        let middle_tab = tab.clone();
        let status_mark: Option<AnyElement> = match status {
            ChatIndicator::Working => Some(
                div()
                    .flex_none()
                    .child(loaders::mini_gradient_spinner(
                        format!("tile-tab-working-{}-{index}", group.0),
                        2.0,
                        cx.entity_id(),
                        cx,
                    ))
                    .into_any_element(),
            ),
            ChatIndicator::AwaitingInput => Some(
                icon(icons::QUESTION_CIRCLE)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(theme.warning)
                    .into_any_element(),
            ),
            ChatIndicator::Completed => Some(
                icon(icons::CHECK)
                    .size(px(11.0))
                    .flex_none()
                    .text_color(theme.success.opacity(0.9))
                    .into_any_element(),
            ),
            _ => None,
        };
        div()
            .id(SharedString::from(format!("tile-tab-{}-{index}", group.0)))
            .group(hover_group.clone())
            .h(px(TILE_TAB_HEIGHT))
            // Prefer the max width, shrink evenly with the siblings down to
            // the minimum (the strip scrolls past that).
            .flex_basis(px(TILE_TAB_MAX_WIDTH))
            .flex_shrink(1.0)
            .min_w(px(TILE_TAB_MIN_WIDTH))
            .overflow_hidden()
            .pl(px(8.0))
            .pr(px(4.0))
            .rounded(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            // Scrollable strip: block the drag region, not the wheel.
            .block_mouse_except_scroll()
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            // Only the focused tile's active tab carries the full wash; the
            // other tiles' active tabs keep a faint one.
            .when(active, |el| {
                el.bg(crate::kit::theme::wash(if tile_focused {
                    0.10
                } else {
                    0.035
                }))
            })
            .when(!active, |el| {
                el.hover(|s| s.bg(crate::kit::theme::wash(0.06)))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.activate_tab(group, index, cx);
            }))
            // Drag to reorder, onto another tile's bar / centre (join) or
            // edge (split); a drop here lands at this tab's position.
            .on_drag(drag, TabDrag::ghost)
            .drag_over::<TabDrag>(move |style, _, _, _| style.bg(accent.opacity(0.18)))
            .on_drop(cx.listener(move |this, drag: &TabDrag, _, cx| {
                this.drop_tab(drag, group, Placement::At(index), cx);
            }))
            // Middle-click closes, like every tab strip.
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, _, cx| this.close_tab(&middle_tab, cx)),
            )
            .when_some(
                harness.map(crate::pickers::harness_brand_icon),
                |el, (path, tint)| {
                    el.child(
                        icon(path)
                            .size(px(13.0))
                            .flex_none()
                            .text_color(tint.unwrap_or(theme.text_muted)),
                    )
                },
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(if active {
                        theme.text.opacity(0.9)
                    } else {
                        theme.text_muted
                    })
                    .child(title),
            )
            .children(status_mark)
            .child(
                div()
                    .id(SharedString::from(format!(
                        "tile-tab-close-{}-{index}",
                        group.0
                    )))
                    .flex_none()
                    .size(px(16.0))
                    .rounded(px(4.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(if active { 1.0 } else { 0.0 })
                    .group_hover(hover_group, |s| s.opacity(1.0))
                    .hover(|s| s.bg(crate::kit::theme::wash(0.12)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.close_tab(&close_tab, cx);
                    }))
                    .child(
                        icon(icons::CLOSE)
                            .size(px(10.0))
                            .text_color(theme.text_muted),
                    ),
            )
            .when_some(target, |el, target| {
                el.tooltip(move |_, cx| cx.new(|_| session::FindTooltip(target.clone())).into())
            })
            .into_any_element()
    }

    /// An empty tile: nothing open yet — start a session here, or pick one in
    /// the sidebar (it opens in the focused tile).
    fn render_empty_tile(&mut self, group: GroupId, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_muted)
                    .child(SharedString::from("No session")),
            )
            .child(
                popover::btn_primary(&theme, "New session")
                    .id(("empty-tile-new-session", group.0))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.workspace.focus(group);
                        this.open_new_session(cx);
                    })),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted.opacity(0.7))
                    .child(SharedString::from("or pick one in the sidebar")),
            )
            .into_any_element()
    }

    /// The titlebar cluster's Layout button + its presets popover.
    pub(super) fn render_layout_button(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut button = div().flex_none().relative().child(
            window_control_button(
                "titlebar-layout",
                icons::WINDOW_FRAME,
                theme,
                cx.listener(|this, _, _, cx| {
                    if this.layout_menu.take_press_was_open() {
                        this.close_layout_menu(cx);
                    } else {
                        this.layout_menu.open(());
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.layout_menu.note_trigger_press()),
            ),
        );
        if self.layout_menu.get().is_some() {
            let theme = Theme::of(cx).clone();
            let closing = self.layout_menu.closing_since();
            let muted = theme.text_muted;
            let mut rows = div().flex().flex_col().gap(px(2.0));
            for preset in Preset::ALL {
                rows = rows.child(
                    popover::menu_row(&theme, false, format!("layout-preset-{preset:?}"))
                        .id(SharedString::from(format!("layout-preset-{preset:?}-row")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.apply_layout_preset(preset, cx);
                            this.close_layout_menu(cx);
                        }))
                        .child(preset_glyph(preset, muted))
                        .child(SharedString::from(preset_label(preset))),
                );
            }
            let action = |id: &'static str, label: &'static str| {
                popover::menu_row(&theme, false, id)
                    .id(SharedString::from(format!("{id}-row")))
                    .child(SharedString::from(label))
            };
            let can_close = self.workspace.group_count() > 1;
            rows = rows
                .child(div().h(px(1.0)).my(px(4.0)).bg(theme.border))
                .child(
                    action("layout-split-right", "Split right").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.split_focused(Edge::Right, cx);
                            this.close_layout_menu(cx);
                        },
                    )),
                )
                .child(
                    action("layout-split-down", "Split down").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.split_focused(Edge::Bottom, cx);
                            this.close_layout_menu(cx);
                        },
                    )),
                )
                .when(can_close, |el| {
                    el.child(
                        action("layout-close-split", "Close split").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.close_focused_group(cx);
                                this.close_layout_menu(cx);
                            },
                        )),
                    )
                });
            let menu = popover::popover_card(&theme)
                .w(px(200.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_layout_menu(cx)))
                .child(rows)
                .into_any_element();
            button = button.child(popover::anchored_menu_below_gap(
                "layout-menu",
                menu,
                closing,
                6.0,
            ));
        }
        button.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str) -> TabKey {
        TabKey::session(id)
    }

    #[test]
    fn route_open_uses_the_focused_group_and_reuses_open_tabs() {
        let mut ws = Workspace::new();
        let first = route_open(&mut ws, s("a"), false);
        assert_eq!(ws.focused_tab(), Some(&s("a")));
        route_open(&mut ws, s("b"), false);
        assert_eq!(ws.group(first).unwrap().tabs(), &[s("a"), s("b")]);
        // Already open: activated where it is, not duplicated.
        assert_eq!(route_open(&mut ws, s("a"), true), first);
        assert_eq!(ws.group(first).unwrap().tabs().len(), 2);
        assert_eq!(ws.focused_tab(), Some(&s("a")));
        assert_eq!(ws.group_count(), 1);
    }

    #[test]
    fn route_open_split_opens_to_the_right() {
        let mut ws = Workspace::new();
        let left = route_open(&mut ws, s("a"), false);
        let right = route_open(&mut ws, s("b"), true);
        assert_ne!(left, right);
        assert_eq!(ws.group_count(), 2);
        assert_eq!(ws.focused(), right);
        assert_eq!(ws.neighbour(left, Edge::Right), Some(right));
        // Re-routing an open session focuses it without moving it.
        route_open(&mut ws, s("a"), true);
        assert_eq!(ws.focused(), left);
        assert_eq!(ws.group_count(), 2);
    }

    #[test]
    fn route_open_split_fills_an_empty_focused_group() {
        let mut ws = Workspace::new();
        route_open(&mut ws, s("a"), false);
        let empty = ws.split_group(ws.focused(), Edge::Right).unwrap();
        assert_eq!(route_open(&mut ws, s("b"), true), empty);
        assert_eq!(ws.group_count(), 2);
    }

    fn tile() -> gpui::Bounds<Pixels> {
        gpui::Bounds::new(
            gpui::point(px(100.0), px(50.0)),
            gpui::size(px(400.0), px(200.0)),
        )
    }

    fn at(x: f32, y: f32) -> Point<Pixels> {
        gpui::point(px(x), px(y))
    }

    #[test]
    fn drop_zone_picks_the_nearest_edge_band() {
        let b = tile();
        assert_eq!(drop_zone(b, at(300.0, 150.0)), Some(Drop::Center));
        assert_eq!(drop_zone(b, at(110.0, 150.0)), Some(Drop::Edge(Edge::Left)));
        assert_eq!(
            drop_zone(b, at(490.0, 150.0)),
            Some(Drop::Edge(Edge::Right))
        );
        assert_eq!(drop_zone(b, at(300.0, 60.0)), Some(Drop::Edge(Edge::Top)));
        assert_eq!(
            drop_zone(b, at(300.0, 240.0)),
            Some(Drop::Edge(Edge::Bottom))
        );
        // The band is 25% per side: 99px from the left of 400 splits, 101 joins.
        assert_eq!(drop_zone(b, at(199.0, 150.0)), Some(Drop::Edge(Edge::Left)));
        assert_eq!(drop_zone(b, at(201.0, 150.0)), Some(Drop::Center));
        // In a corner the relatively nearer side wins: 5% from the left
        // beats 10% from the top.
        assert_eq!(drop_zone(b, at(120.0, 70.0)), Some(Drop::Edge(Edge::Left)));
        assert_eq!(drop_zone(b, at(160.0, 55.0)), Some(Drop::Edge(Edge::Top)));
        // Outside, or a degenerate tile.
        assert_eq!(drop_zone(b, at(99.0, 150.0)), None);
        assert_eq!(drop_zone(b, at(300.0, 251.0)), None);
        let flat = gpui::Bounds::new(gpui::point(px(0.0), px(0.0)), gpui::size(px(0.0), px(0.0)));
        assert_eq!(drop_zone(flat, at(0.0, 0.0)), None);
    }

    #[test]
    fn effective_drop_skips_no_ops_and_fills_empty_tiles() {
        let mut ws = Workspace::new();
        let left = route_open(&mut ws, s("a"), false);
        let empty = ws.split_group(left, Edge::Right).unwrap();
        let edge = Drop::Edge(Edge::Top);
        // A lone tab on its own tile: centre and edges change nothing.
        assert_eq!(effective_drop(&ws, &s("a"), left, Drop::Center), None);
        assert_eq!(effective_drop(&ws, &s("a"), left, edge), None);
        // An empty tile takes it whole, whatever the zone.
        assert_eq!(
            effective_drop(&ws, &s("a"), empty, edge),
            Some(Drop::Center)
        );
        // With a second tab its own edge splits; other tiles take any zone.
        ws.focus(left);
        route_open(&mut ws, s("b"), false);
        assert_eq!(effective_drop(&ws, &s("a"), left, edge), Some(edge));
        assert_eq!(
            effective_drop(&ws, &s("x"), left, Drop::Center),
            Some(Drop::Center)
        );
        assert_eq!(effective_drop(&ws, &s("a"), GroupId(999), edge), None);
    }

    #[test]
    fn route_drop_moves_open_tabs_and_opens_closed_ones() {
        let mut ws = Workspace::new();
        let first = route_open(&mut ws, s("a"), false);
        route_open(&mut ws, s("b"), false);
        // An open tab onto its own tile's edge splits it.
        let right = route_drop(
            &mut ws,
            s("b"),
            first,
            Placement::Zone(Drop::Edge(Edge::Right)),
        )
        .unwrap();
        assert_ne!(right, first);
        assert_eq!(ws.group_count(), 2);
        assert_eq!(ws.focused(), right);
        assert_eq!(ws.neighbour(first, Edge::Right), Some(right));
        // A closed session onto an edge opens in a new split.
        let below = route_drop(
            &mut ws,
            s("c"),
            right,
            Placement::Zone(Drop::Edge(Edge::Bottom)),
        )
        .unwrap();
        assert_eq!(ws.group_count(), 3);
        assert_eq!(ws.neighbour(right, Edge::Bottom), Some(below));
        // A closed session onto a centre joins; into a bar at a position.
        assert_eq!(
            route_drop(&mut ws, s("d"), first, Placement::Zone(Drop::Center)),
            Some(first)
        );
        assert_eq!(
            route_drop(&mut ws, s("e"), first, Placement::At(0)),
            Some(first)
        );
        assert_eq!(ws.group(first).unwrap().tabs(), &[s("e"), s("a"), s("d")]);
        assert_eq!(ws.focused_tab(), Some(&s("e")));
        // Moving a lone tab out collapses its tile.
        assert_eq!(
            route_drop(&mut ws, s("c"), first, Placement::At(usize::MAX)),
            Some(first)
        );
        assert_eq!(ws.group_count(), 2);
        assert_eq!(ws.group(first).unwrap().tabs().last(), Some(&s("c")));
        // A no-op zone (own centre) just activates.
        assert_eq!(
            route_drop(&mut ws, s("a"), first, Placement::Zone(Drop::Center)),
            Some(first)
        );
        assert_eq!(ws.focused_tab(), Some(&s("a")));
        assert_eq!(
            route_drop(&mut ws, s("a"), GroupId(999), Placement::At(0)),
            None
        );
    }

    #[test]
    fn route_drop_fills_an_empty_tile_in_place() {
        let mut ws = Workspace::new();
        let left = route_open(&mut ws, s("a"), false);
        route_open(&mut ws, s("b"), false);
        let empty = ws.split_group(left, Edge::Right).unwrap();
        let edge = Placement::Zone(Drop::Edge(Edge::Left));
        // A closed session and an open tab each fill an empty tile in place.
        assert_eq!(route_drop(&mut ws, s("z"), empty, edge), Some(empty));
        assert_eq!(ws.group_count(), 2);
        let another = ws.split_group(empty, Edge::Right).unwrap();
        assert_eq!(route_drop(&mut ws, s("b"), another, edge), Some(another));
        assert_eq!(ws.group_count(), 3);
        assert_eq!(ws.group(left).unwrap().tabs(), &[s("a")]);
    }

    #[test]
    fn split_drag_delta_measures_from_the_gap_centre() {
        // Two halves of a 408px split: 200px content each around an 8px gap.
        let f = [0.5, 0.5];
        assert!(split_drag_delta(&f, 0, 0.0, 408.0, 204.0).abs() < 1e-6);
        assert!((split_drag_delta(&f, 0, 0.0, 408.0, 244.0) - 0.1).abs() < 1e-6);
        assert!((split_drag_delta(&f, 0, 100.0, 408.0, 264.0) + 0.1).abs() < 1e-6);
        // Second boundary of three equal thirds of 616px (600 content).
        let t = [1.0 / 3.0; 3];
        let centre = 400.0 + 8.0 + 4.0;
        assert!(split_drag_delta(&t, 1, 0.0, 616.0, centre).abs() < 1e-4);
        // Out-of-range boundary and degenerate sizes never move anything.
        assert_eq!(split_drag_delta(&f, 1, 0.0, 408.0, 300.0), 0.0);
        assert_eq!(split_drag_delta(&f, 0, 0.0, 4.0, 300.0), 0.0);
    }

    #[test]
    fn min_tile_fraction_keeps_tiles_usable() {
        assert!((min_tile_fraction(808.0, 2) - 0.25).abs() < 1e-6);
        // Tiny splits cap at half (the model clamps to the pair anyway).
        assert_eq!(min_tile_fraction(300.0, 2), 0.5);
        assert_eq!(min_tile_fraction(0.0, 2), 0.0);
    }

    #[test]
    fn touch_tracks_window_edges_through_splits() {
        let top_row = Touch::ALL.child(Axis::Vertical, 0, 2);
        assert_eq!(top_row, Touch::ALL);
        let bottom_row = Touch::ALL.child(Axis::Vertical, 1, 2);
        assert!(!bottom_row.top && bottom_row.left && bottom_row.right);
        let top_left = top_row.child(Axis::Horizontal, 0, 3);
        assert!(top_left.top && top_left.left && !top_left.right);
        let top_mid = top_row.child(Axis::Horizontal, 1, 3);
        assert!(top_mid.top && !top_mid.left && !top_mid.right);
        let top_right = top_row.child(Axis::Horizontal, 2, 3);
        assert!(top_right.top && !top_right.left && top_right.right);
    }

    #[test]
    fn the_rail_insets_tabs_only_where_it_covers_them() {
        let rail = super::dock::RAIL_WIDTH;
        // Body text is never inset — the rail floats. Tabs stop at the card
        // when the card shares the header band.
        assert_eq!(tile_header_right_inset(Touch::ALL, false, false), 4.0);
        assert_eq!(tile_header_right_inset(Touch::ALL, true, false), 4.0 + rail);
        let lower = Touch {
            top: false,
            left: true,
            right: true,
        };
        assert_eq!(tile_header_right_inset(lower, true, false), 4.0 + rail);
        // Windows caption buttons already clear the corner; the rail starts
        // under them, below the tab row, so the tabs keep only that inset.
        assert_eq!(
            tile_header_right_inset(Touch::ALL, true, true),
            tile_header_right_inset(Touch::ALL, false, true)
        );
        assert!(tile_header_right_inset(Touch::ALL, false, true) > rail);
    }
}
