//! The host relay: this device serving its full RPC surface through its own
//! DeviceRoom, one virtual connection per remote client.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use cypher_trust::noise::{Accepting, Session};

use super::frames::{
    DeviceFrameHeader, decode_device_frame, encode_device_frame, relay_error_code,
};
use super::trust::{
    CHANNEL_FAILED, HANDSHAKE_FAILED, HELLO, NOISE_KIND, NoTrust, PLAINTEXT_REFUSED, PeerTrust,
    REFUSED, REPLY, SECURE_KIND, UNTRUSTED_DEVICE, noise_payload,
};
use super::{
    NUDGE_KIND, NudgeHandler, PING_INTERVAL, RELAY_KIND, RPC_KIND, SILENCE_LEASE, TokenSource,
    device_room_ws_url, token_changed,
};
use crate::{ClientFrame, RpcError, RpcService, ServerFrame, lock, serve_connection};

pub struct HostRelayConfig {
    /// Edge base URL (`http(s)://…`; rewritten to `ws(s)` for the socket).
    pub edge_url: String,
    pub device_id: String,
    pub token: Arc<dyn TokenSource>,
    /// Reconnect delay after a session ends (a small jitter is added).
    pub retry: Duration,
    /// Device trust: which clients may open a Noise session, and whether
    /// plaintext and unpaired clients are refused.
    pub trust: Arc<dyn PeerTrust>,
}

impl HostRelayConfig {
    pub fn new(
        edge_url: impl Into<String>,
        device_id: impl Into<String>,
        token: Arc<dyn TokenSource>,
    ) -> Self {
        Self {
            edge_url: edge_url.into(),
            device_id: device_id.into(),
            token,
            retry: Duration::from_secs(5),
            trust: Arc::new(NoTrust),
        }
    }

    pub fn with_trust(mut self, trust: Arc<dyn PeerTrust>) -> Self {
        self.trust = trust;
        self
    }
}

/// The host end of the relay: one outbound WebSocket to our own DeviceRoom DO, serving
/// `service` to every client conn through virtual string-frame connections. Immortal
/// supervisor: quiet while signed out, reconnects with backoff when the socket drops
/// (including the 4409 "superseded by new host connection" close — the newest host wins,
/// so the superseded process backs off and retries, mirroring zeron's DeviceRoomHost).
pub struct HostRelay {
    task: tokio::task::JoinHandle<()>,
}

impl HostRelay {
    pub fn spawn(
        config: HostRelayConfig,
        service: Arc<dyn RpcService>,
        on_nudge: NudgeHandler,
    ) -> Self {
        let task = tokio::spawn(async move {
            let mut wake = cypher_net::wake::subscribe();
            let mut online = cypher_net::wake::subscribe_online();
            let mut token_changes = config.token.subscribe();
            // Fast-rejoin bookkeeping: the edge DO periodically ends healthy
            // host sessions (hibernation/deploys). Every second the host is
            // away, client dials bounce with "readiness check failed" (user
            // report) — so a session that ENDED CLEANLY after a healthy
            // stretch rejoins near-instantly, and only rapid consecutive
            // failures walk the backoff.
            let mut delay = HOST_REJOIN_MIN;
            loop {
                if let Some(token) = config.token.token().await {
                    let url = device_room_ws_url(
                        &config.edge_url,
                        &config.device_id,
                        "host",
                        None,
                        &token,
                    );
                    let started = tokio::time::Instant::now();
                    let outcome = {
                        let session = host_session(
                            &url,
                            &service,
                            &on_nudge,
                            &config.trust,
                            &config.device_id,
                        );
                        tokio::pin!(session);
                        loop {
                            tokio::select! {
                                outcome = &mut session => break outcome,
                                _ = token_changed(&mut token_changes) => {
                                    // Token rotations keep a healthy socket alive. Sign-out is
                                    // different: dropping the session closes the authenticated
                                    // socket and every virtual RPC connection immediately.
                                    if config.token.token().await.is_none() {
                                        tracing::info!(
                                            "device-room: credentials removed; closing host session"
                                        );
                                        break Ok(());
                                    }
                                }
                            }
                        }
                    };
                    let healthy = started.elapsed() >= HOST_HEALTHY_SESSION;
                    match outcome {
                        Ok(()) => {
                            tracing::info!("device-room: host session ended; reconnecting");
                            delay = HOST_REJOIN_MIN;
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "device-room: host session failed");
                            delay = if healthy {
                                HOST_REJOIN_MIN
                            } else {
                                (delay * 2).min(config.retry)
                            };
                        }
                    }
                } else {
                    // Signed out: poll for credentials at the configured pace.
                    delay = config.retry;
                }
                // Drain stale events (our own dial success notifies too) so
                // only wakes/successes DURING this wait cut it short.
                while online.try_recv().is_ok() {}
                tokio::select! {
                    _ = tokio::time::sleep(delay + jitter()) => {}
                    // Wake = redial NOW (the old socket died with the suspend).
                    _ = wake.recv() => { delay = HOST_REJOIN_MIN; }
                    // A sibling dial succeeded = the network is back.
                    _ = online.recv() => { delay = HOST_REJOIN_MIN; }
                    _ = token_changed(&mut token_changes) => { delay = HOST_REJOIN_MIN; }
                }
            }
        });
        Self { task }
    }
}

