//! Cypher-owned Pi runtime: on-demand download, verification, atomic
//! activation, and a device-global config boundary independent of `~/.pi`.
//!
//! Runtime archives are platform-specific and contain:
//! - `bin/node` + `bin/pi` (and optionally `bin/npm`);
//! - `pi/` (the Pi package root);
//! - `npm/` (the curated extension package project);
//! - `defaults/settings.json`;
//! - `runtime.json`.
//!
//! They are installed under `<data_dir>/pi-runtime/versions/<version>` and
//! activated through `<data_dir>/pi-runtime/current`. Mutable Pi configuration
//! lives separately under `<data_dir>/pi-runtime/agent`; its `npm` entry points
//! at the active runtime's curated package tree.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::watch;

use crate::pi_packages::{PiPackageUpdate, PiUpdateStatus};

const INITIAL_CHECK_DELAY: Duration = Duration::from_secs(20);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const CHECK_RETRY: Duration = Duration::from_secs(30 * 60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiRuntimePaths {
    pub root: PathBuf,
    pub current: PathBuf,
    pub executable: PathBuf,
    pub npm_executable: PathBuf,
    pub package_dir: PathBuf,
    pub agent_dir: PathBuf,
}

impl PiRuntimePaths {
    pub fn for_data_dir(data_dir: &Path) -> Self {
        let root = data_dir.join("pi-runtime");
        let current = cypher_env::var("PI_RUNTIME_DIR")
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("current"));
        Self {
            executable: current.join("bin").join("pi"),
            npm_executable: current.join("bin").join("npm"),
            package_dir: current.join("pi"),
            agent_dir: root.join("agent"),
            root,
            current,
        }
    }

    pub fn installed(&self) -> bool {
        self.executable.is_file()
            && self.package_dir.join("package.json").is_file()
            && self.current.join("runtime.json").is_file()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiRuntimeStatus {
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub installing: bool,
    #[serde(default)]
    pub downloaded_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiRuntimeManifest {
    pub version: String,
    pub pi_version: String,
    #[serde(default)]
    pub plugins: BTreeMap<String, String>,
    #[serde(default)]
    pub files: BTreeMap<String, PiRuntimeFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_cypher_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiRuntimeFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstalledRuntime {
    version: String,
    pi_version: String,
    #[serde(default)]
    plugins: BTreeMap<String, String>,
}

struct Inner {
    edge_url: String,
    paths: PiRuntimePaths,
    runtime_tx: watch::Sender<PiRuntimeStatus>,
    updates_tx: watch::Sender<PiUpdateStatus>,
    operation: tokio::sync::Mutex<()>,
    shutdown_tx: watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct PiRuntimeManager {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl PiRuntimeManager {
    pub fn spawn(edge_url: String, data_dir: &Path) -> Self {
        let paths = PiRuntimePaths::for_data_dir(data_dir);
        let initialization_error = if paths.installed() {
            initialize_agent(&paths, &paths.current)
                .and_then(|_| prune_stale_managed_packages(&paths, &paths.current))
                .err()
        } else {
            None
        };
        let installed = read_installed(&paths);
        let (runtime_tx, _) = watch::channel(PiRuntimeStatus {
            installed: paths.installed(),
            version: installed.as_ref().map(|runtime| runtime.version.clone()),
            installing: false,
            downloaded_bytes: 0,
            total_bytes: None,
            error: initialization_error.clone(),
        });
        let (updates_tx, _) = watch::channel(update_status(
            installed.as_ref(),
            None,
            initialization_error,
            false,
        ));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let manager = Self {
            inner: Arc::new(Inner {
                edge_url,
                paths,
                runtime_tx,
                updates_tx,
                operation: tokio::sync::Mutex::new(()),
                shutdown_tx,
                task: Mutex::new(None),
            }),
        };
        let worker = manager.clone();
        let task = tokio::spawn(async move { worker.check_loop(shutdown).await });
        *lock(&manager.inner.task) = Some(task);
        manager
    }

    pub fn paths(&self) -> &PiRuntimePaths {
        &self.inner.paths
    }

    pub fn status(&self) -> PiRuntimeStatus {
        self.inner.runtime_tx.borrow().clone()
    }

    pub fn watch_runtime(&self) -> watch::Receiver<PiRuntimeStatus> {
        self.inner.runtime_tx.subscribe()
    }

    pub fn watch_updates(&self) -> watch::Receiver<PiUpdateStatus> {
        self.inner.updates_tx.subscribe()
    }

    pub fn update_status(&self) -> PiUpdateStatus {
        self.inner.updates_tx.borrow().clone()
    }

    pub async fn shutdown(&self) {
        let _ = self.inner.shutdown_tx.send(true);
        let task = lock(&self.inner.task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    async fn check_loop(&self, mut shutdown: watch::Receiver<bool>) {
        tokio::select! {
            _ = shutdown.wait_for(|stop| *stop) => {}
            _ = async {
                tokio::time::sleep(INITIAL_CHECK_DELAY).await;
                loop {
                    let ok = self.check_updates().await;
                    tokio::time::sleep(if ok { CHECK_INTERVAL } else { CHECK_RETRY }).await;
                }
            } => {}
        }
    }

    pub async fn check_updates(&self) -> bool {
        let operation = self.inner.operation.lock().await;
        match self.fetch_manifest().await {
            Ok(manifest) => {
                let installed = read_installed(&self.inner.paths);
                let should_apply = installed.as_ref().is_some_and(|current| {
                    cypher_update::version_newer(&manifest.version, &current.version)
                });
                self.inner.updates_tx.send_replace(update_status(
                    installed.as_ref(),
                    Some(&manifest),
                    None,
                    false,
                ));
                drop(operation);
                if should_apply {
                    self.install_latest().await.is_ok()
                } else {
                    true
                }
            }
            Err(err) => {
                self.inner
                    .updates_tx
                    .send_modify(|status| status.error = Some(err.clone()));
                false
            }
        }
    }

    /// Install the newest compatible runtime (on first-run or from the
    /// background update loop). Download and extraction occur in staging;
    /// `current` changes only after checksum verification and a successful
    /// `pi --version` probe.
    pub async fn install_latest(&self) -> Result<PiRuntimeStatus, String> {
        let _operation = self.inner.operation.lock().await;
        if cypher_env::var("PI_RUNTIME_DIR").is_some() {
            return Err("CYPHER_PI_RUNTIME_DIR is set; this runtime is managed externally.".into());
        }
        self.inner.runtime_tx.send_modify(|status| {
            status.installing = true;
            status.downloaded_bytes = 0;
            status.total_bytes = None;
            status.error = None;
        });
        self.inner
            .updates_tx
            .send_modify(|status| status.applying = true);

        let result = tokio::time::timeout(DOWNLOAD_TIMEOUT, self.install_inner())
            .await
            .map_err(|_| "Pi Runtime download timed out.".to_string())
            .and_then(|result| result);
        match result {
            Ok(installed) => {
                let status = PiRuntimeStatus {
                    installed: true,
                    version: Some(installed.version.clone()),
                    installing: false,
                    downloaded_bytes: 0,
                    total_bytes: None,
                    error: None,
                };
                self.inner.runtime_tx.send_replace(status.clone());
                self.inner.updates_tx.send_replace(update_status(
                    Some(&installed),
                    None,
                    None,
                    false,
                ));
                Ok(status)
            }
            Err(err) => {
                self.inner.runtime_tx.send_modify(|status| {
                    status.installing = false;
                    status.error = Some(err.clone());
                });
                self.inner.updates_tx.send_modify(|status| {
                    status.applying = false;
                    status.error = Some(err.clone());
                });
                Err(err)
            }
        }
    }

    async fn install_inner(&self) -> Result<InstalledRuntime, String> {
        let manifest = self.fetch_manifest().await?;
        if let Some(minimum) = manifest.minimum_cypher_version.as_deref()
            && cypher_update::version_newer(minimum, env!("CARGO_PKG_VERSION"))
        {
            return Err(format!(
                "Pi Runtime {} requires Cypher {minimum} or newer.",
                manifest.version
            ));
        }
        if sanitize_version(&manifest.version) != manifest.version
            || manifest.version == "."
            || manifest.version == ".."
        {
            return Err("Pi Runtime manifest contains an unsafe version.".into());
        }
        let platform = platform_key();
        let file = manifest
            .files
            .get(&platform)
            .ok_or_else(|| format!("Pi Runtime {} has no {platform} build.", manifest.version))?;
        if file.sha256.len() != 64 || !file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Pi Runtime manifest contains an invalid SHA-256.".into());
        }

        let versions = self.inner.paths.root.join("versions");
        std::fs::create_dir_all(&versions).map_err(|err| err.to_string())?;
        let destination = versions.join(&manifest.version);
        if validate_runtime_dir(&destination).is_err() {
            let stage = self.inner.paths.root.join(format!(
                ".stage-{}-{}",
                sanitize_version(&manifest.version),
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&stage);
            std::fs::create_dir_all(&stage).map_err(|err| err.to_string())?;
            let archive = stage.join("runtime.tar.gz");
            let install_result = async {
                self.download(&manifest, file, &archive).await?;
                verify_sha256(&archive, &file.sha256)?;
                validate_archive_paths(&archive)?;
                let unpacked = stage.join("unpacked");
                std::fs::create_dir_all(&unpacked).map_err(|err| err.to_string())?;
                extract_archive(&archive, &unpacked)?;
                validate_runtime_dir(&unpacked)?;
                let installed = read_installed_dir(&unpacked)?;
                if installed.version != manifest.version
                    || installed.pi_version != manifest.pi_version
                    || installed.plugins != manifest.plugins
                {
                    return Err("Pi Runtime metadata does not match its manifest.".into());
                }
                probe_runtime(&unpacked, &self.inner.paths.agent_dir)?;
                match std::fs::rename(&unpacked, &destination) {
                    Ok(()) => {}
                    Err(err) if destination.is_dir() => {
                        validate_runtime_dir(&destination)?;
                        tracing::debug!(error = %err, "Pi Runtime install lost an equivalent race");
                    }
                    Err(err) => return Err(err.to_string()),
                }
                Ok::<_, String>(())
            }
            .await;
            let _ = std::fs::remove_dir_all(&stage);
            install_result?;
        }

        initialize_agent(&self.inner.paths, &destination)?;
        activate(&self.inner.paths, &destination)?;
        if let Err(err) = prune_stale_managed_packages(&self.inner.paths, &destination) {
            tracing::warn!(error = %err, "could not prune retired Pi runtime packages");
        }
        read_installed(&self.inner.paths)
            .ok_or_else(|| "Pi Runtime activation completed without valid metadata.".into())
    }

    async fn fetch_manifest(&self) -> Result<PiRuntimeManifest, String> {
        let url = format!("{}/manifest.json", self.base_url());
        let response = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .map_err(|err| format!("Could not fetch Pi Runtime manifest: {err}"))?
            .error_for_status()
            .map_err(|err| format!("Could not fetch Pi Runtime manifest: {err}"))?;
        let manifest = response
            .json::<PiRuntimeManifest>()
            .await
            .map_err(|err| format!("Invalid Pi Runtime manifest: {err}"))?;
        if manifest.version.trim().is_empty() || manifest.pi_version.trim().is_empty() {
            return Err("Pi Runtime manifest is missing its version.".into());
        }
        Ok(manifest)
    }

    fn base_url(&self) -> String {
        cypher_env::var("PI_RUNTIME_BASE_URL")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| public_runtime_base_url(&self.inner.edge_url))
            .trim_end_matches('/')
            .to_string()
    }

    async fn download(
        &self,
        manifest: &PiRuntimeManifest,
        file: &PiRuntimeFile,
        destination: &Path,
    ) -> Result<(), String> {
        let fallback = format!(
            "cypher-pi-runtime-{}-{}.tar.gz",
            manifest.version,
            platform_key()
        );
        let url = match file.url.as_deref() {
            Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
                url.to_string()
            }
            Some(url) => format!("{}/{}", self.base_url(), url.trim_start_matches('/')),
            None => format!("{}/{}", self.base_url(), fallback),
        };
        let response = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .map_err(|err| format!("Could not download Pi Runtime: {err}"))?
            .error_for_status()
            .map_err(|err| format!("Could not download Pi Runtime: {err}"))?;
        let total = response.content_length().or(Some(file.size));
        self.inner.runtime_tx.send_modify(|status| {
            status.total_bytes = total;
            status.downloaded_bytes = 0;
        });
        let mut output = tokio::fs::File::create(destination)
            .await
            .map_err(|err| err.to_string())?;
        let mut stream = response.bytes_stream();
        let mut downloaded = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| format!("Pi Runtime download failed: {err}"))?;
            output
                .write_all(&chunk)
                .await
                .map_err(|err| err.to_string())?;
            downloaded = downloaded.saturating_add(chunk.len() as u64);
            self.inner.runtime_tx.send_modify(|status| {
                status.downloaded_bytes = downloaded;
                status.total_bytes = total;
            });
        }
        output.flush().await.map_err(|err| err.to_string())?;
        if file.size > 0 && downloaded != file.size {
            return Err(format!(
                "Pi Runtime download size mismatch: expected {}, received {downloaded}.",
                file.size
            ));
        }
        Ok(())
    }
}

fn platform_key() -> String {
    let (os, arch) = cypher_update::platform_key();
    format!("{os}-{arch}")
}

fn sanitize_version(version: &str) -> String {
    version
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn public_runtime_base_url(edge_url: &str) -> String {
    format!("{}/releases/runtimes/pi", edge_url.trim_end_matches('/'))
}

fn read_installed(paths: &PiRuntimePaths) -> Option<InstalledRuntime> {
    read_installed_dir(&paths.current).ok()
}

fn read_installed_dir(directory: &Path) -> Result<InstalledRuntime, String> {
    let bytes = std::fs::read(directory.join("runtime.json")).map_err(|err| err.to_string())?;
    serde_json::from_slice(&bytes).map_err(|err| err.to_string())
}

fn update_status(
    installed: Option<&InstalledRuntime>,
    latest: Option<&PiRuntimeManifest>,
    error: Option<String>,
    applying: bool,
) -> PiUpdateStatus {
    let newer = installed.zip(latest).is_some_and(|(installed, latest)| {
        cypher_update::version_newer(&latest.version, &installed.version)
    });
    let package_updates = if newer {
        let installed = installed.expect("newer requires installed");
        let latest = latest.expect("newer requires latest");
        latest
            .plugins
            .iter()
            .filter_map(|(name, version)| {
                let current = installed.plugins.get(name)?;
                (current != version).then(|| PiPackageUpdate {
                    name: name.clone(),
                    current_version: current.clone(),
                    latest_version: version.clone(),
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    PiUpdateStatus {
        pi_installed: installed.is_some(),
        current_pi_version: installed.map(|runtime| runtime.pi_version.clone()),
        latest_pi_version: latest.map(|runtime| runtime.pi_version.clone()),
        pi_update_available: newer
            && installed
                .zip(latest)
                .is_some_and(|(installed, latest)| installed.pi_version != latest.pi_version),
        package_updates,
        applying,
        checked_at: latest.map(|_| chrono::Utc::now().timestamp_millis()),
        error,
    }
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "Pi Runtime checksum mismatch: expected {expected}, got {actual}."
        ))
    }
}

fn validate_archive_paths(archive: &Path) -> Result<(), String> {
    let output = std::process::Command::new("tar")
        .args(["-tzf"])
        .arg(archive)
        .output()
        .map_err(|err| format!("Could not inspect Pi Runtime archive: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let path = Path::new(line.trim_end_matches('/'));
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
        {
            return Err(format!("Unsafe path in Pi Runtime archive: {line}"));
        }
    }
    Ok(())
}

fn extract_archive(archive: &Path, destination: &Path) -> Result<(), String> {
    let output = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(destination)
        .arg("--strip-components=1")
        .output()
        .map_err(|err| format!("Could not extract Pi Runtime: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn validate_runtime_dir(directory: &Path) -> Result<(), String> {
    for relative in [
        "bin/node",
        "bin/pi",
        "pi/package.json",
        "npm/package.json",
        "extensions/cypher-provider-auth.ts",
        "provider-service.mjs",
        "runtime.json",
    ] {
        if !directory.join(relative).is_file() {
            return Err(format!(
                "Pi Runtime is missing {}.",
                directory.join(relative).display()
            ));
        }
    }
    Ok(())
}

fn probe_runtime(directory: &Path, agent_dir: &Path) -> Result<(), String> {
    let output = std::process::Command::new(directory.join("bin/pi"))
        .arg("--version")
        .env("PI_CODING_AGENT_DIR", agent_dir)
        .env("PI_PACKAGE_DIR", directory.join("pi"))
        .output()
        .map_err(|err| format!("Could not start the downloaded Pi Runtime: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Downloaded Pi Runtime failed its self-check: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn initialize_agent(paths: &PiRuntimePaths, runtime: &Path) -> Result<(), String> {
    std::fs::create_dir_all(&paths.agent_dir).map_err(|err| err.to_string())?;
    let settings = paths.agent_dir.join("settings.json");
    let provider_auth_extension = paths
        .current
        .join("extensions/cypher-provider-auth.ts")
        .display()
        .to_string();
    let provider_auth_available = runtime.join("extensions/cypher-provider-auth.ts").is_file();
    if !settings.exists() {
        let description = read_installed_dir(runtime)?;
        let packages = description
            .plugins
            .keys()
            .map(|name| {
                let source = paths
                    .current
                    .join("npm/node_modules")
                    .join(name)
                    .display()
                    .to_string();
                if name == "pi-permission-control" {
                    serde_json::json!({
                        "source": source,
                        "extensions": ["-index.ts"]
                    })
                } else {
                    Value::String(source)
                }
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "packages": packages,
            "extensions": if provider_auth_available {
                vec![provider_auth_extension.clone()]
            } else {
                Vec::new()
            }
        }))
        .map_err(|err| err.to_string())?;
        std::fs::write(&settings, bytes).map_err(|err| err.to_string())?;
    } else {
        let bytes = std::fs::read(&settings).map_err(|err| err.to_string())?;
        let mut root = serde_json::from_slice::<Value>(&bytes).map_err(|err| err.to_string())?;
        let object = root
            .as_object_mut()
            .ok_or_else(|| "Pi settings must be a JSON object.".to_string())?;
        let extensions = object
            .entry("extensions")
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| "Pi settings extensions must be an array.".to_string())?;
        if provider_auth_available
            && !extensions
                .iter()
                .any(|entry| entry.as_str().is_some_and(|source| {
                    source == provider_auth_extension || (Path::new(source).is_absolute() && matches!(
                        (std::fs::canonicalize(source), std::fs::canonicalize(&provider_auth_extension)),
                        (Ok(existing), Ok(expected)) if existing == expected
                    ))
                }))
        {
            extensions.push(Value::String(provider_auth_extension));
            let bytes = serde_json::to_vec_pretty(&root).map_err(|err| err.to_string())?;
            let temporary = settings.with_extension("json.tmp");
            std::fs::write(&temporary, bytes).map_err(|err| err.to_string())?;
            std::fs::rename(temporary, &settings).map_err(|err| err.to_string())?;
        }
    }
    let npm = paths.agent_dir.join("npm");
    if !npm.exists() {
        std::fs::create_dir_all(&npm).map_err(|err| err.to_string())?;
        std::fs::write(
            npm.join("package.json"),
            b"{\n  \"name\": \"cypher-user-pi-packages\",\n  \"private\": true\n}\n",
        )
        .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn prune_stale_managed_packages(paths: &PiRuntimePaths, runtime: &Path) -> Result<(), String> {
    let settings = paths.agent_dir.join("settings.json");
    let bytes = std::fs::read(&settings).map_err(|err| err.to_string())?;
    let mut root = serde_json::from_slice::<Value>(&bytes).map_err(|err| err.to_string())?;
    let Some(packages) = root.get_mut("packages").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    // A runtime release may retire a curated package. Remove only stale
    // Cypher-managed local paths; user npm/git/local packages are untouched.
    let managed = paths.current.join("npm/node_modules");
    let next_packages = runtime.join("npm/node_modules");
    let before = packages.len();
    packages.retain(|entry| {
        let source = match entry {
            Value::String(source) => Some(source.as_str()),
            Value::Object(object) => object.get("source").and_then(Value::as_str),
            _ => None,
        };
        !source.is_some_and(|source| {
            Path::new(source)
                .strip_prefix(&managed)
                .is_ok_and(|relative| !next_packages.join(relative).join("package.json").is_file())
        })
    });
    if packages.len() == before {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&root).map_err(|err| err.to_string())?;
    let temporary = settings.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(|err| err.to_string())?;
    std::fs::rename(temporary, settings).map_err(|err| err.to_string())
}

fn activate(paths: &PiRuntimePaths, destination: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let temporary = paths.root.join(format!(".current-{}", std::process::id()));
        let _ = std::fs::remove_file(&temporary);
        std::os::unix::fs::symlink(destination, &temporary).map_err(|err| err.to_string())?;
        std::fs::rename(&temporary, &paths.current).map_err(|err| err.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (paths, destination);
        Err("Pi Runtime activation is currently supported on Unix only.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_downloads_use_the_public_release_route() {
        for edge in [
            "https://edge.letscypher.app",
            "https://edge.letscypher.app/",
        ] {
            let base = public_runtime_base_url(edge);
            assert_eq!(
                format!("{base}/manifest.json"),
                "https://edge.letscypher.app/releases/runtimes/pi/manifest.json"
            );
            assert_eq!(
                format!("{base}/cypher-pi-runtime-0.85.1.1-macos-arm64.tar.gz"),
                "https://edge.letscypher.app/releases/runtimes/pi/cypher-pi-runtime-0.85.1.1-macos-arm64.tar.gz"
            );
        }
    }

    #[test]
    fn runtime_paths_are_device_scoped() {
        let paths = PiRuntimePaths::for_data_dir(Path::new("/tmp/cypher-test"));
        assert_eq!(
            paths.agent_dir,
            Path::new("/tmp/cypher-test/pi-runtime/agent")
        );
        assert!(paths.executable.ends_with("current/bin/pi"));
    }

    #[test]
    fn update_status_compares_whole_runtime() {
        let installed = InstalledRuntime {
            version: "1".into(),
            pi_version: "0.84.0".into(),
            plugins: BTreeMap::from([("plugin".into(), "1.0.0".into())]),
        };
        let latest = PiRuntimeManifest {
            version: "2".into(),
            pi_version: "0.85.0".into(),
            plugins: BTreeMap::from([("plugin".into(), "1.1.0".into())]),
            files: BTreeMap::new(),
            minimum_cypher_version: None,
        };
        let status = update_status(Some(&installed), Some(&latest), None, false);
        assert!(status.update_available());
        assert!(status.pi_update_available);
        assert_eq!(status.package_updates.len(), 1);
    }

    #[test]
    fn archive_paths_reject_parent_components() {
        let unsafe_path = Path::new("runtime/../../escape");
        assert!(
            unsafe_path
                .components()
                .any(|component| component == Component::ParentDir)
        );
    }

    #[test]
    fn agent_initialization_uses_bundled_paths_but_keeps_user_npm_mutable() {
        let temp = tempfile::tempdir().unwrap();
        let paths = PiRuntimePaths::for_data_dir(temp.path());
        std::fs::create_dir_all(paths.current.join("extensions")).unwrap();
        std::fs::write(
            paths.current.join("extensions/cypher-provider-auth.ts"),
            "export default () => {};",
        )
        .unwrap();
        let package = paths.current.join("npm/node_modules/pi-web-search");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("package.json"),
            r#"{"name":"pi-web-search","version":"1.4.0"}"#,
        )
        .unwrap();
        std::fs::write(
            paths.current.join("runtime.json"),
            serde_json::to_vec(&InstalledRuntime {
                version: "1".into(),
                pi_version: "0.85.0".into(),
                plugins: BTreeMap::from([("pi-web-search".into(), "1.4.0".into())]),
            })
            .unwrap(),
        )
        .unwrap();

        initialize_agent(&paths, &paths.current).unwrap();

        let settings: Value =
            serde_json::from_slice(&std::fs::read(paths.agent_dir.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            settings["packages"][0].as_str(),
            Some(package.to_str().unwrap())
        );
        let user_npm = paths.agent_dir.join("npm");
        assert!(user_npm.is_dir());
        assert!(!user_npm.is_symlink());
        assert!(user_npm.join("package.json").is_file());
        let before = std::fs::read(paths.agent_dir.join("settings.json")).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(temp.path(), &alias).unwrap();
        let alias_paths = PiRuntimePaths::for_data_dir(&alias);
        initialize_agent(&alias_paths, &alias_paths.current).unwrap();
        assert_eq!(
            std::fs::read(paths.agent_dir.join("settings.json")).unwrap(),
            before,
            "data-directory aliases must not duplicate provider extensions or rewrite settings"
        );
    }
}
