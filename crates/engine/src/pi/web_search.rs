//! Web-search fallback for Claude Code sessions (Settings → Providers →
//! Claude): the bundled `pi-web-search-claude-bridge` package routes Pi's
//! `web_search` through another model whenever the conversation model is
//! `claude-bridge/*`.
//!
//! Its config lives in the agent dir as `web-search-claude-bridge.json`:
//! `{"model":"auto"}` (also the default when the file is absent) ranks a
//! search-capable model from the device's catalog on each run, while
//! `{"provider":…,"model":…}` pins one. The package mirrors what automatic
//! chose into `web-search-claude-bridge.resolved.json`; that pointer is
//! display-only and never written here. Enabling the feature is the
//! package's presence in `settings.json`. All of it is device-local.
//!
//! Pinning a `claude-bridge/*` model searches with Claude Code's own
//! WebSearch, in a query separate from the conversation's. That takes the
//! runtime's pi-web-search and pi-claude-bridge patches; a runtime without
//! them refuses the model, so it is only offered where they are (see
//! `pi-runtime/patches/pi-web-search-claude-code.mjs`).

use std::path::Path;

use cypher_proto::WebSearchFallbackSettings;
use serde::Deserialize;

use crate::pi::packages::{self, SetPackageEnabled};
use crate::pi::runtime::PiRuntimePaths;

