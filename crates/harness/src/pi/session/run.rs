//! The per-run event loop (`run_session`) and its agent-event arms.

use super::setup::{
    BUILTIN_TEXT_KEY, InterceptOutcome, builtin_match, compact_params, compact_summary,
    intercept_builtin, refresh_context_usage, setup_session,
};
use super::steer::{Disposition, NextTurn, disposition, emit_boundaries, requeue_stranded};
use super::*;
use crate::CancellationToken;
use crate::process::StderrTail;

/// The agent name crash messages lead with.
const AGENT_NAME: &str = "pi";

/// The role of a `message_start` / `message_end` payload (assistant only —
/// toolResult/user messages are internal to the turn).
fn message_is_assistant(message: Option<&Value>) -> bool {
    message
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        .map(|r| r == "assistant")
        .unwrap_or(false)
}

/// The model that answered an assistant `message_end`: the provider's
/// `responseModel` (pi-ai sets it only when the response named a different
/// model than the request), else the requested `model`. `None` for a message
/// no model produced — an empty error/abort stand-in for a failed request.
fn answered_model(message: &Value) -> Option<AnsweredModel> {
    let text = |key: &str| {
        message
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let requested = text("model");
    if let Some(served) = text("responseModel") {
        return Some(AnsweredModel {
            model: served.to_owned(),
            requested: requested.filter(|r| *r != served).map(str::to_owned),
        });
    }
    let produced = message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|content| !content.is_empty());
    if !produced {
        return None;
    }
    Some(AnsweredModel {
        model: requested?.to_owned(),
        requested: None,
    })
}

/// Owns the child prompt temp file for the run's lifetime: removing it on
/// drop covers every `run_session` return path (early errors included).
struct TempPromptGuard(Option<PathBuf>);

impl Drop for TempPromptGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
            let _ = path.parent().map(std::fs::remove_dir);
        }
    }
}

/// Close the current turn: confirm the routed messages an extension
/// consumed, requeue steers pi only queued (retried after the park), then
/// send the turn's Done. False once the consumer has hung up.
#[allow(clippy::too_many_arguments)]
pub(super) async fn park_turn(
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    client: &PiClient,
    assistant_message_id: &mut String,
    handled_steers: &mut usize,
    steers_queued: &mut VecDeque<String>,
    prompt_backlog: &mut VecDeque<NextTurn>,
    last_assistant_text: &str,
    status: DoneStatus,
    error: Option<String>,
    session_file: &str,
) -> bool {
    if !emit_boundaries(
        event_tx,
        assistant_message_id,
        std::mem::take(handled_steers),
    )
    .await
    {
        return false;
    }
    requeue_stranded(client, steers_queued, prompt_backlog);
    let result = (!last_assistant_text.is_empty()).then(|| last_assistant_text.to_owned());
    send(
        event_tx,
        AgentEvent::Done {
            status,
            result,
            error,
            session_id: Some(session_file.to_owned()),
        },
    )
    .await
}

