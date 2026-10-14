//! A bot's conversation tab: the header that says who you're talking to and
//! what it can see, and the start screen a new bot opens on. The tab's other
//! differences live where they apply (the rail drops the terminal and the
//! dock, the composer asks "Ask <bot>…", the transcript names bot tool
//! calls).

use super::super::session::SlotId;
use super::*;

/// The bot header's height, taken from the top of the chat column.
pub(in crate::shell) const BOT_HEADER_H: f32 = 52.0;

/// What `bot` can see, in a few words: "Billing · reads conversations".
pub(in crate::shell) fn access_summary(bot: &Bot, state: &crate::state::AppState) -> String {
    let scope = &bot.scope;
    let projects = match &scope.projects {
        ProjectGrant::All => "All projects".to_string(),
        ProjectGrant::None => String::new(),
        ProjectGrant::Selected { space_ids } => {
            let names: Vec<String> = space_ids
                .iter()
                .filter_map(|id| state.space_row(id))
                .map(|s| s.display_name().to_string())
                .collect();
            match names.as_slice() {
                [] => String::new(),
                [one] => one.clone(),
                [first, second] => format!("{first} and {second}"),
                many => format!("{} projects", many.len()),
            }
        }
    };
    let sessions = scope.include_sessions.len();
    let reach = match (projects.is_empty(), sessions) {
        (true, 0) => return "No access yet".to_string(),
        (true, n) => format!("{n} shared session{}", if n == 1 { "" } else { "s" }),
        (false, 0) => projects,
        (false, n) => format!("{projects} + {n} session{}", if n == 1 { "" } else { "s" }),
    };
    let depth = if scope.transcripts {
        "reads conversations"
    } else {
        "titles and status only"
    };
    format!("{reach} · {depth}")
}

/// Questions a new bot offers, fitted to what it can see.
fn suggestions(bot: &Bot, state: &crate::state::AppState) -> Vec<String> {
    let mut out = vec![
        "What did I work on today?".to_string(),
        "Which sessions are waiting on me?".to_string(),
    ];
    let first_project = state
        .spaces_sorted()
        .into_iter()
        .find(|s| bot.scope.sees_space(&s.id))
        .map(|s| s.display_name().to_string());
    if let Some(project) = first_project {
        out.push(format!("Where do things stand in {project}?"));
    }
    out.push("What's still unfinished across my projects?".to_string());
    out
}

impl Shell {
    /// The bot whose conversation the slot shows.
    pub(in crate::shell) fn slot_bot(&self, sid: SlotId, cx: &App) -> Option<Bot> {
        let slot = self.tiles.slots.get(&sid)?;
        let chat_id = slot.tab.chat_id()?;
        slot.state.read(cx).bot_for_chat(chat_id).cloned()
    }

    /// Whether the slot shows a bot's conversation (true even once the bot
    /// row is gone: the chat stays a bot chat).
    pub(in crate::shell) fn slot_is_bot_chat(&self, sid: SlotId, cx: &App) -> bool {
        let Some(slot) = self.tiles.slots.get(&sid) else {
            return false;
        };
        let Some(chat_id) = slot.tab.chat_id() else {
            return false;
        };
        slot.state
            .read(cx)
            .chats
            .iter()
            .any(|c| c.id == chat_id && c.is_bot())
    }