pub const PACKAGE: &str = "npm:pi-web-search-claude-bridge";
const CONFIG_FILE: &str = "web-search-claude-bridge.json";
const RESOLVED_FILE: &str = "web-search-claude-bridge.resolved.json";
/// The package's own sentinel for "rank one from the catalog".
const AUTO: &str = "auto";
/// Models that search through Claude Code rather than a native search API.
const CLAUDE_CODE_PROVIDER: &str = "claude-bridge";
/// Left in pi-web-search's source by the runtime patch that routes a pinned
/// `claude-bridge/*` model through Claude Code.
const CLAUDE_CODE_MARKER: &str = "CYPHER-RUNTIME-PATCH: claude-code";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetWebSearchFallback {
    pub enabled: bool,
    /// `provider/model` catalog id; `None` (or absent) selects automatic.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Deserialize)]
struct FileShape {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    model: String,
}

fn agent_file(paths: &PiRuntimePaths, name: &str) -> std::path::PathBuf {
    paths.agent_dir.join(name)
}

fn read(path: &Path) -> Option<FileShape> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// The package's `isAutoConfig`: no provider, and no model or the `auto`
/// sentinel. A missing or unreadable file is automatic too.
fn pinned_model(path: &Path) -> Option<String> {
    let file = read(path)?;
    let (provider, model) = (file.provider.trim(), file.model.trim());
    if model.is_empty() || (provider.is_empty() && model.eq_ignore_ascii_case(AUTO)) {
        return None;
    }
    Some(if provider.is_empty() {
        // A bare id (the package's slash command accepts one); kept verbatim.
        model.to_string()
    } else {
        format!("{provider}/{model}")
    })
}

fn resolved_model(path: &Path) -> Option<String> {
    let file = read(path)?;
    let (provider, model) = (file.provider.trim(), file.model.trim());
    (!provider.is_empty() && !model.is_empty()).then(|| format!("{provider}/{model}"))
}

/// Whether this runtime's pi-web-search can search through Claude Code.
fn claude_code_search(paths: &PiRuntimePaths) -> bool {
    std::fs::read_to_string(
        paths
            .current
            .join("npm/node_modules/pi-web-search/src/api.ts"),
    )
    .is_ok_and(|source| source.contains(CLAUDE_CODE_MARKER))
}

pub fn load(paths: &PiRuntimePaths) -> WebSearchFallbackSettings {
    let pinned = pinned_model(&agent_file(paths, CONFIG_FILE));
    WebSearchFallbackSettings {
        claude_code: claude_code_search(paths),
        available: packages::bundled(paths, PACKAGE),
        enabled: packages::enabled(paths, PACKAGE),
        automatic: pinned.is_none(),
        resolved: pinned
            .is_none()
            .then(|| resolved_model(&agent_file(paths, RESOLVED_FILE)))
            .flatten(),
        model: pinned,
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
    if provider.is_empty() || name.is_empty() || name.eq_ignore_ascii_case(AUTO) {
        return Err(invalid());
    }
    Ok((provider, name))
}

/// Write the choice and enable/disable the package. The config is written
/// first so a package toggle never runs against a stale file.
pub fn save(
    paths: &PiRuntimePaths,
    request: SetWebSearchFallback,
) -> Result<WebSearchFallbackSettings, String> {
    let body = match request.model.as_deref() {
        None => serde_json::json!({ "model": AUTO }),
        Some(model) => {
            let (provider, name) = validate_model(model)?;
            if request.enabled && provider == CLAUDE_CODE_PROVIDER && !claude_code_search(paths) {
                return Err(
                    "This device's Pi Runtime can't search through Claude Code yet; update Runtime first."
                        .into(),
                );
            }
            serde_json::json!({ "provider": provider, "model": name })
        }
    };
    if request.enabled && !packages::bundled(paths, PACKAGE) {
        return Err(
            "This device's Pi Runtime does not include the web-search fallback; update Runtime first."
                .into(),
        );
    }
    std::fs::create_dir_all(&paths.agent_dir).map_err(|e| e.to_string())?;
    let path = agent_file(paths, CONFIG_FILE);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&body).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, format!("{text}\n")).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    if packages::enabled(paths, PACKAGE) != request.enabled {
        packages::set_package_enabled(
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
            r#"{"name":"pi-web-search-claude-bridge","version":"0.2.0"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&paths.agent_dir).unwrap();
        std::fs::write(paths.agent_dir.join("settings.json"), r#"{"packages":[]}"#).unwrap();
        paths
    }

    #[test]
    fn automatic_is_the_default_and_pinning_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let paths = runtime_with_package(temp.path());
        let initial = load(&paths);
        assert!(initial.available);
        assert!(!initial.enabled);
        assert!(initial.automatic, "no config file means automatic");
        assert_eq!(initial.model, None);

        let saved = save(
            &paths,
            SetWebSearchFallback {
                enabled: true,
                model: Some("openai-codex/gpt-5.5".into()),
            },
        )
        .unwrap();
        assert!(saved.enabled);
        assert!(!saved.automatic);
        assert_eq!(saved.model.as_deref(), Some("openai-codex/gpt-5.5"));
        let file: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(agent_file(&paths, CONFIG_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(file["provider"], "openai-codex");
        assert_eq!(file["model"], "gpt-5.5");
        assert!(packages::enabled(&paths, PACKAGE));

        // Back to automatic: the package's own sentinel, no provider key.
        let auto = save(
            &paths,
            SetWebSearchFallback {
                enabled: true,
                model: None,
            },
        )
        .unwrap();
        assert!(auto.automatic);
        assert_eq!(auto.model, None);
        let file: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(agent_file(&paths, CONFIG_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(file["model"], "auto");
        assert!(file.get("provider").is_none());

        // Disabling removes the package but keeps the choice.
        let off = save(
            &paths,
            SetWebSearchFallback {
                enabled: false,
                model: Some("openai-codex/gpt-5.5".into()),
            },
        )
        .unwrap();
        assert!(!off.enabled);
        assert!(!packages::enabled(&paths, PACKAGE));
        assert_eq!(load(&paths).model.as_deref(), Some("openai-codex/gpt-5.5"));
    }

    #[test]
    fn reads_the_packages_config_shapes_and_resolved_pointer() {
        let temp = tempfile::tempdir().unwrap();
        let paths = runtime_with_package(temp.path());
        let config = agent_file(&paths, CONFIG_FILE);
        let resolved = agent_file(&paths, RESOLVED_FILE);
        std::fs::write(&resolved, r#"{"provider":"mvp","model":"gpt-5.6-luna"}"#).unwrap();

        for auto in [
            r#"{"model":"auto"}"#,
            r#"{"model":"AUTO"}"#,
            r#"{}"#,
            r#"{"model":""}"#,
            "not json",
        ] {
            std::fs::write(&config, auto).unwrap();
            let settings = load(&paths);
            assert!(settings.automatic, "{auto}");
            assert_eq!(settings.model, None, "{auto}");
            assert_eq!(
                settings.resolved.as_deref(),
                Some("mvp/gpt-5.6-luna"),
                "{auto}"
            );
        }

        // A pinned choice hides the automatic pointer (it isn't in use).
        std::fs::write(&config, r#"{"provider":"mvp","model":"gpt-5.5"}"#).unwrap();
        let settings = load(&paths);
        assert!(!settings.automatic);
        assert_eq!(settings.model.as_deref(), Some("mvp/gpt-5.5"));
        assert_eq!(settings.resolved, None);

        // The slash command also accepts a bare id; keep it verbatim.
        std::fs::write(&config, r#"{"model":"gpt-5.6-luna"}"#).unwrap();
        let settings = load(&paths);
        assert!(!settings.automatic);
        assert_eq!(settings.model.as_deref(), Some("gpt-5.6-luna"));

        std::fs::remove_file(&resolved).unwrap();
        std::fs::write(&config, r#"{"model":"auto"}"#).unwrap();
        assert_eq!(load(&paths).resolved, None);
    }

    #[test]
    fn claude_code_models_need_the_patched_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let paths = runtime_with_package(temp.path());
        let claude = || SetWebSearchFallback {
            enabled: true,
            model: Some("claude-bridge/claude-haiku-4-5".into()),
        };
        let api = paths
            .current
            .join("npm/node_modules/pi-web-search/src/api.ts");
        std::fs::create_dir_all(api.parent().unwrap()).unwrap();
        std::fs::write(&api, "export async function callApiStream() {}\n").unwrap();

        assert!(!load(&paths).claude_code);
        let err = save(&paths, claude()).unwrap_err();
        assert!(err.contains("update Runtime"), "{err}");
        assert!(
            !packages::enabled(&paths, PACKAGE),
            "a refused pin enables nothing"
        );
        // Remembering it while disabled is fine; it routes nothing.
        assert!(
            save(
                &paths,
                SetWebSearchFallback {
                    enabled: false,
                    ..claude()
                },
            )
            .is_ok()
        );

        std::fs::write(
            &api,
            "// CYPHER-RUNTIME-PATCH: claude-code\nexport async function callApiStream() {}\n",
        )
        .unwrap();
        assert!(load(&paths).claude_code);
        let saved = save(&paths, claude()).unwrap();
        assert!(saved.enabled);
        assert_eq!(
            saved.model.as_deref(),
            Some("claude-bridge/claude-haiku-4-5")
        );
        let file: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(agent_file(&paths, CONFIG_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(file["provider"], "claude-bridge");
        assert_eq!(file["model"], "claude-haiku-4-5");
    }

    #[test]
    fn rejects_bad_models_and_enabling_without_the_package() {
        for bad in [
            "",
            "no-slash",
            "/model",
            "provider/",
            "a b/c",
            "x/y\n",
            "p/auto",
        ] {
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
                model: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("update Runtime"));
        assert!(
            save(
                &paths,
                SetWebSearchFallback {
                    enabled: false,
                    model: None,
                },
            )
            .is_ok()
        );
    }
}
