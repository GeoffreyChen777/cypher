//! Regression coverage for diff-sync reconcile churn.
//!
//! Workspace row writes can themselves wake reconcile. A reconcile pass must
//! therefore reuse its checkout identity and keep an entry alive across a
//! transient row/watch flap; otherwise an idle checkout can repeatedly tear
//! down and recreate its entry, spawning expensive diff captures forever.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cypher_engine::{CheckoutDiffSync, EngineCore, HarnessRegistry};
use cypher_proto::HarnessId;

/// How long a recapture would take to show: diff-sync's 500 ms watch
/// debounce plus the git capture after it, with margin for a loaded machine.
/// Nothing positive marks "no recapture happened", so the churn checks hold
/// this negative window after their writes have landed.
const NO_RECAPTURE_WINDOW: Duration = Duration::from_millis(1500);

async fn init_dirty_repo(dir: &Path) {
    common::init_repo(dir, "one\ntwo\n").await;
    std::fs::write(dir.join("a.txt"), "one\ntwo\nedited\n").expect("dirty tree");
}

fn assemble(dir: &Path) -> EngineCore {
    std::fs::create_dir_all(dir).expect("data dir");
    EngineCore::assemble(dir, Arc::new(HarnessRegistry::new()), HarnessId::Mock, None)
        .expect("engine assembles")
}

async fn wait_for_diff(sync: &CheckoutDiffSync) -> cypher_proto::CheckoutDiff {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(diff) = sync.watch_diffs().borrow().first().cloned() {
            return diff;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "diff was not published"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_chat(core: &EngineCore, chat_id: &str, present: bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let found = core
            .workspace
            .watch_chats()
            .borrow()
            .iter()
            .any(|chat| chat.id == chat_id);
        if found == present {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "chat watch did not settle"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row_write_reconcile_does_not_recapture_an_idle_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_dirty_repo(&repo).await;
    let core = assemble(&tmp.path().join("data"));

    core.workspace
        .create_space(
            "space",
            &core.device_id,
            &repo.to_string_lossy(),
            None,
            true,
        )
        .expect("space");
    core.workspace
        .create_chat("chat", Some("space"), None, None, None)
        .expect("chat");
    core.diff_sync.reconcile_now().await;
    let before = wait_for_diff(&core.diff_sync).await;

    // These are the same kind of workspace writes made by sync_entry itself.
    // They wake the chat watcher, but must not replace the entry or kick a
    // second capture with a new checksum/publish timestamp.
    core.workspace
        .set_chat_branch("chat", "main")
        .expect("branch write");
    core.workspace
        .set_chat_checkout("chat", &before.checkout_id)
        .expect("checkout write");
    common::wait_for(
        || {
            core.workspace.watch_chats().borrow().iter().any(|chat| {
                chat.id == "chat"
                    && chat.branch.as_deref() == Some("main")
                    && chat.checkout_id.as_deref() == Some(before.checkout_id.as_str())
            })
        },
        "the row writes to reach the chat watch",
    )
    .await;
    core.diff_sync.reconcile_now().await;
    tokio::time::sleep(NO_RECAPTURE_WINDOW).await;

    let after = core
        .diff_sync
        .watch_diffs()
        .borrow()
        .first()
        .cloned()
        .expect("diff retained");
    assert_eq!(after.checkout_id, before.checkout_id);
    assert_eq!(after.checksum, before.checksum);
    assert_eq!(
        after.updated_at, before.updated_at,
        "workspace row churn must not recapture an unchanged checkout"
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_flap_keeps_entry_until_absence_is_sustained() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    init_dirty_repo(&repo).await;
    let core = assemble(&tmp.path().join("data"));
    let sync = CheckoutDiffSync::start_with_orphan_grace(
        core.repos.clone(),
        core.workspace.clone(),
        &core.device_id,
        Duration::from_millis(250),
    );

    core.workspace
        .create_space(
            "space",
            &core.device_id,
            &repo.to_string_lossy(),
            None,
            true,
        )
        .expect("space");
    core.workspace
        .create_chat("chat", Some("space"), None, None, None)
        .expect("chat");
    wait_for_chat(&core, "chat", true).await;
    sync.reconcile_now().await;
    let before = wait_for_diff(&sync).await;

    core.workspace.delete_chat("chat").expect("delete chat");
    wait_for_chat(&core, "chat", false).await;
    sync.reconcile_now().await;
    assert_eq!(
        sync.watch_diffs().borrow().len(),
        1,
        "one missing pass only marks the entry orphaned"
    );

    core.workspace
        .create_chat("chat", Some("space"), None, None, None)
        .expect("chat restored");
    wait_for_chat(&core, "chat", true).await;
    sync.reconcile_now().await;
    tokio::time::sleep(NO_RECAPTURE_WINDOW).await;
    let after = sync
        .watch_diffs()
        .borrow()
        .first()
        .cloned()
        .expect("diff retained");
    assert_eq!(after.checkout_id, before.checkout_id);
    assert_eq!(
        after.updated_at, before.updated_at,
        "flapping back must not recapture the checkout"
    );

    core.shutdown().await;
    sync.shutdown().await;
}
