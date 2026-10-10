//! The connection actor: dial, hello/state handshake, push/ack, presence,
//! probe liveness and reconnect, plus the HTTPS fallback sync.

use super::frames::{ClientFrame, ServerFrame};
use super::*;

pub(super) struct Actor {
    pub(super) doc: Arc<Mutex<RegistryDoc>>,
    pub(super) device_id: String,
    pub(super) connector: Arc<dyn TextConnector>,
    pub(super) tuning: RegistryTuning,
    pub(super) events: broadcast::Sender<RegistryEvent>,
    pub(super) shutdown: watch::Receiver<bool>,
    pub(super) nudge_rx: mpsc::Receiver<()>,
    pub(super) probe_rx: mpsc::Receiver<()>,
    pub(super) redial_rx: mpsc::Receiver<()>,
    pub(super) presence_rx: mpsc::Receiver<(i64, Option<serde_json::Value>)>,
    pub(super) presence: Arc<Mutex<HashMap<String, (i64, tokio::time::Instant)>>>,
    pub(super) stats: Arc<Stats>,
    pub(super) transport: Option<Arc<dyn RegistryTransport>>,
    pub(super) sync_busy: Arc<std::sync::atomic::AtomicBool>,
    pub(super) sync_again: Arc<std::sync::atomic::AtomicBool>,
    pub(super) sync_tx: mpsc::Sender<()>,
    pub(super) sync_rx: mpsc::Receiver<()>,
}

enum SessionEnd {
    /// Transport died / probe deadline / requested redial: back off, redial.
    Reconnect,
    /// Shutdown requested: stop the actor.
    Stop,
}

/// How a backoff wait ended.
enum Waited {
    Elapsed,
    /// System wake or a sibling dial succeeded: redial NOW on fresh backoff.
    Woke,
    /// A local write requested an HTTPS sync while the socket was offline.
    Nudge,
    Shutdown,
}

