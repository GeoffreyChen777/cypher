//! The Botboard's workspace column: the current bot's conversation as one
//! card, and the detail panel sliding in beside it. The card renders the
//! board slot with the session machinery a tile uses, under a header naming
//! the bot instead of a tab strip; there is no close button and no rail
//! (a bot chat has no terminal, dock or split to offer).

use super::super::workspace_view::TILE_HEADER_HEIGHT;
use super::*;

/// The gap between the detail panel and the chat card (the panel's resize
/// handle sits in it), and the narrowest the chat gets beside the panel.
const PANEL_GAP: f32 = 8.0;
const CHAT_MIN_W: f32 = 380.0;

/// The drag payload of the panel's resize handle.
pub(in crate::shell) struct BotPanelResize;

impl Shell {
    /// The workspace column on the Botboard (see the module docs).
    pub(in crate::shell) fn render_botboard(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chat = self.render_board_chat(window, cx);
        let panel = self.render_panel_column(window, cx);
        div()
            .id("botboard")
            .flex_1()
            .min_w_0()
            .min_h_0()
            // The workspace's insets: 8px on the window edges, the seam
            // gutter beside the sidebar.
            .ml(px(self.workspace_left_inset()))
            .mt(px(PANEL_EDGE_INSET))
            .mb(px(PANEL_EDGE_INSET))
            .mr(px(PANEL_EDGE_INSET))
            .flex()
            .flex_row()
            .child(chat)
            .children(panel)
            .into_any_element()
    }

