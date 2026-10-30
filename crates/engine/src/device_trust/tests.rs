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
