//! The **Routines** card (§13.1): the bot's routines by next run, their
//! panels and the routine form.
//!
//! A routine runs by hand or on a schedule, either in the workspace (the
//! bot's own agent, in a hidden thread) or as an ordinary session in one of
//! its projects (§8). Scheduled runs continue one standing session; each Run
//! now gets a fresh one, kept until the next. The definition fields are the
//! owner's and go out whole in one Mutate; the run state (last run, next
//! run, runs today, its sessions) is the host's and only read here.

use super::board::{card_corner, card_glyph, card_item, two_lines};
use super::panel::{PanelHead, PanelItem};
use super::parts::{self, CallError};
use super::*;
use chrono::{DateTime, Local, TimeZone};
use cypher_proto::{BotRoutine, RoutineTarget, RunOutcome, RunRecord, Schedule, Space};

/// Each kind of routine wears its own mark, apart from the card's clock: one
/// in the workspace repeats in the bot's colour, one in a project is code
/// work in the project's colour.
const ROUTINE_GLYPH: &str = icons::REFRESH;
const TASK_GLYPH: &str = icons::CODE;

// ── the rows ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum JobKind {
    /// Runs as the bot, in its hidden thread.
    Workspace,
    /// Runs as an ordinary session in this project.
    Project { space_id: String },
}

