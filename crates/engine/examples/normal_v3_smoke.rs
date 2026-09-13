//! Normal EngineCore/DocHost/SessionsEngine → real workerd → Swift acceptance.
use cypher_engine::{EdgeConfig, EngineCore, HarnessRegistry};
use cypher_harness::mock::MockHarness;
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, MessagePart, MessageStatus, RunRequest, SandboxLevel,
    SessionCommandPayload, SessionCommandStatus, ToolCall,
};
use futures::{StreamExt, stream::BoxStream};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

struct PersistentMock {
    initial: MockHarness,
    background: String,
    runs: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl Harness for PersistentMock {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Persistent smoke harness"
    }
    fn reasoning_levels(&self) -> &[cypher_proto::ReasoningLevel] {
        &[]
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> cypher_proto::SteeringMode {
        cypher_proto::SteeringMode::StepBoundary
    }
    async fn models(&self) -> Result<Vec<cypher_proto::Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        if request.prompt != "normal command" {
            return Ok(futures::stream::iter(vec![Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            })])
            .boxed());
        }
        assert_eq!(
            self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            0,
            "one process must produce both turns"
        );
        let initial = self.initial.run(request, controls).await?;
        let background = self.background.clone();
        let followup = futures::stream::once(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            Ok(AgentEvent::TextDelta { text: background })
        })
        .chain(futures::stream::iter(vec![Ok(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        })]));
        Ok(initial
            .then(|event| async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                event
            })
            .chain(followup)
            .boxed())
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args().collect();
    let base = &args[1];
    let parsed = reqwest::Url::parse(base).unwrap();
    assert_eq!(parsed.scheme(), "http");
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    let room = &args[2];
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("device-id"), "host").unwrap();
    let response = "正常执行🙂\"\\\n".repeat(60_000);
    let background = "后台续接🙂\n".repeat(20_000);
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(PersistentMock {
        initial: MockHarness {
            script: vec![
                AgentEvent::ToolCall {
                    id: "private-tool".into(),
                    call: ToolCall::WriteFile {
                        path: "file".into(),
                        content: Some("never-replicate-private-source".into()),
                    },
                },
                AgentEvent::TextDelta {
                    text: response.clone(),
                },
                AgentEvent::ToolResult {
                    id: "private-tool".into(),
                    is_error: false,
                    output: Some("done".into()),
                    diff: None,
                },
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                },
            ],
        },
        background: background.clone(),
        runs: Default::default(),
    }));
    let core = EngineCore::assemble_with_identity(
        temp.path(),
        Arc::new(registry),
        HarnessId::Mock,
        Some(EdgeConfig::with_static_token(base, "sync3-user@sync3-org").with_device("host")),
        "sync3-org",
        "sync3-user",
    )
    .unwrap();
    let peer_dir = tempfile::tempdir().unwrap();
    std::fs::write(peer_dir.path().join("device-id"), "peer").unwrap();
    let peer = EngineCore::assemble_with_identity(
        peer_dir.path(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        Some(EdgeConfig::with_static_token(base, "sync3-user@sync3-org").with_device("peer")),
        "sync3-org",
        "sync3-user",
    )
    .unwrap();
    core.set_links(cypher_rpc::workspace3::Links::new(
        core.workspace.rpc_caller().unwrap(),
    ));
    peer.set_links(cypher_rpc::workspace3::Links::new(
        peer.workspace.rpc_caller().unwrap(),
    ));
    let _host_rpc = core.start_workspace_rpc();
    let _peer_host_rpc = peer.start_workspace_rpc();
    core.workspace
        .create_space(
            "normal-space",
            "host",
            &temp.path().to_string_lossy(),
            None,
            false,
        )
        .unwrap();
    core.workspace
        .create_chat(
            room,
            Some("normal-space"),
            None,
            None,
            Some(temp.path().to_string_lossy().into_owned()),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while peer.workspace.chat(room).unwrap().is_none()
            || core
                .workspace
                .read_devices()
                .unwrap()
                .iter()
                .all(|d| d.id != "peer")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let command = core
        .doc_host
        .queue_command(
            room,
            SessionCommandPayload::Run {
                message_id: "normal-user".into(),
                agent_prompt: None,
                request: RunRequest {
                    prompt: "normal command".into(),
                    harness: Some(HarnessId::Mock),
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: temp.path().to_string_lossy().into(),
                    sandbox: SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                    resume: None,
                    attachments: vec![],
                    pending_attachments: vec![],
                    worktree: None,
                },
            },
        )
        .unwrap();
    let replica = core.sessions.session_replica(room).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !peer
            .workspace
            .read_sessions()
            .unwrap()
            .iter()
            .any(|s| s.chat_id == *room && s.status == cypher_proto::SessionStatus::Working)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut status = replica.watch();
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let current = status.borrow_and_update().clone();
            assert!(current.error.is_none(), "{current:?}");
            let projection = replica.read(|j| j.projection()).unwrap();
            if let Some(command) = projection.commands.get(&command) {
                if command.command.status == SessionCommandStatus::Applied {
                    if projection.runs.len() == 2
                        && projection.runs.values().all(|r| r.outcome.is_some())
                        && projection.executions.values().all(|e| e.closed)
                    {
                        break;
                    }
                    status.changed().await.unwrap();
                    continue;
                }
                assert_eq!(
                    command.command.status,
                    SessionCommandStatus::Pending,
                    "{:?}",
                    command.command.resolution
                );
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|error| {
        let p = replica.read(|j| j.projection()).unwrap();
        panic!(
            "{error}: status={:?}, runs={:?}, executions={:?}",
            status.borrow().clone(),
            p.runs,
            p.executions
        );
    });
    let projection = replica.read(|j| j.projection()).unwrap();
    assert_eq!(
        projection.commands.len(),
        1,
        "normal path cannot mint a shadow command"
    );
    assert!(projection.commands[&command].accepted_op_id.is_some());
    assert!(projection.messages.contains_key("normal-user"));
    assert!(
        !serde_json::to_string(&projection.messages)
            .unwrap()
            .contains("never-replicate-private-source")
    );
    let source = replica
        .read(|j| j.execution_events(&command, 0, 32))
        .unwrap();
    assert!(source.events.iter().any(|e| {
        serde_json::to_string(&e.event)
            .unwrap()
            .contains("never-replicate-private-source")
    }));
    let mut messages: Vec<_> = projection.messages.values().collect();
    messages.sort_by_key(|m| m.created_seq);
    assert!(messages.len() > 3);
    let mut parts = Vec::new();
    for message in &messages {
        assert_eq!(message.entry.status, Some(MessageStatus::Complete));
        for part in &message.entry.parts {
            if let MessagePart::Text { id, text } = part {
                if let Some(MessagePart::Text {
                    id: prior,
                    text: tail,
                }) = parts.last_mut()
                {
                    if id == prior {
                        tail.push_str(text);
                        continue;
                    }
                }
            }
            parts.push(part.clone());
        }
    }
    let text: String = parts
        .iter()
        .filter_map(|p| match p {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, format!("normal command{response}{background}"));
    let mut normalized = serde_json::to_value(parts).unwrap();
    normalized.sort_all_objects();
    let mut report = serde_json::json!({
        "messages": messages.len(),
        "partsDigest": format!("{:x}", Sha256::digest(serde_json::to_vec(&normalized).unwrap()))
    });
    assert_eq!(status.borrow().repairs, 0);
    let peer_rpc = cypher_rpc::memory_client(peer.rpc_service());
    let text = "远程完整文件🙂\"\\\n".repeat(10_000);
    std::fs::write(temp.path().join("rpc-proof.txt"), &text).unwrap();
    let result = peer_rpc
        .call(
            cypher_rpc::methods::READ_WORKSPACE_FILE,
            serde_json::json!({
                "targetDeviceId":"host","chatId":room,"cwd":temp.path(),"path":"rpc-proof.txt"
            }),
        )
        .await
        .unwrap();
    assert_eq!(result["text"].as_str(), Some(text.as_str()), "{result}");
    let error = peer_rpc
        .call(
            cypher_rpc::methods::SEARCH_FILES,
            serde_json::json!({
                "targetDeviceId":"host","query":"界".repeat(32_000)
            }),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("must not exceed 256"), "{error}");
    let forbidden = peer
        .workspace
        .rpc_caller()
        .unwrap()
        .call("host", cypher_rpc::methods::SIGN_OUT, serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        forbidden.to_string().contains("remote_method_forbidden"),
        "{forbidden}"
    );
    for _ in 0..12 {
        let mut stream = peer_rpc
            .subscribe(
                cypher_rpc::methods::WATCH_CHECKOUT_DIFFS,
                serde_json::json!({"targetDeviceId":"host"}),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), stream.recv())
            .await
            .unwrap()
            .expect("first remote item");
        drop(stream);
        tokio::time::timeout(Duration::from_secs(5), async {
            while peer.workspace.rpc_caller().unwrap().active_requests() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dropped idle stream released remote route");
    }
    if let Some(swift) = args.get(4) {
        let result = tokio::process::Command::new(swift)
            .args(["--rpc", base, room, &temp.path().to_string_lossy()])
            .status()
            .await
            .unwrap();
        assert!(result.success(), "Swift normal RPC probe failed");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !replica
                .read(|j| {
                    Ok(j.projection()?
                        .attachments
                        .contains_key("swift-native-upload"))
                })
                .unwrap()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("uploaded attachment seal committed to the native conversation");
    }
    peer_rpc
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({"op":"renameChat","chatId":room,"title":"Renamed from peer"}),
        )
        .await
        .unwrap();
    peer_rpc
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({"op":"setChatArchived","chatId":room,"archived":true}),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let chat = core.workspace.chat(room).unwrap().unwrap();
            if chat.title.as_deref() == Some("Renamed from peer")
                && chat.archived
                && peer
                    .workspace
                    .read_sessions()
                    .unwrap()
                    .iter()
                    .all(|s| s.chat_id != *room || s.status != cypher_proto::SessionStatus::Working)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // The upload probe adds a committed attachment event after the text.
    // Freeze the reader's expected ceiling only after every probe is done.
    report["head"] = serde_json::json!(replica.read(|j| j.cursor()).unwrap());
    std::fs::write(&args[3], serde_json::to_vec(&report).unwrap()).unwrap();
    peer.shutdown().await;
    core.shutdown().await;
    println!(
        "PASS: two normal EngineCores → WorkspaceHub/workerd; metadata, presence, peer edits, fragmented RPC input/output, IPC-only rejection; original command, autonomous run and full output"
    );
}
