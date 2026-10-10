//! Crash and staleness recovery: boot-time journal recovery, orphaned
//! subagent terminalization, and retiring a run that no longer matches its
//! chat's settings.

use super::*;

impl SessionsEngine {
    /// Owner-death terminalization: the harness run for `chat_id` has truly
    /// ended — any subagent run still projected `Running` flips to `Error`
    /// (the owner that would have published its terminal state is gone). The
    /// session row/watch/workspace mirror updates; `status`/`started_at` stay
    /// untouched. Called ONLY when the real harness owner ends (drive_run's
    /// final exit, startup/early-fatal returns) — never on a parked Done,
    /// where a legal background subagent stays Running on the open stream.
    pub fn fail_orphaned_subagents(&self, chat_id: &str, reason: &str) {
        self.inner.fail_orphaned_subagents(chat_id, reason);
    }
}

impl SessionsEngine {
    /// Boot recovery: sweep THIS device's durable session rows and terminalize
    /// any subagent run still projected `Running` (the previous engine died
    /// with them in flight — they can never settle on their own). Remote
    /// device rows are untouched: their owners may still be live. Pure
    /// projection fix — `status`/`started_at` never change. Called right after
    /// [`Self::recover_stale`] at engine assembly.
    pub fn recover_orphaned_subagents(&self) -> Result<usize, EngineError> {
        const REASON: &str = "Subagent owner ended before engine restart";
        let Some(ws) = self.inner.workspace() else {
            return Ok(0);
        };
        let rows = ws.read_sessions()?;
        let mut fixed = 0usize;
        for mut row in rows {
            if row.device_id != self.inner.device_id {
                continue; // another device's row: its owner may still be live
            }
            let failed = fail_running_subagents(&mut row.subagents, now_ms(), REASON);
            if failed > 0 {
                ws.record_session(&row);
                fixed += failed;
            }
        }
        if fixed > 0 {
            tracing::info!(fixed, "orphaned subagent runs terminalized on boot");
        }
        Ok(fixed)
    }
}

