//! This device's place in the user's trust group (`cypher-trust`): its keys,
//! the group's chain kept in step with the edge, pairing in both roles, and
//! the [`PeerTrust`] the device relay consults.
//!
//! - Keys live per device (`{device_root}/device-keys.json`, a
//!   [`cypher_secrets::Secret`]); the chain lives per signed-in user
//!   (`{device_root}/trust/<user>/`).
//! - "Only paired devices" is a policy entry in the chain, so it holds for
//!   every member at once: a single member left open would otherwise relay
//!   for an unpaired device into the members that refuse it.
//! - The first device to find the user's group empty founds it. Every other
//!   device joins with a code shown on a member ([`DeviceTrust::start_invite`]
//!   there, [`DeviceTrust::join`] here).
//! - The chain only moves forward: a server copy that rewrites an entry this
//!   device holds is reported as a conflict, never adopted.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cypher_rpc::{PeerTrust, TokenSource};
use cypher_trust::{
    Comparison, DeviceKeys, DevicePublic, JoinRequest, PairingCode, TrustChain, TrustEntry,
    Welcome, now_ms,
};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::util::lock;

/// How long a shown code stays valid.
const INVITE_TTL: Duration = Duration::from_secs(10 * 60);
/// How long a joining device waits for the member to approve it.
const JOIN_TTL: Duration = Duration::from_secs(15 * 60);
const POLL: Duration = Duration::from_secs(2);
/// Background re-sync cadence.
const RESYNC: Duration = Duration::from_secs(10 * 60);
/// A client the host doesn't recognise triggers at most one refresh per this.
const MISS_REFRESH: Duration = Duration::from_secs(15);

/// What the engine asks of the edge's TrustRoom. [`HttpEdge`] is the real
/// one; tests substitute an in-memory edge.
#[async_trait]
pub trait TrustEdge: Send + Sync + 'static {
    async fn chain(&self) -> Result<Vec<TrustEntry>, String>;
    async fn append(&self, entry: &TrustEntry) -> Result<Append, String>;
    async fn put(&self, mailbox: &str, slot: &str, body: serde_json::Value) -> Result<(), String>;
    async fn get(&self, mailbox: &str, slot: &str) -> Result<Option<serde_json::Value>, String>;
    async fn delete(&self, mailbox: &str) -> Result<(), String>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Append {
    Done,
    /// Someone appended first; re-sync and retry.
    Conflict,
    Invalid(String),
}

/// The edge over HTTP (`/trust/*`).
pub struct HttpEdge {
    base: String,
    token: Arc<dyn TokenSource>,
    http: reqwest::Client,
}