/// One row of the Routines card and panel.
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
            kind: match &r.target {
                RoutineTarget::Workspace => JobKind::Workspace,
                RoutineTarget::Project { space_id, .. } => JobKind::Project {
                    space_id: space_id.clone(),
                },
            },
            id: r.id.clone(),
            name: r.name.clone(),
            schedule: r.schedule.clone(),
            enabled: r.enabled,
            next: next_run(&r.schedule, r.enabled, r.next_run_at, now),
            last: r.last_run.clone(),
            run_requested_at: r.run_requested_at,
        }
    }

    pub(super) fn item(&self) -> PanelItem {
        PanelItem::Routine(self.id.clone())
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

/// A row's sub-line, where it runs and when: "This workspace · Daily
/// 09:00", "<project> · Weekdays 18:30".
pub(super) fn job_sub(job: &Job, project: Option<&str>) -> String {
    let lead = match job.kind {
        JobKind::Workspace => "This workspace",
        JobKind::Project { .. } => project.unwrap_or("A project"),
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

    /// Whether `day` (1 = Monday) is picked; none picked means every day.
    pub(super) fn has_day(&self, day: u8) -> bool {
        self.days.is_empty() || self.days.contains(&day)
    }

    /// Flip one day. Every day (none picked) drops just this one; the last
    /// day left stays, since a daily routine runs on at least one.
    pub(super) fn toggle_day(&mut self, day: u8) {
        if self.days.is_empty() {
            self.days = (1..=7).filter(|d| *d != day).collect();
        } else if let Some(ix) = self.days.iter().position(|d| *d == day) {
            if self.days.len() > 1 {
                self.days.remove(ix);
            }
        } else {
            self.days.push(day);
            self.days.sort_unstable();
            if self.days.len() == 7 {
                self.days.clear();
            }
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
    Routine(String),
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
    work_directly: bool,
    /// Where it runs: a project, or `None` for the workspace.
    space_id: Option<String>,
    /// A project routine's own worktree rather than the project checkout.
    worktree: bool,
    /// The model its runs use; `None` follows the workspace chat.
    model: Option<String>,
    /// The model list: open, its filter, and the device's catalogue.
    model_open: bool,
    model_search: Entity<TextInput>,
    models: ModelList,
    /// The device the catalogue was asked of.
    models_device: Option<String>,
    error: Option<SharedString>,
    busy: bool,
    _events: Vec<Subscription>,
}

/// A device's model catalogue, as the form loads it.
#[derive(Debug, Clone, Default)]
pub(super) enum ModelList {
    #[default]
    Loading,
    Ready(Vec<cypher_proto::Model>),
    Failed(String),
}

/// The Routines panels' own state: the open form, and an item just saved
/// whose row has not landed yet.
#[derive(Default)]
pub(super) struct ScheduleUi {
    form: Option<JobForm>,
    saving: Option<(String, String)>,
    /// Model labels by id, from any device's list, so a routine page names
    /// its model as the pickers do; and the devices already asked.
    model_labels: std::collections::HashMap<String, String>,
    labels_asked: std::collections::HashSet<String>,
}

impl Shell {
    fn schedule_jobs(&self, bot: &Bot, cx: &App) -> Vec<(Job, Option<Space>)> {
        let now = Local::now();
        let state = self.state.read(cx);
        let mut jobs: Vec<Job> = state
            .bot_records
            .routines
            .iter()
            .filter(|r| r.bot_id == bot.id)
            .map(|r| Job::routine(r, &now))
            .collect();
        order_jobs(&mut jobs);
        jobs.into_iter()
            .map(|job| {
                let space = match &job.kind {
                    JobKind::Project { space_id } => state.space_row(space_id).cloned(),
                    JobKind::Workspace => None,
                };
                (job, space)
            })
            .collect()
    }

    /// The bot's projects a routine may run in, in sidebar order.
    fn routine_projects(&self, bot: &Bot, cx: &App) -> Vec<(Space, String)> {
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

    /// One routine row: its mark (the workspace's or the project's), name
    /// over where and when it runs, and the next run.
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
        let (glyph, tint) = job_mark(job, space, bot_tint, theme);
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

    /// A job on the card: its glyph, name and when it runs next (a spinner
    /// while it runs, the note in red when its last run failed).
    fn job_item(
        &self,
        job: &Job,
        space: Option<&Space>,
        bot_tint: gpui::Hsla,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (glyph, tint) = job_mark(job, space, bot_tint, theme);
        let state = job.state();
        let corner = match state {
            JobState::Running => super::super::render::status_mark(
                ChatIndicator::Working,
                self.pulse_ticker.entity_id(),
                theme,
            )
            .unwrap_or_else(|| card_corner("running", None, theme)),
            _ => card_corner(
                state_note(state, &Local::now()),
                job.failed().then_some(theme.danger),
                theme,
            ),
        };
        let dim = !job.enabled && !matches!(state, JobState::Running | JobState::Starting);
        let item = job.item();
        card_item(
            &format!("bot-job-{}", job.id),
            Some(card_glyph(glyph, tint.opacity(0.82))),
            job.name.clone(),
            Some(corner),
            selected,
            theme,
        )
        .when(dim, |el| el.opacity(0.55))
        .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
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
                "Routines",
                "No routines yet",
                PanelItem::Schedule,
                self.card_focused("schedule", cx),
                theme,
                cx,
            ));
        }
        let open = self.board.panel.clone();
        let tint = bot_tint(bot, theme);
        let header = self.card_header(
            "schedule",
            "bot-schedule-header",
            "Routines",
            jobs.len(),
            PanelItem::Schedule,
            None,
            theme,
            cx,
        );
        let rows: Vec<_> = jobs
            .iter()
            .take(parts::CARD_ROWS)
            .map(|(job, space)| {
                let selected = open.as_ref() == Some(&job.item());
                self.job_item(job, space.as_ref(), tint, selected, theme, cx)
            })
            .collect();
        Some(self.fold_card("schedule", header, rows, theme))
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
            PanelItem::Routine(id) => {
                let target = FormTarget::Routine(id.clone());
                if self.form_open(&target) {
                    return Some(self.render_job_form(bot, target, theme, cx));
                }
                self.render_routine(bot, id, theme, cx)
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
                },
                div().p(px(8.0)),
            )
        })
    }

    /// Every routine, with the way to add one.
    fn render_schedule_list(
        &mut self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (PanelHead, gpui::Div) {
        let jobs = self.schedule_jobs(bot, cx);
        let tint = bot_tint(bot, theme);
        let head = PanelHead {
            glyph: icons::CLOCK,
            tint: None,
            title: "Routines".into(),
        };
        // The header's one button, like the Notebook's: + adds a routine.
        self.set_panel_actions(vec![
            super::items::icon_button(
                "bot-schedule-new-routine",
                icons::PLUS,
                "New routine",
                theme,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_job_form(FormTarget::NewRoutine, Some(window), cx)
            }))
            .into_any_element(),
        ]);
        if jobs.is_empty() {
            self.board.panel_empty = Some(
                parts::panel_empty(
                    theme,
                    icons::CLOCK,
                    "No routines yet",
                    "Press + to run a prompt on a schedule, like a morning brief or a nightly \
                     test run.",
                )
                .into_any_element(),
            );
        }
        let mut body = div().p(px(8.0)).flex().flex_col().gap(px(1.0));
        for (job, space) in &jobs {
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

    // ── one routine ─────────────────────────────────────────────────────────

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
        let (space, device) = {
            let state = self.state.read(cx);
            let space = routine
                .target
                .space_id()
                .and_then(|id| state.space_row(id).cloned());
            let device = space.as_ref().map(|s| {
                state
                    .device_name(&s.device_id)
                    .unwrap_or("Unknown device")
                    .to_string()
            });
            (space, device)
        };
        let now = Local::now();
        let job = Job::routine(&routine, &now);
        let (glyph, tint) = job_mark(&job, space.as_ref(), bot_tint(bot, theme), theme);
        let head = PanelHead {
            glyph,
            tint: Some(tint),
            title: routine.name.clone(),
        };
        let mut body = div().p(px(8.0)).flex().flex_col();
        self.board.panel_actions = Some(
            self.job_actions(&routine, &job, theme, cx)
                .into_any_element(),
        );
        if bot.paused {
            body = body.child(
                parts::quiet_line(
                    theme,
                    format!("{} is paused, so no routine runs.", bot.name),
                )
                .text_color(theme.warning),
            );
        }
        // Where and when it runs, then what it last did.
        match &routine.target {
            RoutineTarget::Workspace => {
                body = body.child(parts::fact(
                    theme,
                    "Runs in",
                    SharedString::from("This workspace"),
                ));
            }
            RoutineTarget::Project { worktree, .. } => {
                body = body
                    .child(parts::fact(
                        theme,
                        "Runs in",
                        SharedString::from(format!(
                            "{} · {}",
                            space
                                .as_ref()
                                .map_or("A project it can't see", |s| s.display_name()),
                            device.as_deref().unwrap_or("Unknown device"),
                        )),
                    ))
                    .child(parts::fact(
                        theme,
                        "Checkout",
                        SharedString::from(if *worktree {
                            "Its own worktree"
                        } else {
                            "The project's checkout"
                        }),
                    ));
            }
        }
        body = body.child(parts::fact(
            theme,
            "Model",
            SharedString::from(match routine.chosen_model() {
                Some(model) => {
                    let host = space
                        .as_ref()
                        .map_or(bot.host_device_id.clone(), |s| s.device_id.clone());
                    self.model_label(Some(host), model, cx)
                }
                None => "Default".to_string(),
            }),
        ));
        body = body.child(parts::fact(
            theme,
            "Schedule",
            SharedString::from(schedule_value(&job, &now)),
        ));
        body = body.child(parts::fact(
            theme,
            "Prompt",
            super::items::text_value(&routine.prompt),
        ));
        if routine.last_run.is_some() {
            body = body.child(parts::fact(
                theme,
                "Last run",
                self.last_run_value(routine.last_run.as_ref(), theme),
            ));
        }
        Some((head, body))
    }

    /// The header's buttons: the Enabled switch (for a routine on a
    /// schedule), Run now, Edit, the last run's session and Delete. Its
    /// other settings live in the form.
    fn job_actions(
        &self,
        routine: &BotRoutine,
        job: &Job,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let state = job.state();
        let busy = matches!(state, JobState::Running | JobState::Starting);
        let (run_id, edit_id, delete_id) = (job.id.clone(), job.id.clone(), job.id.clone());
        let name = job.name.clone();
        let button = super::items::icon_button;
        let flip_tip = routine.enabled;
        let run = button(
            "bot-job-run",
            icons::PLAY,
            match state {
                JobState::Running => "Running…",
                JobState::Starting => "Starting…",
                _ => "Run now",
            },
            theme,
        )
        .when(busy, |el| el.opacity(0.45).cursor_default())
        .when(!busy, |el| {
            el.on_click(cx.listener(move |this, _, _, cx| {
                this.board_mutate(
                    serde_json::json!({ "op": "runBotRoutine", "routineId": run_id }),
                    cx,
                );
            }))
        });
        let edit = button("bot-job-edit", icons::PEN, "Edit", theme).on_click(cx.listener(
            move |this, _, window, cx| {
                this.open_job_form(FormTarget::Routine(edit_id.clone()), Some(window), cx);
            },
        ));
        // The session the latest run happened in, to read or watch.
        let session = job.last.as_ref().and_then(|run| {
            let chat_id = run.chat_id.clone()?;
            let tip = if run.outcome == RunOutcome::Running {
                "Watch the run"
            } else {
                "Open the last run's session"
            };
            Some(
                button("bot-job-session", icons::CHAT_ROUND_LINE, tip, theme).on_click(
                    cx.listener(move |this, _, _, cx| this.open_chat(chat_id.clone(), cx)),
                ),
            )
        });
        let delete = button(
            "bot-job-delete",
            icons::TRASH_BIN_MINIMALISTIC,
            "Delete",
            theme,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            let (id, name) = (delete_id.clone(), name.clone());
            this.ask_confirm(parts::Confirm::DeleteRoutine { id, name }, cx);
        }));
        // A manual routine has no schedule to switch off.
        let enabled = (routine.schedule != Schedule::Manual).then(|| {
            let flip = routine.clone();
            div()
                .id("bot-routine-enabled")
                .flex_none()
                .h(px(28.0))
                .px(px(6.0))
                .flex()
                .items_center()
                .cursor_pointer()
                .tooltip(move |_, cx| {
                    cx.new(|_| {
                        super::super::session::FindTooltip(
                            if flip_tip {
                                "Enabled: runs on its schedule"
                            } else {
                                "Disabled: runs only when you press Run now"
                            }
                            .into(),
                        )
                    })
                    .into()
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let mut routine = flip.clone();
                    routine.enabled = !routine.enabled;
                    this.save_routine_now(routine, cx);
                }))
                .child(crate::settings::widgets::toggle_switch(
                    theme,
                    routine.enabled,
                ))
        });
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .children(enabled)
            .child(run)
            .child(edit)
            .children(session)
            .child(delete)
    }

    /// The last run: its outcome and time (and whether it was run by
    /// hand), and its summary. Its session opens from the actions.
    fn last_run_value(&self, run: Option<&RunRecord>, theme: &Theme) -> AnyElement {
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
                        "· {}{}",
                        parts::past_full_ms(run.at),
                        if run.manual { " · run by hand" } else { "" }
                    ))),
            );
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
                        .child(super::items::text_value(&summary)),
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
            FormTarget::Routine(id) => PanelItem::Routine(id.clone()),
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
            work_directly: bool,
            space_id: Option<String>,
            worktree: bool,
            model: Option<String>,
        }
        let seed = {
            let state = self.state.read(cx);
            match &target {
                FormTarget::NewRoutine => Seed {
                    name: String::new(),
                    prompt: String::new(),
                    schedule: Schedule::default(),
                    write_notes: false,
                    work_directly: false,
                    space_id: None,
                    worktree: true,
                    model: None,
                },
                FormTarget::Routine(id) => {
                    let r = state.bot_records.routines.iter().find(|r| r.id == *id)?;
                    let (space_id, worktree) = match &r.target {
                        RoutineTarget::Workspace => (None, true),
                        RoutineTarget::Project {
                            space_id, worktree, ..
                        } => (Some(space_id.clone()), *worktree),
                    };
                    let model = r.chosen_model().map(str::to_string);
                    Seed {
                        name: r.name.clone(),
                        prompt: r.prompt.clone(),
                        schedule: r.schedule.clone(),
                        write_notes: r.write_notes,
                        work_directly: r.work_directly,
                        space_id,
                        worktree,
                        model,
                    }
                }
            }
        };
        let name = cx.new(|cx| TextInput::settings_field("Morning summary", false, cx));
        let prompt = cx.new(|cx| {
            TextInput::settings_prompt_field("What moved, what's stuck, and what needs me?", cx)
        });
        let time = cx.new(|cx| TextInput::settings_field("09:00", false, cx));
        let model_search = cx.new(|cx| TextInput::settings_field("Search models…", false, cx));
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
            work_directly: seed.work_directly,
            space_id: seed.space_id,
            worktree: seed.worktree,
            model: seed.model,
            model_open: false,
            model_search,
            models: ModelList::Loading,
            models_device: None,
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
            FormTarget::Routine(_) => {}
            FormTarget::NewRoutine => self.show_panel(PanelItem::Schedule, cx),
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
        let coordinate = {
            let state = self.state.read(cx);
            state
                .bots
                .iter()
                .find(|b| b.id == form.bot_id)
                .is_some_and(|b| b.capabilities.coordinate)
        };
        let op = built.and_then(|schedule| {
            let existing = match &form.target {
                FormTarget::Routine(id) => records.routines.iter().find(|r| r.id == *id),
                FormTarget::NewRoutine => None,
            };
            let routine = BotRoutine {
                id: existing.map_or_else(|| uuid::Uuid::new_v4().to_string(), |r| r.id.clone()),
                bot_id: form.bot_id.clone(),
                name: name.clone(),
                prompt,
                target: form_target(form.space_id.clone(), form.worktree),
                model: form.model.clone(),
                schedule,
                enabled: existing.is_none_or(|r| r.enabled),
                write_notes: form.write_notes,
                work_directly: form.work_directly,
                created_at: existing.map_or(now, |r| r.created_at),
                updated_at: now,
                run_requested_at: None,
                thread_chat_id: None,
                manual_chat_id: None,
                worktree_path: None,
                last_run: None,
                next_run_at: None,
            };
            routine.validate()?;
            if routine.target.space_id().is_some() && !coordinate {
                return Err(
                    "Running in a project needs Coordinate in workspace settings.".to_string(),
                );
            }
            Ok((
                routine.id.clone(),
                PanelItem::Routine(routine.id.clone()),
                serde_json::json!({ "op": "saveBotRoutine", "routine": routine }),
            ))
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
                                    "This workspace's device can't save routines yet.".to_string()
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
        let projects = self.routine_projects(bot, cx);
        let head = PanelHead {
            glyph: ROUTINE_GLYPH,
            tint: Some(bot_tint(bot, theme)),
            title: match &target {
                FormTarget::NewRoutine => "New routine",
                FormTarget::Routine(_) => "Edit routine",
            }
            .into(),
        };
        let model_picker = self.render_model_picker(bot, theme, cx);
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
            .child(parts::field_label(theme, "What it does each run"))
            .child(parts::field(&form.prompt));

        // ── the schedule editor: one box, a labelled row per choice ──
        let hairline = crate::kit::theme::hairline(0.06);
        let row = |label: &'static str, first: bool| {
            div()
                .when(!first, |el| el.border_t_1().border_color(hairline))
                .h(px(42.0))
                .px(px(12.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(label),
                )
        };
        let compact = |input: &Entity<TextInput>, width: f32| {
            div()
                .flex_none()
                .w(px(width))
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(7.0))
                .border_1()
                .border_color(crate::kit::theme::hairline(0.08))
                .bg(crate::kit::theme::ink(0.04))
                .text_size(px(12.5))
                .child(input.clone().into_any_element())
        };
        let mut when = parts::group_box().child(row("Repeats", true).child(
            div().w(px(216.0)).flex_none().child(parts::segmented(
                "bot-form-kind",
                &[
                    (ScheduleKind::Daily, "Daily"),
                    (ScheduleKind::Every, "Interval"),
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
            )),
        ));
        match form.draft.kind {
            ScheduleKind::Daily => {
                // Like the Repeats control: a track, the picked days lifted
                // — none when it is every day, which the foot line says.
                let every_day = form.draft.days.is_empty();
                let chips = (1..=7u8).map(|day| {
                    let on = form.draft.has_day(day);
                    let lifted = on && !every_day;
                    div()
                        .id(SharedString::from(format!("bot-form-day-{day}")))
                        .w(px(26.0))
                        .py(px(3.0))
                        .rounded(px(6.0))
                        .flex()
                        .justify_center()
                        .text_size(px(12.0))
                        .cursor_pointer()
                        .text_color(if on { theme.text } else { theme.text_muted })
                        .when(lifted, |el| {
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
                            ["M", "T", "W", "T", "F", "S", "S"][day as usize - 1],
                        ))
                });
                let days = div()
                    .flex_none()
                    .p(px(2.0))
                    .rounded(px(8.0))
                    .bg(crate::kit::theme::ink(0.05))
                    .flex()
                    .flex_row()
                    .gap(px(2.0))
                    .children(chips);
                when = when
                    .child(row("Time", false).child(compact(&form.time, 64.0)))
                    .child(row("Days", false).child(days));
            }
            ScheduleKind::Every => {
                when = when.child(row("Every", false).child(compact(&form.every, 52.0)).child(
                    div().w(px(144.0)).flex_none().child(parts::segmented(
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
                    )),
                ));
            }
            ScheduleKind::Manual => {}
        }
        // One line at the box's foot, always there so nothing jumps: when
        // it next runs, or what to fix.
        let (hint, hint_color) = match &preview {
            Ok(Schedule::Manual) => (
                "Runs only when you press Run now.".to_string(),
                theme.text_muted,
            ),
            Ok(schedule) => {
                let now = Local::now();
                let next = schedule
                    .next_after(&now)
                    .map(|at| format!(" · next run {}", lower_first(&parts::next_full(&at, &now))))
                    .unwrap_or_default();
                (format!("{}{next}", schedule.describe()), theme.text_muted)
            }
            Err(problem) => (problem.clone(), theme.danger.opacity(0.85)),
        };
        when = when.child(
            div()
                .border_t_1()
                .border_color(hairline)
                .px(px(12.0))
                .py(px(8.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.5))
                .text_color(hint_color)
                .child(icon(icons::CLOCK).size(px(12.0)).text_color(hint_color))
                .child(SharedString::from(hint)),
        );
        body = body.child(parts::field_label(theme, "When")).child(when);

        // ── where it runs, and what it may do there ──
        let coordinate = bot.capabilities.coordinate;
        let place_row = |key: String,
                         glyph: &'static str,
                         tint: gpui::Hsla,
                         title: String,
                         note: String,
                         on: bool,
                         enabled: bool,
                         pick: Option<String>,
                         cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(key))
                .h(px(30.0))
                .px(px(10.0))
                .rounded(px(7.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(9.0))
                .when(on, |el| {
                    el.bg(crate::kit::theme::card_selected_bg())
                        .shadow(crate::kit::theme::card_selected_shadows())
                })
                .when(!enabled, |el| el.opacity(0.45))
                .when(enabled && !on, |el| {
                    el.cursor_pointer().hover(|s| s.bg(theme.element_hover))
                })
                .when(enabled, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(form) = this.board.schedule.form.as_mut() {
                            form.space_id = pick.clone();
                            form.error = None;
                        }
                        cx.notify();
                    }))
                })
                .child(icon(glyph).size(px(14.0)).text_color(tint))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(SharedString::from(title)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(note)),
                )
                .when(on, |el| {
                    el.child(icon(icons::CHECK).size(px(12.0)).text_color(theme.text))
                })
        };
        let mut places = vec![place_row(
            "bot-form-place-workspace".into(),
            ROUTINE_GLYPH,
            bot_tint(bot, theme),
            "This workspace".into(),
            "as its agent".into(),
            form.space_id.is_none(),
            true,
            None,
            cx,
        )];
        for (space, device) in &projects {
            let glyph = crate::appearance::space_style::space_icon(space.icon.as_deref());
            let tint = crate::appearance::space_style::space_color(space.color.as_deref(), theme)
                .unwrap_or(theme.text_muted);
            let on = form.space_id.as_deref() == Some(space.id.as_str());
            places.push(place_row(
                format!("bot-form-project-{}", space.id),
                glyph,
                tint,
                space.display_name().to_string(),
                device.clone(),
                on,
                coordinate || on,
                Some(space.id.clone()),
                cx,
            ));
        }
        body = body
            .child(parts::field_label(theme, "Runs in"))
            .child(
                div()
                    .mx(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .children(places),
            )
            .when(!coordinate && !projects.is_empty(), |el| {
                el.child(div().mx(px(8.0)).mt(px(6.0)).child(parts::quiet_line(
                    theme,
                    "Running in a project needs Coordinate in workspace settings.",
                )))
            });
        match &form.space_id {
            None => {
                body = body.child(parts::field_label(theme, "Permissions")).child(
                    parts::group_box()
                        .child(parts::grant_row(
                            theme,
                            "Reads what the workspace reads",
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
                        )
                        .child(
                            parts::switch_row(
                                "bot-form-direct",
                                "May run commands",
                                "In the workspace's projects on this device, e.g. to run tests. \
                                 Anything that needs approval fails the run.",
                                form.work_directly,
                                false,
                                theme,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(form) = this.board.schedule.form.as_mut() {
                                    form.work_directly = !form.work_directly;
                                }
                                cx.notify();
                            })),
                        ),
                );
            }
            Some(_) => {
                body = body
                    .child(parts::field_label(theme, "Checkout"))
                    .child(div().mx(px(8.0)).child(parts::segmented(
                        "bot-form-checkout",
                        &[(true, "Its own worktree"), (false, "Project checkout")],
                        form.worktree,
                        |this, worktree, cx| {
                            if let Some(form) = this.board.schedule.form.as_mut() {
                                form.worktree = worktree;
                            }
                            cx.notify();
                        },
                        theme,
                        cx,
                    )))
                    .child(div().mx(px(8.0)).mt(px(8.0)).child(parts::quiet_line(
                        theme,
                        if form.worktree {
                            "A worktree made on the first run and kept, so runs never touch \
                             your checkout. Scheduled runs continue one session; Run now \
                             starts a fresh one."
                        } else {
                            "Runs work in the project's own folder. Scheduled runs continue \
                             one session; Run now starts a fresh one."
                        },
                    )));
            }
        }

        body = body.child(div().h(px(10.0))).child(model_picker);

        let new = target == FormTarget::NewRoutine;
        let (footer, cancel, save) = parts::form_footer(
            theme,
            "bot-form",
            form.error.clone(),
            match (new, form.busy) {
                (_, true) => "Saving…",
                (true, false) => "Create routine",
                (false, false) => "Save",
            },
            form.busy,
        );
        self.board.panel_footer = Some(
            footer
                .child(cancel.on_click(cx.listener(|this, _, _, cx| this.close_job_form(cx))))
                .child(save.on_click(cx.listener(|this, _, _, cx| this.save_job_form(cx))))
                .into_any_element(),
        );
        (head, body.pb(px(16.0)))
    }
}

