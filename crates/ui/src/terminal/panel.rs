//! The terminal panel: session-scoped tabs over engine PTYs.
//!
//! Tabs are per selected chat and restored on return
//! (emulators — and their server-side PTYs — survive navigation; detach is not
//! close). Tab bar supports pointer drag-reorder with 150 ms sliding
//! transforms, middle-click close, and a "+" new-tab button; Cmd/Ctrl+J
//! toggles the panel (the shell owns the height animation + persistence).
//!
//! Data path per tab: `OpenTerminal` → `SubscribeTerminal` stream; Data frames
//! (base64) feed the [`Emulator`]; query responses write back; the stream
//! reconnects with exponential backoff resuming from `afterSeq`; Exit appends
//! the "[process exited N]" line and stops. Keyboard bytes coalesce for 12 ms
//! before `WriteTerminal`; viewport-driven resizes debounce 80 ms before
//! `ResizeTerminal` (the emulator resizes immediately).

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, Context, Entity, FocusHandle, IntoElement, KeyBinding, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollDelta, SharedString,
    Subscription, Task, Window, actions, div, prelude::*, px,
};

use cypher_proto::{TerminalEvent, TerminalSession};
use cypher_rpc::methods;

use crate::kit::motion::{self, AnimationExt as _, TAB_SLIDE};
use crate::kit::theme::Theme;
use crate::kit::theme::terminal::terminal_panel_bg;
use crate::state::{AppState, EngineHandle};

use super::emulator::{CellSnapshot, CursorSnapshot, Emulator, GridPoint, SelectionType, Side};
use super::view::{
    COALESCE_MS, InputCoalescer, RESIZE_DEBOUNCE_MS, SELECTION_DRAG_THRESHOLD, TerminalElement,
    cell_at, keystroke_bytes, paste_bytes,
};

mod pointer;
mod stream;
mod tab_bar;
mod tabs;

pub use tabs::*;

/// Fixed tab width — drag-reorder math stays analytic.
pub const TAB_WIDTH: f32 = 118.0;
pub const TAB_BAR_HEIGHT: f32 = 40.0;
/// Tab chip height inside the bar (the workspace tile tabs match it).
pub const TAB_HEIGHT: f32 = 28.0;

actions!(terminal, [ToggleTerminal]);

/// Bind the terminal keymap (global): Cmd+J on macOS, Ctrl+J elsewhere.
pub fn init(cx: &mut App) {
    let toggle = if cfg!(target_os = "macos") {
        "cmd-j"
    } else {
        "ctrl-j"
    };
    cx.bind_keys([KeyBinding::new(toggle, ToggleTerminal, None)]);
}

/// Whether `key` is the ACTIVE tab of `tabs` — the only close that may
/// dismiss/clear the active selection/comment (a background close must
/// preserve the active tab's editor). Pure so the active/background close
/// decision is testable without a gpui context.
fn is_active_tab(tabs: &ChatTabs, key: u64) -> bool {
    tabs.tabs.get(tabs.active).is_some_and(|t| t.key == key)
}

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

/// A grid snapshot handed to the paint element.
pub struct GridSnapshot {
    pub lines: Vec<Vec<CellSnapshot>>,
    pub cursor: Option<CursorSnapshot>,
}

/// Where the grid landed this frame, in window coordinates.
///
/// Reported by element prepaint because that is the only place the measured
/// font metrics exist. Mouse events arrive on the wrapping div in window
/// space, so mapping a pointer to a cell needs the glyph origin and the cell
/// size the *current* frame used — a stale one puts the selection a row off
/// after a resize.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridGeometry {
    /// Top-left of the first glyph (bounds origin plus padding).
    pub origin: gpui::Point<Pixels>,
    pub cell_w: f32,
    pub line_h: f32,
    pub cols: u16,
    pub rows: u16,
}

/// An in-flight left-button gesture.
///
/// A press alone does not select. It arms this, and only pointer travel past
/// [`SELECTION_DRAG_THRESHOLD`] promotes it to a real selection — otherwise the
/// click that focuses the panel would leave a one-cell selection behind
/// whenever the hand moves a pixel.
#[derive(Debug, Clone, Copy)]
struct SelectionDrag {
    /// Press position, in window space: both the threshold origin and the
    /// selection's anchor, so the selection starts where the press landed
    /// rather than where the threshold happened to trip.
    origin: gpui::Point<Pixels>,
    armed: bool,
}

