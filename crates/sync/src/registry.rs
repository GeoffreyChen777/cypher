//! RegistryClient — WebSocket transport for the workspace registry
//! (docs/registry-sync.md): hello/cursor handshake, push/ack for pending op
//! batches, merged-row broadcasts, presence beats, probe/redial liveness, and
//! reconnect with exponential backoff.
//!
//! The client owns no row semantics: everything applies through the shared
//! [`cypher_doc::RegistryDoc`] under a lock. Wire frames are JSON text —
//! byte-compatible with `edge/src/registry-room.ts`.
//!
//! Liveness discipline: the transport-level text ping elicits a runtime
//! auto-pong that proves NOTHING about the DO, so room-level health is judged
//! only by protocol frames — a probe that goes unanswered past its deadline
//! tears the session down for a fresh dial.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use cypher_doc::{PendingBatch, RegistryDoc, RegistryRow, StateOutcome};

use crate::types::{RoomStatsSnapshot, StaticUrl, SyncError, UrlProvider};
mod actor;
mod frames;

use actor::Actor;

use crate::{lock, now_ms};

/// Text `"ping"` keepalive interval (answered by the DO auto-response pair
/// without waking it — transport liveness only).
const PING_INTERVAL: Duration = Duration::from_secs(15);
/// Transport silence lease: pongs count, so a healthy socket never trips this.
const SILENCE_LEASE: Duration = Duration::from_secs(45);
/// Bound on one dial attempt (URL fetch + WS handshake).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// The server must answer a hello with `state` within this deadline.
const HELLO_DEADLINE: Duration = Duration::from_secs(15);
/// A probe's `probe-ok` (or any other protocol frame) must arrive within this
/// deadline, or the session is torn down for a fresh socket.
const PROBE_DEADLINE: Duration = Duration::from_secs(10);
/// Presence entries older than this are treated as expired (mirrors the
/// EphemeralStore's 30s TTL the old workspace room used).
const PRESENCE_TTL: Duration = Duration::from_secs(30);
const BACKOFF_BASE: Duration = Duration::from_millis(250);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
const HTTP_SYNC_TIMEOUT: Duration = Duration::from_secs(30);
/// Registry pulls are JSON and buffered in memory. The Edge endpoint returns
/// the current table, so refuse an unexpectedly large body before decoding.
const MAX_HTTP_PULL_BYTES: usize = 8 * 1024 * 1024;

/// Quiet-room probe cadence default. The workspace registry is one room per
/// engine, so a fixed 15min cadence costs ~100 DO wakes/day total.
const PROBE_QUIET_DEFAULT: Duration = Duration::from_secs(900);

/// Per-client tuning.
#[derive(Clone, Copy, Debug)]
pub struct RegistryTuning {
    /// Send a liveness probe after this much protocol-frame silence.
    pub probe_quiet: Duration,
    /// Bound one HTTPS push/pull operation so a stalled socket cannot wedge
    /// the single-flight gate forever.
    pub http_timeout: Duration,
}

impl Default for RegistryTuning {
    fn default() -> Self {
        Self {
            probe_quiet: PROBE_QUIET_DEFAULT,
            http_timeout: HTTP_SYNC_TIMEOUT,
        }
    }
}

/// Connection/sync lifecycle notifications (best-effort broadcast).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryEvent {
    /// Joined (or re-joined); the hello state has been applied.
    Connected,
    /// The connection dropped; the client is backing off before redialing.
    Disconnected,
    /// Rows/acks were applied to the doc — republish and persist.
    Applied,
    /// A remote device's presence beat arrived.
    Presence,
}

/// Plain-HTTPS pull/push transport used when the WebSocket is unavailable.
pub trait RegistryTransport: Send + Sync + 'static {
    fn fetch(&self, since: u64) -> BoxFuture<'static, Result<String, SyncError>>;
    fn push(&self, body: String) -> BoxFuture<'static, Result<String, SyncError>>;
}

pub(crate) struct TextPipe {
    pub(crate) tx: mpsc::Sender<String>,
    pub(crate) rx: mpsc::Receiver<String>,
}

pub(crate) trait TextConnector: Send + Sync + 'static {
    fn connect(&self) -> BoxFuture<'static, Result<TextPipe, SyncError>>;
}

struct WsTextConnector {
    url: Arc<dyn UrlProvider>,
}

impl TextConnector for WsTextConnector {
    fn connect(&self) -> BoxFuture<'static, Result<TextPipe, SyncError>> {
        let provider = self.url.clone();
        Box::pin(async move {
            let url = provider.url().await?;
            let ws = cypher_net::dial::connect_ws(&url)
                .await
                .map_err(|e| SyncError::WebSocket(e.to_string()))?;
            let (out_tx, out_rx) = mpsc::channel(64);
            let (in_tx, in_rx) = mpsc::channel(64);
            tokio::spawn(pump(ws, out_rx, in_tx));
            Ok(TextPipe {
                tx: out_tx,
                rx: in_rx,
            })
        })
    }
}

