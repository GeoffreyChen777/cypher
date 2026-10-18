//! The scheduler (docs/design/agent-bots.md §8): runs this device's routines.
//!
//! Everything it knows lives in the rows, so it survives restarts: a pass
//! reads the routines this device runs (a workspace routine where its bot is
//! hosted, a project routine where its project is), writes each one's
//! `nextRunAt`, starts what is due by queueing a durable Run command, and
//! settles runs that ended. A scheduled run continues the routine's standing
//! session; a Run now gets a fresh one, which replaces the last. It wakes every 30 s, on
//! record and bot changes, when a tool saves a job, and while a run it
//! started is in flight, on session status changes.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use cypher_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry,
    join_continuation_entries,
};
use cypher_proto::{
    self as p, ActivityKind, Bot, BotChat, BotOrigin, BotPost, BotRoutine, BotThread, Chat,
    RoutineTarget, RunOutcome, RunRecord, SessionStatus, Space,
};

use super::super::Bots;
use super::record_activity;
use super::schedule::{self, Progress};
use crate::EngineError;
use crate::host::workspace_host::Fields;
use crate::session::engine::UnattendedListener;
use crate::util::{lock, new_id};

/// How often the scheduler looks without being woken.
const TICK: Duration = Duration::from_secs(30);
/// Lets a burst of row writes land before the next pass.
const SETTLE: Duration = Duration::from_millis(250);
/// The longest routine output posted into the bot chat.
const POST_MAX_CHARS: usize = 12_000;
/// A settled run's output is posted only this long after it ended (a
/// restart long after, or a bot chat busy all day, drops it).
const POST_WITHIN_MS: i64 = 24 * 3_600_000;

/// The scheduler's in-memory state; the rows hold everything durable.
#[derive(Default)]
pub(in crate::bots) struct ScheduleState {
    wake: Notify,
    /// One pass at a time: two would both start a due job.
    pass: tokio::sync::Mutex<()>,
    /// Unattended stops by chat id, waiting for the pass that settles the run.
    stops: Mutex<HashMap<String, String>>,
    /// Post entry ids known to be in their bot chat.
    posted: Mutex<HashSet<String>>,
    /// A run is in flight or a post waits for its bot chat: wake on session
    /// status changes too.
    watching: AtomicBool,
}

/// What a run left in its thread or session.
#[derive(Debug, Default)]
struct RunResult {
    /// The last answer's text.
    text: Option<String>,
    /// The last error the run showed.
    error: Option<String>,
    /// It was stopped mid-answer (an interrupt, an engine restart).
    aborted: bool,
}

impl Bots {
    /// The engine's unattended-stop listener: a routine run that asked for
    /// input is settled as failed with the reason.
    pub fn unattended_listener(&self) -> UnattendedListener {
        let inner = Arc::downgrade(&self.inner);
        Arc::new(move |stop| {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            lock(&inner.schedule.stops).insert(stop.chat_id.clone(), stop.reason.clone());
            inner.schedule.wake.notify_one();
        })
    }

    /// Run a scheduler pass soon.
    pub(super) fn wake_schedule(&self) {
        self.inner.schedule.wake.notify_one();
    }

    /// Run the scheduler pass now (tests and callers that just wrote rows).
    pub async fn schedule_now(&self) {
        self.schedule_pass().await;
    }

