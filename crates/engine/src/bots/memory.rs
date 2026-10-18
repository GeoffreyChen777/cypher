//! Memory and routines (docs/design/agent-bots.md §7–§8, P3): the
//! notebook, the scheduler, routines and recurring project tasks.
//!
//! Notes, routines and tasks are registry rows (`cypher_proto::bot_records`).
//! The tools here read and write them for a bot's threads; what a thread may
//! do follows its origin and thread kind ([`access`]), checked on every call.
//! The scheduler that runs routines and tasks is [`host`].

use chrono::{DateTime, Local, Utc};
use serde::Deserialize;

use cypher_proto::{
    self as p, ActivityKind, Bot, BotActivity, BotChat, BotNote, BotOrigin, BotRoutine, BotThread,
    Chat, NoteSource, ProjectTask, Schedule,
};

use super::Bots;
use crate::EngineError;
use crate::host::workspace_host::{Fields, WorkspaceHost};
use crate::util::new_id;

mod host;
mod schedule;
#[cfg(test)]
mod tests;

pub(super) use host::ScheduleState;

/// Most notes `read_notes` lists.
const READ_NOTES_MAX: usize = 200;
/// One answer, like the read tools'.
const ANSWER_MAX_CHARS: usize = 48_000;

/// What one thread may do with the notebook and the schedule (§7, §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Access {
    pub read_notes: bool,
    pub write_notes: bool,
    pub list_schedule: bool,
    pub save_routine: bool,
    pub save_task: bool,
}

/// Owner threads (the bot chat and orchestration threads) read and write the
/// notebook; routine threads read it and write only when their routine says
/// so; peer threads get nothing, so a peer can neither read the owner's notes
/// nor plant one. Routines and tasks are made in the bot chat only, tasks
/// only with *Coordinate*, since every task run starts a session.
pub(super) fn access(marker: &BotChat, bot: &Bot, routine_writes_notes: bool) -> Access {
    match (marker.origin, &marker.thread) {
        (BotOrigin::Owner, BotThread::Main) => Access {
            read_notes: true,
            write_notes: true,
            list_schedule: true,
            save_routine: true,
            save_task: bot.capabilities.coordinate,
        },
        (BotOrigin::Owner, BotThread::Work { .. }) => Access {
            read_notes: true,
            write_notes: true,
            list_schedule: true,
            ..Access::default()
        },
        (BotOrigin::Routine, BotThread::Routine { .. }) => Access {
            read_notes: true,
            write_notes: routine_writes_notes,
            list_schedule: true,
            ..Access::default()
        },
        _ => Access::default(),
    }
}

impl Access {
    fn allows(self, tool: &str) -> bool {
        match tool {
            "read_notes" => self.read_notes,
            "write_note" => self.write_notes,
            "list_schedule" => self.list_schedule,
            "save_routine" => self.save_routine,
            "save_project_task" => self.save_task,
            _ => false,
        }
    }

    fn tools(self) -> Vec<&'static str> {
        p::BOT_MEMORY_TOOLS
            .into_iter()
            .filter(|tool| self.allows(tool))
            .collect()
    }
}

