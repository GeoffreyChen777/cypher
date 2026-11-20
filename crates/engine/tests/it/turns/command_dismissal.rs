//! Dismissing a failed send (`DismissCommand`, the composer's × on "Send
//! failed"): the dismissal lands beside the ledger in the chat doc, covers
//! the message's earlier failed attempts, leaves the host's outcomes and
//! every signature alone, survives a restart, and follows the same device
//! gate as Retry.
//!
//! The chat is hosted by another device throughout, so this engine never
//! drains it: the test writes the host's outcomes itself, the way they'd
//! arrive by sync from an offline or refusing host.

use crate::support;

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use cypher_doc::{CommandAuth, CommandSig, SessionCommandPayload, SessionCommandStatus};
use cypher_engine::CommandTrust;

use support::run_request;

const CHAT: &str = "chat-dismiss";
const HOST: &str = "dev-host";

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

/// Signs with [`checksum`]; `receive_only` makes THIS device receive-only.
struct FakeTrust {
    receive_only: bool,
}

#[async_trait::async_trait]
impl CommandTrust for FakeTrust {
    fn sign(&self, bytes: &[u8]) -> Option<[u8; 64]> {
        Some(checksum(bytes))
    }

    fn is_own_signature(&self, auth: &CommandAuth) -> bool {
        auth.sig == CommandSig::Present(checksum(&auth.signing_bytes))
    }

    fn is_receive_only(&self, device_id: &str) -> bool {
        self.receive_only && device_id != HOST
    }

    fn strict(&self) -> bool {
        true
    }

    fn trusts_record(&self, _: &str, auth: &CommandAuth) -> bool {
        self.is_own_signature(auth)
    }

    async fn check(&self, _: &str, auth: &CommandAuth) -> Result<(), String> {
        if self.is_own_signature(auth) {
            Ok(())
        } else {
            Err("unsigned".into())
        }
    }
}

fn hosted_elsewhere(dir: &std::path::Path, receive_only: bool) -> cypher_engine::EngineCore {
    let core = support::assemble_mock(dir);
    core.doc_host
        .set_command_trust(Arc::new(FakeTrust { receive_only }));
    if core.workspace.chat(CHAT).unwrap().is_none() {
        core.workspace
            .create_chat(CHAT, None, Some(HOST), None, None)
            .unwrap();
    }
    core
}

fn send(core: &cypher_engine::EngineCore, message_id: &str) -> String {
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: run_request(message_id),
                message_id: message_id.into(),
                agent_prompt: None,
            },
        )
        .unwrap()
}

/// What the (remote) host writes when it refuses or expires a command.
fn host_resolves(core: &cypher_engine::EngineCore, id: &str, status: SessionCommandStatus) {
    core.doc_host
        .open(CHAT)
        .unwrap()
        .doc()
        .set_command_status(id, status, Some("host offline"))
        .unwrap();
}

/// `(id, status, resolution, dismissed)` per ledger entry.
fn ledger(
    core: &cypher_engine::EngineCore,
) -> Vec<(String, SessionCommandStatus, Option<String>, bool)> {
    core.doc_host
        .open(CHAT)
        .unwrap()
        .doc()
        .read_commands()
        .unwrap()
        .into_iter()
        .map(|c| (c.id, c.status, c.resolution, c.dismissed))
        .collect()
}

fn auths(core: &cypher_engine::EngineCore) -> Vec<(String, CommandAuth)> {
    core.doc_host
        .open(CHAT)
        .unwrap()
        .doc()
        .read_commands_with_auth(CHAT)
        .unwrap()
        .into_iter()
        .map(|(c, auth)| (c.id, auth))
        .collect()
}

