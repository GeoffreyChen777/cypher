//! The plan card (docs/design/agent-bots.md §13.1): a workspace agent's
//! `propose_plan` call in its conversation, drawn from the plan's record
//! rather than the call. The chat only announces the plan: its title, how
//! many steps and where, a line of summary, and Details / Reject / Approve
//! while it waits. Once decided the card says so and nothing more. The
//! steps, the progress and the result live in the detail panel, which the
//! card opens; a result notice ([`RowKind::BotPost`]) tells the chat when
//! the plan ends. A routine the agent saved is announced the same way
//! ([`RowKind::BotRoutine`]).
//!
//! The record is found by the call's id, else by the chat and title (a
//! record written before the call id was known). The row's version carries
//! a hash of the chat's plans ([`plans_key`]), so a decision redraws it.

use super::*;
use cypher_proto::{BotRecords, BotWork, WorkState};

/// Where a plan stands, as its card says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanPhase {
    /// Proposed; nobody decided yet.
    Awaiting,
    /// Approved here; its host has not started it yet.
    Starting,
    /// Cancelled here; its host has not closed it yet.
    Cancelling,
    Running,
    /// Stop requested; its host has not stopped it yet.
    Stopping,
    Done,
    Failed,
    Stopped,
    Cancelled,
}

impl PlanPhase {
    /// Approve / Cancel apply.
    pub fn awaiting(self) -> bool {
        self == Self::Awaiting
    }

    /// Stop applies.
    pub fn running(self) -> bool {
        self == Self::Running
    }

    /// The orchestration has ended (or never ran): the result shows.
    pub fn ended(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Stopped | Self::Cancelled
        )
    }

    /// The phase's colour on a card: amber while it waits on the owner,
    /// green done, red failed, quiet otherwise.
    pub fn tint(self, theme: &Theme) -> gpui::Hsla {
        match self {
            Self::Awaiting => theme.warning,
            Self::Done => theme.success,
            Self::Failed => theme.danger,
            Self::Running | Self::Starting | Self::Stopping => theme.text,
            Self::Cancelling | Self::Stopped | Self::Cancelled => theme.text_muted,
        }
    }
}

pub fn plan_phase(work: &BotWork) -> PlanPhase {
    match work.state {
        WorkState::Proposed => match &work.decision {
            None => PlanPhase::Awaiting,
            Some(decision) if decision.approve => PlanPhase::Starting,
            Some(_) => PlanPhase::Cancelling,
        },
        WorkState::Running if work.stop_requested_at.is_some() => PlanPhase::Stopping,
        WorkState::Running => PlanPhase::Running,
        WorkState::Done => PlanPhase::Done,
        WorkState::Failed => PlanPhase::Failed,
        WorkState::Stopped => PlanPhase::Stopped,
        WorkState::Cancelled => PlanPhase::Cancelled,
    }
}

/// A duration in its largest units: "40s", "12m", "1h 5m", "3d".
pub fn format_span(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3_600 => format!("{}m", s / 60),
        3_600..86_400 if (s / 60) % 60 == 0 => format!("{}h", s / 3_600),
        3_600..86_400 => format!("{}h {}m", s / 3_600, (s / 60) % 60),
        _ => format!("{}d", s / 86_400),
    }
}

fn steps_word(n: usize) -> String {
    format!("{n} step{}", if n == 1 { "" } else { "s" })
}

/// The plan's state in one line: "Running · step 2 of 4 · 12m",
/// "Done · took 25m", "Awaiting your approval · 3 steps".
pub fn phase_line(work: &BotWork, now_ms: i64) -> String {
    let (done, total) = work.progress();
    let took = work
        .started_at
        .zip(work.ended_at)
        .map(|(start, end)| format_span(end - start));
    match plan_phase(work) {
        PlanPhase::Awaiting => format!("Awaiting your approval · {}", steps_word(total)),
        PlanPhase::Starting => "Approved · starting…".into(),
        PlanPhase::Cancelling => "Cancelling…".into(),
        PlanPhase::Running => {
            let step = work
                .current_step()
                .map_or(done.min(total), |(ix, _)| ix + 1);
            let mut line = format!("Running · step {step} of {total}");
            if let Some(start) = work.started_at {
                line.push_str(&format!(" · {}", format_span(now_ms - start)));
            }
            line
        }
        PlanPhase::Stopping => "Stopping…".into(),
        PlanPhase::Done => took.map_or("Done".into(), |t| format!("Done · took {t}")),
        PlanPhase::Failed => took.map_or("Failed".into(), |t| format!("Failed after {t}")),
        PlanPhase::Stopped => took.map_or("Stopped".into(), |t| format!("Stopped after {t}")),
        PlanPhase::Cancelled => "Cancelled".into(),
    }
}