impl PiRun {
    /// Parked with a queued mailbox message: open the next turn. `None`
    /// proceeds to the loop's `select!`.
    async fn open_parked_turn(&mut self) -> Option<Flow> {
        // Parked with a queued mailbox message: start the NEXT turn via RPC
        // `prompt` with `streamingBehavior:"steer"` — atomic across pi's
        // real state: a truly idle pi starts a fresh turn; a pi still (or
        // newly) active queues the message as a steer instead of rejecting
        // the prompt (the confirmed parked-session wedge). A raw `steer` is
        // never sent idle — a parked pi only QUEUES steers, so it would
        // strand forever. The Steered boundary fires BEFORE the prompt is
        // dispatched: an extension notify can land before the prompt response
        // and must fold into the new turn's segment. If the boundary cannot
        // be sent, the run is over — do not dispatch. A routed message pi
        // already took (`NextTurn::Accepted`) opens its turn the same way,
        // minus the dispatch.
        if !self.in_turn
            && self.idle_prompt.is_none()
            && self.steer_call.is_none()
            && self.steers_queued.is_empty()
            && let Some(turn) = self.prompt_backlog.pop_front()
        {
            let (prev, next) = rotate(&mut self.assistant_message_id);
            if !send(
                &self.event_tx,
                AgentEvent::Steered {
                    assistant_message_id: Some(prev),
                    next_assistant_message_id: Some(next),
                },
            )
            .await
            {
                return Some(Flow::Break);
            }
            // Per-turn reset: Done's result/status, the progress throttle,
            // and activity tracking all belong to the new turn.
            self.last_assistant_text.clear();
            self.last_stop_reason = "stop".to_owned();
            self.last_error_message = None;
            self.agent_started = false;
            self.done_sent = false;
            self.in_turn = true;
            self.progress_last.clear();
            self.progress_ended.clear();
            self.throughput.start_turn();
            self.had_ui = false;
            let text = match turn {
                NextTurn::Prompt(text) => text,
                NextTurn::Accepted => {
                    // Already accepted: arm the grace as the prompt arm
                    // would. A fresh run's first event disarms it; a
                    // consumed message settles through it.
                    self.prompt_is_command = false;
                    self.no_activity = Box::pin(tokio::time::sleep(self.no_activity_grace));
                    return Some(Flow::Continue);
                }
            };
            // The no-activity timer is NOT armed here: the previous turn's
            // sleep may already have elapsed and must not fire during this
            // prompt's preflight. The `idle_prompt.is_none()` guard keeps the
            // branch disabled while the request is in flight; the resolution
            // re-arms it once accepted (lifecycle events that landed first
            // disarm it via agent_started).
            self.prompt_is_command = text.trim_start().starts_with('/');
            let prompt_client = self.client.clone();
            // A parked `/compact` is the built-in too: pi's `prompt` never
            // runs TUI built-ins and would hand the text to the model. The
            // RPC's summary rides back through the prompt slot; the accept
            // arm streams it and the zero grace settles the turn.
            let compact = self
                .intercept
                .compact
                .then(|| builtin_match(&text, "compact"))
                .flatten()
                .map(|instructions| compact_params(instructions));
            self.idle_prompt = Some(match compact {
                Some(params) => Box::pin(async move {
                    let data = prompt_client.request("compact", params).await?;
                    Ok(json!({ BUILTIN_TEXT_KEY: compact_summary(&data) }))
                }),
                None => {
                    let mut params = Map::new();
                    params.insert("message".into(), Value::String(text));
                    // `streamingBehavior:"steer"` makes the parked restart
                    // atomic: an idle pi starts a fresh turn, a
                    // still-streaming pi queues the message as a steer — a
                    // plain prompt would be REJECTED while pi streams (the
                    // confirmed parked-session wedge).
                    params.insert("streamingBehavior".into(), Value::String("steer".into()));
                    Box::pin(async move { prompt_client.request("prompt", params).await })
                }
            });
        }
        None
    }

