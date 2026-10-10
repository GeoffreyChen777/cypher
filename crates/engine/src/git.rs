//! Git-backed features: repositories and worktrees, checkout diffs, GitHub
//! sign-in and workspace file listing.

pub(crate) mod diff_sync;
pub mod github;
pub mod repos;
pub(crate) mod workspace_files;
