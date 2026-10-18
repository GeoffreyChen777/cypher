//! The **Work** card (§13.1): running orchestrations with their current step,
//! a step bar and elapsed time, then the last few finished ones with a
//! one-line result. A row opens the orchestration: every step with its
//! project, sessions, state and duration, Stop while it runs, the result
//! once it ends, and its thread, read-only in the panel.

use super::board::{card_corner, card_item, two_lines};
use super::items::{
    action_row, ago, item_row, mark_cell, progress_bar, project_label, prose, section_label,
    session_mark, step_mark, text_block, trailing, work_mark,
};
use super::needs::{number_mark, step_block};
use super::panel::{PanelHead, PanelItem};
use super::*;
use crate::transcript::bot_plan::{PlanPhase, format_span, phase_line, plan_phase, step_span};
use cypher_proto::{BotWork, WorkState};

/// Finished work the card keeps under the running ones.
const CARD_FINISHED: usize = 3;

/// A bot's work as the card and the list sort it: running first (newest
/// start first), then work awaiting approval, then finished work, newest
/// end first. Cancelled proposals only show in the full list.
pub(super) struct WorkLists<'a> {
    pub running: Vec<&'a BotWork>,
    pub proposed: Vec<&'a BotWork>,
    pub finished: Vec<&'a BotWork>,
}

pub(super) fn work_lists<'a>(
    records: &'a cypher_proto::BotRecords,
    bot_id: &'a str,
) -> WorkLists<'a> {
    let mut running: Vec<&BotWork> = Vec::new();
    let mut proposed: Vec<&BotWork> = Vec::new();
    let mut finished: Vec<&BotWork> = Vec::new();
    for work in records.work_of(bot_id) {
        match work.state {
            WorkState::Running => running.push(work),
            WorkState::Proposed => proposed.push(work),
            _ => finished.push(work),
        }
    }
    running.sort_by_key(|w| std::cmp::Reverse(w.started_at.unwrap_or(w.created_at)));
    proposed.sort_by_key(|w| std::cmp::Reverse(w.created_at));
    finished.sort_by_key(|w| std::cmp::Reverse(w.ended_at.unwrap_or(w.created_at)));
    WorkLists {
        running,
        proposed,
        finished,
    }
}

/// A running item's second line: "Step 2 of 4 · Run the tests in web".
pub(super) fn current_step_line(work: &BotWork) -> String {
    let total = work.steps.len();
    match work.current_step() {
        Some((ix, step)) => format!(
            "Step {} of {total} · {}",
            ix + 1,
            transcript::single_line(&step.title)
        ),
        None if total > 0 => format!("Finishing · {total} of {total} steps done"),
        None => "Starting…".to_string(),
    }
}

/// A finished item's one line: its result, else how it ended.
pub(super) fn result_line(work: &BotWork) -> String {
    work.result
        .as_deref()
        .map(|r| transcript::single_line(&super::items::plain_markdown(r)))
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| phase_line(work, 0))
}

