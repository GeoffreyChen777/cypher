//! Agent bots (docs/design/agent-bots.md §13.1): the main window's Botboard
//! mode, the other half of the sidebar logo's switch. In Botboard the
//! sidebar shows the bot picker ([`picker`]) and the current bot's cards
//! ([`board`]), its shortcuts pinned under them ([`shortcuts`]); the workspace column shows the bot's conversation as one
//! card outside the tile tree, with the detail panel beside it
//! ([`column`], [`panel`]). Bot chats never open as session tabs: whatever
//! would open one switches to the Botboard instead. Also here: a bot's
//! context menu, the create/edit palette ([`dialog`]) and the delete dialog.
//! Child module of `shell` so it renders straight off `Shell`'s private state.

use super::session::SlotId;
use super::*;
use crate::prefs::WindowMode;
use crate::workspace::TabKey;
use cypher_proto::{Bot, BotScope, ProjectGrant};

mod activity;
mod board;
mod chat;
mod column;
mod dialog;
mod items;
pub(super) mod memory;
mod needs;
mod network;
mod panel;
mod parts;
mod picker;
mod projects;
mod schedule;
mod shortcuts;
mod work;

pub(super) use dialog::BotDialog;
pub(super) use panel::PanelItem;

/// Hosts older than this answer `CreateBot` with an unknown-method error, so
/// the dialog does not offer them.
const BOTS_MIN_VERSION: (u64, u64, u64) = (0, 3, 57);

/// The Botboard's own UI state. Everything it shows comes from the main
/// state; this holds only what the board has open.
#[derive(Default)]
pub(super) struct BoardUi {
    /// The slot showing the current bot's conversation. It lives in the slot
    /// table beside the tabs' slots but its tab is never in the workspace,
    /// so the tile tree neither renders nor disposes it. Created on the
    /// first visit and kept through Projects mode.
    pub(super) slot: Option<SlotId>,
    /// What the detail panel shows; `None` while it is shut.
    pub(super) panel: Option<PanelItem>,
    /// What it showed as it slides shut (rendered until the tween ends).
    panel_last: Option<PanelItem>,
    /// The panel's width tween (0 ↔ its width and gap).
    panel_tween: Option<WidthTween>,
    /// A session the panel looks into (an asking worker, an orchestration
    /// thread): a session context watching its transcript, and for a thread
    /// the read-only transcript drawn from it.
    peek: Option<items::Peek>,
    /// The Review panel's editable draft, for the exchange it was opened on.
    review: Option<(String, Entity<TextInput>)>,
    /// Questions answered from the Asking panel, until their session says
    /// so (request ids).
    answered: std::collections::HashSet<String>,
    /// The work item whose Stop is armed (a second click stops it), and the
    /// task that disarms it.
    stop_armed: Option<String>,
    stop_disarm: Option<Task<()>>,
    /// The Activity panel's kind filter; `None` shows everything.
    activity_filter: Option<cypher_proto::ActivityGroup>,
    /// The bot switcher's open list: its filter and keyboard cursor.
    switcher: Option<picker::Switcher>,
    /// The one floating plate under the focused card (the project
    /// sidebar's [`super::spaces::CardPlate`]): the card whose item the
    /// panel shows, else Status. The previous card and the 0→1 glide.
    plate: super::spaces::CardPlate,
    focused_card: Option<&'static str>,
    /// Cards folded to their header, by card key; like the project cards'
    /// collapse, local to this window and never saved.
    collapsed: std::collections::HashSet<&'static str>,
    focus_tween: Option<WidthTween>,
    /// The Schedule and Network panels' forms and drafts.
    schedule: schedule::ScheduleUi,
    network: network::NetworkUi,
    /// The shortcuts under the cards: their clicks and menu.
    shortcuts: shortcuts::ShortcutsUi,
    /// A delete or revoke waiting on the owner's yes.
    confirm: Option<parts::Confirm>,
    /// A form's buttons, pinned under the panel's scrolling body; set by
    /// the panel item while it renders and taken by the card.
    panel_footer: Option<AnyElement>,
    /// An item's own buttons, in the panel header before the close button;
    /// set and taken like `panel_footer`.
    panel_actions: Option<AnyElement>,
    /// The item's own tile left of its title (a work item's status, with
    /// details on hover), in place of the glyph; set and taken likewise.
    panel_mark: Option<AnyElement>,
    /// A quiet note after the item's title (a project's device), set and
    /// taken likewise.
    panel_title_note: Option<AnyElement>,
    /// An empty item's message, centred in the card under the header in
    /// place of its body; set and taken likewise.
    panel_empty: Option<AnyElement>,
    /// Work steps folded to their title in the work panel, by (work id,
    /// step index); local to this window like the cards' folds.
    work_folded: std::collections::HashSet<(String, usize)>,
    /// The Copy button just pressed (its key), and the task that turns its
    /// check back into Copy.
    copied: Option<String>,
    copied_reset: Option<Task<()>>,
}

