//! Host-side worktree materialization: a Run command carrying a
//! `WorktreeSpec` creates the isolated worktree on the HOST at drain time
//! (the durable replacement for the composer's old blocking CreateWorktree
//! relay RPC), runs there, and stamps the chat row's cwd + `cypher/<name>`
//! branch + checkout identity. A second spec-carrying Run for the same chat
//! REUSES the checkout instead of minting another. An invalid base ref
//! Rejects the command and never dispatches the harness.

use crate::support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cypher_doc::{SessionCommandPayload, SessionCommandStatus};
use cypher_engine::{EngineCore, HarnessRegistry};
use cypher_proto::{HarnessId, RunRequest, WorktreeSpec};

use support::{command_statuses, complete_assistant_count, run_request, wait_for};

const CHAT: &str = "chat-worktree-run";

fn run_payload(message_id: &str, repo_path: &str, base_ref: &str) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: RunRequest {
            // Fallback for hosts that predate the spec: the repo's own folder.
            cwd: repo_path.into(),
            worktree: Some(WorktreeSpec {
                repo_path: repo_path.into(),
                base_ref: base_ref.into(),
                name_hint: None,
            }),
            ..run_request("isolated please")
        },
        message_id: message_id.into(),
        agent_prompt: None,
    }
}

/// A minimal repo with one commit on `main`, returned as a canonical path
/// (git records canonical paths in worktree gitdir links, and macOS tempdirs
/// live behind the /var → /private/var symlink).
async fn init_repo(dir: &Path) -> String {
    support::init_repo(dir, "hello\n").await;
    dir.canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

async fn assemble_with(cwds: Arc<Mutex<Vec<String>>>) -> (EngineCore, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let tmp_path = tmp.path().canonicalize().unwrap();
    let worktrees_root = tmp_path.join("worktrees");
    // Completes a one-line turn and records the cwd each run spawned with.
    let harness = support::TestHarness::new(HarnessId::Mock, "Recorder", move |request, _| {
        cwds.lock().unwrap().push(request.cwd.clone());
        support::reply(
            HarnessId::Mock,
            &request,
            "sess-wt",
            &format!("ack: {}", request.prompt),
        )
    });
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(harness));
    let core = support::test_engine(|| {
        EngineCore::assemble(
            &tmp_path.join("data"),
            Arc::new(registry),
            HarnessId::Mock,
            None,
        )
    });
    core.repos.set_worktrees_root(worktrees_root);
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
    let repo_path = init_repo(&tmp_path.join("repo")).await;

    create_chat(&core).await;
    core.doc_host
        .queue_command(CHAT, run_payload("msg-wt-1", &repo_path, "main"))
        .expect("queue run command");
    wait_for(|| complete_assistant_count(&core, CHAT) == 1, "first turn").await;

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
    wait_for(|| complete_assistant_count(&core, CHAT) == 2, "second turn").await;

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
    let statuses = command_statuses(&core, CHAT);
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
    let repo_path = init_repo(&tmp_path.join("repo")).await;

    create_chat(&core).await;
    core.doc_host
        .queue_command(
            CHAT,
            run_payload("msg-bad-1", &repo_path, "no-such-ref-anywhere"),
        )
        .expect("queue run command");
    wait_for(
        || {
            command_statuses(&core, CHAT)
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
    let statuses = command_statuses(&core, CHAT);
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
