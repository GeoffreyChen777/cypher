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
        Box::pin(async { true })
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
    assert_ne!(sent[0].started_at, sent[3].started_at);
}

#[tokio::test(start_paused = true)]
async fn failures_retry_terminal_events_and_invalidate_the_old_success_cache() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let succeeds = Arc::new(AtomicBool::new(true));
    let (record, ok) = (sent.clone(), succeeds.clone());
    let hook = reporter(Arc::new(move |session| {
        lock(&record).push(session);
        let result = ok.load(Ordering::Relaxed);
        Box::pin(async move { result })
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
            true
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
fn ignored_stale_and_invalid_responses_are_not_delivery_receipts() {
    for body in [
        serde_json::json!({"ok":true,"ignored":true}),
        serde_json::json!({"ok":true,"stale":true}),
        serde_json::json!({}),
        serde_json::json!({"ok":false}),
    ] {
        assert!(!accepted(&body));
    }
    assert!(accepted(&serde_json::json!({"ok":true})));
    assert!(accepted(&serde_json::json!({"ok":true,"duplicate":true})));
}

#[tokio::test(start_paused = true)]
async fn obsolete_events_do_not_send_or_poison_fresh_retries() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let hook = reporter(Arc::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { true })
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
