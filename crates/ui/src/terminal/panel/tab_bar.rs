//! The tab bar: tab buttons with drag-reorder, the drag ghost and the new
//! tab button.

use super::*;

struct TabGhost {
    title: SharedString,
}

impl Render for TabGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .w(px(TAB_WIDTH))
            .h(px(TAB_HEIGHT))
            .px(px(Theme::SPACE_SM))
            .flex()
            .items_center()
            .rounded(px(Theme::CONTROL_RADIUS))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border_strong)
            .text_size(px(12.0))
            .text_color(theme.text)
            .opacity(0.85)
            .child(div().truncate().child(self.title.clone()))
    }
}

impl TerminalPanel {
    // ---- render ----

    pub(super) fn render_tab_bar(
        &mut self,
        chat: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // Hover-fade keys are global: scope them to this panel (one per
        // session tile).
        let panel_id = cx.entity_id();
        let theme = crate::appearance::surface_style::theme(
            crate::appearance::surface_style::Region::Terminal,
            cx,
        );
        let tabs = self.chats.get(chat);
        let (active, count) = tabs.map(|t| (t.active, t.tabs.len())).unwrap_or((0, 0));
        let drag = self
            .drag
            .as_ref()
            .map(|d| (d.from, d.over, d.epoch, d.prev_over));
        let chat_owned = chat.to_string();

        let tab_elements: Vec<_> = tabs
            .map(|tabs| {
                tabs.tabs
                    .iter()
                    .enumerate()
                    .map(|(ix, tab)| {
                        let selected = ix == active;
                        let key = tab.key;
                        // Contextual label (user request): the OSC title —
                        // the shell's own cwd/command name — wins over the
                        // fixed "Terminal N" fallback.
                        let title = Self::display_title(tab);
                        let exited = tab.exited.is_some();
                        TabView {
                            ix,
                            key,
                            title,
                            selected,
                            exited,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let bar_chat = chat_owned.clone();
        let drop_chat = chat_owned.clone();
        // Zeron terminal-panel.tsx: `flex h-10 items-center pl-2 pr-1.5` on
        // the #191919 panel — no separate bar fill, and no hairline under
        // the tabs (user request: the tile's own tab row has none either).
        div()
            .id("terminal-tab-bar")
            .h(px(TAB_BAR_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            // Same left inset as the tile's session tab row above, so the
            // two strips' first tabs line up.
            .pl(px(6.0))
            .pr(px(6.0))
            .on_drag_move::<TabDragPayload>(cx.listener(
                move |this, event: &gpui::DragMoveEvent<TabDragPayload>, _, cx| {
                    let payload = event.drag(cx);
                    if payload.chat != bar_chat {
                        return;
                    }
                    let from = payload.from;
                    let rel_x = f32::from(event.event.position.x) - f32::from(event.bounds.left());
                    let over = drop_index(rel_x, TAB_WIDTH, count);
                    this.update_drag_over(from, over, cx);
                },
            ))
            .on_drop::<TabDragPayload>(cx.listener(move |this, payload: &TabDragPayload, _, cx| {
                if payload.chat != drop_chat {
                    this.drag = None;
                    cx.notify();
                    return;
                }
                let to = this.drag.as_ref().map(|d| d.over).unwrap_or(payload.from);
                let chat = drop_chat.clone();
                this.commit_reorder(&chat, payload.from, to, cx);
            }))
            .children(tab_elements.into_iter().map(|tab| {
                let ix = tab.ix;
                let key = tab.key;
                let tab_el = render_tab_button(&chat_owned, tab, panel_id, &theme, cx);
                slide_tab(tab_el, ix, key, drag)
            }))
            .child(new_tab_button(panel_id, &theme, cx))
            // Collapse chevron pinned right (zeron "Hide terminal" ⌘J).
            .child(div().flex_1())
            .child(collapse_button(panel_id, &theme))
    }
}

/// One tab's paint inputs.
struct TabView {
    ix: usize,
    key: u64,
    title: SharedString,
    selected: bool,
    exited: bool,
}

/// One terminal tab (zeron tab: `h-7 rounded-lg pl-2 pr-1 gap-1.5 text-xs`,
/// terminal glyph + label + close; active = white/8 wash): selects on click,
/// closes on middle-click or its ✕, drags to reorder.
fn render_tab_button(
    chat: &str,
    tab: TabView,
    panel_id: gpui::EntityId,
    theme: &Theme,
    cx: &mut Context<TerminalPanel>,
) -> gpui::Stateful<gpui::Div> {
    let TabView {
        ix,
        key,
        title,
        selected,
        exited,
    } = tab;
    let chat_select = chat.to_string();
    let chat_close = chat.to_string();
    let chat_close2 = chat.to_string();
    let chat_drag = chat.to_string();
    let ghost_title = title.clone();
    let (text_color, bg, glyph_alpha) = if selected {
        (theme.text, crate::kit::theme::ink(0.08), 0.8)
    } else {
        (
            theme.text_muted.opacity(0.6),
            gpui::transparent_black(),
            0.6,
        )
    };
    let close_btn = div()
        .id(("terminal-tab-close", key))
        .size(px(20.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .when(!selected, |el| el.invisible())
        .cursor_pointer()
        .hover(|s| s.bg(crate::kit::theme::ink(0.09)))
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.close_tab(&chat_close2, key, window, cx);
        }))
        .child(
            crate::kit::icons::icon(crate::kit::icons::CLOSE)
                .size(px(12.0))
                .text_color(theme.text_muted.opacity(0.8)),
        );
    div()
        .id(("terminal-tab", key))
        .w(px(TAB_WIDTH))
        .h(px(TAB_HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .pl(px(8.0))
        .pr(px(4.0))
        .rounded(px(8.0))
        // zeron terminal-panel.tsx tab: `transition-colors`.
        .bg(motion::hover_blend(
            &format!("term-tab-{panel_id}-{key}"),
            bg,
            theme.element_hover,
        ))
        .on_hover(motion::hover_listener(format!("term-tab-{panel_id}-{key}")))
        .text_size(px(12.0))
        .text_color(text_color)
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select_tab(&chat_select, ix, cx);
        }))
        // Middle-click closes.
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |this, _, window, cx| {
                this.close_tab(&chat_close, key, window, cx);
            }),
        )
        .on_drag(
            TabDragPayload {
                chat: chat_drag,
                from: ix,
                title: ghost_title,
            },
            |payload, _point, _, cx| {
                let title = payload.title.clone();
                cx.stop_propagation();
                cx.new(|_| TabGhost { title })
            },
        )
        .when(exited, |el| el.opacity(0.55))
        .child(
            crate::kit::icons::icon(crate::kit::icons::TERMINAL)
                .size(px(16.0))
                .text_color(text_color.opacity(glyph_alpha)),
        )
        .child(div().flex_1().min_w_0().truncate().child(title))
        .child(close_btn)
}

/// Sliding transform while a sibling is dragged over: animate 150 ms between
/// committed offsets.
fn slide_tab(
    tab_el: gpui::Stateful<gpui::Div>,
    ix: usize,
    key: u64,
    drag: Option<(usize, usize, usize, usize)>,
) -> gpui::AnyElement {
    match drag {
        Some((from, over, epoch, prev_over)) if ix != from => {
            let target = slide_offset(ix, from, over) * TAB_WIDTH;
            let start = slide_offset(ix, from, prev_over) * TAB_WIDTH;
            div()
                .relative()
                .child(tab_el.with_animation(
                    ("terminal-tab-slide", key | ((epoch as u64) << 32)),
                    TAB_SLIDE.animation(),
                    move |el, t| el.left(px(motion::lerp(start, target, t))),
                ))
                .into_any_element()
        }
        // Invisible spacer — the ghost carries the tab; a
        // dimmed original overlapped the sibling that
        // slides into the vacated slot.
        Some((from, ..)) if ix == from => div()
            .w(px(TAB_WIDTH))
            .h(px(TAB_HEIGHT))
            .flex_none()
            .into_any_element(),
        _ => tab_el.into_any_element(),
    }
}

/// The `+` new-tab button.
fn new_tab_button(
    panel_id: gpui::EntityId,
    theme: &Theme,
    cx: &mut Context<TerminalPanel>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id("terminal-new-tab")
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .cursor_pointer()
        // zeron terminal-panel.tsx icon buttons: `transition-colors`.
        .bg(motion::hover_blend(
            &format!("term-new-tab-{panel_id}"),
            gpui::transparent_black(),
            crate::kit::theme::ink(0.05),
        ))
        .on_hover(motion::hover_listener(format!("term-new-tab-{panel_id}")))
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(chat) = this.selected_chat(cx) {
                this.open_tab(chat, cx);
            }
        }))
        .child(
            crate::kit::icons::icon(crate::kit::icons::PLUS)
                .size(px(16.0))
                .text_color(theme.text_muted.opacity(0.6)),
        )
}

/// The collapse chevron (zeron "Hide terminal" ⌘J).
fn collapse_button(panel_id: gpui::EntityId, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id("terminal-collapse")
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .cursor_pointer()
        .bg(motion::hover_blend(
            &format!("term-collapse-{panel_id}"),
            gpui::transparent_black(),
            crate::kit::theme::ink(0.05),
        ))
        .on_hover(motion::hover_listener(format!("term-collapse-{panel_id}")))
        .on_click(|_, window, cx| {
            window.dispatch_action(Box::new(ToggleTerminal), cx);
        })
        .child(
            crate::kit::icons::icon(crate::kit::icons::ALT_ARROW_DOWN)
                .size(px(13.0))
                .text_color(theme.text_muted.opacity(0.55)),
        )
}
