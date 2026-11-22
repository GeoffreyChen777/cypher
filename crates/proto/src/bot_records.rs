//! The records behind the Botboard (docs/design/agent-bots.md §6–§13.1):
//! a bot's plans and orchestrations, notebook, routines, its projects'
//! routines, its activity log, approvals, and its network.
//!
//! Each record is a registry row of its own kind, synced to every owner
//! device like the `bots` rows. Rows are LWW **per field**, so every field
//! has one writer: the bot's host engine writes the state it owns, and any
//! owner device writes the small control fields a person sets (a plan's
//! `decision`, a work item's `stopRequestedAt`, a routine's definition).
//! The host watches the rows and acts on control fields, so approving a
//! plan or stopping work from any device is one row edit, queued offline
//! like every other.
//!
//! The registry refuses any op over 16 KiB and a refused batch wedges sync,
//! so every text field here has a cap ([`clip`]) and writers clamp before
//! writing; `cypher_doc` refuses an oversized row outright as a backstop.
//! Times are epoch milliseconds.

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::BotScope;
use crate::plan_body::REV_MAX;

/// Registry row kinds.
pub const KIND_BOT_WORK: &str = "botWork";
pub const KIND_BOT_NOTES: &str = "botNotes";
pub const KIND_BOT_ROUTINES: &str = "botRoutines";
pub const KIND_BOT_ACTIVITY: &str = "botActivity";
pub const KIND_BOT_APPROVALS: &str = "botApprovals";
pub const KIND_BOT_CONNECTIONS: &str = "botConnections";
pub const KIND_BOT_INVITES: &str = "botInvites";
pub const KIND_BOT_EXCHANGES: &str = "botExchanges";
pub const KIND_BOT_SHORTCUTS: &str = "botShortcuts";

/// Every record kind, in [`BotRecords`] order.
pub const BOT_RECORD_KINDS: [&str; 10] = [
    KIND_BOT_WORK,
    KIND_BOT_NOTES,
    KIND_BOT_ROUTINES,
    KIND_BOT_ACTIVITY,
    KIND_BOT_APPROVALS,
    KIND_BOT_CONNECTIONS,
    KIND_BOT_INVITES,
    KIND_BOT_EXCHANGES,
    crate::KIND_MEMORY_STORES,
    KIND_BOT_SHORTCUTS,
];

// ── caps ────────────────────────────────────────────────────────────────────

pub const TITLE_MAX: usize = 120;
pub const LINE_MAX: usize = 300;
pub const STEP_TITLE_MAX: usize = 160;
pub const DETAIL_MAX: usize = 400;
/// Every step's detail together (what the orchestration needs to start
/// each step); one step may use all of it. With the other caps this keeps a
/// work row under the registry's per-row size ([`WORK_ROW_MAX`]).
pub const PLAN_DETAILS_MAX: usize = 4_000;
/// A step's progress note.
pub const STEP_NOTE_MAX: usize = 200;
/// The largest work row written, in UTF-16 units of its JSON (the registry
/// refuses a row over 14 Ki).
pub const WORK_ROW_MAX: usize = 13_800;
pub const INSTRUCTION_MAX: usize = 1_500;
pub const RESULT_MAX: usize = 2_000;
pub const NOTE_MAX: usize = 1_000;
pub const PROMPT_MAX: usize = 4_000;
pub const ANSWER_MAX: usize = 8_000;
pub const PLAN_STEPS_MAX: usize = 10;
pub const STEP_SESSIONS_MAX: usize = 8;

/// Limits the host enforces per bot (§6.6), when its [`PlanSettings`]
/// leave them out.
pub const MAX_RUNNING_WORK: usize = 3;
pub const MAX_SESSIONS_PER_WORK: usize = 8;
/// How long an orchestration may run before the host stops it.
pub const WORK_TIME_BUDGET_MS: i64 = 4 * 60 * 60 * 1000;
/// What an owner may set them to.
pub const PLAN_TIME_LIMIT_MIN_MINUTES: u32 = 15;
pub const PLAN_TIME_LIMIT_MAX_MINUTES: u32 = 72 * 60;
pub const RUNNING_WORK_CAP: u32 = 10;
pub const SESSIONS_PER_WORK_CAP: u32 = 24;
/// Time an owner adds to a running plan, in all.
pub const PLAN_TIME_ADDED_MAX_MINUTES: u32 = 72 * 60;
/// The coordinating session is told this long before its time is up.
pub const PLAN_WARN_BEFORE_MS: i64 = 15 * 60 * 1000;
/// A model id, as a session choice names it.
pub const MODEL_ID_MAX: usize = 200;
/// Records the host keeps per bot before pruning the oldest.
pub const KEEP_ACTIVITY: usize = 200;
pub const KEEP_FINISHED_WORK: usize = 40;
pub const KEEP_EXCHANGES: usize = 200;
/// Notes the prompt carries (newest first); the tool reads the rest.
pub const NOTES_IN_PROMPT: usize = 40;

/// `text` trimmed to at most `max` characters, an ellipsis marking a cut.
pub fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// What `c` takes in serde_json's output, in UTF-16 units.
fn json_units(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if c < ' ' => 6,
        c => c.len_utf16(),
    }
}

fn clip_opt(text: &Option<String>, max: usize) -> Option<String> {
    text.as_deref()
        .map(|t| clip(t, max))
        .filter(|t| !t.is_empty())
}

// ── capabilities ────────────────────────────────────────────────────────────

/// What a bot's owner threads may do beyond reading (§4.2). Independent
/// switches; rows written before they existed read as all off (read-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BotCapabilities {
    /// Pi's shell and file tools in the bot's local projects, under
    /// permission control with those checkouts as workspace roots.
    pub work_directly: bool,
    /// Plans, worktrees and sessions in its projects, on any owner device.
    pub coordinate: bool,
    /// Steer, answer or stop sessions it did not start (each confirmed by
    /// the owner).
    pub steer: bool,
}

impl BotCapabilities {
    /// What a new bot starts with: it works in its local projects and
    /// coordinates (every plan still waits for the owner's approval); it
    /// steers others' sessions only once the owner turns that on.
    pub fn new_bot() -> Self {
        Self {
            work_directly: true,
            coordinate: true,
            steer: false,
        }
    }
}

// ── session choices and plan settings ───────────────────────────────────────

/// The model, thinking level and GPT Fast mode of a session a workspace
/// starts. Each field left out follows what the session falls back to (a
/// step's choice, then the workspace's, then its chat's).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionChoice {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<crate::ReasoningLevel>,
    /// GPT Fast mode (priority processing on GPT models from 5.4 on).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
}

/// The `modelOptions` key a run's GPT Fast mode travels in.
pub const FAST_MODEL_OPTION: &str = "fast";

impl SessionChoice {
    pub fn is_empty(&self) -> bool {
        self.model.is_none() && self.thinking.is_none() && self.fast.is_none()
    }

    /// `self`, with what it leaves out taken from `fallback`.
    pub fn or(&self, fallback: &SessionChoice) -> SessionChoice {
        SessionChoice {
            model: self.model.clone().or_else(|| fallback.model.clone()),
            thinking: self.thinking.or(fallback.thinking),
            fast: self.fast.or(fallback.fast),
        }
    }

    /// Blank model ids read as none; ids are capped.
    pub fn clamp(&mut self) {
        self.model = clip_opt(&self.model, MODEL_ID_MAX);
    }

    /// What a chat config chooses, as a choice.
    pub fn of_config(config: &crate::ChatConfig) -> SessionChoice {
        SessionChoice {
            model: config.model.clone(),
            thinking: config.reasoning,
            fast: config
                .model_options
                .get(FAST_MODEL_OPTION)
                .and_then(serde_json::Value::as_bool),
        }
    }

    /// Apply the choice to a session's config: a model it names replaces
    /// the config's (and the old model's options, Fast mode aside), a
    /// thinking level replaces its level, Fast mode its switch.
    pub fn apply(&self, config: &mut crate::ChatConfig) {
        if let Some(model) = self.model.as_deref().filter(|m| !m.trim().is_empty()) {
            if config.model.as_deref() != Some(model) {
                config
                    .model_options
                    .retain(|key, _| key == FAST_MODEL_OPTION);
            }
            config.model = Some(model.trim().to_string());
        }
        if let Some(thinking) = self.thinking {
            config.reasoning = Some(thinking);
        }
        if let Some(fast) = self.fast {
            config
                .model_options
                .insert(FAST_MODEL_OPTION.into(), fast.into());
        }
    }

    /// [`Self::apply`] to a run's request.
    pub fn apply_to_run(&self, request: &mut crate::RunRequest) {
        let mut config = crate::ChatConfig {
            harness: request.harness.unwrap_or(crate::HarnessId::Pi),
            model: request.model.take(),
            reasoning: request.reasoning,
            model_options: std::mem::take(&mut request.model_options),
            sandbox: request.sandbox,
        };
        self.apply(&mut config);
        request.model = config.model;
        request.reasoning = config.reasoning;
        request.model_options = config.model_options;
    }

