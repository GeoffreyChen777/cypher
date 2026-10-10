//! EngineRpc — the engine-side `RpcService`: sessions + docs + the workspace-doc
//! entity surface.
//!
//! The method catalogue is `cypher_rpc::methods`; `EngineRpc`'s
//! `RpcService::handle` dispatches each one.
//!
//! ## Device-addressed routing (`targetDeviceId`)
//!
//! ControlRpc methods are relay-forwardable: params may carry `targetDeviceId`. When it
//! names another device, the call is forwarded verbatim over that device's relay DO via
//! the [`LinkCache`] — the remote engine sees its own id and handles locally, so the
//! forward can never loop. Streaming methods are proxied by re-subscribing remotely and
//! piping items. To make another method device-addressable, nothing per-method is needed
//! beyond listing it in [`forwardable`] (and [`is_stream_method`] if it streams);
//! handlers stay transport-agnostic.

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::watch;

use cypher_doc::{MessagePart, SessionCommandPayload, SessionCommandStatus};
use cypher_proto::{
    ChildAgentProfile, EngineInfo, HarnessId, RunRequest, SessionForkRequest, ToolCall,
    WorkspaceScope,
};
use cypher_rpc::{LinkCache, RpcError, RpcReply, RpcService, methods, parse_params};

use crate::auth::Auth;
use crate::git::diff_sync::CheckoutDiffSync;
use crate::git::repos::{Repos, expand_home};
use crate::host::doc_host::DocHost;
use crate::host::workspace_host::WorkspaceHost;
use crate::registry::HarnessRegistry;
use crate::session::engine::SessionsEngine;
use crate::session::forks::SessionForks;
use crate::session::side_chats::SideChats;
use crate::terminals::Terminals;
use crate::uploads::Uploads;

mod chats;
mod diffs;
mod docs;
mod files;
mod github;
mod harnesses;
mod mcp;
mod mutate;
mod params;
mod pi;
mod repos;
mod settings;
mod terminals;
mod updates;
mod uploads;

use params::*;

fn failed(e: impl std::fmt::Display) -> RpcError {
    RpcError::Failed(e.to_string())
}

/// Drop the routing field before a strict parse: the forwarder has consumed it,
/// and an explicit local target still carries it.
fn strip_target(mut params: serde_json::Value) -> serde_json::Value {
    if let Some(object) = params.as_object_mut() {
        object.remove("targetDeviceId");
    }
    params
}

pub struct EngineRpc {
    sessions: SessionsEngine,
    doc_host: DocHost,
    workspace: WorkspaceHost,
    registry: std::sync::Arc<HarnessRegistry>,
    repos: Repos,
    terminals: Terminals,
    diff_sync: CheckoutDiffSync,
    uploads: Uploads,
    side_chats: SideChats,
    session_forks: SessionForks,
    auth: Option<Auth>,
    links: Option<std::sync::Arc<LinkCache>>,
    updater: Option<cypher_update::Updater>,
    pi_runtime: Option<crate::pi::runtime::PiRuntimeManager>,
    mcp_logins: std::sync::Arc<crate::mcp::login::Logins>,
    provider_logins: std::sync::Arc<crate::pi::providers::Logins>,
    local_import: Option<crate::host::local_import::LocalImporter>,
    title_settings: Option<crate::session::title_settings::TitleSettingsStore>,
    github: Option<crate::git::github::Github>,
    engine_info: EngineInfo,
    /// Serializes `StartSubagent` (create-child scan → row → initial-run queue)
    /// so concurrent starts of the same `(parentChatId, runId)` cannot race the
    /// read-then-create scan and mint twins or double-queue the initial run.
    start_subagent_lock: Mutex<()>,
}

impl EngineRpc {
    pub fn with_mcp_logins(mut self, logins: std::sync::Arc<crate::mcp::login::Logins>) -> Self {
        self.mcp_logins = logins;
        self
    }
    pub fn with_provider_logins(
        mut self,
        logins: std::sync::Arc<crate::pi::providers::Logins>,
    ) -> Self {
        self.provider_logins = logins;
        self
    }
    #[allow(clippy::too_many_arguments)] // engine assembly seam, not a public API
    pub fn new(
        sessions: SessionsEngine,
        doc_host: DocHost,
        workspace: WorkspaceHost,
        registry: std::sync::Arc<HarnessRegistry>,
        repos: Repos,
        terminals: Terminals,
        diff_sync: CheckoutDiffSync,
        uploads: Uploads,
        side_chats: SideChats,
        session_forks: SessionForks,
        workspace_scope: WorkspaceScope,
    ) -> Self {
        let engine_info = EngineInfo {
            device_id: doc_host.device_id().to_string(),
            workspace_scope,
        };
        Self {
            sessions,
            doc_host,
            workspace,
            registry,
            repos,
            terminals,
            diff_sync,
            uploads,
            side_chats,
            session_forks,
            auth: None,
            links: None,
            updater: None,
            pi_runtime: None,
            mcp_logins: Default::default(),
            provider_logins: Default::default(),
            local_import: None,
            title_settings: None,
            github: None,
            engine_info,
            start_subagent_lock: Mutex::new(()),
        }
    }

