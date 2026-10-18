//! The **Schedule** card (§13.1): routines and the bot's projects' recurring
//! tasks by next run, their panels and the new-routine and new-task forms.
//!
//! A routine is the bot's own job, run in its hidden thread; a task belongs
//! to one of the bot's projects and starts a session there each run (§8).
//! The definition fields are the owner's and go out whole in one Mutate;
//! the run state (last run, next run, runs today) is the host's and only
//! read here.

use super::board::{card, section_header, two_lines};
use super::panel::{PanelHead, PanelItem};
use super::parts::{self, CallError};
use super::*;
use chrono::{DateTime, Local, TimeZone};
use cypher_proto::{BotRoutine, ProjectTask, RunOutcome, RunRecord, Schedule, Space};

// ── the rows ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum JobKind {
    Routine,
    Task { space_id: String },
}

/// One row of the Schedule: a routine of the bot, or a recurring task of
/// one of its projects.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Job {
    pub kind: JobKind,
    pub id: String,
    pub name: String,
    pub schedule: Schedule,
    pub enabled: bool,
    /// The next run (epoch ms): `None` while off, manual or unplanned.
    pub next: Option<i64>,
    pub last: Option<RunRecord>,
    pub run_requested_at: Option<i64>,
}

/// What a row says about a job, most pressing first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JobState {
    Running,
    /// Run now was pressed and the host has not started it yet.
    Starting,
    Next(i64),
    Manual,
    /// On a schedule, but no next run is known.
    Unplanned,
    Off,
}

/// The run after `now`: the host's plan while it is still ahead, else the
/// schedule's own next slot. `None` while off or manual.
pub(super) fn next_run<Tz: TimeZone>(
    schedule: &Schedule,
    enabled: bool,
    planned: Option<i64>,
    now: &DateTime<Tz>,
) -> Option<i64> {
    if !enabled || *schedule == Schedule::Manual {
        return None;
    }
    let now_ms = now.timestamp_millis();
    planned
        .filter(|at| *at > now_ms)
        .or_else(|| schedule.next_after(now).map(|at| at.timestamp_millis()))
}

impl Job {
    fn routine<Tz: TimeZone>(r: &BotRoutine, now: &DateTime<Tz>) -> Self {
        Self {
            kind: JobKind::Routine,
            id: r.id.clone(),
            name: r.name.clone(),
            schedule: r.schedule.clone(),
            enabled: r.enabled,
            next: next_run(&r.schedule, r.enabled, r.next_run_at, now),
            last: r.last_run.clone(),
            run_requested_at: r.run_requested_at,
        }
    }

    fn task<Tz: TimeZone>(t: &ProjectTask, now: &DateTime<Tz>) -> Self {
        Self {
            kind: JobKind::Task {
                space_id: t.space_id.clone(),
            },
            id: t.id.clone(),
            name: t.name.clone(),
            schedule: t.schedule.clone(),
            enabled: t.enabled,
            next: next_run(&t.schedule, t.enabled, t.next_run_at, now),
            last: t.last_run.clone(),
            run_requested_at: t.run_requested_at,
        }
    }

    pub(super) fn item(&self) -> PanelItem {
        match self.kind {
            JobKind::Routine => PanelItem::Routine(self.id.clone()),
            JobKind::Task { .. } => PanelItem::Task(self.id.clone()),
        }
    }

    pub(super) fn state(&self) -> JobState {
        job_state(
            self.enabled,
            &self.schedule,
            self.next,
            self.last.as_ref(),
            self.run_requested_at,
        )
    }

    fn failed(&self) -> bool {
        self.last
            .as_ref()
            .is_some_and(|run| run.outcome == RunOutcome::Failed)
    }
}

fn job_state(
    enabled: bool,
    schedule: &Schedule,
    next: Option<i64>,
    last: Option<&RunRecord>,
    run_requested_at: Option<i64>,
) -> JobState {
    if last.is_some_and(|run| run.outcome == RunOutcome::Running) {
        return JobState::Running;
    }
    if run_requested_at.is_some_and(|at| last.is_none_or(|run| run.at < at)) {
        return JobState::Starting;
    }
    if !enabled {
        return JobState::Off;
    }
    match (schedule, next) {
        (Schedule::Manual, _) => JobState::Manual,
        (_, Some(at)) => JobState::Next(at),
        (_, None) => JobState::Unplanned,
    }
}

/// Running work first, then by next run, then the manual ones, the
/// switched-off ones last; names break ties.
pub(super) fn order_jobs(jobs: &mut [Job]) {
    jobs.sort_by_cached_key(|job| {
        let (rank, at) = match job.state() {
            JobState::Running => (0, 0),
            JobState::Starting => (1, 0),
            JobState::Next(at) => (2, at),
            JobState::Manual | JobState::Unplanned => (3, 0),
            JobState::Off => (4, 0),
        };
        (rank, at, job.name.to_lowercase())
    });
}

/// A row's sub-line: "Routine · Daily 09:00", "<project> · Weekdays 18:30".
pub(super) fn job_sub(job: &Job, project: Option<&str>) -> String {
    let lead = match job.kind {
        JobKind::Routine => "Routine",
        JobKind::Task { .. } => project.unwrap_or("Project"),
    };
    format!("{lead} · {}", job.schedule.describe())
}

/// A row's right-hand note.
pub(super) fn state_note<Tz: TimeZone>(state: JobState, now: &DateTime<Tz>) -> String {
    match state {
        JobState::Running => "running".into(),
        JobState::Starting => "starting…".into(),
        JobState::Next(at) => match now.timezone().timestamp_millis_opt(at).single() {
            Some(at) => parts::next_label(&at, now),
            None => String::new(),
        },
        JobState::Manual => "manual".into(),
        JobState::Unplanned => "—".into(),
        JobState::Off => "off".into(),
    }
}

/// Runs counted today: the host's count while its day is today.
pub(super) fn runs_today(count: u32, day: Option<&str>, today: &str) -> u32 {
    if day == Some(today) { count } else { 0 }
}

// ── the schedule editor ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScheduleKind {
    Daily,
    Every,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unit {
    Hours,
    Minutes,
}

/// The editor's choices besides its two text fields (the time, and the
/// interval's count).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScheduleDraft {
    pub kind: ScheduleKind,
    /// 1 = Monday … 7 = Sunday; empty = every day.
    pub days: Vec<u8>,
    pub unit: Unit,
}

impl ScheduleDraft {
    /// The editor for `schedule`, with its time and count texts.
    pub(super) fn from_schedule(schedule: &Schedule) -> (Self, String, String) {
        let mut draft = Self {
            kind: ScheduleKind::Daily,
            days: Vec::new(),
            unit: Unit::Hours,
        };
        let (mut time, mut every) = ("09:00".to_string(), "1".to_string());
        match schedule {
            Schedule::Daily { time: at, days } => {
                time = at.clone();
                draft.days = days.clone();
                draft.days.sort_unstable();
                draft.days.dedup();
                if draft.days.len() == 7 {
                    draft.days.clear();
                }
            }
            Schedule::Interval { minutes } => {
                draft.kind = ScheduleKind::Every;
                if minutes % 60 == 0 {
                    every = (minutes / 60).to_string();
                } else {
                    draft.unit = Unit::Minutes;
                    every = minutes.to_string();
                }
            }
            Schedule::Manual => draft.kind = ScheduleKind::Manual,
        }
        (draft, time, every)
    }

