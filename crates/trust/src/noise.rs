//! Device-to-device sessions over the relay: `Noise_IK_25519_ChaChaPoly_SHA256`.
//!
//! The client (initiator) knows the host's static key from the trust chain,
//! so its first message is already encrypted to the host and carries its own
//! static key; the host checks that key against the chain before answering.
//! The prologue binds the session to the host's device id, so a relay cannot
//! splice a handshake meant for one device onto another.
//!
//! Noise messages are at most 65535 bytes, and RPC payloads can be larger
//! (file chunks, diffs). [`Session::seal`] splits a message into fragments,
//! each `flag ‖ ciphertext` with flag `1` while more follow; [`Session::open`]
//! reassembles. The relay delivers a connection's frames in order, so the
//! Noise nonces stay in step.

use snow::{Builder, HandshakeState, TransportState};

use crate::{DeviceKeys, TrustError};

pub const PATTERN: &str = "Noise_IK_25519_ChaChaPoly_SHA256";

/// Largest plaintext per fragment: Noise's limit less the AEAD tag.
const FRAGMENT: usize = 65_535 - 16;

/// Largest reassembled message.
pub const MAX_MESSAGE: usize = 32 * 1024 * 1024;

const MORE: u8 = 1;
const LAST: u8 = 0;

fn noise_err(error: impl std::fmt::Display) -> TrustError {
    TrustError::Noise(error.to_string())
}

pub fn prologue(host_device_id: &str) -> Vec<u8> {
    let mut out = b"cypher-device-room-v1\0".to_vec();
    out.extend_from_slice(host_device_id.as_bytes());
    out
}

fn builder<'a>(prologue: &'a [u8], local: &'a DeviceKeys) -> Result<Builder<'a>, TrustError> {
    Builder::new(PATTERN.parse().map_err(noise_err)?)
        .prologue(prologue)
        .map_err(noise_err)?
        .local_private_key(local.dh_private())
        .map_err(noise_err)
}

/// The client's side of a handshake in progress.
pub struct Initiator {
    state: HandshakeState,
}

impl Initiator {
    /// Start a session with `host_device_id`, whose static key is `host_key`.
    /// Returns the first handshake message, which carries `local`'s device id.
    pub fn start(
        local: &DeviceKeys,
        host_device_id: &str,
        host_key: &[u8; 32],
    ) -> Result<(Self, Vec<u8>), TrustError> {
        Self::start_with(local, host_device_id, host_key, None)
    }

    #[doc(hidden)]
    pub fn start_with(
        local: &DeviceKeys,
        host_device_id: &str,
        host_key: &[u8; 32],
        ephemeral: Option<&[u8; 32]>,
    ) -> Result<(Self, Vec<u8>), TrustError> {
        let prologue = prologue(host_device_id);
        let mut builder = builder(&prologue, local)?
            .remote_public_key(host_key)
            .map_err(noise_err)?;
        if let Some(ephemeral) = ephemeral {
            builder = builder.fixed_ephemeral_key_for_testing_only(ephemeral);
        }
        let mut state = builder.build_initiator().map_err(noise_err)?;
        let mut message = vec![0u8; 1024];
        let len = state
            .write_message(local.device_id.as_bytes(), &mut message)
            .map_err(noise_err)?;
        message.truncate(len);
        Ok((Self { state }, message))
    }

    /// Read the host's answer and enter transport mode.
    pub fn finish(mut self, reply: &[u8]) -> Result<Session, TrustError> {
        let mut payload = vec![0u8; reply.len()];
        self.state
            .read_message(reply, &mut payload)
            .map_err(noise_err)?;
        Session::new(self.state)
    }
}

/// The host's side after reading the first message: the client has proven it
/// holds the static key below, but the host has not answered yet.
pub struct Accepting {
    state: HandshakeState,
    /// The client's Noise static key.
    pub client_key: [u8; 32],
    /// The device id the client claims; check it against `client_key`.
    pub client_id: String,
}

impl Accepting {
    pub fn read(
        local: &DeviceKeys,
        host_device_id: &str,
        first: &[u8],
    ) -> Result<Self, TrustError> {
        Self::read_with(local, host_device_id, first, None)
    }

    #[doc(hidden)]
    pub fn read_with(
        local: &DeviceKeys,
        host_device_id: &str,
        first: &[u8],
        ephemeral: Option<&[u8; 32]>,
    ) -> Result<Self, TrustError> {
        let prologue = prologue(host_device_id);
        let mut builder = builder(&prologue, local)?;
        if let Some(ephemeral) = ephemeral {
            builder = builder.fixed_ephemeral_key_for_testing_only(ephemeral);
        }
        let mut state = builder.build_responder().map_err(noise_err)?;
        let mut payload = vec![0u8; first.len()];
        let len = state.read_message(first, &mut payload).map_err(noise_err)?;
        let client_key: [u8; 32] = state
            .get_remote_static()
            .and_then(|key| key.try_into().ok())
            .ok_or_else(|| noise_err("no client static key"))?;
        let client_id = String::from_utf8(payload[..len].to_vec())
            .map_err(|_| noise_err("client id is not UTF-8"))?;
        Ok(Self {
            state,
            client_key,
            client_id,
        })
    }

    /// Answer the client and enter transport mode.
    pub fn accept(mut self) -> Result<(Vec<u8>, Session), TrustError> {
        let mut message = vec![0u8; 1024];
        let len = self
            .state
            .write_message(&[], &mut message)
            .map_err(noise_err)?;
        message.truncate(len);
        Ok((message, Session::new(self.state)?))
    }
}

