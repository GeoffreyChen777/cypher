//! chat2 wire frames — Rust twin of `apps/edge/src/chat/chat-frames.ts` (the DO's
//! codec). Binary WS frames: `[type u8][headerLen u32 LE][header JSON][payload]`.
//! Headers are tiny JSON; payloads are opaque bytes (Loro updates, checkpoint
//! frontiers, presence ephemera). Cross-language contract: the TypeScript and
//! Swift codecs run the same vectors (`protocol/vectors/chat-frames-v1.json`);
//! change them together (`protocol/README.md`).

use serde::{Deserialize, Serialize};

/// Frame type bytes (shared client/server space; mirror `FRAME` in TS).
pub mod frame_type {
    pub const HELLO: u8 = 0x01;
    pub const STATE: u8 = 0x02;
    pub const ROWS_REQ: u8 = 0x03;
    pub const ROW: u8 = 0x04;
    pub const ROWS_DONE: u8 = 0x05;
    pub const PUSH: u8 = 0x06;
    pub(crate) const ACK: u8 = 0x07;
    pub(crate) const PRESENCE: u8 = 0x08;
    pub(crate) const PROBE: u8 = 0x09;
    pub(crate) const PROBE_OK: u8 = 0x0a;
    pub(crate) const ERROR: u8 = 0x0b;
}

/// Headers are ids + a few integers; anything bigger is a peer bug.
pub(crate) const MAX_HEADER_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq)]
pub struct WireFrame {
    pub kind: u8,
    pub header: serde_json::Value,
    pub payload: Vec<u8>,
}

pub fn encode(kind: u8, header: &impl Serialize, payload: &[u8]) -> Vec<u8> {
    let header = serde_json::to_vec(header).expect("frame header serializes");
    let mut out = Vec::with_capacity(5 + header.len() + payload.len());
    out.push(kind);
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(payload);
    out
}

/// `None` = malformed. Unknown type bytes are NOT rejected here (unlike the
/// DO): the client tolerates future server frame types by skipping them.
pub fn decode(bytes: &[u8]) -> Option<WireFrame> {
    if bytes.len() < 5 {
        return None;
    }
    let kind = bytes[0];
    let header_len = u32::from_le_bytes(bytes[1..5].try_into().ok()?) as usize;
    if header_len > MAX_HEADER_BYTES || 5 + header_len > bytes.len() {
        return None;
    }
    let header: serde_json::Value = serde_json::from_slice(&bytes[5..5 + header_len]).ok()?;
    if !header.is_object() {
        return None;
    }
    Some(WireFrame {
        kind,
        header,
        payload: bytes[5 + header_len..].to_vec(),
    })
}

// ── typed headers (parse via `serde_json::from_value(frame.header)`) ────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HelloHeader<'a> {
    pub cursor: u64,
    pub device: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RowsReqHeader {
    pub after: u64,
    pub exclude_own: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PushHeader<'a> {
    pub batch_id: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StateHeader {
    pub head_seq: u64,
    pub seq_floor: u64,
    pub checkpoint_seq: u64,
    pub checkpoint_size: u64,
    #[serde(default)]
    pub row_count: u64,
    #[serde(default)]
    pub row_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RowHeader {
    pub seq: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RowsDoneHeader {
    pub head_seq: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AckHeader {
    pub batch_id: String,
    pub seq: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProbeOkHeader {
    pub head_seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::Value;

    fn vectors() -> Value {
        serde_json::from_str(include_str!(
            "../../../protocol/vectors/chat-frames-v1.json"
        ))
        .unwrap()
    }

    fn hex(v: &Value) -> Vec<u8> {
        let s = v.as_str().unwrap();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn type_byte(v: &Value) -> u8 {
        v.as_u64().unwrap() as u8
    }

    #[test]
    fn shared_vectors_frame_types() {
        let v = vectors();
        let types = [
            ("hello", frame_type::HELLO),
            ("state", frame_type::STATE),
            ("rowsReq", frame_type::ROWS_REQ),
            ("row", frame_type::ROW),
            ("rowsDone", frame_type::ROWS_DONE),
            ("push", frame_type::PUSH),
            ("ack", frame_type::ACK),
            ("presence", frame_type::PRESENCE),
            ("probe", frame_type::PROBE),
            ("probeOk", frame_type::PROBE_OK),
            ("error", frame_type::ERROR),
        ];
        assert_eq!(v["types"].as_object().unwrap().len(), types.len());
        for (name, byte) in types {
            assert_eq!(type_byte(&v["types"][name]), byte, "{name}");
        }
        assert_eq!(v["maxHeaderBytes"].as_u64(), Some(MAX_HEADER_BYTES as u64));
    }

    #[test]
    fn shared_vectors_encode_and_decode() {
        for c in vectors()["encode"].as_array().unwrap() {
            let name = c["name"].as_str().unwrap();
            let frame = encode(type_byte(&c["type"]), &c["header"], &hex(&c["payload"]));
            assert_eq!(frame, hex(&c["hex"]), "{name}: encode");
            let decoded = decode(&frame).expect(name);
            assert_eq!(decoded.kind, type_byte(&c["type"]), "{name}");
            assert_eq!(decoded.header, c["header"], "{name}");
            assert_eq!(decoded.payload, hex(&c["payload"]), "{name}");
        }
    }

    #[test]
    fn shared_vectors_reject_malformed() {
        for c in vectors()["malformed"].as_array().unwrap() {
            assert!(decode(&hex(&c["hex"])).is_none(), "{}", c["name"]);
        }
    }

    #[test]
    fn shared_vectors_tolerate_unknown_types() {
        // Unlike the DO, the client decodes future frame types and skips them.
        for c in vectors()["unknownType"].as_array().unwrap() {
            let decoded = decode(&hex(&c["hex"])).expect("client decodes");
            assert_eq!(decoded.kind, type_byte(&c["type"]), "{}", c["name"]);
            assert_eq!(decoded.header, c["header"], "{}", c["name"]);
            assert_eq!(decoded.payload, hex(&c["payload"]), "{}", c["name"]);
        }
    }

    #[test]
    fn shared_vectors_header_size_limit() {
        for c in vectors()["headerSize"].as_array().unwrap() {
            // `{"pad":""}` is 10 bytes; the pad fills the header to `bytes`.
            let pad = "x".repeat(c["bytes"].as_u64().unwrap() as usize - 10);
            let frame = encode(frame_type::HELLO, &serde_json::json!({"pad": pad}), &[]);
            let valid = c["valid"].as_bool().unwrap();
            assert_eq!(decode(&frame).is_some(), valid, "{}", c["name"]);
        }
    }

    #[test]
    fn typed_headers_parse_server_shapes() {
        let state: StateHeader = serde_json::from_value(serde_json::json!({
            "headSeq": 10, "seqFloor": 3, "checkpointSeq": 3,
            "checkpointSize": 160_000, "rowCount": 7, "rowBytes": 14_000
        }))
        .unwrap();
        assert_eq!(state.head_seq, 10);
        assert_eq!(state.checkpoint_seq, 3);
        let ack: AckHeader =
            serde_json::from_value(serde_json::json!({"batchId": "b", "seq": 4})).unwrap();
        assert_eq!((ack.batch_id.as_str(), ack.seq), ("b", 4));
    }
}
