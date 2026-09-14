//! Best-effort, per-chat ordered notification delivery. Freshness-only session
//! updates still reach the registry/UI, but do not become duplicate HTTP events.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use cypher_proto::{Session, SessionStatus, SubagentRunMode, SubagentRunStatus};
use futures::future::BoxFuture;

use crate::{auth::Auth, workspace_host::NotificationEventHook};

#[derive(PartialEq)]
struct Signature {
    device: String,
    status: SessionStatus,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    children: Vec<(String, SubagentRunMode, SubagentRunStatus)>,
}

impl From<&Session> for Signature {
    fn from(session: &Session) -> Self {
        Self {
            device: session.device_id.clone(),
            status: session.status,
            started_at: session.started_at,
            // Same bound as the event endpoint. IDs distinguish replacement
            // runs; progress text and timestamp-only heartbeats are not events.
            children: session
                .subagents
                .iter()
                .take(32)
                .map(|run| (run.run_id.clone(), run.mode, run.status))
                .collect(),
        }
    }
}

#[derive(Default)]
struct Delivery {
    delivered: Option<Signature>,
    pending: VecDeque<Session>,
    running: bool,
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

type Report = Arc<dyn Fn(Session) -> BoxFuture<'static, bool> + Send + Sync>;

pub(crate) fn hook(auth: Auth, user: String, org: String) -> NotificationEventHook {
    reporter(Arc::new(move |session| {
        let (auth, user, org) = (auth.clone(), user.clone(), org.clone());
        Box::pin(async move {
            match auth.report_notification_event(&user, &org, &session).await {
                Ok(value) => accepted(&value),
                Err(err) => {
                    tracing::debug!(error = %err, "notification event unavailable");
                    false
                }
            }
        })
    }))
}

fn accepted(value: &serde_json::Value) -> bool {
    // A successful HTTP request can still precede the chat's registry row.
    // Neither an ignored nor an obsolete event earns a delivered-cache entry.
    value["ok"] == true && value["ignored"] != true && value["stale"] != true
}

fn reporter(report: Report) -> NotificationEventHook {
    let chats = Mutex::new(HashMap::<String, Arc<Mutex<Delivery>>>::new());
    Arc::new(move |session| {
        let delivery = lock(&chats)
            .entry(session.chat_id.clone())
            .or_default()
            .clone();
        let mut state = lock(&delivery);
        let signature = Signature::from(session);
        if !state.running && state.delivered.as_ref() == Some(&signature) {
            return;
        }
        if state
            .pending
            .back()
            .is_some_and(|last| Signature::from(last) == signature)
        {
            // Coalesce queued heartbeats, not transitions (working → idle →
            // working must remain three events even while a request is slow).
            *state.pending.back_mut().unwrap() = session.clone();
        } else {
            state.pending.push_back(session.clone());
        }
        if state.running {
            return;
        }
        state.running = true;
        drop(state);
        let report = report.clone();
        tokio::spawn(async move {
            loop {
                let session = {
                    let mut state = lock(&delivery);
                    let Some(session) = state.pending.pop_front() else {
                        state.running = false;
                        return;
                    };
                    if state.delivered.as_ref() == Some(&Signature::from(&session)) {
                        continue;
                    }
                    // A lost response may still have changed the server. Do
                    // not reuse an older successful signature after a failure.
                    state.delivered = None;
                    session
                };
                // Bounded retries also cover terminal events, which have no
                // future heartbeat. A failed request is NEVER marked sent;
                // later heartbeats may retry after this small budget expires.
                for attempt in 0..3 {
                    // The existing endpoint rejects events over five minutes
                    // old. Drain obsolete queued/restored state without HTTP.
                    if chrono::Utc::now().signed_duration_since(session.updated_at)
                        > chrono::Duration::minutes(5)
                    {
                        break;
                    }
                    if report(session.clone()).await {
                        lock(&delivery).delivered = Some(Signature::from(&session));
                        break;
                    }
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_secs(5 << attempt)).await;
                    }
                }
            }
        });
    })
}

#[cfg(test)]
mod tests;
