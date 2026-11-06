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
    common::wait_for_within(
        || {
            command_statuses(core, CHAT)
                .iter()
                .any(|(c, status, _)| c == id && *status != SessionCommandStatus::Pending)
        },
        &format!("{id} resolves"),
        Duration::from_secs(20),
    )
    .await;
    command_statuses(core, CHAT)
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