/// The routine a routine thread runs, when it is this bot's.
fn thread_routine(workspace: &WorkspaceHost, marker: &BotChat) -> Option<BotRoutine> {
    let BotThread::Routine { routine_id } = &marker.thread else {
        return None;
    };
    workspace
        .record::<BotRoutine>(p::KIND_BOT_ROUTINES, routine_id)
        .filter(|routine| routine.bot_id == marker.bot_id)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ReadNotes {
    query: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WriteNote {
    text: String,
}

#[derive(Debug, Deserialize)]
struct SaveRoutine {
    #[serde(default)]
    id: Option<String>,
    name: String,
    prompt: String,
    schedule: Schedule,
    #[serde(default)]
    write_notes: Option<bool>,
    #[serde(default)]
    work_directly: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct SaveProjectTask {
    #[serde(default)]
    id: Option<String>,
    project: String,
    name: String,
    prompt: String,
    schedule: Schedule,
    #[serde(default)]
    worktree: Option<bool>,
    #[serde(default)]
    model: Option<String>,
}

fn args<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> Result<T, EngineError> {
    serde_json::from_value(value.clone())
        .map_err(|err| EngineError::Other(format!("Invalid arguments: {err}")))
}

fn refuse(tool: &str) -> EngineError {
    EngineError::Other(match tool {
        "write_note" => "This thread may read the notebook but not write to it.".into(),
        "save_routine" => "Routines are made in the workspace chat, when your owner asks.".into(),
        "save_project_task" => {
            "Recurring project tasks need the Coordinate permission and are made in the workspace \
             chat, when your owner asks."
                .into()
        }
        _ => format!("{tool} isn't available in this thread."),
    })
}

impl Bots {
    /// Start this track's host loop (called once at engine start).
    pub(super) fn start_schedule_host(&self) {
        let bots = self.clone();
        let cancel = self.hosts_cancelled();
        self.keep_host_task(tokio::spawn(async move {
            bots.run_schedule_host(cancel).await;
        }));
    }

    /// Answer `tool` when it is one of this track's; `None` otherwise.
    pub(super) async fn memory_tool(
        &self,
        chat: &Chat,
        marker: &BotChat,
        tool: &str,
        args: &serde_json::Value,
    ) -> Option<Result<String, EngineError>> {
        if !p::BOT_MEMORY_TOOLS.contains(&tool) {
            return None;
        }
        Some(self.memory_tool_call(chat, marker, tool, args))
    }

    fn memory_tool_call(
        &self,
        chat: &Chat,
        marker: &BotChat,
        tool: &str,
        raw: &serde_json::Value,
    ) -> Result<String, EngineError> {
        let workspace = &self.inner.workspace;
        // The bot chat itself, or one of its hidden threads (only the host
        // writes a thread's marker), like every bot tool.
        let bot = workspace
            .bot(&marker.bot_id)?
            .filter(|bot| bot.chat_id == chat.id || !marker.is_main())
            .ok_or_else(|| EngineError::Other("This workspace has been deleted.".into()))?;
        let routine = thread_routine(workspace, marker);
        let access = access(
            marker,
            &bot,
            routine.as_ref().is_some_and(|r| r.write_notes),
        );
        if !access.allows(tool) {
            return Err(refuse(tool));
        }
        match tool {
            "read_notes" => Ok(self.read_notes(&bot, args::<ReadNotes>(raw)?)),
            "write_note" => self.write_note(&bot, chat, args::<WriteNote>(raw)?),
            "list_schedule" => self.list_schedule(&chat.id),
            "save_routine" => self.save_routine(&bot, args::<SaveRoutine>(raw)?),
            "save_project_task" => self.save_project_task(&bot, &chat.id, args(raw)?),
            _ => Err(refuse(tool)),
        }
    }

    fn read_notes(&self, bot: &Bot, args: ReadNotes) -> String {
        let records = self.inner.workspace.read_bot_records();
        let notes = notes_of(&records.notes, &bot.id);
        let words: Vec<String> = args
            .query
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let found: Vec<&BotNote> = notes
            .into_iter()
            .filter(|note| {
                let text = note.text.to_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .collect();
        if found.is_empty() {
            return if words.is_empty() {
                "Your notebook is empty.".into()
            } else {
                format!("No notes contain \"{}\".", words.join(" "))
            };
        }
        let mut out = vec![if words.is_empty() {
            format!("Your notebook ({} notes, newest first):", found.len())
        } else {
            format!(
                "Notes containing \"{}\" ({}, newest first):",
                words.join(" "),
                found.len()
            )
        }];
        let mut size = out[0].len();
        for (shown, note) in found.iter().enumerate() {
            let line = note_line(note);
            if shown == READ_NOTES_MAX || size + line.len() > ANSWER_MAX_CHARS {
                out.push(format!(
                    "{} older notes not shown; narrow with `query`.",
                    found.len() - shown
                ));
                break;
            }
            size += line.len() + 1;
            out.push(line);
        }
        out.join("\n")
    }

    fn write_note(&self, bot: &Bot, chat: &Chat, args: WriteNote) -> Result<String, EngineError> {
        let text = args.text.trim();
        if text.is_empty() {
            return Err(EngineError::Other("Give write_note some text.".into()));
        }
        let now = p::now_ms();
        let mut note = BotNote {
            id: new_id(),
            bot_id: bot.id.clone(),
            text: text.to_string(),
            source: NoteSource::Bot {
                chat_id: Some(chat.id.clone()),
                title: chat.title.clone().filter(|t| !t.trim().is_empty()),
            },
            created_at: now,
            updated_at: now,
        };
        note.clamp();
        let cut = note.text.chars().count() < text.chars().count();
        self.inner
            .workspace
            .put_record(p::KIND_BOT_NOTES, &note.id, &note, Fields::All)?;
        record_activity(
            &self.inner.workspace,
            &bot.id,
            ActivityKind::NoteWritten,
            &format!("Noted: {}", note.text),
            Some(&chat.id),
            None,
        );
        Ok(if cut {
            format!(
                "Saved to your notebook, cut to {} characters (note {}).",
                p::NOTE_MAX,
                note.id
            )
        } else {
            format!("Saved to your notebook (note {}).", note.id)
        })
    }

    fn list_schedule(&self, chat_id: &str) -> Result<String, EngineError> {
        let view = self.view(chat_id)?;
        let records = self.inner.workspace.read_bot_records();
        Ok(render_schedule(
            &view.bot,
            &records,
            &view.spaces,
            |id| view.device(id).to_string(),
            Local::now(),
        ))
    }

    fn save_routine(&self, bot: &Bot, args: SaveRoutine) -> Result<String, EngineError> {
        let workspace = &self.inner.workspace;
        let now = p::now_ms();
        let existing = match args.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(id) => Some(
                workspace
                    .record::<BotRoutine>(p::KIND_BOT_ROUTINES, id)
                    .filter(|r| r.bot_id == bot.id)
                    .ok_or_else(|| {
                        EngineError::Other(format!("No routine {id}. Use list_schedule."))
                    })?,
            ),
            None => None,
        };
        let mut routine = BotRoutine {
            id: existing
                .as_ref()
                .map_or_else(new_id, |routine| routine.id.clone()),
            bot_id: bot.id.clone(),
            name: args.name,
            prompt: args.prompt,
            schedule: args.schedule,
            enabled: args
                .enabled
                .or(existing.as_ref().map(|r| r.enabled))
                .unwrap_or(true),
            write_notes: args
                .write_notes
                .or(existing.as_ref().map(|r| r.write_notes))
                .unwrap_or(false),
            work_directly: args
                .work_directly
                .or(existing.as_ref().map(|r| r.work_directly))
                .unwrap_or(false),
            max_runs_per_day: existing.as_ref().map_or(24, |r| r.max_runs_per_day),
            created_at: existing.as_ref().map_or(now, |r| r.created_at),
            updated_at: now,
            run_requested_at: None,
            thread_chat_id: None,
            last_run: None,
            next_run_at: None,
            runs_today: 0,
            runs_day: None,
        };
        routine.clamp();
        routine.validate().map_err(EngineError::Other)?;
        workspace.put_record(
            p::KIND_BOT_ROUTINES,
            &routine.id,
            &routine,
            Fields::Except(&[BotRoutine::HOST_FIELDS, &["runRequestedAt"]].concat()),
        )?;
        self.wake_schedule();
        let mut out = vec![format!(
            "Saved routine \"{}\" (id {}): {}{}.",
            routine.name,
            routine.id,
            routine.schedule.describe(),
            next_words(&routine.schedule, routine.enabled, Local::now()),
        )];
        out.push("It runs unattended in its own thread and posts its result in this chat.".into());
        if routine.write_notes {
            out.push("It may write to your notebook.".into());
        }
        if routine.work_directly {
            out.push(if bot.capabilities.work_directly {
                "It may run commands and edit files in your local projects; anything that \
                 needs approval fails the run."
                    .into()
            } else {
                "It asks to work directly, but your owner turned Work directly off for you, \
                 so it reads only."
                    .into()
            });
        }
        if !routine.enabled {
            out.push("It is turned off: it runs only when started by hand.".into());
        }
        if bot.paused {
            out.push(
                "This workspace is paused, so its routines are skipped until it resumes.".into(),
            );
        }
        Ok(out.join(" "))
    }

    fn save_project_task(
        &self,
        bot: &Bot,
        chat_id: &str,
        args: SaveProjectTask,
    ) -> Result<String, EngineError> {
        let workspace = &self.inner.workspace;
        let view = self.view(chat_id)?;
        let spaces = view
            .resolve_project(Some(&args.project))?
            .unwrap_or_default();
        let [space_id] = spaces.as_slice() else {
            return Err(EngineError::Other(format!(
                "Several projects are named \"{}\"; pass the project id.",
                args.project.trim()
            )));
        };
        let space = view
            .space(Some(space_id))
            .ok_or_else(|| EngineError::Other("No such project.".into()))?;
        let now = p::now_ms();
        let existing = match args.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(id) => Some(
                workspace
                    .record::<ProjectTask>(p::KIND_PROJECT_TASKS, id)
                    .filter(|task| view.space(Some(&task.space_id)).is_some())
                    .ok_or_else(|| {
                        EngineError::Other(format!("No task {id}. Use list_schedule."))
                    })?,
            ),
            None => None,
        };
        let mut task = ProjectTask {
            id: existing
                .as_ref()
                .map_or_else(new_id, |task| task.id.clone()),
            space_id: space.id.clone(),
            name: args.name,
            prompt: args.prompt,
            schedule: args.schedule,
            enabled: existing.as_ref().is_none_or(|t| t.enabled),
            worktree: args
                .worktree
                .or(existing.as_ref().map(|t| t.worktree))
                .unwrap_or(false),
            model: args
                .model
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .or_else(|| existing.as_ref().and_then(|t| t.model.clone())),
            created_by: existing
                .as_ref()
                .map_or(Some(bot.id.clone()), |t| t.created_by.clone()),
            created_at: existing.as_ref().map_or(now, |t| t.created_at),
            updated_at: now,
            run_requested_at: None,
            last_run: None,
            next_run_at: None,
        };
        task.clamp();
        task.validate().map_err(EngineError::Other)?;
        workspace.put_record(
            p::KIND_PROJECT_TASKS,
            &task.id,
            &task,
            Fields::Except(&[ProjectTask::HOST_FIELDS, &["runRequestedAt"]].concat()),
        )?;
        self.wake_schedule();
        let place = format!(
            "{} on {}",
            super::prompt::space_name(space),
            view.device(&space.device_id)
        );
        let mut out = vec![format!(
            "Saved task \"{}\" in {place} (id {}): {}{}.",
            task.name,
            task.id,
            task.schedule.describe(),
            next_words(&task.schedule, task.enabled, Local::now()),
        )];
        out.push(if task.worktree {
            "Each run starts a fresh session in a new worktree.".into()
        } else {
            "Each run starts a fresh session in the project's checkout.".into()
        });
        out.push(format!(
            "Runs happen on {} and need it to be on.",
            view.device(&space.device_id)
        ));
        Ok(out.join(" "))
    }
}

/// `", next Mon Oct 19 09:00"` for a job that will run on its own.
fn next_words(schedule: &Schedule, enabled: bool, now: DateTime<Local>) -> String {
    match schedule.next_after(&now).filter(|_| enabled) {
        Some(next) => format!(", next run {}", next.format("%a %b %-d %H:%M")),
        None => String::new(),
    }
}

/// This bot's notes, newest first.
fn notes_of<'a>(notes: &'a [BotNote], bot_id: &str) -> Vec<&'a BotNote> {
    let mut mine: Vec<&BotNote> = notes.iter().filter(|n| n.bot_id == bot_id).collect();
    mine.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    mine
}

/// `- Alice owns auth (owner, 2026-10-14)`.
fn note_line(note: &BotNote) -> String {
    let source = match &note.source {
        NoteSource::Owner => "from your owner".to_string(),
        NoteSource::Bot { title: Some(t), .. } => format!("you, in \"{t}\""),
        NoteSource::Bot { .. } => "you".to_string(),
    };
    let date = DateTime::<Utc>::from_timestamp_millis(note.updated_at)
        .map(|at| at.with_timezone(&Local).format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    format!(
        "- {} ({source}, {date})",
        note.text.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

/// Write one entry of the bot's activity log, pruning its oldest beyond
/// [`p::KEEP_ACTIVITY`]. Best-effort: the log never fails the action.
pub(super) fn record_activity(
    workspace: &WorkspaceHost,
    bot_id: &str,
    kind: ActivityKind,
    text: &str,
    chat_id: Option<&str>,
    space_id: Option<&str>,
) {
    let mut entry = BotActivity {
        id: new_id(),
        bot_id: bot_id.to_string(),
        at: p::now_ms(),
        kind,
        text: text.to_string(),
        chat_id: chat_id.map(str::to_string),
        work_id: None,
        space_id: space_id.map(str::to_string),
    };
    entry.clamp();
    if let Err(err) = workspace.put_record(p::KIND_BOT_ACTIVITY, &entry.id, &entry, Fields::All) {
        tracing::warn!(bot = %bot_id, error = %err, "bot activity write failed");
        return;
    }
    let mut mine: Vec<(i64, String)> = workspace
        .read_bot_records()
        .activity
        .into_iter()
        .filter(|a| a.bot_id == bot_id)
        .map(|a| (a.at, a.id))
        .collect();
    if mine.len() > p::KEEP_ACTIVITY {
        mine.sort();
        let over = mine.len() - p::KEEP_ACTIVITY;
        let stale: Vec<String> = mine.into_iter().take(over).map(|(_, id)| id).collect();
        workspace.delete_records(p::KIND_BOT_ACTIVITY, &stale);
    }
}

/// This track's tools a thread may call, beyond the read tools. A routine
/// thread is offered `write_note` whatever its routine says; the call checks
/// the routine (the allowlist is fixed before the routine row is read).
pub(super) fn memory_tools(marker: &BotChat, bot: Option<&Bot>) -> Vec<&'static str> {
    match bot {
        Some(bot) => access(marker, bot, true).tools(),
        None => Vec::new(),
    }
}

/// This track's section of the thread's system prompt, if any: the newest
/// notes for owner and routine threads, nothing for peers.
pub(super) fn memory_prompt(
    workspace: &WorkspaceHost,
    bot: &Bot,
    marker: &BotChat,
) -> Option<String> {
    let routine = thread_routine(workspace, marker);
    let access = access(marker, bot, routine.as_ref().is_some_and(|r| r.write_notes));
    if !access.read_notes {
        return None;
    }
    let records = workspace.read_bot_records();
    Some(notebook_section(
        &notes_of(&records.notes, &bot.id),
        access,
        marker.is_main(),
    ))
}

/// The prompt's notebook section: the newest [`p::NOTES_IN_PROMPT`] notes
/// and how to use the tools.
pub(super) fn notebook_section(notes: &[&BotNote], access: Access, bot_chat: bool) -> String {
    let mut lines = vec!["Your notebook:".to_string()];
    lines.push(if access.write_notes {
        "- Notes are your memory across conversations: short facts and decisions about your \
         owner's work. Add one with write_note when you learn something worth keeping \
         (\"remember that…\", a decision, who owns what); keep each to one fact."
            .to_string()
    } else {
        "- Notes are your memory across conversations. This thread may read them but not \
         write new ones."
            .to_string()
    });
    if notes.len() > p::NOTES_IN_PROMPT {
        lines.push(format!(
            "- The newest {} of {} notes are below; read_notes finds the rest.",
            p::NOTES_IN_PROMPT,
            notes.len()
        ));
    }
    if bot_chat {
        lines.push(
            "- Routines run on a schedule in their own threads and post their results in this \
             chat. When your owner asks for something recurring (\"every morning, tell me \
             what's stuck\"), save it with save_routine; list_schedule shows your routines and \
             your projects' recurring tasks with their last results."
                .to_string(),
        );
    }
    if notes.is_empty() {
        lines.push("(Your notebook is empty.)".to_string());
    } else {
        lines.extend(notes.iter().take(p::NOTES_IN_PROMPT).map(|n| note_line(n)));
    }
    lines.join("\n")
}

/// The `list_schedule` answer: this bot's routines and the recurring tasks
/// of its projects, each with its schedule, next run and last result.
pub(super) fn render_schedule(
    bot: &Bot,
    records: &p::BotRecords,
    spaces: &[p::Space],
    device: impl Fn(&str) -> String,
    now: DateTime<Local>,
) -> String {
    let time = |ms: i64| {
        DateTime::<Utc>::from_timestamp_millis(ms)
            .map(|at| {
                at.with_timezone(&now.timezone())
                    .format("%a %b %-d %H:%M")
                    .to_string()
            })
            .unwrap_or_default()
    };
    let last = |run: Option<&p::RunRecord>| match run {
        None => "never run".to_string(),
        Some(run) => {
            let outcome = match run.outcome {
                p::RunOutcome::Running => "running",
                p::RunOutcome::Done => "done",
                p::RunOutcome::Failed => "failed",
                p::RunOutcome::Skipped => "skipped",
            };
            let summary = run
                .summary
                .as_deref()
                .map(|s| {
                    let line = s.split_whitespace().collect::<Vec<_>>().join(" ");
                    format!(": {}", p::clip(&line, 300))
                })
                .unwrap_or_default();
            format!("last run {} {outcome}{summary}", time(run.at))
        }
    };
    let next = |enabled: bool, schedule: &Schedule, at: Option<i64>| match at {
        Some(at) if enabled => format!("next {}", time(at)),
        _ if !enabled => "off".to_string(),
        _ if *schedule == Schedule::Manual => "runs by hand".to_string(),
        _ => "next run not planned yet".to_string(),
    };
    let mut out = vec![format!(
        "Times are {}'s local time (now {}).",
        device(&bot.host_device_id),
        now.format("%a %b %-d %H:%M")
    )];
    if bot.paused {
        out.push("This workspace is paused: its routines are skipped until it resumes.".into());
    }
    let mut routines: Vec<&BotRoutine> = records
        .routines
        .iter()
        .filter(|r| r.bot_id == bot.id)
        .collect();
    routines.sort_by(|a, b| a.name.cmp(&b.name));
    if routines.is_empty() {
        out.push("No routines yet.".into());
    } else {
        out.push("Routines:".into());
        for r in routines {
            let mut line = format!(
                "- \"{}\" (id {}) · {} · {} · {}",
                r.name,
                r.id,
                r.schedule.describe(),
                next(r.enabled, &r.schedule, r.next_run_at),
                last(r.last_run.as_ref())
            );
            if r.write_notes {
                line.push_str(" · writes notes");
            }
            out.push(line);
        }
    }
    let mut tasks: Vec<(&ProjectTask, &p::Space)> = records
        .tasks
        .iter()
        .filter_map(|t| {
            let space = spaces.iter().find(|s| s.id == t.space_id)?;
            bot.scope.sees_space(&space.id).then_some((t, space))
        })
        .collect();
    tasks.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    if tasks.is_empty() {
        out.push("No recurring tasks in your projects.".into());
    } else {
        out.push("Recurring tasks in your projects:".into());
        for (t, space) in tasks {
            let mut line = format!(
                "- \"{}\" in {} on {} (id {}) · {} · {} · {}",
                t.name,
                super::prompt::space_name(space),
                device(&space.device_id),
                t.id,
                t.schedule.describe(),
                next(t.enabled, &t.schedule, t.next_run_at),
                last(t.last_run.as_ref())
            );
            if let Some(chat) = t.last_run.as_ref().and_then(|r| r.chat_id.as_deref()) {
                line.push_str(&format!(" · session {chat}"));
            }
            if t.worktree {
                line.push_str(" · new worktree each run");
            }
            out.push(line);
        }
    }
    out.join("\n")
}
