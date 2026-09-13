//! Regression tests for RegistryClient's HTTPS fallback while WebSocket
//! dialing is unavailable. These deliberately use a closed WS port and a
//! controllable fake RegistryTransport.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use cypher_proto::Chat;
use cypher_sync::{RegistryClient, RegistryTransport, RegistryTuning, StaticUrl, SyncError};
use futures::future::BoxFuture;
use tokio::sync::Notify;

fn chat(id: &str, title: &str) -> Chat {
    Chat {
        id: id.into(),
        device_id: "dev-http".into(),
        title: Some(title.into()),
        archived: false,
        cwd: Some("/tmp".into()),
        branch: None,
        checkout_id: None,
        config: None,
        last_message_preview: None,
        last_message_at: None,
        created_at: DateTime::<Utc>::UNIX_EPOCH,
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: None,
        last_seen_at: None,
        room_gen: None,
        child: None,
    }
}

async fn wait_until(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition not reached");
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
