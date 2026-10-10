//! Sidebar grouping: a card's chats bucketed by device, then by checkout
//! (branch / worktree), and the hover "+" that starts a session in one.

use super::*;

/// What a collapsed project or checkout group shows for the sessions it
/// hides, in the session rows' marks: how many wait on input and how many
/// finished unseen (the ones worth counting), and whether any is still
/// working (a spinner, uncounted). Idle and errored rows carry no mark of
/// their own, so they add nothing; an empty summary shows nothing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct StatusSummary {
    pub awaiting: usize,
    pub working: bool,
    pub completed: usize,
}

impl StatusSummary {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

pub(super) fn status_summary<'a>(
    statuses: impl IntoIterator<Item = &'a ChatIndicator>,
) -> StatusSummary {
    let mut summary = StatusSummary::default();
    for status in statuses {
        match status {
            ChatIndicator::AwaitingInput => summary.awaiting += 1,
            ChatIndicator::Working => summary.working = true,
            ChatIndicator::Completed => summary.completed += 1,
            ChatIndicator::Errored | ChatIndicator::Idle => {}
        }
    }
    summary
}

/// Quick chats have no checkout to name, so a merged card sections its
/// sessions by host device instead (first appearance orders the sections,
/// sessions keep their overview order). A single host is one section,
/// which the card then renders inline.
pub(super) fn group_chats_by_device(
    chats: Vec<(ChatIndicator, Chat)>,
    device_name: impl Fn(&str) -> String,
) -> Vec<ChatGroup> {
    let mut groups: Vec<ChatGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (status, chat) in chats {
        if let Some(&ix) = index.get(&chat.device_id) {
            groups[ix].chats.push((status, chat));
            continue;
        }
        index.insert(chat.device_id.clone(), groups.len());
        groups.push(ChatGroup {
            label: device_name(&chat.device_id),
            branch: None,
            worktree: false,
            worktree_path: None,
            icon: icons::MONITOR,
            chats: vec![(status, chat)],
        });
    }
    groups
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
pub(super) fn normalize_worktree_path(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

pub(super) fn is_worktree_cwd(cwd: Option<&str>, space_path: Option<&str>) -> bool {
    let (Some(cwd), Some(path)) = (cwd, space_path) else {
        return false;
    };
    normalize_worktree_path(cwd) != normalize_worktree_path(path)
}

pub(super) fn group_chats(
    chats: Vec<(ChatIndicator, Chat)>,
    space_path: Option<&str>,
) -> Vec<ChatGroup> {
    let mut groups: Vec<ChatGroup> = Vec::new();
    // (worktree, worktree path, label) — worktrees also key on their
    // NORMALIZED checkout path so two same-label detached worktrees never
    // merge, while `/wt` and `/wt/` (one checkout) do.
    let mut index: HashMap<(bool, Option<String>, String), usize> = HashMap::new();
    for (status, chat) in chats {
        let worktree = is_worktree_cwd(chat.cwd.as_deref(), space_path);
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
                icons::GIT_WORKTREE
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
pub(super) fn hover_add_plus(
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
        .hover(|s| s.bg(crate::kit::theme::wash(0.10)))
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
