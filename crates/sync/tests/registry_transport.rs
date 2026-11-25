//! Regression tests for RegistryClient's HTTPS fallback while WebSocket
//! dialing is unavailable. These deliberately use a closed WS port and a
//! controllable fake RegistryTransport.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use cypher_proto::Chat;
use cypher_sync::registry::mock_server::MockRegistryServer;
use cypher_sync::{RegistryClient, RegistryTransport, RegistryTuning, StaticUrl, SyncError};
use futures::future::BoxFuture;
use tokio::sync::Notify;

fn chat(id: &str, title: &str) -> Chat {
    Chat {
        title: Some(title.into()),
        created_at: DateTime::<Utc>::UNIX_EPOCH,
        ..common::chat(id, "dev-http")
    }
}

async fn wait_until(check: impl FnMut() -> bool) {
    common::wait_until(Duration::from_secs(5), check).await
}

struct FakeTransport {
    pushes: AtomicUsize,
    fetches: AtomicUsize,
    push_batches: Mutex<Vec<String>>,
    first_push_started: Arc<Notify>,
    release_first_push: Arc<Notify>,
    gate_first_push: bool,
    hang_push: bool,
}

impl FakeTransport {
    fn gated() -> Arc<Self> {
        Arc::new(Self {
            pushes: AtomicUsize::new(0),
            fetches: AtomicUsize::new(0),
            push_batches: Mutex::new(Vec::new()),
            first_push_started: Arc::new(Notify::new()),
            release_first_push: Arc::new(Notify::new()),
            gate_first_push: true,
            hang_push: false,
        })
    }

    fn hanging() -> Arc<Self> {
        Arc::new(Self {
            pushes: AtomicUsize::new(0),
            fetches: AtomicUsize::new(0),
            push_batches: Mutex::new(Vec::new()),
            first_push_started: Arc::new(Notify::new()),
            release_first_push: Arc::new(Notify::new()),
            gate_first_push: false,
            hang_push: true,
        })
    }
}

impl RegistryTransport for FakeTransport {
    fn fetch(&self, _since: u64) -> BoxFuture<'static, Result<String, SyncError>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(r#"{"seq":0,"full":false,"gcFloor":0,"rows":[],"presence":{}}"#.into())
        })
    }

    fn push(&self, body: String) -> BoxFuture<'static, Result<String, SyncError>> {
        let index = self.pushes.fetch_add(1, Ordering::SeqCst);
        let batch = serde_json::from_str::<serde_json::Value>(&body)
            .expect("fake push body is JSON")["batch"]
            .as_str()
            .expect("fake push has batch")
            .to_string();
        self.push_batches.lock().unwrap().push(batch.clone());
        if index == 0 {
            self.first_push_started.notify_one();
        }
        if self.gate_first_push && index == 0 {
            let release = self.release_first_push.clone();
            Box::pin(async move {
                release.notified().await;
                Ok(serde_json::json!({ "batch": batch, "seq": 1 }).to_string())
            })
        } else if self.hang_push {
            Box::pin(std::future::pending())
        } else {
            Box::pin(async move {
                Ok(serde_json::json!({ "batch": batch, "seq": index as u64 + 1 }).to_string())
            })
        }
    }
}

fn ws_url() -> StaticUrl {
    // Port 9 rejects immediately on this machine, while remaining a valid
    // WebSocket URL. The transport tests must not depend on a live WS.
    StaticUrl("ws://127.0.0.1:9/registry/offline/ws".into())
}

