//! Git-backed features: repositories and worktrees, checkout diffs, GitHub
//! sign-in, chat ref previews and workspace file listing.

pub(crate) mod diff_sync;
pub mod github;
pub(crate) mod ref_previews;
pub mod repos;
pub(crate) mod workspace_files;
