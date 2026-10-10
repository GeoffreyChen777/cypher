//! cypher-rpc — the typed control plane (UiRpc / ControlRpc) over WebSocket + in-memory
//! transports, plus the device-room relay transport ({s,k,to,from} frames — [`device_room`]).
//!
//! Framing: ndjson envelopes, one JSON object per WebSocket text message (or per line on
//! byte transports), matching the shape of zeron's Effect RPC without the Effect runtime:
//!
//! - client → server: `{id, method, params}` to invoke, `{id, cancel: true}` to stop a stream;
//! - server → client: `{id, ok}` / `{id, err}` for unary calls,
//!   `{id, item}`* then `{id, done: true}` (or `{id, err}`) for streams.
//!
//! The server dispatches into an [`RpcService`]; the [`RpcClient`] offers `call` and
//! `subscribe`. Both ends run over any pair of string channels, so the in-memory transport
//! ([`memory_client`]) exercises the exact same code path as the WebSocket one.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

mod client;
pub mod device_room;
mod server;

pub use client::RpcClient;
mod local;
pub use device_room::{
    DeviceFrameHeader, DeviceLink, HostRelay, HostRelayConfig, LinkCache, LinkCacheConfig,
    NudgeHandler, StaticToken, TokenSource, decode_device_frame, device_room_ws_url,
    encode_device_frame,
};
pub use local::{LocalListener, connect_local, probe_local};
use server::serve_connection;

