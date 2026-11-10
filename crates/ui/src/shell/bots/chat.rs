//! A bot's conversation on the Botboard: the note over the composer saying
//! what the bot can see, and the start screen a new bot opens on. Its other
//! differences live where they apply (the card's header wears the bot's
//! avatar, the slot has no terminal and no dock, the composer asks "Ask
//! <bot>…", the transcript names bot tool calls).

use super::super::session::SlotId;
use super::*;

/// What `bot` can see and do, in a few words: "Billing · reads
/// conversations", or with capabilities "3 projects · works directly ·
/// starts sessions".
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
    let caps = &bot.capabilities;
    let sessions = match (caps.coordinate, caps.steer) {
        (true, true) => Some("starts and steers sessions"),
        (true, false) => Some("starts sessions"),
        (false, true) => Some("steers sessions"),
        (false, false) => None,
    };
    let acts: Vec<&str> = caps
        .work_directly
        .then_some("works directly")
        .into_iter()
        .chain(sessions)
        .collect();
    // What it may do says more than how deep it reads, which only stays
    // when it is the narrow kind.
    let depth = match (scope.transcripts, acts.is_empty()) {
        (true, false) => None,
        (true, true) => Some("reads conversations"),
        (false, _) => Some("titles and status only"),
    };
    std::iter::once(reach.as_str())
        .chain(depth)
        .chain(acts)
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Questions a new bot offers, fitted to what it can see, each with its
/// glyph.
fn suggestions(bot: &Bot, state: &crate::state::AppState) -> Vec<(&'static str, String)> {
    let mut out = vec![
        (icons::PULSE, "What did I work on today?".to_string()),
        (
            icons::QUESTION_CIRCLE,
            "Which sessions are waiting on me?".to_string(),
        ),
    ];
    let first_project = state
        .spaces_sorted()
        .into_iter()
        .find(|s| bot.scope.sees_space(&s.id))
        .map(|s| s.display_name().to_string());
    if let Some(project) = first_project {
        out.push((
            icons::FOLDER,
            format!("Where do things stand in {project}?"),
        ));
    }
    out.push((
        icons::CHECKLIST,
        "What's still unfinished across my projects?".to_string(),
    ));
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

    /// The status strip's note in a bot's conversation: what the bot can
    /// see, in a few words; a click opens its settings.
    pub(in crate::shell) fn render_bot_access(
        &self,
        sid: SlotId,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let bot = self.slot_bot(sid, cx)?;
        let access = access_summary(&bot, self.tiles.slots.get(&sid)?.state.read(cx));
        let key = format!("bot-access-{sid}");
        let tint = motion::hover_blend(&key, theme.text_muted.opacity(0.75), theme.text);
        Some(
            div()
                .id(SharedString::from(key.clone()))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(5.0))
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .text_size(px(11.5))
                .text_color(tint)
                .on_hover(motion::hover_listener(key.clone()))
                .tooltip(|_, cx| {
                    cx.new(|_| super::super::session::FindTooltip("Workspace permissions".into()))
                        .into()
                })
                .on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.open_bot_dialog(Some(bot.id.clone()), cx)
                    }),
                )
                .child(icon(icons::SHIELD_CHECK).size(px(12.0)).text_color(tint))
                .child(SharedString::from(access))
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
        let (host, questions, has_access) = {
            let state = self.tiles.slots.get(&sid)?.state.read(cx);
            let has_access = !matches!(bot.scope.projects, ProjectGrant::None)
                || !bot.scope.include_sessions.is_empty();
            (
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
        let mut column = div()
            .w(px(520.0))
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
                        "{} · runs on {host}",
                        bot_role(&bot.capabilities)
                    ))),
            );
        if has_access {
            // Suggested prompts: a 2×2 grid of soft cards (the familiar
            // empty-chat pattern); a click asks one.
            let cards: Vec<_> = questions
                .into_iter()
                .enumerate()
                .map(|(ix, (glyph, question))| {
                    let key = format!("bot-start-q-{sid}-{ix}");
                    let ask = question.clone();
                    div()
                        .id(SharedString::from(key.clone()))
                        .flex_1()
                        .min_w_0()
                        .h(px(76.0))
                        .p(px(12.0))
                        .rounded(px(12.0))
                        .flex()
                        .flex_col()
                        .justify_between()
                        .cursor_pointer()
                        .bg(motion::hover_blend(
                            &key,
                            crate::kit::theme::ink(0.035),
                            crate::kit::theme::ink(0.07),
                        ))
                        .on_hover(motion::hover_listener(key.clone()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(composer) =
                                this.tiles.slots.get(&sid).map(|s| s.composer.clone())
                            {
                                composer.update(cx, |c, cx| c.send_text(&ask, window, cx));
                            }
                        }))
                        .child(icon(glyph).size(px(15.0)).text_color(theme.text_muted))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .line_height(px(17.0))
                                .text_color(theme.text)
                                .child(SharedString::from(question)),
                        )
                })
                .collect();
            let mut grid = div().mt(px(28.0)).w_full().flex().flex_col().gap(px(8.0));
            let mut cards = cards.into_iter();
            while let Some(first) = cards.next() {
                let mut row = div().w_full().flex().flex_row().gap(px(8.0)).child(first);
                row = match cards.next() {
                    Some(second) => row.child(second),
                    None => row.child(div().flex_1()),
                };
                grid = grid.child(row);
            }
            column = column.child(grid);
        } else {
            let bot_id = bot.id.clone();
            column = column
                .child(
                    div()
                        .mt(px(20.0))
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

/// What the bot does, in a few words, from what it may do.
fn bot_role(capabilities: &cypher_proto::BotCapabilities) -> &'static str {
    if capabilities.coordinate {
        "Plans and does work across your projects"
    } else if capabilities.work_directly {
        "Answers questions and works in your projects"
    } else {
        "Answers questions about your work"
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
            capabilities: Default::default(),
            paused: false,
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
    fn access_names_what_the_bot_may_do() {
        let state = state();
        let mut acting = bot(BotScope {
            projects: selected(&["billing", "auth", "web"]),
            ..Default::default()
        });
        acting.capabilities = cypher_proto::BotCapabilities {
            work_directly: true,
            coordinate: true,
            steer: false,
        };
        assert_eq!(
            access_summary(&acting, &state),
            "3 projects · works directly · starts sessions"
        );
        acting.scope.transcripts = false;
        acting.capabilities = cypher_proto::BotCapabilities::new_bot();
        assert_eq!(
            access_summary(&acting, &state),
            "3 projects · titles and status only · works directly · starts sessions"
        );
        acting.scope = BotScope::default();
        assert_eq!(access_summary(&acting, &state), "No access yet");
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
        assert!(
            asks.iter()
                .any(|(_, q)| q == "Where do things stand in Web?")
        );
        let asks = suggestions(&bot(BotScope::default()), &state);
        assert!(asks.iter().all(|(_, q)| !q.contains("stand in")));
    }
}
