//! Pointer input on the grid: selection drags, clearing selections, the
//! comment popup and copying the selection.

use super::*;

impl TerminalPanel {
    /// Window position → grid point, using this frame's placement. `None`
    /// before the first prepaint, or when no tab is active.
    fn grid_point_at(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &App,
    ) -> Option<(GridPoint, Side)> {
        let geometry = self.geometry?;
        let hit = cell_at(
            f32::from(position.x - geometry.origin.x),
            f32::from(position.y - geometry.origin.y),
            geometry.cell_w,
            geometry.line_h,
            geometry.cols as usize,
            geometry.rows as usize,
        );
        let point = self.with_active_emulator(cx, |emu| emu.grid_point(hit.row, hit.col))?;
        Some((point, hit.side))
    }

    pub(super) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        // Any new press invalidates the previous floating pill — a new
        // selection gesture closes ANY surface's offer (only one floating UI
        // may exist) without clearing the just-started selection.
        self.dismiss_any_popup(cx);
        let Some((point, side)) = self.grid_point_at(event.position, cx) else {
            return;
        };
        // Click count picks the granularity, the same mapping every terminal
        // uses: drag, word, line.
        let ty = match event.click_count {
            0 => return,
            1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        let shift = event.modifiers.shift;
        if ty == SelectionType::Simple {
            // Shift+click extends an existing selection instead of replacing
            // it — the one gesture that reaches text off the bottom of a long
            // drag without redoing the whole thing.
            let extended = shift
                && self
                    .with_active_emulator(cx, |emu| {
                        let extend = emu.has_selection();
                        if extend {
                            emu.update_selection(point, side);
                        }
                        extend
                    })
                    .unwrap_or(false);
            if extended {
                self.selection_drag = Some(SelectionDrag {
                    origin: event.position,
                    armed: true,
                });
                cx.notify();
                return;
            }
            // A plain press clears and arms; the selection itself only begins
            // once the pointer travels far enough to mean it.
            self.with_active_emulator(cx, |emu| emu.clear_selection());
            self.selection_drag = Some(SelectionDrag {
                origin: event.position,
                armed: false,
            });
        } else {
            // Word and line selections are complete on the press, so they need
            // no threshold — but keep the drag live so the pointer can extend
            // them at that granularity.
            self.with_active_emulator(cx, |emu| emu.start_selection(ty, point, side));
            self.selection_drag = Some(SelectionDrag {
                origin: event.position,
                armed: true,
            });
        }
        cx.notify();
    }

