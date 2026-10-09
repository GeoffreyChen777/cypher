//! The run task: drives one harness stream into the journal, the broadcast,
//! and the chat doc's folded entries.

use super::*;

/// Apply the render-parts privacy policy: strip heavy/sensitive tool inputs before doc
/// entry. Full inputs live only in the local run journal.
pub(super) fn render_parts(parts: &[MessagePart]) -> Vec<MessagePart> {
    parts
        .iter()
        .map(|part| match part {
            MessagePart::Tool {
                id,
                call,
                is_error,
                resolved,
                output,
                progress,
                diff,
                output_ref,
                output_bytes,
                diff_ref,
                diff_stats,
            } => MessagePart::Tool {
                id: id.clone(),
                call: sanitize_tool_call(call),
                is_error: *is_error,
                resolved: *resolved,
                // Output summaries, diff stats, and sidecar refs are
                // deliberately kept: unlike raw tool inputs they are the
                // transcript's record of what happened, and the strip already
                // bounded them (docs/chat2-sync.md A1). The transient
                // progress tail rides through too — the fold clears it on
                // resolve, so it never survives a settled chip.
                output: output.clone(),
                progress: progress.clone(),
                diff: diff.clone(),
                output_ref: output_ref.clone(),
                output_bytes: *output_bytes,
                diff_ref: diff_ref.clone(),
                diff_stats: diff_stats.clone(),
            },
            other => other.clone(),
        })
        .collect()
}

/// The effective prompt as `harness` may receive it. A quote selected from a
/// displayed translation carries alignment input holding that translation;
/// only Pi runs the extension that resolves and removes it, so any other
/// harness (the mock) gets it stripped here and reads the original passage
/// the quote already holds.
pub(crate) fn agent_prompt_for(harness: HarnessId, agent_prompt: Option<String>) -> Option<String> {
    match harness {
        HarnessId::Pi => agent_prompt,
        _ => agent_prompt.map(|prompt| cypher_proto::agent_prompt::strip_alignment(&prompt)),
    }
}

/// The persisted assistant text of a folded segment (workspace preview source).
pub(super) fn folded_text(parts: &[MessagePart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The segment being written: the parts folded so far and the models that
/// answered them (one per completed assistant message).
struct Segment<'s> {
    folded: &'s [MessagePart],
    models: &'s [AnsweredModel],
}

fn sync_segment<'a>(
    doc: &'a SessionDoc,
    writer: &mut Option<SegmentWriter<'a>>,
    entry_id: &str,
    device_id: &str,
    started_at: i64,
    segment: Segment<'_>,
) -> Result<(), DocError> {
    if segment.folded.is_empty() {
        return Ok(());
    }
    let rendered = render_parts(segment.folded);
    if writer.is_none() {
        *writer = Some(SegmentWriter::begin(doc, entry_id, device_id, started_at)?);
    }
    if let Some(w) = writer.as_mut() {
        w.set_models(segment.models)?;
        w.sync(&rendered)?;
    }
    Ok(())
}

fn finish_segment<'a>(
    doc: &'a SessionDoc,
    writer: Option<SegmentWriter<'a>>,
    entry_id: &str,
    device_id: &str,
    started_at: i64,
    segment: Segment<'_>,
    status: MessageStatus,
) -> Result<(), DocError> {
    let rendered = render_parts(segment.folded);
    let mut writer = match writer {
        Some(w) => w,
        None if !segment.folded.is_empty() => {
            SegmentWriter::begin(doc, entry_id, device_id, started_at)?
        }
        None => return Ok(()),
    };
    writer.set_models(segment.models)?;
    writer.finish(&rendered, status)
}

/// Resume bookkeeping for one run task: which user entry the run answers (so
/// the startup-crash retry re-dispatches idempotently against the same doc
/// entry), whether `dispatch` injected the resume id itself (only
/// engine-injected resumes retry — a caller-specified resume fails loudly),
/// whether this run already IS the retry (one attempt only), and the
/// EFFECTIVE prompt override (the Comment feature) the retry must re-deliver.
pub(super) struct RunResumeState {
    pub(super) user_message_id: String,
    pub(super) resume_injected: bool,
    pub(super) startup_retry: bool,
    pub(super) agent_prompt: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn drive_run(
    inner: Arc<Inner>,
    chat_id: String,
    run_id: String,
    harness: Arc<dyn Harness>,
    request: RunRequest,
    doc: Arc<SessionDoc>,
    controls: RunControls,
    mut engine_rx: mpsc::UnboundedReceiver<AgentEvent>,
    mut cancel_rx: watch::Receiver<bool>,
    resume_state: RunResumeState,
) {
    let device_id = inner.device_id.clone();
    let plugin_epoch = inner.plugin_epoch.load(Ordering::SeqCst);
    // Captured for post-run auto-titling (the request moves into the harness).
    let harness_id = harness.id();
    let user_prompt = request.prompt.clone();
    let run_cwd = request.cwd.clone();
    // Kept whole for the startup-crash retry (same user entry; dispatch
    // re-injects the stored resume id). Option so the retry branch (inside
    // the event loop) can take ownership. Carries the VISIBLE prompt — the
    // retry re-derives the effective override from `resume_state.agent_prompt`.
    let mut retry_request = Some(RunRequest {
        resume: None,
        worktree: None,
        ..request.clone()
    });
    // The harness receives the EFFECTIVE prompt (visible unless an override
    // rides the command). `request` itself stays the visible truth.
    let effective = resume_state
        .agent_prompt
        .clone()
        .unwrap_or_else(|| request.prompt.clone());
    let mut harness_request = request.clone();
    harness_request.prompt = effective;
    let mut stream = match harness.run(harness_request, controls).await {
        Ok(stream) => stream,
        Err(err) => {
            let message = err.to_string();
            inner.publish(
                &chat_id,
                &AgentEvent::Error {
                    message: message.clone(),
                },
            );
            inner.publish(
                &chat_id,
                &AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(message),
                    session_id: None,
                },
            );
            // The owner never came up: nothing will ever settle its subagents.
            inner.fail_orphaned_subagents(&chat_id, "Subagent owner failed to start");
            inner.remove_run(&chat_id, &run_id);
            inner.set_status(&chat_id, SessionStatus::Errored, false);
            return;
        }
    };