    /// "claude-opus-5-5, high thinking, Fast mode", for a model to read;
    /// empty when it chooses nothing.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(model) = &self.model {
            parts.push(model.clone());
        }
        if let Some(thinking) = self.thinking {
            parts.push(format!("{} thinking", thinking.word()));
        }
        match self.fast {
            Some(true) => parts.push("Fast mode".into()),
            Some(false) => parts.push("no Fast mode".into()),
            None => {}
        }
        parts.join(", ")
    }
}

/// How a workspace's plans run (§6.6). Each limit left out is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlanSettings {
    /// Minutes a plan may run before the host stops it (default 240).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_limit_minutes: Option<u32>,
    /// Plans running at once; approved ones wait for a slot (default 3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_max: Option<u32>,
    /// Sessions one plan may start (default 8).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions_max: Option<u32>,
}

impl PlanSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// A plan's time limit, in minutes, within what an owner may set.
    pub fn time_limit_minutes(&self) -> u32 {
        clamp_time_limit(
            self.time_limit_minutes
                .unwrap_or((WORK_TIME_BUDGET_MS / 60_000) as u32),
        )
    }

    pub fn running_max(&self) -> usize {
        self.running_max
            .map_or(MAX_RUNNING_WORK, |n| n.clamp(1, RUNNING_WORK_CAP) as usize)
    }

    pub fn sessions_max(&self) -> usize {
        self.sessions_max.map_or(MAX_SESSIONS_PER_WORK, |n| {
            n.clamp(1, SESSIONS_PER_WORK_CAP) as usize
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(minutes) = self.time_limit_minutes
            && !(PLAN_TIME_LIMIT_MIN_MINUTES..=PLAN_TIME_LIMIT_MAX_MINUTES).contains(&minutes)
        {
            return Err(format!(
                "A plan's time limit is {} minutes to {} hours.",
                PLAN_TIME_LIMIT_MIN_MINUTES,
                PLAN_TIME_LIMIT_MAX_MINUTES / 60
            ));
        }
        if self
            .running_max
            .is_some_and(|n| !(1..=RUNNING_WORK_CAP).contains(&n))
        {
            return Err(format!("Plans at once is 1 to {RUNNING_WORK_CAP}."));
        }
        if self
            .sessions_max
            .is_some_and(|n| !(1..=SESSIONS_PER_WORK_CAP).contains(&n))
        {
            return Err(format!(
                "Sessions per plan is 1 to {SESSIONS_PER_WORK_CAP}."
            ));
        }
        Ok(())
    }
}

/// `minutes` within what a plan's time limit may be.
pub fn clamp_time_limit(minutes: u32) -> u32 {
    minutes.clamp(PLAN_TIME_LIMIT_MIN_MINUTES, PLAN_TIME_LIMIT_MAX_MINUTES)
}

/// "4 hours", "90 minutes", "1 hour 30 minutes".
pub fn describe_minutes(minutes: u32) -> String {
    let plural = |n: u32, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    match (minutes / 60, minutes % 60) {
        (0, m) => plural(m, "minute"),
        (h, 0) => plural(h, "hour"),
        (h, m) => format!("{} {}", plural(h, "hour"), plural(m, "minute")),
    }
}

// ── threads ─────────────────────────────────────────────────────────────────

/// Which of a bot's threads a chat is. The bot chat is `Main`; the rest are
/// hidden chats the host creates.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BotThread {
    #[default]
    Main,
    /// Carries out one approved plan.
    #[serde(rename_all = "camelCase")]
    Work { work_id: String },
    /// Where one routine runs.
    #[serde(rename_all = "camelCase")]
    Routine { routine_id: String },
    /// A project routine's session: an ordinary coding session in the
    /// project that the routine owns. Hidden like the bot's threads, but it
    /// runs as a plain session, not as the bot.
    #[serde(rename_all = "camelCase")]
    RoutineSession { routine_id: String },
    /// Where one connection's questions run.
    #[serde(rename_all = "camelCase")]
    Peer { connection_id: String },
    /// A thread kind from a newer engine: shown nowhere, does nothing.
    #[serde(other)]
    Unknown,
}

// ── plans and orchestrations (§6) ───────────────────────────────────────────

/// One plan, from proposal through its orchestration to the result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotWork {
    pub id: String,
    pub bot_id: String,
    pub title: String,
    /// The owner's instruction, verbatim: the orchestration's brief.
    #[serde(default)]
    pub instruction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub steps: Vec<WorkStep>,
    #[serde(default)]
    pub state: WorkState,
    /// The chat the plan was proposed in (its card lives there).
    pub chat_id: String,
    /// The `propose_plan` tool call that draws the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// The orchestration thread, once approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_chat_id: Option<String>,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Owner-written: approve or cancel the proposal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<PlanDecision>,
    /// Owner-written: stop the orchestration and its sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_requested_at: Option<i64>,
    /// Host-written when the orchestration ends: its workspace's chat is
    /// owed a result notice (the card, and the agent's short turn). A row
    /// that ended under an engine without notices lacks it and gets none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notify: bool,
    /// Minutes it may run, set when it was proposed (the plan's own, else
    /// the workspace's). Rows from before it run on the workspace's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_limit_minutes: Option<u32>,
    /// Owner-written: minutes added to the limit while it runs, in all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_added_minutes: Option<u32>,
    /// Host-written: when the coordinating session was told its time is
    /// nearly up, against the limit as it was then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warned_at: Option<i64>,
    /// The coordinating session's model, thinking and Fast mode; what it
    /// leaves out follows the workspace's chat.
    #[serde(default, skip_serializing_if = "SessionChoice::is_empty")]
    pub coordinator_choice: SessionChoice,
    /// Host-written, set when the plan's text was cut to fit the row: the
    /// rev of the full text in the workspace chat doc's
    /// `planBodies[id].plan` ([`PlanBody`](crate::PlanBody)). Absent = the
    /// row is the whole plan: `instruction` and the steps' details are not
    /// previews.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_rev: Option<String>,
    /// The same for `result` (`planBodies[id].result`,
    /// [`PlanResult`](crate::PlanResult)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_rev: Option<String>,
}

