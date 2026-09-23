//! P0 characterization, NOT a guarantee or a fix: kill an isolated EngineCore
//! process with a real unACKed ChatClient batch, then reopen its stores.
use cypher_doc::{MessagePart, MessageRole, MessageStatus, SessionDoc, SessionMessageEntry};
use cypher_engine::chat2_host::EngineChatSink;
use cypher_engine::profile::EngineProfile;
use cypher_engine::{EngineCore, HarnessRegistry, RunJournal};
use cypher_proto::{AgentEvent, HarnessId};
use cypher_sync::chat_client::{ChatDocSink, CheckpointFetcher};
use cypher_sync::chat_frames::{self as wire, frame_type};
use cypher_sync::{ChatClient, DocsStore, SyncError};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

const CHAT: &str = "p0-crash";
const TAIL: &str = "unacked-text-after-last-snapshot";
struct NoCheckpoint;
impl CheckpointFetcher for NoCheckpoint {
    fn fetch(&self) -> futures::future::BoxFuture<'static, Result<Vec<u8>, SyncError>> {
        Box::pin(async { Err(SyncError::Closed) })
    }
}
fn entry(id: &str) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Text {
            id: "t0".into(),
            text: id.into(),
            agent_text: None,
        }],
        created_at: 1,
        device_id: "p0-host".into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
        completed_at: None,
    }
}
fn contains(doc: &SessionDoc) -> bool {
    doc.read_entries().unwrap().iter().any(|e| e.id == TAIL)
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "helper process, invoked only by the SIGKILL characterization test"]
async fn p0_engine_child() {
    let dir = std::path::PathBuf::from(
        std::env::var("CYPHER_P0_CHILD_DIR").expect("parent provides isolated directory"),
    );
    let profile = EngineProfile::local(&dir).unwrap();
    let core = EngineCore::assemble_with_profile(
        profile.clone(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let handle = core.doc_host.open(CHAT).unwrap();
    let doc = handle.doc_arc();
    doc.push_message(&entry("baseline")).unwrap();
    let store = Arc::new(DocsStore::open(profile.store_root()).unwrap());
    let sink = Arc::new(EngineChatSink::new(&doc, store.clone(), CHAT));
    sink.advance_cursor(1); // last acknowledged baseline, persisted with cursor
    let initial = doc
        .doc()
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/test", listener.local_addr().unwrap());
    let (sent, received) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let mut sent = Some(sent);
        while let Some(Ok(Message::Binary(bytes))) = ws.next().await {
            let frame = wire::decode(&bytes).unwrap();
            let replies = match frame.kind {
                frame_type::HELLO => vec![wire::encode(
                    frame_type::STATE,
                    &serde_json::json!({"headSeq":1,"seqFloor":0,"checkpointSeq":0,"checkpointSize":0,"rowCount":1,"rowBytes":initial.len()}),
                    &[],
                )],
                frame_type::ROWS_REQ => vec![
                    wire::encode(
                        frame_type::ROW,
                        &serde_json::json!({"seq":1,"device":"p0-host","batchId":"baseline"}),
                        &initial,
                    ),
                    wire::encode(
                        frame_type::ROWS_DONE,
                        &serde_json::json!({"headSeq":1}),
                        &[],
                    ),
                ],
                frame_type::PUSH => {
                    if let Some(sent) = sent.take() {
                        sent.send(frame.payload).unwrap();
                    }
                    vec![]
                } // deliberately NO ACK
                _ => vec![],
            };
            for reply in replies {
                ws.send(Message::Binary(reply)).await.unwrap();
            }
        }
    });
    let client = Arc::new(
        ChatClient::connect(&url, sink, Arc::new(NoCheckpoint), "p0-host", 1)
            .await
            .unwrap(),
    );
    let sending = client.clone();
    let _subscription = doc.doc().subscribe_local_update(Box::new(move |bytes| {
        sending.enqueue_update(bytes.clone());
        true
    }));
    let journal = RunJournal::open(profile.store_root().join("journals")).unwrap();
    journal
        .append(CHAT, &AgentEvent::TextDelta { text: TAIL.into() })
        .unwrap();
    doc.push_message(&entry(TAIL)).unwrap();
    let bytes = tokio::time::timeout(Duration::from_secs(5), received)
        .await
        .unwrap()
        .unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(client.stats().pending_pushes, 1);
    assert_eq!(client.stats().cursor, 1);
    if std::env::var("CYPHER_P0_SNAPSHOT").as_deref() == Ok("yes") {
        core.doc_host.flush_all();
    }
    let saved = store.load_snapshot_with_cursor(CHAT).unwrap().unwrap();
    let saved_doc = SessionDoc::from_doc(loro::LoroDoc::new());
    saved_doc.doc().import(&saved.0).unwrap();
    assert!(contains(&saved_doc));
    std::fs::write(
        dir.join("ready"),
        serde_json::to_vec(
            &serde_json::json!({"pending":1,"cursor":1,"tailSaved":contains(&saved_doc)}),
        )
        .unwrap(),
    )
    .unwrap();
    // Freeze this single-threaded executor, preventing the debounce timer
    // from saving before the parent sends SIGKILL. No Drop/graceful shutdown.
    loop {
        std::thread::park();
    }
}

#[tokio::test]
#[cfg(unix)]
async fn sigkill_characterizes_journal_snapshot_and_unacked_queue() {
    use std::os::unix::process::ExitStatusExt;
    for snapshot in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let log = std::fs::File::create(dir.path().join("child.log")).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "p0_engine_child", "--ignored", "--nocapture"])
            .env("CYPHER_P0_CHILD_DIR", dir.path())
            .env("CYPHER_P0_SNAPSHOT", if snapshot { "yes" } else { "no" })
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !dir.path().join("ready").exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!(
                    "child {status}: {}",
                    std::fs::read_to_string(dir.path().join("child.log")).unwrap()
                );
            }
            if Instant::now() > deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("child readiness timeout");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        child.kill().unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(9));
        let profile = EngineProfile::local(dir.path()).unwrap();
        let journal = RunJournal::open(profile.store_root().join("journals")).unwrap();
        let replay = journal.replay(CHAT, 0).unwrap();
        assert!(
            replay
                .iter()
                .any(|(_, event)| matches!(event,AgentEvent::TextDelta{text} if text==TAIL))
        );
        let core = EngineCore::assemble_with_profile(
            profile.clone(),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
        )
        .unwrap();
        let handle = core.doc_host.open(CHAT).unwrap();
        // Durable outbox replay recovers the local update even if the debounce
        // snapshot did not run. The journal alone still is not the replay path.
        assert!(contains(handle.doc()));
        let store = DocsStore::open(profile.store_root()).unwrap();
        assert_eq!(store.load_snapshot_with_cursor(CHAT).unwrap().unwrap().1, 1);
        let sql = rusqlite::Connection::open(profile.store_root().join("docs.sqlite3")).unwrap();
        let tables: Vec<String> = sql
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            tables,
            vec![
                "chat_outbox",
                "processed_commands",
                "schema_migrations",
                "snapshots"
            ]
        );
        println!(
            "P2_CRASH snapshot={snapshot} journal_tail=true transcript_tail=true cursor=1 durable_outbox=true"
        );
        core.shutdown().await;
    }
}