    let doc_ref: &SessionDoc = &doc;
    if let Some(host) = inner.doc_host() {
        host.preview_run(&chat_id, &run_id);
    }
    let mut folded: Vec<MessagePart> = Vec::new();
    // The models that answered the current segment, beside `folded` and
    // cleared with it at every segment boundary.
    let mut segment_models: Vec<AnsweredModel> = Vec::new();
    // Every tool id this run has folded, across segment resets. Adapters
    // re-emit shape-bearing `tool_call_update`s (title/rawInput refreshes,
    // long-running completions) as full ToolCall events; once the fold has
    // reset at a steer/park boundary those ids are gone from `folded`, and
    // folding the echo would mint an orphan chip mid-text in the NEXT
    // segment — the mid-word transcript splits.
    let mut seen_tools: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entry_id = new_id();
    let mut segment_started = now_ms();
    let mut writer: Option<SegmentWriter<'_>> = None;
    let mut dirty = false;
    let mut flush_at = tokio::time::Instant::now();
    // Set when the engine interrupts the run: the harness gets this long to end its own
    // stream (its token was cancelled); past it, a terminal Done is synthesized.
    let mut interrupt_deadline: Option<tokio::time::Instant> = None;
    let mut interrupted = false;
    let mut saw_session_started = false;
    // Liveness heartbeat: this loop RUNNING is proof the harness stream is
    // open, so freshness must not depend on events arriving. Silent stretches
    // are normal and UNBOUNDED — a long tool call, redacted thinking, an
    // agent waiting on an external process, a question parked for an hour —
    // and each starved the UI's 45s staleness gate in turn (working strip /
    // AwaitingInput dot vanishing mid-run, both user-reported). No stall
    // timeout here by design (a first port was rejected — agents may
    // legitimately be quiet for >10min): a live child means Working, dying
    // paths each carry their own error, and engine death stops these ticks
    // so the gate still catches real crashes. touch_session throttles at 10s.
    let mut live_heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
    live_heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // PERSISTENT SESSION (zeron runsBySession): a completed turn on a
    // steerable harness parks here instead of ending the run — the child and
    // its steering mailbox stay warm, and the next user message (dispatch
    // routes into a live run) starts the next turn with zero respawn/resume
    // latency. `Some(when)` = idle since then; the 30-min reaper below ends
    // a session nobody comes back to (zeron SESSION_IDLE_MS).
    const SESSION_IDLE: std::time::Duration = std::time::Duration::from_secs(30 * 60);
    let mut idle_since: Option<tokio::time::Instant> = None;
    let steerable = harness.supports_steering();
    // TURN-QUIESCE WATCHDOG: a harness
    // that loses a turn's Done — the adapter never settles `session/prompt`
    // even though the agent finished — strands Working forever: the live
    // heartbeat above keeps the row fresh, and there is no per-turn timeout
    // by design. This is NOT that stall timeout: it never ends the run or
    // errors anything. When the stream has been silent past the window AND
    // the fold shows completed output with nothing in flight (no unresolved
    // tool, no open question), the turn parks exactly like a Done would —
    // segment finalized Complete, status Idle, child and mailbox warm. A
    // false trip (the agent was quietly waiting on something invisible)
    // costs a status dip: the parked-resume path below re-arms Working the
    // moment output flows again, and nothing is lost. Default 5min: long
    // silent thinking with no reasoning events must not drop the spinner.
    // `CYPHER_TURN_QUIESCE_MS` overrides the window; 0 disables.
    let quiesce_after: Option<std::time::Duration> =
        match cypher_env::var("TURN_QUIESCE_MS").and_then(|v| v.parse::<u64>().ok()) {
            Some(0) => None,
            Some(ms) => Some(std::time::Duration::from_millis(ms)),
            None => Some(std::time::Duration::from_secs(300)),
        };
    let mut last_stream_activity = tokio::time::Instant::now();
    // SELF-CONTINUED turns get a much SHORTER quiesce window. A turn the
    // agent starts on its own (background-task wake) can never receive a
    // harness Done: the adapter has no `session/prompt` outstanding to
    // settle — verified in claude-agent-acp's autonomous-result lane, which
    // consumes the SDK's turn-end without emitting anything; codex shows
    // the same shape. The watchdog is that turn shape's ONLY settle path,
    // so the normal window (then 120s) read as 2min of stuck-Working after every
    // background notification. The in-flight
    // fold gate below still protects running tools; reasoning heartbeats
    // push the window during real thinking. `CYPHER_SELF_TURN_QUIESCE_MS`
    // overrides; 0 falls back to the normal window. An explicit
    // `CYPHER_TURN_QUIESCE_MS=0` still disables the watchdog entirely.
    let self_quiesce_after: Option<std::time::Duration> =
        match cypher_env::var("SELF_TURN_QUIESCE_MS").and_then(|v| v.parse::<u64>().ok()) {
            Some(0) => None,
            Some(ms) => Some(std::time::Duration::from_millis(ms)),
            None => Some(std::time::Duration::from_secs(20)),
        };
    let mut self_continued_turn = false;
    // The last translation frame folded, so the keepalive repeats that keep a
    // slow translation's stream alive are not each journaled in full.
    let mut last_translation: Option<String> = None;

