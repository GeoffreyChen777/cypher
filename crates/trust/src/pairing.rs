//! Joining the trust group with a one-time code.
//!
//! A member shows a code; the new device's user types or scans it there.
//! 20 Crockford base32 symbols carry 100 bits — enough that the edge, which
//! relays both messages but never sees the code, cannot brute-force the MAC
//! key offline. Both ends derive from the code:
//!
//! - a mailbox name, where the two messages meet on the edge;
//! - a MAC key. The new device MACs its public record ([`JoinRequest`]); the
//!   member MACs the log position that adds that record ([`Welcome`]). Each
//!   side therefore knows the other's message came from someone holding the
//!   code, so neither a substituted device nor a substituted log gets through.

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::TrustError;
use crate::encoding::{hex, hex_bytes};
use crate::keys::DevicePublic;

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const SYMBOLS: usize = 20;

#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode(String);

/// The new device's half: its public record, MACed with the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequest {
    pub device: DevicePublic,
    #[serde(with = "hex_bytes")]
    pub mac: [u8; 32],
}

/// The member's half: the log entry that added the device, MACed with the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Welcome {
    pub seq: u64,
    #[serde(with = "hex_bytes")]
    pub entry: [u8; 32],
    #[serde(with = "hex_bytes")]
    pub mac: [u8; 32],
}

impl PairingCode {
    pub fn generate() -> Self {
        let bytes = crate::random::<SYMBOLS>();
        Self(
            bytes
                .iter()
                .map(|b| ALPHABET[(b & 31) as usize] as char)
                .collect(),
        )
    }

    /// Accept what a person typed: any case, dashes and spaces, and the
    /// Crockford look-alikes (O for 0, I and L for 1).
    pub fn parse(input: &str) -> Result<Self, TrustError> {
        let mut code = String::with_capacity(SYMBOLS);
        for c in input.chars() {
            let c = match c.to_ascii_uppercase() {
                '-' | ' ' => continue,
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            };
            if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) {
                return Err(TrustError::InvalidCode);
            }
            code.push(c);
        }
        if code.len() != SYMBOLS {
            return Err(TrustError::InvalidCode);
        }
        Ok(Self(code))
    }

    /// `XXXX-XXXX-XXXX-XXXX-XXXX`.
    pub fn display(&self) -> String {
        self.0
            .as_bytes()
            .chunks(4)
            .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("-")
    }

    fn derive<const N: usize>(&self, info: &[u8]) -> [u8; N] {
        let mut out = [0u8; N];
        Hkdf::<Sha256>::new(Some(b"cypher-pairing-v1"), self.0.as_bytes())
            .expand(info, &mut out)
            .expect("HKDF output length is valid");
        out
    }

    /// Where the two messages meet on the edge.
    pub fn mailbox(&self) -> String {
        hex(&self.derive::<16>(b"mailbox"))
    }

    fn hmac(&self, label: &[u8], parts: &[&[u8]]) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.derive::<32>(b"mac"))
            .expect("HMAC takes any key length");
        mac.update(label);
        for part in parts {
            mac.update(part);
        }
        mac
    }

    fn mac(&self, label: &[u8], parts: &[&[u8]]) -> [u8; 32] {
        self.hmac(label, parts).finalize().into_bytes().into()
    }

    fn check(&self, label: &[u8], parts: &[&[u8]], given: &[u8; 32]) -> Result<(), TrustError> {
        self.hmac(label, parts)
            .verify_slice(given)
            .map_err(|_| TrustError::BadMac)
    }

    pub fn join(&self, device: &DevicePublic) -> JoinRequest {
        let mut record = Vec::new();
        device.put(&mut record);
        JoinRequest {
            device: device.clone(),
            mac: self.mac(b"join\0", &[&record]),
        }
    }

    pub fn verify_join(&self, request: &JoinRequest) -> Result<(), TrustError> {
        request.device.validate()?;
        let mut record = Vec::new();
        request.device.put(&mut record);
        self.check(b"join\0", &[&record], &request.mac)
    }

    /// `entry` is the hash of the log entry at `seq` that adds `device`.
    pub fn welcome(&self, seq: u64, entry: [u8; 32], device: &DevicePublic) -> Welcome {
        let mut record = Vec::new();
        device.put(&mut record);
        Welcome {
            seq,
            entry,
            mac: self.mac(b"welcome\0", &[&seq.to_be_bytes(), &entry, &record]),
        }
    }

    pub fn verify_welcome(
        &self,
        welcome: &Welcome,
        device: &DevicePublic,
    ) -> Result<(), TrustError> {
        let mut record = Vec::new();
        device.put(&mut record);
        self.check(
            b"welcome\0",
            &[&welcome.seq.to_be_bytes(), &welcome.entry, &record],
            &welcome.mac,
        )
    }
}

impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingCode(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceKeys;

    #[test]
    fn codes_parse_the_way_people_type_them() {
        let code = PairingCode::generate();
        let shown = code.display();
        assert_eq!(shown.len(), 24);
        assert_eq!(PairingCode::parse(&shown).unwrap(), code);
        assert_eq!(
            PairingCode::parse(&shown.to_lowercase().replace('-', " ")).unwrap(),
            code
        );
        assert_eq!(
            PairingCode::parse("oil0-0000-0000-0000-0000").unwrap(),
            PairingCode::parse("0110-0000-0000-0000-0000").unwrap()
        );
        assert!(PairingCode::parse("0000-0000-0000-0000-000").is_err());
        assert!(PairingCode::parse("0000-0000-0000-0000-000U").is_err());
        assert!(PairingCode::parse("0000-0000-0000-0000-00000").is_err());
    }

    #[test]
    fn both_messages_authenticate_with_the_code_only() {
        let code = PairingCode::generate();
        let other = PairingCode::generate();
        assert_ne!(code.mailbox(), other.mailbox());
        let device = DeviceKeys::generate("new").public("Phone");

        let join = code.join(&device);
        code.verify_join(&join).unwrap();
        assert_eq!(other.verify_join(&join), Err(TrustError::BadMac));
        let mut swapped = join.clone();
        swapped.device = DeviceKeys::generate("evil").public("Phone");
        assert_eq!(code.verify_join(&swapped), Err(TrustError::BadMac));

        let welcome = code.welcome(3, [9; 32], &device);
        code.verify_welcome(&welcome, &device).unwrap();
        let mut moved = welcome.clone();
        moved.entry = [8; 32];
        assert_eq!(
            code.verify_welcome(&moved, &device),
            Err(TrustError::BadMac)
        );
        assert_eq!(
            other.verify_welcome(&welcome, &device),
            Err(TrustError::BadMac)
        );
    }
}
