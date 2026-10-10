//! Registry room sync: the join/redial loop, snapshot persistence and the
//! background task that republishes watch channels.

use super::*;

/// Debounce window for local snapshot saves after a change.
const SNAPSHOT_DEBOUNCE_MS: u64 = 1_000;

/// Quiet-probe cadence for the registry room: fixed at 15 minutes. One room
/// per engine, so the fixed cadence costs ~100 DO wakes/day total, and the
/// probe is deadline-checked — a mute room is detected within
/// probe cadence + 10s instead of hours.
const REGISTRY_PROBE_QUIET: std::time::Duration = std::time::Duration::from_secs(900);

impl WorkspaceHost {
    pub(super) fn spawn_join(
        &self,
        url: Arc<dyn cypher_sync::UrlProvider>,
        mut token_changes: Option<tokio::sync::watch::Receiver<u64>>,
        token: Option<Arc<dyn cypher_rpc::TokenSource>>,
    ) {
        let org_id = self.inner.config.org_id.clone();
        let reg = self.inner.reg.clone();
        let device_id = self.inner.config.device_id.clone();
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut wake = cypher_sync::wake::subscribe();
            // `RegistryClient` only self-reconnects AFTER a first successful
            // join; an INITIAL failure (a 500 from an overloaded DO, a token
            // racing a refresh, an edge deploy) must not end this task and
            // leave the device offline until an app restart. Retry the first
            // join on a capped, jittered backoff so a transient blip self-heals.
            let mut backoff = JOIN_RETRY_BASE;
            loop {
                if weak.upgrade().is_none() {
                    return; // host dropped
                }
                let tuning = RegistryTuning {
                    probe_quiet: REGISTRY_PROBE_QUIET,
                    ..RegistryTuning::default()
                };
                let client_result = if let Some(inner) = weak.upgrade() {
                    let transport = inner.config.edge.clone().map(|edge| {
                        Arc::new(EdgeRegistryTransport {
                            http: reqwest::Client::builder()
                                .connect_timeout(std::time::Duration::from_secs(10))
                                .timeout(std::time::Duration::from_secs(30))
                                .build()
                                .expect("registry HTTP client"),
                            edge,
                            org_id: org_id.clone(),
                        }) as Arc<dyn RegistryTransport>
                    });
                    match transport {
                        Some(transport) => {
                            RegistryClient::connect_via_transport_tuned(
                                url.clone(),
                                reg.clone(),
                                &device_id,
                                tuning,
                                transport,
                            )
                            .await
                        }
                        None => {
                            RegistryClient::connect_via_tuned(
                                url.clone(),
                                reg.clone(),
                                &device_id,
                                tuning,
                            )
                            .await
                        }
                    }
                } else {
                    return;
                };
                match client_result {
                    Ok(client) => {
                        let client = Arc::new(client);
                        client.set_presence(now_ms());
                        let mut events = client.events();
                        if token_revoked(&token).await {
                            return;
                        }
                        let Some(inner) = weak.upgrade() else { return };
                        *lock(&inner.room) = Some(client.clone());
                        // A transport-backed client is ready local-first,
                        // before either HTTP or WS has returned server truth.
                        // Do not open the orphan-sweep gate until the first
                        // state response has actually been applied.
                        if client.stats().server_known {
                            inner.registry_synced.store(true, Ordering::Relaxed);
                            inner.reconcile_own_device();
                        }
                        inner.bump_changed();
                        tracing::info!(org = %org_id, "registry room joined");
                        drop(inner);
                        // The slot is the sole owner. This lets engine-level
                        // revocation close the socket synchronously by taking it.
                        drop(client);
                        // The event pump lives for the client's whole life
                        // (across its self-reconnects); it ends only when the
                        // client is dropped at host teardown.
                        loop {
                            tokio::select! {
                                event = events.recv() => match event {
                                    Ok(event @ (cypher_sync::RegistryEvent::Applied
                                    | cypher_sync::RegistryEvent::Connected)) => {
                                        let Some(inner) = weak.upgrade() else { return };
                                        if event == cypher_sync::RegistryEvent::Connected
                                            && !lock(&inner.relay_probe_backoff).is_empty()
                                        {
                                            inner.relay_probe_wake.notify_one();
                                        }
                                        if lock(&inner.room)
                                            .as_ref()
                                            .is_some_and(|room| room.stats().server_known)
                                        {
                                            inner.registry_synced.store(true, Ordering::Relaxed);
                                            inner.reconcile_own_device();
                                        }
                                        inner.bump_changed();
                                    }
                                    Ok(cypher_sync::RegistryEvent::Presence) => {
                                        let Some(inner) = weak.upgrade() else { return };
                                        inner.publish();
                                    }
                                    Ok(cypher_sync::RegistryEvent::Disconnected) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                },
                                _ = token_changed(&mut token_changes) => {
                                    if token_revoked(&token).await {
                                        tracing::info!(org = %org_id,
                                            "registry credentials removed; leaving room");
                                        break;
                                    }
                                }
                            }
                        }
                        if let Some(inner) = weak.upgrade() {
                            *lock(&inner.room) = None;
                        }
                        return;
                    }
                    Err(err) => {
                        tracing::warn!(org = %org_id, error = %err, backoff_ms = backoff.as_millis() as u64,
                            "registry room join failed; retrying");
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(backoff + join_retry_jitter()) => {
                        backoff = (backoff * 2).min(JOIN_RETRY_CAP);
                    }
                    _ = wake.recv() => {
                        backoff = JOIN_RETRY_BASE;
                    }
                    _ = token_changed(&mut token_changes) => {
                        if token_revoked(&token).await {
                            return;
                        }
                        backoff = JOIN_RETRY_BASE;
                    }
                }
            }
        });
    }
}

