//! The work host: the loop that acts on plans for the bots hosted on this
//! device. Every state lives in registry rows, so the loop is a reconcile
//! pass, run on every change of the bot records, bot rows or session rows
//! and on a tick, and it picks up where it left off after a restart.
//!
//! A work item moves through:
//!
//! ```text
//! Proposed ── decision cancel / Stop ─────────────────────────► Cancelled
//!    │ decision approve, bot not paused, Coordinate on, a slot free
//!    ▼
//! Running ── finish_work ─────────────────────────────────────► Done | Failed
//!    │  ├─ the thread's run settles without finish_work ───────► Done (its last
//!    │  │                                                          message) | Failed
//!    │  └─ Stop: interrupt the thread and its sessions ────────► Stopped
//!    └─ steps: a worker session waiting on an answer ⇄ Asking
//! ```

use std::collections::HashMap;
use std::time::Duration;

use cypher_doc::SessionCommandPayload;
use cypher_proto::{
    self as p, ActivityKind, ApprovalState, Bot, BotActivity, BotApproval, BotChat, BotOrigin,
    BotThread, BotWork, Chat, RunRequest, Session, SessionStatus, StepState, WorkState,
};
use tokio_util::sync::CancellationToken;

use super::coordinate::{commands_of, last_answer, last_error};
use super::{text, workspace_roots};
use crate::EngineError;
use crate::bots::Bots;
use crate::host::workspace_host::Fields;
use crate::util::new_id;

const TICK: Duration = Duration::from_secs(5);
/// Changes arrive in bursts (a status row, then its transcript); one pass
/// after a short pause covers them.
const SETTLE: Duration = Duration::from_millis(200);
/// Resolved approvals kept per bot.
const KEEP_APPROVALS: usize = 100;
/// A pending approval with no waiter left (its call timed out or the run
/// died) expires this long after its wait ran out.
const APPROVAL_GRACE: Duration = Duration::from_secs(60);

pub(super) async fn run(bots: Bots, cancel: CancellationToken) {
    let ws = bots.inner.workspace.clone();
    let mut records = ws.watch_bot_records();
    let mut bot_rows = ws.watch_bots();
    let mut sessions = ws.watch_session_rows();
    // Approvals the previous process was waiting on have no waiter now.
    bots.expire_approvals(true);
    let mut tick = tokio::time::interval(TICK);
    loop {
        if let Err(err) = bots.reconcile_work() {
            tracing::warn!(error = %err, "bot work pass failed");
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = changed(&mut records) => {}
            _ = changed(&mut bot_rows) => {}
            _ = changed(&mut sessions) => {}
            _ = tick.tick() => {}
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(SETTLE) => {}
        }
        records.borrow_and_update();
        bot_rows.borrow_and_update();
        sessions.borrow_and_update();
    }
}