impl HttpEdge {
    pub fn new(edge_url: &str, token: Arc<dyn TokenSource>) -> Self {
        Self {
            base: format!("{}/trust", edge_url.trim_end_matches('/')),
            token,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, String> {
        let token = self.token.token().await.ok_or("not signed in")?;
        Ok(self
            .http
            .request(method, format!("{}/{path}", self.base))
            .bearer_auth(token))
    }
}

#[async_trait]
impl TrustEdge for HttpEdge {
    async fn chain(&self) -> Result<Vec<TrustEntry>, String> {
        #[derive(Deserialize)]
        struct Chain {
            entries: Vec<TrustEntry>,
        }
        let response = self
            .request(reqwest::Method::GET, "chain")
            .await?
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        Ok(response
            .json::<Chain>()
            .await
            .map_err(|e| e.to_string())?
            .entries)
    }

    async fn append(&self, entry: &TrustEntry) -> Result<Append, String> {
        let response = self
            .request(reqwest::Method::POST, "chain")
            .await?
            .json(&serde_json::json!({ "entry": entry }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        match response.status().as_u16() {
            200 => Ok(Append::Done),
            409 => Ok(Append::Conflict),
            400 => {
                let body: serde_json::Value = response.json().await.unwrap_or_default();
                Ok(Append::Invalid(
                    body["reason"].as_str().unwrap_or("refused").to_string(),
                ))
            }
            status => Err(format!("trust append failed (HTTP {status})")),
        }
    }

    async fn put(&self, mailbox: &str, slot: &str, body: serde_json::Value) -> Result<(), String> {
        self.request(reqwest::Method::PUT, &format!("pairing/{mailbox}/{slot}"))
            .await?
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn get(&self, mailbox: &str, slot: &str) -> Result<Option<serde_json::Value>, String> {
        let response = self
            .request(reqwest::Method::GET, &format!("pairing/{mailbox}/{slot}"))
            .await?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = response.error_for_status().map_err(|e| e.to_string())?;
        response.json().await.map(Some).map_err(|e| e.to_string())
    }

    async fn delete(&self, mailbox: &str) -> Result<(), String> {
        self.request(reqwest::Method::DELETE, &format!("pairing/{mailbox}"))
            .await?
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// One device as Settings → Devices shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustMember {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    pub is_self: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrustState {
    /// Not synced with the edge yet (offline, or signed out).
    Unknown,
    /// The user has a group and this device is not in it.
    Unpaired,
    Paired,
    /// This device's keys were removed from the group.
    Removed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "role")]
pub enum PairingStatus {
    /// This device showed a code; `request` is the device that entered it.
    Inviting {
        code: String,
        expires_at: u64,
        request: Option<TrustMember>,
    },
    /// This device entered a code and waits for the member's approval.
    Joining { error: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustStatus {
    pub device_id: String,
    pub fingerprint: String,
    pub state: TrustState,
    pub members: Vec<TrustMember>,
    pub require_paired: bool,
    pub pairing: Option<PairingStatus>,
    /// The server's copy rewrote history this device holds.
    pub conflict: bool,
    pub error: Option<String>,
}

enum Pairing {
    Inviting {
        code: PairingCode,
        expires: Instant,
        expires_at: u64,
        request: Option<DevicePublic>,
    },
    Joining {
        error: Option<String>,
    },
}

#[derive(Default)]
struct State {
    chain: TrustChain,
    synced: bool,
    group_exists: bool,
    conflict: bool,
    error: Option<String>,
    pairing: Option<Pairing>,
}

struct Inner {
    device_id: String,
    device_name: String,
    keys: Mutex<Arc<DeviceKeys>>,
    keys_secret: cypher_secrets::Secret,
    dir: PathBuf,
    edge: Arc<dyn TrustEdge>,
    state: Mutex<State>,
    changes: watch::Sender<u64>,
    sync_lock: tokio::sync::Mutex<()>,
    last_miss_refresh: Mutex<Option<Instant>>,
    pairing_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    background: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct DeviceTrust {
    inner: Arc<Inner>,
}

fn chain_path(dir: &Path) -> PathBuf {
    dir.join("chain.json")
}

/// Directory-safe form of a user id.
fn user_dir(user_id: &str) -> String {
    user_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl DeviceTrust {
    /// Load (or create) this device's keys and the user's stored chain. Does
    /// not touch the network; see [`Self::spawn_sync`].
    pub fn open(
        device_root: &Path,
        user_id: &str,
        device_id: &str,
        device_name: &str,
        edge: Arc<dyn TrustEdge>,
    ) -> std::io::Result<Self> {
        let keys_secret = cypher_secrets::Secret::new(device_root.join("device-keys.json"));
        let keys = match keys_secret.read()? {
            Some(bytes) => match DeviceKeys::from_bytes(&bytes) {
                Ok(keys) if keys.device_id == device_id => Some(keys),
                Ok(_) => None,
                Err(err) => {
                    tracing::warn!(error = %err, "trust: device keys unreadable; making new ones");
                    None
                }
            },
            None => None,
        };
        let keys = match keys {
            Some(keys) => keys,
            None => {
                let keys = DeviceKeys::generate(device_id);
                keys_secret.write(&keys.to_bytes())?;
                keys
            }
        };
        let dir = device_root.join("trust").join(user_dir(user_id));
        std::fs::create_dir_all(&dir)?;
        let chain = std::fs::read(chain_path(&dir))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<TrustEntry>>(&bytes).ok())
            .map(TrustChain::verify)
            .transpose()
            .unwrap_or_else(|err| {
                tracing::warn!(error = %err, "trust: stored chain invalid; starting over");
                None
            })
            .unwrap_or_default();
        let (changes, _) = watch::channel(0);
        Ok(Self {
            inner: Arc::new(Inner {
                device_id: device_id.to_string(),
                device_name: device_name.to_string(),
                keys: Mutex::new(Arc::new(keys)),
                keys_secret,
                dir,
                edge,
                state: Mutex::new(State {
                    chain,
                    ..State::default()
                }),
                changes,
                sync_lock: tokio::sync::Mutex::new(()),
                last_miss_refresh: Mutex::new(None),
                pairing_task: Mutex::new(None),
                background: Mutex::new(None),
            }),
        })
    }

    /// Sync now, then every [`RESYNC`] and whenever credentials change.
    pub fn spawn_sync(&self, token: Option<Arc<dyn TokenSource>>) {
        let weak = Arc::downgrade(&self.inner);
        let mut token_changes = token.as_ref().and_then(|token| token.subscribe());
        let task = tokio::spawn(async move {
            let mut delay = Duration::from_secs(5);
            loop {
                let Some(inner) = weak.upgrade() else { return };
                let trust = DeviceTrust { inner };
                let ok = match trust.sync().await {
                    Ok(()) => true,
                    Err(err) => {
                        tracing::debug!(error = %err, "trust: sync failed");
                        false
                    }
                };
                drop(trust);
                let wait = if ok {
                    delay = Duration::from_secs(5);
                    RESYNC
                } else {
                    delay = (delay * 2).min(Duration::from_secs(300));
                    delay
                };
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = async {
                        match token_changes.as_mut() {
                            Some(changes) => { let _ = changes.changed().await; }
                            None => std::future::pending::<()>().await,
                        }
                    } => {}
                }
            }
        });
        if let Some(previous) = lock(&self.inner.background).replace(task) {
            previous.abort();
        }
    }

    pub fn shutdown(&self) {
        if let Some(task) = lock(&self.inner.background).take() {
            task.abort();
        }
        if let Some(task) = lock(&self.inner.pairing_task).take() {
            task.abort();
        }
    }

    pub fn watch(&self) -> watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    fn notify(&self) {
        self.inner.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn keys(&self) -> Arc<DeviceKeys> {
        lock(&self.inner.keys).clone()
    }

    fn public(&self) -> DevicePublic {
        self.keys().public(&self.inner.device_name)
    }

    fn is_paired_in(&self, chain: &TrustChain) -> bool {
        chain
            .member(&self.inner.device_id)
            .is_some_and(|member| member.dh_pub == self.keys().dh_public())
    }

    pub fn status(&self) -> TrustStatus {
        let keys = self.keys();
        let state = lock(&self.inner.state);
        let paired = self.is_paired_in(&state.chain);
        let trust_state = if paired {
            TrustState::Paired
        } else if !state.chain.is_empty() {
            TrustState::Removed
        } else if state.group_exists {
            TrustState::Unpaired
        } else {
            TrustState::Unknown
        };
        let member = |device: &DevicePublic| TrustMember {
            id: device.id.clone(),
            name: device.name.clone(),
            fingerprint: device.fingerprint(),
            is_self: device.id == self.inner.device_id,
        };
        TrustStatus {
            device_id: self.inner.device_id.clone(),
            fingerprint: keys.public(&self.inner.device_name).fingerprint(),
            state: trust_state,
            members: if paired {
                state.chain.members().map(member).collect()
            } else {
                Vec::new()
            },
            require_paired: paired && state.chain.require_paired(),
            pairing: state.pairing.as_ref().map(|pairing| match pairing {
                Pairing::Inviting {
                    code,
                    expires_at,
                    request,
                    ..
                } => PairingStatus::Inviting {
                    code: code.display(),
                    expires_at: *expires_at,
                    request: request.as_ref().map(member),
                },
                Pairing::Joining { error } => PairingStatus::Joining {
                    error: error.clone(),
                },
            }),
            conflict: state.conflict,
            error: state.error.clone(),
        }
    }

    fn adopt(&self, chain: TrustChain) {
        let bytes = serde_json::to_vec(chain.entries()).expect("entries serialize");
        if let Err(err) = cypher_secrets::write_private(&chain_path(&self.inner.dir), &bytes) {
            tracing::warn!(error = %err, "trust: failed to save the chain");
        }
        let mut state = lock(&self.inner.state);
        state.group_exists = true;
        state.conflict = false;
        state.chain = chain;
    }

    /// Bring this device's copy of the chain in step with the edge.
    pub async fn sync(&self) -> Result<(), String> {
        let _guard = self.inner.sync_lock.lock().await;
        let result = self.sync_locked().await;
        {
            let mut state = lock(&self.inner.state);
            match &result {
                Ok(()) => {
                    state.synced = true;
                    state.error = None;
                }
                Err(err) => state.error = Some(err.clone()),
            }
        }
        self.notify();
        result
    }

    async fn sync_locked(&self) -> Result<(), String> {
        for _ in 0..3 {
            let remote = self.inner.edge.chain().await?;
            let local = lock(&self.inner.state).chain.clone();
            if !local.is_empty() {
                match local.compare(remote) {
                    Ok(Comparison::Same) => {}
                    Ok(Comparison::Ahead(chain)) => {
                        if self.is_paired_in(&local) && !self.is_paired_in(&chain) {
                            tracing::warn!("trust: this device was removed from the group");
                        }
                        self.adopt(chain);
                    }
                    Ok(Comparison::Behind) => self.republish(&local).await?,
                    Err(cypher_trust::TrustError::Diverged) => {
                        tracing::warn!(
                            "trust: the server's chain rewrites this device's copy; keeping ours"
                        );
                        lock(&self.inner.state).conflict = true;
                    }
                    Err(err) => return Err(err.to_string()),
                }
                return Ok(());
            }
            if remote.is_empty() {
                // Nobody has founded the group yet: this device does.
                let genesis = TrustChain::genesis(&self.keys(), &self.inner.device_name, now_ms())
                    .map_err(|e| e.to_string())?;
                match self.inner.edge.append(&genesis.entries()[0]).await? {
                    Append::Done => {
                        tracing::info!("trust: founded the device group");
                        self.adopt(genesis);
                        return Ok(());
                    }
                    Append::Conflict => continue,
                    Append::Invalid(reason) => return Err(reason),
                }
            }
            // The group exists. A device holding no copy can trust the
            // server's only through a pairing welcome — except its own record
            // in it, which it can recognise: its keys never leave it.
            match TrustChain::verify(remote) {
                Ok(chain) if self.is_paired_in(&chain) => self.adopt(chain),
                Ok(_) => lock(&self.inner.state).group_exists = true,
                Err(err) => return Err(err.to_string()),
            }
            return Ok(());
        }
        Err("the device group kept changing; try again".into())
    }

    /// Push the entries the server lacks, in order.
    async fn republish(&self, local: &TrustChain) -> Result<(), String> {
        let have = self.inner.edge.chain().await?.len();
        for entry in &local.entries()[have.min(local.entries().len())..] {
            match self.inner.edge.append(entry).await? {
                Append::Done => {}
                Append::Conflict => return Err("the device group changed; try again".into()),
                Append::Invalid(reason) => return Err(reason),
            }
        }
        Ok(())
    }

    /// Append the entry `make` builds on top of the current chain, re-syncing
    /// and rebuilding when another device appended first.
    async fn append(
        &self,
        make: impl Fn(&TrustChain, &DeviceKeys) -> Result<TrustEntry, cypher_trust::TrustError>,
    ) -> Result<TrustEntry, String> {
        for _ in 0..3 {
            self.sync().await?;
            let chain = lock(&self.inner.state).chain.clone();
            if !self.is_paired_in(&chain) {
                return Err("This device isn't paired.".into());
            }
            let entry = make(&chain, &self.keys()).map_err(|e| e.to_string())?;
            match self.inner.edge.append(&entry).await? {
                Append::Done => {
                    let mut next = chain;
                    next.append(entry.clone()).map_err(|e| e.to_string())?;
                    self.adopt(next);
                    self.notify();
                    return Ok(entry);
                }
                Append::Conflict => continue,
                Append::Invalid(reason) => return Err(reason),
            }
        }
        Err("the device group kept changing; try again".into())
    }

    // -- pairing: the member's side -------------------------------------------

    /// Show a code for a new device to enter.
    pub async fn start_invite(&self) -> Result<TrustStatus, String> {
        if !self.is_paired_in(&lock(&self.inner.state).chain) {
            return Err("Pair this device before adding others.".into());
        }
        self.cancel_pairing().await;
        let code = PairingCode::generate();
        lock(&self.inner.state).pairing = Some(Pairing::Inviting {
            code: code.clone(),
            expires: Instant::now() + INVITE_TTL,
            expires_at: now_ms() + INVITE_TTL.as_millis() as u64,
            request: None,
        });
        let trust = self.clone();
        let task = tokio::spawn(async move { trust.wait_for_join(code).await });
        *lock(&self.inner.pairing_task) = Some(task);
        self.notify();
        Ok(self.status())
    }

    async fn wait_for_join(&self, code: PairingCode) {
        let mailbox = code.mailbox();
        let deadline = Instant::now() + INVITE_TTL;
        while Instant::now() < deadline {
            tokio::time::sleep(POLL).await;
            let Ok(Some(body)) = self.inner.edge.get(&mailbox, "join").await else {
                continue;
            };
            let Ok(request) = serde_json::from_value::<JoinRequest>(body) else {
                continue;
            };
            // Anyone can write to the mailbox; only the code's holder can MAC.
            if code.verify_join(&request).is_err() {
                tracing::warn!("trust: ignoring a join request with a bad MAC");
                continue;
            }
            if let Some(Pairing::Inviting { request: slot, .. }) =
                lock(&self.inner.state).pairing.as_mut()
            {
                *slot = Some(request.device);
            }
            self.notify();
            return;
        }
        self.expire_pairing("The code expired. Show a new one.");
    }

    fn expire_pairing(&self, message: &str) {
        let mut state = lock(&self.inner.state);
        if matches!(state.pairing, Some(Pairing::Inviting { .. })) {
            state.pairing = None;
            state.error = Some(message.into());
        }
        drop(state);
        self.notify();
    }

    /// Add the device that entered the code.
    pub async fn approve(&self) -> Result<TrustStatus, String> {
        let (code, device) = match &lock(&self.inner.state).pairing {
            Some(Pairing::Inviting {
                code,
                expires,
                request: Some(device),
                ..
            }) if Instant::now() < *expires => (code.clone(), device.clone()),
            _ => return Err("No device is waiting for approval.".into()),
        };
        let added = device.clone();
        let entry = self
            .append(move |chain, keys| chain.propose_add(keys, added.clone(), now_ms()))
            .await?;
        let welcome = code.welcome(entry.seq, entry.hash(), &device);
        self.inner
            .edge
            .put(
                &code.mailbox(),
                "welcome",
                serde_json::to_value(welcome).expect("welcome serializes"),
            )
            .await?;
        lock(&self.inner.state).pairing = None;
        tracing::info!(device = %device.id, "trust: added a device to the group");
        self.notify();
        Ok(self.status())
    }

    pub async fn cancel_pairing(&self) {
        if let Some(task) = lock(&self.inner.pairing_task).take() {
            task.abort();
        }
        let pairing = lock(&self.inner.state).pairing.take();
        if let Some(Pairing::Inviting { code, .. }) = pairing {
            let _ = self.inner.edge.delete(&code.mailbox()).await;
        }
        self.notify();
    }

    // -- pairing: the new device's side ----------------------------------------

    /// Ask to join with a code shown on a member.
    pub async fn join(&self, input: &str) -> Result<TrustStatus, String> {
        let code = PairingCode::parse(input).map_err(|_| {
            "That code isn't valid. It has 20 letters and digits, like ABCD-EFGH-JKMN-PQRS-TVWX."
                .to_string()
        })?;
        if self.is_paired_in(&lock(&self.inner.state).chain) {
            return Err("This device is already paired.".into());
        }
        self.cancel_pairing().await;
        // Keys the group has seen before (this device was removed) cannot be
        // added again: make new ones.
        let seen = self.inner.edge.chain().await?;
        let keys = self.keys();
        if seen.iter().any(|entry| {
            entry.device.sign_pub == keys.public("").sign_pub
                || entry.device.dh_pub == keys.dh_public()
        }) {
            self.rekey()?;
        }
        let public = self.public();
        self.inner
            .edge
            .put(
                &code.mailbox(),
                "join",
                serde_json::to_value(code.join(&public)).expect("join serializes"),
            )
            .await?;
        lock(&self.inner.state).pairing = Some(Pairing::Joining { error: None });
        let trust = self.clone();
        let task = tokio::spawn(async move {
            let outcome = trust.wait_for_welcome(&code, &public).await;
            let _ = trust.inner.edge.delete(&code.mailbox()).await;
            let mut state = lock(&trust.inner.state);
            match outcome {
                Ok(()) => state.pairing = None,
                Err(err) => {
                    tracing::warn!(error = %err, "trust: joining failed");
                    state.pairing = Some(Pairing::Joining { error: Some(err) });
                }
            }
            drop(state);
            trust.notify();
        });
        *lock(&self.inner.pairing_task) = Some(task);
        self.notify();
        Ok(self.status())
    }

    async fn wait_for_welcome(
        &self,
        code: &PairingCode,
        public: &DevicePublic,
    ) -> Result<(), String> {
        let deadline = Instant::now() + JOIN_TTL;
        let welcome = loop {
            if Instant::now() >= deadline {
                return Err("No approval arrived. Show a new code on your other device.".into());
            }
            tokio::time::sleep(POLL).await;
            if let Ok(Some(body)) = self.inner.edge.get(&code.mailbox(), "welcome").await
                && let Ok(welcome) = serde_json::from_value::<Welcome>(body)
            {
                break welcome;
            }
        };
        code.verify_welcome(&welcome, public).map_err(|_| {
            "The approval failed verification. Pair again with a new code.".to_string()
        })?;
        let remote = self.inner.edge.chain().await?;
        let chain = TrustChain::verify(remote).map_err(|e| e.to_string())?;
        // The welcome vouches for the log up to the entry adding this device;
        // the rest is signed by members of that log.
        let adds_us = chain
            .prefix(welcome.seq)
            .and_then(|prefix| prefix.last())
            .is_some_and(|entry| {
                entry.hash() == welcome.entry
                    && entry.kind == cypher_trust::EntryKind::Add
                    && &entry.device == public
            });
        if !adds_us {
            return Err("The server's device list doesn't match the approval.".into());
        }
        if !self.is_paired_in(&chain) {
            return Err("This device was removed right after being added.".into());
        }
        self.adopt(chain);
        tracing::info!("trust: joined the device group");
        Ok(())
    }

    fn rekey(&self) -> Result<(), String> {
        let keys = DeviceKeys::generate(&self.inner.device_id);
        self.inner
            .keys_secret
            .write(&keys.to_bytes())
            .map_err(|e| format!("could not save new device keys: {e}"))?;
        *lock(&self.inner.keys) = Arc::new(keys);
        lock(&self.inner.state).chain = TrustChain::default();
        let _ = std::fs::remove_file(chain_path(&self.inner.dir));
        tracing::info!("trust: made new device keys");
        Ok(())
    }

    // -- membership and settings -----------------------------------------------

    pub async fn remove(&self, device_id: &str) -> Result<TrustStatus, String> {
        let target = device_id.to_string();
        self.append(move |chain, keys| chain.propose_remove(keys, &target, now_ms()))
            .await?;
        Ok(self.status())
    }

    /// Set the group's "Only paired devices" policy, for every member.
    pub async fn set_require_paired(&self, enabled: bool) -> Result<TrustStatus, String> {
        if !self.is_paired_in(&lock(&self.inner.state).chain) {
            return Err("Pair this device first.".into());
        }
        self.append(move |chain, keys| chain.propose_policy(keys, enabled, now_ms()))
            .await?;
        Ok(self.status())
    }

    /// Re-sync after a miss, at most once per [`MISS_REFRESH`].
    async fn refresh_after_miss(&self) {
        {
            let mut last = lock(&self.inner.last_miss_refresh);
            if last.is_some_and(|at| at.elapsed() < MISS_REFRESH) {
                return;
            }
            *last = Some(Instant::now());
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), self.sync()).await;
    }
}

#[async_trait]
impl PeerTrust for DeviceTrust {
    fn local_keys(&self) -> Option<Arc<DeviceKeys>> {
        Some(self.keys())
    }

    fn peer_key(&self, device_id: &str) -> Option<[u8; 32]> {
        let state = lock(&self.inner.state);
        if !self.is_paired_in(&state.chain) {
            return None;
        }
        state.chain.member(device_id).map(|member| member.dh_pub)
    }

    async fn is_member(&self, client_id: &str, client_key: &[u8; 32]) -> bool {
        let check = |trust: &DeviceTrust| {
            let state = lock(&trust.inner.state);
            trust.is_paired_in(&state.chain)
                && state
                    .chain
                    .member(client_id)
                    .is_some_and(|member| &member.dh_pub == client_key)
        };
        if check(self) {
            return true;
        }
        self.refresh_after_miss().await;
        check(self)
    }

    fn require_paired(&self) -> bool {
        lock(&self.inner.state).chain.require_paired()
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(task) = lock(&self.background).take() {
            task.abort();
        }
        if let Some(task) = lock(&self.pairing_task).take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests;
