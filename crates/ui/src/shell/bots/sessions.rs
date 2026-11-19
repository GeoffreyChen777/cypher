//! Two pages of the detail panel that show a conversation's content
//! (§13.1). **A session**, live: one of the workspace's sessions in a
//! project (a session a plan started, a project routine's), with its
//! transcript and its own composer, so the owner reads, steers, stops and
//! answers it without leaving the workspace. These sessions are listed
//! nowhere else. **A result notice**, in full: what a routine's run wrote
//! or how a plan ended, opened from the compact card in the chat.

use super::items::{outcome_value, panel_markdown, section_head};
use super::panel::{PanelHead, PanelItem};
use super::*;
use cypher_proto::{BotPost, BotThread, RunOutcome, WorkState};

/// What a session in the panel belongs to: the page its back arrow leads to.
fn session_owner(state: &crate::state::AppState, chat: &cypher_proto::Chat) -> Option<PanelItem> {
    if let Some(work_id) = chat.started_by.as_ref().and_then(|by| by.work_id.as_ref())
        && state.bot_records.work.iter().any(|w| &w.id == work_id)
    {
        return Some(PanelItem::WorkItem(work_id.clone()));
    }
    match chat.bot.as_ref().map(|marker| &marker.thread) {
        Some(BotThread::RoutineSession { routine_id })
            if state
                .bot_records
                .routines
                .iter()
                .any(|r| &r.id == routine_id) =>
        {
            Some(PanelItem::Routine(routine_id.clone()))
        }
        _ => None,
    }
}

/// A result notice as the chat holds it: who posted it, and its text.
fn find_post(
    entries: &[cypher_doc::SessionMessageEntry],
    entry_id: &str,
) -> Option<(BotPost, String)> {
    entries
        .iter()
        .find(|entry| entry.id == entry_id)?
        .parts
        .iter()
        .find_map(|part| match part {
            cypher_doc::MessagePart::Post { text, from, .. } => Some((from.clone(), text.clone())),
            _ => None,
        })
}

impl Shell {
    /// A result notice's card was clicked in the chat: a plan's opens the
    /// plan's own page while its record is kept, anything else the notice
    /// in full.
    pub(in crate::shell) fn open_bot_post(&mut self, entry_id: &str, cx: &mut Context<Self>) {
        let work = self
            .board_post(entry_id, cx)
            .and_then(|(from, _)| match from {
                BotPost::Work { work_id, .. } => Some(work_id),
                _ => None,
            })
            .filter(|id| {
                self.state
                    .read(cx)
                    .bot_records
                    .work
                    .iter()
                    .any(|w| w.id == *id)
            });
        match work {
            Some(work_id) => self.show_panel(PanelItem::WorkItem(work_id), cx),
            None => self.show_panel(PanelItem::Post(entry_id.to_string()), cx),
        }
    }

    /// A plan card's Details: the plan's page (with its decision while it
    /// waits on one).
    pub(in crate::shell) fn open_bot_work(&mut self, work_id: &str, cx: &mut Context<Self>) {
        let awaiting = self
            .state
            .read(cx)
            .bot_records
            .work
            .iter()
            .find(|w| w.id == work_id)
            .is_some_and(|w| w.awaiting_approval());
        self.show_panel(
            if awaiting {
                PanelItem::Plan(work_id.to_string())
            } else {
                PanelItem::WorkItem(work_id.to_string())
            },
            cx,
        );
    }

    /// The post `entry_id` in the board's conversation.
    fn board_post(&self, entry_id: &str, cx: &App) -> Option<(BotPost, String)> {
        let slot = self.tiles.slots.get(&self.board.slot?)?;
        find_post(&slot.state.read(cx).transcript, entry_id)
    }

