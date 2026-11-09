//! **Shortcuts**: buttons pinned at the foot of a workspace's sidebar, each
//! a request the owner makes often. A click sends it to the workspace's
//! agent in its chat, as if the owner had typed it; a request holding
//! `{input}` asks for one line of text first, in a field under the chips.
//!
//! Nobody makes shortcuts here: the owner asks the agent in the workspace
//! chat ("make this a button") and it saves, changes or removes them with
//! its own tools. Right-click a chip to move or delete it. Shortcuts are
//! registry rows, synced like routines; only the clicks live here.

use super::board::{CARD_TITLE_INSET, ROW_MARGIN, ROW_MARK_INSET, header_frame};
use super::*;
use cypher_proto::BotShortcut;

/// The icons a shortcut can wear, by the name it stores (one of
/// [`cypher_proto::SHORTCUT_ICONS`]).
const SHORTCUT_ICONS: &[(&str, &str)] = &[
    ("bolt", icons::BOLT),
    ("star", icons::STAR),
    ("rocket", icons::ROCKET),
    ("play", icons::PLAY),
    ("chat", icons::CHAT_ROUND_LINE),
    ("checklist", icons::CHECKLIST),
    ("code", icons::CODE),
    ("terminal", icons::TERMINAL),
    ("bug", icons::BUG),
    ("test-tube", icons::TEST_TUBE),
    ("document", icons::DOCUMENT),
    ("global", icons::GLOBAL),
    ("calendar", icons::CALENDAR),
    ("cup-hot", icons::CUP_HOT),
    ("magic-stick", icons::MAGIC_STICK),
    ("flag", icons::FLAG),
];

/// How long a sent request's check stays on its chip.
const SENT_MS: u64 = 1600;
/// How long an armed Delete waits for its confirming click.
const DELETE_ARM_MS: u64 = 4000;
/// The most of a request a chip's tooltip shows.
const TIP_CHARS: usize = 160;

/// A shortcut's icon: the one it names, else a chat bubble (it talks to
/// the agent).
pub(super) fn shortcut_glyph(shortcut: &BotShortcut) -> &'static str {
    shortcut
        .icon
        .as_deref()
        .and_then(icon_asset)
        .unwrap_or(icons::CHAT_ROUND_LINE)
}

fn icon_asset(name: &str) -> Option<&'static str> {
    SHORTCUT_ICONS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, asset)| *asset)
}

/// One line of `text`, at most `max` characters.
fn clip(text: &str, max: usize) -> String {
    let line = transcript::single_line(text);
    if line.chars().count() <= max {
        return line;
    }
    let mut cut: String = line.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

/// What a chip says when its request didn't go out: "Couldn't send Fix
/// tests: A message is still sending."
pub(super) fn failure_line(label: &str, why: &str) -> String {
    format!("Couldn't send {}: {why}", transcript::single_line(label))
}

/// The ids after moving `id` one place by `delta` (-1 earlier, +1 later);
/// `None` when it is already at that end.
pub(super) fn moved(ids: &[String], id: &str, delta: isize) -> Option<Vec<String>> {
    let from = ids.iter().position(|i| i == id)?;
    let to = from
        .checked_add_signed(delta)
        .filter(|to| *to < ids.len())?;
    let mut ids = ids.to_vec();
    ids.swap(from, to);
    Some(ids)
}

// ── state ───────────────────────────────────────────────────────────────────

/// A shortcut's latest click, as its chip shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sent {
    Done,
    Failed(String),
}

/// The shortcut waiting on its input, and the field it is typed in.
struct Asking {
    id: String,
    input: Entity<TextInput>,
    _submit: Subscription,
}

/// The shortcuts' own state: the latest click's outcome by shortcut id
/// (with the click that set it, so a stale reset leaves a newer one
/// alone), the field asking for an input, and the chip menu with its
/// armed Delete.
#[derive(Default)]
pub(super) struct ShortcutsUi {
    sent: std::collections::HashMap<String, (u64, Sent)>,
    last_click: u64,
    asking: Option<Asking>,
    /// The chip menu: (shortcut id, window position).
    menu: popover::Popup<(String, Point<Pixels>)>,
    delete_armed: Option<String>,
    delete_disarm: Option<Task<()>>,
}