/// Shuttle text frames between the WebSocket and the actor's channels, plus
/// the ping keepalive and the transport silence lease.
async fn pump(
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    mut out_rx: mpsc::Receiver<String>,
    in_tx: mpsc::Sender<String>,
) {
    let (mut sink, mut stream) = ws.split();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;
    let mut last_rx = tokio::time::Instant::now();
    loop {
        tokio::select! {
            frame = out_rx.recv() => match frame {
                Some(text) => {
                    if sink.send(WsMessage::Text(text)).await.is_err() {
                        break;
                    }
                }
                None => {
                    let _ = sink.send(WsMessage::Close(None)).await;
                    break;
                }
            },
            frame = stream.next() => match frame {
                Some(Ok(WsMessage::Text(text))) => {
                    last_rx = tokio::time::Instant::now();
                    let text = text.to_string();
                    if text != "pong" && in_tx.send(text).await.is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {
                    last_rx = tokio::time::Instant::now();
                }
                Some(Err(_)) | None => break,
            },
            _ = ping.tick() => {
                if sink.send(WsMessage::Text("ping".into())).await.is_err() {
                    break;
                }
            }
            _ = tokio::time::sleep_until(last_rx + SILENCE_LEASE) => {
                tracing::warn!("registry socket silent past lease; treating as dead");
                break;
            }
        }
    }
}

// ── stats (RoomStatsSnapshot-compatible so SyncStatus/`cypher sync` render it) ─

#[derive(Default)]
struct Stats {
    connected: std::sync::atomic::AtomicBool,
    server_known: std::sync::atomic::AtomicBool,
    last_pushed_ms: std::sync::atomic::AtomicI64,
    last_ack_ms: std::sync::atomic::AtomicI64,
    rejoins: std::sync::atomic::AtomicU64,
    probes: std::sync::atomic::AtomicU64,
    full_resyncs: std::sync::atomic::AtomicU64,
    disconnects: std::sync::atomic::AtomicU64,
    rejected: std::sync::atomic::AtomicU64,
}

impl Stats {
    fn snapshot(&self) -> RoomStatsSnapshot {
        use std::sync::atomic::Ordering::Relaxed;
        RoomStatsSnapshot {
            connected: self.connected.load(Relaxed),
            server_known: self.server_known.load(Relaxed),
            last_pushed_ms: self.last_pushed_ms.load(Relaxed),
            last_ack_ms: self.last_ack_ms.load(Relaxed),
            rejoins: self.rejoins.load(Relaxed),
            probes: self.probes.load(Relaxed),
            full_resyncs: self.full_resyncs.load(Relaxed),
            disconnects: self.disconnects.load(Relaxed),
            rejected: self.rejected.load(Relaxed),
        }
    }
}

// ── the client ──────────────────────────────────────────────────────────────

/// A live registry-room membership for one [`RegistryDoc`].
///
/// Owns a background actor that keeps the doc converged with the room. The
/// engine host mutates the doc under its own lock and calls [`Self::nudge`]
/// to push; server frames apply through the same lock.
pub struct RegistryClient {
    doc: Arc<Mutex<RegistryDoc>>,
    events: broadcast::Sender<RegistryEvent>,
    shutdown: watch::Sender<bool>,
    nudge: mpsc::Sender<()>,
    probe: mpsc::Sender<()>,
    redial: mpsc::Sender<()>,
    presence_out: mpsc::Sender<(i64, Option<serde_json::Value>)>,
    presence: Arc<Mutex<HashMap<String, (i64, tokio::time::Instant)>>>,
    stats: Arc<Stats>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl RegistryClient {
    /// Connect (fixed URL — dev/tests).
    pub async fn connect(
        url: &str,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
    ) -> Result<Self, SyncError> {
        Self::connect_via(Arc::new(StaticUrl(url.to_string())), doc, device_id).await
    }

    /// Connect with a per-dial URL provider (fresh `?token=` every attempt).
    /// Resolves once the initial hello/state handshake lands; a first-attempt
    /// failure is returned as `Err` (callers own the initial-join retry, same
    /// contract as `ChatClient`). After that the client reconnects itself.
    pub(crate) async fn connect_via(
        provider: Arc<dyn UrlProvider>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
    ) -> Result<Self, SyncError> {
        Self::connect_via_tuned(provider, doc, device_id, RegistryTuning::default()).await
    }

    pub async fn connect_via_tuned(
        provider: Arc<dyn UrlProvider>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
    ) -> Result<Self, SyncError> {
        let connector = Arc::new(WsTextConnector { url: provider });
        Self::connect_with_tuned(connector, doc, device_id, tuning).await
    }

    /// Start local-first with an HTTPS pull/push path alongside WebSocket.
    pub async fn connect_via_transport(
        provider: Arc<dyn UrlProvider>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        transport: Arc<dyn RegistryTransport>,
    ) -> Result<Self, SyncError> {
        let connector = Arc::new(WsTextConnector { url: provider });
        Self::connect_with_transport(
            connector,
            doc,
            device_id,
            RegistryTuning::default(),
            Some(transport),
        )
        .await
    }

    /// Tunable variant used by the engine's long-lived registry supervisor.
    pub async fn connect_via_transport_tuned(
        provider: Arc<dyn UrlProvider>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
        transport: Arc<dyn RegistryTransport>,
    ) -> Result<Self, SyncError> {
        let connector = Arc::new(WsTextConnector { url: provider });
        Self::connect_with_transport(connector, doc, device_id, tuning, Some(transport)).await
    }

    pub(crate) async fn connect_with_tuned(
        connector: Arc<dyn TextConnector>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
    ) -> Result<Self, SyncError> {
        Self::connect_with_transport(connector, doc, device_id, tuning, None).await
    }

    pub(crate) async fn connect_with_transport(
        connector: Arc<dyn TextConnector>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
        transport: Option<Arc<dyn RegistryTransport>>,
    ) -> Result<Self, SyncError> {
        let (events, _) = broadcast::channel(256);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = oneshot::channel();
        let (nudge_tx, nudge_rx) = mpsc::channel(1);
        let (probe_tx, probe_rx) = mpsc::channel(1);
        let (redial_tx, redial_rx) = mpsc::channel(1);
        let (sync_tx, sync_rx) = mpsc::channel(1);
        let (presence_tx, presence_rx) = mpsc::channel(4);
        let presence = Arc::new(Mutex::new(HashMap::new()));
        let stats = Arc::new(Stats::default());

        let actor = Actor {
            doc: doc.clone(),
            device_id: device_id.to_string(),
            connector,
            tuning,
            events: events.clone(),
            shutdown: shutdown_rx,
            nudge_rx,
            probe_rx,
            redial_rx,
            presence_rx,
            presence: presence.clone(),
            stats: stats.clone(),
            transport,
            sync_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sync_again: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sync_tx,
            sync_rx,
        };
        let task = tokio::spawn(actor.run(ready_tx));

        match ready_rx.await {
            Ok(Ok(())) => Ok(Self {
                doc,
                events,
                shutdown: shutdown_tx,
                nudge: nudge_tx,
                probe: probe_tx,
                redial: redial_tx,
                presence_out: presence_tx,
                presence,
                stats,
                task: Some(task),
            }),
            Ok(Err(err)) => {
                task.abort();
                Err(err)
            }
            Err(_) => {
                task.abort();
                Err(SyncError::Closed)
            }
        }
    }

    pub fn doc(&self) -> &Arc<Mutex<RegistryDoc>> {
        &self.doc
    }

    pub fn events(&self) -> broadcast::Receiver<RegistryEvent> {
        self.events.subscribe()
    }

    /// Local writes were enqueued — push pending batches now.
    pub fn nudge(&self) {
        // Every actor phase handles this wake, including offline/backoff.
        // A second signal on sync_rx scheduled another HTTP pull for the same
        // mutation. That channel is only for a genuinely overlapping HTTP cycle.
        let _ = self.nudge.try_send(());
    }

    /// Publish this device's presence beat (epoch ms), optionally carrying the
    /// viewport's activity refresh so it costs no request of its own.
    pub fn set_presence(&self, at: i64) {
        let _ = self.presence_out.try_send((at, None));
    }

    /// Presence beat plus a piggybacked activity refresh.
    pub fn set_presence_with_activity(&self, at: i64, activity: serde_json::Value) {
        let _ = self.presence_out.try_send((at, Some(activity)));
    }

    /// Send a presence beat carrying `activity` NOW, for a viewport transition
    /// that must reach the room promptly rather than on the next 15s beat.
    ///
    /// `false` when there is no live session to carry it (or its queue is
    /// full), so the caller can fall back to a request of its own. A frame
    /// accepted here can still be lost if the socket dies right after; the
    /// next regular beat re-carries the same pending activity, so that costs
    /// at most one beat interval of staleness, never a lost transition.
    pub fn beat_with_activity_now(&self, at: i64, activity: serde_json::Value) -> bool {
        self.stats
            .connected
            .load(std::sync::atomic::Ordering::Relaxed)
            && self.presence_out.try_send((at, Some(activity))).is_ok()
    }

    /// Remote devices' live presence beats (entries within the 30s TTL),
    /// device → beat epoch ms.
    pub fn presence(&self) -> HashMap<String, i64> {
        let now = tokio::time::Instant::now();
        lock(&self.presence)
            .iter()
            .filter(|(_, (_, seen))| now.duration_since(*seen) < PRESENCE_TTL)
            .map(|(device, (at, _))| (device.clone(), *at))
            .collect()
    }

    /// Liveness hint: probe the room now (deadline-checked).
    pub fn probe(&self) {
        let _ = self.probe.try_send(());
    }

    /// Escalation: tear the session down and dial a fresh socket.
    pub fn redial(&self) {
        let _ = self.redial.try_send(());
    }

    pub fn stats(&self) -> RoomStatsSnapshot {
        self.stats.snapshot()
    }

    /// Leave cleanly and stop the actor.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for RegistryClient {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[cfg(any(test, feature = "mock-server"))]
pub mod mock_server;
