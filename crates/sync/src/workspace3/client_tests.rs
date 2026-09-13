use super::{
    client::{Client, Event},
    journal::Journal,
    wire::Scope,
};
use crate::{AuthenticatedUrl, StaticUrl, UrlProvider};
use cypher_proto::metadata::OpKind;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_tungstenite::tungstenite::Message;

async fn accept(
    listener: &tokio::net::TcpListener,
) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
    let (stream, _) = listener.accept().await.unwrap();
    tokio_tungstenite::accept_hdr_async(
        stream,
        |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
            assert_eq!(
                request.headers().get("authorization").unwrap(),
                "Bearer fixture"
            );
            assert!(request.uri().query().is_none());
            Ok(response)
        },
    )
    .await
    .unwrap()
}
async fn welcome(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    user: &str,
    org: &str,
) {
    let first: Value =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(first["type"], "hello");
    assert_eq!(first["user"], "user");
    assert_eq!(first["org"], "org");
    ws.send(Message::Text(
        json!({"version":3,"type":"welcome","user":user,"org":org,"connection":"connection",
        "leaseMs":45000,"through":0,"next":0,"done":true,"rows":[]})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
}
fn setup(port: u16) -> (Scope, Arc<AuthenticatedUrl>) {
    (
        Scope {
            endpoint: format!("http://127.0.0.1:{port}"),
            org: "org".into(),
            user: "user".into(),
            actor: "host".into(),
        },
        Arc::new(AuthenticatedUrl::new(
            format!("ws://127.0.0.1:{port}/workspace3/org/ws"),
            "fixture",
        )),
    )
}
#[tokio::test]
async fn v3_requests_do_not_put_bearers_in_urls() {
    assert!(
        StaticUrl("ws://localhost/ws?token=private".into())
            .request()
            .await
            .is_err()
    );
    assert!(
        AuthenticatedUrl::new("ws://user:private@localhost/ws", "fixture")
            .request()
            .await
            .is_err()
    );
    let request = AuthenticatedUrl::new("ws://localhost/ws", "fixture")
        .request()
        .await
        .unwrap();
    assert!(request.uri().query().is_none());
    assert_eq!(request.headers()["authorization"], "Bearer fixture");
}
#[tokio::test]
async fn expected_account_is_verified_before_any_pending_metadata_can_leave() {
    for (user, org) in [("other", "org"), ("user", "other")] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (scope, url) = setup(listener.local_addr().unwrap().port());
        let server = tokio::spawn(async move {
            let mut ws = accept(&listener).await;
            welcome(&mut ws, user, org).await;
            if let Ok(Some(Ok(Message::Text(text)))) =
                tokio::time::timeout(Duration::from_secs(2), ws.next()).await
            {
                panic!("private metadata escaped account check: {text}");
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.sqlite");
        let mut j = Journal::open(&path, scope.clone()).unwrap();
        j.mutate(
            "chats",
            "private",
            OpKind::Upsert,
            Some(BTreeMap::from([("title".into(), json!("must not escape"))])),
            1,
        )
        .unwrap();
        let (client, mut events) = Client::spawn(j, url, true);
        let connected = Arc::new(AtomicUsize::new(0));
        let record = connected.clone();
        let receiver = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if matches!(event, Event::Connected { .. }) {
                    record.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        let mut status = client.watch();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if status
                    .borrow_and_update()
                    .error
                    .as_ref()
                    .is_some_and(|e| e.contains("account_mismatch"))
                {
                    break;
                }
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(connected.load(Ordering::SeqCst), 0);
        client.shutdown().await;
        receiver.await.unwrap();
        server.await.unwrap();
        let reopened = Journal::open(&path, scope).unwrap();
        assert_eq!(reopened.cursor().unwrap(), 0);
        assert!(reopened.pending().unwrap().is_some());
    }
}
#[tokio::test]
async fn sent_control_is_not_replayed_and_old_generations_cannot_send_after_reconnect() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (scope, url) = setup(listener.local_addr().unwrap().port());
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let server = tokio::spawn(async move {
        for expected in ["first", "explicit-new"] {
            let mut ws = accept(&listener).await;
            welcome(&mut ws, "user", "org").await;
            loop {
                let message = ws.next().await.unwrap().unwrap();
                if let Message::Text(text) = message {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    if frame["type"] == "call" {
                        assert_eq!(
                            frame["id"], expected,
                            "a disconnected control must not be replayed"
                        );
                        observed.fetch_add(1, Ordering::SeqCst);
                        ws.close(None).await.unwrap();
                        break;
                    }
                }
            }
        }
    });
    let (client, mut events) = Client::spawn(
        Journal::open(std::path::Path::new(":memory:"), scope).unwrap(),
        url,
        true,
    );
    let receiver = tokio::spawn(async move { while events.recv().await.is_some() {} });
    let mut status = client.watch();
    let generation = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = status.borrow_and_update().clone();
            if s.connected {
                break s.generation;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    client.send(generation, json!({"type":"call","id":"first","target":"other","method":"CreateTerminal","params":{}})).await.unwrap();
    let next = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = status.borrow_and_update().clone();
            if s.connected && s.generation > generation {
                break s.generation;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(client.send(generation, json!({"type":"call","id":"stale","target":"other","method":"CreateTerminal","params":{}})).await.is_err());
    client.send(next, json!({"type":"call","id":"explicit-new","target":"other","method":"ReadFile","params":{}})).await.unwrap();
    server.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    tokio::join!(client.shutdown(), client.shutdown());
    receiver.await.unwrap();
    assert!(client.write(|j| j.cursor()).is_err());
}
