//! Engine-side checker.

use super::*;

/// What the engine reports over the `UpdateStatus` stream. Version facts only —
/// download/apply progress is owned by whoever drives the update (UI or CLI).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub update_available: bool,
    /// Epoch ms of the last successful check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A new app bundle is in place and its relauncher is waiting for this
    /// process to exit: the owning desktop app quits itself when it sees
    /// this (a remotely triggered update has no UI click to do so).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub relaunch_pending: bool,
}

impl UpdateStatus {
    fn initial() -> Self {
        Self {
            current_version: current_version().to_string(),
            latest_version: None,
            update_available: false,
            checked_at: None,
            error: None,
            relaunch_pending: false,
        }
    }
}

/// `CYPHER_AUTO_UPDATE=1|true|yes|on` or `0|false|no|off`. Unset means
/// **on for Linux** — a headless device has no update strip to click, so the
/// service applies releases itself in a quiet window and restarts — and off
/// elsewhere, where the desktop app owns updates.
pub(crate) fn auto_update_enabled() -> bool {
    auto_update_setting(cypher_env::var("AUTO_UPDATE").as_deref())
}

pub fn auto_update_setting(value: Option<&str>) -> bool {
    match value.map(|value| value.trim().to_ascii_lowercase()) {
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on") => true,
        Some(value) if matches!(value.as_str(), "0" | "false" | "no" | "off") => false,
        Some(_) => false,
        None => cfg!(target_os = "linux"),
    }
}

/// "Nothing would be interrupted by a restart right now" — wired by the engine
/// to its live-run and open-terminal registries. `None` = no gate.
pub type QuiescentCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Background release checker: polls `{edge}/releases` on a 6h cadence and
/// publishes [`UpdateStatus`] over a watch channel (the `UpdateStatus` RPC
/// stream). Managed installs with `CYPHER_AUTO_UPDATE` set stage + apply +
/// service restart on their own — but only in a quiet window: while
/// `quiescent` reports activity, the apply defers and re-probes every
/// [`IDLE_RECHECK`].
#[derive(Clone)]
pub struct Updater {
    data_dir: PathBuf,
    edge_url: String,
    status_tx: Arc<watch::Sender<UpdateStatus>>,
    check_tx: Arc<watch::Sender<u64>>,
    quiescent: Option<QuiescentCheck>,
    /// Flips to true exactly once; the check loop selects against it so
    /// cancellation lands at any await point (no tokio-util in this crate).
    shutdown_tx: Arc<watch::Sender<bool>>,
    check_task: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Epoch ms of the last activation-triggered wake, for [`ACTIVATION_COOLDOWN`].
    /// Tracks the trigger rather than `checked_at` so a run of failing checks
    /// cannot turn every focus change into a fresh request.
    last_activation: Arc<std::sync::atomic::AtomicI64>,
}

impl Updater {
    /// Spawn the check loop (must run on a tokio runtime).
    pub fn spawn(edge_url: String, quiescent: Option<QuiescentCheck>, data_dir: PathBuf) -> Self {
        let (status_tx, _) = watch::channel(UpdateStatus::initial());
        // Create the loop's receivers synchronously. If they were subscribed
        // inside the spawned task, an immediate `check_now` could be lost and
        // an immediate `shutdown` could fail while no receiver existed,
        // leaving shutdown waiting forever for the polling loop.
        let (check_tx, checks) = watch::channel(0);
        let (shutdown_tx, shutdown) = watch::channel(false);
        let updater = Self {
            data_dir,
            edge_url,
            status_tx: Arc::new(status_tx),
            check_tx: Arc::new(check_tx),
            quiescent,
            shutdown_tx: Arc::new(shutdown_tx),
            check_task: Arc::new(std::sync::Mutex::new(None)),
            last_activation: Arc::new(std::sync::atomic::AtomicI64::new(0)),
        };
        let for_loop = updater.clone();
        let task = tokio::spawn(async move {
            // Bundles for this version or older were applied or overtaken: the
            // running app never reads them again.
            let data_dir = for_loop.data_dir.clone();
            let _ = off_runtime(move || {
                prune_staged_updates(&data_dir, |staged| {
                    !version_newer(staged, current_version())
                });
                if let InstallKind::Managed { app_root } = detect_install() {
                    prune_managed_versions(&app_root, |installed| {
                        !version_newer(installed, current_version())
                    });
                }
                Ok(())
            })
            .await;
            for_loop.check_loop(shutdown, checks).await
        });
        *crate::lock(&updater.check_task) = Some(task);
        updater
    }

