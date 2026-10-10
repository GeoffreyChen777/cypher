//! Presence: the 15s heartbeat overlay on device rows, the deaf-socket
//! tripwire, and the relay-status probe that double-checks stale peers.

use super::*;

/// A presence heartbeat younger than this marks the device alive (3 missed
/// beats = offline). Also the "peer is reachable" signal that clears the
/// peer-dial cooldown.
pub(super) const PRESENCE_FRESH_MS: i64 = 45_000;
/// Relay-status probe cadence. Presence heartbeats ride the registry room, so
/// any registry pathology (or our own room connection being down) silently
/// starves them — and every device looks offline while its relay works fine.
/// Before believing "offline", ask the device's DeviceRoom
/// (`GET /device/{id}/status` → `hostConnected`), which tracks the host socket
/// authoritatively and shares no machinery with the registry room. Probes only
/// run for devices whose heartbeat is stale, so the steady state (healthy
/// room, fresh beats) sends no extra traffic.
pub(super) const RELAY_PROBE_INTERVAL_MS: u64 = 30_000;
/// Per-request timeout for a relay-status probe.
const RELAY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Ceiling on the negative-cache delay for a peer that keeps answering
/// `hostConnected=false`. This sets the floor rate of the whole probe path:
/// an org's long-offline devices are polled forever at exactly this interval,
/// once per running engine, and that was the account's second largest source
/// of Durable Object requests. A returning device does not wait it out — its
/// presence beat clears the backoff the moment it arrives, and a registry
/// reconnect, a foreground retry, or a system wake re-probes at once — so the
/// cap only bounds the FALLBACK path, for the case where presence itself is
/// unavailable.
pub(super) const RELAY_PROBE_BACKOFF_CAP: std::time::Duration =
    std::time::Duration::from_secs(1_800);

/// Minimum spacing between honored probe resets (foreground retry, system
/// wake, registry reconnect). Window activation alone fires a reset many
/// times an hour; each one used to clear every backoff, re-probing all stale
/// peers on the spot and restarting their ladders at 30s — which kept two
/// device rooms at ~900 `/status` requests a day each.
const RELAY_PROBE_RESET_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// Ceiling for re-verifying a device that keeps answering `hostConnected=true`.
/// Lower than the offline cap: a live device is worth checking on more often
/// than one already known to be away.
pub(super) const RELAY_PROBE_ALIVE_CAP: std::time::Duration = std::time::Duration::from_secs(300);

/// Only an explicit hostConnected=false earns negative-cache backoff. Network
/// failures are not evidence that a device is offline. This is runtime-local;
/// no presence rows or synchronization protocol change is needed.
pub(super) struct RelayProbeRetry {
    pub(super) delay: std::time::Duration,
    pub(super) retry_at: tokio::time::Instant,
    /// Set when this entry came from a probe that answered `hostConnected=true`,
    /// to the presence timestamp that probe stamped.
    ///
    /// A successful probe refreshes `presence_seen` itself, so without this the
    /// candidate filter would read back our own stamp, mistake it for a genuine
    /// heartbeat, and drop the very backoff we just set — leaving a healthy but
    /// presence-quiet device polled forever at the sweep interval.
    pub(super) verified_at: Option<i64>,
}

impl RelayProbeRetry {
    pub(super) fn offline(previous: Option<&Self>, now: tokio::time::Instant) -> Self {
        let delay = previous
            .filter(|retry| retry.verified_at.is_none())
            .map(|retry| (retry.delay * 2).min(RELAY_PROBE_BACKOFF_CAP))
            .unwrap_or(std::time::Duration::from_millis(RELAY_PROBE_INTERVAL_MS));
        Self {
            delay,
            retry_at: now + delay,
            verified_at: None,
        }
    }

    /// Back off re-verifying a device that answered "alive".
    ///
    /// A successful probe grants only `PRESENCE_FRESH_MS` (45s) of freshness
    /// while the sweep runs every 30s, so before this a device that was alive
    /// but whose presence beat never reached us was re-probed about once a
    /// minute, indefinitely — measured at 71 Durable Object requests/hour in
    /// production, the largest remaining HTTP source after the activity
    /// heartbeat moved onto the presence frame.
    ///
    /// Backing off is safe because this is only ever the FALLBACK path: when
    /// presence works, a real beat clears the entry the moment it arrives and
    /// nothing is probed at all. A registry reconnect, foreground retry and
    /// system wake re-probe it early (rate-limited). The cap bounds only how long a badge may keep
    /// showing "online" for a device that went away while its presence channel
    /// was already broken.
    pub(super) fn alive(previous: Option<&Self>, now: tokio::time::Instant, stamped: i64) -> Self {
        let delay = previous
            .and_then(|retry| retry.verified_at.map(|_| retry.delay))
            .map(|delay| (delay * 2).min(RELAY_PROBE_ALIVE_CAP))
            .unwrap_or(std::time::Duration::from_millis(RELAY_PROBE_INTERVAL_MS));
        Self {
            delay,
            retry_at: now + delay,
            verified_at: Some(stamped),
        }
    }
}

