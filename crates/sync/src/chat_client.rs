//! ChatClient — WebSocket transport for chat2 rooms (docs/chat2-sync.md C1):
//! hello/state handshake with client-side checkpoint precision, cursor-based
//! row backfill, push/ack with a pending-unacked queue, inbound presence
//! relay, probe/redial liveness, and reconnect with exponential backoff.
//!
//! The client owns no CRDT semantics: update bytes flow through a
//! [`ChatDocSink`] the engine implements over its `ChatDocHandle` (import +
//! persist doc AND cursor in one transaction — the C2 rule). Wire frames are
//! the binary chat2 codec ([`crate::chat_frames`]), byte-compatible with
//! `edge/src/chat-frames.ts`.
//!
//! Liveness discipline matches `registry.rs`: transport pings prove nothing
//! about the DO; room health is judged only by protocol frames with probe
//! deadlines.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::chat_frames::{self as wire, frame_type};
use crate::types::{StaticUrl, SyncError, UrlProvider};

mod actor;

use actor::*;

const PING_INTERVAL: Duration = Duration::from_secs(15);
const SILENCE_LEASE: Duration = Duration::from_secs(45);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const HELLO_DEADLINE: Duration = Duration::from_secs(15);
/// Backfill after hello must complete (rowsDone) within this deadline —
/// post-strip rooms are KB-scale, so this is generous even at 1.2 Mbps.
const BACKFILL_DEADLINE: Duration = Duration::from_secs(120);
const PROBE_DEADLINE: Duration = Duration::from_secs(10);
/// Transport pongs/presence do not prove a queued write reached the room.
const PUSH_ACK_DEADLINE: Duration = Duration::from_secs(30);
const BACKOFF_BASE: Duration = Duration::from_millis(250);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// Quiet-room probe cadence default (matches the registry's fleet math).
const PROBE_QUIET_DEFAULT: Duration = Duration::from_secs(900);
/// A checkpoint fetch that hasn't finished by now is treated as a dead link
/// and the session redials (the fetch itself is Range-resumable, so a retry
/// picks up where the bytes stopped). Sized for MAX_CHECKPOINT_BYTES over
/// the 1.2 Mbps links this design exists for.
const CHECKPOINT_FETCH_DEADLINE: Duration = Duration::from_secs(120);
/// Re-push cadence after a `quota` rejection (server window is 60 s; pending
/// batches must not wait for the next enqueue/probe to retry).
const QUOTA_RETRY: Duration = Duration::from_secs(5);
/// Client-side push cap: the DO's per-row cap (`chat-log.ts MAX_ROW_BYTES`,
/// 1 MiB) minus frame-overhead headroom. The headroom matters: the runtime
/// closes WS messages at 1 MiB BEFORE the DO runs, so a payload within a
/// frame-header's width of the row cap would die with no error frame (and no
/// batchId to retire) — the silent replay-forever wedge, again. Enforced at
/// enqueue: a batch the server can never accept must not enter the replay
/// queue.
pub const MAX_PUSH_BYTES: usize = 1024 * 1024 - 4096;
/// Upper bound for a buffered HTTPS pull response. The Edge endpoint itself
/// truncates at 4 MiB; this larger client guard protects against a buggy or
/// incompatible server before frame parsing allocates more state.
const MAX_HTTP_PULL_BYTES: usize = 8 * 1024 * 1024;
const HTTP_SYNC_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-client tuning.
#[derive(Clone, Copy, Debug)]
pub struct ChatTuning {
    pub probe_quiet: Duration,
    /// Bound one HTTPS sync cycle so a stalled request cannot wedge the
    /// single-flight gate forever.
    pub http_timeout: Duration,
    /// How long a sent PUSH may wait for its ACK before the link is treated
    /// as dead (transport pongs do not prove the write reached the room).
    pub push_ack_deadline: Duration,
}

impl Default for ChatTuning {
    fn default() -> Self {
        Self {
            probe_quiet: PROBE_QUIET_DEFAULT,
            http_timeout: HTTP_SYNC_TIMEOUT,
            push_ack_deadline: PUSH_ACK_DEADLINE,
        }
    }
}

