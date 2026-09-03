//! Spaces sidebar: the project-grouped session cards (one opaque floating
//! card per Space — every host together, synthetic No-project / Unavailable-
//! project cards), the fixed Cypher / Add project header, checkout-scoped
//! hover actions for new sessions, and the add-space palette (⌘K-style:
//! device tabs + filtered folder browser).
//!
//! A space = a synced (device, folder) pair. The sidebar never filters and
//! has no target dropdown: the new-session canvas's project/device selectors
//! are the only target switcher. Space management lives on the real card
//! headers (rename/delete via the context menu) and in the add-space palette.
//! Child module of `shell` so it renders straight off `Shell`'s private state.

use super::*;
use crate::pickers::{breadcrumbs, browser_rows, completion_prefix_len, parent_path};
use cypher_proto::{Chat, ChatIndicator, Device, FolderListing, Space};
use gpui::FocusHandle;
use std::collections::HashMap;

/// The add-space palette (a command-K surface, summoned by ⌘K): search bar
/// across the top, folder browser on the left, a Devices rail on the right,
/// kbd-hint footer. One surface — picking a device in the rail rebrowses in
/// place, no step wizard.
pub(super) struct AddSpaceFlow {
    /// The device currently browsed (the highlighted rail row).
    device: Option<Device>,
    /// Filter input; Enter descends into the highlighted folder. Carries the
    /// tab-completion ghost (the faint suffix ⇥ accepts), and a trailing `/`
    /// on a folder-naming query descends immediately.
    search: Entity<ComposerInput>,
    browser: Loadable<FolderListing>,
    /// Requested browser path (`None` = the device's default, i.e. home).
    browser_path: Option<String>,
    /// The device's home (the path a `None` browse resolved to) — breadcrumbs
    /// fold everything up to here into the device-name crumb.
    home: Option<String>,
    /// Best-effort git seed for the CURRENT browser path (known when we
    /// descended through an entry whose `is_repo` we saw; the owning device's
    /// SpacesSync re-verifies either way).
    browser_repo: bool,
    /// Keyboard highlight within the FILTERED folder rows.
    active: usize,
    submit_busy: bool,
    error: Option<SharedString>,
    /// Tracked on the card (`track_focus`) — puts the card on the keyboard
    /// dispatch path so ↑↓/⌫/esc reach `add_space_key` while the search input
    /// holds focus (the structure every working picker uses).
    focus: FocusHandle,
    /// Folder-list scroll — keyboard navigation keeps the highlighted row in
    /// view (`scroll_to_item`).
    list_scroll: gpui::ScrollHandle,
    focus_pending: bool,
    load_task: Option<Task<()>>,
    submit_task: Option<Task<()>>,
    _search_events: Subscription,
}

/// The space-row Rename dialog (same shape as [`RenameChatDialog`]).
pub(super) struct RenameSpaceDialog {
    pub space_id: String,
    pub input: Entity<ComposerInput>,
    pub focus_pending: bool,
    pub _events: Subscription,
}

/// One branch/worktree group inside a project card: chats sharing one
/// checkout identity, rendered under a quiet branch/worktree header (icon +
/// truncated label) above their session rows.
struct ChatGroup {
    /// Visible label: the branch name, or the stable "Current checkout"
    /// fallback when the branch is missing/blank.
    label: String,
    /// Normalized ACTUAL branch (trimmed; blank → `None`): carried separately
    /// from `label` so new sessions can be targeted at the checkout with the
    /// real optional branch metadata (the plus button on this header).
    branch: Option<String>,
    /// Whether this checkout lives in a linked worktree (cwd off the Space
    /// root). Half of the group's deterministic identity — the other half is
    /// `label` + the exact path (below) — so a `main` branch and a `main`
    /// worktree never share a collapse key.
    worktree: bool,
    /// The exact checkout cwd for worktree groups (the representative chat's
    /// cwd) — the new-session target for this group, authoritative without
    /// ListRefs. `None` for ordinary checkouts.
    worktree_path: Option<String>,
    /// `icons::FOLDER_WITH_FILES` for linked worktrees, `icons::GIT_BRANCH`
    /// for ordinary checkouts/branches (and every synthetic card).
    icon: &'static str,
    chats: Vec<(ChatIndicator, Chat)>,
}

/// Owned snapshot of one project card for rendering. [`AppState::sidebar_groups`]
/// returns refs into the state (they borrow `cx`, which the `&self` render
/// helpers can't share), so [`Shell::render_active_rows`] materializes cards
/// here — the same clone-per-row cost the pre-grouping sidebar paid.
struct GroupCard {
    key: String,
    title: String,
    device: String,
    offline: bool,
    space_id: Option<String>,
    /// Chats folded into branch/worktree groups (`g.path` of the source
    /// group seeds the worktree detection); empty for quiet spaces.
    groups: Vec<ChatGroup>,
}