    /// The schedule the editor describes, or what to fix.
    pub(super) fn build(&self, time: &str, every: &str) -> Result<Schedule, String> {
        let schedule = match self.kind {
            ScheduleKind::Daily => {
                let mut days = self.days.clone();
                days.sort_unstable();
                days.dedup();
                if days.len() == 7 {
                    days.clear();
                }
                Schedule::Daily {
                    time: normalize_time(time),
                    days,
                }
            }
            ScheduleKind::Every => {
                let count: u32 = every
                    .trim()
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| "Say how often, like every 2 hours.".to_string())?;
                Schedule::Interval {
                    minutes: match self.unit {
                        Unit::Hours => count.saturating_mul(60),
                        Unit::Minutes => count,
                    },
                }
            }
            ScheduleKind::Manual => Schedule::Manual,
        };
        schedule.validate()?;
        Ok(schedule)
    }

    fn toggle_day(&mut self, day: u8) {
        if let Some(ix) = self.days.iter().position(|d| *d == day) {
            self.days.remove(ix);
        } else {
            self.days.push(day);
            self.days.sort_unstable();
        }
    }
}

/// "9:00" → "09:00", "9" → "09:00", "1830" → "18:30"; anything else as
/// typed (for [`Schedule::validate`] to refuse).
pub(super) fn normalize_time(text: &str) -> String {
    let text = text.trim();
    let digits = |s: &str| !s.is_empty() && s.len() <= 2 && s.bytes().all(|b| b.is_ascii_digit());
    let (hour, minute) = match text.split_once(':') {
        Some((h, m)) if digits(h) && m.len() == 2 && digits(m) => (h, m),
        None if digits(text) => (text, "00"),
        None if text.len() == 4 && text.bytes().all(|b| b.is_ascii_digit()) => {
            (&text[..2], &text[2..])
        }
        _ => return text.to_string(),
    };
    format!("{hour:0>2}:{minute}")
}

// ── panel state ─────────────────────────────────────────────────────────────

/// What a form edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FormTarget {
    NewRoutine,
    NewTask,
    Routine(String),
    Task(String),
}

impl FormTarget {
    fn routine(&self) -> bool {
        matches!(self, Self::NewRoutine | Self::Routine(_))
    }
}

pub(super) struct JobForm {
    target: FormTarget,
    bot_id: String,
    name: Entity<TextInput>,
    prompt: Entity<TextInput>,
    time: Entity<TextInput>,
    every: Entity<TextInput>,
    draft: ScheduleDraft,
    write_notes: bool,
    worktree: bool,
    space_id: Option<String>,
    error: Option<SharedString>,
    busy: bool,
    _events: Vec<Subscription>,
}

/// The Schedule panels' own state: the open form, and an item just saved
/// whose row has not landed yet.
#[derive(Default)]
pub(super) struct ScheduleUi {
    form: Option<JobForm>,
    saving: Option<(String, String)>,
}

impl Shell {
    fn schedule_jobs(&self, bot: &Bot, cx: &App) -> Vec<(Job, Option<Space>)> {
        let now = Local::now();
        let state = self.state.read(cx);
        let records = &state.bot_records;
        let routines = records
            .routines
            .iter()
            .filter(|r| r.bot_id == bot.id)
            .map(|r| (Job::routine(r, &now), None));
        let tasks = records.tasks.iter().filter_map(|t| {
            if !bot.scope.sees_space(&t.space_id) {
                return None;
            }
            let space = state.space_row(&t.space_id)?.clone();
            Some((Job::task(t, &now), Some(space)))
        });
        let mut jobs: Vec<(Job, Option<Space>)> = routines.chain(tasks).collect();
        let mut ordered: Vec<Job> = jobs.iter().map(|(job, _)| job.clone()).collect();
        order_jobs(&mut ordered);
        jobs.sort_by_key(|(job, _)| {
            ordered
                .iter()
                .position(|o| o.kind == job.kind && o.id == job.id)
        });
        jobs
    }

    /// The bot's projects a task may run in, in sidebar order.
    fn task_projects(&self, bot: &Bot, cx: &App) -> Vec<(Space, String)> {
        let state = self.state.read(cx);
        state
            .spaces_sorted()
            .into_iter()
            .filter(|s| bot.scope.sees_space(&s.id))
            .map(|s| {
                let device = state
                    .device_name(&s.device_id)
                    .unwrap_or("Unknown device")
                    .to_string();
                (s.clone(), device)
            })
            .collect()
    }

