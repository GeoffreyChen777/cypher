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
                // bounded them (docs/design/chat2-sync.md A1). The transient
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

/// The assistant entry being written: the parts folded so far, the models
/// that answered them (one per completed assistant message), and the doc
/// writer once the entry exists. Reset at every steer/park boundary.
struct Segment<'a> {
    doc: &'a SessionDoc,
    device_id: &'a str,
    folded: Vec<MessagePart>,
    models: Vec<AnsweredModel>,
    entry_id: String,
    started: i64,
    writer: Option<SegmentWriter<'a>>,
    /// Folded parts not yet committed; `flush_at` is their coalesced commit.
    dirty: bool,
    flush_at: tokio::time::Instant,
}

impl<'a> Segment<'a> {
    fn new(doc: &'a SessionDoc, device_id: &'a str) -> Self {
        Self {
            doc,
            device_id,
            folded: Vec::new(),
            models: Vec::new(),
            entry_id: new_id(),
            started: now_ms(),
            writer: None,
            dirty: false,
            flush_at: tokio::time::Instant::now(),
        }
    }

    /// Start the next entry with an empty fold.
    fn reset(&mut self, entry_id: String) {
        self.folded.clear();
        self.models.clear();
        self.dirty = false;
        self.entry_id = entry_id;
        self.started = now_ms();
    }

    /// Whether anything of this entry reached the fold or the doc.
    fn streamed(&self) -> bool {
        !self.folded.is_empty() || self.writer.is_some()
    }

    /// Commit the fold so far, opening the entry on first content.
    fn sync(&mut self) -> Result<(), DocError> {
        if self.folded.is_empty() {
            return Ok(());
        }
        let rendered = render_parts(&self.folded);
        if self.writer.is_none() {
            self.writer = Some(SegmentWriter::begin(
                self.doc,
                &self.entry_id,
                self.device_id,
                self.started,
            )?);
        }
        if let Some(w) = self.writer.as_mut() {
            w.set_models(&self.models)?;
            w.sync(&rendered)?;
        }
        Ok(())
    }

    /// Finalize the entry with `status`; an entry that never streamed is not written.
    fn finish(&mut self, status: MessageStatus) -> Result<(), DocError> {
        let writer = self.writer.take();
        let rendered = render_parts(&self.folded);
        let mut writer = match writer {
            Some(w) => w,
            None if !self.folded.is_empty() => {
                SegmentWriter::begin(self.doc, &self.entry_id, self.device_id, self.started)?
            }
            None => return Ok(()),
        };
        writer.set_models(&self.models)?;
        writer.finish(&rendered, status)
    }

    /// Arm the coalesced STREAM_COMMIT_MS commit for newly folded parts.
    fn schedule_flush(&mut self) {
        if !self.folded.is_empty() && !self.dirty {
            self.dirty = true;
            self.flush_at =
                tokio::time::Instant::now() + std::time::Duration::from_millis(STREAM_COMMIT_MS);
        }
    }
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

/// What one run task knows about itself for the length of the run.
struct RunScope<'r> {
    inner: &'r Arc<Inner>,
    chat_id: &'r str,
    run_id: &'r str,
    harness_id: HarnessId,
    plugin_epoch: u64,
    steerable: bool,
    user_prompt: &'r str,
    run_cwd: &'r str,
}

/// How the loop continues after a terminal Done.
enum DoneFlow {
    /// The turn parked; the persistent session waits for the next one.
    Park,
    /// The run ends with this status.
    End(SessionStatus),
}

/// What one pass of the event select produced.
enum Step {
    /// A timer or control arm did its work; select again.
    Continue,
    /// An event to process.
    Event(AgentEvent),
    /// The run ends with this status, without a final event.
    End(SessionStatus),
}

/// What processing one event decided.
enum Flow {
    Continue,
    End(SessionStatus),
    /// The run was retired and re-dispatched as its startup retry; skip the
    /// end-of-run bookkeeping.
    Retried,
}

/// PERSISTENT SESSION: a completed turn on a steerable harness parks instead
/// of ending the run — the child and its steering mailbox stay warm, and the
/// next user message (dispatch routes into a live run) starts the next turn
/// with zero respawn/resume latency. A session parked this long with nobody
/// coming back is reaped.
const SESSION_IDLE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// The run's event sources, polled by [`RunLoop::next`].
struct RunIo {
    stream: futures::stream::BoxStream<'static, Result<AgentEvent, cypher_harness::HarnessError>>,
    engine_rx: mpsc::UnboundedReceiver<AgentEvent>,
    cancel_rx: watch::Receiver<bool>,
    /// Liveness heartbeat: this loop RUNNING is proof the harness stream is
    /// open, so freshness must not depend on events arriving. Silent stretches
    /// are normal and UNBOUNDED — a long tool call, redacted thinking, an
    /// agent waiting on an external process, a question parked for an hour —
    /// and each would otherwise starve the UI's 45s staleness gate (working
    /// strip / AwaitingInput dot vanishing mid-run). There is no stall timeout
    /// by design (agents may legitimately be quiet for >10min): a live child
    /// means Working, dying paths each carry their own error, and engine death
    /// stops these ticks so the gate still catches real crashes.
    /// `touch_session` throttles at 10s.
    live_heartbeat: tokio::time::Interval,
}

