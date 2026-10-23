//! Narrated progress for long turns — the working trailer's progress bar.
//!
//! A turn that has been Working for [`FIRST_AFTER`] gets a cheap model's
//! reading of how far it has got: a percentage, what the agent is doing now
//! and the steps still to come — an estimate the model revises with every
//! reading, so the upcoming steps refresh as the agent learns more. The run loop checks every few seconds
//! ([`NarrationClock`]) and asks again every [`EVERY`] while the turn runs,
//! never with one reading still being written.
//!
//! The model is the run harness's small-tier model from the run's own
//! provider (`provider/model` ids share the prefix), else the run's model
//! itself. It reads the turn as the transcript has it — the request, the
//! agent's words and its tool steps, bounded — plus its previous reading so
//! the bar moves steadily. Every failure is a silent skip with tracing: a
//! progress reading must never fail or delay a run.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};

use cypher_doc::{MessagePart, MessageRole, SessionMessageEntry};
use cypher_proto::{
    HarnessId, Model, ProgressStage, ReasoningLevel, RunProgress, RunRequest, SandboxLevel,
    ToolCall, attachment_refs,
};

use crate::registry::HarnessRegistry;

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

    /// Mark a reading in flight until the returned guard drops.
    pub fn start(&self) -> InFlight {
        self.in_flight.store(true, Ordering::SeqCst);
        InFlight(self.in_flight.clone())
    }
}

/// Holds a run's reading in flight (see [`NarrationClock::start`]).
pub(crate) struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The request for a reading of the turn that ends `entries` (joined), or
/// `None` when there is no user request to read it against.
pub(crate) fn prompt(
    entries: &[SessionMessageEntry],
    previous: Option<&RunProgress>,
    elapsed: Duration,
) -> Option<String> {
    let user_ix = entries.iter().rposition(|e| e.role == MessageRole::User)?;
    let request = request_text(&entries[user_ix]);
    if request.is_empty() {
        return None;
    }
    let activity = activity_lines(&entries[user_ix + 1..]);
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
            "<previous_reading percent=\"{}\">\ncurrent: {}\n",
            previous.percent, previous.current
        ));
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
    Some(out)
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
    for part in round.iter().flat_map(|e| &e.parts) {
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
                ..
            } => {
                let (label, detail) = cypher_proto::view::tool_chip_content(call);
                let outcome = match (resolved, is_error) {
                    (false, _) => "running",
                    (true, true) => "failed",
                    (true, false) => "done",
                };
                lines.push(format!(
                    "[{label}] {} ({outcome})",
                    clip(&flatten(&detail), MAX_DETAIL_CHARS)
                ));
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
        updated_at: now,
    })
}

/// `reading` with the turn's stages carried over from `previous`: its
/// `current` closes the list, as a new stage when the agent moved on. A
/// stage sits where the turn was when it began, never before the one ahead
/// of it.
pub(crate) fn with_stages(previous: Option<&RunProgress>, mut reading: RunProgress) -> RunProgress {
    let mut stages = previous.map(|p| p.stages.clone()).unwrap_or_default();
    if stages
        .last()
        .is_none_or(|last| last.label != reading.current)
    {
        let floor = stages.last().map_or(0, |last| last.percent);
        stages.push(ProgressStage {
            label: reading.current.clone(),
            percent: reading.percent.max(floor),
        });
    }
    let excess = stages.len().saturating_sub(MAX_STAGES);
    stages.drain(..excess);
    reading.stages = stages;
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
        let guard = clock.start();
        assert!(!clock.due(Some(start), at(125), tick(125)));
        drop(guard);
        assert!(clock.due(Some(start), at(126), tick(126)));
        // A new turn starts over.
        let next = at(200);
        assert!(!clock.due(Some(next), at(230), tick(230)));
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
            updated_at: Utc::now(),
        };
        let text = prompt(&entries, Some(&previous), Duration::from_secs(95)).unwrap();
        assert!(text.contains("<request>\nAdd a progress bar\n</request>"));
        assert!(!text.contains("old request"));
        assert!(text.contains("<elapsed>1m 35s</elapsed>"));
        assert!(text.contains("percent=\"30\">\ncurrent: Reading code\nnext: Wire it up\n"));
        assert!(text.contains("Agent: Looking at the trailer."));
        assert!(text.contains("cargo test (running)"));
        assert!(text.contains("- [x] Engine\n- [ ] UI"));
        assert!(prompt(&[], None, Duration::ZERO).is_none());
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
        let read = |percent, current: &str| RunProgress {
            percent,
            current: current.into(),
            next: vec![],
            steps_left: None,
            stages: vec![],
            updated_at: Utc::now(),
        };
        let stage = |label: &str, percent| ProgressStage {
            label: label.into(),
            percent,
        };
        let one = with_stages(None, read(20, "Store"));
        assert_eq!(one.stages, vec![stage("Store", 20)]);
        // Same stage: it keeps where it began.
        let two = with_stages(Some(&one), read(30, "Store"));
        assert_eq!(two.stages, vec![stage("Store", 20)]);
        // A reading that went back never puts a stage behind the last one.
        let three = with_stages(Some(&two), read(15, "API"));
        assert_eq!(three.stages, vec![stage("Store", 20), stage("API", 20)]);
        let mut long = three;
        for n in 0..20 {
            long = with_stages(Some(&long), read(50, &format!("step {n}")));
        }
        assert_eq!(long.stages.len(), MAX_STAGES);
        assert_eq!(long.stages.last().unwrap().label, "step 19");
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
