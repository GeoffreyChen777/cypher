//! Queue-first attachments (P1 Commit 3): a Run queued with PENDING
//! attachment descriptors must NOT execute until every upload id is sealed in
//! the doc; the seal (written by the host's UploadCommit handler) re-triggers
//! the drain, which then resolves the ids to final paths, fills
//! `request.attachments`, appends the `Attached images` refs trailer to the
//! prompt, and runs — with no pending id ever entering the transcript. A Run
//! whose uploads never seal expires within the attachment grace window instead
//! of staying Pending forever, and the whole queue-before-upload ordering is
//! exercised over the real RPC surface (QueueCommand → UploadChunk →
//! UploadCommit{chatId}).

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;

use cypher_doc::{MessagePart, SessionCommandEntry, SessionCommandPayload, SessionCommandStatus};
use cypher_engine::EngineCore;
use cypher_proto::{HarnessId, PendingAttachment, RunRequest};

use common::{command_statuses, run_request};

const CHAT: &str = "chat-queue-first";

async fn wait_for(predicate: impl FnMut() -> bool, what: &str) {
    common::wait_for_within(predicate, what, Duration::from_secs(20)).await;
}

fn run_payload(message_id: &str, pending: Vec<PendingAttachment>) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: RunRequest {
            pending_attachments: pending,
            ..run_request("look at the photo")
        },
        message_id: message_id.into(),
        agent_prompt: None,
    }
}

fn user_entry_text(core: &EngineCore, message_id: &str) -> Option<String> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .and_then(|entries| {
            entries.into_iter().find(|e| e.id == message_id).map(|e| {
                e.parts.iter().fold(String::new(), |mut acc, p| {
                    if let MessagePart::Text { text, .. } = p {
                        acc.push_str(text);
                    }
                    acc
                })
            })
        })
}

fn transcript_contains(core: &EngineCore, needle: &str) -> bool {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .map(|entries| {
            entries.iter().any(|e| {
                e.parts.iter().any(|p| match p {
                    MessagePart::Text { text, .. } => text.contains(needle),
                    _ => false,
                })
            })
        })
        .unwrap_or(false)
}

type RequestLog = Arc<Mutex<Vec<RunRequest>>>;

/// The returned log records every RunRequest the harness receives
/// (dispatch-side assertions).
async fn assemble() -> (EngineCore, RequestLog, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let requests = RequestLog::default();
    let log = requests.clone();
    let harness = common::TestHarness::new(HarnessId::Mock, "Recorder", move |request, _| {
        log.lock().unwrap().push(request.clone());
        common::reply(
            HarnessId::Mock,
            &request,
            "sess-qf",
            &format!("ack: {}", request.prompt),
        )
    });
    let core = common::engine_at(&tmp.path().join("data"), harness);
    (core, requests, tmp)
}

/// The composer's queue-first send over the real RPC surface: QueueCommand
/// first (with pending descriptors, bare prompt), UploadChunk/UploadCommit
/// with chatId after. The host must hold the Run until the seal, then execute
/// with the final path — and the pending id must never reach the transcript.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_then_commit_seal_releases_the_run_with_final_path() {
    let (core, requests, _tmp) = assemble().await;
    let client = cypher_rpc::memory_client(core.rpc_service());
    let upload_id = "up-qf-1";

    // 1. Queue FIRST — no bytes uploaded yet.
    let command = run_payload(
        "msg-qf-1",
        vec![PendingAttachment {
            upload_id: upload_id.into(),
            file_name: "photo.png".into(),
        }],
    );
    let command = serde_json::to_value(&command).unwrap();
    client
        .call(
            cypher_rpc::methods::QUEUE_COMMAND,
            serde_json::json!({ "chatId": CHAT, "command": command }),
        )
        .await
        .expect("QueueCommand");

    // Give the drain a moment: it must NOT execute while unsealed.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        requests.lock().unwrap().is_empty(),
        "Run must not execute before its attachments are sealed"
    );
    let statuses = command_statuses(&core, CHAT);
    assert_eq!(
        statuses[0].1,
        SessionCommandStatus::Pending,
        "command stays Pending while waiting on the seal"
    );
    assert!(
        !transcript_contains(&core, upload_id),
        "pending id must not appear before execution either"
    );

    // 2. Upload the bytes and commit WITH chatId (the composer's post-queue
    // step) — the host's UploadCommit handler seals against the chat.
    let payload: Vec<u8> = (0..=255u8).cycle().take(9_001).collect();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&payload);
    let (first, second) = b64.split_at(b64.len() / 2);
    for (seq, data) in [(0, first), (1, second)] {
        client
            .call(
                cypher_rpc::methods::UPLOAD_CHUNK,
                serde_json::json!({ "uploadId": upload_id, "seq": seq, "data": data }),
            )
            .await
            .expect("UploadChunk");
    }
    let committed = client
        .call(
            cypher_rpc::methods::UPLOAD_COMMIT,
            serde_json::json!({
                "uploadId": upload_id,
                "fileName": "photo.png",
                "chatId": CHAT,
            }),
        )
        .await
        .expect("UploadCommit");
    let path = committed["path"].as_str().expect("path").to_string();
    assert_eq!(
        std::fs::read(&path).expect("durable upload file"),
        payload,
        "committed file holds the reassembled bytes"
    );
    let handle = core.doc_host.open(CHAT).expect("open queued chat");
    assert_eq!(
        handle.doc().sealed_attachment(upload_id).unwrap(),
        Some((path.clone(), "photo.png".into())),
        "UploadCommit must seal the attachment in the chat doc"
    );

    // 3. The seal releases the Run.
    wait_for(
        || !requests.lock().unwrap().is_empty(),
        "run executes after the attachment seal",
    )
    .await;
    let req = requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.prompt.starts_with("look at the photo"))
        .cloned()
        .expect("chat run request");
    assert_eq!(
        req.attachments,
        vec![path.clone()],
        "pending ids resolve to the sealed final path"
    );
    assert!(
        req.pending_attachments.is_empty(),
        "pending descriptors are consumed before dispatch"
    );
    assert!(
        req.prompt.contains(&path),
        "prompt carries the final-path ref trailer"
    );
    assert!(
        !req.prompt.contains(&format!("pending/{upload_id}")),
        "pending UI ref never reaches the agent prompt"
    );

    // 4. The doc user entry carries the final path (thumbnails render back),
    // and the pending id is nowhere in the transcript.
    wait_for(
        || user_entry_text(&core, "msg-qf-1").is_some(),
        "user entry lands",
    )
    .await;
    let text = user_entry_text(&core, "msg-qf-1").unwrap();
    assert!(
        text.contains(&path) && text.contains("Attached images (local files"),
        "transcript user entry carries the real refs trailer: {text}"
    );
    assert!(
        !text.contains(&format!("pending/{upload_id}")),
        "pending UI ref never enters the transcript"
    );
    assert!(
        !transcript_contains(&core, &format!("pending/{upload_id}")),
        "pending UI ref absent from every transcript part"
    );

    let statuses = command_statuses(&core, CHAT);
    assert_eq!(
        statuses[0].1,
        SessionCommandStatus::Applied,
        "statuses: {statuses:?}"
    );
    core.shutdown().await;
}