    pub(super) async fn run_schedule_host(&self, cancel: CancellationToken) {
        let workspace = &self.inner.workspace;
        let mut records = Some(workspace.watch_bot_records());
        let mut bots = Some(workspace.watch_bots());
        let mut sessions = self
            .inner
            .doc_host
            .sessions()
            .map(|sessions| sessions.watch_sessions());
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            self.schedule_pass().await;
            let watching = self.inner.schedule.watching.load(Ordering::Relaxed);
            tokio::select! {
                () = cancel.cancelled() => return,
                _ = tick.tick() => {}
                () = changed(&mut records) => {}
                () = changed(&mut bots) => {}
                () = changed(&mut sessions), if watching => {}
                () = self.inner.schedule.wake.notified() => {}
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(SETTLE) => {}
            }
            if let Some(rx) = &mut records {
                rx.borrow_and_update();
            }
            if let Some(rx) = &mut bots {
                rx.borrow_and_update();
            }
            if let Some(rx) = &mut sessions {
                rx.borrow_and_update();
            }
        }
    }

    /// One look at every job this device runs.
    async fn schedule_pass(&self) {
        let _pass = self.inner.schedule.pass.lock().await;
        let workspace = &self.inner.workspace;
        let records = workspace.read_bot_records();
        let bots: HashMap<String, Bot> = workspace
            .read_bots()
            .unwrap_or_default()
            .into_iter()
            .map(|bot| (bot.id.clone(), bot))
            .collect();
        let spaces = workspace.read_spaces().unwrap_or_default();
        let now = Local::now();
        let mut watching = false;
        for routine in &records.routines {
            let Some(bot) = bots.get(&routine.bot_id) else {
                continue;
            };
            // A workspace routine runs where its bot is hosted, a project
            // routine where its project is.
            let space = match routine.target.space_id() {
                None if bot.host_device_id == self.device_id() => None,
                None => continue,
                Some(id) => match spaces.iter().find(|s| s.id == id) {
                    Some(space) if space.device_id == self.device_id() => Some(space),
                    _ => continue,
                },
            };
            match self.tend_routine(bot, routine, space, &now).await {
                Ok(watch) => watching |= watch,
                Err(err) => {
                    tracing::warn!(routine = %routine.id, error = %err, "routine pass failed")
                }
            }
        }
        self.inner
            .schedule
            .watching
            .store(watching, Ordering::Relaxed);
    }

    /// Settle, post, plan and start one routine; `true` while the scheduler
    /// should watch session changes for it.
    async fn tend_routine(
        &self,
        bot: &Bot,
        routine: &BotRoutine,
        space: Option<&Space>,
        now: &DateTime<Local>,
    ) -> Result<bool, EngineError> {
        let now_ms = now.timestamp_millis();
        let mut routine = routine.clone();
        let mut write = self.learn_worktree(&mut routine, space);
        let mut watching = false;
        if let Some(run) = routine
            .last_run
            .clone()
            .filter(|r| r.outcome == RunOutcome::Running)
        {
            match self.run_progress(&run, now_ms) {
                Progress::Waiting => watching = true,
                Progress::Ended { failed, reason } => {
                    let settled = self.settle(&run, failed, reason, now_ms);
                    record_activity(
                        &self.inner.workspace,
                        &bot.id,
                        ActivityKind::RoutineRan,
                        &match settled.outcome {
                            RunOutcome::Failed => format!(
                                "\"{}\" failed: {}",
                                routine.name,
                                settled.summary.as_deref().unwrap_or_default()
                            ),
                            _ => format!("Ran \"{}\"", routine.name),
                        },
                        settled.chat_id.as_deref(),
                        None,
                    );
                    routine.last_run = Some(settled);
                    write = true;
                }
            }
        }
        if let Some(run) = routine.last_run.clone() {
            // Written first, so a post never lands for a run whose row still
            // says it is running.
            if write {
                self.write_routine(&mut routine)?;
                write = false;
            }
            watching |= self.post_routine_output(bot, &routine, &run, now_ms);
        }

        let running = schedule::is_running(routine.last_run.as_ref());
        let mut next =
            schedule::planned_next(&routine.schedule, routine.enabled, routine.next_run_at, now);
        let due = schedule::due(
            next,
            routine.run_requested_at,
            routine.last_run.as_ref(),
            now_ms,
        );
        let after = || {
            routine
                .schedule
                .next_after(now)
                .filter(|_| routine.enabled)
                .map(|at| at.timestamp_millis())
        };
        match due {
            // Never two runs at once: a slot that comes due while the last
            // run is going is skipped; a request waits for it to end.
            Some(schedule::Due::Scheduled) if running => next = after(),
            Some(_) if running => {}
            None => {}
            Some(due) => {
                let manual = due == schedule::Due::Requested;
                let today = now.format("%Y-%m-%d").to_string();
                let runs =
                    schedule::runs_on(&today, routine.runs_today, routine.runs_day.as_deref());
                next = after();
                routine.next_run_at = next;
                let reason = schedule::skip_reason(bot.paused, runs, routine.max_runs_per_day)
                    .or_else(|| {
                        routine.target.space_id().and_then(|id| {
                            schedule::project_skip(
                                bot.capabilities.coordinate,
                                bot.scope.sees_space(id),
                            )
                        })
                    });
                match reason {
                    Some(reason) => {
                        routine.last_run = Some(RunRecord {
                            at: now_ms,
                            ended_at: Some(now_ms),
                            outcome: RunOutcome::Skipped,
                            summary: Some(reason),
                            chat_id: None,
                            manual,
                        });
                        self.write_routine(&mut routine)?;
                    }
                    None => {
                        routine.runs_today = runs + 1;
                        routine.runs_day = Some(today);
                        self.start_routine(bot, &mut routine, space, manual, now)
                            .await?;
                        watching = true;
                    }
                }
                return Ok(watching);
            }
        }
        if next != routine.next_run_at {
            routine.next_run_at = next;
            write = true;
        }
        if write {
            self.write_routine(&mut routine)?;
        }
        Ok(watching)
    }

    fn write_routine(&self, routine: &mut BotRoutine) -> Result<(), EngineError> {
        routine.clamp();
        self.inner.workspace.put_record(
            p::KIND_BOT_ROUTINES,
            &routine.id,
            &*routine,
            Fields::Only(BotRoutine::HOST_FIELDS),
        )
    }

    /// A project routine's own worktree, learned from its session once the
    /// run that made it has started (the drain points the chat at it).
    /// `true` when the row needs writing.
    fn learn_worktree(&self, routine: &mut BotRoutine, space: Option<&Space>) -> bool {
        let (Some(space), RoutineTarget::Project { worktree: true, .. }) = (space, &routine.target)
        else {
            return false;
        };
        if routine
            .worktree_path
            .as_deref()
            .is_some_and(|path| std::path::Path::new(path).is_dir())
        {
            return false;
        }
        let Some(cwd) = routine
            .last_run
            .as_ref()
            .and_then(|run| run.chat_id.as_deref())
            .and_then(|id| self.inner.workspace.chat(id).ok().flatten())
            .and_then(|chat| chat.cwd)
            .filter(|cwd| *cwd != space.path && std::path::Path::new(cwd).is_dir())
        else {
            return false;
        };
        routine.worktree_path = Some(cwd);
        true
    }

    /// Start one run: a scheduled one in the routine's standing session, a
    /// Run now in a fresh one that replaces (and deletes) the last. Record
    /// it, then queue the Run.
    async fn start_routine(
        &self,
        bot: &Bot,
        routine: &mut BotRoutine,
        space: Option<&Space>,
        manual: bool,
        now: &DateTime<Local>,
    ) -> Result<(), EngineError> {
        let now_ms = now.timestamp_millis();
        let standing = if manual {
            None
        } else {
            routine.thread_chat_id.clone()
        };
        let session = match space {
            None => self.routine_thread(bot, routine, standing.as_deref())?,
            Some(space) => self.routine_session(bot, routine, space, standing.as_deref())?,
        };
        if manual {
            let previous = routine.manual_chat_id.replace(session.clone());
            if let Some(previous) = previous.filter(|id| {
                *id != session && routine.thread_chat_id.as_deref() != Some(id.as_str())
            }) && let Err(err) = self.inner.workspace.delete_chat(&previous)
            {
                tracing::warn!(chat = %previous, error = %err, "old manual run not deleted");
            }
        } else {
            routine.thread_chat_id = Some(session.clone());
        }
        routine.last_run = Some(RunRecord {
            at: now_ms,
            ended_at: None,
            outcome: RunOutcome::Running,
            summary: None,
            chat_id: Some(session.clone()),
            manual,
        });
        self.write_routine(routine)?;
        let prompt = format!(
            "Routine \"{}\" · {}\n\n{}",
            routine.name,
            now.format("%A %B %-d, %Y, %H:%M"),
            routine.prompt
        );
        let queued = match space {
            None => self
                .inner
                .doc_host
                .request_from_chat_row(&session, &prompt)
                .ok_or_else(|| EngineError::Other("the routine thread has no row".into()))
                .map(|request| (request, None)),
            Some(space) => Ok(self
                .project_request(routine, space, &session, &prompt)
                .await),
        }
        .and_then(|(request, agent_prompt)| {
            self.inner.doc_host.queue_command(
                &session,
                SessionCommandPayload::Run {
                    request,
                    message_id: new_id(),
                    agent_prompt,
                },
            )
        });
        if let Err(err) = queued {
            tracing::warn!(routine = %routine.id, error = %err, "routine run did not queue");
            let run = routine.last_run.clone().expect("just recorded");
            routine.last_run = Some(self.settle(
                &run,
                true,
                Some(format!("The run couldn't start: {err}.")),
                p::now_ms(),
            ));
            self.write_routine(routine)?;
        } else {
            tracing::info!(routine = %routine.id, chat = %session, manual, "routine run queued");
        }
        Ok(())
    }

    /// A project routine's Run: an ordinary unattended session's request,
    /// in the routine's own worktree (made by this run when it has none yet)
    /// or the project's checkout.
    async fn project_request(
        &self,
        routine: &BotRoutine,
        space: &Space,
        session: &str,
        prompt: &str,
    ) -> (p::RunRequest, Option<String>) {
        let (worktree, model) = match &routine.target {
            RoutineTarget::Project {
                worktree, model, ..
            } => (*worktree, model.clone()),
            RoutineTarget::Workspace => (false, None),
        };
        let own = routine
            .worktree_path
            .clone()
            .filter(|path| std::path::Path::new(path).is_dir());
        let (cwd, spec) = match (worktree, own) {
            (true, Some(path)) => (path, None),
            (true, None) => {
                let base = self
                    .inner
                    .repos
                    .current_branch(std::path::Path::new(&space.path))
                    .await
                    .ok()
                    .filter(|b| !b.is_empty() && b != "HEAD")
                    .unwrap_or_else(|| "HEAD".into());
                (
                    space.path.clone(),
                    Some(p::WorktreeSpec {
                        repo_path: space.path.clone(),
                        base_ref: base,
                        name_hint: Some(routine.name.clone()),
                    }),
                )
            }
            (false, _) => (space.path.clone(), None),
        };
        // A standing session follows the routine's current checkout.
        if spec.is_none()
            && let Ok(Some(chat)) = self.inner.workspace.chat(session)
            && chat.cwd.as_deref() != Some(cwd.as_str())
            && let Err(err) = self.inner.workspace.set_chat_cwd(session, &cwd)
        {
            tracing::warn!(chat = %session, error = %err, "routine session cwd not moved");
        }
        let request = p::RunRequest {
            prompt: prompt.to_string(),
            harness: None,
            model,
            reasoning: None,
            model_options: Default::default(),
            cwd,
            sandbox: p::SandboxLevel::WorkspaceWrite,
            auto_approve: false,
            attachments: Vec::new(),
            pending_attachments: Vec::new(),
            resume: None,
            worktree: spec,
        };
        let agent_prompt = format!(
            "This is a run of the routine \"{}\". Nobody is watching it: you cannot ask \
             questions or wait for an approval, and anything that would ask stops the run as \
             failed. When something is unclear, make the reasonable choice and say what you \
             assumed.\n\n{}",
            routine.name, routine.prompt
        );
        (request, Some(agent_prompt))
    }

    /// A workspace routine's hidden thread: `standing` while it is still
    /// this routine's, else a new one (origin routine, in the bot's folder
    /// with the bot chat's settings, titled with the routine's name).
    fn routine_thread(
        &self,
        bot: &Bot,
        routine: &BotRoutine,
        standing: Option<&str>,
    ) -> Result<String, EngineError> {
        let workspace = &self.inner.workspace;
        let marker = BotChat::thread(
            &bot.id,
            BotOrigin::Routine,
            BotThread::Routine {
                routine_id: routine.id.clone(),
            },
        );
        if let Some(id) = self.reusable(standing, &marker, &routine.name)? {
            return Ok(id);
        }
        let folder = self.inner.root.join(&bot.id);
        super::super::create_private_dir(&folder)?;
        let config = workspace.chat(&bot.chat_id)?.and_then(|chat| chat.config);
        let chat = hidden_chat(
            self.device_id(),
            &routine.name,
            folder.to_string_lossy().into_owned(),
            None,
            config,
            marker,
        );
        // The plain row write: only the engine writes a thread's marker.
        workspace.import_chat_row(&chat)?;
        tracing::info!(routine = %routine.id, thread = %chat.id, "routine thread created");
        Ok(chat.id)
    }

    /// A project routine's session: `standing` while it is still this
    /// routine's, else a new ordinary session in the project, hidden by its
    /// marker and run as a plain session ([`super::super::profile::bot_env`]
    /// gives it none of the bot's).
    fn routine_session(
        &self,
        bot: &Bot,
        routine: &BotRoutine,
        space: &Space,
        standing: Option<&str>,
    ) -> Result<String, EngineError> {
        let marker = BotChat::thread(
            &bot.id,
            BotOrigin::Routine,
            BotThread::RoutineSession {
                routine_id: routine.id.clone(),
            },
        );
        if let Some(id) = self.reusable(standing, &marker, &routine.name)? {
            return Ok(id);
        }
        let chat = hidden_chat(
            self.device_id(),
            &routine.name,
            space.path.clone(),
            Some(space.id.clone()),
            None,
            marker,
        );
        self.inner.workspace.import_chat_row(&chat)?;
        tracing::info!(routine = %routine.id, session = %chat.id, "routine session created");
        Ok(chat.id)
    }

    /// `standing` when it is still a chat with `marker`, renamed to `name`.
    fn reusable(
        &self,
        standing: Option<&str>,
        marker: &BotChat,
        name: &str,
    ) -> Result<Option<String>, EngineError> {
        let workspace = &self.inner.workspace;
        let Some(id) = standing else {
            return Ok(None);
        };
        let Some(chat) = workspace.chat(id)? else {
            return Ok(None);
        };
        if chat.bot.as_ref() != Some(marker) {
            return Ok(None);
        }
        if chat.title.as_deref() != Some(name) {
            workspace.rename_chat(id, name)?;
        }
        Ok(Some(chat.id))
    }

    /// How far a recorded run has got.
    fn run_progress(&self, run: &RunRecord, now_ms: i64) -> Progress {
        let Some(chat_id) = run.chat_id.as_deref() else {
            return Progress::Ended {
                failed: true,
                reason: Some("The run never started.".into()),
            };
        };
        // Opening the doc also drains a command queued before a restart.
        let commands = match self.inner.doc_host.open(chat_id) {
            Ok(handle) => handle.doc().read_commands().unwrap_or_default(),
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "run doc open failed");
                return Progress::Waiting;
            }
        };
        let command = commands.iter().rev().find(|c| {
            c.issued_at >= run.at && matches!(c.payload, SessionCommandPayload::Run { .. })
        });
        let status = self
            .inner
            .doc_host
            .sessions()
            .and_then(|sessions| sessions.session_status(chat_id))
            .map(|session| session.status);
        schedule::progress(
            run.at,
            now_ms,
            command.map(|c| (c.status, c.resolution.as_deref())),
            status,
        )
    }

    /// The record of a run that ended: done with its last answer, or failed
    /// with the best reason known.
    fn settle(
        &self,
        run: &RunRecord,
        failed: bool,
        reason: Option<String>,
        now_ms: i64,
    ) -> RunRecord {
        let result = run
            .chat_id
            .as_deref()
            .map(|chat| self.run_result(chat, run.at))
            .unwrap_or_default();
        let stop = run
            .chat_id
            .as_deref()
            .and_then(|chat| lock(&self.inner.schedule.stops).remove(chat));
        // A run with no answer was cut off (stopped, or the engine went
        // down mid-run): there is nothing to deliver.
        let silent = result.text.is_none();
        let failed = failed || stop.is_some() || result.aborted || silent;
        let summary = if failed {
            Some(stop.or(reason).or(result.error).unwrap_or_else(|| {
                if result.aborted || silent {
                    "The run was stopped before it finished.".into()
                } else {
                    "The run failed.".into()
                }
            }))
        } else {
            result.text
        };
        let mut settled = RunRecord {
            at: run.at,
            ended_at: Some(now_ms),
            outcome: if failed {
                RunOutcome::Failed
            } else {
                RunOutcome::Done
            },
            summary,
            chat_id: run.chat_id.clone(),
            manual: run.manual,
        };
        settled.clamp();
        tracing::info!(chat = ?settled.chat_id, outcome = ?settled.outcome, "scheduled run ended");
        settled
    }

    /// The last answer and error of the run that started at `run_at`.
    fn run_result(&self, chat_id: &str, run_at: i64) -> RunResult {
        let entries = match self.inner.doc_host.read_transcript(chat_id) {
            Ok(Some(entries)) => entries,
            _ => return RunResult::default(),
        };
        run_result(entries, run_at)
    }

    /// Post a settled run's output into the bot chat, once, as a `post`
    /// part attributed to the routine. A bot chat in the middle of a turn
    /// gets it after the turn (`true`: watch for that).
    fn post_routine_output(
        &self,
        bot: &Bot,
        routine: &BotRoutine,
        run: &RunRecord,
        now_ms: i64,
    ) -> bool {
        let outcome = run.outcome;
        if !matches!(outcome, RunOutcome::Done | RunOutcome::Failed) {
            return false;
        }
        if run
            .ended_at
            .is_none_or(|ended| now_ms - ended > POST_WITHIN_MS)
        {
            return false;
        }
        let entry_id = post_entry_id(&routine.id, run.at);
        if lock(&self.inner.schedule.posted).contains(&entry_id) {
            return false;
        }
        let busy = self
            .inner
            .doc_host
            .sessions()
            .and_then(|sessions| sessions.session_status(&bot.chat_id))
            .is_some_and(|s| {
                matches!(
                    s.status,
                    SessionStatus::Working | SessionStatus::AwaitingInput
                )
            });
        if busy {
            return true;
        }
        let handle = match self.inner.doc_host.open(&bot.chat_id) {
            Ok(handle) => handle,
            Err(err) => {
                tracing::warn!(bot = %bot.id, error = %err, "bot chat open failed");
                return false;
            }
        };
        let already = handle
            .doc()
            .read_entries()
            .is_ok_and(|entries| entries.iter().any(|e| e.id == entry_id));
        if !already {
            let text = match outcome {
                RunOutcome::Done => run
                    .chat_id
                    .as_deref()
                    .and_then(|chat| self.run_result(chat, run.at).text)
                    .or_else(|| run.summary.clone())
                    .unwrap_or_else(|| "The routine finished without a message.".into()),
                _ => run
                    .summary
                    .clone()
                    .unwrap_or_else(|| "The run failed.".into()),
            };
            let text = p::clip(&text, POST_MAX_CHARS);
            let entry = post_entry(
                &entry_id,
                &text,
                BotPost::Routine {
                    routine_id: routine.id.clone(),
                    name: routine.name.clone(),
                    outcome,
                    run_at: run.at,
                    thread_chat_id: run.chat_id.clone(),
                },
                self.inner.doc_host.device_id(),
                now_ms,
            );
            if let Err(err) = handle.doc().push_message(&entry) {
                tracing::warn!(bot = %bot.id, error = %err, "routine post failed");
                return false;
            }
            self.inner.doc_host.flush_chat_sync(&bot.chat_id);
            self.inner
                .workspace
                .note_message(&bot.chat_id, &format!("{}: {text}", routine.name));
            tracing::info!(bot = %bot.id, routine = %routine.id, "routine output posted");
        }
        lock(&self.inner.schedule.posted).insert(entry_id);
        false
    }
}

