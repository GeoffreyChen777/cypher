//! The right-pane "Changes" content: switchable
//! unified/split diff views over `WatchCheckoutDiffs`.
//!
//! - pure patch parser: `diff --git` sections → file/hunk/line/notice rows,
//!   with add/delete/rename/binary detection and per-file counts;
//! - resolution: the shown diff matches the selected chat by `checkout_id`
//!   first, then by device+cwd, then cwd alone;
//! - states: *preparing* (no diff yet), *clean* (empty patch), *list*; a watch
//!   error shows a banner while the last content stays;
//! - virtualized with gpui `list()` at LINE granularity — every file header,
//!   hunk header, and diff line is its own row (the flat model Zed's editor
//!   uses for its project diff: only the visible slice materializes, and a
//!   collapsed file's body rows are removed from the list outright, not
//!   hidden); each section collapses with a 180 ms height tween on a
//!   clipped stand-in row (analytic heights, capped to what the clip can
//!   reveal) and a 200 ms chevron transition;
//! - syntax highlight reuses the markdown tokenizer per diff line, computed
//!   time-sliced on the background executor and applied as paint-only run
//!   colors (layout never changes);
//! - scopes (t3code parity): *Working tree* rides the watch stream; *Branch
//!   changes* (vs a selectable base ref, default branch preselected) and
//!   *Latest turn* fetch one-shot `GetCheckoutDiff` captures, refreshed when
//!   the watch checksum says the tree moved.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable as _, ListAlignment, ListScrollEvent,
    ListState, SharedString, Subscription, Task, Window, div, list, prelude::*, px,
};

use cypher_proto::{Chat, CheckoutDiff, GitHistoryCommit};
use cypher_rpc::methods;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::history::{GitHistory, GitHistoryCount, GitHistoryEvent, GitHistoryFetchButton};
use crate::markdown::render;
use crate::motion::{self, AnimationExt as _, CHEVRON, COLLAPSE};
use crate::popover::{self, Popup};
use crate::state::{AppState, EngineHandle};
use crate::theme::{MonoStyled, Theme};
use cypher_syntax::LanguageId as Lang;

pub mod layout;
mod patch;
pub use patch::*;
mod resolution;
pub use resolution::*;
mod rows;
pub use rows::*;
mod comments;
mod rendering;
use layout::{DiffLayout, Side};
pub use rendering::*;

// ---------------------------------------------------------------------------
// Layout numbers (analytic — they drive the fold tween)
// ---------------------------------------------------------------------------

pub const FILE_HEADER_HEIGHT: f32 = 36.0;
pub const HUNK_HEADER_HEIGHT: f32 = 28.0;
pub const DIFF_LINE_HEIGHT: f32 = 21.0;
pub const NOTICE_HEIGHT: f32 = 24.0;
pub const BODY_BOTTOM_PAD: f32 = 8.0;
/// Gutter width per line-number column.
pub const GUTTER_WIDTH: f32 = 36.0;
/// The +/−/· marker column between the gutters and the code.
pub const MARKER_WIDTH: f32 = 28.0;
/// Width of the coloured accent bar on the left edge of +/− rows.
pub const ACCENT_BAR_WIDTH: f32 = 3.0;
const DIFF_TEXT_SIZE: f32 = 12.0;

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

struct ParsedDiff {
    /// `checkout_id:checksum` — identity of the parsed content.
    key: String,
    truncated: bool,
    additions: u32,
    deletions: u32,
    file_count: usize,
    files: Arc<Vec<FileDiff>>,
}

#[derive(Default, Clone, Copy)]
struct FileFold {
    collapsed: bool,
    /// Bumped per toggle — keys the height tween + chevron transition.
    epoch: usize,
    from: f32,
    to: f32,
    /// When the toggle happened: the tweens are armed only briefly after the
    /// click — gpui replays an element's animation on remount, and in the
    /// virtualized list a row scrolling back into view is a remount (the
    /// transcript's tool groups had the same flash; user report).
    toggled_at: Option<std::time::Instant>,
}

/// Tween arming window after a fold toggle (COLLAPSE's 180ms plus margin).
const FOLD_TWEEN_WINDOW: Duration = Duration::from_millis(400);

/// Ceiling on how much body a fold tween's stand-in row materializes. A
/// tween always starts from a clicked (on-screen) header, so the revealable
/// slice is at most one viewport tall — everything past this is clipped or
/// below the fold either way.
const FOLD_TWEEN_MAX_PX: f32 = 2400.0;