/// A Run whose uploads never seal must resolve Expired once past the
/// attachment grace window — never Pending forever, never executed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsealed_run_expires_after_grace_not_pending_forever() {
    let (core, requests, _tmp) = assemble().await;

    // Queue directly into the doc with an issued_at already past the grace
    // window (a wedged upload from ~11 minutes ago). `expires_at = None`
    // keeps the 24h default TTL out of the way — the attachment grace is
    // what must trip.
    let handle = core.doc_host.open(CHAT).unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    handle
        .doc()
        .queue_command(&SessionCommandEntry {
            id: "c-stale".into(),
            payload: run_payload(
                "msg-stale",
                vec![PendingAttachment {
                    upload_id: "up-dead".into(),
                    file_name: "x.png".into(),
                }],
            ),
            issued_by: core.device_id.clone(),
            issued_at: now - 11 * 60 * 1000,
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Pending,
            resolution: None,
            sent_at: None,
        })
        .expect("queue stale run");

    wait_for(
        || {
            command_statuses(&core, CHAT)
                .iter()
                .any(|(_, s, _)| *s == SessionCommandStatus::Expired)
        },
        "stale run expires after the grace window",
    )
    .await;
    assert!(
        requests.lock().unwrap().is_empty(),
        "an unsealed Run must never dispatch"
    );
    assert!(
        !transcript_contains(&core, "pending/up-dead"),
        "pending UI ref never entered the transcript"
    );
    core.shutdown().await;
}

/// Arbitrary files ride the same queue-first path as images: a Run queued with
/// an image AND a non-image (non-ASCII name) executes once both seal, the
/// agent gets both final paths, and — because a non-image is present — the
/// trailer switches to the `Attached files` header (image-only sends keep the
/// historical `Attached images` bytes, asserted above).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_image_files_upload_seal_and_reach_the_agent() {
    let (core, requests, _tmp) = assemble().await;
    let client = cypher_rpc::memory_client(core.rpc_service());
    let files = [
        ("up-file-img", "shot.png", b"\x89PNG\r\n\x1a\n".to_vec()),
        (
            "up-file-pdf",
            "季度 报告.pdf",
            b"%PDF-1.7\n%\xe2\xe3\n".to_vec(),
        ),
    ];
    let pending = files
        .iter()
        .map(|(id, name, _)| PendingAttachment {
            upload_id: (*id).into(),
            file_name: (*name).into(),
        })
        .collect();
    let command = serde_json::to_value(run_payload("msg-files", pending)).unwrap();
    client
        .call(
            cypher_rpc::methods::QUEUE_COMMAND,
            serde_json::json!({ "chatId": CHAT, "command": command }),
        )
        .await
        .expect("QueueCommand");

    let mut paths = Vec::new();
    for (id, name, bytes) in &files {
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        client
            .call(
                cypher_rpc::methods::UPLOAD_CHUNK,
                serde_json::json!({ "uploadId": id, "seq": 0, "data": data }),
            )
            .await
            .expect("UploadChunk");
        let committed = client
            .call(
                cypher_rpc::methods::UPLOAD_COMMIT,
                serde_json::json!({ "uploadId": id, "fileName": name, "chatId": CHAT }),
            )
            .await
            .expect("UploadCommit");
        let path = committed["path"].as_str().expect("path").to_string();
        assert_eq!(&std::fs::read(&path).unwrap(), bytes, "{name} bytes intact");
        paths.push(path);
    }
    assert!(
        paths[1].ends_with("-季度_报告.pdf"),
        "unicode letters survive name sanitizing: {}",
        paths[1]
    );

    // Titling runs through the same harness, so its request can land first.
    let chat_run = || {
        requests
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.prompt.starts_with("look at the photo"))
            .cloned()
    };
    wait_for(|| chat_run().is_some(), "run executes after both seals").await;
    let req = chat_run().expect("chat run request");
    assert_eq!(req.attachments, paths, "both final paths reach the harness");
    assert!(
        req.prompt
            .contains("\n\nAttached files (local files — open them to view):\n"),
        "mixed send uses the files header: {}",
        req.prompt
    );
    for path in &paths {
        assert!(req.prompt.contains(&format!("- {path}")), "ref for {path}");
    }
    core.shutdown().await;
}
