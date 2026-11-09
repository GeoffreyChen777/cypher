//! The **Work** card (§13.1): running orchestrations with their current step,
//! a step bar and elapsed time, then the last few finished ones with a
//! one-line result. A row opens the orchestration: its status and request,
//! then the plan as one flow (each step with its state, duration and
//! sessions, ending in the result), Stop while it runs, and its thread,
//! read-only in the panel.

use super::board::{card_corner, card_item, two_lines};
use super::items::{
    ago, decision_bar, item_row, mark_cell, progress_bar, project_label, prose, section_label,
    session_mark, step_mark, trailing, work_mark,
};
use super::panel::{PanelHead, PanelItem};
use super::*;
use crate::transcript::bot_plan::{PlanPhase, format_span, phase_line, plan_phase, step_span};
use cypher_proto::{BotWork, StepState, WorkState};

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

/// A step as the flow draws it: its project, and its sessions (id, then
/// title and status when this device lists the chat).
type FlowStep = (
    Option<super::items::ProjectLabel>,
    Vec<(String, Option<(String, ChatIndicator)>)>,
);

/// A session's state in a word, beside its row; none while it sits idle.
fn session_word(status: ChatIndicator, theme: &Theme) -> Option<(&'static str, gpui::Hsla)> {
    match status {
        ChatIndicator::Working => Some(("Working", theme.text_muted)),
        ChatIndicator::AwaitingInput => Some(("Needs input", theme.warning)),
        ChatIndicator::Errored => Some(("Failed", theme.danger)),
        ChatIndicator::Completed => Some(("Done", theme.text_faint)),
        ChatIndicator::Idle => None,
    }
}

/// Whether the page shows the request: there is one, and it says more
/// than the title.
fn shows_request(work: &BotWork) -> bool {
    let norm = |t: &str| t.trim().trim_end_matches('.').to_lowercase();
    !work.instruction.trim().is_empty() && norm(&work.instruction) != norm(&work.title)
}

/// The hover card of a work item's status tile: its state in its tone,
/// its progress while it runs, then when it was proposed, approved,
/// started and ended, and how long it took.
#[derive(Clone)]
struct WorkStatusTip {
    status: String,
    tone: gpui::Hsla,
    progress: Option<(usize, usize)>,
    times: Vec<(&'static str, String)>,
}

impl WorkStatusTip {
    fn of(work: &BotWork, phase: PlanPhase, theme: &Theme) -> Self {
        let now = Utc::now().timestamp_millis();
        let when = parts::past_full_ms;
        let mut times = vec![("Proposed", when(work.created_at))];
        if let Some(decision) = &work.decision {
            times.push((
                if decision.approve {
                    "Approved"
                } else {
                    "Cancelled"
                },
                when(decision.at),
            ));
        }
        if let Some(start) = work.started_at {
            times.push(("Started", when(start)));
        }
        if let Some(end) = work.ended_at {
            times.push(("Ended", when(end)));
        }
        if let Some(start) = work.started_at {
            let (label, until) = match work.ended_at {
                Some(end) => ("Took", end),
                None => ("Running", now),
            };
            times.push((label, format_span(until - start)));
        }
        Self {
            status: phase_line(work, now),
            tone: match phase {
                PlanPhase::Running | PlanPhase::Starting => theme.text,
                _ => phase.tint(theme),
            },
            progress: matches!(phase, PlanPhase::Running | PlanPhase::Stopping)
                .then(|| work.progress()),
            times,
        }
    }
}

impl Render for WorkStatusTip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let rows = self.times.iter().map(|(label, value)| {
            div()
                .flex()
                .flex_row()
                .gap(px(10.0))
                .child(
                    div()
                        .w(px(64.0))
                        .flex_none()
                        .text_color(theme.text_muted)
                        .child(SharedString::from(*label)),
                )
                .child(
                    div()
                        .text_color(theme.text)
                        .child(SharedString::from(value.clone())),
                )
        });
        motion::fade_quick(
            "bot-work-status-tip",
            div()
                .w(px(240.0))
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.surface_raised)
                .shadow_md()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .line_height(px(17.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(self.tone)
                        .child(SharedString::from(self.status.clone())),
                )
                .children(
                    self.progress
                        .map(|(done, total)| progress_bar(done, total, theme)),
                )
                .child(
                    div()
                        .pt(px(6.0))
                        .border_t_1()
                        .border_color(theme.border)
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .text_size(px(11.5))
                        .line_height(px(16.0))
                        .children(rows),
                ),
        )
    }
}