/// A hidden routine chat row on this device.
fn hidden_chat(
    device_id: &str,
    title: &str,
    cwd: String,
    space_id: Option<String>,
    config: Option<p::ChatConfig>,
    marker: BotChat,
) -> Chat {
    let now = Utc::now();
    Chat {
        id: new_id(),
        device_id: device_id.to_string(),
        title: Some(title.to_string()),
        archived: false,
        pinned: false,
        cwd: Some(cwd),
        branch: None,
        checkout_id: None,
        config,
        last_message_preview: None,
        last_message_at: None,
        created_at: now,
        harness_session_id: None,
        harness_session_cwd: None,
        space_id,
        last_seen_at: Some(now),
        room_gen: Some(2),
        child: None,
        bot: Some(marker),
    }
}

/// The bot-chat entry id of one routine run's post: one per run, so a
/// restart never posts twice.
pub(super) fn post_entry_id(routine_id: &str, run_at: i64) -> String {
    format!("routine-{routine_id}-{run_at}")
}

/// The complete assistant entry a post is.
pub(super) fn post_entry(
    id: &str,
    text: &str,
    from: BotPost,
    device_id: &str,
    now_ms: i64,
) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.to_string(),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Post {
            id: "p0".into(),
            text: text.to_string(),
            from,
        }],
        created_at: now_ms,
        device_id: device_id.to_string(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
        completed_at: Some(now_ms),
        comments: Vec::new(),
        models: Vec::new(),
        turn_changes: None,
        recovery: None,
    }
}

