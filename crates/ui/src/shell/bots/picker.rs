//! The bot switcher heading the Botboard sidebar: the current bot as a row
//! (its avatar and name, on the cards' header grid) that opens a list of
//! every bot by name. Avatars alone stopped telling bots apart past a few,
//! so the list names each one and says what it is doing; past a handful it
//! leads with a search field. The keyboard drives it like the composer's
//! pickers: type to filter, ↑↓ to move, Enter to switch, Esc to close.
//!
//! A right-click on the row opens the current bot's menu (Open /
//! Settings… / Delete…); a right-click on a list row opens that bot's.

use super::*;
use crate::widgets::text_input;
use gpui::{FocusHandle, KeyDownEvent};

/// The row's bot face, and the list rows' one (bare glyphs: the row's own
/// fill sets it apart).
const AVATAR: f32 = 16.0;
const LIST_AVATAR: f32 = 15.0;
/// Bots before the list leads with a search field.
const SEARCH_FROM: usize = 6;
/// The list's height before it scrolls.
const LIST_MAX_H: f32 = 360.0;

/// The open list's filter and keyboard cursor.
pub(in crate::shell) struct Switcher {
    search: Entity<TextInput>,
    focus: FocusHandle,
    highlight: usize,
    _edited: Subscription,
}

/// One bot as the list shows it.
#[derive(Clone)]
struct BotLine {
    bot: Bot,
    status: Option<ChatIndicator>,
    needs: usize,
    online: bool,
}

/// The bots `query` matches, best first (a name prefix before a substring),
/// in the picker's order otherwise.
fn filtered<'a>(lines: &'a [BotLine], query: &str) -> Vec<&'a BotLine> {
    let mut ranked: Vec<(usize, usize, &BotLine)> = lines
        .iter()
        .enumerate()
        .filter_map(|(ix, line)| popover::match_rank(query, &line.bot.name).map(|r| (r, ix, line)))
        .collect();
    ranked.sort_by_key(|(rank, ix, _)| (*rank, *ix));
    ranked.into_iter().map(|(_, _, line)| line).collect()
}

/// What needs the owner in bots other than `current`.
fn needs_elsewhere(lines: &[BotLine], current: &str) -> usize {
    lines
        .iter()
        .filter(|line| line.bot.id != current)
        .map(|line| line.needs)
        .sum()
}

impl Shell {
    fn bot_lines(&self, cx: &App) -> Vec<BotLine> {
        let now = Utc::now();
        let state = self.state.read(cx);
        state
            .bots
            .iter()
            .map(|bot| BotLine {
                status: state
                    .chats
                    .iter()
                    .find(|c| c.id == bot.chat_id)
                    .map(|c| state.display_status_for(c, now)),
                needs: bot_needs(state, bot, now).len(),
                online: state.device_online(&bot.host_device_id, now),
                bot: bot.clone(),
            })
            .collect()
    }

