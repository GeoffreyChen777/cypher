//! Registry wire frames (JSON text) — the Rust side of
//! `apps/edge/src/registry/registry-room.ts`; the two change together.

use std::collections::HashMap;

use cypher_doc::{RegistryRow, RowOp};
use serde::{Deserialize, Serialize};

/// Hello capability: this client understands the `trust` frame (chain-head
/// pushes, T1 in docs/design/do-cost.md). The room sends that frame only to
/// sockets that declared it, because a client redials on a frame type it
/// does not know; an older room ignores the field.
pub const CAP_TRUST: &str = "trust";

#[derive(Serialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum ClientFrame<'a> {
    Hello {
        cursor: Option<u64>,
        device: &'a str,
        caps: &'a [&'a str],
    },
    Push {
        batch: &'a str,
        ops: &'a [RowOp],
    },
    Presence {
        at: i64,
        /// The viewport's periodic activity refresh, piggybacked. This frame
        /// already flows every 15s and bills 20:1, so carrying the refresh
        /// here is free, where the identical report over HTTP cost a whole
        /// billable request. Transitions still go over HTTP — only they need
        /// the reply.
        #[serde(skip_serializing_if = "Option::is_none")]
        activity: Option<&'a serde_json::Value>,
    },
    Probe,
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum ServerFrame {
    State {
        seq: u64,
        full: bool,
        #[serde(rename = "gcFloor", default)]
        gc_floor: u64,
        rows: Vec<RegistryRow>,
        #[serde(default)]
        presence: HashMap<String, i64>,
        /// The user's trust chain length, when the room knows it. Absent from
        /// rooms that predate chain-head pushes.
        #[serde(default)]
        trust: Option<TrustHead>,
    },
    Rows {
        seq: u64,
        rows: Vec<RegistryRow>,
    },
    Ack {
        batch: String,
        seq: u64,
        #[allow(dead_code)]
        applied: u64,
    },
    Presence {
        device: String,
        at: i64,
    },
    #[serde(rename = "probe-ok")]
    ProbeOk {
        #[allow(dead_code)]
        seq: u64,
    },
    Error {
        code: String,
        message: String,
    },
    /// The user's trust chain grew (sent only to sockets with [`CAP_TRUST`]).
    /// A hint: the client still reads and verifies the chain over HTTPS.
    Trust {
        length: u64,
    },
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustHead {
    pub length: u64,
}

// ── transport plumbing (text-frame sibling of chat_client's BinPipe) ───────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_declares_the_trust_capability() {
        let hello = serde_json::to_value(ClientFrame::Hello {
            cursor: Some(4),
            device: "dev-a",
            caps: &[CAP_TRUST],
        })
        .unwrap();
        assert_eq!(
            hello,
            serde_json::json!({"t":"hello","cursor":4,"device":"dev-a","caps":["trust"]})
        );
    }

    #[test]
    fn trust_frame_and_state_trust_parse() {
        let frame: ServerFrame = serde_json::from_str(r#"{"t":"trust","length":7}"#).unwrap();
        assert!(matches!(frame, ServerFrame::Trust { length: 7 }));
        let state: ServerFrame = serde_json::from_str(
            r#"{"t":"state","seq":1,"full":true,"gcFloor":0,"rows":[],"presence":{},"trust":{"length":3}}"#,
        )
        .unwrap();
        assert!(matches!(
            state,
            ServerFrame::State {
                trust: Some(TrustHead { length: 3 }),
                ..
            }
        ));
    }

    #[test]
    fn state_from_a_room_without_chain_heads_has_no_trust() {
        let state: ServerFrame =
            serde_json::from_str(r#"{"t":"state","seq":1,"full":true,"rows":[]}"#).unwrap();
        assert!(matches!(state, ServerFrame::State { trust: None, .. }));
    }
}