impl BotWork {
    /// The fields only an owner device writes.
    pub const OWNER_FIELDS: &'static [&'static str] =
        &["decision", "stopRequestedAt", "timeAddedMinutes"];

    /// Its whole time limit in minutes: its own (else `fallback`, the
    /// workspace's) plus what the owner added.
    pub fn limit_minutes(&self, fallback: u32) -> u32 {
        let own = self.time_limit_minutes.map_or(fallback, clamp_time_limit);
        own + self
            .time_added_minutes
            .unwrap_or(0)
            .min(PLAN_TIME_ADDED_MAX_MINUTES)
    }

    /// When it is stopped for time, once it has started.
    pub fn deadline_ms(&self, fallback: u32) -> Option<i64> {
        self.started_at
            .map(|started| started + i64::from(self.limit_minutes(fallback)) * 60_000)
    }

    /// Cap every text to fit the row. Over a cap, `instruction`, the steps'
    /// details and `result` become previews (an ellipsis marks each cut);
    /// [`Self::cut_plan`] and [`Self::cut_result`] keep the full text as a
    /// body when that happens.
    pub fn clamp(&mut self) {
        self.coordinator_choice.clamp();
        self.body_rev = clip_opt(&self.body_rev, REV_MAX);
        self.result_rev = clip_opt(&self.result_rev, REV_MAX);
        self.title = clip(&self.title, TITLE_MAX);
        self.instruction = clip(&self.instruction, INSTRUCTION_MAX);
        self.summary = clip_opt(&self.summary, DETAIL_MAX);
        self.result = clip_opt(&self.result, RESULT_MAX);
        self.steps.truncate(PLAN_STEPS_MAX);
        for step in &mut self.steps {
            step.clamp();
        }
        // The details share one budget: the longest gives way first.
        while self.details_len() > PLAN_DETAILS_MAX {
            let over = self.details_len() - PLAN_DETAILS_MAX;
            let Some(longest) = self
                .steps
                .iter_mut()
                .filter_map(|step| step.detail.as_mut())
                .max_by_key(|detail| detail.chars().count())
            else {
                break;
            };
            let keep = longest.chars().count().saturating_sub(over);
            *longest = clip(longest, keep.max(1));
        }
        self.fit_row();
    }

    /// The steps' details together, in characters.
    pub fn details_len(&self) -> usize {
        self.steps
            .iter()
            .filter_map(|step| step.detail.as_deref())
            .map(|detail| detail.chars().count())
            .sum()
    }

    /// The row's size as the registry measures it.
    fn row_units(&self) -> usize {
        serde_json::to_string(self).map_or(0, |json| json.encode_utf16().count())
    }

    /// A last resort for text the caps let through but whose JSON runs long
    /// (escaped newlines, quotes and control characters): shorten the
    /// result, then the longest detail, until the row is one the registry
    /// takes. Each cut takes off what the row is over as JSON, not as
    /// characters, so a detail of six-unit escapes loses only what it must.
    fn fit_row(&mut self) {
        for _ in 0..16 {
            let units = self.row_units();
            if units <= WORK_ROW_MAX {
                return;
            }
            let over = units - WORK_ROW_MAX + 16;
            let shorten = |text: &mut String| {
                let chars: Vec<char> = text.chars().collect();
                // The ellipsis that marks the cut costs one unit back.
                let (mut keep, mut freed) = (chars.len(), 0);
                while keep > 1 && freed < over + 1 {
                    keep -= 1;
                    freed += json_units(chars[keep]);
                }
                *text = chars[..keep].iter().chain(['…'].iter()).collect();
            };
            match self.result.as_mut().filter(|r| r.chars().count() > 200) {
                Some(result) => shorten(result),
                None => match self
                    .steps
                    .iter_mut()
                    .filter_map(|step| step.detail.as_mut())
                    .max_by_key(|detail| detail.chars().count())
                {
                    Some(detail) if detail.chars().count() > 1 => shorten(detail),
                    _ => return,
                },
            }
        }
    }

    /// How an open step reads once its plan has ended as `self.state`: what
    /// was under way ended with the plan (done, or failed), what never
    /// started was skipped, and a stopped plan skipped the rest. The steps
    /// of a live plan, of a cancelled one (it never ran: they stay as
    /// proposed) and steps already closed read as they are.
    pub fn ended_step_state(&self, step: StepState) -> StepState {
        let open = matches!(
            step,
            StepState::Pending | StepState::Running | StepState::Asking
        );
        if !open {
            return step;
        }
        match (self.state, step) {
            (WorkState::Proposed | WorkState::Running | WorkState::Cancelled, _) => step,
            (WorkState::Done, StepState::Running | StepState::Asking) => StepState::Done,
            (WorkState::Failed, StepState::Running | StepState::Asking) => StepState::Failed,
            _ => StepState::Skipped,
        }
    }

    /// Close the steps an ended plan left open ([`Self::ended_step_state`]),
    /// ending them when the plan did. `true` when a step changed.
    pub fn close_steps(&mut self, now_ms: i64) -> bool {
        let ended = self.ended_at.unwrap_or(now_ms);
        let states: Vec<StepState> = self
            .steps
            .iter()
            .map(|step| self.ended_step_state(step.state))
            .collect();
        let mut changed = false;
        for (step, state) in self.steps.iter_mut().zip(states) {
            if step.state != state {
                step.state = state;
                step.ended_at.get_or_insert(ended);
                changed = true;
            }
        }
        changed
    }

    pub fn is_live(&self) -> bool {
        matches!(self.state, WorkState::Proposed | WorkState::Running)
    }

    /// Awaiting the owner's approval: proposed, and nobody decided yet.
    pub fn awaiting_approval(&self) -> bool {
        self.state == WorkState::Proposed && self.decision.is_none()
    }

    /// Every session the orchestration started, in step order.
    pub fn session_ids(&self) -> impl Iterator<Item = &str> {
        self.steps
            .iter()
            .flat_map(|s| s.sessions.iter().map(String::as_str))
    }

    /// Steps done out of all, for the Work card's bar.
    pub fn progress(&self) -> (usize, usize) {
        let done = self
            .steps
            .iter()
            .filter(|s| matches!(s.state, StepState::Done | StepState::Skipped))
            .count();
        (done, self.steps.len())
    }

    /// The step running now (or asking), else the first pending one.
    pub fn current_step(&self) -> Option<(usize, &WorkStep)> {
        self.steps
            .iter()
            .enumerate()
            .find(|(_, s)| matches!(s.state, StepState::Running | StepState::Asking))
            .or_else(|| {
                self.steps
                    .iter()
                    .enumerate()
                    .find(|(_, s)| s.state == StepState::Pending)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkState {
    #[default]
    Proposed,
    Running,
    Done,
    Failed,
    Stopped,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDecision {
    pub approve: bool,
    pub at: i64,
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkStep {
    pub id: String,
    pub title: String,
    /// The project (space id) it works in, when it works in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub state: StepState,
    /// Sessions this step started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    /// The model, thinking and Fast mode of the sessions it starts; what it
    /// leaves out follows the workspace's choice for sessions.
    #[serde(default, skip_serializing_if = "SessionChoice::is_empty")]
    pub session_choice: SessionChoice,
}

impl WorkStep {
    fn clamp(&mut self) {
        self.title = clip(&self.title, STEP_TITLE_MAX);
        // No step holds more than the whole plan's budget; the steps share
        // it in `BotWork::clamp`.
        self.detail = clip_opt(&self.detail, PLAN_DETAILS_MAX);
        self.note = clip_opt(&self.note, STEP_NOTE_MAX);
        self.sessions.truncate(STEP_SESSIONS_MAX);
        self.session_choice.clamp();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepState {
    #[default]
    Pending,
    Running,
    /// A session it started is waiting on an answer.
    Asking,
    Done,
    Failed,
    Skipped,
}

// ── notebook (§7) ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotNote {
    pub id: String,
    pub bot_id: String,
    pub text: String,
    #[serde(default)]
    pub source: NoteSource,
    pub created_at: i64,
    pub updated_at: i64,
}

impl BotNote {
    pub fn clamp(&mut self) {
        self.text = clip(&self.text, NOTE_MAX);
        if let NoteSource::Bot { title, .. } = &mut self.source {
            *title = clip_opt(title, TITLE_MAX);
        }
    }
}

/// Where a note came from (provenance, §7).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NoteSource {
    /// Written or edited by the owner.
    #[default]
    Owner,
    /// Written by the bot, from the thread or session named.
    #[serde(rename_all = "camelCase")]
    Bot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
}

// ── schedules and routines (§8) ─────────────────────────────────────────────

/// When a routine or task runs, in the host device's local time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Schedule {
    /// At `time` ("HH:MM") on `days` (1 = Monday … 7 = Sunday; empty =
    /// every day).
    #[serde(rename_all = "camelCase")]
    Daily {
        time: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        days: Vec<u8>,
    },
    /// Every `minutes` (at least [`Schedule::MIN_INTERVAL`]).
    #[serde(rename_all = "camelCase")]
    Interval { minutes: u32 },
    /// Only when run by hand.
    Manual,
    /// Once, at `at` ("YYYY-MM-DDTHH:MM", local time): a reminder.
    #[serde(rename_all = "camelCase")]
    Once { at: String },
}

impl Default for Schedule {
    fn default() -> Self {
        Self::Daily {
            time: "09:00".into(),
            days: Vec::new(),
        }
    }
}

impl Schedule {
    pub const MIN_INTERVAL: u32 = 15;

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Daily { time, days } => {
                NaiveTime::parse_from_str(time, "%H:%M")
                    .map_err(|_| "Pick a time like 09:00.".to_string())?;
                if days.iter().any(|d| !(1..=7).contains(d)) {
                    return Err("Days run from 1 (Monday) to 7 (Sunday).".into());
                }
                Ok(())
            }
            Self::Interval { minutes } if *minutes < Self::MIN_INTERVAL => {
                Err(format!("Run at most every {} minutes.", Self::MIN_INTERVAL))
            }
            Self::Interval { .. } | Self::Manual => Ok(()),
            Self::Once { at } => NaiveDateTime::parse_from_str(at, ONCE_FORMAT)
                .map(drop)
                .map_err(|_| "Pick a date and time like 2026-11-10T09:00.".to_string()),
        }
    }

    /// When a once-only schedule runs, in `tz`.
    pub fn once_at<Tz: TimeZone>(&self, tz: &Tz) -> Option<DateTime<Tz>> {
        let Self::Once { at } = self else {
            return None;
        };
        let at = NaiveDateTime::parse_from_str(at, ONCE_FORMAT).ok()?;
        tz.from_local_datetime(&at).earliest()
    }

    /// The first run strictly after `after`, in `after`'s time zone.
    pub fn next_after<Tz: TimeZone>(&self, after: &DateTime<Tz>) -> Option<DateTime<Tz>> {
        match self {
            Self::Manual => None,
            Self::Once { .. } => self.once_at(&after.timezone()).filter(|at| at > after),
            Self::Interval { minutes } => {
                Some(after.clone() + Duration::minutes((*minutes).max(Self::MIN_INTERVAL) as i64))
            }
            Self::Daily { time, days } => {
                let at = NaiveTime::parse_from_str(time, "%H:%M").ok()?;
                let tz = after.timezone();
                let start = after.date_naive();
                (0..=8).find_map(|offset| {
                    let date = start + Duration::days(offset);
                    let weekday = date.weekday().number_from_monday() as u8;
                    if !days.is_empty() && !days.contains(&weekday) {
                        return None;
                    }
                    let candidate = tz.from_local_datetime(&date.and_time(at)).earliest()?;
                    (candidate > *after).then_some(candidate)
                })
            }
        }
    }

    /// "Daily 09:00", "Weekdays 18:30", "Every 2 h", "Manual".
    pub fn describe(&self) -> String {
        match self {
            Self::Manual => "Manual".into(),
            Self::Once { at } => match NaiveDateTime::parse_from_str(at, ONCE_FORMAT) {
                Ok(at) => format!("Once, {}", at.format("%a %b %-d %H:%M")),
                Err(_) => "Once".into(),
            },
            Self::Interval { minutes } if minutes % 60 == 0 => match minutes / 60 {
                1 => "Every hour".into(),
                h => format!("Every {h} h"),
            },
            Self::Interval { minutes } => format!("Every {minutes} min"),
            Self::Daily { time, days } => {
                let mut sorted = days.clone();
                sorted.sort_unstable();
                sorted.dedup();
                let when = match sorted.as_slice() {
                    [] | [1, 2, 3, 4, 5, 6, 7] => "Daily".to_string(),
                    [1, 2, 3, 4, 5] => "Weekdays".to_string(),
                    [6, 7] => "Weekends".to_string(),
                    days => days
                        .iter()
                        .map(|d| {
                            ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
                                [(*d as usize).clamp(1, 7) - 1]
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                };
                format!("{when} {time}")
            }
        }
    }
}

/// The local date and time a once-only schedule names.
pub const ONCE_FORMAT: &str = "%Y-%m-%dT%H:%M";

/// One run of a routine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    pub outcome: RunOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The thread or session the run happened in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// Started by hand (Run now) in a fresh session, rather than by the
    /// schedule in the routine's standing one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
}

impl RunRecord {
    pub fn clamp(&mut self) {
        self.summary = clip_opt(&self.summary, RESULT_MAX / 2);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunOutcome {
    Running,
    Done,
    Failed,
    /// Skipped: the bot was paused, or can no longer run it there.
    Skipped,
}

/// Who posted a message into a bot chat outside any turn (the doc's `post`
/// message part): a routine's output, attributed to the routine (§8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BotPost {
    /// One routine run's output (or why it failed).
    #[serde(rename_all = "camelCase")]
    Routine {
        routine_id: String,
        /// The routine's name when it ran.
        name: String,
        /// `Done` or `Failed`.
        outcome: RunOutcome,
        /// When the run started.
        run_at: i64,
        /// The routine's thread, where the whole run can be read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_chat_id: Option<String>,
    },
    /// A plan's end (§6.6): how it ended, with the result as the post's
    /// text. The plan's own page holds the rest while its record is kept.
    #[serde(rename_all = "camelCase")]
    Work {
        work_id: String,
        /// The plan's title when it ended.
        title: String,
        /// `Done`, `Failed` or `Stopped`.
        outcome: WorkState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<i64>,
        ended_at: i64,
    },
    /// A source from a newer engine: shown as plain text.
    #[serde(other)]
    Unknown,
}

/// The id prefix of the turn a host starts in a workspace chat to tell its
/// agent that a background job ended (`bot-event-<post entry id>`). The
/// post before it is what the owner sees; apps draw nothing for the turn's
/// own message.
pub const BOT_EVENT_PREFIX: &str = "bot-event-";

/// The workspace-chat entry id of a plan's result post: one per plan.
pub fn work_post_id(work_id: &str) -> String {
    format!("work-{work_id}-ended")
}

/// Where a routine runs and what it can reach.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RoutineTarget {
    /// The bot's own agent, in a hidden thread: it reads everything the bot
    /// reads and posts its output in the bot chat.
    #[default]
    Workspace,
    /// An ordinary coding session in one of the bot's projects, hidden and
    /// opened from the routine; it can change that project's code. Runs on
    /// the project's device and needs the bot's *Coordinate*.
    #[serde(rename_all = "camelCase")]
    Project {
        space_id: String,
        /// Its own worktree (made on the first run, kept for the rest)
        /// rather than the project's checkout.
        #[serde(default)]
        worktree: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
}

impl RoutineTarget {
    pub fn space_id(&self) -> Option<&str> {
        match self {
            Self::Workspace => None,
            Self::Project { space_id, .. } => Some(space_id),
        }
    }
}

/// A bot job run by hand or on a schedule, in the background. Scheduled runs reuse
/// one standing session (`threadChatId`); each Run now gets a fresh one
/// (`manualChatId`), kept until the next replaces it. Its output is posted
/// in the bot chat either way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotRoutine {
    pub id: String,
    pub bot_id: String,
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub target: RoutineTarget,
    /// The model its runs use; `None` follows the workspace chat's (a
    /// project routine written before this field may carry its own in
    /// `target`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Its runs' thinking level; `None` follows what the model falls back
    /// to (a project routine the workspace's choice for sessions, then the
    /// workspace chat's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<crate::ReasoningLevel>,
    /// Its runs' GPT Fast mode; `None` follows the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
    #[serde(default)]
    pub schedule: Schedule,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// May write to the notebook (it always reads it). A workspace routine's.
    #[serde(default)]
    pub write_notes: bool,
    /// May use Pi's shell and file tools in the bot's local projects, like
    /// an owner thread with *Work directly* (and only while the bot has it).
    /// Anything that asks for approval waits for the owner in Needs you.
    #[serde(default)]
    pub work_directly: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// Owner-written: run once now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_requested_at: Option<i64>,
    /// Owner-written with a Run now: extra instructions for that run only.
    /// It belongs to the request it was written with (`at` is that
    /// request's `runRequestedAt`), so a later Run now without one never
    /// repeats it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_note: Option<RunNote>,
    // Host-written (the bot's device for a workspace routine, the
    // project's for a project routine):
    /// The standing session scheduled runs reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_chat_id: Option<String>,
    /// The latest Run now's session; the next one replaces and deletes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_chat_id: Option<String>,
    /// A project routine's own worktree, once its first run made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<RunRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<i64>,
}

/// A Run now's extra instructions ([`BotRoutine::run_note`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunNote {
    pub at: i64,
    pub text: String,
}

/// A Run now's note, at most this long.
pub const RUN_NOTE_MAX: usize = 2_000;

impl BotRoutine {
    /// Its runs' model, thinking level and Fast mode, as far as it names
    /// them.
    pub fn session_choice(&self) -> SessionChoice {
        SessionChoice {
            model: self.chosen_model().map(str::to_string),
            thinking: self.thinking,
            fast: self.fast,
        }
    }

    /// The note the Run now requested at `requested_at` carries.
    pub fn note_for(&self, requested_at: Option<i64>) -> Option<&str> {
        let note = self.run_note.as_ref()?;
        (Some(note.at) == requested_at)
            .then_some(note.text.trim())
            .filter(|text| !text.is_empty())
    }

    /// The model its runs should use, when it names one.
    pub fn chosen_model(&self) -> Option<&str> {
        self.model.as_deref().or(match &self.target {
            RoutineTarget::Project { model, .. } => model.as_deref(),
            RoutineTarget::Workspace => None,
        })
    }

    /// The owner's optional fields: a save that leaves one out clears it.
    pub const OWNER_OPTIONAL_FIELDS: &'static [&'static str] = &["model", "thinking", "fast"];

    /// The fields the host writes; everything else is the owner's.
    pub const HOST_FIELDS: &'static [&'static str] = &[
        "threadChatId",
        "manualChatId",
        "worktreePath",
        "lastRun",
        "nextRunAt",
    ];

    pub fn clamp(&mut self) {
        self.name = clip(&self.name, TITLE_MAX);
        self.prompt = clip(&self.prompt, PROMPT_MAX);
        self.model = clip_opt(&self.model, MODEL_ID_MAX);
        if let Some(note) = &mut self.run_note {
            note.text = clip(&note.text, RUN_NOTE_MAX);
        }
        if let Some(run) = &mut self.last_run {
            run.clamp();
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Name the routine.".into());
        }
        if self.prompt.trim().is_empty() {
            return Err("Say what the routine should do.".into());
        }
        if self
            .target
            .space_id()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err("Pick the project it runs in.".into());
        }
        self.schedule.validate()
    }
}

