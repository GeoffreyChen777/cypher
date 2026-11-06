use std::collections::HashMap;

use super::*;

/// The edge's TrustRoom in memory: validated, compare-and-swap appends and
/// pairing mailboxes. Tests reach in to make it misbehave.
#[derive(Default)]
struct FakeEdge {
    entries: Mutex<Vec<TrustEntry>>,
    mailboxes: Mutex<HashMap<(String, String), serde_json::Value>>,
}

#[async_trait]
impl TrustEdge for FakeEdge {
    async fn chain(&self) -> Result<Vec<TrustEntry>, String> {
        Ok(lock(&self.entries).clone())
    }

    async fn append(&self, entry: &TrustEntry) -> Result<Append, String> {
        let mut entries = lock(&self.entries);
        if entry.seq != entries.len() as u64 {
            return Ok(Append::Conflict);
        }
        let mut next = entries.clone();
        next.push(entry.clone());
        if let Err(err) = TrustChain::verify(next) {
            return Ok(Append::Invalid(err.to_string()));
        }
        entries.push(entry.clone());
        Ok(Append::Done)
    }

    async fn put(&self, mailbox: &str, slot: &str, body: serde_json::Value) -> Result<(), String> {
        lock(&self.mailboxes).insert((mailbox.into(), slot.into()), body);
        Ok(())
    }

    async fn get(&self, mailbox: &str, slot: &str) -> Result<Option<serde_json::Value>, String> {
        Ok(lock(&self.mailboxes)
            .get(&(mailbox.into(), slot.into()))
            .cloned())
    }

    async fn delete(&self, mailbox: &str) -> Result<(), String> {
        lock(&self.mailboxes).retain(|(id, _), _| id != mailbox);
        Ok(())
    }
}

struct Device {
    trust: DeviceTrust,
    _dir: tempfile::TempDir,
}

fn device(id: &str, edge: &Arc<FakeEdge>) -> Device {
    let dir = tempfile::tempdir().unwrap();
    let trust = DeviceTrust::open(dir.path(), "user-1", id, id, edge.clone()).unwrap();
    Device { trust, _dir: dir }
}

/// Wait (in paused time) until `check` holds.
async fn until(mut check: impl FnMut() -> bool) {
    for _ in 0..2_000 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("condition never held");
}

/// A founds the group; B joins through A's code.
async fn paired_pair(edge: &Arc<FakeEdge>) -> (Device, Device) {
    let a = device("dev-a", edge);
    let b = device("dev-b", edge);
    a.trust.sync().await.unwrap();
    b.trust.sync().await.unwrap();
    let Some(PairingStatus::Inviting { code, .. }) = a.trust.start_invite().await.unwrap().pairing
    else {
        panic!("no code shown");
    };
    b.trust.join(&code).await.unwrap();
    until(|| {
        matches!(
            a.trust.status().pairing,
            Some(PairingStatus::Inviting {
                request: Some(_),
                ..
            })
        )
    })
    .await;
    a.trust.approve().await.unwrap();
    until(|| b.trust.status().state == TrustState::Paired).await;
    (a, b)
}

#[tokio::test(start_paused = true)]
async fn the_first_device_founds_the_group_and_the_next_one_is_unpaired() {
    let edge = Arc::new(FakeEdge::default());
    let a = device("dev-a", &edge);
    assert_eq!(a.trust.status().state, TrustState::Unknown);
    a.trust.sync().await.unwrap();
    assert_eq!(a.trust.status().state, TrustState::Paired);
    assert_eq!(lock(&edge.entries).len(), 1);

    let b = device("dev-b", &edge);
    b.trust.sync().await.unwrap();
    let status = b.trust.status();
    assert_eq!(status.state, TrustState::Unpaired);
    assert!(status.members.is_empty());
    assert_eq!(b.trust.peer_key("dev-a"), None);
}

