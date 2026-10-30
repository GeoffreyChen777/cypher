//! A device's private keys and the public record other devices trust.

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::TrustError;
use crate::encoding::{hex, hex_bytes, put_str, unhex};

/// Longest device id or name a trust entry carries, in bytes.
pub const MAX_FIELD: usize = 128;

/// One device as the trust group knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicePublic {
    pub id: String,
    pub name: String,
    /// Ed25519: signs this device's trust entries.
    #[serde(with = "hex_bytes")]
    pub sign_pub: [u8; 32],
    /// X25519: this device's Noise static key.
    #[serde(with = "hex_bytes")]
    pub dh_pub: [u8; 32],
}

impl DevicePublic {
    pub(crate) fn validate(&self) -> Result<(), TrustError> {
        let field = |value: &str, what: &str| {
            if value.is_empty() || value.len() > MAX_FIELD || value.chars().any(char::is_control) {
                Err(TrustError::InvalidEntry(format!("bad device {what}")))
            } else {
                Ok(())
            }
        };
        field(&self.id, "id")?;
        field(&self.name, "name")?;
        VerifyingKey::from_bytes(&self.sign_pub)
            .map_err(|_| TrustError::InvalidEntry("bad signing key".into()))?;
        Ok(())
    }

    /// `str id ‖ str name ‖ sign_pub ‖ dh_pub`, inside signed and MACed messages.
    pub(crate) fn put(&self, out: &mut Vec<u8>) {
        put_str(out, &self.id);
        put_str(out, &self.name);
        out.extend_from_slice(&self.sign_pub);
        out.extend_from_slice(&self.dh_pub);
    }

    pub(crate) fn verify(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        VerifyingKey::from_bytes(&self.sign_pub).is_ok_and(|key| {
            key.verify(message, &ed25519_dalek::Signature::from_bytes(signature))
                .is_ok()
        })
    }

    /// Short, human-comparable form of the device's identity key.
    pub fn fingerprint(&self) -> String {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(self.sign_pub);
        hex(&digest[..8])
    }
}

/// This device's private keys.
pub struct DeviceKeys {
    pub device_id: String,
    sign: SigningKey,
    dh: Zeroizing<[u8; 32]>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredKeys {
    version: u32,
    device_id: String,
    sign: String,
    dh: String,
}

impl DeviceKeys {
    pub fn generate(device_id: impl Into<String>) -> Self {
        Self::from_secrets(
            device_id.into(),
            crate::random::<32>(),
            crate::random::<32>(),
        )
    }

    /// Keys from fixed secrets — the vectors and tests.
    pub fn from_secrets(device_id: String, sign: [u8; 32], dh: [u8; 32]) -> Self {
        Self {
            device_id,
            sign: SigningKey::from_bytes(&sign),
            dh: Zeroizing::new(dh),
        }
    }

    pub fn public(&self, name: &str) -> DevicePublic {
        DevicePublic {
            id: self.device_id.clone(),
            name: name.to_string(),
            sign_pub: self.sign.verifying_key().to_bytes(),
            dh_pub: self.dh_public(),
        }
    }

    pub fn dh_public(&self) -> [u8; 32] {
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*self.dh)).to_bytes()
    }

    pub(crate) fn dh_private(&self) -> &[u8; 32] {
        &self.dh
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.sign.sign(message).to_bytes()
    }

    /// Sign arbitrary bytes — only the vector generator needs this, to build
    /// invalid entries with valid signatures.
    #[doc(hidden)]
    pub fn sign_for_tests(&self, message: &[u8]) -> [u8; 64] {
        self.sign(message)
    }

    pub fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
        let stored = StoredKeys {
            version: 1,
            device_id: self.device_id.clone(),
            sign: hex(&self.sign.to_bytes()),
            dh: hex(self.dh.as_slice()),
        };
        Zeroizing::new(serde_json::to_vec(&stored).expect("keys serialize"))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TrustError> {
        let stored: StoredKeys =
            serde_json::from_slice(bytes).map_err(|e| TrustError::Malformed(e.to_string()))?;
        if stored.version != 1 {
            return Err(TrustError::Malformed("unknown key format".into()));
        }
        let sign = Zeroizing::new(unhex::<32>(&stored.sign)?);
        let dh = Zeroizing::new(unhex::<32>(&stored.dh)?);
        Ok(Self::from_secrets(stored.device_id, *sign, *dh))
    }
}

impl std::fmt::Debug for DeviceKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKeys")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_never_print_secrets() {
        let keys = DeviceKeys::generate("dev-1");
        let restored = DeviceKeys::from_bytes(&keys.to_bytes()).unwrap();
        assert_eq!(restored.public("n"), keys.public("n"));
        let debug = format!("{keys:?}");
        assert!(!debug.contains(&hex(&keys.sign.to_bytes())));
        assert!(!debug.contains(&hex(keys.dh.as_slice())));
    }

    #[test]
    fn signatures_verify_against_the_public_record() {
        let keys = DeviceKeys::generate("dev-1");
        let public = keys.public("Laptop");
        let signature = keys.sign(b"message");
        assert!(public.verify(b"message", &signature));
        assert!(!public.verify(b"other", &signature));
    }

    #[test]
    fn device_records_reject_bad_fields() {
        let mut public = DeviceKeys::generate("dev-1").public("Laptop");
        assert!(public.validate().is_ok());
        public.name = "a\nb".into();
        assert!(public.validate().is_err());
        public.name = "x".repeat(MAX_FIELD + 1);
        assert!(public.validate().is_err());
        public.name = String::new();
        assert!(public.validate().is_err());
    }
}
