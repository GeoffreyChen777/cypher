//! A chat's permission level is its row's `config.sandbox`: the composer's
//! permissions picker writes it with `setChatConfig`, while clients send a
//! fixed default in each `RunRequest`. The host must run with the row's level
//! (the Pi harness hands it to the Runtime's permission extension).

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cypher_doc::SessionCommandPayload;
use cypher_proto::{HarnessId, RunRequest, SandboxLevel};

use common::run_request;

const CHAT: &str = "chat-permission-level";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_chat_row_level_overrides_the_request() {
    let tmp = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<RunRequest>::new()));
    let log = requests.clone();
    let harness = common::TestHarness::new(HarnessId::Mock, "Recorder", move |request, _| {
        log.lock().unwrap().push(request.clone());
        common::reply(HarnessId::Mock, &request, "sess-level", "ok")
    });
    let core = common::engine_at(&tmp.path().join("data"), harness);
    let client = cypher_rpc::memory_client(core.rpc_service());
    client
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": CHAT, "deviceId": core.device_id }),
        )
        .await
        .expect("createChat");
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");
    client
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "setChatConfig",
                "chatId": CHAT,
                "config": {
                    "harness": "mock",
                    "model": null,
                    "reasoning": null,
                    "sandbox": "read-only"
                },
            }),
        )
        .await
        .expect("setChatConfig");

    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: RunRequest {
                    sandbox: SandboxLevel::WorkspaceWrite,
                    ..run_request("hello")
                },
                message_id: "msg-level-1".into(),
                agent_prompt: None,
            },
        )
        .expect("queue run");
    common::wait_for_within(
        || !requests.lock().unwrap().is_empty(),
        "the run",
        Duration::from_secs(20),
    )
    .await;
    assert_eq!(
        requests.lock().unwrap()[0].sandbox,
        SandboxLevel::ReadOnly,
        "the row's level, not the request's"
    );
}
