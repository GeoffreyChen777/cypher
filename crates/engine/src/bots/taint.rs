//! The taint rule (docs/design/agent-bots.md §4.3): a thread that received
//! another user's bot's answer is tainted for the rest of its run, so the
//! actions it could otherwise take silently need the owner's approval.
//!
//! The set lives in memory, keyed by thread chat id: a run ends the taint
//! (the work host clears it when the thread's run settles), and an engine
//! restart ends every run.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::util::lock;

#[derive(Default)]
pub(super) struct Taint {
    /// Thread chat id → (why, e.g. "Alice's Auth bot answered in this
    /// thread"; when it was marked, epoch ms).
    threads: Mutex<HashMap<String, (String, i64)>>,
}

impl Taint {
    pub(super) fn mark(&self, chat_id: &str, reason: impl Into<String>) {
        lock(&self.threads).insert(chat_id.to_string(), (reason.into(), cypher_proto::now_ms()));
    }

    /// Why `chat_id` is tainted, when it is.
    pub(super) fn reason(&self, chat_id: &str) -> Option<String> {
        lock(&self.threads)
            .get(chat_id)
            .map(|(reason, _)| reason.clone())
    }

    /// Clear every thread whose run settled after it was marked;
    /// `settled_at(chat id)` is when its run last settled (epoch ms), `None`
    /// while it runs.
    pub(super) fn clear_settled(&self, settled_at: impl Fn(&str) -> Option<i64>) {
        lock(&self.threads).retain(|chat_id, (_, marked_at)| {
            settled_at(chat_id).is_none_or(|settled| settled < *marked_at)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taint_reads_back_its_reason() {
        let taint = Taint::default();
        assert!(taint.reason("t").is_none());
        taint.mark("t", "a peer answered");
        assert_eq!(taint.reason("t").as_deref(), Some("a peer answered"));
    }

    #[test]
    fn taint_ends_when_the_run_settles_after_it() {
        let taint = Taint::default();
        taint.mark("running", "a peer answered");
        taint.mark("settled-before", "a peer answered");
        taint.mark("settled-after", "a peer answered");
        let now = cypher_proto::now_ms();
        taint.clear_settled(|chat| match chat {
            "settled-before" => Some(now - 60_000),
            "settled-after" => Some(now + 1_000),
            _ => None,
        });
        assert!(taint.reason("running").is_some());
        assert!(
            taint.reason("settled-before").is_some(),
            "an earlier run's settle doesn't clear a later taint"
        );
        assert!(taint.reason("settled-after").is_none());
    }
}
