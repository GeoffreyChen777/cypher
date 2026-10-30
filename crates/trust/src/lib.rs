//! cypher-trust — which devices belong to the user, and how they prove it.
//!
//! The edge relay authenticates *accounts*: any holder of the user's bearer
//! can join any of the user's device rooms. This crate adds the *device*
//! layer on top, so a stolen token or a modified edge cannot drive a device:
//!
//! - [`DeviceKeys`]: each device's Ed25519 identity key (signs trust entries)
//!   and X25519 static key (Noise).
//! - [`TrustChain`]: the user's trust group, a hash-chained log of signed
//!   `add` / `remove` entries. The first entry is a device adding itself; every
//!   later entry must be signed by a current member. The edge stores and
//!   orders the log but cannot forge an entry, and devices only ever accept a
//!   log that extends the one they hold.
//! - [`PairingCode`]: how a new device joins. A member shows a one-time code;
//!   the new device's join request and the member's welcome are both MACed with
//!   a key derived from it, so the edge, which never sees the code, can neither
//!   forge nor redirect either message.
//! - [`noise`]: `Noise_IK_25519_ChaChaPoly_SHA256` sessions over the device
//!   relay. The client must know the host's key from the chain; the host learns
//!   the client's key in the first message and checks it against the chain.
//!
//! Wire encodings are shared with the edge (TypeScript) and the iOS client
//! (Swift) through `protocol/vectors/device-trust-v1.json`.

#![forbid(unsafe_code)]

mod chain;
mod encoding;
mod keys;
pub mod noise;
mod pairing;

pub use chain::{Comparison, EntryKind, TrustChain, TrustEntry};
pub use keys::{DeviceKeys, DevicePublic};
pub use pairing::{JoinRequest, PairingCode, Welcome};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TrustError {
    #[error("invalid trust entry: {0}")]
    InvalidEntry(String),
    #[error("the trust list on the server does not extend this device's copy")]
    Diverged,
    #[error("invalid pairing code")]
    InvalidCode,
    #[error("pairing message failed verification")]
    BadMac,
    #[error("secure channel failed: {0}")]
    Noise(String),
    #[error("malformed data: {0}")]
    Malformed(String),
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the OS CSPRNG must be available");
    bytes
}
