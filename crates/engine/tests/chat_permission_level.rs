//! A chat's permission level is its row's `config.sandbox`: the composer's
//! permissions picker writes it with `setChatConfig`, while clients send a
//! fixed default in each `RunRequest`. The host must run with the row's level
//! (the Pi harness hands it to the Runtime's permission extension). Pi reads
//! the level when its process starts, so a level change must also end a
//! parked process rather than route the next turn into it.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use cypher_doc::SessionCommandPayload;
use cypher_proto::{AgentEvent, HarnessId, RunRequest, SandboxLevel, SessionStatus};

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

const PARKED_CHAT: &str = "chat-permission-level-parked";

async fn set_level(client: &cypher_rpc::RpcClient, chat: &str, level: &str) {
    client
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "setChatConfig",
                "chatId": chat,
                "config": { "harness": "mock", "model": null, "reasoning": null, "sandbox": level },
            }),
        )
        .await
        .expect("setChatConfig");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_level_change_respawns_the_parked_process() {
    let tmp = tempfile::tempdir().unwrap();
    let launches = Arc::new(Mutex::new(Vec::<SandboxLevel>::new()));
    let log = launches.clone();
    // Persistent: answers, parks, and answers each steer until interrupted.
    let harness = common::TestHarness::new(HarnessId::Mock, "Parking", move |request, controls| {
        if request.prompt.contains("concise 3-5 word title") {
            return common::script(vec![common::done("sess-parked")]);
        }
        log.lock().unwrap().push(request.sandbox);
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let mut steering = controls.steering;
        let interrupt = controls.interrupt.clone();
        tokio::spawn(async move {
            let _ = tx.send(common::session_started(
                HarnessId::Mock,
                "m",
                "/tmp",
                "sess-parked",
                "a-parked",
            ));
            let _ = tx.send(common::text("first"));
            let _ = tx.send(common::done("sess-parked"));
            loop {
                tokio::select! {
                    steer = steering.recv() => match steer {
                        Some(_) => {
                            let _ = tx.send(AgentEvent::Steered {
                                assistant_message_id: None,
                                next_assistant_message_id: None,
                            });
                            let _ = tx.send(common::text("routed"));
                            let _ = tx.send(common::done("sess-parked"));
                        }
                        None => break,
                    },
                    _ = interrupt.cancelled() => break,
                }
            }
        });
        Ok(common::channel_stream(rx))
    })
    .steering();
    let core = common::engine_at(&tmp.path().join("data"), harness);
    let client = cypher_rpc::memory_client(core.rpc_service());
    client
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": PARKED_CHAT, "deviceId": core.device_id }),
        )
        .await
        .expect("createChat");
    core.workspace
        .rename_chat(PARKED_CHAT, "Pre-titled")
        .expect("rename chat");
    set_level(&client, PARKED_CHAT, "workspace-write").await;

    let send = |command: SessionCommandPayload| {
        core.doc_host
            .queue_command(PARKED_CHAT, command)
            .expect("queue command")
    };
    let park = || async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        common::wait_for(
            || common::status(&core, PARKED_CHAT) == Some(SessionStatus::Idle),
            "park",
        )
        .await;
    };
    send(SessionCommandPayload::Run {
        request: run_request("hello"),
        message_id: "msg-parked-1".into(),
        agent_prompt: None,
    });
    park().await;

    // /permissions → Full access, then a follow-up while the process is parked.
    set_level(&client, PARKED_CHAT, "danger-full-access").await;
    send(SessionCommandPayload::Steer {
        prompt: "now with full access".into(),
        message_id: Some("msg-parked-2".into()),
        agent_prompt: None,
    });
    common::wait_for_within(
        || launches.lock().unwrap().len() == 2,
        "a second launch",
        Duration::from_secs(20),
    )
    .await;
    assert_eq!(
        *launches.lock().unwrap(),
        vec![SandboxLevel::WorkspaceWrite, SandboxLevel::DangerFullAccess],
        "the follow-up runs in a new process at the new level"
    );
    core.sessions.shutdown().await;
}