    /// One schedule row: the routine's clock in the bot's colour or the
    /// task's project glyph, name over trigger, and the next run.
    #[allow(clippy::too_many_arguments)]
    fn job_row(
        &self,
        key: String,
        job: &Job,
        space: Option<&Space>,
        bot_tint: gpui::Hsla,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (glyph, tint) = match space {
            Some(space) => (
                crate::appearance::space_style::space_icon(space.icon.as_deref()),
                crate::appearance::space_style::space_color(space.color.as_deref(), theme)
                    .unwrap_or(theme.text_muted),
            ),
            None => (icons::CLOCK, bot_tint),
        };
        let state = job.state();
        let note_color = match state {
            JobState::Running => theme.text,
            JobState::Starting | JobState::Next(_) => theme.text_muted,
            JobState::Manual | JobState::Unplanned | JobState::Off => theme.text_faint,
        };
        let mark = match state {
            JobState::Running => super::super::render::status_mark(
                ChatIndicator::Working,
                self.pulse_ticker.entity_id(),
                theme,
            ),
            JobState::Starting => None,
            _ if job.failed() => Some(
                icon(icons::DANGER_TRIANGLE)
                    .size(px(11.0))
                    .text_color(theme.danger)
                    .into_any_element(),
            ),
            _ => None,
        };
        let item = job.item();
        let dim = !job.enabled && !matches!(state, JobState::Running | JobState::Starting);
        div()
            .id(SharedString::from(key.clone()))
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .cursor_pointer()
            .bg(if selected {
                crate::kit::theme::wash(0.08)
            } else {
                motion::hover_blend(
                    &key,
                    crate::kit::theme::wash(0.0),
                    crate::kit::theme::wash(0.05),
                )
            })
            .on_hover(motion::hover_listener(key.clone()))
            .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
            .child(
                icon(glyph)
                    .size(px(14.0))
                    .flex_none()
                    .text_color(tint)
                    .when(dim, |el| el.opacity(0.5)),
            )
            .child(
                two_lines(
                    job.name.clone(),
                    job_sub(job, space.map(|s| s.display_name())),
                    String::new(),
                    theme,
                )
                .when(dim, |el| el.opacity(0.55)),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.0))
                    .children(mark)
                    .child(parts::row_note(
                        state_note(state, &Local::now()),
                        note_color,
                    )),
            )
    }

    /// The sidebar card: every job by next run, or the one-line way to
    /// make the first routine.
    pub(super) fn render_schedule_card(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let jobs = self.schedule_jobs(bot, cx);
        if jobs.is_empty() {
            return Some(parts::empty_card(
                "bot-schedule-empty",
                icons::CLOCK,
                "Schedule",
                "No routines yet",
                "New routine",
                PanelItem::Schedule,
                |this, window, cx| this.open_job_form(FormTarget::NewRoutine, Some(window), cx),
                theme,
                cx,
            ));
        }
        let open = self.board.panel.clone();
        let tint = bot_tint(bot, theme);
        let header = section_header(
            "bot-schedule-header",
            icons::CLOCK,
            "Schedule",
            jobs.len(),
            open == Some(PanelItem::Schedule),
            theme,
        )
        .on_click(cx.listener(|this, _, _, cx| this.open_panel(PanelItem::Schedule, cx)));
        let rows: Vec<_> = jobs
            .iter()
            .take(parts::CARD_ROWS)
            .map(|(job, space)| {
                let selected = open.as_ref() == Some(&job.item());
                self.job_row(
                    format!("bot-job-{}", job.id),
                    job,
                    space.as_ref(),
                    tint,
                    selected,
                    theme,
                    cx,
                )
            })
            .collect();
        Some(
            card(theme)
                .pb(px(4.0))
                .child(header)
                .child(div().px(px(4.0)).flex().flex_col().children(rows))
                .into_any_element(),
        )
    }

    /// The detail panel for this card's items; `None` when the item no
    /// longer resolves.
    pub(super) fn render_schedule_panel(
        &mut self,
        bot: &Bot,
        item: &PanelItem,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        match item {
            PanelItem::Schedule => Some(self.render_schedule_list(bot, theme, cx)),
            PanelItem::NewRoutine => {
                Some(self.render_job_form(bot, FormTarget::NewRoutine, theme, cx))
            }
            PanelItem::NewTask => Some(self.render_job_form(bot, FormTarget::NewTask, theme, cx)),
            PanelItem::Routine(id) => {
                let target = FormTarget::Routine(id.clone());
                if self.form_open(&target) {
                    return Some(self.render_job_form(bot, target, theme, cx));
                }
                self.render_routine(bot, id, theme, cx)
                    .or_else(|| self.render_saving(id))
            }
            PanelItem::Task(id) => {
                let target = FormTarget::Task(id.clone());
                if self.form_open(&target) {
                    return Some(self.render_job_form(bot, target, theme, cx));
                }
                self.render_task(bot, id, theme, cx)
                    .or_else(|| self.render_saving(id))
            }
            _ => None,
        }
    }

    fn form_open(&self, target: &FormTarget) -> bool {
        self.board
            .schedule
            .form
            .as_ref()
            .is_some_and(|f| f.target == *target)
    }

    /// A just-saved item whose row is still on its way.
    fn render_saving(&self, id: &str) -> Option<(PanelHead, gpui::Div)> {
        let (saving, name) = self.board.schedule.saving.as_ref()?;
        (saving == id).then(|| {
            (
                PanelHead {
                    glyph: icons::CLOCK,
                    tint: None,
                    title: name.clone(),
                    subtitle: "Saving…".into(),
                },
                div().p(px(8.0)),
            )
        })
    }

    /// Every job, routines then project tasks, with the ways to add one.
    fn render_schedule_list(
        &mut self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (PanelHead, gpui::Div) {
        let jobs = self.schedule_jobs(bot, cx);
        let has_projects = !self.task_projects(bot, cx).is_empty();
        let tint = bot_tint(bot, theme);
        let (routines, tasks): (Vec<_>, Vec<_>) = jobs
            .iter()
            .partition(|(job, _)| job.kind == JobKind::Routine);
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        // Today's budget use across the bot's routines that are on.
        let budget = {
            let today = Local::now().format("%Y-%m-%d").to_string();
            let state = self.state.read(cx);
            let on: Vec<&BotRoutine> = state
                .bot_records
                .routines
                .iter()
                .filter(|r| r.bot_id == bot.id && r.enabled)
                .collect();
            (!on.is_empty()).then(|| {
                on.iter().fold((0, 0), |(used, budget), r| {
                    (
                        used + runs_today(r.runs_today, r.runs_day.as_deref(), &today),
                        budget + r.max_runs_per_day,
                    )
                })
            })
        };
        let head = PanelHead {
            glyph: icons::CLOCK,
            tint: None,
            title: "Schedule".into(),
            subtitle: {
                let mut line = format!(
                    "{} · {}",
                    plural(routines.len(), "routine"),
                    plural(tasks.len(), "project task")
                );
                if let Some((used, budget)) = budget {
                    line.push_str(&format!(" · {used} of {budget} routine runs today"));
                }
                line
            },
        };
        let actions = div()
            .px(px(4.0))
            .pt(px(2.0))
            .flex()
            .flex_row()
            .gap(px(4.0))
            .child(
                parts::action(
                    "bot-schedule-new-routine",
                    icons::PLUS,
                    "New routine",
                    theme.text,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_job_form(FormTarget::NewRoutine, Some(window), cx)
                })),
            )
            .when(has_projects, |el| {
                el.child(
                    parts::action("bot-schedule-new-task", icons::PLUS, "New task", theme.text)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_job_form(FormTarget::NewTask, Some(window), cx)
                        })),
                )
            });
        let mut body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .gap(px(1.0))
            .child(actions)
            .child(parts::section_label(theme, "Routines"));
        if routines.is_empty() {
            body = body.child(parts::quiet_line(
                theme,
                format!(
                    "A routine runs a prompt on a schedule, like a morning summary, \
                     and {} posts the result here in the chat.",
                    bot.name
                ),
            ));
        }
        for (job, space) in &routines {
            let row = self.job_row(
                format!("bot-panel-job-{}", job.id),
                job,
                space.as_ref(),
                tint,
                false,
                theme,
                cx,
            );
            body = body.child(row.py(px(8.0)));
        }
        body = body.child(parts::section_label(theme, "Project tasks"));
        if tasks.is_empty() {
            body = body.child(parts::quiet_line(
                theme,
                if has_projects {
                    "A recurring task starts a fresh session in a project each run, \
                     like nightly tests."
                } else {
                    "Give the bot a project to schedule tasks there."
                },
            ));
        }
        for (job, space) in &tasks {
            let row = self.job_row(
                format!("bot-panel-job-{}", job.id),
                job,
                space.as_ref(),
                tint,
                false,
                theme,
                cx,
            );
            body = body.child(row.py(px(8.0)));
        }
        (head, body)
    }

    // ── one routine or task ─────────────────────────────────────────────────

    fn render_routine(
        &mut self,
        bot: &Bot,
        id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let routine = {
            let state = self.state.read(cx);
            state
                .bot_records
                .routines
                .iter()
                .find(|r| r.id == id && r.bot_id == bot.id)
                .cloned()
        }?;
        if self
            .board
            .schedule
            .saving
            .as_ref()
            .is_some_and(|(saving, _)| saving == id)
        {
            self.board.schedule.saving = None;
        }
        let now = Local::now();
        let job = Job::routine(&routine, &now);
        let head = PanelHead {
            glyph: icons::CLOCK,
            tint: Some(bot_tint(bot, theme)),
            title: routine.name.clone(),
            subtitle: format!("Routine · {}", routine.schedule.describe()),
        };
        let today = now.format("%Y-%m-%d").to_string();
        let used = runs_today(routine.runs_today, routine.runs_day.as_deref(), &today);
        let thread = routine
            .last_run
            .as_ref()
            .and(routine.thread_chat_id.clone());
        let mut body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .child(self.job_actions(&job, theme, cx));
        if bot.paused {
            body = body.child(
                parts::quiet_line(
                    theme,
                    format!("{} is paused, so no routine runs.", bot.name),
                )
                .text_color(theme.warning),
            );
        }
        body = body
            .child(parts::section_label(theme, "Runs"))
            .child(parts::fact(theme, "Next run", next_value(&job, &now)))
            .child(parts::fact(
                theme,
                "Last run",
                self.last_run_value(routine.last_run.as_ref(), thread, "Open thread", theme, cx),
            ))
            .child(parts::fact(
                theme,
                "Today",
                SharedString::from(format!(
                    "{used} of {} run{}",
                    routine.max_runs_per_day,
                    if routine.max_runs_per_day == 1 {
                        ""
                    } else {
                        "s"
                    }
                )),
            ))
            .child(parts::section_label(theme, "Prompt"))
            .child(parts::well(theme, routine.prompt.clone()));
        let (enabled, write_notes) = (routine.clone(), routine.clone());
        body = body.child(parts::section_label(theme, "Settings")).child(
            parts::group_box()
                .child(
                    parts::switch_row(
                        "bot-routine-enabled",
                        "On schedule",
                        "Off keeps it, and Run now still works.",
                        routine.enabled,
                        true,
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let mut routine = enabled.clone();
                        routine.enabled = !routine.enabled;
                        this.save_routine_now(routine, cx);
                    })),
                )
                .child(parts::grant_row(
                    theme,
                    "Reads the notebook",
                    "Always, at the start of each run.",
                    false,
                ))
                .child(
                    parts::switch_row(
                        "bot-routine-notes",
                        "May write notes",
                        "Lets runs add to the notebook.",
                        routine.write_notes,
                        false,
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let mut routine = write_notes.clone();
                        routine.write_notes = !routine.write_notes;
                        this.save_routine_now(routine, cx);
                    })),
                ),
        );
        Some((head, body))
    }

    fn render_task(
        &mut self,
        bot: &Bot,
        id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let (task, space, device) = {
            let state = self.state.read(cx);
            let task = state
                .bot_records
                .tasks
                .iter()
                .find(|t| t.id == id && bot.scope.sees_space(&t.space_id))
                .cloned()?;
            let space = state.space_row(&task.space_id)?.clone();
            let device = state
                .device_name(&space.device_id)
                .unwrap_or("Unknown device")
                .to_string();
            (task, space, device)
        };
        if self
            .board
            .schedule
            .saving
            .as_ref()
            .is_some_and(|(saving, _)| saving == id)
        {
            self.board.schedule.saving = None;
        }
        let now = Local::now();
        let job = Job::task(&task, &now);
        let head = PanelHead {
            glyph: crate::appearance::space_style::space_icon(space.icon.as_deref()),
            tint: crate::appearance::space_style::space_color(space.color.as_deref(), theme),
            title: task.name.clone(),
            subtitle: format!("{} · {}", space.display_name(), task.schedule.describe()),
        };
        let session = task.last_run.as_ref().and_then(|run| run.chat_id.clone());
        let mut body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .child(self.job_actions(&job, theme, cx))
            .child(parts::section_label(theme, "Runs"))
            .child(parts::fact(
                theme,
                "Project",
                SharedString::from(format!("{} · {device}", space.display_name())),
            ))
            .child(parts::fact(theme, "Next run", next_value(&job, &now)))
            .child(parts::fact(
                theme,
                "Last run",
                self.last_run_value(task.last_run.as_ref(), session, "Open session", theme, cx),
            ));
        if let Some(model) = &task.model {
            body = body.child(parts::fact(
                theme,
                "Model",
                SharedString::from(model.clone()),
            ));
        }
        if task.created_by.as_deref() == Some(bot.id.as_str()) {
            body = body.child(parts::fact(
                theme,
                "Made by",
                SharedString::from(bot.name.clone()),
            ));
        }
        let (enabled, worktree) = (task.clone(), task.clone());
        body = body
            .child(parts::section_label(theme, "Prompt"))
            .child(parts::well(theme, task.prompt.clone()))
            .child(parts::section_label(theme, "Settings"))
            .child(
                parts::group_box()
                    .child(
                        parts::switch_row(
                            "bot-task-enabled",
                            "On schedule",
                            "Off keeps it, and Run now still works.",
                            task.enabled,
                            true,
                            theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mut task = enabled.clone();
                            task.enabled = !task.enabled;
                            this.save_task_now(task, cx);
                        })),
                    )
                    .child(
                        parts::switch_row(
                            "bot-task-worktree",
                            "New worktree each run",
                            "Runs never touch your checkout.",
                            task.worktree,
                            false,
                            theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mut task = worktree.clone();
                            task.worktree = !task.worktree;
                            this.save_task_now(task, cx);
                        })),
                    ),
            );
        Some((head, body))
    }

    /// Run now, Edit and Delete.
    fn job_actions(&self, job: &Job, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let busy = matches!(job.state(), JobState::Running | JobState::Starting);
        let (run_id, edit_id, delete_id) = (job.id.clone(), job.id.clone(), job.id.clone());
        let routine = job.kind == JobKind::Routine;
        let name = job.name.clone();
        // The palette's primary button, led by a play glyph.
        let run = div()
            .id("bot-job-run")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pl(px(10.0))
            .pr(px(12.0))
            .py(px(5.0))
            .rounded(px(8.0))
            .bg(theme.text)
            .text_size(px(13.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.on_solid)
            .cursor_pointer()
            .hover(|s| s.opacity(0.9))
            .when(!busy, |el| {
                el.child(icon(icons::PLAY).size(px(11.0)).text_color(theme.on_solid))
            })
            .child(SharedString::from(match job.state() {
                JobState::Running => "Running…",
                JobState::Starting => "Starting…",
                _ => "Run now",
            }))
            .when(busy, |el| el.opacity(0.55).cursor_default())
            .when(!busy, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| {
                    let op = if routine {
                        serde_json::json!({ "op": "runBotRoutine", "routineId": run_id })
                    } else {
                        serde_json::json!({ "op": "runProjectTask", "taskId": run_id })
                    };
                    this.board_mutate(op, cx);
                }))
            });
        div()
            .px(px(8.0))
            .pt(px(4.0))
            .pb(px(2.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .child(run)
            .child(div().w(px(4.0)))
            .child(
                parts::action("bot-job-edit", icons::PEN, "Edit", theme.text_muted).on_click(
                    cx.listener(move |this, _, window, cx| {
                        let target = if routine {
                            FormTarget::Routine(edit_id.clone())
                        } else {
                            FormTarget::Task(edit_id.clone())
                        };
                        this.open_job_form(target, Some(window), cx);
                    }),
                ),
            )
            .child(div().flex_1())
            .child(
                parts::action(
                    "bot-job-delete",
                    icons::TRASH_BIN_MINIMALISTIC,
                    "Delete",
                    theme.danger,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let (id, name) = (delete_id.clone(), name.clone());
                    this.ask_confirm(
                        if routine {
                            parts::Confirm::DeleteRoutine { id, name }
                        } else {
                            parts::Confirm::DeleteTask { id, name }
                        },
                        cx,
                    );
                })),
            )
    }

    /// The last run: its outcome and time, a link to where it ran, and
    /// its summary.
    fn last_run_value(
        &self,
        run: Option<&RunRecord>,
        chat_id: Option<String>,
        link_label: &'static str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(run) = run else {
            return div()
                .text_color(theme.text_muted)
                .child(SharedString::from("Not run yet"))
                .into_any_element();
        };
        let (word, glyph, color) = match run.outcome {
            RunOutcome::Running => ("Running", None, theme.text),
            RunOutcome::Done => ("Done", Some(icons::CHECK), theme.success),
            RunOutcome::Failed => ("Failed", Some(icons::DANGER_TRIANGLE), theme.danger),
            RunOutcome::Skipped => ("Skipped", None, theme.text_muted),
        };
        let mark = match run.outcome {
            RunOutcome::Running => super::super::render::status_mark(
                ChatIndicator::Working,
                self.pulse_ticker.entity_id(),
                theme,
            ),
            _ => glyph.map(|g| icon(g).size(px(12.0)).text_color(color).into_any_element()),
        };
        let line = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(5.0))
            .children(mark)
            .child(
                div()
                    .text_color(if run.outcome == RunOutcome::Failed {
                        theme.danger
                    } else {
                        theme.text
                    })
                    .child(SharedString::from(word)),
            )
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!(
                        "· {}",
                        parts::past_full_ms(run.at)
                    ))),
            )
            .when_some(chat_id, |el, chat_id| {
                el.child(div().text_color(theme.text_faint).child("·"))
                    .child(
                        parts::link("bot-job-last-link", link_label, theme).on_click(
                            cx.listener(move |this, _, _, cx| this.open_chat(chat_id.clone(), cx)),
                        ),
                    )
            });
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(line)
            .when_some(run.summary.clone(), |el, summary| {
                el.child(
                    div()
                        .text_size(px(12.0))
                        .line_height(px(17.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(summary)),
                )
            })
            .into_any_element()
    }

    fn save_routine_now(&mut self, routine: BotRoutine, cx: &mut Context<Self>) {
        self.board_mutate(
            serde_json::json!({ "op": "saveBotRoutine", "routine": routine }),
            cx,
        );
    }

    fn save_task_now(&mut self, task: ProjectTask, cx: &mut Context<Self>) {
        self.board_mutate(
            serde_json::json!({ "op": "saveProjectTask", "task": task }),
            cx,
        );
    }

    // ── forms ───────────────────────────────────────────────────────────────

    /// Open `target`'s form in the panel. A new-item form keeps its draft
    /// while it stays unsaved; an edit form starts from the record.
    pub(super) fn open_job_form(
        &mut self,
        target: FormTarget,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if !self.form_open(&target) {
            let Some(form) = self.new_job_form(target.clone(), cx) else {
                return;
            };
            self.board.schedule.form = Some(form);
        }
        if let (Some(window), Some(form)) = (window, self.board.schedule.form.as_ref()) {
            window.focus(&form.name.focus_handle(cx), cx);
        }
        let item = match &target {
            FormTarget::NewRoutine => PanelItem::NewRoutine,
            FormTarget::NewTask => PanelItem::NewTask,
            FormTarget::Routine(id) => PanelItem::Routine(id.clone()),
            FormTarget::Task(id) => PanelItem::Task(id.clone()),
        };
        self.show_panel(item, cx);
        cx.notify();
    }

    fn new_job_form(&mut self, target: FormTarget, cx: &mut Context<Self>) -> Option<JobForm> {
        let bot = {
            let state = self.state.read(cx);
            current_bot(&state.bots, self.settings.botboard_bot.as_deref()).cloned()
        }?;
        struct Seed {
            name: String,
            prompt: String,
            schedule: Schedule,
            write_notes: bool,
            worktree: bool,
            space_id: Option<String>,
        }
        let seed = {
            let state = self.state.read(cx);
            let records = &state.bot_records;
            match &target {
                FormTarget::NewRoutine => Seed {
                    name: String::new(),
                    prompt: String::new(),
                    schedule: Schedule::default(),
                    write_notes: false,
                    worktree: true,
                    space_id: None,
                },
                FormTarget::NewTask => Seed {
                    name: String::new(),
                    prompt: String::new(),
                    schedule: Schedule::default(),
                    write_notes: false,
                    worktree: true,
                    space_id: state
                        .spaces_sorted()
                        .into_iter()
                        .find(|s| bot.scope.sees_space(&s.id))
                        .map(|s| s.id.clone()),
                },
                FormTarget::Routine(id) => {
                    let r = records.routines.iter().find(|r| r.id == *id)?;
                    Seed {
                        name: r.name.clone(),
                        prompt: r.prompt.clone(),
                        schedule: r.schedule.clone(),
                        write_notes: r.write_notes,
                        worktree: true,
                        space_id: None,
                    }
                }
                FormTarget::Task(id) => {
                    let t = records.tasks.iter().find(|t| t.id == *id)?;
                    Seed {
                        name: t.name.clone(),
                        prompt: t.prompt.clone(),
                        schedule: t.schedule.clone(),
                        write_notes: false,
                        worktree: t.worktree,
                        space_id: Some(t.space_id.clone()),
                    }
                }
            }
        };
        let routine = target.routine();
        let name = cx.new(|cx| {
            TextInput::settings_field(
                if routine {
                    "Morning summary"
                } else {
                    "Nightly tests"
                },
                false,
                cx,
            )
        });
        let prompt = cx.new(|cx| {
            TextInput::settings_prompt_field(
                if routine {
                    "What moved, what's stuck, and what needs me?"
                } else {
                    "Run the tests and fix anything flaky."
                },
                cx,
            )
        });
        let time = cx.new(|cx| TextInput::settings_field("09:00", false, cx));
        let every = cx.new(|cx| TextInput::settings_field("2", false, cx));
        let (draft, time_text, every_text) = ScheduleDraft::from_schedule(&seed.schedule);
        name.update(cx, |input, cx| input.set_text(seed.name, cx));
        prompt.update(cx, |input, cx| input.set_text(seed.prompt, cx));
        time.update(cx, |input, cx| input.set_text(time_text, cx));
        every.update(cx, |input, cx| input.set_text(every_text, cx));
        let submit = |this: &mut Shell,
                      _: Entity<TextInput>,
                      event: &TextInputEvent,
                      cx: &mut Context<Shell>| {
            if matches!(event, TextInputEvent::Submitted) {
                this.save_job_form(cx);
            }
        };
        let events = vec![
            cx.subscribe(&name, submit),
            cx.subscribe(&time, submit),
            cx.subscribe(&every, submit),
        ];
        Some(JobForm {
            target,
            bot_id: bot.id,
            name,
            prompt,
            time,
            every,
            draft,
            write_notes: seed.write_notes,
            worktree: seed.worktree,
            space_id: seed.space_id,
            error: None,
            busy: false,
            _events: events,
        })
    }

    fn close_job_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.board.schedule.form.take() else {
            return;
        };
        match form.target {
            // An edit goes back to the item; a new one back to the list.
            FormTarget::Routine(_) | FormTarget::Task(_) => {}
            FormTarget::NewRoutine | FormTarget::NewTask => {
                self.show_panel(PanelItem::Schedule, cx)
            }
        }
        cx.notify();
    }

    fn save_job_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.board.schedule.form.as_ref() else {
            return;
        };
        if form.busy {
            return;
        }
        let name = form.name.read(cx).text().trim().to_string();
        let prompt = form.prompt.read(cx).text().trim().to_string();
        let built = form
            .draft
            .build(form.time.read(cx).text(), form.every.read(cx).text());
        let now = cypher_proto::now_ms();
        let records = self.state.read(cx).bot_records.clone();
        let op = built.and_then(|schedule| match &form.target {
            FormTarget::NewRoutine | FormTarget::Routine(_) => {
                let existing = match &form.target {
                    FormTarget::Routine(id) => records.routines.iter().find(|r| r.id == *id),
                    _ => None,
                };
                let routine = BotRoutine {
                    id: existing.map_or_else(|| uuid::Uuid::new_v4().to_string(), |r| r.id.clone()),
                    bot_id: form.bot_id.clone(),
                    name: name.clone(),
                    prompt,
                    schedule,
                    enabled: existing.is_none_or(|r| r.enabled),
                    write_notes: form.write_notes,
                    max_runs_per_day: existing.map_or(24, |r| r.max_runs_per_day),
                    created_at: existing.map_or(now, |r| r.created_at),
                    updated_at: now,
                    run_requested_at: None,
                    thread_chat_id: None,
                    last_run: None,
                    next_run_at: None,
                    runs_today: 0,
                    runs_day: None,
                };
                routine.validate()?;
                Ok((
                    routine.id.clone(),
                    PanelItem::Routine(routine.id.clone()),
                    serde_json::json!({ "op": "saveBotRoutine", "routine": routine }),
                ))
            }
            FormTarget::NewTask | FormTarget::Task(_) => {
                let existing = match &form.target {
                    FormTarget::Task(id) => records.tasks.iter().find(|t| t.id == *id),
                    _ => None,
                };
                let space_id = form
                    .space_id
                    .clone()
                    .ok_or_else(|| "Pick the project it runs in.".to_string())?;
                let task = ProjectTask {
                    id: existing.map_or_else(|| uuid::Uuid::new_v4().to_string(), |t| t.id.clone()),
                    space_id,
                    name: name.clone(),
                    prompt,
                    schedule,
                    enabled: existing.is_none_or(|t| t.enabled),
                    worktree: form.worktree,
                    model: existing.and_then(|t| t.model.clone()),
                    created_by: existing.and_then(|t| t.created_by.clone()),
                    created_at: existing.map_or(now, |t| t.created_at),
                    updated_at: now,
                    run_requested_at: None,
                    last_run: None,
                    next_run_at: None,
                };
                task.validate()?;
                Ok((
                    task.id.clone(),
                    PanelItem::Task(task.id.clone()),
                    serde_json::json!({ "op": "saveProjectTask", "task": task }),
                ))
            }
        });
        let form = self.board.schedule.form.as_mut().expect("checked above");
        let (id, item, op) = match op {
            Ok(op) => op,
            Err(message) => {
                form.error = Some(message.into());
                return cx.notify();
            }
        };
        form.busy = true;
        form.error = None;
        cx.notify();
        self.board_call(
            methods::MUTATE,
            op,
            cx,
            move |this, result, cx| match result {
                Ok(_) => {
                    this.board.schedule.form = None;
                    this.board.schedule.saving = Some((id, name));
                    this.show_panel(item, cx);
                }
                Err(err) => {
                    if let Some(form) = this.board.schedule.form.as_mut() {
                        form.busy = false;
                        form.error = Some(
                            match err {
                                CallError::Unknown => {
                                    "This bot's device can't save routines yet.".to_string()
                                }
                                CallError::Failed(text) => text,
                            }
                            .into(),
                        );
                    }
                }
            },
        );
    }

    fn render_job_form(
        &mut self,
        bot: &Bot,
        target: FormTarget,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (PanelHead, gpui::Div) {
        if !self.form_open(&target) {
            // A panel opened some other way (or restored): a fresh form.
            if let Some(form) = self.new_job_form(target.clone(), cx) {
                self.board.schedule.form = Some(form);
            }
        }
        let projects = self.task_projects(bot, cx);
        let routine = target.routine();
        let head = PanelHead {
            glyph: icons::CLOCK,
            tint: routine.then(|| bot_tint(bot, theme)),
            title: match &target {
                FormTarget::NewRoutine => "New routine",
                FormTarget::NewTask => "New project task",
                FormTarget::Routine(_) => "Edit routine",
                FormTarget::Task(_) => "Edit project task",
            }
            .into(),
            subtitle: if routine {
                format!("Runs in its own thread; {} posts the result", bot.name)
            } else {
                "Each run starts a fresh session in the project".into()
            },
        };
        let Some(form) = self.board.schedule.form.as_ref() else {
            return (head, div());
        };
        let time_text = form.time.read(cx).text().to_string();
        let every_text = form.every.read(cx).text().to_string();
        let preview = form.draft.build(&time_text, &every_text);
        let mut body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .child(parts::field_label(theme, "Name"))
            .child(parts::field(&form.name))
            .child(parts::field_label(
                theme,
                if routine {
                    "What it does each run"
                } else {
                    "What each session is asked"
                },
            ))
            .child(parts::field(&form.prompt));

        // ── the schedule editor ──
        body = body
            .child(parts::field_label(theme, "When"))
            .child(div().mx(px(8.0)).child(parts::segmented(
                "bot-form-kind",
                &[
                    (ScheduleKind::Daily, "Daily"),
                    (ScheduleKind::Every, "Every"),
                    (ScheduleKind::Manual, "Manual"),
                ],
                form.draft.kind,
                |this, kind, cx| {
                    if let Some(form) = this.board.schedule.form.as_mut() {
                        form.draft.kind = kind;
                        form.error = None;
                    }
                    cx.notify();
                },
                theme,
                cx,
            )));
        let when = match form.draft.kind {
            ScheduleKind::Daily => {
                let chips = (1..=7u8).map(|day| {
                    let on = form.draft.days.contains(&day);
                    div()
                        .id(SharedString::from(format!("bot-form-day-{day}")))
                        .flex_1()
                        .h(px(24.0))
                        .rounded(px(6.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(11.5))
                        .cursor_pointer()
                        .text_color(if on { theme.text } else { theme.text_muted })
                        .when(on, |el| {
                            el.bg(crate::kit::theme::card_selected_bg())
                                .shadow(crate::kit::theme::card_selected_shadows())
                                .font_weight(gpui::FontWeight::MEDIUM)
                        })
                        .when(!on, |el| el.hover(|s| s.text_color(theme.text)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(form) = this.board.schedule.form.as_mut() {
                                form.draft.toggle_day(day);
                            }
                            cx.notify();
                        }))
                        .child(SharedString::from(
                            ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][day as usize - 1],
                        ))
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(theme.text_muted)
                                    .child("At"),
                            )
                            .child(parts::field(&form.time).mx(px(0.0)).w(px(76.0))),
                    )
                    // Day chips sit in a segmented control's track; any
                    // number may be on.
                    .child(
                        div()
                            .p(px(2.0))
                            .rounded(px(8.0))
                            .bg(crate::kit::theme::ink(0.05))
                            .flex()
                            .flex_row()
                            .gap(px(2.0))
                            .children(chips),
                    )
            }
            ScheduleKind::Every => div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text_muted)
                        .child("Every"),
                )
                .child(parts::field(&form.every).mx(px(0.0)).w(px(64.0)))
                .child(div().w(px(150.0)).child(parts::segmented(
                    "bot-form-unit",
                    &[(Unit::Hours, "hours"), (Unit::Minutes, "minutes")],
                    form.draft.unit,
                    |this, unit, cx| {
                        if let Some(form) = this.board.schedule.form.as_mut() {
                            form.draft.unit = unit;
                        }
                        cx.notify();
                    },
                    theme,
                    cx,
                ))),
            ScheduleKind::Manual => div(),
        };
        // One line under the editor, always there so nothing jumps: when
        // it next runs, or what to fix.
        let (hint, hint_color) = match &preview {
            Ok(Schedule::Manual) => (
                "Runs only when you press Run now.".to_string(),
                theme.text_faint,
            ),
            Ok(schedule) => {
                let now = Local::now();
                let next = schedule
                    .next_after(&now)
                    .map(|at| format!(" · next run {}", lower_first(&parts::next_full(&at, &now))))
                    .unwrap_or_default();
                (format!("{}{next}", schedule.describe()), theme.text_faint)
            }
            Err(problem) => (problem.clone(), theme.danger.opacity(0.85)),
        };
        body = body.child(
            div()
                .mx(px(8.0))
                .mt(px(10.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(when)
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(hint_color)
                        .child(SharedString::from(hint)),
                ),
        );

        // ── what the job may do, and where it runs ──
        if routine {
            body = body.child(parts::field_label(theme, "Permissions")).child(
                parts::group_box()
                    .child(parts::grant_row(
                        theme,
                        "Reads what the bot reads",
                        "Its projects, and the notebook.",
                        true,
                    ))
                    .child(
                        parts::switch_row(
                            "bot-form-notes",
                            "May write notes",
                            "Lets runs add to the notebook.",
                            form.write_notes,
                            false,
                            theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(form) = this.board.schedule.form.as_mut() {
                                form.write_notes = !form.write_notes;
                            }
                            cx.notify();
                        })),
                    ),
            );
        } else {
            let rows = projects.iter().map(|(space, device)| {
                let on = form.space_id.as_deref() == Some(space.id.as_str());
                let pick = space.id.clone();
                let glyph = crate::appearance::space_style::space_icon(space.icon.as_deref());
                let tint =
                    crate::appearance::space_style::space_color(space.color.as_deref(), theme)
                        .unwrap_or(theme.text_muted);
                div()
                    .id(SharedString::from(format!("bot-form-project-{}", space.id)))
                    .h(px(30.0))
                    .px(px(10.0))
                    .rounded(px(7.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(9.0))
                    .cursor_pointer()
                    .when(on, |el| {
                        el.bg(crate::kit::theme::card_selected_bg())
                            .shadow(crate::kit::theme::card_selected_shadows())
                    })
                    .when(!on, |el| el.hover(|s| s.bg(theme.element_hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(form) = this.board.schedule.form.as_mut() {
                            form.space_id = Some(pick.clone());
                        }
                        cx.notify();
                    }))
                    .child(icon(glyph).size(px(14.0)).text_color(tint))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(theme.text)
                            .child(SharedString::from(space.display_name().to_string())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(device.clone())),
                    )
                    .when(on, |el| {
                        el.child(icon(icons::CHECK).size(px(12.0)).text_color(theme.text))
                    })
            });
            body = body
                .child(parts::field_label(theme, "Project"))
                .child(
                    div()
                        .mx(px(8.0))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .children(rows)
                        .when(projects.is_empty(), |el| {
                            el.child(parts::quiet_line(
                                theme,
                                format!("{} has no projects yet.", bot.name),
                            ))
                        }),
                )
                .child(div().h(px(10.0)))
                .child(
                    parts::group_box().child(
                        parts::switch_row(
                            "bot-form-worktree",
                            "New worktree each run",
                            "Runs never touch your checkout.",
                            form.worktree,
                            true,
                            theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(form) = this.board.schedule.form.as_mut() {
                                form.worktree = !form.worktree;
                            }
                            cx.notify();
                        })),
                    ),
                );
        }

        let new = matches!(target, FormTarget::NewRoutine | FormTarget::NewTask);
        let (footer, cancel, save) = parts::form_footer(
            theme,
            "bot-form",
            form.error.clone(),
            match (new, form.busy) {
                (_, true) => "Saving…",
                (true, false) if routine => "Create routine",
                (true, false) => "Create task",
                (false, false) => "Save",
            },
            form.busy,
        );
        body = body.child(
            footer
                .child(cancel.on_click(cx.listener(|this, _, _, cx| this.close_job_form(cx))))
                .child(save.on_click(cx.listener(|this, _, _, cx| this.save_job_form(cx)))),
        );
        (head, body)
    }
}