/// The state of one run's event loop: the select over its event sources
/// ([`Self::next`]) and the processing of each event ([`Self::handle`]).
struct RunLoop<'r> {
    scope: RunScope<'r>,
    doc: &'r SessionDoc,
    seg: Segment<'r>,
    /// Every tool id this run has folded, across segment resets. Adapters
    /// re-emit shape-bearing tool updates (title/input refreshes, long-running
    /// completions) as full ToolCall events; once the fold has reset at a
    /// steer/park boundary those ids are gone from the fold, and folding the
    /// echo would mint an orphan chip mid-text in the NEXT segment — the
    /// mid-word transcript splits.
    seen_tools: std::collections::HashSet<String>,
    /// Set when the engine interrupts the run: the harness gets this long to
    /// end its own stream (its token was cancelled); past it, a terminal Done
    /// is synthesized.
    interrupt_deadline: Option<tokio::time::Instant>,
    interrupted: bool,
    saw_session_started: bool,
    /// `Some(when)` = parked idle since then (see [`SESSION_IDLE`]).
    idle_since: Option<tokio::time::Instant>,
    /// TURN-QUIESCE WATCHDOG: a harness that loses a turn's Done — the agent
    /// finished but its turn-end never arrives — strands Working forever: the
    /// live heartbeat keeps the row fresh, and there is no per-turn timeout by
    /// design. This is NOT that stall timeout: it never ends the run or errors
    /// anything. When the stream has been silent past the window AND the fold
    /// shows completed output with nothing in flight (no unresolved tool, no
    /// open question), the turn parks exactly like a Done would — segment
    /// finalized Complete, status Idle, child and mailbox warm. A false trip
    /// (the agent was quietly waiting on something invisible) costs a status
    /// dip: the parked-resume path re-arms Working the moment output flows
    /// again, and nothing is lost. SELF-CONTINUED turns (see
    /// [`QuiesceWindows::self_turn`]) use a much shorter window.
    quiesce: QuiesceWindows,
    last_stream_activity: tokio::time::Instant,
    self_continued_turn: bool,
    /// Parked by the WATCHDOG with a prompt-owned turn's Done still
    /// outstanding. The watchdog cannot tell a lost Done from a provider
    /// that is simply slow (a long high-reasoning step), so this park is
    /// provisional: any later sign of life reopens that same turn on the
    /// normal window ([`Self::parked_gate`]), and a run that ends before the
    /// turn ever settles journals it interrupted ([`Self::finish`]).
    quiesced_turn: bool,
    /// The journal holds events of a turn it has not closed with a Done yet.
    /// A run must never end that way: subagent parents wait on the child's
    /// terminal event, and boot recovery reads an open journal as a crash.
    journal_open: bool,
    /// The last translation frame folded, so the keepalive repeats that keep
    /// a slow translation's stream alive are not each journaled in full.
    last_translation: Option<String>,
    /// Kept whole for the startup-crash retry (same user entry; dispatch
    /// re-injects the stored resume id), taken when the retry fires. Carries
    /// the VISIBLE prompt — the retry re-derives the effective override from
    /// `resume_state.agent_prompt`.
    retry_request: Option<RunRequest>,
    resume_state: Option<RunResumeState>,
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
    engine_rx: mpsc::UnboundedReceiver<AgentEvent>,
    cancel_rx: watch::Receiver<bool>,
    resume_state: RunResumeState,
) {
    let device_id = inner.device_id.clone();
    let plugin_epoch = inner.plugin_epoch.load(Ordering::SeqCst);
    // Captured for post-run auto-titling (the request moves into the harness).
    let harness_id = harness.id();
    let user_prompt = request.prompt.clone();
    let run_cwd = request.cwd.clone();
    let retry_request = RunRequest {
        resume: None,
        worktree: None,
        ..request.clone()
    };
    // The harness receives the EFFECTIVE prompt (visible unless an override
    // rides the command). `request` itself stays the visible truth.
    let effective = resume_state
        .agent_prompt
        .clone()
        .unwrap_or_else(|| request.prompt.clone());
    let mut harness_request = request.clone();
    harness_request.prompt = effective;
    let stream = match harness.run(harness_request, controls).await {
        Ok(stream) => stream,
        Err(err) => {
            start_failed(&inner, &chat_id, &run_id, &err);
            return;
        }
    };

    if let Some(host) = inner.doc_host() {
        host.preview_run(&chat_id, &run_id);
    }
    let mut live_heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
    live_heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let scope = RunScope {
        inner: &inner,
        chat_id: &chat_id,
        run_id: &run_id,
        harness_id,
        plugin_epoch,
        steerable: harness.supports_steering(),
        user_prompt: &user_prompt,
        run_cwd: &run_cwd,
    };
    let mut io = RunIo {
        stream,
        engine_rx,
        cancel_rx,
        live_heartbeat,
    };
    let mut run = RunLoop::new(scope, &doc, &device_id, retry_request, resume_state);

    let final_status = loop {
        let event = match run.next(&mut io).await {
            Step::Continue => continue,
            Step::Event(event) => event,
            Step::End(status) => break status,
        };
        match run.handle(event) {
            Flow::Continue => {}
            Flow::End(status) => break status,
            Flow::Retried => return,
        }
    };
    run.finish(final_status);
}

