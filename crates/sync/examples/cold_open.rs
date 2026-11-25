//! Cold-open latency of the registry and chat clients (C1 in
//! `docs/design/do-cost.md`): time from opening a client on an empty local
//! copy to its first remote row, with the HTTPS pull beside the WebSocket.
//!
//! Everything is local: the registry is `MockRegistryServer`, the chat room a
//! small frame server below. Network cost is modelled, not real: the socket
//! runs through a proxy that holds a new connection for `2 × RTT` (TCP and TLS
//! handshakes) and delays every chunk by `RTT / 2` each way, so the upgrade
//! and the hello/state exchange cost one RTT each. The HTTPS pull rides a
//! connection that is already warm, so it costs one RTT. Modes:
//!
//! - `ws`: the socket path works;
//! - `stalled`: the server accepts TCP and never answers the upgrade;
//! - `refused`: nothing listens (the dial fails at once).
//!
//! Each scenario opens a cold client (a local copy that never synced) and a
//! warm one (synced before; the remote rows are new since).
//!
//! Usage: `cargo run -p cypher-sync --features mock-server --example cold_open -- [opens] [rtt_ms...]`
//! Prints one line per (client, copy, mode, rtt): median / p90 / max in ms.

#![allow(clippy::print_stdout, clippy::print_stderr)] // a command-line tool

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cypher_proto::Chat;
use cypher_sync::chat_client::{ChatClient, ChatDocSink, ChatTransport, CheckpointFetcher};
use cypher_sync::chat_frames::{decode, encode, frame_type};
use cypher_sync::registry::mock_server::MockRegistryServer;
use cypher_sync::{RegistryClient, RegistryTransport, StaticUrl, SyncError};
use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ws,
    Stalled,
    Refused,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Ws => "ws",
            Mode::Stalled => "stalled",
            Mode::Refused => "refused",
        }
    }
}

// ── modelled network ────────────────────────────────────────────────────────

/// A TCP proxy to `upstream` that adds `rtt` of latency (see the module doc).
async fn latency_proxy(upstream: std::net::SocketAddr, rtt: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            tokio::spawn(async move {
                tokio::time::sleep(rtt * 2).await; // TCP + TLS handshakes
                let Ok(server) = TcpStream::connect(upstream).await else {
                    return;
                };
                let (cr, cw) = client.into_split();
                let (sr, sw) = server.into_split();
                tokio::spawn(delayed_copy(cr, sw, rtt / 2));
                tokio::spawn(delayed_copy(sr, cw, rtt / 2));
            });
        }
    });
    format!("ws://{addr}")
}

async fn delayed_copy(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    one_way: Duration,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(tokio::time::Instant, Vec<u8>)>();
    tokio::spawn(async move {
        while let Some((due, bytes)) = rx.recv().await {
            tokio::time::sleep_until(due).await;
            if to.write_all(&bytes).await.is_err() {
                return;
            }
        }
    });
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match from.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let due = tokio::time::Instant::now() + one_way;
                if tx.send((due, buf[..n].to_vec())).is_err() {
                    return;
                }
            }
        }
    }
}

/// Accepts TCP, never answers: a blocked or stalled upgrade.
async fn stalled_listener() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    format!("ws://{addr}")
}

/// A closed port: the dial fails at once.
async fn refused_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("ws://{addr}")
}

// ── registry ────────────────────────────────────────────────────────────────

struct RegistryHttp {
    body: String,
    rtt: Duration,
}

impl RegistryTransport for RegistryHttp {
    fn fetch(&self, _since: u64) -> BoxFuture<'static, Result<String, SyncError>> {
        let (body, rtt) = (self.body.clone(), self.rtt);
        Box::pin(async move {
            tokio::time::sleep(rtt).await;
            Ok(body)
        })
    }
    fn push(&self, _body: String) -> BoxFuture<'static, Result<String, SyncError>> {
        Box::pin(async { Err(SyncError::Closed) })
    }
}

