//! Session migration (`MigrateChats`): a root chat and its child chats move
//! to another project — on another device over the device relay, or between
//! two projects of one device — taking their Pi session (header re-pointed at
//! the new folder), run journal and transcript images with them.
//!
//! The two-device cases run two engines, each with its own data dir and
//! device id, joined by the in-memory device room; their registries are
//! seeded with the same rows to stand in for the registry room.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_doc::SessionCommandPayload;
use cypher_engine::{EngineCore, EngineProfile, HarnessRegistry};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, Chat, ChatConfig, ChatMigrationEvent, ChildAgentProfile, ChildChat, DoneStatus,
    HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel, SessionStatus, SteeringMode,
    SubagentRunMode,
};
use cypher_rpc::{LinkCache, LinkCacheConfig, RpcClient, RpcService, StaticToken, methods};

const ORG: &str = "org-migrate";
const USER: &str = "user-migrate";

#[derive(Debug, Clone)]
struct RunRecord {
    prompt: String,
    cwd: String,
    resume: Option<String>,
}

/// A Pi harness that records where each run starts and which session it
/// resumes. A run prompted "hang" never finishes (the chat stays Working).
#[derive(Default)]
struct Recorder {
    runs: Arc<Mutex<Vec<RunRecord>>>,
    counter: AtomicUsize,
}

#[async_trait]
impl Harness for Recorder {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
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
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.runs.lock().unwrap().push(RunRecord {
            prompt: request.prompt.clone(),
            cwd: request.cwd.clone(),
            resume: request.resume.clone(),
        });
        let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let session = request
            .resume
            .clone()
            .unwrap_or_else(|| format!("fresh-{n}"));
        let started = common::session_started(
            HarnessId::Pi,
            "model-x",
            &request.cwd,
            &session,
            &format!("a-{n}"),
        );
        if request.prompt == "hang" {
            return Ok(futures::stream::iter([Ok(started)])
                .chain(futures::stream::pending())
                .boxed());
        }
        Ok(futures::stream::iter([
            Ok(started),
            Ok(common::text("ok")),
            Ok(common::done_with(DoneStatus::Completed, None)),
        ])
        .boxed())
    }
}

struct Device {
    core: EngineCore,
    store: PathBuf,
    runs: Arc<Mutex<Vec<RunRecord>>>,
}

fn device(dir: &Path, device_id: &str) -> Device {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("device-id"), device_id).unwrap();
    let store = EngineProfile::development(dir, ORG, USER)
        .store_root()
        .to_path_buf();
    let recorder = Recorder::default();
    let runs = recorder.runs.clone();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(recorder));
    let core = EngineCore::assemble_with_profile(
        EngineProfile::development(dir, ORG, USER),
        Arc::new(registry),
        HarnessId::Pi,
        None,
    )
    .expect("engine assembles");
    Device { core, store, runs }
}

fn pi_config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Pi,
        model: Some("model-x".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
    }
}

fn folder(root: &Path, name: &str) -> String {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// A Pi session file in `store`'s managed session root.
fn pi_session(store: &Path, name: &str, cwd: &str, lines: &[String]) -> PathBuf {
    let root = store.join("agent-sessions");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(name);
    let header = serde_json::json!({
        "type": "session", "version": 3, "id": "sess-1",
        "timestamp": "2026-10-28T00:00:00.000Z", "cwd": cwd,
    });
    let mut text = format!("{header}\n");
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
    path
}

fn header_cwd(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    let header: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    header["cwd"].as_str().unwrap().to_string()
}

fn body_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
}

fn journal_path(store: &Path, chat_id: &str) -> PathBuf {
    store.join("journals").join(format!("{chat_id}.jsonl"))
}

fn write_journal(store: &Path, chat_id: &str) -> String {
    let done = AgentEvent::Done {
        status: DoneStatus::Completed,
        result: Some("earlier answer".into()),
        error: None,
        session_id: None,
    };
    let line = serde_json::json!({ "seq": 1, "event": done }).to_string();
    let path = journal_path(store, chat_id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("{line}\n")).unwrap();
    line
}

fn child_of(parent: &Chat, id: &str) -> Chat {
    Chat {
        id: id.into(),
        title: Some("Planner".into()),
        harness_session_id: None,
        harness_session_cwd: None,
        child: Some(ChildChat {
            parent_chat_id: parent.id.clone(),
            parent_run_id: "run-1".into(),
            agent: "planner".into(),
            task: "plan it".into(),
            mode: SubagentRunMode::Sync,
            tool_call_id: None,
            profile: ChildAgentProfile {
                system_prompt: "plan".into(),
                tools: vec![],
                model: None,
                thinking: None,
            },
        }),
        ..parent.clone()
    }
}

