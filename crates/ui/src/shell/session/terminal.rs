//! A session's terminal dock: its panel, height, toggle and drag-resize,
//! and the container it renders in.

use super::*;

impl Shell {
    fn terminal_panel(
        &mut self,
        sid: SlotId,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalPanel>> {
        let popup = self.comment_popup.clone().downgrade();
        let slot = self.tiles.slots.get_mut(&sid)?;
        if let Some(terminal) = &slot.terminal {
            return Some(terminal.clone());
        }
        let state = slot.state.clone();
        let terminal = cx.new(|cx| TerminalPanel::new(state, popup, cx));
        slot.terminal = Some(terminal.clone());
        Some(terminal)
    }

    /// The dock's resting height: the session's fraction of its area (the
    /// legacy global height if never dragged), clamped to this tile's
    /// session area (a short tile never loses its composer to it).
    fn terminal_height(&self, slot: &SessionSlot) -> f32 {
        let area_h = f32::from(slot.area.get().size.height);
        let height = terminal_dock_height(
            slot.terminal_fraction,
            self.settings.terminal_height,
            area_h,
        );
        super::dock::fit_terminal_height(height, area_h, slot.bottom_stack.get())
    }

    pub(super) fn terminal_target(&self, slot: &SessionSlot) -> f32 {
        if slot.terminal_open {
            self.terminal_height(slot)
        } else {
            0.0
        }
    }

    /// Cmd/Ctrl+J and the header button. Height
    /// animates 200 ms; closing detaches (PTYs stay alive), opening restores.
    /// The flag is per session tile (zeron `sessionPanels`).
    pub(in crate::shell) fn toggle_terminal(
        &mut self,
        sid: SlotId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.tiles.slots.get(&sid) else {
            return;
        };
        let from = self.terminal_target(slot);
        let Some(panel) = self.terminal_panel(sid, cx) else {
            return;
        };
        let Some(slot) = self.tiles.slots.get_mut(&sid) else {
            return;
        };
        slot.terminal_open = !slot.terminal_open;
        let open = slot.terminal_open;
        let composer = slot.composer.clone();
        let to = self
            .tiles
            .slots
            .get(&sid)
            .map_or(0.0, |slot| self.terminal_target(slot));
        if let Some(slot) = self.tiles.slots.get_mut(&sid) {
            slot.terminal_tween = Some(WidthTween::new(from, to));
        }
        panel.update(cx, |panel, cx| panel.set_open(open, cx));
        if open {
            // Opening lands keyboard focus IN the shell — typing goes straight
            // to the prompt, no click needed (zeron terminal-panel.tsx: the
            // visible+active effect calls `terminal.focus()` on every open).
            // The handle is focusable before the panel's first paint; once the
            // terminal body mounts with `track_focus` it receives the keys.
            window.focus(&panel.read(cx).focus_handle(), cx);
        } else {
            // Hiding the panel removes the (likely focused) terminal view;
            // with nothing focused, window key bindings stop dispatching, so
            // hand focus to the composer. (Cmd+J is a pure toggle — a second
            // press closes even while the terminal is focused, as in zeron's
            // `useHotkey(toggleShortcut, ... setOpenScoped(!open))`.)
            window.focus(&composer.focus_handle(cx), cx);
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(RESIZE.total() + Duration::from_millis(30))
                .await;
            this.update(cx, |shell, cx| {
                if let Some(slot) = shell.tiles.slots.get_mut(&sid) {
                    slot.terminal_tween = None;
                }
                cx.notify();
            })
            .ok();
        });
        if let Some(slot) = self.tiles.slots.get_mut(&sid) {
            slot.terminal_tween_task = Some(task);
        }
        self.remember_slot_docks(sid, cx);
        cx.notify();
    }

    pub(in crate::shell) fn on_terminal_drag(
        &mut self,
        event: &gpui::DragMoveEvent<TerminalResize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sid = event.drag(cx).0;
        let Some(slot) = self.tiles.slots.get_mut(&sid) else {
            return;
        };
        let Some((anchor_y, anchor_h)) = slot.terminal_drag_anchor else {
            return;
        };
        let dy = anchor_y - f32::from(event.event.position.y);
        let area_h = f32::from(slot.area.get().size.height);
        slot.terminal_tween = None; // live drag tracks the pointer
        let height = clamp_terminal_height(anchor_h + dy, area_h);
        slot.terminal_fraction = super::dock::fraction_of(height, area_h);
        self.remember_slot_docks(sid, cx);
        cx.notify();
    }

    /// Terminal panel dock along the session area's bottom (under the chat
    /// column AND the right dock): a 5px height-drag handle
    /// over the panel, the whole container height-animated 200 ms on toggle.
    pub(super) fn render_terminal_container(
        &mut self,
        sid: SlotId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(slot) = self.tiles.slots.get(&sid) else {
            return gpui::Empty.into_any_element();
        };
        let target = self.terminal_target(slot);
        let tween = slot.terminal_tween;
        if target <= 0.0 && tween.is_none() {
            return gpui::Empty.into_any_element();
        }
        // Defensive: an open flag needs its entity (and set_open) even if
        // toggle_terminal never created one.
        if slot.terminal_open
            && slot.terminal.is_none()
            && let Some(panel) = self.terminal_panel(sid, cx)
        {
            panel.update(cx, |panel, cx| panel.set_open(true, cx));
        }
        let Some(slot) = self.tiles.slots.get(&sid) else {
            return gpui::Empty.into_any_element();
        };
        let Some(panel) = slot.terminal.clone() else {
            return gpui::Empty.into_any_element();
        };
        let handle_hover = Theme::of(cx).border_strong;
        let height = self.terminal_height(slot);

        let handle = div()
            .id("terminal-resize")
            .h(px(5.0))
            .w_full()
            .flex_none()
            .cursor_row_resize()
            .hover(move |s| s.bg(handle_hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, _| {
                    let Some(height) = this.tiles.slots.get(&sid).map(|s| this.terminal_height(s))
                    else {
                        return;
                    };
                    if let Some(slot) = this.tiles.slots.get_mut(&sid) {
                        slot.terminal_drag_anchor = Some((f32::from(event.position.y), height));
                    }
                }),
            )
            .on_drag(
                TerminalResize(sid),
                |_, _point: Point<gpui::Pixels>, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| DragGhost)
                },
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                    if event.click_count == 2
                        && let Some(slot) = this.tiles.slots.get_mut(&sid)
                    {
                        let area_h = f32::from(slot.area.get().size.height);
                        slot.terminal_fraction =
                            super::dock::fraction_of(TERMINAL_DEFAULT_HEIGHT, area_h);
                        this.remember_slot_docks(sid, cx);
                        cx.notify();
                    }
                }),
            );

        // Fixed-height inner clipped by the animated container: content never
        // reflows mid-transition (same trick as the side panes). The handle
        // FLOATS over the panel's top edge (painted after, so it wins hit
        // testing) instead of stacking above it — stacked, its 5px read as
        // dead air between the seam and the tab bar (user report).
        let inner = div()
            .h(px(height))
            .w_full()
            .relative()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(panel))
            .child(handle.absolute().top_0().left_0().right_0());

        // A divider line above the terminal separates it from the chat,
        // stopping short of the tile edges; painted after the panel so its
        // fill doesn't cover it.
        div()
            .w_full()
            .flex_none()
            .overflow_hidden()
            .relative()
            .h(px(self.eval_tween(tween, target)))
            .child(inner)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(DIVIDER_INSET))
                    .right(px(DIVIDER_INSET))
                    .h(px(1.0))
                    .bg(Theme::of(cx).border),
            )
            .into_any_element()
    }
}