    /// Share the device's title preferences with the automatic title runner.
    pub fn with_title_settings(
        mut self,
        settings: crate::session::title_settings::TitleSettingsStore,
    ) -> Self {
        self.title_settings = Some(settings);
        self
    }

    /// This device's GitHub sign-in and API access.
    pub fn with_github(mut self, github: crate::git::github::Github) -> Self {
        self.github = Some(github);
        self
    }

    fn github(&self) -> Result<&crate::git::github::Github, RpcError> {
        self.github
            .as_ref()
            .ok_or_else(|| RpcError::Failed("GitHub isn't available on this engine".into()))
    }

    /// Attach the auth service (AuthStatus + AuthRpc mutations).
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Attach the peer link cache — enables `targetDeviceId` relay forwarding.
    pub fn with_links(mut self, links: std::sync::Arc<LinkCache>) -> Self {
        self.links = Some(links);
        self
    }

    /// Attach the release checker (UpdateStatus stream + ApplyUpdate).
    pub fn with_updater(mut self, updater: cypher_update::Updater) -> Self {
        self.updater = Some(updater);
        self
    }

    pub fn with_pi_runtime(mut self, runtime: crate::pi::runtime::PiRuntimeManager) -> Self {
        self.pi_runtime = Some(runtime);
        self
    }

    /// Attach the local→synced profile importer (synced runtimes only).
    pub fn with_local_import(mut self, importer: crate::host::local_import::LocalImporter) -> Self {
        self.local_import = Some(importer);
        self
    }

    fn auth(&self) -> Result<&Auth, RpcError> {
        self.auth
            .as_ref()
            .ok_or_else(|| RpcError::Failed("auth unavailable".into()))
    }

    fn updater(&self) -> Result<&cypher_update::Updater, RpcError> {
        self.updater
            .as_ref()
            .ok_or_else(|| RpcError::Failed("updates unavailable".into()))
    }

    fn pi_runtime(&self) -> Result<&crate::pi::runtime::PiRuntimeManager, RpcError> {
        self.pi_runtime
            .as_ref()
            .ok_or_else(|| RpcError::Failed("Pi Runtime unavailable".into()))
    }

    /// Pi packages changed: rediscover slash commands/models and recycle
    /// parked sessions so the next turn loads the new settings.json.
    async fn reload_pi_runtime(&self) {
        self.registry.invalidate_discovery(HarnessId::Pi);
        self.sessions.recycle_idle_sessions().await;
    }

    /// Reject a selection naming a model outside THIS device's Pi catalog.
    async fn require_pi_catalog_models(
        &self,
        models: &[impl AsRef<str>],
        timeout: Duration,
        missing: &'static str,
    ) -> Result<(), RpcError> {
        let harness = self.registry.resolve(HarnessId::Pi).map_err(failed)?;
        let available = tokio::time::timeout(timeout, harness.models())
            .await
            .map_err(|_| RpcError::Failed("Model catalog timed out".into()))?
            .map_err(failed)?
            .into_iter()
            .map(|model| model.id)
            .collect::<HashSet<_>>();
        if models
            .iter()
            .any(|model| !available.contains(model.as_ref()))
        {
            return Err(RpcError::BadParams(missing.into()));
        }
        Ok(())
    }

    fn local_importer(&self) -> Result<&crate::host::local_import::LocalImporter, RpcError> {
        self.local_import
            .as_ref()
            .ok_or_else(|| RpcError::Failed("local import requires a synced workspace".into()))
    }

    /// Config changes the active MCP / provider sign-in would race.
    fn refuse_during_login(&self, method: &str) -> Result<(), RpcError> {
        if self.mcp_logins.active()
            && matches!(
                method,
                methods::ADD_MCP_SERVERS
                    | methods::REMOVE_MCP_SERVER
                    | methods::SET_MCP_SERVER_ENABLED
                    | methods::START_MCP_AUTH
                    | methods::LOGOUT_MCP_SERVER
            )
        {
            return Err(RpcError::Failed(
                "Finish or cancel the active MCP sign-in before changing MCP configuration.".into(),
            ));
        }
        if self.provider_logins.active()
            && matches!(
                method,
                methods::SAVE_PI_PROVIDER
                    | methods::LOGOUT_PI_PROVIDER
                    | methods::REMOVE_PI_PROVIDER
                    | methods::BEGIN_PI_PROVIDER_LOGIN
            )
        {
            return Err(RpcError::Failed(
                "Finish or cancel the active provider sign-in before changing providers.".into(),
            ));
        }
        Ok(())
    }