impl FileFold {
    fn animating(&self) -> bool {
        self.epoch > 0
            && self
                .toggled_at
                .is_some_and(|at| at.elapsed() < FOLD_TWEEN_WINDOW)
    }
}

struct HighlightSlot {
    fingerprint: u64,
    state: DiffHighlightState,
    _excerpt_task: Option<Task<()>>,
    _fetch_task: Option<Task<()>>,
}

enum DiffHighlightState {
    Pending,
    Ready(Arc<DiffHighlights>),
    Excerpt(Arc<DiffHighlights>),
    Plain,
}

/// The open base-ref dropdown — the same searchable-menu recipe as the
/// composer's ref picker and the spaces filter: a filter input on top
/// (`PaletteSearch` context so ↑↓/⏎ bubble to the card's key handler),
/// ranked substring rows below.
struct RefMenu {
    search: Entity<ComposerInput>,
    /// Keyboard highlight within the filtered rows.
    active: usize,
    /// Tracked on the card — puts it on the keyboard dispatch path while the
    /// search input holds focus (the structure every working picker uses).
    focus: FocusHandle,
    list_scroll: gpui::ScrollHandle,
    _search_events: Subscription,
}

/// The Changes pane entity. Lazy: no RPC until [`Changes::ensure_watch`] runs
/// (the shell calls it when the pane first opens).
pub struct Changes {
    view_layout: DiffLayout,
    layout_error: Option<SharedString>,
    split_scopes: [crate::markdown::selection::SelectionScope; 2],
    horizontal: [f32; 2],
    max_columns: [usize; 2],
    max_gutter: f32,
    pane_width: Rc<std::cell::Cell<f32>>,
    mono_advance: f32,
    state: Entity<AppState>,
    diffs: Vec<CheckoutDiff>,
    started: bool,
    error: Option<SharedString>,
    /// Device the running watch targets: `None` = the connected engine itself,
    /// `Some(id)` = a remote chat's host (relay-forwarded). The stream only
    /// carries the TARGET device's checkouts, so a selection change onto a
    /// chat hosted elsewhere tears the watch down and re-subscribes.
    watch_target: Option<String>,
    watch_task: Option<Task<()>>,
    parsed: Option<ParsedDiff>,
    parse_task: Option<Task<()>>,
    folds: HashMap<String, FileFold>,
    highlights: HashMap<String, HighlightSlot>,
    /// The flattened row model the list virtualizes over (line granularity;
    /// collapsed bodies excluded) + each file's row span within it.
    rows: Vec<DiffRow>,
    row_ranges: Vec<std::ops::Range<usize>>,
    /// Sweeps [`DiffRow::FoldingBody`] stand-ins back to steady-state rows
    /// once their tween window elapses.
    fold_settle: Option<Task<()>>,
    list: ListState,
    /// What the pane diffs against (toolbar dropdown).
    scope: DiffScope,
    /// Comparison ref for [`DiffScope::Branch`] — preset to the repo's
    /// default branch once the branch list lands.
    base_ref: Option<String>,
    branches: Vec<String>,
    /// `device:cwd` the branch list was fetched for.
    branches_for: Option<String>,
    branches_task: Option<Task<()>>,
    /// One-shot scoped capture (Branch / Latest turn) + its fetch key.
    scoped: Option<CheckoutDiff>,
    scoped_for: Option<String>,
    scoped_error: Option<SharedString>,
    scoped_inflight: Option<String>,
    scoped_task: Option<Task<()>>,
    scope_menu: Popup<()>,
    ref_menu: Popup<RefMenu>,
    history: Option<Entity<GitHistory>>,
    history_count: Option<Entity<GitHistoryCount>>,
    history_fetch_button: Option<Entity<GitHistoryFetchButton>>,
    history_events: Option<Subscription>,
    /// Pinned commit for a [`DiffScope::Commit`] pane (sha + subject drive
    /// the fetch and the surface-tab title).
    commit: Option<GitHistoryCommit>,
    /// Per-pane owner token prefixing every selectable diff-line key — two
    /// diff tabs never collide, and a closed pane's keys can't be claimed by
    /// a new one.
    owner: String,
    /// This pane's own selection scope (allocated per pane). The selection
    /// registry, wash, listeners, clear and popup owner all ride it, so a
    /// hidden or background pane never affects the active one.
    sel_scope: crate::markdown::selection::SelectionScope,
    /// The shared shell-level Comment pill/editor (weak — the shell owns it).
    comment_popup: gpui::WeakEntity<crate::comments::CommentPopup>,
    _observe: Subscription,
    _layout_observe: Subscription,
}

