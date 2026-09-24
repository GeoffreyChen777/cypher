//! Opt-in local integration fixture. Runs two real EngineCores with mock only;
//! iOS Simulator connects to the same local workerd room and submits a Run.
use cypher_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use cypher_engine::{EdgeConfig, EngineCore, HarnessRegistry};
use cypher_harness::mock::MockHarness;
use cypher_proto::{AgentEvent, DoneStatus, HarnessId};
use cypher_sync::preview_link::PreviewOptions;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let control = std::path::PathBuf::from(std::env::var("CYPHER_PREVIEW_TEST_CONTROL")?);
    let edge_url = std::env::var("CYPHER_PREVIEW_TEST_EDGE")?;
    let url = reqwest::Url::parse(&edge_url)?;
    anyhow::ensure!(
        url.scheme() == "http" && url.host_str() == Some("127.0.0.1"),
        "local fixture only"
    );
    let host_dir = tempfile::tempdir()?;
    let viewer_dir = tempfile::tempdir()?;
    let registry = Arc::new(HarnessRegistry::new());
    let fragment = "native-preview-你好 👋 ";
    let expected = fragment.repeat(40);
    let mut script: Vec<_> = (0..40)
        .map(|_| AgentEvent::TextDelta {
            text: fragment.into(),
        })
        .collect();
    script.push(AgentEvent::Done {
        status: DoneStatus::Completed,
        result: None,
        error: None,
        session_id: None,
    });
    registry.register(Arc::new(MockHarness { script }));
    let mut host_edge = EdgeConfig::with_static_token(&edge_url, "local-preview-viewer");
    host_edge.preview = Some(PreviewOptions {
        publisher_token: Some("a".repeat(64)),
    });
    let host = EngineCore::assemble(host_dir.path(), registry, HarnessId::Mock, Some(host_edge))?;
    let mut viewer_edge = EdgeConfig::with_static_token(&edge_url, "local-preview-viewer");
    viewer_edge.preview = Some(PreviewOptions {
        publisher_token: None,
    });
    let viewer = EngineCore::assemble(
        viewer_dir.path(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        Some(viewer_edge),
    )?;
    let chat = format!("preview-{}", uuid::Uuid::new_v4());
    // Exercise WatchDocMessages-before-CreateChat: preview starts as a viewer
    // and must upgrade via redial without discarding its durable pending queue.
    let source = host.doc_host.open(&chat)?;
    host.workspace.create_chat(
        &chat,
        None,
        Some(&host.device_id),
        None,
        Some(host_dir.path().to_string_lossy().into()),
    )?;
    viewer
        .workspace
        .create_chat(&chat, None, Some(&host.device_id), None, None)?;
    let target = viewer.doc_host.open(&chat)?;
    let mut shown = target.watch_messages();
    source.doc().push_message(&SessionMessageEntry {
        id: "seed".into(),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Text {
            id: "t".into(),
            text: "preview fixture ready".into(),
            agent_text: None,
        }],
        created_at: 1,
        device_id: host.device_id.clone(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
        completed_at: None,
        comments: Vec::new(),
    })?;
    std::fs::write(
        &control,
        serde_json::to_vec(
            &serde_json::json!({"edge":edge_url,"chatId":chat,"hostDeviceId":host.device_id,
        "cwd":host_dir.path(),"expected":expected}),
        )?,
    )?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut saw_preview = false;
    loop {
        for entry in shown.borrow_and_update().iter() {
            saw_preview |= entry
                .parts
                .iter()
                .any(|p| matches!(p, MessagePart::Text{text,..} if text.contains("实时预览")));
        }
        let ios_receipt = control.with_extension("ios.json");
        if ios_receipt.exists() {
            let ios: serde_json::Value = serde_json::from_slice(&std::fs::read(ios_receipt)?)?;
            anyhow::ensure!(ios["passed"] == true, "iOS fixture did not pass: {ios}");
            let source_entries = source.doc().read_entries()?;
            let target_entries = target.doc().read_entries()?;
            if source_entries == target_entries && saw_preview {
                anyhow::ensure!(
                    source.doc().preview_coverage().is_some_and(|c| c.complete),
                    "no final durable coverage"
                );
                anyhow::ensure!(
                    source.doc().read_commands()?.len() == 1,
                    "preview executed or duplicated a command"
                );
                anyhow::ensure!(
                    target_entries.iter().flat_map(|e| &e.parts).all(
                        |p| !matches!(p, MessagePart::Text{text,..} if text.contains("实时预览"))
                    ),
                    "preview leaked into durable doc"
                );
                std::fs::write(
                    control.with_extension("desktop.json"),
                    serde_json::to_vec(
                        &serde_json::json!({"passed":true,"sawPreview":true,"finalDocsEqual":true,"commands":1}),
                    )?,
                )?;
                break;
            }
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "native fixture timed out; desktop preview seen={saw_preview}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    host.shutdown().await;
    viewer.shutdown().await;
    println!(
        "Native desktop fixture passed: preview rendered, final documents equal, exactly one command"
    );
    Ok(())
}