/// The harness never started: publish the failure and retire the run.
fn start_failed(
    inner: &Arc<Inner>,
    chat_id: &str,
    run_id: &str,
    err: &cypher_harness::HarnessError,
) {
    let message = err.to_string();
    inner.publish(
        chat_id,
        &AgentEvent::Error {
            message: message.clone(),
        },
    );
    inner.publish(
        chat_id,
        &AgentEvent::Done {
            status: DoneStatus::Errored,
            result: None,
            error: Some(message),
            session_id: None,
        },
    );
    // The owner never came up: nothing will ever settle its subagents.
    inner.fail_orphaned_subagents(chat_id, "Subagent owner failed to start");
    inner.remove_run(chat_id, run_id);
    inner.set_status(chat_id, SessionStatus::Errored, false);
}

impl<'r> RunLoop<'r> {
    fn new(
        scope: RunScope<'r>,
        doc: &'r SessionDoc,
        device_id: &'r str,
        retry_request: RunRequest,
        resume_state: RunResumeState,
    ) -> Self {
        let quiesce = scope.inner.quiesce_windows();
        Self {
            scope,
            doc,
            seg: Segment::new(doc, device_id),
            seen_tools: std::collections::HashSet::new(),
            interrupt_deadline: None,
            interrupted: false,
            saw_session_started: false,
            idle_since: None,
            quiesce,
            last_stream_activity: tokio::time::Instant::now(),
            self_continued_turn: false,
            quiesced_turn: false,
            journal_open: false,
            last_translation: None,
            retry_request: Some(retry_request),
            resume_state: Some(resume_state),
        }
    }

