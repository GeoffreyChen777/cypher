//! The viewport's activity report, shared between the RPC handler that
//! receives it and the registry presence beat that carries it.
//!
//! Why this exists: the viewport reports which chat the user is looking at so
//! the edge can suppress a push for a chat that is already on screen. That
//! report is a heartbeat — every 15s while a chat is in the foreground — and
//! sending it over HTTP cost one billable Durable Object request each time.
//! Measured on production that was 377 requests/hour, 56% of all billable DO
//! requests, and by far the largest single line on the bill.
//!
//! A presence beat already goes to the *same* Durable Object on the *same*
//! cadence over an open socket, where inbound messages bill 20:1. Riding that
//! frame makes the refresh effectively free.
//!
//! Transitions keep their own HTTP request: they are rare, and only they need
//! the reply carrying `readEventIds` and the badge count.

use std::sync::{Arc, Mutex};

/// A slot holding the latest activity report until the next presence beat.
///
/// Cloneable and shared: `Auth` hands it to whichever RPC service is live, and
/// `EdgeConfig` hands the same slot to the workspace host that beats.
#[derive(Clone, Default)]
pub struct ViewportActivity {
    pending: Arc<Mutex<Option<serde_json::Value>>>,
}

impl ViewportActivity {
    /// Store the latest report, returning `true` when it is a *transition* —
    /// the user moved to a different chat, or came to / left the foreground —
    /// as opposed to a periodic refresh of a state already reported.
    ///
    /// Only a transition needs an HTTP request of its own.
    pub fn record(&self, activity: serde_json::Value) -> bool {
        let field = |v: &serde_json::Value, k: &str| v.get(k).cloned().unwrap_or_default();
        let mut slot = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let transition = slot.as_ref().is_none_or(|prev| {
            field(prev, "foreground") != field(&activity, "foreground")
                || field(prev, "chatId") != field(&activity, "chatId")
        });
        *slot = Some(activity);
        transition
    }

    /// The report to carry on the next presence beat, if any.
    ///
    /// A stale value is correct, not a leak: once the viewport stops reporting
    /// (backgrounded, or off a chat) the cached `sequence` stops advancing, the
    /// edge treats the repeat as a duplicate and changes nothing, and the
    /// target lease lapses on its own.
    pub fn pending(&self) -> Option<serde_json::Value> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl std::fmt::Debug for ViewportActivity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewportActivity").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(seq: u64, foreground: bool, chat: Option<&str>) -> serde_json::Value {
        json!({
            "clientId": "c", "sequence": seq, "platform": "desktop",
            "foreground": foreground, "interactionAgeMs": 0, "chatId": chat,
        })
    }

    #[test]
    fn first_report_is_a_transition() {
        let slot = ViewportActivity::default();
        assert!(slot.record(report(1, true, Some("chat-a"))));
    }

    #[test]
    fn refreshing_the_same_state_is_not_a_transition() {
        let slot = ViewportActivity::default();
        slot.record(report(1, true, Some("chat-a")));
        // Same chat, same foreground, later sequence: a heartbeat. It must not
        // spend an HTTP request — this is the 377/hour we are removing.
        assert!(!slot.record(report(2, true, Some("chat-a"))));
        assert!(!slot.record(report(3, true, Some("chat-a"))));
    }

    #[test]
    fn switching_chat_or_leaving_the_foreground_is_a_transition() {
        let slot = ViewportActivity::default();
        slot.record(report(1, true, Some("chat-a")));
        assert!(slot.record(report(2, true, Some("chat-b"))));
        assert!(slot.record(report(3, false, Some("chat-b"))));
        assert!(slot.record(report(4, true, Some("chat-b"))));
        // Leaving a chat entirely is also a transition.
        assert!(slot.record(report(5, true, None)));
    }

    #[test]
    fn the_beat_carries_the_latest_report() {
        let slot = ViewportActivity::default();
        assert!(slot.pending().is_none());
        slot.record(report(1, true, Some("chat-a")));
        slot.record(report(2, true, Some("chat-a")));
        let pending = slot.pending().expect("a report is waiting");
        assert_eq!(pending["sequence"], 2);
        assert_eq!(pending["chatId"], "chat-a");
    }

    #[test]
    fn the_slot_is_shared_by_clone() {
        let a = ViewportActivity::default();
        let b = a.clone();
        a.record(report(7, true, Some("chat-z")));
        assert_eq!(b.pending().expect("shared")["sequence"], 7);
    }
}