    /// Forward a device-addressed call over the target device's relay. On transport
    /// failure the cached link is invalidated so the next call re-dials.
    async fn forward(
        &self,
        target: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        let Some(links) = &self.links else {
            return Err(RpcError::Failed(format!(
                "cannot reach device {target}: remote routing unavailable (offline)"
            )));
        };
        if matches!(
            method,
            methods::SAVE_PI_PROVIDER
                | methods::ADD_MCP_SERVERS
                | methods::REMOVE_MCP_SERVER
                | methods::BEGIN_MCP_LOGIN
                | methods::MCP_LOGIN_STATUS
                | methods::COMPLETE_MCP_LOGIN
                | methods::CANCEL_MCP_LOGIN
                | methods::BEGIN_PI_PROVIDER_LOGIN
                | methods::PI_PROVIDER_LOGIN_STATUS
                | methods::COMPLETE_PI_PROVIDER_LOGIN
                | methods::CANCEL_PI_PROVIDER_LOGIN
        ) && !links.credential_transport_allowed()
        {
            return Err(RpcError::Failed(if method != methods::SAVE_PI_PROVIDER {
                "Remote MCP configuration requires an HTTPS/WSS relay (loopback development is allowed).".into()
            } else {
                "Remote provider credentials require an HTTPS/WSS relay (loopback development is allowed).".into()
            }));
        }
        let client = links.client(target).await?;
        if is_stream_method(method) {
            let rx = match client.subscribe(method, params).await {
                Ok(rx) => rx,
                Err(err) => {
                    links.invalidate(target);
                    return Err(err);
                }
            };
            // Pipe remote items; the held client keeps the link's RpcClient alive for
            // the stream's lifetime. A remote error just ends the stream (the relay
            // link-down path fails pending calls; stream receivers close).
            let stream = futures::stream::unfold((rx, client), |(mut rx, client)| async move {
                rx.recv().await.map(|item| (item, (rx, client)))
            });
            return Ok(RpcReply::Stream(stream.boxed()));
        }
        match client.call(method, params).await {
            Ok(value) => Ok(RpcReply::Value(value)),
            Err(err) => {
                if matches!(err, RpcError::Closed | RpcError::Transport(_)) {
                    links.invalidate(target);
                }
                Err(err)
            }
        }
    }
}

/// Checks that run before a call can be forwarded: a blank `targetDeviceId`, and
/// bodies that must never reach another device unvalidated.
fn preflight(method: &str, params: &serde_json::Value) -> Result<(), RpcError> {
    if forwardable(method)
        && let Some(target) = params.get("targetDeviceId")
        && !target.as_str().is_some_and(|id| !id.trim().is_empty())
    {
        return Err(RpcError::BadParams("Invalid target device.".into()));
    }
    if method == methods::SAVE_PI_PROVIDER {
        // Validate before forwarding, without echoing malformed credentials.
        let body = strip_target(params.clone());
        serde_json::from_value::<crate::pi::providers::SaveProvider>(body)
            .map_err(|_| RpcError::BadParams("Invalid provider settings.".into()))?;
    }
    if method == methods::ADD_MCP_SERVERS {
        if !params
            .get("targetDeviceId")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| !id.trim().is_empty())
        {
            return Err(RpcError::BadParams(
                "Select a target device before adding MCP servers.".into(),
            ));
        }
        let body = strip_target(params.clone());
        let request = serde_json::from_value::<crate::mcp::AddMcpServers>(body)
            .map_err(|_| RpcError::BadParams("Invalid MCP configuration.".into()))?;
        request.validate().map_err(RpcError::BadParams)?;
    }
    if method == methods::REMOVE_MCP_SERVER {
        if !params
            .get("targetDeviceId")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| !id.trim().is_empty())
        {
            return Err(RpcError::BadParams(
                "Select a target device before deleting an MCP server.".into(),
            ));
        }
        let body = strip_target(params.clone());
        let request = serde_json::from_value::<crate::mcp::RemoveMcpServer>(body)
            .map_err(|_| RpcError::BadParams("Invalid MCP deletion request.".into()))?;
        request.validate().map_err(RpcError::BadParams)?;
    }
    Ok(())
}

