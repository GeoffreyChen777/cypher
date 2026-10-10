//! cypher-net — the WebSocket transport shared by the room clients
//! (`cypher-sync`) and the device relay (`cypher-rpc`).
//!
//! - [`dial`]: happy-eyeballs WebSocket dialing with proxy tunnelling.
//! - [`wake`]: process-wide system-wake and network-online broadcasts that
//!   tell sockets waiting out a reconnect backoff to redial now.

pub mod dial;
pub mod wake;