fn yes() -> bool {
    true
}

// ── shortcuts ───────────────────────────────────────────────────────────────

/// A shortcut's label, as its chip shows it.
pub const SHORTCUT_LABEL_MAX: usize = 32;
/// The blank a request may hold: the shortcut asks for it on click.
pub const SHORTCUT_INPUT: &str = "{input}";
/// The icons a shortcut may name (the app draws each).
pub const SHORTCUT_ICONS: [&str; 16] = [
    "bolt",
    "star",
    "rocket",
    "play",
    "chat",
    "checklist",
    "code",
    "terminal",
    "bug",
    "test-tube",
    "document",
    "global",
    "calendar",
    "cup-hot",
    "magic-stick",
    "flag",
];

/// A button at the bottom of a workspace's sidebar: a request the owner
/// makes often, sent to the workspace's agent again on each click. Only
/// the agent makes and changes shortcuts, when the owner asks in the
/// workspace chat; the owner's devices only order and delete them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotShortcut {
    pub id: String,
    pub bot_id: String,
    pub label: String,
    /// One of [`SHORTCUT_ICONS`]; the app picks one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// What a click sends to the agent; may hold [`SHORTCUT_INPUT`].
    pub request: String,
    /// Position among the workspace's shortcuts, lowest first.
    #[serde(default)]
    pub order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl BotShortcut {
    pub fn clamp(&mut self) {
        self.label = clip(&self.label, SHORTCUT_LABEL_MAX);
        self.request = clip(&self.request, PROMPT_MAX);
        if self
            .icon
            .as_deref()
            .is_some_and(|icon| !SHORTCUT_ICONS.contains(&icon))
        {
            self.icon = None;
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.label.trim().is_empty() {
            return Err("Give the shortcut a short label.".into());
        }
        if self.request.trim().is_empty() {
            return Err("Give the shortcut the request it sends.".into());
        }
        Ok(())
    }

    /// Whether a click asks for text first.
    pub fn wants_input(&self) -> bool {
        self.request.contains(SHORTCUT_INPUT)
    }

    /// The request a click sends, with `input` in every blank. Left empty,
    /// the blanks are dropped and the text around each tidied (see
    /// [`join_over_blank`]); the rest of the request stays as written.
    pub fn request_with(&self, input: &str) -> String {
        let input = input.trim();
        if !input.is_empty() || !self.wants_input() {
            return self.request.replace(SHORTCUT_INPUT, input);
        }
        let mut pieces = self.request.split(SHORTCUT_INPUT);
        let mut out = pieces.next().unwrap_or_default().to_string();
        for piece in pieces {
            join_over_blank(&mut out, piece);
        }
        out.trim().to_string()
    }
}

/// Punctuation that sits against the word before it.
const SHORTCUT_TIGHT: [char; 12] = [
    ',', '.', ':', ';', '!', '?', '，', '。', '：', '；', '！', '？',
];

/// Append `next` to `out` where an empty blank was: the spaces and tabs
/// on both sides become one space (none before [`SHORTCUT_TIGHT`]
/// punctuation or at a line's edge, none when neither side had any); a
/// blank that started the text or a line, or followed punctuation, takes
/// the punctuation after it along ("{input}, please review" → "please
/// review"); a blank alone on its line takes the line. Line breaks
/// elsewhere stay.
fn join_over_blank(out: &mut String, next: &str) {
    let gap = |c: char| c == ' ' || c == '\t';
    let kept = out.trim_end_matches(gap).len();
    let spaced = kept < out.len() || next.starts_with(gap);
    out.truncate(kept);
    let mut next = next.trim_start_matches(gap);
    if out.is_empty() || out.ends_with('\n') || out.ends_with(SHORTCUT_TIGHT) {
        next = next
            .trim_start_matches(SHORTCUT_TIGHT)
            .trim_start_matches(gap);
    }
    if out.ends_with('\n') && next.starts_with('\n') {
        next = &next[1..];
    }
    if spaced
        && !out.is_empty()
        && !out.ends_with('\n')
        && !next.is_empty()
        && !next.starts_with('\n')
        && !next.starts_with(SHORTCUT_TIGHT)
    {
        out.push(' ');
    }
    out.push_str(next);
}

// ── activity log (§4.4) ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotActivity {
    pub id: String,
    pub bot_id: String,
    pub at: i64,
    pub kind: ActivityKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
}

