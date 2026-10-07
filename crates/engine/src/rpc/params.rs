//! Request parameter shapes for the engine RPC surface.

use serde::Deserialize;

use cypher_doc::SessionCommandPayload;
use cypher_proto::{ChatConfig, HarnessId, SubagentRunMode};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChatParams {
    pub(super) chat_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListModelsParams {
    pub(super) harness: HarnessId,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PiSessionModesParams {
    #[serde(default)]
    pub(super) chat_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DetectPiLanguageParams {
    pub(super) text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SetHarnessEnabledParams {
    pub(super) harness: HarnessId,
    pub(super) enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PiPackageParams {
    pub(super) source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QueueCommandParams {
    pub(super) chat_id: String,
    pub(super) command: SessionCommandPayload,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RetryCommandParams {
    pub(super) chat_id: String,
    pub(super) command_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RepoPathParams {
    /// `repoPath` per §3.5 (the §2.1 shorthand `repo` is accepted as an alias).
    #[serde(alias = "repo")]
    pub(super) repo_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SwitchRefParams {
    /// The checkout to switch — a session's cwd (main folder or worktree).
    pub(super) repo_path: String,
    pub(super) ref_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateWorktreeParams {
    #[serde(alias = "repo")]
    pub(super) repo_path: String,
    pub(super) branch: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeleteWorktreeParams {
    #[serde(alias = "repo")]
    pub(super) repo_path: String,
    #[serde(alias = "path")]
    pub(super) worktree_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListFoldersParams {
    #[serde(default)]
    pub(super) path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileSearchParams {
    pub(super) query: String,
    #[serde(default)]
    pub(super) chat_id: Option<String>,
    #[serde(default)]
    pub(super) space_id: Option<String>,
    /// Existing linked worktree selected for a new chat. The engine accepts it
    /// only after verifying it against the space repository's worktree list.
    #[serde(default)]
    pub(super) path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkspaceFileParams {
    pub(super) chat_id: String,
    /// Optimistic context check: never silently read a newly switched checkout.
    pub(super) cwd: String,
    #[serde(default)]
    pub(super) path: String,
    /// `WriteWorkspaceFile` only: the full replacement text.
    #[serde(default)]
    pub(super) text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OpenTerminalParams {
    pub(super) chat_id: String,
    pub(super) cols: u16,
    pub(super) rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TerminalIdParams {
    pub(super) terminal_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SubscribeTerminalParams {
    pub(super) terminal_id: String,
    #[serde(default)]
    pub(super) after_seq: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WriteTerminalParams {
    pub(super) terminal_id: String,
    /// Base64 input bytes (plain UTF-8 accepted leniently).
    pub(super) data: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ResizeTerminalParams {
    pub(super) terminal_id: String,
    pub(super) cols: u16,
    pub(super) rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListAgentAccountsParams {
    #[serde(default)]
    pub(super) force_usage: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AgentAccountParams {
    pub(super) harness: HarnessId,
    pub(super) account_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StartAgentLoginParams {
    pub(super) harness: HarnessId,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LoginIdParams {
    pub(super) login_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CompleteAgentLoginParams {
    pub(super) login_id: String,
    pub(super) code: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UploadChunkParams {
    pub(super) upload_id: String,
    /// Base64 payload chunk.
    pub(super) data: String,
    #[serde(default)]
    pub(super) seq: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UploadCommitParams {
    pub(super) upload_id: String,
    pub(super) file_name: String,
    /// Chat to seal the committed attachment against (queue-first sends: the
    /// host records the durable final path so a waiting Run can execute).
    /// Additive + defaulted — old clients commit without sealing.
    #[serde(default)]
    pub(super) chat_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReadAttachmentChunkParams {
    pub(super) path: String,
    #[serde(default)]
    pub(super) offset: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FetchToolBlobParams {
    /// Doc-resident sidecar ref (`{chatId}/{partId}` or `…​.diff`).
    pub(super) blob_ref: String,
}

/// `StartSubagent` params — the Cypher bridge's bounded start request. All
/// string fields are length-checked at the handler (see the bounds below) so
/// a misbehaving publisher can never mint an unbounded persisted row.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StartSubagentParams {
    pub(super) parent_chat_id: String,
    /// Parent `cypher.subagents.v1` run id — the idempotence key with
    /// `parent_chat_id`.
    pub(super) run_id: String,
    pub(super) agent: String,
    /// Short task label for the child row (title / Inspector); ≤500 chars.
    pub(super) task: String,
    /// The FULL task text for the initial run when it outgrows `task` (the
    /// extension accepts tasks up to 64 KiB). Absent: `task` is the prompt.
    #[serde(default)]
    pub(super) prompt: Option<String>,
    pub(super) mode: SubagentRunMode,
    /// Parent tool call id this run answers to (sync/async); persisted on the
    /// child row as the durable link to the parent's transcript part.
    #[serde(default)]
    pub(super) tool_call_id: Option<String>,
    /// Optional cwd override; defaults to the parent's cwd.
    #[serde(default)]
    pub(super) cwd: Option<String>,
    /// Persisted child agent profile (reapplied on later child turns).
    pub(super) system_prompt: String,
    #[serde(default)]
    pub(super) tools: Vec<String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) thinking: Option<String>,
    /// Messaging-channel root (the parent extension's `messageRoot`).
    pub(super) message_root: String,
    #[serde(default)]
    pub(super) child_index: u32,
    /// Messaging address of this run when it is not the agent name; host-local
    /// like the channel it names, so it is never written to the synced row.
    #[serde(default)]
    pub(super) address: Option<String>,
}

/// `SavePiSubagent` params: the edited profile, plus the name the editor was
/// opened on so a rename can retire the old file. Absent `originalName` means
/// "create", which refuses to overwrite an existing profile.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SavePiSubagentParams {
    #[serde(flatten)]
    pub(super) agent: crate::pi_subagents::PiSubagent,
    #[serde(default)]
    pub(super) original_name: Option<String>,
}

/// `DeletePiSubagent` params.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeletePiSubagentParams {
    pub(super) name: String,
}

/// The Mutate surface (DataRpc), tagged by `op`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub(super) enum MutateParams {
    #[serde(rename_all = "camelCase")]
    CreateChat {
        chat_id: String,
        /// The project the chat is created in — fixes host device + base cwd.
        /// `None` mints a project-less chat: `deviceId` picks the host and the
        /// cwd defaults to `~` (expanded on the host at run time).
        #[serde(default)]
        space_id: Option<String>,
        /// Host device for a project-less chat; ignored when `spaceId` is set.
        #[serde(default)]
        device_id: Option<String>,
        #[serde(default)]
        config: Option<ChatConfig>,
        /// The picked ref, named on the row from the first frame (the footer
        /// read "Select ref" until the diff reconciler stamped it).
        #[serde(default)]
        branch: Option<String>,
        /// Cwd override (isolated-worktree path); default = the space's folder.
        #[serde(default)]
        cwd: Option<String>,
    },
    /// Create a space (device + folder pair). Idempotent by id; a live
    /// duplicate `(deviceId, path)` no-ops. `gitDetected` is seeded from the
    /// picker's FolderEntry — the owning device's SpacesSync re-verifies.
    #[serde(rename_all = "camelCase")]
    CreateSpace {
        space_id: String,
        device_id: String,
        path: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        git_detected: bool,
    },
    /// LWW display-name set; `name: None` clears back to basename(path).
    #[serde(rename_all = "camelCase")]
    RenameSpace {
        space_id: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Hard delete: cascades to every chat (and session row) in the space.
    /// Live runs hosted here are interrupted best-effort.
    #[serde(rename_all = "camelCase")]
    DeleteSpace { space_id: String },
    #[serde(rename_all = "camelCase")]
    RenameChat { chat_id: String, title: String },
    /// Set the chat's checkout branch label — the sidebar's
    /// "project · branch" sub-line.
    #[serde(rename_all = "camelCase")]
    SetChatBranch { chat_id: String, branch: String },
    /// Retarget a chat onto another folder — mid-session switch to an
    /// EXISTING worktree (the picked ref's checkout). Next run starts a
    /// fresh harness conversation there (resume is cwd-scoped).
    #[serde(rename_all = "camelCase")]
    SetChatCwd { chat_id: String, cwd: String },
    /// Backdate a chat's activity timestamps (epoch ms) — the sidebar's
    /// relative-time column. Used by tooling/seeds; the doc fold sets these on
    /// real message traffic.
    #[serde(rename_all = "camelCase")]
    SetChatActivity {
        chat_id: String,
        #[serde(default)]
        last_message_at: Option<i64>,
        #[serde(default)]
        created_at: Option<i64>,
    },
    /// Re-home a chat to another device (tooling/seeds; device migration later).
    #[serde(rename_all = "camelCase")]
    SetChatHost { chat_id: String, device_id: String },
    #[serde(rename_all = "camelCase")]
    SetChatArchived { chat_id: String, archived: bool },
    /// User pins (synced LWW): pinned projects lead the sidebar, pinned
    /// sessions lead their project's list.
    #[serde(rename_all = "camelCase")]
    SetChatPinned { chat_id: String, pinned: bool },
    #[serde(rename_all = "camelCase")]
    SetSpacePinned { space_id: String, pinned: bool },
    /// Sidebar glyph + colour keys for a project (synced LWW; absent/null
    /// clears to the default).
    #[serde(rename_all = "camelCase")]
    SetSpaceAppearance {
        space_id: String,
        #[serde(default)]
        icon: Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
    /// Full-config replace on the chat row (zeron `SetChatConfig`): the
    /// composer's mid-session model / reasoning / options changes, LWW-synced
    /// so they survive restarts and reach every device.
    #[serde(rename_all = "camelCase")]
    SetChatConfig { chat_id: String, config: ChatConfig },
    /// Tombstone: removes the chats-map row; the session doc remains.
    #[serde(rename_all = "camelCase")]
    DeleteChat { chat_id: String },
    #[serde(rename_all = "camelCase")]
    RenameDevice { device_id: String, name: String },
    /// Unpair a device: tombstones its registry row so it drops out of sync
    /// and continues in local-only mode. Refused for THIS device.
    #[serde(rename_all = "camelCase")]
    DeleteDevice { device_id: String },
    /// Synced seen marker (LWW + monotonic guard): clears the "completed"
    /// badge on every device. `at` is epoch ms; default = now.
    #[serde(rename_all = "camelCase")]
    MarkChatSeen {
        chat_id: String,
        #[serde(default)]
        at: Option<i64>,
    },
    /// Development builds only: upsert a fake peer device row (mock data for
    /// judging multi-device / offline-host UI). `lastSeenAt` is epoch ms;
    /// omitted = never seen, i.e. offline.
    #[cfg(feature = "development")]
    #[serde(rename_all = "camelCase")]
    SeedDevice {
        device_id: String,
        name: String,
        #[serde(default = "seed_device_platform")]
        platform: String,
        #[serde(default)]
        last_seen_at: Option<i64>,
    },
}

#[cfg(feature = "development")]
fn seed_device_platform() -> String {
    "linux".to_string()
}