/// ControlRpc methods that honor `targetDeviceId`. Extend this
/// list (plus [`is_stream_method`] for streams) to make more of the surface
/// device-addressable — the handlers themselves need no changes.
fn forwardable(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_HARNESSES
            | methods::SET_HARNESS_ENABLED
            | methods::LIST_PI_PACKAGES
            | methods::INSTALL_PI
            | methods::INSTALL_PI_PACKAGE
            | methods::SET_PI_PACKAGE_ENABLED
            | methods::PI_RUNTIME_STATUS
            | methods::LIST_PI_SUBAGENTS
            | methods::SAVE_PI_SUBAGENT
            | methods::DELETE_PI_SUBAGENT
            | methods::GET_PI_TRANSLATION_SETTINGS
            | methods::SET_PI_TRANSLATION_SETTINGS
            | methods::DETECT_PI_LANGUAGE
            | methods::LIST_PI_PROVIDERS
            | methods::SAVE_PI_PROVIDER
            | methods::REFRESH_PI_PROVIDER
            | methods::LOGOUT_PI_PROVIDER
            | methods::REMOVE_PI_PROVIDER
            | methods::BEGIN_PI_PROVIDER_LOGIN
            | methods::PI_PROVIDER_LOGIN_STATUS
            | methods::COMPLETE_PI_PROVIDER_LOGIN
            | methods::CANCEL_PI_PROVIDER_LOGIN
            | methods::PI_UPDATE_STATUS
            | methods::CHECK_PI_UPDATE
            | methods::APPLY_PI_UPDATES
            | methods::LIST_MCP_SERVERS
            | methods::ADD_MCP_SERVERS
            | methods::REMOVE_MCP_SERVER
            | methods::SET_MCP_SERVER_ENABLED
            | methods::START_MCP_AUTH
            | methods::BEGIN_MCP_LOGIN
            | methods::MCP_LOGIN_STATUS
            | methods::COMPLETE_MCP_LOGIN
            | methods::CANCEL_MCP_LOGIN
            | methods::LOGOUT_MCP_SERVER
            | methods::LIST_MODELS
            | methods::GET_TITLE_MODEL_SETTINGS
            | methods::SET_TITLE_MODEL_SETTINGS
            | methods::GET_WEB_SEARCH_FALLBACK
            | methods::SET_WEB_SEARCH_FALLBACK
            | methods::LIST_COMMANDS
            // Read from the chat's Pi session, which lives on its host.
            | methods::PI_SESSION_MODES
            | methods::QUEUE_COMMAND
            | methods::RETRY_COMMAND
            | methods::WATCH_DOC_MESSAGES
            | methods::WATCH_DOC_COMMANDS
            // Repos/worktrees/folders are device-local filesystem state.
            | methods::LIST_REPOS
            | methods::ADD_REPO
            | methods::CLONE_REPO
            | methods::CREATE_REPO
            | methods::LIST_BRANCHES
            | methods::LIST_REFS
            | methods::LIST_GIT_HISTORY
            | methods::FETCH_ALL
            | methods::SWITCH_REF
            | methods::LIST_FOLDERS
            | methods::SEARCH_FILES
            | methods::SEARCH_GITHUB_ISSUES
            | methods::GET_GITHUB_ISSUE
            // GitHub logins are per-device, like agent CLI logins.
            | methods::GITHUB_ACCOUNT_STATUS
            | methods::START_GITHUB_LOGIN
            | methods::POLL_GITHUB_LOGIN
            | methods::CANCEL_GITHUB_LOGIN
            | methods::SIGN_OUT_GITHUB
            | methods::LIST_WORKSPACE_FILES
            | methods::READ_WORKSPACE_FILE
            | methods::WRITE_WORKSPACE_FILE
            | methods::CREATE_WORKTREE
            | methods::DELETE_WORKTREE
            | methods::CREATE_SCRATCH_DIR
            | methods::DELETE_SCRATCH_DIR
            // Checkout diffs are produced on the device holding the checkout.
            | methods::WATCH_CHECKOUT_DIFFS
            | methods::GET_CHECKOUT_DIFF
            | methods::GET_CHECKOUT_FILE_DIFF_TEXT
            // Terminals live on the chat's host device.
            | methods::OPEN_TERMINAL
            | methods::SUBSCRIBE_TERMINAL
            | methods::WRITE_TERMINAL
            | methods::RESIZE_TERMINAL
            | methods::CLOSE_TERMINAL
            // Uploads/attachments target the chat's host device (the agent reads
            // the committed file from that device's disk).
            | methods::UPLOAD_CHUNK
            | methods::UPLOAD_COMMIT
            | methods::READ_ATTACHMENT_CHUNK
            // Updates report/apply on the device whose binary they concern.
            | methods::UPDATE_STATUS
            | methods::CHECK_UPDATE
            | methods::UPDATE_ON_ACTIVATION
            | methods::APPLY_UPDATE
            // Side Chats are owned by the parent chat's host device.
            | methods::START_SIDE_CHAT
            | methods::SEND_SIDE_CHAT
            | methods::INTERRUPT_SIDE_CHAT
            | methods::RESPOND_SIDE_CHAT_INPUT
            | methods::WATCH_SIDE_CHAT_STATUS
            | methods::PROMOTE_SIDE_CHAT
            | methods::DISPOSE_SIDE_CHAT
            // Session Forks are owned by the source chat's host device (the
            // Pi session store lives there).
            | methods::FORK_SESSION
            // A rewind rewrites the same chat's Pi session: host device too.
            | methods::REWIND_SESSION
    )
}

/// Forwardable methods whose reply is a stream (proxied item-by-item).
fn is_stream_method(method: &str) -> bool {
    matches!(
        method,
        methods::WATCH_DOC_MESSAGES
            | methods::WATCH_DOC_COMMANDS
            | methods::SUBSCRIBE_TERMINAL
            | methods::WATCH_CHECKOUT_DIFFS
            | methods::UPDATE_STATUS
            | methods::PI_UPDATE_STATUS
            | methods::WATCH_SIDE_CHAT_STATUS
    )
}

