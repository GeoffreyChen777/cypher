//! Mailbox routing: steering a live turn, reading pi's disposition for each
//! routed message, and retrying steers a settled turn stranded.

use super::run::park_turn;
use super::*;

/// What pi did with an accepted `prompt` / `steer`. pi ≥ 0.99 reports it as
/// the response's `data.disposition`; older runtimes report nothing, and the
/// loop falls back to inferring it from its own view of the turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Disposition {
    /// pi was idle: the message started a fresh run.
    Started,
    /// pi was running: the message is queued as a steer and delivered at the
    /// run's next step (pi re-runs for queued input before it settles).
    Queued,
    /// An extension command or input handler consumed it: nothing reaches
    /// the model for it.
    Handled,
}

pub(super) fn disposition(data: &Value) -> Option<Disposition> {
    match data.get("disposition")?.as_str()? {
        "started" => Some(Disposition::Started),
        "queued" => Some(Disposition::Queued),
        "handled" => Some(Disposition::Handled),
        _ => None,
    }
}

/// A mailbox message routed into the live turn, awaiting its response —
/// [`Incoming::Response`] with this id, in order with the event stream.
pub(super) struct RoutedSteer {
    id: String,
    text: String,
}

/// The next turn a parked run opens (the main loop's top branch).
pub(super) enum NextTurn {
    /// A mailbox message to dispatch as a parked `prompt`.
    Prompt(String),
    /// A routed message pi already took after the turn it was meant to steer
    /// had ended: it started a fresh run, or an extension consumed it. The
    /// turn opens with nothing left to dispatch.
    Accepted,
}

/// Route a mailbox message into the live turn. A pi that reports
/// dispositions gets `prompt` with `streamingBehavior:"steer"` — atomic
/// across its real state, and the response says which way it went. Older
/// runtimes get a raw `steer`, whose fate the loop infers from the turn
/// state. Either response arrives in stdout order, so one that followed a
/// settle is always seen after it.
fn route_steer(client: &PiClient, text: String, atomic: bool) -> Option<RoutedSteer> {
    let mut params = Map::new();
    params.insert("message".into(), Value::String(text.clone()));
    let command = if atomic {
        params.insert("streamingBehavior".into(), Value::String("steer".into()));
        "prompt"
    } else {
        "steer"
    };
    match client.send_ordered(command, params) {
        Ok(id) => Some(RoutedSteer { id, text }),
        Err(e) => {
            // The child is gone; its EOF ends the run.
            tracing::debug!(error = %e, "steer not sent (dropped)");
            None
        }
    }
}

/// One `Steered` boundary per routed message. The engine retires one
/// accepted mailbox message per boundary, so a message an extension consumed
/// must confirm too — otherwise the run's exit re-dispatches (re-runs) it.
pub(super) async fn emit_boundaries(
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    assistant_message_id: &mut String,
    count: usize,
) -> bool {
    for _ in 0..count {
        let (prev, next) = rotate(assistant_message_id);
        let boundary = AgentEvent::Steered {
            assistant_message_id: Some(prev),
            next_assistant_message_id: Some(next),
        };
        if !send(event_tx, boundary).await {
            return false;
        }
    }
    true
}

/// Steers pi accepted but never delivered before the turn closed sit in its
/// queue, and pi drains that queue at the start of its next run — a retry
/// alone would deliver each one twice. Clear the queue first (pi applies
/// commands in order, so the clear lands before the retry), then retry each
/// as a parked prompt.
pub(super) fn requeue_stranded(
    client: &PiClient,
    stranded: &mut VecDeque<String>,
    backlog: &mut VecDeque<NextTurn>,
) {
    if stranded.is_empty() {
        return;
    }
    client.send("clear_queue", Map::new());
    backlog.extend(stranded.drain(..).map(NextTurn::Prompt));
}

