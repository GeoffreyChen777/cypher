//! Signed launch records (`cypher_doc::LaunchRecord`): which device hosts a
//! chat, in which folder, and the settings its runs launch with.
//!
//! The chat's workspace row says the same, but any signed-in device can edit
//! rows — a receive-only device included. Under "Only paired devices" a host
//! takes these from records signed by itself or a member with full control:
//! the latest trusted `host` record decides which device drains the chat, and
//! the latest trusted `config` record the permission level and model. A chat
//! with no trusted record yet (from before records, or started on a phone
//! that hasn't sent yet) falls back to its row, with Full access capped at
//! workspace-write.
//!
//! Records are written only for an explicit change: creating a chat,
//! changing its settings or folder, moving it to another device. A sender
//! writes the ones a chat lacks from what it shows, and a host records
//! itself for a chat it hosts that has no host record yet.

use cypher_doc::{LaunchFact, latest_launch};
use cypher_proto::{ChatConfig, SandboxLevel};

use super::*;
use crate::host::workspace_host::LaunchHook;

/// A chat's launch facts, from the records this device trusts.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TrustedLaunch {
    /// The hosting device, and the folder when the record names one.
    pub host: Option<(String, Option<String>)>,
    pub config: Option<ChatConfig>,
}

/// A row's config as a strict host runs it without a trusted record.
pub(crate) fn capped(mut config: ChatConfig) -> ChatConfig {
    if config.sandbox == SandboxLevel::DangerFullAccess {
        config.sandbox = SandboxLevel::WorkspaceWrite;
    }
    config
}

impl DocHost {
    /// The launch facts trusted records establish for `chat_id`; `None`
    /// unless "Only paired devices" is on (rows decide then).
    pub(crate) fn trusted_launch(&self, chat_id: &str) -> Option<TrustedLaunch> {
        let trust = self.command_trust()?;
        if !trust.strict() {
            return None;
        }
        let handle = self.open(chat_id).ok()?;
        let trusted = handle
            .doc
            .read_launch_records(chat_id)
            .into_iter()
            .filter(|record| trust.trusts_record(&record.signer, &record.auth));
        let (host, config) = latest_launch(trusted);
        Some(TrustedLaunch {
            host: host.and_then(|record| match record.fact {
                LaunchFact::Host { device_id, cwd } => Some((device_id, cwd)),
                LaunchFact::Config(_) => None,
            }),
            config: config.and_then(|record| match record.fact {
                LaunchFact::Config(config) => Some(config),
                LaunchFact::Host { .. } => None,
            }),
        })
    }

    /// The device trusted records say hosts `chat_id`, when they say.
    pub fn trusted_host(&self, chat_id: &str) -> Option<String> {
        self.trusted_launch(chat_id)?.host.map(|(host, _)| host)
    }

    /// The hook `WorkspaceHost::launch_config` runs through. Weak: the
    /// workspace host outlives no doc host, and must not keep one alive.
    pub fn launch_hook(&self) -> LaunchHook {
        let weak = Arc::downgrade(&self.inner);
        Arc::new(move |chat_id, row| {
            let Some(inner) = weak.upgrade() else {
                return row;
            };
            match (DocHost { inner }).trusted_launch(chat_id) {
                None => row,
                Some(TrustedLaunch {
                    config: Some(config),
                    ..
                }) => Some(config),
                Some(_) => row.map(capped),
            }
        })
    }

    /// Sign and append a record of `fact`; `false` when this device can't
    /// sign (no device trust, or not paired).
    pub fn record_launch(&self, chat_id: &str, fact: &LaunchFact) -> Result<bool, EngineError> {
        let Some(trust) = self.command_trust() else {
            return Ok(false);
        };
        if crate::rpc::is_relayed() {
            return Ok(false);
        }
        let handle = self.open(chat_id)?;
        Ok(handle.doc.append_launch_record(
            chat_id,
            fact,
            now_ms(),
            &self.inner.config.device_id,
            |bytes| trust.sign(bytes),
        )?)
    }

    /// [`Self::record_launch`], unless a record of the same kind this device
    /// trusts is already there: what a sender or host fills in, never what
    /// overrides someone's explicit change.
    pub fn record_launch_if_missing(
        &self,
        chat_id: &str,
        fact: &LaunchFact,
    ) -> Result<bool, EngineError> {
        let Some(trust) = self.command_trust() else {
            return Ok(false);
        };
        let handle = self.open(chat_id)?;
        let present = handle
            .doc
            .read_launch_records(chat_id)
            .iter()
            .any(|record| {
                same_kind(&record.fact, fact) && trust.trusts_record(&record.signer, &record.auth)
            });
        if present {
            return Ok(false);
        }
        self.record_launch(chat_id, fact)
    }

    /// Record the chat's host (and folder) and its settings as its row has
    /// them now — the host as trusted records name it when they do, so a
    /// folder change doesn't take the row's host along. `if_missing` fills
    /// in only what the chat lacks: what a sender does before queueing a
    /// command, from what it shows. Otherwise it's an explicit change made
    /// here (creating the chat, new settings or folder).
    pub fn record_chat_launch(&self, chat_id: &str, host: bool, config: bool, if_missing: bool) {
        if crate::rpc::is_relayed() {
            return;
        }
        let Some(chat) = self
            .workspace()
            .and_then(|ws| ws.chat(chat_id).ok().flatten())
        else {
            return;
        };
        let mut facts = Vec::new();
        if host {
            facts.push(LaunchFact::Host {
                device_id: self.trusted_host(chat_id).unwrap_or(chat.device_id),
                cwd: chat.cwd,
            });
        }
        if config && let Some(config) = chat.config {
            facts.push(LaunchFact::Config(config));
        }
        for fact in &facts {
            let written = if if_missing {
                self.record_launch_if_missing(chat_id, fact)
            } else {
                self.record_launch(chat_id, fact)
            };
            if let Err(err) = written {
                tracing::warn!(chat = %chat_id, error = %err, "launch record write failed");
            }
        }
    }

    /// A host about to run a command in a chat with no trusted host record
    /// records itself (a chat from before records): from then on, editing
    /// the row doesn't move it.
    pub(super) fn record_hosting(&self, chat_id: &str) {
        let cwd = self
            .workspace()
            .and_then(|ws| ws.chat(chat_id).ok().flatten())
            .and_then(|chat| chat.cwd);
        let fact = LaunchFact::Host {
            device_id: self.inner.config.device_id.clone(),
            cwd,
        };
        if let Err(err) = self.record_launch_if_missing(chat_id, &fact) {
            tracing::warn!(chat = %chat_id, error = %err, "launch record write failed");
        }
    }
}

fn same_kind(a: &LaunchFact, b: &LaunchFact) -> bool {
    matches!(
        (a, b),
        (LaunchFact::Host { .. }, LaunchFact::Host { .. })
            | (LaunchFact::Config(_), LaunchFact::Config(_))
    )
}
