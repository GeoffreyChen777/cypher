//! Inactive P1 codec. Parsing is NOT authorization; see docs/ephemeral-stream-v1.md.
//! Deliberately not registered in ChatClient or the legacy wire dispatcher.

use crate::chat_frames::{self, WireFrame};

pub const CAPABILITY: &str = "ephemeral-stream-v1";
pub const DELTA: u8 = 0x20;
pub const SNAPSHOT: u8 = 0x21;
pub const RESUME: u8 = 0x22;
pub const FINISHED: u8 = 0x23;
pub const START: u8 = 0x24;
pub const STATE: u8 = 0x25;
pub const RECEIPT: u8 = 0x26;
pub const MAX_FRAME_BYTES: usize = 65_536;
pub const MAX_TEXT_BYTES: usize = 61_440;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn id(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 128
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
    })
}

fn number(value: &serde_json::Value) -> Option<u64> {
    // Accept JSON 1.0 / 1e0 consistently with JavaScript and Foundation.
    let n = value.as_f64()?;
    (n >= 0.0 && n <= MAX_SAFE_INTEGER as f64 && n.fract() == 0.0).then_some(n as u64)
}

pub fn decode(bytes: &[u8]) -> Option<WireFrame> {
    if bytes.len() > MAX_FRAME_BYTES {
        return None;
    }
    let frame = chat_frames::decode(bytes)?;
    if matches!(frame.kind, START | STATE) {
        let h = frame.header.as_object()?;
        let keys: &[&str] = if frame.kind == START {
            &["chatId", "runId", "segmentId"]
        } else if h.get("mode")?.as_str()? == "preview" {
            &["chatId", "mode", "runId", "segmentId", "epoch"]
        } else {
            &["chatId", "mode"]
        };
        if !frame.payload.is_empty()
            || h.len() != keys.len()
            || !keys.iter().all(|k| h.contains_key(*k))
            || !keys.iter().filter(|k| **k != "mode").all(|k| id(&h[*k]))
            || (frame.kind == STATE
                && !matches!(h["mode"].as_str()?, "legacy" | "ready" | "preview"))
        {
            return None;
        }
        return Some(frame);
    }
    let extra = match frame.kind {
        DELTA => Some("prevRevision"),
        SNAPSHOT | RESUME | RECEIPT => None,
        FINISHED => Some("batchId"),
        _ => return None,
    };
    let header = frame.header.as_object()?;
    let common = [
        "chatId",
        "runId",
        "segmentId",
        "epoch",
        "revision",
        "baseSeq",
    ];
    if header.len() != common.len() + usize::from(extra.is_some())
        || !common.iter().all(|key| header.contains_key(*key))
        || extra.is_some_and(|key| !header.contains_key(key))
        || !common[..4].iter().all(|key| id(&header[*key]))
    {
        return None;
    }
    let revision = number(&header["revision"])?;
    number(&header["baseSeq"])?;
    match frame.kind {
        DELTA => {
            if number(&header["prevRevision"])? + 1 != revision || frame.payload.is_empty() {
                return None;
            }
        }
        FINISHED if !id(&header["batchId"]) => return None,
        _ => {}
    }
    if frame.payload.len() > MAX_TEXT_BYTES
        || std::str::from_utf8(&frame.payload).is_err()
        || (matches!(frame.kind, RESUME | FINISHED | RECEIPT) && !frame.payload.is_empty())
    {
        return None;
    }
    Some(frame)
}

pub fn encode(kind: u8, header: &serde_json::Value, text: &str) -> Option<Vec<u8>> {
    if text.len() > MAX_TEXT_BYTES {
        return None;
    }
    let bytes = chat_frames::encode(kind, header, text.as_bytes());
    decode(&bytes)?;
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn shared_vectors() {
        let vectors: Vec<Value> = serde_json::from_str(include_str!(
            "../../../edge/src/fixtures/stream-preview-v1.json"
        ))
        .unwrap();
        for v in vectors {
            let bytes = if let Some(hex) = v["hex"].as_str() {
                (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                    .collect()
            } else {
                chat_frames::encode(
                    v["kind"].as_u64().unwrap() as u8,
                    &v["header"],
                    v["text"].as_str().unwrap().as_bytes(),
                )
            };
            assert_eq!(decode(&bytes).is_some(), v["valid"], "{}", v["name"]);
            if let Some(frame) = decode(&bytes) {
                let encoded = encode(
                    frame.kind,
                    &frame.header,
                    std::str::from_utf8(&frame.payload).unwrap(),
                )
                .unwrap();
                assert_eq!(decode(&encoded), Some(frame));
            }
        }
    }

    #[test]
    fn byte_limits_and_no_legacy_reinterpretation() {
        let header = json!({"chatId":"c", "runId":"r", "segmentId":"s", "epoch":"e", "revision":0, "baseSeq":0});
        assert!(encode(SNAPSHOT, &header, &"x".repeat(MAX_TEXT_BYTES)).is_some());
        assert!(encode(SNAPSHOT, &header, &"界".repeat(MAX_TEXT_BYTES / 3 + 1)).is_none());
        assert!(encode(chat_frames::frame_type::PUSH, &header, "x").is_none());
        assert!(decode(&vec![0; MAX_FRAME_BYTES + 1]).is_none());
        let mut bad_utf8 = chat_frames::encode(SNAPSHOT, &header, &[0xff]);
        assert!(decode(&bad_utf8).is_none());
        bad_utf8 = chat_frames::encode(SNAPSHOT, &json!({"pad":"x".repeat(4096)}), &[]);
        assert!(decode(&bad_utf8).is_none());
    }
}
