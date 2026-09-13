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

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_doc::{MessagePart, SessionCommandEntry, SessionCommandPayload, SessionCommandStatus};
use cypher_engine::{EngineCore, HarnessRegistry};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, PendingAttachment, ReasoningLevel, RunRequest,
    SandboxLevel, SteeringMode,
};

const CHAT: &str = "chat-queue-first";

/// Records every RunRequest the harness receives (dispatch-side assertions).
#[derive(Clone, Default)]
struct RecordingHarness {
    requests: Arc<Mutex<Vec<RunRequest>>>,
}

#[async_trait]
impl Harness for RecordingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Recorder"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.requests.lock().unwrap().push(request.clone());
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "sess-qf".into(),
                assistant_message_id: "a-1".into(),
            }),
            Ok(AgentEvent::TextDelta {
                text: format!("ack: {}", request.prompt),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("sess-qf".into()),
            }),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
}

async fn wait_for<F>(mut predicate: F, what: &str)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn run_payload(message_id: &str, pending: Vec<PendingAttachment>) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: RunRequest {
            prompt: "look at the photo".into(),
            harness: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: "/tmp".into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            attachments: Vec::new(),
            pending_attachments: pending,
            resume: None,
            worktree: None,
        },
        message_id: message_id.into(),
        agent_prompt: None,
    }
}

fn command_status(core: &EngineCore) -> Vec<(String, SessionCommandStatus, Option<String>)> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_commands().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|c| (c.id, c.status, c.resolution))
        .collect()
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

async fn assemble() -> (EngineCore, RecordingHarness, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let harness = RecordingHarness::default();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(harness.clone()));
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    (core, harness, tmp)
}

/// The composer's queue-first send over the real RPC surface: QueueCommand
/// first (with pending descriptors, bare prompt), UploadChunk/UploadCommit
/// with chatId after. The host must hold the Run until the seal, then execute
/// with the final path — and the pending id must never reach the transcript.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_then_commit_seal_releases_the_run_with_final_path() {
    let (core, harness, _tmp) = assemble().await;
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
        harness.requests.lock().unwrap().is_empty(),
        "Run must not execute before its attachments are sealed"
    );
    let statuses = command_status(&core);
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
        || !harness.requests.lock().unwrap().is_empty(),
        "run executes after the attachment seal",
    )
    .await;
    let req = harness
        .requests
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

    let statuses = command_status(&core);
    assert_eq!(statuses[0].1, SessionCommandStatus::Applied);
    core.shutdown().await;
}

/// A Run whose uploads never seal must resolve Expired once past the
/// attachment grace window — never Pending forever, never executed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsealed_run_expires_after_grace_not_pending_forever() {
    let (core, harness, _tmp) = assemble().await;

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
            command_status(&core)
                .iter()
                .any(|(_, s, _)| *s == SessionCommandStatus::Expired)
        },
        "stale run expires after the grace window",
    )
    .await;
    assert!(
        harness.requests.lock().unwrap().is_empty(),
        "an unsealed Run must never dispatch"
    );
    assert!(
        !transcript_contains(&core, "pending/up-dead"),
        "pending UI ref never entered the transcript"
    );
    core.shutdown().await;
}
