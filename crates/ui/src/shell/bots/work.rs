//! The **Work** card (§13.1): running orchestrations with their current step,
//! a step bar and elapsed time, approved plans waiting to start, then the
//! last few that ended; "N more" brings the rest in, in place. The panel
//! holds no list of work: a row opens the orchestration, its status and request,
//! then the plan as one flow (each step with its state, duration and
//! sessions, ending in the result), Stop while it runs, and its thread,
//! read-only in the panel.

use super::board::{card_corner, card_item};
use super::items::{
    ago, arrow_in, decision_bar, mark_cell, progress_bar, project_label, section_head,
    section_rule, session_mark, session_word, step_mark, trailing, work_mark,
};
use super::panel::{PanelHead, PanelItem};
use super::*;
use crate::transcript::bot_plan::{PlanPhase, format_span, phase_line, plan_phase, step_span};
use cypher_proto::{BotWork, StepState, WorkState};

use super::board::CARD_SHORT;

/// A bot's work as its card lists it.
pub(super) struct WorkCard<'a> {
    /// Under way, always shown: running plans (newest start first), then
    /// approved plans that have not started yet (oldest approval first,
    /// the order they start in). A plan still awaiting approval is in
    /// Needs you, not here.
    pub live: Vec<&'a BotWork>,
    /// Ended, newest end first: the last [`CARD_SHORT`] that ran, or
    /// every one (rejected plans included) once the card is expanded.
    pub ended: Vec<&'a BotWork>,
    /// How many ended items the short list leaves out.
    pub hidden: usize,
}

