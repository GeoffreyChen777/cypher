//! The host relay: this device serving its full RPC surface through its own
//! DeviceRoom, one virtual connection per remote client.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::frames::{
    DeviceFrameHeader, decode_device_frame, encode_device_frame, relay_error_code,
};
use super::{
    NUDGE_KIND, NudgeHandler, PING_INTERVAL, RELAY_KIND, RPC_KIND, SILENCE_LEASE, TokenSource,
    device_room_ws_url, token_changed,
};
use crate::{RpcError, RpcService, serve_connection};

pub struct HostRelayConfig {
    /// Edge base URL (`http(s)://…`; rewritten to `ws(s)` for the socket).
    pub edge_url: String,
    pub device_id: String,
    pub token: Arc<dyn TokenSource>,
    /// Reconnect delay after a session ends (a small jitter is added).
    pub retry: Duration,
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
        }
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
                        let session = host_session(&url, &service, &on_nudge);
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
/// ([`serve_connection`]); its replies are pumped back as `{to: connId}` frames.
///
/// Teardown is by channel closure, NOT task abort: dropping `in_tx` ends the dispatch
/// loop, which aborts its in-flight request tasks (streams included); their reply senders
/// drop and the pump task drains out. Aborting the dispatch loop directly would strand
/// the request tasks it spawned.
struct VirtualConn {
    in_tx: mpsc::Sender<String>,
}

fn make_virtual_conn(
    service: Arc<dyn RpcService>,
    conn_id: String,
    host_out: mpsc::Sender<Vec<u8>>,
) -> VirtualConn {
    let (in_tx, in_rx) = mpsc::channel::<String>(256);
    let (srv_out_tx, mut srv_out_rx) = mpsc::channel::<String>(256);
    tokio::spawn(serve_connection(service, srv_out_tx, in_rx));
    tokio::spawn(async move {
        while let Some(text) = srv_out_rx.recv().await {
            let header = DeviceFrameHeader::new(RPC_KIND, RPC_KIND).with_to(conn_id.clone());
            match encode_device_frame(&header, text.as_bytes()) {
                Ok(frame) => {
                    if host_out.send(frame).await.is_err() {
                        break; // relay socket gone
                    }
                }
                Err(err) => tracing::error!(error = %err, "device-room: frame encode failed"),
            }
        }
    });
    VirtualConn { in_tx }
}

/// One relay session: connect as host, serve RPC per client conn, until the socket drops.
async fn host_session(
    url: &str,
    service: &Arc<dyn RpcService>,
    on_nudge: &NudgeHandler,
) -> Result<(), RpcError> {
    let ws = cypher_net::dial::connect_ws(url)
        .await
        .map_err(|e| RpcError::Transport(format!("device room unreachable: {e}")))?;
    tracing::info!("device-room: host connected");
    let (mut sink, mut stream) = ws.split();
    // All writers (per-conn pumps) funnel through one outbound queue → one socket writer.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(256);
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
                    handle_host_frame(&bytes, &mut conns, service, &out_tx, on_nudge).await;
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

async fn handle_host_frame(
    bytes: &[u8],
    conns: &mut HashMap<String, VirtualConn>,
    service: &Arc<dyn RpcService>,
    out_tx: &mpsc::Sender<Vec<u8>>,
    on_nudge: &NudgeHandler,
) {
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
            }) => on_nudge(chat_id),
            _ => tracing::warn!("device-room: malformed nudge — ignoring"),
        }
        return;
    }
    if header.k != RPC_KIND {
        return; // future stream kinds (term, tunnel)
    }
    let Some(from) = header.from else {
        return;
    };
    let conn = conns
        .entry(from.clone())
        .or_insert_with(|| make_virtual_conn(service.clone(), from, out_tx.clone()));
    let text = String::from_utf8_lossy(&payload).into_owned();
    if conn.in_tx.send(text).await.is_err() {
        tracing::warn!("device-room: virtual conn dispatch loop gone");
    }
}