impl WorkspaceHostInner {
    pub(super) fn save_snapshot(&self) {
        let bytes = lock(&self.reg).to_bytes();
        match bytes {
            Ok(bytes) => {
                if let Err(err) = self.store.save_snapshot(REGISTRY_DOC_ID, &bytes) {
                    tracing::warn!(error = %err, "registry snapshot save failed");
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "registry snapshot export failed");
            }
        }
    }
}

/// Background task: reacts to registry changes (local mutations and applied
/// server frames) by re-publishing the watch channels and debouncing snapshots,
/// and refreshes presence every [`PRESENCE_INTERVAL_MS`]. Holds only a weak
/// handle so a dropped host tears the task down.
pub(super) async fn workspace_task(
    weak: Weak<WorkspaceHostInner>,
    mut changed_rx: watch::Receiver<u64>,
) {
    let mut presence =
        tokio::time::interval(std::time::Duration::from_millis(PRESENCE_INTERVAL_MS));
    presence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    presence.tick().await; // consume the immediate first tick
    let mut save_deadline: Option<tokio::time::Instant> = None;
    loop {
        let sleep_until = save_deadline.unwrap_or_else(tokio::time::Instant::now);
        tokio::select! {
            changed = changed_rx.changed() => {
                if changed.is_err() {
                    break; // host (and its change sender) is gone
                }
                let Some(inner) = weak.upgrade() else { break };
                inner.publish();
                if save_deadline.is_none() {
                    save_deadline = Some(
                        tokio::time::Instant::now()
                            + std::time::Duration::from_millis(SNAPSHOT_DEBOUNCE_MS),
                    );
                }
            }
            _ = tokio::time::sleep_until(sleep_until), if save_deadline.is_some() => {
                save_deadline = None;
                let Some(inner) = weak.upgrade() else { break };
                // SQLite (5s busy timeout) off the runtime worker: this task
                // also drives the presence heartbeat. Awaited so snapshots
                // never overtake each other.
                let _ = tokio::task::spawn_blocking(move || inner.save_snapshot()).await;
            }
            _ = presence.tick() => {
                let Some(inner) = weak.upgrade() else { break };
                inner.presence_tick();
                // Re-publish on the same cadence: remote heartbeats decay when a
                // device goes silent, and watchers (the UI online dot, "host
                // offline" hints) need a tick to observe that staleness.
                inner.publish();
            }
        }
    }
}
