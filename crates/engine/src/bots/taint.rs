//! The taint rule (docs/design/agent-bots.md §4.3): a thread that received
//! another user's bot's answer is tainted for the rest of its run, so the
//! actions it could otherwise take silently need the owner's approval.
//!
//! The set lives in memory, keyed by thread chat id: a run ends the taint
//! (it is cleared when the run settles), and an engine restart ends every
//! run.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::util::lock;

// Read by the work and network tracks once they land.
#[allow(dead_code)]
#[derive(Default)]
pub(super) struct Taint {
    /// Thread chat id → why (e.g. "Alice's Auth bot answered in this thread").
    threads: Mutex<HashMap<String, String>>,
}

#[allow(dead_code)]
impl Taint {
    pub(super) fn mark(&self, chat_id: &str, reason: impl Into<String>) {
        lock(&self.threads).insert(chat_id.to_string(), reason.into());
    }

    /// Why `chat_id` is tainted, when it is.
    pub(super) fn reason(&self, chat_id: &str) -> Option<String> {
        lock(&self.threads).get(chat_id).cloned()
    }

    pub(super) fn clear(&self, chat_id: &str) {
        lock(&self.threads).remove(chat_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taint_lasts_until_cleared() {
        let taint = Taint::default();
        assert!(taint.reason("t").is_none());
        taint.mark("t", "a peer answered");
        assert_eq!(taint.reason("t").as_deref(), Some("a peer answered"));
        taint.clear("t");
        assert!(taint.reason("t").is_none());
    }
}
