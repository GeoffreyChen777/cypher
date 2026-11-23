//! `WatchPlanBody` (docs/design/plan-body.md §5): the desktop work page's
//! way to a long plan's full text, `planBodies[workId]` in the workspace
//! chat's doc, as `{plan?, result?}` now and on every change to it.
//! The forwarded case (another device answers) is in
//! `sync::device_routing`.

use crate::support;

use std::collections::BTreeMap;
use std::time::Duration;

use cypher_proto::{PlanBody, PlanResult, WorkBodies};
use cypher_rpc::methods;

const CHAT: &str = "ws-chat";

fn body(detail: &str) -> PlanBody {
    PlanBody::new(
        "Rewrite the billing exports".into(),
        BTreeMap::from([("1".to_string(), detail.to_string())]),
        1_000,
    )
}

async fn next(rx: &mut tokio::sync::mpsc::Receiver<serde_json::Value>) -> WorkBodies {
    let item = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a frame in time")
        .expect("the stream is open");
    serde_json::from_value(item).expect("a {plan?, result?} frame")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_watch_sends_the_entry_now_and_each_time_it_changes() {
    let dir = tempfile::tempdir().unwrap();
    let core = support::assemble_mock(dir.path());
    let handle = core.doc_host.open(CHAT).unwrap();
    let client = cypher_rpc::memory_client(core.rpc_service());
    let params = serde_json::json!({ "chatId": CHAT, "workId": "w1" });

    // Nothing written yet: an empty entry, at once.
    let mut rx = client
        .subscribe(methods::WATCH_PLAN_BODY, params.clone())
        .await
        .expect("WatchPlanBody");
    assert_eq!(next(&mut rx).await, WorkBodies::default());

    // The host writes the plan's body: it arrives whole, with its rev.
    let plan = body(&"设".repeat(9_000));
    handle.doc().put_plan_body("w1", &plan).unwrap();
    let frame = next(&mut rx).await;
    assert_eq!(frame.plan.as_ref(), Some(&plan));
    assert_eq!(frame.result, None);

    // What doesn't change the entry sends nothing: the transcript, and
    // another work item's body. The next frame is the result's.
    handle
        .write_user_message("m-1", "Something else", 2_000)
        .unwrap();
    handle
        .doc()
        .put_plan_body("w2", &body("another plan"))
        .unwrap();
    let result = PlanResult::new(format!("Done. {}", "结".repeat(5_000)), 3_000);
    handle.doc().put_plan_result("w1", &result).unwrap();
    let frame = next(&mut rx).await;
    assert_eq!(frame.plan.as_ref(), Some(&plan), "the plan stays beside it");
    assert_eq!(frame.result.as_ref(), Some(&result));

    // A second page opening now gets the whole entry first.
    let mut late = client
        .subscribe(methods::WATCH_PLAN_BODY, params.clone())
        .await
        .expect("a second watch");
    assert_eq!(next(&mut late).await, frame);
    drop(late);

    // Pruned: the entry is gone.
    assert!(handle.doc().delete_plan_body("w1").unwrap());
    assert_eq!(next(&mut rx).await, WorkBodies::default());

    // Bad params are refused, not watched: the stream ends at once.
    let mut refused = client
        .subscribe(
            methods::WATCH_PLAN_BODY,
            serde_json::json!({ "chatId": CHAT }),
        )
        .await
        .expect("the call itself");
    let ended = tokio::time::timeout(Duration::from_secs(5), refused.recv())
        .await
        .expect("an answer in time");
    assert_eq!(ended, None, "no frame without a workId");
    drop(rx);
    core.shutdown().await;
}

/// A body that lands in the doc by sync (an import, as from the chat's
/// room) wakes the watch like a local write.
#[tokio::test(flavor = "multi_thread")]
async fn a_body_that_arrives_by_sync_reaches_the_watch() {
    let dir = tempfile::tempdir().unwrap();
    let core = support::assemble_mock(dir.path());
    let handle = core.doc_host.open(CHAT).unwrap();
    let client = cypher_rpc::memory_client(core.rpc_service());
    let mut rx = client
        .subscribe(
            methods::WATCH_PLAN_BODY,
            serde_json::json!({ "chatId": CHAT, "workId": "w1" }),
        )
        .await
        .expect("WatchPlanBody");
    assert_eq!(next(&mut rx).await, WorkBodies::default());

    // The host's copy of the same chat writes the body; this device
    // imports the update.
    let host = cypher_doc::SessionDoc::init(CHAT).unwrap();
    let local = handle.doc().doc().oplog_vv();
    host.doc()
        .import(
            &handle
                .doc()
                .doc()
                .export(loro::ExportMode::updates(&Default::default()))
                .unwrap(),
        )
        .unwrap();
    let plan = body("from the host");
    host.put_plan_body("w1", &plan).unwrap();
    let update = host
        .doc()
        .export(loro::ExportMode::updates(&local))
        .unwrap();
    handle.doc().doc().import(&update).unwrap();
    assert_eq!(next(&mut rx).await.plan, Some(plan));
    drop(rx);
    core.shutdown().await;
}