/// A watch receiver as a stream: current value first, then every change.
fn watch_stream<T>(rx: watch::Receiver<T>) -> BoxStream<'static, serde_json::Value>
where
    T: serde::Serialize + Clone + Send + Sync + 'static,
{
    futures::stream::unfold((rx, false), |(mut rx, emitted)| async move {
        if emitted {
            rx.changed().await.ok()?;
        }
        let value = {
            let borrowed = rx.borrow_and_update();
            serde_json::to_value(&*borrowed).ok()?
        };
        Some((value, (rx, true)))
    })
    .boxed()
}

/// The transcript watch as delta frames (`cypher_doc::transcript_delta`): a
/// full `reset` first, then only changed entries per commit — the whole-Vec
/// serialization here was the per-tick cost that scaled with transcript size.
fn doc_messages_stream(
    rx: watch::Receiver<Vec<cypher_doc::SessionMessageEntry>>,
) -> BoxStream<'static, serde_json::Value> {
    use cypher_doc::transcript_delta::{TranscriptFrame, diff_transcript};
    futures::stream::unfold(
        (rx, None::<Vec<cypher_doc::SessionMessageEntry>>),
        |(mut rx, mut prev)| async move {
            loop {
                if prev.is_some() {
                    rx.changed().await.ok()?;
                }
                let current: Vec<_> = rx.borrow_and_update().clone();
                let frame = match prev.as_deref() {
                    None => TranscriptFrame::reset(&current),
                    Some(prev) => diff_transcript(prev, &current),
                };
                prev = Some(current);
                // No-op commits (a second watcher attaching, command-only
                // changes) produce empty deltas — skip the frame entirely.
                if frame.is_empty_delta() {
                    continue;
                }
                let value = serde_json::to_value(&frame).ok()?;
                return Some((value, (rx, prev)));
            }
        },
    )
    .boxed()
}

/// Authentication-only RPC surface used while the headed app is waiting for a
/// production WorkOS session. Keeping this independent from [`EngineRpc`] lets
/// the UI show its sign-in and organization gates before identity-scoped Loro
/// stores are opened.
#[derive(Clone)]
pub struct AuthRpc {
    auth: Auth,
}

impl AuthRpc {
    pub fn new(auth: Auth) -> Self {
        Self { auth }
    }

    pub fn handles(method: &str) -> bool {
        matches!(
            method,
            methods::AUTH_STATUS
                | methods::SIGN_IN
                | methods::SIGN_IN_HEADLESS
                | methods::COMPLETE_SIGN_IN
                | methods::SIGN_OUT
                | methods::LIST_ORGS
                | methods::CREATE_ORG
                | methods::SELECT_ORG
                | methods::NOTIFICATION_ACTIVITY
        )
    }
}

#[async_trait]
impl RpcService for AuthRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            methods::NOTIFICATION_ACTIVITY => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct P {
                    expected_user_id: String,
                    expected_org_id: String,
                    client_id: String,
                    sequence: u64,
                    foreground: bool,
                    interaction_age_ms: u64,
                    chat_id: Option<String>,
                }
                let p: P = parse_params(params)?;
                let valid_id = |s: &str| {
                    !s.is_empty()
                        && s.len() <= 128
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                };
                if !valid_id(&p.client_id)
                    || p.chat_id.as_deref().is_some_and(|id| !valid_id(id))
                    || p.interaction_age_ms > 86_400_000
                    || p.sequence == 0
                    || p.sequence > 9_007_199_254_740_991
                {
                    return Err(RpcError::BadParams("invalid activity".into()));
                }
                let activity = serde_json::json!({
                    "clientId": p.client_id, "sequence": p.sequence, "platform": "desktop", "foreground": p.foreground,
                    "interactionAgeMs": p.interaction_age_ms, "chatId": p.chat_id,
                });
                // A refresh of the same state rides the presence beat that is
                // already going out every 15s, costing no request of its own.
                if !self.auth.viewport_activity().record(activity.clone()) {
                    return RpcReply::value(&serde_json::json!({ "ok": true, "deferred": true }));
                }
                // A transition (another chat, or entering/leaving the
                // foreground) must reach the Worker PROMPTLY -- its push
                // suppression reads `foreground` -- but it needs no reply here:
                // `readEventIds` is iOS-only and the desktop UI discards the
                // badge. So it goes out now as an extra presence beat on the
                // open socket (20:1) instead of as an HTTP request (1:1). Every
                // alt-tab used to cost two billable requests this way, ~190 an
                // hour in production. HTTP remains the path when no socket is
                // live, so a socketless client is exactly as prompt as before.
                // Same identity guard as the HTTP path; on a mismatch fall
                // through so that path returns its error.
                if self
                    .auth
                    .notification_identity_matches(&p.expected_user_id, &p.expected_org_id)
                    && self.auth.viewport_activity().beat_now()
                {
                    return RpcReply::value(
                        &serde_json::json!({ "ok": true, "viaPresence": true }),
                    );
                }
                let response = self
                    .auth
                    .report_notification_activity(&p.expected_user_id, &p.expected_org_id, activity)
                    .await
                    .map_err(failed)?;
                RpcReply::value(&response)
            }
            methods::AUTH_STATUS => Ok(RpcReply::Stream(watch_stream(self.auth.watch_state()))),
            methods::SIGN_IN => {
                let url = self.auth.start_sign_in().await.map_err(failed)?;
                RpcReply::value(&serde_json::json!({ "url": url }))
            }
            methods::SIGN_IN_HEADLESS => {
                let url = self.auth.start_headless_sign_in();
                RpcReply::value(&serde_json::json!({ "url": url }))
            }
            methods::COMPLETE_SIGN_IN => {
                #[derive(Deserialize)]
                struct P {
                    code: String,
                }
                let p: P = parse_params(params)?;
                self.auth.complete_sign_in(&p.code).await.map_err(failed)?;
                RpcReply::ok()
            }
            methods::SIGN_OUT => {
                self.auth.sign_out();
                RpcReply::ok()
            }
            methods::LIST_ORGS => {
                let orgs = self.auth.list_orgs().await.map_err(failed)?;
                RpcReply::value(&serde_json::json!({ "orgs": orgs }))
            }
            methods::CREATE_ORG => {
                #[derive(Deserialize)]
                struct P {
                    name: String,
                }
                let p: P = parse_params(params)?;
                self.auth.create_org(&p.name).await.map_err(failed)?;
                RpcReply::ok()
            }
            methods::SELECT_ORG => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    organization_id: String,
                }
                let p: P = parse_params(params)?;
                self.auth
                    .select_org(&p.organization_id)
                    .await
                    .map_err(failed)?;
                RpcReply::ok()
            }
            _ => Err(RpcError::UnknownMethod(method.to_string())),
        }
    }
}