/// Writes on a JOINED room take the socket and nothing else. The HTTPS cycle
/// that used to run beside every mutation cost a full billable Durable Object
/// request each time (a WS message bills at 20:1) and duplicated what the
/// socket was already delivering. Bootstrap still runs its one cycle on a
/// doc that never synced: the local-first path that makes the client usable
/// before the WS answers (C1 hedges it on every other connect).
#[tokio::test]
async fn nudges_on_a_connected_room_stay_on_the_socket() {
    let server = MockRegistryServer::start().await;
    let transport = FakeTransport::gated();
    let doc = Arc::new(Mutex::new(cypher_doc::RegistryDoc::new("dev-http")));
    let client = RegistryClient::connect_via_transport(
        Arc::new(StaticUrl(server.url())),
        doc.clone(),
        "dev-http",
        transport.clone(),
    )
    .await
    .unwrap();
    wait_until(|| client.stats().connected && transport.fetches.load(Ordering::SeqCst) == 1).await;
    // Let the bootstrap cycle retire before measuring ordinary wakes.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let after_bootstrap = (
        transport.fetches.load(Ordering::SeqCst),
        transport.pushes.load(Ordering::SeqCst),
    );

    for i in 0..3 {
        doc.lock()
            .unwrap()
            .upsert_chat(&chat(&format!("chat-{i}"), "on the socket"))
            .unwrap();
        client.nudge();
        // The write lands on the server through the WebSocket.
        wait_until(|| doc.lock().unwrap().pending_len() == 0).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(
        (
            transport.fetches.load(Ordering::SeqCst),
            transport.pushes.load(Ordering::SeqCst)
        ),
        after_bootstrap,
        "a joined room must not spend an HTTPS round trip per mutation"
    );
    assert!(client.stats().connected);
    client.shutdown().await;
}

#[tokio::test]
async fn nudge_while_ws_is_down_triggers_http_push() {
    let transport = FakeTransport::gated();
    let doc = Arc::new(Mutex::new(cypher_doc::RegistryDoc::new("dev-http")));
    doc.lock()
        .unwrap()
        .upsert_chat(&chat("chat-1", "first"))
        .unwrap();

    let started = transport.first_push_started.notified();
    let client = RegistryClient::connect_via_transport(
        Arc::new(ws_url()),
        doc.clone(),
        "dev-http",
        transport.clone(),
    )
    .await
    .unwrap();
    started.await;

    {
        let mut local = doc.lock().unwrap();
        local.upsert_chat(&chat("chat-2", "second")).unwrap();
    }
    client.nudge();
    transport.release_first_push.notify_one();

    wait_until(|| transport.pushes.load(Ordering::SeqCst) >= 2).await;
    wait_until(|| doc.lock().unwrap().pending_len() == 0).await;
    assert_eq!(transport.pushes.load(Ordering::SeqCst), 2);
    client.shutdown().await;
}

#[tokio::test]
async fn http_sync_reruns_after_a_write_arrives_during_single_flight() {
    let transport = FakeTransport::gated();
    let doc = Arc::new(Mutex::new(cypher_doc::RegistryDoc::new("dev-http")));
    doc.lock()
        .unwrap()
        .upsert_chat(&chat("chat-1", "first"))
        .unwrap();

    let started = transport.first_push_started.notified();
    let client = RegistryClient::connect_via_transport(
        Arc::new(ws_url()),
        doc.clone(),
        "dev-http",
        transport.clone(),
    )
    .await
    .unwrap();
    started.await;

    {
        let mut local = doc.lock().unwrap();
        local.upsert_chat(&chat("chat-2", "second")).unwrap();
    }
    client.nudge();
    transport.release_first_push.notify_one();

    wait_until(|| transport.pushes.load(Ordering::SeqCst) >= 2).await;
    assert_eq!(doc.lock().unwrap().pending_len(), 0);
    assert_eq!(transport.push_batches.lock().unwrap().len(), 2);
    client.shutdown().await;
}

