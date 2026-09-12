//! Local workerd end-to-end: independent SQLite journals, actual WebSockets,
//! typed events, restart, and zero HTTP repair on a healthy stream.
//! Run wrangler dev --local -c wrangler.sync3test.jsonc before this example.
use cypher_proto::sync3::{Operation, Projection, Reply, Request};
use cypher_sync::{
    StaticUrl,
    sync3::{
        Error, Journal, Phase,
        transport::{Client, RepairTransport, Tuning},
    },
};
use futures::future::BoxFuture;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

struct HttpRepair {
    url: String,
    requests: Arc<AtomicUsize>,
}
impl RepairTransport for HttpRepair {
    fn exchange(&self, request: Request) -> BoxFuture<'static, Result<Reply, Error>> {
        let url = self.url.clone();
        self.requests.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let response = reqwest::Client::new()
                .post(url)
                .bearer_auth("sync3-user@sync3-org")
                .json(&request)
                .send()
                .await
                .map_err(|_| Error::Protocol("transport_unavailable".into()))?;
            let body = response
                .bytes()
                .await
                .map_err(|_| Error::Protocol("transport_unavailable".into()))?;
            if body.len() > cypher_proto::sync3::MAX_FRAME_BYTES {
                return Err(Error::Protocol("frame_too_large".into()));
            }
            Ok(serde_json::from_slice(&body)?)
        })
    }
}
async fn wait_cursor(client: &Client, cursor: u64) {
    let mut status = client.watch();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let s = status.borrow_and_update().clone();
            if s.cursor >= cursor && s.phase == Phase::Live {
                break;
            }
            assert!(s.error.is_none(), "unexpected client error: {:?}", s);
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("sync convergence timeout");
}
#[tokio::main]
async fn main() {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:27643".into());
    let parsed = reqwest::Url::parse(&base).unwrap();
    assert_eq!(parsed.scheme(), "http");
    assert!(
        matches!(parsed.host_str(), Some("127.0.0.1") | Some("localhost")),
        "local-only test"
    );
    let room = std::env::args()
        .nth(2)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    assert!(cypher_proto::sync3::valid_id(&room));
    let path = format!("{base}/sync3/sync3-org/chats/{room}/");
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
    let head = fixture["operations"].as_array().unwrap().len() as u64;
    if std::env::args().nth(3).as_deref() == Some("--writer") {
        writer_smoke(
            &path,
            fixture,
            std::env::args().nth(4).expect("report path"),
        )
        .await;
        return;
    }
    if matches!(
        std::env::args().nth(3).as_deref(),
        Some("--write-journal" | "--verify-journal")
    ) {
        let file = std::path::PathBuf::from(std::env::args().nth(4).expect("test database path"));
        let writing = std::env::args().nth(3).as_deref() == Some("--write-journal");
        assert_eq!(
            file.exists(),
            !writing,
            "never overwrite/reseed a test database"
        );
        let numbers: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/numbers.json")).unwrap();
        let mut journal = Journal::open(&file, "account", "shared-room", "phone").unwrap();
        if writing {
            journal
                .accept_state(&Reply::State {
                    version: 3,
                    epoch: 1,
                    owner: "host".into(),
                    owner_epoch: 1,
                    head,
                })
                .unwrap();
            let operations: Vec<Operation> =
                serde_json::from_value(fixture["operations"].clone()).unwrap();
            let rows = operations
                .into_iter()
                .enumerate()
                .map(|(i, operation)| cypher_proto::sync3::Row {
                    seq: i as u64 + 1,
                    operation,
                })
                .collect();
            journal
                .apply_page(&Reply::Page {
                    version: 3,
                    epoch: 1,
                    through: head,
                    next: head,
                    done: true,
                    rows,
                })
                .unwrap();
            journal
                .enqueue(&serde_json::from_value(numbers["source"].clone()).unwrap())
                .unwrap();
            println!("PASS: Rust wrote a private normalized SQLite journal for Swift");
        } else {
            assert_eq!(journal.cursor().unwrap(), head + 2);
            let window = journal.message_window(None, 32).unwrap();
            assert_eq!(window.through, head + 2);
            assert_eq!(window.messages.len(), 1);
            assert_eq!(window.messages[0].created_seq, 4);
            assert_eq!(
                journal.projection().unwrap().commands["command"]
                    .command
                    .status,
                cypher_proto::SessionCommandStatus::Applied
            );
            // Re-enqueue the original Rust body after Swift has written the
            // receipt: JSON key ordering must not create a phantom conflict.
            journal
                .enqueue(&serde_json::from_value(numbers["source"].clone()).unwrap())
                .unwrap();
            assert!(journal.pending().unwrap().is_empty());
            assert_eq!(
                serde_json::to_value(
                    &journal.projection().unwrap().commands["numeric-command"].command
                )
                .unwrap(),
                numbers["canonical"]["event"]["command"]
            );
            println!("PASS: Rust reopened Swift's updates to the same SQLite file");
        }
        return;
    }
    if std::env::args().nth(3).as_deref() == Some("--verify-swift") {
        let dir = tempfile::tempdir().unwrap();
        let url = Arc::new(StaticUrl(format!(
            "{}ws?token=sync3-user%40sync3-org",
            path.replacen("http:", "ws:", 1)
        )));
        let reader = Client::spawn(
            Journal::open(
                &dir.path().join("reader.sqlite"),
                "account",
                &path,
                "rust-reader",
            )
            .unwrap(),
            url,
            None,
            Tuning::default(),
        );
        wait_cursor(&reader, head + 1).await;
        let projection = reader.journal().lock().unwrap().projection().unwrap();
        assert_eq!(
            serde_json::to_value(&projection).unwrap()["messages"]["message"]["entry"]["parts"][0]
                ["text"],
            "你好!"
        );
        assert_eq!(projection.commands["swift-command"].actor, "swift-reader");
        reader.shutdown().await;
        println!("PASS: Rust reads the command committed by Swift through real workerd");
        return;
    }
    let response = reqwest::Client::new()
        .post(format!("{path}init"))
        .bearer_auth("sync3-user@sync3-org")
        .json(&serde_json::json!({"owner":"host"}))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "initialization failed: {}",
        response.status()
    );
    let ops: Vec<Operation> = serde_json::from_value(fixture["operations"].clone()).unwrap();
    let expected: Projection = serde_json::from_value(fixture["projection"].clone()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let repair: Arc<dyn RepairTransport> = Arc::new(HttpRepair {
        url: format!("{path}exchange"),
        requests: count.clone(),
    });
    let url = Arc::new(StaticUrl(format!(
        "{}ws?token=sync3-user%40sync3-org",
        path.replacen("http:", "ws:", 1)
    )));
    let host = Client::spawn(
        Journal::open(&dir.path().join("host.sqlite"), "account", &path, "host").unwrap(),
        url.clone(),
        Some(repair.clone()),
        Tuning::default(),
    );
    let phone_path = dir.path().join("phone.sqlite");
    let phone = Client::spawn(
        Journal::open(&phone_path, "account", &path, "phone").unwrap(),
        url.clone(),
        Some(repair.clone()),
        Tuning::default(),
    );
    wait_cursor(&host, 0).await;
    wait_cursor(&phone, 0).await;
    phone.enqueue(&ops[0]).unwrap();
    wait_cursor(&host, 1).await;
    for op in ops.iter().skip(1) {
        host.enqueue(op).unwrap();
    }
    wait_cursor(&phone, head).await;
    wait_cursor(&host, head).await;
    assert_eq!(
        phone.journal().lock().unwrap().projection().unwrap(),
        expected
    );
    assert_eq!(
        host.journal().lock().unwrap().projection().unwrap(),
        expected
    );
    phone.shutdown().await;
    let restarted = Client::spawn(
        Journal::open(&phone_path, "account", &path, "phone").unwrap(),
        url,
        Some(repair),
        Tuning::default(),
    );
    wait_cursor(&restarted, head).await;
    assert!(
        restarted
            .journal()
            .lock()
            .unwrap()
            .pending()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        restarted.journal().lock().unwrap().projection().unwrap(),
        expected
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "healthy sync must never use HTTP fallback"
    );
    restarted.shutdown().await;
    host.shutdown().await;
    println!(
        "PASS: workerd ↔ Rust host/phone; {head} typed events; UTF-8; restart; healthy HTTP repairs=0"
    );
}

