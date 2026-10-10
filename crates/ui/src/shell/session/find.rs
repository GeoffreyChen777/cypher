//! Find in chat: opening, stepping and closing the find bar, and rendering
//! it.

use super::*;

impl Shell {
    // ---- in-chat find (⌘F) ----

    /// ⌘F (and Edit → Find in Chat). Opens the find bar over the open
    /// conversation, or — when it is already open — just puts the caret back
    /// in the field with the previous query intact, the way every find bar
    /// behaves. There is nothing to search on the new-chat canvas or in
    /// Settings, so both are no-ops rather than an empty bar.
    pub(in crate::shell) fn open_find(&mut self, sid: SlotId, cx: &mut Context<Self>) {
        let showing_setup = self.showing_setup();
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        if !matches!(self.route, Route::Chat)
            || showing_setup
            || slot.state.read(cx).selected_chat.is_none()
        {
            return;
        }
        let query = slot.find_input.read(cx).text().to_owned();
        slot.transcript.update(cx, |transcript, cx| {
            transcript.open_find(cx);
            // Re-opening with a retained query must re-run it: the index was
            // dropped when find closed.
            transcript.set_find_query(&query, cx);
        });
        slot.find_focus_pending = true;
        cx.notify();
    }

    /// Close the bar and hand the keyboard back to the composer — where it
    /// was before ⌘F, and the only place in the chat route that wants it.
    fn close_find(&mut self, sid: SlotId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get_mut(&sid) else {
            return;
        };
        slot.transcript
            .update(cx, |transcript, cx| transcript.close_find(cx));
        slot.find_focus_pending = false;
        window.focus(&slot.composer.focus_handle(cx), cx);
        cx.notify();
    }

    fn step_find(&mut self, sid: SlotId, delta: isize, cx: &mut Context<Self>) {
        if let Some(slot) = self.slots.get(&sid) {
            slot.transcript
                .update(cx, |transcript, cx| transcript.step_find(delta, cx));
        }
    }

    /// Find-bar keys, bubbling from the focused field ("PaletteSearch" leaves
    /// ↵/⇧↵/↑↓/esc unbound exactly so they arrive here).
    pub(super) fn find_key(
        &mut self,
        sid: SlotId,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => self.close_find(sid, window, cx),
            "enter" => {
                let delta = if event.keystroke.modifiers.shift {
                    -1
                } else {
                    1
                };
                self.step_find(sid, delta, cx);
            }
            "up" => self.step_find(sid, -1, cx),
            "down" => self.step_find(sid, 1, cx),
            _ => {}
        }
    }

    /// The find bar: a floating pill in the conversation column's top-right,
    /// below the titlebar and clear of the message rail (which hugs the left
    /// edge). Rendered by the SHELL rather than inside the transcript for the
    /// same reason as the jump pill — the transcript outlet sits inside the
    /// EdgeFade scope, which would fade the bar out against the top band.
    pub(super) fn render_find_bar(
        &mut self,
        sid: SlotId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let slot = self.slots.get_mut(&sid)?;
        if !slot.transcript.read(cx).find_open() {
            return None;
        }
        if std::mem::take(&mut slot.find_focus_pending) {
            let handle = slot.find_input.focus_handle(cx);
            window.focus(&handle, cx);
        }
        let (find_input, find_focus) = (slot.find_input.clone(), slot.find_focus.clone());
        let theme = Theme::of(cx).clone();
        let (position, total) = slot.transcript.read(cx).find_status();
        let typed = !find_input.read(cx).text().trim().is_empty();
        // Empty field reads as "nothing asked for yet", not "nothing found".
        let counter: SharedString = match (typed, total) {
            (false, _) => "".into(),
            (true, 0) => "No results".into(),
            (true, total) => format!("{position} of {total}").into(),
        };
        let has_matches = total > 0;
        let step_button = |shell_key: &'static str,
                           glyph: &'static str,
                           tooltip: &'static str,
                           delta: isize,
                           cx: &mut Context<Self>| {
            div()
                .id(shell_key)
                .size(px(22.0))
                .flex_none()
                .rounded(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .when(has_matches, |el| {
                    // Hover-fade keys are global: suffix the slot so two
                    // tiles' find bars never fade together.
                    let fade_key = format!("{shell_key}-{sid}");
                    el.cursor_pointer()
                        .bg(motion::hover_blend(
                            &fade_key,
                            gpui::transparent_black(),
                            theme.element_hover,
                        ))
                        .on_hover(motion::hover_listener(fade_key))
                        .on_click(cx.listener(move |this, _, _, cx| this.step_find(sid, delta, cx)))
                })
                .child(
                    icon(glyph)
                        .size(px(12.0))
                        .text_color(if has_matches {
                            theme.text_muted
                        } else {
                            theme.text_faint.opacity(0.5)
                        })
                        .flex_none(),
                )
                .tooltip(move |_, cx| cx.new(|_| FindTooltip(tooltip.into())).into())
        };
        let bar = div()
            .id("find-bar")
            .h(px(34.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pl(px(10.0))
            .pr(px(5.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .shadow_md()
            .track_focus(&find_focus)
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    this.find_key(sid, event, window, cx)
                }),
            )
            .child(
                icon(icons::MAGNIFER)
                    .size(px(13.0))
                    .flex_none()
                    .text_color(theme.text_faint),
            )
            .child(
                div()
                    .w(px(190.0))
                    .flex_none()
                    .text_size(px(13.0))
                    .child(find_input.clone()),
            )
            .child(
                div()
                    // Fixed width so stepping through matches ("9 of 12" →
                    // "10 of 12") never nudges the buttons under the cursor.
                    .w(px(64.0))
                    .flex_none()
                    .text_size(px(11.5))
                    .text_color(theme.text_faint)
                    .truncate()
                    .child(counter),
            )
            .child(div().w(px(1.0)).h(px(16.0)).flex_none().bg(theme.border))
            .child(step_button(
                "find-prev",
                icons::ARROW_UP,
                "Previous match (⇧↵)",
                -1,
                cx,
            ))
            .child(step_button(
                "find-next",
                icons::ARROW_DOWN,
                "Next match (↵)",
                1,
                cx,
            ))
            .child(
                div()
                    .id("find-close")
                    .size(px(22.0))
                    .flex_none()
                    .rounded(px(5.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .bg(motion::hover_blend(
                        &format!("find-close-{sid}"),
                        gpui::transparent_black(),
                        theme.element_hover,
                    ))
                    .on_hover(motion::hover_listener(format!("find-close-{sid}")))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.close_find(sid, window, cx)),
                    )
                    .child(
                        icon(icons::CLOSE)
                            .size(px(11.0))
                            .flex_none()
                            .text_color(theme.text_muted),
                    )
                    .tooltip(|_, cx| cx.new(|_| FindTooltip("Close (esc)".into())).into()),
            );
        Some(
            div()
                .absolute()
                .top(px(8.0))
                .right(px(14.0))
                .child(motion::dialog_in("find-bar-in", bar))
                .into_any_element(),
        )
    }
}
