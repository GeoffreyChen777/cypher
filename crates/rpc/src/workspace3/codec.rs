use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, Write};

/// A call/result is bounded independently of the 64 KiB routing envelope.
/// At most 256 input fragments and two unacknowledged fragments per direction.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
pub const CHUNK_BYTES: usize = 32 * 1024;
const CODEC: &str = "json-base64-v3";

fn error() -> crate::RpcError {
    crate::RpcError::Transport("invalid_or_oversized_rpc_payload".into())
}

struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_MESSAGE_BYTES - self.0.len() {
            return Err(io::Error::other("rpc_payload_too_large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Part {
    codec: String,
    length: usize,
    offset: usize,
    end: bool,
    data: String,
}

pub struct Encoder {
    bytes: Vec<u8>,
    offset: usize,
}
impl Encoder {
    pub fn new(value: &impl Serialize) -> Result<Self, crate::RpcError> {
        let mut bytes = Bounded(Vec::new());
        serde_json::to_writer(&mut bytes, value).map_err(|_| error())?;
        Ok(Self {
            bytes: bytes.0,
            offset: 0,
        })
    }
}
impl Iterator for Encoder {
    type Item = Value;
    fn next(&mut self) -> Option<Value> {
        if self.offset == self.bytes.len() {
            return None;
        }
        let next = (self.offset + CHUNK_BYTES).min(self.bytes.len());
        let value = serde_json::json!({
            "codec": CODEC, "length": self.bytes.len(), "offset": self.offset,
            "end": next == self.bytes.len(), "data": STANDARD.encode(&self.bytes[self.offset..next]),
        });
        self.offset = next;
        Some(value)
    }
}

#[derive(Default)]
pub struct Decoder {
    bytes: Vec<u8>,
    length: Option<usize>,
    poisoned: bool,
}
impl Decoder {
    /// None means a *bounded* incomplete value. An error permanently poisons
    /// this call: later bytes cannot repair/reinterpret a malformed prefix.
    pub fn push(&mut self, value: Value) -> Result<Option<Value>, crate::RpcError> {
        if self.poisoned {
            return Err(error());
        }
        self.poisoned = true;
        let result = self.consume(value);
        if result.is_ok() {
            self.poisoned = false;
        } else {
            self.bytes = Vec::new();
        }
        result
    }
    fn consume(&mut self, value: Value) -> Result<Option<Value>, crate::RpcError> {
        let p: Part = serde_json::from_value(value).map_err(|_| error())?;
        if p.codec != CODEC
            || p.length == 0
            || p.length > MAX_MESSAGE_BYTES
            || p.offset != self.bytes.len()
            || p.offset >= p.length
            || self.length.is_some_and(|length| length != p.length)
            || p.data.len() > CHUNK_BYTES.div_ceil(3) * 4
        {
            return Err(error());
        }
        let data = STANDARD.decode(&p.data).map_err(|_| error())?;
        if data.is_empty()
            || data.len() > CHUNK_BYTES
            || data.len() > p.length - p.offset
            || p.end != (p.offset + data.len() == p.length)
            || (!p.end && data.len() != CHUNK_BYTES)
        {
            return Err(error());
        }
        self.length = Some(p.length);
        self.bytes.extend(data);
        if !p.end {
            return Ok(None);
        }
        let bytes = std::mem::take(&mut self.bytes);
        self.length = None;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| error())
    }
    pub fn incomplete(&self) -> bool {
        self.length.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lossless_unicode_and_multiple_stream_items_with_bounded_frames() {
        let value = serde_json::json!({"text":"🙂中文\n\"\\\u{0}".repeat(60_000)});
        let mut decoder = Decoder::default();
        for expected in [value, Value::Null, serde_json::json!({"done":true})] {
            let parts: Vec<_> = Encoder::new(&expected).unwrap().collect();
            for (i, part) in parts.iter().enumerate() {
                assert!(serde_json::to_vec(part).unwrap().len() < 64 * 1024);
                let got = decoder.push(part.clone()).unwrap();
                if i + 1 == parts.len() {
                    assert_eq!(got, Some(expected.clone()));
                } else {
                    assert_eq!(got, None);
                    assert!(decoder.incomplete());
                }
            }
            assert!(!decoder.incomplete());
        }
    }
    #[test]
    fn rejects_reordering_mutation_unknown_fields_and_oversized_input() {
        let parts: Vec<_> = Encoder::new(&"a".repeat(CHUNK_BYTES * 2))
            .unwrap()
            .collect();
        assert!(Decoder::default().push(parts[1].clone()).is_err());
        for (key, value) in [
            ("length", serde_json::json!(MAX_MESSAGE_BYTES + 1)),
            ("extra", Value::Null),
            ("data", serde_json::json!("%%%")),
            ("end", Value::Bool(true)),
        ] {
            let mut bad = parts[0].clone();
            bad[key] = value;
            let mut decoder = Decoder::default();
            assert!(decoder.push(bad).is_err());
            assert!(
                decoder.push(parts[0].clone()).is_err(),
                "poisoned call cannot restart"
            );
        }
        let mut decoder = Decoder::default();
        decoder.push(parts[0].clone()).unwrap();
        assert!(decoder.push(parts[0].clone()).is_err());
        assert!(Encoder::new(&"x".repeat(MAX_MESSAGE_BYTES)).is_err());
    }
}
