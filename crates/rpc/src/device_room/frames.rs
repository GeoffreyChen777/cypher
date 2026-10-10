//! The device-frame codec: `uleb128(header_len) ‖ JSON header ‖ payload`,
//! byte-identical to `apps/edge/src/device/device-frame.ts` and the Swift client
//! (shared vectors: `protocol/vectors/device-frames-v1.json`).

use serde::{Deserialize, Serialize};

use crate::RpcError;

/// The JSON frame header. Field order matters for byte-parity with the TS encoder
/// (`JSON.stringify` of `{s, k, to?, from?}`); absent routing keys are omitted, not null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceFrameHeader {
    /// Stream id, unique per (connId, logical stream).
    pub s: String,
    /// Stream kind: `"rpc"` | `"term"` | … — opaque to the relay.
    pub k: String,
    /// Routing: host → client target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Routing: client → host origin (stamped by the relay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl DeviceFrameHeader {
    pub fn new(s: impl Into<String>, k: impl Into<String>) -> Self {
        Self {
            s: s.into(),
            k: k.into(),
            to: None,
            from: None,
        }
    }

    pub fn with_to(mut self, conn_id: impl Into<String>) -> Self {
        self.to = Some(conn_id.into());
        self
    }
}

/// Encode `uleb128(header_len) ‖ header JSON ‖ payload`.
pub fn encode_device_frame(
    header: &DeviceFrameHeader,
    payload: &[u8],
) -> Result<Vec<u8>, RpcError> {
    let json = serde_json::to_vec(header)
        .map_err(|e| RpcError::Transport(format!("encode frame header: {e}")))?;
    let mut out = Vec::with_capacity(json.len() + payload.len() + 5);
    let mut n = json.len();
    loop {
        let mut byte = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&json);
    out.extend_from_slice(payload);
    Ok(out)
}

/// Decode a device frame; the payload is the remainder after the JSON header.
pub fn decode_device_frame(bytes: &[u8]) -> Result<(DeviceFrameHeader, Vec<u8>), RpcError> {
    let bad = |m: &str| RpcError::Transport(format!("device frame: {m}"));
    let mut offset = 0usize;
    let mut len: usize = 0;
    let mut shift = 0u32;
    loop {
        let byte = *bytes.get(offset).ok_or_else(|| bad("truncated uleb128"))?;
        offset += 1;
        if shift >= 32 {
            return Err(bad("uleb128 overflow"));
        }
        len |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    let header_end = offset
        .checked_add(len)
        .ok_or_else(|| bad("header length overflow"))?;
    let header_bytes = bytes
        .get(offset..header_end)
        .ok_or_else(|| bad("truncated header"))?;
    let header: DeviceFrameHeader =
        serde_json::from_slice(header_bytes).map_err(|e| bad(&format!("bad header JSON: {e}")))?;
    Ok((header, bytes[header_end..].to_vec()))
}

/// Extract the error code from a relay control payload (`{"error": code}`).
pub(crate) fn relay_error_code(payload: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct RelayError {
        error: String,
    }
    serde_json::from_slice::<RelayError>(payload)
        .ok()
        .map(|e| e.error)
}