    /// Stop the check loop and wait for it to exit — a replaced runtime must
    /// not keep polling `{edge}/releases` (or auto-applying) in the background.
    /// Idempotent, and callable from any clone.
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        let task = crate::lock(&self.check_task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    pub fn watch(&self) -> watch::Receiver<UpdateStatus> {
        self.status_tx.subscribe()
    }

    /// Wake the release checker immediately, for example when authentication
    /// recovers after the process started offline.
    pub fn check_now(&self) {
        self.check_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }

    /// Run one release check immediately and return the resulting status.
    /// Menu "Check for Updates" waits on this rather than the polling cadence.
    pub async fn check(&self) -> UpdateStatus {
        let _ = self.check_once().await;
        self.status_tx.borrow().clone()
    }

    /// The user came back to the app. Check now, so a release published while
    /// they were away is visible when they return instead of on the next tick.
    ///
    /// Rate-limited by [`ACTIVATION_COOLDOWN`]: returns whether it actually
    /// woke the checker, so callers can be wired to a noisy focus signal
    /// without special-casing.
    pub fn check_on_activation(&self) -> bool {
        use std::sync::atomic::Ordering;
        let now = now_ms();
        let cooldown = ACTIVATION_COOLDOWN.as_millis() as i64;
        let previous = self.last_activation.load(Ordering::Relaxed);
        // A clock that moved backwards must not disable checking until it
        // catches up, so treat any past-dated stamp as expired.
        if previous != 0 && now >= previous && now - previous < cooldown {
            return false;
        }
        if self
            .last_activation
            .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return false; // Another activation won the race and is checking.
        }
        self.check_now();
        true
    }

    /// Credentials changed, but the public release endpoint does not need
    /// them. Only a new sign-in or recovery from a failed check merits an
    /// early retry; ordinary token rotation must not bypass the cadence.
    pub fn check_after_auth_change(&self, signed_in: bool, recovered: bool) {
        if signed_in && (recovered || self.status_tx.borrow().error.is_some()) {
            self.check_now();
        }
    }

    fn quiescent_now(&self) -> bool {
        self.quiescent.as_ref().is_none_or(|check| check())
    }

    async fn check_loop(
        &self,
        mut shutdown: watch::Receiver<bool>,
        mut checks: watch::Receiver<u64>,
    ) {
        // Shutdown must cut the loop at ANY await point — including mid
        // `check_once()` / `auto_apply_when_idle()` HTTP — so the whole body
        // races the flag rather than checking it between iterations.
        tokio::select! {
            _ = shutdown.wait_for(|stop| *stop) => {}
            _ = async {
                tokio::select! {
                    _ = tokio::time::sleep(CHECK_INITIAL_DELAY) => {}
                    _ = checks.changed() => {}
                }
                loop {
                    let ok = self.check_once().await;
                    if ok
                        && self.status_tx.borrow().update_available
                        && auto_update_enabled()
                        && let InstallKind::Managed { .. } = detect_install()
                    {
                        self.auto_apply_when_idle().await;
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(if ok { CHECK_INTERVAL } else { CHECK_RETRY }) => {}
                        _ = checks.changed() => {}
                    }
                }
            } => {}
        }
    }

