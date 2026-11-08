//! Signed durable commands on the drain: the host signs the commands it
//! queues, checks each command against its own list entry before running
//! it, and rejects the ones device trust refuses. `DeviceTrust`'s own rules are unit
//! tested in `device_trust/tests.rs`; this stands in a keyed checksum for
//! the identity key.

mod common;

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cypher_doc::{
    CommandAuth, CommandSig, SessionCommandEntry, SessionCommandPayload, SessionCommandStatus,
};
use cypher_engine::CommandTrust;
use cypher_proto::HarnessId;

use common::{command_statuses, run_request};

const CHAT: &str = "chat-signed";

fn checksum(bytes: &[u8]) -> [u8; 64] {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let mut sig = [0u8; 64];
    for chunk in sig.chunks_mut(8) {
        chunk.copy_from_slice(&hasher.finish().to_be_bytes());
        hasher.write_u8(1);
    }
    sig
}

/// Signs with [`checksum`]; refuses unsigned commands, mismatches, and
/// commands from `dev-untrusted`.
struct FakeTrust;

#[async_trait::async_trait]
impl CommandTrust for FakeTrust {
    fn sign(&self, bytes: &[u8]) -> Option<[u8; 64]> {
        Some(checksum(bytes))
    }

    fn is_own_signature(&self, auth: &CommandAuth) -> bool {
        auth.sig == CommandSig::Present(checksum(&auth.signing_bytes))
    }

    fn is_receive_only(&self, device_id: &str) -> bool {
        device_id == "dev-untrusted"
    }

    fn strict(&self) -> bool {
        true
    }

    fn trusts_record(&self, signer: &str, auth: &CommandAuth) -> bool {
        !self.is_receive_only(signer) && self.is_own_signature(auth)
    }

    async fn check(&self, issued_by: &str, auth: &CommandAuth) -> Result<(), String> {
        match &auth.sig {
            CommandSig::Present(sig) if *sig == checksum(&auth.signing_bytes) => {}
            CommandSig::Present(_) | CommandSig::Malformed => return Err("tampered".into()),
            CommandSig::None => return Err("unsigned".into()),
        }
        if self.is_receive_only(issued_by) {
            return Err("receive-only".into());
        }
        Ok(())
    }
}

fn run(id: &str, issued_by: &str, prompt: &str) -> SessionCommandEntry {
    SessionCommandEntry {
        id: id.into(),
        payload: SessionCommandPayload::Run {
            request: run_request(prompt),
            message_id: format!("msg-{id}"),
            agent_prompt: None,
        },
        issued_by: issued_by.into(),
        issued_at: chrono::Utc::now().timestamp_millis(),
        based_on: None,
        expires_at: None,
        status: SessionCommandStatus::Pending,
        resolution: None,
        sent_at: None,
    }
}

async fn resolved(core: &cypher_engine::EngineCore, id: &str) -> (SessionCommandStatus, String) {
    resolved_in(core, CHAT, id).await
}