    /// The in-flight `prompt` RPC resolved.
    async fn on_prompt_response(&mut self, res: Result<Value, HarnessError>) -> Flow {
        let _ = self.idle_prompt.take();
        // The response only means the prompt was ACCEPTED — the turn
        // streams from here. A rejected prompt is the one case
        // nothing will ever stream for it: one Done Errored ends the
        // run (and the terminal bookkeeping reaps the child).
        match res {
            Ok(data) if data.get(BUILTIN_TEXT_KEY).is_some() => {
                // A parked built-in finished: its line is the turn's
                // whole output, and the zero grace settles it through
                // the no-activity arm (Done + park, like any inert
                // command).
                let text = data[BUILTIN_TEXT_KEY]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                self.last_assistant_text.push_str(&text);
                if !send(&self.event_tx, AgentEvent::TextDelta { text }).await {
                    return Flow::Break;
                }
                self.no_activity = Box::pin(tokio::time::sleep(Duration::ZERO));
            }
            Ok(data) => {
                let disposition = disposition(&data);
                self.atomic_steer |= disposition.is_some();
                // Arm the no-activity grace NOW that the prompt is
                // accepted. Lifecycle events that landed during the
                // preflight already set agent_started (disarming the
                // branch); a genuinely inert (notify-only) turn gets
                // a fresh grace window from here — never a stale
                // timer from the previous turn.
                // Extension commands ACK only after the handler
                // returns. If a command already showed a dialog or
                // notified and no agent started, skip the 2s wait
                // (close-picker spin). Zero-sleep still yields to
                // `incoming` first (biased select) so a
                // ui-select-then-ACK-then-text burst is not cut off.
                // pi names a consumed prompt `handled`; a `started`
                // one (a skill or prompt template, despite its slash)
                // always gets the full grace.
                let is_command =
                    disposition.map_or(self.prompt_is_command, |d| d == Disposition::Handled);
                let grace = if self.had_ui && is_command && !self.agent_started {
                    Duration::ZERO
                } else {
                    self.no_activity_grace
                };
                self.no_activity = Box::pin(tokio::time::sleep(grace));
            }
            Err(e) => {
                self.done_sent = true;
                let _ = self
                    .event_tx
                    .send(Ok(AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(e.to_string()),
                        session_id: Some(self.session_file.clone()),
                    }))
                    .await;
                return Flow::Break;
            }
        }
        Flow::Continue
    }

    async fn on_event(&mut self, ev: Value) -> Flow {
        // Only AGENT-LIFECYCLE events prove a turn is running and
        // disarm the no-activity grace. Informational events fire
        // outside any turn — `thinking_level_changed` rides the
        // set_model/set_thinking_level setup commands, `extension_error` can
        // arrive from extension activity — counting either would
        // leave a no-LLM run parked "Working" forever.
        let kind = ev.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(
            kind,
            "agent_start"
                | "turn_start"
                | "message_start"
                | "message_update"
                | "message_end"
                | "tool_execution_start"
                | "tool_execution_update"
                | "tool_execution_end"
                | "agent_end"
                | "agent_settled"
        ) {
            self.agent_started = true;
        }
        match kind {
            "message_update" => self.on_message_update(&ev).await,
            "message_start" => self.on_message_start(&ev).await,
            "message_end" => self.on_message_end(&ev).await,
            "tool_execution_start" => self.on_tool_start(&ev).await,
            "tool_execution_update" => self.on_tool_update(&ev).await,
            "tool_execution_end" => self.on_tool_end(&ev).await,
            "extension_error" => self.on_extension_error(&ev).await,
            "agent_settled" => self.on_agent_settled().await,
            // Manual (parked `/compact`) and automatic compactions
            // both end here: re-read the gauge, falling back to the
            // compaction's own estimate while pi reports none.
            "compaction_end" => {
                let estimate = ev
                    .get("result")
                    .and_then(|r| r.get("estimatedTokensAfter"))
                    .cloned();
                refresh_context_usage(&self.client, &self.event_tx, estimate);
                Flow::Continue
            }
            // agent_end/turn_*/queue_update/compaction_start/auto_retry_*/
            // summarization_*/bash_execution_update: nothing cypher
            // renders — ignored.
            _ => Flow::Continue,
        }
    }

    /// A streamed delta: text, thinking or tool-call arguments.
    async fn on_message_update(&mut self, ev: &Value) -> Flow {
        let ame = ev.get("assistantMessageEvent");
        match ame.and_then(|a| a.get("type")).and_then(Value::as_str) {
            Some("text_delta") => {
                if let Some(text) = ame.and_then(|a| a.get("delta")).and_then(Value::as_str) {
                    self.last_assistant_text.push_str(text);
                    if !text.is_empty()
                        && !send(
                            &self.event_tx,
                            AgentEvent::TextDelta {
                                text: text.to_owned(),
                            },
                        )
                        .await
                    {
                        return Flow::Break;
                    }
                }
            }
            Some("thinking_delta") => {
                if let Some(text) = ame.and_then(|a| a.get("delta")).and_then(Value::as_str)
                    && !text.is_empty()
                    && !send(
                        &self.event_tx,
                        AgentEvent::ReasoningDelta {
                            text: text.to_owned(),
                        },
                    )
                    .await
                {
                    return Flow::Break;
                }
            }
            // *_start/*_end/toolcall_*: internal state only.
            _ => {}
        }
        // Every streamed delta is output — text,
        // thinking, and tool-call arguments alike.
        let kind = ame.and_then(|a| a.get("type")).and_then(Value::as_str);
        let delta = ame.and_then(|a| a.get("delta")).and_then(Value::as_str);
        if let (Some(kind @ ("text_delta" | "thinking_delta" | "toolcall_delta")), Some(delta)) =
            (kind, delta)
            && let Some(reading) =
                self.throughput
                    .delta(delta, kind == "thinking_delta", Instant::now())
            && !send(
                &self.event_tx,
                AgentEvent::Throughput {
                    throughput: Some(reading),
                },
            )
            .await
        {
            return Flow::Break;
        }
        Flow::Continue
    }

    /// A message opened; an assistant one starts a fresh text accumulator.
    async fn on_message_start(&mut self, ev: &Value) -> Flow {
        // A new assistant message starts a fresh text
        // accumulator — Done's `result` is the LAST
        // assistant message's text, not the whole turn's.
        // (toolResult/user messages are internal.)
        if message_is_assistant(ev.get("message")) {
            self.last_assistant_text.clear();
            self.throughput.start_message(Instant::now());
            // The NEXT assistant message after a queued
            // steer is the steer's reply: split the doc entry
            // here (before its content streams). Messages an
            // extension consumed split here too.
            let delivered = self.steers_queued.pop_front().is_some();
            if delivered {
                // A steer delivery opens a turn: even if
                // an agent_settled raced ahead of it, the
                // steer reply's own settle must Done.
                self.in_turn = true;
            }
            let boundaries = std::mem::take(&mut self.handled_steers) + usize::from(delivered);
            if !emit_boundaries(&self.event_tx, &mut self.assistant_message_id, boundaries).await {
                return Flow::Break;
            }
        }
        Flow::Continue
    }

    /// A message closed; an assistant one records its outcome and rotates the id.
    async fn on_message_end(&mut self, ev: &Value) -> Flow {
        if message_is_assistant(ev.get("message")) {
            refresh_context_usage(&self.client, &self.event_tx, None);
            // The reported count replaces the estimate;
            // the rate drops until the next message.
            let usage = ev.get("message").and_then(|m| m.get("usage"));
            let count = |key: &str| usage.and_then(|u| u.get(key)).and_then(Value::as_u64);
            let reading = self
                .throughput
                .end_message(count("output"), count("reasoning"));
            if !send(
                &self.event_tx,
                AgentEvent::Throughput {
                    throughput: Some(reading),
                },
            )
            .await
            {
                return Flow::Break;
            }
            if let Some(message) = ev.get("message") {
                if let Some(stop) = message.get("stopReason").and_then(Value::as_str) {
                    self.last_stop_reason = stop.to_owned();
                }
                if let Some(err) = message.get("errorMessage").and_then(Value::as_str) {
                    self.last_error_message = Some(err.to_owned());
                }
            }
            // Journal boundary marker: the doc fold treats
            // this as a no-op (one segment per turn until
            // a Steered/Done), but the journal records it
            // per assistant message. Rotate the id for the
            // next one.
            let completed = self.assistant_message_id.clone();
            rotate(&mut self.assistant_message_id);
            if !send(
                &self.event_tx,
                AgentEvent::AssistantMessageCompleted {
                    assistant_message_id: completed,
                    model: ev.get("message").and_then(answered_model),
                },
            )
            .await
            {
                return Flow::Break;
            }
        }
        Flow::Continue
    }

    async fn on_tool_start(&mut self, ev: &Value) -> Flow {
        let id = ev
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let name = ev
            .get("toolName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let args = ev.get("args").cloned().unwrap_or(Value::Null);
        if !send(
            &self.event_tx,
            AgentEvent::ToolCall {
                id,
                call: pi_typed_call(&name, &args, &self.mcp_servers),
            },
        )
        .await
        {
            return Flow::Break;
        }
        Flow::Continue
    }

    async fn on_tool_update(&mut self, ev: &Value) -> Flow {
        // Live progress for an unresolved tool: same
        // `{content:[{type:"text",...}]}` shape as the
        // end result, so `tool_output_text` (which caps
        // at OUTPUT_CAP) extracts it. Throttled per
        // toolCallId (first always forwards) — the doc
        // fold only ever keeps the last 8 lines, so
        // forwarding every partial chunk is pure churn.
        let id = ev
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let output = ev.get("partialResult").and_then(tool_output_text);
        if let (Some(output), false) = (output, id.is_empty())
            && !self.progress_ended.contains(&id)
        {
            let now = Instant::now();
            let due = self
                .progress_last
                .get(&id)
                .is_none_or(|last| now.duration_since(*last) >= PROGRESS_THROTTLE);
            if due {
                self.progress_last.insert(id.clone(), now);
                if !send(&self.event_tx, AgentEvent::ToolProgress { id, output }).await {
                    return Flow::Break;
                }
            }
        }
        Flow::Continue
    }

    async fn on_tool_end(&mut self, ev: &Value) -> Flow {
        let id = ev
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let is_error = ev.get("isError").and_then(Value::as_bool).unwrap_or(false);
        let output = ev.get("result").and_then(|result| {
            if ev.get("toolName").and_then(Value::as_str) == Some(cypher_proto::view::CODEMODE_TOOL)
            {
                tool_output_text(&without_codemode_header(result))
            } else {
                tool_output_text(result)
            }
        });
        // The tool settled: retire its throttle entry (no
        // more progress) and stop forwarding late updates.
        self.progress_last.remove(&id);
        self.progress_ended.insert(id.clone());
        if !send(
            &self.event_tx,
            AgentEvent::ToolResult {
                id,
                is_error,
                output,
                diff: None,
            },
        )
        .await
        {
            return Flow::Break;
        }
        Flow::Continue
    }

    async fn on_extension_error(&mut self, ev: &Value) -> Flow {
        let message = ev
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("extension error")
            .to_owned();
        if !send(&self.event_tx, AgentEvent::Error { message }).await {
            return Flow::Break;
        }
        Flow::Continue
    }

    /// The turn settled: send its Done, then park or end the run.
    async fn on_agent_settled(&mut self) -> Flow {
        // A stale duplicate (or an abort racing a settled
        // turn) must not double-Done.
        if !self.in_turn {
            return Flow::Continue;
        }
        self.in_turn = false;
        // The settled branch is final: its gauge reading
        // supersedes any per-message one that raced a
        // session write.
        refresh_context_usage(&self.client, &self.event_tx, None);
        self.done_sent = true;
        let (status, error) = if self.interrupted {
            (DoneStatus::Interrupted, None)
        } else {
            match self.last_stop_reason.as_str() {
                "error" => (
                    DoneStatus::Errored,
                    Some(
                        self.last_error_message
                            .clone()
                            .filter(|m| !m.trim().is_empty())
                            .or_else(|| {
                                (!self.last_assistant_text.is_empty())
                                    .then(|| self.last_assistant_text.clone())
                            })
                            .unwrap_or_else(|| "The agent reported an error.".into()),
                    ),
                ),
                "aborted" => (DoneStatus::Interrupted, None),
                _ => (DoneStatus::Completed, None),
            }
        };
        // Messages an extension consumed confirm before
        // the Done (the last segment then ends empty).
        // Steers pi queued but never delivered (the turn
        // settled before the steer reply streamed) are
        // stranded — an idle pi only QUEUES steers. They
        // retry as idle prompts after the park, never
        // dropped.
        if !park_turn(
            &self.event_tx,
            &self.client,
            &mut self.assistant_message_id,
            &mut self.handled_steers,
            &mut self.steers_queued,
            &mut self.prompt_backlog,
            &self.last_assistant_text,
            status,
            error,
            &self.session_file,
        )
        .await
        {
            return Flow::Break;
        }
        // An interrupt or an errored turn ends the run; a
        // clean turn parks the child + mailbox for the
        // next routed send. With the mailbox closed AND
        // nothing left to dispatch, the run ends here.
        if self.interrupted
            || status == DoneStatus::Errored
            || (!self.steering_open
                && self.idle_prompt.is_none()
                && self.steer_call.is_none()
                && self.steers_queued.is_empty()
                && self.prompt_backlog.is_empty())
        {
            return Flow::Break;
        }
        Flow::Continue
    }

    async fn on_eof(&mut self, child: &mut Child, stderr_tail: &StderrTail) -> Flow {
        // A child death while PARKED (turn already settled) ends
        // the run cleanly — the engine treats a parked stream end
        // as such. Mid-turn, it is a crash.
        if self.done_sent && !self.in_turn {
            return Flow::Break;
        }
        if self.interrupted {
            self.done_sent = true;
            let _ = self
                .event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: Some(self.session_file.clone()),
                }))
                .await;
        } else {
            self.done_sent = true;
            let status = child.try_wait().ok().flatten();
            let _ = self
                .event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(crash_message(AGENT_NAME, status, stderr_tail)),
                    session_id: Some(self.session_file.clone()),
                }))
                .await;
        }
        Flow::Break
    }

    fn on_interrupt(&mut self, child: &Child) -> Flow {
        self.interrupt_sent = true;
        self.interrupted = true;
        if self.in_turn {
            self.client.send("abort", Map::new());
            // Escalate if pi doesn't wind down (agent_settled) within
            // the grace periods.
            if let Some(pid) = child.id() {
                let (interrupt_grace, kill_grace) = (self.interrupt_grace, self.kill_grace);
                self.escalation = Some(tokio::spawn(async move {
                    tokio::time::sleep(interrupt_grace).await;
                    send_signal(pid, Signal::Term);
                    tokio::time::sleep(kill_grace).await;
                    send_signal(pid, Signal::Kill);
                }));
            }
        } else {
            // Idle between turns: nothing to abort — the terminal
            // bookkeeping below still guarantees Done { Interrupted }.
            return Flow::Break;
        }
        Flow::Continue
    }

    async fn on_no_activity(&mut self) -> Flow {
        // The prompt was accepted but no agent event ever arrived
        // (an extension command whose handler only notifies, say).
        // Settle this TURN with the notify output, but keep a
        // steerable child parked exactly like `agent_settled`.
        // Stateful extension commands can keep toggles in process
        // memory; reaping here made every invocation start from the
        // configured default instead of observing the previous turn.
        self.in_turn = false;
        self.done_sent = true;
        // The turn's routed messages settle exactly as at
        // `agent_settled`: consumed ones confirm, and a steer an idle
        // pi only queued retries after the park.
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
        // If nobody can route another turn, there is no reason to
        // retain the child. Otherwise remain parked until a mailbox
        // prompt, interrupt, child exit, or sender close arrives.
        if !self.steering_open
            && self.idle_prompt.is_none()
            && self.steer_call.is_none()
            && self.steers_queued.is_empty()
            && self.prompt_backlog.is_empty()
        {
            return Flow::Break;
        }
        Flow::Continue
    }
}