impl Actor {
    pub(super) async fn run(mut self, ready: oneshot::Sender<Result<(), SyncError>>) {
        let mut ready = Some(ready);
        let mut backoff = BACKOFF_BASE;
        if self.transport.is_some() {
            if let Some(ready) = ready.take() {
                let _ = ready.send(Ok(()));
            }
            self.spawn_offline_sync();
        }
        // Suspend/resume and sibling-dial successes are EVENTS that end a
        // backoff wait immediately (`cypher_net::wake`) — without them a recovered
        // network still waited out the full accumulated delay.
        let mut wake = cypher_net::wake::subscribe();
        let mut online = cypher_net::wake::subscribe_online();
        loop {
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
                    _ = self.nudge_rx.recv() => self.spawn_offline_sync(),
                    _ = self.sync_rx.recv() => self.spawn_offline_sync(),
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
                    tracing::warn!(error = %err, "registry dial failed; backing off");
                    self.spawn_offline_sync();
                    match self.wait_backoff(&mut wake, &mut online, backoff).await {
                        Waited::Shutdown => return,
                        Waited::Nudge => self.spawn_offline_sync(),
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
                    tracing::warn!("registry dial timed out; backing off");
                    self.spawn_offline_sync();
                    match self.wait_backoff(&mut wake, &mut online, backoff).await {
                        Waited::Shutdown => return,
                        Waited::Nudge => self.spawn_offline_sync(),
                        Waited::Woke => backoff = BACKOFF_BASE,
                        Waited::Elapsed => backoff = (backoff * 2).min(BACKOFF_CAP),
                    }
                    continue;
                }
            };

            match self.run_session(pipe, &mut ready).await {
                SessionEnd::Stop => return,
                SessionEnd::Reconnect => {
                    lock(&self.doc).mark_disconnected();
                    // A session that had joined resets the backoff — without
                    // this, ~7 flaps pinned every future reconnect at the cap
                    // for the life of the client.
                    let joined = self
                        .stats
                        .connected
                        .swap(false, std::sync::atomic::Ordering::Relaxed);
                    self.stats
                        .disconnects
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let _ = self.events.send(RegistryEvent::Disconnected);
                    if ready.is_some() {
                        // Handshake failed on the very first session.
                        if let Some(ready) = ready.take() {
                            let _ = ready
                                .send(Err(SyncError::Protocol("registry handshake failed".into())));
                        }
                        return;
                    }
                    if joined {
                        backoff = BACKOFF_BASE;
                    }
                    self.spawn_offline_sync();
                    match self.wait_backoff(&mut wake, &mut online, backoff).await {
                        Waited::Shutdown => return,
                        Waited::Nudge => self.spawn_offline_sync(),
                        Waited::Woke => backoff = BACKOFF_BASE,
                        Waited::Elapsed => backoff = (backoff * 2).min(BACKOFF_CAP),
                    }
                }
            }
        }
    }

    /// Flush pending registry ops and then pull the server delta over HTTPS.
    /// The server applies the same LWW/sequence semantics as the WS path.
    fn spawn_offline_sync(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let Some(transport) = self.transport.clone() else {
            return;
        };
        if self.sync_busy.swap(true, Relaxed) {
            self.sync_again.store(true, Relaxed);
            return;
        }
        let doc = self.doc.clone();
        let events = self.events.clone();
        let presence = self.presence.clone();
        let stats = self.stats.clone();
        let busy = self.sync_busy.clone();
        let sync_again = self.sync_again.clone();
        let sync_tx = self.sync_tx.clone();
        let http_timeout = self.tuning.http_timeout;
        tokio::spawn(async move {
            let batches: Vec<PendingBatch> = lock(&doc).take_pushable();
            let mut push_failed = false;
            for batch in batches {
                let body = serde_json::json!({
                    "batch": batch.batch,
                    "ops": batch.ops
                })
                .to_string();
                match tokio::time::timeout(http_timeout, transport.push(body)).await {
                    Err(_) => {
                        tracing::debug!("registry HTTPS push timed out");
                        push_failed = true;
                        break;
                    }
                    Ok(Err(err)) => {
                        tracing::debug!(error = %err, "registry HTTPS push failed");
                        push_failed = true;
                        break;
                    }
                    Ok(Ok(ack)) => match serde_json::from_str::<serde_json::Value>(&ack) {
                        Ok(value) => {
                            if let (Some(batch), Some(seq)) =
                                (value["batch"].as_str(), value["seq"].as_u64())
                            {
                                lock(&doc).ack_batch(batch, seq);
                                stats.last_ack_ms.store(now_ms(), Relaxed);
                                let _ = events.send(RegistryEvent::Applied);
                            } else {
                                tracing::debug!("registry HTTPS push ACK missing batch/seq");
                                push_failed = true;
                                break;
                            }
                        }
                        Err(err) => {
                            tracing::debug!(error = %err, "registry HTTPS push ACK malformed");
                            push_failed = true;
                            break;
                        }
                    },
                }
            }
            if push_failed {
                lock(&doc).mark_disconnected();
            }
            let since = lock(&doc).cursor();
            match tokio::time::timeout(http_timeout, transport.fetch(since)).await {
                Err(_) => tracing::debug!("registry HTTPS pull timed out"),
                Ok(Err(err)) => tracing::debug!(error = %err, "registry HTTPS pull failed"),
                Ok(Ok(body)) => {
                    if body.len() > MAX_HTTP_PULL_BYTES {
                        tracing::warn!(
                            bytes = body.len(),
                            "registry HTTPS pull response too large"
                        );
                        // Keep the single-flight cleanup below on all paths.
                    }
                    if body.len() <= MAX_HTTP_PULL_BYTES {
                        #[derive(Deserialize)]
                        #[serde(rename_all = "camelCase")]
                        struct PullBody {
                            seq: u64,
                            full: bool,
                            gc_floor: u64,
                            rows: Vec<RegistryRow>,
                            #[serde(default)]
                            presence: HashMap<String, i64>,
                        }
                        match serde_json::from_str::<PullBody>(&body) {
                            Ok(pull) => {
                                let mut registry = lock(&doc);
                                registry.apply_state(pull.seq, pull.full, pull.gc_floor, pull.rows);
                                drop(registry);
                                let now = tokio::time::Instant::now();
                                let mut map = lock(&presence);
                                for (device, at) in pull.presence {
                                    map.insert(device, (at, now));
                                }
                                stats.server_known.store(true, Relaxed);
                                stats.last_pushed_ms.store(now_ms(), Relaxed);
                                let _ = events.send(RegistryEvent::Applied);
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, "registry HTTPS pull body malformed")
                            }
                        }
                    }
                }
            }
            busy.store(false, Relaxed);
            if sync_again.swap(false, Relaxed) {
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
            _ = self.sync_rx.recv() => Waited::Nudge,
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
        mut pipe: TextPipe,
        ready: &mut Option<oneshot::Sender<Result<(), SyncError>>>,
    ) -> SessionEnd {
        use std::sync::atomic::Ordering::Relaxed;

        // ── hello / state handshake ─────────────────────────────────────────
        let cursor = {
            let doc = lock(&self.doc);
            let cursor = doc.cursor();
            if cursor == 0 && doc.pending_len() == 0 && doc.generation() == 0 {
                None
            } else {
                Some(cursor)
            }
        };
        let hello = serde_json::to_string(&ClientFrame::Hello {
            cursor,
            device: &self.device_id,
        })
        .expect("hello serializes");
        if pipe.tx.send(hello).await.is_err() {
            return SessionEnd::Reconnect;
        }
        let state = tokio::time::timeout(HELLO_DEADLINE, async {
            loop {
                match pipe.rx.recv().await {
                    Some(text) => match serde_json::from_str::<ServerFrame>(&text) {
                        Ok(frame @ ServerFrame::State { .. }) => return Some(frame),
                        Ok(_) => continue, // stale broadcast before our state
                        Err(err) => {
                            tracing::warn!(error = %err, "registry: bad frame during handshake");
                            return None;
                        }
                    },
                    None => None?,
                }
            }
        })
        .await;
        let Ok(Some(ServerFrame::State {
            seq,
            full,
            gc_floor,
            rows,
            presence,
        })) = state
        else {
            tracing::warn!("registry: no state frame within deadline");
            return SessionEnd::Reconnect;
        };
        {
            let mut doc = lock(&self.doc);
            let outcome = doc.apply_state(seq, full, gc_floor, rows);
            if full {
                self.stats.full_resyncs.fetch_add(1, Relaxed);
            }
            if outcome == StateOutcome::Reseeded {
                tracing::info!("registry: server behind local state; re-seeding");
            }
        }
        {
            let now = tokio::time::Instant::now();
            let mut map = lock(&self.presence);
            for (device, at) in presence {
                map.insert(device, (at, now));
            }
        }
        self.stats.connected.store(true, Relaxed);
        self.stats.server_known.store(true, Relaxed);
        self.stats.last_pushed_ms.store(now_ms(), Relaxed);
        if ready.is_none() {
            self.stats.rejoins.fetch_add(1, Relaxed);
        }
        if let Some(ready) = ready.take() {
            let _ = ready.send(Ok(()));
        }
        let _ = self.events.send(RegistryEvent::Connected);
        let _ = self.events.send(RegistryEvent::Applied);

        // Anything pending (offline writes, reseeds, migration) pushes now.
        if !self.push_pending(&mut pipe).await {
            return SessionEnd::Reconnect;
        }

        // ── steady state ────────────────────────────────────────────────────
        let mut last_frame = tokio::time::Instant::now();
        let mut probe_deadline: Option<tokio::time::Instant> = None;
        loop {
            let quiet_probe_at = last_frame + self.tuning.probe_quiet;
            let deadline_at = probe_deadline
                .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86_400));
            tokio::select! {
                frame = pipe.rx.recv() => {
                    let Some(text) = frame else {
                        return SessionEnd::Reconnect;
                    };
                    last_frame = tokio::time::Instant::now();
                    probe_deadline = None;
                    if !self.handle_frame(&text) {
                        return SessionEnd::Reconnect;
                    }
                }
                _ = self.nudge_rx.recv() => {
                    // Joined: the socket carries the write, and nothing else
                    // does. The HTTPS cycle that used to run beside it cost a
                    // full billable Durable Object request per mutation (a WS
                    // message bills at 20:1), and it bought no safety the
                    // socket does not already provide — SILENCE_LEASE tears
                    // down a wedged session inside 45s, and a gap in the row
                    // sequence redials through the probe path. The HTTPS
                    // transport stays exactly where it is needed: dialing,
                    // backoff waits, and every offline branch below.
                    if !self.push_pending(&mut pipe).await {
                        return SessionEnd::Reconnect;
                    }
                }
                _ = self.sync_rx.recv() => {
                    // An overlapping offline cycle finished. Anything it left
                    // queued belongs on the socket now, not on a second pull
                    // (chat2's rule, chat_client.rs: once joined, queued
                    // writes take one path).
                    if !self.push_pending(&mut pipe).await {
                        return SessionEnd::Reconnect;
                    }
                }
                beat = self.presence_rx.recv() => {
                    if let Some((at, activity)) = beat {
                        let frame = serde_json::to_string(&ClientFrame::Presence {
                            at,
                            activity: activity.as_ref(),
                        })
                        .expect("presence serializes");
                        if pipe.tx.send(frame).await.is_err() {
                            return SessionEnd::Reconnect;
                        }
                    }
                }
                _ = self.probe_rx.recv() => {
                    if !self.send_probe(&mut pipe, &mut probe_deadline).await {
                        return SessionEnd::Reconnect;
                    }
                }
                _ = self.redial_rx.recv() => {
                    tracing::info!("registry: redial requested");
                    return SessionEnd::Reconnect;
                }
                _ = tokio::time::sleep_until(quiet_probe_at) => {
                    if !self.send_probe(&mut pipe, &mut probe_deadline).await {
                        return SessionEnd::Reconnect;
                    }
                    // Don't re-arm the quiet timer against the same silence.
                    last_frame = tokio::time::Instant::now();
                }
                _ = tokio::time::sleep_until(deadline_at) => {
                    tracing::warn!("registry: probe unanswered past deadline; redialing");
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

    async fn send_probe(
        &self,
        pipe: &mut TextPipe,
        probe_deadline: &mut Option<tokio::time::Instant>,
    ) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        self.stats.probes.fetch_add(1, Relaxed);
        let frame = serde_json::to_string(&ClientFrame::Probe).expect("probe serializes");
        if pipe.tx.send(frame).await.is_err() {
            return false;
        }
        if probe_deadline.is_none() {
            *probe_deadline = Some(tokio::time::Instant::now() + PROBE_DEADLINE);
        }
        true
    }

    async fn push_pending(&self, pipe: &mut TextPipe) -> bool {
        let batches: Vec<PendingBatch> = lock(&self.doc).take_pushable();
        for batch in batches {
            let frame = serde_json::to_string(&ClientFrame::Push {
                batch: &batch.batch,
                ops: &batch.ops,
            })
            .expect("push serializes");
            if pipe.tx.send(frame).await.is_err() {
                return false;
            }
        }
        true
    }

    /// Apply one inbound protocol frame. False = protocol breakdown, redial.
    fn handle_frame(&self, text: &str) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        let frame = match serde_json::from_str::<ServerFrame>(text) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::warn!(error = %err, "registry: unparseable frame");
                return false;
            }
        };
        match frame {
            ServerFrame::Rows { seq, rows } => {
                let contiguous = lock(&self.doc).apply_rows(seq, rows);
                self.stats.last_pushed_ms.store(now_ms(), Relaxed);
                let _ = self.events.send(RegistryEvent::Applied);
                if !contiguous {
                    // The frame itself is useful, but the cursor held at the
                    // last contiguous row. Reconnect so the next hello
                    // backfills the missing sequence range.
                    tracing::warn!(seq, "registry: broadcast seq gap; resyncing");
                    return false;
                }
            }
            ServerFrame::Ack { batch, seq, .. } => {
                lock(&self.doc).ack_batch(&batch, seq);
                self.stats.last_ack_ms.store(now_ms(), Relaxed);
                let _ = self.events.send(RegistryEvent::Applied);
            }
            ServerFrame::Presence { device, at } => {
                lock(&self.presence).insert(device, (at, tokio::time::Instant::now()));
                let _ = self.events.send(RegistryEvent::Presence);
            }
            ServerFrame::ProbeOk { .. } => {
                self.stats.last_pushed_ms.store(now_ms(), Relaxed);
            }
            ServerFrame::State {
                seq,
                full,
                gc_floor,
                rows,
                presence,
            } => {
                // Servers only send state as a hello answer, but applying a
                // late duplicate is harmless and simpler than special-casing.
                let mut doc = lock(&self.doc);
                doc.apply_state(seq, full, gc_floor, rows);
                drop(doc);
                self.stats.server_known.store(true, Relaxed);
                let now = tokio::time::Instant::now();
                let mut map = lock(&self.presence);
                for (device, at) in presence {
                    map.insert(device, (at, now));
                }
                let _ = self.events.send(RegistryEvent::Applied);
            }
            ServerFrame::Error { code, message } => {
                self.stats.rejected.fetch_add(1, Relaxed);
                tracing::warn!(code, message, "registry: server rejected a frame");
            }
        }
        true
    }
}
