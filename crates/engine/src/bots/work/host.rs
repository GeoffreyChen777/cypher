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
//!    │  │                                    message) | Failed (errored, cut off
//!    │  │                                    mid-answer, or nothing said)
//!    │  └─ Stop: interrupt the thread and its sessions ────────► Stopped
//!    └─ steps: a worker session waiting on an answer ⇄ Asking
//! ```
//!
//! However a plan ends, the steps it left open end with it
//! ([`BotWork::close_steps`]).

use std::collections::HashMap;
use std::time::Duration;

use cypher_doc::{MessageRole, MessageStatus, SessionCommandPayload};
use cypher_proto::{
    self as p, ActivityKind, ApprovalState, Bot, BotActivity, BotApproval, BotChat, BotOrigin,
    BotThread, BotWork, Chat, RunRequest, Session, SessionStatus, StepState, WorkState,
};
use tokio_util::sync::CancellationToken;

use super::bodies::{CLEANUP_EVERY, MISSING_BODY};
use super::coordinate::{commands_of, last_answer, last_error};
use super::{text, workspace_roots};
use crate::EngineError;
use crate::bots::Bots;
use crate::bots::notice::{Notice, Wake};
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
/// A plan's result is announced in its chat only this long after it ended
/// (a host that was off for longer drops it; the plan's page still has it).
const NOTICE_WITHIN_MS: i64 = 24 * 3_600_000;

