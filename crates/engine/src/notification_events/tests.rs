use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

fn session() -> Session {
    let mut session: Session = serde_json::from_value(serde_json::json!({
        "chatId": "chat", "deviceId": "device", "status": "working",
        "startedAt": "2026-09-13T00:00:00Z", "updatedAt": "2026-09-13T00:00:01Z",
        "subagents": [{"runId":"child", "agent":"helper", "task":"test",
            "mode":"async", "status":"running", "startedAt": 1, "updatedAt": 2}]
    }))
    .unwrap();
    session.updated_at = chrono::Utc::now();
    session.started_at = Some(session.updated_at - chrono::Duration::seconds(1));
    session
}

async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn heartbeats_are_free_but_transitions_and_child_changes_are_preserved() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let record = sent.clone();
    let hook = reporter(Arc::new(move |session| {
        lock(&record).push(session);
        Box::pin(async { Reply::Delivered })
    }));
    let mut current = session();
    hook(&current);
    settle().await;
    for i in 0..100 {
        current.updated_at += chrono::Duration::seconds(10);
        current.subagents[0].updated_at += 10_000;
        current.subagents[0].progress = Some(format!("heartbeat {i}"));
        hook(&current);
    }
    settle().await;
    assert_eq!(lock(&sent).len(), 1);
    // Queued transitions must not be collapsed to the last snapshot.
    current.status = SessionStatus::Idle;
    hook(&current);
    current.subagents[0].status = SubagentRunStatus::Error;
    hook(&current);
    current.status = SessionStatus::Working;
    current.started_at = Some(current.updated_at);
    hook(&current);
    current.subagents[0].run_id = "replacement".into();
    hook(&current);
    settle().await;
    let sent = lock(&sent);
    assert_eq!(sent.len(), 5);
    assert_eq!(sent[1].status, SessionStatus::Idle);
    assert_eq!(sent[1].subagents[0].status, SubagentRunStatus::Running);
    assert_eq!(sent[2].subagents[0].status, SubagentRunStatus::Error);
    let first_started = sent[0].started_at;
    let fourth_started = sent[3].started_at;
    assert_ne!(first_started, fourth_started);
}

#[tokio::test(start_paused = true)]
async fn failures_retry_terminal_events_and_invalidate_the_old_success_cache() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let succeeds = Arc::new(AtomicBool::new(true));
    let (record, ok) = (sent.clone(), succeeds.clone());
    let hook = reporter(Arc::new(move |session| {
        lock(&record).push(session);
        let result = ok.load(Ordering::Relaxed);
        Box::pin(async move {
            if result {
                Reply::Delivered
            } else {
                Reply::Failed
            }
        })
    }));
    let working = session();
    hook(&working);
    settle().await;
    succeeds.store(false, Ordering::Relaxed);
    let mut idle = working.clone();
    idle.status = SessionStatus::Idle;
    hook(&idle);
    settle().await;
    assert_eq!(lock(&sent).len(), 2);
    for delay in [5, 10] {
        tokio::time::advance(Duration::from_secs(delay)).await;
        settle().await;
    }
    assert_eq!(
        lock(&sent).len(),
        4,
        "terminal event gets bounded retries without a heartbeat"
    );
    // The failed idle response might have reached the server. Returning to
    // the old working signature must send again, not reuse its stale cache.
    succeeds.store(true, Ordering::Relaxed);
    hook(&working);
    settle().await;
    assert_eq!(lock(&sent).len(), 5);
    hook(&working);
    settle().await;
    assert_eq!(lock(&sent).len(), 5);
}

#[tokio::test(start_paused = true)]
async fn in_flight_heartbeats_coalesce_without_blocking_other_chats() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(tokio::sync::Notify::new());
    let (record, release) = (sent.clone(), gate.clone());
    let hook = reporter(Arc::new(move |session| {
        let blocked = session.chat_id == "chat" && session.status == SessionStatus::Working;
        lock(&record).push(session);
        let release = release.clone();
        Box::pin(async move {
            if blocked {
                release.notified().await;
            }
            Reply::Delivered
        })
    }));
    let mut current = session();
    hook(&current);
    settle().await;
    for _ in 0..100 {
        hook(&current);
    }
    current.status = SessionStatus::Idle;
    hook(&current);
    let mut other = current.clone();
    other.chat_id = "other".into();
    hook(&other);
    settle().await;
    assert_eq!(lock(&sent).len(), 2);
    gate.notify_one();
    settle().await;
    let sent = lock(&sent);
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[2].status, SessionStatus::Idle);
}

#[test]
fn only_a_real_receipt_is_delivered_and_ignored_is_its_own_answer() {
    for body in [
        serde_json::json!({"ok":true,"stale":true}),
        serde_json::json!({}),
        serde_json::json!({"ok":false}),
        serde_json::json!({"ok":false,"ignored":true}),
    ] {
        assert_eq!(classify(&body), Reply::Failed, "{body}");
    }
    assert_eq!(
        classify(&serde_json::json!({"ok":true,"ignored":true})),
        Reply::Ignored
    );
    assert_eq!(
        classify(&serde_json::json!({"ok":true,"ignored":true,"reason":"archived"})),
        Reply::Ignored
    );
    assert_eq!(classify(&serde_json::json!({"ok":true})), Reply::Delivered);
    assert_eq!(
        classify(&serde_json::json!({"ok":true,"duplicate":true})),
        Reply::Delivered
    );
}

