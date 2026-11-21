//! The **Work** card (§13.1): running orchestrations with their progress
//! ring, current step and elapsed time, approved plans waiting to start,
//! then the last few that ended; "N more" brings the rest in, in place. The
//! panel holds no list of work: a row opens the orchestration. Its page has
//! every control at the very top beside its status, then its title; under
//! them what was asked, the result once it ended, and the steps on one rail
//! (each with its state, duration and what it reported, its sessions under
//! the step that is under way). Its thread opens read-only in the panel.

use super::board::{card_corner, card_item};
use super::items::{
    ago, arrow_in, mark_cell, project_label, section_head, section_rule, session_mark,
    session_word, step_mark, trailing, work_mark,
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

/// The one project every step that names a project works in (its space
/// id): the page names it once, in its header.
fn shared_project(work: &BotWork) -> Option<&str> {
    let mut projects = work.steps.iter().filter_map(|s| s.project.as_deref());
    let first = projects.next()?;
    projects.all(|p| p == first).then_some(first)
}

/// A work item's state in a word, in its tone, for the page's header.
fn phase_word(phase: PlanPhase, theme: &Theme) -> (&'static str, gpui::Hsla) {
    let word = match phase {
        PlanPhase::Awaiting => "Awaiting approval",
        PlanPhase::Starting => "Waiting to start",
        PlanPhase::Cancelling | PlanPhase::Cancelled => "Rejected",
        PlanPhase::Running => "Running",
        PlanPhase::Stopping => "Stopping",
        PlanPhase::Done => "Done",
        PlanPhase::Failed => "Failed",
        PlanPhase::Stopped => "Stopped",
    };
    (word, phase.tint(theme))
}

/// A request as its tooltip shows it: at most a few lines' worth.
fn clip_request(text: &str) -> String {
    const MAX: usize = 400;
    let text = text.trim();
    match text.char_indices().nth(MAX) {
        Some((end, _)) => format!("{}…", text[..end].trim_end()),
        None => text.to_string(),
    }
}

/// Whether a step shows its details before anyone clicked it: every step
/// of a plan still to read, else only one that is under way or went wrong.
fn step_opens(state: StepState, numbered: bool) -> bool {
    numbered
        || matches!(
            state,
            StepState::Running | StepState::Asking | StepState::Failed
        )
}

/// The hover card of a work item's status: when it was proposed, approved,
/// started and ended, and how long it took or has run. The page itself
/// shows none of these; the status word and the steps say where it is.
#[derive(Clone)]
struct WorkStatusTip {
    times: Vec<(&'static str, String)>,
}

impl WorkStatusTip {
    fn of(work: &BotWork) -> Self {
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
        Self { times }
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
                .gap(px(3.0))
                .text_size(px(11.5))
                .line_height(px(16.0))
                .children(rows),
        )
    }
}

/// What the owner asked, on hover over the page's title once the plan is
/// under way: wrapped, where the one-line tooltip would run off.
#[derive(Clone)]
struct RequestTip(SharedString);

impl Render for RequestTip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        motion::fade_quick(
            "bot-work-request-tip",
            div()
                .w(px(280.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.surface_raised)
                .shadow_md()
                .text_size(px(11.5))
                .line_height(px(16.0))
                .text_color(theme.text_muted)
                .child(self.0.clone()),
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
    /// A running item on the card: its progress ring (the steps done, with
    /// the spinner in it), its title and how long it has run, then the step
    /// it is on.
    fn running_row(
        &self,
        work: &BotWork,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let now = Utc::now().timestamp_millis();
        let item = PanelItem::WorkItem(work.id.clone());
        let selected = self.board.panel.as_ref() == Some(&item);
        let elapsed = format_span(now - work.started_at.unwrap_or(work.created_at));
        let stopping = plan_phase(work) == PlanPhase::Stopping;
        let detail = div()
            .pt(px(2.0))
            .min_w_0()
            .truncate()
            .text_size(px(11.0))
            .line_height(px(14.0))
            .text_color(theme.text_muted.opacity(0.85))
            .child(SharedString::from(if stopping {
                "Stopping…".to_string()
            } else {
                current_step_line(work)
            }))
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

    /// One work item in every phase, the plan to approve included.
    ///
    /// The page says each thing once and nothing it can leave out. Its
    /// header holds every control at the very top beside a word of how it
    /// stands (the times on hover), then its title. The body is the result
    /// once it ended, then the steps ([`Self::work_flow`]); a plan still to
    /// approve also quotes what was asked and gives its approach.
    pub(super) fn render_work_item(
        &mut self,
        work_id: &str,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let now = Utc::now();
        let (work, steps, thread_listed, shared) = {
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
            let shared = shared_project(&work).and_then(|space| {
                project_label(state, space, theme).map(|label| (space.to_string(), label))
            });
            (work, steps, thread_listed, shared)
        };
        let phase = plan_phase(&work);
        let head = PanelHead {
            glyph: icons::HIERARCHY,
            tint: None,
            title: work.title.clone(),
        };
        self.board.panel_header =
            Some(self.work_header(&work, phase, shared.as_ref(), thread_listed, theme, cx));

        let mut body = div().p(px(8.0)).pb(px(14.0)).flex().flex_col();
        // What was asked, as a quote, while the plan is still to judge
        // against it; after that the title stands for it (the request is
        // its tooltip).
        let quoted = phase.awaiting() && shows_request(&work);
        if quoted {
            body = body.child(
                div()
                    .mx(px(8.0))
                    .mt(px(6.0))
                    .mb(px(10.0))
                    .pl(px(10.0))
                    .border_l_2()
                    .border_color(crate::kit::theme::hairline(0.2))
                    .child(super::items::panel_markdown(
                        &work.instruction,
                        format!("bot-work-request-{}", work.id),
                        12.5,
                        theme.text_muted,
                        theme,
                        window,
                    )),
            );
        }
        let mut resulted = false;
        // What it produced comes before how it got there.
        if let Some(result) = work
            .result
            .as_ref()
            .filter(|r| phase.ended() && !r.trim().is_empty())
        {
            let key = format!("bot-work-result-{}", work.id);
            body = body.child(
                div()
                    .id(SharedString::from(key.clone()))
                    .on_hover(motion::hover_listener(key.clone()))
                    .flex()
                    .flex_col()
                    .child(
                        section_head("Result", true, theme)
                            .child(div().flex_1())
                            // Copy, shown with the pointer over the result.
                            .child(
                                self.copy_button(
                                    format!("{key}-copy"),
                                    result.clone(),
                                    motion::hover_t(&key),
                                    theme,
                                    cx,
                                )
                                .mr(px(-6.0)),
                            ),
                    )
                    .child(div().px(px(8.0)).child(super::items::panel_markdown(
                        result,
                        format!("{key}-text"),
                        13.0,
                        theme.text,
                        theme,
                        window,
                    ))),
            );
            resulted = true;
        }
        if !work.steps.is_empty() {
            body = body.child(self.work_flow(
                &work,
                steps,
                phase,
                resulted,
                shared.is_some(),
                theme,
                window,
                cx,
            ));
        }
        Some((head, body))
    }

    /// The work page's header. A bar of controls at the very top: the
    /// status at the left (its mark and a word, the times on hover), every
    /// button at the right. Then the title, and under it only the project
    /// when every step shares one. Counts and times are not repeated here:
    /// the steps show how far it is, the hover card when.
    fn work_header(
        &self,
        work: &BotWork,
        phase: PlanPhase,
        shared: Option<&(String, super::items::ProjectLabel)>,
        thread_listed: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tip = WorkStatusTip::of(work);
        // A running plan parked on a question says so, as its ring does.
        let asking = phase.running()
            && work
                .current_step()
                .is_some_and(|(_, step)| step.state == StepState::Asking);
        let (word, tone) = if asking {
            ("Waiting on you", theme.warning)
        } else {
            phase_word(phase, theme)
        };
        let status = super::items::header_status(
            format!("bot-work-status-{}", work.id),
            work_mark(work, self.pulse_ticker.entity_id(), theme),
            word,
            tone,
        )
        .tooltip(move |_, cx| cx.new(|_| tip.clone()).into());

        let mut controls: Vec<AnyElement> = Vec::new();
        if thread_listed {
            let thread = PanelItem::Thread(work.thread_chat_id.clone().unwrap_or_default());
            let tip = match super::choice::choice_words(&work.coordinator_choice, |id| {
                self.known_model_label(id)
            }) {
                Some(words) => {
                    format!("Open the coordinator: the session that runs this plan ({words})")
                }
                None => "Open the coordinator: the session that runs this plan".to_string(),
            };
            controls.push(
                // Not a step's session (those are rows under their step):
                // the one that runs the plan and starts them.
                super::items::icon_button("bot-work-thread", icons::HIERARCHY, tip, theme)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.open_panel(thread.clone(), cx)),
                    )
                    .into_any_element(),
            );
        }
        if phase.ended() {
            let (id, name) = (work.id.clone(), work.title.clone());
            controls.push(
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
        if phase.awaiting() {
            let (approve, cancel) = (work.id.clone(), work.id.clone());
            let (edit_id, edit_title) = (work.id.clone(), work.title.clone());
            // Reject it and start the reply that asks for the change.
            controls.push(
                super::items::icon_button(
                    "bot-plan-edit",
                    icons::PEN,
                    "Ask for a change: rejects it and starts your reply",
                    theme,
                )
                .on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.edit_bot_plan(&edit_id, &edit_title, cx)
                    }),
                )
                .into_any_element(),
            );
            controls.push(
                popover::btn_ghost(theme, "Reject", "bot-plan-cancel")
                    .id("bot-plan-cancel")
                    .flex_none()
                    .px(px(10.0))
                    .py(px(4.0))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mutate(
                            serde_json::json!({
                                "op": "resolveBotPlan", "workId": cancel, "approve": false,
                            }),
                            cx,
                        )
                    }))
                    .into_any_element(),
            );
            controls.push(
                popover::btn_primary(theme, "Approve")
                    .id("bot-plan-approve")
                    .flex_none()
                    .py(px(4.0))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mutate(
                            serde_json::json!({
                                "op": "resolveBotPlan", "workId": approve, "approve": true,
                            }),
                            cx,
                        )
                    }))
                    .into_any_element(),
            );
        } else if phase.running() {
            controls.extend(self.time_button(work, theme, cx));
            controls.push(
                self.stop_button("bot-work-stop", &work.id, theme, cx)
                    .into_any_element(),
            );
        }

        // Past its approval the page does not quote the request: it is
        // here for whoever wants the exact words.
        let request = (!phase.awaiting() && shows_request(work)).then(|| {
            SharedString::from(clip_request(&super::items::plain_markdown(
                &work.instruction,
            )))
        });
        let place = shared.map(|(space, label)| {
            let key = format!("bot-work-place-{}", work.id);
            let space = space.clone();
            let tip = SharedString::from(format!("Open the project · on {}", label.device));
            div()
                .id(SharedString::from(key.clone()))
                .flex_none()
                .max_w(px(160.0))
                .ml(px(-4.0))
                .px(px(5.0))
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
                .tooltip(move |_, cx| {
                    let tip = tip.clone();
                    cx.new(|_| super::super::session::FindTooltip(tip)).into()
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_panel(PanelItem::Project(space.clone()), cx)
                }))
                .text_color(motion::hover_blend(&key, theme.text_muted, theme.text))
                .child(
                    icon(label.glyph)
                        .size(px(11.0))
                        .flex_none()
                        .text_color(label.tint),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(label.name.clone())),
                )
        });
        let title =
            super::items::header_title(format!("bot-work-title-{}", work.id), &work.title, theme)
                .when_some(request, |el, request| {
                    el.tooltip(move |_, cx| cx.new(|_| RequestTip(request.clone())).into())
                });
        // Under the title only the one project every step works in.
        super::items::item_header(
            status,
            controls,
            title,
            place.map(IntoElement::into_any_element),
            theme,
        )
    }

    /// A running plan's time: "+1h" adds an hour to its limit; near the end
    /// it says how long is left, in the warning colour. `None` once no
    /// more time may be added.
    fn time_button(
        &self,
        work: &BotWork,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let fallback = self
            .state
            .read(cx)
            .bots
            .iter()
            .find(|b| b.id == work.bot_id)
            .map_or(240, |b| b.plans.time_limit_minutes());
        let deadline = work.deadline_ms(fallback)?;
        let added = work.time_added_minutes.unwrap_or(0);
        if added + 60 > cypher_proto::PLAN_TIME_ADDED_MAX_MINUTES {
            return None;
        }
        let left = (deadline - cypher_proto::now_ms()).max(0);
        let near = left <= cypher_proto::PLAN_WARN_BEFORE_MS;
        let left_words = format_span(left);
        let tip = SharedString::from(format!(
            "Time limit {}: {left_words} left. Add an hour.",
            crate::transcript::bot_plan::limit_words(work.limit_minutes(fallback))
        ));
        let id = work.id.clone();
        Some(
            popover::btn_ghost(
                theme,
                &if near {
                    format!("{left_words} left · +1h")
                } else {
                    "+1h".to_string()
                },
                "bot-work-time",
            )
            .id("bot-work-time")
            .flex_none()
            .px(px(10.0))
            .py(px(4.0))
            .when(near, |el| el.text_color(theme.warning))
            .tooltip(move |_, cx| {
                let tip = tip.clone();
                cx.new(|_| super::super::session::FindTooltip(tip)).into()
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.mutate(
                    serde_json::json!({ "op": "addBotWorkTime", "workId": id, "minutes": 60 }),
                    cx,
                )
            }))
            .into_any_element(),
        )
    }

    /// The steps on one rail: each with its state, how long it took and
    /// what it reported. A step with one session is the way into it; one
    /// with several lists them while it is running, asking or failed, and
    /// otherwise keeps to a line that a click opens. A plan still to
    /// approve shows every step's description. `heading` says a result
    /// stands above; `shared` that the header already names the project.
    #[allow(clippy::too_many_arguments)]
    fn work_flow(
        &self,
        work: &BotWork,
        steps: Vec<FlowStep>,
        phase: PlanPhase,
        heading: bool,
        shared: bool,
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
        // A heading only where a result stands above the steps; alone on
        // the page they need none.
        let mut flow = div().flex().flex_col().pt(px(2.0)).when(heading, |el| {
            el.child(section_rule())
                .child(section_head("Steps", false, theme))
        });
        // The approach in a sentence, for the decision; once it runs the
        // steps say it.
        if phase.awaiting()
            && let Some(summary) = work.summary.as_ref().filter(|s| !s.trim().is_empty())
        {
            flow = flow.child(div().px(px(8.0)).pb(px(6.0)).child(md(
                summary,
                format!("bot-work-summary-{}", work.id),
                12.0,
                theme.text_muted,
            )));
        }

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
                .py(px(5.0))
                .rounded(px(10.0))
                .flex()
                .flex_row()
                .items_stretch()
                .gap(px(10.0))
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
        let last = work.steps.len().saturating_sub(1);
        for (ix, (step, (project, sessions))) in work.steps.iter().zip(steps).enumerate() {
            let key = format!("bot-work-node-{}-{ix}", work.id);
            let mark = if numbered {
                flow_number(ix + 1, theme)
            } else {
                step_mark(step.state, ticker, theme)
            };
            let span = (!numbered).then(|| step_span(step, now_ms)).flatten();
            let project = project.filter(|_| !shared);
            // While it waits the plan says what each step will do; once it
            // runs, what the step reported.
            let detail = if numbered {
                // What its sessions run on, when the plan chose it.
                let runs = super::choice::choice_words(&step.session_choice, |id| {
                    self.known_model_label(id)
                })
                .map(|words| format!("Its sessions: {words}."));
                match (step.detail.clone().filter(|d| !d.trim().is_empty()), runs) {
                    (Some(detail), Some(runs)) => Some(format!("{detail}\n\n{runs}")),
                    (detail, runs) => detail.or(runs),
                }
            } else {
                step.note.clone()
            }
            .filter(|d| !d.trim().is_empty());
            let asking = step.state == StepState::Asking;
            // A step with one session is that session: its row would say
            // the step's title and state again, so the step is the link.
            let (link, sessions) = match sessions.as_slice() {
                [(chat_id, Some(_))] => (Some(chat_id.clone()), Vec::new()),
                _ => (None, sessions),
            };
            let sessions_n = sessions.len();
            let foldable = link.is_none() && (detail.is_some() || sessions_n > 0);
            let (timed, placed) = (span.is_some(), project.is_some());
            let toggle = (work.id.clone(), ix);
            let open = match link {
                // No fold: its note in full while it is under way.
                Some(_) => step_opens(step.state, numbered),
                None => {
                    foldable
                        && self
                            .board
                            .work_open
                            .get(&toggle)
                            .copied()
                            .unwrap_or_else(|| step_opens(step.state, numbered))
                }
            };
            let waiting = !numbered && step.state == StepState::Pending;
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
                            this.board.work_open.insert(toggle.clone(), !open);
                            cx.notify();
                        }))
                })
                .when_some(link.clone(), |el, chat_id| {
                    el.cursor_pointer()
                        .tooltip(|_, cx| {
                            cx.new(|_| {
                                super::super::session::FindTooltip("Open its session".into())
                            })
                            .into()
                        })
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.open_chat(chat_id.clone(), cx)),
                        )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.0))
                        .line_height(px(18.0))
                        .text_color(if waiting || step.state == StepState::Skipped {
                            theme.text_muted
                        } else {
                            theme.text
                        })
                        .child(SharedString::from(step.title.clone())),
                )
                // Where it works, quietly before its time: the name, and
                // the device only when that is another machine (it is in
                // the tooltip either way).
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(px(4.0))
                        .children(project.map(|project| {
                            let tip = SharedString::from(format!(
                                "{} · on {}",
                                project.name, project.device
                            ));
                            div()
                                .id(SharedString::from(format!("{key}-place")))
                                .flex_none()
                                .max_w(px(120.0))
                                .pt(px(2.0))
                                .truncate()
                                .text_size(px(11.0))
                                .text_color(theme.text_muted.opacity(0.85))
                                .tooltip(move |_, cx| {
                                    let tip = tip.clone();
                                    cx.new(|_| super::super::session::FindTooltip(tip)).into()
                                })
                                .child(SharedString::from(project.short()))
                        }))
                        .when(timed && placed, |el| {
                            el.child(
                                div()
                                    .flex_none()
                                    .pt(px(2.0))
                                    .text_size(px(11.0))
                                    .text_color(theme.text_faint)
                                    .child("·"),
                            )
                        })
                        .children(span.map(|span| {
                            // Its exact times on hover.
                            let tip = match (step.started_at, step.ended_at) {
                                (Some(start), Some(end)) => format!(
                                    "{} → {}",
                                    parts::past_full_ms(start),
                                    parts::past_full_ms(end)
                                ),
                                (Some(start), None) => {
                                    format!("Since {}", parts::past_full_ms(start))
                                }
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
                        })),
                )
                // The chevron's slot is kept on every step, so the times
                // line up in one column. A closed step shows it faintly:
                // there is more under it.
                .child(
                    div()
                        .flex_none()
                        .w(px(CHEVRON_W))
                        .pt(px(4.0))
                        // The way into its one session, shown on hover
                        // like a session row's.
                        .when(link.is_some(), |el| el.child(arrow_in(&key, theme)))
                        .when(foldable, |el| {
                            let hover = motion::hover_t(&key);
                            el.child(
                                icon(if open {
                                    icons::ALT_ARROW_DOWN
                                } else {
                                    icons::ALT_ARROW_RIGHT
                                })
                                .size(px(CHEVRON_W))
                                .text_color(
                                    theme.text_faint.opacity(if open {
                                        hover
                                    } else {
                                        0.55 + 0.45 * hover
                                    }),
                                ),
                            )
                        }),
                );
            let mut content = div().flex_1().min_w_0().flex().flex_col().child(title);
            if open && (detail.is_some() || sessions_n > 0) {
                let mut details = div().flex().flex_col().gap(px(2.0)).pt(px(2.0));
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
                // Opening fades the details in.
                content = content.child(motion::fade_quick(
                    SharedString::from(format!("{key}-details")),
                    details,
                ));
            } else if let Some(detail) = &detail {
                // Closed, it keeps what it reported to one line.
                content = content.child(
                    div()
                        .pr(px(CHEVRON_W + TITLE_GAP))
                        .truncate()
                        .text_size(px(11.5))
                        .line_height(px(16.0))
                        .text_color(theme.text_muted.opacity(0.85))
                        .child(SharedString::from(transcript::single_line(
                            &super::items::plain_markdown(detail),
                        ))),
                );
            }
            let joined = (ix < last).then(|| segment(step.state));
            flow = flow.child(node(&key, mark, joined).child(content.pb(px(6.0))));
        }
        flow
    }

    /// A session a step started, under the step: its status mark, its
    /// title, and a click that opens it.
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
                // Its mark says how it stands; the word is in the tooltip.
                .when_some(session_word(status, theme), |el, (word, _)| {
                    el.tooltip(move |_, cx| {
                        cx.new(|_| super::super::session::FindTooltip(word.into()))
                            .into()
                    })
                })
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

    /// A hidden thread's transcript, read-only, with the way back to what
    /// owns it (its work item, routine or connection) as an arrow left of
    /// the title. The panel draws its body unscrolled: the transcript
    /// scrolls itself.
    fn render_thread(
        &mut self,
        thread: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let (title, back_item, back_label, coordinator) = {
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
                        true,
                    )
                }
                cypher_proto::BotThread::Routine { routine_id }
                | cypher_proto::BotThread::RoutineSession { routine_id } => {
                    let routine = records.routines.iter().find(|r| &r.id == routine_id)?;
                    (
                        routine.name.clone(),
                        PanelItem::Routine(routine.id.clone()),
                        "Back to the routine",
                        false,
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
                        false,
                    )
                }
                cypher_proto::BotThread::Main | cypher_proto::BotThread::Unknown => return None,
            }
        };
        self.peek(thread, true, cx);
        let view = self.board.peeks.get(thread)?.transcript.clone()?;
        // The way back is the arrow left of the title, where an item has
        // its icon.
        let head = PanelHead {
            glyph: icons::CHAT_ROUND_LINE,
            tint: None,
            title,
        };
        self.set_panel_back("bot-thread-back", back_label, back_item, cx);
        // A plan's thread is its coordinator, named so beside the title: it
        // is not one of the steps' sessions.
        if coordinator {
            self.board.panel_title_note = Some(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child("Coordinator")
                    .into_any_element(),
            );
        }
        let body = div().size_full().child(view);
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
            session_choice: Default::default(),
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
            time_limit_minutes: None,
            time_added_minutes: None,
            warned_at: None,
            coordinator_choice: Default::default(),
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
    fn a_long_request_is_clipped_for_its_tooltip() {
        assert_eq!(clip_request("  Speed it up.  "), "Speed it up.");
        let long = "é".repeat(450);
        let clipped = clip_request(&long);
        assert_eq!(clipped.chars().count(), 401);
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn one_project_is_shared_when_no_step_names_another() {
        let mut w = work("w", WorkState::Running, 0);
        w.steps = vec![
            step("a", StepState::Done),
            step("b", StepState::Running),
            step("report", StepState::Pending),
        ];
        assert_eq!(shared_project(&w), None);
        w.steps[0].project = Some("billing".into());
        w.steps[1].project = Some("billing".into());
        // A step with no project (a report) does not break it.
        assert_eq!(shared_project(&w), Some("billing"));
        w.steps[2].project = Some("web".into());
        assert_eq!(shared_project(&w), None);
    }

    #[test]
    fn a_step_names_its_device_only_when_it_is_another_machine() {
        let mut label = super::super::items::ProjectLabel {
            glyph: icons::FOLDER,
            tint: gpui::black(),
            name: "web-api".into(),
            device: "Build box".into(),
            local: true,
        };
        assert_eq!(label.short(), "web-api");
        label.local = false;
        assert_eq!(label.short(), "web-api · Build box");
    }

    #[test]
    fn steps_open_when_they_are_under_way_or_still_to_read() {
        for state in [StepState::Running, StepState::Asking, StepState::Failed] {
            assert!(step_opens(state, false), "{state:?}");
        }
        for state in [StepState::Done, StepState::Pending, StepState::Skipped] {
            assert!(!step_opens(state, false), "{state:?}");
            assert!(step_opens(state, true), "{state:?}");
        }
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