/// Deaf-socket escalation: live peer presence dark this long after the
/// tripwire probe → redial on a fresh socket (see `check_presence_deafness`).
const PRESENCE_DEAF_REDIAL_MS: i64 = 60_000;

/// State for the presence deafness tripwire. Presence heartbeats ride the
/// SAME socket and the same DO broadcast fan-out as row updates, so "peers I
/// was seeing live via this room all went dark at once" is a *delivery*
/// signal, and it is bounded (~45-60s) at zero server cost — every device
/// already heartbeats each 15s. The monotonic seen-cache and the relay
/// status probe deliberately keep devices *fresh-looking* through other
/// paths; they must never feed this tripwire (they'd mask exactly the
/// failure it exists to catch).
#[derive(Default)]
pub(super) struct PresenceWatch {
    /// Armed once at least one OTHER device has been seen live via the
    /// room's presence map this session.
    armed: bool,
    /// Epoch ms when live peers first all went dark (0 = not dark).
    dark_since_ms: i64,
    /// The cheap first response (probe) already fired.
    probed: bool,
}

impl WorkspaceHostInner {
    /// Fold the 15s presence heartbeats into the device rows' `lastSeenAt`
    /// before publishing. The row is written on boot/shutdown ONLY (server-
    /// state hygiene), so without this overlay every device looks offline
    /// ~70s after its boot — and a genuinely dead host is indistinguishable
    /// from slow sync. Fresh remote heartbeats also fire the peer-alive hook
    /// (dial-cooldown reset).
    pub(super) fn overlay_presence(&self, devices: &mut [Device]) {
        let mut alive_peers: Vec<String> = Vec::new();
        {
            // No live room handle is NOT "everyone is offline": the cache (fed
            // by past heartbeats and the relay-status probe) still overlays —
            // a dead registry room must never fake an offline badge for
            // devices whose relay connection is fine.
            let room = lock(&self.room);
            let live_map = room
                .as_ref()
                .map(|room| room.presence())
                .unwrap_or_default();
            let mut seen = lock(&self.presence_seen);
            let now = now_ms();
            let mut live_fresh_peers = 0usize;
            for device in devices.iter_mut() {
                // RegistryClient intentionally exposes REMOTE presence only.
                // The local engine being able to publish this view is itself
                // authoritative proof that its own device is online.
                if device.id == self.config.device_id {
                    device.last_seen_at = chrono::DateTime::<Utc>::from_timestamp_millis(now);
                    continue;
                }
                // Freshest of the live presence entry and the cache: the room
                // map's 30s TTL (and its empty state right after a rejoin)
                // must not erase freshness this engine already witnessed — the
                // device is offline only once heartbeats genuinely stop
                // arriving for the UI's whole online window.
                let live = live_map.get(&device.id).copied();
                if live.is_some_and(|ms| now.saturating_sub(ms) < PRESENCE_FRESH_MS) {
                    live_fresh_peers += 1;
                }
                let cached = seen.get(&device.id).copied();
                let Some(ms) = live.into_iter().chain(cached).max() else {
                    continue;
                };
                seen.insert(device.id.clone(), ms);
                if let Some(at) = chrono::DateTime::<Utc>::from_timestamp_millis(ms)
                    && device.last_seen_at.is_none_or(|prev| prev < at)
                {
                    device.last_seen_at = Some(at);
                }
                if now.saturating_sub(ms) < PRESENCE_FRESH_MS {
                    alive_peers.push(device.id.clone());
                }
            }
            if let Some(room) = room.as_ref() {
                self.check_presence_deafness(room, live_fresh_peers, now);
            }
        }
        if alive_peers.is_empty() {
            return;
        }
        {
            let mut backoff = lock(&self.relay_probe_backoff);
            for id in &alive_peers {
                backoff.remove(id);
            }
        }
        let hook = lock(&self.peer_alive).clone();
        if let Some(hook) = hook {
            for id in &alive_peers {
                hook(id);
            }
        }
    }

