//! Agent bots in the sidebar (docs/design/agent-bots.md §10): the bot strip
//! between the header and the project cards, a bot's context menu, and the
//! create/edit dialog. A bot opens as an ordinary session tab on its chat.
//! Child module of `shell` so it renders straight off `Shell`'s private state.

use super::*;
use cypher_proto::{Bot, BotScope, ProjectGrant};

mod chat;
mod dialog;
mod strip;

pub(super) use dialog::BotDialog;

/// Hosts older than this answer `CreateBot` with an unknown-method error, so
/// the dialog does not offer them.
const BOTS_MIN_VERSION: (u64, u64, u64) = (0, 3, 53);

/// Whether a device can host a bot: a Mac or Linux engine, online, and new
/// enough. The local engine always qualifies (it is running this code).
fn can_host(device: &cypher_proto::Device, local: Option<&str>, online: bool) -> bool {
    if local == Some(device.id.as_str()) {
        return true;
    }
    if matches!(device.platform.as_str(), "ios" | "android") || !online {
        return false;
    }
    device
        .version
        .as_deref()
        .and_then(parse_version)
        .is_some_and(|v| v >= BOTS_MIN_VERSION)
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u64>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

impl Shell {
    fn bot_row(&self, bot_id: &str, cx: &App) -> Option<Bot> {
        self.state
            .read(cx)
            .bots
            .iter()
            .find(|b| b.id == bot_id)
            .cloned()
    }

    fn close_bot_menu(&mut self, cx: &mut Context<Self>) {
        if self.menus.bot.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.menus.bot);
            cx.notify();
        }
    }

    /// Open a bot's conversation as a session tab.
    pub(super) fn open_bot(&mut self, bot_id: &str, cx: &mut Context<Self>) {
        self.close_bot_menu(cx);
        if let Some(bot) = self.bot_row(bot_id, cx) {
            self.open_chat(bot.chat_id, cx);
        }
    }

    fn confirm_delete_bot(&mut self, bot_id: String, cx: &mut Context<Self>) {
        self.dialogs.delete_bot = None;
        self.mutate(
            serde_json::json!({ "op": "deleteBot", "botId": bot_id }),
            cx,
        );
        cx.notify();
    }

    /// The bot menus and dialogs, in paint order.
    pub(super) fn render_bot_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        [
            self.render_new_item_menu(&theme, cx),
            self.render_bot_menu(&theme, cx),
            self.render_bot_dialog(viewport, &theme, window, cx),
            self.render_delete_bot_dialog(viewport, &theme, cx),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub(super) fn close_new_item_menu(&mut self, cx: &mut Context<Self>) {
        if self.menus.new_item.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.menus.new_item);
            cx.notify();
        }
    }

    /// The sidebar header's `+` menu: a new project or a new bot.
    fn render_new_item_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let position = *self.menus.new_item.get()?;
        let closing = self.menus.new_item.closing_since();
        let row = |id: &'static str, glyph: &'static str, label: &'static str| {
            popover::menu_row(theme, false, id)
                .id(id)
                .child(icon(glyph).size(px(16.0)).text_color(theme.text_muted))
                .child(SharedString::from(label))
        };
        let menu = popover::popover_card(theme)
            .w(px(170.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_new_item_menu(cx)))
            .flex()
            .flex_col()
            .child(
                row("new-item-project", icons::FOLDER, "New project…").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.close_new_item_menu(cx);
                        this.open_add_space(cx);
                    },
                )),
            )
            .child(
                row("new-item-bot", icons::BOT, "New bot…").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.close_new_item_menu(cx);
                        this.open_bot_dialog(None, cx);
                    },
                )),
            )
            .into_any_element();
        Some(popover::menu_at("new-item-menu", position, menu, closing))
    }

    /// A bot tile's context menu: open, edit, delete.
    fn render_bot_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (bot_id, position) = self.menus.bot.get().cloned()?;
        let closing = self.menus.bot.closing_since();
        let (open_id, edit_id, delete_id) = (bot_id.clone(), bot_id.clone(), bot_id.clone());
        let menu = popover::popover_card(theme)
            .w(px(170.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_bot_menu(cx)))
            .flex()
            .flex_col()
            .child(
                popover::menu_row(theme, false, format!("bot-menu-open-{bot_id}"))
                    .id("bot-menu-open")
                    .on_click(cx.listener(move |this, _, _, cx| this.open_bot(&open_id, cx)))
                    .child(
                        icon(icons::CHAT_ROUND_LINE)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    )
                    .child(SharedString::from("Open")),
            )
            .child(
                popover::menu_row(theme, false, format!("bot-menu-edit-{bot_id}"))
                    .id("bot-menu-edit")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_bot_menu(cx);
                        this.open_bot_dialog(Some(edit_id.clone()), cx);
                    }))
                    .child(icon(icons::PEN).size(px(16.0)).text_color(theme.text_muted))
                    .child(SharedString::from("Settings…")),
            )
            .child(popover::menu_separator())
            .child(
                popover::menu_row(theme, false, format!("bot-menu-delete-{bot_id}"))
                    .id("bot-menu-delete")
                    .text_color(theme.danger)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_bot_menu(cx);
                        this.dialogs.delete_bot = Some(delete_id.clone());
                        cx.notify();
                    }))
                    .child(
                        icon(icons::TRASH_BIN_MINIMALISTIC)
                            .size(px(16.0))
                            .text_color(theme.danger),
                    )
                    .child(SharedString::from("Delete…")),
            )
            .into_any_element();
        Some(popover::menu_at(
            "bot-context-menu",
            position,
            menu,
            closing,
        ))
    }

    fn render_delete_bot_dialog(
        &self,
        viewport: gpui::Size<Pixels>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let bot_id = self.dialogs.delete_bot.clone()?;
        let name = self
            .bot_row(&bot_id, cx)
            .map(|b| b.name)
            .unwrap_or_else(|| "This bot".into());
        let card = popover::dialog_card(theme)
            .child(popover::dialog_title(theme, "Delete bot?"))
            .child(div().mt(px(6.0)).child(popover::dialog_body(
                theme,
                format!(
                    "\u{201C}{name}\u{201D} and its conversation will be permanently deleted. \
                     Your sessions are not affected."
                ),
            )))
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(theme, "Cancel", "delete-bot-cancel")
                            .id("delete-bot-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dialogs.delete_bot = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_danger(theme, "Delete")
                            .id("delete-bot-confirm")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm_delete_bot(bot_id.clone(), cx)
                            })),
                    ),
            )
            .into_any_element();
        Some(popover::modal("delete-bot-dialog", viewport, card))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, platform: &str, version: Option<&str>) -> cypher_proto::Device {
        cypher_proto::Device {
            id: id.into(),
            name: id.into(),
            platform: platform.into(),
            last_seen_at: None,
            created_at: None,
            version: version.map(Into::into),
        }
    }

    #[test]
    fn hosts_are_desktop_engines_new_enough_and_online() {
        let local = Some("mac");
        assert!(can_host(&device("mac", "macos", None), local, false));
        assert!(can_host(
            &device("vps", "linux", Some("0.3.53")),
            local,
            true
        ));
        assert!(can_host(
            &device("vps", "linux", Some("0.4.0")),
            local,
            true
        ));
        assert!(!can_host(
            &device("vps", "linux", Some("0.3.52")),
            local,
            true
        ));
        assert!(!can_host(&device("vps", "linux", None), local, true));
        assert!(!can_host(
            &device("vps", "linux", Some("0.4.0")),
            local,
            false
        ));
        assert!(!can_host(
            &device("phone", "ios", Some("9.9.9")),
            local,
            true
        ));
    }

    #[test]
    fn versions_parse_loosely() {
        assert_eq!(parse_version("0.3.53"), Some((0, 3, 53)));
        assert_eq!(parse_version("v1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("0.3.53-dev+abc"), Some((0, 3, 53)));
        assert_eq!(parse_version("dev"), None);
    }
}
