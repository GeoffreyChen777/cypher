//! Uploads chunk→commit→readback + path jail, chat auto-titling with the mock
//! harness, and the RPC dispatch for the upload methods over the memory
//! transport.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use cypher_engine::{EngineCore, HarnessRegistry, Repos, Uploads, worktree_branch_from_title};
use cypher_harness::mock::MockHarness;
use cypher_proto::{AgentEvent, DoneStatus, HarnessId, SandboxLevel};
use cypher_rpc::methods;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn assemble_with_mock(dir: &Path, script: Vec<AgentEvent>) -> EngineCore {
    std::fs::create_dir_all(dir).expect("data dir");
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness { script }));
    EngineCore::assemble(dir, Arc::new(registry), HarnessId::Mock, None).expect("engine assembles")
}

async fn init_repo(dir: &Path) {
    common::init_repo(dir, "one\n").await;
}

/// Poll until `probe` yields Some, or panic at the deadline.
// ---------------------------------------------------------------------------
// Uploads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uploads_chunk_commit_readback_and_jail() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let uploads = Uploads::new(tmp.path());

    // 100KB of pseudo-random bytes, staged as three positional base64 chunks
    // (out of order, with one retried) — chunk boundaries are multiples of 3
    // bytes so independent base64 strings concatenate losslessly.
    let payload: Vec<u8> = (0..100_002u32)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    let chunks: Vec<String> = payload.chunks(45_000).map(|c| BASE64.encode(c)).collect();
    assert_eq!(chunks.len(), 3);
    uploads
        .append("up-1", &chunks[2], Some(2))
        .expect("chunk 2");
    uploads
        .append("up-1", &chunks[0], Some(0))
        .expect("chunk 0");
    uploads
        .append("up-1", &chunks[0], Some(0))
        .expect("chunk 0 retry is idempotent");
    uploads
        .append("up-1", &chunks[1], Some(1))
        .expect("chunk 1");
    let path = uploads.commit("up-1", "photo.png").expect("commit");
    assert!(path.ends_with("up-1-photo.png"), "path: {path}");
    assert_eq!(std::fs::read(&path).expect("committed file"), payload);

    // Readback: chunked reassembly round-trips.
    let mut assembled = Vec::new();
    let mut offset = 0u64;
    loop {
        let chunk = uploads.read_chunk(&path, offset, &[]).expect("read chunk");
        assert_eq!(chunk.mime_type, "image/png");
        assert_eq!(chunk.name, "up-1-photo.png");
        assembled.extend(BASE64.decode(&chunk.data).expect("chunk base64"));
        offset = chunk.next_offset;
        if chunk.done {
            break;
        }
    }
    assert_eq!(assembled, payload);

    // Missing chunk → commit fails.
    uploads
        .append("up-2", &chunks[0], Some(0))
        .expect("chunk 0");
    uploads
        .append("up-2", &chunks[2], Some(2))
        .expect("chunk 2 (hole at 1)");
    assert!(
        uploads.commit("up-2", "holey.png").is_err(),
        "hole detected"
    );

    // Path jail: files outside the uploads dir (and outside any allowed cwd
    // root) are rejected, including traversal attempts and the dir itself.
    let outside = tmp.path().join("outside.png");
    std::fs::write(&outside, b"nope").expect("outside file");
    assert!(
        uploads
            .read_chunk(&outside.to_string_lossy(), 0, &[])
            .is_err()
    );
    assert!(uploads.read_chunk("/etc/passwd", 0, &[]).is_err());
    let sneaky = format!("{}/../outside.png", uploads.dir().display());
    assert!(
        uploads.read_chunk(&sneaky, 0, &[]).is_err(),
        "traversal rejected"
    );
    // …but a workspace-known cwd root admits its files.
    let ok = uploads
        .read_chunk(&outside.to_string_lossy(), 0, &[tmp.path().to_path_buf()])
        .expect("cwd-rooted read");
    assert_eq!(BASE64.decode(&ok.data).expect("data"), b"nope");
    // Non-image extensions are refused even inside the jail (zeron parity).
    let text = PathBuf::from(uploads.dir()).join("notes.txt");
    std::fs::create_dir_all(uploads.dir()).expect("uploads dir");
    std::fs::write(&text, b"text").expect("txt");
    assert!(uploads.read_chunk(&text.to_string_lossy(), 0, &[]).is_err());

    // Bogus upload ids never become paths.
    assert!(uploads.append("../evil", "aGk=", None).is_err());
    assert!(uploads.commit("unknown-upload", "x.png").is_err());
}

