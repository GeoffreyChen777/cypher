//! The connection actor: dial, handshake, backfill, push/ack and reconnect, plus
//! the HTTPS pull fallback.

use super::*;

pub(super) struct Actor {
    pub(super) shared: Arc<Mutex<Shared>>,
    pub(super) sink: Arc<dyn ChatDocSink>,
    pub(super) fetcher: Arc<dyn CheckpointFetcher>,
    pub(super) device_id: String,
    pub(super) connector: Arc<dyn BinConnector>,
    pub(super) tuning: ChatTuning,
    pub(super) events: broadcast::Sender<ChatEvent>,
    pub(super) shutdown: watch::Receiver<bool>,
    pub(super) nudge_rx: mpsc::Receiver<()>,
    pub(super) probe_rx: mpsc::Receiver<()>,
    pub(super) redial_rx: mpsc::Receiver<()>,
    pub(super) flags: Arc<Flags>,
    /// False until the first backfill of THIS client instance completes.
    /// (Continuity is instance-scoped: a host that restores an older doc
    /// snapshot must construct a fresh `ChatClient` — C3 wiring contract.)
    /// The first
    /// backfill must NOT exclude own rows: after a restart the pending queue
    /// is gone, and a restored-backup/copied-device doc may be missing its
    /// own post-backup writes — they exist only on the server. Loro
    /// re-import of rows the doc does hold is a no-op, so redownloading own
    /// bytes once is pure safety. Same-process reconnects have queue
    /// continuity and skip them (the reconnect-after-offline-work path the
    /// spec optimizes).
    pub(super) resumed: bool,
    /// Once per client instance, refetch rows from the checkpoint (or zero)
    /// when a restored cursor may have advanced over parked Loro operations.
    pub(super) cursor_amnesty_done: std::sync::atomic::AtomicBool,
    /// Optional plain-HTTPS bootstrap and recovery path.
    pub(super) transport: Option<Arc<dyn ChatTransport>>,
    /// Prevent overlapping pull cycles from racing one another.
    pub(super) http_sync_busy: Arc<std::sync::atomic::AtomicBool>,
    pub(super) http_sync_again: Arc<std::sync::atomic::AtomicBool>,
    pub(super) http_sync_tx: mpsc::Sender<()>,
    pub(super) http_sync_rx: mpsc::Receiver<()>,
}

enum SessionEnd {
    Reconnect,
    Stop,
}

/// How a backoff wait ended.
enum Waited {
    Elapsed,
    /// System wake or a sibling dial succeeded: redial NOW on fresh backoff.
    Woke,
    /// A local write requested HTTPS sync while the socket was offline.
    Nudge,
    Shutdown,
}

impl Actor {
    pub(super) async fn run(mut self, ready: oneshot::Sender<Result<(), SyncError>>) {
        let mut ready = Some(ready);
        let mut backoff = BACKOFF_BASE;
        if self.transport.is_some() {
            // HTTPS is the bootstrap path: callers can render the local doc
            // immediately while both transports converge in the background.
            if let Some(ready) = ready.take() {
                let _ = ready.send(Ok(()));
            }
            self.spawn_http_sync();
        }
        // Suspend/resume and sibling-dial successes are EVENTS that end a
        // backoff wait immediately (see room.rs) — without them a recovered
        // network still waited out the full accumulated delay.
        let mut wake = crate::wake::subscribe();
        let mut online = crate::wake::subscribe_online();
        loop {
            if let Some(preview) = self.sink.preview() {
                let cursor = lock(&self.shared).cursor;
                preview.tick(cursor);
            }
            if *self.shutdown.borrow() {
                return;
            }
            let dial = self.connector.connect();
            tokio::pin!(dial);
            let deadline = tokio::time::sleep(CONNECT_TIMEOUT);
            tokio::pin!(deadline);
            let dial = loop {
                tokio::select! {
                    result = &mut dial => break Some(result),
                    _ = self.nudge_rx.recv() => self.spawn_http_sync(),
                    _ = self.http_sync_rx.recv() => self.spawn_http_sync(),
                    _ = &mut deadline => break None,
                    _ = self.shutdown.changed() => {
                        if *self.shutdown.borrow() {
                            return;
                        }
                    }
                }
            };
            let pipe = match dial {
                Some(Ok(pipe)) => pipe,
                Some(Err(err)) => {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(Err(err));
                        return; // first join failed: caller owns the retry
                    }
                    tracing::warn!(error = %err, "chat2 dial failed; backing off");
                    self.spawn_http_sync();
                    match self.wait_backoff(&mut wake, &mut online, backoff).await {
                        Waited::Shutdown => return,
                        Waited::Nudge => self.spawn_http_sync(),
                        Waited::Woke => backoff = BACKOFF_BASE,
                        Waited::Elapsed => backoff = (backoff * 2).min(BACKOFF_CAP),
                    }
                    continue;
                }
                None => {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(Err(SyncError::WebSocket("connect timeout".into())));
                        return;
                    }
                    tracing::warn!("chat2 dial timed out; backing off");
                    self.spawn_http_sync();
                    match self.wait_backoff(&mut wake, &mut online, backoff).await {
                        Waited::Shutdown => return,
                        Waited::Nudge => self.spawn_http_sync(),
                        Waited::Woke => backoff = BACKOFF_BASE,
                        Waited::Elapsed => backoff = (backoff * 2).min(BACKOFF_CAP),
                    }
                    continue;
                }
            };