/// The in-flight `prompt` RPC's response (the branch is guarded on `Some`).
async fn prompt_response(
    slot: &mut Option<BoxFuture<'static, Result<Value, HarnessError>>>,
) -> Result<Value, HarnessError> {
    slot.as_mut().expect("guarded by if").await
}

/// The per-run event loop: one task multiplexing agent events, the steering
/// mailbox, the interrupt token, and consumer liveness.
pub async fn run_session(session: Session) {
    let Session {
        mut child,
        client,
        mut incoming,
        event_tx,
        controls,
        request,
        interrupt_grace,
        kill_grace,
        handshake_timeout,
        no_activity_grace,
        model_catalog_wait,
        stderr_tail,
        intercept,
        temp_prompt,
        mcp_servers,
    } = session;
    // Dropped at the end of every path — the temp prompt file never leaks.
    let _temp_prompt = TempPromptGuard(temp_prompt);
    let RunControls {
        request_input,
        mut steering,
        interrupt,
        host,
    } = controls;
    let _host = host; // child env was already applied at spawn; kept for clarity
    let request_input = std::sync::Arc::new(request_input);

    let handshake = handshake(
        &client,
        &request,
        &mut child,
        &stderr_tail,
        &interrupt,
        handshake_timeout,
        model_catalog_wait,
    );
    let (session_file, model_name) = match handshake.await {
        Ok(ready) => ready,
        Err(error) => {
            let status = match error {
                Some(_) => DoneStatus::Errored,
                None => DoneStatus::Interrupted,
            };
            let done = AgentEvent::Done {
                status,
                result: None,
                error,
                session_id: None,
            };
            let _ = event_tx.send(Ok(done)).await;
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
    };

    let assistant_message_id = new_message_id();
    if !send(
        &event_tx,
        AgentEvent::SessionStarted {
            harness: HarnessId::Pi,
            model: if model_name.is_empty() {
                request.model.clone().unwrap_or_default()
            } else {
                model_name
            },
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: session_file.clone(),
            assistant_message_id: assistant_message_id.clone(),
        },
    )
    .await
    {
        shutdown_child(&mut child, kill_grace).await;
        return;
    }

    // ---- synthesized built-in command interception -------------------------
    // The current assistant message's streamed text (Done's `result` and the
    // error text for an `error` stopReason). The built-ins below also feed it.
    let mut last_assistant_text = String::new();
    // `/compact` and `/export-html` are pi built-in TUI commands with RPC
    // equivalents: pi's `get_commands` never advertises built-ins and sending
    // one as prompt text would not execute it. A same-name extension/prompt/
    // skill command discovered in the cache wins (interception skipped — pi
    // handles it); otherwise the harness dispatches the RPC directly and Dones
    // immediately (no agent stream to wait on, so no no-activity grace).
    // compaction_start/end events, if any, stay ignored like the main loop's.
    match intercept_builtin(
        &client,
        &event_tx,
        &request.prompt,
        &session_file,
        intercept,
        &mut last_assistant_text,
    )
    .await
    {
        InterceptOutcome::Handled => {
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
        InterceptOutcome::Passthrough => {}
    }

    // The first prompt is dispatched FROM the main loop (same path as a
    // parked restart). Real pi only ACKs an extension command after its
    // handler returns — and handlers like `/subagent-config` block on
    // `ctx.ui.select` first. Awaiting the ACK here would deadlock: the
    // select arrives as `Incoming::UiRequest`, which is only drained in
    // the loop.
    let prompt_params = first_prompt(&request);
    let first_prompt_client = client.clone();

    let mut run = PiRun {
        client,
        event_tx,
        session_file,
        mcp_servers,
        request_input,
        intercept,
        no_activity_grace,
        interrupt_grace,
        kill_grace,
        assistant_message_id,
        last_assistant_text,
        last_stop_reason: "stop".to_owned(),
        last_error_message: None,
        interrupted: false,
        interrupt_sent: false,
        done_sent: false,
        progress_last: HashMap::new(),
        progress_ended: HashSet::new(),
        throughput: throughput::ThroughputMeter::default(),
        agent_started: false,
        in_turn: true,
        steering_open: true,
        steers_queued: VecDeque::new(),
        handled_steers: 0,
        steer_call: None,
        steer_backlog: VecDeque::new(),
        atomic_steer: false,
        idle_prompt: Some(Box::pin(async move {
            first_prompt_client.request("prompt", prompt_params).await
        })),
        had_ui: false,
        prompt_is_command: request.prompt.trim_start().starts_with('/'),
        prompt_backlog: VecDeque::new(),
        escalation: None,
        no_activity: Box::pin(tokio::time::sleep(no_activity_grace)),
    };

    loop {
        match run.open_parked_turn().await {
            Some(Flow::Break) => break,
            Some(Flow::Continue) => continue,
            None => {}
        }
        let flow = tokio::select! {
            // biased: queued output drains before the no-activity grace can
            // fire (a zero grace still yields to `incoming` first).
            biased;
            res = prompt_response(&mut run.idle_prompt), if run.idle_prompt.is_some() => {
                run.on_prompt_response(res).await
            }
            inc = incoming.recv() => match inc {
                Some(Incoming::Event(ev)) => run.on_event(ev).await,
                Some(Incoming::Response { id, result }) => {
                    run.on_steer_response(id, result).await
                }
                Some(Incoming::UiRequest { id, method, payload }) => {
                    run.on_ui_request(id, method, payload).await
                }
                Some(Incoming::Eof) | None => {
                    run.on_eof(&mut child, &stderr_tail).await
                }
            },
            steer = steering.recv(), if run.steering_open && !run.interrupted => {
                run.on_steer(steer)
            }
            _ = interrupt.cancelled(), if !run.interrupt_sent => run.on_interrupt(&child),
            _ = &mut run.no_activity,
                if !run.agent_started && !run.done_sent && run.idle_prompt.is_none() =>
            {
                run.on_no_activity().await
            }
            _ = run.event_tx.closed() => Flow::Break,
        };
        if let Flow::Break = flow {
            break;
        }
    }

    run.finish(&mut child, &stderr_tail).await;
}

/// Handshake + session setup, raced against the interrupt and bounded by
/// `handshake_timeout`. Returns the session file and model name; an error is
/// the setup failure's message, or `None` when the interrupt won.
async fn handshake(
    client: &PiClient,
    request: &RunRequest,
    child: &mut Child,
    stderr_tail: &StderrTail,
    interrupt: &CancellationToken,
    handshake_timeout: Duration,
    model_catalog_wait: Duration,
) -> Result<(String, String), Option<String>> {
    let setup = setup_session(client, request, model_catalog_wait);
    let error = tokio::select! {
        res = tokio::time::timeout(handshake_timeout, setup) => {
            let res = res.unwrap_or_else(|_| Err(HarnessError::Protocol(format!(
                "pi did not complete the RPC handshake within {}s (the agent \
                 may be waiting for a login — try running it once in a terminal)",
                handshake_timeout.as_secs()
            ))));
            match res {
                Ok(ready) => return Ok(ready),
                Err(e) => e,
            }
        },
        _ = interrupt.cancelled() => return Err(None),
    };
    let error = match child.try_wait() {
        Ok(Some(status)) => {
            tokio::time::sleep(Duration::from_millis(200)).await;
            format!(
                "{error}; {}",
                crash_message(AGENT_NAME, Some(status), stderr_tail)
            )
        }
        _ => match stderr_tail.snapshot() {
            Some(tail) => format!("{error}; stderr: {tail}"),
            None => error.to_string(),
        },
    };
    tracing::warn!(%error, "pi setup failed");
    Err(Some(error))
}

/// The first prompt's params. Attachments are inlined ONLY here: routed
/// mailbox messages carry none, and re-sending them would duplicate.
fn first_prompt(request: &RunRequest) -> Map<String, Value> {
    let mut params = Map::new();
    params.insert("message".into(), Value::String(request.prompt.clone()));
    if let Some(images) = inline_images(&request.attachments) {
        params.insert("images".into(), images);
    }
    params
}

impl PiRun {
    /// Terminal bookkeeping once the loop ends: send the Done the run still
    /// owes, stop the interrupt escalation, then reap the child.
    async fn finish(self, child: &mut Child, stderr_tail: &StderrTail) {
        // Terminal bookkeeping: never end the stream without a Done unless the
        // consumer already hung up.
        if !self.event_tx.is_closed() && !self.done_sent {
            if self.interrupted {
                let _ = self
                    .event_tx
                    .send(Ok(AgentEvent::Done {
                        status: DoneStatus::Interrupted,
                        result: None,
                        error: None,
                        session_id: Some(self.session_file.clone()),
                    }))
                    .await;
            } else {
                let status = child.try_wait().ok().flatten();
                let _ = self
                    .event_tx
                    .send(Ok(AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(crash_message(AGENT_NAME, status, stderr_tail)),
                        session_id: Some(self.session_file.clone()),
                    }))
                    .await;
            }
        }

        // Escalation dies BEFORE the child is reaped: after `shutdown_child`
        // waits the pid, a still-armed SIGTERM/SIGKILL timer would fire at a
        // freed (reusable) pid.
        if let Some(handle) = self.escalation {
            handle.abort();
        }
        shutdown_child(child, self.kill_grace).await;
    }
}

/// `RunRequest.attachments` (absolute paths already staged on the run device)
/// → pi `prompt`/`steer` `images` blocks. Only the formats provider vision
/// APIs accept inline (PNG/JPEG/GIF/WebP) — an image block in any other type
/// (BMP, SVG, TIFF…) is rejected by the provider and fails the whole turn.
/// Everything else is left to the prompt text refs.
pub fn inline_images(paths: &[String]) -> Option<Value> {
    use base64::Engine as _;
    let mut images: Vec<Value> = Vec::new();
    for path in paths {
        let mime = mime_for_path(path);
        if !matches!(
            mime.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            tracing::debug!(%path, "attachment unreadable");
            continue;
        };
        images.push(json!({
            "type": "image",
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            "mimeType": mime,
        }));
    }
    (!images.is_empty()).then_some(Value::Array(images))
}

/// Guess a MIME type from the file extension (attachments carry no explicit
/// type). Unknown extensions default to octet-stream and are never inlined.
fn mime_for_path(path: &str) -> String {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        _ => "application/octet-stream",
    }
    .to_owned()
}
