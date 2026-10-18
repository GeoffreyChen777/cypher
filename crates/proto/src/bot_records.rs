//! The records behind the Botboard (docs/design/agent-bots.md §6–§13.1):
//! a bot's plans and orchestrations, notebook, routines, its projects'
//! recurring tasks, its activity log, approvals, and its network.
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

use chrono::{DateTime, Datelike, Duration, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::BotScope;

/// Registry row kinds.
pub const KIND_BOT_WORK: &str = "botWork";
pub const KIND_BOT_NOTES: &str = "botNotes";
pub const KIND_BOT_ROUTINES: &str = "botRoutines";
pub const KIND_PROJECT_TASKS: &str = "projectTasks";
pub const KIND_BOT_ACTIVITY: &str = "botActivity";
pub const KIND_BOT_APPROVALS: &str = "botApprovals";
pub const KIND_BOT_CONNECTIONS: &str = "botConnections";
pub const KIND_BOT_INVITES: &str = "botInvites";
pub const KIND_BOT_EXCHANGES: &str = "botExchanges";

/// Every record kind, in [`BotRecords`] order.
pub const BOT_RECORD_KINDS: [&str; 9] = [
    KIND_BOT_WORK,
    KIND_BOT_NOTES,
    KIND_BOT_ROUTINES,
    KIND_PROJECT_TASKS,
    KIND_BOT_ACTIVITY,
    KIND_BOT_APPROVALS,
    KIND_BOT_CONNECTIONS,
    KIND_BOT_INVITES,
    KIND_BOT_EXCHANGES,
];

// ── caps ────────────────────────────────────────────────────────────────────

pub const TITLE_MAX: usize = 120;
pub const LINE_MAX: usize = 300;
pub const STEP_TITLE_MAX: usize = 160;
pub const DETAIL_MAX: usize = 400;
pub const INSTRUCTION_MAX: usize = 1_500;
pub const RESULT_MAX: usize = 2_000;
pub const NOTE_MAX: usize = 1_000;
pub const PROMPT_MAX: usize = 4_000;
pub const ANSWER_MAX: usize = 8_000;
pub const PLAN_STEPS_MAX: usize = 10;
pub const STEP_SESSIONS_MAX: usize = 8;

/// Limits the host enforces per bot (§6.6).
pub const MAX_RUNNING_WORK: usize = 3;
pub const MAX_SESSIONS_PER_WORK: usize = 8;
/// How long an orchestration may run before the host stops it.
pub const WORK_TIME_BUDGET_MS: i64 = 4 * 60 * 60 * 1000;
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
    /// What a new bot starts with: it may work in its local projects; it
    /// coordinates only once the owner turns that on.
    pub fn new_bot() -> Self {
        Self {
            work_directly: true,
            coordinate: false,
            steer: false,
        }
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
}

impl BotWork {
    /// The fields only an owner device writes.
    pub const OWNER_FIELDS: &'static [&'static str] = &["decision", "stopRequestedAt"];

    pub fn clamp(&mut self) {
        self.title = clip(&self.title, TITLE_MAX);
        self.instruction = clip(&self.instruction, INSTRUCTION_MAX);
        self.summary = clip_opt(&self.summary, DETAIL_MAX);
        self.result = clip_opt(&self.result, RESULT_MAX);
        self.steps.truncate(PLAN_STEPS_MAX);
        for step in &mut self.steps {
            step.clamp();
        }
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
}

impl WorkStep {
    fn clamp(&mut self) {
        self.title = clip(&self.title, STEP_TITLE_MAX);
        self.detail = clip_opt(&self.detail, DETAIL_MAX);
        self.note = clip_opt(&self.note, STEP_TITLE_MAX);
        self.sessions.truncate(STEP_SESSIONS_MAX);
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

// ── schedules, routines and recurring tasks (§8) ────────────────────────────

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
        }
    }

    /// The first run strictly after `after`, in `after`'s time zone.
    pub fn next_after<Tz: TimeZone>(&self, after: &DateTime<Tz>) -> Option<DateTime<Tz>> {
        match self {
            Self::Manual => None,
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

/// One run of a routine or task.
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
    /// Skipped: the bot was paused, or the day's budget was spent.
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
    /// A source from a newer engine: shown as plain text.
    #[serde(other)]
    Unknown,
}

/// A bot job on a schedule, run unattended in its own thread; its output is
/// a message in the bot chat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotRoutine {
    pub id: String,
    pub bot_id: String,
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub schedule: Schedule,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// May write to the notebook (it always reads it).
    #[serde(default)]
    pub write_notes: bool,
    /// May use Pi's shell and file tools in the bot's local projects, like
    /// an owner thread with *Work directly* (and only while the bot has it).
    /// Unattended: anything that would ask for approval fails the run.
    #[serde(default)]
    pub work_directly: bool,
    /// Runs a day before the rest are skipped.
    #[serde(default = "default_runs_per_day")]
    pub max_runs_per_day: u32,
    pub created_at: i64,
    pub updated_at: i64,
    /// Owner-written: run once now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_requested_at: Option<i64>,
    // Host-written:
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<RunRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<i64>,
    /// Runs on `runs_day` (the host's local date, "YYYY-MM-DD").
    #[serde(default)]
    pub runs_today: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_day: Option<String>,
}

impl BotRoutine {
    /// The fields the host writes; everything else is the owner's.
    pub const HOST_FIELDS: &'static [&'static str] = &[
        "threadChatId",
        "lastRun",
        "nextRunAt",
        "runsToday",
        "runsDay",
    ];

    pub fn clamp(&mut self) {
        self.name = clip(&self.name, TITLE_MAX);
        self.prompt = clip(&self.prompt, PROMPT_MAX);
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
        self.schedule.validate()
    }
}