            match self.run_session(pipe, &mut ready).await {
                SessionEnd::Stop => {
                    if let Some(preview) = self.sink.preview() {
                        preview.disconnected();
                    }
                    return;
                }
                SessionEnd::Reconnect => {
                    if let Some(preview) = self.sink.preview() {
                        preview.disconnected();
                    }
                    use std::sync::atomic::Ordering::Relaxed;
                    // A session that had joined resets the backoff — without
                    // this, ~7 flaps pinned every future reconnect at the cap
                    // for the life of the client.
                    let joined = self.flags.connected.swap(false, Relaxed);
                    self.flags.disconnects.fetch_add(1, Relaxed);
                    let _ = self.events.send(ChatEvent::Disconnected);
                    if ready.is_some() {
                        if let Some(ready) = ready.take() {
                            let _ = ready
                                .send(Err(SyncError::Protocol("chat2 handshake failed".into())));
                        }
                        return;
                    }
                    if joined {
                        backoff = BACKOFF_BASE;
                    }
                    self.spawn_http_sync();
                    match self.wait_backoff(&mut wake, &mut online, backoff).await {
                        Waited::Shutdown => return,
                        Waited::Nudge => self.spawn_http_sync(),
                        Waited::Woke => backoff = BACKOFF_BASE,
                        Waited::Elapsed => backoff = (backoff * 2).min(BACKOFF_CAP),
                    }
                }
            }
        }
    }

    /// Run one bounded HTTPS synchronization cycle. The endpoint returns
    /// length-prefixed WS-compatible frames, so state/frontier/checkpoint
    /// decisions and row application stay on the same P0 code paths.
    fn spawn_http_sync(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let Some(transport) = self.transport.clone() else {
            return;
        };
        if self.http_sync_busy.swap(true, Relaxed) {
            self.http_sync_again.store(true, Relaxed);
            return;
        }
        let shared = self.shared.clone();
        let sink = self.sink.clone();
        let fetcher = self.fetcher.clone();
        let events = self.events.clone();
        let busy = self.http_sync_busy.clone();
        let sync_again = self.http_sync_again.clone();
        let sync_tx = self.http_sync_tx.clone();
        let http_timeout = self.tuning.http_timeout;
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                http_timeout,
                http_sync_once(
                    transport.as_ref(),
                    &shared,
                    sink.as_ref(),
                    fetcher.as_ref(),
                    &events,
                ),
            )
            .await
            .map_err(|_| SyncError::Protocol("chat HTTPS sync timed out".into()))
            .and_then(|result| result);
            let more_pending = result.is_ok() && !lock(&shared).pending.is_empty();
            if let Err(err) = result {
                tracing::debug!(error = %err, "chat2 HTTPS sync failed; WS/retry will continue");
            }
            busy.store(false, Relaxed);
            if sync_again.swap(false, Relaxed) || more_pending {
                let _ = sync_tx.try_send(());
            }
        });
    }

    /// Sleep out one backoff, cut short by system wake, a sibling dial
    /// success, or shutdown.
    async fn wait_backoff(
        &mut self,
        wake: &mut tokio::sync::broadcast::Receiver<()>,
        online: &mut tokio::sync::broadcast::Receiver<()>,
        wait: Duration,
    ) -> Waited {
        // Drain stale events: only wakes/successes DURING this wait count,
        // or our own last dial would cut every wait to zero.
        while wake.try_recv().is_ok() {}
        while online.try_recv().is_ok() {}
        tokio::select! {
            _ = tokio::time::sleep(wait) => Waited::Elapsed,
            _ = wake.recv() => Waited::Woke,
            _ = online.recv() => Waited::Woke,
            _ = self.nudge_rx.recv() => Waited::Nudge,
            _ = self.http_sync_rx.recv() => Waited::Nudge,
            _ = self.shutdown.changed() => {
                if *self.shutdown.borrow() {
                    Waited::Shutdown
                } else {
                    Waited::Elapsed
                }
            }
        }
    }

    async fn run_session(
        &mut self,
        mut pipe: BinPipe,
        ready: &mut Option<oneshot::Sender<Result<(), SyncError>>>,
    ) -> SessionEnd {
        use std::sync::atomic::Ordering::Relaxed;

        // ── hello / state ───────────────────────────────────────────────────
        lock(&self.shared).in_flight = None;
        let hello_cursor = lock(&self.shared).cursor;
        let hello = wire::encode(
            frame_type::HELLO,
            &wire::HelloHeader {
                cursor: hello_cursor,
                device: &self.device_id,
            },
            &[],
        );
        if pipe.tx.send(hello).await.is_err() {
            return SessionEnd::Reconnect;
        }
        let state = tokio::time::timeout(HELLO_DEADLINE, async {
            loop {
                let bytes = pipe.rx.recv().await?;
                let Some(frame) = wire::decode(&bytes) else {
                    tracing::warn!("chat2: bad frame during handshake");
                    return None;
                };
                if frame.kind == frame_type::STATE {
                    return Some(frame);
                }
                // Stale broadcast before our state: skip.
            }
        })
        .await;
        let Ok(Some(state_frame)) = state else {
            tracing::warn!("chat2: no state frame within deadline");
            return SessionEnd::Reconnect;
        };
        let Ok(state) = serde_json::from_value::<wire::StateHeader>(state_frame.header.clone())
        else {
            tracing::warn!("chat2: malformed state header");
            return SessionEnd::Reconnect;
        };
        lock(&self.shared).server = Some(state);

        // Cursor amnesty, once per client: a persisted cursor above the
        // checkpoint may be lying about what the local Loro document
        // materialized. Clamp it before planning catch-up so rows are
        // re-read. With no checkpoint, zero is the only honest lower bound.
        // Re-imports are idempotent and the room's compaction policy bounds
        // the amount of history normally revisited.
        if !self.cursor_amnesty_done.swap(true, Relaxed) {
            let clamp_to = if state.checkpoint_size > 0 {
                state.checkpoint_seq
            } else {
                0
            };
            let mut shared = lock(&self.shared);
            if shared.cursor > clamp_to {
                tracing::info!(
                    from = shared.cursor,
                    to = clamp_to,
                    "chat2: cursor amnesty — refetching rows the doc may have parked"
                );
                shared.cursor = clamp_to;
            }
        }
        let cursor = lock(&self.shared).cursor;
        self.flags.connected.store(true, Relaxed);
        if ready.is_none() {
            self.flags.rejoins.fetch_add(1, Relaxed);
        }
        let _ = self.events.send(ChatEvent::Connected);

        // Server behind our cursor = the room was reset/wiped. plan_catch_up
        // treats the cursor as fresh; SURFACE the signal too — the host's
        // re-seed recovery (chat-room.ts /reset) hangs off this event, and
        // masking it was exactly how the s2 wedge class stayed invisible.
        if hello_cursor > state.head_seq {
            self.flags.server_resets.fetch_add(1, Relaxed);
            tracing::warn!(
                cursor = hello_cursor,
                head_seq = state.head_seq,
                "chat2: server lost state (headSeq < cursor) — treating as \
                 fresh; host should re-seed via checkpoint"
            );
            let _ = self.events.send(ChatEvent::ServerReset);
        }

        // ── catch-up: checkpoint precision + row backfill ───────────────────
        // Same presence rule as `plan_catch_up`: SIZE, not seq — a seeded
        // room's checkpoint covers seq 0 (see the decision-table test).
        let contained =
            state.checkpoint_size == 0 || self.sink.contains_frontier(&state_frame.payload);
        let plan = plan_catch_up(cursor, &state, contained);
        let after = match plan {
            CatchUpPlan::RowsOnly { after } => after,
            CatchUpPlan::CheckpointThenRows { after } => {
                tracing::info!(
                    checkpoint_seq = state.checkpoint_seq,
                    checkpoint_size = state.checkpoint_size,
                    "chat2: fetching checkpoint"
                );
                // Deadline + shutdown-interruptible: a hung fetch (half-open
                // TCP, stalled link) must neither pin the actor forever nor
                // block `shutdown()`. The fetch is Range-resumable, so the
                // redial retries from wherever the bytes stopped.
                let fetch = self.fetcher.fetch();
                let fetched = tokio::select! {
                    fetched = tokio::time::timeout(CHECKPOINT_FETCH_DEADLINE, fetch) => fetched,
                    _ = self.shutdown.changed() => return SessionEnd::Stop,
                };
                let bytes = match fetched {
                    Ok(Ok(bytes)) => bytes,
                    Ok(Err(err)) => {
                        tracing::warn!(error = %err, "chat2: checkpoint fetch failed");
                        return SessionEnd::Reconnect;
                    }
                    Err(_) => {
                        tracing::warn!("chat2: checkpoint fetch timed out; redialing");
                        return SessionEnd::Reconnect;
                    }
                };
                if let Err(err) = self.sink.apply_checkpoint(&bytes, state.checkpoint_seq) {
                    tracing::warn!(error = %err, "chat2: checkpoint import failed");
                    return SessionEnd::Reconnect;
                }
                let mut shared = lock(&self.shared);
                shared.cursor = shared.cursor.max(state.checkpoint_seq);
                drop(shared);
                let _ = self.events.send(ChatEvent::Applied);
                after
            }
        };
        // The plan's `after` is the cursor for this backfill. It may move
        // upward when a contained checkpoint covers an older cursor, or
        // downward after a reset/amnesty. Keeping the old lower cursor would
        // make the first row after the checkpoint look like a false gap.
        lock(&self.shared).cursor = after;
        let rows_req = wire::encode(
            frame_type::ROWS_REQ,
            &wire::RowsReqHeader {
                after,
                // First backfill of this process redownloads own rows (see
                // `Actor::resumed`); reconnects skip them.
                exclude_own: self.resumed,
            },
            &[],
        );
        if pipe.tx.send(rows_req).await.is_err() {
            return SessionEnd::Reconnect;
        }
        let backfill = tokio::time::timeout(BACKFILL_DEADLINE, async {
            loop {
                let bytes = pipe.rx.recv().await?;
                let frame = wire::decode(&bytes)?;
                match frame.kind {
                    frame_type::ROWS_DONE => {
                        let done: wire::RowsDoneHeader =
                            serde_json::from_value(frame.header).ok()?;
                        return Some(done.head_seq);
                    }
                    _ => {
                        if !self.handle_frame(frame) {
                            return None;
                        }
                    }
                }
            }
        })
        .await;
        let Ok(Some(head_seq)) = backfill else {
            tracing::warn!("chat2: backfill did not complete");
            return SessionEnd::Reconnect;
        };
        {
            let mut shared = lock(&self.shared);
            shared.gap_repair |= head_seq > shared.cursor;
        }
        self.resumed = true;
        if let Some(ready) = ready.take() {
            let _ = ready.send(Ok(()));
        }
        let _ = self.events.send(ChatEvent::CaughtUp { head_seq });

        // Anything pending (offline writes, reconnect re-pushes) goes now —
        // the server's batchId dedupe makes replays exact no-ops.
        if !self.push_pending(&mut pipe).await {
            return SessionEnd::Reconnect;
        }

        // ── steady state ────────────────────────────────────────────────────
        let preview = self.sink.preview();
        let preview_notify = preview
            .as_ref()
            .map(|p| p.notify.clone())
            .unwrap_or_default();
        let mut preview_tick = tokio::time::interval(Duration::from_secs(1));
        preview_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_progress = tokio::time::Instant::now();
        let mut probe_deadline: Option<tokio::time::Instant> = None;
        let mut repair_deadline: Option<tokio::time::Instant> = None;
        // A live row can outrun the backfill and expose a hole immediately
        // after joining. Repair it before waiting for ordinary traffic.
        let mut gap_repairs = 0u32;
        if !self
            .maybe_repair_gap(&mut pipe, &mut gap_repairs, &mut repair_deadline)
            .await
        {
            return SessionEnd::Reconnect;
        }
        loop {
            let quiet_probe_at = last_progress + self.tuning.probe_quiet;
            let distant = tokio::time::Instant::now() + Duration::from_secs(86_400);
            let ack_at = lock(&self.shared)
                .in_flight
                .as_ref()
                .map(|(_, at)| *at)
                .unwrap_or(distant);
            let repair_at = repair_deadline.unwrap_or(distant);
            let deadline_at = probe_deadline
                .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86_400));
            let retry_at = lock(&self.shared)
                .retry_at
                .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86_400));
            let flush_at = lock(&self.shared)
                .flush_at
                .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86_400));
            tokio::select! {
                _ = preview_tick.tick(), if preview.is_some() => {
                    let cursor = lock(&self.shared).cursor;
                    preview.as_ref().unwrap().tick(cursor);
                }
                _ = preview_notify.notified(), if preview.is_some() => {
                    let p = preview.as_ref().unwrap();
                    let cursor = lock(&self.shared).cursor;
                    if let Some(bytes) = p.next_frame(cursor)
                        && pipe.tx.try_send(bytes).is_err() { p.send_failed(); }
                }
                frame = pipe.rx.recv() => {
                    let Some(bytes) = frame else {
                        return SessionEnd::Reconnect;
                    };
                    let Some(frame) = wire::decode(&bytes) else {
                        tracing::warn!("chat2: unparseable frame");
                        return SessionEnd::Reconnect;
                    };
                    let kind = frame.kind;
                    if !self.handle_frame(frame) {
                        return SessionEnd::Reconnect;
                    }
                    if matches!(kind, frame_type::ROW | frame_type::ACK | frame_type::STATE | frame_type::ROWS_DONE | frame_type::PROBE_OK) {
                        last_progress = tokio::time::Instant::now();
                    }
                    if kind == frame_type::PROBE_OK {
                        probe_deadline = None;
                    }
                    if kind == frame_type::ROWS_DONE {
                        repair_deadline = None;
                        if !lock(&self.shared).gap_repair {
                            gap_repairs = 0;
                        }
                    }
                    if !self
                        .maybe_repair_gap(&mut pipe, &mut gap_repairs, &mut repair_deadline)
                        .await
                    {
                        return SessionEnd::Reconnect;
                    }
                }
                _ = self.nudge_rx.recv() => {
                    let force_flush = lock(&self.shared).force_flush;
                    if force_flush && !self.push_head(&mut pipe).await {
                        return SessionEnd::Reconnect;
                    }
                }
                _ = self.http_sync_rx.recv() => {
                    // An overlapping bootstrap/recovery cycle finished.
                    // Once joined, queued writes belong on WS, not both paths.
                    if !self.push_pending(&mut pipe).await {
                        return SessionEnd::Reconnect;
                    }
                }
                _ = self.probe_rx.recv() => {
                    if !self.send_probe(&mut pipe, &mut probe_deadline).await {
                        return SessionEnd::Reconnect;
                    }
                }
                _ = self.redial_rx.recv() => {
                    tracing::info!("chat2: redial requested");
                    return SessionEnd::Reconnect;
                }
                // Transient (quota) rejection: probe with the HEAD batch on
                // a short clock (see `Shared::quota_blocked`); acks re-arm
                // the clock so the queue drains one-per-grant.
                _ = tokio::time::sleep_until(retry_at) => {
                    lock(&self.shared).retry_at = None;
                    if !self.push_head(&mut pipe).await {
                        return SessionEnd::Reconnect;
                    }
                }
                _ = tokio::time::sleep_until(flush_at), if flush_at < distant => {
                    {
                        let mut shared = lock(&self.shared);
                        shared.flush_at = None;
                        shared.force_flush = true;
                    }
                    if !self.push_head(&mut pipe).await { return SessionEnd::Reconnect; }
                }
                _ = tokio::time::sleep_until(quiet_probe_at) => {
                    if !self.send_probe(&mut pipe, &mut probe_deadline).await {
                        return SessionEnd::Reconnect;
                    }
                    last_progress = tokio::time::Instant::now();
                }
                _ = tokio::time::sleep_until(ack_at) => {
                    // An HTTP ACK may have retired it while we were waiting.
                    if lock(&self.shared).in_flight.as_ref().is_some_and(|(_, at)| *at <= tokio::time::Instant::now()) {
                        tracing::warn!("chat2: push ACK overdue; recovering via HTTP and redial");
                        return SessionEnd::Reconnect;
                    }
                }
                _ = tokio::time::sleep_until(repair_at) => {
                    tracing::warn!("chat2: row repair stalled; recovering via HTTP and redial");
                    return SessionEnd::Reconnect;
                }
                _ = tokio::time::sleep_until(deadline_at) => {
                    tracing::warn!("chat2: probe unanswered past deadline; redialing");
                    return SessionEnd::Reconnect;
                }
                _ = self.shutdown.changed() => {
                    if *self.shutdown.borrow() {
                        return SessionEnd::Stop;
                    }
                }
            }
        }
    }

    /// Request rows from the last honest cursor after a row/ack gap. The
    /// bound prevents a malformed or permanently truncated server log from
    /// causing an infinite request loop; the next reconnect performs the
    /// stronger full catch-up path.
    async fn maybe_repair_gap(
        &self,
        pipe: &mut BinPipe,
        repairs: &mut u32,
        deadline: &mut Option<tokio::time::Instant>,
    ) -> bool {
        const MAX_GAP_REPAIRS_PER_SESSION: u32 = 3;
        if deadline.is_some() {
            return true;
        }
        let (repair, after) = {
            let mut shared = lock(&self.shared);
            (std::mem::take(&mut shared.gap_repair), shared.cursor)
        };
        if !repair {
            return true;
        }
        *repairs += 1;
        if *repairs > MAX_GAP_REPAIRS_PER_SESSION {
            tracing::warn!("chat2: gap repairs exhausted; redialing for a full catch-up");
            return false;
        }
        tracing::info!(
            after,
            attempt = *repairs,
            "chat2: backfilling over a row gap"
        );
        *deadline = Some(tokio::time::Instant::now() + BACKFILL_DEADLINE);
        let req = wire::encode(
            frame_type::ROWS_REQ,
            &wire::RowsReqHeader {
                after,
                // A repair must include own rows too: an ACK can identify an
                // own row beyond the cursor while interleaved remote rows are
                // still missing.
                exclude_own: false,
            },
            &[],
        );
        pipe.tx.send(req).await.is_ok()
    }

    async fn send_probe(
        &self,
        pipe: &mut BinPipe,
        probe_deadline: &mut Option<tokio::time::Instant>,
    ) -> bool {
        let frame = wire::encode(frame_type::PROBE, &serde_json::json!({}), &[]);
        if pipe.tx.send(frame).await.is_err() {
            return false;
        }
        if probe_deadline.is_none() {
            *probe_deadline = Some(tokio::time::Instant::now() + PROBE_DEADLINE);
        }
        true
    }

    /// Send only the queue's head batch — the quota-probe path.
    async fn push_head(&self, pipe: &mut BinPipe) -> bool {
        let frame = {
            let mut shared = lock(&self.shared);
            if shared.in_flight.is_some() {
                return true;
            }
            // A held update never opens a push of its own.
            if shared.pending.front().is_some_and(|push| push.deferred) {
                return true;
            }
            if !shared.force_flush
                && shared
                    .flush_at
                    .is_some_and(|at| at > tokio::time::Instant::now())
            {
                return true;
            }
            shared.force_flush = !shared.pending.is_empty();
            shared.flush_at = None;
            if let Some(push) = shared.pending.front_mut() {
                if !push.persisted {
                    if self
                        .sink
                        .enqueue_outbox(&push.batch_id, &push.bytes)
                        .is_err()
                    {
                        shared.retry_at =
                            Some(tokio::time::Instant::now() + Duration::from_secs(1));
                        return true;
                    }
                    push.persisted = true;
                }
                push.sent = true;
            }
            let frame = shared.pending.front().map(|push| {
                (
                    push.batch_id.clone(),
                    wire::encode(
                        frame_type::PUSH,
                        &wire::PushHeader {
                            batch_id: &push.batch_id,
                        },
                        &push.bytes,
                    ),
                )
            });
            if let Some((id, _)) = &frame {
                shared.in_flight = Some((
                    id.clone(),
                    tokio::time::Instant::now() + self.tuning.push_ack_deadline,
                ));
            }
            frame.map(|(_, frame)| frame)
        };
        match frame {
            Some(frame) => pipe.tx.send(frame).await.is_ok(),
            None => true,
        }
    }

    async fn push_pending(&self, pipe: &mut BinPipe) -> bool {
        // Never burst the whole replay queue on reconnect. Send one head and
        // let its ACK advance the queue; this keeps a network recovery below
        // the server quota and prevents a quota error from becoming a replay
        // storm across reconnects.
        let has_pending = !lock(&self.shared).pending.is_empty();
        lock(&self.shared).force_flush = true;
        if has_pending {
            lock(&self.shared).quota_blocked = true;
        }
        self.push_head(pipe).await
    }

    /// Apply one inbound protocol frame. False = protocol breakdown, redial.
    fn handle_frame(&self, frame: wire::WireFrame) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        if frame.kind == frame_type::ERROR
            && let Some(preview) = self.sink.preview()
        {
            let code = frame.header["code"].as_str().unwrap_or("");
            if code.starts_with("preview_") || code.starts_with("bad_preview_") {
                preview.rejected(code);
                return true; // Never reject a durable batch or start HTTP recovery for preview errors.
            }
        }
        if (0x20..=0x26).contains(&frame.kind) {
            if let Some(preview) = self.sink.preview() {
                preview.receive(&wire::encode(frame.kind, &frame.header, &frame.payload));
            }
            return true; // Preview does not count as durable business progress.
        }
        match frame.kind {
            frame_type::ROW => {
                let Ok(row) = serde_json::from_value::<wire::RowHeader>(frame.header) else {
                    return false;
                };
                // Own-device rows can still arrive (live relay of a racing
                // second socket, or a server that ignored excludeOwn) — Loro
                // re-import is a no-op; the cursor advance is what matters.
                // A row beyond cursor+1 proves only that the server has it,
                // not that this client received the missing rows. Keep the
                // cursor honest and ask the session loop to repair the gap.
                let effective = {
                    let mut shared = lock(&self.shared);
                    if row.seq > shared.cursor + 1 {
                        shared.gap_repair = true;
                        tracing::warn!(
                            seq = row.seq,
                            cursor = shared.cursor,
                            "chat2: row gap detected; holding cursor"
                        );
                    } else {
                        shared.cursor = shared.cursor.max(row.seq);
                    }
                    shared.cursor
                };
                self.sink.apply_row(&frame.payload, effective);
                let _ = self.events.send(ChatEvent::Applied);
            }
            frame_type::ACK => {
                let Ok(ack) = serde_json::from_value::<wire::AckHeader>(frame.header) else {
                    return false;
                };
                if let Err(err) =
                    acknowledge_durable(&self.shared, self.sink.as_ref(), &ack.batch_id, ack.seq)
                {
                    tracing::error!(error = %err, "chat2: ACK persistence failed; retaining batch");
                    return false;
                }
                let _ = self.events.send(ChatEvent::Applied);
            }
            frame_type::PRESENCE => {
                let _ = self.events.send(ChatEvent::Presence);
            }
            frame_type::PROBE_OK => {
                let Ok(probe) = serde_json::from_value::<wire::ProbeOkHeader>(frame.header) else {
                    return false;
                };
                let mut shared = lock(&self.shared);
                if let Some(server) = &mut shared.server {
                    server.head_seq = server.head_seq.max(probe.head_seq);
                }
                shared.gap_repair |= probe.head_seq > shared.cursor;
            }
            frame_type::ROWS_DONE => {
                let Ok(done) = serde_json::from_value::<wire::RowsDoneHeader>(frame.header) else {
                    return false;
                };
                let mut shared = lock(&self.shared);
                shared.gap_repair = done.head_seq > shared.cursor;
            }
            frame_type::STATE => {
                // Late duplicate of a hello answer — refresh the server view.
                if let Ok(state) = serde_json::from_value::<wire::StateHeader>(frame.header) {
                    lock(&self.shared).server = Some(state);
                }
            }
            frame_type::ERROR => {
                self.flags.rejected.fetch_add(1, Relaxed);
                let code = frame.header["code"].as_str().unwrap_or("?").to_string();
                let message = frame.header["message"].as_str().unwrap_or("").to_string();
                let batch_id = frame.header["batchId"].as_str().unwrap_or("");
                lock(&self.shared).in_flight = None;
                match code.as_str() {
                    // Permanent verdicts on a specific batch: retire it, or
                    // it replays on every nudge/reconnect forever — the
                    // wedge class this design exists to kill. The ops stay
                    // in the local doc and travel with the next checkpoint.
                    "too_large" | "empty" | "bad_push" if !batch_id.is_empty() => {
                        let mut shared = lock(&self.shared);
                        let before = shared.pending.len();
                        shared.pending.retain(|p| p.batch_id != batch_id);
                        let dropped = before != shared.pending.len();
                        if !shared.pending.is_empty() {
                            shared.retry_at = Some(tokio::time::Instant::now());
                        }
                        drop(shared);
                        if dropped {
                            tracing::error!(
                                code,
                                batch_id,
                                "chat2: batch permanently rejected — retired \
                                 from the replay queue"
                            );
                            let _ = self.events.send(ChatEvent::PushRejected);
                        }
                    }
                    // Transient: the quota window passes on its own — keep
                    // the batch queued and head-probe on a short clock.
                    "quota" => {
                        let mut shared = lock(&self.shared);
                        shared.in_flight = None;
                        shared.quota_blocked = true;
                        shared.retry_at = Some(tokio::time::Instant::now() + QUOTA_RETRY);
                    }
                    // An unclassified rejection must not leave an unarmed
                    // queue stuck until the user happens to type again.
                    _ => return false,
                }
                tracing::warn!(code, message, "chat2: server rejected a frame");
            }
            other => {
                // Unknown server frame: tolerate (future protocol additions).
                tracing::debug!(kind = other, "chat2: ignoring unknown frame type");
            }
        }
        true
    }
}

