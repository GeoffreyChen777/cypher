//! What a durable command's signature covers.
//!
//! The issuing device signs its entry with its device-trust identity key; the
//! host checks the signature before it runs the command (the engine's
//! `doc_host`), so whoever can write to the synced doc — a stolen sign-in, a
//! modified server — can't queue or alter commands under a paired device's
//! name.
//!
//! The signed bytes are `"cypher-command-v1\0" ‖ str(chatId) ‖ value(entry)`,
//! where `entry` is the entry's Loro map without the fields the host writes
//! later ([`UNSIGNED_FIELDS`]). Values are encoded from the Loro value tree,
//! not JSON, so the Rust and Swift writers agree on every byte:
//!
//! | value  | encoding                                             |
//! | ------ | ---------------------------------------------------- |
//! | null   | `0x00`                                               |
//! | bool   | `0x01` ‖ `0x00`/`0x01`                               |
//! | i64    | `0x02` ‖ 8 bytes big-endian                          |
//! | double | `0x03` ‖ IEEE 754 bits, 8 bytes big-endian           |
//! | string | `0x04` ‖ u32 BE byte length ‖ UTF-8                  |
//! | binary | `0x05` ‖ u32 BE length ‖ bytes                       |
//! | list   | `0x06` ‖ u32 BE count ‖ items                        |
//! | map    | `0x07` ‖ u32 BE count ‖ (str(key) ‖ value)… by key bytes |
//!
//! `str(s)` is `u32 BE byte length ‖ UTF-8`. Shared with iOS through
//! `protocol/vectors/command-signing-v1.json`.

use std::collections::BTreeMap;

use loro::LoroValue;

/// The entry field carrying the signature: 128 lowercase hex digits.
pub const SIG_FIELD: &str = "sig";

/// Fields the signature leaves out: the host's outcome, and itself.
pub const UNSIGNED_FIELDS: [&str; 3] = ["status", "resolution", SIG_FIELD];

const DOMAIN: &[u8] = b"cypher-command-v1\0";

/// How a command entry stands for signing: the bytes its signature covers and
/// the signature it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandAuth {
    pub signing_bytes: Vec<u8>,
    pub sig: CommandSig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSig {
    /// No signature (an older device, or one that isn't paired).
    None,
    /// A `sig` field that isn't 64 bytes of hex: never valid.
    Malformed,
    Present([u8; 64]),
}

impl CommandSig {
    pub(crate) fn parse(value: Option<&LoroValue>) -> Self {
        match value {
            None | Some(LoroValue::Null) => CommandSig::None,
            Some(LoroValue::String(hex)) => decode_hex(hex)
                .map(CommandSig::Present)
                .unwrap_or(CommandSig::Malformed),
            Some(_) => CommandSig::Malformed,
        }
    }
}

/// The bytes a signature over `fields` covers, for a command in `chat_id`.
/// [`UNSIGNED_FIELDS`] are skipped wherever they appear at the top level.
pub fn command_signing_bytes<'a>(
    chat_id: &str,
    fields: impl IntoIterator<Item = (&'a str, &'a LoroValue)>,
) -> Vec<u8> {
    let signed: BTreeMap<&str, &LoroValue> = fields
        .into_iter()
        .filter(|(key, _)| !UNSIGNED_FIELDS.contains(key))
        .collect();
    let mut out = DOMAIN.to_vec();
    put_str(&mut out, chat_id);
    out.push(0x07);
    put_len(&mut out, signed.len());
    for (key, value) in signed {
        put_str(&mut out, key);
        put_value(&mut out, value);
    }
    out
}

/// 64 bytes as the `sig` field's hex.
pub fn encode_sig(sig: &[u8; 64]) -> String {
    sig.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex(hex: &str) -> Option<[u8; 64]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 128 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 64];
    for (i, pair) in bytes.chunks(2).enumerate() {
        out[i] = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Some(out)
}