fn yes() -> bool {
    true
}

fn default_runs_per_day() -> u32 {
    24
}

/// A recurring job on a project: every run starts a fresh session there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTask {
    pub id: String,
    pub space_id: String,
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub schedule: Schedule,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Each run in a new worktree.
    #[serde(default)]
    pub worktree: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The bot that created it, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_requested_at: Option<i64>,
    // Host-written (the project's device):
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<RunRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<i64>,
}

impl ProjectTask {
    pub const HOST_FIELDS: &'static [&'static str] = &["lastRun", "nextRunAt"];

    pub fn clamp(&mut self) {
        self.name = clip(&self.name, TITLE_MAX);
        self.prompt = clip(&self.prompt, PROMPT_MAX);
        if let Some(run) = &mut self.last_run {
            run.clamp();
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Name the task.".into());
        }
        if self.prompt.trim().is_empty() {
            return Err("Say what each run should do.".into());
        }
        self.schedule.validate()
    }
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
            Self::NoteWritten | Self::RoutineRan | Self::TaskRan => ActivityGroup::Memory,
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
    Memory,
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
    pub tasks: Vec<ProjectTask>,
    pub activity: Vec<BotActivity>,
    pub approvals: Vec<BotApproval>,
    pub connections: Vec<BotConnection>,
    pub invites: Vec<BotInvite>,
    pub exchanges: Vec<BotExchange>,
}

/// One item on a bot's **Needs you** card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeedsYou {
    /// A plan awaiting approval (work id).
    Plan(String),
    /// An action awaiting approval (approval id).
    Approval(String),
    /// A session the bot started is asking (session id, work id).
    Asking { chat_id: String, work_id: String },
    /// A peer's answer drafted in Review mode (exchange id).
    Review(String),
    /// A routine's last run failed (routine id).
    RoutineFailed(String),
}

impl BotRecords {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// What needs the owner for `bot_id`, oldest first within each kind and
    /// most pressing kind first. `asking(chat id)` says whether a session is
    /// waiting on an answer.
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
        for w in &work {
            if w.state == WorkState::Running {
                for chat_id in w.session_ids() {
                    if asking(chat_id) {
                        items.push(NeedsYou::Asking {
                            chat_id: chat_id.to_string(),
                            work_id: w.id.clone(),
                        });
                    }
                }
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
        items.extend(
            self.routines
                .iter()
                .filter(|r| {
                    r.bot_id == bot_id
                        && r.enabled
                        && r.last_run
                            .as_ref()
                            .is_some_and(|run| run.outcome == RunOutcome::Failed)
                })
                .map(|r| NeedsYou::RoutineFailed(r.id.clone())),
        );
        items
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
        };
        work.clamp();
        let units = serde_json::to_string(&work).unwrap().encode_utf16().count();
        assert!(units < 14 * 1024, "{units}");
        assert_eq!(work.title.chars().count(), TITLE_MAX);
        assert!(work.title.ends_with('…'));
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
        };
        records.work = vec![
            work("plan", WorkState::Proposed, vec![]),
            work("run", WorkState::Running, vec!["s1", "s2"]),
            work("other-bot", WorkState::Proposed, vec![]),
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
        let items = records.needs_you("b", |chat| chat == "s2");
        assert_eq!(
            items,
            vec![
                NeedsYou::Approval("a".into()),
                NeedsYou::Asking {
                    chat_id: "s2".into(),
                    work_id: "run".into()
                },
                NeedsYou::Plan("plan".into()),
            ]
        );
        assert_eq!(records.started_by("s1").unwrap().0.id, "run");
    }
}