async fn resolved_in(
    core: &cypher_engine::EngineCore,
    chat: &str,
    id: &str,
) -> (SessionCommandStatus, String) {
    common::wait_for_within(
        || {
            command_statuses(core, chat)
                .iter()
                .any(|(c, status, _)| c == id && *status != SessionCommandStatus::Pending)
        },
        &format!("{id} resolves"),
        Duration::from_secs(20),
    )
    .await;
    command_statuses(core, chat)
        .into_iter()
        .find(|(c, ..)| c == id)
        .map(|(_, status, resolution)| (status, resolution.unwrap_or_default()))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_runs_only_commands_device_trust_accepts() {
    let tmp = tempfile::tempdir().unwrap();
    let prompts: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = prompts.clone();
    let harness = common::TestHarness::new(HarnessId::Mock, "Recorder", move |request, _| {
        log.lock().unwrap().push(request.prompt.clone());
        common::reply(HarnessId::Mock, &request, "sess-signed", "ack")
    });
    let core = common::engine_at(&tmp.path().join("data"), harness);
    core.doc_host.set_command_trust(Arc::new(FakeTrust));
    let doc = core.doc_host.open(CHAT).unwrap();
    let doc = doc.doc();

    // What someone with write access to the doc can do without the key.
    doc.queue_command(&run("c-unsigned", &core.device_id, "unsigned"))
        .unwrap();
    assert_eq!(
        resolved(&core, "c-unsigned").await.0,
        SessionCommandStatus::Rejected
    );
    doc.queue_signed_command(CHAT, &run("c-receive-only", "dev-untrusted", "ro"), |b| {
        Some(checksum(b))
    })
    .unwrap();
    assert_eq!(
        resolved(&core, "c-receive-only").await,
        (SessionCommandStatus::Rejected, "receive-only".into())
    );
    // A signature for another chat doesn't carry over.
    doc.queue_signed_command(
        "chat-other",
        &run("c-replayed", &core.device_id, "replay"),
        |b| Some(checksum(b)),
    )
    .unwrap();
    assert_eq!(
        resolved(&core, "c-replayed").await.0,
        SessionCommandStatus::Rejected
    );
    // A signature that doesn't match the entry.
    doc.queue_signed_command(CHAT, &run("c-bad-sig", &core.device_id, "forged"), |_| {
        Some([0; 64])
    })
    .unwrap();
    assert_eq!(
        resolved(&core, "c-bad-sig").await.0,
        SessionCommandStatus::Rejected
    );

    // Retry lends this device's signature only to what it signed itself: a
    // planted command claiming to be this device's stays unsigned.
    doc.queue_command(&run("c-planted", &core.device_id, "planted"))
        .unwrap();
    assert_eq!(
        resolved(&core, "c-planted").await.0,
        SessionCommandStatus::Rejected
    );
    let retried = core.doc_host.retry_command(CHAT, "c-planted").unwrap();
    assert_eq!(
        resolved(&core, &retried).await.0,
        SessionCommandStatus::Rejected
    );
    // One it did sign (expired here) is signed again and runs.
    let mut expired = run("c-expired", &core.device_id, "signed and retried");
    expired.expires_at = Some(1);
    doc.queue_signed_command(CHAT, &expired, |b| Some(checksum(b)))
        .unwrap();
    assert_eq!(
        resolved(&core, "c-expired").await.0,
        SessionCommandStatus::Expired
    );
    let retried = core.doc_host.retry_command(CHAT, "c-expired").unwrap();
    assert_eq!(
        resolved(&core, &retried).await.0,
        SessionCommandStatus::Applied
    );

    // The host's own command, queued the normal way, is signed and runs.
    let id = core
        .doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: run_request("signed by the host"),
                message_id: "msg-own".into(),
                agent_prompt: None,
            },
        )
        .unwrap();
    assert_eq!(resolved(&core, &id).await.0, SessionCommandStatus::Applied);
    let ran: Vec<_> = prompts
        .lock()
        .unwrap()
        .iter()
        .filter(|p| !p.contains("concise 3-5 word title"))
        .cloned()
        .collect();
    assert_eq!(ran, ["signed and retried", "signed by the host"]);
    core.shutdown().await;
}

const LAUNCH_CHAT: &str = "chat-launch-records";

async fn mutate(client: &cypher_rpc::RpcClient, params: serde_json::Value) {
    client
        .call(cypher_rpc::methods::MUTATE, params)
        .await
        .expect("mutate");
}

fn config(level: &str) -> serde_json::Value {
    serde_json::json!({ "harness": "mock", "model": null, "reasoning": null, "sandbox": level })
}

