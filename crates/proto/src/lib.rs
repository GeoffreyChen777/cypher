//! cypher-proto — wire types shared by engine, UI, and RPC.
//!
//! Ported from zeron's `packages/control/src/wire.ts` + `packages/harness/src/types.ts`.
//! Per-turn token accounting is excluded by design; the `Usage` agent event is kept as a
//! harness-level passthrough (rate-limit meters), never persisted into docs. The one
//! usage surface is the live context-window gauge ([`ContextUsage`]), which rides the
//! engine's local session projection and is never written to a synced doc.

pub mod agent;
pub mod agent_prompt;
pub mod entities;
pub mod motion;
pub mod scratch;
pub mod session_fork;
pub mod side_chat;
pub mod view;
pub mod workspace;

pub use agent::*;
pub use entities::*;
pub use session_fork::*;
pub use side_chat::*;
pub use workspace::*;
