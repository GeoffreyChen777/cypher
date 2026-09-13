//! Real workerd + independent native workspace SQLite clients. No providers,
//! user credentials or remote filesystem effects are involved.
use cypher_proto::metadata::{OpKind, RowOp, apply_op};
use cypher_sync::{
    AuthenticatedUrl,
    workspace3::{
        client::{Client, Event},
        journal::Journal,
        wire::Scope,
    },
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[tokio::main]
async fn main() {
    let base = std::env::args().nth(1).unwrap();
    if let Some(mode) = std::env::args().nth(2) {
        if mode == "--write-journal"
            || mode == "--verify-journal"
            || mode == "--verify-swift-created"
        {
            let path = std::env::args().nth(3).unwrap();
            let mut journal = Journal::open(
                std::path::Path::new(&path),
                Scope {
                    endpoint: "https://workspace.fixture".into(),
                    org: "org".into(),
                    user: "user".into(),
                    actor: "shared".into(),
                },
            )
            .unwrap();
            if mode == "--verify-swift-created" {
                assert_eq!(journal.cursor().unwrap(), 0);
                assert_eq!(
                    journal
                        .row("chats", "swift-created")
                        .unwrap()
                        .unwrap()
                        .fields["title"],
                    json!("Swift created")
                );
                assert!(journal.pending().unwrap().is_some());
                println!(
                    "PASS: Rust opened Swift-created workspace SQLite with semantic scope binding"
                );
            } else if mode == "--write-journal" {
                journal
                    .mutate(
                        "chats",
                        "shared-chat",
                        OpKind::Upsert,
                        Some(BTreeMap::from([
                            ("id".into(), json!("shared-chat")),
                            ("title".into(), json!("Rust 工作区🙂")),
                        ])),
                        100,
                    )
                    .unwrap();
                println!("PASS: Rust wrote scoped workspace3 SQLite/outbox");
            } else {
                assert_eq!(journal.cursor().unwrap(), 1);
                assert_eq!(
                    journal.row("chats", "shared-chat").unwrap().unwrap().fields["title"],
                    json!("Swift 工作区🙂")
                );
                let pending = journal.pending().unwrap().unwrap();
                let frame: serde_json::Value = serde_json::from_str(&pending.request).unwrap();
                let op: RowOp = serde_json::from_value(frame["ops"][0].clone()).unwrap();
                let old = journal.canonical_row("chats", "shared-chat").unwrap();
                let (Some(mut row), _) = apply_op(old.as_ref(), &op) else {
                    panic!("missing row");
                };
                row.seq = 2;
                journal
                    .acknowledge(&pending.id, &pending.hash, 2, &[row])
                    .unwrap();
                assert_eq!(journal.cursor().unwrap(), 1);
                println!(
                    "PASS: Rust reopened Swift workspace state and retired its exact outbox receipt"
                );
            }
            return;
        }
    }
    let parsed = reqwest::Url::parse(&base).unwrap();
    assert_eq!(parsed.scheme(), "http");
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    let dir = tempfile::tempdir().unwrap();
    let scope = |actor: &str| Scope {
        endpoint: base.clone(),
        org: "sync3-org".into(),
        user: "sync3-user".into(),
        actor: actor.into(),
    };
    let url = Arc::new(AuthenticatedUrl::new(
        format!(
            "{}/workspace3/sync3-org/ws",
            base.replacen("http:", "ws:", 1)
        ),
        "sync3-user@sync3-org",
    ));
    let mut journal = Journal::open(&dir.path().join("host.sqlite"), scope("host")).unwrap();
    journal
        .mutate(
            "chats",
            "native-chat",
            OpKind::Upsert,
            Some(BTreeMap::from([
                ("id".into(), json!("native-chat")),
                ("deviceId".into(), json!("host")),
                ("title".into(), json!("workspace native🙂")),
            ])),
            1_700_000_000_000,
        )
        .unwrap();
    let (host, mut host_events) = Client::spawn(journal, url.clone(), true);
    let host = Arc::new(host);
    let demanded = Arc::new(AtomicBool::new(false));
    let host_task = {
        let host = host.clone();
        let demanded = demanded.clone();
        tokio::spawn(async move {
            while let Some(event) = host_events.recv().await {
                if let Event::Frame { generation, frame } = event {
                    match frame["type"].as_str() {
                        Some("demand")
                            if frame["chats"]
                                .as_array()
                                .unwrap()
                                .contains(&json!("native-chat")) =>
                        {
                            demanded.store(true, Ordering::Release)
                        }
                        Some("call") => {
                            assert_eq!(frame["method"], "ReadFixture");
                            for (sequence, value) in
                                ["workspace🙂", " result"].into_iter().enumerate()
                            {
                                host.send(
                                    generation,
                                    json!({"type":"reply","token":frame["token"],
                                    "sequence":sequence,"done":sequence==1,"value":value}),
                                )
                                .await
                                .unwrap();
                            }
                        }
                        Some("error") => panic!("host error: {frame}"),
                        _ => {}
                    }
                }
            }
        })
    };
    let (viewer, mut viewer_events) = Client::spawn(
        Journal::open(&dir.path().join("viewer.sqlite"), scope("phone")).unwrap(),
        url.clone(),
        false,
    );
    let viewer = Arc::new(viewer);
    let (done, result) = tokio::sync::oneshot::channel();
    let viewer_task = {
        let viewer = viewer.clone();
        tokio::spawn(async move {
            let mut token = None;
            let mut text = String::new();
            let mut done = Some(done);
            while let Some(event) = viewer_events.recv().await {
                if let Event::Frame { generation, frame } = event {
                    match frame["type"].as_str() {
                        Some("routed") => token = frame["token"].as_str().map(str::to_owned),
                        Some("reply") => {
                            text.push_str(frame["value"].as_str().unwrap());
                            viewer
                                .send(
                                    generation,
                                    json!({"type":"ack","token":token.as_ref().unwrap(),
                                "through":frame["sequence"].as_u64().unwrap()+1}),
                                )
                                .await
                                .unwrap();
                            if frame["done"] == true {
                                if let Some(done) = done.take() {
                                    let _ = done.send(text.clone());
                                }
                            }
                        }
                        Some("error") => panic!("viewer error: {frame}"),
                        _ => {}
                    }
                }
            }
        })
    };
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            assert!(
                host.watch().borrow().error.is_none(),
                "{:?}",
                host.watch().borrow()
            );
            assert!(
                viewer.watch().borrow().error.is_none(),
                "{:?}",
                viewer.watch().borrow()
            );
            if viewer
                .read(|j| j.row("chats", "native-chat"))
                .unwrap()
                .is_some()
                && host.read(|j| j.pending()).unwrap().is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    viewer.watch_chats(vec!["native-chat".into()]).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !demanded.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let generation = viewer.watch().borrow().generation;
    viewer
        .send(
            generation,
            json!({"type":"call","id":"read","target":"host","method":"ReadFixture","params":{}}),
        )
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), result)
            .await
            .unwrap()
            .unwrap(),
        "workspace🙂 result"
    );
    assert!(viewer.send(generation + 1, json!({"type":"call","id":"stale","target":"host","method":"ReadFixture","params":{}})).await.is_err());

    let mut wrong = scope("wrong-actor");
    wrong.user = "wrong-user".into();
    let wrong_path = dir.path().join("wrong.sqlite");
    let mut journal = Journal::open(&wrong_path, wrong.clone()).unwrap();
    journal
        .mutate(
            "chats",
            "must-not-upload",
            OpKind::Upsert,
            Some(BTreeMap::from([(
                "title".into(),
                json!("private pending value"),
            )])),
            1,
        )
        .unwrap();
    let (bad, mut bad_events) = Client::spawn(journal, url, false);
    let bad_task = tokio::spawn(async move { while bad_events.recv().await.is_some() {} });
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut status = bad.watch();
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
    bad.shutdown().await;
    bad_task.await.unwrap();
    assert!(
        Journal::open(&wrong_path, wrong)
            .unwrap()
            .pending()
            .unwrap()
            .is_some()
    );
    assert!(
        viewer
            .read(|j| j.row("chats", "must-not-upload"))
            .unwrap()
            .is_none()
    );
    viewer.shutdown().await;
    host.shutdown().await;
    viewer_task.await.unwrap();
    host_task.await.unwrap();
    assert!(
        viewer.read(|j| j.cursor()).is_err(),
        "retired scope cannot serve late callbacks"
    );
    let reopened = Journal::open(&dir.path().join("viewer.sqlite"), scope("phone")).unwrap();
    assert_eq!(
        reopened
            .row("chats", "native-chat")
            .unwrap()
            .unwrap()
            .fields["title"],
        "workspace native🙂"
    );
    assert!(reopened.cursor().unwrap() > 0);
    println!(
        "PASS: workerd ↔ native Workspace3; offline SQLite outbox, exact ACK, demand, RPC credits, account fence, generation retirement and restart"
    );
}
