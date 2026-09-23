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

/// What the Worker made of one event report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reply {
    /// Recorded, or a duplicate of what is recorded: a delivery receipt.
    Delivered,
    /// The Worker declined to act on this chat (`ignored`): no registry row
    /// yet, archived, hosted on another device, or its space is gone.
    Ignored,
    /// Anything else: transport failure, rejection, an obsolete or malformed
    /// reply. Not evidence about the chat either way.
    Failed,
}

/// First pause before re-reporting a signature the Worker ignored, and the
/// ceiling that pause doubles to.
///
/// An ignored report used to be retried like a network failure: three quick
/// attempts, then again on every 15s heartbeat for as long as the chat ran.
/// For a chat the Worker will never notify for that was an unbounded loop --
/// one host was measured at 245 identical reports in 30 minutes, the single
/// largest line on the Durable Object bill. Most of those conditions cannot
/// change on their own, but two can: a brand-new chat's registry row may land
/// just after its first event, and a user may unarchive a running chat. So an
/// ignored signature backs off instead of settling: the in-loop retries still
/// cover the row race, and the cap bounds how late an unarchive is noticed.
const IGNORED_RETRY_BASE: Duration = Duration::from_secs(30);
const IGNORED_RETRY_CAP: Duration = Duration::from_secs(600);

struct Ignored {
    signature: Signature,
    delay: Duration,
    retry_at: tokio::time::Instant,
}

#[derive(Default)]
struct Delivery {
    delivered: Option<Signature>,
    ignored: Option<Ignored>,
    pending: VecDeque<Session>,
    running: bool,
}

impl Delivery {
    /// Nothing new to say: already delivered, or ignored and still backing off.
    fn settled(&self, signature: &Signature, now: tokio::time::Instant) -> bool {
        self.delivered.as_ref() == Some(signature)
            || self
                .ignored
                .as_ref()
                .is_some_and(|ignored| &ignored.signature == signature && now < ignored.retry_at)
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

type Report = Arc<dyn Fn(Session) -> BoxFuture<'static, Reply> + Send + Sync>;

pub(crate) fn hook(auth: Auth, user: String, org: String) -> NotificationEventHook {
    reporter(Arc::new(move |session| {
        let (auth, user, org) = (auth.clone(), user.clone(), org.clone());
        Box::pin(async move {
            match auth.report_notification_event(&user, &org, &session).await {
                Ok(value) => {
                    let reply = classify(&value);
                    if reply == Reply::Ignored {
                        // Bounded by the backoff, so this is a handful of
                        // lines an hour per chat, and it is the only place a
                        // host can see WHY a chat is not notifying.
                        tracing::info!(
                            chat = %session.chat_id,
                            reason = value["reason"].as_str().unwrap_or("unspecified"),
                            "notification event ignored by the edge; backing off"
                        );
                    }
                    reply
                }
                Err(err) => {
                    tracing::debug!(error = %err, "notification event unavailable");
                    Reply::Failed
                }
            }
        })
    }))
}

fn classify(value: &serde_json::Value) -> Reply {
    if value["ok"] != true {
        return Reply::Failed;
    }
    if value["ignored"] == true {
        return Reply::Ignored;
    }
    // An obsolete report is not a receipt. It needs no backoff of its own:
    // the next heartbeat carries a fresh `updatedAt` and lands as a duplicate.
    if value["stale"] == true {
        return Reply::Failed;
    }
    Reply::Delivered
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
        if !state.running && state.settled(&signature, tokio::time::Instant::now()) {
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
                    if state.settled(&Signature::from(&session), tokio::time::Instant::now()) {
                        continue;
                    }
                    // A lost response may still have changed the server. Do
                    // not reuse an older successful signature after a failure.
                    state.delivered = None;
                    session
                };
                // Only the first ignore of a signature earns the quick in-loop
                // retries below: that is the window in which the chat's
                // registry row can still be in flight. Later reports of the
                // same signature are the backoff's, one attempt each.
                let first_ignore = !lock(&delivery)
                    .ignored
                    .as_ref()
                    .is_some_and(|ignored| ignored.signature == Signature::from(&session));
                let mut last = Reply::Failed;
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
                    last = report(session.clone()).await;
                    match last {
                        Reply::Delivered => {
                            let mut state = lock(&delivery);
                            state.delivered = Some(Signature::from(&session));
                            state.ignored = None;
                            break;
                        }
                        Reply::Ignored if !first_ignore => break,
                        Reply::Ignored | Reply::Failed => {}
                    }
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_secs(5 << attempt)).await;
                    }
                }
                if last == Reply::Ignored {
                    let mut state = lock(&delivery);
                    let signature = Signature::from(&session);
                    let delay = state
                        .ignored
                        .as_ref()
                        .filter(|ignored| ignored.signature == signature)
                        .map_or(IGNORED_RETRY_BASE, |ignored| {
                            (ignored.delay * 2).min(IGNORED_RETRY_CAP)
                        });
                    state.ignored = Some(Ignored {
                        signature,
                        delay,
                        retry_at: tokio::time::Instant::now() + delay,
                    });
                }
            }
        });
    })
}

#[cfg(test)]
mod tests;
