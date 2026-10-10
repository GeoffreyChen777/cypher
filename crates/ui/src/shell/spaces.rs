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
use crate::kit::theme::MonoStyled;
use crate::pickers::{breadcrumbs, browser_rows, completion_prefix_len, parent_path};
use crate::state::SidebarGroupKind;
use cypher_proto::{Chat, ChatIndicator, Device, FolderListing, Space};
use gpui::FocusHandle;
use std::collections::{HashMap, HashSet};

mod add_space;
mod group_cards;
mod groups;
mod overlays;
mod quick_chat;
mod style_menu;

use groups::*;

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

/// The quick-chat palette: the add-space palette's shell with only the
/// device choice (keyboard highlight + frame focus for ↑↓/⏎/esc).
pub(super) struct QuickChatFlow {
    active: usize,
    focus: FocusHandle,
    focus_pending: bool,
    /// Device-list scroll — keyboard navigation keeps the highlight in view.
    list_scroll: gpui::ScrollHandle,
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
    /// `icons::GIT_WORKTREE` for linked worktrees, `icons::GIT_BRANCH`
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
    kind: SidebarGroupKind,
    pinned: bool,
    icon: Option<String>,
    color: Option<String>,
    title: String,
    device: String,
    /// Whether the header may name its host at all: only when the sidebar
    /// spans more than one device (and then only on hover, or while the
    /// host is offline).
    show_device: bool,
    /// Whether session rows carry their agent mark: only when the sidebar
    /// mixes runtimes — with a single one every row showed the same glyph.
    show_harness: bool,
    offline: bool,
    space_id: Option<String>,
    /// Chats folded into branch/worktree groups (`g.path` of the source
    /// group seeds the worktree detection); on a Quick chats card, one group
    /// per host device instead. Empty for quiet spaces.
    groups: Vec<ChatGroup>,
}

impl GroupCard {
    /// A lone ordinary checkout gets no section header: its sessions sit
    /// directly under the project and the branch rides the project header
    /// as a suffix. A lone linked worktree keeps its header — that checkout
    /// is not the project root, and its plus targets the worktree.
    fn inline_groups(&self) -> bool {
        match self.groups.as_slice() {
            [] => true,
            [only] => !only.worktree,
            _ => false,
        }
    }

    fn chat_count(&self) -> usize {
        self.groups.iter().map(|g| g.chats.len()).sum()
    }
}

/// Target of the follow-up "delete the worktree too?" dialog shown after the
/// last session using a linked worktree is removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OrphanWorktree {
    pub repo_path: String,
    pub worktree_path: String,
    pub device_id: String,
    /// Sidebar group label (branch, else "Current checkout") so the dialog
    /// names the same checkout the user just emptied.
    pub label: String,
}

/// If deleting `chat_id` would leave its linked worktree with no remaining
/// sessions (including archived ones; excluding children of this chat, which
/// cascade with the parent), return that worktree so the shell can ask
/// whether to delete it too.
pub(super) fn orphan_worktree_after_delete(
    chats: &[Chat],
    spaces: &[Space],
    chat_id: &str,
) -> Option<OrphanWorktree> {
    let chat = chats.iter().find(|c| c.id == chat_id)?;
    let cwd = chat.cwd.as_deref()?;
    let space = chat
        .space_id
        .as_deref()
        .and_then(|id| spaces.iter().find(|s| s.id == id))?;
    if !is_worktree_cwd(Some(cwd), Some(space.path.as_str())) {
        return None;
    }
    let cwd_key = normalize_worktree_path(cwd);
    let others_remain = chats.iter().any(|other| {
        if other.id == chat_id || other.parent_chat_id() == Some(chat_id) {
            return false;
        }
        if other.device_id != chat.device_id {
            return false;
        }
        other
            .cwd
            .as_deref()
            .is_some_and(|path| normalize_worktree_path(path) == cwd_key)
    });
    if others_remain {
        return None;
    }
    let label = chat
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "Current checkout".into());
    Some(OrphanWorktree {
        repo_path: space.path.clone(),
        worktree_path: cwd.to_string(),
        device_id: chat.device_id.clone(),
        label,
    })
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
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.14),
            ))
            .on_hover(motion::hover_listener("sidebar-add-project"))
            .on_click(cx.listener(|this, _, _, cx| this.open_add_space(cx)))
            .child(icon(icons::PLUS).size(px(14.0)).text_color(theme.text));
        // View menu: filter the cards by device and pick their sort. Tinted
        // while a non-default view is active so the narrowed list is obvious.
        let view_active = self.settings.sidebar_device_filter.is_some()
            || self.settings.sidebar_sort != crate::prefs::SidebarSort::Activity
            || self.settings.sidebar_sort_reversed;
        let view_tint = if view_active {
            theme.accent
        } else {
            motion::hover_blend(
                "sidebar-view-menu",
                theme.text_muted.opacity(0.8),
                theme.text,
            )
        };
        let view_button = div()
            .id("sidebar-view-menu")
            .size(px(28.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.0))
            .cursor_pointer()
            .text_color(view_tint)
            .bg(motion::hover_blend(
                "sidebar-view-menu",
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.14),
            ))
            .on_hover(motion::hover_listener("sidebar-view-menu"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.close_space_menu(cx);
                    this.sidebar_view_menu.open(event.position);
                    cx.notify();
                }),
            )
            // gpui SVGs take no colour from their parent: tint the glyph itself.
            .child(icon(icons::TUNING).size(px(14.0)).text_color(view_tint));
        // Quick chat: a session in a throwaway folder on a device of your
        // choice — no project needed (the dialog only asks for the device).
        let quick_chat = div()
            .id("sidebar-quick-chat")
            .size(px(28.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.0))
            .cursor_pointer()
            .text_color(motion::hover_blend(
                "sidebar-quick-chat",
                theme.text_muted.opacity(0.8),
                theme.text,
            ))
            .bg(motion::hover_blend(
                "sidebar-quick-chat",
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.14),
            ))
            .on_hover(motion::hover_listener("sidebar-quick-chat"))
            .on_click(cx.listener(|this, _, _, cx| this.open_quick_chat_dialog(cx)))
            .child(
                icon(icons::CHAT_ROUND_LINE)
                    .size(px(14.0))
                    .text_color(theme.text),
            );
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
            // A project window lists one fixed project: no view menu, quick
            // chats or new projects (those belong to the main window).
            .when(!self.is_project_window(), |el| {
                el.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(2.0))
                        .child(view_button)
                        .child(quick_chat)
                        .child(add_project),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