/// Apply one HTTPS pull response without weakening the Chat2 cursor invariant.
/// The response is a sequence of length-prefixed WS frames. A truncated or
/// malformed response is retryable; rows after a hole are not applied and do
/// not advance the cursor.
pub(super) async fn http_sync_once(
    transport: &dyn ChatTransport,
    shared: &Arc<Mutex<Shared>>,
    sink: &dyn ChatDocSink,
    fetcher: &dyn CheckpointFetcher,
    events: &broadcast::Sender<ChatEvent>,
) -> Result<(), SyncError> {
    // POST pending rows first. A successful ACK retires only that batch; its
    // sequence number is not allowed to jump the receive cursor over
    // interleaved remote rows (the Chat2 P0 invariant).
    let pending: Vec<(String, Vec<u8>)> = {
        let mut shared = lock(shared);
        for push in shared.pending.iter_mut() {
            if !push.persisted {
                sink.enqueue_outbox(&push.batch_id, &push.bytes)
                    .map_err(SyncError::Protocol)?;
                push.persisted = true;
            }
            push.sent = true;
        }
        shared
            .pending
            .iter()
            .map(|push| (push.batch_id.clone(), push.bytes.clone()))
            .collect()
    };
    for (batch_id, bytes) in pending {
        let ack = transport.push(batch_id.clone(), bytes).await?;
        let value = serde_json::from_str::<serde_json::Value>(&ack)
            .map_err(|e| SyncError::Protocol(format!("chat push bad ack: {e}")))?;
        let ack_batch = value["batchId"]
            .as_str()
            .ok_or_else(|| SyncError::Protocol("chat push ack missing batchId".into()))?;
        if let Some(seq) = value["seq"].as_u64() {
            if ack_batch != batch_id {
                return Err(SyncError::Protocol("chat push ack batchId mismatch".into()));
            }
            acknowledge_durable(shared, sink, ack_batch, seq).map_err(SyncError::Protocol)?;
        } else {
            return Err(SyncError::Protocol("chat push ack missing seq".into()));
        }
        let _ = events.send(ChatEvent::Applied);
    }

    let mut cursor = lock(shared).cursor;
    let body = transport.fetch_rows(cursor).await?;
    if body.len() > MAX_HTTP_PULL_BYTES {
        return Err(SyncError::Protocol("chat pull response too large".into()));
    }
    let mut frames = Vec::new();
    let mut offset = 0usize;
    while offset < body.len() {
        if body.len() - offset < 4 {
            return Err(SyncError::Protocol(
                "chat pull truncated frame length".into(),
            ));
        }
        let len = u32::from_le_bytes([
            body[offset],
            body[offset + 1],
            body[offset + 2],
            body[offset + 3],
        ]) as usize;
        offset += 4;
        if len == 0 || len > body.len() - offset {
            return Err(SyncError::Protocol("chat pull truncated frame".into()));
        }
        let frame = wire::decode(&body[offset..offset + len])
            .ok_or_else(|| SyncError::Protocol("chat pull malformed frame".into()))?;
        frames.push(frame);
        offset += len;
    }
    let Some(state_frame) = frames.first() else {
        return Err(SyncError::Protocol("chat pull missing state".into()));
    };
    if state_frame.kind != frame_type::STATE {
        return Err(SyncError::Protocol("chat pull state is not first".into()));
    }
    let state = serde_json::from_value::<wire::StateHeader>(state_frame.header.clone())
        .map_err(|e| SyncError::Protocol(format!("chat pull bad state: {e}")))?;
    lock(shared).server = Some(state);
    if cursor > state.head_seq {
        lock(shared).cursor = 0;
        cursor = 0;
        let _ = events.send(ChatEvent::ServerReset);
    }
    let contained = state.checkpoint_size == 0 || sink.contains_frontier(&state_frame.payload);
    let plan = plan_catch_up(cursor, &state, contained);
    let after = match plan {
        CatchUpPlan::RowsOnly { after } => after,
        CatchUpPlan::CheckpointThenRows { after } => {
            let bytes = tokio::time::timeout(CHECKPOINT_FETCH_DEADLINE, fetcher.fetch())
                .await
                .map_err(|_| SyncError::Protocol("chat checkpoint pull timed out".into()))??;
            sink.apply_checkpoint(&bytes, state.checkpoint_seq)
                .map_err(SyncError::Loro)?;
            let mut shared_state = lock(shared);
            shared_state.cursor = shared_state.cursor.max(state.checkpoint_seq);
            drop(shared_state);
            let _ = events.send(ChatEvent::Applied);
            after
        }
    };
    if lock(shared).cursor < after {
        lock(shared).cursor = after;
        // A contained checkpoint may let us skip older rows. Persist that
        // cursor movement with the unchanged local document so restart does
        // not repeat the same HTTP pull forever.
        sink.advance_cursor(after);
    }
    let mut applied = false;
    for frame in frames.into_iter().skip(1) {
        if frame.kind == frame_type::ROWS_DONE {
            continue;
        }
        if frame.kind != frame_type::ROW {
            continue;
        }
        let row = serde_json::from_value::<wire::RowHeader>(frame.header)
            .map_err(|e| SyncError::Protocol(format!("chat pull bad row: {e}")))?;
        let current = lock(shared).cursor;
        if row.seq <= current {
            continue;
        }
        if row.seq != current + 1 {
            tracing::warn!(
                cursor = current,
                seq = row.seq,
                "chat HTTPS pull found a row gap"
            );
            break;
        }
        sink.apply_row(&frame.payload, row.seq);
        lock(shared).cursor = row.seq;
        applied = true;
    }
    if applied {
        let _ = events.send(ChatEvent::Applied);
    }
    Ok(())
}