struct TerminalTab {
    key: u64,
    title: SharedString,
    terminal_id: Option<String>,
    emulator: Emulator,
    exited: Option<i32>,
    last_seq: u64,
    coalescer: InputCoalescer,
    flush_task: Option<Task<()>>,
    resize_task: Option<Task<()>>,
    /// Open + subscribe/reconnect lifecycle; dropping it cancels the stream.
    _run: Option<Task<()>>,
}

#[derive(Default)]
struct ChatTabs {
    tabs: Vec<TerminalTab>,
    active: usize,
}

/// Drag-reorder state; `epoch` keys the 150 ms slide animation restarts.
struct DragState {
    from: usize,
    over: usize,
    epoch: usize,
    prev_over: usize,
}

/// The dragged-tab payload (gpui drag-and-drop).
struct TabDragPayload {
    chat: String,
    from: usize,
    title: SharedString,
}

pub struct TerminalPanel {
    state: Entity<AppState>,
    focus_handle: FocusHandle,
    chats: HashMap<String, ChatTabs>,
    /// Shell-driven visibility gate: no RPC happens while closed (lazy).
    open: bool,
    tab_seq: u64,
    drag: Option<DragState>,
    last_selected: Option<String>,
    /// Last reported grid placement; `None` until the first prepaint.
    geometry: Option<GridGeometry>,
    /// Left-button gesture in flight, if any.
    selection_drag: Option<SelectionDrag>,
    /// The shared shell-level Comment pill/editor (weak — the shell owns it):
    /// a settled terminal selection offers its text; scroll/tab/close/chat/
    /// resize/output dismiss it.
    comment_popup: gpui::WeakEntity<crate::comment_popup::CommentPopup>,
    /// THIS panel's popup owner id — allocated per panel so the panels of
    /// different session tiles never dismiss each other's pill.
    comment_owner: crate::comment_popup::CommentOwner,
    _observe: Subscription,
}

impl TerminalPanel {
    pub fn new(
        state: Entity<AppState>,
        comment_popup: gpui::WeakEntity<crate::comment_popup::CommentPopup>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.on_state_changed(cx));
        Self {
            state,
            focus_handle: cx.focus_handle(),
            chats: HashMap::new(),
            open: false,
            tab_seq: 0,
            drag: None,
            last_selected: None,
            geometry: None,
            selection_drag: None,
            comment_popup,
            comment_owner: crate::comment_popup::CommentOwner::next_terminal(),
            _observe: observe,
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// Re-bind a parked panel (its tab closed; the shell kept it so the
    /// chat's PTYs stay reachable) to a new tile's session context. The
    /// chat's tabs are keyed by chat id, so they reattach as they were.
    pub fn rebind(&mut self, state: Entity<AppState>, cx: &mut Context<Self>) {
        self._observe = cx.observe(&state, |this: &mut Self, _, cx| this.on_state_changed(cx));
        self.state = state;
        self.on_state_changed(cx);
        cx.notify();
    }

    /// Close every terminal this panel holds (its chat is gone): release
    /// the PTYs host-side and drop the tabs.
    pub fn close_all(&mut self, cx: &mut Context<Self>) {
        self.dismiss_popup(cx);
        self.clear_active_selection(cx);
        let engine = self.engine(cx);
        for (chat, tabs) in std::mem::take(&mut self.chats) {
            let target = self.chat_target(&chat, cx);
            for id in tabs.tabs.into_iter().filter_map(|tab| tab.terminal_id) {
                let Some(engine) = engine.clone() else {
                    continue;
                };
                let target = target.clone();
                cx.spawn(async move |_, _| {
                    let _ = engine
                        .client()
                        .call(
                            methods::CLOSE_TERMINAL,
                            with_target(serde_json::json!({ "terminalId": id }), &target),
                        )
                        .await;
                })
                .detach();
            }
        }
        self.drag = None;
        cx.notify();
    }

    /// Shell toggle hook. Opening lazily creates the first tab for the
    /// selected chat; closing
    /// keeps every session alive (detach ≠ close).
    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.open = open;
        if open {
            self.ensure_tab(cx);
        }
        if !open {
            // Closing the drawer: its comment pill/selection would float over
            // the wrong surface.
            self.dismiss_popup(cx);
            self.clear_active_selection(cx);
        }
        cx.notify();
    }