impl SessionsEngine {
    /// Keep a chat's live run from serving a turn that wants different model
    /// settings than its harness process was launched with (a mid-session
    /// model/reasoning/harness switch). A PARKED run ends cleanly — the idle
    /// reaper's path, no aborted stamp — so the caller's turn spawns fresh
    /// (engine-owned resume keeps the conversation). A BUSY run keeps its
    /// current turn (anything routed into it now still lands there); its
    /// launch config is unchanged, so the first send after it parks ends it.
    /// Either way the retained run config takes the new settings, so an
    /// orphaned-steer re-dispatch or steer fallback uses them too.
    pub async fn retire_stale_run(
        &self,
        chat_id: &str,
        wanted: &LaunchConfig,
    ) -> Result<(), EngineError> {
        let target = lock(&self.inner.runs).get(chat_id).and_then(|h| {
            (h.launch != *wanted).then(|| {
                (
                    h.run_id.clone(),
                    h.interrupt_token.clone(),
                    h.launch.clone(),
                )
            })
        });
        let Some((run_id, token, launched)) = target else {
            return Ok(());
        };
        if let Some(request) = lock(&self.inner.last_requests).get_mut(chat_id) {
            wanted.apply_to(request);
        }
        let parked = self
            .session_status(chat_id)
            .is_some_and(|session| session.status == SessionStatus::Idle);
        if !parked {
            return Ok(());
        }
        tracing::info!(
            chat = %chat_id,
            from = ?launched.model,
            to = ?wanted.model,
            "model settings changed; ending parked session so the next turn respawns"
        );
        // Harness token only: flipping the engine cancel watch would stamp
        // the parked turn aborted.
        token.cancel();
        for _ in 0..500 {
            if !self.is_live(chat_id, &run_id) {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        tracing::warn!(chat = %chat_id, "parked session ignored clean end; interrupting");
        self.interrupt(chat_id).await.map(drop)
    }
}

impl SessionsEngine {
    /// Boot recovery: for every journal whose last event is not `Done` (a run died
    /// mid-stream), stamp this device's abandoned `streaming` doc entries `aborted`
    /// with a VISIBLE "Run interrupted by engine restart" error part, close the
    /// journal with a synthetic `Done{interrupted}` — and then PICK THE RUN BACK
    /// UP: a fresh crashed turn with revival budget left is re-dispatched against
    /// the remembered harness session (zeron: "not just eulogized";
    /// `MAX_AUTO_RESUME` = 3 consecutive revivals, fresh = crashed < 12h ago).
    pub fn recover_stale(&self) -> Result<usize, EngineError> {
        const MAX_AUTO_RESUME: u32 = 3;
        const RESUME_FRESH_MS: i64 = 12 * 60 * 60 * 1000;

        let stale = self.inner.journal.stale_sessions()?;
        let mut recovered = 0usize;
        for chat_id in stale {
            if lock(&self.inner.runs).contains_key(&chat_id) {
                continue; // a live run owns this journal
            }
            let handle = self.doc_handle(&chat_id)?;
            // Harness continuity first: the crashed run's session id may only
            // exist in the journal (the debounced workspace-row write may
            // never have landed) — remember it so the revived run resumes the
            // same harness conversation.
            if let Some((session_id, cwd)) = self.inner.journal_harness_session(&chat_id) {
                self.inner
                    .remember_harness_session(&chat_id, &session_id, &cwd);
            }
            // The revival prompt: the last user message (idempotent re-dispatch
            // under the SAME id — `write_user_message` dedupes by id, so the
            // transcript never shows a duplicate).
            let prompt = handle.doc().read_entries().ok().and_then(|entries| {
                entries
                    .iter()
                    .rev()
                    .find(|e| e.role == MessageRole::User)
                    .and_then(|e| {
                        e.parts.iter().find_map(|p| match p {
                            MessagePart::Text { text, .. } => Some((e.id.clone(), text.clone())),
                            _ => None,
                        })
                    })
            });
            let attempts = self.inner.journal.resume_attempts(&chat_id);
            let fresh = handle
                .doc()
                .read_entries()
                .ok()
                .and_then(|entries| {
                    entries
                        .iter()
                        .rev()
                        .find(|e| e.status == Some(MessageStatus::Streaming))
                        .map(|e| now_ms() - e.created_at < RESUME_FRESH_MS)
                })
                .unwrap_or(false);
            let will_resume = fresh && prompt.is_some() && attempts < MAX_AUTO_RESUME;

            let note = if will_resume {
                "Run interrupted by engine restart — resuming"
            } else {
                "Run interrupted by engine restart"
            };
            let done = AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: Some(note.into()),
                session_id: None,
            };
            self.inner.publish(&chat_id, &done);
            let stamped = handle.mark_abandoned_streams(note)?.len();
            self.set_status(&chat_id, SessionStatus::Idle, false);
            tracing::info!(chat = %chat_id, stamped, will_resume, attempts, "recovered stale session journal");
            recovered += 1;

            if !will_resume {
                continue;
            }
            let attempt = self.inner.journal.note_resume_attempt(&chat_id);
            let (user_id, prompt_text) = prompt.expect("gated by will_resume");
            let sessions = self.clone();
            tokio::spawn(async move {
                let Some(host) = sessions.inner.doc_host() else {
                    return;
                };
                let request = sessions
                    .last_request(&chat_id)
                    .or_else(|| host.request_from_chat_row(&chat_id, &prompt_text))
                    // Last resort: the journal's own cwd (zeron's draft config)
                    // — a crash can predate the debounced workspace-row write.
                    .or_else(|| {
                        let (_, cwd) = sessions.inner.journal_harness_session(&chat_id)?;
                        Some(RunRequest {
                            prompt: String::new(),
                            harness: None,
                            model: None,
                            reasoning: None,
                            model_options: Default::default(),
                            cwd,
                            sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
                            auto_approve: false,
                            attachments: Vec::new(),
                            pending_attachments: Vec::new(),
                            resume: None,
                            worktree: None,
                        })
                    });
                let Some(mut request) = request else {
                    tracing::warn!(chat = %chat_id, "auto-resume skipped: no run config");
                    return;
                };
                request.prompt = prompt_text;
                request.resume = None; // dispatch re-injects the remembered session
                request.attachments = Vec::new();
                let harness_id = host.harness_for_request(&chat_id, &request);
                match sessions
                    .dispatch(&chat_id, harness_id, request, Some(user_id))
                    .await
                {
                    Ok(_) => {
                        tracing::info!(chat = %chat_id, attempt, "auto-resumed crashed run")
                    }
                    Err(err) => {
                        tracing::warn!(chat = %chat_id, error = %err, "auto-resume dispatch failed")
                    }
                }
            });
        }
        Ok(recovered)
    }
}

impl Inner {
    /// Owner-death sweep for one chat's live projection: flip every `Running`
    /// subagent to `Error` with a bounded reason (the harness owner that would
    /// have published its terminal state is gone). Only the projection
    /// changes — session `status`/`started_at` are never touched. Updates the
    /// in-memory row + watch + workspace mirror. No-op when nothing is running.
    pub(super) fn fail_orphaned_subagents(&self, chat_id: &str, reason: &str) {
        let now = now_ms();
        // Statuses guard released before publish (publish re-locks it).
        let session = {
            let mut statuses = lock(&self.statuses);
            let Some(entry) = statuses.get_mut(chat_id) else {
                return;
            };
            if fail_running_subagents(&mut entry.subagents, now, reason) == 0 {
                return;
            }
            entry.updated_at = Utc::now();
            entry.clone()
        };
        self.publish_session(chat_id, &session);
    }
}

/// Pure owner-death terminalization of one run list: only `Running` runs flip
/// to `Error` with `updated_at`/`ended_at` stamped now and `progress` set to a
/// length-controlled reason; settled runs (Done/Error) are untouched. Returns
/// how many runs were failed. Never a status/started_at change at the session
/// level — that lives with the caller.
fn fail_running_subagents(runs: &mut [SubagentRun], now_ms: i64, reason: &str) -> usize {
    // Bound the reason so a long owner-death message can never blow the
    // snapshot's per-run progress cap (the publisher caps at 4KiB; this is
    // a defensive reader-side trim for engine-synthesized reasons only).
    let reason: String = reason.chars().take(200).collect();
    let mut failed = 0usize;
    for run in runs {
        if run.status != SubagentRunStatus::Running {
            continue;
        }
        run.status = SubagentRunStatus::Error;
        run.updated_at = now_ms;
        run.ended_at = Some(now_ms);
        run.progress = Some(reason.clone());
        failed += 1;
    }
    failed
}