/// A watch's next change; never, once its sender is gone (the tick still
/// runs the loop).
async fn changed<T>(rx: &mut tokio::sync::watch::Receiver<T>) {
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// The steps' states with `asking(session)` applied: a running step whose
/// session waits on an answer is Asking, and back to Running once none
/// does. `None` when nothing changes.
pub(super) fn settle_steps(
    work: &BotWork,
    asking: impl Fn(&str) -> bool,
) -> Option<Vec<StepState>> {
    let states: Vec<StepState> = work
        .steps
        .iter()
        .map(|step| {
            let waiting = step.sessions.iter().any(|id| asking(id));
            match step.state {
                StepState::Running if waiting => StepState::Asking,
                StepState::Asking if !waiting => StepState::Running,
                other => other,
            }
        })
        .collect();
    let changed = states
        .iter()
        .zip(&work.steps)
        .any(|(state, step)| *state != step.state);
    changed.then_some(states)
}

/// What to delete of one bot's records: finished work beyond
/// [`p::KEEP_FINISHED_WORK`] and activity beyond [`p::KEEP_ACTIVITY`], the
/// oldest first. Returns (work ids, activity ids).
pub(super) fn prune(work: &[&BotWork], activity: &[&BotActivity]) -> (Vec<String>, Vec<String>) {
    let mut finished: Vec<&&BotWork> = work.iter().filter(|w| !w.is_live()).collect();
    finished.sort_by_key(|w| std::cmp::Reverse(w.ended_at.unwrap_or(w.created_at)));
    let work_ids = finished
        .into_iter()
        .skip(p::KEEP_FINISHED_WORK)
        .map(|w| w.id.clone())
        .collect();
    let mut lines: Vec<&&BotActivity> = activity.iter().collect();
    lines.sort_by_key(|a| std::cmp::Reverse(a.at));
    let activity_ids = lines
        .into_iter()
        .skip(p::KEEP_ACTIVITY)
        .map(|a| a.id.clone())
        .collect();
    (work_ids, activity_ids)
}

impl Bots {
    /// One pass over the work of every bot hosted here.
    fn reconcile_work(&self) -> Result<(), EngineError> {
        let ws = &self.inner.workspace;
        let sessions: HashMap<String, Session> = ws
            .read_sessions()?
            .into_iter()
            .map(|s| (s.chat_id.clone(), s))
            .collect();
        // A thread's taint ends when its run does.
        self.inner.taint.clear_settled(|chat_id| {
            sessions
                .get(chat_id)
                .filter(|s| matches!(s.status, SessionStatus::Idle | SessionStatus::Errored))
                .map(|s| s.updated_at.timestamp_millis())
        });
        let bots: Vec<Bot> = ws
            .read_bots()?
            .into_iter()
            .filter(|bot| bot.host_device_id == self.device_id())
            .collect();
        if bots.is_empty() {
            return Ok(());
        }
        let records = ws.read_bot_records();
        for bot in &bots {
            let mut work: Vec<&BotWork> = records.work_of(&bot.id).collect();
            work.sort_by_key(|w| w.created_at);
            let mut running = 0;
            let mut approved = Vec::new();
            // One item's failure (a doc that won't open) never holds up
            // the rest; the next pass retries it.
            for w in &work {
                let step = match w.state {
                    WorkState::Proposed => match &w.decision {
                        _ if w.stop_requested_at.is_some() => self.cancel_plan(bot, w),
                        Some(decision) if !decision.approve => self.cancel_plan(bot, w),
                        Some(decision) => {
                            approved.push((decision.at, *w));
                            Ok(())
                        }
                        None => Ok(()),
                    },
                    WorkState::Running if w.stop_requested_at.is_some() => {
                        self.stop_work(bot, w, &sessions)
                    }
                    WorkState::Running => {
                        running += 1;
                        self.follow_work(bot, w, &sessions)
                    }
                    _ => Ok(()),
                };
                if let Err(err) = step {
                    tracing::warn!(work = %w.id, error = %err, "bot work item pass failed");
                }
            }
            // Approved plans start oldest decision first, while the bot may
            // start work and a slot is free.
            approved.sort_by_key(|(at, _)| *at);
            for (_, w) in approved {
                if bot.paused || !bot.capabilities.coordinate || running >= p::MAX_RUNNING_WORK {
                    break;
                }
                if let Err(err) = self.start_work(bot, w) {
                    tracing::warn!(work = %w.id, error = %err, "could not start bot work");
                }
                running += 1;
            }
            let activity: Vec<&BotActivity> = records
                .activity
                .iter()
                .filter(|a| a.bot_id == bot.id)
                .collect();
            let (work_ids, activity_ids) = prune(&work, &activity);
            for id in &work_ids {
                if let Some(thread) = work
                    .iter()
                    .find(|w| &w.id == id)
                    .and_then(|w| w.thread_chat_id.as_deref())
                    && let Err(err) = ws.delete_chat(thread)
                {
                    tracing::warn!(chat = %thread, error = %err, "could not delete a pruned thread");
                }
            }
            ws.delete_records(p::KIND_BOT_WORK, &work_ids);
            ws.delete_records(p::KIND_BOT_ACTIVITY, &activity_ids);
            let mut resolved: Vec<&BotApproval> = records
                .approvals
                .iter()
                .filter(|a| a.bot_id == bot.id && a.state != ApprovalState::Pending)
                .collect();
            resolved.sort_by_key(|a| std::cmp::Reverse(a.created_at));
            let old: Vec<String> = resolved
                .into_iter()
                .skip(KEEP_APPROVALS)
                .map(|a| a.id.clone())
                .collect();
            ws.delete_records(p::KIND_BOT_APPROVALS, &old);
        }
        self.expire_approvals(false);
        Ok(())
    }

    /// Expire this device's bots' pending approvals: all of them at start
    /// (nobody waits on them any more), else those past their wait.
    fn expire_approvals(&self, all: bool) {
        let ws = &self.inner.workspace;
        let here: Vec<String> = ws
            .read_bots()
            .unwrap_or_default()
            .into_iter()
            .filter(|b| b.host_device_id == self.device_id())
            .map(|b| b.id)
            .collect();
        let wait = *crate::util::lock(&self.inner.work.approval_wait) + APPROVAL_GRACE;
        let cutoff = p::now_ms() - wait.as_millis() as i64;
        for mut approval in ws.read_bot_records().approvals {
            if approval.state != ApprovalState::Pending
                || !here.contains(&approval.bot_id)
                || (!all && approval.created_at > cutoff)
            {
                continue;
            }
            approval.state = ApprovalState::Expired;
            if let Err(err) = ws.put_record(
                p::KIND_BOT_APPROVALS,
                &approval.id,
                &approval,
                Fields::Only(&["state"]),
            ) {
                tracing::warn!(error = %err, "could not expire an approval");
            }
        }
    }

    fn cancel_plan(&self, bot: &Bot, work: &BotWork) -> Result<(), EngineError> {
        let now = p::now_ms();
        let cancelled = self.edit_work(&work.id, |w| {
            if w.state != WorkState::Proposed {
                return false;
            }
            w.state = WorkState::Cancelled;
            w.ended_at = Some(now);
            true
        })?;
        if cancelled == Some(true) {
            self.log(
                &bot.id,
                ActivityKind::PlanCancelled,
                format!("Cancelled the plan: {}", work.title),
                Some(&work.chat_id),
                Some(&work.id),
                None,
            );
        }
        Ok(())
    }

    /// Approved: create the orchestration thread and start it on the brief.
    fn start_work(&self, bot: &Bot, work: &BotWork) -> Result<(), EngineError> {
        let thread_id = new_id();
        let now = p::now_ms();
        // Running first: a crash after this finds the thread missing and
        // creates it then.
        let started = self.edit_work(&work.id, |w| {
            if w.state != WorkState::Proposed || w.stop_requested_at.is_some() {
                return false;
            }
            w.state = WorkState::Running;
            w.thread_chat_id = Some(thread_id.clone());
            w.started_at = Some(now);
            true
        })?;
        if started != Some(true) {
            return Ok(());
        }
        let thread = self.create_thread(bot, work, &thread_id)?;
        self.log(
            &bot.id,
            ActivityKind::PlanApproved,
            format!("Started the plan: {}", work.title),
            Some(&thread_id),
            Some(&work.id),
            None,
        );
        self.dispatch_brief(bot, &work.id, &thread)?;
        tracing::info!(bot = %bot.id, work = %work.id, thread = %thread_id, "orchestration started");
        Ok(())
    }

    /// The hidden orchestration thread: an owner thread of the bot, in its
    /// folder, with the bot chat's config.
    fn create_thread(
        &self,
        bot: &Bot,
        work: &BotWork,
        thread_id: &str,
    ) -> Result<Chat, EngineError> {
        let ws = &self.inner.workspace;
        let bot_chat = ws.chat(&bot.chat_id)?;
        let folder = bot_chat
            .as_ref()
            .and_then(|c| c.cwd.clone())
            .unwrap_or_else(|| self.folder(&bot.id).to_string_lossy().into_owned());
        let now = chrono::Utc::now();
        let chat = Chat {
            id: thread_id.to_string(),
            device_id: self.device_id().to_string(),
            title: Some(p::clip(&work.title, p::TITLE_MAX)),
            archived: false,
            pinned: false,
            cwd: Some(folder),
            branch: None,
            checkout_id: None,
            config: bot_chat.and_then(|c| c.config),
            last_message_preview: None,
            last_message_at: None,
            created_at: now,
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: Some(now),
            room_gen: Some(2),
            child: None,
            bot: Some(BotChat::thread(
                &bot.id,
                BotOrigin::Owner,
                BotThread::Work {
                    work_id: work.id.clone(),
                },
            )),
        };
        ws.import_chat_row(&chat)?;
        Ok(chat)
    }

    /// Queue the thread's first run: the brief. Its message id is fixed per
    /// work item, so a repeat after a crash never shows twice.
    fn dispatch_brief(&self, bot: &Bot, work_id: &str, thread: &Chat) -> Result<(), EngineError> {
        let ws = &self.inner.workspace;
        let Some(work) = ws.record::<BotWork>(p::KIND_BOT_WORK, work_id) else {
            return Ok(());
        };
        let marker = thread
            .bot
            .clone()
            .unwrap_or_else(|| BotChat::owner(&bot.id));
        let roots = workspace_roots(ws, bot, &marker);
        let brief = text::brief(
            bot,
            &work,
            &ws.read_spaces()?,
            &ws.read_devices()?,
            self.device_id(),
            &roots,
        );
        let config = thread.config.clone().unwrap_or(p::ChatConfig {
            harness: p::HarnessId::Pi,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            sandbox: p::SandboxLevel::ReadOnly,
        });
        self.inner.doc_host.queue_command(
            &thread.id,
            SessionCommandPayload::Run {
                request: RunRequest {
                    prompt: brief,
                    harness: Some(config.harness),
                    model: config.model,
                    reasoning: config.reasoning,
                    model_options: config.model_options,
                    cwd: thread.cwd.clone().unwrap_or_default(),
                    // The engine decides a bot thread's level at dispatch.
                    sandbox: config.sandbox,
                    auto_approve: false,
                    attachments: Vec::new(),
                    pending_attachments: Vec::new(),
                    resume: None,
                    worktree: None,
                },
                message_id: format!("{work_id}-brief"),
                agent_prompt: None,
            },
        )?;
        Ok(())
    }

    /// A running orchestration: repair a missing thread or first run, settle
    /// it once its run ends, and follow its sessions' questions.
    fn follow_work(
        &self,
        bot: &Bot,
        work: &BotWork,
        sessions: &HashMap<String, Session>,
    ) -> Result<(), EngineError> {
        let ws = &self.inner.workspace;
        let thread = match &work.thread_chat_id {
            Some(id) => ws.chat(id)?,
            None => None,
        };
        let Some(thread) = thread else {
            // Approved but its thread never got written (a crash in
            // between): create it now.
            let thread_id = work.thread_chat_id.clone().unwrap_or_else(new_id);
            if work.thread_chat_id.is_none() {
                self.edit_work(&work.id, |w| w.thread_chat_id = Some(thread_id.clone()))?;
            }
            let thread = self.create_thread(bot, work, &thread_id)?;
            return self.dispatch_brief(bot, &work.id, &thread);
        };
        let handle = self.inner.doc_host.open(&thread.id)?;
        let commands = commands_of(&handle.doc().read_commands()?);
        let row = sessions.get(&thread.id);
        if row.is_none() && !commands.any {
            return self.dispatch_brief(bot, &work.id, &thread);
        }
        if let Some(reason) = &commands.refused {
            return self.settle_work(
                bot,
                work,
                Some(format!("Its run didn't start: {reason}")),
                None,
            );
        }
        let started = work.started_at.unwrap_or(work.created_at);
        if let Some(row) = row
            && matches!(row.status, SessionStatus::Idle | SessionStatus::Errored)
            && row.updated_at.timestamp_millis() >= started
            && !commands.pending
        {
            let entries = handle.doc().read_entries()?;
            let last = last_answer(&entries);
            let error = (row.status == SessionStatus::Errored).then(|| {
                last_error(&entries).unwrap_or_else(|| "The orchestration thread failed.".into())
            });
            return self.settle_work(bot, work, error, last);
        }
        let asking = |id: &str| {
            sessions
                .get(id)
                .is_some_and(|s| s.status == SessionStatus::AwaitingInput)
        };
        if let Some(states) = settle_steps(work, asking) {
            self.edit_work(&work.id, |w| {
                for (step, state) in w.steps.iter_mut().zip(&states) {
                    step.state = *state;
                }
            })?;
        }
        Ok(())
    }

    /// The thread's run ended without `finish_work`: done with its last
    /// message, or failed with `error`.
    fn settle_work(
        &self,
        bot: &Bot,
        work: &BotWork,
        error: Option<String>,
        last: Option<String>,
    ) -> Result<(), EngineError> {
        let now = p::now_ms();
        let failed = error.is_some();
        let settled = self.edit_work(&work.id, |w| {
            if w.state != WorkState::Running {
                return false;
            }
            w.state = if failed {
                WorkState::Failed
            } else {
                WorkState::Done
            };
            w.result = Some(match (&error, &last) {
                (Some(error), _) => error.clone(),
                (None, Some(last)) => last.clone(),
                (None, None) => "The orchestration ended without a result.".into(),
            });
            w.ended_at = Some(now);
            true
        })?;
        if settled == Some(true) {
            let (kind, line) = if failed {
                (ActivityKind::WorkFailed, "The plan failed")
            } else {
                (ActivityKind::WorkFinished, "Finished the plan")
            };
            self.log(
                &bot.id,
                kind,
                format!("{line}: {}", work.title),
                work.thread_chat_id.as_deref(),
                Some(&work.id),
                None,
            );
        }
        Ok(())
    }

    /// Stop: interrupt the thread and every session it started that still
    /// runs, and skip the steps not done.
    fn stop_work(
        &self,
        bot: &Bot,
        work: &BotWork,
        sessions: &HashMap<String, Session>,
    ) -> Result<(), EngineError> {
        let docs = &self.inner.doc_host;
        if let Some(thread) = &work.thread_chat_id
            && let Err(err) = docs.queue_command(thread, SessionCommandPayload::Interrupt {})
        {
            tracing::warn!(work = %work.id, error = %err, "could not interrupt the thread");
        }
        for id in work.session_ids() {
            let live = sessions.get(id).is_some_and(|s| {
                matches!(
                    s.status,
                    SessionStatus::Working | SessionStatus::AwaitingInput
                )
            });
            // A session whose row hasn't arrived may still be starting.
            if (live || !sessions.contains_key(id))
                && let Err(err) = docs.queue_command(id, SessionCommandPayload::Interrupt {})
            {
                tracing::warn!(work = %work.id, session = %id, error = %err, "could not interrupt a session");
            }
        }
        let now = p::now_ms();
        let stopped = self.edit_work(&work.id, |w| {
            if w.state != WorkState::Running {
                return false;
            }
            w.state = WorkState::Stopped;
            w.ended_at = Some(now);
            for step in &mut w.steps {
                if !matches!(
                    step.state,
                    StepState::Done | StepState::Failed | StepState::Skipped
                ) {
                    step.state = StepState::Skipped;
                    step.ended_at = Some(now);
                }
            }
            true
        })?;
        if stopped == Some(true) {
            self.log(
                &bot.id,
                ActivityKind::WorkStopped,
                format!("Stopped the plan: {}", work.title),
                work.thread_chat_id.as_deref(),
                Some(&work.id),
                None,
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, state: StepState, sessions: &[&str]) -> p::WorkStep {
        p::WorkStep {
            id: id.into(),
            title: format!("step {id}"),
            project: None,
            detail: None,
            state,
            sessions: sessions.iter().map(|s| s.to_string()).collect(),
            note: None,
            started_at: None,
            ended_at: None,
        }
    }

    fn work(id: &str, state: WorkState, at: i64) -> BotWork {
        BotWork {
            id: id.into(),
            bot_id: "b".into(),
            title: id.into(),
            instruction: String::new(),
            summary: None,
            steps: Vec::new(),
            state,
            chat_id: "c".into(),
            tool_call_id: None,
            thread_chat_id: None,
            created_at: at,
            started_at: None,
            ended_at: Some(at),
            result: None,
            decision: None,
            stop_requested_at: None,
        }
    }

    #[test]
    fn steps_ask_while_a_session_waits() {
        let mut w = work("w", WorkState::Running, 0);
        w.steps = vec![
            step("1", StepState::Running, &["s1"]),
            step("2", StepState::Asking, &["s2"]),
            step("3", StepState::Done, &["s3"]),
        ];
        let states = settle_steps(&w, |id| id == "s1" || id == "s3").unwrap();
        assert_eq!(
            states,
            [StepState::Asking, StepState::Running, StepState::Done],
            "a finished step stays finished"
        );
        w.steps[0].state = StepState::Asking;
        w.steps[1].state = StepState::Running;
        assert!(
            settle_steps(&w, |id| id == "s1").is_none(),
            "nothing changes"
        );
    }

    #[test]
    fn pruning_keeps_the_newest_and_every_live_item() {
        let mut items: Vec<BotWork> = (0..(p::KEEP_FINISHED_WORK as i64 + 5))
            .map(|i| work(&format!("done-{i}"), WorkState::Done, i))
            .collect();
        items.push(work("live", WorkState::Running, -1));
        items.push(work("proposed", WorkState::Proposed, -2));
        let activity: Vec<BotActivity> = (0..(p::KEEP_ACTIVITY as i64 + 3))
            .map(|i| BotActivity {
                id: format!("a-{i}"),
                bot_id: "b".into(),
                at: i,
                kind: ActivityKind::SessionStarted,
                text: String::new(),
                chat_id: None,
                work_id: None,
                space_id: None,
            })
            .collect();
        let (work_ids, activity_ids) = prune(
            &items.iter().collect::<Vec<_>>(),
            &activity.iter().collect::<Vec<_>>(),
        );
        assert_eq!(work_ids, ["done-4", "done-3", "done-2", "done-1", "done-0"]);
        assert_eq!(activity_ids, ["a-2", "a-1", "a-0"]);
    }
}