impl BotActivity {
    pub fn clamp(&mut self) {
        self.text = clip(&self.text, LINE_MAX);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivityKind {
    PlanProposed,
    PlanApproved,
    PlanCancelled,
    WorkFinished,
    WorkFailed,
    WorkStopped,
    SessionStarted,
    SessionSteered,
    SessionAnswered,
    SessionStopped,
    WorktreeCreated,
    NoteWritten,
    RoutineRan,
    TaskRan,
    ApprovalAsked,
    PeerAsked,
    PeerAnswered,
    ConnectionAdded,
    ConnectionRevoked,
    #[serde(other)]
    Other,
}

impl ActivityKind {
    /// The Activity panel's filter groups.
    pub fn group(self) -> ActivityGroup {
        match self {
            Self::PlanProposed
            | Self::PlanApproved
            | Self::PlanCancelled
            | Self::WorkFinished
            | Self::WorkFailed
            | Self::WorkStopped
            | Self::ApprovalAsked => ActivityGroup::Plans,
            Self::SessionStarted
            | Self::SessionSteered
            | Self::SessionAnswered
            | Self::SessionStopped
            | Self::WorktreeCreated => ActivityGroup::Sessions,
            Self::RoutineRan | Self::TaskRan => ActivityGroup::Routines,
            Self::NoteWritten => ActivityGroup::Notes,
            Self::PeerAsked
            | Self::PeerAnswered
            | Self::ConnectionAdded
            | Self::ConnectionRevoked => ActivityGroup::Network,
            Self::Other => ActivityGroup::Plans,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityGroup {
    Plans,
    Sessions,
    Routines,
    Notes,
    Network,
}

// ── approvals (the taint rule, §4.3; steering others' sessions) ─────────────

/// An action a thread may only take once the owner approves it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotApproval {
    pub id: String,
    pub bot_id: String,
    pub thread_chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<String>,
    /// The action in plain words ("Start a session in web: …").
    pub action: String,
    /// Why it needs approval.
    pub reason: String,
    pub created_at: i64,
    #[serde(default)]
    pub state: ApprovalState,
    /// Owner-written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<PlanDecision>,
}

impl BotApproval {
    pub const OWNER_FIELDS: &'static [&'static str] = &["decision"];

    pub fn clamp(&mut self) {
        self.action = clip(&self.action, DETAIL_MAX);
        self.reason = clip(&self.reason, LINE_MAX);
    }

    pub fn awaiting(&self) -> bool {
        self.state == ApprovalState::Pending && self.decision.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalState {
    #[default]
    Pending,
    Approved,
    Declined,
    Expired,
}

// ── network (§11) ───────────────────────────────────────────────────────────

/// Another user's bot, as its owner published it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerBot {
    pub room_id: String,
    pub name: String,
    #[serde(default)]
    pub owner_name: String,
    /// The owner's profile photo, from their identity provider; only a URL
    /// [`crate::peer_avatar_url`] accepts ever lands here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_avatar_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplyMode {
    /// Answers go back at once.
    #[default]
    Auto,
    /// Answers wait as drafts until the owner sends them.
    Review,
    /// Questions are declined.
    Off,
}

/// A mutual link between this bot and another user's bot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotConnection {
    pub id: String,
    pub bot_id: String,
    pub peer: PeerBot,
    pub created_at: i64,
    /// Owner-written.
    #[serde(default)]
    pub reply_mode: ReplyMode,
    /// Owner-written: narrows what this peer's questions may read (never
    /// wider than the bot's own scope). `None` = the bot's scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<BotScope>,
    /// Owner-written: revoke on both sides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoke_requested_at: Option<i64>,
    // Host-written:
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_exchange_at: Option<i64>,
}

impl BotConnection {
    pub const OWNER_FIELDS: &'static [&'static str] = &["replyMode", "scope", "revokeRequestedAt"];

    pub fn is_live(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// An invite the owner created; the code itself is shown once and never
/// stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotInvite {
    pub id: String,
    pub bot_id: String,
    pub created_at: i64,
    pub expires_at: i64,
    #[serde(default)]
    pub uses: u32,
    pub max_uses: u32,
    /// Owner-written: revoke before it is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoke_requested_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
}

impl BotInvite {
    pub const OWNER_FIELDS: &'static [&'static str] = &["revokeRequestedAt"];

    pub fn is_open(&self, now_ms: i64) -> bool {
        self.revoked_at.is_none() && self.uses < self.max_uses && now_ms < self.expires_at
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExchangeDirection {
    /// A peer asked this bot.
    Incoming,
    /// This bot asked a peer.
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExchangeState {
    #[default]
    Queued,
    Delivered,
    Working,
    AwaitingReview,
    Answered,
    Declined,
    Expired,
}

/// One question between two bots and its answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotExchange {
    pub id: String,
    pub bot_id: String,
    pub connection_id: String,
    pub direction: ExchangeDirection,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default)]
    pub state: ExchangeState,
    pub asked_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<i64>,
    /// Owner-written, for an incoming exchange in Review mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewDecision>,
}

impl BotExchange {
    pub const OWNER_FIELDS: &'static [&'static str] = &["review"];

    pub fn clamp(&mut self) {
        self.question = clip(&self.question, ANSWER_MAX / 2);
        self.answer = clip_opt(&self.answer, ANSWER_MAX);
    }

    pub fn awaiting_review(&self) -> bool {
        self.direction == ExchangeDirection::Incoming
            && self.state == ExchangeState::AwaitingReview
            && self.review.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDecision {
    /// Send (`true`, with the edited `text` when the owner changed it) or
    /// decline.
    pub send: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub at: i64,
}

// ── the whole set ───────────────────────────────────────────────────────────

/// Every bot record in the registry (the `WatchBotRecords` frame).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BotRecords {
    pub work: Vec<BotWork>,
    pub notes: Vec<BotNote>,
    pub routines: Vec<BotRoutine>,
    pub activity: Vec<BotActivity>,
    pub approvals: Vec<BotApproval>,
    pub connections: Vec<BotConnection>,
    pub invites: Vec<BotInvite>,
    pub exchanges: Vec<BotExchange>,
    /// Every memory store's row (docs/design/memory.md); not only
    /// workspaces' — the app lists all of them from this frame.
    pub memory_stores: Vec<crate::MemoryStore>,
    pub shortcuts: Vec<BotShortcut>,
}

/// One item on a bot's **Needs you** card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeedsYou {
    /// A plan awaiting approval (work id).
    Plan(String),
    /// An action awaiting approval (approval id).
    Approval(String),
    /// A worker or routine is asking (session id, work id for workers).
    Asking {
        chat_id: String,
        work_id: Option<String>,
    },
    /// A peer's answer drafted in Review mode (exchange id).
    Review(String),
}

impl BotRecords {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// What waits on the owner's decision for `bot_id`, oldest first within
    /// each kind and most pressing kind first. `asking(chat id)` says
    /// whether a session is waiting on an answer. Only decisions: a failed
    /// routine run is state, shown on the routine, not here.
    pub fn needs_you(&self, bot_id: &str, asking: impl Fn(&str) -> bool) -> Vec<NeedsYou> {
        let mut items = Vec::new();
        let mut approvals: Vec<&BotApproval> = self
            .approvals
            .iter()
            .filter(|a| a.bot_id == bot_id && a.awaiting())
            .collect();
        approvals.sort_by_key(|a| a.created_at);
        items.extend(
            approvals
                .into_iter()
                .map(|a| NeedsYou::Approval(a.id.clone())),
        );
        let mut work: Vec<&BotWork> = self.work.iter().filter(|w| w.bot_id == bot_id).collect();
        work.sort_by_key(|w| w.created_at);
        // A session the bot started that waits on an answer needs the owner
        // whatever its work did since: the work may have ended (or been
        // stopped) while the question stays open.
        for w in &work {
            for chat_id in w.session_ids() {
                if asking(chat_id) {
                    items.push(NeedsYou::Asking {
                        chat_id: chat_id.to_string(),
                        work_id: Some(w.id.clone()),
                    });
                }
            }
        }
        let mut routines: Vec<&BotRoutine> = self
            .routines
            .iter()
            .filter(|r| r.bot_id == bot_id)
            .collect();
        routines.sort_by_key(|r| r.last_run.as_ref().map(|run| run.at));
        for routine in routines {
            if let Some(run) = &routine.last_run
                && run.outcome == RunOutcome::Running
                && let Some(chat_id) = &run.chat_id
                && asking(chat_id)
            {
                items.push(NeedsYou::Asking {
                    chat_id: chat_id.clone(),
                    work_id: None,
                });
            }
        }
        items.extend(
            work.iter()
                .filter(|w| w.awaiting_approval())
                .map(|w| NeedsYou::Plan(w.id.clone())),
        );
        let mut reviews: Vec<&BotExchange> = self
            .exchanges
            .iter()
            .filter(|e| e.bot_id == bot_id && e.awaiting_review())
            .collect();
        reviews.sort_by_key(|e| e.asked_at);
        items.extend(reviews.into_iter().map(|e| NeedsYou::Review(e.id.clone())));
        items
    }