impl Shell {
    /// The device a form's routine runs on: its project's, else the
    /// workspace's host.
    fn form_device(&self, bot: &Bot, cx: &App) -> Option<String> {
        let form = self.board.schedule.form.as_ref()?;
        let state = self.state.read(cx);
        Some(match &form.space_id {
            Some(id) => state.space_row(id)?.device_id.clone(),
            None => bot.host_device_id.clone(),
        })
    }

    /// The label for a routine's model, asking its device for its models
    /// once when no list has named it yet; the id until then.
    fn model_label(&mut self, device: Option<String>, id: &str, cx: &mut Context<Self>) -> String {
        if let Some(label) = self.board.schedule.model_labels.get(id) {
            return label.clone();
        }
        if let Some(device) = device
            && self.board.schedule.labels_asked.insert(device.clone())
        {
            self.board_call(
                methods::LIST_MODELS,
                serde_json::json!({ "harness": "pi", "targetDeviceId": device }),
                cx,
                |this, result, cx| {
                    let Ok(value) = result else { return };
                    let Ok(models) = serde_json::from_value::<Vec<cypher_proto::Model>>(value)
                    else {
                        return;
                    };
                    this.board.schedule.model_labels.extend(
                        crate::pickers::normalize_model_rows(models)
                            .into_iter()
                            .map(|m| (m.id, m.label)),
                    );
                    cx.notify();
                },
            );
        }
        id.to_string()
    }

