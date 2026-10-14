//! Session slots (docs/design/workspace-layout.md): the per-session UI behind one
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
//! session in `UiSettings.session_docks` (docs/design/workspace-layout.md,
//! decision 7).

use std::cell::Cell;
use std::rc::Rc;

use super::dock::{Dock, DockSurface, terminal_dock_height};
use super::*;
use crate::prefs::SessionDock;
use crate::workspace::TabKey;

mod find;
mod fork;
mod render;
mod side_chats;
mod slots;
mod terminal;

/// Stable slot identity: survives a canvas tab becoming its session
/// (`TabKey::NewSession` → `TabKey::Session`), so async replies and element
/// listeners can hold it.
pub(super) type SlotId = u64;

/// Side chat cap: at most this many temporary side chat tabs per
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
    pub(super) find_input: Entity<TextInput>,
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
    /// Temporary Side Chat tabs: one [`SideChatPanel`] per open
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
        let Some(slot) = self.tiles.slots.get(&sid) else {
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
}