    /// The row of the store for `scope`, when it has one.
    pub fn memory_store(&self, scope: &crate::MemoryScope) -> Option<&crate::MemoryStore> {
        self.memory_stores.iter().find(|s| &s.scope == scope)
    }

    /// `bot_id`'s shortcuts, in their order.
    pub fn shortcuts_of(&self, bot_id: &str) -> Vec<&BotShortcut> {
        let mut mine: Vec<&BotShortcut> = self
            .shortcuts
            .iter()
            .filter(|s| s.bot_id == bot_id)
            .collect();
        mine.sort_by(|a, b| {
            a.order
                .cmp(&b.order)
                .then(a.created_at.cmp(&b.created_at))
                .then(a.id.cmp(&b.id))
        });
        mine
    }

    pub fn work_of<'a>(&'a self, bot_id: &'a str) -> impl Iterator<Item = &'a BotWork> + 'a {
        self.work.iter().filter(move |w| w.bot_id == bot_id)
    }

    /// The work item and step that started `chat_id`, when a bot did.
    pub fn started_by(&self, chat_id: &str) -> Option<(&BotWork, &WorkStep)> {
        self.work.iter().find_map(|w| {
            w.steps
                .iter()
                .find(|s| s.sessions.iter().any(|c| c == chat_id))
                .map(|s| (w, s))
        })
    }
}

