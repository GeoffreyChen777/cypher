//! The client end: links to peer devices' relays, dialed lazily and cached
//! per device id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::frames::{
    DeviceFrameHeader, decode_device_frame, encode_device_frame, relay_error_code,
};
use super::{
    PING_INTERVAL, RELAY_KIND, RPC_KIND, SILENCE_LEASE, TokenSource, device_room_ws_url,
    token_changed,
};
use crate::{RpcClient, RpcError, lock};

/// In-call dial retries (see [`LinkCache::client`]): total attempts, and the
/// spacing multiplier between them (1.5s, then 3s — ~4.5s of cover, several
/// times the host's post-eviction rejoin window).
const DIAL_ATTEMPTS: u32 = 3;

const DIAL_RETRY_SPACING: Duration = Duration::from_millis(1500);

/// The client end: one WebSocket to a peer device's relay carrying a single RPC stream,
/// exposed as an ordinary [`RpcClient`]. `host_offline` / `host_closed` relay frames (and
/// socket drops) mark the link down — in-flight calls fail with [`RpcError::Closed`] and
/// [`LinkCache`] evicts the entry so the next call re-dials.
pub struct DeviceLink {
    client: Arc<RpcClient>,
    closed_rx: watch::Receiver<Option<String>>,
    pump: tokio::task::JoinHandle<()>,
}

