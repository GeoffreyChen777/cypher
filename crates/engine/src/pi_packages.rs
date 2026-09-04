//! Pi installation and package management for Settings → Agents.
//!
//! Pi owns the package format and settings schema, so package changes go
//! through the Cypher-owned Pi runtime while enablement is represented by its
//! isolated `<data_dir>/pi-runtime/agent/settings.json`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiPackageUpdate {
    pub name: String,
    pub current_version: String,
    pub latest_version: String,
}

/// Pi CLI + extension update facts published by the engine every six hours.
/// Runtime bundles apply automatically; the explicit action remains available
/// as a retry/repair path from older UI clients.
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
    pub fn update_available(&self) -> bool {
        self.pi_update_available || !self.package_updates.is_empty()
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

fn settings_path(paths: &crate::pi_runtime::PiRuntimePaths) -> PathBuf {
    paths.agent_dir.join("settings.json")
}

fn npm_dir(paths: &crate::pi_runtime::PiRuntimePaths) -> PathBuf {
    paths.agent_dir.join("npm/node_modules")
}

fn source_name(source: &str) -> String {
    if let Some(raw) = source.strip_prefix("npm:") {
        return raw
            .rsplit_once('@')
            .filter(|(name, version)| !name.is_empty() && !version.is_empty())
            .map(|(name, _)| name)
            .unwrap_or(raw)
            .to_string();
    }
    std::fs::read_to_string(Path::new(source).join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| {
            value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| source.to_string())
}

fn package_at(root: &Path, name: &str) -> PathBuf {
    if let Some((scope, package)) = name.split_once('/') {
        root.join(scope).join(package)
    } else {
        root.join(name)
    }
}

fn user_package_dir(paths: &crate::pi_runtime::PiRuntimePaths, name: &str) -> PathBuf {
    package_at(&npm_dir(paths), name)
}

fn bundled_package_dir(paths: &crate::pi_runtime::PiRuntimePaths, name: &str) -> PathBuf {
    package_at(&paths.current.join("npm/node_modules"), name)
}

fn bundled_source(paths: &crate::pi_runtime::PiRuntimePaths, source: &str) -> Option<String> {
    let path = bundled_package_dir(paths, &source_name(source));
    path.join("package.json")
        .is_file()
        .then(|| path.display().to_string())
}

fn bundled_setting(paths: &crate::pi_runtime::PiRuntimePaths, source: &str) -> Option<Value> {
    let name = source_name(source);
    let source = bundled_source(paths, source)?;
    Some(if name == "pi-permission-control" {
        serde_json::json!({
            "source": source,
            "extensions": ["-index.ts"]
        })
    } else {
        Value::String(source)
    })
}

fn npm(paths: &crate::pi_runtime::PiRuntimePaths) -> Option<PathBuf> {
    paths
        .npm_executable
        .is_file()
        .then(|| paths.npm_executable.clone())
}

fn pi(paths: &crate::pi_runtime::PiRuntimePaths) -> Option<PathBuf> {
    paths.executable.is_file().then(|| paths.executable.clone())
}

fn configured_packages(paths: &crate::pi_runtime::PiRuntimePaths) -> Vec<Value> {
    let path = settings_path(paths);
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

fn manifest(
    paths: &crate::pi_runtime::PiRuntimePaths,
    name: &str,
) -> (Option<String>, Option<String>) {
    let bundled = bundled_package_dir(paths, name);
    let directory = if bundled.join("package.json").is_file() {
        bundled
    } else {
        user_package_dir(paths, name)
    };
    let path = directory.join("package.json");
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

pub fn list(paths: &crate::pi_runtime::PiRuntimePaths) -> PiPackagesSnapshot {
    let configured = configured_packages(paths);
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
        .map(|source| {
            let name = source_name(&source);
            let (version, manifest_description) = manifest(paths, &name);
            let recommended = RECOMMENDED.iter().any(|(s, _)| source_name(s) == name);
            let installed = if recommended {
                bundled_package_dir(paths, &name)
                    .join("package.json")
                    .is_file()
            } else if source.starts_with("npm:") {
                user_package_dir(paths, &name)
                    .join("package.json")
                    .is_file()
            } else {
                Path::new(&source).join("package.json").is_file()
            };
            let description = RECOMMENDED
                .iter()
                .find(|(recommended, _)| source_name(recommended) == name)
                .map(|(_, description)| (*description).to_string())
                .or(manifest_description);
            PiPackage {
                recommended,
                enabled: package_enabled(&source, &configured),
                installed,
                source,
                name,
                version,
                description,
            }
        })
        .collect();
    PiPackagesSnapshot {
        pi_installed: pi(paths).is_some(),
        npm_available: npm(paths).is_some(),
        packages,
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

pub async fn install_package(
    paths: &crate::pi_runtime::PiRuntimePaths,
    source: &str,
) -> Result<(), String> {
    if !source.starts_with("npm:") {
        return Err("Only npm Pi packages can be installed from this page.".into());
    }
    if let Some(source) = bundled_source(paths, source) {
        return set_package_enabled(
            paths,
            SetPackageEnabled {
                source,
                enabled: true,
            },
        );
    }
    let pi = pi(paths).ok_or_else(|| "Cypher Pi Runtime is not installed yet.".to_string())?;
    let mut command = Command::new(&pi);
    command.args(["install", source]);
    command.env("PI_CODING_AGENT_DIR", &paths.agent_dir);
    command.env("PI_PACKAGE_DIR", &paths.package_dir);
    run(&pi, command).await
}

fn write_settings(paths: &crate::pi_runtime::PiRuntimePaths, value: Value) -> Result<(), String> {
    let path = settings_path(paths);
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

pub fn set_package_enabled(
    paths: &crate::pi_runtime::PiRuntimePaths,
    params: SetPackageEnabled,
) -> Result<(), String> {
    let path = settings_path(paths);
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
            let setting = bundled_setting(paths, &params.source)
                .unwrap_or_else(|| Value::String(params.source));
            list.push(setting);
        }
    } else if let Some(index) = index {
        list.remove(index);
    }
    write_settings(paths, root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_path_is_inside_the_cypher_runtime_root() {
        let paths =
            crate::pi_runtime::PiRuntimePaths::for_data_dir(Path::new("/tmp/cypher-isolated"));
        assert_eq!(
            settings_path(&paths),
            Path::new("/tmp/cypher-isolated/pi-runtime/agent/settings.json")
        );
    }

    #[test]
    fn enabling_a_bundled_package_uses_the_stable_runtime_path() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::pi_runtime::PiRuntimePaths::for_data_dir(temp.path());
        let package = paths.current.join("npm/node_modules/pi-web-search");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("package.json"),
            r#"{"name":"pi-web-search","version":"1.4.0"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&paths.agent_dir).unwrap();
        std::fs::write(settings_path(&paths), r#"{"packages":[]}"#).unwrap();

        set_package_enabled(
            &paths,
            SetPackageEnabled {
                source: "npm:pi-web-search".into(),
                enabled: true,
            },
        )
        .unwrap();

        let configured = configured_packages(&paths);
        assert_eq!(configured.len(), 1);
        assert_eq!(
            value_source(&configured[0]).as_deref(),
            Some(package.to_str().unwrap())
        );
        assert!(package_enabled("npm:pi-web-search", &configured));
    }
}
