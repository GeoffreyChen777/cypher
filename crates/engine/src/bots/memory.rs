//! Routines (docs/design/agent-bots.md §8, P3): the scheduler and the
//! routine tools. A workspace's memory, which replaced its notebook, is
//! `crate::memory` (docs/design/memory.md).
//!
//! Routines are registry rows (`cypher_proto::bot_records`). The tools here
//! read and write them for a bot's threads; what a thread may do follows its
//! origin and thread kind ([`access`]), checked on every call. The scheduler
//! that runs routines is [`host`].

use chrono::{DateTime, Local, Utc};
use serde::Deserialize;

use cypher_proto::{
    self as p, ActivityKind, Bot, BotActivity, BotChat, BotOrigin, BotRoutine, BotThread,
    RoutineTarget, Schedule,
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

/// How far a hidden run started at `run_at` has got, from its latest Run
/// command and session status: `None` while it runs, else `(failed,
/// reason)`. The memory service settles consolidation passes with it.
pub(super) fn run_progress(
    run_at: i64,
    now_ms: i64,
    command: Option<&cypher_doc::SessionCommandEntry>,
    status: Option<p::SessionStatus>,
) -> Option<(bool, Option<String>)> {
    match schedule::progress(
        run_at,
        now_ms,
        command.map(|c| (c.status, c.resolution.as_deref())),
        status,
    ) {
        schedule::Progress::Waiting => None,
        schedule::Progress::Ended { failed, reason } => Some((failed, reason)),
    }
}

/// What one thread may do with routines (§8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Access {
    pub list_routines: bool,
    /// Saving, running now and deleting: the owner's say, so the bot chat's.
    pub save_routine: bool,
}

/// Owner and routine threads list routines; routines are made, run by hand
/// and deleted in the bot chat only (one that runs in a project also needs
/// *Coordinate*, [`Bots::save_routine`]); peer threads get nothing.
pub(super) fn access(marker: &BotChat) -> Access {
    match (marker.origin, &marker.thread) {
        (BotOrigin::Owner, BotThread::Main) => Access {
            list_routines: true,
            save_routine: true,
        },
        (BotOrigin::Owner, BotThread::Work { .. })
        | (BotOrigin::Routine, BotThread::Routine { .. }) => Access {
            list_routines: true,
            save_routine: false,
        },
        _ => Access::default(),
    }
}

impl Access {
    fn allows(self, tool: &str) -> bool {
        match tool {
            "list_routines" => self.list_routines,
            "save_routine" | "run_routine" | "delete_routine" => self.save_routine,
            _ => false,
        }
    }

