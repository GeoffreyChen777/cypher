//! Pi harness integration: the Cypher-owned runtime install, packages,
//! providers, subagents, session modes, translation, permissions, the
//! appended system prompt and the web-search fallback extension.

pub mod packages;
pub mod permissions;
pub mod providers;
pub mod runtime;
pub mod session_modes;
pub mod subagents;
pub(crate) mod system_prompt;
pub mod translation;
pub(crate) mod web_search;