impl PiRun {
    /// A routed steer's response, in order with the events: any settle pi
    /// wrote before it has already been handled.
    pub(super) async fn on_steer_response(
        &mut self,
        id: String,
        result: Result<Value, String>,
    ) -> Flow {
        let Some(call) = self.steer_call.take_if(|call| call.id == id) else {
            return Flow::Continue;
        };
        if !self.interrupted {
            match result.map(|data| disposition(&data)) {
                // pi was running: delivered at its next step,
                // where the Steered boundary fires.
                Ok(Some(Disposition::Queued)) => self.steers_queued.push_back(call.text),
                Ok(Some(Disposition::Handled)) if self.in_turn => self.handled_steers += 1,
                // pi was idle — the turn this message meant to
                // steer had ended — so it started a fresh run, or
                // an extension took it. Either way it opens the
                // next turn; nothing is retried.
                Ok(Some(Disposition::Started | Disposition::Handled)) => {
                    if self.in_turn {
                        // pi is idle, yet the turn is open: only an
                        // inert one (command output, no agent run)
                        // — any run's settle would have preceded
                        // this response. Close it as its grace would.
                        self.in_turn = false;
                        self.done_sent = true;
                        if !park_turn(
                            &self.event_tx,
                            &self.client,
                            &mut self.assistant_message_id,
                            &mut self.handled_steers,
                            &mut self.steers_queued,
                            &mut self.prompt_backlog,
                            &self.last_assistant_text,
                            DoneStatus::Completed,
                            None,
                            &self.session_file,
                        )
                        .await
                        {
                            return Flow::Break;
                        }
                    }
                    self.prompt_backlog.push_front(NextTurn::Accepted);
                }
                // A runtime without dispositions: a steer accepted
                // in a live turn is delivered at its next step...
                Ok(None) if self.in_turn => self.steers_queued.push_back(call.text),
                // ...but one the turn settled ahead of is stranded
                // (an idle pi only QUEUES steers).
                Ok(None) => requeue_stranded(
                    &self.client,
                    &mut VecDeque::from([call.text]),
                    &mut self.prompt_backlog,
                ),
                Err(e) if self.in_turn => {
                    tracing::debug!(error = %e, "steer rejected (dropped)");
                }
                // Rejected once the turn had ended: restart with
                // it like any parked message — a real failure
                // surfaces there as that turn's error.
                Err(_) => self.prompt_backlog.push_back(NextTurn::Prompt(call.text)),
            }
        }
        if !self.in_turn {
            // Parked: queued followers can't be delivered as
            // steers — they restart the next turn via prompt.
            self.prompt_backlog
                .extend(self.steer_backlog.drain(..).map(NextTurn::Prompt));
        } else if let Some(text) = self.steer_backlog.pop_front() {
            self.steer_call = route_steer(&self.client, text, self.atomic_steer);
        }
        if !self.steering_open
            && !self.in_turn
            && self.steers_queued.is_empty()
            && self.steer_call.is_none()
            && self.prompt_backlog.is_empty()
        {
            return Flow::Break;
        }
        Flow::Continue
    }

    /// A mailbox message, or the mailbox closing.
    pub(super) fn on_steer(&mut self, steer: Option<crate::SteerMessage>) -> Flow {
        match steer {
            Some(msg) => {
                if !self.in_turn || self.idle_prompt.is_some() {
                    // Parked (or a parked-turn prompt still in preflight):
                    // this mailbox message starts a NEW turn via RPC
                    // prompt — a parked pi only QUEUES steers, so sending
                    // one idle would strand it forever. Followers queue
                    // for after the turn settles (never concurrent with
                    // the in-flight prompt).
                    self.prompt_backlog.push_back(NextTurn::Prompt(msg.prompt));
                } else {
                    // Active turn: pi-native mid-run steer — delivered
                    // after the current assistant message's tool calls,
                    // before the next LLM call. One in flight at a time.
                    if self.steer_call.is_some() {
                        self.steer_backlog.push_back(msg.prompt);
                    } else {
                        self.steer_call = route_steer(&self.client, msg.prompt, self.atomic_steer);
                    }
                }
            }
            None => {
                self.steering_open = false;
                if !self.in_turn
                    && self.steers_queued.is_empty()
                    && self.steer_call.is_none()
                    && self.prompt_backlog.is_empty()
                {
                    return Flow::Break;
                }
            }
        }
        Flow::Continue
    }
}