    /// Ask the routine's device for its models, once per device.
    fn load_form_models(&mut self, bot: &Bot, cx: &mut Context<Self>) {
        let Some(device) = self.form_device(bot, cx) else {
            return;
        };
        let Some(form) = self.board.schedule.form.as_mut() else {
            return;
        };
        if form.models_device.as_deref() == Some(device.as_str()) {
            return;
        }
        form.models_device = Some(device.clone());
        form.models = ModelList::Loading;
        let asked = device.clone();
        self.board_call(
            methods::LIST_MODELS,
            serde_json::json!({ "harness": "pi", "targetDeviceId": device }),
            cx,
            move |this, result, cx| {
                let Some(form) = this.board.schedule.form.as_mut() else {
                    return;
                };
                if form.models_device.as_deref() != Some(asked.as_str()) {
                    return;
                }
                form.models = match result {
                    Ok(value) => match serde_json::from_value::<Vec<cypher_proto::Model>>(value) {
                        Ok(models) => {
                            let models = crate::pickers::normalize_model_rows(models);
                            this.board
                                .schedule
                                .model_labels
                                .extend(models.iter().map(|m| (m.id.clone(), m.label.clone())));
                            ModelList::Ready(models)
                        }
                        Err(err) => ModelList::Failed(err.to_string()),
                    },
                    Err(CallError::Unknown) => {
                        ModelList::Failed("This device can't list its models.".into())
                    }
                    Err(CallError::Failed(text)) => ModelList::Failed(text),
                };
                cx.notify();
            },
        );
    }