/// RPC method names — single source of truth for both ends.
pub mod methods {
    pub const LIST_HARNESSES: &str = "ListHarnesses";
    /// Flip a harness's enablement on the target device (Settings → Agents);
    /// replies with the device's fresh `ListHarnesses` catalog.
    pub const SET_HARNESS_ENABLED: &str = "SetHarnessEnabled";
    pub const LIST_PI_PACKAGES: &str = "ListPiPackages";
    pub const INSTALL_PI: &str = "InstallPi";
    pub const INSTALL_PI_PACKAGE: &str = "InstallPiPackage";
    pub const SET_PI_PACKAGE_ENABLED: &str = "SetPiPackageEnabled";
    /// Download/install state for Cypher's isolated Pi runtime.
    pub const PI_RUNTIME_STATUS: &str = "PiRuntimeStatus";
    /// Subagent profiles (`agents/*.md`) discoverable on the target device —
    /// the extension's built-ins with the user's overrides applied.
    pub const LIST_PI_SUBAGENTS: &str = "ListPiSubagents";
    /// Create, edit or rename one user-level subagent profile; replies with
    /// the device's fresh `ListPiSubagents` list.
    pub const SAVE_PI_SUBAGENT: &str = "SavePiSubagent";
    /// Remove a user-level profile. A built-in cannot be deleted — deleting an
    /// override restores it.
    pub const DELETE_PI_SUBAGENT: &str = "DeletePiSubagent";
    pub const GET_PI_TRANSLATION_SETTINGS: &str = "GetPiTranslationSettings";
    pub const SET_PI_TRANSLATION_SETTINGS: &str = "SetPiTranslationSettings";
    pub const DETECT_PI_LANGUAGE: &str = "DetectPiLanguage";
    pub const LIST_PI_PROVIDERS: &str = "ListPiProviders";
    pub const SAVE_PI_PROVIDER: &str = "SavePiProvider";
    pub const REFRESH_PI_PROVIDER: &str = "RefreshPiProvider";
    pub const LOGOUT_PI_PROVIDER: &str = "LogoutPiProvider";
    pub const REMOVE_PI_PROVIDER: &str = "RemovePiProvider";
    pub const BEGIN_PI_PROVIDER_LOGIN: &str = "BeginPiProviderLogin";
    pub const PI_PROVIDER_LOGIN_STATUS: &str = "PiProviderLoginStatus";
    pub const COMPLETE_PI_PROVIDER_LOGIN: &str = "CompletePiProviderLogin";
    pub const CANCEL_PI_PROVIDER_LOGIN: &str = "CancelPiProviderLogin";
    /// Current Pi CLI + package update facts, then every six-hour refresh or
    /// apply transition.
    pub const PI_UPDATE_STATUS: &str = "PiUpdateStatus";
    /// Run one Runtime manifest check NOW instead of waiting for the six-hour
    /// sweep, and reply with the resulting `PiUpdateStatus`. Same semantics as
    /// that sweep: a newer bundle installs itself, so the reply lands after the
    /// install and live progress arrives on `PiUpdateStatus`.
    pub const CHECK_PI_UPDATE: &str = "CheckPiUpdate";
    /// Explicit retry/repair of the latest isolated Pi runtime bundle.
    pub const APPLY_PI_UPDATES: &str = "ApplyPiUpdates";
    pub const LIST_MCP_SERVERS: &str = "ListMcpServers";
    pub const ADD_MCP_SERVERS: &str = "AddMcpServers";
    pub const REMOVE_MCP_SERVER: &str = "RemoveMcpServer";
    pub const SET_MCP_SERVER_ENABLED: &str = "SetMcpServerEnabled";
    pub const START_MCP_AUTH: &str = "StartMcpAuth";
    pub const BEGIN_MCP_LOGIN: &str = "BeginMcpLogin";
    pub const MCP_LOGIN_STATUS: &str = "McpLoginStatus";
    pub const COMPLETE_MCP_LOGIN: &str = "CompleteMcpLogin";
    pub const CANCEL_MCP_LOGIN: &str = "CancelMcpLogin";
    pub const LOGOUT_MCP_SERVER: &str = "LogoutMcpServer";
    pub const LIST_MODELS: &str = "ListModels";
    pub const GET_TITLE_MODEL_SETTINGS: &str = "GetTitleModelSettings";
    pub const SET_TITLE_MODEL_SETTINGS: &str = "SetTitleModelSettings";
    pub const GET_WEB_SEARCH_FALLBACK: &str = "GetWebSearchFallback";
    pub const SET_WEB_SEARCH_FALLBACK: &str = "SetWebSearchFallback";
    pub const LIST_COMMANDS: &str = "ListCommands";
    /// What the Pi plugins' per-chat switches are set to (GPT Fast mode,
    /// adaptive orchestration, the current goal), read from the chat's Pi
    /// session on its host device. Params `{chatId?}`; without a chat (or a
    /// session yet) the reply carries the plugins' defaults.
    pub const PI_SESSION_MODES: &str = "PiSessionModes";
    pub const QUEUE_COMMAND: &str = "QueueCommand";
    /// Re-issue a failed/expired durable message command with a fresh command
    /// id while preserving its logical message identity.
    pub const RETRY_COMMAND: &str = "RetryCommand";
    pub const WATCH_DOC_MESSAGES: &str = "WatchDocMessages";
    /// Durable command ledger of one chat (forwardable stream, params
    /// `{chatId}`): the current command list first, then every doc change.
    /// The UI projects Queued / Failed / Retrying from these entries — the
    /// authoritative source over the local optimistic overlay.
    pub const WATCH_DOC_COMMANDS: &str = "WatchDocCommands";
    /// Nudge every open room client to verify liveness NOW (window focus,
    /// app foregrounded). No params; IPC-only. Each room ignores the hint
    /// unless it has been broadcast-quiet ≥30s, so this is cheap to spam.
    pub const PROBE_SYNC: &str = "ProbeSync";
    pub const NOTIFICATION_ACTIVITY: &str = "ReportNotificationActivity";
    /// Live sync introspection (`cypher sync` / debug surfaces): per-room
    /// connection state, last pushed-frame/ack ages, rejoin/probe/resync
    /// counters for the workspace room and every open chat doc. No params;
    /// IPC-only.
    pub const SYNC_STATUS: &str = "SyncStatus";
    pub const WATCH_CHATS: &str = "WatchChats";
    pub const WATCH_DEVICES: &str = "WatchDevices";
    pub const WATCH_SESSIONS: &str = "WatchSessions";
    /// Spaces registry (device+folder pairs) from the workspace doc.
    pub const WATCH_SPACES: &str = "WatchSpaces";
    /// Entity mutations against the workspace doc.
    /// Params are tagged `{op: createChat|createSpace|renameSpace|deleteSpace|
    /// renameChat|setChatArchived|deleteChat|renameDevice|deleteDevice|markChatSeen, …}`.
    pub const MUTATE: &str = "Mutate";
    /// This engine runtime's fixed identity → `{deviceId, workspaceScope}`
    /// (IPC-only; never relay-forwarded — the answer is about whichever engine
    /// you are directly connected to).
    pub const ENGINE_INFO: &str = "EngineInfo";
    /// Readiness barrier for the engine runtime. The call completes once stores
    /// and journals are assembled, or fails with the assembly error.
    pub const ENGINE_READY: &str = "EngineReady";
    /// Ask a headless IPC owner to drain its runtime and exit successfully.
    /// Headed IPC owners do not implement this method: closing another app's
    /// engine behind its windows would leave that process unusable.
    pub const STOP_ENGINE: &str = "StopEngine";
    pub const AUTH_STATUS: &str = "AuthStatus";
    // Auth mutations (IPC-only).
    pub const SIGN_IN: &str = "SignIn";
    pub const SIGN_IN_HEADLESS: &str = "SignInHeadless";
    pub const COMPLETE_SIGN_IN: &str = "CompleteSignIn";
    pub const SIGN_OUT: &str = "SignOut";
    pub const LIST_ORGS: &str = "ListOrgs";
    pub const CREATE_ORG: &str = "CreateOrg";
    pub const SELECT_ORG: &str = "SelectOrg";
    /// One-time local→synced profile import: what's importable (unary).
    pub const LOCAL_IMPORT_STATUS: &str = "LocalImportStatus";
    /// One-time local→synced profile import: run it (stream of progress items).
    pub const IMPORT_LOCAL_WORKSPACE: &str = "ImportLocalWorkspace";
    // Repos / worktrees / folders (ControlRpc, relay-forwardable).
    pub const LIST_REPOS: &str = "ListRepos";
    pub const ADD_REPO: &str = "AddRepo";
    pub const CLONE_REPO: &str = "CloneRepo";
    pub const CREATE_REPO: &str = "CreateRepo";
    pub const LIST_BRANCHES: &str = "ListBranches";
    pub const LIST_REFS: &str = "ListRefs";
    pub const LIST_GIT_HISTORY: &str = "ListGitHistory";
    /// Update remote-tracking refs without changing HEAD, the index, or files.
    pub const FETCH_ALL: &str = "FetchAll";
    pub const SWITCH_REF: &str = "SwitchRef";
    pub const LIST_FOLDERS: &str = "ListFolders";
    /// Fuzzy relative-path search rooted in a known chat or space checkout.
    pub const SEARCH_FILES: &str = "SearchFiles";
    /// `#` issue completion for a chat or space checkout, answered with the
    /// host device's GitHub credential; {chatId | spaceId, path?, query}.
    pub const SEARCH_GITHUB_ISSUES: &str = "SearchGithubIssues";
    /// Bounded send-time snapshot of one issue; {repo, number}.
    pub const GET_GITHUB_ISSUE: &str = "GetGithubIssue";
    // GitHub sign-in (relay-forwardable — every device holds its own login).
    pub const GITHUB_ACCOUNT_STATUS: &str = "GithubAccountStatus";
    /// Device-flow sign-in with the Cypher GitHub App; the target engine
    /// polls GitHub and stores the token itself.
    pub const START_GITHUB_LOGIN: &str = "StartGithubLogin";
    pub const POLL_GITHUB_LOGIN: &str = "PollGithubLogin";
    pub const CANCEL_GITHUB_LOGIN: &str = "CancelGithubLogin";
    pub const SIGN_OUT_GITHUB: &str = "SignOutGithub";
    /// Read-only browser; {chatId, cwd, path}. cwd must still be the chat's
    /// assigned checkout, and path is relative with no symlink traversal.
    pub const LIST_WORKSPACE_FILES: &str = "ListWorkspaceFiles";
    pub const READ_WORKSPACE_FILE: &str = "ReadWorkspaceFile";
    /// Replace the text of an EXISTING regular file; {chatId, cwd, path,
    /// text}. Same checkout/path rules as the reads (no creation, no symlink
    /// traversal, bounded size).
    pub const WRITE_WORKSPACE_FILE: &str = "WriteWorkspaceFile";
    pub const CREATE_WORKTREE: &str = "CreateWorktree";
    pub const DELETE_WORKTREE: &str = "DeleteWorktree";
    // Quick-chat scratch folders live on the chat's host device.
    pub const CREATE_SCRATCH_DIR: &str = "CreateScratchDir";
    pub const DELETE_SCRATCH_DIR: &str = "DeleteScratchDir";
    // Terminals (ControlRpc, relay-forwardable; SubscribeTerminal streams).
    pub const OPEN_TERMINAL: &str = "OpenTerminal";
    pub const SUBSCRIBE_TERMINAL: &str = "SubscribeTerminal";
    pub const WRITE_TERMINAL: &str = "WriteTerminal";
    pub const RESIZE_TERMINAL: &str = "ResizeTerminal";
    pub const CLOSE_TERMINAL: &str = "CloseTerminal";
    /// Checkout-diff stream for the target device's chats (DataRpc,
    /// relay-forwardable — diffs are produced where the checkout lives).
    pub const WATCH_CHECKOUT_DIFFS: &str = "WatchCheckoutDiffs";
    pub const GET_CHECKOUT_DIFF: &str = "GetCheckoutDiff";
    pub const GET_CHECKOUT_FILE_DIFF_TEXT: &str = "GetCheckoutFileDiffText";
    // Uploads / attachments (ControlRpc, relay-forwardable — target the chat's host device).
    pub const UPLOAD_CHUNK: &str = "UploadChunk";
    pub const UPLOAD_COMMIT: &str = "UploadCommit";
    pub const READ_ATTACHMENT_CHUNK: &str = "ReadAttachmentChunk";
    /// Lazy full-tool-output fetch from the R2 sidecar by doc-resident ref
    /// (chat2-sync A3). Edge-direct from any device — never relay-forwarded.
    pub const FETCH_TOOL_BLOB: &str = "FetchToolBlob";
    // Updates (ControlRpc, relay-forwardable — a device reports/applies its own
    // binary's update). Stream: current UpdateStatus, then every change.
    pub const UPDATE_STATUS: &str = "UpdateStatus";
    /// Run one release check now and return the resulting UpdateStatus.
    pub const CHECK_UPDATE: &str = "CheckUpdate";
    /// The user returned to the app. Wakes the release checker so a release
    /// published while they were away is visible on return rather than at the
    /// next tick. Rate-limited by the engine, so it is safe to call on every
    /// window activation; replies `{woke}` and never blocks on the network.
    pub const UPDATE_ON_ACTIVATION: &str = "UpdateOnActivation";
    /// Download + apply the newest release on the target device (symlink-managed
    /// installs; the service restart is scheduled after the reply flushes).
    pub const APPLY_UPDATE: &str = "ApplyUpdate";
    /// Cypher child-subagent bridge (IPC-only, unary): idempotently create the
    /// same-device child Chat for `(parentChatId, runId)` and queue its initial
    /// durable Pi run; replies `{childChatId}`. See `StartSubagent` in the
    /// engine RPC + docs/research/pi-rpc.md.
    pub const START_SUBAGENT: &str = "StartSubagent";
    /// Cypher child-subagent bridge (IPC-only, stream): replayable agent events
    /// for a chat (journal replay after `afterSeq`, then live) — the parent
    /// extension observes the child's terminal `Done`/result through this.
    pub const WATCH_AGENT_EVENTS: &str = "WatchAgentEvents";
    // Selected-text Side Chats: temporary engine-hosted chats
    // opened from a settled selection. All are relay-forwardable — the parent
    // chat's host device owns the side chat, so every call carries
    // `targetDeviceId` (see the engine `side_chats` module).
    /// Open a temporary Side Chat from a settled selection (unary).
    pub const START_SIDE_CHAT: &str = "StartSideChat";
    /// Send a user turn into a Side Chat (unary). The FIRST send injects the
    /// stored selection + bounded parent context into the effective
    /// `agentPrompt`; later sends resume normally.
    pub const SEND_SIDE_CHAT: &str = "SendSideChat";
    /// Interrupt a Side Chat's live run (unary).
    pub const INTERRUPT_SIDE_CHAT: &str = "InterruptSideChat";
    /// Answer an in-flight input request in a Side Chat (unary).
    pub const RESPOND_SIDE_CHAT_INPUT: &str = "RespondSideChatInput";
    /// Private live status stream for one Side Chat (stream) — never the
    /// public WatchSessions stream.
    pub const WATCH_SIDE_CHAT_STATUS: &str = "WatchSideChatStatus";
    /// Promote a Side Chat into a normal root chat (unary, idempotent).
    pub const PROMOTE_SIDE_CHAT: &str = "PromoteSideChat";
    /// Dispose an UNPROMOTED Side Chat: interrupt the run and drop all
    /// ephemeral state (unary, no-op after promotion).
    pub const DISPOSE_SIDE_CHAT: &str = "DisposeSideChat";
    /// Fork a settled transcript prefix into a NEW durable root chat on the
    /// source chat's host device (unary, idempotent by the client-minted
    /// `requestId` = target chat id). Session Fork is Pi-only in v1.
    pub const FORK_SESSION: &str = "ForkSession";
    /// Rewind a chat IN PLACE on its host device (unary): truncate the
    /// transcript at a settled anchor and re-point the SAME chat at a
    /// freshly materialized, truncated Pi session. Pi-only, like the fork.
    pub const REWIND_SESSION: &str = "RewindSession";

