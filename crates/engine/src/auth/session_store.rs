//! The persisted session file, the access-token cache and the sync-rejoin
//! marker.

use super::*;

/// The persisted session (refresh token + user + last org scope).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StoredSession {
    pub(super) refresh_token: String,
    pub(super) user: AuthUser,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) org_id: Option<String>,
}

/// Access-token cache. Expiry ages the token's own lifetime (`exp - iat`) by
/// BOTH clocks, pessimistically. Monotonic alone (`Instant`) freezes across
/// system sleep (macOS `mach_absolute_time` and Linux `CLOCK_MONOTONIC` both
/// exclude suspend), so a laptop waking from hours of sleep presented a
/// wall-expired token that still read "fresh" — every room/relay redial got a
/// 401 with the same stale bearer and sync never recovered (user report).
/// Wall clock alone breaks under skewed device clocks (`exp` vs local time);
/// the elapsed-since-issue reading is skew-immune, and a BACKWARD wall step
/// (NTP correction) degrades harmlessly to the monotonic reading.
pub(super) struct AccessEntry {
    pub(super) token: String,
    pub(super) ttl: Duration,
    pub(super) got_at: Instant,
    pub(super) got_wall: std::time::SystemTime,
}

impl AccessEntry {
    pub(super) fn fresh(token: String) -> Self {
        let ttl = jwt_claims(&token)
            .and_then(|c| match (c.exp, c.iat) {
                (Some(exp), Some(iat)) if exp > iat => {
                    Some(Duration::from_secs((exp - iat) as u64))
                }
                _ => None,
            })
            .unwrap_or(Duration::from_secs(240));
        Self {
            token,
            ttl,
            got_at: Instant::now(),
            got_wall: std::time::SystemTime::now(),
        }
    }

    pub(super) fn remaining(&self) -> Duration {
        let monotonic = self.got_at.elapsed();
        let wall = std::time::SystemTime::now()
            .duration_since(self.got_wall)
            .unwrap_or(Duration::ZERO);
        self.ttl.saturating_sub(monotonic.max(wall))
    }
}

impl Auth {
    pub(super) fn session_file(&self) -> PathBuf {
        self.inner.config.data_dir.join("session.json")
    }

    /// Persist (0600) or remove the stored session. Never panics: a disk error degrades
    /// to a logged warning, not a crash mid-refresh.
    pub(super) fn persist<S: std::borrow::Borrow<StoredSession>>(&self, session: Option<S>) {
        let path = self.session_file();
        let outcome = match session {
            Some(session) => serde_json::to_vec(session.borrow())
                .map_err(std::io::Error::other)
                .and_then(|bytes| write_private(&path, &bytes)),
            None => match std::fs::remove_file(&path) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            },
        };
        if let Err(err) = outcome {
            tracing::warn!(error = %err, "auth: failed to persist session");
        }
    }
}

pub(super) const SYNC_REJOIN_MARKER: &str = "sync-rejoin";

/// Written on a successful sign-in so the next synced runtime may revive a
/// tombstoned device row (the user is explicitly re-pairing this machine).
pub(crate) fn mark_sync_rejoin(data_dir: &Path) {
    if let Err(err) = std::fs::write(data_dir.join(SYNC_REJOIN_MARKER), b"1") {
        tracing::warn!(error = %err, "auth: failed to write sync-rejoin marker");
    }
}

/// Consume the one-shot rejoin marker. `true` = this boot should announce
/// even if the registry still has a tombstone for this device.
pub(crate) fn consume_sync_rejoin(data_dir: &Path) -> bool {
    let path = data_dir.join(SYNC_REJOIN_MARKER);
    match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            tracing::warn!(error = %err, "auth: failed to consume sync-rejoin marker");
            false
        }
    }
}

/// Write a file readable only by the owner (0600). On non-unix targets a plain write.
pub(super) fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // An existing file keeps its old mode through OpenOptions — enforce 0600 anyway.
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}