impl Drop for HostRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Floor for host-relay rejoins after a clean/healthy session end — the DO
/// evicting an idle host must not leave a multi-second unreachable window.
const HOST_REJOIN_MIN: Duration = Duration::from_millis(250);

/// A session that lasted at least this long counts as healthy: its end is the
/// DO's lifecycle, not our failure, so the rejoin does not escalate.
const HOST_HEALTHY_SESSION: Duration = Duration::from_secs(30);

fn jitter() -> Duration {
    // Cheap decorrelation without a rand dependency.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    Duration::from_millis(u64::from(nanos) % 2_000)
}

/// One per-client virtual connection: `in_tx` feeds the ndjson dispatch loop
/// ([`serve_connection`]); its replies are pumped back as `{to: connId}` frames,
/// sealed when the client opened a Noise session.
///
/// Teardown is by channel closure, NOT task abort: dropping `in_tx` ends the dispatch
/// loop, which aborts its in-flight request tasks (streams included); their reply senders
/// drop and the pump task drains out. Aborting the dispatch loop directly would strand
/// the request tasks it spawned.
struct VirtualConn {
    in_tx: mpsc::Sender<String>,
    session: Option<Arc<std::sync::Mutex<Session>>>,
    /// The device id and Noise key a sealed session's client proved.
    client: Option<(String, [u8; 32])>,
}

fn make_virtual_conn(
    service: Arc<dyn RpcService>,
    conn_id: String,
    host_out: mpsc::Sender<Vec<u8>>,
    session: Option<Arc<std::sync::Mutex<Session>>>,
) -> VirtualConn {
    let (in_tx, in_rx) = mpsc::channel::<String>(256);
    let (srv_out_tx, mut srv_out_rx) = mpsc::channel::<String>(256);
    tokio::spawn(serve_connection(service, srv_out_tx, in_rx));
    let sealing = session.clone();
    tokio::spawn(async move {
        while let Some(text) = srv_out_rx.recv().await {
            let payloads = match &sealing {
                Some(session) => match lock(session).seal(text.as_bytes()) {
                    Ok(fragments) => fragments.into_iter().map(|f| (SECURE_KIND, f)).collect(),
                    Err(err) => {
                        tracing::error!(error = %err, "device-room: seal failed; closing conn");
                        break;
                    }
                },
                None => vec![(RPC_KIND, text.into_bytes())],
            };
            for (kind, payload) in payloads {
                let header = DeviceFrameHeader::new(RPC_KIND, kind).with_to(conn_id.clone());
                match encode_device_frame(&header, &payload) {
                    Ok(frame) => {
                        if host_out.send(frame).await.is_err() {
                            return; // relay socket gone
                        }
                    }
                    Err(err) => tracing::error!(error = %err, "device-room: frame encode failed"),
                }
            }
        }
    });
    VirtualConn {
        in_tx,
        session,
        client: None,
    }
}

