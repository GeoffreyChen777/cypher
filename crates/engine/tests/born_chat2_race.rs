//! The born-chat2 race: the composer attaches the transcript watch — opening
//! the chat doc — BEFORE its own `Mutate createChat` lands, so `open()` sees
//! no registry row. An absent row must mean "being born on chat2": the
//! transcript written under that open must survive the row's arrival and a
//! restart.

mod common;

use cypher_doc::{MessageRole, SessionCommandPayload};
use cypher_engine::EngineCore;
use cypher_proto::HarnessId;

use common::{complete_assistant_count, entries, run_request, wait_for};

const CHAT: &str = "chat-born-gen2-race";

/// Completes a one-line turn (the transcript payload the test asserts on).
fn assemble(dir: &std::path::Path) -> EngineCore {
    let harness = common::TestHarness::new(HarnessId::Mock, "OneLiner", |request, _| {
        common::reply(
            HarnessId::Mock,
            &request,
            "sess-race",
            "the codeword is PINEAPPLE",
        )
    });
    common::engine_at(dir, harness)
}

#[tokio::test(flavor = "multi_thread")]
async fn transcript_survives_open_racing_create_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");

    let watch_holder;
    {
        let core = assemble(&dir);

        // The racing transcript watch: the doc opens BEFORE any registry row
        // exists, and a live doc ref (the run, in production) pins the handle
        // across the row's arrival so no drop-and-reopen heal can save us —
        // the open itself must land on chat2.
        let handle = core.doc_host.open(CHAT).expect("open before createChat");
        watch_holder = handle.watch_messages();
        let live_writer_ref = handle.doc_arc();

        // The mint lands a beat later, exactly as the composer sends it.
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
            .expect("createChat lands after the open");
        // Pre-title so the auto-titler's harness request stays out of the run.
        core.workspace
            .rename_chat(CHAT, "Pre-titled")
            .expect("rename chat");

        core.doc_host
            .queue_command(
                CHAT,
                SessionCommandPayload::Run {
                    request: cypher_proto::RunRequest {
                        cwd: "~".into(),
                        ..run_request("what's the codeword?")
                    },
                    message_id: "msg-race-1".into(),

                    agent_prompt: None,
                },
            )
            .expect("queue run command");
        wait_for(
            || complete_assistant_count(&core, CHAT) == 1,
            "turn to complete",
        )
        .await;

        // The row arriving mid-life must not blank the transcript.
        let mid = entries(&core, CHAT);
        assert!(
            mid.iter().any(|e| e.role == MessageRole::User),
            "user message lost after registry row arrived: {mid:?}"
        );

        drop(live_writer_ref);
        core.shutdown().await;
    }
    drop(watch_holder);

    // Restart on the same data dir: the transcript must have persisted.
    let core = assemble(&dir);
    let after = entries(&core, CHAT);
    assert_eq!(
        complete_assistant_count(&core, CHAT),
        1,
        "assistant turn lost across restart: {after:?}"
    );
    assert!(
        after.iter().any(|e| e.role == MessageRole::User),
        "user message lost across restart: {after:?}"
    );
    core.shutdown().await;
}
