//! Host-side worktree materialization: a Run command carrying a
//! `WorktreeSpec` creates the isolated worktree on the HOST at drain time
//! (the durable replacement for the composer's old blocking CreateWorktree
//! relay RPC), runs there, and stamps the chat row's cwd + `cypher/<name>`
//! branch + checkout identity. A second spec-carrying Run for the same chat
//! REUSES the checkout instead of minting another. An invalid base ref
//! Rejects the command and never dispatches the harness.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_doc::{
    MessageRole, MessageStatus, SessionCommandPayload, SessionCommandStatus, SessionMessageEntry,
};
use cypher_engine::{EngineCore, HarnessRegistry};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode, WorktreeSpec,
};

const CHAT: &str = "chat-worktree-run";

/// Completes a one-line turn and records the cwd each run spawned with.
struct RecordingHarness {
    cwds: Arc<Mutex<Vec<String>>>,
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
        self.cwds.lock().unwrap().push(request.cwd.clone());
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "sess-wt".into(),
                assistant_message_id: "a-1".into(),
            }),
            Ok(AgentEvent::TextDelta {
                text: format!("ack: {}", request.prompt),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("sess-wt".into()),
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

fn complete_assistant_count(core: &EngineCore) -> usize {
    let entries: Vec<SessionMessageEntry> = core
        .doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default();
    entries
        .iter()
        .filter(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
        .count()
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

fn run_payload(message_id: &str, repo_path: &str, base_ref: &str) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: RunRequest {
            prompt: "isolated please".into(),
            harness: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            // Fallback for hosts that predate the spec: the repo's own folder.
            cwd: repo_path.into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            attachments: Vec::new(),
            pending_attachments: Vec::new(),
            resume: None,
            worktree: Some(WorktreeSpec {
                repo_path: repo_path.into(),
                base_ref: base_ref.into(),
                name_hint: None,
            }),
        },
        message_id: message_id.into(),
        agent_prompt: None,
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A minimal repo with one commit on `main`, returned as a canonical path
/// (git records canonical paths in worktree gitdir links, and macOS tempdirs
/// live behind the /var → /private/var symlink).
fn init_repo(dir: &Path) -> String {
    std::fs::create_dir_all(dir).expect("repo dir");
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.email", "t@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("README.md"), "hello\n").expect("readme");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "init"]);
    dir.canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

async fn assemble_with(cwds: Arc<Mutex<Vec<String>>>) -> (EngineCore, tempfile::TempDir) {
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let tmp = tempfile::tempdir().unwrap();
    let tmp_path = tmp.path().canonicalize().unwrap();
    let worktrees_root = tmp_path.join("worktrees");
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(RecordingHarness { cwds: cwds.clone() }));
    // Repos captures this value during construction. The two parallel tests
    // used to overwrite each other's roots, then assert against the wrong one.
    let core = {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("CYPHER_WORKTREES_DIR");
        unsafe { std::env::set_var("CYPHER_WORKTREES_DIR", &worktrees_root) };
        let core = EngineCore::assemble(
            &tmp_path.join("data"),
            Arc::new(registry),
            HarnessId::Mock,
            None,
        );
        unsafe {
            match previous {
                Some(value) => std::env::set_var("CYPHER_WORKTREES_DIR", value),
                None => std::env::remove_var("CYPHER_WORKTREES_DIR"),
            }
        }
        core.expect("engine core assembles")
    };
    // Return the owner, not just a PathBuf: otherwise this function deletes the
    // live engine's data directory before the test even queues its first run.
    (core, tmp)
}

/// Mirror the composer: createChat lands first (cwd-less; the engine resolves
/// the project folder), then the queued Run carries the spec.
async fn create_chat(core: &EngineCore) {
    let client = cypher_rpc::memory_client(core.rpc_service());
    client
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "createChat",
                "chatId": CHAT,
                "deviceId": core.device_id,
            }),
        )
        .await
        .expect("createChat");
    // Pre-title so the auto-titler's harness request stays out of the flow.
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_with_worktree_spec_materializes_on_host_and_reuses() {
    let cwds: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (core, tmp) = assemble_with(cwds.clone()).await;
    let tmp_path = tmp.path().canonicalize().unwrap();
    let worktrees_root = tmp_path.join("worktrees");
    let repo_path = init_repo(&tmp_path.join("repo"));

    create_chat(&core).await;
    core.doc_host
        .queue_command(CHAT, run_payload("msg-wt-1", &repo_path, "main"))
        .expect("queue run command");
    wait_for(|| complete_assistant_count(&core) == 1, "first turn").await;

    let first_cwd = cwds.lock().unwrap().first().cloned().expect("run recorded");
    assert_ne!(
        first_cwd, repo_path,
        "the run must execute in a fresh worktree, not the repo folder"
    );
    let first = PathBuf::from(&first_cwd);
    assert!(
        first.starts_with(&worktrees_root),
        "worktree lands under the worktrees root: {first_cwd}"
    );
    assert!(
        first.join(".git").is_file(),
        "a linked worktree has a .git FILE"
    );

    // The chat row follows: cwd repointed at the worktree, branch stamped
    // with the actual cypher/<name>, checkout identity recorded.
    let chat = core
        .workspace
        .chat(CHAT)
        .expect("read chat row")
        .expect("chat row exists");
    assert_eq!(chat.cwd.as_deref(), Some(first_cwd.as_str()));
    let branch = chat.branch.expect("branch stamped");
    assert!(
        branch.starts_with("cypher/"),
        "branch is cypher/<name>: {branch}"
    );
    assert!(
        chat.checkout_id.is_some(),
        "checkout identity stamped so diff grouping matches the worktree"
    );

    // A second spec-carrying Run (client retry after a lost ack) reuses the
    // SAME worktree — never a second checkout.
    core.doc_host
        .queue_command(CHAT, run_payload("msg-wt-2", &repo_path, "main"))
        .expect("queue second run command");
    wait_for(|| complete_assistant_count(&core) == 2, "second turn").await;

    let recorded: Vec<String> = cwds.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);
    assert_eq!(
        recorded[1], first_cwd,
        "duplicate spec-carrying Run reuses the existing worktree"
    );
    let worktrees = std::fs::read_dir(worktrees_root.join("repo"))
        .expect("worktrees root/repo")
        .count();
    assert_eq!(worktrees, 1, "exactly one worktree for the chat");
    let statuses = command_status(&core);
    assert!(
        statuses
            .iter()
            .all(|(_, s, _)| *s == SessionCommandStatus::Applied),
        "both commands applied: {statuses:?}"
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_base_ref_rejects_and_never_dispatches() {
    let cwds: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (core, tmp) = assemble_with(cwds.clone()).await;
    let tmp_path = tmp.path().canonicalize().unwrap();
    let worktrees_root = tmp_path.join("worktrees");
    let repo_path = init_repo(&tmp_path.join("repo"));

    create_chat(&core).await;
    core.doc_host
        .queue_command(
            CHAT,
            run_payload("msg-bad-1", &repo_path, "no-such-ref-anywhere"),
        )
        .expect("queue run command");
    wait_for(
        || {
            command_status(&core)
                .iter()
                .any(|(_, s, _)| *s == SessionCommandStatus::Rejected)
        },
        "command rejected",
    )
    .await;

    // The harness never ran and no worktree was created — the request for a
    // new worktree never silently degraded to the base checkout.
    assert!(
        cwds.lock().unwrap().is_empty(),
        "harness must not execute on an invalid base ref"
    );
    let statuses = command_status(&core);
    let (_, _, resolution) = statuses
        .iter()
        .find(|(_, s, _)| *s == SessionCommandStatus::Rejected)
        .expect("rejected command found");
    let resolution = resolution.as_deref().unwrap_or("");
    assert!(
        resolution.contains("worktree create failed"),
        "resolution names the worktree failure: {resolution}"
    );
    assert!(
        !worktrees_root.join("repo").exists(),
        "no worktree materialized for an invalid base ref"
    );
    core.shutdown().await;
}
