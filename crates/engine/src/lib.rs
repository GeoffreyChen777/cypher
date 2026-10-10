//! cypher-engine — the headless backend: sessions engine, doc host + command executor,
//! run journal + crash recovery, and the IPC RPC server.
//!
//! Spec: ARCHITECTURE.md §5. Also hosts terminals, repos/diffs, uploads, auth, and
//! the device-room relay.
//!
//! The root re-exports are what an app needs to assemble, lock, authenticate
//! and serve an engine; everything else is addressed by module path
//! (`pi::packages`, `session::forks`, `host::local_import`, …).

mod auth;
mod core;
mod device_identity;
mod error;
pub mod git;
mod headless;
pub mod host;
mod instance_lock;
pub mod mcp;
pub mod pi;
mod profile;
pub mod registry;
pub mod rpc;
mod runtime;
pub mod session;
mod terminals;
mod uploads;
mod util;

pub use cypher_proto::{EngineInfo, HarnessId, WorkspaceScope};

pub use auth::{Auth, AuthConfig, AuthState, AuthUser, OrgMembership};
pub use core::EngineCore;
pub use error::EngineError;
pub use git::diff_sync::{
    CheckoutDiffSync, capture_commit_diff, capture_diff, capture_diff_against, capture_turn_diff,
    merge_base, read_diff_file_text, snapshot_tree, working_diff_base,
};
pub use git::repos::{Repos, worktree_branch_from_title};
pub use host::doc_host::{ChatDocHandle, DocHost, DocHostConfig, EdgeConfig};
pub use host::spaces::SpacesSync;
pub use host::workspace_host::{DEFAULT_ORG_ID, DEFAULT_USER_ID, WorkspaceHost};
pub use instance_lock::InstanceLock;
pub use profile::EngineProfile;
pub use registry::{
    HarnessDescriptor, HarnessRegistry, default_registry, default_registry_with_bridge,
};
pub use runtime::{Engine, EngineConfig, EngineRuntime, serve_ipc, shutdown_signal};
pub use session::engine::{QuiesceWindows, SessionsEngine, SteerOutcome};
pub use session::forks::SessionForks;
pub use session::journal::RunJournal;
pub use session::side_chats::bounded_transcript_context;
pub use terminals::Terminals;
pub use uploads::Uploads;