    /// The conversation card: the bot's header over its session area, or the
    /// empty state with no bots.
    fn render_board_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let header = self.render_board_header(&theme, cx);
        let body = match self.board.slot {
            Some(sid) => self.render_session(sid, window, cx),
            None => self.render_board_placeholder(&theme, cx),
        };
        let card_bg = crate::appearance::chat_style::panel_background(
            crate::appearance::chat_style::settings(cx),
            Theme::of(cx),
            true,
        );
        div()
            .id("botboard-chat")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(PANEL_CORNER_RADIUS))
            .bg(card_bg)
            .shadow_sm()
            .overflow_hidden()
            .child(header)
            .child(div().flex_1().min_h_0().relative().flex().child(body))
            .into_any_element()
    }

    /// The card's header: the bot's avatar and name where a tile has its
    /// tabs, and the window's drag region like any top-row tile header. It
    /// clears the window controls while the sidebar is collapsed.
    fn render_board_header(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let bot = {
            let state = self.state.read(cx);
            self.settings
                .botboard_bot
                .as_deref()
                .and_then(|id| state.bots.iter().find(|b| b.id == id))
                .cloned()
        };
        let sidebar_now = self.eval_tween(self.motion.sidebar_tween, self.sidebar_target());
        let card_left = sidebar_now + self.workspace_left_inset();
        let left = (self.title_bar_content_start() - card_left).max(14.0);
        let header = div()
            .flex_none()
            .h(px(TILE_HEADER_HEIGHT))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .pl(px(left))
            .pr(px(titlebar_right_padding(
                cfg!(target_os = "windows"),
                12.0,
            )))
            .when_some(bot, |el, bot| {
                el.child(crate::appearance::bot_style::bot_avatar(
                    &bot.name,
                    bot.icon.as_deref(),
                    bot.color.as_deref(),
                    18.0,
                    theme,
                ))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.5))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text.opacity(0.9))
                        .child(SharedString::from(transcript::single_line(&bot.name))),
                )
                .child(div().flex_1())
                .child(self.render_board_actions(&bot, theme, cx))
            });
        self.titlebar_drag_region("botboard-header", header, cx)
            .into_any_element()
    }

    /// The bot's own actions where a tile keeps its own: the activity log,
    /// pause (or resume) and settings. A press here never starts the window
    /// drag the header is.
    fn render_board_actions(&self, bot: &Bot, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let edit_id = bot.id.clone();
        let settings = super::items::icon_button(
            "bot-header-settings",
            icons::TUNING,
            "Workspace settings",
            theme,
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(
            cx.listener(move |this, _, _, cx| this.open_bot_dialog(Some(edit_id.clone()), cx)),
        );
        let activity = super::items::icon_button(
            "bot-header-activity",
            icons::PULSE,
            "Activity: everything it did",
            theme,
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(|this, _, _, cx| this.open_panel(PanelItem::Activity, cx)));
        let (glyph, tip) = if bot.paused {
            (icons::PLAY, "Resume: let it run routines and start work")
        } else {
            (icons::PAUSE, "Pause: no routines run and no work starts")
        };
        let (pause_id, paused) = (bot.id.clone(), bot.paused);
        let pause = super::items::icon_button("bot-header-pause", glyph, tip, theme)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.mutate(
                    serde_json::json!({
                        "op": "setBotPaused", "botId": pause_id, "paused": !paused,
                    }),
                    cx,
                )
            }));
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .child(activity)
            .child(pause)
            .child(settings)
            .into_any_element()
    }

    /// No conversation to show: the empty state once the bots frame says
    /// there are none, else nothing while it loads.
    fn render_board_placeholder(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let empty = {
            let state = self.state.read(cx);
            state.bots_synced && state.bots.is_empty()
        };
        if !empty {
            return div().size_full().into_any_element();
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .child(
                div()
                    .size(px(56.0))
                    .rounded(px(15.0))
                    .bg(crate::kit::theme::ink(0.06))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        icon(icons::BOT)
                            .size(px(26.0))
                            .text_color(theme.text_muted),
                    ),
            )
            .child(
                div()
                    .mt(px(4.0))
                    .text_size(px(15.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from("No workspaces yet")),
            )
            .child(
                div()
                    .max_w(px(340.0))
                    .text_center()
                    .text_size(px(13.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(
                        "A workspace looks after the projects you give it: it answers questions, plans work and runs it.",
                    )),
            )
            .child(
                popover::btn_primary(theme, "New workspace")
                    .id("botboard-empty-new-bot")
                    .mt(px(6.0))
                    .on_click(cx.listener(|this, _, _, cx| this.open_bot_dialog(None, cx))),
            )
            .into_any_element()
    }

    /// The panel's animated container: it grows from the column's right
    /// edge as the chat narrows, and clips a fixed-width card so the content
    /// never reflows mid-slide. `None` once shut.
    fn render_panel_column(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let span = self.panel_span_in(window);
        let target = if self.board.panel.is_some() {
            span
        } else {
            0.0
        };
        let width = self.eval_tween(self.board.panel_tween, target).min(span);
        if width < 0.5 && self.board.panel.is_none() {
            self.board.panel_last = None;
            self.board.panel_tween = None;
            self.drop_panel_extras();
            return None;
        }
        let item = self
            .board
            .panel
            .clone()
            .or_else(|| self.board.panel_last.clone())?;
        // An item that no longer resolves (its record went away) slides the
        // panel shut, empty, instead of leaving the chat narrowed.
        let card = match self.render_panel_card(&item, window, cx) {
            Some(card) => card,
            None => {
                if self.board.panel.as_ref() == Some(&item) {
                    self.close_panel(cx);
                }
                let card_bg = crate::appearance::chat_style::panel_background(
                    crate::appearance::chat_style::settings(cx),
                    Theme::of(cx),
                    true,
                );
                div()
                    .size_full()
                    .rounded(px(PANEL_CORNER_RADIUS))
                    .bg(card_bg)
                    .shadow_sm()
                    .into_any_element()
            }
        };
        // The handle is the gap itself, drawn like the gap between split
        // tiles: a faint wash on hover. A drag resizes, a double click
        // resets.
        let wash = Theme::of(cx).border_strong.opacity(0.5);
        let handle = self
            .resize_handle(
                "bot-panel-resize",
                || BotPanelResize,
                |shell, _| {
                    shell.settings.bot_panel_width = crate::prefs::BOT_PANEL_DEFAULT;
                    shell.board.panel_tween = None;
                },
                cx,
            )
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(px(PANEL_GAP))
            .child(
                div()
                    .size_full()
                    .rounded(px(2.0))
                    .hover(move |s| s.bg(wash)),
            );
        Some(
            div()
                .h_full()
                .flex_none()
                .overflow_hidden()
                .w(px(width))
                .child(
                    div()
                        .relative()
                        .w(px(span))
                        .h_full()
                        .pl(px(PANEL_GAP))
                        .child(card)
                        .when(self.board.panel.is_some(), |el| el.child(handle)),
                )
                .into_any_element(),
        )
    }

    /// The panel and its gap as stored, for the open and close tweens.
    pub(super) fn panel_span(&self) -> f32 {
        self.settings.bot_panel_width + PANEL_GAP
    }

    /// The panel and its gap as drawn: the stored width, less what the chat
    /// needs to keep its minimum in this window.
    fn panel_span_in(&self, window: &Window) -> f32 {
        let sidebar = if self.settings.sidebar_collapsed {
            0.0
        } else {
            self.settings.sidebar_width
        };
        let room = f32::from(window.viewport_size().width)
            - sidebar
            - self.workspace_left_inset()
            - PANEL_EDGE_INSET
            - CHAT_MIN_W;
        self.panel_span()
            .min(room.max(crate::prefs::BOT_PANEL_MIN + PANEL_GAP))
    }

    /// Dragging the handle: the panel's left edge follows the pointer.
    pub(in crate::shell) fn on_bot_panel_drag(
        &mut self,
        event: &gpui::DragMoveEvent<BotPanelResize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let right = f32::from(window.viewport_size().width) - PANEL_EDGE_INSET;
        let width = right - f32::from(event.event.position.x) - PANEL_GAP / 2.0;
        self.settings.bot_panel_width =
            width.clamp(crate::prefs::BOT_PANEL_MIN, crate::prefs::BOT_PANEL_MAX);
        self.board.panel_tween = None;
        self.schedule_save(cx);
        cx.notify();
    }
}