impl Shell {
    fn shortcut_row(&self, id: &str, cx: &App) -> Option<BotShortcut> {
        self.state
            .read(cx)
            .bot_records
            .shortcuts
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    /// The current workspace and its shortcuts, in order.
    fn board_shortcuts(&self, cx: &App) -> Option<(Bot, Vec<BotShortcut>)> {
        let state = self.state.read(cx);
        let bot = current_bot(&state.bots, self.settings.botboard_bot.as_deref())?.clone();
        let shortcuts = state
            .bot_records
            .shortcuts_of(&bot.id)
            .into_iter()
            .cloned()
            .collect();
        Some((bot, shortcuts))
    }

    // ── the area ────────────────────────────────────────────────────────────

    /// The shortcuts pinned under the workspace's cards: a quiet head, then
    /// the chips (or a line saying how to get one), the field a chip asks
    /// for its input in, and what a failed click said. `None` with no
    /// workspace.
    pub(in crate::shell) fn render_board_shortcuts(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (bot, shortcuts) = self.board_shortcuts(cx)?;
        if self
            .board
            .shortcuts
            .asking
            .as_ref()
            .is_some_and(|asking| !shortcuts.iter().any(|s| s.id == asking.id))
        {
            self.board.shortcuts.asking = None;
        }
        let header = header_frame("bot-shortcuts-header", "Shortcuts", false, theme);
        let mut card = super::board::card(theme).child(header);
        if shortcuts.is_empty() {
            card = card.child(
                div()
                    .pl(px(CARD_TITLE_INSET))
                    .pr(px(10.0))
                    .pb(px(GROUP_CARD_BODY_PADDING + 6.0))
                    .mt(px(-4.0))
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .text_color(theme.text_muted.opacity(0.6))
                    .child(SharedString::from(format!(
                        "Ask {} to make a button for something you do often.",
                        transcript::single_line(&bot.name)
                    ))),
            );
        } else {
            let asking = self.board.shortcuts.asking.as_ref().map(|a| a.id.clone());
            let chips: Vec<_> = shortcuts
                .iter()
                .map(|shortcut| {
                    let selected = asking.as_deref() == Some(shortcut.id.as_str());
                    self.shortcut_chip(shortcut, selected, theme, cx)
                })
                .collect();
            let failures: Vec<_> = shortcuts
                .iter()
                .filter_map(
                    |shortcut| match self.board.shortcuts.sent.get(&shortcut.id) {
                        Some((_, Sent::Failed(line))) => Some((shortcut.id.clone(), line.clone())),
                        _ => None,
                    },
                )
                .map(|(id, line)| {
                    div()
                        .id(SharedString::from(format!("bot-shortcut-failed-{id}")))
                        .mx(px(ROW_MARGIN))
                        .px(px(ROW_MARK_INSET))
                        .pt(px(4.0))
                        .text_size(px(11.5))
                        .line_height(px(16.0))
                        .text_color(theme.danger.opacity(0.9))
                        .cursor_pointer()
                        .tooltip(|_, cx| {
                            cx.new(|_| super::super::session::FindTooltip("Dismiss".into()))
                                .into()
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.board.shortcuts.sent.remove(&id);
                            cx.notify();
                        }))
                        .child(SharedString::from(line))
                })
                .collect();
            card = card.child(
                div()
                    .pb(px(GROUP_CARD_BODY_PADDING + 4.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .mx(px(ROW_MARGIN))
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(4.0))
                            .children(chips),
                    )
                    .children(self.render_shortcut_input(theme, cx))
                    .children(failures),
            );
        }
        Some(
            div()
                .flex_none()
                .px(px(Theme::SPACE_SM))
                .pb(px(Theme::SPACE_SM))
                .child(
                    div()
                        .border_t_1()
                        .border_color(crate::kit::theme::hairline(0.06))
                        .child(card),
                )
                .into_any_element(),
        )
    }