pub(super) fn work_card<'a>(
    records: &'a cypher_proto::BotRecords,
    bot_id: &'a str,
    expanded: bool,
) -> WorkCard<'a> {
    let mut running: Vec<&BotWork> = Vec::new();
    let mut waiting: Vec<&BotWork> = Vec::new();
    let mut ended: Vec<&BotWork> = Vec::new();
    for work in records.work_of(bot_id) {
        match work.state {
            WorkState::Running => running.push(work),
            WorkState::Proposed => {
                if work.decision.as_ref().is_some_and(|d| d.approve) {
                    waiting.push(work)
                }
            }
            _ => ended.push(work),
        }
    }
    running.sort_by_key(|w| std::cmp::Reverse(w.started_at.unwrap_or(w.created_at)));
    waiting.sort_by_key(|w| w.decision.as_ref().map_or(w.created_at, |d| d.at));
    ended.sort_by_key(|w| std::cmp::Reverse(w.ended_at.unwrap_or(w.created_at)));
    let total = ended.len();
    let short: Vec<&BotWork> = ended
        .iter()
        .copied()
        .filter(|w| w.state != WorkState::Cancelled)
        .take(CARD_SHORT)
        .collect();
    let hidden = total - short.len();
    running.extend(waiting);
    WorkCard {
        live: running,
        ended: if expanded { ended } else { short },
        hidden,
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

/// A finished item's one line: its result's first paragraph, else how it
/// ended.
pub(super) fn result_line(work: &BotWork) -> String {
    // Its first paragraph: a list or detail after it would run on.
    work.result
        .as_deref()
        .and_then(|r| r.split("\n\n").map(str::trim).find(|p| !p.is_empty()))
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
    /// A running item on the card: its mark, title and how long it has run,
    /// then the step it is on over a small bar of the steps done.
    fn running_row(
        &self,
        work: &BotWork,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let now = Utc::now().timestamp_millis();
        let item = PanelItem::WorkItem(work.id.clone());
        let selected = self.board.panel.as_ref() == Some(&item);
        let (done, total) = work.progress();
        let elapsed = format_span(now - work.started_at.unwrap_or(work.created_at));
        let stopping = plan_phase(work) == PlanPhase::Stopping;
        let detail = div()
            .pt(px(3.0))
            .flex()
            .flex_col()
            .gap(px(5.0))
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
            .child(div().pb(px(2.0)).child(progress_bar(done, total, theme)))
            .into_any_element();
        super::board::card_item_detail(
            &format!("bot-work-{}", work.id),
            Some(work_mark(work, self.pulse_ticker.entity_id(), theme)),
            transcript::single_line(&work.title),
            Some(card_corner(elapsed, None, theme)),
            Some(detail),
            selected,
            theme,
        )
        .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
    }

    /// An item that is not running, on the card: its mark, its title and
    /// when it ended; an approved plan that has not started says it waits
    /// (for a free slot, or for the workspace to be resumed).
    fn settled_row(
        &self,
        work: &BotWork,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let item = PanelItem::WorkItem(work.id.clone());
        let selected = self.board.panel.as_ref() == Some(&item);
        let corner = if work.state == WorkState::Proposed {
            "waiting".to_string()
        } else {
            ago(work.ended_at.unwrap_or(work.created_at), Utc::now())
        };
        card_item(
            &format!("bot-work-{}", work.id),
            Some(work_mark(work, self.pulse_ticker.entity_id(), theme)),
            transcript::single_line(&work.title),
            Some(card_corner(corner, None, theme)),
            selected,
            theme,
        )
        .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
    }

    /// The sidebar card; `None` when there is nothing to show. What is
    /// under way always shows; of what ended, the last few, and the rest
    /// in place on "N more" ([`work_card`]).
    pub(super) fn render_work_card(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let expanded = self.list_expanded("work");
        let (live, ended, hidden) = {
            let state = self.state.read(cx);
            let card = work_card(&state.bot_records, &bot.id, expanded);
            let own = |list: Vec<&BotWork>| list.into_iter().cloned().collect::<Vec<_>>();
            (own(card.live), own(card.ended), card.hidden)
        };
        if live.is_empty() && ended.is_empty() && hidden == 0 {
            return None;
        }
        let header = self.card_header(
            "work",
            "bot-work-header",
            "Work",
            live.len(),
            None,
            None,
            theme,
            cx,
        );
        let mut rows: Vec<AnyElement> = Vec::new();
        for w in live.iter().chain(&ended) {
            let row = if w.state == WorkState::Running {
                self.running_row(w, theme, cx)
            } else {
                self.settled_row(w, theme, cx)
            };
            rows.push(row.into_any_element());
        }
        rows.extend(self.more_row("work".into(), hidden, theme, cx));
        Some(self.fold_card("work", header, rows, theme))
    }

    /// The detail panel for this card's items; `None` when the item no
    /// longer resolves.
    pub(super) fn render_work_panel(
        &mut self,
        item: &PanelItem,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        match item {
            PanelItem::WorkItem(id) => self.render_work_item(id, theme, window, cx),
            PanelItem::Thread(id) => self.render_thread(id, theme, cx),
            _ => None,
        }
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
                .child(section_head("Request", true, theme))
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
            let (edit_id, edit_title) = (work.id.clone(), work.title.clone());
            self.set_panel_footer(
                decision_bar(Some("Nothing runs until you approve."), theme)
                    .child(
                        popover::btn_ghost(theme, "Reject", "bot-plan-cancel")
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
                    // Reject it and start the reply that asks for the change.
                    .child(
                        popover::btn_ghost(theme, "Edit", "bot-plan-edit")
                            .id("bot-plan-edit")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_bot_plan(&edit_id, &edit_title, cx)
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
            .when(!first, |el| el.child(section_rule()))
            .child(
                section_head("Plan", first, theme)
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
                // Copy, shown on hover; its label (not its padding) meets
                // the times.
                el.child(
                    self.copy_button(
                        format!("{key}-copy"),
                        text,
                        motion::hover_t(&key),
                        theme,
                        cx,
                    )
                    .mr(px(-6.0)),
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
                .child(arrow_in(&key, theme)),
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
        let view = self.board.peeks.get(thread)?.transcript.clone()?;
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
            notify: false,
        }
    }

    #[test]
    fn the_card_keeps_whats_under_way_and_the_last_few_that_ended() {
        let mut records = cypher_proto::BotRecords::default();
        let mut other = work("other", WorkState::Running, 9);
        other.bot_id = "x".into();
        let decided = |id: &str, approve: bool, at: i64| {
            let mut plan = work(id, WorkState::Proposed, at);
            plan.decision = Some(cypher_proto::PlanDecision { approve, at });
            plan
        };
        records.work = vec![
            work("old-run", WorkState::Running, 1),
            work("new-run", WorkState::Running, 5),
            // Awaiting approval: Needs you lists it, not Work.
            work("plan", WorkState::Proposed, 2),
            // Approved, not started yet (a full house, a paused workspace).
            decided("second", true, 8),
            decided("first", true, 7),
            // Rejected here, not closed by its host yet.
            decided("rejecting", false, 6),
            work("done-1", WorkState::Done, 1),
            work("failed-2", WorkState::Failed, 2),
            work("rejected-3", WorkState::Cancelled, 3),
            work("stopped-4", WorkState::Stopped, 4),
            work("done-5", WorkState::Done, 5),
            other,
        ];
        let ids = |list: &[&BotWork]| list.iter().map(|w| w.id.clone()).collect::<Vec<_>>();
        let short = work_card(&records, "b", false);
        assert_eq!(ids(&short.live), ["new-run", "old-run", "first", "second"]);
        assert_eq!(ids(&short.ended), ["done-5", "stopped-4", "failed-2"]);
        assert_eq!(short.hidden, 2, "an older one and the rejected one");
        // Expanded in place: everything that ended, rejected plans too.
        let all = work_card(&records, "b", true);
        assert_eq!(ids(&all.live), ids(&short.live));
        assert_eq!(
            ids(&all.ended),
            ["done-5", "stopped-4", "rejected-3", "failed-2", "done-1"]
        );
        assert_eq!(all.hidden, 2, "what Show less puts away again");
        // Nothing left out: no row to expand.
        records.work.truncate(2);
        assert_eq!(work_card(&records, "b", false).hidden, 0);
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