    /// The deaf-socket tripwire (see [`PresenceWatch`]). LIVE presence
    /// freshness only — never the seen-cache or relay probe. Escalation
    /// ladder: first all-dark observation → deadline-checked probe (free on a
    /// healthy room); still dark [`PRESENCE_DEAF_REDIAL_MS`] later → fresh-
    /// socket redial (the only cure when the server→client path drops even
    /// probe answers). Disarms after the redial and re-arms when a peer is
    /// next seen live, so a genuinely-offline fleet costs one probe + one
    /// redial, ever.
    fn check_presence_deafness(&self, room: &RegistryClient, live_fresh_peers: usize, now: i64) {
        let mut watch = lock(&self.presence_watch);
        if live_fresh_peers > 0 {
            watch.armed = true;
            watch.dark_since_ms = 0;
            watch.probed = false;
            return;
        }
        if !watch.armed {
            return;
        }
        if watch.dark_since_ms == 0 {
            watch.dark_since_ms = now;
        }
        if !watch.probed {
            tracing::info!(
                "all live peer presence went dark; probing registry room (deaf-socket tripwire)"
            );
            room.probe();
            watch.probed = true;
        } else if now.saturating_sub(watch.dark_since_ms) > PRESENCE_DEAF_REDIAL_MS {
            tracing::warn!(
                dark_ms = now.saturating_sub(watch.dark_since_ms),
                "peer presence still dark after probe; requesting registry room redial"
            );
            room.redial();
            watch.armed = false;
            watch.dark_since_ms = 0;
            watch.probed = false;
        }
    }
}

impl WorkspaceHostInner {
    /// Deliver the viewport's pending activity on the registry socket now, as
    /// an extra presence beat. `false` when there is no live socket to carry
    /// it, which is the caller's cue to spend an HTTP request instead.
    pub(super) fn beat_activity_now(&self) -> bool {
        let Some(activity) = self
            .config
            .edge
            .as_ref()
            .and_then(|edge| edge.viewport_activity.pending())
        else {
            return false;
        };
        lock(&self.room)
            .as_ref()
            .is_some_and(|room| room.beat_with_activity_now(now_ms(), activity))
    }

    /// Presence heartbeat — a memory-only frame on the room, never a row write.
    /// Carries the viewport's pending activity refresh when there is one, so
    /// that refresh costs no request of its own.
    pub(super) fn presence_tick(&self) {
        if let Some(room) = lock(&self.room).as_ref() {
            let pending = self
                .config
                .edge
                .as_ref()
                .and_then(|edge| edge.viewport_activity.pending());
            match pending {
                Some(activity) => room.set_presence_with_activity(now_ms(), activity),
                None => room.set_presence(now_ms()),
            }
        }
    }
}