fn dismissed(core: &cypher_engine::EngineCore, id: &str) -> bool {
    ledger(core).iter().any(|(c, .., d)| c == id && *d)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dismissal_covers_the_messages_failed_attempts_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let core = hosted_elsewhere(&data, false);

    let first = send(&core, "m1");
    host_resolves(&core, &first, SessionCommandStatus::Rejected);
    let retry = core.doc_host.retry_command(CHAT, &first).unwrap();
    // A retry in flight owns the message: nothing to dismiss yet.
    let err = core.doc_host.dismiss_command(CHAT, &first).unwrap_err();
    assert!(err.to_string().contains("already pending"), "{err}");
    let err = core.doc_host.dismiss_command(CHAT, &retry).unwrap_err();
    assert!(err.to_string().contains("only failed"), "{err}");
    host_resolves(&core, &retry, SessionCommandStatus::Expired);
    // Another message's failure, and a non-message command that failed.
    let other = send(&core, "m2");
    host_resolves(&core, &other, SessionCommandStatus::Rejected);
    let interrupt = core
        .doc_host
        .queue_command(CHAT, SessionCommandPayload::Interrupt {})
        .unwrap();
    host_resolves(&core, &interrupt, SessionCommandStatus::Rejected);

    let auth_before = auths(&core);
    let mut ids = core.doc_host.dismiss_command(CHAT, &retry).unwrap();
    ids.sort();
    let mut expected = vec![first.clone(), retry.clone()];
    expected.sort();
    assert_eq!(ids, expected, "the latest attempt and the earlier one");
    assert!(dismissed(&core, &first) && dismissed(&core, &retry));
    assert!(!dismissed(&core, &other), "another message stays failed");

    // Not an outcome write: the host's statuses and resolutions stand, and
    // every entry's signed bytes and signature are what they were.
    let statuses: Vec<_> = ledger(&core)
        .into_iter()
        .map(|(id, status, resolution, _)| (id, status, resolution))
        .collect();
    assert!(statuses.contains(&(
        first.clone(),
        SessionCommandStatus::Rejected,
        Some("host offline".into())
    )));
    assert!(statuses.contains(&(
        retry.clone(),
        SessionCommandStatus::Expired,
        Some("host offline".into())
    )));
    assert_eq!(auths(&core), auth_before);

    // Idempotent.
    assert_eq!(
        core.doc_host.dismiss_command(CHAT, &retry).unwrap().len(),
        2
    );
    // Only failed message commands of this chat.
    let err = core.doc_host.dismiss_command(CHAT, &interrupt).unwrap_err();
    assert!(err.to_string().contains("only message commands"), "{err}");
    assert!(!dismissed(&core, &interrupt));
    let err = core.doc_host.dismiss_command(CHAT, "nope").unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");

    // Through the RPC surface, as the composer calls it.
    let client = cypher_rpc::memory_client(core.rpc_service());
    let reply = client
        .call(
            cypher_rpc::methods::DISMISS_COMMAND,
            serde_json::json!({ "chatId": CHAT, "commandId": other }),
        )
        .await
        .expect("DismissCommand");
    assert_eq!(reply["commandIds"], serde_json::json!([other]));
    // ...and the commands watch carries the flag to the UI.
    let mut rx = client
        .subscribe(
            cypher_rpc::methods::WATCH_DOC_COMMANDS,
            serde_json::json!({ "chatId": CHAT }),
        )
        .await
        .expect("WatchDocCommands");
    let frame = rx.recv().await.expect("first frame");
    let flagged: Vec<&str> = frame
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["dismissed"] == serde_json::json!(true))
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(flagged.len(), 3);
    assert!(
        frame
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["id"] == serde_json::json!(interrupt))
            .all(|c| c.get("dismissed").is_none()),
        "undismissed entries keep their old frame shape"
    );
    drop(rx);
    drop(client);

    // A retry of a dismissed attempt is still this device's own signature:
    // the dismissal didn't touch the bytes it signed.
    let again = core.doc_host.retry_command(CHAT, &retry).unwrap();
    let signed = auths(&core)
        .into_iter()
        .find(|(id, _)| *id == again)
        .unwrap()
        .1;
    assert_eq!(
        signed.sig,
        CommandSig::Present(checksum(&signed.signing_bytes))
    );
    host_resolves(&core, &again, SessionCommandStatus::Rejected);
    assert!(!dismissed(&core, &again), "a new failure shows again");

    // Durable: a restarted engine reads the same dismissals.
    core.shutdown().await;
    drop(core);
    let core = hosted_elsewhere(&data, false);
    assert!(dismissed(&core, &first) && dismissed(&core, &retry) && dismissed(&core, &other));
    assert!(!dismissed(&core, &again));
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_receive_only_device_cannot_dismiss_on_another_hosts_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    // Queued while this device still had full control.
    let core = hosted_elsewhere(&data, false);
    let id = send(&core, "m1");
    host_resolves(&core, &id, SessionCommandStatus::Rejected);
    core.doc_host
        .set_command_trust(Arc::new(FakeTrust { receive_only: true }));

    let err = core.doc_host.dismiss_command(CHAT, &id).unwrap_err();
    assert!(err.to_string().contains("receive-only"), "{err}");
    assert!(!dismissed(&core, &id));
    let err = core.doc_host.retry_command(CHAT, &id).unwrap_err();
    assert!(
        err.to_string().contains("receive-only"),
        "same gate as Retry"
    );

    // Its own chat it still may.
    core.workspace.set_chat_host(CHAT, &core.device_id).unwrap();
    assert_eq!(
        core.doc_host.dismiss_command(CHAT, &id).unwrap(),
        std::slice::from_ref(&id)
    );
    assert!(dismissed(&core, &id));
    core.shutdown().await;
}
