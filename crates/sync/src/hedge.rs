//! The HTTPS pull that hedges a WebSocket dial (C1, `docs/design/do-cost.md`).
//!
//! A connect used to start an HTTPS pull at the same instant as the dial, and
//! the socket's `state` then carried the same rows: two billable Durable
//! Object requests for one connect. Now a connect that follows the client's
//! start or a dropped session arms the hedge; the dial that follows starts
//! it, and the pull runs only if the session has not joined (`state`
//! received) by [`HEDGE_DELAY`]. Dial failures and timeouts still pull at
//! once, and so does a connect with a local write waiting to be sent, a
//! client start on a local copy that never synced (a cold open, so its first
//! row is not delayed), and a reconnect after a socket that stalled; those
//! paths are the callers'.

use std::time::Duration;

use tokio::time::Instant;

/// How long a dial may take to join before the HTTPS pull starts beside it.
pub(crate) const HEDGE_DELAY: Duration = Duration::from_millis(1500);

#[derive(Debug, Default)]
pub(crate) struct Hedge {
    armed: bool,
    at: Option<Instant>,
}

impl Hedge {
    /// The next dial hedges: the client started or a joined session dropped.
    pub(crate) fn arm(&mut self) {
        self.armed = true;
    }

    /// A dial starts: an armed hedge becomes due [`HEDGE_DELAY`] from now.
    pub(crate) fn dial(&mut self) {
        if std::mem::take(&mut self.armed) {
            self.at = Some(Instant::now() + HEDGE_DELAY);
        }
    }

    /// The session joined, or a pull already ran for this connect.
    pub(crate) fn cancel(&mut self) {
        self.armed = false;
        self.at = None;
    }

    pub(crate) fn due_at(&self) -> Option<Instant> {
        self.at
    }
}

/// Resolves at `at`; never when there is no hedge.
pub(crate) async fn until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