    let final_status = loop {
        let event: AgentEvent = tokio::select! {
            biased;
            changed = cancel_rx.changed(), if !interrupted => {
                let _ = changed;
                interrupted = true;
                interrupt_deadline = Some(
                    tokio::time::Instant::now() + std::time::Duration::from_secs(3),
                );
                continue;
            }
            _ = tokio::time::sleep_until(
                interrupt_deadline.unwrap_or_else(tokio::time::Instant::now)
            ), if interrupt_deadline.is_some() => AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: None,
                session_id: None,
            },
            _ = live_heartbeat.tick() => {
                inner.touch_session(&chat_id);
                continue;
            }
            // Idle reaper (zeron SESSION_IDLE_MS): a parked persistent session
            // nobody returned to in 30 minutes releases its child. The turn
            // was finalized at Done, so this end is clean — no aborted stamp.
            _ = tokio::time::sleep_until(
                idle_since.map(|at| at + SESSION_IDLE).unwrap_or_else(tokio::time::Instant::now)
            ), if idle_since.is_some() => {
                tracing::info!(chat = %chat_id, "reaping idle persistent session");
                if let Some(token) = lock(&inner.runs)
                    .get(&chat_id)
                    .filter(|h| h.run_id == run_id)
                    .map(|h| h.interrupt_token.clone())
                {
                    token.cancel();
                }
                break SessionStatus::Idle;
            }
            Some(event) = engine_rx.recv() => event,
            next = stream.next() => match next {
                Some(Ok(event)) => event,
                // A stream error while PARKED is a post-turn child death —
                // the turn was already finalized, so the run ends clean
                // instead of stamping a completed session Errored.
                Some(Err(err)) if idle_since.is_some() => {
                    tracing::warn!(chat = %chat_id, error = %err, "parked session child died; ending clean");
                    break SessionStatus::Idle;
                }
                Some(Err(err)) => AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(err.to_string()),
                    session_id: None,
                },
                None if interrupted => AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: None,
                },
                // Stream end while PARKED idle: a per-turn adapter closing
                // after its final Done — a clean end, not a crash (the turn
                // was already finalized). Persistent adapters keep the
                // stream open and never hit this.
                None if idle_since.is_some() => break SessionStatus::Idle,
                None => AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some("harness stream ended without Done".into()),
                    session_id: None,
                },
            },
            _ = tokio::time::sleep_until(flush_at), if dirty => {
                // Coalesced STREAM_COMMIT_MS tick: one doc commit per window.
                let segment = Segment { folded: &folded, models: &segment_models };
                if let Err(err) = sync_segment(
                    doc_ref, &mut writer, &entry_id, &device_id, segment_started, segment,
                ) {
                    tracing::warn!(chat = %chat_id, error = %err, "segment sync failed");
                }
                dirty = false;
                continue;
            }
            // Turn-quiesce watchdog (see the knob above). Armed only when the
            // fold says nothing is in flight: an unresolved tool part is a
            // command still running (legitimately silent for minutes — the
            // rejected stall-timeout case), an unresolved input part is a
            // question awaiting the user. The live-plan chip is exempt from
            // the tool check: it is a singleton that never resolves. An EMPTY
            // fold still arms — a Steered boundary that no output ever
            // follows is one of the wedge shapes — it just parks without
            // writing a segment (an empty finalize would leave a stub entry).
            _ = tokio::time::sleep_until({
                let mut window = quiesce_after.unwrap_or_default();
                if self_continued_turn && let Some(short) = self_quiesce_after {
                    window = window.min(short);
                }
                last_stream_activity + window
            }), if quiesce_after.is_some()
                && idle_since.is_none()
                && !interrupted
                && steerable
                && !folded.iter().any(|p| match p {
                    MessagePart::Tool { id, resolved: false, .. } => {
                        id != cypher_proto::LIVE_PLAN_TOOL_ID
                    }
                    MessagePart::Input { resolved: false, .. } => true,
                    _ => false,
                }) =>
            {
                tracing::warn!(
                    chat = %chat_id,
                    quiet_ms = quiesce_after.unwrap_or_default().as_millis() as u64,
                    "turn quiesced: stream silent after completed output with no \
                     turn-end; parking (suspected missing harness Done)"
                );
                if !folded.is_empty() || writer.is_some() {
                    if let Err(err) = finish_segment(
                        doc_ref,
                        writer.take(),
                        &entry_id,
                        &device_id,
                        segment_started,
                        Segment { folded: &folded, models: &segment_models },
                        MessageStatus::Complete,
                    ) {
                        tracing::warn!(chat = %chat_id, error = %err, "quiesce segment finish failed");
                    }
                    inner.note_message(&chat_id, &folded_text(&folded));
                    if let Some(host) = inner.doc_host() { host.flush_chat_sync(&chat_id); }
                }
                folded.clear();
                segment_models.clear();
                dirty = false;
                entry_id = new_id();
                segment_started = now_ms();
                idle_since = Some(tokio::time::Instant::now());
                self_continued_turn = false;
                inner.set_status(&chat_id, SessionStatus::Idle, false);
                continue;
            }
        };

        // SubagentStatus is a LIVE PROJECTION, not run activity: mirror it
        // onto the chat's session row and stop — no journal append, no doc
        // fold, no status transition, no session freshness touch, and no
        // parked-session resume or quiesce-watchdog push (a background
        // subagent finishing must never re-arm a parked session as Working;
        // `set_subagents`' own updated_at bump keeps the row fresh instead).
        if let AgentEvent::SubagentStatus { runs } = &event {
            inner.set_subagents(&chat_id, runs.clone());
            continue;
        }
        // The context gauge is the same kind of projection: mirrored onto
        // the local session row, never journaled, folded, or allowed to
        // resume a parked session (Claude re-reports it after compaction).
        if let AgentEvent::ContextUsage { used, size } = event {
            inner.set_context_usage(&chat_id, ContextUsage { used, size });
            continue;
        }
        // Throughput too: a live reading for the working trailer, never
        // run activity (the extension's final clear lands after agent_end).
        if let AgentEvent::Throughput { throughput } = event {
            inner.set_throughput(&chat_id, throughput);
            continue;
        }
        // A prompt's translation belongs to the USER entry it replaced, which
        // was written before the run saw it — stamped there directly, never
        // folded into the assistant segment. Handled ahead of the parked gate
        // because the extension translates a prompt BEFORE the turn it opens
        // starts, while the session may still be parked from the last one.
        if let AgentEvent::InputTranslation { source, text } = &event {
            match doc_ref.stamp_user_agent_text(source, text) {
                Ok(true) => {
                    if let Some(host) = inner.doc_host() {
                        host.flush_chat_sync(&chat_id);
                    }
                }
                Ok(false) => {
                    tracing::debug!(chat = %chat_id, "prompt translation matched no recent user entry");
                }
                Err(err) => {
                    tracing::warn!(chat = %chat_id, error = %err, "prompt translation stamp failed");
                }
            }
            continue;
        }

        // Any stream activity proves the run is alive — keep the session's
        // freshness inside the UI's 45s staleness window (throttled), and
        // push the quiesce watchdog's window out.
        inner.touch_session(&chat_id);
        last_stream_activity = tokio::time::Instant::now();
        // The engine's input bridge is the sole authority on input requests:
        // it mints the id and parks the resolver BEFORE emitting the event,
        // so a legitimate id is always pending here. A harness emitting its
        // own copy (a different id no resolver knows) would fold an
        // unanswerable twin chip into the doc — and answering the twin would
        // never resume the run. Dropped BEFORE the parked gate below: letting
        // it un-park the session and then dropping it would disarm the idle
        // reaper with no turn running (a leaked child no reap ever ends).
        if let AgentEvent::InputRequested { request_id, .. } = &event {
            let pending = lock(&inner.runs)
                .get(&chat_id)
                .map(|h| h.pending_inputs.clone());
            let known = pending.is_some_and(|p| lock(&p).contains_key(request_id));
            if !known {
                tracing::warn!(
                    chat = %chat_id,
                    request = %request_id,
                    "dropping harness-emitted InputRequested (unknown id; \
                     the engine input bridge owns this lifecycle)"
                );
                continue;
            }
        }
        // PARKED: a steer boundary, a terminal Done, or SELF-CONTINUED OUTPUT
        // re-opens the session; everything else stays gated. The ACP child
        // keeps forwarding `session/update` frames after a turn completes,
        // and they split two ways:
        //
        // - Post-turn NOISE — late tool_call_updates for commands folded in a
        //   prior segment, command refreshes, reasoning heartbeats. Treating
        //   those as "the next turn" re-armed Working with no Done ever
        //   coming (the eternally-running session bug) and folded orphan
        //   parts into a phantom segment. Still dropped.
        // - SELF-CONTINUED WORK — Claude Code re-invokes itself when a
        //   background task finishes (turns no prompt started) and streams
        //   real output for them. Dropping those LOST transcript content
        //   (agent output that never reached the doc). Fresh text or a genuinely new tool
        //   call resumes the session: new segment, Working, and the turn
        //   settles again via Done — or via the quiesce watchdog, which is
        //   what makes this resume safe where the naive version was not.
        //
        // The RESUME_GATE separates the two by arrival time: a finished
        // turn's tail flush lands within milliseconds of its Done, while a
        // self-continued turn starts a whole new agent round trip (seconds
        // at minimum, minutes in the incident). Inside the gate everything
        // non-boundary stays inert, exactly as before.
        const RESUME_GATE: std::time::Duration = std::time::Duration::from_secs(1);
        if idle_since.is_some() {
            let self_continued = idle_since
                .is_some_and(|parked_at| parked_at.elapsed() >= RESUME_GATE)
                && (matches!(
                    &event,
                    AgentEvent::TextDelta { text } if !text.is_empty()
                ) || matches!(
                    &event,
                    AgentEvent::ToolCall { id, .. }
                        if id == cypher_proto::LIVE_PLAN_TOOL_ID || !seen_tools.contains(id)
                ));
            if self_continued {
                tracing::info!(
                    chat = %chat_id,
                    "parked session resumed by self-continued agent output"
                );
                idle_since = None;
                self_continued_turn = true;
                // The park cleared the fold; rotate to a fresh entry and
                // fall through — this event is the new segment's first part.
                entry_id = new_id();
                segment_started = now_ms();
                inner.set_status(&chat_id, SessionStatus::Working, true);
            } else {
                match &event {
                    AgentEvent::Steered { .. } => {
                        idle_since = None;
                        inner.set_status(&chat_id, SessionStatus::Working, true);
                    }
                    AgentEvent::Done { .. } => {
                        idle_since = None;
                    }
                    // A question with NO turn behind it (post-turn permission
                    // noise): answer it empty. But a routed send already flipped
                    // Working — `/subagent-config` and friends ARE the turn,
                    // and auto-declining them hangs the extension handler.
                    AgentEvent::InputRequested { request_id, .. } => {
                        let live = lock(&inner.statuses).get(&chat_id).is_some_and(|s| {
                            matches!(
                                s.status,
                                SessionStatus::Working | SessionStatus::AwaitingInput
                            )
                        });
                        if live {
                            idle_since = None;
                        } else {
                            let resolver = lock(&inner.runs)
                                .get(&chat_id)
                                .and_then(|h| lock(&h.pending_inputs).remove(request_id));
                            if let Some(tx) = resolver {
                                let _ = tx.send(Vec::new());
                            }
                            tracing::debug!(chat = %chat_id, "parked session: post-turn input request auto-declined");
                            continue;
                        }
                    }
                    // A stale answer settling after its turn already closed:
                    // nothing is running — stay parked.
                    AgentEvent::InputResolved { .. } => continue,
                    _ => continue,
                }
            }
        }
        // Empty reasoning deltas are PURE heartbeats: redacted thinking and
        // tool-input-generation windows stream them with no text. They fold
        // to nothing, so journaling/publishing them is only noise (hundreds
        // per long turn observed) — the touch above already did their job.
        if matches!(&event, AgentEvent::ReasoningDelta { text } if text.is_empty()) {
            continue;
        }
        // Same reasoning for a repeated translation frame. A frame carries the
        // WHOLE rendering, so one identical to the frame right before it is a
        // keepalive: the translation extension re-states the current text every
        // few seconds so a slow translation cannot go quiet and be parked by
        // the watchdog above. The touch already did that job, and folding it
        // would assign the text that is already there — journaling and
        // broadcasting a full copy of the answer per tick would be pure noise.
        // Only an UNBROKEN repeat is dropped, so a later turn whose answer
        // happens to translate identically still renders.
        match &event {
            AgentEvent::Translation { text }
                if last_translation.as_deref() == Some(text.as_str()) =>
            {
                continue;
            }
            AgentEvent::Translation { text } => last_translation = Some(text.clone()),
            _ => last_translation = None,
        }

        // Stale tool echoes: a ToolCall/ToolResult naming an id folded in a
        // PRIOR segment (the fold reset at a steer or park since) belongs to
        // a chip that already rendered in its own entry. Folding the echo
        // would splice a phantom chip into the middle of the current
        // segment's streaming text (the mid-word transcript splits). Dropped;
        // same-segment refreshes still land in place.
        let in_segment = |folded: &[MessagePart], id: &str| {
            folded
                .iter()
                .any(|p| matches!(p, MessagePart::Tool { id: pid, .. } if pid == id))
        };
        match &event {
            // The live plan chip is a deliberate singleton (every update
            // reuses `LIVE_PLAN_TOOL_ID` so the fold refreshes in place):
            // treating its reappearance after a park/steer reset as a stale
            // echo dropped the todo list for the rest of the run — from the
            // first boundary on, plans never rendered again.
            AgentEvent::ToolCall { id, .. } if id == cypher_proto::LIVE_PLAN_TOOL_ID => {}
            AgentEvent::ToolResult { id, .. } if id == cypher_proto::LIVE_PLAN_TOOL_ID => {}
            AgentEvent::ToolCall { id, .. } => {
                if !in_segment(&folded, id) && seen_tools.contains(id) {
                    continue;
                }
                seen_tools.insert(id.clone());
            }
            AgentEvent::ToolResult { id, .. }
                if !in_segment(&folded, id) && seen_tools.contains(id) =>
            {
                continue;
            }
            // `ToolProgress` deliberately falls through: it arrives with its
            // ToolCall in the SAME segment, so it is never a stale echo here
            // — and the doc fold is the single authority on whether to apply
            // it (id known + !resolved). A cross-segment tick reaches the
            // fold with no matching part and is a no-op there; a post-resolve
            // tick is ignored the same way. No exemption arm needed.
            _ => {}
        }

        // Startup-crash retry: a run that dies before ever starting (errored
        // Done, no SessionStarted, nothing streamed) means the AGENT CHILD
        // failed to come up — not that the injected resume id was bad. Since
        // the ACP conversion a stale id is handled inside the
        // harness (`session/load` falls back to `session/new`), so the old
        // guess here — tombstone the id, retry fresh — fired only on child
        // startup failures and permanently severed GOOD conversations. The id stays; retry ONCE against the same
        // user entry, resume and all, in case the crash was transient. A
        // helper that is down hard fails the retry too and surfaces its
        // crash text (the harness now appends exit status + stderr).
        if resume_state.resume_injected
            && !resume_state.startup_retry
            && !saw_session_started
            && folded.is_empty()
            && !interrupted
            && matches!(
                &event,
                AgentEvent::Done {
                    status: DoneStatus::Errored,
                    ..
                }
            )
            && let Some(retry) = retry_request.take()
        {
            tracing::warn!(
                chat = %chat_id,
                "run died before session start; retrying once (resume kept)"
            );
            // This harness owner is gone (nothing ever started under it).
            inner.fail_orphaned_subagents(&chat_id, "Subagent owner failed to start");
            inner.remove_run(&chat_id, &run_id);
            let engine = SessionsEngine {
                inner: inner.clone(),
            };
            let chat = chat_id.clone();
            let message_id = resume_state.user_message_id.clone();
            tokio::spawn(async move {
                // The user entry write inside dispatch is idempotent by
                // message id; `startup_retry` makes this attempt final.
                if let Err(err) = engine
                    .dispatch_with(
                        &chat,
                        harness_id,
                        retry,
                        resume_state.agent_prompt.clone(),
                        Some(message_id),
                        true,
                    )
                    .await
                {
                    tracing::error!(chat = %chat, error = %err, "startup-crash retry dispatch failed");
                    // No run is coming: leaving the row Working would spin
                    // the session forever with nothing behind it.
                    engine
                        .inner
                        .set_status(&chat, SessionStatus::Errored, false);
                }
            });
            return;
        }

        // A steer boundary splits the assistant entry exactly where the fold resets.
        if let AgentEvent::Steered {
            next_assistant_message_id,
            ..
        } = &event
        {
            inner.publish(&chat_id, &event);
            // A steer boundary means a real prompt owns the turn again — its
            // Done will come; the short self-continued window stands down.
            self_continued_turn = false;
            if let Err(err) = finish_segment(
                doc_ref,
                writer.take(),
                &entry_id,
                &device_id,
                segment_started,
                Segment {
                    folded: &folded,
                    models: &segment_models,
                },
                MessageStatus::Complete,
            ) {
                tracing::warn!(chat = %chat_id, error = %err, "segment finish failed");
            }
            if let Some(host) = inner.doc_host() {
                host.flush_chat_sync(&chat_id);
            }
            inner.note_message(&chat_id, &folded_text(&folded));
            folded.clear();
            segment_models.clear();
            dirty = false;
            entry_id = next_assistant_message_id.clone().unwrap_or_else(new_id);
            segment_started = now_ms();
            // The elapsed timer is per user message, not per child process: a
            // steer boundary restarts it (matches the parked-resume path and
            // the composer's optimistic overlay, which already reads 0:00).
            inner.set_status(&chat_id, SessionStatus::Working, true);
            // The boundary confirms delivery of the oldest accepted steer —
            // retire its at-least-once ledger entry.
            if let Some(h) = lock(&inner.runs)
                .get(&chat_id)
                .filter(|h| h.run_id == run_id)
            {
                lock(&h.routed_steers).pop_front();
            }
            continue;
        }

        match &event {
            AgentEvent::SessionStarted {
                session_id, cwd, ..
            } => {
                saw_session_started = true;
                // The event's own cwd (where the harness actually created the
                // session) scopes the stored id, not the request's.
                inner.remember_harness_session(&chat_id, session_id, cwd);
            }
            AgentEvent::Done {
                session_id: Some(session_id),
                ..
            } => {
                inner.remember_harness_session(&chat_id, session_id, &run_cwd);
            }
            AgentEvent::InputRequested { .. } => {
                // Known-id guaranteed: the unknown-id twin was dropped above,
                // before the parked gate.
                inner.set_status(&chat_id, SessionStatus::AwaitingInput, false);
            }
            AgentEvent::InputResolved { .. } => {
                inner.set_status(&chat_id, SessionStatus::Working, false);
            }
            // Lands on the segment with its next sync or finish (a steer
            // splits only at the NEXT message's start, so this message's
            // model labels the segment it wrote).
            AgentEvent::AssistantMessageCompleted {
                model: Some(model), ..
            } if !segment_models.contains(model) => {
                segment_models.push(model.clone());
            }
            _ => {}
        }

        inner.publish(&chat_id, &event);

        // Defensive rule from zeron: a mid-run SessionStarted re-emission (Claude SDK
        // background re-invocations) must not wipe the segment being written.
        let skip_fold = matches!(&event, AgentEvent::SessionStarted { .. }) && !folded.is_empty();
        if !skip_fold {
            fold_event_into_parts(&mut folded, &event);
            // R2 sidecar is parked: the fold's bounded output summary and
            // diff stats are the doc-resident record used by the transcript.
            // Full output survives only in the host's local run journal.
            // To add a full-output affordance later, reintroduce
            // `cypher_doc::sidecar_payload(&event)` →
            // `apply_sidecar_refs` → `doc_host.upload_tool_sidecar`; all
            // supporting code remains in place and tested.
        }

        if let AgentEvent::Done { status, .. } = &event {
            // A question still pending at turn end can never be legitimately
            // answered (its turn is over): drain the resolvers NOW, or a late
            // `respond_input` finds one, emits InputResolved, and un-parks
            // the session into Working with no turn behind it — stranded
            // Working, timer forever, reaper disarmed. Empty answers unblock
            // the harness-side bridge like an interrupt does.
            let pending = lock(&inner.runs)
                .get(&chat_id)
                .filter(|h| h.run_id == run_id)
                .map(|h| h.pending_inputs.clone());
            if let Some(pending) = pending {
                for (_, tx) in lock(&pending).drain() {
                    let _ = tx.send(Vec::new());
                }
            }
            let message_status = match status {
                DoneStatus::Interrupted => MessageStatus::Aborted,
                DoneStatus::Completed | DoneStatus::Errored => MessageStatus::Complete,
            };
            // No dangling chips: a run that ends for ANY reason (completed,
            // errored, interrupted) terminally resolves its input parts — an
            // unresolved question must not outlive the run that asked it
            // (its resolver died with the run; an answer could never land).
            for part in folded.iter_mut() {
                if let MessagePart::Input { resolved, .. } = part {
                    *resolved = true;
                }
            }
            // A Done landing on a PARKED session with nothing streamed (the
            // idle reaper's or an interrupt's own teardown) has no entry to
            // finalize — writing one would leave an empty aborted stub.
            let nothing_streamed = writer.is_none() && folded.is_empty();
            if !nothing_streamed {
                if let Err(err) = finish_segment(
                    doc_ref,
                    writer.take(),
                    &entry_id,
                    &device_id,
                    segment_started,
                    Segment {
                        folded: &folded,
                        models: &segment_models,
                    },
                    message_status,
                ) {
                    tracing::warn!(chat = %chat_id, error = %err, "final segment finish failed");
                }
                inner.note_message(&chat_id, &folded_text(&folded));
                if let Some(host) = inner.doc_host() {
                    host.flush_chat_sync(&chat_id);
                }
            }
            if *status == DoneStatus::Completed {
                // A cleanly completed turn resets the auto-resume revival
                // budget: only consecutive crash-revive-crash cycles spend it.
                inner.journal.clear_resume_attempts(&chat_id);
            }
            // Exchange completed on an untitled chat → name it (fire-and-forget;
            // interrupted/errored turns never trigger naming).
            if *status == DoneStatus::Completed
                && !inner.is_ephemeral(&chat_id)
                && let Some(titles) = inner.titles.get()
            {
                titles.maybe_generate(&chat_id, harness_id, &user_prompt, &run_cwd);
            }
            // PERSISTENT SESSION: a cleanly completed turn on a steerable
            // harness PARKS instead of ending — child + mailbox stay warm for
            // the next routed dispatch; per-turn state resets for it.
            // A Pi-package change mid-turn bumps `plugin_epoch`: skip the park
            // so the next send respawns against the new settings.json.
            if *status == DoneStatus::Completed && steerable && !interrupted {
                if inner.plugin_epoch.load(Ordering::SeqCst) != plugin_epoch {
                    tracing::info!(
                        chat = %chat_id,
                        "ending session so the next turn loads updated Pi packages"
                    );
                    break SessionStatus::Idle;
                }
                folded.clear();
                segment_models.clear();
                dirty = false;
                entry_id = new_id();
                segment_started = now_ms();
                // Resume-retry is strictly a first-turn concern.
                saw_session_started = true;
                idle_since = Some(tokio::time::Instant::now());
                self_continued_turn = false;
                inner.set_status(&chat_id, SessionStatus::Idle, false);
                continue;
            }
            break match status {
                DoneStatus::Errored => SessionStatus::Errored,
                _ => SessionStatus::Idle,
            };
        }

        if !folded.is_empty() && !dirty {
            dirty = true;
            flush_at =
                tokio::time::Instant::now() + std::time::Duration::from_millis(STREAM_COMMIT_MS);
        }
    };

    // Claim any accepted-but-unconfirmed steers BEFORE the handle goes away:
    // a routed send that raced this exit either finds its entry gone (we own
    // it — re-dispatched below) or reclaims it and starts a fresh run itself.
    let orphans: Vec<RoutedSteer> = lock(&inner.runs)
        .get(&chat_id)
        .filter(|h| h.run_id == run_id)
        .map(|h| std::mem::take(&mut *lock(&h.routed_steers)).into())
        .unwrap_or_default();
    // The harness owner for this chat has ended (stream EOF/error, interrupt,
    // idle reaper, or a final Done on a non-parked harness). Any subagent run
    // still projected Running can never settle on its own — terminalize it.
    // This deliberately does NOT fire on a parked Done (steerable sessions
    // keep the stream open; a legal background subagent stays Running on it).
    let owner_reason = if interrupted {
        "Subagent owner interrupted"
    } else if final_status == SessionStatus::Errored {
        "Subagent owner ended in error"
    } else {
        "Subagent owner session ended"
    };
    inner.fail_orphaned_subagents(&chat_id, owner_reason);
    inner.remove_run(&chat_id, &run_id);
    inner.set_status(&chat_id, final_status, false);
    if !interrupted && !orphans.is_empty() {
        // The dying run accepted these into its mailbox but never confirmed a
        // Steered boundary (idle-reaper race, a mid-turn error discarding
        // queued boundary steers, a parked child death). Their user entries
        // are already in the transcript — a message that shows as sent must
        // never silently not run. Re-dispatch each as a fresh turn
        // (write_user_message dedupes by id; resume is engine-injected).
        let engine = SessionsEngine {
            inner: inner.clone(),
        };
        let chat = chat_id.clone();
        tokio::spawn(async move {
            for steer in orphans {
                let Some(mut request) = engine.last_request(&chat) else {
                    tracing::warn!(chat = %chat, "orphaned steer lost: no run config to re-dispatch");
                    break;
                };
                request.prompt = steer.prompt.clone();
                request.resume = None;
                request.attachments = Vec::new();
                tracing::info!(chat = %chat, "re-dispatching steer orphaned by a dying run");
                if let Err(err) = engine
                    .dispatch_augmented(
                        &chat,
                        harness_id,
                        request,
                        steer.agent_prompt.clone(),
                        Some(steer.message_id.clone()),
                    )
                    .await
                {
                    tracing::warn!(chat = %chat, error = %err, "orphaned steer re-dispatch failed");
                    engine
                        .inner
                        .set_status(&chat, SessionStatus::Errored, false);
                    break;
                }
            }
        });
    }
}

#[cfg(test)]
mod agent_prompt_tests {
    use super::*;
    use cypher_proto::agent_prompt::{AgentQuote, PromptComment, QuoteAlign, comments_block, wrap};

    /// Only Pi resolves a translated quote's alignment input; every other
    /// agent must never receive the displayed translation it holds.
    #[test]
    fn only_pi_receives_alignment_input() {
        let origin = AgentQuote::Align(QuoteAlign {
            passage: "Original passage.".into(),
            before: String::new(),
            selected: "译文".into(),
            after: String::new(),
        });
        let prompt = wrap(
            &[comments_block(&[PromptComment::new(
                "译文",
                Some(&origin),
                "why",
            )])],
            "go",
        );
        assert_eq!(
            agent_prompt_for(HarnessId::Pi, Some(prompt.clone())).as_deref(),
            Some(prompt.as_str())
        );
        let sent = agent_prompt_for(HarnessId::Mock, Some(prompt.clone())).unwrap();
        assert!(
            !sent.contains("译文") && sent.contains("Original passage."),
            "{sent}"
        );
        assert_eq!(agent_prompt_for(HarnessId::Mock, None), None);
    }
}