/// Events the host (the right pane's surface strip) listens for.
pub enum ChangesEvent {
    /// A History row was clicked — open this commit as its own diff tab.
    OpenCommit(GitHistoryCommit),
}

impl gpui::EventEmitter<ChangesEvent> for Changes {}

struct DiffLayoutTooltip(DiffLayout);
impl Render for DiffLayoutTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(5.0))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border_strong)
            .text_size(px(11.0))
            .text_color(theme.text)
            .child(match self.0 {
                DiffLayout::Unified => "Unified diff",
                DiffLayout::Split => "Side-by-side diff",
            })
    }
}

impl Changes {
    fn horizontal_limit(&self, side: Side) -> f32 {
        let gutter = self.max_gutter;
        let visible = (self.pane_width.get() * 0.5 - gutter - 18.0 - 8.0 - 1.0).max(1.0);
        (self.max_columns[side.index()] as f32 * self.mono_advance + 16.0 - visible).max(0.0)
    }
    fn scroll_side(&mut self, side: Side, amount: f32, cx: &mut Context<Self>) {
        let next = (self.horizontal[side.index()] + amount).clamp(0.0, self.horizontal_limit(side));
        if next != self.horizontal[side.index()] {
            self.horizontal[side.index()] = next;
            self.invalidate_selection(cx);
            cx.notify();
        }
    }
    fn split_headers(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .h(px(28.0))
            .flex_none()
            .flex()
            .border_b_1()
            .border_color(theme.border)
            .children([Side::Old, Side::New].map(|side| {
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .when(side == Side::Old, |el| {
                        el.border_r_1().border_color(theme.border)
                    })
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(side.label()),
                    )
                    .children(
                        [
                            (-120.0, crate::icons::ALT_ARROW_LEFT),
                            (120.0, crate::icons::ALT_ARROW_RIGHT),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(i, (step, icon))| {
                            div()
                                .id(SharedString::from(format!(
                                    "diff-scroll-{}-{i}",
                                    side.label()
                                )))
                                .size(px(22.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .child(
                                    crate::icons::icon(icon)
                                        .size(px(12.0))
                                        .text_color(theme.text_muted),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.scroll_side(side, step, cx);
                                }))
                        }),
                    )
            }))
            .into_any_element()
    }
    #[allow(clippy::too_many_arguments)]
    fn split_cell(
        &self,
        row: usize,
        side: Side,
        line: Option<&DiffLine>,
        key: Option<String>,
        highlights: Option<&DiffHighlights>,
        gutter: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut cell = div()
            .id(SharedString::from(format!(
                "split-cell-{}-{row}",
                side.label()
            )))
            .flex_1()
            .min_w_0()
            .h(px(DIFF_LINE_HEIGHT))
            .flex()
            .items_center()
            .overflow_hidden()
            .when(side == Side::Old, |el| {
                el.border_r_1().border_color(theme.border)
            })
            .on_scroll_wheel(
                cx.listener(move |this, event: &gpui::ScrollWheelEvent, _, cx| {
                    let (x, y) = match event.delta {
                        gpui::ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
                        gpui::ScrollDelta::Lines(p) => (p.x * 24.0, p.y * 24.0),
                    };
                    let delta = if event.modifiers.shift {
                        if x != 0.0 { x } else { y }
                    } else if x.abs() > y.abs() {
                        x
                    } else {
                        0.0
                    };
                    if delta != 0.0 {
                        this.scroll_side(side, -delta, cx);
                        cx.stop_propagation();
                    }
                }),
            );
        let Some(line) = line else {
            return cell.bg(theme.ink(0.025)).into_any_element();
        };
        if line.kind == LineKind::Meta {
            return cell
                .child(
                    div()
                        .min_w_0()
                        .px(px(8.0))
                        .truncate()
                        .text_size(px(10.0))
                        .text_color(theme.text_faint)
                        .italic()
                        .child(line.text.clone()),
                )
                .into_any_element();
        }
        let (marker, tint, number_color) = match line.kind {
            LineKind::Add => (
                "+",
                Some(
                    theme
                        .regions
                        .git_added
                        .unwrap_or(theme.diff_add.opacity(0.055)),
                ),
                theme.diff_add,
            ),
            LineKind::Del => (
                "−",
                Some(
                    theme
                        .regions
                        .git_deleted
                        .unwrap_or(theme.diff_del.opacity(0.055)),
                ),
                theme.diff_del,
            ),
            _ => (" ", None, theme.text_faint.opacity(0.8)),
        };
        cell = cell.when_some(tint, |el, color| el.bg(color));
        let number = match side {
            Side::Old => line.old_no,
            Side::New => line.new_no,
        };
        let spans = highlights
            .map(|h| h.spans_for_side(line, side))
            .unwrap_or(&[]);
        let mono = theme.mono();
        let runs = render::runs_for_syntax_line_with_plain(
            &line.text,
            spans,
            &mono,
            theme.text.opacity(0.92),
            theme,
        );
        let scope = self.split_scopes[side.index()];
        let selection = self.selection_ui_for(scope, Some(side), cx);
        let key = key.expect("non-placeholder cells have source identities");
        let offset = self.horizontal[side.index()].min(self.horizontal_limit(side));
        let width = (self.max_columns[side.index()] as f32 * self.mono_advance + 16.0).max(1.0);
        cell.child(
            div()
                .w(px(gutter))
                .flex_none()
                .pr(px(8.0))
                .flex()
                .justify_end()
                .mono(theme)
                .text_size(px(11.0))
                .text_color(theme.regions.git_line_number.unwrap_or(number_color))
                .child(number.map(|n| n.to_string()).unwrap_or_default()),
        )
        .child(
            div()
                .w(px(18.0))
                .flex_none()
                .text_size(px(DIFF_TEXT_SIZE))
                .text_color(number_color)
                .child(marker),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_hidden()
                .pl(px(8.0))
                .child(
                    div()
                        .relative()
                        .left(px(-offset))
                        .w(px(width))
                        .h(px(DIFF_LINE_HEIGHT))
                        .mono(theme)
                        .text_size(px(DIFF_TEXT_SIZE))
                        .line_height(px(DIFF_LINE_HEIGHT))
                        .whitespace_nowrap()
                        .child(diff_text_element(
                            line,
                            runs,
                            theme,
                            Some((scope, &key, &selection)),
                        )),
                ),
        )
        .into_any_element()
    }

    pub fn new(
        state: Entity<AppState>,
        comment_popup: gpui::WeakEntity<crate::comments::CommentPopup>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.sync(cx));
        let layout_observe = cx.observe_global::<layout::LayoutState>(|this: &mut Self, cx| {
            this.apply_layout(layout::current(cx), cx);
        });
        // Allocate THIS pane's selection scope before the scroll handler
        // (which captures it) — per-pane so hidden panes never touch the
        // active pane's selection/popup.
        let sel_scope = crate::markdown::selection::next_change_scope();
        let split_scopes = [
            crate::markdown::selection::next_change_scope(),
            crate::markdown::selection::next_change_scope(),
        ];
        let scopes = [sel_scope, split_scopes[0], split_scopes[1]];
        // Rows are single lines now — a deep overdraw is cheap and keeps
        // fast wheel flicks from outrunning measurement.
        let list = ListState::new(0, ListAlignment::Top, px(1024.0));
        // User input scrolled the diff: the pill would float over the wrong
        // line — dismiss this pane's offer and drop its selection wash. Safe
        // to call directly here (the popup is a different entity; the
        // selection state is process-global — neither touches `self`).
        let scroll_popup = comment_popup.clone();
        list.set_scroll_handler(move |_event: &ListScrollEvent, _window, cx| {
            for scope in scopes {
                if let Some(popup) = scroll_popup.upgrade() {
                    popup.update(cx, |popup, cx| {
                        popup.dismiss_if_owner(crate::comments::CommentOwner::Markdown(scope), cx)
                    });
                }
                crate::markdown::selection::clear(scope);
            }
        });
        Self {
            view_layout: layout::current(cx),
            layout_error: None,
            split_scopes,
            horizontal: [0.0; 2],
            max_columns: [0; 2],
            max_gutter: GUTTER_WIDTH,
            pane_width: Rc::new(std::cell::Cell::new(0.0)),
            mono_advance: DIFF_TEXT_SIZE * 0.6,
            state,
            diffs: Vec::new(),
            started: false,
            error: None,
            watch_target: None,
            watch_task: None,
            parsed: None,
            parse_task: None,
            folds: HashMap::new(),
            highlights: HashMap::new(),
            rows: Vec::new(),
            row_ranges: Vec::new(),
            fold_settle: None,
            list,
            scope: DiffScope::default(),
            base_ref: None,
            branches: Vec::new(),
            branches_for: None,
            branches_task: None,
            scoped: None,
            scoped_for: None,
            scoped_error: None,
            scoped_inflight: None,
            scoped_task: None,
            scope_menu: Popup::default(),
            ref_menu: Popup::default(),
            history: None,
            history_count: None,
            history_fetch_button: None,
            history_events: None,
            commit: None,
            owner: format!("changes-{}", cx.entity_id()),
            sel_scope,
            comment_popup,
            _observe: observe,
            _layout_observe: layout_observe,
        }
    }

    fn apply_layout(&mut self, mode: DiffLayout, cx: &mut Context<Self>) {
        if self.view_layout == mode {
            return;
        }
        let anchor = self.list.logical_scroll_top();
        let old_row = self.rows.get(anchor.item_ix).copied();
        self.view_layout = mode;
        self.layout_error = None;
        self.fold_settle = None;
        for fold in self.folds.values_mut() {
            fold.toggled_at = None;
        }
        if let Some(parsed) = &self.parsed {
            let (rows, ranges) = layout::flatten(mode, &parsed.files, |i| {
                self.folds
                    .get(&parsed.files[i].path)
                    .is_some_and(|fold| fold.collapsed)
            });
            let target = old_row
                .and_then(|r| layout::relocate(r, &rows))
                .unwrap_or(0);
            let offset = if old_row.is_some_and(|r| {
                matches!(r, DiffRow::Line { .. } | DiffRow::SplitLine { .. })
                    && matches!(
                        rows.get(target),
                        Some(DiffRow::Line { .. } | DiffRow::SplitLine { .. })
                    )
            }) {
                anchor.offset_in_item.min(px(DIFF_LINE_HEIGHT))
            } else {
                px(0.0)
            };
            self.list
                .reset_with_uniform_height(rows.len(), px(DIFF_LINE_HEIGHT));
            self.list.scroll_to(gpui::ListOffset {
                item_ix: target,
                offset_in_item: offset,
            });
            self.rows = rows;
            self.row_ranges = ranges;
        }
        self.invalidate_selection(cx);
        cx.notify();
    }

    fn layout_picker(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_none()
            .flex()
            .gap(px(2.0))
            .children(DiffLayout::ALL.map(|mode| {
                div()
                    .id(SharedString::from(format!("diff-layout-{}", mode.label())))
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.0))
                    .role(gpui::Role::Button)
                    .aria_label(match mode {
                        DiffLayout::Unified => "Unified diff",
                        DiffLayout::Split => "Side-by-side diff",
                    })
                    .cursor_pointer()
                    .when(self.view_layout == mode, |el| {
                        el.bg(theme.element_active).text_color(theme.text)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        match layout::set(mode, cx) {
                            Ok(()) => {
                                this.layout_error = None;
                                this.apply_layout(mode, cx);
                                cx.notify();
                            }
                            Err(error) => {
                                this.layout_error =
                                    Some(format!("Could not save diff layout: {error}").into());
                                cx.notify();
                            }
                        }
                    }))
                    .tooltip(move |_, cx| cx.new(|_| DiffLayoutTooltip(mode)).into())
                    .tooltip_show_delay(Duration::from_millis(300))
                    .child(
                        crate::icons::icon(match mode {
                            DiffLayout::Unified => crate::icons::DIFF_UNIFIED,
                            DiffLayout::Split => crate::icons::DIFF_SPLIT,
                        })
                        .size(px(16.0))
                        .text_color(if self.view_layout == mode {
                            theme.text
                        } else {
                            theme.text_muted
                        }),
                    )
            }))
            .into_any_element()
    }

    /// A pane pinned to one commit's diff (a History row click) — fetches
    /// `parent vs commit` once and never offers the scope menu.
    pub fn for_commit(
        state: Entity<AppState>,
        comment_popup: gpui::WeakEntity<crate::comments::CommentPopup>,
        commit: GitHistoryCommit,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut changes = Self::new(state, comment_popup, cx);
        changes.scope = DiffScope::Commit;
        changes.commit = Some(commit);
        changes
    }

    /// The surface-tab title (contextual, user request): the pinned commit's
    /// subject (short sha for subject-less commits), else the scope's label.
    pub fn tab_title(&self) -> gpui::SharedString {
        if let Some(commit) = &self.commit {
            let subject = commit.subject.trim();
            if !subject.is_empty() {
                return subject.to_string().into();
            }
            return commit.sha.chars().take(7).collect::<String>().into();
        }
        gpui::SharedString::from(self.scope.label())
    }
}

#[cfg(test)]
mod tests;
