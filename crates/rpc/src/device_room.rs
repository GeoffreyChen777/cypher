//! Device-room relay transport (ARCHITECTURE §1): the byte-frame
//! codec spoken by the edge `DeviceRoom` DO, the **host relay** (this device serving its
//! full RPC surface through the relay), and the **client link** (dialing another device's
//! relay and speaking ordinary [`RpcClient`] RPC over it).
//!
//! Frame encoding (must stay byte-identical to `apps/edge/src/device/device-frame.ts`):
//! `uleb128(header_len) ‖ UTF-8 JSON header ‖ payload`, header `{s, k, to?, from?}`.
//! - client → DO: the DO stamps `from = connId` and forwards to the host socket;
//! - host → DO: must carry `to = connId`; the DO strips routing keys and delivers;
//! - relay control frames use kind [`RELAY_KIND`] with payload `{"error": code}` —
//!   codes `host_offline`, `host_closed`, `client_gone`, `client_closed`;
//! - nudge frames use kind [`NUDGE_KIND`] with payload `{"chatId": …}`.
//!
//! The RPC path multiplexes NOTHING new: each distinct client `connId` becomes a virtual
//! string-frame connection feeding the existing [`serve_connection`] seam, so every RPC
//! handler works through the relay untouched.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

mod frames;
mod links;
mod relay;
#[cfg(test)]
mod tests;

pub use frames::{DeviceFrameHeader, decode_device_frame, encode_device_frame};
pub use links::{DeviceLink, LinkCache, LinkCacheConfig};
pub use relay::{HostRelay, HostRelayConfig};

/// Relay-emitted control frames. MUST byte-match the DO's `RELAY_KIND` (yes, it has a
/// leading space — clients compare with equality; a mismatch makes host_offline invisible).
pub const RELAY_KIND: &str = " relay";

/// Durable command nudge frames (§7 cold-chat delivery): payload `{chatId}`.
pub const NUDGE_KIND: &str = "nudge";

/// The RPC stream over the relay: both `s` (stream id) and `k` (kind) are `"rpc"`.
pub(crate) const RPC_KIND: &str = "rpc";

/// Relay error codes (payload `{"error": code}` on [`RELAY_KIND`] frames).
pub const HOST_OFFLINE: &str = "host_offline";

pub const HOST_CLOSED: &str = "host_closed";

pub const CLIENT_GONE: &str = "client_gone";

pub const CLIENT_CLOSED: &str = "client_closed";

/// Text `"ping"` keepalive — answered by the DO's hibernation-safe auto-response
/// pair (`apps/edge/src/device/device-room.ts`) without waking it.
///
/// 15s, not 30: a laptop's uplink (corporate proxy, VPN split-tunnel extension,
/// consumer NAT) can reap an idle flow well inside a minute, and a keepalive
/// that races the reaper loses. The frame is 4 bytes and never wakes the DO, so
/// the only cost of halving the interval is that the host stays reachable.
const PING_INTERVAL: Duration = Duration::from_secs(15);

/// Silence lease: every ping elicits an auto-pong, so a healthy socket sees
/// inbound traffic at least once per `PING_INTERVAL`. No inbound frame for a
/// couple of intervals plus grace = dead socket (half-open TCP after NAT
/// timeout or sleep/wake) — drop it and reconnect instead of waiting on a TCP
/// write error. Must stay well under the relay's own host-liveness window
/// (`HOST_LIVENESS_MS`, apps/edge/src/device/device-room.ts) so a host replaces its dead
/// socket before the relay gives up on the device.
const SILENCE_LEASE: Duration = Duration::from_secs(40);

/// Build the device-room WebSocket URL from the http(s) edge base URL.
pub fn device_room_ws_url(
    edge_url: &str,
    device_id: &str,
    role: &str,
    conn_id: Option<&str>,
    token: &str,
) -> String {
    let ws_base = edge_url.replacen("http", "ws", 1);
    let ws_base = ws_base.trim_end_matches('/');
    let conn = conn_id.map(|c| format!("&connId={c}")).unwrap_or_default();
    format!("{ws_base}/device/{device_id}/ws?role={role}{conn}&token={token}")
}

/// Fresh-bearer provider: the relay re-reads it on every (re)dial so an expired access
/// token is never reused after a refresh. `None` = signed out (host relay idles quietly).
#[async_trait]
pub trait TokenSource: Send + Sync + 'static {
    async fn token(&self) -> Option<String>;

    /// Changes whenever credentials become available or are replaced. Long-lived
    /// supervisors use this to retry immediately instead of waiting for backoff.
    fn subscribe(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }
}

async fn token_changed(changes: &mut Option<tokio::sync::watch::Receiver<u64>>) {
    match changes {
        Some(changes) => {
            let _ = changes.changed().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// A fixed token (dev mode / tests).
pub struct StaticToken(pub String);

#[async_trait]
impl TokenSource for StaticToken {
    async fn token(&self) -> Option<String> {
        Some(self.0.clone())
    }
}

/// Called with the chat id of every nudge frame ("this chat's doc has pending commands —
/// open it and drain"); the engine warms/opens the chat doc.
pub type NudgeHandler = Arc<dyn Fn(String) + Send + Sync>;