// ---------------------------------------------------------------------------
// Titling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn titling_e2e_names_chat_and_renames_worktree_branch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Worktree root must be inside the tempdir (EngineCore reads the env-less
    // default otherwise) — create the worktree with a dedicated Repos handle.
    let repo_dir = tmp.path().join("repo");
    init_repo(&repo_dir).await;
    let repos = Repos::with_worktrees_root(
        &tmp.path().join("data"),
        "device-test",
        tmp.path().join("worktrees"),
    );
    let worktree = repos
        .create_worktree(&repo_dir, "main")
        .await
        .expect("worktree");

    let core = assemble_with_mock(
        &tmp.path().join("data"),
        vec![
            AgentEvent::TextDelta {
                text: "Fix Login Flow".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            },
        ],
    );
    let chat_id = "chat-title-1";
    core.workspace
        .create_space(
            "space-title",
            &core.device_id,
            &repo_dir.to_string_lossy(),
            None,
            true,
        )
        .expect("create space");
    core.workspace
        .create_chat(
            chat_id,
            Some("space-title"),
            None,
            None,
            Some(worktree.path.clone()),
        )
        .expect("create chat");
    core.workspace
        .set_chat_branch(chat_id, &worktree.branch)
        .expect("set branch");

    let request = cypher_proto::RunRequest {
        prompt: "please fix the login flow".into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: worktree.path.clone(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        pending_attachments: Vec::new(),
        resume: None,
        worktree: None,
    };
    core.sessions
        .dispatch(chat_id, HarnessId::Mock, request, None)
        .await
        .expect("dispatch");

    // The mock's scripted reply doubles as the titling model's output.
    let chat = common::wait_for_some("chat title", || {
        core.workspace
            .chat(chat_id)
            .ok()
            .flatten()
            .filter(|c| c.title.as_deref().is_some_and(|t| !t.is_empty()))
    })
    .await;
    assert_eq!(chat.title.as_deref(), Some("Fix Login Flow"));
    // Branch renamed from the title, chat row updated to match. The rename
    // lands in its own commit after the title — wait for it before asserting
    // (the title wait above can race it).
    common::wait_for_some("branch rename", || {
        core.workspace
            .chat(chat_id)
            .ok()
            .flatten()
            .filter(|c| c.branch.as_deref() == Some("cypher/fix-login-flow"))
            .map(|_c| ())
    })
    .await;
    let head = tokio::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(&worktree.path)
        .output()
        .await
        .expect("git");
    assert_eq!(
        String::from_utf8_lossy(&head.stdout).trim(),
        "cypher/fix-login-flow"
    );

    // A titled chat is never re-titled: rename, run again, title sticks.
    core.workspace
        .rename_chat(chat_id, "My Custom Name")
        .expect("rename");
    let request = cypher_proto::RunRequest {
        prompt: "another request".into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: worktree.path.clone(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        pending_attachments: Vec::new(),
        resume: None,
        worktree: None,
    };
    core.sessions
        .dispatch(chat_id, HarnessId::Mock, request, None)
        .await
        .expect("second dispatch");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let chat = core.workspace.chat(chat_id).expect("chat").expect("row");
    assert_eq!(chat.title.as_deref(), Some("My Custom Name"));
    core.shutdown().await;
}

#[tokio::test]
async fn rename_worktree_branch_guards_and_collisions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_dir = tmp.path().join("repo");
    init_repo(&repo_dir).await;
    let repos = Repos::with_worktrees_root(
        &tmp.path().join("data"),
        "device-test",
        tmp.path().join("worktrees"),
    );
    let wt = repos
        .create_worktree(&repo_dir, "main")
        .await
        .expect("worktree");
    let wt_path = Path::new(&wt.path);

    // Guard: expected branch mismatch → no-op, returns the actual branch.
    let unchanged = repos
        .rename_worktree_branch(wt_path, "cypher/not-this-one", "Some Title")
        .await
        .expect("guarded");
    assert_eq!(unchanged, wt.branch);

    // Happy path: renamed to the title slug.
    let renamed = repos
        .rename_worktree_branch(wt_path, &wt.branch, "Add Dark Mode!")
        .await
        .expect("renamed");
    assert_eq!(renamed, "cypher/add-dark-mode");

    // Already renamed → the guard (branch no longer cypher/<folder>) makes any
    // further title rename a no-op.
    let again = repos
        .rename_worktree_branch(wt_path, "cypher/add-dark-mode", "Different Title")
        .await
        .expect("second rename");
    assert_eq!(again, "cypher/add-dark-mode");

    // Collision: a second worktree whose title slug already exists gets the
    // stable hash suffix.
    let wt2 = repos
        .create_worktree(&repo_dir, "main")
        .await
        .expect("worktree 2");
    let renamed2 = repos
        .rename_worktree_branch(Path::new(&wt2.path), &wt2.branch, "Add Dark Mode!")
        .await
        .expect("suffixed rename");
    assert!(
        renamed2.starts_with("cypher/add-dark-mode-")
            && renamed2.len() == "cypher/add-dark-mode-".len() + 6,
        "suffixed: {renamed2}"
    );

    // Slug edge cases.
    assert_eq!(
        worktree_branch_from_title("  Fix `Login` Flow!  "),
        "cypher/fix-login-flow"
    );
    assert_eq!(worktree_branch_from_title("***"), "cypher/update");
    assert_eq!(
        worktree_branch_from_title("Cafe's Dark Mode"),
        "cypher/cafes-dark-mode"
    );
}