/// How long a step took, or has been running.
pub fn step_span(step: &cypher_proto::WorkStep, now_ms: i64) -> Option<String> {
    let start = step.started_at?;
    let end = step.ended_at.unwrap_or(now_ms);
    Some(format_span(end - start))
}

/// The record behind a `propose_plan` call: by the call's id, else the plan
/// of that title in the chat.
pub fn find_plan<'a>(
    records: &'a BotRecords,
    call_id: &str,
    chat_id: Option<&str>,
    title: &str,
) -> Option<&'a BotWork> {
    records
        .work
        .iter()
        .find(|w| w.tool_call_id.as_deref() == Some(call_id))
        .or_else(|| {
            let chat_id = chat_id?;
            records
                .work
                .iter()
                .filter(|w| w.chat_id == chat_id && w.tool_call_id.is_none())
                .find(|w| w.title.trim() == title.trim())
        })
}

/// A hash of what `chat_id`'s notice cards read from the records: its
/// plans, and the routines of `bot_id` (the workspace whose chat it is) as
/// their cards name them. Folded into those rows' versions.
pub(super) fn plans_key(records: &BotRecords, chat_id: Option<&str>, bot_id: Option<&str>) -> u64 {
    let Some(chat_id) = chat_id else {
        return 0;
    };
    let mut acc: Vec<u8> = Vec::new();
    for work in records.work.iter().filter(|w| w.chat_id == chat_id) {
        if let Ok(bytes) = serde_json::to_vec(work) {
            acc.extend_from_slice(&bytes);
        }
    }
    for routine in records
        .routines
        .iter()
        .filter(|r| Some(r.bot_id.as_str()) == bot_id)
    {
        for text in [&routine.id, &routine.name, &routine.schedule.describe()] {
            acc.extend_from_slice(text.as_bytes());
            acc.push(0);
        }
    }
    if acc.is_empty() { 0 } else { fnv1a(&acc) }
}

/// The plan title a `propose_plan` call names.
pub(super) fn call_title(call: &ToolCall) -> String {
    match call {
        ToolCall::Unknown {
            input: Some(input), ..
        } => input
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned(),
        _ => String::new(),
    }
}

/// Whether a tool part is a successful `propose_plan` call (a refused one
/// stays an ordinary chip).
pub(super) fn is_plan_call(part: &MessagePart) -> bool {
    matches!(
        part,
        MessagePart::Tool { call, is_error: false, .. }
            if cypher_proto::view::bot_tool_kind(call)
                == Some(cypher_proto::view::BotToolKind::ProposePlan)
    )
}

/// The routine a tool part saved: `Some` for a `save_routine` call that went
/// through, whose answer names the routine (a refused or still running
/// call, or an answer this build can't read, stays an ordinary chip).
pub(super) fn saved_routine(part: &MessagePart) -> Option<SavedRoutine> {
    match part {
        MessagePart::Tool {
            call,
            is_error: false,
            resolved: true,
            output: Some(output),
            ..
        } if cypher_proto::view::bot_tool_kind(call)
            == Some(cypher_proto::view::BotToolKind::SaveRoutine) =>
        {
            parse_saved_routine(output)
        }
        _ => None,
    }
}

/// A routine as its `save_routine` answer names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SavedRoutine {
    pub name: String,
    pub id: String,
}

/// Read `Saved routine "<name>" (id <id>): …`, the tool's answer (the doc
/// keeps a tool's first lines but not its input).
fn parse_saved_routine(output: &str) -> Option<SavedRoutine> {
    let rest = output.trim_start().strip_prefix("Saved routine \"")?;
    let (name, rest) = rest.split_once("\" (id ")?;
    let (id, _) = rest.split_once(')')?;
    let (name, id) = (name.trim(), id.trim());
    (!name.is_empty() && !id.is_empty() && !id.contains(char::is_whitespace)).then(|| {
        SavedRoutine {
            name: name.to_owned(),
            id: id.to_owned(),
        }
    })
}

/// The card's corner radius.
const PLAN_RADIUS: f32 = 14.0;

