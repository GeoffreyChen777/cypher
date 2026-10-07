//! Sending a message to an archived chat revives it: `queue_command` flips the
//! workspace row's `archived` flag back off for message-bearing commands (Run,
//! Steer) — and only those; an Interrupt leaves the archive state alone.

mod common;

use cypher_doc::SessionCommandPayload;
use cypher_engine::EngineCore;
use cypher_proto::{HarnessId, RunRequest};

use common::{complete_assistant_count, run_request, wait_for};

const CHAT: &str = "chat-unarchive";

fn run_payload(message_id: &str) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: RunRequest {
            cwd: "~".into(),
            ..run_request("back from the archive")
        },
        message_id: message_id.into(),

        agent_prompt: None,
    }
}

fn archived(core: &EngineCore) -> bool {
    core.workspace
        .chat(CHAT)
        .expect("read chat row")
        .expect("chat row exists")
        .archived
}

#[tokio::test(flavor = "multi_thread")]
async fn sending_a_message_unarchives_the_chat() {
    let tmp = tempfile::tempdir().unwrap();
    // Completes a one-line turn for any request.
    let harness = common::TestHarness::new(HarnessId::Mock, "Ack", |request, _| {
        common::reply(
            HarnessId::Mock,
            &request,
            "sess-ua",
            &format!("ack: {}", request.prompt),
        )
    });
    let core = common::engine_at(&tmp.path().join("data"), harness);

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

    core.workspace
        .set_chat_archived(CHAT, true)
        .expect("archive chat");
    assert!(archived(&core), "precondition: chat is archived");

    // A Run command (the composer's send) revives the row synchronously.
    core.doc_host
        .queue_command(CHAT, run_payload("msg-ua-1"))
        .expect("queue run command");
    assert!(
        !archived(&core),
        "sending a message must unarchive the chat"
    );
    wait_for(
        || complete_assistant_count(&core, CHAT) == 1,
        "turn to complete",
    )
    .await;

    // A non-message command must NOT revive it.
    core.workspace
        .set_chat_archived(CHAT, true)
        .expect("re-archive chat");
    core.doc_host
        .queue_command(CHAT, SessionCommandPayload::Interrupt {})
        .expect("queue interrupt");
    assert!(
        archived(&core),
        "an interrupt is not a message and must not unarchive"
    );

    core.shutdown().await;
}