/// Connection/sync lifecycle notifications (best-effort broadcast).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatEvent {
    /// Joined (or re-joined); the hello state has been received.
    Connected,
    /// Backfill finished — the doc is converged with the room at this head.
    CaughtUp { head_seq: u64 },
    /// Remote rows/acks were applied through the sink — republish.
    Applied,
    /// The connection dropped; the client is backing off before redialing.
    Disconnected,
    /// A remote device's presence beat arrived.
    Presence,
    /// The server's headSeq is behind our persisted cursor — the room was
    /// reset/wiped. The catch-up treats the cursor as fresh; the HOST should
    /// react by re-seeding via checkpoint (chat-room.ts `/reset` recovery).
    ServerReset,
    /// A queued batch was permanently rejected (or refused at enqueue) and
    /// dropped from the replay queue. The ops remain in the local doc; the
    /// row-path for them is gone, so they reach peers only when THIS device
    /// next posts a checkpoint — the C3 host should treat this event as a
    /// checkpoint trigger, not a shrug.
    PushRejected,
}

// ── engine-facing traits ────────────────────────────────────────────────────

/// Where remote bytes land. The engine implements this over its doc handle;
/// every method persists doc content AND the room cursor in one transaction
/// (`DocsStore::save_snapshot_with_cursor`) so they can never diverge.
///
/// Lock contract. The client never holds its internal state lock while
/// calling a method that touches the document (`acknowledge_outbox`,
/// `advance_cursor`, `apply_row`, `apply_checkpoint`, `load_outbox`,
/// `contains_frontier`, `coalesce_updates`): exporting or importing a Loro
/// doc runs its commit hooks synchronously on the calling thread, and the
/// engine's local-update hook re-enters this client through
/// [`ChatClient::enqueue_update`]. The
/// storage-only outbox methods (`enqueue_outbox`, `update_outbox`) MAY be
/// called under that lock and therefore must never touch the document.
pub trait ChatDocSink: Send + Sync + 'static {
    fn load_outbox(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        Ok(Vec::new())
    }
    fn enqueue_outbox(&self, _batch_id: &str, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
    fn update_outbox(&self, _batch_id: &str, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
    /// One payload carrying exactly the ops of `older` followed by `newer`,
    /// or `None` to keep them as separate batches. The default can only merge
    /// self-contained updates; a sink backed by the live doc should re-export
    /// the combined range ([`coalesce_from_doc`]) so streaming deltas merge.
    fn coalesce_updates(&self, older: &[u8], newer: &[u8]) -> Option<Vec<u8>> {
        merge_loro_updates(&[older.to_vec(), newer.to_vec()])
    }
    /// Persist the current document/cursor BEFORE retiring this exact batch.
    /// A failed commit leaves it pending for retry on either transport.
    fn acknowledge_outbox(&self, _batch_id: &str, cursor: u64) -> Result<(), String> {
        self.advance_cursor(cursor);
        Ok(())
    }
    fn preview(&self) -> Option<Arc<crate::preview_link::PreviewLink>> {
        None
    }
    /// Import one remote update row; `cursor` is the row's seq.
    fn apply_row(&self, bytes: &[u8], cursor: u64);
    /// Replace/merge from a checkpoint blob; `cursor` is its checkpointSeq.
    fn apply_checkpoint(&self, bytes: &[u8], cursor: u64) -> Result<(), String>;
    /// Client-side precision (replaces the server VV diff): is the server
    /// checkpoint's frontier already contained in the local doc?
    fn contains_frontier(&self, frontier: &[u8]) -> bool;
    /// An own-write ack advanced the cursor with no content change.
    fn advance_cursor(&self, cursor: u64);
}

/// `GET /chat2/{chatId}/checkpoint` over HTTP. Implementations should resume
/// partial downloads with `Range: bytes=N-` (the DO serves 206) — that
/// resumability is the point of checkpoint-over-HTTP vs export-per-join.
pub trait CheckpointFetcher: Send + Sync + 'static {
    fn fetch(&self) -> BoxFuture<'static, Result<Vec<u8>, SyncError>>;
}

/// Plain-HTTPS pull/push transport for networks where a WebSocket upgrade is
/// unavailable or slow. Chat pulls use the same binary frames as the socket:
/// a length-prefixed `state`, zero or more `row` frames, and `rowsDone`.
/// Pushes return the JSON acknowledgement body. Implementations must use a
/// fresh bearer per request and treat delivery as at-least-once.
pub trait ChatTransport: Send + Sync + 'static {
    fn fetch_rows(&self, after: u64) -> BoxFuture<'static, Result<Vec<u8>, SyncError>>;
    fn push(
        &self,
        batch_id: String,
        bytes: Vec<u8>,
    ) -> BoxFuture<'static, Result<String, SyncError>>;
}