    /// How one method is answered and routed. The engine derives its routing
    /// predicates from [`SPECS`], so adding a method means adding its constant
    /// above and one entry there.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MethodSpec {
        pub name: &'static str,
        /// Replies with a stream of items instead of one value.
        pub stream: bool,
        /// Honors `targetDeviceId`: the engine relays the call to that device
        /// instead of answering it locally.
        pub forwardable: bool,
        /// Served by the auth-only surface, which answers before a workspace
        /// profile is open (and by the full engine through it).
        pub auth: bool,
        /// Carries credentials or server configuration, so it may only be
        /// relayed over an HTTPS/WSS (or loopback development) link.
        pub credentials: bool,
    }

    impl MethodSpec {
        const fn unary(name: &'static str) -> Self {
            Self {
                name,
                stream: false,
                forwardable: false,
                auth: false,
                credentials: false,
            }
        }

        const fn stream(name: &'static str) -> Self {
            Self {
                stream: true,
                ..Self::unary(name)
            }
        }

        const fn forwardable(self) -> Self {
            Self {
                forwardable: true,
                ..self
            }
        }

        const fn auth(self) -> Self {
            Self { auth: true, ..self }
        }

        const fn credentials(self) -> Self {
            Self {
                credentials: true,
                ..self
            }
        }
    }

    /// Every method, in declaration order.
    pub const SPECS: &[MethodSpec] = &[
        MethodSpec::unary(LIST_HARNESSES).forwardable(),
        MethodSpec::unary(SET_HARNESS_ENABLED).forwardable(),
        MethodSpec::unary(LIST_PI_PACKAGES).forwardable(),
        MethodSpec::unary(INSTALL_PI).forwardable(),
        MethodSpec::unary(INSTALL_PI_PACKAGE).forwardable(),
        MethodSpec::unary(SET_PI_PACKAGE_ENABLED).forwardable(),
        MethodSpec::unary(PI_RUNTIME_STATUS).forwardable(),
        MethodSpec::unary(LIST_PI_SUBAGENTS).forwardable(),
        MethodSpec::unary(SAVE_PI_SUBAGENT).forwardable(),
        MethodSpec::unary(DELETE_PI_SUBAGENT).forwardable(),
        MethodSpec::unary(GET_PI_TRANSLATION_SETTINGS).forwardable(),
        MethodSpec::unary(SET_PI_TRANSLATION_SETTINGS).forwardable(),
        MethodSpec::unary(DETECT_PI_LANGUAGE).forwardable(),
        MethodSpec::unary(LIST_PI_PROVIDERS).forwardable(),
        MethodSpec::unary(SAVE_PI_PROVIDER)
            .forwardable()
            .credentials(),
        MethodSpec::unary(REFRESH_PI_PROVIDER).forwardable(),
        MethodSpec::unary(LOGOUT_PI_PROVIDER).forwardable(),
        MethodSpec::unary(REMOVE_PI_PROVIDER).forwardable(),
        MethodSpec::unary(BEGIN_PI_PROVIDER_LOGIN)
            .forwardable()
            .credentials(),
        MethodSpec::unary(PI_PROVIDER_LOGIN_STATUS)
            .forwardable()
            .credentials(),
        MethodSpec::unary(COMPLETE_PI_PROVIDER_LOGIN)
            .forwardable()
            .credentials(),
        MethodSpec::unary(CANCEL_PI_PROVIDER_LOGIN)
            .forwardable()
            .credentials(),
        MethodSpec::stream(PI_UPDATE_STATUS).forwardable(),
        MethodSpec::unary(CHECK_PI_UPDATE).forwardable(),
        MethodSpec::unary(APPLY_PI_UPDATES).forwardable(),
        MethodSpec::unary(LIST_MCP_SERVERS).forwardable(),
        MethodSpec::unary(ADD_MCP_SERVERS)
            .forwardable()
            .credentials(),
        MethodSpec::unary(REMOVE_MCP_SERVER)
            .forwardable()
            .credentials(),
        MethodSpec::unary(SET_MCP_SERVER_ENABLED).forwardable(),
        MethodSpec::unary(START_MCP_AUTH).forwardable(),
        MethodSpec::unary(BEGIN_MCP_LOGIN)
            .forwardable()
            .credentials(),
        MethodSpec::unary(MCP_LOGIN_STATUS)
            .forwardable()
            .credentials(),
        MethodSpec::unary(COMPLETE_MCP_LOGIN)
            .forwardable()
            .credentials(),
        MethodSpec::unary(CANCEL_MCP_LOGIN)
            .forwardable()
            .credentials(),
        MethodSpec::unary(LOGOUT_MCP_SERVER).forwardable(),
        MethodSpec::unary(LIST_MODELS).forwardable(),
        MethodSpec::unary(GET_TITLE_MODEL_SETTINGS).forwardable(),
        MethodSpec::unary(SET_TITLE_MODEL_SETTINGS).forwardable(),
        MethodSpec::unary(GET_WEB_SEARCH_FALLBACK).forwardable(),
        MethodSpec::unary(SET_WEB_SEARCH_FALLBACK).forwardable(),
        MethodSpec::unary(LIST_COMMANDS).forwardable(),
        MethodSpec::unary(PI_SESSION_MODES).forwardable(),
        MethodSpec::unary(QUEUE_COMMAND).forwardable(),
        MethodSpec::unary(RETRY_COMMAND).forwardable(),
        MethodSpec::stream(WATCH_DOC_MESSAGES).forwardable(),
        MethodSpec::stream(WATCH_DOC_COMMANDS).forwardable(),
        MethodSpec::unary(PROBE_SYNC),
        MethodSpec::unary(NOTIFICATION_ACTIVITY).auth(),
        MethodSpec::unary(SYNC_STATUS),
        MethodSpec::stream(WATCH_CHATS),
        MethodSpec::stream(WATCH_DEVICES),
        MethodSpec::stream(WATCH_SESSIONS),
        MethodSpec::stream(WATCH_SPACES),
        MethodSpec::unary(MUTATE),
        MethodSpec::unary(ENGINE_INFO),
        MethodSpec::unary(ENGINE_READY),
        MethodSpec::unary(STOP_ENGINE),
        MethodSpec::stream(AUTH_STATUS).auth(),
        MethodSpec::unary(SIGN_IN).auth(),
        MethodSpec::unary(SIGN_IN_HEADLESS).auth(),
        MethodSpec::unary(COMPLETE_SIGN_IN).auth(),
        MethodSpec::unary(SIGN_OUT).auth(),
        MethodSpec::unary(LIST_ORGS).auth(),
        MethodSpec::unary(CREATE_ORG).auth(),
        MethodSpec::unary(SELECT_ORG).auth(),
        MethodSpec::unary(LOCAL_IMPORT_STATUS),
        MethodSpec::stream(IMPORT_LOCAL_WORKSPACE),
        MethodSpec::unary(LIST_REPOS).forwardable(),
        MethodSpec::unary(ADD_REPO).forwardable(),
        MethodSpec::unary(CLONE_REPO).forwardable(),
        MethodSpec::unary(CREATE_REPO).forwardable(),
        MethodSpec::unary(LIST_BRANCHES).forwardable(),
        MethodSpec::unary(LIST_REFS).forwardable(),
        MethodSpec::unary(LIST_GIT_HISTORY).forwardable(),
        MethodSpec::unary(FETCH_ALL).forwardable(),
        MethodSpec::unary(SWITCH_REF).forwardable(),
        MethodSpec::unary(LIST_FOLDERS).forwardable(),
        MethodSpec::unary(SEARCH_FILES).forwardable(),
        MethodSpec::unary(SEARCH_GITHUB_ISSUES).forwardable(),
        MethodSpec::unary(GET_GITHUB_ISSUE).forwardable(),
        MethodSpec::unary(GITHUB_ACCOUNT_STATUS).forwardable(),
        MethodSpec::unary(START_GITHUB_LOGIN).forwardable(),
        MethodSpec::unary(POLL_GITHUB_LOGIN).forwardable(),
        MethodSpec::unary(CANCEL_GITHUB_LOGIN).forwardable(),
        MethodSpec::unary(SIGN_OUT_GITHUB).forwardable(),
        MethodSpec::unary(LIST_WORKSPACE_FILES).forwardable(),
        MethodSpec::unary(READ_WORKSPACE_FILE).forwardable(),
        MethodSpec::unary(WRITE_WORKSPACE_FILE).forwardable(),
        MethodSpec::unary(CREATE_WORKTREE).forwardable(),
        MethodSpec::unary(DELETE_WORKTREE).forwardable(),
        MethodSpec::unary(CREATE_SCRATCH_DIR).forwardable(),
        MethodSpec::unary(DELETE_SCRATCH_DIR).forwardable(),
        MethodSpec::unary(OPEN_TERMINAL).forwardable(),
        MethodSpec::stream(SUBSCRIBE_TERMINAL).forwardable(),
        MethodSpec::unary(WRITE_TERMINAL).forwardable(),
        MethodSpec::unary(RESIZE_TERMINAL).forwardable(),
        MethodSpec::unary(CLOSE_TERMINAL).forwardable(),
        MethodSpec::stream(WATCH_CHECKOUT_DIFFS).forwardable(),
        MethodSpec::unary(GET_CHECKOUT_DIFF).forwardable(),
        MethodSpec::unary(GET_CHECKOUT_FILE_DIFF_TEXT).forwardable(),
        MethodSpec::unary(UPLOAD_CHUNK).forwardable(),
        MethodSpec::unary(UPLOAD_COMMIT).forwardable(),
        MethodSpec::unary(READ_ATTACHMENT_CHUNK).forwardable(),
        MethodSpec::unary(FETCH_TOOL_BLOB),
        MethodSpec::stream(UPDATE_STATUS).forwardable(),
        MethodSpec::unary(CHECK_UPDATE).forwardable(),
        MethodSpec::unary(UPDATE_ON_ACTIVATION).forwardable(),
        MethodSpec::unary(APPLY_UPDATE).forwardable(),
        MethodSpec::unary(START_SUBAGENT),
        MethodSpec::stream(WATCH_AGENT_EVENTS),
        MethodSpec::unary(START_SIDE_CHAT).forwardable(),
        MethodSpec::unary(SEND_SIDE_CHAT).forwardable(),
        MethodSpec::unary(INTERRUPT_SIDE_CHAT).forwardable(),
        MethodSpec::unary(RESPOND_SIDE_CHAT_INPUT).forwardable(),
        MethodSpec::stream(WATCH_SIDE_CHAT_STATUS).forwardable(),
        MethodSpec::unary(PROMOTE_SIDE_CHAT).forwardable(),
        MethodSpec::unary(DISPOSE_SIDE_CHAT).forwardable(),
        MethodSpec::unary(FORK_SESSION).forwardable(),
        MethodSpec::unary(REWIND_SESSION).forwardable(),
    ];

    /// The spec for `name`, or `None` for an unknown method.
    pub fn spec(name: &str) -> Option<&'static MethodSpec> {
        SPECS.iter().find(|spec| spec.name == name)
    }

    #[cfg(test)]
    mod tests {
        use super::{SPECS, spec};

        /// The method constants as declared in this module's source.
        fn declared_methods() -> Vec<String> {
            let source = include_str!("lib.rs");
            let start = source.find("pub mod methods {").unwrap();
            let end = source[start..].find("pub struct MethodSpec").unwrap() + start;
            source[start..end]
                .lines()
                .filter_map(|line| {
                    let rest = line.trim().strip_prefix("pub const ")?;
                    let (_, value) = rest.split_once(": &str = \"")?;
                    Some(value.strip_suffix("\";")?.to_string())
                })
                .collect()
        }

        #[test]
        fn every_method_constant_has_exactly_one_spec() {
            let declared = declared_methods();
            assert!(
                declared.len() > 100,
                "the source scan found {}",
                declared.len()
            );
            for name in &declared {
                let count = SPECS.iter().filter(|spec| spec.name == name).count();
                assert_eq!(count, 1, "{name} must have exactly one spec");
            }
            for spec in SPECS {
                assert!(
                    declared.iter().any(|name| name == spec.name),
                    "{} has a spec but no constant",
                    spec.name
                );
            }
            assert_eq!(SPECS.len(), declared.len());
            assert!(spec("NoSuchMethod").is_none());
        }

        #[test]
        fn routing_flags_are_consistent() {
            for spec in SPECS {
                assert!(
                    !spec.credentials || spec.forwardable,
                    "{}: only relayed methods need a credential transport",
                    spec.name
                );
                assert!(
                    !(spec.auth && spec.forwardable),
                    "{}: auth methods answer for the local engine only",
                    spec.name
                );
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("bad params: {0}")]
    BadParams(String),
    #[error("{0}")]
    Failed(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("connection closed")]
    Closed,
}

/// A client-originated frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientFrame {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancel: bool,
}

