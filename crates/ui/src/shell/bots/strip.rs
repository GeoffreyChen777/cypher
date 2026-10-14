//! The bot strip: one avatar square per bot (bots are created from the
//! sidebar header's `+` menu). A click opens the bot's conversation; a
//! right-click opens its menu.
//! A dot in the corner carries the conversation's status, like a session
//! row's.

use super::*;

/// Avatar square size and the gap between squares.
const TILE: f32 = 34.0;
const GAP: f32 = 6.0;

impl Shell {
    /// `None` with no bots, and in a project window (bots belong to the main
    /// window, like quick chats).
    pub(in crate::shell) fn render_bot_strip(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.is_project_window() {
            return None;
        }
        let now = Utc::now();
        let tiles: Vec<AnyElement> = {
            let state = self.state.read(cx);
            state
                .bots
                .iter()
                .map(|bot| {
                    let chat = state.chats.iter().find(|c| c.id == bot.chat_id);
                    let status = chat.map(|c| state.display_status_for(c, now));
                    let selected = state.selected_chat.as_deref() == Some(bot.chat_id.as_str());
                    (bot.clone(), status, selected)
                })
                .collect::<Vec<_>>()
        }
        .into_iter()
        .map(|(bot, status, selected)| self.render_bot_tile(bot, status, selected, theme, cx))
        .collect();

        if tiles.is_empty() {
            return None;
        }
        Some(
            div()
                .id("bot-strip")
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(GAP))
                // Line the first square up with the project cards' glyphs.
                .px(px(6.0))
                .pb(px(10.0))
                .children(tiles)
                .into_any_element(),
        )
    }

    fn render_bot_tile(
        &self,
        bot: Bot,
        status: Option<ChatIndicator>,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("bot-tile-{}", bot.id);
        let dot = match status {
            Some(ChatIndicator::Working) => Some(theme.accent),
            Some(ChatIndicator::AwaitingInput) => Some(theme.warning),
            Some(ChatIndicator::Errored) => Some(theme.danger),
            Some(ChatIndicator::Completed) => Some(theme.text),
            Some(ChatIndicator::Idle) | None => None,
        };
        let avatar = crate::appearance::bot_style::bot_avatar(
            &bot.name,
            bot.icon.as_deref(),
            bot.color.as_deref(),
            TILE,
            theme,
        );
        let name: SharedString = bot.name.clone().into();
        let (open_id, menu_id) = (bot.id.clone(), bot.id.clone());
        div()
            .id(SharedString::from(key.clone()))
            .relative()
            .flex_none()
            .size(px(TILE))
            .rounded(px((TILE * 0.27).round() + 2.0))
            .cursor_pointer()
            .when(selected, |el| {
                el.shadow(crate::kit::theme::card_selected_shadows())
            })
            .hover(|s| s.opacity(0.88))
            .tooltip(move |_, cx| cx.new(|_| super::session::FindTooltip(name.clone())).into())
            .on_click(cx.listener(move |this, _, _, cx| this.open_bot(&open_id, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.menus.bot.open((menu_id.clone(), event.position));
                    cx.notify();
                }),
            )
            .child(avatar)
            .when_some(dot, |el, color| {
                el.child(
                    div()
                        .absolute()
                        .top(px(-2.0))
                        .right(px(-2.0))
                        .size(px(9.0))
                        .rounded_full()
                        .border_2()
                        .border_color(theme.bg)
                        .bg(color),
                )
            })
            .into_any_element()
    }
}