/// One relay session: connect as host, serve RPC per client conn, until the socket drops.
async fn host_session(
    url: &str,
    service: &Arc<dyn RpcService>,
    on_nudge: &NudgeHandler,
    trust: &Arc<dyn PeerTrust>,
    device_id: &str,
) -> Result<(), RpcError> {
    let ws = cypher_net::dial::connect_ws(url)
        .await
        .map_err(|e| RpcError::Transport(format!("device room unreachable: {e}")))?;
    tracing::info!("device-room: host connected");
    let (mut sink, mut stream) = ws.split();
    // All writers (per-conn pumps) funnel through one outbound queue → one socket writer.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(256);
    let host = Host {
        service: service.clone(),
        out_tx,
        on_nudge: on_nudge.clone(),
        trust: trust.clone(),
        device_id: device_id.to_string(),
    };
    let mut conns: HashMap<String, VirtualConn> = HashMap::new();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await; // consume the immediate first tick
    let mut last_rx = tokio::time::Instant::now();

    loop {
        tokio::select! {
            frame = out_rx.recv() => match frame {
                Some(bytes) => {
                    if sink.send(WsMessage::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                None => break, // unreachable: we hold out_tx
            },
            message = stream.next() => match message {
                Some(Ok(WsMessage::Binary(bytes))) => {
                    last_rx = tokio::time::Instant::now();
                    host.handle_frame(&bytes, &mut conns).await;
                }
                Some(Ok(WsMessage::Close(frame))) => {
                    if let Some(frame) = frame {
                        tracing::info!(code = %frame.code, reason = %frame.reason,
                            "device-room: host socket closed by relay");
                    }
                    break;
                }
                Some(Err(_)) | None => break,
                // Text "pong" / control frames: proof of life for the lease.
                Some(Ok(_)) => last_rx = tokio::time::Instant::now(),
            },
            _ = ping.tick() => {
                if let Err(err) = sink.send(WsMessage::Text("ping".into())).await {
                    // The usual way a silently-reaped uplink surfaces: reads
                    // saw nothing, the keepalive write is what finds the body.
                    tracing::warn!(error = %err, "device-room: host keepalive failed; reconnecting");
                    break;
                }
            }
            _ = tokio::time::sleep_until(last_rx + SILENCE_LEASE) => {
                tracing::warn!("device-room: host socket silent past lease; reconnecting");
                break;
            }
        }
    }
    // Dropping the conns aborts every per-client dispatch loop (terminals etc. reaped).
    conns.clear();
    Ok(())
}

/// What a host session needs to answer client frames.
struct Host {
    service: Arc<dyn RpcService>,
    out_tx: mpsc::Sender<Vec<u8>>,
    on_nudge: NudgeHandler,
    trust: Arc<dyn PeerTrust>,
    device_id: String,
}

impl Host {
    async fn send(&self, to: &str, kind: &str, payload: &[u8]) {
        let header = DeviceFrameHeader::new(kind, kind).with_to(to);
        match encode_device_frame(&header, payload) {
            Ok(frame) => {
                let _ = self.out_tx.send(frame).await;
            }
            Err(err) => tracing::error!(error = %err, "device-room: frame encode failed"),
        }
    }

    async fn refuse(&self, to: &str, reason: &str) {
        self.send(to, NOISE_KIND, &noise_payload(REFUSED, reason.as_bytes()))
            .await;
    }

    async fn handle_frame(&self, bytes: &[u8], conns: &mut HashMap<String, VirtualConn>) {
        let (header, payload) = match decode_device_frame(bytes) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::warn!(error = %err, "device-room: malformed frame — skipping");
                return;
            }
        };
        if header.k == RELAY_KIND {
            // Relay control: a client went away (`client_closed` carries `from`; a bounced
            // `client_gone` carries `to`) — tear down that conn's RPC server.
            let code = relay_error_code(&payload).unwrap_or_default();
            if let Some(conn_id) = header.from.as_deref().or(header.to.as_deref()) {
                tracing::debug!(conn = %conn_id, %code, "device-room: client conn torn down");
                conns.remove(conn_id);
            }
            return;
        }
        if header.k == NUDGE_KIND {
            // Durable command nudge (§7): open the chat doc so drain fires.
            #[derive(Deserialize)]
            struct Nudge {
                #[serde(rename = "chatId")]
                chat_id: Option<String>,
            }
            match serde_json::from_slice::<Nudge>(&payload) {
                Ok(Nudge {
                    chat_id: Some(chat_id),
                }) => (self.on_nudge)(chat_id),
                _ => tracing::warn!("device-room: malformed nudge — ignoring"),
            }
            return;
        }
        let Some(from) = header.from else {
            return;
        };
        match header.k.as_str() {
            NOISE_KIND => self.hello(&from, &payload, conns).await,
            SECURE_KIND => {
                let Some(conn) = conns.get(&from) else { return };
                let Some(session) = &conn.session else { return };
                if self.trust.require_paired()
                    && !conn
                        .client
                        .as_ref()
                        .is_some_and(|(id, key)| self.trust.is_known_member(id, key))
                {
                    tracing::warn!(conn = %from, "device-room: closing a session whose device left the trust group");
                    conns.remove(&from);
                    self.refuse(&from, UNTRUSTED_DEVICE).await;
                    return;
                }
                let opened = lock(session).open(&payload);
                match opened {
                    Ok(Some(message)) => {
                        let text = String::from_utf8_lossy(&message).into_owned();
                        if conn.in_tx.send(text).await.is_err() {
                            tracing::warn!("device-room: virtual conn dispatch loop gone");
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!(conn = %from, error = %err, "device-room: secure frame refused; closing conn");
                        conns.remove(&from);
                        self.refuse(&from, CHANNEL_FAILED).await;
                    }
                }
            }
            RPC_KIND => {
                if conns.get(&from).is_some_and(|conn| conn.session.is_some()) {
                    tracing::warn!(conn = %from, "device-room: dropping a plaintext frame on a secure conn");
                    return;
                }
                if !conns.contains_key(&from) {
                    self.trust.refresh().await;
                }
                if self.trust.require_paired() {
                    conns.remove(&from);
                    self.refuse_plaintext(&from, &payload).await;
                    return;
                }
                let conn = conns.entry(from.clone()).or_insert_with(|| {
                    make_virtual_conn(self.service.clone(), from, self.out_tx.clone(), None)
                });
                let text = String::from_utf8_lossy(&payload).into_owned();
                if conn.in_tx.send(text).await.is_err() {
                    tracing::warn!("device-room: virtual conn dispatch loop gone");
                }
            }
            _ => {} // future stream kinds (term, tunnel)
        }
    }

    /// The first handshake message: authenticate the client against the
    /// trust group and open a sealed connection.
    async fn hello(&self, from: &str, payload: &[u8], conns: &mut HashMap<String, VirtualConn>) {
        let Some((&HELLO, message)) = payload.split_first() else {
            return;
        };
        let Some(keys) = self.trust.local_keys() else {
            self.refuse(from, HANDSHAKE_FAILED).await;
            return;
        };
        let accepting = match Accepting::read(&keys, &self.device_id, message) {
            Ok(accepting) => accepting,
            Err(err) => {
                tracing::warn!(conn = %from, error = %err, "device-room: handshake refused");
                self.refuse(from, HANDSHAKE_FAILED).await;
                return;
            }
        };
        self.trust.refresh().await;
        let member = self
            .trust
            .is_member(&accepting.client_id, &accepting.client_key)
            .await;
        if !member {
            if self.trust.require_paired() {
                tracing::warn!(conn = %from, client = %accepting.client_id,
                    "device-room: refusing a device outside the trust group");
                self.refuse(from, UNTRUSTED_DEVICE).await;
                return;
            }
            tracing::info!(conn = %from, client = %accepting.client_id,
                "device-room: secure session from a device outside the trust group");
        }
        let client = accepting.client_id.clone();
        let client_key = accepting.client_key;
        match accepting.accept() {
            Ok((reply, session)) => {
                let session = Arc::new(std::sync::Mutex::new(session));
                let mut conn = make_virtual_conn(
                    self.service.clone(),
                    from.to_string(),
                    self.out_tx.clone(),
                    Some(session),
                );
                conn.client = Some((client.clone(), client_key));
                conns.insert(from.to_string(), conn);
                self.send(from, NOISE_KIND, &noise_payload(REPLY, &reply))
                    .await;
                tracing::info!(conn = %from, %client, member, "device-room: secure session open");
            }
            Err(err) => {
                tracing::warn!(conn = %from, error = %err, "device-room: handshake failed");
                self.refuse(from, HANDSHAKE_FAILED).await;
            }
        }
    }

    /// A plaintext client on a host that only accepts paired devices: answer
    /// each request with an error, so it fails fast instead of timing out.
    async fn refuse_plaintext(&self, from: &str, payload: &[u8]) {
        for line in String::from_utf8_lossy(payload).lines() {
            let Ok(frame) = serde_json::from_str::<ClientFrame>(line.trim()) else {
                continue;
            };
            if frame.method.is_none() {
                continue;
            }
            let reply = ServerFrame {
                id: frame.id,
                err: Some(PLAINTEXT_REFUSED.into()),
                ..Default::default()
            };
            if let Ok(text) = serde_json::to_string(&reply) {
                self.send(from, RPC_KIND, text.as_bytes()).await;
            }
        }
    }
}
