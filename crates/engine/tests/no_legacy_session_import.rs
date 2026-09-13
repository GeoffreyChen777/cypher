//! Fresh v3 does not import retired Loro/chat2 snapshots or their command
//! ledgers. Native v3 crash recovery is tested separately in e2e.rs.
use cypher_engine::{EngineCore, HarnessRegistry};
use cypher_proto::HarnessId;
use std::sync::Arc;

#[tokio::test]
async fn legacy_snapshots_do_not_seed_a_v3_session() {
    let dir = tempfile::tempdir().unwrap();
    let account = dir.path().join("orgs/dev-org/dev-user");
    let old = cypher_sync::DocsStore::open(&account).unwrap();
    old.save_snapshot("chat", b"retired Loro data").unwrap();
    old.save_snapshot("chat.pre-chat2", b"retired rollback data")
        .unwrap();
    let core = EngineCore::assemble(
        dir.path(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let handle = core.doc_host.open("chat").unwrap();
    assert!(handle.read_entries().unwrap().is_empty());
    assert!(handle.read_commands().unwrap().is_empty());
    assert!(handle.replica().is_some());
    assert_eq!(handle.replica().unwrap().read(|j| j.cursor()).unwrap(), 0);
    core.shutdown().await;
}