    /// Open the list, its filter cleared and focused, the cursor on the
    /// current bot.
    fn open_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let search = cx.new(|cx| TextInput::new("Find a bot…", cx));
        // The field's own bindings take Enter, ↑ and ↓ before any key
        // listener sees them: Enter arrives as Submitted, the arrows are
        // caught in the list's capture phase ([`Self::render_switcher_list`]).
        let edited = cx.subscribe(
            &search,
            |this: &mut Shell, _, event: &TextInputEvent, cx| match event {
                TextInputEvent::Edited => {
                    if let Some(switcher) = this.board.switcher.as_mut() {
                        switcher.highlight = 0;
                        cx.notify();
                    }
                }
                TextInputEvent::Submitted => this.switcher_pick(cx),
                _ => {}
            },
        );
        let lines = self.bot_lines(cx);
        let current = self.settings.botboard_bot.clone();
        let highlight = lines
            .iter()
            .position(|line| Some(&line.bot.id) == current.as_ref())
            .unwrap_or(0);
        let focus = cx.focus_handle();
        if lines.len() >= SEARCH_FROM {
            window.focus(&search.read(cx).focus_handle(cx), cx);
        } else {
            window.focus(&focus, cx);
        }
        self.board.switcher = Some(Switcher {
            search,
            focus,
            highlight,
            _edited: edited,
        });
        self.menus.bot_switcher.open(());
        cx.notify();
    }

    pub(super) fn close_switcher(&mut self, cx: &mut Context<Self>) {
        if self.menus.bot_switcher.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.menus.bot_switcher);
            self.tiles.focus_pending = true;
            cx.notify();
        }
    }

    fn switch_to(&mut self, bot_id: &str, cx: &mut Context<Self>) {
        self.close_switcher(cx);
        self.open_bot(bot_id, cx);
    }

    /// Move the cursor by `step` through the rows the filter shows.
    fn switcher_step(&mut self, step: isize, cx: &mut Context<Self>) {
        let lines = self.bot_lines(cx);
        let Some(switcher) = self.board.switcher.as_mut() else {
            return;
        };
        let query = switcher.search.read(cx).text().to_string();
        let count = filtered(&lines, &query).len() as isize;
        if count > 0 {
            switcher.highlight = (switcher.highlight as isize + step).rem_euclid(count) as usize;
            cx.notify();
        }
    }

    /// Switch to the row under the cursor.
    fn switcher_pick(&mut self, cx: &mut Context<Self>) {
        let lines = self.bot_lines(cx);
        let Some(switcher) = self.board.switcher.as_ref() else {
            return;
        };
        let query = switcher.search.read(cx).text().to_string();
        let shown = filtered(&lines, &query);
        if let Some(line) = shown.get(switcher.highlight.min(shown.len().saturating_sub(1))) {
            let id = line.bot.id.clone();
            self.switch_to(&id, cx);
        }
    }

    /// The list's keys that reach it as keys: arrows and Enter while the
    /// field is not focused (a short list has none), and Esc.
    fn switcher_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "down" => self.switcher_step(1, cx),
            "up" => self.switcher_step(-1, cx),
            "enter" => self.switcher_pick(cx),
            "escape" => self.close_switcher(cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The switcher row; `None` before the first bots frame and with no bots
    /// (the empty card offers the first one).
    pub(in crate::shell) fn render_bot_picker(
        &mut self,
        current: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.state.read(cx).bots_synced {
            return None;
        }
        let lines = self.bot_lines(cx);
        let current = lines
            .iter()
            .find(|line| Some(line.bot.id.as_str()) == current)?;
        let elsewhere = needs_elsewhere(&lines, &current.bot.id);
        let bot = current.bot.clone();
        let key = "bot-switcher";
        let open = self.menus.bot_switcher.get().is_some();
        let avatar = crate::appearance::bot_style::bot_glyph(
            &bot.name,
            bot.icon.as_deref(),
            bot.color.as_deref(),
            AVATAR,
            theme,
        );
        let menu_id = bot.id.clone();
        let mut row = div()
            .id(key)
            .relative()
            .h(px(GROUP_CARD_HEADER_HEIGHT))
            .px(px(10.0))
            .rounded(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(7.0))
            .cursor_pointer()
            // Always filled, so the switcher reads as a control above the
            // cards; deeper on hover and while its list is open.
            .bg(if open {
                crate::kit::theme::wash(0.12)
            } else {
                motion::hover_blend(
                    key,
                    crate::kit::theme::wash(0.07),
                    crate::kit::theme::wash(0.10),
                )
            })
            .on_hover(motion::hover_listener(key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.menus.bot_switcher.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                if this.menus.bot_switcher.take_press_was_open() {
                    this.close_switcher(cx);
                } else {
                    this.open_switcher(window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.menus.bot.open((menu_id.clone(), event.position));
                    cx.notify();
                }),
            )
            // The face sits in the cards' 20px glyph box, so the name
            // shares the card titles' column below.
            .child(
                div()
                    .flex_none()
                    .size(px(20.0))
                    .ml(px(-3.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(avatar),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(14.0))
                    .line_height(px(18.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from(transcript::single_line(&bot.name))),
            )
            .child(div().flex_1())
            // Other bots' attention, which the row would otherwise hide.
            .when(elsewhere > 0, |el| {
                let tip: SharedString = format!(
                    "{elsewhere} {} you in your other bots",
                    if elsewhere == 1 { "needs" } else { "need" }
                )
                .into();
                el.child(
                    div()
                        .id("bot-switcher-elsewhere")
                        .flex_none()
                        .min_w(px(16.0))
                        .h(px(16.0))
                        .px(px(4.0))
                        .rounded_full()
                        .bg(theme.warning.opacity(0.16))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(10.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.warning)
                        .tooltip(move |_, cx| {
                            cx.new(|_| super::super::session::FindTooltip(tip.clone()))
                                .into()
                        })
                        .child(SharedString::from(if elsewhere > 9 {
                            "9+".to_string()
                        } else {
                            elsewhere.to_string()
                        })),
                )
            })
            .child(
                icon(icons::ALT_ARROW_UP_DOWN)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(theme.text_muted.opacity(0.7)),
            );
        if let Some(list) = self.render_switcher_list(&lines, &bot.id, theme, cx) {
            row = row.child(popover::anchored_menu_below_gap(
                "bot-switcher-menu",
                list,
                self.menus.bot_switcher.closing_since(),
                4.0,
            ));
        }
        Some(row.into_any_element())
    }

    /// The open list: every bot by name with what it is doing, filtered by
    /// the search field once there are enough, and New bot at the foot.
    fn render_switcher_list(
        &self,
        lines: &[BotLine],
        current: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.menus.bot_switcher.get()?;
        let switcher = self.board.switcher.as_ref()?;
        let query = switcher.search.read(cx).text().to_string();
        let shown = filtered(lines, &query);
        let highlight = switcher.highlight.min(shown.len().saturating_sub(1));
        let ticker = self.pulse_ticker.entity_id();
        let rows: Vec<AnyElement> = shown
            .iter()
            .enumerate()
            .map(|(ix, line)| {
                let bot = &line.bot;
                let key = format!("bot-switcher-{}", bot.id);
                let selected = bot.id == current;
                let (pick, menu_id) = (bot.id.clone(), bot.id.clone());
                // What it is doing, most pressing first: what needs the
                // owner, a run, a pause, an offline host.
                let state: Option<AnyElement> = if line.needs > 0 {
                    Some(
                        div()
                            .flex_none()
                            .min_w(px(16.0))
                            .h(px(16.0))
                            .px(px(4.0))
                            .rounded_full()
                            .bg(theme.warning.opacity(0.16))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(10.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.warning)
                            .child(SharedString::from(line.needs.to_string()))
                            .into_any_element(),
                    )
                } else if line.status == Some(ChatIndicator::Working) {
                    super::super::render::status_mark(ChatIndicator::Working, ticker, theme)
                } else if bot.paused {
                    Some(
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted.opacity(0.7))
                            .child(SharedString::from("paused"))
                            .into_any_element(),
                    )
                } else if !line.online {
                    Some(
                        icon(icons::CLOUD_OFF)
                            .size(px(12.0))
                            .flex_none()
                            .text_color(theme.text_muted.opacity(0.6))
                            .into_any_element(),
                    )
                } else {
                    None
                };
                popover::menu_row_nav(theme, selected, ix == highlight, key.clone())
                    .id(SharedString::from(key))
                    .gap(px(8.0))
                    .on_click(cx.listener(move |this, _, _, cx| this.switch_to(&pick, cx)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.close_switcher(cx);
                            this.menus.bot.open((menu_id.clone(), event.position));
                            cx.notify();
                        }),
                    )
                    .child(crate::appearance::bot_style::bot_glyph(
                        &bot.name,
                        bot.icon.as_deref(),
                        bot.color.as_deref(),
                        LIST_AVATAR,
                        theme,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(transcript::single_line(&bot.name))),
                    )
                    .children(state)
                    .child(div().flex_none().w(px(14.0)).when(selected, |el| {
                        el.child(
                            icon(icons::CHECK)
                                .size(px(13.0))
                                .text_color(theme.text_muted),
                        )
                    }))
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty().then(|| {
            div()
                .px(px(8.0))
                .py(px(8.0))
                .text_size(px(12.5))
                .text_color(theme.text_muted)
                .child(SharedString::from("No bot by that name"))
        });
        let new_bot = popover::menu_row(theme, false, "bot-switcher-new")
            .id("bot-switcher-new")
            .gap(px(8.0))
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_switcher(cx);
                this.open_bot_dialog(None, cx);
            }))
            .child(
                div()
                    .flex_none()
                    .size(px(LIST_AVATAR))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        icon(icons::PLUS)
                            .size(px(13.0))
                            .text_color(theme.text_muted),
                    ),
            )
            .child(SharedString::from("New bot"));
        let width = (self.settings.sidebar_width - 2.0 * Theme::SPACE_SM).max(220.0);
        Some(
            popover::popover_card(theme)
                .w(px(width))
                .track_focus(&switcher.focus)
                .on_key_down(
                    cx.listener(|this, event: &KeyDownEvent, _, cx| this.switcher_key(event, cx)),
                )
                .capture_action(cx.listener(|this, _: &text_input::Up, _, cx| {
                    cx.stop_propagation();
                    this.switcher_step(-1, cx);
                }))
                .capture_action(cx.listener(|this, _: &text_input::Down, _, cx| {
                    cx.stop_propagation();
                    this.switcher_step(1, cx);
                }))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_switcher(cx)))
                .flex()
                .flex_col()
                .when(lines.len() >= SEARCH_FROM, |el| {
                    el.child(popover::search_input_frame(
                        theme,
                        switcher.search.clone().into_any_element(),
                    ))
                })
                .child(
                    div()
                        .id("bot-switcher-rows")
                        .max_h(px(LIST_MAX_H))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .children(rows)
                        .children(empty),
                )
                .child(popover::menu_section().child(new_bot))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(name: &str, needs: usize) -> BotLine {
        BotLine {
            bot: Bot {
                id: name.to_lowercase(),
                name: name.into(),
                ..crate::shell::tests::test_bot("x")
            },
            status: None,
            needs,
            online: true,
        }
    }

    #[test]
    fn the_list_filters_by_name_prefix_first() {
        let lines = [line("Release", 0), line("Billing", 0), line("Bob", 2)];
        let names = |q: &str| -> Vec<String> {
            filtered(&lines, q)
                .into_iter()
                .map(|l| l.bot.name.clone())
                .collect()
        };
        assert_eq!(names(""), ["Release", "Billing", "Bob"], "picker order");
        assert_eq!(names("b"), ["Billing", "Bob"]);
        assert_eq!(names("ll"), ["Billing"]);
        assert!(names("zzz").is_empty());
        assert_eq!(needs_elsewhere(&lines, "bob"), 0);
        assert_eq!(needs_elsewhere(&lines, "release"), 2);
    }
}