    pub(super) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        let Some(drag) = self.selection_drag else {
            return;
        };
        if !drag.armed {
            let dx = f32::from(event.position.x - drag.origin.x);
            let dy = f32::from(event.position.y - drag.origin.y);
            if dx.hypot(dy) < SELECTION_DRAG_THRESHOLD {
                return;
            }
            // Threshold tripped: anchor at the *press*, not here, so the
            // selection covers the whole gesture.
            let Some((anchor, side)) = self.grid_point_at(drag.origin, cx) else {
                return;
            };
            self.with_active_emulator(cx, |emu| {
                emu.start_selection(SelectionType::Simple, anchor, side)
            });
            self.selection_drag = Some(SelectionDrag {
                armed: true,
                ..drag
            });
        }
        let Some((point, side)) = self.grid_point_at(event.position, cx) else {
            return;
        };
        self.with_active_emulator(cx, |emu| emu.update_selection(point, side));
        cx.notify();
    }

    pub(super) fn on_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selection_drag = None;
        // The chat selected at SETTLE time — captured so a switch before Save
        // can't misroute the comment.
        let Some(chat_id) = self.selected_chat(cx) else {
            return;
        };
        let Some((tab_key, tab_title)) = self
            .active_tab(cx)
            .map(|tab| (tab.key, Self::display_title(tab).to_string()))
        else {
            return;
        };
        // A settled non-empty selection offers its text to the shared Comment
        // pill (native `Emulator::selection_text()` — the emulator keeps the
        // selection, so Cmd+C still copies exactly what was dragged). The
        // Side Chat action carries the tab's display title as its source
        // metadata.
        if let Some(text) = self
            .with_active_emulator(cx, |emu| emu.selection_text())
            .flatten()
            && !text.trim().is_empty()
            && let Some(popup) = self.comment_popup.upgrade()
        {
            let clear = {
                let panel = cx.weak_entity();
                let clear_chat = chat_id.clone();
                Rc::new(move |cx: &mut gpui::App| {
                    panel
                        .update(cx, |panel, cx| {
                            panel.clear_tab_selection(&clear_chat, tab_key, cx)
                        })
                        .ok();
                })
            };
            let owner = self.comment_owner;
            popup.update(cx, |popup, cx| {
                popup.offer(
                    chat_id,
                    text,
                    // Terminal text is never a displayed translation.
                    None,
                    event.position,
                    owner,
                    None,
                    clear,
                    Some(cypher_proto::SideChatSource::Terminal {
                        title: Some(tab_title),
                    }),
                    cx,
                );
            });
        }
    }

    /// Drop the active tab's selection wash (comment saved/cancelled, or a
    /// lifecycle dismissal that invalidates the quote).
    pub(super) fn clear_active_selection(&mut self, cx: &mut Context<Self>) {
        self.with_active_emulator(cx, |emu| emu.clear_selection());
        cx.notify();
    }

    pub(super) fn clear_chat_active_selection(&mut self, chat: &str, cx: &mut Context<Self>) {
        let Some(tabs) = self.chats.get_mut(chat) else {
            return;
        };
        let active = tabs.active;
        if let Some(tab) = tabs.tabs.get_mut(active) {
            tab.emulator.clear_selection();
            cx.notify();
        }
    }

    fn clear_tab_selection(&mut self, chat: &str, key: u64, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_mut(chat, key) {
            tab.emulator.clear_selection();
            cx.notify();
        }
    }

    /// Lifecycle dismissal of the shared popup, scoped to THIS panel's
    /// terminal offers only (scroll/tab/close/chat/resize/output never hide
    /// the transcript's or a diff pane's pill). Public for the shell
    /// (surface switches).
    pub fn dismiss_popup(&mut self, cx: &mut Context<Self>) {
        let owner = self.comment_owner;
        if let Some(popup) = self.comment_popup.upgrade() {
            // Terminal lifecycle methods already hold this panel's mutable
            // entity lease. Invoking the popup's cleanup callback here would
            // recursively update this same panel and double-lease it.
            popup.update(cx, |popup, cx| popup.dismiss_if_owner(owner, cx));
        }
    }

    /// Shell-driven surface detach: dismiss this panel's popup and clear the
    /// currently visible terminal selection under the existing panel lease.
    pub fn detach_comment_selection(&mut self, cx: &mut Context<Self>) {
        self.dismiss_popup(cx);
        self.clear_active_selection(cx);
    }

    /// A new gesture in THIS panel closes ANY surface's floating pill — only
    /// one floating UI may exist — without clearing the just-started
    /// selection.
    fn dismiss_any_popup(&mut self, cx: &mut Context<Self>) {
        let owner = self.comment_owner;
        if let Some(popup) = self.comment_popup.upgrade() {
            popup.update(cx, |popup, cx| popup.selection_started(owner, cx));
        }
    }

    /// Ongoing terminal output stales the quoted endpoint: dismiss THIS
    /// panel's OFFER pill only — an open editor and its draft survive.
    pub(super) fn dismiss_offer_popup(&mut self, cx: &mut Context<Self>) {
        let owner = self.comment_owner;
        if let Some(popup) = self.comment_popup.upgrade() {
            popup.update(cx, |popup, cx| popup.dismiss_offer_if_owner(owner, cx));
        }
    }

    /// Copy the selection. Returns whether anything was copied, so the caller
    /// can decide whether to swallow the keystroke.
    pub(super) fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(text) = self
            .with_active_emulator(cx, |emu| emu.selection_text())
            .flatten()
        else {
            return false;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        true
    }

    pub(super) fn copy_surface_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(text) = crate::markdown::selection::selected_text() else {
            return false;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        true
    }
}