impl DeviceLink {
    pub async fn connect(url: &str) -> Result<Self, RpcError> {
        let ws = cypher_net::dial::connect_ws(url)
            .await
            .map_err(|e| RpcError::Transport(format!("device room unreachable: {e}")))?;
        let (mut sink, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
        let (in_tx, in_rx) = mpsc::channel::<String>(256);
        let (closed_tx, closed_rx) = watch::channel::<Option<String>>(None);

        let pump = tokio::spawn(async move {
            let mut ping = tokio::time::interval(PING_INTERVAL);
            ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ping.tick().await; // consume the immediate first tick
            let mut last_rx = tokio::time::Instant::now();
            let reason = loop {
                tokio::select! {
                    frame = out_rx.recv() => match frame {
                        Some(text) => {
                            let header = DeviceFrameHeader::new(RPC_KIND, RPC_KIND);
                            let encoded = match encode_device_frame(&header, text.as_bytes()) {
                                Ok(bytes) => bytes,
                                Err(err) => {
                                    tracing::error!(error = %err, "device-room: frame encode failed");
                                    continue;
                                }
                            };
                            if sink.send(WsMessage::Binary(encoded)).await.is_err() {
                                break "connection lost".to_string();
                            }
                        }
                        None => {
                            let _ = sink.send(WsMessage::Close(None)).await;
                            break "closed".to_string();
                        }
                    },
                    message = stream.next() => match message {
                        Some(Ok(WsMessage::Binary(bytes))) => {
                            last_rx = tokio::time::Instant::now();
                            match decode_device_frame(&bytes) {
                                Ok((header, payload)) if header.k == RELAY_KIND => {
                                    // host_offline / host_closed: surface as link-down.
                                    let code = relay_error_code(&payload)
                                        .unwrap_or_else(|| "relay error".into());
                                    tracing::info!(%code, "device-room: link down");
                                    break code;
                                }
                                Ok((header, payload)) if header.k == RPC_KIND => {
                                    let text = String::from_utf8_lossy(&payload).into_owned();
                                    if in_tx.send(text).await.is_err() {
                                        break "client dropped".to_string();
                                    }
                                }
                                Ok(_) => {}
                                Err(err) => {
                                    tracing::warn!(error = %err, "device-room: malformed frame");
                                }
                            }
                        }
                        Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => {
                            break "connection lost".to_string();
                        }
                        // Text "pong" / control frames: proof of life for the lease.
                        Some(Ok(_)) => last_rx = tokio::time::Instant::now(),
                    },
                    _ = ping.tick() => {
                        if sink.send(WsMessage::Text("ping".into())).await.is_err() {
                            break "connection lost".to_string();
                        }
                    }
                    _ = tokio::time::sleep_until(last_rx + SILENCE_LEASE) => {
                        break "silent past lease".to_string();
                    }
                }
            };
            // Dropping in_tx ends the RpcClient reader → pending calls fail Closed.
            let _ = closed_tx.send(Some(reason));
        });

        Ok(Self {
            client: Arc::new(RpcClient::new(out_tx, in_rx)),
            closed_rx,
            pump,
        })
    }

    pub fn client(&self) -> Arc<RpcClient> {
        self.client.clone()
    }

    pub fn is_closed(&self) -> bool {
        self.closed_rx.borrow().is_some()
    }

    /// Watch that resolves to `Some(reason)` when the link drops.
    pub fn closed(&self) -> watch::Receiver<Option<String>> {
        self.closed_rx.clone()
    }
}

impl Drop for DeviceLink {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

pub struct LinkCacheConfig {
    pub edge_url: String,
    pub token: Arc<dyn TokenSource>,
    /// Exponential dial cooldown after failures (base, cap) — a dead peer must not be
    /// redialed at full cadence; callers fail fast in between (zeron peers.ts behavior).
    pub cooldown_base: Duration,
    pub cooldown_max: Duration,
    /// Readiness probe budget: the relay accepts client joins even when the host is
    /// offline, so a `ListHarnesses` round-trip proves the path before caching.
    pub probe_timeout: Duration,
}

impl LinkCacheConfig {
    pub fn new(edge_url: impl Into<String>, token: Arc<dyn TokenSource>) -> Self {
        // Interactive remote control (remote folders, terminals, accounts) rides
        // this cache: one blip must cost seconds, not minutes. The old zeron
        // 15s→5min curve punished a single failed dial with a 5-minute refusal;
        // here the first failure backs off 5s and even a dead peer is re-probed
        // within a minute. A generous probe budget keeps a slow-waking laptop
        // (radio up, engine still thawing) from counting as a failure.
        Self {
            edge_url: edge_url.into(),
            token,
            cooldown_base: Duration::from_secs(5),
            cooldown_max: Duration::from_secs(60),
            probe_timeout: Duration::from_secs(10),
        }
    }
}

/// Consecutive failures older than this decay to zero — a blip an hour ago must
/// not escalate today's first retry up the backoff curve.
const FAILURE_DECAY: Duration = Duration::from_secs(600);

#[derive(Default)]
struct DialState {
    failures: u32,
    last_failure: Option<Instant>,
    cooldown_until: Option<Instant>,
}

/// Lazily-dialed, cached peer links keyed by device id — the Rust twin of zeron's
/// `Peers`. Cache hits never wait behind an in-flight dial; dials to the same device are
/// serialized per device (a global lock would head-of-line-block healthy peers); links
/// self-evict when the transport drops; a failed RPC should call [`LinkCache::invalidate`]
/// so the next call re-dials.
pub struct LinkCache {
    config: LinkCacheConfig,
    revoked: AtomicBool,
    links: Mutex<HashMap<String, Arc<DeviceLink>>>,
    dial_state: Mutex<HashMap<String, DialState>>,
    dial_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl LinkCache {
    /// Secret-bearing control requests require TLS, except for a loopback
    /// relay used by local development/tests. This is transport encryption,
    /// not E2EE: the authenticated relay remains part of the trust boundary.
    pub fn credential_transport_allowed(&self) -> bool {
        credential_transport_allowed(&self.config.edge_url)
    }

    pub fn new(config: LinkCacheConfig) -> Arc<Self> {
        let cache = Arc::new(Self {
            config,
            revoked: AtomicBool::new(false),
            links: Mutex::new(HashMap::new()),
            dial_state: Mutex::new(HashMap::new()),
            dial_locks: Mutex::new(HashMap::new()),
        });
        // Wake = every cached link is half-open and every cooldown is moot
        // (the failures belonged to the pre-suspend network). Drop them so the
        // next call redials immediately with fresh credentials. Skipped
        // outside a runtime (sync unit tests).
        if tokio::runtime::Handle::try_current().is_ok() {
            let weak = Arc::downgrade(&cache);
            tokio::spawn(async move {
                let mut wake = cypher_net::wake::subscribe();
                let mut token_changes = weak
                    .upgrade()
                    .and_then(|cache| cache.config.token.subscribe());
                loop {
                    tokio::select! {
                        result = wake.recv() => {
                            if result.is_err() { return; }
                            let Some(cache) = weak.upgrade() else { return };
                            lock(&cache.links).clear();
                            lock(&cache.dial_state).clear();
                            tracing::info!("peer: links + cooldowns cleared after wake");
                        }
                        _ = token_changed(&mut token_changes) => {
                            let Some(cache) = weak.upgrade() else { return };
                            let signed_out = cache.config.token.token().await.is_none();
                            if signed_out {
                                // Cached clients were authenticated when their sockets
                                // opened. Revocation must close them even though the
                                // server has not independently reaped those sockets yet.
                                cache.revoked.store(true, Ordering::Release);
                                lock(&cache.links).clear();
                            } else {
                                cache.revoked.store(false, Ordering::Release);
                            }
                            lock(&cache.dial_state).clear();
                            if signed_out {
                                tracing::info!("peer: credentials removed; links closed");
                            } else {
                                tracing::info!("peer: dial cooldowns cleared after token refresh");
                            }
                        }
                    }
                }
            });
        }
        cache
    }

    /// A live `RpcClient` to `device_id`'s engine (dialed + cached on first use).
    /// Transient dial failures retry in place a couple of times before
    /// surfacing: the host relay's DO periodically ends its session and
    /// rejoins within ~a second, and a user-facing call landing in that
    /// window should ride over it, not error (user report: refs/folders
    /// "unstable" vs the old app).
    pub async fn client(self: &Arc<Self>, device_id: &str) -> Result<Arc<RpcClient>, RpcError> {
        if self.revoked.load(Ordering::Acquire) {
            return Err(RpcError::Transport("not signed in".into()));
        }
        // Fast path outside any lock.
        if let Some(link) = self.cached(device_id) {
            return Ok(link.client());
        }
        let dial_lock = {
            let mut locks = lock(&self.dial_locks);
            // The map only ever grew (one entry per peer ever dialed); prune
            // idle locks once it's clearly beyond any real org's device count.
            if locks.len() > 64 {
                locks.retain(|_, l| Arc::strong_count(l) > 1);
            }
            locks.entry(device_id.to_string()).or_default().clone()
        };
        let _guard = dial_lock.lock().await;
        // Re-check under the per-device lock: a concurrent dial may have won.
        if let Some(link) = self.cached(device_id) {
            return Ok(link.client());
        }
        if let Some(message) = self.cooling(device_id) {
            return Err(RpcError::Transport(message));
        }
        let mut last_err = None;
        for attempt in 0..DIAL_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(DIAL_RETRY_SPACING * attempt).await;
            }
            match self.dial(device_id).await {
                Ok(link) => {
                    lock(&self.dial_state).remove(device_id);
                    let mut links = lock(&self.links);
                    if self.revoked.load(Ordering::Acquire) {
                        return Err(RpcError::Transport("not signed in".into()));
                    }
                    links.insert(device_id.to_string(), link.clone());
                    drop(links);
                    self.spawn_evictor(device_id.to_string(), &link);
                    tracing::info!(device = %device_id, "peer: connected via device room");
                    return Ok(link.client());
                }
                Err(err) => {
                    tracing::debug!(device = %device_id, attempt, error = %err, "peer: dial attempt failed");
                    last_err = Some(err);
                }
            }
        }
        // Only the exhausted sequence counts as ONE failure on the cooldown
        // curve — the in-call retries must not escalate it by themselves.
        self.note_failure(device_id);
        Err(last_err.unwrap_or(RpcError::Closed))
    }