#[async_trait]
impl RpcService for EngineRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        preflight(method, &params)?;
        // Device-addressed routing: forward calls that target another device over its
        // relay. The target compares the id to its own, so forwards cannot loop.
        if forwardable(method)
            && let Some(target) = params.get("targetDeviceId").and_then(|v| v.as_str())
            && target != self.doc_host.device_id()
        {
            let target = target.to_string();
            return self.forward(&target, method, params).await;
        }
        if AuthRpc::handles(method) {
            return AuthRpc::new(self.auth()?.clone())
                .handle(method, params)
                .await;
        }
        self.refuse_during_login(method)?;
        match method {
            methods::ENGINE_INFO => RpcReply::value(&self.engine_info),
            methods::ENGINE_READY => RpcReply::value(&serde_json::json!({ "ready": true })),
            methods::LIST_HARNESSES => RpcReply::value(&self.registry.descriptors()),
            methods::SET_HARNESS_ENABLED => harnesses::set_harness_enabled(self, params),
            methods::LIST_PI_PACKAGES => {
                RpcReply::value(&crate::pi::packages::list(self.pi_runtime()?.paths()))
            }
            methods::INSTALL_PI => pi::install_pi(self).await,
            methods::INSTALL_PI_PACKAGE => pi::install_pi_package(self, params).await,
            methods::SET_PI_PACKAGE_ENABLED => pi::set_pi_package_enabled(self, params).await,
            methods::PI_RUNTIME_STATUS => RpcReply::value(&self.pi_runtime()?.status()),
            methods::LIST_PI_SUBAGENTS => pi::list_pi_subagents(self).await,
            methods::SAVE_PI_SUBAGENT => pi::save_pi_subagent(self, params).await,
            methods::DELETE_PI_SUBAGENT => pi::delete_pi_subagent(self, params).await,
            methods::GET_PI_TRANSLATION_SETTINGS => pi::get_pi_translation_settings(self).await,
            methods::SET_PI_TRANSLATION_SETTINGS => {
                pi::set_pi_translation_settings(self, params).await
            }
            methods::DETECT_PI_LANGUAGE => pi::detect_pi_language(params),
            methods::LIST_PI_PROVIDERS
            | methods::SAVE_PI_PROVIDER
            | methods::REFRESH_PI_PROVIDER
            | methods::LOGOUT_PI_PROVIDER
            | methods::REMOVE_PI_PROVIDER => pi::pi_provider(self, method, params).await,
            methods::BEGIN_PI_PROVIDER_LOGIN => pi::begin_pi_provider_login(self, params),
            methods::PI_PROVIDER_LOGIN_STATUS
            | methods::COMPLETE_PI_PROVIDER_LOGIN
            | methods::CANCEL_PI_PROVIDER_LOGIN => {
                pi::pi_provider_login(self, method, params).await
            }
            methods::LIST_MCP_SERVERS => mcp::list_mcp_servers(self).await,
            methods::ADD_MCP_SERVERS => mcp::add_mcp_servers(self, params).await,
            methods::SET_MCP_SERVER_ENABLED => mcp::set_mcp_server_enabled(self, params).await,
            methods::REMOVE_MCP_SERVER => mcp::remove_mcp_server(self, params).await,
            methods::BEGIN_MCP_LOGIN => mcp::begin_mcp_login(self, params),
            methods::MCP_LOGIN_STATUS | methods::COMPLETE_MCP_LOGIN | methods::CANCEL_MCP_LOGIN => {
                mcp::mcp_login(self, method, params).await
            }
            methods::START_MCP_AUTH => mcp::start_mcp_auth(self, params).await,
            methods::LOGOUT_MCP_SERVER => mcp::logout_mcp_server(self, params).await,
            methods::GET_WEB_SEARCH_FALLBACK => {
                RpcReply::value(&crate::pi::web_search::load(self.pi_runtime()?.paths()))
            }
            methods::SET_WEB_SEARCH_FALLBACK => {
                settings::set_web_search_fallback(self, params).await
            }
            methods::GET_TITLE_MODEL_SETTINGS => settings::get_title_model_settings(self),
            methods::SET_TITLE_MODEL_SETTINGS => {
                settings::set_title_model_settings(self, params).await
            }
            methods::LIST_MODELS => {
                let p: ListModelsParams = parse_params(params)?;
                let harness = self.registry.resolve(p.harness).map_err(failed)?;
                let models = harness.models().await.map_err(failed)?;
                RpcReply::value(&models)
            }
            methods::LIST_COMMANDS => harnesses::list_commands(self, params).await,
            methods::PI_SESSION_MODES => pi::pi_session_modes(self, params).await,
            methods::QUEUE_COMMAND => docs::queue_command(self, params),
            methods::RETRY_COMMAND => docs::retry_command(self, params),
            methods::WATCH_DOC_MESSAGES => docs::watch_doc_messages(self, params),
            methods::WATCH_DOC_COMMANDS => docs::watch_doc_commands(self, params),
            methods::PROBE_SYNC => {
                self.workspace.probe();
                self.doc_host.probe_open_chats();
                RpcReply::value(&serde_json::json!({}))
            }
            methods::SYNC_STATUS => docs::sync_status(self),
            methods::WATCH_CHATS => {
                Ok(RpcReply::Stream(watch_stream(self.workspace.watch_chats())))
            }
            methods::WATCH_DEVICES => Ok(RpcReply::Stream(watch_stream(
                self.workspace.watch_devices(),
            ))),
            methods::WATCH_SPACES => Ok(RpcReply::Stream(watch_stream(
                self.workspace.watch_spaces(),
            ))),
            methods::WATCH_SESSIONS => docs::watch_sessions(self),
            methods::LOCAL_IMPORT_STATUS => docs::local_import_status(self).await,
            methods::IMPORT_LOCAL_WORKSPACE => docs::import_local_workspace(self),
            methods::UPDATE_STATUS => Ok(RpcReply::Stream(watch_stream(self.updater()?.watch()))),
            methods::CHECK_UPDATE => RpcReply::value(&self.updater()?.check().await),
            methods::UPDATE_ON_ACTIVATION => RpcReply::value(&serde_json::json!({
                "woke": self.updater()?.check_on_activation(),
            })),
            methods::APPLY_UPDATE => updates::apply_update(self, params).await,
            methods::PI_UPDATE_STATUS => Ok(RpcReply::Stream(watch_stream(
                self.pi_runtime()?.watch_updates(),
            ))),
            methods::CHECK_PI_UPDATE => updates::check_pi_update(self).await,
            methods::APPLY_PI_UPDATES => updates::apply_pi_updates(self).await,
            methods::MUTATE => {
                let p: MutateParams = parse_params(params)?;
                self.mutate(p)?;
                RpcReply::ok()
            }
            methods::WATCH_CHECKOUT_DIFFS => {
                Ok(RpcReply::Stream(watch_stream(self.diff_sync.watch_diffs())))
            }
            // Keep the scoped-diff futures off the dispatcher's stack: the
            // per-commit path adds another nested git-capture future, and every
            // unrelated RPC would otherwise carry their state in `handle`'s frame.
            methods::GET_CHECKOUT_DIFF => Box::pin(diffs::get_checkout_diff(self, params)).await,
            methods::GET_CHECKOUT_FILE_DIFF_TEXT => {
                Box::pin(diffs::get_checkout_file_diff_text(self, params)).await
            }
            methods::LIST_REPOS => RpcReply::value(&self.repos.list().await),
            methods::ADD_REPO => {
                let p: PathParams = parse_params(params)?;
                let repo = self.repos.add(&p.path).await.map_err(failed)?;
                RpcReply::value(&repo)
            }
            methods::CLONE_REPO => {
                let p: UrlParams = parse_params(params)?;
                let repo = self.repos.clone_repo(&p.url).await.map_err(failed)?;
                RpcReply::value(&repo)
            }
            methods::CREATE_REPO => {
                let p: NameParams = parse_params(params)?;
                let repo = self.repos.create(&p.name).await.map_err(failed)?;
                RpcReply::value(&repo)
            }
            methods::LIST_BRANCHES => repos::list_branches(self, params).await,
            methods::LIST_REFS => repos::list_refs(self, params).await,
            methods::LIST_GIT_HISTORY => repos::list_git_history(self, params).await,
            methods::FETCH_ALL => repos::fetch_all(self, params).await,
            methods::SWITCH_REF => repos::switch_ref(self, params).await,
            methods::LIST_FOLDERS => {
                let p: ListFoldersParams = parse_params(params)?;
                let listing = self.repos.list_folders(p.path).await.map_err(failed)?;
                RpcReply::value(&listing)
            }
            methods::LIST_WORKSPACE_FILES
            | methods::READ_WORKSPACE_FILE
            | methods::WRITE_WORKSPACE_FILE => files::workspace_file(self, method, params).await,
            methods::SEARCH_FILES => files::search_files(self, params).await,
            methods::SEARCH_GITHUB_ISSUES => github::search_github_issues(self, params).await,
            methods::GET_GITHUB_ISSUE => github::get_github_issue(self, params).await,
            methods::GITHUB_ACCOUNT_STATUS => RpcReply::value(&self.github()?.status().await),
            methods::START_GITHUB_LOGIN => {
                let start = self.github()?.start_login().await.map_err(failed)?;
                RpcReply::value(&start)
            }
            methods::POLL_GITHUB_LOGIN | methods::CANCEL_GITHUB_LOGIN => {
                github::github_login(self, method, params)
            }
            methods::SIGN_OUT_GITHUB => {
                self.github()?.sign_out().await;
                RpcReply::ok()
            }
            methods::CREATE_WORKTREE => repos::create_worktree(self, params).await,
            methods::DELETE_WORKTREE => repos::delete_worktree(self, params).await,
            methods::CREATE_SCRATCH_DIR => {
                let p: ChatParams = parse_params(params)?;
                let path = crate::session::scratch::create(&p.chat_id).map_err(RpcError::Failed)?;
                RpcReply::value(&serde_json::json!({ "path": path }))
            }
            methods::DELETE_SCRATCH_DIR => {
                let p: DeleteScratchDirParams = parse_params(params)?;
                let removed = crate::session::scratch::delete(&p.chat_id, &p.path)
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&serde_json::json!({ "ok": true, "removed": removed }))
            }
            methods::OPEN_TERMINAL => terminals::open_terminal(self, params),
            methods::SUBSCRIBE_TERMINAL => terminals::subscribe_terminal(self, params),
            methods::WRITE_TERMINAL => terminals::write_terminal(self, params),
            methods::RESIZE_TERMINAL => terminals::resize_terminal(self, params),
            methods::CLOSE_TERMINAL => {
                let p: TerminalIdParams = parse_params(params)?;
                self.terminals.close(&p.terminal_id).map_err(failed)?;
                RpcReply::ok()
            }
            methods::UPLOAD_CHUNK => uploads::upload_chunk(self, params),
            methods::UPLOAD_COMMIT => uploads::upload_commit(self, params),
            methods::READ_ATTACHMENT_CHUNK => uploads::read_attachment_chunk(self, params),
            methods::FETCH_TOOL_BLOB => uploads::fetch_tool_blob(self, params).await,
            methods::START_SUBAGENT => {
                let p: StartSubagentParams = parse_params(params)?;
                self.start_subagent(p).await
            }
            methods::START_SIDE_CHAT => chats::start_side_chat(self, params),
            methods::SEND_SIDE_CHAT => chats::send_side_chat(self, params).await,
            methods::INTERRUPT_SIDE_CHAT => chats::interrupt_side_chat(self, params).await,
            methods::RESPOND_SIDE_CHAT_INPUT => chats::respond_side_chat_input(self, params),
            methods::WATCH_SIDE_CHAT_STATUS => chats::watch_side_chat_status(self, params),
            methods::PROMOTE_SIDE_CHAT => {
                let p: SideChatIdParams = parse_params(params)?;
                let promoted = self.side_chats.promote(&p.side_chat_id).map_err(failed)?;
                RpcReply::value(&promoted)
            }
            methods::DISPOSE_SIDE_CHAT => chats::dispose_side_chat(self, params).await,
            methods::FORK_SESSION => {
                let p: SessionForkRequest = parse_params(params)?;
                let reply = self.session_forks.fork(p).await.map_err(failed)?;
                RpcReply::value(&reply)
            }
            methods::REWIND_SESSION => {
                let p: cypher_proto::SessionRewindRequest = parse_params(params)?;
                let reply = self.session_forks.rewind(p).await.map_err(failed)?;
                RpcReply::value(&reply)
            }
            methods::WATCH_AGENT_EVENTS => chats::watch_agent_events(self, params),
            other => Err(RpcError::UnknownMethod(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests;
