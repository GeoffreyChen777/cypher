//! The plan card (docs/design/agent-bots.md §6.1, §6.6): a bot's
//! `propose_plan` call in its conversation, drawn from the plan's record
//! rather than the call. One card through the plan's life: its steps with
//! Approve / Cancel while it waits, live progress and Stop while it runs,
//! the result once it ends.
//!
//! The record is found by the call's id, else by the chat and title (a
//! record written before the call id was known). The row's version carries
//! a hash of the chat's plans ([`plans_key`]), so a state change that
//! resizes the card re-measures it.

use super::*;
use cypher_proto::{BotRecords, BotWork, StepState, WorkState};

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

/// A hash of `chat_id`'s plans, folded into its plan rows' versions.
pub(super) fn plans_key(records: &BotRecords, chat_id: Option<&str>) -> u64 {
    let Some(chat_id) = chat_id else {
        return 0;
    };
    let mut acc: Vec<u8> = Vec::new();
    for work in records.work.iter().filter(|w| w.chat_id == chat_id) {
        if let Ok(bytes) = serde_json::to_vec(work) {
            acc.extend_from_slice(&bytes);
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

/// One step's project as the card names it.
struct StepProject {
    glyph: &'static str,
    tint: gpui::Hsla,
    name: String,
    device: Option<String>,
}

/// A session a step started.
struct StepSession {
    id: String,
    title: String,
    status: cypher_proto::ChatIndicator,
}

/// The card's corner radius.
const PLAN_RADIUS: f32 = 14.0;

/// The card's glass, the composer pill's like a routine's result card: its
/// fill, border and frost, with its soft lift drawn only outside the shape.
fn plan_surface(theme: &Theme, body: gpui::Div) -> AnyElement {
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

/// The phase's colour for the card's tile: amber while it waits on the
/// owner, the accent while it works, green done, red failed.
fn phase_tone(phase: PlanPhase, theme: &Theme) -> gpui::Hsla {
    match phase {
        PlanPhase::Awaiting => theme.warning,
        PlanPhase::Starting | PlanPhase::Running | PlanPhase::Stopping => theme.accent,
        PlanPhase::Done => theme.success,
        PlanPhase::Failed => theme.danger,
        PlanPhase::Cancelling | PlanPhase::Stopped | PlanPhase::Cancelled => theme.text_muted,
    }
}

/// The header's status, at its right end with no background: how the plan
/// stands and, once it has run, for how long.
fn phase_status(
    work: &BotWork,
    phase: PlanPhase,
    now_ms: i64,
    theme: &Theme,
    view: EntityId,
) -> gpui::Div {
    let took = |end: i64| {
        work.started_at
            .map(|start| format_span(end.saturating_sub(start)))
    };
    let ended = work.ended_at.unwrap_or(now_ms);
    let with_time = |word: &str| match took(ended) {
        Some(t) => format!("{word} · {t}"),
        None => word.to_string(),
    };
    let (mark, text, color): (Option<AnyElement>, String, gpui::Hsla) = match phase {
        PlanPhase::Awaiting => (
            Some(
                div()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(theme.warning)
                    .into_any_element(),
            ),
            "Awaiting approval".into(),
            theme.warning,
        ),
        PlanPhase::Starting => (None, "Starting…".into(), theme.text_muted),
        PlanPhase::Cancelling => (None, "Cancelling…".into(), theme.text_muted),
        PlanPhase::Running => (
            Some(crate::kit::loaders::status_arc(theme.accent, 11.0, view).into_any_element()),
            with_time("Running"),
            theme.text,
        ),
        PlanPhase::Stopping => (None, "Stopping…".into(), theme.text_muted),
        PlanPhase::Done => (
            Some(
                crate::kit::icons::icon(crate::kit::icons::CHECK)
                    .size(px(11.0))
                    .text_color(theme.success)
                    .into_any_element(),
            ),
            with_time("Done"),
            theme.success,
        ),
        PlanPhase::Failed => (
            Some(
                crate::kit::icons::icon(crate::kit::icons::DANGER_TRIANGLE)
                    .size(px(11.0))
                    .text_color(theme.danger)
                    .into_any_element(),
            ),
            with_time("Failed"),
            theme.danger,
        ),
        PlanPhase::Stopped => (None, with_time("Stopped"), theme.text_muted),
        PlanPhase::Cancelled => (None, "Cancelled".into(), theme.text_muted),
    };
    div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(5.0))
        .text_size(px(11.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(color)
        .children(mark)
        .child(SharedString::from(text))
}

/// The card's head: the plan's tile in its phase's colour, the title over
/// "Plan · 4 steps · <where>", and its status at the right.
fn plan_head(
    title: &str,
    sub: String,
    tone: gpui::Hsla,
    status: Option<gpui::Div>,
    theme: &Theme,
) -> gpui::Div {
    div()
        .px(px(16.0))
        .pt(px(13.0))
        .pb(px(12.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .flex_none()
                .size(px(28.0))
                .rounded(px(8.0))
                .bg(tone.opacity(0.14))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    crate::kit::icons::icon(crate::kit::icons::CHECKLIST)
                        .size(px(14.0))
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
                        .text_size(px(13.5))
                        .line_height(px(18.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(SharedString::from(title.to_owned())),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(sub)),
                ),
        )
        .children(status)
}

/// Whether a step names its own place: it has a project and the header
/// couldn't name one for every step.
fn place_shown(common: &Option<String>, project: &Option<StepProject>) -> bool {
    common.is_none() && project.is_some()
}

/// A hairline between the card's sections.
fn plan_rule() -> gpui::Div {
    div()
        .mx(px(16.0))
        .h(px(1.0))
        .bg(crate::kit::theme::hairline(0.07))
}

/// A plan whose record has not landed (or is gone): its title, and why
/// there is nothing more.
fn plan_placeholder(title: &str, resolved: bool, theme: &Theme) -> AnyElement {
    let note = if resolved {
        "This plan is no longer available."
    } else {
        "Writing the plan…"
    };
    let title = if title.is_empty() { "Plan" } else { title };
    plan_surface(
        theme,
        div().child(plan_head(title, note.into(), theme.text_muted, None, theme)),
    )
}

/// A step's state mark: its number while the plan waits, then how it went.
fn step_mark(
    state: StepState,
    number: usize,
    numbered: bool,
    theme: &Theme,
    view: EntityId,
) -> AnyElement {
    let cell = || {
        div()
            .size(px(18.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
    };
    let glyph = |path, color| {
        cell()
            .child(
                crate::kit::icons::icon(path)
                    .size(px(12.0))
                    .text_color(color),
            )
            .into_any_element()
    };
    if numbered {
        return cell()
            .child(
                div()
                    .size(px(17.0))
                    .rounded_full()
                    .border_1()
                    .border_color(crate::kit::theme::hairline(0.16))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child(SharedString::from(number.to_string())),
            )
            .into_any_element();
    }
    match state {
        StepState::Pending => cell()
            .child(
                div()
                    .size(px(9.0))
                    .rounded_full()
                    .border_1()
                    .border_color(theme.text_faint.opacity(0.8)),
            )
            .into_any_element(),
        StepState::Running => cell()
            .child(crate::kit::loaders::status_arc(
                theme.text_muted,
                12.0,
                view,
            ))
            .into_any_element(),
        StepState::Asking => glyph(crate::kit::icons::QUESTION_CIRCLE, theme.warning),
        StepState::Done => glyph(crate::kit::icons::CHECK, theme.success),
        StepState::Failed => glyph(crate::kit::icons::CROSS, theme.danger),
        StepState::Skipped => cell()
            .child(
                div()
                    .w(px(8.0))
                    .h(px(1.5))
                    .rounded_full()
                    .bg(theme.text_faint),
            )
            .into_any_element(),
    }
}

/// The steps-done bar of a running plan (the Work card's too).
pub fn progress_bar(done: usize, total: usize, theme: &Theme) -> gpui::Div {
    let fraction = if total == 0 {
        0.0
    } else {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    };
    div()
        .w_full()
        .h(px(3.0))
        .rounded_full()
        .bg(crate::kit::theme::ink(0.08))
        .child(
            div()
                .h_full()
                .w(gpui::relative(fraction))
                .rounded_full()
                .bg(theme.accent),
        )
}

impl Transcript {
    /// The plan card for the `propose_plan` call `call_id` (see the module
    /// docs).
    pub(super) fn render_bot_plan(
        &mut self,
        row_id: &SharedString,
        kind: &RowKind,
        theme: &Theme,
        window: &mut Window,
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
        let resolved = *resolved;
        let now = chrono::Utc::now();
        let now_ms = now.timestamp_millis();
        let view = cx.entity_id();
        let found = {
            let state = self.state.read(cx);
            find_plan(&state.bot_records, call_id, self.chat_id.as_deref(), title).map(|work| {
                let steps: Vec<(Option<StepProject>, Vec<StepSession>)> = work
                    .steps
                    .iter()
                    .map(|step| {
                        let project = step
                            .project
                            .as_deref()
                            .and_then(|id| state.space_row(id))
                            .map(|space| StepProject {
                                glyph: crate::appearance::space_style::space_icon(
                                    space.icon.as_deref(),
                                ),
                                tint: crate::appearance::space_style::space_color(
                                    space.color.as_deref(),
                                    theme,
                                )
                                .unwrap_or(theme.text_muted),
                                name: space.display_name().to_string(),
                                device: state.device_name(&space.device_id).map(str::to_owned),
                            });
                        let sessions = step
                            .sessions
                            .iter()
                            .filter_map(|id| state.chats.iter().find(|c| c.id == *id))
                            .map(|chat| StepSession {
                                id: chat.id.clone(),
                                title: single_line(chat.title.as_deref().unwrap_or("New session")),
                                status: state.display_status_for(chat, now),
                            })
                            .collect();
                        (project, sessions)
                    })
                    .collect();
                (work.clone(), steps)
            })
        };
        let Some((work, steps)) = found else {
            return plan_placeholder(title, resolved, theme);
        };
        let phase = plan_phase(&work);
        let (done, total) = work.progress();
        let numbered = matches!(
            phase,
            PlanPhase::Awaiting
                | PlanPhase::Starting
                | PlanPhase::Cancelling
                | PlanPhase::Cancelled
        );
        // Where it works, named once when every step shares the project.
        let place_of = |project: &StepProject| match &project.device {
            Some(device) => format!("{} · {device}", project.name),
            None => project.name.clone(),
        };
        let places: Vec<Option<String>> = steps
            .iter()
            .map(|(project, _)| project.as_ref().map(place_of))
            .collect();
        let common = places
            .first()
            .cloned()
            .flatten()
            .filter(|first| places.iter().all(|p| p.as_deref() == Some(first.as_str())));
        let mut sub = format!("Plan · {}", steps_word(total));
        if let Some(place) = &common {
            sub.push_str(&format!(" · {place}"));
        }
        let mut body = div()
            .px(px(16.0))
            .pt(px(12.0))
            .pb(px(14.0))
            .flex()
            .flex_col();
        if let Some(summary) = work.summary.as_ref().filter(|s| !s.trim().is_empty()) {
            body = body.child(
                div()
                    .mb(px(12.0))
                    .text_size(px(13.0))
                    .line_height(px(19.0))
                    .text_color(theme.text)
                    .child(SharedString::from(summary.clone())),
            );
        }
        if matches!(phase, PlanPhase::Running | PlanPhase::Stopping) {
            body = body.child(
                div()
                    .mb(px(12.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().flex_1().child(progress_bar(done, total, theme)))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!("{done} of {total}"))),
                    ),
            );
        }
        let last = work.steps.len().saturating_sub(1);
        let step_rows =
            work.steps
                .iter()
                .zip(steps)
                .enumerate()
                .map(|(ix, (step, (project, sessions)))| {
                    let span = (!numbered).then(|| step_span(step, now_ms)).flatten();
                    // Its own place only when the header can't name one for all.
                    let place = project
                        .as_ref()
                        .filter(|_| common.is_none())
                        .map(|p| (p.glyph, p.tint, place_of(p)));
                    let after_place = if place.is_some() { "· " } else { "" };
                    let sub = div()
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(5.0))
                        .text_size(px(11.5))
                        .line_height(px(15.0))
                        .text_color(theme.text_muted.opacity(0.85))
                        .when_some(place, |el, (glyph, tint, place)| {
                            el.child(
                                crate::kit::icons::icon(glyph)
                                    .size(px(11.0))
                                    .flex_none()
                                    .text_color(tint),
                            )
                            .child(div().min_w_0().truncate().child(SharedString::from(place)))
                        })
                        .when_some(step.note.clone().filter(|_| !numbered), |el, note| {
                            el.child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(format!("{after_place}{note}"))),
                            )
                        });
                    let has_sub =
                        place_shown(&common, &project) || (!numbered && step.note.is_some());
                    let detail = step
                        .detail
                        .clone()
                        .filter(|d| !d.trim().is_empty() && numbered);
                    let session_rows = sessions.into_iter().enumerate().map(|(six, session)| {
                        let link = format!(
                            "{}{}",
                            crate::markdown::session_link::SESSION_LINK_PREFIX,
                            session.id
                        );
                        let key = format!("{row_id}-s{ix}-{six}");
                        let mark_color = match session.status {
                            cypher_proto::ChatIndicator::Working => theme.accent,
                            cypher_proto::ChatIndicator::AwaitingInput => theme.warning,
                            cypher_proto::ChatIndicator::Errored => theme.danger,
                            cypher_proto::ChatIndicator::Completed => theme.success,
                            cypher_proto::ChatIndicator::Idle => theme.text_faint,
                        };
                        div()
                            .id(SharedString::from(key.clone()))
                            .h(px(22.0))
                            .px(px(6.0))
                            .ml(px(-6.0))
                            .rounded(px(6.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(7.0))
                            .cursor_pointer()
                            .text_size(px(12.0))
                            .text_color(motion::hover_blend(&key, theme.text_muted, theme.text))
                            .bg(motion::hover_blend(
                                &key,
                                crate::kit::theme::wash(0.0),
                                crate::kit::theme::wash(0.06),
                            ))
                            .on_hover(motion::hover_listener(key.clone()))
                            .on_click(move |_, window, cx| {
                                crate::markdown::session_link::open_link(&link, window, cx)
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(6.0))
                                    .rounded_full()
                                    .bg(mark_color),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(session.title)),
                            )
                            .child(
                                crate::kit::icons::icon(crate::kit::icons::ALT_ARROW_RIGHT)
                                    .size(px(10.0))
                                    .flex_none()
                                    .text_color(theme.text_faint.opacity(motion::hover_t(&key))),
                            )
                    });
                    // A timeline: each step's mark, joined to the next by a
                    // hairline that runs the step's full height.
                    let rail = div()
                        .flex_none()
                        .w(px(18.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .child(step_mark(step.state, ix + 1, numbered, theme, view))
                        .when(ix < last, |el| {
                            el.child(
                                div()
                                    .flex_1()
                                    .w(px(1.0))
                                    .mt(px(3.0))
                                    .bg(crate::kit::theme::hairline(0.1)),
                            )
                        });
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .items_stretch()
                        .gap(px(10.0))
                        .child(rail)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .pb(px(if ix < last { 12.0 } else { 0.0 }))
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .line_height(px(18.0))
                                        .text_color(if step.state == StepState::Skipped {
                                            theme.text_muted
                                        } else {
                                            theme.text
                                        })
                                        .child(SharedString::from(step.title.clone())),
                                )
                                .when_some(detail, |el, detail| {
                                    el.child(
                                        div()
                                            .text_size(px(12.0))
                                            .line_height(px(17.0))
                                            .text_color(theme.text_muted)
                                            .child(SharedString::from(detail)),
                                    )
                                })
                                .when(has_sub, |el| el.child(sub))
                                .when(!numbered, |el| el.children(session_rows)),
                        )
                        .when_some(span, |el, span| {
                            el.child(
                                div()
                                    .flex_none()
                                    .pt(px(1.0))
                                    .text_size(px(11.0))
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(span)),
                            )
                        })
                });
        body = body.child(div().flex().flex_col().children(step_rows));
        if phase.ended()
            && let Some(result) = work.result.as_ref().filter(|r| !r.trim().is_empty())
        {
            body = body.child(
                div()
                    .mt(px(14.0))
                    .px(px(12.0))
                    .py(px(9.0))
                    .rounded(px(9.0))
                    .bg(crate::kit::theme::ink(0.04))
                    .text_color(theme.text)
                    // Markdown like any answer: the result cites its
                    // sessions as links.
                    .child(crate::markdown::render::render_tree(
                        &crate::markdown::parser::parse_full(result),
                        &crate::markdown::render::RenderOptions::settled(
                            format!("{row_id}-result").into(),
                        ),
                        theme,
                        window,
                        &|_| None,
                    )),
            );
        }
        let card = div()
            .flex()
            .flex_col()
            .child(plan_head(
                &work.title,
                sub,
                phase_tone(phase, theme),
                Some(phase_status(&work, phase, now_ms, theme, view)),
                theme,
            ))
            .child(plan_rule())
            .child(body);
        let actions = self.plan_actions(row_id, &work, phase, theme, cx);
        let card = match actions {
            Some(actions) => card.child(plan_rule()).child(actions),
            None => card,
        };
        plan_surface(theme, card)
    }

    /// Approve / Cancel while the plan waits; Stop (armed by a first click)
    /// while it runs.
    fn plan_actions(
        &mut self,
        row_id: &SharedString,
        work: &BotWork,
        phase: PlanPhase,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let row = div()
            .px(px(16.0))
            .py(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0));
        let hint = |text: &'static str| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .text_color(theme.text_muted)
                .child(text)
        };
        if phase.awaiting() {
            let (approve, cancel) = (work.id.clone(), work.id.clone());
            let (edit_id, edit_title) = (work.id.clone(), work.title.clone());
            let approve_button = crate::kit::popover::btn_primary(theme, "Approve")
                .id(SharedString::from(format!("{row_id}-approve")))
                .py(px(5.0))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.bot_mutate(
                        serde_json::json!({
                            "op": "resolveBotPlan", "workId": approve, "approve": true,
                        }),
                        cx,
                    )
                }));
            let edit_button =
                crate::kit::popover::btn_ghost(theme, "Edit", format!("{row_id}-edit"))
                    .id(SharedString::from(format!("{row_id}-edit")))
                    .py(px(5.0))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(super::TranscriptEvent::EditBotPlan {
                            work_id: edit_id.clone(),
                            title: edit_title.clone(),
                        })
                    }));
            let cancel_button =
                crate::kit::popover::btn_ghost(theme, "Cancel", format!("{row_id}-cancel"))
                    .id(SharedString::from(format!("{row_id}-cancel")))
                    .py(px(5.0))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.bot_mutate(
                            serde_json::json!({
                                "op": "resolveBotPlan", "workId": cancel, "approve": false,
                            }),
                            cx,
                        )
                    }));
            return Some(
                row.child(hint("Nothing runs until you approve."))
                    .child(cancel_button)
                    .child(edit_button)
                    .child(approve_button),
            );
        }
        if !phase.running() {
            return None;
        }
        let armed = self.bot_plan_armed.as_deref() == Some(work.id.as_str());
        let key = format!("{row_id}-stop");
        let work_id = work.id.clone();
        let stop = div()
            .id(SharedString::from(key.clone()))
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .text_size(px(12.5))
            .when(armed, |el| {
                el.bg(theme.danger.opacity(0.12)).text_color(theme.danger)
            })
            .when(!armed, |el| {
                el.text_color(motion::hover_blend(&key, theme.text_muted, theme.text))
                    .bg(motion::hover_blend(
                        &key,
                        crate::kit::theme::wash(0.0),
                        crate::kit::theme::ink(0.06),
                    ))
            })
            .on_hover(motion::hover_listener(key.clone()))
            .on_click(cx.listener(move |this, _, _, cx| this.arm_or_stop_plan(work_id.clone(), cx)))
            .child(div().size(px(8.0)).rounded(px(2.0)).bg(if armed {
                theme.danger
            } else {
                theme.text_muted
            }))
            .child(SharedString::from(if armed {
                "Stop the work and its sessions?"
            } else {
                "Stop"
            }));
        Some(
            row.child(hint("Stop ends the work and every session it started."))
                .child(stop),
        )
    }

    /// First click arms Stop; a second within [`REWIND_ARM_MS`] stops the
    /// work (the rewind affordance's confirm, for the same kind of damage).
    fn arm_or_stop_plan(&mut self, work_id: String, cx: &mut Context<Self>) {
        if self.bot_plan_armed.as_deref() == Some(work_id.as_str()) {
            self.bot_plan_armed = None;
            self.bot_plan_disarm = None;
            self.bot_mutate(
                serde_json::json!({ "op": "stopBotWork", "workId": work_id }),
                cx,
            );
            cx.notify();
            return;
        }
        self.bot_plan_armed = Some(work_id.clone());
        self.bot_plan_disarm = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(REWIND_ARM_MS))
                .await;
            let _ = this.update(cx, |this: &mut Transcript, cx| {
                if this.bot_plan_armed.as_deref() == Some(work_id.as_str()) {
                    this.bot_plan_armed = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
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
    use cypher_proto::{PlanDecision, WorkStep};

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
        assert_ne!(plans_key(&records, Some("chat")), 0);
        assert_eq!(plans_key(&records, Some("elsewhere")), 0);
    }
}
