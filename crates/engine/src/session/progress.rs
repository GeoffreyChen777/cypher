//! Narrated progress for long turns — the working trailer's progress bar.
//!
//! A turn that has been Working for [`FIRST_AFTER`] gets a cheap model's
//! reading of how far it has got: a percentage, what the agent is doing now
//! and the steps still to come — an estimate the model revises with every
//! reading, so the upcoming steps refresh as the agent learns more. When
//! they grow past anything the plan has counted, the stages already passed
//! shrink to make room ([`with_stages`]). The run loop checks every few seconds
//! ([`NarrationClock`]) and asks again every [`EVERY`] while the turn runs,
//! never with one reading still being written, and only once the turn has
//! moved on from what the last reading saw: a turn waiting on one long, quiet
//! step keeps its reading rather than asking again. A running tool's latest
//! output counts as moving on, so one command that chains several steps
//! still moves the bar.
//!
//! The model is the run harness's small-tier model from the run's own
//! provider (`provider/model` ids share the prefix), else the run's model
//! itself. It reads the turn as the transcript has it — the request, the
//! agent's words and its tool steps, bounded — plus its previous reading so
//! the bar moves steadily. Every failure is a silent skip with tracing: a
//! progress reading must never fail or delay a run.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};

use cypher_doc::{MessagePart, MessageRole, SessionMessageEntry};
use cypher_proto::{
    HarnessId, Model, ProgressStage, ReasoningLevel, RunProgress, RunRequest, SandboxLevel,
    ToolCall, attachment_refs,
};

use crate::registry::HarnessRegistry;
use crate::util::lock;

/// How long a turn runs before its first reading.
pub(crate) const FIRST_AFTER: Duration = Duration::from_secs(60);
/// How often a reading is asked for after that.
pub(crate) const EVERY: Duration = Duration::from_secs(30);
/// How often the run loop checks whether a reading is due.
pub(crate) const CHECK_EVERY: Duration = Duration::from_secs(5);
/// One reading, spawn included. Past this the reading is skipped.
const GENERATION_TIMEOUT: Duration = Duration::from_secs(45);

/// Bounds on what the model is shown and what it may write.
const MAX_REQUEST_CHARS: usize = 2_000;
const MAX_ACTIVITY_LINES: usize = 40;
const MAX_TEXT_CHARS: usize = 300;
const MAX_DETAIL_CHARS: usize = 160;
/// A `[Run]` command, as run: the turn's latest tool step and any still
/// running keep nearly all of it (one command may chain several steps),
/// older finished ones a shorter head.
const MAX_COMMAND_CHARS: usize = 600;
const MAX_PAST_COMMAND_CHARS: usize = 200;
/// The last lines a running tool printed.
const MAX_OUTPUT_LINES: usize = 3;
const MAX_CURRENT_CHARS: usize = 80;
const MAX_NEXT_ITEMS: usize = 8;
/// The most steps left a reading may claim.
const MAX_STEPS_LEFT: u16 = 99;
const MAX_NEXT_CHARS: usize = 60;
/// Most stages a turn keeps (the oldest drop first).
const MAX_STAGES: usize = 16;

/// When a run's next reading is due. One per run loop; the turn it times is
/// the chat's `started_at`, so a persistent session's next turn starts over.
#[derive(Default)]
pub(crate) struct NarrationClock {
    turn: Option<DateTime<Utc>>,
    last_asked: Option<tokio::time::Instant>,
    in_flight: Arc<AtomicBool>,
    /// The activity (see [`Prompt::activity`]) the last reading that landed
    /// was written from.
    read: Arc<Mutex<Option<u64>>>,
}

impl NarrationClock {
    /// Whether to ask now for the running turn that started at `turn`
    /// (`None` = no turn running). A `true` answer counts as asking.
    pub fn due(
        &mut self,
        turn: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
        instant: tokio::time::Instant,
    ) -> bool {
        if self.turn != turn {
            self.turn = turn;
            self.last_asked = None;
            *lock(&self.read) = None;
        }
        let Some(turn) = turn else {
            return false;
        };
        let ran = now.signed_duration_since(turn).to_std().unwrap_or_default();
        if ran < FIRST_AFTER || self.in_flight.load(Ordering::SeqCst) {
            return false;
        }
        if self
            .last_asked
            .is_some_and(|at| instant.duration_since(at) < EVERY)
        {
            return false;
        }
        self.last_asked = Some(instant);
        true
    }