    /// Wait for the next thing the run must react to. Arm order is the
    /// priority order (`biased`): interrupts first, the event sources next,
    /// timers last.
    async fn next(&mut self, io: &mut RunIo) -> Step {
        tokio::select! {
            biased;
            changed = io.cancel_rx.changed(), if !self.interrupted => {
                let _ = changed;
                self.on_cancel();
                Step::Continue
            }
            _ = tokio::time::sleep_until(
                self.interrupt_deadline.unwrap_or_else(tokio::time::Instant::now)
            ), if self.interrupt_deadline.is_some() => Step::Event(AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: None,
                session_id: None,
            }),
            _ = io.live_heartbeat.tick() => {
                self.scope.inner.touch_session(self.scope.chat_id);
                Step::Continue
            }
            // Idle reaper: a parked persistent session nobody returned to in
            // 30 minutes releases its child. The turn was finalized at Done,
            // so this end is clean — no aborted stamp.
            _ = tokio::time::sleep_until(
                self.idle_since.map(|at| at + SESSION_IDLE).unwrap_or_else(tokio::time::Instant::now)
            ), if self.idle_since.is_some() => Step::End(self.reap_idle()),
            Some(event) = io.engine_rx.recv() => Step::Event(event),
            next = io.stream.next() => self.on_stream_item(next),
            _ = tokio::time::sleep_until(self.seg.flush_at), if self.seg.dirty => {
                self.on_flush_tick();
                Step::Continue
            }
            _ = tokio::time::sleep_until(self.quiesce_deadline()), if self.quiesce_armed() => {
                self.on_quiesce();
                Step::Continue
            }
        }
    }

    fn on_cancel(&mut self) {
        self.interrupted = true;
        self.interrupt_deadline =
            Some(tokio::time::Instant::now() + std::time::Duration::from_secs(3));
    }

    fn reap_idle(&self) -> SessionStatus {
        let (inner, chat_id) = (self.scope.inner, self.scope.chat_id);
        tracing::info!(chat = %chat_id, "reaping idle persistent session");
        if let Some(token) = lock(&inner.runs)
            .get(chat_id)
            .filter(|h| h.run_id == self.scope.run_id)
            .map(|h| h.interrupt_token.clone())
        {
            token.cancel();
        }
        SessionStatus::Idle
    }

    fn on_stream_item(
        &self,
        next: Option<Result<AgentEvent, cypher_harness::HarnessError>>,
    ) -> Step {
        let chat_id = self.scope.chat_id;
        match next {
            Some(Ok(event)) => Step::Event(event),
            // A stream error while PARKED is a post-turn child death —
            // the turn was already finalized, so the run ends clean
            // instead of stamping a completed session Errored.
            Some(Err(err)) if self.idle_since.is_some() => {
                tracing::warn!(chat = %chat_id, error = %err, "parked session child died; ending clean");
                Step::End(SessionStatus::Idle)
            }
            Some(Err(err)) => Step::Event(AgentEvent::Done {
                status: DoneStatus::Errored,
                result: None,
                error: Some(err.to_string()),
                session_id: None,
            }),
            None if self.interrupted => Step::Event(AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: None,
                session_id: None,
            }),
            // Stream end while PARKED idle: a per-turn adapter closing
            // after its final Done — a clean end, not a crash (the turn
            // was already finalized). Persistent adapters keep the
            // stream open and never hit this.
            None if self.idle_since.is_some() => Step::End(SessionStatus::Idle),
            None => Step::Event(AgentEvent::Done {
                status: DoneStatus::Errored,
                result: None,
                error: Some("harness stream ended without Done".into()),
                session_id: None,
            }),
        }
    }

    /// Coalesced STREAM_COMMIT_MS tick: one doc commit per window.
    fn on_flush_tick(&mut self) {
        if let Err(err) = self.seg.sync() {
            tracing::warn!(chat = %self.scope.chat_id, error = %err, "segment sync failed");
        }
        self.seg.dirty = false;
    }

    /// The silence window the watchdog applies now: the turn window, or the
    /// shorter self-turn window for a self-continued turn.
    fn quiesce_window(&self) -> std::time::Duration {
        let window = self.quiesce.turn.unwrap_or_default();
        match self.quiesce.self_turn {
            Some(short) if self.self_continued_turn => window.min(short),
            _ => window,
        }
    }

    /// The watchdog's deadline: [`Self::quiesce_window`] after the last
    /// stream activity.
    fn quiesce_deadline(&self) -> tokio::time::Instant {
        self.last_stream_activity + self.quiesce_window()
    }

    /// The watchdog is armed only when the fold says nothing is in flight: an
    /// unresolved tool part is a command still running (legitimately silent
    /// for minutes), an unresolved input part is a question awaiting the user.
    /// The live-plan chip is exempt from the tool check: it is a singleton
    /// that never resolves. An EMPTY fold still arms — a Steered boundary that
    /// no output ever follows is one of the wedge shapes — it just parks
    /// without writing a segment (an empty finalize would leave a stub entry).
    fn quiesce_armed(&self) -> bool {
        self.quiesce.turn.is_some()
            && self.idle_since.is_none()
            && !self.interrupted
            && self.scope.steerable
            && !self.seg.folded.iter().any(|p| match p {
                MessagePart::Tool {
                    id,
                    resolved: false,
                    ..
                } => id != cypher_proto::LIVE_PLAN_TOOL_ID,
                MessagePart::Input {
                    resolved: false, ..
                } => true,
                _ => false,
            })
    }

    fn on_quiesce(&mut self) {
        let (inner, chat_id) = (self.scope.inner, self.scope.chat_id);
        // The window actually applied and the silence actually measured: the
        // configured turn window alone misreported a self-turn park as a
        // full-window one.
        tracing::warn!(
            chat = %chat_id,
            window_ms = self.quiesce_window().as_millis() as u64,
            silent_ms = self.last_stream_activity.elapsed().as_millis() as u64,
            self_continued = self.self_continued_turn,
            "turn quiesced: stream silent after completed output with no \
             turn-end; parking (suspected missing harness Done)"
        );
        // A prompt-owned turn still owes its Done; a self-continued one
        // never gets one, so for it this park is the settle.
        let outstanding = !self.self_continued_turn;
        if self.seg.streamed() {
            if let Err(err) = self.seg.finish(MessageStatus::Complete) {
                tracing::warn!(chat = %chat_id, error = %err, "quiesce segment finish failed");
            }
            inner.note_message(chat_id, &folded_text(&self.seg.folded));
            if let Some(host) = inner.doc_host() {
                host.flush_chat_sync(chat_id);
            }
        }
        self.park();
        self.quiesced_turn = outstanding;
    }

    /// Process one event: projections, filters, the startup retry, steer
    /// boundaries, then publish + fold, settling a terminal Done.
    fn handle(&mut self, event: AgentEvent) -> Flow {
        let (inner, chat_id) = (self.scope.inner, self.scope.chat_id);
        if apply_projection(&self.scope, self.doc, &event) {
            return Flow::Continue;
        }

        // Any stream activity proves the run is alive — keep the session's
        // freshness inside the UI's 45s staleness window (throttled), and
        // push the quiesce watchdog's window out.
        inner.touch_session(chat_id);
        self.last_stream_activity = tokio::time::Instant::now();
        if self.should_drop(&event) {
            return Flow::Continue;
        }
        if self.try_startup_retry(&event) {
            return Flow::Retried;
        }
        if let AgentEvent::Steered {
            next_assistant_message_id,
            ..
        } = &event
        {
            self.on_steered(&event, next_assistant_message_id.clone());
            return Flow::Continue;
        }
        self.record(&event);

        inner.publish(chat_id, &event);
        self.journal_open = !matches!(event, AgentEvent::Done { .. });

        // A mid-run SessionStarted re-emission (background re-invocations)
        // must not wipe the segment being written.
        let skip_fold =
            matches!(&event, AgentEvent::SessionStarted { .. }) && !self.seg.folded.is_empty();
        if !skip_fold {
            // Full tool output stays in the host's local run journal; the doc
            // keeps the fold's bounded summary and diff stats.
            fold_event_into_parts(&mut self.seg.folded, &event);
        }

        if let AgentEvent::Done { status, .. } = &event {
            return match self.on_done(*status) {
                DoneFlow::Park => Flow::Continue,
                DoneFlow::End(status) => Flow::End(status),
            };
        }

        self.seg.schedule_flush();
        Flow::Continue
    }

    /// Events that are dropped after the activity touch, in this order.
    fn should_drop(&mut self, event: &AgentEvent) -> bool {
        let (inner, chat_id) = (self.scope.inner, self.scope.chat_id);
        // The engine's input bridge is the sole authority on input requests:
        // it mints the id and parks the resolver BEFORE emitting the event,
        // so a legitimate id is always pending here. A harness emitting its
        // own copy (a different id no resolver knows) would fold an
        // unanswerable twin chip into the doc — and answering the twin would
        // never resume the run. Dropped BEFORE the parked gate below: letting
        // it un-park the session and then dropping it would disarm the idle
        // reaper with no turn running (a leaked child no reap ever ends).
        if let AgentEvent::InputRequested { request_id, .. } = event {
            let pending = lock(&inner.runs)
                .get(chat_id)
                .map(|h| h.pending_inputs.clone());
            let known = pending.is_some_and(|p| lock(&p).contains_key(request_id));
            if !known {
                tracing::warn!(
                    chat = %chat_id,
                    request = %request_id,
                    "dropping harness-emitted InputRequested (unknown id; \
                     the engine input bridge owns this lifecycle)"
                );
                return true;
            }
        }
        if let Gate::Drop = self.parked_gate(event) {
            return true;
        }
        // Empty reasoning deltas are PURE heartbeats: redacted thinking and
        // tool-input-generation windows stream them with no text. They fold
        // to nothing, so journaling/publishing them is only noise (hundreds
        // per long turn observed) — the touch above already did their job.
        if matches!(event, AgentEvent::ReasoningDelta { text } if text.is_empty()) {
            return true;
        }
        // Same reasoning for a repeated translation frame. A frame carries the
        // WHOLE rendering, so one identical to the frame right before it is a
        // keepalive: the translation extension re-states the current text every
        // few seconds so a slow translation cannot go quiet and be parked by
        // the watchdog. The touch already did that job, and folding it would
        // assign the text that is already there — journaling and broadcasting
        // a full copy of the answer per tick would be pure noise. Only an
        // UNBROKEN repeat is dropped, so a later turn whose answer happens to
        // translate identically still renders.
        match event {
            AgentEvent::Translation { text }
                if self.last_translation.as_deref() == Some(text.as_str()) =>
            {
                return true;
            }
            AgentEvent::Translation { text } => self.last_translation = Some(text.clone()),
            _ => self.last_translation = None,
        }
        stale_tool_echo(event, &self.seg.folded, &mut self.seen_tools)
    }

    /// Startup-crash retry: a run that dies before ever starting (errored
    /// Done, no SessionStarted, nothing streamed) means the AGENT CHILD failed
    /// to come up — not that the injected resume id was bad (the harness falls
    /// back to a fresh session on a stale id itself). The id stays; retry ONCE
    /// against the same user entry, resume and all, in case the crash was
    /// transient. A helper that is down hard fails the retry too and surfaces
    /// its crash text (the harness appends exit status + stderr).
    fn try_startup_retry(&mut self, event: &AgentEvent) -> bool {
        if self
            .resume_state
            .as_ref()
            .is_some_and(|state| state.resume_injected && !state.startup_retry)
            && !self.saw_session_started
            && self.seg.folded.is_empty()
            && !self.interrupted
            && matches!(
                event,
                AgentEvent::Done {
                    status: DoneStatus::Errored,
                    ..
                }
            )
            && let Some(retry) = self.retry_request.take()
            && let Some(resume_state) = self.resume_state.take()
        {
            spawn_startup_retry(&self.scope, retry, resume_state);
            return true;
        }
        false
    }

    /// A steer boundary splits the assistant entry exactly where the fold resets.
    fn on_steered(&mut self, event: &AgentEvent, next_assistant_message_id: Option<String>) {
        let (inner, chat_id) = (self.scope.inner, self.scope.chat_id);
        inner.publish(chat_id, event);
        self.journal_open = true;
        // A steer boundary means a real prompt owns the turn again — its
        // Done will come; the short self-continued window stands down.
        self.self_continued_turn = false;
        if let Err(err) = self.seg.finish(MessageStatus::Complete) {
            tracing::warn!(chat = %chat_id, error = %err, "segment finish failed");
        }
        if let Some(host) = inner.doc_host() {
            host.flush_chat_sync(chat_id);
        }
        inner.note_message(chat_id, &folded_text(&self.seg.folded));
        self.seg
            .reset(next_assistant_message_id.unwrap_or_else(new_id));
        // The elapsed timer is per user message, not per child process: a
        // steer boundary restarts it (matches the parked-resume path and
        // the composer's optimistic overlay, which already reads 0:00).
        inner.set_status(chat_id, SessionStatus::Working, true);
        // The boundary confirms delivery of the oldest accepted steer —
        // retire its at-least-once ledger entry.
        if let Some(h) = lock(&inner.runs)
            .get(chat_id)
            .filter(|h| h.run_id == self.scope.run_id)
        {
            lock(&h.routed_steers).pop_front();
        }
    }

    /// Per-event bookkeeping before the event is published and folded.
    fn record(&mut self, event: &AgentEvent) {
        let (inner, chat_id) = (self.scope.inner, self.scope.chat_id);
        match event {
            AgentEvent::SessionStarted {
                session_id, cwd, ..
            } => {
                self.saw_session_started = true;
                // The event's own cwd (where the harness actually created the
                // session) scopes the stored id, not the request's.
                inner.remember_harness_session(chat_id, session_id, cwd);
            }
            AgentEvent::Done {
                session_id: Some(session_id),
                ..
            } => {
                inner.remember_harness_session(chat_id, session_id, self.scope.run_cwd);
            }
            AgentEvent::InputRequested { .. } => {
                // Known-id guaranteed: the unknown-id twin was dropped in
                // `should_drop`, before the parked gate.
                inner.set_status(chat_id, SessionStatus::AwaitingInput, false);
            }
            AgentEvent::InputResolved { .. } => {
                inner.set_status(chat_id, SessionStatus::Working, false);
            }
            // Lands on the segment with its next sync or finish (a steer
            // splits only at the NEXT message's start, so this message's
            // model labels the segment it wrote).
            AgentEvent::AssistantMessageCompleted {
                model: Some(model), ..
            } if !self.seg.models.contains(model) => {
                self.seg.models.push(model.clone());
            }
            _ => {}
        }
    }

    /// Park the turn: the persistent session idles with child and mailbox warm,
    /// and the next entry starts empty.
    fn park(&mut self) {
        self.seg.reset(new_id());
        self.idle_since = Some(tokio::time::Instant::now());
        self.self_continued_turn = false;
        self.quiesced_turn = false;
        self.scope
            .inner
            .set_status(self.scope.chat_id, SessionStatus::Idle, false);
    }

    /// Reopen a parked session for the event at hand: a fresh entry (the
    /// park cleared the fold; this event is the new segment's first part)
    /// and Working. `self_continued` picks the watchdog window the turn runs
    /// on from here.
    fn reopen(&mut self, self_continued: bool) {
        self.idle_since = None;
        self.quiesced_turn = false;
        self.self_continued_turn = self_continued;
        self.seg.entry_id = new_id();
        self.seg.started = now_ms();
        self.scope
            .inner
            .set_status(self.scope.chat_id, SessionStatus::Working, true);
    }

    /// PARKED: a steer boundary, a terminal Done, or SELF-CONTINUED OUTPUT
    /// re-opens the session; everything else stays gated. A parked child can
    /// keep streaming after its turn completed, and those frames split two ways:
    ///
    /// - Post-turn NOISE — late tool updates for commands folded in a prior
    ///   segment, command refreshes, reasoning heartbeats. Treating those as
    ///   "the next turn" re-armed Working with no Done ever coming (the
    ///   eternally-running session bug) and folded orphan parts into a phantom
    ///   segment. Still dropped.
    /// - SELF-CONTINUED WORK — an agent re-invoking itself when a background
    ///   task finishes (turns no prompt started) streams real output for them.
    ///   Dropping those LOST transcript content. Fresh text or a genuinely new
    ///   tool call resumes the session: new segment, Working, and the turn
    ///   settles again via Done — or via the quiesce watchdog, which is what
    ///   makes this resume safe where the naive version was not.
    ///
    /// The RESUME_GATE separates the two by arrival time: a finished turn's tail
    /// flush lands within milliseconds of its Done, while a self-continued turn
    /// starts a whole new agent round trip (seconds at minimum). Inside the gate
    /// everything non-boundary stays inert.
    ///
    /// A QUIESCED turn is different: no Done ever closed it, so nothing that
    /// follows can be its tail. Any sign of life — output, a reasoning
    /// heartbeat, a question — means the harness was slow, not lost (a long
    /// high-reasoning step), and reopens that same prompt-owned turn on the
    /// normal window. Gating it instead re-parked the turn on every quiet
    /// step (as "self-continued", on the short window) and auto-declined its
    /// questions.
    fn parked_gate(&mut self, event: &AgentEvent) -> Gate {
        const RESUME_GATE: std::time::Duration = std::time::Duration::from_secs(1);
        let (inner, chat_id) = (self.scope.inner, self.scope.chat_id);
        if self.idle_since.is_none() {
            return Gate::Pass;
        }
        if self.quiesced_turn
            && !matches!(
                event,
                AgentEvent::Done { .. }
                    | AgentEvent::Steered { .. }
                    | AgentEvent::InputResolved { .. }
            )
        {
            tracing::info!(
                chat = %chat_id,
                "quiesced turn resumed: the harness is still working on it"
            );
            self.reopen(false);
            return Gate::Pass;
        }
        let self_continued = self
            .idle_since
            .is_some_and(|parked_at| parked_at.elapsed() >= RESUME_GATE)
            && (matches!(
                event,
                AgentEvent::TextDelta { text } if !text.is_empty()
            ) || matches!(
                event,
                AgentEvent::ToolCall { id, .. }
                    if id == cypher_proto::LIVE_PLAN_TOOL_ID || !self.seen_tools.contains(id)
            ));
        if self_continued {
            tracing::info!(
                chat = %chat_id,
                "parked session resumed by self-continued agent output"
            );
            self.reopen(true);
            return Gate::Pass;
        }
        match event {
            AgentEvent::Steered { .. } => {
                self.idle_since = None;
                inner.set_status(chat_id, SessionStatus::Working, true);
                Gate::Pass
            }
            AgentEvent::Done { .. } => {
                self.idle_since = None;
                Gate::Pass
            }
            // A question with NO turn behind it (post-turn permission
            // noise): answer it empty. But a routed send already flipped
            // Working — `/subagent-config` and friends ARE the turn,
            // and auto-declining them hangs the extension handler.
            AgentEvent::InputRequested { request_id, .. } => {
                let live = lock(&inner.statuses).get(chat_id).is_some_and(|s| {
                    matches!(
                        s.status,
                        SessionStatus::Working | SessionStatus::AwaitingInput
                    )
                });
                if live {
                    self.idle_since = None;
                    Gate::Pass
                } else {
                    let resolver = lock(&inner.runs)
                        .get(chat_id)
                        .and_then(|h| lock(&h.pending_inputs).remove(request_id));
                    if let Some(tx) = resolver {
                        let _ = tx.send(Vec::new());
                    }
                    tracing::debug!(chat = %chat_id, "parked session: post-turn input request auto-declined");
                    Gate::Drop
                }
            }
            // A stale answer settling after its turn already closed:
            // nothing is running — stay parked.
            AgentEvent::InputResolved { .. } => Gate::Drop,
            _ => Gate::Drop,
        }
    }

    /// Settle a terminal Done: drain pending inputs, finalize the segment, title
    /// the chat, then park the persistent session or end the run.
    fn on_done(&mut self, status: DoneStatus) -> DoneFlow {
        let scope = &self.scope;
        let (inner, chat_id) = (scope.inner, scope.chat_id);
        // A question still pending at turn end can never be legitimately
        // answered (its turn is over): drain the resolvers NOW, or a late
        // `respond_input` finds one, emits InputResolved, and un-parks
        // the session into Working with no turn behind it — stranded
        // Working, timer forever, reaper disarmed. Empty answers unblock
        // the harness-side bridge like an interrupt does.
        let pending = lock(&inner.runs)
            .get(chat_id)
            .filter(|h| h.run_id == scope.run_id)
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
        for part in self.seg.folded.iter_mut() {
            if let MessagePart::Input { resolved, .. } = part {
                *resolved = true;
            }
        }
        // A Done landing on a PARKED session with nothing streamed (the
        // idle reaper's or an interrupt's own teardown) has no entry to
        // finalize — writing one would leave an empty aborted stub.
        if self.seg.streamed() {
            if let Err(err) = self.seg.finish(message_status) {
                tracing::warn!(chat = %chat_id, error = %err, "final segment finish failed");
            }
            inner.note_message(chat_id, &folded_text(&self.seg.folded));
            if let Some(host) = inner.doc_host() {
                host.flush_chat_sync(chat_id);
            }
        }
        if status == DoneStatus::Completed {
            // A cleanly completed turn resets the auto-resume revival
            // budget: only consecutive crash-revive-crash cycles spend it.
            inner.journal.clear_resume_attempts(chat_id);
        }
        // Exchange completed on an untitled chat → name it (fire-and-forget;
        // interrupted/errored turns never trigger naming).
        if status == DoneStatus::Completed
            && !inner.is_ephemeral(chat_id)
            && let Some(titles) = inner.titles.get()
        {
            titles.maybe_generate(chat_id, scope.harness_id, scope.user_prompt, scope.run_cwd);
        }
        // PERSISTENT SESSION: a cleanly completed turn on a steerable
        // harness PARKS instead of ending — child + mailbox stay warm for
        // the next routed dispatch; per-turn state resets for it.
        // A Pi-package change mid-turn bumps `plugin_epoch`: skip the park
        // so the next send respawns against the new settings.json.
        if status == DoneStatus::Completed && scope.steerable && !self.interrupted {
            if inner.plugin_epoch.load(Ordering::SeqCst) != scope.plugin_epoch {
                tracing::info!(
                    chat = %chat_id,
                    "ending session so the next turn loads updated Pi packages"
                );
                return DoneFlow::End(SessionStatus::Idle);
            }
            // Resume-retry is strictly a first-turn concern.
            self.saw_session_started = true;
            self.park();
            return DoneFlow::Park;
        }
        DoneFlow::End(match status {
            DoneStatus::Errored => SessionStatus::Errored,
            _ => SessionStatus::Idle,
        })
    }

    /// End-of-run bookkeeping once the loop has settled on `final_status`.
    fn finish(self, final_status: SessionStatus) {
        let (inner, chat_id, run_id) = (self.scope.inner, self.scope.chat_id, self.scope.run_id);
        // The journal ends on its turn's terminal event. A run can still end
        // with a turn open — the watchdog parked it provisionally, and the
        // reaper, a child death or the stream's end retired the run before
        // the harness settled it. Settle it here, where it is truly over: a
        // subagent parent waiting on WatchAgentEvents gets its terminal
        // event, and boot recovery does not read the turn as a crash
        // mid-stream. The watchdog's own park publishes nothing — it may be
        // wrong, and a premature Done would settle a waiting parent early.
        if self.journal_open {
            let (status, error) = if self.quiesced_turn {
                (
                    DoneStatus::Interrupted,
                    Some("The agent stopped responding before finishing its turn".to_owned()),
                )
            } else {
                (DoneStatus::Completed, None)
            };
            inner.publish(
                chat_id,
                &AgentEvent::Done {
                    status,
                    result: None,
                    error,
                    session_id: None,
                },
            );
        }
        // Claim any accepted-but-unconfirmed steers BEFORE the handle goes away:
        // a routed send that raced this exit either finds its entry gone (we own
        // it — re-dispatched below) or reclaims it and starts a fresh run itself.
        let orphans: Vec<RoutedSteer> = lock(&inner.runs)
            .get(chat_id)
            .filter(|h| h.run_id == run_id)
            .map(|h| std::mem::take(&mut *lock(&h.routed_steers)).into())
            .unwrap_or_default();
        // The harness owner for this chat has ended (stream EOF/error, interrupt,
        // idle reaper, or a final Done on a non-parked harness). Any subagent run
        // still projected Running can never settle on its own — terminalize it.
        // This deliberately does NOT fire on a parked Done (steerable sessions
        // keep the stream open; a legal background subagent stays Running on it).
        let owner_reason = if self.interrupted {
            "Subagent owner interrupted"
        } else if final_status == SessionStatus::Errored {
            "Subagent owner ended in error"
        } else {
            "Subagent owner session ended"
        };
        inner.fail_orphaned_subagents(chat_id, owner_reason);
        inner.remove_run(chat_id, run_id);
        inner.set_status(chat_id, final_status, false);
        if !self.interrupted && !orphans.is_empty() {
            redispatch_orphans(&self.scope, orphans);
        }
    }
}

