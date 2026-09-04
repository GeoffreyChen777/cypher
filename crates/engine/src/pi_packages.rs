//! Pi installation and package management for Settings → Agents.
//!
//! Pi owns the package format and settings schema, so package changes go
//! through `pi install` while enablement is represented by the same
//! `~/.pi/agent/settings.json` package list that Pi reads.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::watch;

const PI_PACKAGE: &str = "@earendil-works/pi-coding-agent";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(60);
const UPDATE_CHECK_INITIAL_DELAY: Duration = Duration::from_secs(20);
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const UPDATE_CHECK_RETRY: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiPackageUpdate {
    pub name: String,
    pub current_version: String,
    pub latest_version: String,
}

/// Pi CLI + extension update facts published by the engine every six hours.
/// Applying remains an explicit user action from the Cypher notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiUpdateStatus {
    pub pi_installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_pi_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_pi_version: Option<String>,
    #[serde(default)]
    pub pi_update_available: bool,
    #[serde(default)]
    pub package_updates: Vec<PiPackageUpdate>,
    #[serde(default)]
    pub applying: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PiUpdateStatus {
    fn initial() -> Self {
        Self {
            pi_installed: pi().is_some(),
            current_pi_version: None,
            latest_pi_version: None,
            pi_update_available: false,
            package_updates: Vec::new(),
            applying: false,
            checked_at: None,
            error: None,
        }
    }

    pub fn update_available(&self) -> bool {
        self.pi_update_available || !self.package_updates.is_empty()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Background Pi/package checker. It mirrors Cypher's own updater cadence:
/// first check shortly after boot, then every six hours (30-minute retry after
/// network/tooling failures). The task is explicitly shut down with its engine
/// runtime so account/profile replacement cannot leave duplicate pollers.
#[derive(Clone)]
pub struct PiUpdater {
    status_tx: Arc<watch::Sender<PiUpdateStatus>>,
    shutdown_tx: Arc<watch::Sender<bool>>,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    operation: Arc<tokio::sync::Mutex<()>>,
}

impl PiUpdater {
    pub fn spawn() -> Self {
        let (status_tx, _) = watch::channel(PiUpdateStatus::initial());
        let (shutdown_tx, shutdown) = watch::channel(false);
        let updater = Self {
            status_tx: Arc::new(status_tx),
            shutdown_tx: Arc::new(shutdown_tx),
            task: Arc::new(Mutex::new(None)),
            operation: Arc::new(tokio::sync::Mutex::new(())),
        };
        let for_task = updater.clone();
        let task = tokio::spawn(async move { for_task.check_loop(shutdown).await });
        *lock(&updater.task) = Some(task);
        updater
    }

    pub fn watch(&self) -> watch::Receiver<PiUpdateStatus> {
        self.status_tx.subscribe()
    }

    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        let task = lock(&self.task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    async fn check_loop(&self, mut shutdown: watch::Receiver<bool>) {
        tokio::select! {
            _ = shutdown.wait_for(|stop| *stop) => {}
            _ = async {
                tokio::time::sleep(UPDATE_CHECK_INITIAL_DELAY).await;
                loop {
                    let ok = self.check().await;
                    tokio::time::sleep(if ok {
                        UPDATE_CHECK_INTERVAL
                    } else {
                        UPDATE_CHECK_RETRY
                    }).await;
                }
            } => {}
        }
    }

    /// Refresh the update facts. Returns false when any required registry
    /// lookup failed so the loop retries sooner while preserving partial facts.
    pub async fn check(&self) -> bool {
        let _operation = self.operation.lock().await;
        self.check_inner().await
    }

    async fn check_inner(&self) -> bool {
        let pi_path = pi();
        let npm_path = npm();
        let pi_installed = pi_path.is_some();
        let (pi_result, packages_result) = match (pi_path.as_deref(), npm_path.as_deref()) {
            (Some(pi_path), Some(npm_path)) => tokio::join!(
                check_pi_update(pi_path, npm_path),
                check_package_updates(npm_path)
            ),
            (None, Some(npm_path)) => (
                Ok((None, None, false)),
                check_package_updates(npm_path).await,
            ),
            (_, None) => (
                Err("npm was not found; Pi updates cannot be checked.".into()),
                Err("npm was not found; package updates cannot be checked.".into()),
            ),
        };

        let mut errors = Vec::new();
        let (current_pi_version, latest_pi_version, pi_update_available) = match pi_result {
            Ok(facts) => facts,
            Err(err) => {
                errors.push(err);
                (None, None, false)
            }
        };
        let package_updates = match packages_result {
            Ok(updates) => updates,
            Err(err) => {
                errors.push(err);
                Vec::new()
            }
        };
        let ok = errors.is_empty();
        let status = PiUpdateStatus {
            pi_installed,
            current_pi_version,
            latest_pi_version,
            pi_update_available,
            package_updates,
            applying: false,
            checked_at: Some(chrono::Utc::now().timestamp_millis()),
            error: (!ok).then(|| errors.join(" ")),
        };
        if status.update_available() {
            tracing::info!(
                pi = status.pi_update_available,
                packages = status.package_updates.len(),
                "Pi or package updates available"
            );
        }
        self.status_tx.send_replace(status);
        ok
    }

    /// Explicit one-click action: update Pi and every unpinned installed
    /// extension using Pi's own package manager, then refresh the facts.
    pub async fn apply_all(&self) -> Result<PiUpdateStatus, String> {
        let _operation = self.operation.lock().await;
        let pi = pi().ok_or_else(|| format!("Pi is not installed ({TOOLCHAIN_HINT})."))?;
        self.status_tx.send_modify(|status| {
            status.applying = true;
            status.error = None;
        });
        let mut command = Command::new(&pi);
        command.args(["update", "--all", "--no-approve"]);
        if let Some(home) = home() {
            // Keep this device-level action out of whichever project happened
            // to launch Cypher; only the global Pi/package settings are updated.
            command.current_dir(home);
        }
        if let Err(err) = run(&pi, command).await {
            self.status_tx.send_modify(|status| {
                status.applying = false;
                status.error = Some(err.clone());
            });
            return Err(err);
        }
        // Clear the actionable notification immediately; the registry refresh
        // below repopulates it only if something genuinely remains outdated.
        self.status_tx.send_modify(|status| {
            status.pi_update_available = false;
            status.package_updates.clear();
            status.applying = false;
            status.error = None;
            status.checked_at = Some(chrono::Utc::now().timestamp_millis());
        });
        let _ = self.check_inner().await;
        Ok(self.status_tx.borrow().clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiPackage {
    pub source: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub installed: bool,
    pub enabled: bool,
    pub recommended: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiPackagesSnapshot {
    pub pi_installed: bool,
    pub npm_available: bool,
    pub packages: Vec<PiPackage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPackageEnabled {
    pub source: String,
    pub enabled: bool,
}

/// These are the packages used by the Cypher development setup. Keep the list
/// deliberately small and curated: extensions execute with full user access.
const RECOMMENDED: &[(&str, &str)] = &[
    (
        "npm:pi-web-search",
        "Web search tools for current information.",
    ),
    (
        "npm:@lll9p/pi-better-compaction",
        "More predictable context compaction.",
    ),
    (
        "npm:pi-codex-tools",
        "Codex-compatible coding and review tools.",
    ),
    (
        "npm:@narumitw/pi-goal",
        "Goal tracking and completion workflow.",
    ),
    (
        "npm:gpt-fast-pi",
        "Provider-agnostic GPT Fast mode controls.",
    ),
    ("npm:pi-mcp-adapter", "Use MCP servers from Pi."),
    ("npm:pi-compact-ui", "Compact, information-dense Pi UI."),
    (
        "npm:pi-editor-info",
        "Editor and workspace context helpers.",
    ),
    (
        "npm:pi-permission-control",
        "Permission prompts and controls.",
    ),
    (
        "npm:pi-ask-user",
        "Let the model ask you questions with a choice picker.",
    ),
    ("npm:pi-agent-squad", "Coordinate multiple Pi agents."),
    ("npm:pi-provider-newapi", "Additional provider integration."),
];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn settings_path() -> Option<PathBuf> {
    home().map(|h| h.join(".pi/agent/settings.json"))
}

fn npm_dir() -> Option<PathBuf> {
    home().map(|h| h.join(".pi/agent/npm/node_modules"))
}

fn source_name(source: &str) -> String {
    let raw = source.strip_prefix("npm:").unwrap_or(source);
    raw.rsplit_once('@')
        .filter(|(name, version)| !name.is_empty() && !version.is_empty())
        .map(|(name, _)| name)
        .unwrap_or(raw)
        .to_string()
}

fn source_is_pinned(source: &str) -> bool {
    let raw = source.strip_prefix("npm:").unwrap_or(source);
    raw.rsplit_once('@')
        .is_some_and(|(name, version)| !name.is_empty() && !version.is_empty())
}

fn package_dir(name: &str) -> Option<PathBuf> {
    let root = npm_dir()?;
    if let Some((scope, package)) = name.split_once('/') {
        Some(root.join(scope).join(package))
    } else {
        Some(root.join(name))
    }
}

fn npm() -> Option<PathBuf> {
    cypher_harness::resolve_cli("npm")
}

fn pi() -> Option<PathBuf> {
    cypher_harness::resolve_cli("pi")
}

fn configured_packages() -> Vec<Value> {
    let Some(path) = settings_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("packages").cloned())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn value_source(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("source").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

fn package_enabled(source: &str, values: &[Value]) -> bool {
    values.iter().any(|v| {
        value_source(v).is_some_and(|configured| source_name(&configured) == source_name(source))
            && !v
                .as_object()
                .and_then(|o| o.get("autoload"))
                .is_some_and(|v| v == false)
    })
}

fn manifest(name: &str) -> (Option<String>, Option<String>) {
    let Some(path) = package_dir(name).map(|p| p.join("package.json")) else {
        return (None, None);
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return (None, None);
    };
    (
        value
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string),
        value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
    )
}

pub fn list() -> PiPackagesSnapshot {
    let configured = configured_packages();
    let mut sources: Vec<String> = RECOMMENDED.iter().map(|(s, _)| (*s).to_string()).collect();
    for value in &configured {
        if let Some(source) = value_source(value)
            && !sources
                .iter()
                .any(|s| source_name(s) == source_name(&source))
        {
            sources.push(source);
        }
    }
    let packages = sources
        .into_iter()
        .filter(|source| source.starts_with("npm:"))
        .map(|source| {
            let name = source_name(&source);
            let (version, manifest_description) = manifest(&name);
            let description = RECOMMENDED
                .iter()
                .find(|(recommended, _)| source_name(recommended) == name)
                .map(|(_, description)| (*description).to_string())
                .or(manifest_description);
            PiPackage {
                recommended: RECOMMENDED.iter().any(|(s, _)| source_name(s) == name),
                enabled: package_enabled(&source, &configured),
                installed: package_dir(&name).is_some_and(|p| p.join("package.json").is_file()),
                source,
                name,
                version,
                description,
            }
        })
        .collect();
    PiPackagesSnapshot {
        pi_installed: pi().is_some(),
        npm_available: npm().is_some(),
        packages,
    }
}

async fn capture(exe: &Path, mut command: Command, timeout: Duration) -> Result<Output, String> {
    cypher_harness::compose_child_path(&mut command, exe);
    tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| "The update check timed out. Check your network and try again.".to_string())?
        .map_err(|e| e.to_string())
}

async fn check_pi_update(
    pi_path: &Path,
    npm_path: &Path,
) -> Result<(Option<String>, Option<String>, bool), String> {
    let mut current = Command::new(pi_path);
    current.arg("--version");
    let mut latest = Command::new(npm_path);
    latest.args(["view", PI_PACKAGE, "version", "--json"]);
    let (current, latest) = tokio::join!(
        capture(pi_path, current, UPDATE_CHECK_TIMEOUT),
        capture(npm_path, latest, UPDATE_CHECK_TIMEOUT)
    );
    let current = current?;
    if !current.status.success() {
        return Err(command_error(&current));
    }
    let latest = latest?;
    if !latest.status.success() {
        return Err(command_error(&latest));
    }
    let current = String::from_utf8_lossy(&current.stdout).trim().to_string();
    let latest_text = String::from_utf8_lossy(&latest.stdout);
    let latest = serde_json::from_str::<String>(latest_text.trim())
        .unwrap_or_else(|_| latest_text.trim().trim_matches('"').to_string());
    if current.is_empty() || latest.is_empty() {
        return Err("Pi returned an empty version while checking for updates.".into());
    }
    let available = cypher_update::version_newer(&latest, &current);
    Ok((Some(current), Some(latest), available))
}

fn configured_unpinned_npm_packages() -> std::collections::HashSet<String> {
    configured_packages()
        .iter()
        .filter_map(value_source)
        .filter(|source| source.starts_with("npm:") && !source_is_pinned(source))
        .map(|source| source_name(&source))
        .collect()
}

fn parse_outdated_packages(
    value: &Value,
    configured: &std::collections::HashSet<String>,
) -> Vec<PiPackageUpdate> {
    let mut updates: Vec<PiPackageUpdate> = value
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(name, _)| configured.contains(name.as_str()))
        .filter_map(|(name, facts)| {
            let current_version = facts.get("current")?.as_str()?.to_string();
            let latest_version = facts.get("latest")?.as_str()?.to_string();
            (current_version != latest_version).then(|| PiPackageUpdate {
                name: name.clone(),
                current_version,
                latest_version,
            })
        })
        .collect();
    updates.sort_by(|a, b| a.name.cmp(&b.name));
    updates
}

async fn check_package_updates(npm_path: &Path) -> Result<Vec<PiPackageUpdate>, String> {
    let Some(root) = npm_dir().and_then(|path| path.parent().map(Path::to_path_buf)) else {
        return Ok(Vec::new());
    };
    if !root.join("package.json").is_file() {
        return Ok(Vec::new());
    }
    let mut command = Command::new(npm_path);
    command.args(["outdated", "--prefix"]);
    command.arg(&root);
    command.args(["--json", "--long"]);
    let output = capture(npm_path, command, UPDATE_CHECK_TIMEOUT).await?;
    // npm intentionally exits 1 when outdated dependencies exist. Any JSON
    // object on stdout is therefore authoritative regardless of 0/1.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|_| command_error(&output))?;
    let configured = configured_unpinned_npm_packages();
    Ok(parse_outdated_packages(&value, &configured))
}

fn command_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("Command exited with {}", output.status)
    }
}

async fn run(exe: &Path, mut command: Command) -> Result<(), String> {
    cypher_harness::compose_child_path(&mut command, exe);
    let output = tokio::time::timeout(COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| "The installation timed out. Check your network and try again.".to_string())?
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Err(if !stderr.is_empty() { stderr } else { stdout })
}

const TOOLCHAIN_HINT: &str = "searched PATH, your login shell, Homebrew, and fnm/nvm/volta/pnpm";

pub async fn install_pi() -> Result<(), String> {
    let npm = npm().ok_or_else(|| {
        format!("npm was not found ({TOOLCHAIN_HINT}). Install Node.js/npm first.")
    })?;
    let mut command = Command::new(&npm);
    command.args(["install", "-g", "--ignore-scripts", PI_PACKAGE]);
    run(&npm, command).await
}

pub async fn install_package(source: &str) -> Result<(), String> {
    if !source.starts_with("npm:") {
        return Err("Only npm Pi packages can be installed from this page.".into());
    }
    let pi = pi().ok_or_else(|| format!("Pi is not installed yet ({TOOLCHAIN_HINT})."))?;
    let mut command = Command::new(&pi);
    command.args(["install", source]);
    run(&pi, command).await
}

fn write_settings(value: Value) -> Result<(), String> {
    let path = settings_path().ok_or_else(|| "HOME is not set.".to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "Invalid Pi settings path.".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn set_package_enabled(params: SetPackageEnabled) -> Result<(), String> {
    let path = settings_path().ok_or_else(|| "HOME is not set.".to_string())?;
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut root: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let packages = root
        .as_object_mut()
        .ok_or_else(|| "Pi settings are not a JSON object.".to_string())?
        .entry("packages")
        .or_insert_with(|| Value::Array(Vec::new()));
    let list = packages
        .as_array_mut()
        .ok_or_else(|| "Pi settings packages is not an array.".to_string())?;
    let index = list.iter().position(|v| {
        value_source(v).is_some_and(|source| source_name(&source) == source_name(&params.source))
    });
    if params.enabled {
        if index.is_none() {
            list.push(Value::String(params.source));
        }
    } else if let Some(index) = index {
        list.remove(index);
    }
    write_settings(root)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn npm_source_pin_detection_handles_scoped_and_plain_names() {
        assert!(!source_is_pinned("npm:pi-web-search"));
        assert!(!source_is_pinned("npm:@scope/package"));
        assert!(source_is_pinned("npm:pi-web-search@1.2.3"));
        assert!(source_is_pinned("npm:@scope/package@1.2.3"));
    }

    #[test]
    fn outdated_parser_keeps_only_configured_unpinned_packages() {
        let value = serde_json::json!({
            "pi-web-search": {
                "current": "1.0.0",
                "wanted": "1.1.0",
                "latest": "1.1.0"
            },
            "not-configured": {
                "current": "2.0.0",
                "wanted": "3.0.0",
                "latest": "3.0.0"
            }
        });
        let configured = HashSet::from(["pi-web-search".to_string()]);

        assert_eq!(
            parse_outdated_packages(&value, &configured),
            vec![PiPackageUpdate {
                name: "pi-web-search".into(),
                current_version: "1.0.0".into(),
                latest_version: "1.1.0".into(),
            }]
        );
    }
}