/// An established session.
pub struct Session {
    transport: TransportState,
    inbox: Vec<u8>,
}

impl Session {
    fn new(state: HandshakeState) -> Result<Self, TrustError> {
        Ok(Self {
            transport: state.into_transport_mode().map_err(noise_err)?,
            inbox: Vec::new(),
        })
    }

    /// The peer's Noise static key.
    pub fn remote_key(&self) -> Option<[u8; 32]> {
        self.transport
            .get_remote_static()
            .and_then(|key| key.try_into().ok())
    }

    /// Encrypt one message into one or more fragments, in sending order.
    pub fn seal(&mut self, message: &[u8]) -> Result<Vec<Vec<u8>>, TrustError> {
        if message.len() > MAX_MESSAGE {
            return Err(noise_err("message too large"));
        }
        let chunks: Vec<&[u8]> = if message.is_empty() {
            vec![&[]]
        } else {
            message.chunks(FRAGMENT).collect()
        };
        let last = chunks.len() - 1;
        chunks
            .into_iter()
            .enumerate()
            .map(|(i, chunk)| {
                let mut fragment = vec![0u8; 1 + chunk.len() + 16];
                fragment[0] = if i == last { LAST } else { MORE };
                let len = self
                    .transport
                    .write_message(chunk, &mut fragment[1..])
                    .map_err(noise_err)?;
                fragment.truncate(1 + len);
                Ok(fragment)
            })
            .collect()
    }

    /// Decrypt one fragment; `Some` once it completes a message. Any failure
    /// leaves the session unusable — the caller must drop the connection.
    pub fn open(&mut self, fragment: &[u8]) -> Result<Option<Vec<u8>>, TrustError> {
        let (&flag, ciphertext) = fragment
            .split_first()
            .ok_or_else(|| noise_err("empty fragment"))?;
        if flag != MORE && flag != LAST {
            return Err(noise_err("bad fragment flag"));
        }
        let start = self.inbox.len();
        if start + ciphertext.len() > MAX_MESSAGE + 16 {
            return Err(noise_err("message too large"));
        }
        self.inbox.resize(start + ciphertext.len(), 0);
        let len = self
            .transport
            .read_message(ciphertext, &mut self.inbox[start..])
            .map_err(noise_err)?;
        self.inbox.truncate(start + len);
        if flag == MORE {
            return Ok(None);
        }
        Ok(Some(std::mem::take(&mut self.inbox)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (DeviceKeys, DeviceKeys) {
        (DeviceKeys::generate("client"), DeviceKeys::generate("host"))
    }

    fn connect(client: &DeviceKeys, host: &DeviceKeys) -> (Session, Session) {
        let (initiator, first) = Initiator::start(client, "host", &host.dh_public()).unwrap();
        let accepting = Accepting::read(host, "host", &first).unwrap();
        assert_eq!(accepting.client_key, client.dh_public());
        assert_eq!(accepting.client_id, "client");
        let (reply, host_session) = accepting.accept().unwrap();
        (initiator.finish(&reply).unwrap(), host_session)
    }

    #[test]
    fn messages_of_any_size_cross_in_both_directions() {
        let (client, host) = pair();
        let (mut c, mut h) = connect(&client, &host);
        assert_eq!(h.remote_key(), Some(client.dh_public()));
        for size in [0, 1, FRAGMENT - 1, FRAGMENT, FRAGMENT + 1, 3 * FRAGMENT + 7] {
            let message: Vec<u8> = (0..size).map(|i| i as u8).collect();
            let fragments = c.seal(&message).unwrap();
            assert_eq!(fragments.len(), size.div_ceil(FRAGMENT).max(1));
            let mut opened = None;
            for fragment in &fragments {
                opened = h.open(fragment).unwrap();
            }
            assert_eq!(opened.as_deref(), Some(&message[..]));
            let back = h.seal(&message).unwrap();
            let mut opened = None;
            for fragment in &back {
                opened = c.open(fragment).unwrap();
            }
            assert_eq!(opened, Some(message));
        }
    }

    #[test]
    fn a_wrong_host_key_or_device_fails_the_handshake() {
        let (client, host) = pair();
        let impostor = DeviceKeys::generate("host");
        let (_, first) = Initiator::start(&client, "host", &host.dh_public()).unwrap();
        assert!(Accepting::read(&impostor, "host", &first).is_err());
        // The same keys under another device id: the prologue differs.
        assert!(Accepting::read(&host, "other", &first).is_err());

        // A host answering with the wrong key is caught by the client.
        let (initiator, first) = Initiator::start(&client, "host", &host.dh_public()).unwrap();
        let accepting = Accepting::read(&host, "host", &first).unwrap();
        let (reply, _) = accepting.accept().unwrap();
        let mut tampered = reply.clone();
        tampered[40] ^= 1;
        assert!(initiator.finish(&tampered).is_err());
    }

    #[test]
    fn tampered_replayed_or_reordered_fragments_are_refused() {
        let (client, host) = pair();

        let (mut c, mut h) = connect(&client, &host);
        let mut tampered = c.seal(b"one").unwrap().remove(0);
        tampered[5] ^= 1;
        assert!(h.open(&tampered).is_err(), "tampered");

        let (mut c, mut h) = connect(&client, &host);
        let _first = c.seal(b"one").unwrap();
        let second = c.seal(b"two").unwrap().remove(0);
        assert!(h.open(&second).is_err(), "out of order");

        let (mut c, mut h) = connect(&client, &host);
        let first = c.seal(b"one").unwrap().remove(0);
        h.open(&first).unwrap();
        assert!(h.open(&first).is_err(), "replay");
    }
}
