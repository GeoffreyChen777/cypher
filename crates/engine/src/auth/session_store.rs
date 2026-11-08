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
    pub(super) fn session_secret(&self) -> cypher_secrets::Secret {
        session_secret(&self.inner.config.data_dir)
    }

    /// Persist or remove the stored session. Never panics: a storage error degrades
    /// to a logged warning, not a crash mid-refresh.
    pub(super) fn persist<S: std::borrow::Borrow<StoredSession>>(&self, session: Option<S>) {
        let secret = self.session_secret();
        let outcome = match session {
            Some(session) => serde_json::to_vec(session.borrow())
                .map_err(std::io::Error::other)
                .and_then(|bytes| secret.write(&bytes)),
            None => secret.delete(),
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

/// The stored session holds the WorkOS refresh token: a
/// [`cypher_secrets::Secret`] whose home file is `{data_dir}/session.json`.
pub(crate) fn session_secret(data_dir: &Path) -> cypher_secrets::Secret {
    cypher_secrets::Secret::new(data_dir.join("session.json"))
}

/// Read the stored session; unreadable or malformed reads as none.
pub(super) fn load_session(data_dir: &Path) -> Option<StoredSession> {
    let bytes = match session_secret(data_dir).read() {
        Ok(bytes) => bytes?,
        Err(err) => {
            tracing::warn!(error = %err, "auth: stored session unreadable");
            return None;
        }
    };
    serde_json::from_slice(&bytes).ok()
}