#[tokio::test(start_paused = true)]
async fn pairing_with_a_code_makes_both_devices_trust_each_other() {
    let edge = Arc::new(FakeEdge::default());
    let (a, b) = paired_pair(&edge).await;
    a.trust.sync().await.unwrap();
    for (one, other, other_id) in [(&a, &b, "dev-b"), (&b, &a, "dev-a")] {
        let key = other.trust.keys().dh_public();
        assert_eq!(one.trust.peer_key(other_id), Some(key));
        assert!(one.trust.is_member(other_id, &key).await);
        assert!(!one.trust.is_member(other_id, &[7; 32]).await);
        let ids: Vec<_> = one
            .trust
            .status()
            .members
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["dev-a", "dev-b"]);
    }
    assert!(
        lock(&edge.mailboxes).is_empty(),
        "the joiner clears the mailbox"
    );

    // State survives a restart.
    let reopened =
        DeviceTrust::open(b._dir.path(), "user-1", "dev-b", "dev-b", edge.clone()).unwrap();
    assert_eq!(reopened.status().state, TrustState::Paired);
    assert_eq!(reopened.peer_key("dev-a"), a.trust.peer_key("dev-a"));
}

#[tokio::test(start_paused = true)]
async fn a_join_request_without_the_code_is_ignored() {
    let edge = Arc::new(FakeEdge::default());
    let a = device("dev-a", &edge);
    a.trust.sync().await.unwrap();
    let Some(PairingStatus::Inviting { code, .. }) = a.trust.start_invite().await.unwrap().pairing
    else {
        panic!("no code shown");
    };
    // The edge knows the mailbox but not the code: it can write a request,
    // not MAC one.
    let code = PairingCode::parse(&code).unwrap();
    let forged = PairingCode::generate().join(&DeviceKeys::generate("evil").public("Evil"));
    edge.put(
        &code.mailbox(),
        "join",
        serde_json::to_value(forged).unwrap(),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert!(matches!(
        a.trust.status().pairing,
        Some(PairingStatus::Inviting { request: None, .. })
    ));
    assert!(a.trust.approve().await.is_err());
}

#[tokio::test(start_paused = true)]
async fn a_joiner_refuses_a_device_list_the_member_did_not_approve() {
    let edge = Arc::new(FakeEdge::default());
    let a = device("dev-a", &edge);
    let b = device("dev-b", &edge);
    a.trust.sync().await.unwrap();
    let Some(PairingStatus::Inviting { code, .. }) = a.trust.start_invite().await.unwrap().pairing
    else {
        panic!("no code shown");
    };
    b.trust.join(&code).await.unwrap();
    until(|| {
        matches!(
            a.trust.status().pairing,
            Some(PairingStatus::Inviting {
                request: Some(_),
                ..
            })
        )
    })
    .await;
    a.trust.approve().await.unwrap();
    // Before B fetches it, the edge swaps in a group of its own that "adds" B.
    let attacker = DeviceKeys::generate("attacker");
    let mut fake = TrustChain::genesis(&attacker, "Attacker", 1).unwrap();
    let b_public = b.trust.public();
    fake.append(fake.propose_add(&attacker, b_public, 2).unwrap())
        .unwrap();
    *lock(&edge.entries) = fake.entries().to_vec();
    until(|| {
        matches!(
            b.trust.status().pairing,
            Some(PairingStatus::Joining { error: Some(_) })
        )
    })
    .await;
    assert_ne!(b.trust.status().state, TrustState::Paired);
    assert_eq!(b.trust.peer_key("attacker"), None);
}

#[tokio::test(start_paused = true)]
async fn a_removed_device_loses_trust_and_can_pair_again_with_new_keys() {
    let edge = Arc::new(FakeEdge::default());
    let (a, b) = paired_pair(&edge).await;
    let old_key = b.trust.keys().dh_public();
    a.trust.remove("dev-b").await.unwrap();
    assert!(!a.trust.is_member("dev-b", &old_key).await);
    b.trust.sync().await.unwrap();
    assert_eq!(b.trust.status().state, TrustState::Removed);
    assert_eq!(b.trust.peer_key("dev-a"), None);

    let Some(PairingStatus::Inviting { code, .. }) = a.trust.start_invite().await.unwrap().pairing
    else {
        panic!("no code shown");
    };
    b.trust.join(&code).await.unwrap();
    assert_ne!(
        b.trust.keys().dh_public(),
        old_key,
        "re-pairing takes new keys"
    );
    until(|| {
        matches!(
            a.trust.status().pairing,
            Some(PairingStatus::Inviting {
                request: Some(_),
                ..
            })
        )
    })
    .await;
    a.trust.approve().await.unwrap();
    until(|| b.trust.status().state == TrustState::Paired).await;
    assert!(!a.trust.is_member("dev-b", &old_key).await);
    assert!(
        a.trust
            .is_member("dev-b", &b.trust.keys().dh_public())
            .await
    );
}

#[tokio::test(start_paused = true)]
async fn the_chain_never_rolls_back_and_a_lost_copy_is_republished() {
    let edge = Arc::new(FakeEdge::default());
    let (a, _b) = paired_pair(&edge).await;
    let held = a.trust.status().members.len();

    // The server lost everything: A puts it back.
    lock(&edge.entries).clear();
    a.trust.sync().await.unwrap();
    assert_eq!(lock(&edge.entries).len(), 2);

    // The server forks history: A keeps its copy and says so.
    let attacker = DeviceKeys::generate("attacker");
    *lock(&edge.entries) = TrustChain::genesis(&attacker, "Attacker", 1)
        .unwrap()
        .entries()
        .to_vec();
    a.trust.sync().await.unwrap();
    let status = a.trust.status();
    assert!(status.conflict);
    assert_eq!(status.state, TrustState::Paired);
    assert_eq!(status.members.len(), held);
    assert_eq!(a.trust.peer_key("attacker"), None);
}

#[tokio::test(start_paused = true)]
async fn only_paired_devices_is_a_group_policy() {
    let edge = Arc::new(FakeEdge::default());
    let (a, b) = paired_pair(&edge).await;
    assert!(!a.trust.require_paired() && !b.trust.require_paired());
    a.trust.set_require_paired(true).await.unwrap();
    // Every member picks it up on its next sync, and it survives a restart.
    b.trust.sync().await.unwrap();
    assert!(b.trust.require_paired());
    assert!(b.trust.status().require_paired);
    let reopened =
        DeviceTrust::open(b._dir.path(), "user-1", "dev-b", "dev-b", edge.clone()).unwrap();
    assert!(reopened.require_paired());
    b.trust.set_require_paired(false).await.unwrap();
    a.trust.sync().await.unwrap();
    assert!(!a.trust.require_paired());

    // An unpaired device cannot change it.
    let c = device("dev-c", &edge);
    c.trust.sync().await.unwrap();
    assert!(c.trust.set_require_paired(true).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn a_receive_only_device_is_controlled_but_changes_nothing() {
    let edge = Arc::new(FakeEdge::default());
    let (a, b) = paired_pair(&edge).await;
    assert!(!a.trust.status().require_paired);

    // Making B receive-only turns on "Only paired devices" first.
    let status = a.trust.set_receive_only("dev-b", true).await.unwrap();
    assert!(status.require_paired);
    assert!(
        status
            .members
            .iter()
            .any(|m| m.id == "dev-b" && m.receive_only)
    );
    assert!(a.trust.member_is_receive_only("dev-b"));
    assert!(!a.trust.local_receive_only());
    // A device can't change its own access.
    assert!(a.trust.set_receive_only("dev-a", true).await.is_err());

    b.trust.sync().await.unwrap();
    assert!(b.trust.status().receive_only);
    assert!(b.trust.local_receive_only());
    // B signs nothing.
    for err in [
        b.trust.start_invite().await.err(),
        b.trust.remove("dev-a").await.err(),
        b.trust.set_require_paired(false).await.err(),
        b.trust.set_receive_only("dev-a", true).await.err(),
    ] {
        assert!(err.unwrap().contains("receive-only"));
    }
    // The policy stays on until B has full control again.
    let err = a.trust.set_require_paired(false).await.unwrap_err();
    assert!(err.contains("full control"), "{err}");
    a.trust.set_receive_only("dev-b", false).await.unwrap();
    a.trust.set_require_paired(false).await.unwrap();
    b.trust.sync().await.unwrap();
    assert!(!b.trust.local_receive_only());
    assert!(!b.trust.status().require_paired);
}

/// What a host sees of a command `issued_by` queued into `chat`, signed by
/// `signer` (or not).
fn command(chat: &str, issued_by: &str, signer: Option<&DeviceTrust>) -> CommandAuth {
    let doc = cypher_doc::SessionDoc::init(chat).unwrap();
    let entry = cypher_doc::SessionCommandEntry {
        id: "c1".into(),
        payload: cypher_doc::SessionCommandPayload::Interrupt {},
        issued_by: issued_by.into(),
        issued_at: 1,
        based_on: None,
        expires_at: None,
        status: cypher_doc::SessionCommandStatus::Pending,
        resolution: None,
        sent_at: None,
    };
    doc.queue_signed_command(chat, &entry, |bytes| signer.and_then(|s| s.sign(bytes)))
        .unwrap();
    doc.read_commands_with_auth(chat).unwrap().remove(0).1
}

#[tokio::test(start_paused = true)]
async fn hosts_check_command_signatures_against_the_group() {
    let edge = Arc::new(FakeEdge::default());
    let (a, b) = paired_pair(&edge).await;
    a.trust.sync().await.unwrap();
    let outsider = device("dev-x", &edge);
    outsider.trust.sync().await.unwrap();
    assert_eq!(
        outsider.trust.sign(b"cypher-command-v1\0"),
        None,
        "unpaired"
    );

    let by_b = command("chat", "dev-b", Some(&b.trust));
    let unsigned = command("chat", "dev-b", None);
    let mut tampered = by_b.clone();
    tampered.signing_bytes.push(0);
    // B's signature for another chat.
    let mut elsewhere = command("chat-other", "dev-b", Some(&b.trust));
    elsewhere.signing_bytes = command("chat", "dev-b", None).signing_bytes;
    // B's signature, claimed for A.
    let mut as_a = command("chat", "dev-a", None);
    as_a.sig = by_b.sig.clone();
    let malformed = CommandAuth {
        sig: CommandSig::Malformed,
        ..unsigned.clone()
    };

    // While unpaired devices are allowed, unsigned commands still run; a
    // signature that doesn't match never does.
    assert_eq!(a.trust.check("dev-b", &by_b).await, Ok(()));
    assert_eq!(a.trust.check("dev-b", &unsigned).await, Ok(()));
    assert_eq!(a.trust.check("dev-x", &unsigned).await, Ok(()));
    for (issuer, auth) in [
        ("dev-b", &tampered),
        ("dev-b", &elsewhere),
        ("dev-a", &as_a),
        ("dev-b", &malformed),
    ] {
        assert_eq!(
            a.trust.check(issuer, auth).await,
            Err(COMMAND_TAMPERED.into())
        );
    }

    // "Only paired devices": a member's signature, or nothing runs.
    a.trust.set_require_paired(true).await.unwrap();
    assert_eq!(a.trust.check("dev-b", &by_b).await, Ok(()));
    let own = command("chat", "dev-a", Some(&a.trust));
    assert_eq!(a.trust.check("dev-a", &own).await, Ok(()));
    for (issuer, auth) in [
        ("dev-b", &unsigned),
        ("dev-a", &command("chat", "dev-a", None)),
        ("dev-x", &command("chat", "dev-x", None)),
    ] {
        assert_eq!(
            a.trust.check(issuer, auth).await,
            Err(COMMAND_UNSIGNED.into())
        );
    }
    // Only its own signature lets a device sign a copy again.
    assert!(a.trust.is_own_signature(&own));
    assert!(!a.trust.is_own_signature(&by_b));
    assert!(!a.trust.is_own_signature(&command("chat", "dev-a", None)));
    // A key the group doesn't know: re-sync, then refuse.
    let stranger = DeviceKeys::generate("dev-x");
    let mut foreign = command("chat", "dev-x", None);
    foreign.sig = CommandSig::Present(stranger.sign_command(&foreign.signing_bytes).unwrap());
    assert_eq!(
        a.trust.check("dev-x", &foreign).await,
        Err(COMMAND_UNSIGNED.into())
    );

    // A receive-only member's commands run only on itself.
    a.trust.set_receive_only("dev-b", true).await.unwrap();
    b.trust.sync().await.unwrap();
    assert_eq!(
        a.trust.check("dev-b", &by_b).await,
        Err(COMMAND_FROM_RECEIVE_ONLY.into())
    );
    assert_eq!(b.trust.check("dev-b", &by_b).await, Ok(()));
    assert_eq!(b.trust.check("dev-a", &own).await, Ok(()));
}
