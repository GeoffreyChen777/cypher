//! Signed launch records: which device hosts a chat (and in which folder),
//! and the settings its runs launch with (permission level, model). Both also
//! live on the chat's workspace row, which any signed-in device can edit; a
//! record is signed by the device that made the change, so a host that only
//! accepts paired devices can take them from someone it trusts (the engine's
//! `doc_host`).
//!
//! Records sit in the chat doc's `launch` list and are signed like commands
//! ([`command_signing_bytes`] over every field but `sig`). Their `kind` —
//! `host` or `config` — is no command's kind, so neither can pass for the
//! other.

use cypher_proto::ChatConfig;
use loro::{LoroMap, LoroValue, ToJson};

use super::{SessionDoc, loro_value_from_json};
use crate::DocError;
use crate::command_auth::{CommandAuth, CommandSig, SIG_FIELD, command_signing_bytes, encode_sig};

const LIST: &str = "launch";

/// What a launch record says.
#[derive(Debug, Clone, PartialEq)]
pub enum LaunchFact {
    /// The chat runs on `device_id`, in `cwd` when given.
    Host {
        device_id: String,
        cwd: Option<String>,
    },
    /// The chat's runs launch with `config`.
    Config(ChatConfig),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaunchRecord {
    pub fact: LaunchFact,
    /// Epoch millis, as claimed by the signer. The latest trusted record of a
    /// kind wins.
    pub at: i64,
    /// The device that signed it.
    pub signer: String,
    pub auth: CommandAuth,
}

impl SessionDoc {
    /// Append a record of `fact` signed by `signer` through `sign`; `false`
    /// (nothing written) when `sign` has no key to sign with.
    pub fn append_launch_record(
        &self,
        chat_id: &str,
        fact: &LaunchFact,
        at: i64,
        signer: &str,
        sign: impl FnOnce(&[u8]) -> Option<[u8; 64]>,
    ) -> Result<bool, DocError> {
        let mut fields: Vec<(&str, LoroValue)> = vec![("at", at.into()), ("signer", signer.into())];
        match fact {
            LaunchFact::Host { device_id, cwd } => {
                fields.push(("kind", "host".into()));
                fields.push(("host", device_id.as_str().into()));
                if let Some(cwd) = cwd {
                    fields.push(("cwd", cwd.as_str().into()));
                }
            }
            LaunchFact::Config(config) => {
                fields.push(("kind", "config".into()));
                fields.push((
                    "config",
                    loro_value_from_json(&serde_json::to_value(config)?),
                ));
            }
        }
        let Some(sig) = sign(&command_signing_bytes(
            chat_id,
            fields.iter().map(|(key, value)| (*key, value)),
        )) else {
            return Ok(false);
        };
        let map = self.doc.get_list(LIST).push_container(LoroMap::new())?;
        for (key, value) in fields {
            map.insert(key, value)?;
        }
        map.insert(SIG_FIELD, encode_sig(&sig))?;
        self.doc.commit();
        Ok(true)
    }

    /// Every well-formed launch record, with what its signature covers for
    /// `chat_id`. Whether a signer is trusted is the caller's decision.
    pub fn read_launch_records(&self, chat_id: &str) -> Vec<LaunchRecord> {
        let LoroValue::List(items) = self.doc.get_list(LIST).get_deep_value() else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| {
                let LoroValue::Map(fields) = item else {
                    return None;
                };
                let string = |key: &str| match fields.get(key) {
                    Some(LoroValue::String(s)) => Some(s.to_string()),
                    _ => None,
                };
                let at = match fields.get("at") {
                    Some(LoroValue::I64(at)) => *at,
                    _ => return None,
                };
                let signer = string("signer")?;
                let fact = match string("kind")?.as_str() {
                    "host" => LaunchFact::Host {
                        device_id: string("host")?,
                        cwd: string("cwd"),
                    },
                    "config" => LaunchFact::Config(
                        serde_json::from_value(fields.get("config")?.to_json_value()).ok()?,
                    ),
                    _ => return None,
                };
                let auth = CommandAuth {
                    signing_bytes: command_signing_bytes(
                        chat_id,
                        fields.iter().map(|(key, value)| (key.as_str(), value)),
                    ),
                    sig: CommandSig::parse(fields.get(SIG_FIELD)),
                };
                Some(LaunchRecord {
                    fact,
                    at,
                    signer,
                    auth,
                })
            })
            .collect()
    }
}

/// The latest record of each kind among `records` (already filtered to the
/// trusted ones): the host and the config. Ties go to the larger signer id,
/// so every replica picks the same one.
pub fn latest_launch(
    records: impl IntoIterator<Item = LaunchRecord>,
) -> (Option<LaunchRecord>, Option<LaunchRecord>) {
    let mut host: Option<LaunchRecord> = None;
    let mut config: Option<LaunchRecord> = None;
    for record in records {
        let slot = match record.fact {
            LaunchFact::Host { .. } => &mut host,
            LaunchFact::Config(_) => &mut config,
        };
        let newer = slot
            .as_ref()
            .is_none_or(|kept| (record.at, &record.signer) > (kept.at, &kept.signer));
        if newer {
            *slot = Some(record);
        }
    }
    (host, config)
}
