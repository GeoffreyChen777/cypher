//! Workspace layout model (docs/workspace-layout.md): a tree of splits whose
//! leaves are tile groups of session tabs.
//!
//! Pure data — no gpui types. The shell renders from it and routes every
//! layout change (open, close, drag, presets, resize) through the operations
//! here, which keep the invariants:
//!
//! - every group in the tree exists in the group map and vice versa;
//! - a tab appears at most once across all groups;
//! - a split has ≥2 children whose fractions sum to 1, and no child split
//!   shares its parent's axis (same-axis splits are flattened);
//! - `focused` (and `zoomed`, if set) name live groups.
//!
//! Only the last remaining group may be left empty by closing tabs (it renders
//! the new-session picker); presets and explicit splits create empty groups
//! on purpose.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

/// Tolerance for geometric comparisons in unit space.
const EPS: f32 = 1e-3;

/// What a tab shows: a session (keyed by chat id) or a new-session canvas that
/// has no chat yet. Canvas ids come from [`Workspace::new_session_tab`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TabKey {
    Session(String),
    NewSession(u64),
}

impl TabKey {
    pub fn session(chat_id: impl Into<String>) -> Self {
        Self::Session(chat_id.into())
    }

    /// The chat id, if this tab shows a session.
    pub fn chat_id(&self) -> Option<&str> {
        match self {
            Self::Session(id) => Some(id),
            Self::NewSession(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GroupId(pub u64);

/// A tile group: session tabs with one active. `active` is 0 when empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Group {
    tabs: Vec<TabKey>,
    active: usize,
}

impl Group {
    pub fn tabs(&self) -> &[TabKey] {
        &self.tabs
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn active_tab(&self) -> Option<&TabKey> {
        self.tabs.get(self.active)
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    fn push(&mut self, tab: TabKey) {
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    /// Remove the tab at `index`; the active tab stays active, or its right
    /// neighbour (left if it was last) takes over.
    fn remove_at(&mut self, index: usize) -> TabKey {
        let tab = self.tabs.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        self.clamp_active();
        tab
    }

    fn clamp_active(&mut self) {
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }
}

/// Split direction. `Horizontal` lays children out left→right, `Vertical`
/// top→bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// The split axis a new tile at this edge creates.
    pub fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Horizontal,
            Self::Top | Self::Bottom => Axis::Vertical,
        }
    }

    /// Whether the new tile goes before the target (left / above).
    fn is_leading(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }
}

/// Where a dragged tab lands on a group: joining it, or splitting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drop {
    Center,
    Edge(Edge),
}

/// A layout node. Split children carry their fraction of the split's size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Node {
    Split {
        axis: Axis,
        children: Vec<(Node, f32)>,
    },
    Group(GroupId),
}

impl Node {
    fn split(axis: Axis, nodes: Vec<Node>) -> Self {
        let fraction = 1.0 / nodes.len() as f32;
        Self::Split {
            axis,
            children: nodes.into_iter().map(|node| (node, fraction)).collect(),
        }
    }

    /// The node at `path` (child indices from this node).
    pub fn at(&self, path: &[usize]) -> Option<&Node> {
        path.iter().try_fold(self, |node, &i| match node {
            Self::Split { children, .. } => children.get(i).map(|(child, _)| child),
            Self::Group(_) => None,
        })
    }

    fn at_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        path.iter().try_fold(self, |node, &i| match node {
            Self::Split { children, .. } => children.get_mut(i).map(|(child, _)| child),
            Self::Group(_) => None,
        })
    }

    fn path_to(&self, id: GroupId) -> Option<Vec<usize>> {
        match self {
            Self::Group(group) => (*group == id).then(Vec::new),
            Self::Split { children, .. } => {
                children.iter().enumerate().find_map(|(i, (child, _))| {
                    let mut path = child.path_to(id)?;
                    path.insert(0, i);
                    Some(path)
                })
            }
        }
    }

    fn collect_groups(&self, out: &mut Vec<GroupId>) {
        match self {
            Self::Group(id) => out.push(*id),
            Self::Split { children, .. } => {
                for (child, _) in children {
                    child.collect_groups(out);
                }
            }
        }
    }

    fn first_group(&self) -> Option<GroupId> {
        match self {
            Self::Group(id) => Some(*id),
            Self::Split { children, .. } => children.first()?.0.first_group(),
        }
    }

    fn last_group(&self) -> Option<GroupId> {
        match self {
            Self::Group(id) => Some(*id),
            Self::Split { children, .. } => children.last()?.0.last_group(),
        }
    }

    fn layout(&self, rect: Rect, out: &mut Vec<(GroupId, Rect)>) {
        match self {
            Self::Group(id) => out.push((*id, rect)),
            Self::Split { axis, children } => {
                let mut offset = 0.0;
                for (child, fraction) in children {
                    let child_rect = match axis {
                        Axis::Horizontal => Rect {
                            x: rect.x + offset * rect.w,
                            w: fraction * rect.w,
                            ..rect
                        },
                        Axis::Vertical => Rect {
                            y: rect.y + offset * rect.h,
                            h: fraction * rect.h,
                            ..rect
                        },
                    };
                    child.layout(child_rect, out);
                    offset += fraction;
                }
            }
        }
    }
}

/// Rebuild `node` under the invariants: drop leaves that are unknown or
/// already seen, sanitize and renormalize fractions, flatten same-axis child
/// splits, collapse single-child splits. `None` when nothing survives.
fn normalize_node(
    node: Node,
    groups: &BTreeMap<GroupId, Group>,
    seen: &mut HashSet<GroupId>,
) -> Option<Node> {
    let (axis, children) = match node {
        Node::Group(id) => return (groups.contains_key(&id) && seen.insert(id)).then_some(node),
        Node::Split { axis, children } => (axis, children),
    };
    let valid = |f: f32| f.is_finite() && f > 0.0;
    let (sum, count) = children
        .iter()
        .filter(|(_, f)| valid(*f))
        .fold((0.0, 0), |(sum, count), (_, f)| (sum + f, count + 1));
    // Broken fractions take the mean of the good ones (equal split if none).
    let fill = if count == 0 { 1.0 } else { sum / count as f32 };
    let mut out = Vec::with_capacity(children.len());
    for (child, fraction) in children {
        let fraction = if valid(fraction) { fraction } else { fill };
        match normalize_node(child, groups, seen) {
            None => {}
            Some(Node::Split {
                axis: inner,
                children,
            }) if inner == axis => {
                out.extend(children.into_iter().map(|(c, f)| (c, f * fraction)));
            }
            Some(child) => out.push((child, fraction)),
        }
    }
    match out.len() {
        0 => None,
        1 => out.pop().map(|(child, _)| child),
        _ => {
            let total: f32 = out.iter().map(|(_, f)| f).sum();
            for (_, f) in &mut out {
                *f /= total;
            }
            Some(Node::Split {
                axis,
                children: out,
            })
        }
    }
}

/// A group's rectangle in unit space (the whole workspace is 0..1 × 0..1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    const UNIT: Self = Self {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };
}

/// Length of the overlap of `[a, a + a_len]` and `[b, b + b_len]`.
fn overlap(a: f32, a_len: f32, b: f32, b_len: f32) -> f32 {
    ((a + a_len).min(b + b_len) - a.max(b)).max(0.0)
}