// ── catch-up planning (pure — the client-side precision rule) ───────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchUpPlan {
    /// Local doc already contains the checkpoint frontier (or there is no
    /// checkpoint): stream rows only.
    RowsOnly { after: u64 },
    /// Fetch + import the checkpoint first, then rows after it.
    CheckpointThenRows { after: u64 },
}

/// Decide the catch-up path from the hello state. `frontier_contained` is the
/// sink's verdict on the checkpoint frontier payload.
pub fn plan_catch_up(
    cursor: u64,
    state: &wire::StateHeader,
    frontier_contained: bool,
) -> CatchUpPlan {
    // A cursor ahead of the server means the server lost state (reset/wipe);
    // our cursor is meaningless there — treat as fresh.
    let cursor = if cursor > state.head_seq { 0 } else { cursor };
    // Presence test is the SIZE, not the seq: a freshly SEEDED room's
    // checkpoint legitimately covers seq 0 (M1 seeds before any rows
    // exist), and seq==0 misread as "no checkpoint" made every adopted
    // reader skip the seed and render an empty transcript.
    if state.checkpoint_size == 0 {
        return CatchUpPlan::RowsOnly { after: cursor };
    }
    if frontier_contained {
        // Rows ≤ checkpointSeq are covered by a checkpoint we already
        // contain — skip straight past them even if our cursor is older.
        CatchUpPlan::RowsOnly {
            after: cursor.max(state.checkpoint_seq),
        }
    } else {
        CatchUpPlan::CheckpointThenRows {
            after: state.checkpoint_seq,
        }
    }
}

// ── transport plumbing (binary sibling of registry.rs's TextPipe) ───────────

pub(crate) struct BinPipe {
    pub(crate) tx: mpsc::Sender<Vec<u8>>,
    pub(crate) rx: mpsc::Receiver<Vec<u8>>,
}

pub(crate) trait BinConnector: Send + Sync + 'static {
    fn connect(&self) -> BoxFuture<'static, Result<BinPipe, SyncError>>;
}

struct WsBinConnector {
    url: Arc<dyn UrlProvider>,
    preview: Option<Arc<crate::preview_link::PreviewLink>>,
    ping_interval: Duration,
}

impl BinConnector for WsBinConnector {
    fn connect(&self) -> BoxFuture<'static, Result<BinPipe, SyncError>> {
        let provider = self.url.clone();
        let preview = self.preview.clone();
        let ping_interval = self.ping_interval;
        Box::pin(async move {
            let url = provider.url().await?;
            use tokio_tungstenite::tungstenite::client::IntoClientRequest;
            let mut request = url
                .as_str()
                .into_client_request()
                .map_err(|_| SyncError::WebSocket("invalid socket request".into()))?;
            if let Some(preview) = preview {
                request.headers_mut().insert(
                    "x-cypher-preview-capability",
                    crate::stream_preview::CAPABILITY.parse().unwrap(),
                );
                if let Some(token) = &preview.options().publisher_token {
                    let mut value: tokio_tungstenite::tungstenite::http::HeaderValue = token
                        .parse()
                        .map_err(|_| SyncError::Auth("invalid preview credential".into()))?;
                    value.set_sensitive(true);
                    request
                        .headers_mut()
                        .insert("x-cypher-preview-publisher", value);
                }
            }
            let ws = crate::dial::connect_request(request)
                .await
                .map_err(|e| SyncError::WebSocket(e.to_string()))?;
            let (out_tx, out_rx) = mpsc::channel(64);
            let (in_tx, in_rx) = mpsc::channel(64);
            tokio::spawn(pump(ws, out_rx, in_tx, ping_interval));
            Ok(BinPipe {
                tx: out_tx,
                rx: in_rx,
            })
        })
    }
}

