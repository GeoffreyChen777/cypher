//! The Botboard's workspace column: the current bot's conversation as one
//! card, and the detail panel sliding in beside it. The card renders the
//! board slot with the session machinery a tile uses, under a header naming
//! the bot instead of a tab strip; there is no close button and no rail
//! (a bot chat has no terminal, dock or split to offer).

use super::super::workspace_view::TILE_HEADER_HEIGHT;
use super::*;

/// The detail panel's width, the gap between it and the chat card, and the
/// width the two take from the column.
const PANEL_W: f32 = 420.0;
const PANEL_GAP: f32 = 8.0;
pub(super) const PANEL_SPAN: f32 = PANEL_W + PANEL_GAP;

impl Shell {
    /// The workspace column on the Botboard (see the module docs).
    pub(in crate::shell) fn render_botboard(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chat = self.render_board_chat(window, cx);
        let panel = self.render_panel_column(cx);
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
            });
        self.titlebar_drag_region("botboard-header", header, cx)
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
                    .child(SharedString::from("No bots yet")),
            )
            .child(
                div()
                    .max_w(px(340.0))
                    .text_center()
                    .text_size(px(13.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(
                        "A bot reads the projects you give it and answers questions about your work.",
                    )),
            )
            .child(
                popover::btn_primary(theme, "New bot")
                    .id("botboard-empty-new-bot")
                    .mt(px(6.0))
                    .on_click(cx.listener(|this, _, _, cx| this.open_bot_dialog(None, cx))),
            )
            .into_any_element()
    }

    /// The panel's animated container: it grows from the column's right
    /// edge as the chat narrows, and clips a fixed-width card so the content
    /// never reflows mid-slide. `None` once shut.
    fn render_panel_column(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let target = if self.board.panel.is_some() {
            PANEL_SPAN
        } else {
            0.0
        };
        let width = self.eval_tween(self.board.panel_tween, target);
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
        let card = match self.render_panel_card(&item, cx) {
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
        Some(
            div()
                .h_full()
                .flex_none()
                .overflow_hidden()
                .w(px(width))
                .child(
                    div()
                        .w(px(PANEL_SPAN))
                        .h_full()
                        .pl(px(PANEL_GAP))
                        .child(card),
                )
                .into_any_element(),
        )
    }
}