/// A Next run fact's words: the next scheduled run even while one runs.
fn next_value(job: &Job, now: &DateTime<Local>) -> SharedString {
    match (job.next.and_then(parts::local), job.state()) {
        (Some(at), _) => parts::next_full(&at, now).into(),
        (None, _) if !job.enabled => "Off".into(),
        (None, _) if job.schedule == Schedule::Manual => "Only when run by hand".into(),
        (None, _) => "Not planned yet".into(),
    }
}

/// "Today 09:00" → "today 09:00", for mid-sentence use.
fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if text.starts_with("To") => first.to_lowercase().chain(chars).collect(),
        _ => text.to_string(),
    }
}

/// The bot's own colour (routines wear it), the muted ink without one.
fn bot_tint(bot: &Bot, theme: &Theme) -> gpui::Hsla {
    crate::appearance::space_style::space_color(bot.color.as_deref(), theme)
        .unwrap_or(theme.text_muted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    fn daily(time: &str, days: Vec<u8>) -> Schedule {
        Schedule::Daily {
            time: time.into(),
            days,
        }
    }

    fn job(id: &str, schedule: Schedule, enabled: bool, next: Option<i64>) -> Job {
        Job {
            kind: JobKind::Routine,
            id: id.into(),
            name: id.into(),
            schedule,
            enabled,
            next,
            last: None,
            run_requested_at: None,
        }
    }

    fn run(at: i64, outcome: RunOutcome) -> RunRecord {
        RunRecord {
            at,
            ended_at: None,
            outcome,
            summary: None,
            chat_id: None,
        }
    }

    #[test]
    fn next_runs_prefer_the_hosts_plan_while_it_is_ahead() {
        let now = at("2026-10-18T10:00:00+01:00");
        let nine = daily("09:00", vec![]);
        let tomorrow = at("2026-10-19T09:00:00+01:00").timestamp_millis();
        assert_eq!(next_run(&nine, true, None, &now), Some(tomorrow));
        let later = at("2026-10-18T12:00:00+01:00").timestamp_millis();
        assert_eq!(next_run(&nine, true, Some(later), &now), Some(later));
        let stale = at("2026-10-18T09:00:00+01:00").timestamp_millis();
        assert_eq!(
            next_run(&nine, true, Some(stale), &now),
            Some(tomorrow),
            "a plan in the past is recomputed"
        );
        assert_eq!(next_run(&nine, false, Some(later), &now), None);
        assert_eq!(next_run(&Schedule::Manual, true, None, &now), None);
    }

    #[test]
    fn jobs_order_by_next_run_with_manual_and_off_last() {
        let mut running = job("running", daily("09:00", vec![]), true, Some(900));
        running.last = Some(run(1, RunOutcome::Running));
        let mut starting = job("starting", Schedule::Manual, true, None);
        starting.last = Some(run(1, RunOutcome::Done));
        starting.run_requested_at = Some(2);
        let mut jobs = vec![
            job("off", daily("01:00", vec![]), false, None),
            job("manual", Schedule::Manual, true, None),
            job("late", daily("09:00", vec![]), true, Some(500)),
            running,
            job("soon", daily("09:00", vec![]), true, Some(100)),
            starting,
        ];
        order_jobs(&mut jobs);
        let ids: Vec<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(
            ids,
            ["running", "starting", "soon", "late", "manual", "off"]
        );
        // A request older than the last run has been served.
        let mut served = job("served", Schedule::Manual, true, None);
        served.run_requested_at = Some(1);
        served.last = Some(run(5, RunOutcome::Failed));
        assert_eq!(served.state(), JobState::Manual);
        assert!(served.failed());
    }

    #[test]
    fn rows_name_their_trigger_and_next_run() {
        let routine = job("r", daily("09:00", vec![]), true, None);
        assert_eq!(job_sub(&routine, None), "Routine · Daily 09:00");
        let task = Job {
            kind: JobKind::Task {
                space_id: "web".into(),
            },
            ..job("t", daily("18:30", vec![1, 2, 3, 4, 5]), true, None)
        };
        assert_eq!(job_sub(&task, Some("Web")), "Web · Weekdays 18:30");
        let now = at("2026-10-18T10:00:00+01:00");
        let next = at("2026-10-19T02:00:00+01:00").timestamp_millis();
        assert_eq!(state_note(JobState::Next(next), &now), "02:00");
        assert_eq!(state_note(JobState::Off, &now), "off");
        assert_eq!(state_note(JobState::Manual, &now), "manual");
        assert_eq!(runs_today(3, Some("2026-10-18"), "2026-10-18"), 3);
        assert_eq!(runs_today(3, Some("2026-10-17"), "2026-10-18"), 0);
    }

    #[test]
    fn the_schedule_editor_round_trips() {
        for schedule in [
            daily("09:00", vec![]),
            daily("18:30", vec![1, 2, 3, 4, 5]),
            daily("07:15", vec![6, 7]),
            Schedule::Interval { minutes: 120 },
            Schedule::Interval { minutes: 45 },
            Schedule::Manual,
        ] {
            let (draft, time, every) = ScheduleDraft::from_schedule(&schedule);
            assert_eq!(draft.build(&time, &every), Ok(schedule.clone()));
        }
        let (draft, _, every) = ScheduleDraft::from_schedule(&Schedule::Interval { minutes: 45 });
        assert_eq!((draft.unit, every.as_str()), (Unit::Minutes, "45"));
        // Every day picked is the same as none picked.
        let all = ScheduleDraft {
            kind: ScheduleKind::Daily,
            days: (1..=7).collect(),
            unit: Unit::Hours,
        };
        assert_eq!(all.build("9:30", ""), Ok(daily("09:30", vec![])));
    }

    #[test]
    fn the_editor_says_what_to_fix() {
        let draft = |kind, unit| ScheduleDraft {
            kind,
            days: vec![],
            unit,
        };
        assert_eq!(
            draft(ScheduleKind::Daily, Unit::Hours).build("25:00", ""),
            Err("Pick a time like 09:00.".into())
        );
        assert_eq!(
            draft(ScheduleKind::Every, Unit::Minutes).build("", "5"),
            Err("Run at most every 15 minutes.".into())
        );
        assert_eq!(
            draft(ScheduleKind::Every, Unit::Hours).build("", "often"),
            Err("Say how often, like every 2 hours.".into())
        );
        assert_eq!(normalize_time("9"), "09:00");
        assert_eq!(normalize_time("9:05"), "09:05");
        assert_eq!(normalize_time("1830"), "18:30");
        assert_eq!(normalize_time(" 07:00 "), "07:00");
        assert_eq!(normalize_time("noon"), "noon");
    }

    #[test]
    fn forms_validate_through_the_records() {
        let routine = BotRoutine {
            id: "r".into(),
            bot_id: "b".into(),
            name: " ".into(),
            prompt: "Summarize".into(),
            schedule: Schedule::default(),
            enabled: true,
            write_notes: false,
            max_runs_per_day: 24,
            created_at: 0,
            updated_at: 0,
            run_requested_at: None,
            thread_chat_id: None,
            last_run: None,
            next_run_at: None,
            runs_today: 0,
            runs_day: None,
        };
        assert_eq!(routine.validate(), Err("Name the routine.".into()));
    }
}