/// Live projections mirrored onto the chat's session row instead of being
/// treated as run activity. Returns true when `event` was consumed.
fn apply_projection(scope: &RunScope<'_>, doc: &SessionDoc, event: &AgentEvent) -> bool {
    let (inner, chat_id) = (scope.inner, scope.chat_id);
    match event {
        // SubagentStatus is a LIVE PROJECTION, not run activity: mirror it
        // onto the chat's session row and stop — no journal append, no doc
        // fold, no status transition, no session freshness touch, and no
        // parked-session resume or quiesce-watchdog push (a background
        // subagent finishing must never re-arm a parked session as Working;
        // `set_subagents`' own updated_at bump keeps the row fresh instead).
        AgentEvent::SubagentStatus { runs } => {
            inner.set_subagents(chat_id, runs.clone());
        }
        // The context gauge is the same kind of projection: mirrored onto
        // the local session row, never journaled, folded, or allowed to
        // resume a parked session (re-reported after compaction).
        AgentEvent::ContextUsage { used, size } => {
            inner.set_context_usage(
                chat_id,
                ContextUsage {
                    used: *used,
                    size: *size,
                },
            );
        }
        // Throughput too: a live reading for the working trailer, never
        // run activity (the extension's final clear lands after agent_end).
        AgentEvent::Throughput { throughput } => {
            inner.set_throughput(chat_id, *throughput);
        }
        // A prompt's translation belongs to the USER entry it replaced, which
        // was written before the run saw it — stamped there directly, never
        // folded into the assistant segment. Handled ahead of the parked gate
        // because the extension translates a prompt BEFORE the turn it opens
        // starts, while the session may still be parked from the last one.
        AgentEvent::InputTranslation { source, text } => {
            match doc.stamp_user_agent_text(source, text) {
                Ok(true) => {
                    if let Some(host) = inner.doc_host() {
                        host.flush_chat_sync(chat_id);
                    }
                }
                Ok(false) => {
                    tracing::debug!(chat = %chat_id, "prompt translation matched no recent user entry");
                }
                Err(err) => {
                    tracing::warn!(chat = %chat_id, error = %err, "prompt translation stamp failed");
                }
            }
        }
        _ => return false,
    }
    true
}

