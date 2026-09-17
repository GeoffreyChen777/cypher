//! Web-search fallback for Claude Code sessions (Settings → Providers →
//! Claude): the bundled `pi-web-search-claude-bridge` package routes Pi's
//! `web_search` through a fallback model whenever the conversation model is
//! `claude-bridge/*`. Its config lives in the agent dir as
//! `web-search-claude-bridge.json` (`{"provider", "model"}`); enabling it is
//! the package's presence in `settings.json`. Both are device-local.

use std::path::Path;

use cypher_proto::WebSearchFallbackSettings;
use serde::Deserialize;

use crate::pi_packages::{self, SetPackageEnabled};
use crate::pi_runtime::PiRuntimePaths;

pub const PACKAGE: &str = "npm:pi-web-search-claude-bridge";
const CONFIG_FILE: &str = "web-search-claude-bridge.json";
/// The package's own default when the file is absent.
const DEFAULT_MODEL: &str = "mvp-lab/gpt-5.6-luna";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetWebSearchFallback {
    pub enabled: bool,
    /// `provider/model` catalog id.
    pub model: String,
}

#[derive(Deserialize)]
struct FileShape {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    model: String,
}

fn config_path(paths: &PiRuntimePaths) -> std::path::PathBuf {
    paths.agent_dir.join(CONFIG_FILE)
}

/// The fallback model stored in the package's config file, as a catalog id.
fn stored_model(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let file: FileShape = serde_json::from_str(&text).ok()?;
    (!file.provider.is_empty() && !file.model.is_empty())
        .then(|| format!("{}/{}", file.provider, file.model))
}

pub fn load(paths: &PiRuntimePaths) -> WebSearchFallbackSettings {
    WebSearchFallbackSettings {
        available: pi_packages::bundled(paths, PACKAGE),
        enabled: pi_packages::enabled(paths, PACKAGE),
        model: stored_model(&config_path(paths)).unwrap_or_else(|| DEFAULT_MODEL.into()),
    }
}

/// `provider/model`: both halves non-empty, no whitespace or control
/// characters, bounded — the same discipline as the title model.
pub fn validate_model(model: &str) -> Result<(&str, &str), String> {
    let invalid = || "Choose a valid model from this device's catalog.".to_string();
    if model.len() > 512 || model.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid());
    }
    let (provider, name) = model.split_once('/').ok_or_else(invalid)?;
    if provider.is_empty() || name.is_empty() {
        return Err(invalid());
    }
    Ok((provider, name))
}

/// Write the model and enable/disable the package. The model is written
/// first so a package toggle never runs against a stale file.
pub fn save(
    paths: &PiRuntimePaths,
    request: SetWebSearchFallback,
) -> Result<WebSearchFallbackSettings, String> {
    let (provider, model) = validate_model(&request.model)?;
    if request.enabled && !pi_packages::bundled(paths, PACKAGE) {
        return Err(
            "This device's Pi Runtime does not include the web-search fallback; update Runtime first."
                .into(),
        );
    }
    std::fs::create_dir_all(&paths.agent_dir).map_err(|e| e.to_string())?;
    let path = config_path(paths);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&serde_json::json!({
        "provider": provider,
        "model": model,
    }))
    .map_err(|e| e.to_string())?;
    std::fs::write(&tmp, format!("{text}\n")).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    if pi_packages::enabled(paths, PACKAGE) != request.enabled {
        pi_packages::set_package_enabled(
            paths,
            SetPackageEnabled {
                source: PACKAGE.into(),
                enabled: request.enabled,
            },
        )?;
    }
    Ok(load(paths))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_with_package(temp: &Path) -> PiRuntimePaths {
        let paths = PiRuntimePaths::for_data_dir(temp);
        let package = paths
            .current
            .join("npm/node_modules/pi-web-search-claude-bridge");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("package.json"),
            r#"{"name":"pi-web-search-claude-bridge","version":"0.1.0"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&paths.agent_dir).unwrap();
        std::fs::write(paths.agent_dir.join("settings.json"), r#"{"packages":[]}"#).unwrap();
        paths
    }

    #[test]
    fn defaults_then_round_trip_and_package_toggle() {
        let temp = tempfile::tempdir().unwrap();
        let paths = runtime_with_package(temp.path());
        let initial = load(&paths);
        assert!(initial.available);
        assert!(!initial.enabled);
        assert_eq!(initial.model, DEFAULT_MODEL);

        let saved = save(
            &paths,
            SetWebSearchFallback {
                enabled: true,
                model: "openai-codex/gpt-5.5".into(),
            },
        )
        .unwrap();
        assert!(saved.enabled);
        assert_eq!(saved.model, "openai-codex/gpt-5.5");
        let file: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(config_path(&paths)).unwrap()).unwrap();
        assert_eq!(file["provider"], "openai-codex");
        assert_eq!(file["model"], "gpt-5.5");
        assert!(pi_packages::enabled(&paths, PACKAGE));

        let off = save(
            &paths,
            SetWebSearchFallback {
                enabled: false,
                model: "openai-codex/gpt-5.5".into(),
            },
        )
        .unwrap();
        assert!(!off.enabled);
        assert!(!pi_packages::enabled(&paths, PACKAGE));
        assert_eq!(
            load(&paths).model,
            "openai-codex/gpt-5.5",
            "model survives disable"
        );
    }

    #[test]
    fn rejects_bad_models_and_missing_package() {
        for bad in ["", "no-slash", "/model", "provider/", "a b/c", "x/y\n"] {
            assert!(validate_model(bad).is_err(), "{bad:?}");
        }
        let temp = tempfile::tempdir().unwrap();
        let paths = PiRuntimePaths::for_data_dir(temp.path());
        std::fs::create_dir_all(&paths.agent_dir).unwrap();
        std::fs::write(paths.agent_dir.join("settings.json"), r#"{"packages":[]}"#).unwrap();
        assert!(!load(&paths).available);
        let err = save(
            &paths,
            SetWebSearchFallback {
                enabled: true,
                model: "p/m".into(),
            },
        )
        .unwrap_err();
        assert!(err.contains("update Runtime"));
        // Disabled writes are fine without the package (the model is kept).
        assert!(
            save(
                &paths,
                SetWebSearchFallback {
                    enabled: false,
                    model: "p/m".into(),
                },
            )
            .is_ok()
        );
    }
}