    /// One of the workspace's sessions, live: the session view a tile
    /// draws, in the panel's width, with no docks. Its back arrow leads to
    /// the plan or routine it belongs to.
    pub(super) fn render_session_panel(
        &mut self,
        bot: &Bot,
        chat_id: &str,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let (title, place, owner) = {
            let state = self.state.read(cx);
            let chat = state.chats.iter().find(|c| c.id == chat_id)?;
            // Only this workspace's own sessions open here.
            if chat.workspace_session_of() != Some(bot.id.as_str()) {
                return None;
            }
            let place = chat
                .space_id
                .as_deref()
                .and_then(|id| state.space_row(id))
                .map(|space| space.display_name().to_string());
            (
                transcript::single_line(&super::items::plain_markdown(
                    chat.title.as_deref().unwrap_or("New session"),
                )),
                place,
                session_owner(state, chat),
            )
        };
        let sid = self.board.session_slot.filter(|sid| {
            self.tiles
                .slots
                .get(sid)
                .is_some_and(|slot| slot.tab.chat_id() == Some(chat_id))
        })?;
        if let Some(owner) = owner {
            let tip = match owner {
                PanelItem::Routine(_) => "Back to the routine",
                _ => "Back to the plan",
            };
            self.set_panel_back("bot-session-back", tip, owner, cx);
        }
        if let Some(place) = place {
            self.board.panel_title_note = Some(
                div()
                    .min_w_0()
                    .flex_shrink(1.0)
                    .truncate()
                    .text_size(px(12.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(place))
                    .into_any_element(),
            );
        }
        let head = PanelHead {
            glyph: icons::CHAT_ROUND_LINE,
            tint: None,
            title,
        };
        let session = self.render_session(sid, window, cx);
        let body = div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().relative().flex().child(session));
        Some((head, body))
    }

