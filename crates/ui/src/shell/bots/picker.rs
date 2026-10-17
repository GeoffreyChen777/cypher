//! The bot picker heading the Botboard sidebar: one avatar square per bot,
//! the current one ringed in its colour, then a dashed `+` for a new bot. A
//! click makes that bot current; a right-click opens its menu (Open /
//! Settings… / Delete…). A dot in a tile's corner carries its conversation's
//! status, like a session row's.

use super::*;

/// Avatar square size, the selection ring around it (ring width + its gap
/// to the square), and the gap between tiles.
const TILE: f32 = 30.0;
const RING: f32 = 3.0;
const GAP: f32 = 4.0;

impl Shell {
    /// The picker row; `None` before the first bots frame. With no bots it
    /// is the `+` alone.
    pub(in crate::shell) fn render_bot_picker(
        &mut self,
        current: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = Utc::now();
        let tiles: Vec<(Bot, Option<ChatIndicator>)> = {
            let state = self.state.read(cx);
            if !state.bots_synced {
                return None;
            }
            state
                .bots
                .iter()
                .map(|bot| {
                    let chat = state.chats.iter().find(|c| c.id == bot.chat_id);
                    (bot.clone(), chat.map(|c| state.display_status_for(c, now)))
                })
                .collect()
        };
        let tiles: Vec<AnyElement> = tiles
            .into_iter()
            .map(|(bot, status)| {
                let selected = current == Some(bot.id.as_str());
                self.render_bot_tile(bot, status, selected, theme, cx)
            })
            .collect();
        let add_key = "bot-picker-add";
        let add = div()
            .id(add_key)
            .flex_none()
            .size(px(TILE + 2.0 * RING))
            .p(px(RING))
            .cursor_pointer()
            .tooltip(|_, cx| {
                cx.new(|_| super::super::session::FindTooltip("New bot".into()))
                    .into()
            })
            .on_hover(motion::hover_listener(add_key))
            .on_click(cx.listener(|this, _, _, cx| this.open_bot_dialog(None, cx)))
            .child(
                div()
                    .size(px(TILE))
                    .rounded(px((TILE * 0.27).round()))
                    .border_1()
                    .border_dashed()
                    .border_color(motion::hover_blend(
                        add_key,
                        theme.text_muted.opacity(0.35),
                        theme.text_muted.opacity(0.7),
                    ))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        icon(icons::PLUS)
                            .size(px(13.0))
                            .text_color(motion::hover_blend(
                                add_key,
                                theme.text_muted.opacity(0.75),
                                theme.text,
                            )),
                    ),
            );
        Some(
            div()
                .id("bot-picker")
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(GAP))
                // The first tile's ring lines up with the cards' edges
                // below.
                .pb(px(4.0))
                .children(tiles)
                .child(add)
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
        let menu_open = self.menus.bot.get().is_some_and(|(id, _)| *id == bot.id);
        let (open_id, menu_id) = (bot.id.clone(), bot.id.clone());
        // The current bot is ringed (the workspace-switcher convention): a
        // 1.5px outline 1.5px off the square, in the bot's own colour.
        let ring = crate::appearance::space_style::space_color(bot.color.as_deref(), theme)
            .unwrap_or(theme.text_muted);
        div()
            .id(SharedString::from(key.clone()))
            .relative()
            .flex_none()
            .size(px(TILE + 2.0 * RING))
            .p(px(RING - 1.5))
            .rounded(px((TILE * 0.27).round() + RING))
            .border(px(1.5))
            .border_color(if selected {
                ring.opacity(0.55)
            } else {
                gpui::transparent_black()
            })
            .cursor_pointer()
            .when(!selected, |el| el.hover(|s| s.opacity(0.85)))
            // No name over the tile's own open menu.
            .when(!menu_open, |el| {
                el.tooltip(move |_, cx| {
                    cx.new(|_| super::session::FindTooltip(name.clone())).into()
                })
            })
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
                        .top(px(0.0))
                        .right(px(0.0))
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
