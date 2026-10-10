//! Steering: pushing a prompt into a live run, and the routed-steer ledger
//! that keeps an accepted steer from being lost if the run ends first.

use super::*;

/// Outcome of a steer attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerOutcome {
    /// Delivered into the live run's steering mailbox.
    Accepted,
    /// No live steerable run — the caller should dispatch the prompt as a new turn.
    NotSteerable,
}

/// One accepted-but-unconfirmed steer: enough to re-dispatch it verbatim.
/// `prompt` is the VISIBLE prompt (the doc user entry); `agent_prompt` the
/// optional EFFECTIVE override the harness should receive.
#[derive(Debug, Clone)]
pub(super) struct RoutedSteer {
    pub(super) prompt: String,
    pub(super) agent_prompt: Option<String>,
    pub(super) message_id: String,
}

impl SessionsEngine {
    /// Push a steer prompt into the live run's mailbox. `NotSteerable` when no live
    /// steerable run exists — the caller (command executor) dispatches a new turn.
    pub async fn steer(
        &self,
        chat_id: &str,
        prompt: &str,
        message_id: Option<String>,
    ) -> Result<SteerOutcome, EngineError> {
        self.steer_augmented(chat_id, prompt, None, message_id)
            .await
    }
}

impl SessionsEngine {
    /// [`Self::steer`] with an EFFECTIVE harness prompt override (the Comment
    /// feature): the doc entry keeps `prompt` while the harness mailbox
    /// receives `agent_prompt` when present — alignment stripped for any
    /// running harness but Pi ([`agent_prompt_for`]).
    pub async fn steer_augmented(
        &self,
        chat_id: &str,
        prompt: &str,
        agent_prompt: Option<String>,
        message_id: Option<String>,
    ) -> Result<SteerOutcome, EngineError> {
        let target = lock(&self.inner.runs)
            .get(chat_id)
            .filter(|h| h.steerable)
            .map(|h| {
                (
                    h.run_id.clone(),
                    h.steer_tx.clone(),
                    h.routed_steers.clone(),
                    h.launch.harness,
                )
            });
        let Some((run_id, steer_tx, ledger, harness)) = target else {
            return Ok(SteerOutcome::NotSteerable);
        };
        let comments = MessageComment::from_agent_prompt(agent_prompt.as_deref().unwrap_or(""));
        let user_id = message_id.clone().unwrap_or_else(new_id);
        // Accepted: the ledger entry and the mailbox send are atomic under
        // the ledger lock — the entry goes in BEFORE try_send, so the run
        // task can never observe an accepted mailbox message without its
        // ledger entry (the parked-pi path emits Steered immediately on
        // mailbox receive, and the exit drain must always find what it
        // owns). A failed send rolls the exact entry back while still
        // holding the lock. After that, the user entry (client-minted id),
        // then Working BEFORE the lastMessageAt bump — same causal-order
        // invariant as the dispatch route (an observer must never hold [new
        // message, settled status]: the phantom "completed" flash).
        let effective =
            agent_prompt_for(harness, agent_prompt.clone()).unwrap_or_else(|| prompt.to_string());
        let sent = {
            let mut ledger = lock(&ledger);
            // Unstripped: an orphan re-dispatch strips for its own harness.
            ledger.push_back(RoutedSteer {
                prompt: prompt.to_string(),
                agent_prompt: agent_prompt.clone(),
                message_id: user_id.clone(),
            });
            let message = SteerMessage {
                prompt: effective,
                message_id: message_id.clone(),
            };
            let ok = steer_tx.try_send(message).is_ok();
            if !ok {
                // Mailbox closed (runtime mid-teardown / non-steering
                // harness): drop the exact entry we just added.
                ledger.retain(|s| s.message_id != user_id);
            }
            ok
        };
        if !sent {
            return Ok(SteerOutcome::NotSteerable);
        }
        let handle = self.doc_handle(chat_id)?;
        handle.write_user_prompt(&user_id, prompt, &comments, now_ms())?;
        // A routed steer is a turn too. Fired here (not only on the confirmed
        // path) — a reclaim falls back to dispatch, which just re-snapshots.
        if let Some(request) = self.last_request(chat_id) {
            self.note_turn_start(chat_id, &request.cwd);
        }
        if self.is_live(chat_id, &run_id) {
            self.set_status(chat_id, SessionStatus::Working, false);
            self.inner.note_message(chat_id, prompt);
            return Ok(SteerOutcome::Accepted);
        }
        // The run died around the send. Exit drain claimed the entry → its
        // re-dispatch owns the message; still ours → reclaim and report
        // NotSteerable so the executor falls back to a fresh dispatch
        // (same message id — the doc entry dedupes).
        let reclaimed = {
            let mut ledger = lock(&ledger);
            let before = ledger.len();
            ledger.retain(|s| s.message_id != user_id);
            ledger.len() != before
        };
        if reclaimed {
            return Ok(SteerOutcome::NotSteerable);
        }
        self.inner.note_message(chat_id, prompt);
        Ok(SteerOutcome::Accepted)
    }
}