/// What the parked gate decided for one event.
enum Gate {
    /// Not parked, or the event re-opened the session: process it.
    Pass,
    /// Post-turn noise on a parked session: drop it.
    Drop,
}

/// Stale tool echoes: a ToolCall/ToolResult naming an id folded in a PRIOR
/// segment (the fold reset at a steer or park since) belongs to a chip that
/// already rendered in its own entry. Folding the echo would splice a phantom
/// chip into the middle of the current segment's streaming text (the mid-word
/// transcript splits). Returns true to drop it; same-segment refreshes still
/// land in place. Records each tool id the run folds.
fn stale_tool_echo(
    event: &AgentEvent,
    folded: &[MessagePart],
    seen_tools: &mut std::collections::HashSet<String>,
) -> bool {
    let in_segment = |folded: &[MessagePart], id: &str| {
        folded
            .iter()
            .any(|p| matches!(p, MessagePart::Tool { id: pid, .. } if pid == id))
    };
    match event {
        // The live plan chip is a deliberate singleton (every update
        // reuses `LIVE_PLAN_TOOL_ID` so the fold refreshes in place):
        // treating its reappearance after a park/steer reset as a stale
        // echo dropped the todo list for the rest of the run — from the
        // first boundary on, plans never rendered again.
        AgentEvent::ToolCall { id, .. } if id == cypher_proto::LIVE_PLAN_TOOL_ID => {}
        AgentEvent::ToolResult { id, .. } if id == cypher_proto::LIVE_PLAN_TOOL_ID => {}
        AgentEvent::ToolCall { id, .. } => {
            if !in_segment(folded, id) && seen_tools.contains(id) {
                return true;
            }
            seen_tools.insert(id.clone());
        }
        AgentEvent::ToolResult { id, .. } if !in_segment(folded, id) && seen_tools.contains(id) => {
            return true;
        }
        // `ToolProgress` deliberately falls through: it arrives with its
        // ToolCall in the SAME segment, so it is never a stale echo here
        // — and the doc fold is the single authority on whether to apply
        // it (id known + !resolved). A cross-segment tick reaches the
        // fold with no matching part and is a no-op there; a post-resolve
        // tick is ignored the same way. No exemption arm needed.
        _ => {}
    }
    false
}