/// Fold a card's chats into branch/worktree groups in their existing order.
/// A group is keyed by the checkout label plus whether the chat lives in a
/// linked worktree; first appearance orders the groups, and each group keeps
/// the chats' overview order (status changes never re-key). The branch name
/// is the visible/key identity; a missing or blank branch falls back to the
/// stable "Current checkout" label, so branch-less chats never disappear.
/// A chat whose cwd differs from the Space root is operating in another
/// checkout/worktree (the same rule used by the composer checkout summary).
/// Synthetic cards carry no space path — nothing there reads as a worktree
/// and the GIT_BRANCH icon is used throughout.
/// One normalized identity for a worktree's checkout path: trailing `/` or
/// `\\` removed, valid leading/trailing whitespace PRESERVED. Both the chat
/// grouping identity and the disclosure key use this, so an equivalent `/wt`
/// and `/wt/` fold into one group with one stable collapse key — while the
/// exact raw cwd is kept separately as the actual checkout target.
fn normalize_worktree_path(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

fn group_chats(chats: Vec<(ChatIndicator, Chat)>, space_path: Option<&str>) -> Vec<ChatGroup> {
    fn is_worktree(cwd: Option<&str>, space_path: Option<&str>) -> bool {
        let (Some(cwd), Some(path)) = (cwd, space_path) else {
            return false;
        };
        normalize_worktree_path(cwd) != normalize_worktree_path(path)
    }

    let mut groups: Vec<ChatGroup> = Vec::new();
    // (worktree, worktree path, label) — worktrees also key on their
    // NORMALIZED checkout path so two same-label detached worktrees never
    // merge, while `/wt` and `/wt/` (one checkout) do.
    let mut index: HashMap<(bool, Option<String>, String), usize> = HashMap::new();
    for (status, chat) in chats {
        let worktree = is_worktree(chat.cwd.as_deref(), space_path);
        // Normalized actual branch (trimmed, blank → None). The visible label
        // keeps the stable "Current checkout" fallback; the actual branch is
        // carried for targeting new sessions.
        let branch = chat
            .branch
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(str::to_string);
        let label = branch
            .clone()
            .unwrap_or_else(|| "Current checkout".to_string());
        let path_key = worktree.then(|| {
            chat.cwd
                .as_deref()
                .map(normalize_worktree_path)
                .map(str::to_string)
                .unwrap_or_default()
        });
        let key = (worktree, path_key, label.clone());
        if let Some(&ix) = index.get(&key) {
            groups[ix].chats.push((status, chat));
            continue;
        }
        index.insert(key, groups.len());
        groups.push(ChatGroup {
            label,
            branch,
            worktree,
            worktree_path: worktree.then(|| chat.cwd.clone()).flatten(),
            icon: if worktree {
                icons::FOLDER_WITH_FILES
            } else {
                icons::GIT_BRANCH
            },
            chats: vec![(status, chat)],
        });
    }
    groups
}

/// Shared compact hover plus control (the project-card and branch-group
/// trailing add buttons). Its width animates from zero while the owning row is
/// hovered, so the hidden state leaves no empty slot. Both the left mouse-down
/// and the click stop propagation: the project header's collapse toggle/context
/// menu and the branch header's collapse toggle must not fire when the plus is
/// pressed. The button carries the only hover wash — the branch row itself has
/// none.
fn hover_add_plus(
    id: impl Into<SharedString>,
    hover_key: &str,
    row_gap: f32,
    theme: &Theme,
    cx: &mut Context<Shell>,
    on_click: impl Fn(&mut Shell, &gpui::ClickEvent, &mut Window, &mut Context<Shell>) + 'static,
) -> AnyElement {
    let id: SharedString = id.into();
    let hover_t = motion::hover_t(hover_key);
    div()
        .id(id)
        .flex_none()
        .w(px(18.0 * hover_t))
        .h(px(18.0))
        // The parent flex gap would remain even at width zero. Cancel that
        // gap while hidden, then release it with the same hover progress.
        .mr(px(-row_gap * (1.0 - hover_t)))
        .overflow_hidden()
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .relative()
        .left(px(2.0 * (1.0 - hover_t)))
        .opacity(hover_t)
        .cursor_pointer()
        .hover(|s| s.bg(crate::theme::wash(0.10)))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_click(cx.listener(move |this, event, window, cx| {
            cx.stop_propagation();
            on_click(this, event, window, cx);
        }))
        .child(
            icon(icons::PLUS)
                .size(px(12.0))
                .text_color(theme.text_muted.opacity(0.75)),
        )
        .into_any_element()
}

impl Shell {
    /// Land in a just-added space: select it for the new-session canvas and
    /// open the canvas. The sidebar is never filtered — every project stays
    /// visible; the canvas selectors are the only target switcher.
    pub(super) fn land_in_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        // A just-added project's canvas targets its ordinary/current checkout
        // (same as the project header's plus) — and the pin explicitly resets
        // to `CurrentCheckout { branch: None }` so no stale worktree draft
        // survives. Navigation, save, and notify all live in the helper.
        self.open_new_session_for(
            space_id,
            crate::pickers::CheckoutPlan::CurrentCheckout { branch: None },
            cx,
        );
    }

    // ---- sidebar sections ----

    /// The fixed sidebar header above the project-card list: product identity
    /// on the left and one compact Add project action on the right. New
    /// sessions are created from project/checkout hover actions (or ⌘N).
    pub(super) fn render_sidebar_header(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let add_project = div()
            .id("sidebar-add-project")
            .size(px(28.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.0))
            .cursor_pointer()
            .text_color(motion::hover_blend(
                "sidebar-add-project",
                theme.text_muted.opacity(0.8),
                theme.text,
            ))
            .bg(motion::hover_blend(
                "sidebar-add-project",
                crate::theme::wash(0.0),
                crate::theme::wash(0.14),
            ))
            .on_hover(motion::hover_listener("sidebar-add-project"))
            .on_click(cx.listener(|this, _, _, cx| this.open_add_space(cx)))
            .child(icon(icons::PLUS).size(px(14.0)).text_color(theme.text));
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            // Align the brand with the project icons inside their inset cards.
            .pl(px(18.0))
            .pr(px(Theme::SPACE_SM))
            .pt(px(8.0))
            .pb(px(4.0))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family("Oxanium")
                    .text_size(px(16.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from("Cypher")),
            )
            .child(add_project)
            .into_any_element()
    }

    /// Deterministic disclosure identity for a project card — the same
    /// `g:{key}` the FLIP resort diff keys cards by, so one key drives both
    /// the collapse state and the resort baseline. Local Shell UI state only.
    fn project_group_key(card_key: &str) -> String {
        format!("g:{card_key}")
    }

    /// Deterministic disclosure identity for a branch/worktree group, scoped
    /// under its project card. `worktree` + `label` + the worktree's NORMALIZED
    /// path is the same identity [`group_chats`] keys groups by, so a group
    /// that reappears after status churn or a reorder keeps its collapsed
    /// state — and two same-label detached worktrees stay distinct. The
    /// normalization makes an equivalent `/wt` and `/wt/` one stable key.
    fn branch_group_key(
        card_key: &str,
        worktree: bool,
        label: &str,
        worktree_path: Option<&str>,
    ) -> String {
        format!(
            "g:{card_key}/b:{worktree}:{label}:{}",
            worktree_path.map(normalize_worktree_path).unwrap_or("")
        )
    }

    /// Is this disclosure group (project card or branch/worktree group)
    /// currently collapsed?
    fn sidebar_group_collapsed(&self, key: &str) -> bool {
        self.sidebar_collapsed.contains(key)
    }

    /// Toggle a disclosure group (project card or branch/worktree group).
    /// Collapse state is local Shell UI state — never persisted or synced;
    /// everything starts expanded.
    fn toggle_sidebar_group(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.sidebar_collapsed.remove(&key) {
            self.sidebar_collapsed.insert(key);
        }
        cx.notify();
    }

    /// The sidebar's project-grouped card list: one opaque floating card per
    /// `Space` (every host together, empty spaces included) plus synthetic
    /// No-project / Unavailable-project cards. Ordering comes from
    /// [`AppState::sidebar_groups`]; cards are keyed for the FLIP resort
    /// glide (a group's height is an estimate — header + visible rows). The
    /// state's refs borrow `cx`, so the cards are snapshotted into owned form
    /// first (the same clone-per-row cost the pre-grouping sidebar paid).
    pub(super) fn render_active_rows(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<(String, f32, AnyElement)> {
        let now = Utc::now();
        let selected = self.state.read(cx).selected_chat.clone();
        let cards: Vec<GroupCard> = {
            let groups = self.state.read(cx).sidebar_groups(now);
            groups
                .into_iter()
                .map(|g| GroupCard {
                    key: g.key,
                    title: g.title,
                    device: g.device,
                    offline: g.offline,
                    space_id: g.space_id.map(str::to_string),
                    groups: group_chats(
                        g.chats
                            .into_iter()
                            .map(|(status, chat)| (status, chat.clone()))
                            .collect(),
                        g.path.as_deref(),
                    ),
                })
                .collect()
        };
        cards
            .into_iter()
            .map(|card| {
                let key = card.key.clone();
                // A card's height is an estimate for the FLIP resort glide:
                // header + one branch-group header per group + its rows — but
                // only for what's currently visible. A collapsed project is
                // header only; a collapsed branch group keeps its header and
                // drops its rows.
                let project_key = Self::project_group_key(&key);
                let height = super::GROUP_CARD_HEADER_HEIGHT
                    + if self.sidebar_group_collapsed(&project_key) {
                        0.0
                    } else {
                        card.groups.iter().fold(0.0_f32, |acc, g| {
                            let group_key = Self::branch_group_key(
                                &key,
                                g.worktree,
                                &g.label,
                                g.worktree_path.as_deref(),
                            );
                            acc + super::BRANCH_GROUP_HEADER_HEIGHT
                                + if self.sidebar_group_collapsed(&group_key) {
                                    0.0
                                } else {
                                    g.chats.len() as f32 * super::CHAT_ROW_HEIGHT
                                }
                        })
                    };
                let element = self.render_group_card(&card, &selected, now, theme, cx);
                (format!("g:{key}"), height, element)
            })
            .collect()
    }

    /// One project card: an opaque floating surface (`theme.surface`, 12px
    /// radius, subtle shadow, clipped) whose single-line header owns the
    /// prominent project label and muted target machine with a presence dot.
    /// Real space headers host the rename/remove context menu on right-click;
    /// synthetic cards have no menu. Below the header, chats are
    /// grouped by checkout: a quiet branch/worktree header introduces each
    /// group, then the compact rows (agent + title, then time) with
    /// selection and context menu behavior.
    fn render_group_card(
        &self,
        group: &GroupCard,
        selected: &Option<String>,
        now: chrono::DateTime<Utc>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project_key = Self::project_group_key(&group.key);
        let project_collapsed = self.sidebar_group_collapsed(&project_key);
        let header = self.render_group_header(group, theme, cx);
        // Rows are the visible branch/worktree group headers and their session
        // rows: a collapsed project hides every group, a collapsed branch
        // group keeps its header and drops its rows.
        let rows: Vec<AnyElement> = if project_collapsed {
            Vec::new()
        } else {
            group
                .groups
                .iter()
                .flat_map(|chat_group| {
                    let group_key = Self::branch_group_key(
                        &group.key,
                        chat_group.worktree,
                        &chat_group.label,
                        chat_group.worktree_path.as_deref(),
                    );
                    let group_collapsed = self.sidebar_group_collapsed(&group_key);
                    let mut elements: Vec<AnyElement> =
                        Vec::with_capacity(chat_group.chats.len() + 1);
                    elements.push(self.render_branch_group_header(
                        &group_key,
                        chat_group,
                        group_collapsed,
                        group.space_id.as_deref(),
                        theme,
                        cx,
                    ));
                    if !group_collapsed {
                        elements.extend(chat_group.chats.iter().map(|(status, chat)| {
                            let time_ago: SharedString = format_time_ago(
                                chat.last_message_at.unwrap_or(chat.created_at),
                                now,
                            )
                            .into();
                            let is_selected = selected.as_deref() == Some(chat.id.as_str());
                            let harness = chat.config.as_ref().map(|c| c.harness);
                            self.render_chat_row(
                                chat.id.clone(),
                                transcript::single_line(
                                    &chat.title.clone().unwrap_or_else(|| "New session".into()),
                                )
                                .into(),
                                time_ago,
                                harness,
                                *status,
                                is_selected,
                                theme,
                                cx,
                            )
                        }));
                    }
                    elements
                })
                .collect()
        };
        div()
            .rounded(px(12.0))
            .bg(theme.surface)
            .shadow_sm()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(header)
            .children(rows)
            .into_any_element()
    }

    /// One branch/worktree section label between the project and its sessions.
    /// The icon aligns with the project icon, while a trailing hairline turns
    /// the row into a clear section divider; session agent marks remain
    /// indented beneath it. The label stays secondary but readable. A compact
    /// disclosure chevron at the far right marks the whole row as a toggle —
    /// clicking hides/shows the group's sessions.
    fn render_branch_group_header(
        &self,
        key: &str,
        group: &ChatGroup,
        collapsed: bool,
        space_id: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let toggle_key = key.to_string();
        // The row's hover group: hovering anywhere on the header reveals its
        // trailing plus (nothing moves — the button is always laid out).
        let hover_key = format!("branch-add-hover-{key}");
        let worktree = group.worktree;
        let worktree_path = group.worktree_path.clone();
        let branch = group.branch.clone();
        // Quiet trailing disclosure marker (chevron-right closed,
        // chevron-down open) kept smaller than the 13px branch icon.
        let chevron = div().flex_none().size(px(10.0)).child(
            icon(if collapsed {
                icons::ALT_ARROW_RIGHT
            } else {
                icons::ALT_ARROW_DOWN
            })
            .size(px(9.0))
            .text_color(theme.text_muted.opacity(0.5)),
        );
        let mut header = div()
            .id(SharedString::from(format!("branch-hdr-{key}")))
            .h(px(33.0))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .on_hover(motion::hover_listener(hover_key.clone()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_sidebar_group(toggle_key.clone(), cx);
            }))
            // Project and group icons share the 10px card column; sessions
            // remain nested at 26px.
            .mx(px(10.0))
            // Match the project header's icon/type scale; hierarchy comes
            // from muted color, the divider, and indented session rows rather
            // than from slightly mismatched glyph and font sizes.
            .text_size(px(12.5))
            .line_height(px(14.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.text_muted.opacity(0.68))
            .child(
                div()
                    .min_w_0()
                    .max_w(px(150.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.0))
                    .child(
                        icon(group.icon)
                            .size(px(13.0))
                            .flex_none()
                            .text_color(theme.text_muted.opacity(0.72)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(group.label.clone())),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(12.0))
                    .h(px(1.0))
                    .bg(crate::theme::hairline(0.06)),
            )
            .child(chevron);
        // Real-space groups get the trailing add plus at the far tail (after
        // the disclosure chevron), opening a canvas targeted at THIS checkout
        // — the worktree's exact path (authoritative without ListRefs) or the
        // project-root checkout with the real optional branch metadata.
        if let Some(space_id) = space_id {
            let space_id = space_id.to_string();
            header = header.child(hover_add_plus(
                format!("branch-add-{key}"),
                &hover_key,
                6.0,
                theme,
                cx,
                move |this, _, _, cx| {
                    let plan = if worktree {
                        crate::pickers::CheckoutPlan::ReuseWorktree {
                            path: worktree_path.clone().unwrap_or_default(),
                            branch: branch.clone(),
                        }
                    } else {
                        crate::pickers::CheckoutPlan::CurrentCheckout {
                            branch: branch.clone(),
                        }
                    };
                    this.open_new_session_for(space_id.clone(), plan, cx);
                },
            ));
        }
        header.into_any_element()
    }

    /// A project card's single-line header: folder icon + prominent project
    /// name on the left, then the quiet right-aligned target-machine name and
    /// a far-right presence dot (emerald online, faint offline). The header
    /// toggles the whole card body (all branch/worktree groups + sessions) on
    /// left-press; real-space headers open the rename/remove context menu on
    /// right-click, while synthetic cards render no menu.
    fn render_group_header(
        &self,
        group: &GroupCard,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu_space = group.space_id.clone();
        // The header's hover group: hovering anywhere on the row reveals its
        // trailing plus (the button is always laid out — nothing moves).
        let hover_key = format!("space-add-hover-{}", group.key);
        let toggle_key = Self::project_group_key(&group.key);
        let title = group.title.clone();
        let device: SharedString = group.device.clone().into();
        let offline = group.offline;
        let presence = div()
            .size(px(6.0))
            .flex_none()
            .rounded_full()
            .when(!offline, |el| {
                let emerald = theme.success;
                el.bg(emerald.opacity(0.9)).shadow(vec![gpui::BoxShadow {
                    color: emerald.opacity(0.45),
                    offset: gpui::point(px(0.0), px(0.0)),
                    blur_radius: px(4.0),
                    spread_radius: px(0.0),
                    inset: false,
                }])
            })
            .when(offline, |el| el.bg(crate::theme::ink(0.22)));
        let mut header = div()
            .id(SharedString::from(format!(
                "space-card-{}-header",
                group.key
            )))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(7.0))
            .px(px(10.0))
            .pt(px(8.0))
            .pb(px(6.0))
            // The whole header toggles the card body on left-press.
            // Right-click stays the project menu; see `menu_space` below.
            .cursor_pointer()
            .on_hover(motion::hover_listener(hover_key.clone()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.toggle_sidebar_group(toggle_key.clone(), cx);
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        icon(icons::FOLDER)
                            .size(px(13.0))
                            .flex_none()
                            .text_color(theme.text),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(title)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .min_w_0()
                    .max_w(px(96.0))
                    .truncate()
                    .text_right()
                    .text_size(px(10.5))
                    .text_color(theme.text_muted.opacity(0.62))
                    .child(device),
            )
            .child(presence);
        if let Some(space_id) = menu_space {
            // Real-space headers also get the trailing add plus (after the
            // presence dot): a canvas explicitly targeted at the project's
            // ordinary/current checkout — pinned `CurrentCheckout { branch:
            // None }` so no stale worktree draft survives. Synthetic cards
            // get neither the plus nor the menu.
            let plus_space = space_id.clone();
            header = header.child(hover_add_plus(
                format!("space-add-{}", group.key),
                &hover_key,
                7.0,
                theme,
                cx,
                move |this, _, _, cx| {
                    this.open_new_session_for(
                        plus_space.clone(),
                        crate::pickers::CheckoutPlan::CurrentCheckout { branch: None },
                        cx,
                    );
                },
            ));
            // Right-click anywhere on the header opens the project menu.
            let menu_id = space_id;
            header = header.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.space_menu.open((menu_id.clone(), event.position));
                    cx.notify();
                }),
            );
        }
        header.into_any_element()
    }

    // ---- add-space flow (the ⌘K palette) ----

    pub(super) fn open_add_space(&mut self, cx: &mut Context<Self>) {
        let devices: Vec<Device> = self.state.read(cx).devices.clone();
        let local = self.state.read(cx).local_device_id.clone();
        // Land on this device's tab (else the first registered device).
        let device = devices
            .iter()
            .find(|d| local.as_deref() == Some(d.id.as_str()))
            .or_else(|| devices.first())
            .cloned();
        // "PaletteSearch" context: navigation keys stay unbound so ↑↓/←/→/⏎
        // bubble to the palette frame (`add_space_key`) instead of moving the
        // text caret — Enter and ⌘Enter are both handled there.
        let search =
            cx.new(|cx| ComposerInput::with_context("Search folders…", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                // Typing `/` after a query that names a folder descends into
                // it — the query reads as a path segment, so the slash IS the
                // pick (shell-style). Otherwise the slash stays in the query
                // (it matches nothing, which is honest feedback).
                if this.add_space_slash_descend(cx) {
                    return;
                }
                if let Some(flow) = this.add_space.as_mut() {
                    flow.active = 0;
                }
                cx.notify();
            }
        });
        let has_device = device.is_some();
        self.add_space = Some(AddSpaceFlow {
            device,
            search,
            browser: Loadable::Idle,
            browser_path: None,
            home: None,
            browser_repo: false,
            active: 0,
            submit_busy: false,
            error: None,
            focus: cx.focus_handle(),
            list_scroll: gpui::ScrollHandle::new(),
            focus_pending: true,
            load_task: None,
            submit_task: None,
            _search_events: search_events,
        });
        if has_device {
            self.load_space_folders(None, cx);
        }
        cx.notify();
    }

    /// Devices-rail click: rebrowse the same palette on another device.
    fn add_space_pick_device(&mut self, device: Device, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        if flow.device.as_ref().is_some_and(|d| d.id == device.id) {
            return;
        }
        flow.device = Some(device);
        flow.browser = Loadable::Idle;
        flow.browser_path = None;
        flow.home = None;
        flow.browser_repo = false;
        flow.active = 0;
        flow.error = None;
        let search = flow.search.clone();
        search.update(cx, |input, cx| input.set_text("", cx));
        self.load_space_folders(None, cx);
        cx.notify();
    }

    /// The current listing's folder rows filtered by the search query
    /// (prefix matches first — `popover::filter_indices`).
    fn add_space_filtered(&self, cx: &App) -> Vec<cypher_proto::FolderEntry> {
        let Some(flow) = self.add_space.as_ref() else {
            return Vec::new();
        };
        let Some(listing) = flow.browser.ready() else {
            return Vec::new();
        };
        let dirs = browser_rows(listing);
        let query = flow.search.read(cx).text().to_string();
        let names: Vec<&str> = dirs.iter().map(|e| e.name.as_str()).collect();
        popover::filter_indices(&query, &names)
            .into_iter()
            .map(|ix| dirs[ix].clone())
            .collect()
    }

    /// Descend into the highlighted (filtered) folder; clears the query.
    fn add_space_open_active(&mut self, cx: &mut Context<Self>) {
        let rows = self.add_space_filtered(cx);
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        let Some(listing) = flow.browser.ready() else {
            return;
        };
        let Some(entry) = rows.get(flow.active) else {
            return;
        };
        let full = crate::pickers::child_path(&listing.path, &entry.name);
        let is_repo = entry.is_repo;
        let search = flow.search.clone();
        if let Some(flow) = self.add_space.as_mut() {
            flow.browser_repo = is_repo;
        }
        search.update(cx, |input, cx| input.set_text("", cx));
        self.load_space_folders(Some(full), cx);
    }

    /// Slash-descend: when the query ends in `/` and the part before it names
    /// a folder of the current listing (exact name — matching casing wins
    /// over a case-colliding sibling — else a unique prefix), descend into it
    /// as though it were picked. Returns whether it fired —
    /// descending clears the query, so the caller must not keep acting on the
    /// old text.
    fn add_space_slash_descend(&mut self, cx: &mut Context<Self>) -> bool {
        let target = {
            let Some(flow) = self.add_space.as_ref() else {
                return false;
            };
            let text = flow.search.read(cx).text().to_string();
            let Some(query) = text.strip_suffix('/') else {
                return false;
            };
            if query.is_empty() || query.contains('/') {
                return false;
            }
            let Some(listing) = flow.browser.ready() else {
                return false;
            };
            let dirs = browser_rows(listing);
            let names: Vec<&str> = dirs.iter().map(|e| e.name.as_str()).collect();
            crate::pickers::segment_target(&names, query).map(|ix| {
                (
                    crate::pickers::child_path(&listing.path, &dirs[ix].name),
                    dirs[ix].is_repo,
                )
            })
        };
        let Some((full, is_repo)) = target else {
            return false;
        };
        self.add_space_descend(full, is_repo, cx);
        true
    }

    /// The tab-completion target: the highlighted row when the query prefixes
    /// its name, else the first prefix match (filtering ranks those first).
    /// `(full name, remaining suffix)`; `None` on an empty query or when the
    /// match is already complete.
    fn add_space_completion(&self, cx: &App) -> Option<(String, String)> {
        let flow = self.add_space.as_ref()?;
        let query = flow.search.read(cx).text().to_string();
        if query.is_empty() {
            return None;
        }
        let rows = self.add_space_filtered(cx);
        let entry = rows
            .get(flow.active)
            .filter(|e| completion_prefix_len(&e.name, &query).is_some())
            .or_else(|| {
                rows.iter()
                    .find(|e| completion_prefix_len(&e.name, &query).is_some())
            })?;
        let len = completion_prefix_len(&entry.name, &query)?;
        if len >= entry.name.len() {
            return None;
        }
        Some((entry.name.clone(), entry.name[len..].to_string()))
    }

    /// ⇥: accept the completion — the query becomes the full folder name
    /// (the ghost the input was previewing). Descending stays on `/`/⏎.
    fn add_space_accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some((name, _)) = self.add_space_completion(cx) else {
            return;
        };
        if let Some(flow) = self.add_space.as_ref() {
            let search = flow.search.clone();
            search.update(cx, |input, cx| input.set_text(name, cx));
        }
    }

    /// Descend into a specific folder row (mouse path); clears the query.
    fn add_space_descend(&mut self, full: String, is_repo: bool, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.browser_repo = is_repo;
        let search = flow.search.clone();
        search.update(cx, |input, cx| input.set_text("", cx));
        self.load_space_folders(Some(full), cx);
    }

    /// ListFolders on the flow's device (relay-forwarded when remote).
    pub(super) fn load_space_folders(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        let device_id = flow.device.as_ref().map(|d| d.id.clone());
        let went_home = path.is_none();
        flow.browser_path = path.clone();
        flow.browser = Loadable::Loading;
        flow.active = 0;
        flow.list_scroll.set_offset(gpui::Point::default());
        flow.load_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            if let Some(p) = &path {
                params.insert("path".into(), serde_json::Value::String(p.clone()));
            }
            // Only target remote devices — local calls skip the relay.
            if let (Some(target), local) = (&device_id, &local)
                && local.as_deref() != Some(target.as_str())
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target.clone()),
                );
            }
            let result = engine
                .client()
                .call(methods::LIST_FOLDERS, serde_json::Value::Object(params))
                .await;
            this.update(cx, |shell, cx| {
                if let Some(flow) = shell.add_space.as_mut() {
                    flow.browser = match result {
                        Ok(value) => match serde_json::from_value::<FolderListing>(value) {
                            Ok(listing) => {
                                // A pathless browse resolved home — remember it
                                // so the breadcrumbs can fold it into the
                                // device crumb.
                                if went_home {
                                    flow.home = Some(listing.path.clone());
                                }
                                Loadable::Ready(listing)
                            }
                            Err(err) => Loadable::Error(err.to_string()),
                        },
                        Err(err) => Loadable::Error(err.to_string()),
                    };
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Create the space for the browser's current folder.
    fn submit_add_space(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        if flow.submit_busy {
            return;
        }
        let Some(device) = flow.device.clone() else {
            return;
        };
        let Some(listing) = flow.browser.ready() else {
            return;
        };
        let path = listing.path.clone();
        let git_detected = flow.browser_repo;
        // Same (device, folder) already has a space → just switch to it. The
        // engine dedupes this case too (a createSpace for a duplicate pair
        // no-ops), so creating would leave the minted id dangling.
        if let Some(existing) = self
            .state
            .read(cx)
            .spaces
            .iter()
            .find(|s| s.device_id == device.id && s.path == path)
            .map(|s| s.id.clone())
        {
            self.add_space = None;
            self.land_in_space(existing, cx);
            return;
        }
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.submit_busy = true;
        flow.error = None;
        let space_id = uuid::Uuid::new_v4().to_string();
        // Optimistic echo: the watch frame carrying the real row replaces it
        // by id (apply_spaces re-sorts; same-id upsert is idempotent).
        let space = Space {
            id: space_id.clone(),
            device_id: device.id.clone(),
            path: path.clone(),
            name: None,
            git_detected,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        };
        self.state.update(cx, |s, cx| {
            if !s.spaces.iter().any(|existing| existing.id == space.id) {
                s.spaces.push(space);
            }
            cx.notify();
        });
        let params = serde_json::json!({
            "op": "createSpace",
            "spaceId": space_id,
            "deviceId": device.id,
            "path": path,
            "gitDetected": git_detected,
        });
        let submit_id = space_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::MUTATE, params).await;
            this.update(cx, |shell, cx| {
                match result {
                    Ok(_) => {
                        shell.add_space = None;
                        shell.land_in_space(submit_id.clone(), cx);
                    }
                    Err(err) => {
                        // Roll the optimistic row back; surface the error inline.
                        shell.state.update(cx, |s, cx| {
                            s.spaces.retain(|space| space.id != submit_id);
                            cx.notify();
                        });
                        if let Some(flow) = shell.add_space.as_mut() {
                            flow.submit_busy = false;
                            flow.error = Some(format!("{err}").into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        });
        if let Some(flow) = self.add_space.as_mut() {
            flow.submit_task = Some(task);
        }
        cx.notify();
    }

    /// Go up to the parent folder (←, and ⌫ on an empty query).
    fn add_space_go_up(&mut self, cx: &mut Context<Self>) {
        let parent = self
            .add_space
            .as_ref()
            .and_then(|f| f.browser.ready())
            .and_then(|l| parent_path(&l.path));
        if let Some(parent) = parent {
            if let Some(flow) = self.add_space.as_mut() {
                flow.browser_repo = false; // unknown at the parent
            }
            self.load_space_folders(Some(parent), cx);
        }
    }

    /// Palette keys (bubbling from the focused search input) — every legend
    /// maps to a REAL key: ↑↓ (or ctrl-n/p) navigate, →/⏎ open the
    /// highlighted folder, ← up a level, ⇥ completes the query to the
    /// previewed folder name, ⌘⏎ add the OPEN folder, ⌫ (empty query) also
    /// goes up, esc closes. (Typing `/` also descends — see the Edited
    /// subscription.)
    fn add_space_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        // ←/→ act on the FOLDERS, not the text cursor — the palette is a
        // navigator first; queries are short and edited with ⌫.
        match event.keystroke.key.as_str() {
            "right" => {
                self.add_space_open_active(cx);
                return;
            }
            "left" => {
                self.add_space_go_up(cx);
                return;
            }
            // Unbound in "PaletteSearch" (like enter), so it bubbles here
            // instead of editing text or moving focus.
            "tab" => {
                self.add_space_accept_completion(cx);
                return;
            }
            _ => {}
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.add_space = None;
                cx.notify();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = self.add_space_filtered(cx).len();
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(flow) = self.add_space.as_mut() {
                    flow.active = popover::menu_step(Some(flow.active), count, delta).unwrap_or(0);
                    // Keep the highlighted row in view as the cursor walks
                    // past the viewport (user-reported: the list didn't
                    // follow the keyboard).
                    flow.list_scroll.scroll_to_item(flow.active);
                    cx.notify();
                }
            }
            // ⏎ opens the highlighted folder (an alias for →); the space is
            // added with ⌘⏎ — and the chord acts on the folder OPEN in the
            // breadcrumbs, not the highlight. The highlight auto-rests on the
            // first row, so a chord that took it would add arbitrary
            // subfolders; the usual target (a repo root full of subfolders)
            // is only ever "the folder you're standing in".
            popover::MenuKey::Enter => self.add_space_open_active(cx),
            popover::MenuKey::ModEnter => self.submit_add_space(cx),
            popover::MenuKey::Backspace => {
                let empty = self
                    .add_space
                    .as_ref()
                    .is_some_and(|f| f.search.read(cx).is_empty());
                if empty {
                    self.add_space_go_up(cx);
                }
            }
            popover::MenuKey::Other => {}
        }
    }

    /// The palette card: ⌘K search bar (with the ⌘⏎ add / esc chips) ·
    /// breadcrumbs + folder list beside the devices rail · kbd-hint footer.
    pub(super) fn render_add_space_overlay(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        {
            let flow = self.add_space.as_mut()?;
            if std::mem::take(&mut flow.focus_pending) {
                let handle = flow.search.focus_handle(cx);
                window.focus(&handle, cx);
            }
        }
        let (
            device,
            search,
            error,
            submit_busy,
            active,
            loading,
            load_error,
            listing,
            focus,
            list_scroll,
            home,
        ) = {
            let flow = self.add_space.as_ref()?;
            (
                flow.device.clone(),
                flow.search.clone(),
                flow.error.clone(),
                flow.submit_busy,
                flow.active,
                matches!(flow.browser, Loadable::Loading | Loadable::Idle),
                flow.browser.error().map(str::to_string),
                flow.browser.ready().cloned(),
                flow.focus.clone(),
                flow.list_scroll.clone(),
                flow.home.clone(),
            )
        };
        let devices = self.state.read(cx).devices.clone();
        let rows = self.add_space_filtered(cx);
        // Push the completion preview into the input — the faint suffix ahead
        // of the caret that ⇥ accepts. Recomputed every render (query, active
        // row, and listing all move it); `set_ghost` no-ops when unchanged.
        let ghost = self
            .add_space_completion(cx)
            .map(|(_, suffix)| SharedString::from(suffix));
        search.update(cx, |input, cx| input.set_ghost(ghost, cx));
        let query_empty = search.read(cx).is_empty();
        let hairline = crate::theme::hairline(0.06);
        let now = Utc::now();
        // (browsed device name, online) per rail row — presence is the same
        // signal the sidebar space rows use.
        let device_presence: Vec<bool> = {
            let state = self.state.read(cx);
            devices
                .iter()
                .map(|d| state.device_online(&d.id, now))
                .collect()
        };
        let device_name: SharedString = device
            .as_ref()
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "This device".to_string())
            .into();

        // A quiet mono key-cap chip ("⌘K" / "esc") for the search bar ends.
        let key_chip = |theme: &Theme| {
            div()
                .h(px(22.0))
                .px(px(6.0))
                .rounded(px(5.0))
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .bg(crate::theme::ink(0.05))
                .text_size(px(11.0))
                .font_family(theme.font_mono.clone())
                .text_color(theme.text_muted.opacity(0.7))
        };

        // ── search bar (the ⌘K bar): summon chip · input · "⌘ Enter" add ·
        //    esc. The primary chip leads with the ⌘ glyph, then says "Enter"
        //    in words (user request — the bare return arrow read as noise).
        let submit_chip = popover::btn_primary(&theme, "")
            .id("add-space-submit")
            .h(px(22.0))
            .px(px(8.0))
            .py(px(0.0))
            // Match the key-cap chips beside it (rounded-5) — btn_primary's
            // rounded-8 at this size read as a different component.
            .rounded(px(5.0))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .text_size(px(12.0))
            .when(submit_busy || listing.is_none(), |el| el.opacity(0.6))
            .on_click(cx.listener(|this, _, _, cx| this.submit_add_space(cx)))
            .when(!submit_busy, |el| {
                el.child(
                    icon(icons::COMMAND)
                        .size(px(11.0))
                        .text_color(theme.on_solid.opacity(0.8)),
                )
                .child(SharedString::from("Enter"))
            })
            .when(submit_busy, |el| el.child(SharedString::from("Adding…")));
        // Header and footer sit a shade DEEPER than the body (the shared
        // recessed-band tone) — the bands frame the folder list, which stays
        // on the brighter tint.
        let band = popover::band();
        let input_row = div()
            .h(px(46.0))
            .flex_none()
            .pl(px(12.0))
            .pr(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .bg(band)
            .border_b_1()
            .border_color(hairline)
            .child(
                key_chip(&theme)
                    .child(
                        icon(icons::COMMAND)
                            .size(px(11.0))
                            .text_color(theme.text_muted.opacity(0.7)),
                    )
                    .child(SharedString::from("K")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(14.0))
                    .child(search.clone().into_any_element()),
            )
            .child(submit_chip)
            .child(
                key_chip(&theme)
                    .id("add-space-esc")
                    .cursor_pointer()
                    .hover(|s| s.bg(crate::theme::ink(0.09)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.add_space = None;
                        cx.notify();
                    }))
                    .child(SharedString::from("esc")),
            );

        // ── breadcrumbs ("MacBook Pro / Projects / cypher"): the quiet mono
        //    path voice, `/` separators. The device crumb stands in for home —
        //    everything up to the resolved home path folds into it; below
        //    home the full path shows. Ancestors (device crumb included) are
        //    clickable.
        let crumbs: AnyElement = match &listing {
            Some(listing) => {
                let segments = breadcrumbs(&listing.path);
                let last = segments.len().saturating_sub(1);
                // Root "/" chip always folds; the home segments fold too when
                // the browsed path sits at/under home.
                let at_home = home.as_deref() == Some(listing.path.as_str());
                let folded = 1 + home
                    .as_deref()
                    .filter(|h| listing.path == *h || listing.path.starts_with(&format!("{h}/")))
                    .map(|h| h.split('/').filter(|s| !s.is_empty()).count())
                    .unwrap_or(0);
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .px(px(13.0))
                    .pt(px(10.0))
                    .pb(px(2.0))
                    .text_size(px(11.0))
                    .font_family(theme.font_mono.clone())
                    .child({
                        let crumb = div()
                            .id("add-space-crumb-device")
                            .px(px(3.0))
                            .rounded(px(4.0))
                            .child(device_name.clone());
                        if at_home {
                            // Standing at home — the device crumb IS the
                            // current folder.
                            crumb
                                .text_color(theme.text.opacity(0.85))
                                .into_any_element()
                        } else {
                            crumb
                                .text_color(theme.text_muted.opacity(0.55))
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme.text))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(flow) = this.add_space.as_mut() {
                                        flow.browser_repo = false;
                                    }
                                    this.load_space_folders(None, cx);
                                }))
                                .into_any_element()
                        }
                    })
                    .children(segments.into_iter().enumerate().skip(folded).map(
                        |(ix, (label, full))| {
                            let is_last = ix == last;
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .child(
                                    div()
                                        .text_color(theme.text_faint.opacity(0.7))
                                        .child(SharedString::from("/")),
                                )
                                .child({
                                    let crumb = div()
                                        .id(("add-space-crumb", ix))
                                        .px(px(3.0))
                                        .rounded(px(4.0))
                                        .text_color(if is_last {
                                            theme.text.opacity(0.85)
                                        } else {
                                            theme.text_muted.opacity(0.55)
                                        })
                                        .child(SharedString::from(label));
                                    if is_last {
                                        crumb.into_any_element()
                                    } else {
                                        crumb
                                            .cursor_pointer()
                                            .hover(|s| s.text_color(theme.text))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(flow) = this.add_space.as_mut() {
                                                    flow.browser_repo = false;
                                                }
                                                this.load_space_folders(Some(full.clone()), cx);
                                            }))
                                            .into_any_element()
                                    }
                                })
                        },
                    ))
                    .into_any_element()
            }
            None => div().pt(px(6.0)).into_any_element(),
        };

        // ── folder list ─────────────────────────────────────────────────────
        let base_path = listing.as_ref().map(|l| l.path.clone()).unwrap_or_default();
        let list: AnyElement = if loading {
            div()
                .px(px(8.0))
                .py(px(6.0))
                .child(popover::skeleton_rows(
                    "add-space-skeleton",
                    &theme,
                    6,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element()
        } else if let Some(message) = load_error {
            let device_line = device
                .as_ref()
                .map(|d| format!("{} didn't respond — is it online?", d.name))
                .unwrap_or(message);
            popover::error_row(&theme, &device_line)
                .px(px(14.0))
                .py(px(10.0))
                .child(
                    div()
                        .id("add-space-retry")
                        .px(px(Theme::SPACE_SM))
                        .py(px(3.0))
                        .rounded(px(Theme::CONTROL_RADIUS))
                        .border_1()
                        .border_color(theme.border)
                        .text_color(theme.text)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.element_hover))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let path = this.add_space.as_ref().and_then(|f| f.browser_path.clone());
                            this.load_space_folders(path, cx);
                        }))
                        .child(SharedString::from("Retry")),
                )
                .into_any_element()
        } else if rows.is_empty() {
            div()
                .px(px(14.0))
                .py(px(16.0))
                .text_size(px(12.5))
                .text_color(theme.text_faint)
                .child(SharedString::from(if query_empty {
                    "No folders here"
                } else {
                    "No folders match"
                }))
                .into_any_element()
        } else {
            // The 6px gutters live on a WRAPPER, outside the scroll viewport:
            // in-content padding/spacers can't do it — the wheel's max offset
            // eats bottom padding, and `scroll_to_item` (keyboard) pins the
            // row's bottom to the viewport edge regardless.
            div()
                .flex_1()
                .min_h_0()
                .py(px(6.0))
                .child(
                    div()
                        .id("add-space-folders")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&list_scroll)
                        .px(px(8.0))
                        .flex()
                        .flex_col()
                        // The app-wide list rhythm (sidebar rows, menu rows): 2px.
                        .gap(px(2.0))
                        .children(rows.into_iter().enumerate().map(|(ix, entry)| {
                            let name: SharedString = entry.name.clone().into();
                            let full = crate::pickers::child_path(&base_path, &entry.name);
                            let is_repo = entry.is_repo;
                            popover::menu_row_nav(
                                &theme,
                                false,
                                ix == active,
                                format!("add-space-folder-{ix}"),
                            )
                            // The floating-card selection language: the wash
                            // plus the ring-only inset outline.
                            .when(ix == active, |el| {
                                el.shadow(crate::theme::card_selected_shadows())
                            })
                            .id(("add-space-folder", ix))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_space_descend(full.clone(), is_repo, cx);
                            }))
                            .child(
                                icon(icons::FOLDER)
                                    .size(px(15.0))
                                    .flex_none()
                                    .text_color(theme.text_muted.opacity(0.8)),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(name))
                            // Repos get a quiet trailing branch glyph — the row
                            // you're usually hunting for announces itself.
                            .when(is_repo, |el| {
                                el.child(
                                    icon(icons::GIT_BRANCH)
                                        .size(px(13.0))
                                        .flex_none()
                                        .text_color(theme.text_muted.opacity(0.5)),
                                )
                            })
                        })),
                )
                .into_any_element()
        };

        // ── devices rail (mock right column): platform glyph + name +
        //    presence dot per row, an info line naming the browsed device.
        //    Rows are the tab recipe (h-28 rounded-8 washes), vertical.
        let rail = div()
            .w(px(196.0))
            .flex_none()
            .border_l_1()
            .border_color(hairline)
            .px(px(8.0))
            .py(px(8.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .px(px(8.0))
                    .pt(px(2.0))
                    .pb(px(4.0))
                    .text_size(px(11.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_muted.opacity(0.6))
                    .child(SharedString::from("Devices")),
            )
            .children(devices.into_iter().enumerate().map(|(ix, dev)| {
                let is_active = device.as_ref().is_some_and(|d| d.id == dev.id);
                let online = device_presence.get(ix).copied().unwrap_or(false);
                // The Devices-page platform mapping (settings::devices).
                let platform_icon = match dev.platform.as_str() {
                    "macos" | "darwin" => icons::LAPTOP,
                    "web" => icons::GLOBAL,
                    "ios" | "android" => icons::SMARTPHONE,
                    _ => icons::MONITOR,
                };
                let name: SharedString = dev.name.clone().into();
                let pick = dev.clone();
                div()
                    .id(("add-space-device", ix))
                    .h(px(28.0))
                    .px(px(8.0))
                    .rounded(px(8.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(12.5))
                    .cursor_pointer()
                    .when(is_active, |el| {
                        // The floating-card selection language: wash +
                        // ring-only inset outline.
                        el.bg(crate::theme::card_selected_bg())
                            .shadow(crate::theme::card_selected_shadows())
                            .text_color(theme.text)
                    })
                    .when(!is_active, |el| {
                        el.text_color(theme.text_muted.opacity(0.7))
                            .hover(|s| s.bg(theme.element_hover))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.add_space_pick_device(pick.clone(), cx);
                    }))
                    .child(
                        icon(platform_icon)
                            .size(px(14.0))
                            .flex_none()
                            .text_color(theme.text_muted.opacity(0.8)),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(
                        div()
                            .size(px(5.0))
                            .rounded_full()
                            .flex_none()
                            .when(online, |el| {
                                // The Devices-page presence emerald, soft glow
                                // included.
                                let emerald = theme.success;
                                el.bg(emerald.opacity(0.9)).shadow(vec![gpui::BoxShadow {
                                    color: emerald.opacity(0.55),
                                    offset: gpui::point(px(0.0), px(0.0)),
                                    blur_radius: px(6.0),
                                    spread_radius: px(0.0),
                                    inset: false,
                                }])
                            })
                            .when(!online, |el| el.bg(crate::theme::ink(0.22))),
                    )
            }))
            .child(div().h(px(1.0)).mx(px(2.0)).my(px(6.0)).bg(hairline))
            .child(
                div()
                    .px(px(8.0))
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .line_height(px(15.0))
                    .text_color(theme.text_muted.opacity(0.5))
                    .child(
                        icon(icons::INFO_CIRCLE)
                            .size(px(12.0))
                            .flex_none()
                            .mt(px(1.0))
                            .text_color(theme.text_muted.opacity(0.5)),
                    )
                    .child(div().min_w_0().child(SharedString::from(format!(
                        "Showing folders from {device_name} only"
                    )))),
            );

        // ── body: folder column (crumbs + list) beside the devices rail.
        //    FIXED height — sparse folders, loading skeletons, and device
        //    switches must not resize the card (the list fills and scrolls).
        let body = div()
            .h(px(330.0))
            .flex()
            .flex_row()
            .items_stretch()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(crumbs)
                    .child(list),
            )
            .child(rail);

        // ── footer: the shared key-cap legend voice (popover::key_hint).
        let footer = div()
            .flex_none()
            .bg(band)
            .border_t_1()
            .border_color(hairline)
            .px(px(12.0))
            .py(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .child(popover::key_hint_pair(
                &theme,
                icons::ARROW_UP,
                icons::ARROW_DOWN,
                "Navigate",
            ))
            .child(popover::key_hint(&theme, icons::ARROW_LEFT, "Up"))
            .child(popover::key_hint(&theme, icons::ARROW_RIGHT, "Open"))
            .child(popover::key_hint_text(&theme, "tab", "Complete"))
            .when_some(error, |el, message| {
                el.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.0))
                        .text_color(theme.danger)
                        .child(message),
                )
            });

        let card =
            div()
                .id("add-space-palette")
                .w(px(680.0))
                .rounded(px(14.0))
                .border_1()
                .border_color(crate::theme::hairline(0.10))
                // The popover_card glass recipe: a translucent tint over the
                // frosted backdrop blur (`popover::modal` wraps in `frosted`) —
                // an opaque fill here killed the vibrancy every other float has.
                .bg(if theme.is_glass() {
                    theme.glass_overlay()
                } else {
                    theme.surface_overlay
                })
                .shadow_lg()
                .overflow_hidden()
                .flex()
                .flex_col()
                .text_color(theme.text)
                // On the keyboard dispatch path (see `AddSpaceFlow::focus`) — the
                // pickers' proven structure for frame-level keys with a focused
                // child input.
                .track_focus(&focus)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    this.add_space_key(event, cx)
                }))
                // Clicking the scrim dismisses (user requirement) — same close
                // path as Escape.
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.add_space = None;
                    cx.notify();
                }))
                .child(input_row)
                .child(body)
                .child(footer)
                .into_any_element();
        // The glass-modal variant: lighter scrim + a frost radius matching
        // this card's 14px rounding, so the palette reads like the popovers
        // instead of a flat slab over a 60% dim (user request).
        Some(popover::modal_glass(
            "add-space-dialog",
            viewport,
            card,
            14.0,
        ))
    }

    // ---- space context menu / rename / delete overlays ----

    fn close_space_menu(&mut self, cx: &mut Context<Self>) {
        if self.space_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.space_menu);
            cx.notify();
        }
    }

    pub(super) fn open_rename_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.close_space_menu(cx);
        let current = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|s| s.display_name().to_string())
            .unwrap_or_default();
        let input = cx.new(|cx| ComposerInput::new("Project name", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_rename_space(cx);
            }
        });
        self.rename_space_dialog = Some(RenameSpaceDialog {
            space_id,
            input,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    pub(super) fn submit_rename_space(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_space_dialog.take() else {
            return;
        };
        let name = dialog.input.read(cx).text().trim().to_string();
        if !name.is_empty() {
            self.mutate(
                serde_json::json!({ "op": "renameSpace", "spaceId": dialog.space_id, "name": name }),
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn delete_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.delete_space_confirm = None;
        self.mutate(
            serde_json::json!({ "op": "deleteSpace", "spaceId": space_id }),
            cx,
        );
        cx.notify();
    }

    /// Space context menu + rename dialog + delete confirm (appended to the
    /// shell's overlay list).
    pub(super) fn render_space_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let mut overlays: Vec<AnyElement> = Vec::new();

        if let Some((space_id, position)) = self.space_menu.get().cloned() {
            let closing = self.space_menu.closing_since();
            let rename_id = space_id.clone();
            let delete_id = space_id.clone();
            let menu = popover::popover_card(&theme)
                .w(px(170.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_space_menu(cx);
                }))
                .flex()
                .flex_col()
                .child(
                    popover::menu_row(&theme, false, format!("space-menu-rename-{space_id}"))
                        .id("space-menu-rename")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_rename_space(rename_id.clone(), cx)
                        }))
                        .child(icon(icons::PEN).size(px(16.0)).text_color(theme.text_muted))
                        .child(SharedString::from("Rename…")),
                )
                .child(popover::menu_separator())
                .child(
                    popover::menu_row(&theme, false, format!("space-menu-delete-{space_id}"))
                        .id("space-menu-delete")
                        .text_color(theme.danger)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_space_menu(cx);
                            this.delete_space_confirm = Some(delete_id.clone());
                            cx.notify();
                        }))
                        .child(
                            icon(icons::TRASH_BIN_MINIMALISTIC)
                                .size(px(16.0))
                                .text_color(theme.danger),
                        )
                        .child(SharedString::from("Remove…")),
                )
                .into_any_element();
            overlays.push(popover::menu_at(
                "space-context-menu",
                position,
                menu,
                closing,
            ));
        }

        if let Some(dialog) = &mut self.rename_space_dialog {
            if std::mem::take(&mut dialog.focus_pending) {
                window.focus(&dialog.input.focus_handle(cx), cx);
            }
            let input = dialog.input.clone();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.rename_space_dialog = None;
                        cx.notify();
                    }
                }))
                .child(popover::dialog_title(&theme, "Rename project"))
                .child(
                    div()
                        .mt(px(12.0))
                        .child(popover::dialog_field(input.into_any_element())),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "rename-space-cancel")
                                .id("rename-space-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.rename_space_dialog = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Rename")
                                .id("rename-space-save")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.submit_rename_space(cx)),
                                ),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("rename-space-dialog", viewport, card));
        }

        if let Some(space_id) = self.delete_space_confirm.clone() {
            let (name, device, count) = {
                let state = self.state.read(cx);
                let space = state.space_row(&space_id);
                (
                    space
                        .map(|s| s.display_name().to_string())
                        .unwrap_or_else(|| "this project".into()),
                    space
                        .and_then(|s| state.device_name(&s.device_id))
                        .unwrap_or("its device")
                        .to_string(),
                    state.chats_in_space(&space_id).len(),
                )
            };
            let copy = if count == 1 {
                format!(
                    "Removing “{name}” permanently deletes its 1 session on {device}. This can’t be undone."
                )
            } else {
                format!(
                    "Removing “{name}” permanently deletes its {count} sessions on {device}. This can’t be undone."
                )
            };
            let card = popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Remove project?"))
                .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, copy)))
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "delete-space-cancel")
                                .id("delete-space-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.delete_space_confirm = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_danger(&theme, "Remove")
                                .id("delete-space-confirm")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_space(space_id.clone(), cx)
                                })),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("delete-space-dialog", viewport, card));
        }

        overlays
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str, branch: Option<&str>, cwd: Option<&str>) -> (ChatIndicator, Chat) {
        (
            ChatIndicator::Idle,
            Chat {
                id: id.into(),
                device_id: "dev".into(),
                title: None,
                archived: false,
                cwd: cwd.map(Into::into),
                branch: branch.map(Into::into),
                checkout_id: None,
                config: None,
                last_message_preview: None,
                last_message_at: None,
                created_at: Utc::now(),
                harness_session_id: None,
                harness_session_cwd: None,
                space_id: None,
                last_seen_at: None,
                room_gen: None,
                child: None,
            },
        )
    }

    #[test]
    fn group_chats_keeps_two_same_label_detached_worktrees_distinct() {
        // Two worktrees whose chats carry the same branch label ("detached")
        // but different paths are DIFFERENT checkouts — they must not merge
        // into one group (each header gets its own add button and collapse).
        let groups = group_chats(
            vec![
                chat("a", Some("detached"), Some("/repo/.worktrees/one")),
                chat("b", Some("detached"), Some("/repo/.worktrees/two")),
                chat("c", Some("detached"), Some("/repo/.worktrees/one")),
            ],
            Some("/repo"),
        );
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].chats.len(), 2); // "one" merged
        assert_eq!(
            groups[0].worktree_path.as_deref(),
            Some("/repo/.worktrees/one")
        );
        assert_eq!(groups[1].chats.len(), 1); // "two" distinct
        assert_eq!(
            groups[1].worktree_path.as_deref(),
            Some("/repo/.worktrees/two")
        );
        // Both are worktrees with the same label.
        assert!(groups[0].worktree && groups[1].worktree);
        assert_eq!(groups[0].label, groups[1].label);
        // Branch metadata is preserved for targeting new sessions.
        assert_eq!(groups[0].branch.as_deref(), Some("detached"));
    }

    #[test]
    fn group_chats_ordinary_vs_worktree_with_same_label_never_merge() {
        // A `main` branch chat in the space root and a `main` worktree chat
        // are different checkouts — distinct groups, distinct collapse keys.
        let groups = group_chats(
            vec![
                chat("a", Some("main"), Some("/repo")),
                chat("b", Some("main"), Some("/repo/.worktrees/main")),
            ],
            Some("/repo"),
        );
        assert_eq!(groups.len(), 2);
        assert!(!groups[0].worktree && groups[1].worktree);
        assert_eq!(groups[0].worktree_path, None);
        assert_eq!(
            groups[1].worktree_path.as_deref(),
            Some("/repo/.worktrees/main")
        );
        assert_eq!(groups[0].branch.as_deref(), Some("main"));
        assert_eq!(groups[1].branch.as_deref(), Some("main"));
    }

    #[test]
    fn group_chats_normalizes_cwd_trailing_slashes_for_worktree_detection() {
        // Worktree detection trims trailing slashes/backslashes: a cwd of
        // "/repo/" is the space root, not a worktree — and the representative
        // worktree path stays EXACT (not trimmed) for targeting new sessions.
        let groups = group_chats(
            vec![
                chat("a", Some("main"), Some("/repo/")),
                chat("b", Some("feat"), Some("/repo/.worktrees/feat/")),
            ],
            Some("/repo"),
        );
        assert_eq!(groups.len(), 2);
        assert!(
            !groups[0].worktree,
            "trailing slash on the root is NOT a worktree"
        );
        assert_eq!(groups[0].label, "main");
        assert!(groups[1].worktree);
        // The exact path is preserved as the add-button target.
        assert_eq!(
            groups[1].worktree_path.as_deref(),
            Some("/repo/.worktrees/feat/")
        );
        assert_eq!(groups[1].label, "feat");
    }

    #[test]
    fn group_chats_branch_blank_falls_back_to_current_checkout_label_with_none_branch() {
        // A branch-less/blank chat is the ordinary current checkout: label
        // falls back, but the carried branch metadata stays None (a new
        // session targets the checkout with no branch to name).
        let groups = group_chats(
            vec![
                chat("a", None, Some("/repo")),
                chat("b", Some("  "), Some("/repo")),
            ],
            Some("/repo"),
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "Current checkout");
        assert_eq!(groups[0].branch, None);
        assert!(!groups[0].worktree);
        assert_eq!(groups[0].worktree_path, None);
    }

    #[test]
    fn group_chats_folds_trailing_slash_worktree_into_one_group() {
        // `/repo/.worktrees/wt` and `/repo/.worktrees/wt/` are the SAME
        // checkout — they must fold into ONE group (one identity, one collapse
        // key), while the representative group's raw cwd stays exact as the
        // add-button target (a trailing slash is kept there, not trimmed).
        let groups = group_chats(
            vec![
                chat("a", Some("detached"), Some("/repo/.worktrees/wt")),
                chat("b", Some("detached"), Some("/repo/.worktrees/wt/")),
            ],
            Some("/repo"),
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].chats.len(), 2);
        assert!(groups[0].worktree);
        // The first appearance's EXACT raw cwd is the target.
        assert_eq!(
            groups[0].worktree_path.as_deref(),
            Some("/repo/.worktrees/wt")
        );
    }

    #[test]
    fn group_chats_preserves_trailing_space_in_worktree_identity() {
        // A trailing SPACE is valid path content, not a separator: `/wt` and
        // `/wt ` are distinct identities and must NOT fold together.
        let groups = group_chats(
            vec![
                chat("a", Some("detached"), Some("/repo/.worktrees/wt")),
                chat("b", Some("detached"), Some("/repo/.worktrees/wt ")),
            ],
            Some("/repo"),
        );
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0].worktree_path.as_deref(),
            Some("/repo/.worktrees/wt")
        );
        assert_eq!(
            groups[1].worktree_path.as_deref(),
            Some("/repo/.worktrees/wt ")
        );
    }

    #[test]
    fn branch_group_key_includes_worktree_path() {
        // The collapse key must distinguish same-label detached worktrees,
        // mirroring the grouping identity.
        let a = Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/one"));
        let b = Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/two"));
        let root = Shell::branch_group_key("s:1", false, "main", None);
        assert_ne!(a, b);
        assert_ne!(a, root);
        // Same identity → same key (stable across renders).
        assert_eq!(
            a,
            Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/one"))
        );
    }

    #[test]
    fn branch_group_key_normalizes_trailing_slashes_for_a_stable_collapse_key() {
        // `/wt` and `/wt/` are the same checkout: same disclosure key, so
        // collapsing one collapses the other (status churn never re-opens it).
        let a = Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/wt"));
        let with_slash =
            Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/wt/"));
        assert_eq!(a, with_slash);
        // A trailing SPACE is part of the identity — stays distinct from the
        // bare path (whitespace is never trimmed).
        let spaced = Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/wt "));
        assert_ne!(a, spaced);
    }
}