pub(super) async fn run(bots: Bots, cancel: CancellationToken) {
    let ws = bots.inner.workspace.clone();
    let mut records = ws.watch_bot_records();
    let mut bot_rows = ws.watch_bots();
    let mut sessions = ws.watch_session_rows();
    // Approvals the previous process was waiting on have no waiter now.
    bots.expire_approvals(true);
    let mut tick = tokio::time::interval(TICK);
    // Orphaned plan bodies go at start, then hourly (plan-body.md §8).
    let mut next_cleanup = tokio::time::Instant::now();
    loop {
        if let Err(err) = bots.reconcile_work() {
            tracing::warn!(error = %err, "bot work pass failed");
        }
        if tokio::time::Instant::now() >= next_cleanup {
            let bots = bots.clone();
            let cleaned =
                crate::util::off_runtime(move || bots.clean_plan_bodies(p::now_ms())).await;
            if let Err(err) = cleaned {
                tracing::warn!(error = %err, "plan body cleanup failed");
            }
            next_cleanup = tokio::time::Instant::now() + CLEANUP_EVERY;
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

/// The steps' states with `asking(session)` applied
/// ([`BotWork::settled_steps`]: a running step whose sessions, or a steered
/// session for the current step, wait on an answer is Asking, and back to
/// Running once none does). `None` when nothing changes.
pub(super) fn settle_steps(
    work: &BotWork,
    asking: impl Fn(&str) -> bool,
) -> Option<Vec<StepState>> {
    let states = work.settled_steps(asking);
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
                        self.stop_work(bot, w, &sessions, None)
                    }
                    // The time limit (§6.6) stops it like the owner would.
                    WorkState::Running if over_budget(bot, w, p::now_ms()) => {
                        let limit = w.limit_minutes(bot.plans.time_limit_minutes());
                        let reason = format!(
                            "It ran past its time limit of {}, so it was stopped.",
                            p::describe_minutes(limit)
                        );
                        self.stop_work(bot, w, &sessions, Some(&reason))
                    }
                    WorkState::Running => {
                        running += 1;
                        self.warn_before_deadline(bot, w, p::now_ms());
                        self.apply_plan_level(bot, w);
                        self.follow_work(bot, w, &sessions)
                    }
                    // Ended work whose steps were left open (a row from an
                    // engine that closed them only on Stop): close them, or
                    // one spins on the owner's card for good.
                    WorkState::Done | WorkState::Failed | WorkState::Stopped
                        if w.steps
                            .iter()
                            .any(|step| w.ended_step_state(step.state) != step.state) =>
                    {
                        self.edit_work(&w.id, |w| w.close_steps(p::now_ms()))
                            .map(drop)
                    }
                    _ => Ok(()),
                };
                if let Err(err) = step {
                    tracing::warn!(work = %w.id, error = %err, "bot work item pass failed");
                }
                self.mark_sessions(bot, w);
                self.post_work_notice(bot, w, p::now_ms());
            }
            // Approved plans start oldest decision first, while the bot may
            // start work and a slot is free.
            approved.sort_by_key(|(at, _)| *at);
            for (_, w) in approved {
                if bot.paused || !bot.capabilities.coordinate || running >= bot.plans.running_max()
                {
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
            // A pruned plan's full text leaves its chat with its row.
            let pruned: Vec<&BotWork> = work
                .iter()
                .copied()
                .filter(|w| work_ids.contains(&w.id))
                .collect();
            self.delete_bodies(&pruned);
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

    /// Stamp the sessions `work` started as its workspace's
    /// ([`Chat::started_by`]): the Sessions view leaves them out and the
    /// workspace lists them. `start_session` stamps a new one; this covers
    /// the ones started before the field existed, once each.
    fn mark_sessions(&self, bot: &Bot, work: &BotWork) {
        let ws = &self.inner.workspace;
        for id in work.session_ids() {
            if crate::util::lock(&self.inner.work.marked).contains(id) {
                continue;
            }
            match ws.chat(id) {
                Ok(Some(chat)) if chat.started_by.is_none() && chat.bot.is_none() => {
                    let by = p::ChatStartedBy {
                        bot_id: bot.id.clone(),
                        work_id: Some(work.id.clone()),
                    };
                    if let Err(err) = ws.set_chat_started_by(id, &by) {
                        tracing::warn!(session = %id, error = %err, "could not mark a session");
                        continue;
                    }
                }
                Ok(Some(_)) => {}
                // Its row may still be on its way (another device's), or it
                // was deleted: look again next pass, it costs one lookup.
                _ => continue,
            }
            crate::util::lock(&self.inner.work.marked).insert(id.to_string());
        }
    }

    /// Tell the workspace's chat a plan ended, once ([`crate::bots::notice`]):
    /// its result as a post, and the agent's short turn about it. A plan
    /// the owner stopped gets the post alone: they know.
    fn post_work_notice(&self, bot: &Bot, work: &BotWork, now_ms: i64) {
        let Some(ended) = work.ended_at else {
            return;
        };
        if !work.notify
            || !matches!(
                work.state,
                WorkState::Done | WorkState::Failed | WorkState::Stopped
            )
            || now_ms - ended > NOTICE_WITHIN_MS
        {
            return;
        }
        let entry_id = p::work_post_id(&work.id);
        if self.notice_posted(&entry_id) {
            return;
        }
        let (word, fallback) = match work.state {
            WorkState::Done => ("finished", "The plan finished."),
            WorkState::Failed => ("failed", "The plan failed."),
            _ => ("was stopped", "The plan was stopped."),
        };
        // The post holds the whole result: from the chat doc when the row
        // keeps a preview.
        let result = match self.plan_text(work) {
            Ok(text) => text.result,
            Err(err) => {
                tracing::warn!(work = %work.id, error = %err, "the plan's full result is unreadable; posting its preview");
                work.result.clone()
            }
        };
        let text = result
            .filter(|r| !r.trim().is_empty())
            .unwrap_or_else(|| fallback.to_string());
        let owner_stopped = work.state == WorkState::Stopped && work.stop_requested_at.is_some();
        let notice = Notice {
            preview: format!("{}: {text}", work.title),
            from: p::BotPost::Work {
                work_id: work.id.clone(),
                title: work.title.clone(),
                outcome: work.state,
                started_at: work.started_at,
                ended_at: ended,
            },
            text,
            wake: (!owner_stopped).then(|| Wake {
                headline: format!("Plan \"{}\" {word}", work.title),
                what: "the plan you proposed just ended",
                author: cypher_doc::command_author::PLAN,
                whole: Some(format!("get_work with work_id {} shows all of it", work.id)),
            }),
            entry_id,
        };
        self.post_notice(bot, notice, now_ms);
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
    /// A plan whose row holds previews starts only with its full text here.
    fn start_work(&self, bot: &Bot, work: &BotWork) -> Result<(), EngineError> {
        if self.plan_text(work)?.plan.is_preview() {
            return self.fail_without_body(bot, work);
        }
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
            // Its sessions start at the level it runs at now; a later change
            // is the owner's and reaches them ([`Self::apply_plan_level`]).
            w.applied_level = Some(w.effective_level());
            true
        })?;
        if started != Some(true) {
            return Ok(());
        }
        let thread = self.create_thread(bot, work, &thread_id)?;
        self.log(
            &bot.id,
            ActivityKind::PlanApproved,
            format!(
                "Started the plan at {}: {}",
                work.effective_level().label(),
                work.title
            ),
            Some(&thread_id),
            Some(&work.id),
            None,
        );
        self.dispatch_brief(bot, &work.id, &thread)?;
        tracing::info!(bot = %bot.id, work = %work.id, thread = %thread_id, "orchestration started");
        Ok(())
    }

    /// The owner changed a running plan's level (Mutate `setBotWorkLevel`):
    /// every session it started gets it, signed so a host under "Only
    /// paired devices" runs it, and the change is logged. The coordinating
    /// session reads the plan's level itself ([`super::run_sandbox`]), and
    /// sessions it starts from now on start at it. Each change is applied
    /// once: a session whose own level the owner changed since keeps it.
    /// Live runs take the new level at once, on whichever device hosts them
    /// (`SessionsEngine::start_live_levels`).
    fn apply_plan_level(&self, bot: &Bot, work: &BotWork) {
        let level = work.effective_level();
        let from = work.level_in_effect();
        if level == from {
            return;
        }
        let ws = &self.inner.workspace;
        for id in work.session_ids() {
            let Some(mut config) = ws.chat_config(id) else {
                continue;
            };
            if config.sandbox == level {
                continue;
            }
            config.sandbox = level;
            if let Err(err) = ws.set_chat_config(id, &config) {
                tracing::warn!(session = %id, error = %err, "could not give a plan's session its level");
                // Tried again on the next pass.
                return;
            }
            self.inner
                .doc_host
                .record_chat_launch(id, false, true, false);
        }
        match self.edit_work(&work.id, |w| w.applied_level = Some(level)) {
            Ok(Some(())) => self.log(
                &bot.id,
                ActivityKind::PlanLevelChanged,
                format!(
                    "Your owner changed the plan's permission level from {} to {}: {}",
                    from.label(),
                    level.label(),
                    work.title
                ),
                work.thread_chat_id.as_deref(),
                Some(&work.id),
                None,
            ),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(work = %work.id, error = %err, "could not record a plan's level")
            }
        }
    }

    /// The hidden orchestration thread: an owner thread of the bot, in its
    /// folder, with the bot chat's config and the plan's coordinator choice.
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
        let mut config = bot_chat.and_then(|c| c.config);
        if !work.coordinator_choice.is_empty() {
            let config = config.get_or_insert_with(|| p::ChatConfig {
                harness: p::HarnessId::Pi,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: p::SandboxLevel::ReadOnly,
            });
            work.coordinator_choice.apply(config);
        }
        let chat = Chat {
            id: thread_id.to_string(),
            device_id: self.device_id().to_string(),
            title: Some(p::clip(&work.title, p::TITLE_MAX)),
            archived: false,
            pinned: false,
            cwd: Some(folder),
            branch: None,
            checkout_id: None,
            config,
            last_message_preview: None,
            last_message_at: None,
            created_at: now,
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: Some(now),
            room_gen: Some(2),
            child: None,
            memory: None,
            started_by: None,
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

    /// A plan that can't run because its full text isn't here (§4): it
    /// fails, and its chat is told. Never run on the previews.
    fn fail_without_body(&self, bot: &Bot, work: &BotWork) -> Result<(), EngineError> {
        let now = p::now_ms();
        let failed = self.edit_work(&work.id, |w| {
            if !w.is_live() {
                return false;
            }
            w.state = WorkState::Failed;
            w.result = Some(MISSING_BODY.to_string());
            w.ended_at = Some(now);
            w.close_steps(now);
            // Approved, then failed before it ran: the chat still hears.
            w.notify = true;
            true
        })?;
        if failed == Some(true) {
            tracing::warn!(work = %work.id, "plan's full text missing on its host; not run");
            self.log(
                &bot.id,
                ActivityKind::WorkFailed,
                format!("The plan failed: {}", work.title),
                work.thread_chat_id.as_deref(),
                Some(&work.id),
                None,
            );
        }
        Ok(())
    }

    /// Queue the thread's first run: the brief. Its message id is fixed per
    /// work item, so a repeat after a crash never shows twice.
    fn dispatch_brief(&self, bot: &Bot, work_id: &str, thread: &Chat) -> Result<(), EngineError> {
        let ws = &self.inner.workspace;
        let Some(work) = ws.record::<BotWork>(p::KIND_BOT_WORK, work_id) else {
            return Ok(());
        };
        // The brief carries the full text; without it the plan fails.
        let plan_text = self.plan_text(&work)?;
        if plan_text.plan.is_preview() {
            return self.fail_without_body(bot, &work);
        }
        let marker = thread
            .bot
            .clone()
            .unwrap_or_else(|| BotChat::owner(&bot.id));
        let roots = workspace_roots(ws, bot, &marker);
        let brief = text::brief(
            bot,
            &work,
            &plan_text,
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
        self.inner.doc_host.queue_agent_command(
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
            cypher_doc::command_author::PLAN,
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
            // Cut off mid-answer (interrupted, or an engine restart it
            // didn't come back from): whatever it had written by then is not
            // a result.
            let cut_off = entries
                .iter()
                .rev()
                .find(|entry| entry.role == MessageRole::Assistant)
                .is_some_and(|entry| entry.status == Some(MessageStatus::Aborted));
            let error = if row.status == SessionStatus::Errored {
                Some(last_error(&entries).unwrap_or_else(|| "The work's session failed.".into()))
            } else if cut_off {
                Some(match &last {
                    Some(last) => format!(
                        "Its background session was stopped before it finished the plan. \
                         Its last message:\n\n{last}"
                    ),
                    None => {
                        "Its background session was stopped before it finished the plan.".into()
                    }
                })
            } else if last.is_none() {
                Some("Its background session ended without a result.".into())
            } else {
                None
            };
            return self.settle_work(bot, work, error, last);
        }
        // The plan-question rule every Needs you reads (plan-asks.md): the
        // chat or a child chat under it has an open question, whatever its
        // row's age; a question on an offline device still asks.
        let chats = ws.read_chats()?;
        let tree = p::view::ChatTree::new(&chats);
        let now = chrono::Utc::now();
        let asking = |id: &str| {
            p::view::plan_question(&tree, id, |c| sessions.get(c), |_| true, now).is_some()
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
        // The row as it is now: a body is written only for work still
        // running (finish_work may have ended it since this pass began).
        let Some(current) = self
            .inner
            .workspace
            .record::<BotWork>(p::KIND_BOT_WORK, &work.id)
            .filter(|w| w.state == WorkState::Running)
        else {
            return Ok(());
        };
        let state = if failed {
            WorkState::Failed
        } else {
            WorkState::Done
        };
        let end = |w: &mut BotWork| {
            w.state = state;
            w.ended_at = Some(now);
            w.close_steps(now);
        };
        let result = match (&error, &last) {
            (Some(error), _) => error.clone(),
            (None, Some(last)) => last.clone(),
            (None, None) => "The orchestration ended without a result.".into(),
        };
        // The full quote goes to the chat doc first, outside the row lock.
        let mut row = current;
        end(&mut row);
        let (preview, rev) = self.host_result_fields(row, &result, now);
        let settled = self.edit_work(&work.id, |w| {
            if w.state != WorkState::Running {
                return false;
            }
            end(w);
            w.result = preview;
            w.result_rev = rev;
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
        reason: Option<&str>,
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
        let end = |w: &mut BotWork| {
            w.state = WorkState::Stopped;
            w.ended_at = Some(now);
            w.close_steps(now);
        };
        // Like settle_work: a result the row must cut is written first.
        let result = reason.map(|reason| {
            let mut row = work.clone();
            end(&mut row);
            self.host_result_fields(row, reason, now)
        });
        let stopped = self.edit_work(&work.id, |w| {
            if w.state != WorkState::Running {
                return false;
            }
            end(w);
            if let Some((preview, rev)) = result {
                w.result = preview;
                w.result_rev = rev;
            }
            true
        })?;
        if stopped == Some(true) {
            self.log(
                &bot.id,
                ActivityKind::WorkStopped,
                match reason {
                    Some(_) => format!("Stopped the plan at its time budget: {}", work.title),
                    None => format!("Stopped the plan: {}", work.title),
                },
                work.thread_chat_id.as_deref(),
                Some(&work.id),
                None,
            );
        }
        Ok(())
    }
}

/// Running work past its time limit ([`BotWork::deadline_ms`]).
fn over_budget(bot: &Bot, work: &BotWork, now_ms: i64) -> bool {
    work.deadline_ms(bot.plans.time_limit_minutes())
        .is_some_and(|deadline| now_ms > deadline)
}

/// Whether the coordinating session is owed its warning now: its deadline
/// is near, and it was not yet told about this deadline (time the owner
/// added since moves the deadline, and earns a warning of its own).
fn owes_warning(bot: &Bot, work: &BotWork, now_ms: i64) -> bool {
    let Some(deadline) = work.deadline_ms(bot.plans.time_limit_minutes()) else {
        return false;
    };
    let warn_at = deadline - p::PLAN_WARN_BEFORE_MS;
    now_ms >= warn_at && now_ms <= deadline && work.warned_at.is_none_or(|at| at < warn_at)
}

impl Bots {
    /// Tell a running plan's coordinating session its time is nearly up,
    /// once per deadline, so it can wrap up and report before it is stopped.
    fn warn_before_deadline(&self, bot: &Bot, work: &BotWork, now_ms: i64) {
        if !owes_warning(bot, work, now_ms) {
            return;
        }
        let Some(thread) = work.thread_chat_id.as_deref() else {
            return;
        };
        let left = work
            .deadline_ms(bot.plans.time_limit_minutes())
            .map_or(0, |deadline| ((deadline - now_ms).max(0) / 60_000) as u32);
        let text = format!(
            "About {} left before this plan's time limit; then it is stopped with every \
             session it started.",
            p::describe_minutes(left.max(1))
        );
        let agent_prompt = format!(
            "{text} Don't start anything new. Let running sessions finish what they can, \
             record each step's state with update_step, and call finish_work with what was \
             done and what is left, so your owner gets a result rather than a stopped plan. \
             Your owner can add time from the plan's page; get_work shows the time left."
        );
        let queued = self.inner.doc_host.queue_agent_command(
            thread,
            SessionCommandPayload::Steer {
                prompt: text,
                message_id: Some(format!("{}-warn-{}", work.id, now_ms)),
                agent_prompt: Some(agent_prompt),
            },
            cypher_doc::command_author::PLAN,
        );
        if let Err(err) = queued {
            tracing::warn!(work = %work.id, error = %err, "could not warn the plan's session");
            return;
        }
        if let Err(err) = self.edit_work(&work.id, |w| w.warned_at = Some(now_ms)) {
            tracing::warn!(work = %work.id, error = %err, "could not record the warning");
        }
        tracing::info!(work = %work.id, left_minutes = left, "warned the plan of its time limit");
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
            session_choice: Default::default(),
        }
    }

    fn bot(plans: p::PlanSettings) -> Bot {
        Bot {
            id: "b".into(),
            name: "Bob".into(),
            icon: None,
            color: None,
            host_device_id: "d".into(),
            chat_id: "c".into(),
            instructions: None,
            scope: Default::default(),
            capabilities: p::BotCapabilities::new_bot(),
            paused: false,
            created_at: chrono::DateTime::from_timestamp_millis(0).unwrap(),
            plans,
            session_choice: Default::default(),
        }
    }

    fn work(id: &str, state: WorkState, at: i64) -> BotWork {
        BotWork {
            steered: Vec::new(),
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
            notify: false,
            time_limit_minutes: None,
            time_added_minutes: None,
            warned_at: None,
            coordinator_choice: Default::default(),
            body_rev: None,
            result_rev: None,
            level: None,
            proposed_level: None,
            level_quote: None,
            applied_level: None,
        }
    }

    const MINUTE: i64 = 60_000;

    #[test]
    fn work_stops_at_its_time_budget() {
        let bot = bot(Default::default());
        let mut work = work("w", WorkState::Running, 0);
        assert!(!over_budget(&bot, &work, 10), "not started");
        work.started_at = Some(1_000);
        assert!(!over_budget(&bot, &work, 1_000 + p::WORK_TIME_BUDGET_MS));
        assert!(over_budget(&bot, &work, 1_001 + p::WORK_TIME_BUDGET_MS));
    }

    #[test]
    fn the_time_limit_is_the_plans_else_the_workspaces_plus_what_was_added() {
        let hour = 60 * MINUTE;
        let bot = bot(p::PlanSettings {
            time_limit_minutes: Some(120),
            ..Default::default()
        });
        let mut work = work("w", WorkState::Running, 0);
        work.started_at = Some(0);
        assert!(
            !over_budget(&bot, &work, 2 * hour),
            "the workspace's two hours"
        );
        assert!(over_budget(&bot, &work, 2 * hour + 1));

        work.time_limit_minutes = Some(30);
        assert!(over_budget(&bot, &work, 30 * MINUTE + 1), "the plan's own");

        work.time_added_minutes = Some(60);
        assert!(!over_budget(&bot, &work, 90 * MINUTE), "an hour added");
        assert!(over_budget(&bot, &work, 90 * MINUTE + 1));

        // Out-of-range values written by hand are held to the bounds.
        work.time_limit_minutes = Some(1);
        work.time_added_minutes = None;
        assert!(!over_budget(&bot, &work, 15 * MINUTE));
    }

    #[test]
    fn the_warning_comes_once_per_deadline() {
        let bot = bot(Default::default());
        let mut work = work("w", WorkState::Running, 0);
        work.started_at = Some(0);
        work.time_limit_minutes = Some(60);
        assert!(!owes_warning(&bot, &work, 44 * MINUTE), "too early");
        assert!(owes_warning(&bot, &work, 45 * MINUTE));
        work.warned_at = Some(45 * MINUTE);
        assert!(!owes_warning(&bot, &work, 50 * MINUTE), "already told");
        // The owner adds an hour: a new deadline earns a new warning.
        work.time_added_minutes = Some(60);
        assert!(!owes_warning(&bot, &work, 60 * MINUTE));
        assert!(owes_warning(&bot, &work, 105 * MINUTE));
        assert!(
            !owes_warning(&bot, &work, 121 * MINUTE),
            "past it: it stops instead"
        );
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