    fn tools(self) -> Vec<&'static str> {
        p::BOT_SCHEDULE_TOOLS
            .into_iter()
            .filter(|tool| self.allows(tool))
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct SaveRoutine {
    #[serde(default)]
    id: Option<String>,
    name: String,
    prompt: String,
    schedule: Schedule,
    /// May write the workspace's memory (named for the notebook it
    /// replaced; `write_memory` reads the same).
    #[serde(default, alias = "write_memory")]
    write_notes: Option<bool>,
    #[serde(default)]
    work_directly: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
    /// Where it runs: a project's name or id; empty for this workspace;
    /// left out, as before (a new routine runs in the workspace).
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    worktree: Option<bool>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RoutineId {
    id: String,
}

fn args<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> Result<T, EngineError> {
    serde_json::from_value(value.clone())
        .map_err(|err| EngineError::Other(format!("Invalid arguments: {err}")))
}

fn refuse(tool: &str) -> EngineError {
    EngineError::Other(match tool {
        "save_routine" | "run_routine" | "delete_routine" => {
            "Routines are made, run by hand and deleted in the workspace chat, when your owner \
             asks."
                .into()
        }
        _ => format!("{tool} isn't available in this session."),
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
        chat: &p::Chat,
        marker: &BotChat,
        tool: &str,
        args: &serde_json::Value,
    ) -> Option<Result<String, EngineError>> {
        if !p::BOT_SCHEDULE_TOOLS.contains(&tool) {
            return None;
        }
        Some(self.schedule_tool_call(chat, marker, tool, args).await)
    }

    async fn schedule_tool_call(
        &self,
        chat: &p::Chat,
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
        if !access(marker).allows(tool) {
            return Err(refuse(tool));
        }
        match tool {
            "list_routines" => self.list_routines(&chat.id),
            "save_routine" => self.save_routine(&bot, &chat.id, args::<SaveRoutine>(raw)?),
            "run_routine" => {
                let routine = self.own_routine(&bot, args::<RoutineId>(raw)?)?;
                self.gate_owner(
                    &bot,
                    chat,
                    format!("Run the routine \"{}\" now", routine.name),
                )
                .await?;
                self.run_routine_now(&bot, &routine)
            }
            "delete_routine" => {
                let routine = self.own_routine(&bot, args::<RoutineId>(raw)?)?;
                self.gate_owner(
                    &bot,
                    chat,
                    format!("Delete the routine \"{}\"", routine.name),
                )
                .await?;
                self.delete_routine(&bot, chat, &routine)
            }
            _ => Err(refuse(tool)),
        }
    }

    /// One of `bot`'s routines, by id.
    fn own_routine(&self, bot: &Bot, args: RoutineId) -> Result<BotRoutine, EngineError> {
        let id = args.id.trim();
        self.inner
            .workspace
            .record::<BotRoutine>(p::KIND_BOT_ROUTINES, id)
            .filter(|r| r.bot_id == bot.id)
            .ok_or_else(|| {
                EngineError::Other(format!("No routine {id}. list_routines shows their ids."))
            })
    }

    /// The taint rule for an acting call in the bot chat (§4.3): after
    /// another workspace's answer, the owner approves it first.
    pub(super) async fn gate_owner(
        &self,
        bot: &Bot,
        chat: &p::Chat,
        action: String,
    ) -> Result<(), EngineError> {
        match self.inner.taint.reason(&chat.id) {
            Some(reason) => self.ask_owner(bot, chat, None, action, reason).await,
            None => Ok(()),
        }
    }

    /// Run Now: a fresh run the scheduler starts on its next pass, like the
    /// routine page's button; its result posts in this chat.
    fn run_routine_now(&self, bot: &Bot, routine: &BotRoutine) -> Result<String, EngineError> {
        let now = p::now_ms();
        let mut fields = serde_json::Map::new();
        fields.insert("runRequestedAt".into(), now.into());
        self.inner
            .workspace
            .update_record(p::KIND_BOT_ROUTINES, &routine.id, fields)?;
        self.wake_schedule();
        // A request the scheduler will skip is not a run that started.
        if let Some(reason) = schedule::skip_reason(bot.paused).or_else(|| {
            routine.target.space_id().and_then(|id| {
                schedule::project_skip(bot.capabilities.coordinate, bot.scope.sees_space(id))
            })
        }) {
            return Ok(format!("\"{}\" didn't run. {reason}", routine.name));
        }
        let mut out = vec![format!(
            "Started \"{}\": a fresh run; its result will be posted in this chat when it ends.",
            routine.name
        )];
        if schedule::is_running(routine.last_run.as_ref()) {
            out.push("A run is going now; this one starts when it ends.".into());
        }
        Ok(out.join(" "))
    }

    /// Delete a routine, like the routine page's Delete: its sessions go
    /// with it; a worktree it made stays on disk.
    fn delete_routine(
        &self,
        bot: &Bot,
        chat: &p::Chat,
        routine: &BotRoutine,
    ) -> Result<String, EngineError> {
        let workspace = &self.inner.workspace;
        workspace.delete_records(p::KIND_BOT_ROUTINES, std::slice::from_ref(&routine.id));
        for session in [&routine.thread_chat_id, &routine.manual_chat_id]
            .into_iter()
            .flatten()
        {
            if let Err(err) = workspace.delete_chat(session) {
                tracing::warn!(chat = %session, error = %err, "routine session not deleted");
            }
        }
        record_activity(
            workspace,
            &bot.id,
            ActivityKind::RoutineRan,
            &format!("Deleted the routine \"{}\"", routine.name),
            Some(&chat.id),
            None,
        );
        Ok(format!("Deleted the routine \"{}\".", routine.name))
    }

    fn list_routines(&self, chat_id: &str) -> Result<String, EngineError> {
        let view = self.view(chat_id)?;
        let records = self.inner.workspace.read_bot_records();
        Ok(render_routines(
            &view.bot,
            &records,
            &view.spaces,
            |id| view.device(id).to_string(),
            Local::now(),
        ))
    }

    fn save_routine(
        &self,
        bot: &Bot,
        chat_id: &str,
        args: SaveRoutine,
    ) -> Result<String, EngineError> {
        let workspace = &self.inner.workspace;
        let view = self.view(chat_id)?;
        let now = p::now_ms();
        let existing = match args.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(id) => Some(
                workspace
                    .record::<BotRoutine>(p::KIND_BOT_ROUTINES, id)
                    .filter(|r| r.bot_id == bot.id)
                    .ok_or_else(|| {
                        EngineError::Other(format!("No routine {id}. Use list_routines."))
                    })?,
            ),
            None => None,
        };
        let model = args
            .model
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        let target = match args.project.as_deref().map(str::trim) {
            Some("") => RoutineTarget::Workspace,
            Some(project) => {
                let spaces = view.resolve_project(Some(project))?.unwrap_or_default();
                let [space_id] = spaces.as_slice() else {
                    return Err(EngineError::Other(format!(
                        "Several projects are named \"{project}\"; pass the project id."
                    )));
                };
                let previous = existing.as_ref().and_then(|r| match &r.target {
                    RoutineTarget::Project { worktree, .. } => Some(*worktree),
                    RoutineTarget::Workspace => None,
                });
                RoutineTarget::Project {
                    space_id: space_id.clone(),
                    worktree: args.worktree.or(previous).unwrap_or(false),
                    model: None,
                }
            }
            None => existing
                .as_ref()
                .map(|r| r.target.clone())
                .unwrap_or_default(),
        };
        // The model lives on the routine itself, whichever way it runs.
        let model = model.or_else(|| {
            existing
                .as_ref()
                .and_then(|r| r.chosen_model().map(str::to_string))
        });
        let target = match target {
            RoutineTarget::Project {
                space_id, worktree, ..
            } => RoutineTarget::Project {
                space_id,
                worktree,
                model: None,
            },
            other => other,
        };
        if let Some(space_id) = target.space_id() {
            if !bot.capabilities.coordinate {
                return Err(EngineError::Other(
                    "Running a routine in a project needs the workspace's Coordinate \
                     permission. Ask your owner to turn it on, or run it in this workspace."
                        .into(),
                ));
            }
            if view.space(Some(space_id)).is_none() {
                return Err(EngineError::Other("No such project.".into()));
            }
        }
        let mut routine = BotRoutine {
            id: existing
                .as_ref()
                .map_or_else(new_id, |routine| routine.id.clone()),
            bot_id: bot.id.clone(),
            name: args.name,
            prompt: args.prompt,
            target,
            model,
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
            created_at: existing.as_ref().map_or(now, |r| r.created_at),
            updated_at: now,
            run_requested_at: None,
            thread_chat_id: None,
            manual_chat_id: None,
            worktree_path: None,
            last_run: None,
            next_run_at: None,
        };
        routine.clamp();
        routine.validate().map_err(EngineError::Other)?;
        if matches!(routine.schedule, Schedule::Once { .. })
            && routine.schedule.next_after(&Local::now()).is_none()
        {
            return Err(EngineError::Other(
                "That time has already passed; pick a later one (list_routines shows the time \
                 now)."
                    .into(),
            ));
        }
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
        out.push(match &routine.target {
            RoutineTarget::Workspace => "It runs unattended as you, in its own thread, and posts \
                 its result in this chat."
                .to_string(),
            RoutineTarget::Project {
                space_id, worktree, ..
            } => {
                let space = view.space(Some(space_id));
                format!(
                    "It runs unattended as an ordinary session in {} on {}, {}; scheduled runs \
                     continue one session and Run now starts a fresh one. Its result is posted \
                     in this chat.",
                    space.map(super::prompt::space_name).unwrap_or_default(),
                    space
                        .map(|s| view.device(&s.device_id).to_string())
                        .unwrap_or_default(),
                    if *worktree {
                        "in its own worktree"
                    } else {
                        "in the project's checkout"
                    }
                )
            }
        });
        if routine.write_notes {
            out.push("It may write to your memory.".into());
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
}

/// `", next Mon Oct 19 09:00"` for a job that will run on its own.
fn next_words(schedule: &Schedule, enabled: bool, now: DateTime<Local>) -> String {
    match schedule.next_after(&now).filter(|_| enabled) {
        Some(next) => format!(", next run {}", next.format("%a %b %-d %H:%M")),
        None => String::new(),
    }
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
/// This track's tools a thread may call, beyond the read tools.
pub(super) fn memory_tools(marker: &BotChat, bot: Option<&Bot>) -> Vec<&'static str> {
    match bot {
        Some(_) => access(marker).tools(),
        None => Vec::new(),
    }
}

/// This track's section of the thread's system prompt: how routines work,
/// in the bot chat (where they are made).
pub(super) fn memory_prompt(marker: &BotChat) -> Option<String> {
    (marker.origin == BotOrigin::Owner && marker.is_main()).then(|| ROUTINES_LINE.to_string())
}

/// What the bot chat is told about routines.
pub(super) const ROUTINES_LINE: &str = "Routines:\n- Routines run by hand or on a schedule and \
     post their results in this chat. One runs either as you, in its own thread (\"every \
     morning, tell me what's stuck\"), or as an ordinary session in one of your projects \
     (\"every night, run the tests and fix anything flaky\"). When your owner asks for \
     something recurring, save it with save_routine; list_routines shows your routines with \
     their last results. run_routine runs one now (a fresh run that posts here when it \
     ends) and delete_routine removes one, when your owner asks.\n- Reminders are routines too. When your owner asks to be reminded \
     (\"remind me tomorrow morning to review the code in invoice-demo\"), save a routine as \
     you with a once schedule at that local time (morning is 09:00 unless they say; \
     list_routines shows the time now) and a prompt that tells you to remind them and add \
     what helps, like what changed in the project since. Say when it will come: \"I'll remind \
     you tomorrow, Tue Nov 10, at 9:00.\" Don't put reminders in memory: memory is read when \
     a run starts, and nothing would start one.";

/// The `list_routines` answer: this bot's routines, each with where it
/// runs, its schedule, next run and last result.
pub(super) fn render_routines(
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
            line.push_str(&match &r.target {
                RoutineTarget::Workspace => " · runs as you".to_string(),
                RoutineTarget::Project {
                    space_id, worktree, ..
                } => {
                    let place = spaces.iter().find(|s| &s.id == space_id).map_or_else(
                        || "a project you can't see".to_string(),
                        |s| {
                            format!(
                                "{} on {}",
                                super::prompt::space_name(s),
                                device(&s.device_id)
                            )
                        },
                    );
                    let checkout = if *worktree {
                        "its own worktree"
                    } else {
                        "the project's checkout"
                    };
                    format!(" · a session in {place}, in {checkout}")
                }
            });
            if let Some(model) = r.chosen_model() {
                line.push_str(&format!(" · model {model}"));
            }
            if r.write_notes {
                line.push_str(" · writes memory");
            }
            out.push(line);
        }
    }
    out.join("\n")
}