    /// A result notice in full: how the job ended and when, what it wrote,
    /// and the ways on to the routine or plan and to its session.
    pub(super) fn render_post_panel(
        &mut self,
        entry_id: &str,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let (from, text) = self.board_post(entry_id, cx)?;
        let (head, facts, session, owner): (PanelHead, Vec<gpui::Div>, Option<String>, _) =
            match &from {
                BotPost::Routine {
                    routine_id,
                    name,
                    outcome,
                    run_at,
                    thread_chat_id,
                } => {
                    let failed = *outcome == RunOutcome::Failed;
                    let known = self
                        .state
                        .read(cx)
                        .bot_records
                        .routines
                        .iter()
                        .any(|r| r.id == *routine_id);
                    (
                        PanelHead {
                            glyph: icons::CLOCK,
                            tint: failed.then_some(theme.danger),
                            title: transcript::single_line(name),
                        },
                        vec![
                            parts::fact(
                                theme,
                                "Status",
                                outcome_value(
                                    if failed { "Failed" } else { "Done" },
                                    !failed,
                                    theme,
                                ),
                            ),
                            parts::fact(
                                theme,
                                "Ran",
                                SharedString::from(parts::past_full_ms(*run_at)),
                            ),
                        ],
                        thread_chat_id.clone(),
                        known.then(|| {
                            (
                                "bot-post-routine",
                                icons::CLOCK,
                                "Open the routine",
                                PanelItem::Routine(routine_id.clone()),
                            )
                        }),
                    )
                }
                BotPost::Work {
                    title,
                    outcome,
                    started_at,
                    ended_at,
                    ..
                } => {
                    let (word, ok) = match outcome {
                        WorkState::Failed => ("Failed", false),
                        WorkState::Stopped => ("Stopped", false),
                        _ => ("Done", true),
                    };
                    let mut facts = vec![
                        parts::fact(theme, "Status", outcome_value(word, ok, theme)),
                        parts::fact(
                            theme,
                            "Ended",
                            SharedString::from(parts::past_full_ms(*ended_at)),
                        ),
                    ];
                    if let Some(start) = started_at {
                        facts.push(parts::fact(
                            theme,
                            "Took",
                            SharedString::from(crate::transcript::bot_plan::format_span(
                                ended_at.saturating_sub(*start),
                            )),
                        ));
                    }
                    (
                        PanelHead {
                            glyph: icons::HIERARCHY,
                            tint: (*outcome == WorkState::Failed).then_some(theme.danger),
                            title: transcript::single_line(title),
                        },
                        facts,
                        None,
                        None,
                    )
                }
                BotPost::Unknown => return None,
            };
        let mut buttons = Vec::new();
        if let Some((key, glyph, tip, item)) = owner {
            buttons.push(
                super::items::icon_button(key, glyph, tip, theme)
                    .on_click(cx.listener(move |this, _, _, cx| this.show_panel(item.clone(), cx)))
                    .into_any_element(),
            );
        }
        // Its session, while this device still lists it (a Run now's is
        // replaced by the next one).
        let session = session.filter(|id| self.state.read(cx).chats.iter().any(|c| c.id == *id));
        if let Some(session) = session {
            buttons.push(
                super::items::icon_button(
                    "bot-post-session",
                    icons::CHAT_ROUND_LINE,
                    "Open its session",
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_chat(session.clone(), cx)))
                .into_any_element(),
            );
        }
        self.set_panel_actions(buttons);
        let copy = self.copy_button(
            format!("bot-post-copy-{entry_id}"),
            text.clone(),
            1.0,
            theme,
            cx,
        );
        let body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .children(facts)
            .child(super::items::section_rule())
            .child(
                section_head("Result", false, theme)
                    .child(div().flex_1())
                    .child(copy),
            )
            .child(div().px(px(8.0)).pb(px(8.0)).child(panel_markdown(
                &text,
                format!("bot-post-{entry_id}"),
                13.0,
                theme.text,
                theme,
                window,
            )));
        Some((head, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures;

    #[test]
    fn a_post_is_found_by_its_entry() {
        let from = BotPost::Routine {
            routine_id: "r".into(),
            name: "Morning brief".into(),
            outcome: RunOutcome::Done,
            run_at: 5,
            thread_chat_id: None,
        };
        let entries = vec![
            cypher_doc::SessionMessageEntry {
                id: "plain".into(),
                ..test_fixtures::entry()
            },
            cypher_doc::SessionMessageEntry {
                id: "routine-r-5".into(),
                parts: vec![cypher_doc::MessagePart::Post {
                    id: "p0".into(),
                    text: "Three sessions moved.".into(),
                    from: from.clone(),
                }],
                ..test_fixtures::entry()
            },
        ];
        assert_eq!(
            find_post(&entries, "routine-r-5"),
            Some((from, "Three sessions moved.".into()))
        );
        assert_eq!(find_post(&entries, "plain"), None);
        assert_eq!(find_post(&entries, "gone"), None);
    }

    #[test]
    fn a_session_leads_back_to_what_it_belongs_to() {
        let mut state = crate::state::AppState::new();
        state.bot_records.work = vec![test_fixtures::bot_work("w", "b")];
        state.bot_records.routines = vec![test_fixtures::bot_routine("r", "b")];
        let mut worker = test_fixtures::chat();
        worker.started_by = Some(cypher_proto::ChatStartedBy {
            bot_id: "b".into(),
            work_id: Some("w".into()),
        });
        assert_eq!(
            session_owner(&state, &worker),
            Some(PanelItem::WorkItem("w".into()))
        );
        // A plan whose record was pruned leaves the session on its own.
        worker.started_by.as_mut().unwrap().work_id = Some("pruned".into());
        assert_eq!(session_owner(&state, &worker), None);
        let mut routine = test_fixtures::chat();
        routine.bot = Some(cypher_proto::BotChat::thread(
            "b",
            cypher_proto::BotOrigin::Routine,
            BotThread::RoutineSession {
                routine_id: "r".into(),
            },
        ));
        assert_eq!(
            session_owner(&state, &routine),
            Some(PanelItem::Routine("r".into()))
        );
        assert_eq!(session_owner(&state, &test_fixtures::chat()), None);
    }
}