/// One item on a bot's **Needs you** card: its conversation waiting on an
/// answer, or a record that waits on the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Need {
    Chat,
    Record(cypher_proto::NeedsYou),
}

/// Whether `chat_id` is a session waiting on an answer.
fn chat_asking(state: &crate::state::AppState, chat_id: &str, now: chrono::DateTime<Utc>) -> bool {
    state
        .chats
        .iter()
        .find(|c| c.id == chat_id)
        .is_some_and(|chat| state.display_status_for(chat, now) == ChatIndicator::AwaitingInput)
}

/// What needs the owner for `bot`, most pressing first: its own question,
/// then the records' items ([`cypher_proto::BotRecords::needs_you`]). The
/// Needs you card, the Status line, the picker and the logo badge all count
/// this one list, so resolving an item anywhere updates them together.
pub(super) fn bot_needs(
    state: &crate::state::AppState,
    bot: &Bot,
    now: chrono::DateTime<Utc>,
) -> Vec<Need> {
    let chat = chat_asking(state, &bot.chat_id, now).then_some(Need::Chat);
    chat.into_iter()
        .chain(
            state
                .bot_records
                .needs_you(&bot.id, |chat_id| chat_asking(state, chat_id, now))
                .into_iter()
                .map(Need::Record),
        )
        .collect()
}

/// What needs the owner across every bot: the logo's count in the Sessions
/// view.
pub(super) fn needs_you_count(state: &crate::state::AppState, now: chrono::DateTime<Utc>) -> usize {
    state
        .bots
        .iter()
        .map(|bot| bot_needs(state, bot, now).len())
        .sum()
}

/// "by Bob" for a session a bot started, from its work records.
pub(super) fn started_by_mark(state: &crate::state::AppState, chat_id: &str) -> Option<String> {
    let (work, _) = state.bot_records.started_by(chat_id)?;
    let bot = state.bots.iter().find(|b| b.id == work.bot_id)?;
    Some(format!("by {}", transcript::single_line(&bot.name)))
}