/// The card's glass, the composer pill's like a routine's result card: its
/// fill, border and frost, with its soft lift drawn only outside the shape.
pub(super) fn plan_surface(theme: &Theme, body: gpui::Div) -> AnyElement {
    let reach = crate::kit::soft_shadow::Reach {
        top: 3.0,
        side: 8.0,
        bottom: 14.0,
    };
    let face = div()
        .w_full()
        .rounded(px(PLAN_RADIUS))
        .bg(theme.input_glass_bg())
        .border_1()
        .border_color(theme.border)
        .flex()
        .flex_col()
        .child(body);
    div()
        .relative()
        .w_full()
        .child(crate::kit::frost::frosted(PLAN_RADIUS, 16.0, face))
        .child(
            div()
                .absolute()
                .top(px(-reach.top))
                .bottom(px(-reach.bottom))
                .left(px(-reach.side))
                .right(px(-reach.side))
                .child(
                    crate::kit::soft_shadow::outside_shadow(
                        PLAN_RADIUS,
                        reach,
                        theme.lift_shadow(),
                    )
                    .size_full(),
                ),
        )
        .into_any_element()
}

/// A hairline between the card's sections.
pub(super) fn plan_rule() -> gpui::Div {
    div()
        .mx(px(16.0))
        .h(px(1.0))
        .bg(crate::kit::theme::hairline(0.07))
}

/// A notice card's corner radius.
const NOTICE_RADIUS: f32 = 12.0;

/// The quiet box the chat's notices share (a plan, a result): a hairline
/// and a faint fill, no glass and no lift, so it reads as a note in the
/// conversation rather than a panel of its own.
fn notice_surface(theme: &Theme) -> gpui::Div {
    div()
        .w_full()
        .rounded(px(NOTICE_RADIUS))
        .border_1()
        .border_color(theme.border)
        .bg(crate::kit::theme::ink(0.025))
        .flex()
        .flex_col()
}

/// A notice's head: a small tile in its tone, the title over one quiet
/// line, and whatever closes the row (a status, a way in).
fn notice_head(
    glyph: &'static str,
    tone: gpui::Hsla,
    title: SharedString,
    sub: SharedString,
    theme: &Theme,
) -> gpui::Div {
    div()
        .px(px(12.0))
        .py(px(9.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .flex_none()
                .size(px(26.0))
                .rounded(px(7.0))
                .bg(tone.opacity(0.13))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    crate::kit::icons::icon(glyph)
                        .size(px(13.0))
                        .text_color(tone),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(13.0))
                        .line_height(px(17.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(title),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.5))
                        .line_height(px(15.0))
                        .text_color(theme.text_muted)
                        .child(sub),
                ),
        )
}