    /// A tab's display label: the live OSC 0/2 title when the running
    /// program set one (shells title themselves with the cwd / running
    /// command — the contextual name, user request), else the fixed
    /// "Terminal N".
    fn display_title(tab: &TerminalTab) -> SharedString {
        match tab.emulator.title().map(str::trim) {
            Some(title) if !title.is_empty() => title.to_string().into(),
            _ => tab.title.clone(),
        }
    }

    fn on_state_changed(&mut self, cx: &mut Context<Self>) {
        let selected = self.state.read(cx).selected_chat.clone();
        let switched = selected != self.last_selected;
        if switched {
            let previous = self.last_selected.clone();
            self.last_selected = selected;
            self.drag = None;
            // A chat switch invalidates the previous chat's selection.
            self.dismiss_popup(cx);
            if let Some(previous) = previous {
                self.clear_chat_active_selection(&previous, cx);
            }
        }
        if self.open {
            // Returning to a chat with tabs restores them; a fresh chat (or an
            // engine that only just finished booting) gets its first tab —
            // ensure_tab is idempotent, so calling on every state change is safe.
            self.ensure_tab(cx);
        }
        if switched {
            cx.notify();
        }
    }

    fn engine(&self, cx: &App) -> Option<EngineHandle> {
        self.state.read(cx).engine().cloned()
    }

    /// The chat's host device when it differs from the connected engine's own —
    /// the PTY lives on the chat's host device, so every terminal RPC for a remote
    /// chat needs the `targetDeviceId` passthrough. Without it the local
    /// engine checks the chat's cwd against its OWN filesystem and fails with
    /// "Session working directory is unavailable" (user report).
    fn chat_target(&self, chat: &str, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let device = state.chats.iter().find(|c| c.id == chat)?.device_id.clone();
        (state.local_device_id.as_deref() != Some(device.as_str())).then_some(device)
    }

    fn selected_chat(&self, cx: &App) -> Option<String> {
        self.state.read(cx).selected_chat.clone()
    }

    fn ensure_tab(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.selected_chat(cx) else {
            return;
        };
        if self.chats.get(&chat).is_none_or(|c| c.tabs.is_empty()) {
            self.open_tab(chat, cx);
        }
    }

    fn tab_mut(&mut self, chat: &str, key: u64) -> Option<&mut TerminalTab> {
        self.chats
            .get_mut(chat)?
            .tabs
            .iter_mut()
            .find(|t| t.key == key)
    }

    fn active_tab(&self, cx: &App) -> Option<&TerminalTab> {
        let chat = self.state.read(cx).selected_chat.clone()?;
        let tabs = self.chats.get(&chat)?;
        tabs.tabs.get(tabs.active)
    }

    // ---- open / stream lifecycle ----