    /// The form's model choice, drawn like Settings' model pickers: a row
    /// with a bordered trigger naming it, opening a menu over it with a
    /// search field over the device's models, "Default" first.
    fn render_model_picker(
        &mut self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use crate::settings::widgets;
        self.load_form_models(bot, cx);
        let Some(form) = self.board.schedule.form.as_ref() else {
            return div();
        };
        let current = match (&form.model, &form.models) {
            (None, _) => "Default".to_string(),
            (Some(id), ModelList::Ready(models)) => models
                .iter()
                .find(|m| &m.id == id)
                .map(|m| m.label.clone())
                .unwrap_or_else(|| id.clone()),
            (Some(id), _) => id.clone(),
        };
        let popup = form.model_open.then(|| {
            let row =
                |id: Option<String>, label: String, selected: bool, cx: &mut Context<Self>| {
                    div()
                        .id(SharedString::from(format!(
                            "bot-form-model-{}",
                            id.as_deref().unwrap_or("default")
                        )))
                        .px(px(10.0))
                        .py(px(8.0))
                        .rounded(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .cursor_pointer()
                        .bg(if selected {
                            theme.ink(0.07)
                        } else {
                            gpui::transparent_black()
                        })
                        .hover(|style| style.bg(theme.ink(0.05)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(form) = this.board.schedule.form.as_mut() {
                                form.model = id.clone();
                                form.model_open = false;
                            }
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(13.0))
                                .text_color(theme.text)
                                .child(SharedString::from(label)),
                        )
                        .child(div().w(px(16.0)).when(selected, |row| {
                            row.child(icon(icons::CHECK).size(px(12.0)).text_color(theme.accent))
                        }))
                };
            let chosen = form.model.clone();
            let mut menu = div()
                .w(px(280.0))
                .rounded(px(10.0))
                .bg(theme.surface_overlay)
                .border_1()
                .border_color(theme.border_strong)
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    if let Some(form) = this.board.schedule.form.as_mut() {
                        form.model_open = false;
                    }
                    cx.notify();
                }))
                .child(div().p(px(8.0)).child(form.model_search.clone()))
                .child(
                    div()
                        .px(px(4.0))
                        .child(row(None, "Default".into(), chosen.is_none(), cx)),
                );
            match form.models.clone() {
                ModelList::Ready(models) => {
                    let query = form.model_search.read(cx).text().trim().to_lowercase();
                    let matching: Vec<_> = models
                        .into_iter()
                        .filter(|m| {
                            query.is_empty()
                                || m.label.to_lowercase().contains(&query)
                                || m.id.to_lowercase().contains(&query)
                        })
                        .collect();
                    if matching.is_empty() {
                        menu = menu.child(
                            div()
                                .p(px(12.0))
                                .text_size(px(12.0))
                                .text_color(theme.text_muted)
                                .child("No matching models."),
                        );
                    } else {
                        let mut list = div()
                            .id("bot-form-model-list")
                            .max_h(px(240.0))
                            .overflow_y_scroll()
                            .px(px(4.0))
                            .pb(px(4.0));
                        for m in matching {
                            let on = chosen.as_deref() == Some(m.id.as_str());
                            list = list.child(row(Some(m.id), m.label, on, cx));
                        }
                        menu = menu.child(list);
                    }
                }
                ModelList::Failed(error) => {
                    menu = menu.child(widgets::error_strip(theme, &error).mt(px(0.0)))
                }
                ModelList::Loading => {
                    menu = menu.child(
                        div()
                            .p(px(12.0))
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child("Loading models…"),
                    )
                }
            }
            // The row sits at the form's foot, by the window's corner: the
            // menu opens upward, aligned to the trigger's right edge.
            popover::anchored_menu_above_end("bot-form-model-popup", menu.into_any_element(), None)
        });
        let trigger = widgets::ghost_action(theme)
            .id("bot-form-model-trigger")
            .relative()
            .w(px(180.0))
            .flex_none()
            .border_1()
            .border_color(theme.border)
            .hover(|s| widgets::ghost_hover(theme, s))
            .on_click(cx.listener(|this, _, window, cx| {
                if let Some(form) = this.board.schedule.form.as_mut() {
                    form.model_open = !form.model_open;
                    if form.model_open {
                        window.focus(&form.model_search.focus_handle(cx), cx);
                    }
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text)
                    .child(SharedString::from(current)),
            )
            .child(
                icon(icons::ALT_ARROW_DOWN)
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            )
            .children(popup);
        parts::group_box().child(
            div()
                .px(px(12.0))
                .py(px(9.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child("Model"),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .child("Default: the workspace chat's."),
                        ),
                )
                .child(trigger),
        )
    }
}