/// Layout presets. Groups are filled in reading order (row by row, a stacked
/// column top to bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Single,
    Columns2,
    Rows2,
    Columns3,
    Rows3,
    Grid2x2,
    Grid3x3,
    /// Left column of two stacked tiles, one tile on the right.
    TwoStackedPlusOne,
    /// One tile on the left, right column of two stacked tiles.
    OnePlusTwoStacked,
}

impl Preset {
    pub const ALL: [Preset; 9] = [
        Self::Single,
        Self::Columns2,
        Self::Rows2,
        Self::Columns3,
        Self::Rows3,
        Self::Grid2x2,
        Self::Grid3x3,
        Self::TwoStackedPlusOne,
        Self::OnePlusTwoStacked,
    ];

    pub fn group_count(self) -> usize {
        match self {
            Self::Single => 1,
            Self::Columns2 | Self::Rows2 => 2,
            Self::Columns3 | Self::Rows3 | Self::TwoStackedPlusOne | Self::OnePlusTwoStacked => 3,
            Self::Grid2x2 => 4,
            Self::Grid3x3 => 9,
        }
    }

    /// The preset's tree over `ids` (exactly [`Self::group_count`] of them).
    fn build(self, ids: &[GroupId]) -> Node {
        use Axis::{Horizontal as H, Vertical as V};
        let g = |i: usize| Node::Group(ids[i]);
        let grid = |n: usize| {
            Node::split(
                V,
                (0..n)
                    .map(|row| Node::split(H, (0..n).map(|col| g(row * n + col)).collect()))
                    .collect(),
            )
        };
        match self {
            Self::Single => g(0),
            Self::Columns2 => Node::split(H, vec![g(0), g(1)]),
            Self::Rows2 => Node::split(V, vec![g(0), g(1)]),
            Self::Columns3 => Node::split(H, vec![g(0), g(1), g(2)]),
            Self::Rows3 => Node::split(V, vec![g(0), g(1), g(2)]),
            Self::Grid2x2 => grid(2),
            Self::Grid3x3 => grid(3),
            Self::TwoStackedPlusOne => Node::split(H, vec![Node::split(V, vec![g(0), g(1)]), g(2)]),
            Self::OnePlusTwoStacked => Node::split(H, vec![g(0), Node::split(V, vec![g(1), g(2)])]),
        }
    }
}

/// One window's workspace. Serialized as-is; deserialization runs
/// [`Workspace::repair`], so a hand-edited or stale file can't break the
/// invariants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "WorkspaceRepr")]
pub struct Workspace {
    root: Node,
    groups: BTreeMap<GroupId, Group>,
    focused: GroupId,
    #[serde(skip_serializing_if = "Option::is_none")]
    zoomed: Option<GroupId>,
    /// Next id for groups and new-session canvases (one counter for both).
    next_id: u64,
}

/// Lenient on-disk shape; every field may be missing or inconsistent.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct WorkspaceRepr {
    root: Option<Node>,
    groups: BTreeMap<GroupId, Group>,
    focused: Option<GroupId>,
    zoomed: Option<GroupId>,
    next_id: u64,
}

