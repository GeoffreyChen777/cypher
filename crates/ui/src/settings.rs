//! The Settings pages, one module per page plus the shared page widgets.
//! What they edit lives in [`crate::prefs`] (client-local files) or behind
//! engine RPCs.

pub mod appearance;
pub mod archived;
pub mod commands;
pub mod device_target;
pub mod devices;
pub mod github;
pub mod harnesses;
pub mod mcp;
pub mod notifications;
pub mod providers;
pub mod setup;
pub mod shortcuts;
pub mod subagents;
pub mod titles;
pub mod translation;
pub mod web_search;
pub mod widgets;
