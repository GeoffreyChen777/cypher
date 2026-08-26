//! Temporary Side Chats (round 21): engine-side integration.
//!
//! - `StartSideChat` mints an ephemeral chat: NO workspace row, NO public
//!   `WatchSessions` entry, no snapshot; validates the selection (non-empty,
//!   ≤64 KiB) and the parent (must exist and be hosted by this engine), and
//!   enforces the GLOBAL 8-chat cap;
//! - `SendSideChat`'s FIRST send injects the stored selection IN FULL +
//!   bounded parent context into the EFFECTIVE prompt (the doc user entry
//!   keeps the visible prompt); a FAILED dispatch keeps the chat first-send-
//!   eligible so a retry still injects; later sends resume normally;
//! - the run executes through the normal harness and `WatchSideChatStatus`
//!   streams the PRIVATE status (the public session list never sees it);
//! - `PromoteSideChat` turns it into a normal root chat (persisted snapshot →
//!   workspace row with a quote-derived title + the parent's device/cwd/
//!   config, room_gen 2, public status backfill) and is idempotent; a failed
//!   promotion retains the temporary state and is retryable;
//! - `DisposeSideChat` tears an unpromoted chat down with no durable
//!   remnants (no row, no status, no snapshot) and never deletes a durable
//!   promoted chat;
//! - `EngineCore::shutdown` reaps unpromoted chats.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_engine::{EngineCore, HarnessRegistry};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, ChatConfig, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionStatus, SteeringMode,
};
use cypher_rpc::{RpcError, RpcReply, RpcService, methods};

const PARENT: &str = "chat-parent";
const SELECTED: &str = "the exact selected quote for this side chat";

/// One-liner harness that RECORDS every RunRequest (the effective-prompt
/// assertion) and streams a quick Done.
struct RecordingHarness {
    requests: Arc<Mutex<Vec<RunRequest>>>,
}

#[async_trait]
impl Harness for RecordingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }
    fn display_name(&self) -> &str {
        "Recording"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
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
        self.requests.lock().unwrap().push(request);
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Pi,
                model: "model-x".into(),
                tools: vec![],
                cwd: "/tmp/repo".into(),
                session_id: "hs-1".into(),
                assistant_message_id: "a-1".into(),
            }),
            Ok(AgentEvent::TextDelta {
                text: "side answer".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: Some("side answer".into()),
                error: None,
                session_id: None,
            }),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
}

struct Rig {
    core: EngineCore,
    requests: Arc<Mutex<Vec<RunRequest>>>,
    _dir: tempfile::TempDir,
}

fn assemble() -> Rig {
    let registry = HarnessRegistry::new();
    let requests = Arc::new(Mutex::new(Vec::new()));
    registry.register(Arc::new(RecordingHarness {
        requests: requests.clone(),
    }));
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Pi, None)
        .expect("engine core assembles");
    Rig {
        core,
        requests,
        _dir: dir,
    }
}

/// Assemble the engine behind an `Arc` so concurrent tests can hand the same
/// core to many spawned tasks at once.
fn assemble_arc() -> (
    Arc<EngineCore>,
    Arc<Mutex<Vec<RunRequest>>>,
    tempfile::TempDir,
) {
    let registry = HarnessRegistry::new();
    let requests = Arc::new(Mutex::new(Vec::new()));
    registry.register(Arc::new(RecordingHarness {
        requests: requests.clone(),
    }));
    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(
        EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Pi, None)
            .expect("engine core assembles"),
    );
    (core, requests, dir)
}

async fn rpc(
    core: &EngineCore,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    match core.rpc_service().handle(method, params).await? {
        RpcReply::Value(value) => Ok(value),
        _ => Err(RpcError::Failed("expected unary reply".into())),
    }
}

