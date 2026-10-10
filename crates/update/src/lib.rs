//! cypher-update — release checking and self-update, shared by the engine (the
//! background checker + `ApplyUpdate`), the CLI (`cypher update`), and the UI
//! (the sidebar update strip + macOS bundle swap).
//!
//! Release layout (see `.github/workflows/{linux,macos}.yml` and
//! `apps/edge/src/install.sh`): artifacts live in the `cypher-releases` R2 bucket,
//! served pre-auth at `{edge}/releases/*`. Platforms publish independently, so
//! each has its own channel: `{platform}/manifest.json` carries that platform's
//! version, build, per-artifact sha256 and the role→file-name mapping used to
//! resolve a download. The shared `manifest.json` and `latest.txt` remain as
//! fallbacks for clients and channels predating the per-platform split; they
//! only ever name a version that every desktop platform covers at build 1.
//!
//! Install kinds and their update paths:
//! - **Managed** (`~/.cypher/app/<ver>` + `current` symlink — the curl|sh
//!   installer): download the headless tarball into a new versioned dir, flip
//!   the symlink, restart the service. Same flow the installer script performs,
//!   natively.
//! - **MacApp** (running out of a `Cypher.app` bundle): download the app
//!   tarball, swap the bundle directory, relaunch. Driven by the UI.
//! - **Unmanaged** (source builds, hand-copied binaries): report only.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, bail};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::watch;

/// The version compiled into this binary (the workspace version).
pub const fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Background check cadence.
/// Platforms publish independently, so a release aimed at one platform should
/// not sit unnoticed for most of a day. The manifest is small and the edge
/// serves it with `max-age=60`, so checking hourly costs almost nothing.
const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);
/// Retry sooner after a failed check (offline boot, transient edge error).
const CHECK_RETRY: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// First check waits out engine boot (room joins, doc re-sync).
const CHECK_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_secs(20);
/// While an auto-apply is deferred behind active sessions, re-probe idleness
/// this often.
const IDLE_RECHECK: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Floor on how often *coming back to the app* may trigger a check. Focus
/// changes are frequent and user-driven; without this, alt-tabbing would poll
/// the release endpoint continuously.
const ACTIVATION_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(5 * 60);

mod checker;
mod download;
mod install;
mod macos;
mod managed;
mod release;

pub use checker::*;
use download::*;
pub use install::*;
pub use macos::*;
pub use managed::*;
pub use release::*;

/// Lock a mutex, ignoring poisoning: the guarded slots stay consistent
/// across every critical section.
pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