/// Background task: relay-verified presence. Every [`RELAY_PROBE_INTERVAL_MS`],
/// for each known device whose merged heartbeat freshness has gone stale, ask
/// its DeviceRoom whether the host socket is live (`/device/{id}/status`); a
/// positive answer refreshes the presence cache so the overlay keeps the badge
/// online. The DeviceRoom shares no machinery with the registry room, so a
/// false "offline" now requires BOTH independent paths to be down — at which
/// point the device is, for every purpose the app has, genuinely offline.
/// Steady state (healthy room, fresh heartbeats) probes nothing. Repeated
/// answers back off ([`RELAY_PROBE_BACKOFF_CAP`] offline,
/// [`RELAY_PROBE_ALIVE_CAP`] alive; checked on the 30s sweep). Foreground
/// retry, system wake, and a registry reconnect make every peer due at once,
/// at most every [`RELAY_PROBE_RESET_MIN_INTERVAL`], without restarting its
/// backoff ladder.
pub(super) async fn relay_probe_task(weak: Weak<WorkspaceHostInner>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(RELAY_PROBE_INTERVAL_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // consume the immediate first tick
    let client = reqwest::Client::new();
    let mut system_wake = cypher_sync::wake::subscribe();
    let Some(probe_wake) = weak.upgrade().map(|inner| inner.relay_probe_wake.clone()) else {
        return;
    };
    let mut last_reset: Option<tokio::time::Instant> = None;
    loop {
        let reset = tokio::select! {
            _ = tick.tick() => false,
            _ = probe_wake.notified() => true,
            _ = system_wake.recv() => true,
        };
        let Some(inner) = weak.upgrade() else { return };
        let Some(edge) = inner.config.edge.clone() else {
            return;
        };
        let now = tokio::time::Instant::now();
        if reset
            && last_reset.is_none_or(|at| now.duration_since(at) >= RELAY_PROBE_RESET_MIN_INTERVAL)
        {
            last_reset = Some(now);
            inner.expedite_relay_probes(now);
        }
        let stale = inner.relay_probe_candidates(now);
        drop(inner);
        if stale.is_empty() {
            continue;
        }
        let Some(bearer) = edge.bearer().await else {
            continue; // signed out
        };
        let mut refreshed = false;
        for device_id in stale {
            let url = format!(
                "{}/device/{}/status",
                edge.url.trim_end_matches('/'),
                device_id
            );
            let attempted_at = tokio::time::Instant::now();
            let response = client
                .get(&url)
                .bearer_auth(&bearer)
                .timeout(RELAY_PROBE_TIMEOUT)
                .send()
                .await;
            let Ok(response) = response else { continue };
            let status = response.status();
            let body = if status.is_success() {
                response.json::<serde_json::Value>().await.ok()
            } else {
                None
            };
            let connected = relay_probe_answer(status, body.as_ref());
            let Some(inner) = weak.upgrade() else { return };
            refreshed |= inner.record_relay_probe(&device_id, connected, attempted_at);
        }
        if refreshed && let Some(inner) = weak.upgrade() {
            inner.publish();
        }
    }
}

/// What a `/device/{id}/status` response says about the peer's relay host:
/// `Some(true)` live, `Some(false)` authoritatively not, `None` inconclusive.
///
/// Only an authoritative "not" earns offline backoff. Network failures, 5xx and
/// unparsable bodies stay inconclusive, because they are not evidence about
/// the peer. But two statuses are answers, not failures: 404 means the room has
/// never had an owner, i.e. no host has ever joined it, and 403 means it
/// belongs to someone else. Neither can change by asking again in 30 seconds.
/// Treating them as inconclusive once kept three engines re-probing an
/// unhosted device every sweep, forever -- ~350 billable Durable Object
/// requests an hour, measured in production, and the largest line on the bill
/// once the activity heartbeat moved onto the presence frame. A peer that later
/// starts hosting is not delayed by this: its first presence beat clears the
/// backoff at once.
pub(super) fn relay_probe_answer(
    status: reqwest::StatusCode,
    body: Option<&serde_json::Value>,
) -> Option<bool> {
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::FORBIDDEN {
        return Some(false);
    }
    if !status.is_success() {
        return None;
    }
    body?
        .get("hostConnected")
        .and_then(serde_json::Value::as_bool)
}

impl WorkspaceHostInner {
    pub(super) fn relay_probe_candidates(&self, now: tokio::time::Instant) -> Vec<String> {
        let Ok(devices) = lock(&self.reg).read_devices() else {
            return Vec::new();
        };
        let seen = lock(&self.presence_seen);
        let mut backoff = lock(&self.relay_probe_backoff);
        let known: std::collections::HashSet<_> = devices.iter().map(|d| d.id.as_str()).collect();
        backoff.retain(|id, _| known.contains(id.as_str()));
        let wall_now = now_ms();
        devices
            .into_iter()
            .filter_map(|device| {
                if device.id == self.config.device_id {
                    return None;
                }
                if let Some(at) = seen.get(&device.id).copied()
                    && wall_now.saturating_sub(at) < PRESENCE_FRESH_MS
                {
                    // Only a genuine heartbeat clears the backoff. Freshness a
                    // probe granted itself must not erase that probe's own
                    // backoff, or the device is polled forever.
                    let self_granted = backoff
                        .get(&device.id)
                        .is_some_and(|retry| retry.verified_at == Some(at));
                    if !self_granted {
                        backoff.remove(&device.id);
                    }
                    return None;
                }
                backoff
                    .get(&device.id)
                    .is_none_or(|retry| now >= retry.retry_at)
                    .then_some(device.id)
            })
            .collect()
    }

    /// Make every backed-off peer due now, keeping its delay: the next answer
    /// continues the ladder instead of restarting it at the sweep interval.
    pub(super) fn expedite_relay_probes(&self, now: tokio::time::Instant) {
        for retry in lock(&self.relay_probe_backoff).values_mut() {
            retry.retry_at = retry.retry_at.min(now);
        }
    }

    pub(super) fn record_relay_probe(
        &self,
        device: &str,
        connected: Option<bool>,
        now: tokio::time::Instant,
    ) -> bool {
        let Some(connected) = connected else {
            return false;
        };
        let wall_now = now_ms();
        let mut seen = lock(&self.presence_seen);
        let mut backoff = lock(&self.relay_probe_backoff);
        if connected {
            seen.insert(device.to_string(), wall_now);
            // Keep an entry rather than clearing it: a device that answers
            // "alive" while its presence beat stays silent would otherwise be
            // re-probed every sweep forever.
            let retry = RelayProbeRetry::alive(backoff.get(device), now, wall_now);
            backoff.insert(device.to_string(), retry);
            tracing::debug!(device, "presence: relay-verified alive");
            return true;
        }
        // A fresh presence frame can race a negative HTTP response. Do not
        // re-arm an offline delay for a peer we have just observed alive.
        if !seen
            .get(device)
            .is_some_and(|at| wall_now.saturating_sub(*at) < PRESENCE_FRESH_MS)
        {
            let retry = RelayProbeRetry::offline(backoff.get(device), now);
            backoff.insert(device.to_string(), retry);
        }
        false
    }
}