/// Now, in epoch milliseconds.
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    #[test]
    fn daily_schedules_find_the_next_matching_day() {
        let daily = Schedule::Daily {
            time: "09:00".into(),
            days: vec![],
        };
        // Monday 2026-10-19, 08:00 local (+01:00).
        let monday_early = at("2026-10-19T08:00:00+01:00");
        assert_eq!(
            daily.next_after(&monday_early).unwrap(),
            at("2026-10-19T09:00:00+01:00")
        );
        assert_eq!(
            daily.next_after(&at("2026-10-19T09:00:00+01:00")).unwrap(),
            at("2026-10-20T09:00:00+01:00"),
            "strictly after"
        );
        let weekdays = Schedule::Daily {
            time: "18:30".into(),
            days: vec![1, 2, 3, 4, 5],
        };
        // Friday evening → Monday.
        assert_eq!(
            weekdays
                .next_after(&at("2026-10-23T19:00:00+01:00"))
                .unwrap(),
            at("2026-10-26T18:30:00+01:00")
        );
        assert_eq!(weekdays.describe(), "Weekdays 18:30");
        assert_eq!(daily.describe(), "Daily 09:00");
        assert_eq!(Schedule::Interval { minutes: 120 }.describe(), "Every 2 h");
        assert!(Schedule::Manual.next_after(&monday_early).is_none());
        assert!(Schedule::Interval { minutes: 5 }.validate().is_err());
        assert!(
            Schedule::Daily {
                time: "25:00".into(),
                days: vec![]
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn records_round_trip_and_default_missing_fields() {
        let work: BotWork = serde_json::from_value(serde_json::json!({
            "id": "w1", "botId": "b", "title": "Fix it", "chatId": "c", "createdAt": 1,
            "steps": [{ "id": "1", "title": "Do", "state": "running", "sessions": ["s1"] }],
            "newField": true,
        }))
        .unwrap();
        assert_eq!(work.state, WorkState::Proposed);
        assert!(work.awaiting_approval());
        assert_eq!(work.session_ids().collect::<Vec<_>>(), ["s1"]);
        let thread: BotThread =
            serde_json::from_value(serde_json::json!({ "kind": "somethingNew" })).unwrap();
        assert_eq!(thread, BotThread::Unknown);
        let caps: BotCapabilities = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(caps, BotCapabilities::default());
        let kind: ActivityKind = serde_json::from_value(serde_json::json!("future")).unwrap();
        assert_eq!(kind, ActivityKind::Other);
    }

    #[test]
    fn clamping_bounds_every_text() {
        let mut work = BotWork {
            id: "w".into(),
            bot_id: "b".into(),
            title: "t".repeat(500),
            instruction: "i".repeat(10_000),
            summary: Some("s".repeat(5_000)),
            steps: (0..30)
                .map(|i| WorkStep {
                    id: i.to_string(),
                    title: "x".repeat(1_000),
                    project: None,
                    detail: Some("d".repeat(2_000)),
                    state: StepState::Pending,
                    sessions: vec![],
                    note: None,
                    started_at: None,
                    ended_at: None,
                    session_choice: Default::default(),
                })
                .collect(),
            state: WorkState::Proposed,
            chat_id: "c".into(),
            tool_call_id: None,
            thread_chat_id: None,
            created_at: 0,
            started_at: None,
            ended_at: None,
            result: Some("r".repeat(9_000)),
            decision: None,
            stop_requested_at: None,
            notify: false,
            time_limit_minutes: None,
            time_added_minutes: None,
            warned_at: None,
            coordinator_choice: Default::default(),
            body_rev: None,
            result_rev: None,
        };
        work.clamp();
        let units = serde_json::to_string(&work).unwrap().encode_utf16().count();
        assert!(units < 14 * 1024, "{units}");
        assert_eq!(work.title.chars().count(), TITLE_MAX);
        assert!(work.title.ends_with('…'));
        assert!(work.details_len() <= PLAN_DETAILS_MAX);

        // Text whose JSON runs long (every character escaped), with every
        // step noted and the most sessions a work starts: still a row the
        // registry takes.
        work.result = Some("\"".repeat(RESULT_MAX));
        work.instruction = "\"".repeat(INSTRUCTION_MAX);
        // The most sessions any workspace may let a plan start, and the
        // longest model choices, too.
        let choice = SessionChoice {
            model: Some("m".repeat(MODEL_ID_MAX)),
            thinking: Some(crate::ReasoningLevel::XHigh),
            fast: Some(true),
        };
        work.coordinator_choice = choice.clone();
        work.time_limit_minutes = Some(PLAN_TIME_LIMIT_MAX_MINUTES);
        work.time_added_minutes = Some(PLAN_TIME_ADDED_MAX_MINUTES);
        work.warned_at = Some(1_794_465_204_971);
        let steps = work.steps.len();
        for (index, step) in work.steps.iter_mut().enumerate() {
            step.detail = Some("\"".repeat(PLAN_DETAILS_MAX));
            step.note = Some("n".repeat(STEP_NOTE_MAX));
            let share = (SESSIONS_PER_WORK_CAP as usize).div_ceil(steps);
            let count = share.min(
                SESSIONS_PER_WORK_CAP as usize
                    - (index * share).min(SESSIONS_PER_WORK_CAP as usize),
            );
            step.sessions = vec!["0".repeat(36); count];
            step.session_choice = choice.clone();
            step.started_at = Some(1_794_465_204_971);
            step.ended_at = Some(1_794_465_204_971);
        }
        assert_eq!(work.session_ids().count(), SESSIONS_PER_WORK_CAP as usize);
        work.clamp();
        let units = serde_json::to_string(&work).unwrap().encode_utf16().count();
        assert!(units <= WORK_ROW_MAX, "{units}");
        assert_eq!(
            work.session_ids().count(),
            SESSIONS_PER_WORK_CAP as usize,
            "no session dropped"
        );
    }

    #[test]
    fn a_session_choice_falls_back_field_by_field_and_applies_to_a_config() {
        let call = SessionChoice {
            model: Some("call".into()),
            ..Default::default()
        };
        let step = SessionChoice {
            model: Some("step".into()),
            thinking: Some(crate::ReasoningLevel::High),
            fast: None,
        };
        let workspace = SessionChoice {
            thinking: Some(crate::ReasoningLevel::Low),
            fast: Some(true),
            ..Default::default()
        };
        let choice = call.or(&step).or(&workspace);
        assert_eq!(choice.model.as_deref(), Some("call"));
        assert_eq!(choice.thinking, Some(crate::ReasoningLevel::High));
        assert_eq!(choice.fast, Some(true));
        assert_eq!(choice.describe(), "call, high thinking, Fast mode");

        let mut options = serde_json::Map::new();
        options.insert("effort".into(), "x".into());
        let mut config = crate::ChatConfig {
            harness: crate::HarnessId::Pi,
            model: Some("chat".into()),
            reasoning: Some(crate::ReasoningLevel::Medium),
            model_options: options,
            sandbox: crate::SandboxLevel::WorkspaceWrite,
        };
        choice.apply(&mut config);
        assert_eq!(config.model.as_deref(), Some("call"));
        assert_eq!(config.reasoning, Some(crate::ReasoningLevel::High));
        assert_eq!(
            serde_json::Value::Object(config.model_options.clone()),
            serde_json::json!({ "fast": true }),
            "the old model's options go; Fast mode is set"
        );
        assert_eq!(SessionChoice::of_config(&config), choice);

        // An empty choice changes nothing, and is left out of the row.
        let before = config.clone();
        SessionChoice::default().apply(&mut config);
        assert_eq!(config, before);
        let mut blank = SessionChoice {
            model: Some("  ".into()),
            ..Default::default()
        };
        blank.clamp();
        assert!(blank.is_empty());
    }

    #[test]
    fn plan_settings_default_and_stay_in_bounds() {
        let plans = PlanSettings::default();
        assert_eq!(plans.time_limit_minutes(), 240);
        assert_eq!(plans.running_max(), MAX_RUNNING_WORK);
        assert_eq!(plans.sessions_max(), MAX_SESSIONS_PER_WORK);
        assert!(plans.validate().is_ok() && plans.is_default());
        let wild = PlanSettings {
            time_limit_minutes: Some(1),
            running_max: Some(0),
            sessions_max: Some(99),
        };
        assert!(wild.validate().is_err());
        assert_eq!(wild.time_limit_minutes(), PLAN_TIME_LIMIT_MIN_MINUTES);
        assert_eq!(wild.running_max(), 1);
        assert_eq!(wild.sessions_max(), SESSIONS_PER_WORK_CAP as usize);
        assert_eq!(
            serde_json::to_value(PlanSettings {
                time_limit_minutes: Some(480),
                ..Default::default()
            })
            .unwrap(),
            serde_json::json!({ "timeLimitMinutes": 480 })
        );
        assert_eq!(describe_minutes(240), "4 hours");
        assert_eq!(describe_minutes(90), "1 hour 30 minutes");
        assert_eq!(describe_minutes(1), "1 minute");
    }

    #[test]
    fn a_run_note_belongs_to_its_request() {
        let mut routine: BotRoutine = serde_json::from_value(serde_json::json!({
            "id": "r", "botId": "b", "name": "n", "prompt": "p",
            "createdAt": 0, "updatedAt": 0,
            "runRequestedAt": 5, "runNote": { "at": 5, "text": " only web " },
        }))
        .unwrap();
        assert_eq!(routine.note_for(Some(5)), Some("only web"));
        routine.run_requested_at = Some(9);
        assert_eq!(
            routine.note_for(Some(9)),
            None,
            "a later request without one"
        );
    }

    /// One step may take the whole details budget. Every other text at its
    /// cap and made of quotes (two units each in JSON), on a plan that ran
    /// to its end: the row still fits. A detail of quotes, newlines and
    /// backslashes costs a proposal none of its characters.
    #[test]
    fn one_step_may_carry_every_detail_character() {
        let id = "0".repeat(36);
        let at = Some(1_794_465_204_971);
        let choice = SessionChoice {
            model: Some("m".repeat(MODEL_ID_MAX)),
            thinking: Some(crate::ReasoningLevel::XHigh),
            fast: Some(true),
        };
        let q = |max: usize| "\"".repeat(max);
        let ended = |detail: &str| BotWork {
            id: id.clone(),
            bot_id: id.clone(),
            title: q(TITLE_MAX),
            instruction: q(INSTRUCTION_MAX),
            summary: Some(q(DETAIL_MAX)),
            steps: vec![WorkStep {
                id: "1".into(),
                title: q(STEP_TITLE_MAX),
                project: Some(id.clone()),
                detail: Some(detail.into()),
                state: StepState::Done,
                sessions: vec![id.clone(); STEP_SESSIONS_MAX],
                note: Some(q(STEP_NOTE_MAX)),
                started_at: at,
                ended_at: at,
                session_choice: choice.clone(),
            }],
            state: WorkState::Done,
            chat_id: id.clone(),
            tool_call_id: Some(id.clone()),
            thread_chat_id: Some(id.clone()),
            created_at: 1_794_465_204_971,
            started_at: at,
            ended_at: at,
            result: Some(q(RESULT_MAX)),
            decision: Some(PlanDecision {
                approve: true,
                at: 1_794_465_204_971,
            }),
            stop_requested_at: at,
            notify: true,
            time_limit_minutes: Some(PLAN_TIME_LIMIT_MAX_MINUTES),
            time_added_minutes: Some(PLAN_TIME_ADDED_MAX_MINUTES),
            warned_at: at,
            coordinator_choice: choice.clone(),
            body_rev: None,
            result_rev: None,
        };
        // The proposal as the engine stores it: nothing has run yet.
        let proposed = |detail: &str| {
            let mut work = ended(detail);
            work.state = WorkState::Proposed;
            work.started_at = None;
            work.ended_at = None;
            work.result = None;
            work.decision = None;
            work.stop_requested_at = None;
            work.time_added_minutes = None;
            work.warned_at = None;
            let step = &mut work.steps[0];
            step.state = StepState::Pending;
            step.sessions.clear();
            step.note = None;
            step.started_at = None;
            step.ended_at = None;
            work
        };
        // `PLAN_DETAILS_MAX` characters of `unit`, between letters so that
        // clamp's trim takes none of them.
        let detail = |unit: &str| {
            let fill: String = unit.chars().cycle().take(PLAN_DETAILS_MAX - 2).collect();
            format!("d{fill}d")
        };
        for unit in ["\"", "\n", "\"\n\\\t"] {
            let detail = detail(unit);
            assert_eq!(detail.chars().count(), PLAN_DETAILS_MAX);
            let mut work = proposed(&detail);
            work.clamp();
            let units = work.row_units();
            assert!(units <= WORK_ROW_MAX, "{unit:?}: {units}");
            assert!(
                work.steps[0].detail.as_deref() == Some(detail.as_str()),
                "{unit:?}: a proposal keeps its detail ({units} units)"
            );
            // Ended, with every field filled in, it runs over even without
            // its result: the result goes, then only as much of the detail
            // as the row is over.
            let mut work = ended(&detail);
            work.clamp();
            let units = work.row_units();
            assert!(units <= WORK_ROW_MAX, "{unit:?} ended: {units}");
            assert!(
                units > WORK_ROW_MAX - 40,
                "{unit:?}: cut too much ({units})"
            );
            assert!(work.result.as_ref().is_some_and(|r| r.chars().count() <= 2));
            assert!(work.steps[0].detail.as_ref().unwrap().ends_with('…'));
        }
        // Control characters escape to six units each (`\u0001`): more than
        // the row holds, so the detail gives way, but only so far.
        let control = detail("\u{1}");
        for mut work in [proposed(&control), ended(&control)] {
            work.clamp();
            let units = work.row_units();
            assert!(units <= WORK_ROW_MAX, "control characters: {units}");
            assert!(units > WORK_ROW_MAX - 40, "cut too much ({units})");
        }
    }

    #[test]
    fn a_plan_within_its_caps_is_stored_whole() {
        let step = |detail: &str| WorkStep {
            id: "1".into(),
            title: "t".repeat(STEP_TITLE_MAX),
            project: None,
            detail: Some(detail.into()),
            state: StepState::Pending,
            sessions: vec![],
            note: None,
            started_at: None,
            ended_at: None,
            session_choice: Default::default(),
        };
        let mut work = BotWork {
            id: "w".into(),
            bot_id: "b".into(),
            title: "t".repeat(TITLE_MAX),
            instruction: "指".repeat(INSTRUCTION_MAX),
            summary: Some("s".repeat(DETAIL_MAX)),
            steps: vec![
                step(&"设".repeat(1_500)),
                step(&"计".repeat(1_500)),
                step(&"d".repeat(PLAN_DETAILS_MAX - 3_000)),
            ],
            state: WorkState::Proposed,
            chat_id: "c".into(),
            tool_call_id: None,
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
            body_rev: None,
            result_rev: None,
        };
        let before = work.clone();
        work.clamp();
        assert_eq!(work, before, "nothing within the caps is cut");
    }

    #[test]
    fn an_ended_plan_closes_the_steps_it_left_open() {
        let step = |state: StepState| WorkStep {
            id: "1".into(),
            title: "s".into(),
            project: None,
            detail: None,
            state,
            sessions: vec![],
            note: None,
            started_at: Some(10),
            ended_at: None,
            session_choice: Default::default(),
        };
        let work = |state: WorkState| BotWork {
            id: "w".into(),
            bot_id: "b".into(),
            title: "w".into(),
            instruction: String::new(),
            summary: None,
            steps: vec![
                step(StepState::Done),
                step(StepState::Running),
                step(StepState::Asking),
                step(StepState::Pending),
            ],
            state,
            chat_id: "c".into(),
            tool_call_id: None,
            thread_chat_id: None,
            created_at: 0,
            started_at: Some(5),
            ended_at: Some(90),
            result: None,
            decision: None,
            stop_requested_at: None,
            notify: false,
            time_limit_minutes: None,
            time_added_minutes: None,
            warned_at: None,
            coordinator_choice: Default::default(),
            body_rev: None,
            result_rev: None,
        };
        let states = |w: &BotWork| w.steps.iter().map(|s| s.state).collect::<Vec<_>>();
        use StepState::{Asking, Done, Failed, Pending, Running, Skipped};

        let mut running = work(WorkState::Running);
        assert!(!running.close_steps(100), "a live plan keeps its steps");
        assert_eq!(states(&running), [Done, Running, Asking, Pending]);

        let mut done = work(WorkState::Done);
        assert!(done.close_steps(100));
        assert_eq!(states(&done), [Done, Done, Done, Skipped]);
        assert_eq!(done.steps[0].ended_at, None, "a closed step is left alone");
        assert_eq!(done.steps[1].ended_at, Some(90), "it ended with its plan");
        assert!(!done.close_steps(100), "nothing left to close");

        let mut failed = work(WorkState::Failed);
        failed.close_steps(100);
        assert_eq!(states(&failed), [Done, Failed, Failed, Skipped]);

        let mut stopped = work(WorkState::Stopped);
        stopped.close_steps(100);
        assert_eq!(states(&stopped), [Done, Skipped, Skipped, Skipped]);

        let mut cancelled = work(WorkState::Cancelled);
        assert!(!cancelled.close_steps(100), "it never ran");
        assert_eq!(states(&cancelled), [Done, Running, Asking, Pending]);
    }

    #[test]
    fn needs_you_orders_by_urgency() {
        let mut records = BotRecords::default();
        let step = |sessions: Vec<&str>| WorkStep {
            id: "1".into(),
            title: "s".into(),
            project: None,
            detail: None,
            state: StepState::Running,
            sessions: sessions.into_iter().map(Into::into).collect(),
            note: None,
            started_at: None,
            ended_at: None,
            session_choice: Default::default(),
        };
        let work = |id: &str, state: WorkState, sessions: Vec<&str>| BotWork {
            id: id.into(),
            bot_id: "b".into(),
            title: id.into(),
            instruction: String::new(),
            summary: None,
            steps: vec![step(sessions)],
            state,
            chat_id: "c".into(),
            tool_call_id: None,
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
            body_rev: None,
            result_rev: None,
        };
        records.work = vec![
            work("plan", WorkState::Proposed, vec![]),
            work("run", WorkState::Running, vec!["s1", "s2"]),
            work("other-bot", WorkState::Proposed, vec![]),
            work("ended", WorkState::Done, vec!["s3"]),
        ];
        records.work[2].bot_id = "x".into();
        records.approvals = vec![BotApproval {
            id: "a".into(),
            bot_id: "b".into(),
            thread_chat_id: "t".into(),
            work_id: None,
            action: "Start".into(),
            reason: "Peer".into(),
            created_at: 0,
            state: ApprovalState::Pending,
            decision: None,
        }];
        let items = records.needs_you("b", |chat| chat == "s2" || chat == "s3");
        assert_eq!(
            items,
            vec![
                NeedsYou::Approval("a".into()),
                NeedsYou::Asking {
                    chat_id: "s2".into(),
                    work_id: Some("run".into())
                },
                NeedsYou::Asking {
                    chat_id: "s3".into(),
                    work_id: Some("ended".into())
                },
                NeedsYou::Plan("plan".into()),
            ]
        );
        assert_eq!(records.started_by("s1").unwrap().0.id, "run");
    }

    #[test]
    fn routine_approvals_need_the_owner_only_while_the_run_is_waiting() {
        let mut routine: BotRoutine = serde_json::from_value(serde_json::json!({
            "id": "r", "botId": "b", "name": "Cleanup", "prompt": "Clean",
            "createdAt": 0, "updatedAt": 0,
            "lastRun": { "at": 1, "outcome": "running", "chatId": "routine-chat" }
        }))
        .unwrap();
        let mut records = BotRecords {
            routines: vec![routine.clone()],
            ..Default::default()
        };
        assert_eq!(
            records.needs_you("b", |id| id == "routine-chat"),
            vec![NeedsYou::Asking {
                chat_id: "routine-chat".into(),
                work_id: None
            }]
        );
        assert!(records.needs_you("other", |_| true).is_empty());
        assert!(records.needs_you("b", |_| false).is_empty());
        for outcome in [RunOutcome::Done, RunOutcome::Failed, RunOutcome::Skipped] {
            routine.last_run.as_mut().unwrap().outcome = outcome;
            records.routines = vec![routine.clone()];
            assert!(records.needs_you("b", |_| true).is_empty());
        }
    }

    #[test]
    fn shortcuts_read_back_in_order_and_fill_their_blank() {
        let shortcut: BotShortcut = serde_json::from_value(serde_json::json!({
            "id": "s1", "botId": "b1", "label": "Fix tests", "icon": "test-tube",
            "request": "Start a session in invoice-demo to fix the tests. {input}",
            "createdAt": 1, "updatedAt": 1,
        }))
        .unwrap();
        assert!(shortcut.validate().is_ok());
        assert!(shortcut.wants_input());
        assert_eq!(
            shortcut.request_with("  Focus on tax. "),
            "Start a session in invoice-demo to fix the tests. Focus on tax."
        );
        let mut odd = shortcut.clone();
        odd.icon = Some("unicorn".into());
        odd.clamp();
        assert_eq!(odd.icon, None, "only the app's icons");
        odd.request = " ".into();
        assert!(odd.validate().is_err());

        let mut records = BotRecords::default();
        for (id, order, bot) in [
            ("c", 2, "b1"),
            ("a", 0, "b1"),
            ("x", 0, "b2"),
            ("b", 1, "b1"),
        ] {
            let mut s = shortcut.clone();
            s.id = id.into();
            s.order = order;
            s.bot_id = bot.into();
            records.shortcuts.push(s);
        }
        let ids: Vec<&str> = records
            .shortcuts_of("b1")
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }

    #[test]
    fn an_empty_blank_drops_out_and_the_text_around_it_closes_up() {
        let with = |request: &str, input: &str| {
            let s: BotShortcut = serde_json::from_value(serde_json::json!({
                "id": "s", "botId": "b", "label": "x", "request": request,
                "createdAt": 1, "updatedAt": 1,
            }))
            .unwrap();
            s.request_with(input)
        };
        let cases = [
            (
                "Create a PR from branch {input} to main.",
                "",
                "Create a PR from branch to main.",
            ),
            ("Fix the tests. {input}", "  ", "Fix the tests."),
            // A leading blank takes its orphaned punctuation with it.
            ("{input}, please review", "", "please review"),
            ("Review the branch {input}.", "", "Review the branch."),
            ("Check {input} , then {input}  ship", "", "Check, then ship"),
            ("Before, {input}, after", "", "Before, after"),
            ("Fix it:\t{input}\nThen ship.", "", "Fix it:\nThen ship."),
            // A blank alone on its line takes the line; other lines stay.
            (
                "Line one\n{input}\nLine two\n\nLine  three",
                "",
                "Line one\nLine two\n\nLine  three",
            ),
            ("Explain{input}simply", "", "Explainsimply"),
            ("请修复{input}，然后测试", "", "请修复，然后测试"),
            ("{input}", "", ""),
            // Without a blank, or with text, nothing is tidied.
            ("  What's  stuck?  ", "", "  What's  stuck?  "),
            (
                "Create a PR from branch {input} to main.",
                " dev ",
                "Create a PR from branch dev to main.",
            ),
            ("{input}, please review", "This", "This, please review"),
            ("Fix  {input} ,then", "x", "Fix  x ,then"),
        ];
        for (request, input, sent) in cases {
            assert_eq!(with(request, input), sent, "{request:?} + {input:?}");
        }
    }
}