/// What a device that can edit workspace rows, but holds no trusted key, can
/// do: write the chat's row directly.
fn tamper_level(core: &cypher_engine::EngineCore, level: cypher_proto::SandboxLevel) {
    let mut row = core.workspace.chat_config(LAUNCH_CHAT).unwrap();
    row.sandbox = level;
    core.workspace.set_chat_config(LAUNCH_CHAT, &row).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_launch_from_signed_records_not_the_row() {
    use cypher_proto::SandboxLevel;

    let tmp = tempfile::tempdir().unwrap();
    let levels: Arc<Mutex<Vec<(String, SandboxLevel)>>> = Arc::default();
    let log = levels.clone();
    let harness = common::TestHarness::new(HarnessId::Mock, "Recorder", move |request, _| {
        if !request.prompt.contains("concise 3-5 word title") {
            log.lock()
                .unwrap()
                .push((request.prompt.clone(), request.sandbox));
        }
        common::reply(HarnessId::Mock, &request, "sess-launch", "ack")
    });
    let core = common::engine_at(&tmp.path().join("data"), harness);
    core.doc_host.set_command_trust(Arc::new(FakeTrust));
    core.workspace.set_launch_hook(core.doc_host.launch_hook());
    let client = cypher_rpc::memory_client(core.rpc_service());
    mutate(
        &client,
        serde_json::json!({ "op": "createChat", "chatId": LAUNCH_CHAT, "deviceId": core.device_id }),
    )
    .await;
    mutate(
        &client,
        serde_json::json!({ "op": "setChatConfig", "chatId": LAUNCH_CHAT, "config": config("workspace-write") }),
    )
    .await;

    let send = |prompt: &str| {
        let id = core
            .doc_host
            .queue_command(
                LAUNCH_CHAT,
                SessionCommandPayload::Run {
                    request: run_request(prompt),
                    message_id: format!("msg-{prompt}"),
                    agent_prompt: None,
                },
            )
            .unwrap();
        let (core, prompt) = (&core, prompt.to_string());
        async move {
            let (status, why) = resolved_in(core, LAUNCH_CHAT, &id).await;
            assert_eq!(status, SessionCommandStatus::Applied, "{prompt}: {why}");
        }
    };
    let level_of = |prompt: &str| {
        levels
            .lock()
            .unwrap()
            .iter()
            .find(|(p, _)| p == prompt)
            .map(|(_, level)| *level)
    };

    // Someone edits the row to Full access: runs keep the signed level.
    tamper_level(&core, SandboxLevel::DangerFullAccess);
    send("after the row edit").await;
    assert_eq!(
        level_of("after the row edit"),
        Some(SandboxLevel::WorkspaceWrite)
    );
    // A record from a device the host doesn't trust changes nothing either.
    let raise = cypher_doc::LaunchFact::Config(
        serde_json::from_value(config("danger-full-access")).unwrap(),
    );
    let doc = core.doc_host.open(LAUNCH_CHAT).unwrap();
    doc.doc()
        .append_launch_record(LAUNCH_CHAT, &raise, i64::MAX / 2, "dev-untrusted", |b| {
            Some(checksum(b))
        })
        .unwrap();
    send("after an untrusted record").await;
    assert_eq!(
        level_of("after an untrusted record"),
        Some(SandboxLevel::WorkspaceWrite)
    );
    // The user changing it here does.
    mutate(
        &client,
        serde_json::json!({ "op": "setChatConfig", "chatId": LAUNCH_CHAT, "config": config("read-only") }),
    )
    .await;
    send("after the user's change").await;
    assert_eq!(
        level_of("after the user's change"),
        Some(SandboxLevel::ReadOnly)
    );

    // Moving the chat by its row doesn't move it: this device still runs it.
    core.workspace
        .set_chat_host(LAUNCH_CHAT, "dev-other")
        .unwrap();
    send("after the row move").await;
    assert_eq!(level_of("after the row move"), Some(SandboxLevel::ReadOnly));
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_without_records_runs_at_most_workspace_write() {
    use cypher_proto::SandboxLevel;

    let tmp = tempfile::tempdir().unwrap();
    let levels: Arc<Mutex<Vec<SandboxLevel>>> = Arc::default();
    let log = levels.clone();
    let harness = common::TestHarness::new(HarnessId::Mock, "Recorder", move |request, _| {
        if !request.prompt.contains("concise 3-5 word title") {
            log.lock().unwrap().push(request.sandbox);
        }
        common::reply(HarnessId::Mock, &request, "sess-legacy", "ack")
    });
    let core = common::engine_at(&tmp.path().join("data"), harness);
    // Created before records existed: row only.
    core.workspace
        .create_chat(
            LAUNCH_CHAT,
            None,
            Some(&core.device_id),
            Some(serde_json::from_value(config("danger-full-access")).unwrap()),
            None,
        )
        .unwrap();
    core.doc_host.set_command_trust(Arc::new(FakeTrust));
    core.workspace.set_launch_hook(core.doc_host.launch_hook());
    // Queued signed, straight into the doc, as an older sender would.
    core.doc_host
        .open(LAUNCH_CHAT)
        .unwrap()
        .doc()
        .queue_signed_command(
            LAUNCH_CHAT,
            &run("c-legacy", &core.device_id, "legacy"),
            |b| Some(checksum(b)),
        )
        .unwrap();
    let (status, why) = resolved_in(&core, LAUNCH_CHAT, "c-legacy").await;
    assert_eq!(status, SessionCommandStatus::Applied, "{why}");
    assert_eq!(levels.lock().unwrap()[..], [SandboxLevel::WorkspaceWrite]);
    core.shutdown().await;
}