/// A routine's Schedule fact: when it repeats, and how soon it next runs
/// ("Weekdays 08:30 · next in 9h"), "off" while disabled; a manual one
/// runs only when asked.
fn schedule_value(job: &Job, now: &DateTime<Local>) -> String {
    if job.schedule == Schedule::Manual {
        return "Only when you press Run now".into();
    }
    let when = job.schedule.describe();
    if !job.enabled {
        return format!("{when} · off");
    }
    match job.next.and_then(parts::local) {
        Some(at) => format!("{when} · next {}", in_words(&at, now)),
        None => when,
    }
}

/// A routine as another page lists it: its glyph, and when it runs
/// ("Wed 08:30 · next in 2 days", "Only when you press Run now").
pub(super) fn routine_brief(routine: &BotRoutine) -> (&'static str, String) {
    let now = Local::now();
    let job = Job::routine(routine, &now);
    let glyph = match job.kind {
        JobKind::Workspace => ROUTINE_GLYPH,
        JobKind::Project { .. } => TASK_GLYPH,
    };
    (glyph, schedule_value(&job, &now))
}

/// How soon `at` comes: "in 25m", "in 9h", "in 3 days".
fn in_words(at: &DateTime<Local>, now: &DateTime<Local>) -> String {
    let minutes = (*at - *now).num_minutes().max(0);
    match minutes {
        0 => "now".into(),
        1..=59 => format!("in {minutes}m"),
        60..=2879 => format!("in {}h", minutes / 60),
        _ => format!("in {} days", minutes / 1440),
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

/// A routine's mark: the workspace's repeat in the bot's colour, or code
/// work in its project's colour.
fn job_mark(
    job: &Job,
    space: Option<&Space>,
    bot_tint: gpui::Hsla,
    theme: &Theme,
) -> (&'static str, gpui::Hsla) {
    match job.kind {
        JobKind::Workspace => (ROUTINE_GLYPH, bot_tint),
        JobKind::Project { .. } => (
            TASK_GLYPH,
            space
                .and_then(|s| {
                    crate::appearance::space_style::space_color(s.color.as_deref(), theme)
                })
                .unwrap_or(theme.text_muted),
        ),
    }
}

/// The routine target a form describes (its model lives on the routine).
fn form_target(space_id: Option<String>, worktree: bool) -> RoutineTarget {
    match space_id {
        None => RoutineTarget::Workspace,
        Some(space_id) => RoutineTarget::Project {
            space_id,
            worktree,
            model: None,
        },
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
            kind: JobKind::Workspace,
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
            manual: false,
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
        assert_eq!(job_sub(&routine, None), "This workspace · Daily 09:00");
        let project = Job {
            kind: JobKind::Project {
                space_id: "web".into(),
            },
            ..job("t", daily("18:30", vec![1, 2, 3, 4, 5]), true, None)
        };
        assert_eq!(job_sub(&project, Some("Web")), "Web · Weekdays 18:30");
        assert_eq!(project.item(), PanelItem::Routine("t".into()));
        let now = at("2026-10-18T10:00:00+01:00");
        let next = at("2026-10-19T02:00:00+01:00").timestamp_millis();
        assert_eq!(state_note(JobState::Next(next), &now), "Mon");
        assert_eq!(state_note(JobState::Off, &now), "off");
        assert_eq!(state_note(JobState::Manual, &now), "manual");
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
    fn days_start_as_every_day_and_keep_at_least_one() {
        let mut draft = ScheduleDraft {
            kind: ScheduleKind::Daily,
            days: vec![],
            unit: Unit::Hours,
        };
        assert!(
            (1..=7).all(|d| draft.has_day(d)),
            "none picked is every day"
        );
        draft.toggle_day(6);
        draft.toggle_day(7);
        assert_eq!(
            draft.build("08:30", ""),
            Ok(daily("08:30", vec![1, 2, 3, 4, 5]))
        );
        for day in 1..=5 {
            draft.toggle_day(day);
        }
        assert_eq!(draft.days, [5], "the last day stays");
        for day in [1, 2, 3, 4, 6, 7] {
            draft.toggle_day(day);
        }
        assert!(draft.days.is_empty(), "all seven is every day again");
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
            target: RoutineTarget::Workspace,
            model: None,
            schedule: Schedule::default(),
            enabled: true,
            write_notes: false,
            work_directly: false,
            created_at: 0,
            updated_at: 0,
            run_requested_at: None,
            thread_chat_id: None,
            manual_chat_id: None,
            worktree_path: None,
            last_run: None,
            next_run_at: None,
        };
        assert_eq!(routine.validate(), Err("Name the routine.".into()));
    }

    #[test]
    fn forms_say_where_a_routine_runs() {
        assert_eq!(form_target(None, true), RoutineTarget::Workspace);
        let target = form_target(Some("web".into()), false);
        assert_eq!(
            serde_json::to_value(&target).unwrap(),
            serde_json::json!({ "kind": "project", "spaceId": "web", "worktree": false })
        );
        assert_eq!(
            serde_json::to_value(RoutineTarget::Workspace).unwrap(),
            serde_json::json!({ "kind": "workspace" })
        );
    }
}