/// The last answer and error among the entries of a run that started at
/// `run_at` (the run's own prompt and everything after it).
fn run_result(entries: Vec<SessionMessageEntry>, run_at: i64) -> RunResult {
    let mut result = RunResult::default();
    for entry in join_continuation_entries(entries)
        .into_iter()
        .filter(|e| e.created_at >= run_at && e.role == MessageRole::Assistant)
    {
        let text: Vec<&str> = entry
            .parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .filter(|text| !text.trim().is_empty())
            .collect();
        if let Some(last) = text.last() {
            result.text = Some(last.trim().to_string());
        }
        if let Some(error) = entry.parts.iter().rev().find_map(|part| match part {
            MessagePart::Error { message, .. } if !message.trim().is_empty() => {
                Some(message.trim().to_string())
            }
            _ => None,
        }) {
            result.error = Some(error);
        }
        result.aborted = entry.status == Some(MessageStatus::Aborted);
    }
    result
}

/// Resolves when `rx` changes; never, once its sender is gone.
async fn changed<T>(rx: &mut Option<watch::Receiver<T>>) {
    match rx {
        Some(receiver) => {
            if receiver.changed().await.is_err() {
                *rx = None;
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, role: MessageRole, at: i64, parts: Vec<MessagePart>) -> SessionMessageEntry {
        let mut e = post_entry(id, "", BotPost::Unknown, "dev", at);
        e.role = role;
        e.parts = parts;
        e
    }

    fn text(t: &str) -> MessagePart {
        MessagePart::Text {
            id: "t".into(),
            text: t.into(),
            agent_text: None,
        }
    }

    #[test]
    fn a_runs_result_is_its_last_answer_and_error() {
        let entries = vec![
            entry("old", MessageRole::Assistant, 5, vec![text("yesterday")]),
            entry("u", MessageRole::User, 10, vec![text("Routine")]),
            entry(
                "a",
                MessageRole::Assistant,
                11,
                vec![text("Looking."), text("Two sessions are stuck.")],
            ),
        ];
        let result = run_result(entries.clone(), 10);
        assert_eq!(result.text.as_deref(), Some("Two sessions are stuck."));
        assert!(result.error.is_none() && !result.aborted);

        let mut failed = entries;
        failed.push(entry(
            "b",
            MessageRole::Assistant,
            12,
            vec![MessagePart::Error {
                id: "e".into(),
                message: "This run is unattended, so it couldn't ask a question. It stopped."
                    .into(),
            }],
        ));
        failed.last_mut().unwrap().status = Some(MessageStatus::Aborted);
        let result = run_result(failed, 10);
        assert!(result.aborted);
        assert!(result.error.unwrap().starts_with("This run is unattended"));
        assert!(run_result(vec![], 10).text.is_none());
    }
}