async fn migrate(client: &RpcClient, params: serde_json::Value) -> Vec<ChatMigrationEvent> {
    let mut rx = client
        .subscribe(methods::MIGRATE_CHATS, params)
        .await
        .expect("MigrateChats");
    let mut events = Vec::new();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .expect("migration progress")
    {
        let event: ChatMigrationEvent = serde_json::from_value(item).expect("typed event");
        let done = matches!(event, ChatMigrationEvent::Done { .. });
        events.push(event);
        if done {
            break;
        }
    }
    events
}

fn chat_errors(events: &[ChatMigrationEvent]) -> Vec<(String, Option<String>)> {
    events
        .iter()
        .filter_map(|event| match event {
            ChatMigrationEvent::Chat { chat_id, error, .. } => {
                Some((chat_id.clone(), error.clone()))
            }
            _ => None,
        })
        .collect()
}

fn done(events: &[ChatMigrationEvent]) -> (usize, usize) {
    match events.last() {
        Some(ChatMigrationEvent::Done { moved, failed }) => (*moved, *failed),
        other => panic!("migration ended without Done: {other:?}"),
    }
}

/// Wait until A's link to `device` answers (the host relay dials with
/// backoff).
async fn wait_for_link(client: &RpcClient, device: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        match client
            .call(
                methods::LIST_HARNESSES,
                serde_json::json!({ "targetDeviceId": device }),
            )
            .await
        {
            Ok(_) => return,
            Err(err) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "relay never came up: {err}"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moves_a_chat_family_to_a_project_on_another_device() {
    let (relay_url, _relay) = common::relay::fake_device_room().await;
    let dirs = tempfile::tempdir().unwrap();
    let a = device(&dirs.path().join("a"), "device-a");
    let b = device(&dirs.path().join("b"), "device-b");
    let _host = b.core.start_host_relay(&relay_url);
    let mut links = LinkCacheConfig::new(relay_url, Arc::new(StaticToken("test-user".into())));
    links.probe_timeout = Duration::from_secs(5);
    a.core.set_links(LinkCache::new(links));

    let from_dir = folder(dirs.path(), "laptop-project");
    let to_dir = folder(dirs.path(), "server-project");
    a.core
        .workspace
        .create_space("space-a", "device-a", &from_dir, None, false)
        .unwrap();
    a.core
        .workspace
        .create_space("space-b", "device-b", &to_dir, None, false)
        .unwrap();
    a.core
        .workspace
        .create_chat("chat-1", Some("space-a"), None, Some(pi_config()), None)
        .unwrap();
    a.core.workspace.rename_chat("chat-1", "Refactor").unwrap();

    // A big session: the transfer crosses several relay slices.
    let filler = "x".repeat(1_000);
    let lines: Vec<String> = (0..1_500)
        .map(|i| {
            serde_json::json!({ "type": "message", "id": format!("e{i}"), "text": filler })
                .to_string()
        })
        .collect();
    let session_a = pi_session(&a.store, "2026-10-28_sess-1.jsonl", &from_dir, &lines);
    a.core
        .workspace
        .set_chat_harness_session("chat-1", &session_a.to_string_lossy(), &from_dir);
    let journal_line = write_journal(&a.store, "chat-1");

    // A transcript image in A's upload cache, named by the user message.
    let image_a = a.core.uploads.dir().join("ab12cd34-screen.png");
    std::fs::create_dir_all(a.core.uploads.dir()).unwrap();
    std::fs::write(&image_a, b"\x89PNG fake image bytes").unwrap();
    let image_path = image_a.to_string_lossy().into_owned();
    a.core
        .doc_host
        .open("chat-1")
        .unwrap()
        .write_user_message(
            "m-1",
            &cypher_proto::attachment_refs::with_refs(
                "look at this",
                std::slice::from_ref(&image_path),
            ),
            1_000,
        )
        .unwrap();

    let parent = a.core.workspace.chat("chat-1").unwrap().unwrap();
    a.core
        .workspace
        .import_chat_row(&child_of(&parent, "chat-1-child"))
        .unwrap();

    // The registry room's job, done by hand: B knows the same rows.
    for space in a.core.workspace.read_spaces().unwrap() {
        b.core.workspace.import_space_row(&space).unwrap();
    }
    for chat in a.core.workspace.read_chats().unwrap() {
        b.core.workspace.import_chat_row(&chat).unwrap();
    }

    let client = cypher_rpc::memory_client(a.core.rpc_service());
    wait_for_link(&client, "device-b").await;
    let events = migrate(
        &client,
        serde_json::json!({ "chatIds": ["chat-1"], "toSpaceId": "space-b" }),
    )
    .await;
    assert_eq!(
        chat_errors(&events),
        vec![("chat-1".to_string(), None)],
        "{events:?}"
    );
    assert_eq!(done(&events), (1, 0));

    // Both replicas name B as the host, in B's project and folder.
    for core in [&a.core, &b.core] {
        for id in ["chat-1", "chat-1-child"] {
            let chat = core.workspace.chat(id).unwrap().unwrap();
            assert_eq!(chat.device_id, "device-b", "{id}");
            assert_eq!(chat.space_id.as_deref(), Some("space-b"), "{id}");
            assert_eq!(chat.cwd.as_deref(), Some(to_dir.as_str()), "{id}");
            assert_eq!(chat.harness_session_cwd.as_deref(), Some(to_dir.as_str()));
        }
        let chat = core.workspace.chat("chat-1").unwrap().unwrap();
        assert_eq!(chat.title.as_deref(), Some("Refactor"));
        assert_eq!(chat.config, Some(pi_config()));
    }

    // The Pi session lives on B now, pointed at B's folder, every entry intact.
    let moved = b.core.workspace.chat("chat-1").unwrap().unwrap();
    let session_b = PathBuf::from(moved.harness_session_id.unwrap());
    assert!(session_b.starts_with(b.store.join("agent-sessions")));
    assert_eq!(header_cwd(&session_b), to_dir);
    assert_eq!(body_lines(&session_b), lines);
    assert!(!session_a.exists(), "A's copy is gone");
    // The child had no session: it starts fresh rather than reaching for A's.
    let child = b.core.workspace.chat("chat-1-child").unwrap().unwrap();
    assert_eq!(child.harness_session_id.as_deref(), Some(""));

    // The journal moved with it.
    assert!(!journal_path(&a.store, "chat-1").exists());
    assert_eq!(
        std::fs::read_to_string(journal_path(&b.store, "chat-1")).unwrap(),
        format!("{journal_line}\n")
    );

    // B serves the transcript image under the path the transcript names,
    // even once A's file is gone.
    std::fs::remove_file(&image_a).unwrap();
    let chunk = match b
        .core
        .rpc_service()
        .handle(
            methods::READ_ATTACHMENT_CHUNK,
            serde_json::json!({ "path": image_path, "offset": 0 }),
        )
        .await
        .expect("image served by B")
    {
        cypher_rpc::RpcReply::Value(value) => value,
        _ => panic!("unary reply expected"),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(chunk["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(bytes, b"\x89PNG fake image bytes");

    // Nothing ran anywhere during the move.
    assert!(a.runs.lock().unwrap().is_empty());
    assert!(b.runs.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moves_between_projects_on_one_device_and_resumes_there() {
    let dirs = tempfile::tempdir().unwrap();
    let a = device(&dirs.path().join("a"), "device-a");
    let old_dir = folder(dirs.path(), "old-project");
    let new_dir = folder(dirs.path(), "new-project");
    let ws = &a.core.workspace;
    ws.create_space("space-old", "device-a", &old_dir, None, false)
        .unwrap();
    ws.create_space("space-new", "device-a", &new_dir, None, false)
        .unwrap();
    ws.create_chat("chat-1", Some("space-old"), None, Some(pi_config()), None)
        .unwrap();
    ws.rename_chat("chat-1", "Moving").unwrap();
    let lines = vec![r#"{"type":"message","id":"e1"}"#.to_string()];
    let session_old = pi_session(&a.store, "2026-10-28_sess-1.jsonl", &old_dir, &lines);
    ws.set_chat_harness_session("chat-1", &session_old.to_string_lossy(), &old_dir);
    let journal_line = write_journal(&a.store, "chat-1");

    let client = cypher_rpc::memory_client(a.core.rpc_service());
    let events = migrate(
        &client,
        serde_json::json!({ "chatIds": ["chat-1"], "toSpaceId": "space-new" }),
    )
    .await;
    assert_eq!(done(&events), (1, 0), "{events:?}");

    let moved = ws.chat("chat-1").unwrap().unwrap();
    assert_eq!(moved.device_id, "device-a");
    assert_eq!(moved.space_id.as_deref(), Some("space-new"));
    assert_eq!(moved.cwd.as_deref(), Some(new_dir.as_str()));
    let session_new = PathBuf::from(moved.harness_session_id.clone().unwrap());
    assert_ne!(session_new, session_old);
    assert_eq!(header_cwd(&session_new), new_dir);
    assert_eq!(body_lines(&session_new), lines);
    assert!(!session_old.exists());
    // Same device: the journal stays where it is.
    assert_eq!(
        std::fs::read_to_string(journal_path(&a.store, "chat-1")).unwrap(),
        format!("{journal_line}\n")
    );

    // A message a stale viewer queued with the old folder runs in the new
    // one and resumes the moved session.
    let payload = SessionCommandPayload::Run {
        request: RunRequest {
            prompt: "after move".into(),
            harness: Some(HarnessId::Pi),
            model: Some("model-x".into()),
            reasoning: None,
            model_options: Default::default(),
            cwd: old_dir.clone(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
            resume: None,
            worktree: None,
            attachments: Vec::new(),
            pending_attachments: Vec::new(),
        },
        message_id: "m-after".into(),
        agent_prompt: None,
    };
    client
        .call(
            methods::QUEUE_COMMAND,
            serde_json::json!({ "chatId": "chat-1", "command": payload }),
        )
        .await
        .expect("queue");
    let run = common::wait_for_some("the queued run", || {
        a.runs
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.prompt == "after move")
            .cloned()
    })
    .await;
    assert_eq!(run.cwd, new_dir);
    assert_eq!(
        run.resume.as_deref(),
        Some(session_new.to_string_lossy().as_ref())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_project_move_takes_its_chats_and_leaves_running_ones() {
    let dirs = tempfile::tempdir().unwrap();
    let a = device(&dirs.path().join("a"), "device-a");
    let from_dir = folder(dirs.path(), "from");
    let to_dir = folder(dirs.path(), "to");
    let ws = &a.core.workspace;
    ws.create_space("space-from", "device-a", &from_dir, None, false)
        .unwrap();
    ws.create_space("space-to", "device-a", &to_dir, None, false)
        .unwrap();
    for id in ["chat-idle", "chat-archived", "chat-busy"] {
        ws.create_chat(id, Some("space-from"), None, Some(pi_config()), None)
            .unwrap();
        ws.rename_chat(id, id).unwrap();
    }
    ws.set_chat_archived("chat-archived", true).unwrap();
    ws.create_chat("quick", None, Some("device-a"), Some(pi_config()), None)
        .unwrap();

    // chat-busy is mid-turn.
    a.core
        .sessions
        .dispatch(
            "chat-busy",
            HarnessId::Pi,
            RunRequest {
                prompt: "hang".into(),
                harness: Some(HarnessId::Pi),
                model: Some("model-x".into()),
                reasoning: None,
                model_options: Default::default(),
                cwd: from_dir.clone(),
                sandbox: SandboxLevel::WorkspaceWrite,
                auto_approve: false,
                resume: None,
                worktree: None,
                attachments: Vec::new(),
                pending_attachments: Vec::new(),
            },
            Some("m-hang".into()),
        )
        .await
        .expect("dispatch");
    common::wait_for(
        || {
            a.core
                .sessions
                .session_status("chat-busy")
                .is_some_and(|s| s.status == SessionStatus::Working)
        },
        "chat-busy working",
    )
    .await;

    let client = cypher_rpc::memory_client(a.core.rpc_service());
    let events = migrate(
        &client,
        serde_json::json!({ "fromSpaceId": "space-from", "toSpaceId": "space-to" }),
    )
    .await;
    assert_eq!(done(&events), (2, 1), "{events:?}");
    let errors = chat_errors(&events);
    assert!(
        errors.iter().any(|(id, error)| id == "chat-busy"
            && error
                .as_deref()
                .is_some_and(|e| e.contains("still running"))),
        "{errors:?}"
    );
    for id in ["chat-idle", "chat-archived"] {
        let chat = ws.chat(id).unwrap().unwrap();
        assert_eq!(chat.space_id.as_deref(), Some("space-to"), "{id}");
        assert_eq!(chat.cwd.as_deref(), Some(to_dir.as_str()), "{id}");
    }
    assert!(ws.chat("chat-archived").unwrap().unwrap().archived);
    let busy = ws.chat("chat-busy").unwrap().unwrap();
    assert_eq!(busy.space_id.as_deref(), Some("space-from"));
    assert_eq!(busy.cwd.as_deref(), Some(from_dir.as_str()));

    // Quick chats, chats already there, and a project onto itself refuse.
    let events = migrate(
        &client,
        serde_json::json!({ "chatIds": ["quick", "chat-idle", "nope"], "toSpaceId": "space-to" }),
    )
    .await;
    assert_eq!(done(&events), (0, 3), "{events:?}");
    let refused = client
        .subscribe(
            methods::MIGRATE_CHATS,
            serde_json::json!({ "fromSpaceId": "space-to", "toSpaceId": "space-to" }),
        )
        .await;
    match refused {
        Err(err) => assert!(err.to_string().contains("different project"), "{err}"),
        Ok(mut rx) => assert!(rx.recv().await.is_none(), "a self-move must not start"),
    }
    let _ = a.core.sessions.interrupt("chat-busy").await;
}