/// The bot the Botboard should show: the saved one while it exists, else
/// the oldest. `None` with no bots.
fn current_bot<'a>(bots: &'a [Bot], saved: Option<&str>) -> Option<&'a Bot> {
    saved
        .and_then(|id| bots.iter().find(|b| b.id == id))
        .or_else(|| bots.first())
}

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

    /// Whether this window shows the Botboard: the main window in Botboard
    /// mode (a project window has none).
    pub(super) fn botboard(&self) -> bool {
        !self.is_project_window() && self.settings.window_mode == WindowMode::Botboard
    }

    /// Switch the main window's mode (the sidebar logo). Each mode keeps
    /// its own state: the tab layout stays as it was through the Botboard,
    /// and the Botboard keeps its bot.
    pub(super) fn set_window_mode(&mut self, mode: WindowMode, cx: &mut Context<Self>) {
        if self.is_project_window() || self.settings.window_mode == mode {
            return;
        }
        self.settings.window_mode = mode;
        self.route = Route::Chat;
        self.close_bot_menu(cx);
        self.close_sidebar_view_menu(cx);
        self.sync_board(cx);
        self.tiles.focus_pending = true;
        self.schedule_save(cx);
        // Comment UI of the tiles that left the screen goes, main follows
        // the conversation now in front.
        self.workspace_changed(cx);
    }

    /// Show `bot_id` on the Botboard, switching to it. A project window
    /// hands it to the main window.
    pub(super) fn open_bot(&mut self, bot_id: &str, cx: &mut Context<Self>) {
        self.close_bot_menu(cx);
        if self.is_project_window() {
            let bot_id = bot_id.to_string();
            self.forward_to_main(cx, move |main, cx| main.open_bot(&bot_id, cx));
            return;
        }
        if self.settings.botboard_bot.as_deref() != Some(bot_id) {
            self.settings.botboard_bot = Some(bot_id.to_string());
            self.close_panel_now();
            self.schedule_save(cx);
        }
        if self.botboard() {
            self.sync_board(cx);
            self.tiles.focus_pending = true;
            cx.notify();
        } else {
            self.set_window_mode(WindowMode::Botboard, cx);
        }
    }

    /// The bot whose conversation `chat_id` is, by the chat's own marker
    /// (the bot row may not have landed yet).
    pub(super) fn bot_of_chat(&self, chat_id: &str, cx: &App) -> Option<String> {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == chat_id)
            .and_then(|c| c.bot.as_ref())
            .map(|marker| marker.bot_id.clone())
    }

    /// The Botboard's conversation: the board slot's chat.
    pub(super) fn board_chat(&self) -> Option<&str> {
        self.board
            .slot
            .and_then(|sid| self.tiles.slots.get(&sid))
            .and_then(|slot| slot.tab.chat_id())
    }

    /// Keep the Botboard on a bot that exists and its slot on that bot's
    /// conversation. A deleted current bot hands over to the oldest one (or
    /// the empty state); nothing moves before the first bots frame, nor
    /// while a just-created bot's row is still on its way. The slot is
    /// created on the Botboard only, and replaced when the conversation
    /// changes. Runs on every main-state change.
    pub(super) fn sync_board(&mut self, cx: &mut Context<Self>) {
        if self.is_project_window() {
            return;
        }
        // The slot of a bot created moments ago, whose rows may trail.
        let pending = self
            .board_chat()
            .filter(|chat_id| self.chat_expected(chat_id))
            .map(str::to_string);
        let (current, wanted, listed) = {
            let state = self.state.read(cx);
            let saved = self.settings.botboard_bot.as_deref();
            let saved_live = saved.is_some_and(|id| state.bots.iter().any(|b| b.id == id));
            if !saved_live && (pending.is_some() || !state.bots_synced) {
                return;
            }
            let bot = current_bot(&state.bots, saved);
            let wanted = bot.map(|b| b.chat_id.clone());
            // A bot row can land before its chat row; the slot waits for it.
            let listed = wanted
                .as_ref()
                .is_some_and(|id| state.chats.iter().any(|c| c.id == *id));
            (bot.map(|b| b.id.clone()), wanted, listed)
        };
        if current != self.settings.botboard_bot {
            self.settings.botboard_bot = current;
            self.close_panel_now();
            self.schedule_save(cx);
        }
        if pending.is_some() && pending == wanted {
            return;
        }
        let chat = wanted.filter(|_| listed);
        let bound = self.board.slot.and_then(|sid| {
            let slot = self.tiles.slots.get(&sid)?;
            let selected = slot.state.read(cx).selected_chat.clone();
            Some((slot.tab.chat_id().map(str::to_string), selected))
        });
        let settled = match &bound {
            Some((tab, selected)) => *tab == chat && *selected == chat,
            None => chat.is_none() || !self.botboard(),
        };
        if settled {
            return;
        }
        if let Some(sid) = self.board.slot.take() {
            self.dispose_slot(sid, cx);
        }
        if let Some(chat_id) = chat.filter(|_| self.botboard()) {
            self.board.slot = Some(self.create_slot(TabKey::Session(chat_id), cx));
            self.tiles.focus_pending = true;
        }
        self.sync_follow(cx);
        cx.notify();
    }

    /// A bot palette that just created `bot`: show it on the Botboard (its
    /// row may trail the reply; the slot holds on to its chat meanwhile).
    fn show_new_bot(&mut self, bot: Bot, cx: &mut Context<Self>) {
        self.expect_chat(&bot.chat_id);
        self.settings.botboard_bot = Some(bot.id.clone());
        self.close_panel_now();
        if let Some(sid) = self.board.slot.take() {
            self.dispose_slot(sid, cx);
        }
        self.board.slot = Some(self.create_slot(TabKey::Session(bot.chat_id), cx));
        if self.botboard() {
            self.tiles.focus_pending = true;
            self.schedule_save(cx);
            self.workspace_changed(cx);
        } else {
            self.set_window_mode(WindowMode::Botboard, cx);
        }
    }

    /// A plan card's Edit: cancel the proposal and start a reply asking for
    /// the change, so the bot proposes a revised plan.
    pub(super) fn edit_bot_plan(
        &mut self,
        sid: SlotId,
        work_id: &str,
        title: &str,
        cx: &mut Context<Self>,
    ) {
        self.mutate(
            serde_json::json!({ "op": "resolveBotPlan", "workId": work_id, "approve": false }),
            cx,
        );
        let Some(slot) = self.tiles.slots.get(&sid) else {
            return;
        };
        let (composer, chat_id) = (slot.composer.clone(), slot.tab.chat_id().map(str::to_owned));
        if let Some(chat_id) = chat_id {
            let text = format!(
                "Change the plan \u{201C}{}\u{201D}: ",
                transcript::single_line(title)
            );
            composer.update(cx, |composer, cx| composer.seed_draft(&chat_id, text, cx));
        }
        self.tiles.focus_pending = true;
        cx.notify();
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
            self.render_bot_menu(&theme, cx),
            self.render_bot_dialog(viewport, &theme, window, cx),
            self.render_delete_bot_dialog(viewport, &theme, cx),
            self.render_board_confirm(viewport, &theme, cx),
            self.render_shortcut_menu(&theme, cx),
        ]
        .into_iter()
        .flatten()
        .collect()
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
            .unwrap_or_else(|| "This workspace".into());
        let card = popover::dialog_card(theme)
            .child(popover::dialog_title(theme, "Delete workspace?"))
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
            &device("vps", "linux", Some("0.3.57")),
            local,
            true
        ));
        assert!(can_host(
            &device("vps", "linux", Some("0.4.0")),
            local,
            true
        ));
        assert!(!can_host(
            &device("vps", "linux", Some("0.3.56")),
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