    fn open_tab(&mut self, chat: String, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else {
            return;
        };
        // The new tab becomes ACTIVE: before the switch, dismiss/clear the
        // outgoing active tab's comment/selection (scoped to THIS panel's
        // owner — never another surface's pill). The first tab has no
        // outgoing active tab, so opening it must not disturb anything.
        if self.chats.get(&chat).is_some_and(|c| !c.tabs.is_empty()) {
            self.dismiss_popup(cx);
            self.clear_chat_active_selection(&chat, cx);
        }
        self.tab_seq += 1;
        let key = self.tab_seq;
        let entry = self.chats.entry(chat.clone()).or_default();
        let tab_no = entry.tabs.len() + 1;
        entry.tabs.push(TerminalTab {
            key,
            title: format!("Terminal {tab_no}").into(),
            terminal_id: None,
            emulator: Emulator::new(80, 24),
            exited: None,
            last_seq: 0,
            coalescer: InputCoalescer::default(),
            flush_task: None,
            resize_task: None,
            _run: None,
        });
        entry.active = entry.tabs.len() - 1;

        let target = self.chat_target(&chat, cx);
        let run = Self::spawn_session(chat.clone(), key, engine, target, cx);
        if let Some(tab) = self.tab_mut(&chat, key) {
            tab._run = Some(run);
        }
        cx.notify();
    }

    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let bracketed = self
            .active_tab(cx)
            .map(|tab| tab.emulator.bracketed_paste_mode())
            .unwrap_or(false);
        let bytes = paste_bytes(&text, bracketed);
        self.queue_input(&bytes, cx);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let mods = &ks.modifiers;
        // Paste: Cmd+V (macOS) / Ctrl+Shift+V.
        if ks.key == "v" && (mods.platform || (mods.control && mods.shift)) {
            self.paste_clipboard(cx);
            cx.stop_propagation();
            return;
        }
        // Copy: Cmd+C (macOS) / Ctrl+Shift+C. Only swallowed when it actually
        // copied — so Ctrl+Shift+C with nothing selected still falls through
        // to the interrupt, and plain Ctrl+C (no shift) never reaches here.
        if ks.key == "c" && (mods.platform || (mods.control && mods.shift)) {
            // Terminal-native selection wins. If focus stayed in the terminal
            // while the user selected transcript/diff text, fall back to the
            // shared custom selection just like TextInput does.
            if self.copy_selection(cx) || self.copy_surface_selection(cx) {
                cx.stop_propagation();
                return;
            }
        }
        let app_cursor = self
            .active_tab(cx)
            .map(|tab| tab.emulator.app_cursor_mode())
            .unwrap_or(false);
        if let Some(bytes) = keystroke_bytes(&ks.key, ks.key_char.as_deref(), mods, app_cursor) {
            self.queue_input(&bytes, cx);
            cx.stop_propagation();
        }
    }

    // ---- grid metrics / element hooks ----

    /// Called from element prepaint with the frame's grid placement. Resizes
    /// the emulator immediately; the `ResizeTerminal` RPC debounces 80 ms.
    pub fn on_grid_metrics(&mut self, geometry: GridGeometry, cx: &mut Context<Self>) {
        // Stash unconditionally, before the early returns below: pointer
        // mapping needs the placement even on frames where nothing resized,
        // which is almost all of them.
        self.geometry = Some(geometry);
        let (cols, rows) = (geometry.cols, geometry.rows);
        let Some(chat) = self.selected_chat(cx) else {
            return;
        };
        let Some(tabs) = self.chats.get_mut(&chat) else {
            return;
        };
        let active = tabs.active;
        let Some(tab) = tabs.tabs.get_mut(active) else {
            return;
        };
        if tab.emulator.cols() == cols as usize && tab.emulator.rows() == rows as usize {
            return;
        }
        tab.emulator.resize(cols, rows);
        let key = tab.key;
        // A resize invalidates the grid geometry the selection was made in —
        // drop the terminal pill and its selection wash. No notify: this runs
        // during prepaint of the current frame (which paints the new grid).
        self.dismiss_popup(cx);
        self.with_active_emulator(cx, |emu| emu.clear_selection());
        let engine = self.engine(cx);
        let target = self.chat_target(&chat, cx);
        if let (Some(engine), Some(tab)) = (engine, self.tab_mut(&chat, key)) {
            let id = tab.terminal_id.clone();
            tab.resize_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(RESIZE_DEBOUNCE_MS))
                    .await;
                // Re-read the *current* size — later prepaints may have
                // resized again inside the debounce window.
                let Ok(current) = this.update(cx, |panel, _| {
                    panel
                        .tab_mut(&chat, key)
                        .map(|t| (t.terminal_id.clone(), t.emulator.cols(), t.emulator.rows()))
                }) else {
                    return;
                };
                let Some((stored_id, cols, rows)) = current else {
                    return;
                };
                let Some(id) = stored_id.or(id) else { return };
                let _ = engine
                    .client()
                    .call(
                        methods::RESIZE_TERMINAL,
                        with_target(
                            serde_json::json!({ "terminalId": id, "cols": cols, "rows": rows }),
                            &target,
                        ),
                    )
                    .await;
            }));
        }
        // Deliberately no cx.notify(): this runs during prepaint of the
        // current frame, which already paints the resized grid.
    }

    /// Snapshot for the paint element.
    pub fn active_grid_snapshot(&self, cx: &App) -> Option<GridSnapshot> {
        let tab = self.active_tab(cx)?;
        Some(GridSnapshot {
            lines: tab.emulator.lines(),
            cursor: tab.emulator.cursor(),
        })
    }

    // ---- selection ----

    /// Run `f` against the active tab's emulator.
    fn with_active_emulator<R>(
        &mut self,
        cx: &App,
        f: impl FnOnce(&mut Emulator) -> R,
    ) -> Option<R> {
        let chat = self.selected_chat(cx)?;
        let tabs = self.chats.get_mut(&chat)?;
        let active = tabs.active;
        tabs.tabs.get_mut(active).map(|tab| f(&mut tab.emulator))
    }

    fn scroll_active(&mut self, delta_lines: i32, cx: &mut Context<Self>) {
        if delta_lines == 0 {
            return;
        }
        // Scrolling moves the grid under the pill's window anchor.
        self.dismiss_popup(cx);
        self.clear_active_selection(cx);
        let Some(chat) = self.selected_chat(cx) else {
            return;
        };
        let Some(tabs) = self.chats.get_mut(&chat) else {
            return;
        };
        let active = tabs.active;
        if let Some(tab) = tabs.tabs.get_mut(active) {
            tab.emulator.scroll(delta_lines);
            cx.notify();
        }
    }

    // ---- tab management ----

    fn select_tab(&mut self, chat: &str, ix: usize, cx: &mut Context<Self>) {
        let active = self
            .chats
            .get(chat)
            .map(|tabs| tabs.active)
            .unwrap_or(usize::MAX);
        if active != ix {
            // The selection belongs to the tab being left.
            self.dismiss_popup(cx);
            self.clear_active_selection(cx);
        }
        if let Some(tabs) = self.chats.get_mut(chat)
            && ix < tabs.tabs.len()
        {
            tabs.active = ix;
            cx.notify();
        }
    }

    fn close_tab(&mut self, chat: &str, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        // Only closing the ACTIVE tab invalidates its selection/comment — a
        // background close must preserve the active tab's editor. If the
        // active tab falls back to a sibling, stale UI is gone before the
        // swap.
        let closing_active = self
            .chats
            .get(chat)
            .is_some_and(|tabs| is_active_tab(tabs, key));
        if closing_active {
            self.dismiss_popup(cx);
            self.clear_active_selection(cx);
        }
        let engine = self.engine(cx);
        let target = self.chat_target(chat, cx);
        let Some(tabs) = self.chats.get_mut(chat) else {
            return;
        };
        let Some(ix) = tabs.tabs.iter().position(|t| t.key == key) else {
            return;
        };
        let tab = tabs.tabs.remove(ix);
        tabs.active = active_after_close(tabs.active, ix, tabs.tabs.len());
        let now_empty = tabs.tabs.is_empty();
        self.drag = None;
        // Closing the LAST terminal closes the drawer too — an empty dock is
        // dead space (user request). Same path as the collapse chevron.
        if now_empty && self.open {
            window.dispatch_action(Box::new(ToggleTerminal), cx);
        }
        if let (Some(engine), Some(id)) = (engine, tab.terminal_id.clone()) {
            cx.spawn(async move |_, _| {
                let _ = engine
                    .client()
                    .call(
                        methods::CLOSE_TERMINAL,
                        with_target(serde_json::json!({ "terminalId": id }), &target),
                    )
                    .await;
            })
            .detach();
        }
        cx.notify();
    }

    fn commit_reorder(&mut self, chat: &str, from: usize, to: usize, cx: &mut Context<Self>) {
        if let Some(tabs) = self.chats.get_mut(chat) {
            let active = tabs.active;
            reorder_tabs(&mut tabs.tabs, from, to);
            tabs.active = active_after_reorder(active, from, to);
        }
        self.drag = None;
        cx.notify();
    }

    fn update_drag_over(&mut self, from: usize, over: usize, cx: &mut Context<Self>) {
        match &mut self.drag {
            Some(drag) if drag.over != over => {
                drag.prev_over = drag.over;
                drag.over = over;
                drag.epoch += 1;
                cx.notify();
            }
            Some(_) => {}
            None => {
                self.drag = Some(DragState {
                    from,
                    over,
                    epoch: 0,
                    prev_over: from,
                });
                cx.notify();
            }
        }
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::appearance::surface_style::theme(
            crate::appearance::surface_style::Region::Terminal,
            cx,
        );
        // Heal drag state if the pointer was released outside the bar.
        if self.drag.is_some() && !cx.has_active_drag() {
            self.drag = None;
        }
        let panel_bg = terminal_panel_bg(&theme);
        let Some(chat) = self.selected_chat(cx) else {
            return div()
                .size_full()
                .bg(panel_bg)
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .child(SharedString::from("Select a chat to open a terminal"))
                .into_any_element();
        };
        let focused = self.focus_handle.is_focused(window);

        let tab_bar = self.render_tab_bar(&chat, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(panel_bg)
            .child(tab_bar)
            .child(
                div()
                    .id("terminal-body")
                    .flex_1()
                    .min_h_0()
                    .key_context("Terminal")
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(Self::on_key_down))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    // Bound on the window, not the element: a drag that ends
                    // outside the panel still has to end the gesture, or the
                    // next unrelated pointer move keeps extending a selection
                    // the user let go of.
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                        let lines = match event.delta {
                            ScrollDelta::Lines(delta) => delta.y,
                            ScrollDelta::Pixels(delta) => {
                                f32::from(delta.y) / super::view::TERM_LINE_HEIGHT
                            }
                        };
                        let step = lines.round() as i32;
                        this.scroll_active(step, cx);
                    }))
                    .child(TerminalElement::new(cx.entity(), focused)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_clamps_between_160_and_55vh() {
        assert_eq!(clamp_terminal_height(300.0, 900.0), 300.0);
        assert_eq!(clamp_terminal_height(10.0, 900.0), 160.0);
        assert_eq!(clamp_terminal_height(4000.0, 900.0), 900.0 * 0.55);
        // Tiny windows: min wins over the 55vh cap.
        assert_eq!(clamp_terminal_height(200.0, 100.0), 160.0);
        assert_eq!(clamp_terminal_height(f32::NAN, 900.0), 160.0);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_ms(0), 500);
        assert_eq!(backoff_ms(1), 1000);
        assert_eq!(backoff_ms(2), 2000);
        assert_eq!(backoff_ms(3), 4000);
        assert_eq!(backoff_ms(4), 8000);
        assert_eq!(backoff_ms(10), 8000);
        assert_eq!(backoff_ms(u32::MAX), 8000);
    }

    #[test]
    fn reorder_moves_forward_and_backward() {
        let mut v = vec!["a", "b", "c", "d"];
        reorder_tabs(&mut v, 0, 2);
        assert_eq!(v, ["b", "c", "a", "d"]);
        reorder_tabs(&mut v, 3, 0);
        assert_eq!(v, ["d", "b", "c", "a"]);
        // Out-of-range / no-op moves leave the vec untouched.
        reorder_tabs(&mut v, 9, 0);
        reorder_tabs(&mut v, 1, 1);
        assert_eq!(v, ["d", "b", "c", "a"]);
    }

    #[test]
    fn drop_index_quantizes_and_clamps() {
        assert_eq!(drop_index(-10.0, 150.0, 3), 0);
        assert_eq!(drop_index(0.0, 150.0, 3), 0);
        assert_eq!(drop_index(149.0, 150.0, 3), 0);
        assert_eq!(drop_index(150.0, 150.0, 3), 1);
        assert_eq!(drop_index(700.0, 150.0, 3), 2);
        assert_eq!(drop_index(50.0, 150.0, 0), 0);
    }

    #[test]
    fn slide_offsets_shift_toward_the_gap() {
        // Dragging 0 over 2: tabs 1 and 2 slide left one slot.
        assert_eq!(slide_offset(0, 0, 2), 0.0);
        assert_eq!(slide_offset(1, 0, 2), -1.0);
        assert_eq!(slide_offset(2, 0, 2), -1.0);
        assert_eq!(slide_offset(3, 0, 2), 0.0);
        // Dragging 3 over 1: tabs 1 and 2 slide right.
        assert_eq!(slide_offset(0, 3, 1), 0.0);
        assert_eq!(slide_offset(1, 3, 1), 1.0);
        assert_eq!(slide_offset(2, 3, 1), 1.0);
        assert_eq!(slide_offset(3, 3, 1), 0.0);
        // Hovering the origin: nothing moves.
        for ix in 0..4 {
            assert_eq!(slide_offset(ix, 2, 2), 0.0);
        }
    }

    #[test]
    fn active_index_tracks_reorders() {
        // The active tab itself moves.
        assert_eq!(active_after_reorder(1, 1, 3), 3);
        // A tab hopping over the active one from the left shifts it down.
        assert_eq!(active_after_reorder(2, 0, 3), 1);
        // …and from the right shifts it up.
        assert_eq!(active_after_reorder(1, 3, 0), 2);
        // Disjoint moves leave it alone.
        assert_eq!(active_after_reorder(0, 2, 3), 0);
    }

    #[test]
    fn active_index_tracks_closes() {
        assert_eq!(active_after_close(2, 0, 3), 1); // close left of active
        assert_eq!(active_after_close(1, 1, 2), 1); // close active mid-list
        assert_eq!(active_after_close(2, 2, 2), 1); // close active at tail
        assert_eq!(active_after_close(0, 0, 0), 0); // last tab closed
    }

    /// Minimal [`ChatTabs`] with `count` tabs keyed `0..count`, tab 0 active.
    fn test_tabs(count: usize) -> ChatTabs {
        ChatTabs {
            tabs: (0..count)
                .map(|key| TerminalTab {
                    key: key as u64,
                    title: format!("Terminal {}", key + 1).into(),
                    terminal_id: None,
                    emulator: Emulator::new(80, 24),
                    exited: None,
                    last_seq: 0,
                    coalescer: InputCoalescer::default(),
                    flush_task: None,
                    resize_task: None,
                    _run: None,
                })
                .collect(),
            active: 0,
        }
    }

    #[test]
    fn close_decision_targets_only_the_active_tab() {
        // Tab 0 is active; background closes must not touch it.
        let tabs = test_tabs(3);
        assert!(is_active_tab(&tabs, 0), "active close dismisses/clears");
        assert!(!is_active_tab(&tabs, 1), "background close preserves");
        assert!(!is_active_tab(&tabs, 2), "background close preserves");
        // An unknown key (already-closed tab) is never active.
        assert!(!is_active_tab(&tabs, 99));
        // An empty chat has no active tab to dismiss.
        assert!(!is_active_tab(&ChatTabs::default(), 0));
    }

    #[test]
    fn close_decision_follows_the_active_index() {
        let mut tabs = test_tabs(3);
        tabs.active = 2;
        assert!(!is_active_tab(&tabs, 0));
        assert!(!is_active_tab(&tabs, 1));
        assert!(is_active_tab(&tabs, 2));
        // A stale active index past the end (empty after close) matches nothing.
        let empty = ChatTabs {
            tabs: Vec::new(),
            active: 1,
        };
        assert!(!is_active_tab(&empty, 0));
    }

    #[test]
    fn exit_message_format() {
        let text = String::from_utf8(exit_message(0)).unwrap();
        assert!(text.contains("[process exited 0]"));
        let text = String::from_utf8(exit_message(137)).unwrap();
        assert!(text.contains("[process exited 137]"));
        assert!(text.starts_with("\r\n"));
        assert!(text.ends_with("\r\n"));
    }

    #[test]
    fn stream_events_deserialize_per_contract() {
        let data: TerminalEvent =
            serde_json::from_str(r#"{"type":"data","seq":7,"data":"aGk="}"#).unwrap();
        assert_eq!(
            data,
            TerminalEvent::Data {
                seq: 7,
                data: "aGk=".into()
            }
        );
        let exit: TerminalEvent =
            serde_json::from_str(r#"{"type":"exit","seq":8,"exitCode":130}"#).unwrap();
        assert_eq!(
            exit,
            TerminalEvent::Exit {
                seq: 8,
                exit_code: 130,
                signal: None
            }
        );
        let session: TerminalSession =
            serde_json::from_str(r#"{"id":"t1","cwd":"/w","shell":"/bin/zsh"}"#).unwrap();
        assert_eq!(session.id, "t1");
        assert_eq!(session.shell, "/bin/zsh");
    }

    #[test]
    fn base64_round_trip_and_tolerance() {
        assert_eq!(decode_base64("aGk="), b"hi".to_vec());
        assert_eq!(
            decode_base64("aGk"),
            b"hi".to_vec(),
            "unpadded input tolerated"
        );
        assert_eq!(
            decode_base64("!!!"),
            Vec::<u8>::new(),
            "garbage decodes to nothing"
        );
        assert_eq!(encode_base64(b"hi"), "aGk=");
    }

    #[test]
    fn exit_message_feeds_cleanly_through_the_emulator() {
        let mut emulator = Emulator::new(40, 4);
        emulator.feed(b"$ done");
        emulator.feed(&exit_message(1));
        assert_eq!(emulator.row_text(1), "[process exited 1]");
    }
}