/// A section heading on the work page (Status, Request, Plan): small caps,
/// its content below; a row, so a section can add a note on the right.
fn work_heading(label: &str, first: bool, theme: &Theme) -> gpui::Div {
    div()
        .px(px(8.0))
        .pt(px(if first { 4.0 } else { 14.0 }))
        .pb(px(8.0))
        .min_h(px(20.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_none()
                .text_size(px(11.0))
                .line_height(px(14.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text_muted)
                .child(SharedString::from(label.to_uppercase())),
        )
}

/// The hairline that closes one section of the work page before the next.
fn work_rule() -> gpui::Div {
    div()
        .mx(px(8.0))
        .mt(px(16.0))
        .h(px(1.0))
        .bg(crate::kit::theme::hairline(0.08))
}

/// A step's fold chevron, and the gap before it in the title row.
const CHEVRON_W: f32 = 10.0;
const TITLE_GAP: f32 = 8.0;

/// The rail's 18px cell a node's mark sits centred in.
fn flow_cell() -> gpui::Div {
    div()
        .size(px(18.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
}

/// A step's number while the plan waits, as the chat's plan card draws it:
/// an even ring, the digit nudged to its optical middle.
fn flow_number(n: usize, theme: &Theme) -> AnyElement {
    div()
        .size(px(18.0))
        .rounded_full()
        .border_1()
        .border_color(crate::kit::theme::hairline(0.16))
        .pr(px(1.0))
        .pb(px(1.0))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.0))
        .line_height(px(10.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_muted)
        .child(SharedString::from(n.to_string()))
        .into_any_element()
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
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        match item {
            PanelItem::Work => Some(self.render_work_list(bot, theme, cx)),
            PanelItem::WorkItem(id) => self.render_work_item(id, theme, window, cx),
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

    /// One work item in every phase, the plan to approve included: its
    /// status and what was asked, then [`Self::work_flow`].
    /// Approve / Cancel or Stop sit in the footer; opening its background
    /// session and Delete in the header.
    pub(super) fn render_work_item(
        &mut self,
        work_id: &str,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let now = Utc::now();
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
                                    // A title an agent wrote may carry markdown.
                                    transcript::single_line(
                                        super::items::plain_markdown(
                                            chat.title.as_deref().unwrap_or("New session"),
                                        )
                                        .trim_start_matches(['-', '*', '#', '>', ' ']),
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
                .then(|| phase.tint(theme)),
            title: work.title.clone(),
        };
        // ── header: its background session, and Delete once it ended ──
        let mut buttons = Vec::new();
        if thread_listed {
            let thread = PanelItem::Thread(work.thread_chat_id.clone().unwrap_or_default());
            buttons.push(
                super::items::icon_button(
                    "bot-work-thread",
                    icons::CHAT_ROUND_LINE,
                    "Open its session",
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_panel(thread.clone(), cx)))
                .into_any_element(),
            );
        }
        if phase.ended() {
            let (id, name) = (work.id.clone(), work.title.clone());
            buttons.push(
                super::items::icon_button(
                    "bot-work-delete",
                    icons::TRASH_BIN_MINIMALISTIC,
                    "Delete",
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let (id, name) = (id.clone(), name.clone());
                    this.ask_confirm(parts::Confirm::DeleteWork { id, name }, cx);
                }))
                .into_any_element(),
            );
        }
        self.set_panel_actions(buttons);

        // ── its status: the tile left of the title, details on hover ──
        self.board.panel_mark = Some(self.status_tile(&work, phase, theme));

        // Request and Plan: sections under one kind of heading.
        let mut body = div().p(px(8.0)).flex().flex_col();
        if shows_request(&work) {
            body = body
                .child(work_heading("Request", true, theme))
                .child(div().px(px(8.0)).child(super::items::panel_markdown(
                    &work.instruction,
                    format!("bot-work-request-{}", work.id),
                    13.0,
                    theme.text,
                    theme,
                    window,
                )));
        }
        if !work.steps.is_empty() {
            body = body.child(self.work_flow(&work, steps, phase, theme, window, cx));
        }

        // ── footer: the decision it waits on ──
        if phase.awaiting() {
            let (approve, cancel) = (work.id.clone(), work.id.clone());
            self.set_panel_footer(
                decision_bar(Some("Nothing runs until you approve."), theme)
                    .child(
                        popover::btn_ghost(theme, "Cancel plan", "bot-plan-cancel")
                            .id("bot-plan-cancel")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mutate(
                                    serde_json::json!({
                                        "op": "resolveBotPlan", "workId": cancel, "approve": false,
                                    }),
                                    cx,
                                )
                            })),
                    )
                    .child(
                        popover::btn_primary(theme, "Approve")
                            .id("bot-plan-approve")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mutate(
                                    serde_json::json!({
                                        "op": "resolveBotPlan", "workId": approve, "approve": true,
                                    }),
                                    cx,
                                )
                            })),
                    ),
            );
        } else if phase.running() {
            let stop = self.stop_button("bot-work-stop", &work.id, theme, cx);
            self.set_panel_footer(
                decision_bar(
                    Some("Stop ends the work and every session it started."),
                    theme,
                )
                .child(stop),
            );
        }
        Some((head, body))
    }

    /// The plan as one flow on one rail: each step (its sessions under it),
    /// then the result the last step leads to. The rail fills in as steps
    /// finish; a project every step shares is named once, over the flow.
    fn work_flow(
        &self,
        work: &BotWork,
        steps: Vec<FlowStep>,
        phase: PlanPhase,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let now_ms = Utc::now().timestamp_millis();
        let ticker = self.pulse_ticker.entity_id();
        let md = |text: &str, key: String, size: f32, tone: gpui::Hsla| {
            super::items::panel_markdown(text, key, size, tone, theme, window)
        };
        let numbered = matches!(
            phase,
            PlanPhase::Awaiting
                | PlanPhase::Starting
                | PlanPhase::Cancelling
                | PlanPhase::Cancelled
        );
        let place = |p: &super::items::ProjectLabel| format!("{} · {}", p.name, p.device);
        let common = steps
            .first()
            .and_then(|(p, _)| p.as_ref().map(|p| (p.glyph, p.tint, place(p))))
            .filter(|(_, _, first)| {
                steps
                    .iter()
                    .all(|(p, _)| p.as_ref().map(place).as_deref() == Some(first.as_str()))
            });

        // ── its heading (where it works on the right), then the summary ──
        let first = !shows_request(work);
        let mut flow = div()
            .flex()
            .flex_col()
            .when(!first, |el| el.child(work_rule()))
            .child(
                work_heading("Plan", first, theme)
                    .child(div().flex_1())
                    .when_some(common.clone(), |el, (glyph, tint, place)| {
                        // The shared project, a link to its page.
                        let key = format!("bot-work-place-{}", work.id);
                        let space = work.steps.first().and_then(|s| s.project.clone());
                        el.child(
                            div()
                                .id(SharedString::from(key.clone()))
                                .min_w_0()
                                .mr(px(-6.0))
                                .px(px(6.0))
                                .h(px(20.0))
                                .rounded(px(6.0))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(5.0))
                                .cursor_pointer()
                                .bg(motion::hover_blend(
                                    &key,
                                    crate::kit::theme::wash(0.0),
                                    crate::kit::theme::wash(0.07),
                                ))
                                .on_hover(motion::hover_listener(key.clone()))
                                .tooltip(|_, cx| {
                                    cx.new(|_| {
                                        super::super::session::FindTooltip(
                                            "Open the project".into(),
                                        )
                                    })
                                    .into()
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(space) = space.clone() {
                                        this.open_panel(PanelItem::Project(space), cx);
                                    }
                                }))
                                .text_size(px(11.0))
                                .text_color(motion::hover_blend(
                                    &key,
                                    theme.text_muted.opacity(0.85),
                                    theme.text,
                                ))
                                .child(icon(glyph).size(px(11.0)).flex_none().text_color(tint))
                                .child(div().min_w_0().truncate().child(SharedString::from(place))),
                        )
                    }),
            );
        if let Some(summary) = work.summary.as_ref().filter(|s| !s.trim().is_empty()) {
            flow = flow.child(div().px(px(8.0)).pb(px(6.0)).child(md(
                summary,
                format!("bot-work-summary-{}", work.id),
                12.0,
                theme.text_muted,
            )));
        }

        // ── the rail: steps, then the result ──
        // One node: its mark over the segment to the next node, beside its
        // content. Hovering a node washes it and firms up its segment.
        let node = |key: &str, mark: AnyElement, joined: Option<(gpui::Hsla, gpui::Hsla)>| {
            let rail = div()
                .flex_none()
                .w(px(18.0))
                .flex()
                .flex_col()
                .items_center()
                .child(flow_cell().child(mark))
                .when_some(joined, |el, (rest, hover)| {
                    el.child(
                        div()
                            .flex_1()
                            .w(px(1.5))
                            .mt(px(3.0))
                            // Reaches into the next node's top padding.
                            .mb(px(-3.0))
                            .rounded_full()
                            .bg(motion::hover_blend(key, rest, hover)),
                    )
                });
            div()
                .id(SharedString::from(key.to_string()))
                .mx(px(2.0))
                .px(px(6.0))
                .py(px(6.0))
                .rounded(px(10.0))
                .flex()
                .flex_row()
                .items_stretch()
                .gap(px(12.0))
                .bg(motion::hover_blend(
                    key,
                    crate::kit::theme::wash(0.0),
                    crate::kit::theme::wash(0.035),
                ))
                .on_hover(motion::hover_listener(key.to_string()))
                .child(rail)
        };
        // A segment past a finished step is filled; the rest stay a hairline.
        let segment = |state: StepState| {
            if state == StepState::Done {
                (theme.success.opacity(0.45), theme.success.opacity(0.8))
            } else {
                (
                    crate::kit::theme::hairline(0.12),
                    crate::kit::theme::hairline(0.3),
                )
            }
        };
        flow = flow.child(div().h(px(2.0)));
        for (ix, (step, (project, sessions))) in work.steps.iter().zip(steps).enumerate() {
            let key = format!("bot-work-node-{}-{ix}", work.id);
            let mark = if numbered {
                flow_number(ix + 1, theme)
            } else {
                step_mark(step.state, ticker, theme)
            };
            let span = (!numbered).then(|| step_span(step, now_ms)).flatten();
            let project = project.filter(|_| common.is_none());
            // While it waits the plan says what each step will do; once it
            // runs, what the step reported.
            let detail = if numbered {
                step.detail.clone()
            } else {
                step.note.clone()
            }
            .filter(|d| !d.trim().is_empty());
            let asking = step.state == StepState::Asking;
            let sessions_n = sessions.len();
            let foldable = project.is_some() || detail.is_some() || sessions_n > 0;
            let folded = foldable && self.board.work_folded.contains(&(work.id.clone(), ix));
            // Its title line folds the details away and back; a chevron
            // shows on hover.
            let toggle = (work.id.clone(), ix);
            let title_key = format!("{key}-title");
            let title = div()
                .id(SharedString::from(title_key.clone()))
                .min_h(px(18.0))
                .flex()
                .flex_row()
                .items_start()
                .gap(px(TITLE_GAP))
                .when(foldable, |el| {
                    el.cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.board.work_folded.remove(&toggle) {
                                this.board.work_folded.insert(toggle.clone());
                            }
                            cx.notify();
                        }))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.0))
                        .line_height(px(18.0))
                        .text_color(if step.state == StepState::Skipped {
                            theme.text_muted
                        } else {
                            theme.text
                        })
                        .child(SharedString::from(step.title.clone())),
                )
                .children(span.map(|span| {
                    // Its exact times on hover.
                    let tip = match (step.started_at, step.ended_at) {
                        (Some(start), Some(end)) => format!(
                            "{} → {}",
                            parts::past_full_ms(start),
                            parts::past_full_ms(end)
                        ),
                        (Some(start), None) => format!("Since {}", parts::past_full_ms(start)),
                        _ => String::new(),
                    };
                    div()
                        .id(SharedString::from(format!("{key}-span")))
                        .flex_none()
                        .pt(px(2.0))
                        .when(!tip.is_empty(), |el| {
                            el.tooltip(move |_, cx| {
                                let tip = tip.clone();
                                cx.new(|_| super::super::session::FindTooltip(tip.into()))
                                    .into()
                            })
                        })
                        .child(trailing(span, theme.text_faint))
                }))
                // The chevron's slot is kept on every step, so the times
                // line up in one column (and the result's right edge with
                // them).
                .child(
                    div()
                        .flex_none()
                        .w(px(CHEVRON_W))
                        .pt(px(4.0))
                        .when(foldable, |el| {
                            el.child(
                                icon(if folded {
                                    icons::ALT_ARROW_RIGHT
                                } else {
                                    icons::ALT_ARROW_DOWN
                                })
                                .size(px(CHEVRON_W))
                                .text_color(theme.text_faint.opacity(motion::hover_t(&key))),
                            )
                        }),
                );
            let mut content = div().flex_1().min_w_0().flex().flex_col().child(title);
            if !folded {
                let mut details = div().flex().flex_col().gap(px(2.0)).pt(px(2.0));
                if let Some(project) = project {
                    details = details.child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(5.0))
                            .text_size(px(11.5))
                            .line_height(px(16.0))
                            .text_color(theme.text_muted.opacity(0.85))
                            .child(
                                icon(project.glyph)
                                    .size(px(11.0))
                                    .flex_none()
                                    .text_color(project.tint),
                            )
                            .child(div().min_w_0().truncate().child(SharedString::from(format!(
                                "{} · {}",
                                project.name, project.device
                            )))),
                    );
                }
                if let Some(detail) = &detail {
                    details = details.child(md(
                        detail,
                        format!("bot-work-step-{}-{ix}", work.id),
                        12.0,
                        if asking {
                            theme.warning
                        } else {
                            theme.text_muted
                        },
                    ));
                }
                if sessions_n > 0 {
                    details = details.child(
                        div().pt(px(4.0)).flex().flex_col().gap(px(1.0)).children(
                            sessions
                                .into_iter()
                                .map(|(chat_id, chat)| self.flow_session(chat_id, chat, theme, cx)),
                        ),
                    );
                }
                // Unfolding fades the details in.
                content = content.when(foldable, |el| {
                    el.child(motion::fade_quick(
                        SharedString::from(format!("{key}-details")),
                        details,
                    ))
                });
            }
            flow =
                flow.child(node(&key, mark, Some(segment(step.state))).child(content.pb(px(6.0))));
        }

        // ── the result: where the last step leads ──
        let (tone, done) = match phase {
            PlanPhase::Done => (theme.success, true),
            PlanPhase::Failed => (theme.danger, true),
            _ if phase.ended() => (theme.text_muted, true),
            _ => (theme.text_faint, false),
        };
        let result = work
            .result
            .as_ref()
            .filter(|r| !r.trim().is_empty() && done);
        let key = format!("bot-work-node-{}-result", work.id);
        // The flag alone, in the outcome's tone; it deepens on hover.
        let mark = icon(icons::FLAG)
            .size(px(12.0))
            .text_color(motion::mix(
                tone.opacity(if done { 0.85 } else { 0.7 }),
                tone,
                motion::hover_t(&key),
            ))
            .into_any_element();
        let copied = self.board.work_copied.as_deref() == Some(work.id.as_str());
        let title = div()
            .min_h(px(18.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(if done { theme.text } else { theme.text_muted })
                    .child("Result"),
            )
            .when_some(result.cloned(), |el, text| {
                // Copy, shown on hover (and while it says Copied).
                let copy_key = format!("{key}-copy");
                let id = work.id.clone();
                let shown = if copied { 1.0 } else { motion::hover_t(&key) };
                let ink = if copied {
                    theme.success
                } else {
                    motion::hover_blend(&copy_key, theme.text_muted, theme.text)
                };
                el.child(
                    div()
                        .id(SharedString::from(copy_key.clone()))
                        .flex_none()
                        .h(px(20.0))
                        // Its label, not its padding, meets the times.
                        .mr(px(-6.0))
                        .px(px(6.0))
                        .rounded(px(6.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.0))
                        .cursor_pointer()
                        .opacity(shown)
                        .bg(motion::hover_blend(
                            &copy_key,
                            crate::kit::theme::wash(0.0),
                            crate::kit::theme::wash(0.08),
                        ))
                        .on_hover(motion::hover_listener(copy_key.clone()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_work_result(id.clone(), text.clone(), cx)
                        }))
                        .text_size(px(11.0))
                        .text_color(ink)
                        .child(
                            icon(if copied { icons::CHECK } else { icons::COPY })
                                .size(px(11.0))
                                .text_color(ink),
                        )
                        .child(if copied { "Copied" } else { "Copy" }),
                )
            });
        let content = match result {
            // What it produced, under its title; the flag carries the tone.
            Some(result) => div().flex().flex_col().gap(px(4.0)).child(title).child(md(
                result,
                format!("bot-work-result-{}", work.id),
                12.5,
                theme.text,
            )),
            None => div().flex().flex_col().gap(px(2.0)).child(title).child(
                div()
                    .text_size(px(12.0))
                    .line_height(px(17.0))
                    .text_color(if done {
                        theme.text_muted
                    } else {
                        theme.text_faint
                    })
                    .child(SharedString::from(if done {
                        phase_line(work, now_ms)
                    } else {
                        "Reported when the last step ends".to_string()
                    })),
            ),
        };
        // Its right edge meets the steps' times, past which they keep the
        // chevron's slot.
        flow.child(
            node(&key, mark, None).child(
                content
                    .flex_1()
                    .min_w_0()
                    .pr(px(CHEVRON_W + TITLE_GAP))
                    .pb(px(4.0)),
            ),
        )
    }

    /// A work item's status as the mark left of the panel's title, bare
    /// (no tile); hovering it shows what state it is in and when.
    fn status_tile(&self, work: &BotWork, phase: PlanPhase, theme: &Theme) -> AnyElement {
        let key = format!("bot-work-status-{}", work.id);
        let tip = WorkStatusTip::of(work, phase, theme);
        div()
            .id(SharedString::from(key))
            .flex_none()
            .size(px(20.0))
            .flex()
            .items_center()
            .justify_center()
            .tooltip(move |_, cx| cx.new(|_| tip.clone()).into())
            .child(work_mark(work, self.pulse_ticker.entity_id(), theme))
            .into_any_element()
    }

    /// Copy a work item's result; the button says Copied for a moment.
    fn copy_work_result(&mut self, work_id: String, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.board.work_copied = Some(work_id.clone());
        self.board.work_copied_reset = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1600))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.board.work_copied.as_deref() == Some(work_id.as_str()) {
                    this.board.work_copied = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// A session a step started, under the step: its status, title, and a
    /// click that opens it.
    fn flow_session(
        &self,
        chat_id: String,
        chat: Option<(String, ChatIndicator)>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let ticker = self.pulse_ticker.entity_id();
        let key = format!("bot-work-session-{chat_id}");
        let row = div()
            .id(SharedString::from(key.clone()))
            .h(px(24.0))
            .ml(px(-6.0))
            .px(px(6.0))
            .rounded(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .text_size(px(12.0));
        match chat {
            Some((title, status)) => row
                .cursor_pointer()
                .bg(motion::hover_blend(
                    &key,
                    crate::kit::theme::wash(0.0),
                    crate::kit::theme::wash(0.06),
                ))
                .on_hover(motion::hover_listener(key.clone()))
                .on_click(cx.listener(move |this, _, _, cx| this.open_chat(chat_id.clone(), cx)))
                .child(mark_cell(session_mark(status, ticker, theme)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(motion::hover_blend(&key, theme.text_muted, theme.text))
                        .child(SharedString::from(title)),
                )
                .children(session_word(status, theme).map(|(word, color)| {
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(color)
                        .child(word)
                }))
                .child(
                    // Slides in from the left as it fades in.
                    div()
                        .flex_none()
                        .ml(px(-4.0 * (1.0 - motion::hover_t(&key))))
                        .child(
                            icon(icons::ALT_ARROW_RIGHT)
                                .size(px(10.0))
                                .text_color(theme.text_faint.opacity(motion::hover_t(&key))),
                        ),
                ),
            None => row
                .child(mark_cell(session_mark(ChatIndicator::Idle, ticker, theme)))
                .child(
                    div()
                        .text_color(theme.text_faint)
                        .child("A session not on this device yet"),
                ),
        }
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
        let (title, back_item, back_label) = {
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
            title,
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
