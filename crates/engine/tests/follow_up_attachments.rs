//! A follow-up message's attachments reach the harness with it. Only a run's
//! FIRST prompt used to carry `attachments`: a message routed into a parked
//! (or live) process went through the steering mailbox as text alone, and a
//! Steer command — which has no attachments field — never had any, so pi saw
//! just the refs trailer and the model had to open the file itself.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use cypher_doc::SessionCommandPayload;
use cypher_engine::EngineCore;
use cypher_proto::attachment_refs::with_refs;
use cypher_proto::{AgentEvent, HarnessId, RunRequest, SessionStatus};

use common::run_request;

const CHAT: &str = "chat-follow-up-attachments";
const SHOT: &str = "/tmp/cypher-follow-up/shot.png";

/// `(prompt, attachments)` of each launch and of each mailbox message.
#[derive(Default)]
struct Seen {
    launches: Vec<(String, Vec<String>)>,
    mailbox: Vec<(String, Vec<String>)>,
}

struct Rig {
    core: EngineCore,
    seen: Arc<Mutex<Seen>>,
    _dir: tempfile::TempDir,
}

/// A persistent session like pi's: answers, parks, and answers each mailbox
/// message (Steered, reply, Done) until interrupted.
async fn assemble() -> Rig {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let log = seen.clone();
    let harness = common::TestHarness::new(HarnessId::Mock, "Parking", move |request, controls| {
        if request.prompt.contains("concise 3-5 word title") {
            return common::script(vec![common::done("sess-follow-up")]);
        }
        log.lock()
            .unwrap()
            .launches
            .push((request.prompt.clone(), request.attachments.clone()));
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let mut steering = controls.steering;
        let interrupt = controls.interrupt.clone();
        let log = log.clone();
        tokio::spawn(async move {
            let _ = tx.send(common::session_started(
                HarnessId::Mock,
                "m",
                "/tmp",
                "sess-follow-up",
                "a-follow-up",
            ));
            let _ = tx.send(common::text("first"));
            let _ = tx.send(common::done("sess-follow-up"));
            loop {
                tokio::select! {
                    steer = steering.recv() => match steer {
                        Some(message) => {
                            log.lock()
                                .unwrap()
                                .mailbox
                                .push((message.prompt, message.attachments));
                            let _ = tx.send(AgentEvent::Steered {
                                assistant_message_id: None,
                                next_assistant_message_id: None,
                            });
                            let _ = tx.send(common::text("routed"));
                            let _ = tx.send(common::done("sess-follow-up"));
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
    let dir = tempfile::tempdir().unwrap();
    let core = common::engine_at(&dir.path().join("data"), harness);
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
    Rig {
        core,
        seen,
        _dir: dir,
    }
}

impl Rig {
    fn send(&self, command: SessionCommandPayload) {
        self.core
            .doc_host
            .queue_command(CHAT, command)
            .expect("queue command");
    }

    fn run(&self, request: RunRequest, message_id: &str) {
        self.send(SessionCommandPayload::Run {
            request,
            message_id: message_id.into(),
            agent_prompt: None,
        });
    }

    fn steer(&self, prompt: String, message_id: &str) {
        self.send(SessionCommandPayload::Steer {
            prompt,
            message_id: Some(message_id.into()),
            agent_prompt: None,
        });
    }

    async fn park(&self) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        common::wait_for(
            || common::status(&self.core, CHAT) == Some(SessionStatus::Idle),
            "park",
        )
        .await;
    }

    async fn mailbox(&self) -> (String, Vec<String>) {
        common::wait_for_within(
            || !self.seen.lock().unwrap().mailbox.is_empty(),
            "a mailbox message",
            Duration::from_secs(20),
        )
        .await;
        self.seen.lock().unwrap().mailbox[0].clone()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_routed_into_the_parked_process_keeps_its_attachments() {
    let rig = assemble().await;
    rig.run(run_request("hello"), "msg-1");
    rig.park().await;

    let prompt = with_refs("what is this?", &[SHOT.into()]);
    rig.run(
        RunRequest {
            attachments: vec![SHOT.into()],
            ..run_request(&prompt)
        },
        "msg-2",
    );
    assert_eq!(rig.mailbox().await, (prompt, vec![SHOT.to_string()]));
    assert_eq!(
        rig.seen.lock().unwrap().launches.len(),
        1,
        "routed, not respawned"
    );
    rig.core.sessions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_carries_the_attachments_its_prompt_lists() {
    let rig = assemble().await;
    rig.run(run_request("hello"), "msg-1");
    rig.park().await;

    let prompt = with_refs("and this one?", &[SHOT.into()]);
    rig.steer(prompt.clone(), "msg-2");
    assert_eq!(rig.mailbox().await, (prompt, vec![SHOT.to_string()]));
    rig.core.sessions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_with_no_live_run_launches_with_its_own_attachments() {
    let rig = assemble().await;
    rig.run(
        RunRequest {
            attachments: vec!["/tmp/cypher-follow-up/earlier.png".into()],
            ..run_request("hello")
        },
        "msg-1",
    );
    rig.park().await;
    rig.core.sessions.interrupt(CHAT).await.expect("interrupt");
    rig.park().await;

    // The new turn reuses the last run's config, but not its images.
    let prompt = with_refs("and now?", &[SHOT.into()]);
    rig.steer(prompt.clone(), "msg-2");
    common::wait_for_within(
        || rig.seen.lock().unwrap().launches.len() == 2,
        "a second launch",
        Duration::from_secs(20),
    )
    .await;
    assert_eq!(
        rig.seen.lock().unwrap().launches[1],
        (prompt, vec![SHOT.to_string()])
    );
    rig.core.sessions.shutdown().await;
}
