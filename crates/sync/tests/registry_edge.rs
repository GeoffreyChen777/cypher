//! Live-edge interop: two Rust `RegistryClient`s converge through a REAL
//! RegistryRoom Durable Object (the TS implementation in
//! apps/edge/src/registry/registry-room.ts), proving the JSON protocol and the mirrored
//! merge semantics are byte-compatible across the language boundary.
//!
//! Ignored by default — requires the TS edge running (`wrangler dev` in
//! `apps/edge/` with AUTH_MODE=dev). Run with:
//!
//! ```sh
//! CYPHER_EDGE_WS=ws://127.0.0.1:27640 cargo test -p cypher-sync --test registry_edge -- --ignored
//! ```

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cypher_doc::RegistryDoc;
use cypher_proto::{Chat, Session, SessionStatus};
use cypher_sync::RegistryClient;

use common::ts;

fn chat(id: &str, device_id: &str) -> Chat {
    Chat {
        title: Some("live chat".into()),
        ..common::chat(id, device_id)
    }
}

/// A live edge round-trips over the network: allow it time.
async fn wait_until(condition: impl FnMut() -> bool) {
    common::wait_until(Duration::from_secs(30), condition).await
}

fn edge_url(org: &str, user: &str) -> String {
    let base = cypher_env::var("EDGE_WS")
        .expect("set CYPHER_EDGE_WS to the edge origin, e.g. ws://127.0.0.1:27640");
    // Dev-mode bearer `user@org` carries the org claim the registry route checks.
    format!("{base}/registry/{org}/ws?token={user}@{org}&device=it")
}

#[tokio::test]
#[ignore = "requires a live edge: set CYPHER_EDGE_WS (e.g. ws://127.0.0.1:27640)"]
async fn two_rust_clients_converge_through_a_real_registry_do() {
    let org = format!("org{}", uuid::Uuid::new_v4().simple());
    let url = edge_url(&org, "alice");

    let doc_a = Arc::new(Mutex::new(RegistryDoc::new("dev-live-a")));
    let doc_b = Arc::new(Mutex::new(RegistryDoc::new("dev-live-b")));

    // A pushes rows through the real DO.
    {
        let mut doc = doc_a.lock().unwrap();
        doc.upsert_chat(&chat("chat-live", "dev-live-a")).unwrap();
        doc.upsert_session(&Session {
            chat_id: "chat-live".into(),
            device_id: "dev-live-a".into(),
            status: SessionStatus::Working,
            started_at: Some(ts(3_000)),
            updated_at: ts(3_500),
            subagents: Vec::new(),
            context_usage: None,
            throughput: None,
        })
        .unwrap();
    }
    let client_a = RegistryClient::connect(&url, doc_a.clone(), "dev-live-a")
        .await
        .expect("a connects to the real DO");
    client_a.nudge();
    wait_until(|| doc_a.lock().unwrap().pending_len() == 0).await;

    // B joins fresh and receives the full state from the DO's row table.
    let client_b = RegistryClient::connect(&url, doc_b.clone(), "dev-live-b")
        .await
        .expect("b connects to the real DO");
    wait_until(|| {
        let doc = doc_b.lock().unwrap();
        doc.read_chats().unwrap().len() == 1 && doc.read_sessions().unwrap().len() == 1
    })
    .await;

    // Live broadcast: B renames, A observes through the DO fan-out.
    {
        let mut doc = doc_b.lock().unwrap();
        assert!(doc.rename_chat("chat-live", "renamed via DO").unwrap());
    }
    client_b.nudge();
    wait_until(|| {
        doc_a
            .lock()
            .unwrap()
            .chat("chat-live")
            .unwrap()
            .is_some_and(|c| c.title.as_deref() == Some("renamed via DO"))
    })
    .await;

    // Concurrent same-field writes settle identically on both sides (the TS
    // merge and the Rust merge agree on the winner).
    {
        doc_a
            .lock()
            .unwrap()
            .rename_chat("chat-live", "A wins?")
            .unwrap();
        doc_b
            .lock()
            .unwrap()
            .rename_chat("chat-live", "B wins?")
            .unwrap();
    }
    client_a.nudge();
    client_b.nudge();
    wait_until(|| {
        doc_a.lock().unwrap().pending_len() == 0 && doc_b.lock().unwrap().pending_len() == 0
    })
    .await;
    wait_until(|| {
        let a = doc_a
            .lock()
            .unwrap()
            .chat("chat-live")
            .unwrap()
            .unwrap()
            .title;
        let b = doc_b
            .lock()
            .unwrap()
            .chat("chat-live")
            .unwrap()
            .unwrap()
            .title;
        a == b
    })
    .await;

    // Presence beats cross the DO.
    client_a.set_presence(987_654);
    wait_until(|| client_b.presence().get("dev-live-a") == Some(&987_654)).await;

    // Probe round-trips against the real room.
    client_a.probe();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stats = client_a.stats();
    assert!(stats.connected);
    assert_eq!(stats.disconnects, 0);

    client_a.shutdown().await;
    client_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires a live edge: set CYPHER_EDGE_WS (e.g. ws://127.0.0.1:27640)"]
async fn cursor_delta_and_churn_stay_bounded_on_a_real_do() {
    let org = format!("org{}", uuid::Uuid::new_v4().simple());
    let url = edge_url(&org, "alice");

    let doc = Arc::new(Mutex::new(RegistryDoc::new("dev-churn")));
    {
        let mut d = doc.lock().unwrap();
        d.upsert_chat(&chat("chat-churn", "dev-churn")).unwrap();
    }
    let client = RegistryClient::connect(&url, doc.clone(), "dev-churn")
        .await
        .expect("connects");
    client.nudge();
    wait_until(|| doc.lock().unwrap().pending_len() == 0).await;

    // 500 status flips against the real DO: state must stay 2 rows.
    for i in 0..500i64 {
        {
            let mut d = doc.lock().unwrap();
            d.upsert_session(&Session {
                chat_id: "chat-churn".into(),
                device_id: "dev-churn".into(),
                status: if i % 2 == 0 {
                    SessionStatus::Working
                } else {
                    SessionStatus::Idle
                },
                started_at: Some(ts(i)),
                updated_at: ts(i + 1),
                subagents: Vec::new(),
                context_usage: None,
                throughput: None,
            })
            .unwrap();
        }
        if i % 50 == 0 {
            client.nudge();
            wait_until(|| doc.lock().unwrap().pending_len() == 0).await;
        }
    }
    client.nudge();
    wait_until(|| doc.lock().unwrap().pending_len() == 0).await;
    let cursor = doc.lock().unwrap().cursor();
    client.shutdown().await;

    // A fresh device full-syncs 2 rows — not 501 updates of history.
    let doc_b = Arc::new(Mutex::new(RegistryDoc::new("dev-churn-b")));
    let client_b = RegistryClient::connect(&url, doc_b.clone(), "dev-churn-b")
        .await
        .expect("b connects");
    wait_until(|| {
        let d = doc_b.lock().unwrap();
        d.read_chats().unwrap().len() == 1 && d.read_sessions().unwrap().len() == 1
    })
    .await;
    assert!(doc_b.lock().unwrap().cursor() >= cursor);
    client_b.shutdown().await;
}
