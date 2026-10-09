//! cypher-proto — wire types shared by engine, UI, and RPC.
//!
//! Ported from zeron's `packages/control/src/wire.ts` + `packages/harness/src/types.ts`.
//! Per-turn token accounting is excluded by design; the `Usage` agent event is kept as a
//! harness-level passthrough (rate-limit meters), never persisted into docs. The one
//! usage surface is the live context-window gauge ([`ContextUsage`]), which rides the
//! engine's local session projection and is never written to a synced doc. The
//! working trailer's tok/s ([`Throughput`]) is an estimate on the same local-only
//! path, never persisted anywhere.

mod agent;
pub mod agent_prompt;
pub mod attachment_refs;
mod entities;
mod github;
pub mod motion;
pub mod scratch;
mod session_fork;
mod side_chat;
pub mod view;
mod workspace;

pub use agent::*;
pub use entities::*;
pub use github::*;
pub use session_fork::*;
pub use side_chat::*;
pub use workspace::*;
