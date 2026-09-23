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
//! A transition -- another chat, or entering/leaving the foreground -- cannot
//! wait for the next beat, because the Worker's push suppression reads it. It
//! does not need a reply either (`readEventIds` is iOS-only, and the desktop UI
//! discards the badge), so it goes out at once as an extra presence beat on the
//! same socket. Only a client with no live socket spends an HTTP request.

use std::sync::{Arc, Mutex};

/// A slot holding the latest activity report until the next presence beat.
///
/// Cloneable and shared: `Auth` hands it to whichever RPC service is live, and
/// `EdgeConfig` hands the same slot to the workspace host that beats.
#[derive(Clone, Default)]
pub struct ViewportActivity {
    pending: Arc<Mutex<Option<serde_json::Value>>>,
    /// Set by the workspace host: send the pending report on the registry
    /// socket now, returning whether a live socket carried it.
    immediate: Arc<Mutex<Option<ImmediateBeat>>>,
}

type ImmediateBeat = Arc<dyn Fn() -> bool + Send + Sync>;

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

    /// Register how to put the pending report on the wire immediately.
    pub fn set_immediate_beat(&self, beat: impl Fn() -> bool + Send + Sync + 'static) {
        *self.immediate.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(beat));
    }

    /// Send the pending report now as an extra presence beat. `false` when no
    /// host is registered or its registry socket is not live -- the caller's
    /// cue to spend an HTTP request instead.
    pub fn beat_now(&self) -> bool {
        // Clone the hook out so it never runs under this slot's lock: it reads
        // `pending()`, which takes the other one.
        let beat = self
            .immediate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        beat.is_some_and(|beat| beat())
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
    fn beat_now_falls_back_until_a_live_host_carries_it() {
        let slot = ViewportActivity::default();
        slot.record(report(1, false, None));
        // No workspace host registered yet (still booting, or no Edge).
        assert!(!slot.beat_now());
        let live = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = live.clone();
        slot.set_immediate_beat(move || flag.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!slot.beat_now(), "registered, but the socket is down");
        live.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(slot.beat_now());
    }

    #[test]
    fn the_hook_reads_the_pending_report_without_deadlocking() {
        // The host's hook reads `pending()`; `beat_now` must not hold a lock
        // across the call or this test hangs instead of failing.
        let slot = ViewportActivity::default();
        let seen = Arc::new(Mutex::new(None));
        let (inner, record) = (slot.clone(), seen.clone());
        slot.set_immediate_beat(move || {
            *record.lock().unwrap() = inner.pending();
            true
        });
        slot.record(report(3, true, Some("chat-q")));
        assert!(slot.beat_now());
        assert_eq!(seen.lock().unwrap().as_ref().unwrap()["chatId"], "chat-q");
    }

    #[test]
    fn the_slot_is_shared_by_clone() {
        let a = ViewportActivity::default();
        let b = a.clone();
        a.record(report(7, true, Some("chat-z")));
        assert_eq!(b.pending().expect("shared")["sequence"], 7);
    }
}