impl From<WorkspaceRepr> for Workspace {
    fn from(repr: WorkspaceRepr) -> Self {
        let mut workspace = Self {
            // An unknown id; `repair` replaces a dead tree with a fresh group.
            root: repr.root.unwrap_or(Node::Split {
                axis: Axis::Horizontal,
                children: Vec::new(),
            }),
            groups: repr.groups,
            focused: repr.focused.unwrap_or(GroupId(u64::MAX)),
            zoomed: repr.zoomed,
            next_id: repr.next_id,
        };
        workspace.repair();
        workspace
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspace {
    /// One empty, focused group.
    pub fn new() -> Self {
        let id = GroupId(0);
        Self {
            root: Node::Group(id),
            groups: BTreeMap::from([(id, Group::default())]),
            focused: id,
            zoomed: None,
            next_id: 1,
        }
    }

    // ---- queries ----

    pub fn root(&self) -> &Node {
        &self.root
    }

    pub fn group(&self, id: GroupId) -> Option<&Group> {
        self.groups.get(&id)
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub fn focused(&self) -> GroupId {
        self.focused
    }

    /// The active tab of the focused group.
    pub fn focused_tab(&self) -> Option<&TabKey> {
        self.groups.get(&self.focused)?.active_tab()
    }

    pub fn zoomed(&self) -> Option<GroupId> {
        self.zoomed
    }

    /// Where `tab` lives: its group and index there.
    pub fn find(&self, tab: &TabKey) -> Option<(GroupId, usize)> {
        self.groups.iter().find_map(|(id, group)| {
            let index = group.tabs.iter().position(|t| t == tab)?;
            Some((*id, index))
        })
    }

    pub fn contains(&self, tab: &TabKey) -> bool {
        self.find(tab).is_some()
    }

    /// Every tab, groups in reading order.
    pub fn tabs(&self) -> impl Iterator<Item = &TabKey> {
        self.groups_in_reading_order()
            .into_iter()
            .filter_map(|id| self.groups.get(&id))
            .flat_map(|group| group.tabs.iter())
    }

    /// The active tab of every group — what's on screen (only the zoomed
    /// group's when zoomed).
    pub fn visible_tabs(&self) -> Vec<&TabKey> {
        let ids = match self.zoomed {
            Some(id) => vec![id],
            None => self.groups_in_reading_order(),
        };
        ids.into_iter()
            .filter_map(|id| self.groups.get(&id)?.active_tab())
            .collect()
    }

    /// Groups in depth-first, left→right / top→bottom order.
    pub fn groups_in_reading_order(&self) -> Vec<GroupId> {
        let mut out = Vec::with_capacity(self.groups.len());
        self.root.collect_groups(&mut out);
        out
    }

    /// Every group's rectangle in unit space, reading order.
    pub fn group_rects(&self) -> Vec<(GroupId, Rect)> {
        let mut out = Vec::with_capacity(self.groups.len());
        self.root.layout(Rect::UNIT, &mut out);
        out
    }

    /// The group adjacent to `group` across `edge`, for keyboard focus
    /// movement. Among several, the one sharing the longest border wins, then
    /// the one whose centre is closest, then reading order.
    pub fn neighbour(&self, group: GroupId, edge: Edge) -> Option<GroupId> {
        let rects = self.group_rects();
        let from = rects.iter().find(|(id, _)| *id == group)?.1;
        let (cx, cy) = (from.x + from.w / 2.0, from.y + from.h / 2.0);
        rects
            .iter()
            .filter(|(id, _)| *id != group)
            .filter_map(|(id, r)| {
                let (gap, shared, dist) = match edge {
                    Edge::Left => (
                        from.x - (r.x + r.w),
                        overlap(from.y, from.h, r.y, r.h),
                        (r.y + r.h / 2.0 - cy).abs(),
                    ),
                    Edge::Right => (
                        r.x - (from.x + from.w),
                        overlap(from.y, from.h, r.y, r.h),
                        (r.y + r.h / 2.0 - cy).abs(),
                    ),
                    Edge::Top => (
                        from.y - (r.y + r.h),
                        overlap(from.x, from.w, r.x, r.w),
                        (r.x + r.w / 2.0 - cx).abs(),
                    ),
                    Edge::Bottom => (
                        r.y - (from.y + from.h),
                        overlap(from.x, from.w, r.x, r.w),
                        (r.x + r.w / 2.0 - cx).abs(),
                    ),
                };
                (gap.abs() < EPS && shared > EPS).then_some((*id, shared, dist))
            })
            .min_by(|a, b| b.1.total_cmp(&a.1).then(a.2.total_cmp(&b.2)))
            .map(|(id, _, _)| id)
    }

    // ---- tabs ----

    /// A fresh new-session canvas key (not yet opened).
    pub fn new_session_tab(&mut self) -> TabKey {
        TabKey::NewSession(self.alloc_id())
    }

    /// Open `tab` in the focused group. See [`Self::open_in`].
    pub fn open(&mut self, tab: TabKey) -> GroupId {
        self.open_in(self.focused, tab)
    }

    /// Open `tab` as the active tab of `group` (the focused group if `group`
    /// is gone) and focus it. A tab already open anywhere is activated and
    /// focused where it is instead — a session lives in one tab.
    pub fn open_in(&mut self, group: GroupId, tab: TabKey) -> GroupId {
        if let Some((id, index)) = self.find(&tab) {
            self.activate(id, index);
            return id;
        }
        let id = if self.groups.contains_key(&group) {
            group
        } else {
            self.focused
        };
        if let Some(target) = self.groups.get_mut(&id) {
            target.push(tab);
        }
        self.focused = id;
        id
    }

    /// Open `tab` in a new group at `edge` of `group` (⌘-click opens to the
    /// right). An already-open tab moves there. Returns the tab's group.
    pub fn open_split(&mut self, tab: TabKey, group: GroupId, edge: Edge) -> Option<GroupId> {
        if !self.groups.contains_key(&group) {
            return None;
        }
        if self.contains(&tab) {
            self.move_tab(&tab, group, Drop::Edge(edge));
            return self.find(&tab).map(|(id, _)| id);
        }
        let id = self.insert_group(group, edge, Group::default())?;
        self.open_in(id, tab);
        Some(id)
    }

    /// Activate tab `index` of `group` and focus the group.
    pub fn activate(&mut self, group: GroupId, index: usize) -> bool {
        match self.groups.get_mut(&group) {
            Some(target) if index < target.tabs.len() => {
                target.active = index;
                self.focused = group;
                true
            }
            _ => false,
        }
    }

    pub fn focus(&mut self, group: GroupId) -> bool {
        let live = self.groups.contains_key(&group);
        if live {
            self.focused = group;
        }
        live
    }

    /// Close `tab`. A group it leaves empty collapses (its space goes to a
    /// neighbour, which inherits focus) unless it's the only group.
    pub fn close(&mut self, tab: &TabKey) -> bool {
        let Some((group, _)) = self.take_tab(tab) else {
            return false;
        };
        if self.groups[&group].is_empty() {
            self.remove_group(group);
        }
        true
    }

    /// Replace `old` in place (a new-session canvas becoming its session after
    /// the first send). If `new` is already open elsewhere, `old` closes and
    /// `new` is focused where it is.
    pub fn replace_tab(&mut self, old: &TabKey, new: TabKey) -> bool {
        if old == &new {
            return self.contains(old);
        }
        let Some((group, index)) = self.find(old) else {
            return false;
        };
        if self.contains(&new) {
            self.close(old);
            self.open(new);
        } else {
            self.groups.get_mut(&group).expect("found group").tabs[index] = new;
        }
        true
    }

    /// Drop `tab` on `target`: `Center` joins the group, an edge splits it
    /// with a new group holding the tab. A group the tab leaves empty
    /// collapses. Returns whether the layout changed; dropping a group's only
    /// tab on its own edge (or its own centre) is a no-op.
    pub fn move_tab(&mut self, tab: &TabKey, target: GroupId, drop: Drop) -> bool {
        let Some((source, index)) = self.find(tab) else {
            return false;
        };
        if !self.groups.contains_key(&target) {
            return false;
        }
        match drop {
            Drop::Center if source == target => {
                self.activate(source, index);
                return false;
            }
            Drop::Edge(_) if source == target && self.groups[&source].tabs.len() == 1 => {
                return false;
            }
            _ => {}
        }
        let tab = self
            .groups
            .get_mut(&source)
            .expect("found group")
            .remove_at(index);
        let landed = match drop {
            Drop::Center => {
                self.groups.get_mut(&target).expect("live target").push(tab);
                target
            }
            Drop::Edge(edge) => {
                let mut group = Group::default();
                group.push(tab);
                self.insert_group(target, edge, group).expect("live target")
            }
        };
        if source != target && self.groups[&source].is_empty() {
            self.remove_group(source);
        }
        self.focused = landed;
        true
    }

    /// Reorder `group`'s tabs: the tab at `from` moves to `to` (tab-bar drag).
    /// The moved tab becomes active and the group is focused. Returns whether
    /// the order changed.
    pub fn move_within(&mut self, group: GroupId, from: usize, to: usize) -> bool {
        let Some(target) = self.groups.get_mut(&group) else {
            return false;
        };
        if from >= target.tabs.len() || to >= target.tabs.len() {
            return false;
        }
        let tab = target.tabs.remove(from);
        target.tabs.insert(to, tab);
        target.active = to;
        self.focused = group;
        from != to
    }

    /// Drop `tab` into `target`'s tab bar at `index` (clamped): a reorder
    /// within its own group, else a join at that position (like
    /// [`Drop::Center`]). The tab becomes active, the target is focused, and a
    /// group the tab leaves empty collapses. Returns whether the layout
    /// changed.
    pub fn move_to_index(&mut self, tab: &TabKey, target: GroupId, index: usize) -> bool {
        let Some((source, from)) = self.find(tab) else {
            return false;
        };
        let Some(len) = self.groups.get(&target).map(|g| g.tabs.len()) else {
            return false;
        };
        if source == target {
            return self.move_within(target, from, index.min(len - 1));
        }
        let tab = self
            .groups
            .get_mut(&source)
            .expect("found group")
            .remove_at(from);
        let group = self.groups.get_mut(&target).expect("live target");
        let at = index.min(len);
        group.tabs.insert(at, tab);
        group.active = at;
        if self.groups[&source].is_empty() {
            self.remove_group(source);
        }
        self.focused = target;
        true
    }

    /// Keep only tabs matching `keep` (prunes deleted or archived sessions).
    /// Groups emptied by this collapse like [`Self::close`]; one group always
    /// remains. Returns whether anything was removed.
    pub fn retain_tabs(&mut self, mut keep: impl FnMut(&TabKey) -> bool) -> bool {
        let mut changed = false;
        let mut emptied = Vec::new();
        for (id, group) in &mut self.groups {
            let before = group.tabs.len();
            let mut kept_before_active = 0;
            let mut index = 0;
            group.tabs.retain(|tab| {
                let kept = keep(tab);
                if kept && index < group.active {
                    kept_before_active += 1;
                }
                index += 1;
                kept
            });
            if group.tabs.len() == before {
                continue;
            }
            changed = true;
            group.active = kept_before_active;
            group.clamp_active();
            if group.tabs.is_empty() {
                emptied.push(*id);
            }
        }
        for id in emptied {
            self.remove_group(id);
        }
        changed
    }

    // ---- layout ----

    /// Split `group` with a new empty group at `edge` and focus it ("split
    /// right/down" commands).
    pub fn split_group(&mut self, group: GroupId, edge: Edge) -> Option<GroupId> {
        let id = self.insert_group(group, edge, Group::default())?;
        self.focused = id;
        Some(id)
    }

    /// Close `group` ("close split"): its tile collapses into the adjacent
    /// sibling (like [`Self::close`] emptying it), which inherits its tabs —
    /// appended after its own, keeping its active tab unless it had none —
    /// and, if `group` was focused, focus. The only group never closes.
    pub fn close_group(&mut self, group: GroupId) -> bool {
        if self.groups.len() <= 1 || !self.groups.contains_key(&group) {
            return false;
        }
        let was_focused = self.focused;
        let moved = std::mem::take(self.groups.get_mut(&group).expect("live group"));
        // Removing a focused group hands focus to its heir: read it back.
        self.focused = group;
        if !self.remove_group(group) {
            self.groups.insert(group, moved);
            self.focused = was_focused;
            return false;
        }
        let heir = self.focused;
        let target = self.groups.get_mut(&heir).expect("heir");
        if target.tabs.is_empty() {
            target.active = moved.active;
        }
        target.tabs.extend(moved.tabs);
        target.clamp_active();
        if was_focused != group {
            self.focused = was_focused;
        }
        true
    }

    /// Move the boundary between children `boundary` and `boundary + 1` of the
    /// split at `path` by `delta` (a fraction of that split's size), keeping
    /// both at least `min_fraction` of it. Returns whether anything moved.
    pub fn resize(
        &mut self,
        path: &[usize],
        boundary: usize,
        delta: f32,
        min_fraction: f32,
    ) -> bool {
        let Some(Node::Split { children, .. }) = self.root.at_mut(path) else {
            return false;
        };
        if boundary + 1 >= children.len() || !delta.is_finite() {
            return false;
        }
        let (a, b) = (children[boundary].1, children[boundary + 1].1);
        let min = if min_fraction.is_finite() {
            min_fraction.clamp(0.0, (a + b) / 2.0)
        } else {
            0.0
        };
        // max/min rather than clamp: rounding may invert the bounds by an ulp.
        let delta = delta.max(min - a).min(b - min);
        if delta.abs() < 1e-6 {
            return false;
        }
        children[boundary].1 = a + delta;
        children[boundary + 1].1 = b - delta;
        true
    }

    /// Give every child of the split at `path` an equal share.
    pub fn equalize(&mut self, path: &[usize]) -> bool {
        let Some(Node::Split { children, .. }) = self.root.at_mut(path) else {
            return false;
        };
        let fraction = 1.0 / children.len() as f32;
        for (_, f) in children {
            *f = fraction;
        }
        true
    }

    /// Rebuild the layout as `preset`. Existing groups keep their ids in
    /// reading order (extra slots get fresh ids). Every tab is redistributed
    /// in reading order, one per group; when there are more tabs than groups
    /// the leftovers join the last group. The focused tab stays focused and
    /// active. Clears zoom.
    pub fn apply_preset(&mut self, preset: Preset) {
        let order = self.groups_in_reading_order();
        let focused_tab = self.focused_tab().cloned();
        let tabs: Vec<TabKey> = self.tabs().cloned().collect();
        let count = preset.group_count();
        let mut ids: Vec<GroupId> = order.iter().copied().take(count).collect();
        while ids.len() < count {
            ids.push(GroupId(self.alloc_id()));
        }
        let mut groups: BTreeMap<GroupId, Group> =
            ids.iter().map(|&id| (id, Group::default())).collect();
        for (i, tab) in tabs.into_iter().enumerate() {
            let group = groups.get_mut(&ids[i.min(count - 1)]).expect("slot");
            group.tabs.push(tab);
        }
        let mut focused = ids.contains(&self.focused).then_some(self.focused);
        if let Some(tab) = &focused_tab {
            for (id, group) in &mut groups {
                if let Some(index) = group.tabs.iter().position(|t| t == tab) {
                    group.active = index;
                    focused = Some(*id);
                }
            }
        }
        self.root = preset.build(&ids);
        self.groups = groups;
        self.focused = focused.unwrap_or(ids[0]);
        self.zoomed = None;
        self.normalize();
    }

    /// Zoom `group` to fill the workspace, or unzoom it. Returns whether it is
    /// now zoomed.
    pub fn toggle_zoom(&mut self, group: GroupId) -> bool {
        if !self.groups.contains_key(&group) {
            return false;
        }
        if self.zoomed == Some(group) {
            self.zoomed = None;
            false
        } else {
            self.zoomed = Some(group);
            self.focused = group;
            true
        }
    }

    // ---- invariants ----

    /// Restore the tree invariants: drop leaves naming unknown or repeated
    /// groups, drop groups the tree doesn't reference, sanitize and
    /// renormalize fractions, flatten same-axis splits, collapse single-child
    /// splits, and repoint a dangling `focused`/`zoomed`.
    pub fn normalize(&mut self) {
        let max_group = self.groups.keys().map(|id| id.0 + 1).max().unwrap_or(0);
        let max_canvas = self
            .groups
            .values()
            .flat_map(|group| &group.tabs)
            .filter_map(|tab| match tab {
                TabKey::NewSession(id) => Some(id + 1),
                TabKey::Session(_) => None,
            })
            .max()
            .unwrap_or(0);
        self.next_id = self.next_id.max(max_group).max(max_canvas);

        let mut seen = HashSet::new();
        let root = std::mem::replace(&mut self.root, Node::Group(GroupId(0)));
        self.root = match normalize_node(root, &self.groups, &mut seen) {
            Some(root) => root,
            None => {
                let id = GroupId(self.alloc_id());
                self.groups.insert(id, Group::default());
                seen.insert(id);
                Node::Group(id)
            }
        };
        self.groups.retain(|id, _| seen.contains(id));
        for group in self.groups.values_mut() {
            group.clamp_active();
        }
        if !self.groups.contains_key(&self.focused) {
            self.focused = self.root.first_group().expect("non-empty tree");
        }
        self.zoomed = self.zoomed.filter(|id| self.groups.contains_key(id));
    }

    /// [`Self::normalize`] plus de-duplication of tabs (first occurrence in
    /// reading order wins). Run on anything loaded from disk.
    pub fn repair(&mut self) {
        self.normalize();
        let mut seen = HashSet::new();
        for id in self.groups_in_reading_order() {
            let group = self.groups.get_mut(&id).expect("normalized");
            let active = group.active_tab().cloned();
            group.tabs.retain(|tab| seen.insert(tab.clone()));
            group.active = active
                .and_then(|tab| group.tabs.iter().position(|t| *t == tab))
                .unwrap_or(0);
            group.clamp_active();
        }
    }

    // ---- internals ----

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Remove `tab` from its group (no collapse).
    fn take_tab(&mut self, tab: &TabKey) -> Option<(GroupId, TabKey)> {
        let (group, index) = self.find(tab)?;
        let tab = self.groups.get_mut(&group)?.remove_at(index);
        Some((group, tab))
    }

    /// Insert `group` as a new tile at `edge` of `target`. Clears zoom.
    fn insert_group(&mut self, target: GroupId, edge: Edge, group: Group) -> Option<GroupId> {
        let path = self.root.path_to(target)?;
        let id = GroupId(self.alloc_id());
        let leaf = self.root.at_mut(&path)?;
        let old = std::mem::replace(leaf, Node::Group(id));
        let new = Node::Group(id);
        let (first, second) = if edge.is_leading() {
            (new, old)
        } else {
            (old, new)
        };
        *leaf = Node::split(edge.axis(), vec![first, second]);
        self.groups.insert(id, group);
        self.zoomed = None;
        self.normalize();
        Some(id)
    }

    /// Remove `group` and collapse its tile: the adjacent sibling (the one
    /// before it, else after) takes its space and, if `group` was focused,
    /// focus. The only group is never removed.
    fn remove_group(&mut self, group: GroupId) -> bool {
        if self.groups.len() <= 1 {
            return false;
        }
        let Some(path) = self.root.path_to(group) else {
            return false;
        };
        let Some((&index, parent)) = path.split_last() else {
            return false;
        };
        let Some(Node::Split { children, .. }) = self.root.at_mut(parent) else {
            return false;
        };
        let (_, fraction) = children.remove(index);
        let heir = if index > 0 { index - 1 } else { 0 };
        let Some((sibling, sibling_fraction)) = children.get_mut(heir) else {
            return false;
        };
        *sibling_fraction += fraction;
        let heir = if index > 0 {
            sibling.last_group()
        } else {
            sibling.first_group()
        };
        self.groups.remove(&group);
        if self.focused == group
            && let Some(heir) = heir
        {
            self.focused = heir;
        }
        if self.zoomed == Some(group) {
            self.zoomed = None;
        }
        self.normalize();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str) -> TabKey {
        TabKey::session(id)
    }

    /// Assert every documented invariant.
    fn check(ws: &Workspace) {
        let order = ws.groups_in_reading_order();
        let tree: HashSet<_> = order.iter().copied().collect();
        assert_eq!(tree.len(), order.len(), "group repeated in tree");
        let map: HashSet<_> = ws.groups.keys().copied().collect();
        assert_eq!(tree, map, "tree and map disagree");
        let mut tabs = HashSet::new();
        for group in ws.groups.values() {
            for tab in &group.tabs {
                assert!(tabs.insert(tab.clone()), "duplicate tab {tab:?}");
            }
            assert!(group.active < group.tabs.len().max(1));
        }
        fn walk(node: &Node, parent: Option<Axis>) {
            if let Node::Split { axis, children } = node {
                assert!(
                    children.len() >= 2,
                    "split with {} children",
                    children.len()
                );
                assert_ne!(Some(*axis), parent, "same-axis nesting");
                let sum: f32 = children.iter().map(|(_, f)| f).sum();
                assert!((sum - 1.0).abs() < 1e-4, "fractions sum {sum}");
                for (child, _) in children {
                    walk(child, Some(*axis));
                }
            }
        }
        walk(&ws.root, None);
        assert!(ws.groups.contains_key(&ws.focused));
        if let Some(z) = ws.zoomed {
            assert!(ws.groups.contains_key(&z));
        }
        assert!(ws.groups.keys().all(|id| id.0 < ws.next_id));
    }

    /// Tabs per group in reading order.
    fn shape(ws: &Workspace) -> Vec<Vec<TabKey>> {
        ws.groups_in_reading_order()
            .into_iter()
            .map(|id| ws.groups[&id].tabs.clone())
            .collect()
    }

    fn fractions(node: &Node) -> Vec<f32> {
        match node {
            Node::Split { children, .. } => children.iter().map(|(_, f)| *f).collect(),
            Node::Group(_) => vec![],
        }
    }

    fn approx(a: &[f32], b: &[f32]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    fn with_tabs(preset: Preset, tabs: &[&str]) -> Workspace {
        let mut ws = Workspace::new();
        for t in tabs {
            ws.open(s(t));
        }
        ws.apply_preset(preset);
        check(&ws);
        ws
    }

    #[test]
    fn new_is_one_empty_focused_group() {
        let ws = Workspace::new();
        check(&ws);
        assert_eq!(ws.group_count(), 1);
        assert!(ws.group(ws.focused()).unwrap().is_empty());
        assert_eq!(ws.focused_tab(), None);
    }

    #[test]
    fn open_adds_to_focused_and_refocuses_existing() {
        let mut ws = Workspace::new();
        let g0 = ws.focused();
        ws.open(s("a"));
        ws.open(s("b"));
        assert_eq!(ws.focused_tab(), Some(&s("b")));
        let g1 = ws.split_group(g0, Edge::Right).unwrap();
        assert_eq!(ws.focused(), g1);
        ws.open(s("c"));
        assert_eq!(ws.find(&s("c")), Some((g1, 0)));
        // Already open: activate it where it is, don't duplicate.
        assert_eq!(ws.open(s("a")), g0);
        assert_eq!(ws.focused(), g0);
        assert_eq!(ws.focused_tab(), Some(&s("a")));
        assert_eq!(ws.tabs().count(), 3);
        check(&ws);
    }

    #[test]
    fn open_in_dead_group_falls_back_to_focused() {
        let mut ws = Workspace::new();
        let g = ws.open_in(GroupId(99), s("a"));
        assert_eq!(g, ws.focused());
        check(&ws);
    }

    #[test]
    fn preset_shapes() {
        use Axis::{Horizontal as H, Vertical as V};
        let top = |ws: &Workspace| match &ws.root {
            Node::Split { axis, children } => Some((*axis, children.len())),
            Node::Group(_) => None,
        };
        let ws = with_tabs(Preset::Single, &[]);
        assert_eq!((ws.group_count(), top(&ws)), (1, None));
        for (preset, axis, n) in [
            (Preset::Columns2, H, 2),
            (Preset::Rows2, V, 2),
            (Preset::Columns3, H, 3),
            (Preset::Rows3, V, 3),
        ] {
            let ws = with_tabs(preset, &[]);
            assert_eq!(ws.group_count(), n);
            assert_eq!(top(&ws), Some((axis, n)));
            let even = vec![1.0 / n as f32; n];
            assert!(approx(&fractions(&ws.root), &even));
        }
        for (preset, n) in [(Preset::Grid2x2, 2), (Preset::Grid3x3, 3)] {
            let ws = with_tabs(preset, &[]);
            assert_eq!(ws.group_count(), n * n);
            assert_eq!(top(&ws), Some((V, n)));
            let rects = ws.group_rects();
            let cell = 1.0 / n as f32;
            for (i, (_, r)) in rects.iter().enumerate() {
                let (row, col) = (i / n, i % n);
                assert!((r.x - col as f32 * cell).abs() < 1e-4);
                assert!((r.y - row as f32 * cell).abs() < 1e-4);
                assert!((r.w - cell).abs() < 1e-4 && (r.h - cell).abs() < 1e-4);
            }
        }
        let ws = with_tabs(Preset::TwoStackedPlusOne, &[]);
        let rects: Vec<Rect> = ws.group_rects().into_iter().map(|(_, r)| r).collect();
        assert_eq!(rects.len(), 3);
        assert!(rects[0].x == 0.0 && rects[0].y == 0.0 && rects[0].h == 0.5);
        assert!(rects[1].x == 0.0 && rects[1].y == 0.5);
        assert!(rects[2].x == 0.5 && rects[2].h == 1.0);
        let ws = with_tabs(Preset::OnePlusTwoStacked, &[]);
        let rects: Vec<Rect> = ws.group_rects().into_iter().map(|(_, r)| r).collect();
        assert!(rects[0].x == 0.0 && rects[0].h == 1.0);
        assert!(rects[1].x == 0.5 && rects[1].y == 0.0);
        assert!(rects[2].x == 0.5 && rects[2].y == 0.5);
        assert_eq!(Preset::ALL.len(), 9);
    }

    #[test]
    fn preset_distributes_tabs_and_keeps_focus() {
        // Fewer tabs than groups: one each, rest empty.
        let mut ws = Workspace::new();
        ws.open(s("a"));
        ws.open(s("b"));
        ws.activate(ws.focused(), 0);
        ws.apply_preset(Preset::Columns3);
        check(&ws);
        assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("b")], vec![]]);
        assert_eq!(ws.focused_tab(), Some(&s("a")));

        // More tabs than groups: leftovers join the last group, focus follows.
        let mut ws = Workspace::new();
        for t in ["a", "b", "c", "d"] {
            ws.open(s(t));
        }
        ws.activate(ws.focused(), 2);
        let before = ws.focused();
        ws.apply_preset(Preset::Columns2);
        check(&ws);
        assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("b"), s("c"), s("d")]]);
        assert_eq!(ws.focused_tab(), Some(&s("c")));
        // Existing group ids are reused in reading order.
        assert_eq!(ws.groups_in_reading_order()[0], before);

        // Back to single: everything in one group, focused tab still active.
        ws.apply_preset(Preset::Single);
        check(&ws);
        assert_eq!(shape(&ws), vec![vec![s("a"), s("b"), s("c"), s("d")]]);
        assert_eq!(ws.focused_tab(), Some(&s("c")));
    }

    #[test]
    fn close_collapses_group_and_moves_focus() {
        let mut ws = with_tabs(Preset::Columns3, &["a", "b", "c"]);
        let ids = ws.groups_in_reading_order();
        ws.focus(ids[1]);
        assert!(ws.close(&s("b")));
        check(&ws);
        assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("c")]]);
        // The left neighbour absorbs the space and the focus.
        assert_eq!(ws.focused(), ids[0]);
        assert!(approx(&fractions(&ws.root), &[2.0 / 3.0, 1.0 / 3.0]));

        // Collapsing to one group replaces the split with the group.
        assert!(ws.close(&s("a")));
        check(&ws);
        assert_eq!(ws.root, Node::Group(ids[2]));
        assert_eq!(ws.focused(), ids[2]);

        // The last group may be empty.
        assert!(ws.close(&s("c")));
        check(&ws);
        assert_eq!(ws.group_count(), 1);
        assert!(!ws.close(&s("c")));
    }

    #[test]
    fn close_keeps_neighbour_active_tab() {
        let mut ws = Workspace::new();
        for t in ["a", "b", "c"] {
            ws.open(s(t));
        }
        let g = ws.focused();
        ws.activate(g, 1);
        ws.close(&s("a"));
        assert_eq!(ws.focused_tab(), Some(&s("b")));
        ws.close(&s("b"));
        assert_eq!(ws.focused_tab(), Some(&s("c")));
        check(&ws);
    }

    #[test]
    fn close_in_nested_split_collapses_to_parent() {
        // 2x2; closing one cell turns its row into a single group, which must
        // not leave a vertical split nested in a vertical split.
        let mut ws = with_tabs(Preset::Grid2x2, &["a", "b", "c", "d"]);
        ws.close(&s("b"));
        check(&ws);
        let Node::Split { axis, children } = &ws.root else {
            panic!("expected split");
        };
        assert_eq!(*axis, Axis::Vertical);
        assert!(matches!(children[0].0, Node::Group(_)));
        assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("c")], vec![s("d")]]);
    }

    #[test]
    fn move_to_edge_creates_split() {
        let mut ws = Workspace::new();
        ws.open(s("a"));
        ws.open(s("b"));
        let g0 = ws.focused();
        assert!(ws.move_tab(&s("b"), g0, Drop::Edge(Edge::Right)));
        check(&ws);
        assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("b")]]);
        let Node::Split { axis, .. } = &ws.root else {
            panic!()
        };
        assert_eq!(*axis, Axis::Horizontal);
        assert_eq!(ws.focused_tab(), Some(&s("b")));

        // Only tab onto its own edge: no-op.
        let g1 = ws.find(&s("b")).unwrap().0;
        let snapshot = ws.clone();
        assert!(!ws.move_tab(&s("b"), g1, Drop::Edge(Edge::Bottom)));
        assert_eq!(ws, snapshot);

        // Moving the only tab out removes its group.
        assert!(ws.move_tab(&s("b"), g0, Drop::Edge(Edge::Top)));
        check(&ws);
        assert_eq!(shape(&ws), vec![vec![s("b")], vec![s("a")]]);
        let Node::Split { axis, .. } = &ws.root else {
            panic!()
        };
        assert_eq!(*axis, Axis::Vertical);
    }

    #[test]
    fn move_to_center_joins_group() {
        let mut ws = with_tabs(Preset::Columns2, &["a", "b"]);
        let ids = ws.groups_in_reading_order();
        assert!(ws.move_tab(&s("a"), ids[1], Drop::Center));
        check(&ws);
        assert_eq!(shape(&ws), vec![vec![s("b"), s("a")]]);
        assert_eq!(ws.focused_tab(), Some(&s("a")));
        // Own centre just activates.
        assert!(!ws.move_tab(&s("b"), ids[1], Drop::Center));
        assert_eq!(ws.focused_tab(), Some(&s("b")));
        // Unknown tab / group.
        assert!(!ws.move_tab(&s("zz"), ids[1], Drop::Center));
        assert!(!ws.move_tab(&s("a"), GroupId(999), Drop::Center));
    }

    #[test]
    fn move_within_reorders_and_activates() {
        let mut ws = Workspace::new();
        for t in ["a", "b", "c"] {
            ws.open(s(t));
        }
        let g = ws.focused();
        assert!(ws.move_within(g, 2, 0));
        check(&ws);
        assert_eq!(shape(&ws), vec![vec![s("c"), s("a"), s("b")]]);
        assert_eq!(ws.focused_tab(), Some(&s("c")));
        assert!(ws.move_within(g, 0, 2));
        assert_eq!(shape(&ws), vec![vec![s("a"), s("b"), s("c")]]);
        // Same slot just activates; out of range / unknown group: no-op.
        assert!(!ws.move_within(g, 1, 1));
        assert_eq!(ws.focused_tab(), Some(&s("b")));
        let snapshot = ws.clone();
        assert!(!ws.move_within(g, 0, 3));
        assert!(!ws.move_within(GroupId(999), 0, 1));
        assert_eq!(ws, snapshot);
    }

    #[test]
    fn move_to_index_joins_at_position() {
        let mut ws = with_tabs(Preset::Columns2, &["a", "b"]);
        let before = shape(&ws);
        assert_eq!(before.len(), 2);
        assert_eq!(before[1].len(), 1);
        let ids = ws.groups_in_reading_order();
        let lone = before[1][0].clone();
        // Into the first group's bar at the front; its old group collapses.
        assert!(ws.move_to_index(&lone, ids[0], 0));
        check(&ws);
        assert_eq!(ws.group_count(), 1);
        assert_eq!(ws.group(ids[0]).unwrap().tabs()[0], lone);
        assert_eq!(ws.focused_tab(), Some(&lone));
        // Past the end clamps (append); within the group it reorders.
        let first = ws.group(ids[0]).unwrap().tabs()[0].clone();
        assert!(ws.move_to_index(&first, ids[0], 99));
        check(&ws);
        assert_eq!(ws.group(ids[0]).unwrap().tabs().last(), Some(&first));
        assert_eq!(ws.focused_tab(), Some(&first));
        // Unknown tab / group.
        assert!(!ws.move_to_index(&s("zz"), ids[0], 0));
        assert!(!ws.move_to_index(&first, GroupId(999), 0));
    }

    #[test]
    fn move_to_index_keeps_a_nonempty_source() {
        let mut ws = Workspace::new();
        ws.open(s("a"));
        ws.open(s("b"));
        let left = ws.focused();
        let right = ws.split_group(left, Edge::Right).unwrap();
        assert!(ws.move_to_index(&s("a"), right, 0));
        check(&ws);
        assert_eq!(shape(&ws), vec![vec![s("b")], vec![s("a")]]);
        assert_eq!(ws.focused(), right);
        assert_eq!(ws.group(left).unwrap().active_tab(), Some(&s("b")));
    }

    #[test]
    fn same_axis_splits_flatten() {
        let mut ws = Workspace::new();
        let g0 = ws.focused();
        let g1 = ws.split_group(g0, Edge::Right).unwrap();
        let g2 = ws.split_group(g1, Edge::Right).unwrap();
        check(&ws);
        let Node::Split { axis, children } = &ws.root else {
            panic!()
        };
        assert_eq!(*axis, Axis::Horizontal);
        assert_eq!(children.len(), 3);
        // Splitting halves the target.
        assert!(approx(&fractions(&ws.root), &[0.5, 0.25, 0.25]));
        assert_eq!(ws.groups_in_reading_order(), vec![g0, g1, g2]);

        // Directly normalize a nested same-axis tree.
        let mut ws = Workspace::new();
        let [a, b, c] = [GroupId(10), GroupId(11), GroupId(12)];
        for id in [a, b, c] {
            ws.groups.insert(id, Group::default());
        }
        ws.root = Node::Split {
            axis: Axis::Vertical,
            children: vec![
                (Node::Group(a), 0.5),
                (
                    Node::Split {
                        axis: Axis::Vertical,
                        children: vec![(Node::Group(b), 0.4), (Node::Group(c), 0.6)],
                    },
                    0.5,
                ),
            ],
        };
        ws.normalize();
        check(&ws);
        assert!(approx(&fractions(&ws.root), &[0.5, 0.2, 0.3]));
        assert!(
            !ws.groups.contains_key(&GroupId(0)),
            "unreferenced group dropped"
        );
    }

    #[test]
    fn two_stacked_plus_one_via_ops() {
        let mut ws = Workspace::new();
        ws.open(s("a"));
        ws.open(s("b"));
        ws.open(s("c"));
        let g0 = ws.focused();
        ws.move_tab(&s("c"), g0, Drop::Edge(Edge::Right));
        ws.move_tab(&s("b"), g0, Drop::Edge(Edge::Bottom));
        check(&ws);
        let ops = ws.clone();
        let preset = with_tabs(Preset::TwoStackedPlusOne, &["a", "b", "c"]);
        assert_eq!(shape(&ops), shape(&preset));
        let rects = |ws: &Workspace| -> Vec<Rect> {
            ws.group_rects().into_iter().map(|(_, r)| r).collect()
        };
        assert_eq!(rects(&ops), rects(&preset));
    }

    #[test]
    fn resize_clamps_and_equalize() {
        let mut ws = with_tabs(Preset::Columns3, &[]);
        assert!(ws.resize(&[], 0, 0.1, 0.1));
        assert!(approx(
            &fractions(&ws.root),
            &[1.0 / 3.0 + 0.1, 1.0 / 3.0 - 0.1, 1.0 / 3.0]
        ));
        // Clamped at the minimum.
        assert!(ws.resize(&[], 0, 5.0, 0.1));
        let f = fractions(&ws.root);
        assert!((f[1] - 0.1).abs() < 1e-4 && (f[0] + f[1] - 2.0 / 3.0).abs() < 1e-4);
        assert!(!ws.resize(&[], 0, 1.0, 0.1), "already at the limit");
        assert!(!ws.resize(&[], 1, -5.0, 0.1), "child 1 is at its minimum");
        assert!(ws.resize(&[], 1, 5.0, 0.1));
        assert!((fractions(&ws.root)[2] - 0.1).abs() < 1e-4);
        check(&ws);
        // Bad input.
        assert!(!ws.resize(&[], 2, 0.1, 0.1));
        assert!(!ws.resize(&[0], 0, 0.1, 0.1));
        assert!(!ws.resize(&[], 0, f32::NAN, 0.1));
        // An impossible minimum can't panic or invert.
        ws.resize(&[], 0, 0.3, 0.9);
        check(&ws);
        assert!(ws.equalize(&[]));
        assert!(approx(&fractions(&ws.root), &[1.0 / 3.0; 3]));

        // Nested path.
        let mut ws = with_tabs(Preset::Grid2x2, &[]);
        assert!(ws.resize(&[1], 0, 0.2, 0.05));
        assert!(approx(&fractions(ws.root.at(&[1]).unwrap()), &[0.7, 0.3]));
        check(&ws);
    }

    #[test]
    fn zoom() {
        let mut ws = with_tabs(Preset::Columns2, &["a", "b"]);
        let ids = ws.groups_in_reading_order();
        assert!(ws.toggle_zoom(ids[1]));
        assert_eq!(ws.zoomed(), Some(ids[1]));
        assert_eq!(ws.focused(), ids[1]);
        assert_eq!(ws.visible_tabs(), vec![&s("b")]);
        assert!(!ws.toggle_zoom(ids[1]));
        assert_eq!(ws.visible_tabs(), vec![&s("a"), &s("b")]);
        // Removing the zoomed group clears zoom.
        ws.toggle_zoom(ids[1]);
        ws.close(&s("b"));
        assert_eq!(ws.zoomed(), None);
        check(&ws);
        // Splitting clears zoom.
        ws.toggle_zoom(ids[0]);
        ws.split_group(ids[0], Edge::Right);
        assert_eq!(ws.zoomed(), None);
    }

    #[test]
    fn retain_tabs_prunes_and_collapses() {
        let mut ws = with_tabs(Preset::Columns3, &["a", "b", "c", "d"]);
        // Last group holds c, d with d active.
        let last = ws.groups_in_reading_order()[2];
        ws.activate(last, 1);
        assert!(ws.retain_tabs(|t| t != &s("b") && t != &s("c")));
        check(&ws);
        assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("d")]]);
        assert_eq!(ws.group(last).unwrap().active_tab(), Some(&s("d")));
        assert!(!ws.retain_tabs(|_| true));
        assert!(ws.retain_tabs(|_| false));
        check(&ws);
        assert_eq!(ws.group_count(), 1);
        assert_eq!(ws.tabs().count(), 0);
    }

    #[test]
    fn new_session_tab_and_replace() {
        let mut ws = Workspace::new();
        let canvas = ws.new_session_tab();
        let other = ws.new_session_tab();
        assert_ne!(canvas, other);
        ws.open(canvas.clone());
        assert!(ws.replace_tab(&canvas, s("chat")));
        assert_eq!(ws.focused_tab(), Some(&s("chat")));
        assert!(!ws.contains(&canvas));
        // Replacing with an already-open session focuses it and drops the old.
        let g0 = ws.focused();
        let canvas = ws.new_session_tab();
        ws.open_split(canvas.clone(), g0, Edge::Right).unwrap();
        assert!(ws.replace_tab(&canvas, s("chat")));
        check(&ws);
        assert_eq!(shape(&ws), vec![vec![s("chat")]]);
        assert_eq!(s("chat").chat_id(), Some("chat"));
        assert_eq!(canvas.chat_id(), None);
    }

    #[test]
    fn open_split_places_or_moves() {
        let mut ws = Workspace::new();
        ws.open(s("a"));
        let g0 = ws.focused();
        let g1 = ws.open_split(s("b"), g0, Edge::Right).unwrap();
        assert_eq!(ws.focused(), g1);
        assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("b")]]);
        // Already open elsewhere: moves.
        let g2 = ws.open_split(s("a"), g1, Edge::Bottom).unwrap();
        check(&ws);
        assert_eq!(ws.find(&s("a")), Some((g2, 0)));
        assert_eq!(ws.group_count(), 2);
        assert!(ws.open_split(s("c"), GroupId(999), Edge::Left).is_none());
    }

    #[test]
    fn serde_round_trip() {
        let mut ws = with_tabs(Preset::Grid2x2, &["a", "b", "c"]);
        let canvas = ws.new_session_tab();
        ws.open(canvas);
        ws.resize(&[0], 0, 0.1, 0.05);
        ws.toggle_zoom(ws.groups_in_reading_order()[1]);
        let json = serde_json::to_string(&ws).unwrap();
        assert!(json.contains("\"nextId\""), "camelCase: {json}");
        let back: Workspace = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ws);
        check(&back);
    }

    #[test]
    fn repair_corrupted_input() {
        // Missing everything.
        let ws: Workspace = serde_json::from_str("{}").unwrap();
        check(&ws);
        assert_eq!(ws.group_count(), 1);

        // Dangling tree ids, unreferenced groups, duplicate tabs and leaf ids,
        // bad fractions, single-child and same-axis splits, dead focus/zoom,
        // out-of-range active, stale nextId.
        let json = r#"{
            "root": {"split": {"axis": "horizontal", "children": [
                [{"group": 1}, -3.0],
                [{"split": {"axis": "horizontal", "children": [
                    [{"group": 2}, 1.0], [{"group": 7}, 1.0]
                ]}}, 0.0],
                [{"split": {"axis": "vertical", "children": [[{"group": 3}, 1.0]]}}, 2.0],
                [{"group": 1}, 1.0]
            ]}},
            "groups": {
                "1": {"tabs": [{"session": "a"}, {"session": "b"}], "active": 9},
                "2": {"tabs": [{"session": "b"}, {"newSession": 40}], "active": 0},
                "3": {"tabs": [{"session": "a"}]},
                "5": {"tabs": [{"session": "x"}]}
            },
            "focused": 42,
            "zoomed": 7,
            "nextId": 0
        }"#;
        let ws: Workspace = serde_json::from_str(json).unwrap();
        check(&ws);
        assert_eq!(
            ws.groups_in_reading_order(),
            vec![GroupId(1), GroupId(2), GroupId(3)]
        );
        assert_eq!(
            shape(&ws),
            vec![vec![s("a"), s("b")], vec![TabKey::NewSession(40)], vec![]]
        );
        assert_eq!(ws.group(GroupId(1)).unwrap().active(), 1);
        assert_eq!(ws.focused(), GroupId(1));
        assert_eq!(ws.zoomed(), None);
        assert!(ws.next_id > 40);
        let Node::Split { axis, children } = &ws.root else {
            panic!()
        };
        assert_eq!(*axis, Axis::Horizontal);
        assert_eq!(children.len(), 3);

        // A tree with no live groups gets a fresh one.
        let ws: Workspace =
            serde_json::from_str(r#"{"root": {"group": 3}, "groups": {}}"#).unwrap();
        check(&ws);
        assert_eq!(ws.group_count(), 1);
    }

    #[test]
    fn neighbours_in_grids() {
        let ws = with_tabs(Preset::Grid2x2, &[]);
        let g = ws.groups_in_reading_order();
        assert_eq!(ws.neighbour(g[0], Edge::Right), Some(g[1]));
        assert_eq!(ws.neighbour(g[0], Edge::Bottom), Some(g[2]));
        assert_eq!(ws.neighbour(g[3], Edge::Left), Some(g[2]));
        assert_eq!(ws.neighbour(g[3], Edge::Top), Some(g[1]));
        assert_eq!(ws.neighbour(g[0], Edge::Left), None);
        assert_eq!(ws.neighbour(g[0], Edge::Top), None);

        let ws = with_tabs(Preset::Grid3x3, &[]);
        let g = ws.groups_in_reading_order();
        let centre = g[4];
        assert_eq!(ws.neighbour(centre, Edge::Left), Some(g[3]));
        assert_eq!(ws.neighbour(centre, Edge::Right), Some(g[5]));
        assert_eq!(ws.neighbour(centre, Edge::Top), Some(g[1]));
        assert_eq!(ws.neighbour(centre, Edge::Bottom), Some(g[7]));
        assert_eq!(ws.neighbour(g[8], Edge::Right), None);
        assert_eq!(ws.neighbour(g[2], Edge::Bottom), Some(g[5]));

        // Uneven: from the tall right tile, the longest shared border wins;
        // ties go to reading order.
        let mut ws = with_tabs(Preset::TwoStackedPlusOne, &[]);
        let g = ws.groups_in_reading_order();
        assert_eq!(ws.neighbour(g[2], Edge::Left), Some(g[0]));
        ws.resize(&[0], 0, 0.2, 0.05);
        assert_eq!(ws.neighbour(g[2], Edge::Left), Some(g[0]));
        ws.resize(&[0], 0, -0.4, 0.05);
        assert_eq!(ws.neighbour(g[2], Edge::Left), Some(g[1]));
        assert_eq!(ws.neighbour(g[1], Edge::Right), Some(g[2]));
        assert_eq!(ws.neighbour(GroupId(999), Edge::Right), None);
    }

    #[test]
    fn close_group_hands_tabs_and_focus_to_the_heir() {
        let mut ws = Workspace::new();
        ws.open(s("a"));
        let right = ws.open_split(s("b"), ws.focused(), Edge::Right).unwrap();
        ws.open_in(right, s("c"));
        ws.activate(right, 0);
        let left = ws.groups_in_reading_order()[0];
        assert!(ws.close_group(right));
        check(&ws);
        assert_eq!(ws.group_count(), 1);
        assert_eq!(shape(&ws), vec![vec![s("a"), s("b"), s("c")]]);
        // The heir keeps its own active tab and takes focus.
        assert_eq!(ws.focused(), left);
        assert_eq!(ws.focused_tab(), Some(&s("a")));
        // The last group never closes.
        assert!(!ws.close_group(left));
        check(&ws);
    }

    #[test]
    fn close_group_into_an_empty_heir_keeps_the_moved_active_tab() {
        let mut ws = Workspace::new();
        let left = ws.focused();
        let right = ws.split_group(left, Edge::Right).unwrap();
        ws.open_in(right, s("a"));
        ws.open_in(right, s("b"));
        ws.activate(right, 0);
        // Focus elsewhere survives closing an unfocused group.
        ws.focus(left);
        assert!(ws.close_group(right));
        check(&ws);
        assert_eq!(ws.focused(), left);
        assert_eq!(ws.focused_tab(), Some(&s("a")));
        assert!(!ws.close_group(GroupId(999)));
    }
}