/// A server-originated frame. Exactly one of `ok` / `err` / `item` / `done` is meaningful.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerFrame {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub done: bool,
}

/// What a service returns for one invocation.
pub enum RpcReply {
    /// Unary response — sent as `{id, ok}`.
    Value(serde_json::Value),
    /// Stream — each item sent as `{id, item}`, then `{id, done: true}` when it ends.
    Stream(BoxStream<'static, serde_json::Value>),
}

impl RpcReply {
    /// Serialize a value into a unary reply.
    pub fn value<T: Serialize>(value: &T) -> Result<Self, RpcError> {
        serde_json::to_value(value)
            .map(RpcReply::Value)
            .map_err(|e| RpcError::Failed(format!("serialize response: {e}")))
    }

    /// The `{"ok": true}` acknowledgement.
    pub fn ok() -> Result<Self, RpcError> {
        Ok(RpcReply::Value(serde_json::json!({ "ok": true })))
    }
}

/// Server-side dispatch: one implementation serves every transport.
#[async_trait]
pub trait RpcService: Send + Sync + 'static {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError>;
}

/// Deserialize typed params out of the envelope's `params` value.
pub fn parse_params<T: serde::de::DeserializeOwned>(
    params: serde_json::Value,
) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))
}

/// Spawn an in-memory server for `service` and return a connected client.
/// Same envelopes, same dispatch loop as the WebSocket path — the in-process UI
/// transport (ARCHITECTURE §1 "zero serialization shortcuts").
pub fn memory_client(service: Arc<dyn RpcService>) -> RpcClient {
    let (client_out, server_in) = tokio::sync::mpsc::channel::<String>(256);
    let (server_out, client_in) = tokio::sync::mpsc::channel::<String>(256);
    tokio::spawn(serve_connection(service, server_out, server_in));
    RpcClient::new(client_out, client_in)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    struct TestService;

    #[async_trait]
    impl RpcService for TestService {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                "Echo" => Ok(RpcReply::Value(params)),
                "Count" => {
                    let n = params.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
                    Ok(RpcReply::Stream(
                        futures::stream::iter((0..n).map(|i| serde_json::json!(i))).boxed(),
                    ))
                }
                "Never" => Ok(RpcReply::Stream(futures::stream::pending().boxed())),
                "Boom" => Err(RpcError::Failed("boom".into())),
                other => Err(RpcError::UnknownMethod(other.into())),
            }
        }
    }

    #[tokio::test]
    async fn memory_call_stream_and_error() {
        let client = memory_client(Arc::new(TestService));

        let echoed = client
            .call("Echo", serde_json::json!({"x": 1}))
            .await
            .unwrap();
        assert_eq!(echoed, serde_json::json!({"x": 1}));

        let mut items = client
            .subscribe("Count", serde_json::json!({"n": 3}))
            .await
            .unwrap();
        let mut seen = Vec::new();
        while let Some(v) = items.recv().await {
            seen.push(v);
        }
        assert_eq!(
            seen,
            vec![
                serde_json::json!(0),
                serde_json::json!(1),
                serde_json::json!(2)
            ]
        );

        let err = client
            .call("Boom", serde_json::Value::Null)
            .await
            .unwrap_err();
        assert!(matches!(err, RpcError::Failed(m) if m == "boom"));
    }

    #[tokio::test]
    async fn websocket_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = cypher_env::ipc_socket(dir.path()).unwrap();
        let listener = LocalListener::bind(&path).await.unwrap();
        let server = tokio::spawn(listener.serve(Arc::new(TestService)));

        let client = connect_local(&path).await.unwrap();
        let echoed = client
            .call("Echo", serde_json::json!("hello"))
            .await
            .unwrap();
        assert_eq!(echoed, serde_json::json!("hello"));

        let mut items = client
            .subscribe("Count", serde_json::json!({"n": 2}))
            .await
            .unwrap();
        assert_eq!(items.recv().await, Some(serde_json::json!(0)));
        assert_eq!(items.recv().await, Some(serde_json::json!(1)));
        assert_eq!(items.recv().await, None);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn handshake_with_origin_header_is_rejected() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let dir = tempfile::tempdir().unwrap();
        let path = cypher_env::ipc_socket(dir.path()).unwrap();
        let listener = LocalListener::bind(&path).await.unwrap();
        let server = tokio::spawn(listener.serve(Arc::new(TestService)));

        // A browser page opening ws://127.0.0.1:{port} always sends Origin;
        // the server must refuse the handshake before serving any RPC.
        let mut req = "ws://localhost/ipc".into_client_request().unwrap();
        req.headers_mut()
            .insert("origin", "https://evil.example".parse().unwrap());
        req.headers_mut()
            .insert("sec-websocket-protocol", "cypher.rpc.v1".parse().unwrap());
        let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
        let result = tokio_tungstenite::client_async(req, stream).await;
        assert!(
            result.is_err(),
            "handshake carrying an Origin header must be rejected"
        );

        // A native viewport (no Origin) still connects and can call RPC — the
        // reject must not be a blanket denial.
        let client = connect_local(&path).await.unwrap();
        let echoed = client.call("Echo", serde_json::json!("ok")).await.unwrap();
        assert_eq!(echoed, serde_json::json!("ok"));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn dropping_stream_receiver_cancels_server_side() {
        let client = memory_client(Arc::new(TestService));
        let items = client
            .subscribe("Never", serde_json::Value::Null)
            .await
            .unwrap();
        drop(items);
        // The next unary call still works — the dead stream didn't wedge the connection.
        let echoed = client.call("Echo", serde_json::json!(2)).await.unwrap();
        assert_eq!(echoed, serde_json::json!(2));
    }
}
