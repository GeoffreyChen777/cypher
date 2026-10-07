//! "Don't work in a project" against a real engine: a chat minted through the
//! UI's exact wire shape (`Mutate createChat` with a `deviceId` and no
//! `spaceId`) stores cwd `~`, spawns its run from the host's REAL home dir,
//! and never mints a space row — the two failure modes of pre-#40 engines
//! (a phantom project at root, and the run dying on the literal `~`).

mod common;

use std::sync::{Arc, Mutex};

use cypher_doc::SessionCommandPayload;
use cypher_proto::{HarnessId, RunRequest};

use common::{complete_assistant_count, run_request, wait_for};

const CHAT: &str = "chat-projectless";

type RequestLog = Arc<Mutex<Vec<RunRequest>>>;

#[tokio::test(flavor = "multi_thread")]
async fn projectless_chat_runs_from_home_and_mints_no_space() {
    let tmp = tempfile::tempdir().unwrap();
    let requests: RequestLog = RequestLog::default();
    // Records every `RunRequest` it receives (the cwd probe), then completes
    // a one-line turn.
    let log = requests.clone();
    let harness = common::TestHarness::new(HarnessId::Mock, "Recording", move |request, _| {
        log.lock().expect("request log").push(request.clone());
        common::reply(
            HarnessId::Mock,
            &request,
            "sess-np",
            &format!("ack: {}", request.prompt),
        )
    });
    let core = common::engine_at(&tmp.path().join("data"), harness);

    // The composer's exact wire shape for "Don't work in a project": a
    // deviceId, no spaceId, no cwd.
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
        .expect("createChat without a space");
    // Pre-title so the auto-titler's own harness request stays out of the log.
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");

    let chat = core
        .workspace
        .chat(CHAT)
        .expect("read chat row")
        .expect("chat row exists");
    assert_eq!(chat.space_id, None, "project-less chat must carry no space");
    assert_eq!(chat.cwd.as_deref(), Some("~"), "cwd defaults to `~`");
    assert_eq!(chat.device_id, core.device_id);

    // Run exactly as the composer sends it: the chat's stored cwd, `~`.
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: RunRequest {
                    cwd: "~".into(),
                    ..run_request("hello from no project")
                },
                message_id: "msg-np-1".into(),

                agent_prompt: None,
            },
        )
        .expect("queue run command");
    wait_for(
        || complete_assistant_count(&core, CHAT) == 1,
        "turn to complete",
    )
    .await;

    // The harness must see the host's real home dir, not the literal `~`.
    let cwds: Vec<String> = requests
        .lock()
        .expect("request log")
        .iter()
        .map(|r| r.cwd.clone())
        .collect();
    let home = std::env::var("HOME").expect("HOME set in test env");
    assert_eq!(cwds, vec![home], "run spawns from the expanded home dir");

    // And no phantom project: the flow must not mint any space row.
    let spaces = core.workspace.read_spaces().expect("read spaces");
    assert!(
        spaces.is_empty(),
        "project-less chat minted a space: {spaces:?}"
    );

    core.shutdown().await;
}