// ---------------------------------------------------------------------------
// RPC dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rpc_dispatch_for_upload_methods() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let core = assemble_with_mock(&tmp.path().join("data"), Vec::new());
    let client = cypher_rpc::memory_client(core.rpc_service());

    // Uploads: chunk → commit → readback over the wire.
    let payload = b"fake png bytes".to_vec();
    let ok = client
        .call(
            methods::UPLOAD_CHUNK,
            serde_json::json!({ "uploadId": "rpc-up", "data": BASE64.encode(&payload), "seq": 0 }),
        )
        .await
        .expect("UploadChunk");
    assert_eq!(ok["ok"], true);
    let committed = client
        .call(
            methods::UPLOAD_COMMIT,
            serde_json::json!({ "uploadId": "rpc-up", "fileName": "shot.png" }),
        )
        .await
        .expect("UploadCommit");
    let path = committed["path"].as_str().expect("path").to_string();
    assert!(path.ends_with("rpc-up-shot.png"));
    let chunk = client
        .call(
            methods::READ_ATTACHMENT_CHUNK,
            serde_json::json!({ "path": path, "offset": 0 }),
        )
        .await
        .expect("ReadAttachmentChunk");
    assert_eq!(chunk["mimeType"], "image/png");
    assert_eq!(chunk["done"], true);
    assert_eq!(
        BASE64
            .decode(chunk["data"].as_str().expect("data"))
            .expect("base64"),
        payload
    );
    // Jail holds over RPC too.
    assert!(
        client
            .call(
                methods::READ_ATTACHMENT_CHUNK,
                serde_json::json!({ "path": "/etc/passwd", "offset": 0 })
            )
            .await
            .is_err()
    );
    core.shutdown().await;
}