impl Shell {
    /// A running item: spinner, title, its current step over a step bar,
    /// and the time it has run.
    fn running_row(
        &self,
        key: String,
        work: &BotWork,
        roomy: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let now = Utc::now().timestamp_millis();
        let item = PanelItem::WorkItem(work.id.clone());
        let selected = self.board.panel.as_ref() == Some(&item);
        let (done, total) = work.progress();
        let elapsed = format_span(now - work.started_at.unwrap_or(work.created_at));
        let stopping = plan_phase(work) == PlanPhase::Stopping;
        if !roomy {
            // On the card: its mark, title and how far it got.
            return card_item(
                &key,
                Some(work_mark(work, self.pulse_ticker.entity_id(), theme)),
                transcript::single_line(&work.title),
                Some(card_corner(
                    if stopping {
                        "stopping".to_string()
                    } else {
                        format!("{done}/{total}")
                    },
                    None,
                    theme,
                )),
                selected,
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)));
        }
        item_row(&key, selected)
            .items_start()
            .py(px(8.0))
            .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
            .child(div().pt(px(2.0)).child(mark_cell(work_mark(
                work,
                self.pulse_ticker.entity_id(),
                theme,
            ))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.0))
                            .line_height(px(17.0))
                            .text_color(theme.text)
                            .child(SharedString::from(transcript::single_line(&work.title))),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.0))
                            .line_height(px(14.0))
                            .text_color(theme.text_muted.opacity(0.85))
                            .child(SharedString::from(if stopping {
                                "Stopping…".to_string()
                            } else {
                                current_step_line(work)
                            })),
                    )
                    .child(
                        div()
                            .pt(px(5.0))
                            .pb(px(2.0))
                            .child(progress_bar(done, total, theme)),
                    ),
            )
            .child(div().pt(px(1.0)).child(trailing(elapsed, theme.text_muted)))
    }

    /// A finished or waiting item: its outcome mark, title and one line.
    fn settled_row(
        &self,
        key: String,
        work: &BotWork,
        roomy: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let now = Utc::now();
        let item = if work.awaiting_approval() {
            PanelItem::Plan(work.id.clone())
        } else {
            PanelItem::WorkItem(work.id.clone())
        };
        let selected = self.board.panel.as_ref() == Some(&item);
        let sub = if work.state == WorkState::Proposed {
            phase_line(work, now.timestamp_millis())
        } else {
            result_line(work)
        };
        if !roomy {
            return card_item(
                &key,
                Some(work_mark(work, self.pulse_ticker.entity_id(), theme)),
                transcript::single_line(&work.title),
                Some(card_corner(
                    ago(work.ended_at.unwrap_or(work.created_at), now),
                    None,
                    theme,
                )),
                selected,
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)));
        }
        item_row(&key, selected)
            .py(px(8.0))
            .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
            .child(mark_cell(work_mark(
                work,
                self.pulse_ticker.entity_id(),
                theme,
            )))
            .child(two_lines(
                transcript::single_line(&work.title),
                sub,
                String::new(),
                theme,
            ))
            .child(trailing(
                ago(work.ended_at.unwrap_or(work.created_at), now),
                theme.text_faint,
            ))
    }

    /// The sidebar card; `None` when there is nothing to show.
    pub(super) fn render_work_card(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (running, finished) = {
            let state = self.state.read(cx);
            let lists = work_lists(&state.bot_records, &bot.id);
            let finished: Vec<BotWork> = lists
                .finished
                .into_iter()
                .filter(|w| w.state != WorkState::Cancelled)
                .take(CARD_FINISHED)
                .cloned()
                .collect();
            let running: Vec<BotWork> = lists.running.into_iter().cloned().collect();
            (running, finished)
        };
        if running.is_empty() && finished.is_empty() {
            return None;
        }
        let header = self.card_header(
            "work",
            "bot-work-header",
            icons::HIERARCHY,
            "Work",
            if running.is_empty() {
                finished.len()
            } else {
                running.len()
            },
            PanelItem::Work,
            None,
            theme,
            cx,
        );
        let mut rows = Vec::new();
        for w in &running {
            rows.push(self.running_row(format!("bot-work-{}", w.id), w, false, theme, cx));
        }
        for w in &finished {
            rows.push(self.settled_row(format!("bot-work-{}", w.id), w, false, theme, cx));
        }
        Some(self.fold_card("work", header, rows, theme))
    }

    /// The detail panel for this card's items; `None` when the item no
    /// longer resolves.
    pub(super) fn render_work_panel(
        &mut self,
        bot: &Bot,
        item: &PanelItem,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        match item {
            PanelItem::Work => Some(self.render_work_list(bot, theme, cx)),
            PanelItem::WorkItem(id) => self.render_work_item(id, theme, cx),
            PanelItem::Thread(id) => self.render_thread(id, theme, cx),
            _ => None,
        }
    }

    fn render_work_list(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (PanelHead, gpui::Div) {
        let (running, proposed, finished) = {
            let state = self.state.read(cx);
            let lists = work_lists(&state.bot_records, &bot.id);
            let own = |list: Vec<&BotWork>| list.into_iter().cloned().collect::<Vec<_>>();
            (own(lists.running), own(lists.proposed), own(lists.finished))
        };
        let head = PanelHead {
            glyph: icons::HIERARCHY,
            tint: None,
            title: "Work".into(),
            subtitle: match running.len() {
                0 => format!("Nothing running for {}", bot.name),
                1 => "1 task running".into(),
                n => format!("{n} tasks running"),
            },
        };
        let mut body = div().p(px(8.0)).flex().flex_col().gap(px(1.0));
        if !running.is_empty() {
            body =
                body.child(section_label("Running", theme))
                    .children(running.iter().map(|w| {
                        self.running_row(format!("bot-panel-work-{}", w.id), w, true, theme, cx)
                    }));
        }
        if !proposed.is_empty() {
            body =
                body.child(section_label("Proposed", theme))
                    .children(proposed.iter().map(|w| {
                        self.settled_row(format!("bot-panel-work-{}", w.id), w, true, theme, cx)
                    }));
        }
        if !finished.is_empty() {
            body =
                body.child(section_label("Finished", theme))
                    .children(finished.iter().map(|w| {
                        self.settled_row(format!("bot-panel-work-{}", w.id), w, true, theme, cx)
                    }));
        }
        if running.is_empty() && proposed.is_empty() && finished.is_empty() {
            body = body.child(div().py(px(10.0)).child(prose(
                "No work yet. Ask it for something that takes a few steps.".into(),
                theme.text_muted,
            )));
        }
        // The full log lives here, not in a card of its own.
        body =
            body.child(div().px(px(10.0)).pt(px(12.0)).text_size(px(12.0)).child(
                super::parts::link("bot-work-all-activity", "View all activity", theme).on_click(
                    cx.listener(|this, _, _, cx| this.open_panel(PanelItem::Activity, cx)),
                ),
            ));
        (head, body)
    }

    /// One orchestration in full.
    fn render_work_item(
        &self,
        work_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let now = Utc::now();
        let now_ms = now.timestamp_millis();
        let (work, steps, thread_listed) = {
            let state = self.state.read(cx);
            let work = state
                .bot_records
                .work
                .iter()
                .find(|w| w.id == work_id)?
                .clone();
            let steps: Vec<_> = work
                .steps
                .iter()
                .map(|step| {
                    let project = step
                        .project
                        .as_deref()
                        .and_then(|id| project_label(state, id, theme));
                    let sessions: Vec<(String, Option<(String, ChatIndicator)>)> = step
                        .sessions
                        .iter()
                        .map(|id| {
                            let chat = state.chats.iter().find(|c| c.id == *id).map(|chat| {
                                (
                                    transcript::single_line(
                                        chat.title.as_deref().unwrap_or("New session"),
                                    ),
                                    state.display_status_for(chat, now),
                                )
                            });
                            (id.clone(), chat)
                        })
                        .collect();
                    (project, sessions)
                })
                .collect();
            let thread_listed = work
                .thread_chat_id
                .as_deref()
                .is_some_and(|id| state.chats.iter().any(|c| c.id == id));
            (work, steps, thread_listed)
        };
        let phase = plan_phase(&work);
        let head = PanelHead {
            glyph: icons::HIERARCHY,
            tint: (!matches!(phase, PlanPhase::Running | PlanPhase::Starting))
                .then_some(phase.tint(theme)),
            title: work.title.clone(),
            subtitle: phase_line(&work, now_ms),
        };
        let ticker = self.pulse_ticker.entity_id();
        let mut body = div().p(px(8.0)).flex().flex_col();
        if matches!(phase, PlanPhase::Running | PlanPhase::Stopping) {
            let (done, total) = work.progress();
            body = body.child(
                div()
                    .px(px(8.0))
                    .pt(px(8.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().flex_1().child(progress_bar(done, total, theme)))
                    .child(trailing(format!("{done} of {total}"), theme.text_muted)),
            );
        }
        if !work.instruction.trim().is_empty() {
            body = body
                .child(section_label("You asked", theme))
                .child(text_block(work.instruction.clone(), theme));
        }
        let numbered = matches!(
            phase,
            PlanPhase::Awaiting
                | PlanPhase::Starting
                | PlanPhase::Cancelling
                | PlanPhase::Cancelled
        );
        body = body.child(section_label("Steps", theme));
        for (ix, (step, (project, sessions))) in work.steps.iter().zip(steps).enumerate() {
            let mark = if numbered {
                number_mark(ix + 1, theme)
            } else {
                step_mark(step.state, ticker, theme)
            };
            let span = (!numbered).then(|| step_span(step, now_ms)).flatten();
            body = body.child(step_block(step, project, mark, theme).children(
                span.map(|span| div().pt(px(1.0)).child(trailing(span, theme.text_faint))),
            ));
            for (chat_id, chat) in sessions {
                let key = format!("bot-work-session-{chat_id}");
                // Its mark sits under the step's title.
                let row = item_row(&key, false).ml(px(24.0)).py(px(4.0));
                body = body.child(match chat {
                    Some((title, status)) => {
                        let open = chat_id.clone();
                        row.on_click(
                            cx.listener(move |this, _, _, cx| this.open_chat(open.clone(), cx)),
                        )
                        .child(mark_cell(session_mark(status, ticker, theme)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child(SharedString::from(title)),
                        )
                        .child(
                            icon(icons::ALT_ARROW_RIGHT)
                                .size(px(10.0))
                                .flex_none()
                                .text_color(theme.text_faint.opacity(motion::hover_t(&key))),
                        )
                    }
                    None => row
                        .cursor_default()
                        .child(mark_cell(session_mark(ChatIndicator::Idle, ticker, theme)))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(theme.text_muted)
                                .child(SharedString::from("A session not on this device yet")),
                        ),
                });
            }
        }
        if phase.ended()
            && let Some(result) = work.result.clone().filter(|r| !r.trim().is_empty())
        {
            body = body
                .child(section_label("Result", theme))
                .child(text_block(result, theme));
        }
        let mut actions = action_row();
        let mut any = false;
        if phase.awaiting() {
            let plan = PanelItem::Plan(work.id.clone());
            actions = actions.child(
                popover::btn_primary(theme, "Review the plan")
                    .id("bot-work-review-plan")
                    .on_click(cx.listener(move |this, _, _, cx| this.open_panel(plan.clone(), cx))),
            );
            any = true;
        }
        if phase.running() {
            actions = actions.child(self.stop_button("bot-work-stop", &work.id, theme, cx));
            any = true;
        }
        if thread_listed {
            let thread = PanelItem::Thread(work.thread_chat_id.clone().unwrap_or_default());
            actions = actions.child(
                popover::btn_ghost(theme, "View session", "bot-work-thread")
                    .id("bot-work-thread")
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.open_panel(thread.clone(), cx)),
                    ),
            );
            any = true;
        }
        // Finished work can go (proposed or running work is cancelled or
        // stopped first).
        if phase.ended() {
            let (id, name) = (work.id.clone(), work.title.clone());
            actions = actions.child(div().flex_1()).child(
                parts::action(
                    "bot-work-delete",
                    icons::TRASH_BIN_MINIMALISTIC,
                    "Delete",
                    theme.danger,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let (id, name) = (id.clone(), name.clone());
                    this.ask_confirm(parts::Confirm::DeleteWork { id, name }, cx);
                })),
            );
            any = true;
        }
        if let Some(stopped) = work
            .stop_requested_at
            .filter(|_| phase == PlanPhase::Stopping)
        {
            let when = match ago(stopped, now).as_str() {
                "now" => "just now".to_string(),
                ago => format!("{ago} ago"),
            };
            body = body.child(div().pt(px(10.0)).child(prose(
                format!("Stop requested {when}; waiting for its device."),
                theme.text_muted,
            )));
        }
        Some((head, body.when(any, |el| el.child(actions))))
    }

    /// The orchestration's thread, read-only, with the way back to its work.
    /// The panel draws its body unscrolled: the transcript scrolls itself.
    /// A hidden thread's transcript, read-only, with a way back to what
    /// owns it: its work item, routine or connection.
    fn render_thread(
        &mut self,
        thread: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let (subtitle, back_item, back_label) = {
            let state = self.state.read(cx);
            let chat = state.chats.iter().find(|c| c.id == thread)?;
            let marker = chat.bot.as_ref()?;
            let records = &state.bot_records;
            match &marker.thread {
                cypher_proto::BotThread::Work { work_id } => {
                    let work = records.work.iter().find(|w| &w.id == work_id)?;
                    (
                        work.title.clone(),
                        PanelItem::WorkItem(work.id.clone()),
                        "Back to the work",
                    )
                }
                cypher_proto::BotThread::Routine { routine_id }
                | cypher_proto::BotThread::RoutineSession { routine_id } => {
                    let routine = records.routines.iter().find(|r| &r.id == routine_id)?;
                    (
                        routine.name.clone(),
                        PanelItem::Routine(routine.id.clone()),
                        "Back to the routine",
                    )
                }
                cypher_proto::BotThread::Peer { connection_id } => {
                    let connection = records
                        .connections
                        .iter()
                        .find(|c| &c.id == connection_id)?;
                    (
                        format!("Questions from {}", connection.peer.name),
                        PanelItem::Connection(connection.id.clone()),
                        "Back to the connection",
                    )
                }
                cypher_proto::BotThread::Main | cypher_proto::BotThread::Unknown => return None,
            }
        };
        self.peek(thread, true, cx);
        let view = self.board.peek.as_ref()?.transcript.clone()?;
        let head = PanelHead {
            glyph: icons::CHAT_ROUND_LINE,
            tint: None,
            title: "Session".into(),
            subtitle,
        };
        let back_key = "bot-thread-back";
        let back = div()
            .id(back_key)
            .flex_none()
            .mx(px(8.0))
            .mt(px(6.0))
            .px(px(8.0))
            .h(px(26.0))
            .rounded(px(7.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .text_size(px(12.0))
            .text_color(motion::hover_blend(back_key, theme.text_muted, theme.text))
            .bg(motion::hover_blend(
                back_key,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.06),
            ))
            .on_hover(motion::hover_listener(back_key))
            .on_click(cx.listener(move |this, _, _, cx| this.open_panel(back_item.clone(), cx)))
            .child(
                icon(icons::ALT_ARROW_LEFT)
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            )
            .child(SharedString::from(back_label));
        let body = div()
            .size_full()
            .flex()
            .flex_col()
            .child(back)
            .child(div().flex_1().min_h_0().child(view));
        Some((head, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::{StepState, WorkStep};

    fn step(title: &str, state: StepState) -> WorkStep {
        WorkStep {
            id: title.into(),
            title: title.into(),
            project: None,
            detail: None,
            state,
            sessions: vec![],
            note: None,
            started_at: None,
            ended_at: None,
        }
    }

    fn work(id: &str, state: WorkState, at: i64) -> BotWork {
        BotWork {
            id: id.into(),
            bot_id: "b".into(),
            title: id.into(),
            instruction: String::new(),
            summary: None,
            steps: vec![],
            state,
            chat_id: "c".into(),
            tool_call_id: None,
            thread_chat_id: None,
            created_at: at,
            started_at: Some(at),
            ended_at: Some(at),
            result: None,
            decision: None,
            stop_requested_at: None,
        }
    }

    #[test]
    fn work_sorts_into_running_proposed_and_finished() {
        let mut records = cypher_proto::BotRecords::default();
        let mut other = work("other", WorkState::Running, 9);
        other.bot_id = "x".into();
        records.work = vec![
            work("old-run", WorkState::Running, 1),
            work("new-run", WorkState::Running, 5),
            work("plan", WorkState::Proposed, 2),
            work("done", WorkState::Done, 3),
            work("failed", WorkState::Failed, 4),
            other,
        ];
        let lists = work_lists(&records, "b");
        let ids = |list: &[&BotWork]| list.iter().map(|w| w.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&lists.running), ["new-run", "old-run"]);
        assert_eq!(ids(&lists.proposed), ["plan"]);
        assert_eq!(ids(&lists.finished), ["failed", "done"]);
    }

    #[test]
    fn lines_say_the_current_step_and_the_result() {
        let mut running = work("w", WorkState::Running, 0);
        running.steps = vec![
            step("Fix rounding", StepState::Done),
            step("Run the tests\nin web", StepState::Running),
            step("Open a PR", StepState::Pending),
        ];
        assert_eq!(
            current_step_line(&running),
            "Step 2 of 3 · Run the tests in web"
        );
        running
            .steps
            .iter_mut()
            .for_each(|s| s.state = StepState::Done);
        assert_eq!(current_step_line(&running), "Finishing · 3 of 3 steps done");
        let mut done = work("w", WorkState::Done, 0);
        done.ended_at = Some(60_000);
        assert_eq!(result_line(&done), "Done · took 1m");
        done.result = Some("Opened PR #42\nwith the fix".into());
        assert_eq!(result_line(&done), "Opened PR #42 with the fix");
    }
}
