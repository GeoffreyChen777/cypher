//! Small crate-wide helpers: poison-ignoring locks, clocks, ids and the
//! blocking-pool bridge.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock a `std::sync::Mutex`, ignoring poisoning. Engine critical sections are
/// short, so one panicking holder must not take every later caller down too.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Epoch millis now — the doc/journal timestamp base.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Run blocking work (subprocesses, archive extraction, large directory
/// removals, SQLite) on tokio's blocking pool instead of a runtime worker.
///
/// The headed app embeds this engine in a small runtime shared with the IPC
/// server, presence heartbeats, sync and agent runs. A synchronous call on a
/// worker thread stalls every other task on that worker; enough of them at
/// once stall the engine entirely with no panic and no log line (presence
/// goes dark, `cypher sync` times out in the WebSocket handshake). Anything
/// that can take more than a few milliseconds goes through here.
pub(crate) async fn off_runtime<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| format!("background task failed: {err}"))
}

/// Trimmed env var or the given default.
pub(crate) fn env_or(key: &str, default: &str) -> String {
    cypher_env::var(key).unwrap_or_else(|| default.to_string())
}