    /// Drop a cached link after a failed RPC so the next call re-dials.
    pub fn invalidate(&self, device_id: &str) {
        lock(&self.links).remove(device_id);
    }

    /// Close every authenticated peer socket. Future dials still consult the
    /// live token provider and therefore remain disabled while signed out.
    pub fn disconnect_all(&self) {
        self.revoked.store(true, Ordering::Release);
        lock(&self.links).clear();
        lock(&self.dial_state).clear();
    }

    /// Data-driven cooldown reset: called when out-of-band evidence says the
    /// peer is alive again (fresh workspace presence heartbeat). The next call
    /// dials immediately instead of waiting out the backoff window.
    pub fn reset_cooldown(&self, device_id: &str) {
        if lock(&self.dial_state).remove(device_id).is_some() {
            tracing::info!(device = %device_id, "peer: cooldown cleared (peer is alive)");
        }
    }

    fn cached(&self, device_id: &str) -> Option<Arc<DeviceLink>> {
        let mut links = lock(&self.links);
        match links.get(device_id) {
            Some(link) if !link.is_closed() => Some(link.clone()),
            Some(_) => {
                links.remove(device_id);
                None
            }
            None => None,
        }
    }

    fn cooling(&self, device_id: &str) -> Option<String> {
        let state = lock(&self.dial_state);
        let entry = state.get(device_id)?;
        let until = entry.cooldown_until?;
        let now = Instant::now();
        if now >= until {
            return None;
        }
        Some(format!(
            "peer {device_id}: unreachable (backing off after {} failed dials; retrying in ~{}s)",
            entry.failures,
            (until - now).as_secs().max(1)
        ))
    }