fn wait_for(cond: impl Fn() -> bool, what: &str) {
    for _ in 0..400 {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

/// Seed ONLY the parent chat row (no transcript) — for the empty-parent-\
/// context first-send case.
async fn seed_parent_row(core: &EngineCore) {
    core.workspace
        .create_chat(
            PARENT,
            None,
            Some(core.device_id.as_str()),
            Some(ChatConfig {
                harness: HarnessId::Pi,
                model: Some("parent-model".into()),
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            }),
            Some("/tmp/repo".into()),
        )
        .expect("parent chat row");
}

/// Seed the parent chat row + a two-entry transcript (user m1, assistant a-1)
/// so the side chat's first send has parent context to inject.
async fn seed_parent(core: &EngineCore) {
    seed_parent_row(core).await;
    core.sessions
        .dispatch(
            PARENT,
            HarnessId::Pi,
            RunRequest {
                prompt: "parent first question".into(),
                harness: Some(HarnessId::Pi),
                model: Some("parent-model".into()),
                reasoning: None,
                model_options: Default::default(),
                cwd: "/tmp/repo".into(),
                sandbox: SandboxLevel::WorkspaceWrite,
                auto_approve: false,
                resume: None,
                worktree: None,
                attachments: Vec::new(),
                pending_attachments: Vec::new(),
            },
            Some("m1".into()),
        )
        .await
        .expect("parent dispatch");
    wait_for(
        || {
            core.sessions
                .session_status(PARENT)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "parent run to settle",
    );
}

async fn start_side_chat(core: &EngineCore) -> String {
    start_side_chat_with(core, SELECTED).await
}

async fn start_side_chat_with(core: &EngineCore, selected: &str) -> String {
    let reply = rpc(
        core,
        methods::START_SIDE_CHAT,
        serde_json::json!({
            "parentChatId": PARENT,
            "source": { "kind": "transcript", "anchorMessageId": "m1" },
            "selectedText": selected,
        }),
    )
    .await
    .expect("StartSideChat ok");
    assert_eq!(
        reply.get("parentChatId").and_then(|v| v.as_str()),
        Some(PARENT)
    );
    assert_eq!(
        reply.get("targetDeviceId").and_then(|v| v.as_str()),
        Some(core.device_id.as_str())
    );
    reply
        .get("sideChatId")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .expect("sideChatId in reply")
}

#[tokio::test(flavor = "multi_thread")]
async fn start_send_promote_flow() {
    let rig = assemble();
    seed_parent(&rig.core).await;

    let side = start_side_chat(&rig.core).await;

    // Ephemeral: no workspace row, no public status, private status watch.
    assert!(
        rig.core.workspace.chat(&side).unwrap().is_none(),
        "no row before promote"
    );
    assert!(
        rig.core.sessions.session_status(&side).is_none(),
        "no public status"
    );
    let handle = rig.core.doc_host.open(&side).expect("ephemeral handle");
    assert!(handle.is_ephemeral(), "host-memory doc until promotion");

    // FIRST send: effective prompt carries the selected quote IN FULL plus
    // the bounded parent context; the doc entry keeps the visible prompt.
    let before = rig.requests.lock().unwrap().len();
    rpc(
        &rig.core,
        methods::SEND_SIDE_CHAT,
        serde_json::json!({
            "sideChatId": side,
            "messageId": "s1",
            "request": {
                "prompt": "side question one",
                "harness": "pi",
                "cwd": "/tmp/repo",
                "sandbox": "workspace-write",
            },
        }),
    )
    .await
    .expect("SendSideChat ok");
    wait_for(
        || rig.requests.lock().unwrap().len() == before + 1,
        "side run to reach the harness",
    );
    let first = rig.requests.lock().unwrap()[before].clone();
    assert!(
        first
            .prompt
            .contains(&format!("Selected text:\n{SELECTED}")),
        "first send injects the selected quote in full: {}",
        first.prompt
    );
    assert!(
        first
            .prompt
            .contains("Parent chat context:\nuser: parent first question"),
        "first send injects the bounded parent context: {}",
        first.prompt
    );
    assert!(
        first.prompt.ends_with("User request:\nside question one"),
        "effective prompt ends with the visible user request untouched: {}",
        first.prompt
    );
    wait_for(
        || {
            rig.core
                .sessions
                .session_status(&side)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "side run to settle",
    );
    // Public session list never sees the temporary chat.
    let public = rig.core.sessions.watch_sessions().borrow().clone();
    assert!(
        !public.iter().any(|s| s.chat_id == side),
        "temp side chat is absent from the public sessions list"
    );
    // The doc entry kept the visible prompt (the quote/context never land in
    // the user's visible doc entry).
    let entries = rig
        .core
        .doc_host
        .open(&side)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    let user = entries.iter().find(|e| e.id == "s1").expect("user entry");
    let user_text: String = user
        .parts
        .iter()
        .filter_map(|p| match p {
            cypher_doc::MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(user_text, "side question one");

    // WatchSideChatStatus: subscribe BEFORE the second send so the stream
    // catches the run's Working→Idle transitions live (the first send already
    // settled by now). Frames are collected by a spawned task.
    let reply = rig
        .core
        .rpc_service()
        .handle(
            methods::WATCH_SIDE_CHAT_STATUS,
            serde_json::json!({ "sideChatId": side }),
        )
        .await
        .expect("status watch starts");
    let RpcReply::Stream(stream) = reply else {
        panic!("WatchSideChatStatus must stream");
    };
    let statuses: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let statuses = statuses.clone();
        let mut stream = Box::pin(stream);
        tokio::spawn(async move {
            while let Some(frame) = stream.next().await {
                if let Some(status) = frame.get("status").and_then(|v| v.as_str()) {
                    statuses.lock().unwrap().push(status.to_string());
                }
            }
        });
    }

    // SECOND send: no injection.
    let before2 = rig.requests.lock().unwrap().len();
    rpc(
        &rig.core,
        methods::SEND_SIDE_CHAT,
        serde_json::json!({
            "sideChatId": side,
            "messageId": "s2",
            "request": {
                "prompt": "side question two",
                "harness": "pi",
                "cwd": "/tmp/repo",
                "sandbox": "workspace-write",
            },
        }),
    )
    .await
    .expect("second SendSideChat ok");
    wait_for(
        || rig.requests.lock().unwrap().len() == before2 + 1,
        "second side run to reach the harness",
    );
    assert_eq!(
        rig.requests.lock().unwrap()[before2].prompt,
        "side question two"
    );
    wait_for(
        || {
            rig.core
                .sessions
                .session_status(&side)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "second side run to settle",
    );
    // Give the collector a beat to drain the watch's current value.
    std::thread::sleep(Duration::from_millis(100));
    let seen = statuses.lock().unwrap().clone();
    assert!(
        seen.iter().any(|s| s == "working"),
        "status watch streams Working: {seen:?}"
    );
    assert!(
        seen.iter().any(|s| s == "idle"),
        "status watch streams the settled Idle: {seen:?}"
    );

    // Promote: normal root chat with a quote-derived title + the parent's
    // device/cwd/config, on chat2.
    let promoted = rpc(
        &rig.core,
        methods::PROMOTE_SIDE_CHAT,
        serde_json::json!({ "sideChatId": side }),
    )
    .await
    .expect("PromoteSideChat ok");
    assert_eq!(
        promoted.get("chatId").and_then(|v| v.as_str()),
        Some(side.as_str())
    );
    let row = rig
        .core
        .workspace
        .chat(&side)
        .unwrap()
        .expect("row exists after promote");
    assert_eq!(row.device_id, rig.core.device_id, "parent's device");
    assert_eq!(row.cwd.as_deref(), Some("/tmp/repo"), "parent's cwd");
    assert_eq!(row.room_gen, Some(2), "born on chat2");
    assert!(row.child.is_none(), "a normal ROOT chat, not a child");
    assert_eq!(
        row.config.as_ref().and_then(|c| c.model.clone()).as_deref(),
        Some("parent-model"),
        "inherits the parent's config"
    );
    assert_eq!(
        row.title.as_deref(),
        Some("the exact selected quote for"),
        "promoted row carries a deterministic quote-derived title"
    );
    let handle = rig.core.doc_host.open(&side).expect("promoted handle");
    assert!(
        !handle.is_ephemeral(),
        "same handle now serves a normal chat"
    );
    assert!(
        rig.core.sessions.session_status(&side).is_some(),
        "promoted chat has a public status"
    );
    // Public status backfilled: the promoted chat appears in WatchSessions.
    let public = rig.core.sessions.watch_sessions().borrow().clone();
    assert!(
        public.iter().any(|s| s.chat_id == side),
        "promoted chat appears in the public sessions list"
    );

    // Idempotent retry: a lost PromoteSideChat reply retried returns the same id.
    let retried = rpc(
        &rig.core,
        methods::PROMOTE_SIDE_CHAT,
        serde_json::json!({ "sideChatId": side }),
    )
    .await
    .expect("promote retry ok");
    assert_eq!(
        retried.get("chatId").and_then(|v| v.as_str()),
        Some(side.as_str()),
        "retry returns the same chat without double-promoting"
    );

    // Dispose after promote is a no-op (the chat stays a normal root chat).
    rpc(
        &rig.core,
        methods::DISPOSE_SIDE_CHAT,
        serde_json::json!({ "sideChatId": side }),
    )
    .await
    .expect("DisposeSideChat after promote ok");
    assert!(
        rig.core.workspace.chat(&side).unwrap().is_some(),
        "promoted chat survives dispose"
    );

    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dispose_tears_down_without_remnants() {
    let rig = assemble();
    seed_parent(&rig.core).await;
    let side = start_side_chat(&rig.core).await;

    rpc(
        &rig.core,
        methods::SEND_SIDE_CHAT,
        serde_json::json!({
            "sideChatId": side,
            "messageId": "s1",
            "request": {
                "prompt": "dispose me",
                "harness": "pi",
                "cwd": "/tmp/repo",
                "sandbox": "workspace-write",
            },
        }),
    )
    .await
    .expect("SendSideChat ok");
    wait_for(
        || {
            rig.core
                .sessions
                .session_status(&side)
                .is_some_and(|s| s.status == SessionStatus::Working)
                || rig.requests.lock().unwrap().len() >= 1
        },
        "side run to start",
    );

    rpc(
        &rig.core,
        methods::DISPOSE_SIDE_CHAT,
        serde_json::json!({ "sideChatId": side }),
    )
    .await
    .expect("DisposeSideChat ok");

    // No durable remnants: no row, no status (public or private), no doc.
    assert!(rig.core.workspace.chat(&side).unwrap().is_none(), "no row");
    assert!(
        rig.core.sessions.session_status(&side).is_none(),
        "no status"
    );
    // The ephemeral doc handle is gone: a fresh open would be a fresh
    // (non-ephemeral) materialized doc — the temp handle must not survive.
    let handle = rig.core.doc_host.open(&side).expect("reopened doc");
    assert!(
        !handle.is_ephemeral(),
        "dispose dropped the ephemeral handle; the doc is not resurrected as temp"
    );
    // The public session list must never contain the disposed chat.
    let public = rig.core.sessions.watch_sessions().borrow().clone();
    assert!(
        !public.iter().any(|s| s.chat_id == side),
        "no leaked status"
    );

    // Dispose of an unknown id is a no-op (clean).
    rpc(
        &rig.core,
        methods::DISPOSE_SIDE_CHAT,
        serde_json::json!({ "sideChatId": "never-existed" }),
    )
    .await
    .expect("dispose unknown ok");

    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_reaps_unpromoted_side_chats() {
    let rig = assemble();
    seed_parent(&rig.core).await;
    let side = start_side_chat(&rig.core).await;

    rpc(
        &rig.core,
        methods::SEND_SIDE_CHAT,
        serde_json::json!({
            "sideChatId": side,
            "messageId": "s1",
            "request": {
                "prompt": "do work",
                "harness": "pi",
                "cwd": "/tmp/repo",
                "sandbox": "workspace-write",
            },
        }),
    )
    .await
    .expect("SendSideChat ok");
    wait_for(
        || rig.requests.lock().unwrap().len() >= 1,
        "side run to start",
    );

    // Shutdown reaps the unpromoted side chat: no row, no status, no doc.
    rig.core.shutdown().await;
    assert!(
        rig.core.workspace.chat(&side).unwrap().is_none(),
        "reaped row"
    );
    assert!(
        rig.core.sessions.session_status(&side).is_none(),
        "reaped status"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_dispatch_keeps_first_send_context_for_retry() {
    let rig = assemble();
    seed_parent(&rig.core).await;
    let side = start_side_chat(&rig.core).await;

    // First send with an UNREGISTERED harness: dispatch is REJECTED
    // (registry resolve fails before any doc write). The chat must stay
    // first-send-eligible. (The parent seed already recorded one run.)
    let before_fail = rig.requests.lock().unwrap().len();
    let err = rpc(
        &rig.core,
        methods::SEND_SIDE_CHAT,
        serde_json::json!({
            "sideChatId": side,
            "messageId": "s1",
            "request": {
                "prompt": "retry me",
                "harness": "claude-code",
                "cwd": "/tmp/repo",
                "sandbox": "workspace-write",
            },
        }),
    )
    .await
    .expect_err("unregistered harness rejects the dispatch");
    assert!(
        err.to_string().contains("ClaudeCode") || err.to_string().contains("not found"),
        "clear dispatch error: {err}"
    );
    assert_eq!(
        rig.requests.lock().unwrap().len(),
        before_fail,
        "no run reached the harness for the rejected dispatch"
    );

    // Retry with the registered harness: the FULL first-send context (quote +
    // parent) is still injected.
    let before = rig.requests.lock().unwrap().len();
    rpc(
        &rig.core,
        methods::SEND_SIDE_CHAT,
        serde_json::json!({
            "sideChatId": side,
            "messageId": "s1",
            "request": {
                "prompt": "retry me",
                "harness": "pi",
                "cwd": "/tmp/repo",
                "sandbox": "workspace-write",
            },
        }),
    )
    .await
    .expect("retry SendSideChat ok");
    wait_for(
        || rig.requests.lock().unwrap().len() == before + 1,
        "retry to reach the harness",
    );
    let first = rig.requests.lock().unwrap()[before].clone();
    assert!(
        first
            .prompt
            .contains(&format!("Selected text:\n{SELECTED}")),
        "retry still injects the selected quote: {}",
        first.prompt
    );
    assert!(
        first
            .prompt
            .contains("Parent chat context:\nuser: parent first question"),
        "retry still injects the parent context: {}",
        first.prompt
    );
    assert!(
        first.prompt.ends_with("User request:\nretry me"),
        "visible request untouched: {}",
        first.prompt
    );

    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn start_validates_selection_parent_and_global_cap() {
    let rig = assemble();
    seed_parent(&rig.core).await;

    // Empty selection rejected.
    let err = rpc(
        &rig.core,
        methods::START_SIDE_CHAT,
        serde_json::json!({
            "parentChatId": PARENT,
            "source": { "kind": "transcript", "anchorMessageId": "m1" },
            "selectedText": "   ",
        }),
    )
    .await
    .expect_err("empty selection rejected");
    assert!(err.to_string().contains("empty"), "clear error: {err}");

    // >64 KiB selection rejected.
    let huge = "q".repeat(64 * 1024 + 1);
    let err = rpc(
        &rig.core,
        methods::START_SIDE_CHAT,
        serde_json::json!({
            "parentChatId": PARENT,
            "source": { "kind": "transcript", "anchorMessageId": "m1" },
            "selectedText": huge,
        }),
    )
    .await
    .expect_err("oversized selection rejected");
    assert!(
        err.to_string().contains("65536") || err.to_string().contains("64"),
        "clear error: {err}"
    );

    // Missing parent rejected (never a silent local fallback).
    let err = rpc(
        &rig.core,
        methods::START_SIDE_CHAT,
        serde_json::json!({
            "parentChatId": "chat-that-does-not-exist",
            "source": { "kind": "transcript", "anchorMessageId": "m1" },
            "selectedText": SELECTED,
        }),
    )
    .await
    .expect_err("missing parent rejected");
    assert!(err.to_string().contains("not found"), "clear error: {err}");

    // Global cap: the 9th unpromoted side chat is rejected by the ENGINE.
    for _ in 0..8 {
        start_side_chat(&rig.core).await;
    }
    let err = rpc(
        &rig.core,
        methods::START_SIDE_CHAT,
        serde_json::json!({
            "parentChatId": PARENT,
            "source": { "kind": "transcript", "anchorMessageId": "m1" },
            "selectedText": SELECTED,
        }),
    )
    .await
    .expect_err("9th side chat hits the global cap");
    assert!(err.to_string().contains("limit"), "clear cap error: {err}");

    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_promotion_retains_temp_state_and_retries() {
    let rig = assemble();
    seed_parent(&rig.core).await;
    let side = start_side_chat(&rig.core).await;

    // Remove the parent row: promotion must fail (parent not found) and the
    // side chat must stay temporary and retryable — NOT a durable row.
    rig.core.workspace.delete_chat(PARENT).unwrap();
    let err = rpc(
        &rig.core,
        methods::PROMOTE_SIDE_CHAT,
        serde_json::json!({ "sideChatId": side }),
    )
    .await
    .expect_err("promote with missing parent fails");
    assert!(
        err.to_string().contains("parent chat not found"),
        "clear error: {err}"
    );
    assert!(
        rig.core.workspace.chat(&side).unwrap().is_none(),
        "no row after failed promote"
    );
    let handle = rig.core.doc_host.open(&side).expect("still-open handle");
    assert!(
        handle.is_ephemeral(),
        "chat is still temporary after failed promote"
    );

    // Re-seed the parent and retry: the same chat promotes cleanly.
    seed_parent(&rig.core).await;
    let promoted = rpc(
        &rig.core,
        methods::PROMOTE_SIDE_CHAT,
        serde_json::json!({ "sideChatId": side }),
    )
    .await
    .expect("promote retry succeeds");
    assert_eq!(
        promoted.get("chatId").and_then(|v| v.as_str()),
        Some(side.as_str())
    );
    let row = rig
        .core
        .workspace
        .chat(&side)
        .unwrap()
        .expect("row after retry");
    assert_eq!(row.device_id, rig.core.device_id);

    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn first_send_injects_quote_with_empty_parent_context() {
    // Round-21 audit: the FIRST send ALWAYS injects the selected text + the
    // user request, even when the parent transcript is empty/unreadable —
    // a safe marker stands in for the parent context. Never a None prompt.
    let rig = assemble();
    seed_parent_row(&rig.core).await; // row exists, NO transcript yet
    let side = start_side_chat(&rig.core).await;

    let before = rig.requests.lock().unwrap().len();
    rpc(
        &rig.core,
        methods::SEND_SIDE_CHAT,
        serde_json::json!({
            "sideChatId": side,
            "messageId": "s1",
            "request": {
                "prompt": "orphan question",
                "harness": "pi",
                "cwd": "/tmp/repo",
                "sandbox": "workspace-write",
            },
        }),
    )
    .await
    .expect("SendSideChat ok");
    wait_for(
        || rig.requests.lock().unwrap().len() == before + 1,
        "side run to reach the harness",
    );
    let first = rig.requests.lock().unwrap()[before].clone();
    assert!(
        first
            .prompt
            .contains(&format!("Selected text:\n{SELECTED}")),
        "empty parent transcript still injects the selected quote: {}",
        first.prompt
    );
    assert!(
        first
            .prompt
            .contains("Parent chat context:\n(no prior transcript context)"),
        "empty parent transcript yields the safe marker: {}",
        first.prompt
    );
    assert!(
        first.prompt.ends_with("User request:\norphan question"),
        "visible request untouched: {}",
        first.prompt
    );

    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_starts_cannot_exceed_global_cap() {
    // Round-21 audit: the manager-level start mutex serializes the capacity
    // check + insertion, so 16 racing starts land EXACTLY 8 successes and 8
    // cap rejections — never a 9th record.
    let (core, _requests, _dir) = assemble_arc();
    seed_parent(&core).await;

    let mut handles = Vec::new();
    for _ in 0..16 {
        let core = core.clone();
        handles.push(tokio::spawn(async move {
            rpc(
                &core,
                methods::START_SIDE_CHAT,
                serde_json::json!({
                    "parentChatId": PARENT,
                    "source": { "kind": "transcript", "anchorMessageId": "m1" },
                    "selectedText": SELECTED,
                }),
            )
            .await
        }));
    }
    let mut ok = 0usize;
    let mut limit_errors = 0usize;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(_) => ok += 1,
            Err(e) if e.to_string().contains("limit") => limit_errors += 1,
            Err(e) => panic!("unexpected start error: {e}"),
        }
    }
    assert_eq!(ok, 8, "exactly the cap succeeds");
    assert_eq!(limit_errors, 8, "the rest hit the global cap");

    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_promotes_are_idempotent() {
    // Round-21 audit: the manager-level promotion mutex serializes promotes
    // so a concurrent promote WAITS and then observes the completed durable
    // row — every caller gets the same chat id, exactly one row, and the
    // chat is fully promoted (never a fake early success with no row).
    let (core, requests, _dir) = assemble_arc();
    seed_parent(&core).await;
    let side = start_side_chat(&core).await;

    // Run one turn first so the chat has a live (private) status to backfill
    // on promotion — the public-status assertion below needs one.
    let before = requests.lock().unwrap().len();
    rpc(
        &core,
        methods::SEND_SIDE_CHAT,
        serde_json::json!({
            "sideChatId": side,
            "messageId": "s1",
            "request": {
                "prompt": "promote me",
                "harness": "pi",
                "cwd": "/tmp/repo",
                "sandbox": "workspace-write",
            },
        }),
    )
    .await
    .expect("SendSideChat ok");
    wait_for(
        || requests.lock().unwrap().len() == before + 1,
        "side run to reach the harness",
    );

    let mut handles = Vec::new();
    for _ in 0..8 {
        let core = core.clone();
        let side = side.clone();
        handles.push(tokio::spawn(async move {
            rpc(
                &core,
                methods::PROMOTE_SIDE_CHAT,
                serde_json::json!({ "sideChatId": side }),
            )
            .await
        }));
    }
    for handle in handles {
        let reply = handle.await.unwrap().expect("promote ok");
        assert_eq!(
            reply.get("chatId").and_then(|v| v.as_str()),
            Some(side.as_str()),
            "every concurrent promote reports the same chat id"
        );
    }
    // Exactly one durable row and a fully-promoted chat.
    let row = core
        .workspace
        .chat(&side)
        .unwrap()
        .expect("one durable row after concurrent promotes");
    assert_eq!(row.id, side);
    let handle = core.doc_host.open(&side).expect("promoted handle");
    assert!(!handle.is_ephemeral(), "fully promoted (finish ran)");
    assert!(
        core.sessions.session_status(&side).is_some(),
        "public status backfilled"
    );

    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dispose_rolls_back_partially_promoted_row() {
    // Round-21 audit: a retained record with a durable row is a PARTIALLY-
    // FAILED promotion (finish never ran — the handle is still ephemeral),
    // NOT a completed durable chat. Dispose rolls the partial row back and
    // drops every ephemeral remnant.
    let rig = assemble();
    seed_parent(&rig.core).await;
    let side = start_side_chat(&rig.core).await;

    // Simulate the state a promote that failed between row creation and the
    // doc flip leaves behind: workspace row exists, record retained, handle
    // still ephemeral, status still private.
    let parent = rig
        .core
        .workspace
        .chat(PARENT)
        .unwrap()
        .expect("parent row");
    let created = rig
        .core
        .workspace
        .promote_side_chat(&side, &parent, "partial title")
        .unwrap();
    assert!(created, "row created by the partial promote");
    let handle = rig.core.doc_host.open(&side).expect("side handle");
    assert!(
        handle.is_ephemeral(),
        "handle still ephemeral (finish_promotion never ran)"
    );

    rpc(
        &rig.core,
        methods::DISPOSE_SIDE_CHAT,
        serde_json::json!({ "sideChatId": side }),
    )
    .await
    .expect("DisposeSideChat ok");

    assert!(
        rig.core.workspace.chat(&side).unwrap().is_none(),
        "partial row rolled back"
    );
    assert!(
        rig.core.sessions.session_status(&side).is_none(),
        "no status remnant"
    );
    let reopened = rig.core.doc_host.open(&side).expect("reopened doc");
    assert!(
        !reopened.is_ephemeral(),
        "ephemeral handle + prepared snapshot purged"
    );
    let public = rig.core.sessions.watch_sessions().borrow().clone();
    assert!(
        !public.iter().any(|s| s.chat_id == side),
        "no leaked public status"
    );

    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_side_chat_send_and_watch_are_rejected() {
    // Round-21 final audit: SEND_SIDE_CHAT must NEVER mint/claim an arbitrary
    // hidden chat for an unknown id, and WATCH_SIDE_CHAT_STATUS must never
    // grow the private status map with a sender nothing would remove.
    let rig = assemble();
    seed_parent(&rig.core).await;

    // Send to a never-created id: rejected, and no run/doc is minted.
    let before = rig.requests.lock().unwrap().len();
    let err = rpc(
        &rig.core,
        methods::SEND_SIDE_CHAT,
        serde_json::json!({
            "sideChatId": "never-existed",
            "messageId": "s1",
            "request": {
                "prompt": "phantom",
                "harness": "pi",
                "cwd": "/tmp/repo",
                "sandbox": "workspace-write",
            },
        }),
    )
    .await
    .expect_err("unknown send rejected");
    assert!(
        err.to_string().contains("unknown side chat"),
        "clear error: {err}"
    );
    assert_eq!(
        rig.requests.lock().unwrap().len(),
        before,
        "no run minted for the unknown id"
    );
    assert!(
        rig.core.workspace.chat("never-existed").unwrap().is_none(),
        "no row minted for the unknown id"
    );

    // Watch an unknown id: rejected instead of silently creating a private
    // sender that nothing would ever remove.
    let watch_result = rig
        .core
        .rpc_service()
        .handle(
            methods::WATCH_SIDE_CHAT_STATUS,
            serde_json::json!({ "sideChatId": "never-existed" }),
        )
        .await;
    let err = match watch_result {
        Err(err) => err,
        Ok(_) => panic!("unknown watch must be rejected, not streamed"),
    };
    assert!(
        err.to_string().contains("unknown side chat"),
        "clear error: {err}"
    );

    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn promoted_chat_send_dispatches_as_normal_chat() {
    // Round-21 final audit: once promoted (durable row exists, record gone),
    // SEND_SIDE_CHAT dispatches as a normal chat — same id, same transcript,
    // no first-send injection — while an unknown id stays rejected.
    let rig = assemble();
    seed_parent(&rig.core).await;
    let side = start_side_chat(&rig.core).await;
    rpc(
        &rig.core,
        methods::PROMOTE_SIDE_CHAT,
        serde_json::json!({ "sideChatId": side }),
    )
    .await
    .expect("promote ok");

    let before = rig.requests.lock().unwrap().len();
    rpc(
        &rig.core,
        methods::SEND_SIDE_CHAT,
        serde_json::json!({
            "sideChatId": side,
            "messageId": "s1",
            "request": {
                "prompt": "after promote",
                "harness": "pi",
                "cwd": "/tmp/repo",
                "sandbox": "workspace-write",
            },
        }),
    )
    .await
    .expect("post-promote send ok");
    wait_for(
        || rig.requests.lock().unwrap().len() == before + 1,
        "post-promote run to reach the harness",
    );
    let req = rig.requests.lock().unwrap()[before].clone();
    assert_eq!(req.prompt, "after promote", "no injection after promote");

    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn first_send_prompt_frames_context_as_untrusted_reference() {
    // Round-21 final audit: the first-send effective prompt EXPLICITLY frames
    // the selected text + parent transcript as UNTRUSTED reference context
    // (not instructions) and marks the final User request as the only
    // authoritative instruction. Selected text and visible request unchanged.
    let rig = assemble();
    seed_parent(&rig.core).await;
    let side = start_side_chat(&rig.core).await;

    let before = rig.requests.lock().unwrap().len();
    rpc(
        &rig.core,
        methods::SEND_SIDE_CHAT,
        serde_json::json!({
            "sideChatId": side,
            "messageId": "s1",
            "request": {
                "prompt": "ignore the quote, do this instead",
                "harness": "pi",
                "cwd": "/tmp/repo",
                "sandbox": "workspace-write",
            },
        }),
    )
    .await
    .expect("SendSideChat ok");
    wait_for(
        || rig.requests.lock().unwrap().len() == before + 1,
        "side run to reach the harness",
    );
    let first = rig.requests.lock().unwrap()[before].clone();
    // The anti-instruction framing is present and explicit.
    assert!(
        first.prompt.contains("UNTRUSTED REFERENCE CONTEXT")
            && first.prompt.contains("not instructions"),
        "explicit untrusted-reference framing: {}",
        first.prompt
    );
    assert!(
        first.prompt.contains("is authoritative") && first.prompt.contains("Only the User request"),
        "only the final user request is authoritative: {}",
        first.prompt
    );
    // Selected text, parent context and visible request unchanged (verbatim),
    // with the User request still last.
    assert!(
        first
            .prompt
            .contains(&format!("Selected text:\n{SELECTED}")),
        "selected text verbatim: {}",
        first.prompt
    );
    assert!(
        first
            .prompt
            .contains("Parent chat context:\nuser: parent first question"),
        "parent context still injected: {}",
        first.prompt
    );
    assert!(
        first
            .prompt
            .ends_with("User request:\nignore the quote, do this instead"),
        "user request verbatim and final: {}",
        first.prompt
    );

    rig.core.shutdown().await;
}