    /// The bar over a bot's conversation: avatar, name, host, what it can
    /// see, and its settings.
    pub(in crate::shell) fn render_bot_header(
        &mut self,
        sid: SlotId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let bot = self.slot_bot(sid, cx)?;
        let theme = crate::appearance::chat_style::theme(cx);
        let (host, access) = {
            let state = self.tiles.slots.get(&sid)?.state.read(cx);
            (
                state
                    .device_name(&bot.host_device_id)
                    .unwrap_or("an unknown device")
                    .to_string(),
                access_summary(&bot, state),
            )
        };
        let avatar = crate::appearance::bot_style::bot_avatar(
            &bot.name,
            bot.icon.as_deref(),
            bot.color.as_deref(),
            30.0,
            &theme,
        );
        let settings_key = format!("bot-header-settings-{sid}");
        let bot_id = bot.id.clone();
        Some(
            div()
                .id(("bot-header", sid))
                .h(px(BOT_HEADER_H))
                .flex_none()
                .w_full()
                .pl(px(16.0))
                // Clear of the session rail, which floats over the column's
                // top-right corner.
                .pr(px(super::super::dock::RAIL_WIDTH + 6.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .border_b_1()
                .border_color(crate::kit::theme::hairline(0.06))
                .child(avatar)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(13.5))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme.text)
                                        .child(SharedString::from(bot.name.clone())),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .px(px(5.0))
                                        .rounded(px(4.0))
                                        .bg(crate::kit::theme::ink(0.06))
                                        .text_size(px(10.0))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.text_muted)
                                        .child(SharedString::from("BOT")),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(5.0))
                                .min_w_0()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .child(
                                    icon(icons::SHIELD_CHECK)
                                        .size(px(12.0))
                                        .flex_none()
                                        .text_color(theme.text_muted.opacity(0.8)),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .child(SharedString::from(format!("{access} · on {host}"))),
                                ),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(settings_key.clone()))
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(9.0))
                        .py(px(5.0))
                        .rounded(px(8.0))
                        .cursor_pointer()
                        .text_size(px(12.0))
                        .text_color(motion::hover_blend(
                            &settings_key,
                            theme.text_muted,
                            theme.text,
                        ))
                        .bg(motion::hover_blend(
                            &settings_key,
                            crate::kit::theme::wash(0.0),
                            crate::kit::theme::wash(0.10),
                        ))
                        .on_hover(motion::hover_listener(settings_key.clone()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_bot_dialog(Some(bot_id.clone()), cx)
                        }))
                        .child(
                            icon(icons::TUNING)
                                .size(px(13.0))
                                .text_color(theme.text_muted),
                        )
                        .child(SharedString::from("Settings")),
                )
                .into_any_element(),
        )
    }

    /// Whether the slot's bot conversation has nothing in it yet: the start
    /// screen shows instead of an empty transcript.
    pub(in crate::shell) fn bot_chat_is_fresh(&self, sid: SlotId, cx: &App) -> bool {
        let Some(slot) = self.tiles.slots.get(&sid) else {
            return false;
        };
        let state = slot.state.read(cx);
        let Some(chat) = state.selected_chat_row() else {
            return false;
        };
        chat.is_bot()
            && chat.last_message_at.is_none()
            && state.transcript.is_empty()
            && state.pending_echoes().is_empty()
            && !slot.composer.read(cx).is_sending()
    }

    /// A new bot's start screen: who it is, what it can see, and questions
    /// to start with (a click asks one).
    pub(in crate::shell) fn render_bot_start(
        &mut self,
        sid: SlotId,
        stack_h: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let bot = self.slot_bot(sid, cx)?;
        let theme = crate::appearance::chat_style::theme(cx);
        let (access, host, questions, has_access) = {
            let state = self.tiles.slots.get(&sid)?.state.read(cx);
            let has_access = !matches!(bot.scope.projects, ProjectGrant::None)
                || !bot.scope.include_sessions.is_empty();
            (
                access_summary(&bot, state),
                state
                    .device_name(&bot.host_device_id)
                    .unwrap_or("an unknown device")
                    .to_string(),
                suggestions(&bot, state),
                has_access,
            )
        };
        let avatar = crate::appearance::bot_style::bot_avatar(
            &bot.name,
            bot.icon.as_deref(),
            bot.color.as_deref(),
            64.0,
            &theme,
        );
        let hairline = crate::kit::theme::hairline(0.08);
        let mut column = div()
            .w(px(440.0))
            .max_w_full()
            .px(px(12.0))
            .flex()
            .flex_col()
            .items_center()
            .child(avatar)
            .child(
                div()
                    .mt(px(14.0))
                    .text_size(px(20.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from(bot.name.clone())),
            )
            .child(
                div()
                    .mt(px(4.0))
                    .text_size(px(13.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!(
                        "Answers questions about your work · runs on {host}"
                    ))),
            )
            .child(
                div()
                    .mt(px(12.0))
                    .px(px(10.0))
                    .py(px(5.0))
                    .rounded_full()
                    .border_1()
                    .border_color(hairline)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(
                        icon(icons::SHIELD_CHECK)
                            .size(px(13.0))
                            .text_color(theme.text_muted),
                    )
                    .child(SharedString::from(format!("{access} · read-only"))),
            );
        if has_access {
            let rows = questions.into_iter().enumerate().map(|(ix, question)| {
                let key = format!("bot-start-q-{sid}-{ix}");
                let ask = question.clone();
                div()
                    .id(SharedString::from(key.clone()))
                    .w_full()
                    .px(px(14.0))
                    .py(px(10.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(hairline)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .cursor_pointer()
                    .bg(motion::hover_blend(
                        &key,
                        crate::kit::theme::wash(0.0),
                        crate::kit::theme::wash(0.08),
                    ))
                    .on_hover(motion::hover_listener(key.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(composer) =
                            this.tiles.slots.get(&sid).map(|s| s.composer.clone())
                        {
                            composer.update(cx, |c, cx| c.send_text(&ask, window, cx));
                        }
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.5))
                            .text_color(theme.text)
                            .child(SharedString::from(question)),
                    )
                    .child(
                        icon(icons::ARROW_RIGHT)
                            .size(px(13.0))
                            .text_color(theme.text_faint),
                    )
            });
            column = column
                .child(
                    div()
                        .mt(px(28.0))
                        .mb(px(8.0))
                        .w_full()
                        .text_size(px(11.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text_muted.opacity(0.7))
                        .child(SharedString::from("Try asking")),
                )
                .child(div().w_full().flex().flex_col().gap(px(6.0)).children(rows));
        } else {
            let bot_id = bot.id.clone();
            column = column
                .child(
                    div()
                        .mt(px(24.0))
                        .text_size(px(13.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from("It can't see any of your sessions yet.")),
                )
                .child(
                    popover::btn_primary(&theme, "Choose what it can see")
                        .id(("bot-start-permissions", sid))
                        .mt(px(12.0))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_bot_dialog(Some(bot_id.clone()), cx)
                        })),
                );
        }
        Some(
            div()
                .id(("bot-start", sid))
                .size_full()
                .pb(px(stack_h))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(motion::fade_in(("bot-start-in", sid), column))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::BotScope;

    fn bot(scope: BotScope) -> Bot {
        Bot {
            id: "b".into(),
            name: "Bot".into(),
            icon: None,
            color: None,
            host_device_id: "dev".into(),
            chat_id: "c".into(),
            instructions: None,
            scope,
            created_at: Utc::now(),
        }
    }

    fn state() -> crate::state::AppState {
        let mut state = crate::state::AppState::new();
        state.apply_spaces(
            ["Billing", "Auth", "Web"]
                .iter()
                .map(|name| cypher_proto::Space {
                    id: name.to_lowercase(),
                    name: Some((*name).into()),
                    ..crate::test_fixtures::space()
                })
                .collect(),
        );
        state
    }

    fn selected(ids: &[&str]) -> ProjectGrant {
        ProjectGrant::Selected {
            space_ids: ids.iter().map(|id| (*id).into()).collect(),
        }
    }

    #[test]
    fn access_reads_in_a_few_words() {
        let state = state();
        let summary = |scope| access_summary(&bot(scope), &state);
        assert_eq!(summary(BotScope::default()), "No access yet");
        assert_eq!(
            summary(BotScope {
                projects: ProjectGrant::All,
                ..Default::default()
            }),
            "All projects · reads conversations"
        );
        assert_eq!(
            summary(BotScope {
                projects: selected(&["billing", "gone"]),
                transcripts: false,
                ..Default::default()
            }),
            "Billing · titles and status only"
        );
        assert_eq!(
            summary(BotScope {
                projects: selected(&["billing", "auth", "web"]),
                include_sessions: vec!["x".into()],
                ..Default::default()
            }),
            "3 projects + 1 session · reads conversations"
        );
        assert_eq!(
            summary(BotScope {
                include_sessions: vec!["x".into(), "y".into()],
                ..Default::default()
            }),
            "2 shared sessions · reads conversations"
        );
    }

    #[test]
    fn suggestions_name_a_visible_project() {
        let state = state();
        let asks = suggestions(
            &bot(BotScope {
                projects: selected(&["web"]),
                ..Default::default()
            }),
            &state,
        );
        assert!(asks.contains(&"Where do things stand in Web?".to_string()));
        let asks = suggestions(&bot(BotScope::default()), &state);
        assert!(asks.iter().all(|q| !q.contains("stand in")));
    }
}