fn put_len(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&(len as u32).to_be_bytes());
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_len(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

fn put_value(out: &mut Vec<u8>, value: &LoroValue) {
    match value {
        LoroValue::Null => out.push(0x00),
        LoroValue::Bool(b) => out.extend_from_slice(&[0x01, *b as u8]),
        LoroValue::I64(n) => {
            out.push(0x02);
            out.extend_from_slice(&n.to_be_bytes());
        }
        LoroValue::Double(f) => {
            out.push(0x03);
            out.extend_from_slice(&f.to_bits().to_be_bytes());
        }
        LoroValue::String(s) => {
            out.push(0x04);
            put_str(out, s);
        }
        LoroValue::Binary(b) => {
            out.push(0x05);
            put_len(out, b.len());
            out.extend_from_slice(b);
        }
        LoroValue::List(items) => {
            out.push(0x06);
            put_len(out, items.len());
            for item in items.iter() {
                put_value(out, item);
            }
        }
        LoroValue::Map(map) => {
            let sorted: BTreeMap<&str, &LoroValue> =
                map.iter().map(|(k, v)| (k.as_str(), v)).collect();
            out.push(0x07);
            put_len(out, sorted.len());
            for (key, value) in sorted {
                put_str(out, key);
                put_value(out, value);
            }
        }
        // A deep value holds no containers; an entry built from one can't be
        // signed meaningfully, and no writer produces one.
        LoroValue::Container(_) => out.push(0xff),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, LoroValue)]) -> LoroValue {
        LoroValue::Map(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect::<std::collections::HashMap<_, _>>()
                .into(),
        )
    }

    #[test]
    fn the_encoding_is_typed_ordered_and_skips_the_hosts_fields() {
        let payload = map(&[("b", LoroValue::I64(1)), ("a", LoroValue::Double(1.0))]);
        let id = LoroValue::from("c1");
        let status = LoroValue::from("pending");
        let bytes = command_signing_bytes("chat", [("payload", &payload), ("id", &id)]);
        let with_status = command_signing_bytes(
            "chat",
            [("id", &id), ("status", &status), ("payload", &payload)],
        );
        assert_eq!(bytes, with_status, "order and host fields don't matter");
        assert_ne!(
            bytes,
            command_signing_bytes("other", [("id", &id), ("payload", &payload)])
        );
        // An integer and the same number as a double differ.
        let as_int = map(&[("b", LoroValue::I64(1)), ("a", LoroValue::I64(1))]);
        assert_ne!(
            bytes,
            command_signing_bytes("chat", [("payload", &as_int), ("id", &id)])
        );
        let mut expected = b"cypher-command-v1\0".to_vec();
        expected.extend_from_slice(&[0, 0, 0, 4]);
        expected.extend_from_slice(b"chat");
        expected.extend_from_slice(&[7, 0, 0, 0, 2]);
        expected.extend_from_slice(&[0, 0, 0, 2]);
        expected.extend_from_slice(b"id");
        expected.extend_from_slice(&[4, 0, 0, 0, 2]);
        expected.extend_from_slice(b"c1");
        expected.extend_from_slice(&[0, 0, 0, 7]);
        expected.extend_from_slice(b"payload");
        expected.extend_from_slice(&[7, 0, 0, 0, 2, 0, 0, 0, 1, b'a', 3]);
        expected.extend_from_slice(&1.0f64.to_bits().to_be_bytes());
        expected.extend_from_slice(&[0, 0, 0, 1, b'b', 2, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn signatures_parse_strictly() {
        let sig = [0xabu8; 64];
        let hex = encode_sig(&sig);
        assert_eq!(
            CommandSig::parse(Some(&LoroValue::from(hex.as_str()))),
            CommandSig::Present(sig)
        );
        assert_eq!(CommandSig::parse(None), CommandSig::None);
        assert_eq!(CommandSig::parse(Some(&LoroValue::Null)), CommandSig::None);
        for bad in [hex.to_uppercase(), hex[2..].to_string(), "zz".repeat(64)] {
            assert_eq!(
                CommandSig::parse(Some(&LoroValue::from(bad.as_str()))),
                CommandSig::Malformed
            );
        }
        assert_eq!(
            CommandSig::parse(Some(&LoroValue::I64(1))),
            CommandSig::Malformed
        );
    }
}