    /// Whether the last reading that landed already saw `activity`, so
    /// asking again would only repeat it.
    pub fn already_read(&self, activity: u64) -> bool {
        *lock(&self.read) == Some(activity)
    }

    /// Mark a reading of `activity` in flight until the returned guard drops.
    pub fn start(&self, activity: u64) -> InFlight {
        self.in_flight.store(true, Ordering::SeqCst);
        InFlight {
            flag: self.in_flight.clone(),
            read: self.read.clone(),
            activity,
        }
    }
}

/// Holds a run's reading in flight (see [`NarrationClock::start`]).
pub(crate) struct InFlight {
    flag: Arc<AtomicBool>,
    read: Arc<Mutex<Option<u64>>>,
    activity: u64,
}

impl InFlight {
    /// The reading landed: the turn needs no other until it moves on. A
    /// reading that failed never calls this, so the next tick tries again.
    pub fn landed(&self) {
        *lock(&self.read) = Some(self.activity);
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

/// A reading's request, with what it reads the turn from.
pub(crate) struct Prompt {
    pub text: String,
    /// A hash of the turn's request, the agent's activity (a running tool's
    /// latest output included) and its plan: not the elapsed time or the
    /// previous reading, which change on their own.
    pub activity: u64,
}

/// The request for a reading of the turn that ends `entries` (joined), or
/// `None` when there is no user request to read it against.
pub(crate) fn prompt(
    entries: &[SessionMessageEntry],
    previous: Option<&RunProgress>,
    elapsed: Duration,
) -> Option<Prompt> {
    let user_ix = entries.iter().rposition(|e| e.role == MessageRole::User)?;
    let request = request_text(&entries[user_ix]);
    if request.is_empty() {
        return None;
    }
    let activity = activity_lines(&entries[user_ix + 1..]);
    let mut hasher = DefaultHasher::new();
    (&request, &activity).hash(&mut hasher);
    let fingerprint = hasher.finish();
    let mut out = String::from(
        "You are watching an AI coding agent work on a user's request, and you \
         report its progress to the user. Reply with ONLY one JSON object, no \
         code fence, no other text:\n\
         {\"percent\": <integer 0-99>, \"current\": \"<what the agent is doing now>\", \
         \"next\": [\"<a step still to come>\", ...], \"steps_left\": <integer>}\n\n\
         Rules:\n\
         - `percent` is how much of the whole request is done. Judge it from the \
         agent's plan (todo list) when it has one, else from the work so far \
         against what the request needs. Never 100: the agent is still working.\n\
         - Keep it steady: do not go below the previous reading unless the agent \
         clearly started over or found much more work.\n\
         - While the agent is still on the previous reading's step, repeat its \
         `current` text exactly; write a new one only when it moves on.\n\
         - `current` is one short phrase (at most 8 words).\n\
         - `next` is every step still to come after `current`, in order, as short \
         phrases (at most 8 words each, at most 8 steps). Take them from the \
         agent's plan when it has one; otherwise estimate what the request still \
         needs. Revise the previous reading's steps with what the agent has \
         learned since: drop steps it no longer needs, add ones it has found. \
         Empty when nothing is left.\n\
         - `steps_left` is how many steps remain after `current`: the length of \
         `next`, or more when there are more than 8.\n\
         - Steps marked `done` in the previous reading are finished: never list \
         them in `next` again.\n\
         - One `[Run]` command may chain several steps (`&&`, `;`, one per \
         line): judge the work it does, not how many lines the activity has. A \
         running command's `output:` lines show which of its steps it is on.\n\
         - Write `current` and `next` in the same language as the user's request.\n\n",
    );
    out.push_str(&format!("<request>\n{request}\n</request>\n\n"));
    out.push_str(&format!(
        "<elapsed>{}m {}s</elapsed>\n\n",
        elapsed.as_secs() / 60,
        elapsed.as_secs() % 60
    ));
    if let Some(previous) = previous {
        out.push_str(&format!(
            "<previous_reading percent=\"{}\">\n",
            previous.percent
        ));
        // Every stage but the last (which is `current`) is finished.
        let passed = previous.stages.len().saturating_sub(1);
        for stage in &previous.stages[..passed] {
            out.push_str(&format!("done: {}\n", stage.label));
        }
        out.push_str(&format!("current: {}\n", previous.current));
        for step in &previous.next {
            out.push_str(&format!("next: {step}\n"));
        }
        out.push_str("</previous_reading>\n\n");
    }
    // The mock harness tells this request apart by the tag (its progress
    // demo answers it from the quoted plan).
    out.push_str("<agent_activity>\n");
    if activity.is_empty() {
        out.push_str("(nothing yet)\n");
    }
    for line in activity {
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str("</agent_activity>\n");
    Some(Prompt {
        text: out,
        activity: fingerprint,
    })
}

fn request_text(entry: &SessionMessageEntry) -> String {
    let raw = entry
        .parts
        .iter()
        .filter_map(|p| match p {
            MessagePart::Text {
                text, agent_text, ..
            } => Some(agent_text.as_deref().unwrap_or(text)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    clip(attachment_refs::strip(&raw).trim(), MAX_REQUEST_CHARS)
}

/// The turn's work in order, one line per step: the latest
/// [`MAX_ACTIVITY_LINES`], with the agent's newest plan in full.
fn activity_lines(round: &[SessionMessageEntry]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut plan = None;
    let parts: Vec<&MessagePart> = round.iter().flat_map(|e| &e.parts).collect();
    let latest_tool = parts.iter().rposition(
        |p| matches!(p, MessagePart::Tool { call, .. } if !matches!(call, ToolCall::Todo { .. })),
    );
    for (ix, part) in parts.into_iter().enumerate() {
        match part {
            MessagePart::Text { text, .. } => {
                let flat = flatten(text);
                if !flat.is_empty() {
                    lines.push(format!("Agent: {}", clip(&flat, MAX_TEXT_CHARS)));
                }
            }
            MessagePart::Tool {
                call: ToolCall::Todo { items },
                ..
            } => plan = Some(items.clone()),
            MessagePart::Tool {
                call,
                is_error,
                resolved,
                progress,
                ..
            } => {
                let (label, detail) = cypher_proto::view::tool_chip_content(call);
                let detail = match call {
                    ToolCall::Exec { command } => {
                        let max = if !resolved || Some(ix) == latest_tool {
                            MAX_COMMAND_CHARS
                        } else {
                            MAX_PAST_COMMAND_CHARS
                        };
                        command_text(command, max)
                    }
                    _ => clip(&flatten(&detail), MAX_DETAIL_CHARS),
                };
                let outcome = match (resolved, is_error) {
                    (false, _) => "running",
                    (true, true) => "failed",
                    (true, false) => "done",
                };
                let mut line = format!("[{label}] {detail} ({outcome})");
                if !resolved {
                    for output in output_tail(progress.as_deref()) {
                        line.push_str("\n  output: ");
                        line.push_str(&output);
                    }
                }
                lines.push(line);
            }
            _ => {}
        }
    }
    let skipped = lines.len().saturating_sub(MAX_ACTIVITY_LINES);
    let mut out: Vec<String> = Vec::new();
    if skipped > 0 {
        out.push(format!("({skipped} earlier steps omitted)"));
    }
    out.extend(lines.into_iter().skip(skipped));
    if let Some(items) = plan.filter(|items| !items.is_empty()) {
        out.push("Agent's current plan:".into());
        for item in items {
            let mark = if item.done { "x" } else { " " };
            out.push(format!(
                "- [{mark}] {}",
                clip(&flatten(&item.text), MAX_DETAIL_CHARS)
            ));
        }
    }
    out
}

/// A command as it was run, its lines kept (indented under its `[Run]`
/// line) so a script reads step by step.
fn command_text(command: &str, max: usize) -> String {
    let lines: Vec<&str> = command
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    clip(&lines.join("\n"), max).replace('\n', "\n    ")
}

/// The last [`MAX_OUTPUT_LINES`] a running tool printed, from the tail its
/// part keeps while it runs.
fn output_tail(progress: Option<&str>) -> Vec<String> {
    let lines: Vec<String> = progress
        .unwrap_or_default()
        .lines()
        // A line redrawn with `\r` (a progress meter) reads as its last draw.
        .filter_map(|line| line.rsplit('\r').find(|draw| !draw.trim().is_empty()))
        .map(flatten)
        .collect();
    let skipped = lines.len().saturating_sub(MAX_OUTPUT_LINES);
    lines
        .into_iter()
        .skip(skipped)
        .map(|line| clip(&line, MAX_DETAIL_CHARS))
        .collect()
}

fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{}…", kept.trim_end())
}

/// Read the model's answer: the first JSON object in it, leniently.
pub(crate) fn parse(raw: &str, now: DateTime<Utc>) -> Option<RunProgress> {
    #[derive(serde::Deserialize)]
    struct Answer {
        percent: Option<serde_json::Value>,
        current: Option<String>,
        #[serde(default)]
        next: Vec<serde_json::Value>,
        #[serde(default)]
        steps_left: Option<serde_json::Value>,
    }
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    let answer: Answer = serde_json::from_str(raw.get(start..=end)?).ok()?;
    let current = clip(&flatten(answer.current.as_deref()?), MAX_CURRENT_CHARS);
    if current.is_empty() {
        return None;
    }
    let percent = match answer.percent? {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::String(s) => s.trim().trim_end_matches('%').parse().ok()?,
        _ => return None,
    };
    let next: Vec<String> = answer
        .next
        .iter()
        .filter_map(|item| item.as_str())
        .map(|item| clip(&flatten(item), MAX_NEXT_CHARS))
        .filter(|item| !item.is_empty())
        .take(MAX_NEXT_ITEMS)
        .collect();
    // Only a count beyond the listed steps says anything `next` does not.
    let steps_left = answer
        .steps_left
        .and_then(|value| value.as_f64())
        .map(|n| n.round().clamp(0.0, f64::from(MAX_STEPS_LEFT)) as u16)
        .filter(|&n| usize::from(n) > next.len());
    Some(RunProgress {
        percent: percent.round().clamp(0.0, 99.0) as u8,
        current,
        next,
        steps_left,
        stages: Vec::new(),
        planned: None,
        updated_at: now,
    })
}

/// How many steps a reading counts after its current one: the listed
/// steps, or the model's count when it names more.
fn steps_ahead(progress: &RunProgress) -> usize {
    usize::from(progress.steps_left.unwrap_or(0)).max(progress.next.len())
}

/// `reading` with the turn's stages carried over from `previous`: its
/// `current` closes the list, as a new stage when the agent moved on. A
/// reading moves the fill, the current step and the steps ahead, and its
/// percentage never falls behind where the current stage began, so every
/// stage stays behind the fill.
///
/// The stages already recorded keep their places, with one exception: when
/// a reading's plan (its stages and the steps ahead) is bigger than it has
/// ever been, the stages and the fill shrink to make room, so the steps
/// still to come (the current one and those ahead) get an average step's
/// share each rather than piling up before the finish. They shrink by no
/// more than the plan grew, and a reading that went back further itself
/// keeps its own lower percentage. A plan that only regrows to an earlier
/// size moves nothing.
pub(crate) fn with_stages(previous: Option<&RunProgress>, mut reading: RunProgress) -> RunProgress {
    let mut stages = previous.map(|p| p.stages.clone()).unwrap_or_default();
    let moved = stages
        .last()
        .is_none_or(|last| last.label != reading.current);
    let planned = previous.map_or(0, |p| {
        usize::from(p.planned.unwrap_or(0)).max(p.stages.len() + steps_ahead(p))
    });
    let plan = stages.len() + usize::from(moved) + steps_ahead(&reading);
    if let Some(began) = stages.iter().map(|s| s.percent).max() {
        let mut fill = f64::from(reading.percent.max(began));
        if planned > 0 && plan > planned && fill > 0.0 {
            let to_come = 1 + steps_ahead(&reading);
            let room = 100.0 * to_come as f64 / plan as f64;
            let mut scale = ((100.0 - room) / fill).clamp(planned as f64 / plan as f64, 1.0);
            if reading.percent < began {
                scale = scale.min(f64::from(reading.percent) / f64::from(began));
            }
            for stage in &mut stages {
                stage.percent = (f64::from(stage.percent) * scale).round() as u8;
            }
            fill *= scale;
        }
        reading.percent = fill.round() as u8;
    }
    if moved {
        stages.push(ProgressStage {
            label: reading.current.clone(),
            percent: reading.percent,
        });
    }
    let excess = stages.len().saturating_sub(MAX_STAGES);
    stages.drain(..excess);
    reading.stages = stages;
    // Counted against the stages kept, so it shrinks as the oldest drop.
    let planned = planned.max(plan) - excess;
    reading.planned = Some(u16::try_from(planned).unwrap_or(u16::MAX));
    reading
}

/// The model a reading runs on: the harness's small-tier model from the
/// run's provider, else the run's model (`None` = the harness default).
pub(crate) fn pick_model(models: &[Model], run_model: Option<&str>) -> Option<String> {
    let provider = run_model.and_then(|m| m.split_once('/')).map(|(p, _)| p);
    models
        .iter()
        .filter(|m| provider.is_none_or(|p| m.id.split_once('/').is_some_and(|(q, _)| q == p)))
        .find(|m| crate::session::titles::is_callable_small_tier(m))
        .map(|m| m.id.clone())
        .or_else(|| run_model.map(str::to_owned))
}

/// Ask for one reading. `None` on any failure (logged).
pub(crate) async fn ask(
    registry: &Arc<HarnessRegistry>,
    harness_id: HarnessId,
    run_model: Option<&str>,
    cwd: &str,
    prompt: String,
) -> Option<RunProgress> {
    let harness = registry
        .resolve(harness_id)
        .inspect_err(|err| tracing::debug!(error = %err, "progress harness unavailable"))
        .ok()?;
    let models = harness.models().await.unwrap_or_default();
    let model = pick_model(&models, run_model);
    let request = RunRequest {
        prompt,
        harness: Some(harness_id),
        model: model.clone(),
        reasoning: Some(ReasoningLevel::Minimal),
        model_options: serde_json::Map::new(),
        cwd: cwd.to_owned(),
        sandbox: SandboxLevel::ReadOnly,
        auto_approve: true,
        attachments: Vec::new(),
        pending_attachments: Vec::new(),
        resume: None,
        worktree: None,
    };
    let raw = match tokio::time::timeout(
        GENERATION_TIMEOUT,
        crate::session::titles::collect_text(harness.as_ref(), request),
    )
    .await
    {
        Ok(Ok(raw)) => raw,
        Ok(Err(err)) => {
            tracing::debug!(model = model.as_deref().unwrap_or("default"), error = %err, "progress reading failed");
            return None;
        }
        Err(_) => {
            tracing::debug!(
                model = model.as_deref().unwrap_or("default"),
                "progress reading timed out"
            );
            return None;
        }
    };
    let progress = parse(&raw, Utc::now());
    if progress.is_none() {
        tracing::debug!(
            model = model.as_deref().unwrap_or("default"),
            "progress reading unreadable"
        );
    }
    progress
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::TodoItem;

    fn model(id: &str) -> Model {
        Model {
            id: id.into(),
            label: id.into(),
            description: None,
            reasoning_levels: vec![],
            options: vec![],
        }
    }

    fn entry(role: MessageRole, parts: Vec<MessagePart>) -> SessionMessageEntry {
        SessionMessageEntry {
            id: format!("{role:?}"),
            role,
            parts,
            created_at: 0,
            device_id: "dev".into(),
            status: None,
            continuation_of: None,
            completed_at: None,
            comments: Vec::new(),
            models: Vec::new(),
            turn_changes: None,
            recovery: None,
        }
    }

    fn text(text: &str) -> MessagePart {
        MessagePart::Text {
            id: "t".into(),
            text: text.into(),
            agent_text: None,
        }
    }

    fn tool(id: &str, call: ToolCall, resolved: bool) -> MessagePart {
        MessagePart::Tool {
            id: id.into(),
            call,
            is_error: false,
            resolved,
            output: None,
            progress: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            permission: None,
        }
    }

    #[test]
    fn the_clock_waits_a_minute_then_asks_every_half_minute() {
        let start = Utc::now();
        let at = |secs: i64| start + chrono::Duration::seconds(secs);
        let base = tokio::time::Instant::now();
        let tick = |secs: u64| base + Duration::from_secs(secs);
        let mut clock = NarrationClock::default();
        assert!(!clock.due(Some(start), at(59), tick(59)));
        assert!(clock.due(Some(start), at(60), tick(60)));
        assert!(!clock.due(Some(start), at(85), tick(85)));
        assert!(clock.due(Some(start), at(90), tick(90)));
        // One reading at a time.
        let guard = clock.start(7);
        assert!(!clock.due(Some(start), at(125), tick(125)));
        drop(guard);
        assert!(clock.due(Some(start), at(126), tick(126)));
        // A failed reading leaves its activity unread; a landed one doesn't.
        assert!(!clock.already_read(7));
        clock.start(7).landed();
        assert!(clock.already_read(7));
        assert!(!clock.already_read(8));
        // A new turn starts over.
        let next = at(200);
        assert!(!clock.due(Some(next), at(230), tick(230)));
        assert!(!clock.already_read(7));
        assert!(clock.due(Some(next), at(260), tick(260)));
        assert!(!clock.due(None, at(400), tick(400)));
    }

    #[test]
    fn the_prompt_reads_the_last_turn_with_its_plan() {
        let entries = vec![
            entry(MessageRole::User, vec![text("old request")]),
            entry(MessageRole::Assistant, vec![text("old answer")]),
            entry(MessageRole::User, vec![text("Add a progress bar")]),
            entry(
                MessageRole::Assistant,
                vec![
                    text("Looking at the trailer."),
                    tool(
                        "1",
                        ToolCall::Exec {
                            command: "cargo test".into(),
                        },
                        false,
                    ),
                    tool(
                        "2",
                        ToolCall::Todo {
                            items: vec![
                                TodoItem {
                                    text: "Engine".into(),
                                    done: true,
                                },
                                TodoItem {
                                    text: "UI".into(),
                                    done: false,
                                },
                            ],
                        },
                        true,
                    ),
                ],
            ),
        ];
        let previous = RunProgress {
            percent: 30,
            current: "Reading code".into(),
            next: vec!["Wire it up".into()],
            steps_left: None,
            stages: vec![],
            planned: None,
            updated_at: Utc::now(),
        };
        let read = prompt(&entries, Some(&previous), Duration::from_secs(95)).unwrap();
        let body = &read.text;
        assert!(body.contains("<request>\nAdd a progress bar\n</request>"));
        assert!(!body.contains("old request"));
        assert!(body.contains("<elapsed>1m 35s</elapsed>"));
        assert!(body.contains("percent=\"30\">\ncurrent: Reading code\nnext: Wire it up\n"));
        assert!(body.contains("Agent: Looking at the trailer."));
        assert!(body.contains("cargo test (running)"));
        assert!(body.contains("- [x] Engine\n- [ ] UI"));
        assert!(prompt(&[], None, Duration::ZERO).is_none());
        // Time passing and a new previous reading leave the activity as it
        // was; a step finishing changes it.
        let later = prompt(&entries, None, Duration::from_secs(400)).unwrap();
        assert_eq!(later.activity, read.activity);
        let mut moved = entries.clone();
        moved[3].parts.push(text("Tests pass."));
        let moved = prompt(&moved, Some(&previous), Duration::from_secs(95)).unwrap();
        assert_ne!(moved.activity, read.activity);
        // The passed stages are shown as done, the last one is `current`.
        let staged = RunProgress {
            stages: vec![
                ProgressStage {
                    label: "Plan".into(),
                    percent: 5,
                },
                ProgressStage {
                    label: "Reading code".into(),
                    percent: 20,
                },
            ],
            ..previous
        };
        let read = prompt(&entries, Some(&staged), Duration::from_secs(95)).unwrap();
        assert!(
            read.text
                .contains("percent=\"30\">\ndone: Plan\ncurrent: Reading code\n")
        );
    }

    #[test]
    fn commands_read_whole_and_running_ones_show_their_output() {
        let exec = |command: String| ToolCall::Exec { command };
        let long_tail = format!("cargo test -p cypher-engine {}", "x".repeat(300));
        let chained = format!("cd /work/tree && cargo build\ncargo fmt  \n\n{long_tail}");
        let mut running = tool("2", exec(chained), false);
        if let MessagePart::Tool { progress, .. } = &mut running {
            *progress = Some("one\ntwo\nthree\nBuilding 10%\rBuilding 80%\r\nfour\n".into());
        }
        let entries = |parts| {
            vec![
                entry(MessageRole::User, vec![text("Ship it")]),
                entry(MessageRole::Assistant, parts),
            ]
        };
        let old = tool("1", exec(format!("ls && {}", "y".repeat(400))), true);
        let read = prompt(
            &entries(vec![old.clone(), running.clone()]),
            None,
            Duration::ZERO,
        )
        .unwrap();
        let body = &read.text;
        // The running command keeps its lines and well past the chip's 160
        // characters; an older finished one keeps a shorter head.
        assert!(body.contains(
            "[Run] cd /work/tree && cargo build\n    cargo fmt\n    cargo test -p cypher-engine x"
        ));
        assert!(body.contains(&format!("[Run] ls && {}… (done)", "y".repeat(194))));
        // Its last output lines ride under it, a redrawn meter as its last draw.
        assert!(body.contains(
            "(running)\n  output: three\n  output: Building 80%\n  output: four\n</agent_activity>"
        ));
        assert!(!body.contains("output: two"));
        // New output is new activity: the next tick asks again.
        let mut moved = running.clone();
        if let MessagePart::Tool { progress, .. } = &mut moved {
            *progress = Some("five".into());
        }
        let later = prompt(&entries(vec![old.clone(), moved]), None, Duration::ZERO).unwrap();
        assert_ne!(later.activity, read.activity);
        // The latest step keeps its command whole once finished too.
        let done = tool("1", exec(format!("ls && {}", "y".repeat(700))), true);
        let read = prompt(&entries(vec![done]), None, Duration::ZERO).unwrap();
        assert!(
            read.text
                .contains(&format!("[Run] ls && {}… (done)", "y".repeat(594)))
        );
    }

    #[test]
    fn answers_are_read_leniently() {
        let now = Utc::now();
        let p = parse(
            "Sure:\n```json\n{\"percent\": 42.6, \"current\": \"  Wiring   the UI \", \"next\": [\"Test\", \"\", 3, \"Ship\", \"More\", \"Extra\"]}\n```",
            now,
        )
        .unwrap();
        assert_eq!(p.percent, 43);
        assert_eq!(p.current, "Wiring the UI");
        assert_eq!(p.next, vec!["Test", "Ship", "More", "Extra"]);
        assert_eq!(p.steps_left, None);
        // A count past the listed steps is kept; one that is not, dropped.
        let counted = |left: &str| {
            parse(
                &format!("{{\"percent\": 5, \"current\": \"A\", \"next\": [\"B\", \"C\"], \"steps_left\": {left}}}"),
                now,
            )
            .unwrap()
            .steps_left
        };
        assert_eq!(counted("11"), Some(11));
        assert_eq!(counted("2"), None);
        assert_eq!(counted("\"many\""), None);
        assert_eq!(
            parse("{\"percent\": \"100%\", \"current\": \"Done\"}", now)
                .unwrap()
                .percent,
            99
        );
        assert!(parse("{\"percent\": 10, \"current\": \"\"}", now).is_none());
        assert!(parse("no json", now).is_none());
    }

    #[test]
    fn stages_carry_over_and_grow_when_the_agent_moves_on() {
        // `ahead` steps after `current`: these readings keep a four-step plan.
        let read = |percent, current: &str, ahead| RunProgress {
            percent,
            current: current.into(),
            next: vec![],
            steps_left: Some(ahead),
            stages: vec![],
            planned: None,
            updated_at: Utc::now(),
        };
        let stage = |label: &str, percent| ProgressStage {
            label: label.into(),
            percent,
        };
        let one = with_stages(None, read(20, "Store", 3));
        assert_eq!(one.stages, vec![stage("Store", 20)]);
        assert_eq!(one.planned, Some(4));
        // Same stage: it keeps where it began.
        let two = with_stages(Some(&one), read(30, "Store", 3));
        assert_eq!(two.stages, vec![stage("Store", 20)]);
        // The current stage moves on: the fill may move within it, but a
        // reading that went back stops where it began.
        let three = with_stages(Some(&two), read(40, "API", 2));
        assert_eq!(three.stages, vec![stage("Store", 20), stage("API", 40)]);
        let four = with_stages(Some(&three), read(15, "API", 2));
        assert_eq!((four.percent, &four.stages), (40, &three.stages));
        // A new stage after a reading that went back: the passed ones stay
        // put and the new one begins where the last left off.
        let five = with_stages(Some(&four), read(30, "Tests", 1));
        assert_eq!(
            five.stages,
            vec![stage("Store", 20), stage("API", 40), stage("Tests", 40)]
        );
        assert_eq!((five.percent, five.planned), (40, Some(4)));
        // Stages already ahead of the reading (older data) never move.
        let mut ahead = read(22, "Wait", 0);
        ahead.stages = vec![stage("A", 35), stage("B", 60), stage("Wait", 62)];
        let kept = with_stages(Some(&ahead), read(23, "Wait", 0));
        assert_eq!((kept.percent, &kept.stages), (62, &ahead.stages));
        // The oldest stages drop first, and the plan's count with them.
        let mut long = three;
        for n in 0..20 {
            long = with_stages(Some(&long), read(50, &format!("step {n}"), 30 - n));
        }
        assert_eq!(long.stages.len(), MAX_STAGES);
        assert_eq!(long.stages.last().unwrap().label, "step 19");
        assert_eq!(long.planned, Some(MAX_STAGES as u16 + 11));
    }

    #[test]
    fn a_plan_that_grows_makes_room_for_its_new_steps() {
        let read = |percent, current: &str, ahead| RunProgress {
            percent,
            current: current.into(),
            next: vec![],
            steps_left: Some(ahead),
            stages: vec![],
            planned: None,
            updated_at: Utc::now(),
        };
        let stage = |label: &str, percent| ProgressStage {
            label: label.into(),
            percent,
        };
        let at = |percent, current: &str, ahead, stages| RunProgress {
            stages,
            ..read(percent, current, ahead)
        };
        // Five steps planned, the third under way at 60%, then five more turn
        // up: rather than eight steps in the last 40%, everything done so far
        // shrinks by the plan's growth (5 to 10), every stage kept.
        let before = at(
            60,
            "C",
            2,
            vec![stage("A", 10), stage("B", 40), stage("C", 60)],
        );
        let grown = with_stages(Some(&before), read(62, "C", 7));
        assert_eq!(
            grown.stages,
            vec![stage("A", 5), stage("B", 20), stage("C", 30)]
        );
        assert_eq!((grown.percent, grown.planned), (31, Some(10)));
        // A plan that shrinks and regrows to an earlier size moves nothing.
        let shrunk = with_stages(Some(&grown), read(35, "C", 5));
        let regrown = with_stages(Some(&shrunk), read(36, "C", 7));
        assert_eq!(
            (regrown.percent, &regrown.stages, regrown.planned),
            (36, &grown.stages, Some(10))
        );
        // Moving on into a step nobody planned grows the plan too; a reading
        // that went back further itself keeps its own percentage.
        let further = with_stages(Some(&regrown), read(18, "D", 9));
        assert_eq!(
            further.stages,
            vec![
                stage("A", 3),
                stage("B", 12),
                stage("C", 18),
                stage("D", 18)
            ]
        );
        assert_eq!((further.percent, further.planned), (18, Some(13)));
        // Where the steps still to come have most of their room already, the
        // stages shrink only as far as gives each an average step's share
        // (here 1/9) — and not at all where they have it.
        let early = at(12, "B", 6, vec![stage("A", 5), stage("B", 10)]);
        let more = with_stages(Some(&early), read(12, "B", 7));
        assert_eq!(more.stages, vec![stage("A", 5), stage("B", 9)]);
        assert_eq!(more.percent, 11);
        let low = at(8, "B", 6, vec![stage("A", 5), stage("B", 8)]);
        let roomy = with_stages(Some(&low), read(8, "B", 7));
        assert_eq!((roomy.percent, &roomy.stages), (8, &low.stages));
        assert_eq!(roomy.planned, Some(9));
    }

    #[test]
    fn the_model_is_a_small_one_from_the_runs_provider() {
        let models = vec![
            model("mvp-lab/claude-opus"),
            model("other/gpt-mini"),
            model("mvp-lab/gpt-5.4-mini-openai-compact"),
            model("mvp-lab/deepseek-flash"),
        ];
        assert_eq!(
            pick_model(&models, Some("mvp-lab/claude-opus")).as_deref(),
            Some("mvp-lab/deepseek-flash")
        );
        // No small model in the provider: the run's own model.
        assert_eq!(
            pick_model(&models, Some("solo/big")).as_deref(),
            Some("solo/big")
        );
        // Providerless ids (one-provider harnesses): any small model.
        let plain = vec![model("opus"), model("haiku")];
        assert_eq!(pick_model(&plain, Some("opus")).as_deref(), Some("haiku"));
        assert_eq!(pick_model(&plain, None).as_deref(), Some("haiku"));
        assert_eq!(pick_model(&[], None), None);
    }
}