#[tokio::test]
async fn timed_out_http_sync_releases_single_flight_for_retry() {
    let transport = FakeTransport::hanging();
    let doc = Arc::new(Mutex::new(cypher_doc::RegistryDoc::new("dev-http")));
    doc.lock()
        .unwrap()
        .upsert_chat(&chat("chat-1", "first"))
        .unwrap();
    let tuning = RegistryTuning {
        probe_quiet: Duration::from_secs(60),
        http_timeout: Duration::from_millis(20),
    };
    let client = RegistryClient::connect_via_transport_tuned(
        Arc::new(ws_url()),
        doc,
        "dev-http",
        tuning,
        transport.clone(),
    )
    .await
    .unwrap();
    wait_until(|| transport.pushes.load(Ordering::SeqCst) == 1).await;
    client.nudge();
    wait_until(|| transport.pushes.load(Ordering::SeqCst) >= 2).await;
    client.shutdown().await;
}

/// A viewport transition must reach the room promptly (the Worker's push
/// suppression reads it) but needs no reply, so the engine sends it as an
/// extra presence beat on the socket instead of an HTTP request. That is only
/// safe if the call says truthfully whether a socket carried it: `false` is
/// the engine's cue to fall back to HTTP, so a socketless client -- one on a
/// network that blocks WebSockets -- stays exactly as prompt as before.
#[tokio::test]
async fn an_immediate_activity_beat_only_claims_success_on_a_live_socket() {
    let activity = serde_json::json!({
        "clientId": "c", "sequence": 1, "platform": "desktop",
        "foreground": false, "interactionAgeMs": 0, "chatId": null
    });

    // No socket: the HTTPS bootstrap hands back a usable client before (and
    // here, instead of) any WebSocket session.
    let dead = RegistryClient::connect_via_transport(
        Arc::new(StaticUrl("ws://127.0.0.1:1/registry".into())),
        Arc::new(Mutex::new(cypher_doc::RegistryDoc::new("dev-socketless"))),
        "dev-socketless",
        FakeTransport::gated(),
    )
    .await
    .unwrap();
    assert!(!dead.stats().connected);
    assert!(
        !dead.beat_with_activity_now(1, activity.clone()),
        "no live socket: the caller must fall back to HTTP"
    );

    // A live socket: the beat is claimed, and a peer actually receives it.
    let server = MockRegistryServer::start().await;
    let live = RegistryClient::connect_via_transport(
        Arc::new(StaticUrl(server.url())),
        Arc::new(Mutex::new(cypher_doc::RegistryDoc::new("dev-live"))),
        "dev-live",
        FakeTransport::gated(),
    )
    .await
    .unwrap();
    let peer = RegistryClient::connect(
        &server.url(),
        Arc::new(Mutex::new(cypher_doc::RegistryDoc::new("dev-peer"))),
        "dev-peer",
    )
    .await
    .unwrap();
    wait_until(|| live.stats().connected).await;
    assert!(live.beat_with_activity_now(424_242, activity));
    wait_until(|| peer.presence().get("dev-live") == Some(&424_242)).await;

    dead.shutdown().await;
    live.shutdown().await;
    peer.shutdown().await;
}

// ── C1: the HTTPS pull hedges the dial ──────────────────────────────────────

/// The hedge (1.5 s) plus scheduling slack.
const PAST_HEDGE: Duration = Duration::from_millis(1_900);

/// A TCP port that accepts and then never answers the WebSocket upgrade.
async fn stalled_ws() -> (StaticUrl, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = StaticUrl(format!("ws://{}/registry", listener.local_addr().unwrap()));
    let task = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    (url, task)
}

/// A doc that synced before (generation 1): connects hedge their pull.
fn warm_doc() -> Arc<Mutex<cypher_doc::RegistryDoc>> {
    let mut doc = cypher_doc::RegistryDoc::new("dev-http");
    doc.apply_state(0, false, 0, Vec::new());
    Arc::new(Mutex::new(doc))
}