    /// One chip: its icon (a check once its request went out, a warning
    /// when it didn't) and label, a pill that lifts on hover; `selected`
    /// while it asks for its input. A click sends it; a right-click opens
    /// its menu.
    fn shortcut_chip(
        &self,
        shortcut: &BotShortcut,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let key = format!("bot-shortcut-{}", shortcut.id);
        let sent = self
            .board
            .shortcuts
            .sent
            .get(&shortcut.id)
            .map(|(_, sent)| sent);
        let ticker = self.pulse_ticker.entity_id();
        let mark = match sent {
            Some(Sent::Done) => {
                super::super::render::status_mark(ChatIndicator::Completed, ticker, theme)
            }
            Some(Sent::Failed(_)) => Some(
                icon(icons::DANGER_TRIANGLE)
                    .size(px(12.0))
                    .text_color(theme.danger)
                    .into_any_element(),
            ),
            None => None,
        }
        .unwrap_or_else(|| {
            icon(shortcut_glyph(shortcut))
                .size(px(12.0))
                .text_color(theme.text_muted)
                .into_any_element()
        });
        let tip: SharedString = match sent {
            Some(Sent::Failed(line)) => line.clone(),
            _ => clip(&shortcut.request, TIP_CHARS),
        }
        .into();
        let hover = crate::appearance::surface_style::sidebar_hover(theme);
        let rest = if selected {
            crate::appearance::surface_style::sidebar_selected(theme)
        } else {
            crate::kit::theme::ink(0.045)
        };
        let (menu_id, click_id) = (shortcut.id.clone(), shortcut.id.clone());
        div()
            .id(SharedString::from(key.clone()))
            .flex_none()
            .max_w_full()
            .h(px(26.0))
            .pl(px(ROW_MARK_INSET))
            .pr(px(10.0))
            .rounded(px(13.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .bg(motion::hover_blend(&key, rest, hover))
            .active(|s| s.bg(crate::kit::theme::wash(0.14)))
            .text_size(px(12.0))
            .text_color(motion::hover_blend(
                &key,
                theme.text.opacity(if selected { 1.0 } else { 0.85 }),
                theme.text,
            ))
            .on_hover(motion::hover_listener(key))
            .tooltip(move |_, cx| {
                cx.new(|_| super::super::session::FindTooltip(tip.clone()))
                    .into()
            })
            .on_click(
                cx.listener(move |this, _, window, cx| this.click_shortcut(&click_id, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.board.shortcuts.delete_armed = None;
                    this.board
                        .shortcuts
                        .menu
                        .open((menu_id.clone(), event.position));
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_none()
                    .size(px(14.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(mark),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(shortcut.label.clone())),
            )
    }

    /// The field a chip asks for its input in: Enter sends, Esc cancels.
    fn render_shortcut_input(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let asking = self.board.shortcuts.asking.as_ref()?;
        Some(
            div()
                .mx(px(ROW_MARGIN))
                .mt(px(6.0))
                .px(px(10.0))
                .py(px(5.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(crate::kit::theme::hairline(0.08))
                .bg(crate::kit::theme::ink(0.04))
                .text_size(px(12.5))
                .text_color(theme.text)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    let keys = &event.keystroke;
                    if keys.key == "escape" && !keys.modifiers.modified() {
                        this.board.shortcuts.asking = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .child(asking.input.clone().into_any_element())
                .into_any_element(),
        )
    }

    // ── sending ─────────────────────────────────────────────────────────────

    /// A chip's click: ask for its input first when its request has a
    /// blank, else send it.
    fn click_shortcut(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shortcut) = self.shortcut_row(id, cx) else {
            return;
        };
        if !shortcut.wants_input() {
            return self.send_shortcut(&shortcut, "", cx);
        }
        if let Some(asking) = self.board.shortcuts.asking.as_ref()
            && asking.id == id
        {
            window.focus(&asking.input.focus_handle(cx), cx);
            return;
        }
        let input = cx.new(|cx| {
            TextInput::settings_field(
                format!(
                    "Text for {}, then Enter",
                    transcript::single_line(&shortcut.label)
                ),
                false,
                cx,
            )
        });
        let shortcut_id = shortcut.id.clone();
        let submit = cx.subscribe(
            &input,
            move |this: &mut Self, input: Entity<TextInput>, event: &TextInputEvent, cx| {
                if !matches!(event, TextInputEvent::Submitted) {
                    return;
                }
                let text = input.read(cx).text().trim().to_string();
                if text.is_empty() {
                    return;
                }
                this.board.shortcuts.asking = None;
                if let Some(shortcut) = this.shortcut_row(&shortcut_id, cx) {
                    this.send_shortcut(&shortcut, &text, cx);
                }
                cx.notify();
            },
        );
        window.focus(&input.focus_handle(cx), cx);
        self.board.shortcuts.asking = Some(Asking {
            id: shortcut.id,
            input,
            _submit: submit,
        });
        cx.notify();
    }

    /// Send `shortcut`'s request, `input` in its blanks, to its workspace's
    /// chat (brought to the front) beside whatever the owner is typing
    /// there. Its chip shows a check for a moment, or why it didn't go.
    fn send_shortcut(&mut self, shortcut: &BotShortcut, input: &str, cx: &mut Context<Self>) {
        let request = shortcut.request_with(input);
        let outcome = match self.send_to_board_chat(&shortcut.bot_id, &request, cx) {
            Ok(()) => Sent::Done,
            Err(why) => Sent::Failed(failure_line(&shortcut.label, &why)),
        };
        let done = outcome == Sent::Done;
        let ui = &mut self.board.shortcuts;
        ui.last_click += 1;
        let click = ui.last_click;
        ui.sent.insert(shortcut.id.clone(), (click, outcome));
        cx.notify();
        if !done {
            return;
        }
        let id = shortcut.id.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(SENT_MS))
                .await;
            this.update(cx, |this, cx| {
                let sent = &mut this.board.shortcuts.sent;
                if sent.get(&id).is_some_and(|(at, _)| *at == click) {
                    sent.remove(&id);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Send `text` in `bot_id`'s chat, which the Botboard shows.
    fn send_to_board_chat(
        &mut self,
        bot_id: &str,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let bot = self
            .bot_row(bot_id, cx)
            .ok_or_else(|| "The workspace was deleted.".to_string())?;
        self.open_bot(bot_id, cx);
        let composer = self
            .board
            .slot
            .filter(|_| self.board_chat() == Some(bot.chat_id.as_str()))
            .and_then(|sid| self.tiles.slots.get(&sid))
            .map(|slot| slot.composer.clone())
            .ok_or_else(|| "The workspace's chat isn't ready yet.".to_string())?;
        composer
            .update(cx, |composer, cx| composer.send_aside(text, cx))
            .map_err(|why| why.to_string())?;
        self.tiles.focus_pending = true;
        Ok(())
    }

    // ── the chip menu ───────────────────────────────────────────────────────

    fn close_shortcut_menu(&mut self, cx: &mut Context<Self>) {
        self.board.shortcuts.delete_armed = None;
        self.board.shortcuts.delete_disarm = None;
        if self.board.shortcuts.menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.board.shortcuts.menu);
            cx.notify();
        }
    }

    /// Esc closes an open chip menu. Returns whether it did.
    pub(super) fn close_shortcut_menu_on_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.board.shortcuts.menu.is_open() {
            return false;
        }
        self.close_shortcut_menu(cx);
        true
    }

    /// A chip's menu: Move left, Move right, and Delete, which asks once
    /// more by turning red until a second click within a moment.
    pub(super) fn render_shortcut_menu(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (id, position) = self.board.shortcuts.menu.get().cloned()?;
        let closing = self.board.shortcuts.menu.closing_since();
        let (bot, shortcuts) = self.board_shortcuts(cx)?;
        let ids: Vec<String> = shortcuts.iter().map(|s| s.id.clone()).collect();
        let row = |key: &str, glyph: &'static str, label: &str, enabled: bool, danger: bool| {
            let (ink, mark) = if danger {
                (theme.danger, theme.danger)
            } else {
                (theme.text, theme.text_muted)
            };
            popover::menu_row(theme, false, format!("bot-shortcut-menu-{key}"))
                .id(SharedString::from(format!("bot-shortcut-menu-{key}")))
                .when(!enabled, |el| el.opacity(0.4).cursor_default())
                .text_color(ink)
                .child(icon(glyph).size(px(16.0)).text_color(mark))
                .child(SharedString::from(label.to_string()))
        };
        let mover =
            |key: &str, glyph: &'static str, label: &str, delta: isize, cx: &mut Context<Self>| {
                let order = moved(&ids, &id, delta);
                let bot_id = bot.id.clone();
                row(key, glyph, label, order.is_some(), false).when_some(order, |el, order| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        this.close_shortcut_menu(cx);
                        this.board_mutate(
                            serde_json::json!({
                                "op": "orderBotShortcuts", "botId": bot_id, "shortcutIds": order,
                            }),
                            cx,
                        );
                    }))
                })
            };
        let left = mover("left", icons::ARROW_LEFT, "Move left", -1, cx);
        let right = mover("right", icons::ARROW_RIGHT, "Move right", 1, cx);
        let armed = self.board.shortcuts.delete_armed.as_deref() == Some(id.as_str());
        let delete_id = id.clone();
        let delete = row(
            "delete",
            icons::TRASH_BIN_MINIMALISTIC,
            if armed {
                "Click again to delete"
            } else {
                "Delete"
            },
            true,
            true,
        )
        .when(armed, |el| el.bg(theme.danger.opacity(0.14)))
        .on_click(
            cx.listener(move |this, _, _, cx| this.arm_or_delete_shortcut(delete_id.clone(), cx)),
        );
        let menu = popover::popover_card(theme)
            .w(px(170.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_shortcut_menu(cx)))
            .flex()
            .flex_col()
            .child(left)
            .child(right)
            .child(popover::menu_separator())
            .child(delete)
            .into_any_element();
        Some(popover::menu_at(
            "bot-shortcut-menu",
            position,
            menu,
            closing,
        ))
    }

    fn arm_or_delete_shortcut(&mut self, id: String, cx: &mut Context<Self>) {
        if self.board.shortcuts.delete_armed.as_deref() == Some(id.as_str()) {
            self.close_shortcut_menu(cx);
            self.board.shortcuts.sent.remove(&id);
            if self
                .board
                .shortcuts
                .asking
                .as_ref()
                .is_some_and(|asking| asking.id == id)
            {
                self.board.shortcuts.asking = None;
            }
            self.board_mutate(
                serde_json::json!({ "op": "deleteBotShortcut", "shortcutId": id }),
                cx,
            );
            return cx.notify();
        }
        self.board.shortcuts.delete_armed = Some(id.clone());
        self.board.shortcuts.delete_disarm = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(DELETE_ARM_MS))
                .await;
            this.update(cx, |this, cx| {
                if this.board.shortcuts.delete_armed.as_deref() == Some(id.as_str()) {
                    this.board.shortcuts.delete_armed = None;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(request: &str, icon: Option<&str>) -> BotShortcut {
        BotShortcut {
            id: "s".into(),
            bot_id: "b".into(),
            label: "Fix tests".into(),
            icon: icon.map(str::to_string),
            request: request.into(),
            order: 0,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn the_input_fills_every_blank() {
        let s = shortcut("Fix {input}, then test {input}", None);
        assert!(s.wants_input());
        assert_eq!(s.request_with(" #42 "), "Fix #42, then test #42");
        let plain = shortcut("What's stuck?", None);
        assert!(!plain.wants_input());
        assert_eq!(plain.request_with(""), "What's stuck?");
    }

    #[test]
    fn every_icon_name_has_a_glyph_and_others_fall_back() {
        for name in cypher_proto::SHORTCUT_ICONS {
            assert!(icon_asset(name).is_some(), "{name}");
        }
        assert_eq!(SHORTCUT_ICONS.len(), cypher_proto::SHORTCUT_ICONS.len());
        assert_eq!(
            shortcut_glyph(&shortcut("x", Some("rocket"))),
            icons::ROCKET
        );
        assert_eq!(
            shortcut_glyph(&shortcut("x", Some("unheard-of"))),
            icons::CHAT_ROUND_LINE
        );
        assert_eq!(shortcut_glyph(&shortcut("x", None)), icons::CHAT_ROUND_LINE);
    }

    #[test]
    fn tips_and_failures_read_in_one_line() {
        assert_eq!(clip("Fix the\ntests", TIP_CHARS), "Fix the tests");
        let long = "a".repeat(TIP_CHARS + 10);
        let tip = clip(&long, TIP_CHARS);
        assert_eq!(tip.chars().count(), TIP_CHARS);
        assert!(tip.ends_with('…'));
        assert_eq!(
            failure_line("Fix tests", "A message is still sending."),
            "Couldn't send Fix tests: A message is still sending."
        );
    }

    #[test]
    fn moves_swap_neighbours_and_stop_at_the_ends() {
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(moved(&ids, "b", -1).unwrap(), ["b", "a", "c"]);
        assert_eq!(moved(&ids, "b", 1).unwrap(), ["a", "c", "b"]);
        assert_eq!(moved(&ids, "a", -1), None);
        assert_eq!(moved(&ids, "c", 1), None);
        assert_eq!(moved(&ids, "gone", 1), None);
    }
}