    /// Sessions must never die to an update: pre-stage the download now
    /// (harmless while busy), wait for a quiet window (no live runs, no open
    /// terminals), then apply — which re-fetches the manifest (so a long defer
    /// lands on whatever is newest) and reuses the staged dir, keeping the
    /// idle→restart gap to well under a second.
    async fn auto_apply_when_idle(&self) {
        if let InstallKind::Managed { app_root } = detect_install() {
            match fetch_latest(&self.edge_url).await {
                Ok(manifest) if version_newer(&manifest.version, current_version()) => {
                    if let Err(err) = stage_headless(&self.edge_url, &manifest, &app_root).await {
                        tracing::warn!(error = %err, "auto-update staging failed");
                        return;
                    }
                }
                Ok(_) => return,
                Err(err) => {
                    tracing::warn!(error = %err, "auto-update staging fetch failed");
                    return;
                }
            }
        }
        let mut deferred = false;
        while !self.quiescent_now() {
            if !deferred {
                deferred = true;
                tracing::info!("auto-update deferred: sessions or terminals active");
            }
            tokio::time::sleep(IDLE_RECHECK).await;
        }
        match self.apply(true).await {
            Ok(version) => {
                tracing::info!(%version, "auto-update applied; service restarting")
            }
            Err(err) => tracing::warn!(error = %err, "auto-update failed"),
        }
    }

    /// One check; returns false on fetch failure (retry sooner).
    async fn check_once(&self) -> bool {
        match fetch_latest(&self.edge_url).await {
            Ok(manifest) => {
                let status = UpdateStatus {
                    current_version: current_version().to_string(),
                    update_available: version_newer(&manifest.version, current_version()),
                    latest_version: Some(manifest.version),
                    checked_at: Some(now_ms()),
                    error: None,
                    relaunch_pending: self.status_tx.borrow().relaunch_pending,
                };
                if status.update_available {
                    tracing::info!(
                        latest = status.latest_version.as_deref().unwrap_or(""),
                        current = %status.current_version,
                        "update available"
                    );
                }
                self.status_tx.send_replace(status);
                true
            }
            Err(err) => {
                tracing::debug!(error = %err, "update check failed");
                self.status_tx
                    .send_modify(|s| s.error = Some(format!("{err:#}")));
                false
            }
        }
    }

    /// Stage + apply the newest release on THIS device — the path a remote
    /// desktop's Devices → Update takes. Managed (Linux) installs swap the
    /// symlink and restart the service after a short delay so the caller's
    /// reply flushes first. App bundles (a Mac) swap the bundle, arm the
    /// relauncher, and raise `relaunch_pending` on the status stream; the
    /// desktop app observing that stream quits, and the relauncher opens the
    /// new bundle. Without `force`, live runs or open terminals refuse the
    /// update instead of killing them.
    pub async fn apply(&self, force: bool) -> anyhow::Result<String> {
        if !force && !self.quiescent_now() {
            bail!("busy: this device has active runs or open terminals");
        }
        let manifest = fetch_latest(&self.edge_url).await?;
        if !version_newer(&manifest.version, current_version()) {
            bail!("already up to date ({})", current_version());
        }
        match detect_install() {
            InstallKind::Managed { app_root } => {
                stage_headless(&self.edge_url, &manifest, &app_root).await?;
                {
                    let (app_root, version) = (app_root.clone(), manifest.version.clone());
                    off_runtime(move || apply_headless(&app_root, &version)).await?;
                }
                let data_dir = self.data_dir.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                    if let Err(err) = restart_service_from_engine(&data_dir) {
                        tracing::warn!(error = %err, "service restart failed — restart the engine to finish the update");
                    }
                });
                Ok(manifest.version)
            }
            InstallKind::MacApp { bundle } => {
                let staged = stage_mac_app(&self.edge_url, &manifest, &self.data_dir).await?;
                {
                    let bundle = bundle.clone();
                    off_runtime(move || apply_mac_app(&staged, &bundle)).await?;
                }
                relaunch_app_after_exit(&bundle);
                self.status_tx
                    .send_modify(|status| status.relaunch_pending = true);
                tracing::info!(version = %manifest.version, "app bundle replaced; waiting for the app to quit and relaunch");
                Ok(manifest.version)
            }
            InstallKind::Unmanaged => {
                bail!("this install is not update-managed — source builds update via git")
            }
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