/// Shuttle binary frames between the WebSocket and the actor's channels; the
/// text `"ping"` keepalive rides the same socket (runtime-answered pair).
async fn pump(
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    mut out_rx: mpsc::Receiver<Vec<u8>>,
    in_tx: mpsc::Sender<Vec<u8>>,
    ping_interval: Duration,
) {
    let (mut sink, mut stream) = ws.split();
    let mut ping = tokio::time::interval(ping_interval);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;
    let mut last_rx = tokio::time::Instant::now();
    loop {
        tokio::select! {
            frame = out_rx.recv() => match frame {
                Some(bytes) => {
                    if sink.send(WsMessage::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                None => {
                    let _ = sink.send(WsMessage::Close(None)).await;
                    break;
                }
            },
            frame = stream.next() => match frame {
                Some(Ok(WsMessage::Binary(bytes))) => {
                    last_rx = tokio::time::Instant::now();
                    if in_tx.send(bytes.to_vec()).await.is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {
                    // Text pong / control frames: transport liveness only.
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
                tracing::warn!("chat2 socket silent past lease; treating as dead");
                break;
            }
        }
    }
}

// ── shared client state ─────────────────────────────────────────────────────

struct PendingPush {
    batch_id: String,
    bytes: Vec<u8>,
    persisted: bool,
    sent: bool,
    /// Held for the next push rather than opening one: see
    /// [`ChatClient::enqueue_deferred_update`].
    deferred: bool,
}

/// Make every held batch due: the push about to happen carries them.
fn release_deferred(shared: &mut Shared) {
    for push in shared.pending.iter_mut() {
        push.deferred = false;
    }
}

fn merge_loro_updates(updates: &[Vec<u8>]) -> Option<Vec<u8>> {
    if updates.len() < 2 {
        return None;
    }
    let doc = loro::LoroDoc::new();
    for update in updates {
        doc.import(update).ok()?;
    }
    // A successful import can park operations with missing causal history.
    // Exporting that empty/partial document would silently discard them.
    // Only coalesce when every input range is actually in the oplog.
    let vv = doc.oplog_vv();
    for (index, update) in updates.iter().enumerate() {
        let meta = loro::LoroDoc::decode_import_blob_meta(update, true).ok()?;
        if index == 0 && !meta.start_frontiers.is_empty() {
            return None;
        }
        if !vv.includes_vv(&meta.partial_start_vv) || !vv.includes_vv(&meta.partial_end_vv) {
            return None;
        }
    }
    doc.export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .ok()
        .filter(|bytes| bytes.len() <= MAX_PUSH_BYTES)
}

/// Merge two consecutive update blobs by re-exporting their combined op range
/// from `doc`, which must already contain both. Unlike [`merge_loro_updates`]
/// this works for dependent deltas — every streaming commit in a chat that
/// already has content — because the ops' causal history stays in `doc`.
/// `None` when the ranges are not contiguous per peer (ops between them
/// belong to another batch) or `doc` lacks any of them.
pub fn coalesce_from_doc(doc: &loro::LoroDoc, older: &[u8], newer: &[u8]) -> Option<Vec<u8>> {
    let older = loro::LoroDoc::decode_import_blob_meta(older, false).ok()?;
    let newer = loro::LoroDoc::decode_import_blob_meta(newer, false).ok()?;
    let range = |meta: &loro::ImportBlobMetadata, peer: &loro::PeerID| {
        let end = meta.partial_end_vv.get(peer).copied().unwrap_or(0);
        let start = meta.partial_start_vv.get(peer).copied().unwrap_or(0);
        (end > start).then_some((start, end))
    };
    let mut peers: Vec<loro::PeerID> = older
        .partial_end_vv
        .keys()
        .chain(newer.partial_end_vv.keys())
        .copied()
        .collect();
    peers.sort_unstable();
    peers.dedup();
    let have = doc.oplog_vv();
    let mut spans = Vec::with_capacity(peers.len());
    for peer in peers {
        let (start, end) = match (range(&older, &peer), range(&newer, &peer)) {
            (Some(a), Some(b)) if a.1 >= b.0 && b.1 >= a.0 => (a.0.min(b.0), a.1.max(b.1)),
            (Some(_), Some(_)) => return None,
            (Some(r), None) | (None, Some(r)) => r,
            (None, None) => continue,
        };
        if have.get(&peer).copied().unwrap_or(0) < end {
            return None;
        }
        spans.push(loro::IdSpan::new(peer, start, end));
    }
    if spans.is_empty() {
        return None;
    }
    doc.export(loro::ExportMode::updates_in_range(spans))
        .ok()
        .filter(|bytes| bytes.len() <= MAX_PUSH_BYTES)
}

#[derive(Default)]
struct Shared {
    cursor: u64,
    pending: VecDeque<PendingPush>,
    in_flight: Option<(String, tokio::time::Instant)>,
    /// Last hello/probe view of the server log (checkpoint-policy inputs).
    server: Option<wire::StateHeader>,
    /// Set by a transient (`quota`) rejection: re-push at this instant
    /// instead of waiting for the next enqueue/probe/reconnect.
    retry_at: Option<tokio::time::Instant>,
    /// True while draining a quota-rejected queue. Retry ticks then probe
    /// with the HEAD batch only (a full-queue replay would itself consume
    /// the server's quota window — N pending × 12 ticks/window livelocks
    /// past N≈25), and each ack immediately re-arms the clock until the
    /// queue empties.
    quota_blocked: bool,
    /// A row or acknowledgement arrived beyond `cursor + 1`. The cursor is
    /// a claim that every row up to it is reflected in the local doc, so a
    /// gap must be repaired rather than skipped.
    gap_repair: bool,
    flush_at: Option<tokio::time::Instant>,
    force_flush: bool,
}

impl Shared {
    fn acknowledge(&mut self, batch_id: &str) {
        if self
            .in_flight
            .as_ref()
            .is_some_and(|(id, _)| id == batch_id)
        {
            self.in_flight = None;
        }
        self.pending.retain(|p| p.batch_id != batch_id);
        if self.pending.is_empty() {
            self.force_flush = false;
            self.flush_at = None;
        } else if self.force_flush {
            self.retry_at = Some(tokio::time::Instant::now());
        }
        if self.quota_blocked {
            if self.pending.is_empty() {
                self.quota_blocked = false;
                self.retry_at = None;
            } else {
                self.retry_at = Some(tokio::time::Instant::now());
            }
        }
    }
}

/// Retire `batch_id` after the server acknowledged it at `seq`, persisting
/// the cursor through the sink FIRST (a failed persist leaves the batch
/// pending for retry on either transport).
///
/// The sink runs with `shared` UNLOCKED. The engine's sink exports the Loro
/// document to persist it, and a Loro export runs the doc's commit hooks
/// synchronously on this thread — including the local-update feed that ends
/// in [`ChatClient::enqueue_update`], which takes `shared` itself. Holding
/// the lock across that call was a self-deadlock, and because Loro parks
/// every other thread committing to the same doc behind the running hook,
/// it froze the agent run too and then the whole runtime. The
/// final phase re-derives against the live state: a row may have advanced
/// the cursor while the sink ran, and the cursor only ever moves forward.
fn acknowledge_durable(
    shared: &Mutex<Shared>,
    sink: &dyn ChatDocSink,
    batch_id: &str,
    seq: u64,
) -> Result<(), String> {
    let cursor = {
        let shared = lock(shared);
        let Some(push) = shared.pending.iter().find(|p| p.batch_id == batch_id) else {
            return Ok(());
        };
        if !push.persisted || !push.sent {
            return Err("ACK for an unsent/unpersisted batch".into());
        }
        if seq == 0 {
            return Err("invalid ACK sequence".into());
        }
        if seq <= shared.cursor.saturating_add(1) {
            shared.cursor.max(seq)
        } else {
            shared.cursor
        }
    };
    sink.acknowledge_outbox(batch_id, cursor)?;
    let mut shared = lock(shared);
    shared.gap_repair |= seq > shared.cursor.saturating_add(1);
    if seq <= shared.cursor.saturating_add(1) {
        shared.cursor = shared.cursor.max(seq);
    }
    shared.acknowledge(batch_id);
    Ok(())
}

/// `cypher sync` surface (plan: cursor / headSeq / floorLag / pendingPushes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChatStatsSnapshot {
    pub connected: bool,
    /// A state response was received from the server (WS hello or HTTPS pull).
    /// Until this is true, server counters are only local zero-value
    /// placeholders and consumers must not make recovery decisions from them.
    pub server_known: bool,
    pub cursor: u64,
    pub head_seq: u64,
    pub seq_floor: u64,
    pub checkpoint_seq: u64,
    /// Byte size of the room's stored checkpoint (0 = none). The host's
    /// bootstrap heal keys off this: a room with rows but NO checkpoint
    /// cannot cover its rows' causal deps for cold readers.
    pub checkpoint_size: u64,
    pub row_count: u64,
    pub row_bytes: u64,
    pub pending_pushes: u64,
    pub rejoins: u64,
    pub disconnects: u64,
    pub rejected: u64,
    /// Times a hello found the server behind our cursor (room reset/wiped).
    /// Nonzero means the host owes the room a re-seed checkpoint.
    pub server_resets: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ── the client ──────────────────────────────────────────────────────────────

/// A live chat2-room membership for one chat doc.
pub struct ChatClient {
    sink: Arc<dyn ChatDocSink>,
    shared: Arc<Mutex<Shared>>,
    events: broadcast::Sender<ChatEvent>,
    shutdown: watch::Sender<bool>,
    nudge: mpsc::Sender<()>,
    probe: mpsc::Sender<()>,
    redial: mpsc::Sender<()>,
    flags: Arc<Flags>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Default)]
struct Flags {
    connected: std::sync::atomic::AtomicBool,
    rejoins: std::sync::atomic::AtomicU64,
    disconnects: std::sync::atomic::AtomicU64,
    rejected: std::sync::atomic::AtomicU64,
    server_resets: std::sync::atomic::AtomicU64,
}

impl ChatClient {
    /// Connect (fixed URL — dev/tests).
    pub async fn connect(
        url: &str,
        sink: Arc<dyn ChatDocSink>,
        fetcher: Arc<dyn CheckpointFetcher>,
        device_id: &str,
        initial_cursor: u64,
    ) -> Result<Self, SyncError> {
        Self::connect_via(
            Arc::new(StaticUrl(url.to_string())),
            sink,
            fetcher,
            device_id,
            initial_cursor,
        )
        .await
    }

    /// Connect with a per-dial URL provider (fresh `?token=` every attempt).
    /// Resolves once hello/state lands AND the initial catch-up (checkpoint
    /// if needed + row backfill) completes; first-attempt failures are `Err`
    /// (callers own the initial-join retry). After that it reconnects itself.
    pub async fn connect_via(
        provider: Arc<dyn UrlProvider>,
        sink: Arc<dyn ChatDocSink>,
        fetcher: Arc<dyn CheckpointFetcher>,
        device_id: &str,
        initial_cursor: u64,
    ) -> Result<Self, SyncError> {
        let connector = Arc::new(WsBinConnector {
            url: provider,
            preview: sink.preview(),
            ping_interval: PING_INTERVAL,
        });
        Self::connect_with_tuned(
            connector,
            sink,
            fetcher,
            device_id,
            initial_cursor,
            ChatTuning::default(),
        )
        .await
    }

    /// Start a local-first membership with HTTPS pull/push alongside the
    /// WebSocket. The returned client is ready immediately; the HTTP worker
    /// converges the document while the socket attempts its normal join.
    pub async fn connect_via_transport(
        provider: Arc<dyn UrlProvider>,
        sink: Arc<dyn ChatDocSink>,
        fetcher: Arc<dyn CheckpointFetcher>,
        device_id: &str,
        initial_cursor: u64,
        transport: Arc<dyn ChatTransport>,
    ) -> Result<Self, SyncError> {
        let connector = Arc::new(WsBinConnector {
            url: provider,
            preview: sink.preview(),
            ping_interval: PING_INTERVAL,
        });
        Self::connect_with_transport(
            connector,
            sink,
            fetcher,
            device_id,
            initial_cursor,
            ChatTuning::default(),
            Some(transport),
        )
        .await
    }

    pub(crate) async fn connect_with_tuned(
        connector: Arc<dyn BinConnector>,
        sink: Arc<dyn ChatDocSink>,
        fetcher: Arc<dyn CheckpointFetcher>,
        device_id: &str,
        initial_cursor: u64,
        tuning: ChatTuning,
    ) -> Result<Self, SyncError> {
        Self::connect_with_transport(
            connector,
            sink,
            fetcher,
            device_id,
            initial_cursor,
            tuning,
            None,
        )
        .await
    }

    pub(crate) async fn connect_with_transport(
        connector: Arc<dyn BinConnector>,
        sink: Arc<dyn ChatDocSink>,
        fetcher: Arc<dyn CheckpointFetcher>,
        device_id: &str,
        initial_cursor: u64,
        tuning: ChatTuning,
        transport: Option<Arc<dyn ChatTransport>>,
    ) -> Result<Self, SyncError> {
        let (events, _) = broadcast::channel(256);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = oneshot::channel();
        let (nudge_tx, nudge_rx) = mpsc::channel(1);
        let (probe_tx, probe_rx) = mpsc::channel(1);
        let (redial_tx, redial_rx) = mpsc::channel(1);
        let (sync_tx, sync_rx) = mpsc::channel(1);
        let mut restored = Shared {
            cursor: initial_cursor,
            ..Shared::default()
        };
        restored.pending.extend(
            sink.load_outbox()
                .map_err(SyncError::Protocol)?
                .into_iter()
                .map(|(batch_id, bytes)| PendingPush {
                    batch_id,
                    bytes,
                    persisted: true,
                    // The server may have accepted this batch before a crash.
                    // Never change its payload under the restored batch ID.
                    sent: true,
                    deferred: false,
                }),
        );
        let shared = Arc::new(Mutex::new(restored));
        let flags = Arc::new(Flags::default());

        let sink_for_client = sink.clone();
        let actor = Actor {
            shared: shared.clone(),
            sink,
            fetcher,
            device_id: device_id.to_string(),
            connector,
            tuning,
            events: events.clone(),
            shutdown: shutdown_rx,
            nudge_rx,
            probe_rx,
            redial_rx,
            flags: flags.clone(),
            resumed: false,
            cursor_amnesty_done: std::sync::atomic::AtomicBool::new(false),
            transport,
            http_sync_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_sync_again: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_sync_tx: sync_tx.clone(),
            http_sync_rx: sync_rx,
        };
        let task = tokio::spawn(actor.run(ready_tx));

        match ready_rx.await {
            Ok(Ok(())) => Ok(Self {
                sink: sink_for_client,
                shared,
                events,
                shutdown: shutdown_tx,
                nudge: nudge_tx,
                probe: probe_tx,
                redial: redial_tx,
                flags,
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

    pub fn events(&self) -> broadcast::Receiver<ChatEvent> {
        self.events.subscribe()
    }

    /// Queue one local update batch for push (a fresh batch id is minted; the
    /// batch survives reconnects until acked — the server dedupes replays).
    ///
    /// Batches over [`MAX_PUSH_BYTES`] are refused here: the server can never
    /// accept them (`MAX_ROW_BYTES`), and a queued-forever batch would replay
    /// on every reconnect — the exact wedge class chat2 replaces. The ops
    /// stay in the local doc and reach peers via the next checkpoint.
    pub fn enqueue_update(&self, bytes: Vec<u8>) {
        self.enqueue(bytes, false);
    }

    /// Queue an update that does not justify a push of its own (a commit
    /// that only grew the model's thinking). It is stored in the outbox like
    /// any batch, but rides along with the next push: any
    /// [`Self::enqueue_update`] releases it, as does [`Self::flush_pending`]
    /// at a segment boundary. Thinking then costs no durable writes beyond
    /// the pushes a turn makes anyway.
    pub fn enqueue_deferred_update(&self, bytes: Vec<u8>) {
        self.enqueue(bytes, true);
    }

    fn enqueue(&self, bytes: Vec<u8>, deferred: bool) {
        if bytes.len() > MAX_PUSH_BYTES {
            use std::sync::atomic::Ordering::Relaxed;
            tracing::error!(
                bytes = bytes.len(),
                "chat2: update exceeds the row cap; not queued (post-strip \
                 updates are KB-scale — this is an upstream bug)"
            );
            self.flags.rejected.fetch_add(1, Relaxed);
            let _ = self.events.send(ChatEvent::PushRejected);
            return;
        }
        // Coalesce into the queue's unsent tail — also while the head is in
        // flight, so the next push carries everything produced during one
        // ACK round trip. Without this, every 120 ms stream commit became its
        // own row, drained one per round trip, and peers fell further behind
        // for as long as the model kept streaming. A sent batch is immutable.
        let tail = lock(&self.shared)
            .pending
            .back()
            .filter(|last| last.persisted && !last.sent)
            .map(|last| (last.batch_id.clone(), last.bytes.clone()));
        // Merging may export from the doc, so it runs outside `shared` (see
        // the `ChatDocSink` lock contract); the actor may send the tail
        // meanwhile, hence the re-check before replacing its payload.
        if let Some((batch_id, old)) = tail
            && let Some(merged) = self.sink.coalesce_updates(&old, &bytes)
        {
            let mut shared = lock(&self.shared);
            if let Some(last) = shared.pending.back_mut()
                && last.batch_id == batch_id
                && !last.sent
                && last.bytes == old
                && self.sink.update_outbox(&batch_id, &merged).is_ok()
            {
                last.bytes = merged;
                if deferred {
                    return;
                }
                release_deferred(&mut shared);
                shared
                    .flush_at
                    .get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_secs(2));
                drop(shared);
                let _ = self.nudge.try_send(());
                return;
            }
        }
        {
            // The outbox writes below run under `shared`. That is allowed
            // only because they are storage-only (see the `ChatDocSink`
            // lock contract): nothing here may export or import the doc.
            let mut shared = lock(&self.shared);
            let batch_id = uuid::Uuid::new_v4().to_string();
            let persisted = self.sink.enqueue_outbox(&batch_id, &bytes).is_ok();
            if !persisted {
                tracing::error!("chat2: local outbox write failed; holding update, not sending");
                shared.retry_at = Some(tokio::time::Instant::now() + Duration::from_secs(1));
                let _ = self.events.send(ChatEvent::PushRejected);
            }
            if !deferred {
                // Batches stay in order: whatever was held goes out first.
                release_deferred(&mut shared);
            }
            shared.pending.push_back(PendingPush {
                batch_id,
                bytes,
                persisted,
                sent: false,
                deferred,
            });
            if deferred {
                return;
            }
            shared
                .flush_at
                .get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_secs(2));
        }
        let _ = self.nudge.try_send(());
    }

    /// Force the currently queued durable batch now (Run/Steer/Interrupt/
    /// completion boundaries will use this hook). Does not alter local docs.
    pub fn flush_pending(&self) {
        let mut shared = lock(&self.shared);
        release_deferred(&mut shared);
        shared.force_flush = true;
        drop(shared);
        let _ = self.nudge.try_send(());
    }

    /// Liveness hint: probe the room now (deadline-checked).
    pub fn probe(&self) {
        let _ = self.probe.try_send(());
    }

    /// Escalation: tear the session down and dial a fresh socket.
    pub fn redial(&self) {
        let _ = self.redial.try_send(());
    }

    /// The host posted a checkpoint covering `seq_covered` (C3 policy):
    /// fold it into the cached server view so the thresholds don't re-trip
    /// on stale hello-time numbers every quiesce tick (the DO doesn't
    /// broadcast state after a checkpoint commit).
    pub fn note_checkpoint(&self, seq_covered: u64, size: u64) {
        let mut shared = lock(&self.shared);
        if let Some(server) = &mut shared.server {
            server.checkpoint_seq = seq_covered;
            server.checkpoint_size = size;
            server.seq_floor = seq_covered;
            server.row_count = 0;
            server.row_bytes = 0;
        }
    }

    pub fn stats(&self) -> ChatStatsSnapshot {
        use std::sync::atomic::Ordering::Relaxed;
        let shared = lock(&self.shared);
        let server = shared.server.unwrap_or(wire::StateHeader {
            head_seq: 0,
            seq_floor: 0,
            checkpoint_seq: 0,
            checkpoint_size: 0,
            row_count: 0,
            row_bytes: 0,
        });
        ChatStatsSnapshot {
            connected: self.flags.connected.load(Relaxed),
            server_known: shared.server.is_some(),
            cursor: shared.cursor,
            // The server's honest view — deliberately NOT clamped to the
            // cursor: cursor > headSeq is the reset signal and must stay
            // visible to the observability surface, not be masked by it.
            head_seq: server.head_seq,
            seq_floor: server.seq_floor,
            checkpoint_seq: server.checkpoint_seq,
            checkpoint_size: server.checkpoint_size,
            row_count: server.row_count,
            row_bytes: server.row_bytes,
            pending_pushes: shared.pending.len() as u64,
            rejoins: self.flags.rejoins.load(Relaxed),
            disconnects: self.flags.disconnects.load(Relaxed),
            rejected: self.flags.rejected.load(Relaxed),
            server_resets: self.flags.server_resets.load(Relaxed),
        }
    }

    /// Leave cleanly and stop the actor.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for ChatClient {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests;
