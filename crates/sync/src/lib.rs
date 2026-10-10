//! cypher-sync — the edge room clients (registry rows + chat2 row protocol over
//! WebSocket/HTTPS pull-push against the TS edge) and the local `DocsStore` (SQLite snapshots +
//! processed-command ledger).
//!
//! - [`ChatClient`]: joins a ChatRoom DO (`wss://…/chat2/{chatId}/ws?token=`),
//!   catches up via checkpoint + row backfill, pushes local loro updates as
//!   rows, and reconnects with exponential backoff.
//! - [`RegistryClient`]: the per-profile workspace registry room (sidebar rows,
//!   presence).
//! - [`DocsStore`]: snapshot persistence (the doc IS the outbox — commands + user entries
//!   flush immediately) and the processed-command ledger with mark-BEFORE-execute semantics.

#![forbid(unsafe_code)]

pub mod chat_client;
pub mod chat_frames;
pub mod preview_link;
pub mod registry;
mod store;
pub(crate) mod stream_preview;
mod types;

pub use chat_client::{
    ChatClient, ChatDocSink, ChatEvent, ChatStatsSnapshot, ChatTransport, ChatTuning,
    CheckpointFetcher,
};
pub use registry::{RegistryClient, RegistryEvent, RegistryTransport, RegistryTuning};
pub use store::{DocsStore, StoreError};
pub use types::{RoomStatsSnapshot, StaticUrl, SyncError, UrlProvider};

/// Lock a mutex, ignoring poisoning: the guarded state stays consistent
/// across every critical section, so one panicking thread must not take the
/// room clients down with it.
pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Wall-clock milliseconds since the Unix epoch (0 if the clock is before it).
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