async fn writer_smoke(path: &str, fixture: serde_json::Value, report_path: String) {
    use cypher_proto::{
        AgentEvent, DoneStatus, MessagePart, MessageRole, MessageStatus, SessionMessageEntry,
        ToolCall,
        parts::{fold_event_into_parts, render_parts},
    };
    use cypher_sync::sync3::execution::{Plan, Progress as ExecutionProgress};
    use sha2::{Digest, Sha256};
    let response = reqwest::Client::new()
        .post(format!("{path}init"))
        .bearer_auth("sync3-user@sync3-org")
        .json(&serde_json::json!({"owner":"host"}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let dir = tempfile::tempdir().unwrap();
    let repairs = Arc::new(AtomicUsize::new(0));
    let url = Arc::new(StaticUrl(format!(
        "{}ws?token=sync3-user%40sync3-org",
        path.replacen("http:", "ws:", 1)
    )));
    let repair: Arc<dyn RepairTransport> = Arc::new(HttpRepair {
        url: format!("{path}exchange"),
        requests: repairs.clone(),
    });
    let host = Client::spawn(
        Journal::open(&dir.path().join("host.sqlite"), "account", path, "host").unwrap(),
        url.clone(),
        Some(repair.clone()),
        Tuning::default(),
    );
    let phone = Client::spawn(
        Journal::open(&dir.path().join("phone.sqlite"), "account", path, "phone").unwrap(),
        url,
        Some(repair),
        Tuning::default(),
    );
    wait_cursor(&host, 0).await;
    wait_cursor(&phone, 0).await;
    let prefix: Vec<Operation> = serde_json::from_value(fixture["operations"].clone()).unwrap();
    phone.enqueue(&prefix[0]).unwrap();
    wait_cursor(&host, 1).await;
    host.prepare_execution("command", Plan::Run).unwrap();
    wait_cursor(&host, 2).await;
    assert!(matches!(
        host.advance_execution("command").unwrap(),
        ExecutionProgress::WaitingForRun
    ));
    wait_cursor(&host, 3).await;
    let ExecutionProgress::Dispatch(permit) = host.advance_execution("command").unwrap() else {
        panic!("missing durable permit")
    };
    assert!(matches!(
        host.advance_execution("command").unwrap(),
        ExecutionProgress::RecoveryRequired
    ));
    let entry = SessionMessageEntry {
        id: "writer-message".into(),
        role: MessageRole::Assistant,
        device_id: "host".into(),
        created_at: 1000,
        parts: vec![],
        status: Some(MessageStatus::Streaming),
        continuation_of: None,
    };
    let mut writer = host
        .journal()
        .lock()
        .unwrap()
        .new_execution_writer(&permit, &entry)
        .unwrap();
    let source = vec![
        AgentEvent::TextDelta {
            text: "Before tool".into(),
        },
        AgentEvent::ToolCall {
            id: "tool".into(),
            call: ToolCall::WriteFile {
                path: "fixture".into(),
                content: Some("fixture-private-input".repeat(100_000)),
            },
        },
        AgentEvent::ToolProgress {
            id: "tool".into(),
            output: "working".into(),
        },
        AgentEvent::TextDelta {
            text: "你好🙂e\u{301}\n\"\\\0".repeat(70_000),
        },
    ];
    let mut parts = Vec::new();
    let mut source_seq = 0;
    for event in source {
        source_seq = host
            .append_execution_events(&permit, source_seq + 1, std::slice::from_ref(&event))
            .unwrap();
        fold_event_into_parts(&mut parts, &event);
    }
    let first = writer
        .sync(&parts, |frame| {
            host.enqueue_execution_frame(&permit, source_seq, frame)
        })
        .unwrap();
    assert!(first.more);
    let mut head = 3 + first.operations as u64;
    // Simulate producer destruction after durable enqueue, independently of
    // whether the transport has already received its ACK.
    drop(writer);
    let mut writer = host
        .journal()
        .lock()
        .unwrap()
        .load_writer("writer-message")
        .unwrap()
        .unwrap();
    loop {
        let progress = writer
            .sync(&parts, |frame| {
                host.enqueue_execution_frame(&permit, source_seq, frame)
            })
            .unwrap();
        head += progress.operations as u64;
        if !progress.more {
            break;
        }
    }
    for event in [
        AgentEvent::ToolResult {
            id: "tool".into(),
            is_error: false,
            output: Some("File written".into()),
            diff: None,
        },
        AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        },
    ] {
        source_seq = host
            .append_execution_events(&permit, source_seq + 1, std::slice::from_ref(&event))
            .unwrap();
        fold_event_into_parts(&mut parts, &event);
    }
    loop {
        let progress = writer
            .finish(&parts, Some(MessageStatus::Complete), |frame| {
                host.enqueue_execution_frame(&permit, source_seq, frame)
            })
            .unwrap();
        head += progress.operations as u64;
        if !progress.more {
            break;
        }
    }
    host.complete_execution(
        &permit,
        Some(cypher_proto::sync3::Outcome::Completed),
        cypher_proto::SessionCommandStatus::Applied,
        None,
    )
    .unwrap();
    head += 2;
    wait_cursor(&host, head).await;
    wait_cursor(&phone, head).await;
    let projection = phone.journal().lock().unwrap().projection().unwrap();
    let mut messages = projection.messages.into_values().collect::<Vec<_>>();
    messages.sort_by_key(|m| m.created_seq);
    assert!(messages.len() > 3);
    let mut actual = Vec::<MessagePart>::new();
    for message in &messages {
        assert_eq!(message.entry.status, Some(MessageStatus::Complete));
        for part in &message.entry.parts {
            if let MessagePart::Text { id, text } = part {
                if let Some(MessagePart::Text {
                    id: old_id,
                    text: old_text,
                }) = actual.last_mut()
                {
                    if old_id == id {
                        old_text.push_str(text);
                        continue;
                    }
                }
            }
            actual.push(part.clone());
        }
    }
    let expected = render_parts(&parts);
    assert_eq!(actual, expected);
    let mut value = serde_json::to_value(expected).unwrap();
    value.sort_all_objects();
    let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()));
    std::fs::write(
        report_path,
        serde_json::to_vec(&serde_json::json!({
            "head":head, "partsDigest":digest, "messages":messages.len()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(repairs.load(Ordering::SeqCst), 0);
    host.shutdown().await;
    phone.shutdown().await;
    println!(
        "PASS: bounded Rust producer → real workerd → Rust reader; Unicode rollover, late tool resolution, durable producer restart; HTTP repairs=0"
    );
}