fn seed_chat() -> Chat {
    Chat {
        pinned: false,
        id: "chat-seed".into(),
        device_id: "dev-seed".into(),
        title: Some("seed".into()),
        archived: false,
        cwd: None,
        branch: None,
        checkout_id: None,
        config: None,
        last_message_preview: None,
        last_message_at: None,
        created_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: None,
        last_seen_at: None,
        room_gen: None,
        child: None,
        bot: None,
        memory: None,
        started_by: None,
    }
}

async fn seeded_registry() -> (MockRegistryServer, String) {
    let server = MockRegistryServer::start().await;
    let doc = Arc::new(Mutex::new(cypher_doc::RegistryDoc::new("dev-seed")));
    let seeder = RegistryClient::connect(&server.url(), doc.clone(), "dev-seed")
        .await
        .unwrap();
    doc.lock().unwrap().upsert_chat(&seed_chat()).unwrap();
    seeder.nudge();
    while server.row("chats", "chat-seed").is_none() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    seeder.shutdown().await;
    let row = server.row("chats", "chat-seed").unwrap();
    let body = serde_json::json!({
        "seq": server.seq(), "full": true, "gcFloor": 0,
        "rows": [row], "presence": {}
    })
    .to_string();
    (server, body)
}

async fn registry_open(url: String, body: String, rtt: Duration, warm: bool) -> Duration {
    let mut doc = cypher_doc::RegistryDoc::new("dev-cold");
    if warm {
        // Synced before, at an empty room: the seed row is new since.
        doc.apply_state(0, false, 0, Vec::new());
    }
    let doc = Arc::new(Mutex::new(doc));
    let started = Instant::now();
    let client = RegistryClient::connect_via_transport(
        Arc::new(StaticUrl(url)),
        doc.clone(),
        "dev-cold",
        Arc::new(RegistryHttp { body, rtt }),
    )
    .await
    .unwrap();
    loop {
        if doc
            .lock()
            .unwrap()
            .chat("chat-seed")
            .ok()
            .flatten()
            .is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let took = started.elapsed();
    client.shutdown().await;
    took
}

// ── chat ────────────────────────────────────────────────────────────────────

/// Two rows: a warm client (cursor 1) still re-reads both, because without a
/// checkpoint the first join's cursor amnesty clamps it to zero.
fn chat_state() -> serde_json::Value {
    serde_json::json!({"headSeq": 2, "seqFloor": 0, "checkpointSeq": 0,
        "checkpointSize": 0, "rowCount": 2, "rowBytes": 6})
}

fn chat_frames(with_state: bool) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    if with_state {
        frames.push(encode(frame_type::STATE, &chat_state(), &[]));
    }
    for seq in 1..=2 {
        frames.push(encode(
            frame_type::ROW,
            &serde_json::json!({"seq": seq, "device": "dev-seed", "batchId": format!("seed-{seq}")}),
            b"row",
        ));
    }
    frames.push(encode(
        frame_type::ROWS_DONE,
        &serde_json::json!({"headSeq": 2}),
        &[],
    ));
    frames
}

/// A two-row chat room speaking the binary frames.
async fn chat_room() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                while let Some(Ok(message)) = ws.next().await {
                    let replies = match message {
                        WsMessage::Text(_) => vec![WsMessage::Text("pong".into())],
                        WsMessage::Binary(bytes) => match decode(&bytes).map(|f| f.kind) {
                            Some(frame_type::HELLO) => {
                                vec![WsMessage::Binary(encode(
                                    frame_type::STATE,
                                    &chat_state(),
                                    &[],
                                ))]
                            }
                            Some(frame_type::ROWS_REQ) => chat_frames(false)
                                .into_iter()
                                .map(WsMessage::Binary)
                                .collect(),
                            _ => vec![],
                        },
                        _ => vec![],
                    };
                    for reply in replies {
                        if ws.send(reply).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    addr
}

struct ChatHttp {
    rtt: Duration,
}

impl ChatTransport for ChatHttp {
    fn fetch_rows(&self, _after: u64) -> BoxFuture<'static, Result<Vec<u8>, SyncError>> {
        let rtt = self.rtt;
        Box::pin(async move {
            tokio::time::sleep(rtt).await;
            let mut body = Vec::new();
            for frame in chat_frames(true) {
                body.extend_from_slice(&(frame.len() as u32).to_le_bytes());
                body.extend_from_slice(&frame);
            }
            Ok(body)
        })
    }
    fn push(&self, _id: String, _bytes: Vec<u8>) -> BoxFuture<'static, Result<String, SyncError>> {
        Box::pin(async { Err(SyncError::Closed) })
    }
}

