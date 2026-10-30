//! Device trust on the relay (crates/trust): who may use this host, which
//! hosts this device will talk to, and the handshake frames.
//!
//! A client that knows the host's Noise key — the host is in its trust group —
//! always opens a Noise session: kind [`NOISE_KIND`] frames for the
//! handshake, then kind [`SECURE_KIND`] frames carrying sealed RPC. Without
//! the key it can only speak plaintext `rpc`, and only while "Only paired
//! devices" ([`PeerTrust::require_paired`]) is off on both ends.

use std::sync::Arc;

use async_trait::async_trait;
use cypher_trust::DeviceKeys;

/// Handshake frames. Payload: one type byte, then the Noise message or a
/// UTF-8 refusal reason.
pub const NOISE_KIND: &str = "noise";

/// Sealed RPC: each payload is one fragment from `cypher_trust::noise::Session`.
pub const SECURE_KIND: &str = "rpcx";

pub(crate) const HELLO: u8 = 1;
pub(crate) const REPLY: u8 = 2;
pub(crate) const REFUSED: u8 = 3;

/// Refusal reasons a host sends in a [`REFUSED`] frame.
pub const UNTRUSTED_DEVICE: &str = "untrusted_device";
pub const HANDSHAKE_FAILED: &str = "handshake_failed";
pub const CHANNEL_FAILED: &str = "secure_channel_failed";

/// What the relay needs to know about device trust. The engine implements it
/// over the trust chain; [`NoTrust`] keeps the plaintext behaviour.
#[async_trait]
pub trait PeerTrust: Send + Sync + 'static {
    /// This device's keys, once it has them.
    fn local_keys(&self) -> Option<Arc<DeviceKeys>>;
    /// `device_id`'s Noise key when it is in this device's trust group.
    fn peer_key(&self, device_id: &str) -> Option<[u8; 32]>;
    /// Whether `client_id` with Noise key `client_key` is in the group. May
    /// refresh the group from the edge first, so a device paired a moment
    /// ago is recognised.
    async fn is_member(&self, client_id: &str, client_key: &[u8; 32]) -> bool;
    /// "Only paired devices": refuse plaintext and unknown devices, as host
    /// and as client.
    fn require_paired(&self) -> bool;
    /// [`Self::is_member`] against the group as this device holds it now,
    /// without refreshing: checked on every frame of an open session, so a
    /// removal cuts it off as soon as this device learns of it.
    fn is_known_member(&self, client_id: &str, client_key: &[u8; 32]) -> bool;
    /// Re-read the group before deciding on a new client, so a
    /// policy switched on elsewhere a moment ago already holds. Implementations
    /// rate-limit it.
    async fn refresh(&self) {}
}

/// No device keys: plaintext only, never refuses.
pub struct NoTrust;

#[async_trait]
impl PeerTrust for NoTrust {
    fn local_keys(&self) -> Option<Arc<DeviceKeys>> {
        None
    }

    fn peer_key(&self, _: &str) -> Option<[u8; 32]> {
        None
    }

    async fn is_member(&self, _: &str, _: &[u8; 32]) -> bool {
        false
    }

    fn is_known_member(&self, _: &str, _: &[u8; 32]) -> bool {
        false
    }

    fn require_paired(&self) -> bool {
        false
    }
}

/// `[kind] ‖ body`.
pub(crate) fn noise_payload(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + body.len());
    out.push(kind);
    out.extend_from_slice(body);
    out
}

/// What a refusal means for the person using the client.
pub(crate) fn refusal_message(host: &str, reason: &str) -> String {
    match reason {
        UNTRUSTED_DEVICE => format!(
            "Device {host} only accepts paired devices, and this one isn't paired with it. Pair it in Settings → Devices."
        ),
        _ => format!("Device {host} refused the secure connection ({reason})."),
    }
}

/// The error a host that only accepts paired devices answers plaintext
/// requests with.
pub(crate) const PLAINTEXT_REFUSED: &str = "This device only accepts paired devices. Update Cypher on the device you're using and pair it in Settings → Devices.";
