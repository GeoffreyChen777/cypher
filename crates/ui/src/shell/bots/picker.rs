//! The view switcher in the sidebar header: the "Cypher" wordmark, followed
//! on the Botboard by the current workspace as a soft tag in its colour. A click
//! opens one list: Sessions (the projects view, the default), every
//! workspace by name with what it is doing, then New workspace. Past a
//! handful it leads with a search field. The keyboard drives it like the
//! composer's pickers: type to filter, ↑↓ to move, Enter to pick, Esc to
//! close.
//!
//! A right-click on the tag opens the current workspace's menu (Open /
//! Settings… / Delete…); a right-click on a list row opens that one's.

use super::*;
use crate::widgets::text_input;
use gpui::{FocusHandle, KeyDownEvent};

/// The list rows' bot face (a bare glyph).
const LIST_AVATAR: f32 = 15.0;
/// The Sessions row's label, which the filter matches too.
const SESSIONS: &str = "Sessions";
/// Rows before the list leads with a search field.
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

/// One row of the list: the projects view, or a workspace.
#[derive(Clone, Copy)]
enum Pick<'a> {
    Sessions,
    Bot(&'a BotLine),
}

/// The rows `query` shows: Sessions first while it matches, then the
/// workspaces it matches.
fn picks<'a>(lines: &'a [BotLine], query: &str) -> Vec<Pick<'a>> {
    let sessions = popover::match_rank(query, SESSIONS).is_some();
    sessions
        .then_some(Pick::Sessions)
        .into_iter()
        .chain(filtered(lines, query).into_iter().map(Pick::Bot))
        .collect()
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
        let search = cx.new(|cx| TextInput::new("Find a workspace…", cx));
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
        // The cursor on what shows now: Sessions (row 0) in the projects
        // view, else the current workspace.
        let highlight = if self.botboard() {
            let current = self.settings.botboard_bot.clone();
            lines
                .iter()
                .position(|line| Some(&line.bot.id) == current.as_ref())
                .map_or(0, |ix| ix + 1)
        } else {
            0
        };
        let focus = cx.focus_handle();
        if lines.len() + 1 >= SEARCH_FROM {
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
        let count = picks(&lines, &query).len() as isize;
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
        let shown = picks(&lines, &query);
        match shown.get(switcher.highlight.min(shown.len().saturating_sub(1))) {
            Some(Pick::Sessions) => self.show_sessions(cx),
            Some(Pick::Bot(line)) => {
                let id = line.bot.id.clone();
                self.switch_to(&id, cx);
            }
            None => {}
        }
    }

    /// Back to the projects view, the default.
    fn show_sessions(&mut self, cx: &mut Context<Self>) {
        self.close_switcher(cx);
        self.set_window_mode(crate::prefs::WindowMode::Projects, cx);
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

    /// The wordmark as the view switcher: "Cypher", the current workspace
    /// as a soft tag in its colour on the Botboard, an amber count of what needs the
    /// owner in views not shown, and ⇅. A click opens the list below it.
    pub(in crate::shell) fn render_view_switch(
        &mut self,
        word: gpui::Div,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let botboard = self.botboard();
        let lines = self.bot_lines(cx);
        let current = botboard
            .then(|| {
                let id = self.settings.botboard_bot.clone();
                lines.iter().find(|line| Some(&line.bot.id) == id.as_ref())
            })
            .flatten()
            .cloned();
        // What waits elsewhere: every workspace from the projects view, the
        // others from a workspace.
        let waiting = match &current {
            Some(line) => needs_elsewhere(&lines, &line.bot.id),
            None => needs_you_count(self.state.read(cx), Utc::now()),
        };
        let key = "sidebar-logo";
        let open = self.menus.bot_switcher.get().is_some();
        // The workspace as a soft tag in its own colour: its face and name
        // on a faint wash of that colour (a plain wash without one).
        let tag = current.as_ref().map(|line| {
            let menu_id = line.bot.id.clone();
            let tint =
                crate::appearance::space_style::space_color(line.bot.color.as_deref(), theme);
            let ink = tint.unwrap_or(theme.text);
            div()
                .id("sidebar-logo-tag")
                .min_w_0()
                .h(px(20.0))
                .pl(px(5.0))
                .pr(px(7.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .bg(tint.map_or(crate::kit::theme::ink(0.07), |t| t.opacity(0.14)))
                .text_size(px(12.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(ink)
                .child(crate::appearance::bot_style::bot_glyph(
                    &line.bot.name,
                    line.bot.icon.as_deref(),
                    line.bot.color.as_deref(),
                    12.0,
                    theme,
                ))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.menus.bot.open((menu_id.clone(), event.position));
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(transcript::single_line(&line.bot.name))),
                )
        });
        let badge = (waiting > 0).then(|| {
            let tip: SharedString = format!(
                "{waiting} {} you {}",
                if waiting == 1 { "needs" } else { "need" },
                if current.is_some() {
                    "in your other workspaces"
                } else {
                    "in your workspaces"
                }
            )
            .into();
            div()
                .id("sidebar-logo-waiting")
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
                .child(SharedString::from(if waiting > 9 {
                    "9+".to_string()
                } else {
                    waiting.to_string()
                }))
        });
        let mut trigger = div()
            .id(key)
            .relative()
            .min_w_0()
            .h(px(28.0))
            // The wash pads past the wordmark without moving it off the
            // cards' column.
            .ml(px(-6.0))
            .px(px(6.0))
            .rounded(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .bg(if open {
                crate::kit::theme::wash(0.14)
            } else {
                motion::hover_blend(
                    key,
                    crate::kit::theme::wash(0.0),
                    crate::kit::theme::wash(0.14),
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
            .child(word)
            .children(tag)
            .children(badge)
            .child(
                icon(icons::ALT_ARROW_UP_DOWN)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(motion::hover_blend(
                        key,
                        theme.text_muted.opacity(0.6),
                        theme.text_muted,
                    )),
            );
        let current_id = current.map(|line| line.bot.id);
        if let Some(list) = self.render_switcher_list(&lines, current_id.as_deref(), theme, cx) {
            trigger = trigger.child(popover::anchored_menu_below_gap(
                "bot-switcher-menu",
                list,
                self.menus.bot_switcher.closing_since(),
                4.0,
            ));
        }
        trigger.into_any_element()
    }

    /// The open list: Sessions, then every workspace by name with what it
    /// is doing, filtered by the search field once there are enough, and
    /// New workspace at the foot. `current` is the workspace shown, `None`
    /// in the projects view.
    fn render_switcher_list(
        &self,
        lines: &[BotLine],
        current: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.menus.bot_switcher.get()?;
        let switcher = self.board.switcher.as_ref()?;
        let query = switcher.search.read(cx).text().to_string();
        let shown = picks(lines, &query);
        let highlight = switcher.highlight.min(shown.len().saturating_sub(1));
        let ticker = self.pulse_ticker.entity_id();
        let check = |selected: bool| {
            div().flex_none().w(px(14.0)).when(selected, |el| {
                el.child(
                    icon(icons::CHECK)
                        .size(px(13.0))
                        .text_color(theme.text_muted),
                )
            })
        };
        let rows: Vec<AnyElement> = shown
            .iter()
            .enumerate()
            .map(|(ix, pick)| {
                let line = match pick {
                    Pick::Bot(line) => *line,
                    Pick::Sessions => {
                        let selected = current.is_none();
                        let key = "bot-switcher-sessions";
                        return popover::menu_row_nav(theme, selected, ix == highlight, key)
                            .id(key)
                            .gap(px(8.0))
                            .on_click(cx.listener(|this, _, _, cx| this.show_sessions(cx)))
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(LIST_AVATAR))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        icon(icons::CHAT_ROUND_LINE)
                                            .size(px(14.0))
                                            .text_color(theme.text_muted),
                                    ),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(SESSIONS))
                            .child(check(selected))
                            // The workspaces below it, a section of their own.
                            .when(shown.len() > 1, |el| el.mb(px(4.0)))
                            .into_any_element();
                    }
                };
                let bot = &line.bot;
                let key = format!("bot-switcher-{}", bot.id);
                let selected = Some(bot.id.as_str()) == current;
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
                    .child(check(selected))
                    .into_any_element()
            })
            .collect();
        let mut rows = rows;
        if matches!(shown.first(), Some(Pick::Sessions)) && shown.len() > 1 {
            rows.insert(
                1,
                div()
                    .mb(px(4.0))
                    .h(px(1.0))
                    .bg(crate::kit::theme::hairline(0.06))
                    .into_any_element(),
            );
        }
        let empty = rows.is_empty().then(|| {
            div()
                .px(px(8.0))
                .py(px(8.0))
                .text_size(px(12.5))
                .text_color(theme.text_muted)
                .child(SharedString::from("Nothing by that name"))
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
            .child(SharedString::from("New workspace"));
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
                .when(lines.len() + 1 >= SEARCH_FROM, |el| {
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

    #[test]
    fn sessions_leads_the_list_while_it_matches() {
        let lines = [line("Atlas", 0), line("Scout", 0)];
        let names = |q: &str| -> Vec<String> {
            picks(&lines, q)
                .into_iter()
                .map(|p| match p {
                    Pick::Sessions => SESSIONS.to_string(),
                    Pick::Bot(l) => l.bot.name.clone(),
                })
                .collect()
        };
        assert_eq!(names(""), ["Sessions", "Atlas", "Scout"]);
        assert_eq!(names("s"), ["Sessions", "Scout", "Atlas"]);
        assert_eq!(names("at"), ["Atlas"]);
    }
}