/// A notice's status at the end of its head: a small mark and a word.
fn notice_status(glyph: Option<&'static str>, word: &'static str, color: gpui::Hsla) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .text_size(px(11.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(color)
        .children(glyph.map(|glyph| {
            crate::kit::icons::icon(glyph)
                .size(px(11.0))
                .text_color(color)
        }))
        .child(SharedString::from(word))
}

/// "Details ›": the way from a notice into the detail panel.
fn details_link(key: SharedString, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(key.clone())
        .flex_none()
        .h(px(24.0))
        .pl(px(8.0))
        .pr(px(5.0))
        .rounded(px(7.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(3.0))
        .cursor_pointer()
        .text_size(px(12.0))
        .text_color(motion::hover_blend(&key, theme.text_muted, theme.text))
        .bg(motion::hover_blend(
            &key,
            crate::kit::theme::wash(0.0),
            crate::kit::theme::wash(0.08),
        ))
        .on_hover(motion::hover_listener(key.clone()))
        .child(SharedString::from("Details"))
        .child(
            crate::kit::icons::icon(crate::kit::icons::ALT_ARROW_RIGHT)
                .size(px(10.0))
                .text_color(motion::hover_blend(&key, theme.text_faint, theme.text)),
        )
}

/// How a decided plan reads on its card: the owner's decision, never the
/// run's progress (that is the panel's).
pub fn decision_word(phase: PlanPhase) -> Option<(&'static str, bool)> {
    match phase {
        PlanPhase::Awaiting => None,
        PlanPhase::Cancelling | PlanPhase::Cancelled => Some(("Rejected", false)),
        _ => Some(("Approved", true)),
    }
}

/// A plan card's one quiet line: "Plan · 4 steps · web-api · Studio",
/// and its time limit when it asked for its own: "· up to 8h".
pub fn plan_sub(steps: usize, place: Option<&str>, limit: Option<u32>) -> String {
    let mut sub = format!("Plan · {}", steps_word(steps));
    if let Some(place) = place {
        sub.push_str(&format!(" · {place}"));
    }
    if let Some(minutes) = limit {
        sub.push_str(&format!(" · up to {}", limit_words(minutes)));
    }
    sub
}

/// A time limit in hours and minutes: "8h", "1h 30m", "45m", "36h".
pub fn limit_words(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// A plan's own time limit, when it differs from its workspace's.
pub fn own_limit(work: &BotWork, workspace_minutes: u32) -> Option<u32> {
    work.time_limit_minutes
        .map(cypher_proto::clamp_time_limit)
        .filter(|minutes| *minutes != workspace_minutes)
}

/// When a notice happened, as its card says it: "Today 09:00", "Nov 3, 09:00".
pub fn notice_when(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|at| {
            let local = at.with_timezone(&chrono::Local);
            if local.date_naive() == chrono::Local::now().date_naive() {
                format!("Today {}", local.format("%H:%M"))
            } else {
                local.format("%b %-d, %H:%M").to_string()
            }
        })
        .unwrap_or_default()
}

impl Transcript {
    /// The plan card for the `propose_plan` call `call_id` (see the module
    /// docs).
    pub(super) fn render_bot_plan(
        &mut self,
        row_id: &SharedString,
        kind: &RowKind,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let RowKind::BotPlan {
            call_id,
            title,
            resolved,
        } = kind
        else {
            return gpui::Empty.into_any_element();
        };
        let found = {
            let state = self.state.read(cx);
            find_plan(&state.bot_records, call_id, self.chat_id.as_deref(), title).map(|work| {
                // Where it works, named when every step shares the project.
                let places: Vec<Option<String>> = work
                    .steps
                    .iter()
                    .map(|step| {
                        let space = state.space_row(step.project.as_deref()?)?;
                        Some(match state.device_name(&space.device_id) {
                            Some(device) => format!("{} · {device}", space.display_name()),
                            None => space.display_name().to_string(),
                        })
                    })
                    .collect();
                let common =
                    places.first().cloned().flatten().filter(|first| {
                        places.iter().all(|p| p.as_deref() == Some(first.as_str()))
                    });
                let workspace_limit = state
                    .bots
                    .iter()
                    .find(|b| b.id == work.bot_id)
                    .map_or(240, |b| b.plans.time_limit_minutes());
                (work.clone(), common, own_limit(work, workspace_limit))
            })
        };
        let Some((work, place, limit)) = found else {
            // Its record has not landed (or is gone): the title, and why
            // there is nothing more.
            let note = if *resolved {
                "This plan is no longer available."
            } else {
                "Writing the plan…"
            };
            let title = if title.is_empty() {
                SharedString::from("Plan")
            } else {
                title.clone()
            };
            return notice_surface(theme)
                .child(notice_head(
                    crate::kit::icons::CHECKLIST,
                    theme.text_muted,
                    title,
                    note.into(),
                    theme,
                ))
                .into_any_element();
        };
        let phase = plan_phase(&work);
        let tone = if phase.awaiting() {
            theme.warning
        } else {
            theme.text_muted
        };
        let open_id = work.id.clone();
        let details = details_link(format!("{row_id}-details").into(), theme).on_click(
            cx.listener(move |_, _, _, cx| {
                cx.emit(super::TranscriptEvent::OpenBotWork {
                    work_id: open_id.clone(),
                })
            }),
        );
        let head = notice_head(
            crate::kit::icons::CHECKLIST,
            tone,
            single_line(&work.title).into(),
            plan_sub(work.steps.len(), place.as_deref(), limit).into(),
            theme,
        );
        let Some((word, approved)) = decision_word(phase) else {
            // Waiting on the owner: a line of what it is for, and the
            // decision.
            let summary = work
                .summary
                .as_deref()
                .map(single_line)
                .filter(|s| !s.is_empty());
            let (approve, reject) = (work.id.clone(), work.id.clone());
            let approve_button = crate::kit::popover::btn_primary(theme, "Approve")
                .id(SharedString::from(format!("{row_id}-approve")))
                .py(px(4.0))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.bot_mutate(
                        serde_json::json!({
                            "op": "resolveBotPlan", "workId": approve, "approve": true,
                        }),
                        cx,
                    )
                }));
            let reject_button =
                crate::kit::popover::btn_ghost(theme, "Reject", format!("{row_id}-reject"))
                    .id(SharedString::from(format!("{row_id}-reject")))
                    .py(px(4.0))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.bot_mutate(
                            serde_json::json!({
                                "op": "resolveBotPlan", "workId": reject, "approve": false,
                            }),
                            cx,
                        )
                    }));
            return notice_surface(theme)
                .child(head.child(notice_status(None, "Awaiting approval", theme.warning)))
                .children(summary.map(|summary| {
                    div()
                        .px(px(12.0))
                        .pb(px(8.0))
                        .text_size(px(12.5))
                        .line_height(px(18.0))
                        .text_color(theme.text.opacity(0.85))
                        .line_clamp(2)
                        .child(SharedString::from(summary))
                }))
                .child(plan_rule())
                .child(
                    div()
                        .px(px(8.0))
                        .py(px(7.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(details)
                        .child(div().flex_1())
                        .child(reject_button)
                        .child(approve_button),
                )
                .into_any_element();
        };
        let (glyph, color) = if approved {
            (crate::kit::icons::CHECK, theme.text_muted)
        } else {
            (crate::kit::icons::CROSS, theme.text_muted)
        };
        notice_surface(theme)
            .child(
                head.pr(px(8.0))
                    .child(notice_status(Some(glyph), word, color))
                    .child(details),
            )
            .into_any_element()
    }

    /// A saved routine's card ([`RowKind::BotRoutine`]): its name over where
    /// and when it runs, and the way to its page. Read from the routine's
    /// record; one that is gone keeps the name the call gave.
    pub(super) fn render_bot_routine(
        &mut self,
        row_id: &SharedString,
        kind: &RowKind,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let RowKind::BotRoutine {
            name, routine_id, ..
        } = kind
        else {
            return gpui::Empty.into_any_element();
        };
        let found = {
            let state = self.state.read(cx);
            state
                .bot_records
                .routines
                .iter()
                .find(|r| r.id == routine_id.as_ref())
                .map(|routine| {
                    let place = routine
                        .target
                        .space_id()
                        .and_then(|id| state.space_row(id))
                        .map(|space| space.display_name().to_string());
                    let mut sub = format!("Routine · {}", routine.schedule.describe());
                    if let Some(place) = place {
                        sub.push_str(&format!(" · in {place}"));
                    }
                    (routine.id.clone(), single_line(&routine.name), sub)
                })
        };
        let Some((id, title, sub)) = found else {
            return notice_surface(theme)
                .child(notice_head(
                    crate::kit::icons::CLOCK,
                    theme.text_muted,
                    name.clone(),
                    "Routine · no longer here".into(),
                    theme,
                ))
                .into_any_element();
        };
        let key: SharedString = format!("{row_id}-open").into();
        let arrow = crate::kit::icons::icon(crate::kit::icons::ALT_ARROW_RIGHT)
            .size(px(10.0))
            .flex_none()
            .text_color(motion::hover_blend(&key, theme.text_faint, theme.text));
        notice_surface(theme)
            .id(key.clone())
            .cursor_pointer()
            .bg(motion::hover_blend(
                &key,
                crate::kit::theme::ink(0.025),
                crate::kit::theme::ink(0.05),
            ))
            .on_hover(motion::hover_listener(key.clone()))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(super::TranscriptEvent::OpenBotRoutine {
                    routine_id: id.clone(),
                })
            }))
            .child(
                notice_head(
                    crate::kit::icons::CLOCK,
                    theme.text_muted,
                    title.into(),
                    sub.into(),
                    theme,
                )
                .child(notice_status(
                    Some(crate::kit::icons::CHECK),
                    "Saved",
                    theme.text_muted,
                ))
                .child(arrow),
            )
            .into_any_element()
    }

    /// A result notice's card ([`RowKind::BotPost`]): what ended, how, and
    /// the way to its details. The whole card opens them.
    pub(super) fn render_bot_post(
        &mut self,
        row_id: &SharedString,
        kind: &RowKind,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let RowKind::BotPost {
            entry,
            title,
            source,
            outcome,
            at,
            took_ms,
        } = kind
        else {
            return gpui::Empty.into_any_element();
        };
        use crate::kit::icons::{CHECK, CHECKLIST, CLOCK, DANGER_TRIANGLE};
        let (status_glyph, word, color) = match outcome {
            PostOutcome::Done => (Some(CHECK), "Done", theme.success),
            PostOutcome::Failed => (Some(DANGER_TRIANGLE), "Failed", theme.danger),
            PostOutcome::Stopped => (None, "Stopped", theme.text_muted),
        };
        let (glyph, sub) = match source {
            PostSource::Routine => (CLOCK, format!("Routine · {}", notice_when(*at))),
            PostSource::Plan => (
                CHECKLIST,
                match took_ms {
                    Some(ms) => format!("Plan · took {}", format_span(*ms)),
                    None => format!("Plan · {}", notice_when(*at)),
                },
            ),
        };
        let tone = match outcome {
            PostOutcome::Done => theme.success,
            PostOutcome::Failed => theme.danger,
            PostOutcome::Stopped => theme.text_muted,
        };
        let key: SharedString = format!("{row_id}-open").into();
        let entry_id = entry.to_string();
        let arrow = crate::kit::icons::icon(crate::kit::icons::ALT_ARROW_RIGHT)
            .size(px(10.0))
            .flex_none()
            .text_color(motion::hover_blend(&key, theme.text_faint, theme.text));
        notice_surface(theme)
            .id(key.clone())
            .cursor_pointer()
            .bg(motion::hover_blend(
                &key,
                crate::kit::theme::ink(0.025),
                crate::kit::theme::ink(0.05),
            ))
            .on_hover(motion::hover_listener(key.clone()))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(super::TranscriptEvent::OpenBotPost {
                    entry_id: entry_id.clone(),
                })
            }))
            .child(
                notice_head(glyph, tone, title.clone(), sub.into(), theme)
                    .child(notice_status(status_glyph, word, color))
                    .child(arrow),
            )
            .into_any_element()
    }

    /// Fire an owner op on a bot record; its row update redraws the card.
    fn bot_mutate(&mut self, params: serde_json::Value, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        cx.spawn(async move |_, _| {
            if let Err(err) = engine
                .client()
                .call(cypher_rpc::methods::MUTATE, params)
                .await
            {
                tracing::warn!(error = %err, "bot plan action failed");
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::{PlanDecision, StepState, WorkStep};

    fn step(state: StepState) -> WorkStep {
        WorkStep {
            id: "s".into(),
            title: "Step".into(),
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

    fn work(state: WorkState, steps: Vec<WorkStep>) -> BotWork {
        BotWork {
            id: "w".into(),
            bot_id: "b".into(),
            title: "Fix invoice rounding".into(),
            instruction: String::new(),
            summary: None,
            steps,
            state,
            chat_id: "chat".into(),
            tool_call_id: Some("call-1".into()),
            thread_chat_id: None,
            created_at: 0,
            started_at: None,
            ended_at: None,
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
    fn phases_follow_the_record_and_the_owner_fields() {
        let mut plan = work(WorkState::Proposed, vec![step(StepState::Pending)]);
        assert_eq!(plan_phase(&plan), PlanPhase::Awaiting);
        assert_eq!(phase_line(&plan, 0), "Awaiting your approval · 1 step");
        plan.decision = Some(PlanDecision {
            approve: true,
            at: 1,
        });
        assert_eq!(plan_phase(&plan), PlanPhase::Starting);
        plan.decision = Some(PlanDecision {
            approve: false,
            at: 1,
        });
        assert_eq!(plan_phase(&plan), PlanPhase::Cancelling);

        let mut running = work(
            WorkState::Running,
            vec![
                step(StepState::Done),
                step(StepState::Asking),
                step(StepState::Pending),
            ],
        );
        running.started_at = Some(0);
        assert_eq!(plan_phase(&running), PlanPhase::Running);
        assert_eq!(
            phase_line(&running, 12 * 60_000),
            "Running · step 2 of 3 · 12m"
        );
        running.stop_requested_at = Some(5);
        assert_eq!(plan_phase(&running), PlanPhase::Stopping);

        let mut done = work(WorkState::Done, vec![step(StepState::Done)]);
        done.started_at = Some(0);
        done.ended_at = Some(65 * 60_000);
        assert_eq!(phase_line(&done, 0), "Done · took 1h 5m");
        assert!(plan_phase(&done).ended());
        assert_eq!(phase_line(&work(WorkState::Failed, vec![]), 0), "Failed");
    }

    #[test]
    fn a_plan_call_is_a_card_of_its_own() {
        let tool = |id: &str, name: &str, is_error: bool| MessagePart::Tool {
            id: id.into(),
            call: ToolCall::Unknown {
                name: name.into(),
                input: Some(serde_json::json!({ "title": "Fix invoice rounding" })),
            },
            is_error,
            resolved: true,
            output: None,
            progress: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            permission: None,
        };
        let entry = SessionMessageEntry {
            role: MessageRole::Assistant,
            parts: vec![
                tool("read", "list_projects", false),
                tool("call-1", "propose_plan", false),
                tool("refused", "propose_plan", true),
            ],
            ..crate::test_fixtures::entry()
        };
        let rows = rows_for_entry(&entry, false, &mut |_, text| Arc::new(parse_full(text)));
        let kinds: Vec<&str> = rows
            .iter()
            .map(|row| match &row.kind {
                RowKind::ToolGroup { .. } => "group",
                RowKind::Activity { .. } => "activity",
                RowKind::BotPlan { call_id, title, .. } => {
                    assert_eq!(call_id.as_ref(), "call-1");
                    assert_eq!(title.as_ref(), "Fix invoice rounding");
                    "plan"
                }
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["group", "plan", "group"]);
    }

    #[test]
    fn a_decided_plan_says_the_decision_not_the_progress() {
        use PlanPhase::*;
        assert_eq!(decision_word(Awaiting), None);
        for phase in [Starting, Running, Stopping, Done, Failed, Stopped] {
            assert_eq!(decision_word(phase), Some(("Approved", true)), "{phase:?}");
        }
        for phase in [Cancelling, Cancelled] {
            assert_eq!(decision_word(phase), Some(("Rejected", false)), "{phase:?}");
        }
        assert_eq!(plan_sub(1, None, None), "Plan · 1 step");
        assert_eq!(plan_sub(2, None, Some(90)), "Plan · 2 steps · up to 1h 30m");
        assert_eq!(
            plan_sub(4, Some("web-api · Studio"), None),
            "Plan · 4 steps · web-api · Studio"
        );
    }

    #[test]
    fn a_result_notice_is_one_card_and_its_turn_draws_nothing() {
        let post = |id: &str, text: &str, from: cypher_proto::BotPost| SessionMessageEntry {
            id: id.into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Post {
                id: "p0".into(),
                text: text.into(),
                from,
            }],
            ..crate::test_fixtures::entry()
        };
        let mut parse = |_: &str, text: &str| Arc::new(parse_full(text));
        // A routine's run: one card, however long its output.
        let routine = post(
            "routine-r-5",
            "# Report\n\nThree sessions moved.\n\n- one\n- two",
            cypher_proto::BotPost::Routine {
                routine_id: "r".into(),
                name: "Morning brief".into(),
                outcome: cypher_proto::RunOutcome::Failed,
                run_at: 5,
                thread_chat_id: None,
            },
        );
        let rows = rows_for_entry(&routine, false, &mut parse);
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            &rows[0].kind,
            RowKind::BotPost { entry, title, source: PostSource::Routine, outcome: PostOutcome::Failed, at: 5, took_ms: None }
                if entry.as_ref() == "routine-r-5" && title.as_ref() == "Morning brief"
        ));
        // A plan's end, with how long it took.
        let plan = post(
            "work-w-ended",
            "Migrated invoices.",
            cypher_proto::BotPost::Work {
                work_id: "w".into(),
                title: "Fix invoice rounding".into(),
                outcome: WorkState::Done,
                started_at: Some(1_000),
                ended_at: 61_000,
            },
        );
        let rows = rows_for_entry(&plan, false, &mut parse);
        assert!(matches!(
            &rows[..],
            [Row {
                kind: RowKind::BotPost {
                    source: PostSource::Plan,
                    outcome: PostOutcome::Done,
                    took_ms: Some(60_000),
                    ..
                },
                ..
            }]
        ));
        // A source this build doesn't know reads as the text it carries.
        let unknown = post("x", "Plain words.", cypher_proto::BotPost::Unknown);
        let rows = rows_for_entry(&unknown, false, &mut parse);
        assert!(matches!(&rows[0].kind, RowKind::Markdown { .. }));
        // The turn that tells the agent draws nothing, sent or still pending.
        let told = SessionMessageEntry {
            id: format!("{}work-w-ended", cypher_proto::BOT_EVENT_PREFIX),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "t0".into(),
                text: "[Plan \"Fix invoice rounding\" finished]".into(),
                agent_text: None,
            }],
            ..crate::test_fixtures::entry()
        };
        assert!(rows_for_entry(&told, false, &mut parse).is_empty());
        assert!(rows_for_entry(&told, true, &mut parse).is_empty());
    }

    #[test]
    fn spans_read_in_their_largest_units() {
        assert_eq!(format_span(40_000), "40s");
        assert_eq!(format_span(12 * 60_000), "12m");
        assert_eq!(format_span(2 * 3_600_000), "2h");
        assert_eq!(format_span(3 * 86_400_000), "3d");
        assert_eq!(format_span(-5), "0s");
    }

    #[test]
    fn plans_are_found_by_call_then_by_title() {
        let mut records = BotRecords::default();
        let mut by_title = work(WorkState::Proposed, vec![]);
        by_title.id = "t".into();
        by_title.tool_call_id = None;
        by_title.title = "Other plan".into();
        records.work = vec![work(WorkState::Proposed, vec![]), by_title];
        assert_eq!(
            find_plan(&records, "call-1", Some("chat"), "x").map(|w| w.id.as_str()),
            Some("w")
        );
        assert_eq!(
            find_plan(&records, "call-2", Some("chat"), " Other plan").map(|w| w.id.as_str()),
            Some("t")
        );
        assert!(find_plan(&records, "call-2", Some("elsewhere"), "Other plan").is_none());
        assert_ne!(plans_key(&records, Some("chat"), None), 0);
        assert_eq!(plans_key(&records, Some("elsewhere"), None), 0);
        // A workspace's routines count in its own chat's key, so a routine's
        // card follows a rename or a new schedule.
        let mut routine = crate::test_fixtures::bot_routine("r", "b");
        records.routines = vec![routine.clone()];
        let before = plans_key(&records, Some("elsewhere"), Some("b"));
        assert_ne!(before, 0);
        assert_eq!(plans_key(&records, Some("elsewhere"), Some("other")), 0);
        routine.name = "Renamed".into();
        records.routines = vec![routine];
        assert_ne!(plans_key(&records, Some("elsewhere"), Some("b")), before);
    }

    #[test]
    fn a_saved_routine_is_a_card_named_by_its_answer() {
        assert_eq!(
            parse_saved_routine(
                "Saved routine \"Morning brief\" (id 2974bf05-4a69): Manual. It runs in the background."
            ),
            Some(SavedRoutine {
                name: "Morning brief".into(),
                id: "2974bf05-4a69".into(),
            })
        );
        assert_eq!(parse_saved_routine("No routine \"x\"."), None);
        assert_eq!(parse_saved_routine("Saved routine \"x\" (id )"), None);
        // The doc keeps a workspace tool's answer, not its input.
        let tool = |is_error: bool, resolved: bool, output: Option<&str>| MessagePart::Tool {
            id: "call".into(),
            call: ToolCall::Unknown {
                name: "save_routine".into(),
                input: None,
            },
            is_error,
            resolved,
            output: output.map(str::to_owned),
            progress: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            permission: None,
        };
        let saved = Some("Saved routine \"Morning brief\" (id r1): Manual.");
        assert!(saved_routine(&tool(false, true, saved)).is_some());
        assert_eq!(
            saved_routine(&tool(true, true, saved)),
            None,
            "a refused call"
        );
        assert_eq!(
            saved_routine(&tool(false, false, saved)),
            None,
            "one still running"
        );
        assert_eq!(
            saved_routine(&tool(false, true, None)),
            None,
            "an answer not kept"
        );
        let entry = SessionMessageEntry {
            role: MessageRole::Assistant,
            parts: vec![tool(false, true, saved), tool(true, true, saved)],
            ..crate::test_fixtures::entry()
        };
        let rows = rows_for_entry(&entry, false, &mut |_, text| Arc::new(parse_full(text)));
        assert!(matches!(
            &rows[..],
            [
                Row { kind: RowKind::BotRoutine { name, routine_id, .. }, .. },
                Row { kind: RowKind::ToolGroup { .. }, .. },
            ] if name.as_ref() == "Morning brief" && routine_id.as_ref() == "r1"
        ));
    }
}
