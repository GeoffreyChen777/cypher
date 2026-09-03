//! Pi installation and package management for Settings → Agents.
//!
//! Pi owns the package format and settings schema, so package changes go
//! through `pi install` while enablement is represented by the same
//! `~/.pi/agent/settings.json` package list that Pi reads.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const PI_PACKAGE: &str = "@earendil-works/pi-coding-agent";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15 * 60);

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
        "npm:@tunnckocore/pi-gpt-fast-mode",
        "Fast model mode controls.",
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
