//! Doc hosting: chat docs and their command executor, the workspace registry
//! doc, spaces, presence/viewport activity, notification events and the
//! local→synced import.

pub mod chat2_host;
pub(crate) mod doc_host;
pub mod local_import;
pub(crate) mod notification_events;
pub(crate) mod spaces;
pub(crate) mod viewport_activity;
pub(crate) mod workspace_host;