/// Retire this run (nothing ever started under it) and dispatch the startup
/// retry against the same user entry.
fn spawn_startup_retry(scope: &RunScope<'_>, retry: RunRequest, resume_state: RunResumeState) {
    let (inner, chat_id) = (scope.inner, scope.chat_id);
    tracing::warn!(
        chat = %chat_id,
        "run died before session start; retrying once (resume kept)"
    );
    // This harness owner is gone (nothing ever started under it).
    inner.fail_orphaned_subagents(chat_id, "Subagent owner failed to start");
    inner.remove_run(chat_id, scope.run_id);
    let engine = SessionsEngine {
        inner: inner.clone(),
    };
    let chat = chat_id.to_string();
    let harness_id = scope.harness_id;
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
}

/// Re-dispatch steers a dying run accepted into its mailbox but never
/// confirmed with a Steered boundary (idle-reaper race, a mid-turn error
/// discarding queued boundary steers, a parked child death). Their user
/// entries are already in the transcript — a message that shows as sent must
/// never silently not run. Each becomes a fresh turn (write_user_message
/// dedupes by id; resume is engine-injected).
fn redispatch_orphans(scope: &RunScope<'_>, orphans: Vec<RoutedSteer>) {
    let engine = SessionsEngine {
        inner: scope.inner.clone(),
    };
    let chat = scope.chat_id.to_string();
    let harness_id = scope.harness_id;
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