    fn note_failure(&self, device_id: &str) {
        let mut state = lock(&self.dial_state);
        let entry = state.entry(device_id.to_string()).or_default();
        // Stale streaks restart the curve rather than escalating it.
        if entry
            .last_failure
            .is_some_and(|at| at.elapsed() > FAILURE_DECAY)
        {
            entry.failures = 0;
        }
        entry.last_failure = Some(Instant::now());
        entry.failures += 1;
        let backoff = self
            .config
            .cooldown_base
            .saturating_mul(1u32 << (entry.failures - 1).min(16))
            .min(self.config.cooldown_max);
        entry.cooldown_until = Some(Instant::now() + backoff);
    }

    async fn dial(&self, device_id: &str) -> Result<Arc<DeviceLink>, RpcError> {
        // Fresh token on every attempt — an expired one is never reused.
        let token = self
            .config
            .token
            .token()
            .await
            .ok_or_else(|| RpcError::Transport("not signed in".into()))?;
        let conn_id = uuid::Uuid::new_v4().to_string();
        let url = device_room_ws_url(
            &self.config.edge_url,
            device_id,
            "client",
            Some(&conn_id),
            &token,
        );
        tracing::info!(device = %device_id, "peer: dialing via device room");
        let link = Arc::new(DeviceLink::connect(&url).await?);
        // Readiness probe: prove the host answers before caching (an offline host bounces
        // host_offline, which closes the link and fails this call fast).
        let client = link.client();
        let probe = client.call(crate::methods::LIST_HARNESSES, serde_json::json!({}));
        tokio::time::timeout(self.config.probe_timeout, probe)
            .await
            .map_err(|_| {
                RpcError::Transport(format!("peer {device_id}: readiness check timed out"))
            })?
            .map_err(|e| {
                RpcError::Transport(format!("peer {device_id}: readiness check failed: {e}"))
            })?;
        Ok(link)
    }

    fn spawn_evictor(self: &Arc<Self>, device_id: String, link: &Arc<DeviceLink>) {
        let mut closed = link.closed();
        let cache = Arc::downgrade(self);
        let link_ptr = Arc::as_ptr(link) as usize;
        tokio::spawn(async move {
            loop {
                if closed.borrow().is_some() {
                    break;
                }
                if closed.changed().await.is_err() {
                    break;
                }
            }
            let Some(cache) = cache.upgrade() else { return };
            let mut links = lock(&cache.links);
            // Evict only if this exact link is still the cached one.
            if links
                .get(&device_id)
                .is_some_and(|l| Arc::as_ptr(l) as usize == link_ptr)
            {
                tracing::info!(device = %device_id, "peer: link dropped — evicting");
                links.remove(&device_id);
            }
        });
    }
}

pub(super) fn credential_transport_allowed(base: &str) -> bool {
    let Ok(url) = url::Url::parse(base) else {
        return false;
    };
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    matches!(url.scheme(), "https" | "wss")
        || (matches!(url.scheme(), "http" | "ws")
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
}