#[tokio::test(start_paused = true)]
async fn obsolete_events_do_not_send_or_poison_fresh_retries() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let hook = reporter(Arc::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Reply::Delivered })
    }));
    let mut current = session();
    current.updated_at -= chrono::Duration::minutes(6);
    hook(&current);
    settle().await;
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    current.updated_at = chrono::Utc::now();
    hook(&current);
    settle().await;
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

/// A running chat heartbeats every 15s. Drive `seconds` of that against a
/// reporter whose Worker answer is `answer()` at the time of each report.
async fn heartbeat_for(hook: &NotificationEventHook, current: &mut Session, seconds: u64) {
    for _ in 0..seconds / 15 {
        current.updated_at = chrono::Utc::now();
        hook(current);
        settle().await;
        tokio::time::advance(Duration::from_secs(15)).await;
        settle().await;
    }
}

fn scripted(sent: Arc<Mutex<Vec<Session>>>, answer: Arc<Mutex<Reply>>) -> NotificationEventHook {
    reporter(Arc::new(move |session| {
        lock(&sent).push(session);
        let reply = *lock(&answer);
        Box::pin(async move { reply })
    }))
}

#[tokio::test(start_paused = true)]
async fn a_chat_the_worker_ignores_is_not_re_reported_every_heartbeat() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let answer = Arc::new(Mutex::new(Reply::Ignored));
    let hook = scripted(sent.clone(), answer);
    let mut current = session();
    current.subagents.clear();
    heartbeat_for(&hook, &mut current, 3600).await;
    let first_hour = lock(&sent).len();
    // Before: every heartbeat plus its in-loop retries, ~490 an hour.
    assert!(first_hour <= 12, "first hour sent {first_hour}");
    heartbeat_for(&hook, &mut current, 3600).await;
    let second_hour = lock(&sent).len() - first_hour;
    // At the 600s cap: six an hour, whatever the heartbeat does.
    assert!(
        (5..=7).contains(&second_hour),
        "second hour sent {second_hour}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_registry_row_that_lands_late_still_gets_its_terminal_event() {
    // A terminal event has no later heartbeat to retry it, so the in-loop
    // retries of a FIRST ignore must survive: the row may be milliseconds
    // behind the event.
    let sent = Arc::new(Mutex::new(Vec::new()));
    let answer = Arc::new(Mutex::new(Reply::Ignored));
    let hook = scripted(sent.clone(), answer.clone());
    let mut idle = session();
    idle.status = SessionStatus::Idle;
    idle.subagents.clear();
    hook(&idle);
    settle().await;
    assert_eq!(lock(&sent).len(), 1);
    *lock(&answer) = Reply::Delivered; // the row arrived
    tokio::time::advance(Duration::from_secs(5)).await;
    settle().await;
    assert_eq!(
        lock(&sent).len(),
        2,
        "retried inside the loop, no heartbeat needed"
    );
    tokio::time::advance(Duration::from_secs(60)).await;
    settle().await;
    hook(&idle);
    settle().await;
    assert_eq!(lock(&sent).len(), 2, "delivered, so nothing more to say");
}

#[tokio::test(start_paused = true)]
async fn an_unarchived_running_chat_is_reported_again_within_the_cap() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let answer = Arc::new(Mutex::new(Reply::Ignored));
    let hook = scripted(sent.clone(), answer.clone());
    let mut current = session();
    current.subagents.clear();
    heartbeat_for(&hook, &mut current, 3600).await; // settled at the cap
    *lock(&answer) = Reply::Delivered; // user unarchived it
    let before = lock(&sent).len();
    heartbeat_for(&hook, &mut current, 615).await;
    assert!(
        lock(&sent).len() > before,
        "must be re-reported within one cap interval"
    );
    let delivered_at = lock(&sent).len();
    heartbeat_for(&hook, &mut current, 900).await;
    assert_eq!(
        lock(&sent).len(),
        delivered_at,
        "then heartbeats are free again"
    );
}

#[tokio::test(start_paused = true)]
async fn a_transition_is_never_held_back_by_an_ignored_signature() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let answer = Arc::new(Mutex::new(Reply::Ignored));
    let hook = scripted(sent.clone(), answer.clone());
    let mut current = session();
    current.subagents.clear();
    heartbeat_for(&hook, &mut current, 600).await; // working, backing off
    let before = lock(&sent).len();
    *lock(&answer) = Reply::Delivered;
    current.status = SessionStatus::Idle;
    current.updated_at = chrono::Utc::now();
    hook(&current);
    settle().await;
    assert_eq!(
        lock(&sent).len(),
        before + 1,
        "a new signature goes out at once"
    );
    assert_eq!(lock(&sent).last().unwrap().status, SessionStatus::Idle);
}
