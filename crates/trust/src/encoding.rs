//! Byte encodings shared with the edge and the iOS client: lowercase hex on
//! the JSON wire, and length-prefixed fields in signed and MACed messages.

use crate::TrustError;

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn unhex<const N: usize>(text: &str) -> Result<[u8; N], TrustError> {
    let bad = || TrustError::Malformed(format!("expected {N} hex-encoded bytes"));
    if text.len() != N * 2 {
        return Err(bad());
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2).ok_or_else(bad)?, 16)
            .map_err(|_| bad())?;
    }
    Ok(out)
}

/// `u32be(len) ‖ utf8`.
pub(crate) fn put_str(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

/// Serde adapters for fixed-size byte arrays as lowercase hex.
pub(crate) mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer, const N: usize>(
        bytes: &[u8; N],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::hex(bytes))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<[u8; N], D::Error> {
        let text = String::deserialize(deserializer)?;
        super::unhex(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_rejects_bad_input() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(unhex::<3>("00abff").unwrap(), [0x00, 0xab, 0xff]);
        assert!(unhex::<3>("00abf").is_err());
        assert!(unhex::<3>("00abfg").is_err());
        assert!(unhex::<2>("é0").is_err());
    }
}