#[tokio::test]
async fn a_cold_doc_pulls_beside_its_first_dial() {
    let (url, stall) = stalled_ws().await;
    let transport = FakeTransport::gated();
    let client = RegistryClient::connect_via_transport(
        Arc::new(url),
        Arc::new(Mutex::new(cypher_doc::RegistryDoc::new("dev-http"))),
        "dev-http",
        transport.clone(),
    )
    .await
    .unwrap();
    common::wait_until(Duration::from_millis(500), || {
        transport.fetches.load(Ordering::SeqCst) == 1
    })
    .await;
    tokio::time::sleep(PAST_HEDGE).await;
    assert_eq!(transport.fetches.load(Ordering::SeqCst), 1);
    client.shutdown().await;
    stall.abort();
}

#[tokio::test]
async fn a_join_and_a_rejoin_inside_the_hedge_make_no_https_pull() {
    let server = MockRegistryServer::start().await;
    let transport = FakeTransport::gated();
    let client = RegistryClient::connect_via_transport(
        Arc::new(StaticUrl(server.url())),
        warm_doc(),
        "dev-http",
        transport.clone(),
    )
    .await
    .unwrap();
    wait_until(|| client.stats().connected).await;
    client.redial();
    wait_until(|| client.stats().disconnects == 1 && client.stats().connected).await;
    tokio::time::sleep(PAST_HEDGE).await;
    assert_eq!(transport.fetches.load(Ordering::SeqCst), 0);
    assert_eq!(transport.pushes.load(Ordering::SeqCst), 0);
    client.shutdown().await;
}

#[tokio::test]
async fn a_stalled_dial_pulls_once_at_the_hedge() {
    let (url, stall) = stalled_ws().await;
    let transport = FakeTransport::gated();
    let client = RegistryClient::connect_via_transport(
        Arc::new(url),
        warm_doc(),
        "dev-http",
        transport.clone(),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert_eq!(
        transport.fetches.load(Ordering::SeqCst),
        0,
        "not before 1.5 s"
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        transport.fetches.load(Ordering::SeqCst),
        1,
        "one pull at 1.5 s"
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(transport.fetches.load(Ordering::SeqCst), 1);
    client.shutdown().await;
    stall.abort();
}

#[tokio::test]
async fn a_slow_join_pulls_at_the_hedge() {
    let server = MockRegistryServer::start().await;
    server.set_state_delay(Duration::from_secs(5));
    let transport = FakeTransport::gated();
    let client = RegistryClient::connect_via_transport(
        Arc::new(StaticUrl(server.url())),
        warm_doc(),
        "dev-http",
        transport.clone(),
    )
    .await
    .unwrap();
    tokio::time::sleep(PAST_HEDGE).await;
    assert!(!client.stats().connected);
    assert_eq!(transport.fetches.load(Ordering::SeqCst), 1);
    client.shutdown().await;
}

#[tokio::test]
async fn a_failed_dial_pulls_at_once() {
    let transport = FakeTransport::gated();
    let client = RegistryClient::connect_via_transport(
        Arc::new(ws_url()),
        warm_doc(),
        "dev-http",
        transport.clone(),
    )
    .await
    .unwrap();
    common::wait_until(Duration::from_millis(500), || {
        transport.fetches.load(Ordering::SeqCst) >= 1
    })
    .await;
    client.shutdown().await;
}

#[tokio::test]
async fn a_waiting_local_write_pulls_at_once() {
    let (url, stall) = stalled_ws().await;
    let transport = FakeTransport::hanging();
    let doc = warm_doc();
    doc.lock()
        .unwrap()
        .upsert_chat(&chat("chat-1", "offline"))
        .unwrap();
    let client = RegistryClient::connect_via_transport_tuned(
        Arc::new(url),
        doc,
        "dev-http",
        RegistryTuning {
            probe_quiet: Duration::from_secs(60),
            http_timeout: Duration::from_millis(100),
        },
        transport.clone(),
    )
    .await
    .unwrap();
    common::wait_until(Duration::from_millis(500), || {
        transport.pushes.load(Ordering::SeqCst) == 1
            && transport.fetches.load(Ordering::SeqCst) == 1
    })
    .await;
    client.shutdown().await;
    stall.abort();
}