struct FirstRow(tokio::sync::watch::Sender<bool>);

impl ChatDocSink for FirstRow {
    fn apply_row(&self, _bytes: &[u8], _cursor: u64) {
        let _ = self.0.send(true);
    }
    fn apply_checkpoint(&self, _bytes: &[u8], _cursor: u64) -> Result<(), String> {
        Ok(())
    }
    fn contains_frontier(&self, _frontier: &[u8]) -> bool {
        false
    }
    fn advance_cursor(&self, _cursor: u64) {}
}

struct NoCheckpoint;

impl CheckpointFetcher for NoCheckpoint {
    fn fetch(&self) -> BoxFuture<'static, Result<Vec<u8>, SyncError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

async fn chat_open(url: String, rtt: Duration, warm: bool) -> Duration {
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let started = Instant::now();
    let client = ChatClient::connect_via_transport(
        Arc::new(StaticUrl(url)),
        Arc::new(FirstRow(tx)),
        Arc::new(NoCheckpoint),
        "dev-cold",
        u64::from(warm),
        Arc::new(ChatHttp { rtt }),
    )
    .await
    .unwrap();
    rx.wait_for(|row| *row).await.unwrap();
    let took = started.elapsed();
    client.shutdown().await;
    took
}

// ── driver ──────────────────────────────────────────────────────────────────

fn summary(mut samples: Vec<Duration>) -> String {
    samples.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let at = |q: f64| samples[((samples.len() as f64 - 1.0) * q).round() as usize];
    format!(
        "median {:7.1}  p90 {:7.1}  max {:7.1}",
        ms(at(0.5)),
        ms(at(0.9)),
        ms(*samples.last().unwrap())
    )
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let opens: usize = args.next().map_or(20, |n| n.parse().unwrap());
    let rtts: Vec<u64> = {
        let rest: Vec<u64> = args.map(|n| n.parse().unwrap()).collect();
        if rest.is_empty() {
            vec![0, 50, 150]
        } else {
            rest
        }
    };
    let (registry, body) = seeded_registry().await;
    let room = chat_room().await;
    let registry_addr: std::net::SocketAddr =
        registry.url().trim_start_matches("ws://").parse().unwrap();
    for rtt_ms in rtts {
        let rtt = Duration::from_millis(rtt_ms);
        for mode in [Mode::Ws, Mode::Stalled, Mode::Refused] {
            let (registry_url, chat_url) = match mode {
                Mode::Ws => (
                    latency_proxy(registry_addr, rtt).await,
                    format!("{}/chat2/cold/ws", latency_proxy(room, rtt).await),
                ),
                Mode::Stalled => (stalled_listener().await, stalled_listener().await),
                Mode::Refused => (refused_url().await, refused_url().await),
            };
            for warm in [false, true] {
                let mut reg = Vec::new();
                let mut chat = Vec::new();
                let limit = Duration::from_secs(30);
                for _ in 0..opens {
                    let one = registry_open(registry_url.clone(), body.clone(), rtt, warm);
                    let one = tokio::time::timeout(limit, one).await;
                    reg.push(one.expect("registry open hung"));
                    let one = chat_open(chat_url.clone(), rtt, warm);
                    let one = tokio::time::timeout(limit, one).await;
                    chat.push(one.expect("chat open hung"));
                }
                let copy = if warm { "warm" } else { "cold" };
                let mode = mode.name();
                println!(
                    "registry {copy} {mode:8} rtt {rtt_ms:3} ms  {}",
                    summary(reg)
                );
                println!(
                    "chat     {copy} {mode:8} rtt {rtt_ms:3} ms  {}",
                    summary(chat)
                );
            }
        }
    }
}
