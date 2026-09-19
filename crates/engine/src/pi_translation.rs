//! Device-scoped settings for the Pi translation extension.
//!
//! The extension itself runs inside Pi and owns the translation requests. The
//! engine only persists the small, non-secret configuration that the Settings
//! → Agents page edits.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::pi_runtime::PiRuntimePaths;

const FILE_NAME: &str = "translation.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TranslationOutputMode {
    /// Replace the original final response once translation is complete.
    #[default]
    Replace,
    /// Keep the original response and append the translated response below it.
    Append,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PiTranslationSettings {
    /// A language name, ISO code, or `auto`.
    pub source_language: String,
    pub target_language: String,
    /// `provider/model` used for the actual translation request.
    pub translation_model: String,
    /// `provider/model` ids for which translation is enabled in Pi sessions.
    pub enabled_models: Vec<String>,
    pub output_mode: TranslationOutputMode,
    pub translate_user_messages: bool,
    pub translate_final_responses: bool,
}

impl Default for PiTranslationSettings {
    fn default() -> Self {
        Self {
            source_language: "auto".into(),
            target_language: "English".into(),
            translation_model: String::new(),
            enabled_models: Vec::new(),
            output_mode: TranslationOutputMode::Replace,
            translate_user_messages: true,
            translate_final_responses: true,
        }
    }
}

impl PiTranslationSettings {
    pub fn new_model_selections<'a>(&'a self, previous: &Self) -> Vec<&'a String> {
        let mut added: Vec<_> = self
            .enabled_models
            .iter()
            .filter(|id| !previous.enabled_models.contains(id))
            .collect();
        if !self.translation_model.is_empty()
            && self.translation_model != previous.translation_model
        {
            added.push(&self.translation_model);
        }
        added
    }

    pub fn validate(&self) -> Result<(), String> {
        for (label, value, limit) in [
            ("source language", self.source_language.as_str(), 64),
            ("target language", self.target_language.as_str(), 64),
            ("translation model", self.translation_model.as_str(), 256),
        ] {
            if value.chars().count() > limit {
                return Err(format!("{label} is too long."));
            }
            if value.contains(['\r', '\n', '\0']) {
                return Err(format!("{label} contains an invalid character."));
            }
        }
        if !self.enabled_models.is_empty()
            && (self.source_language.trim().is_empty()
                || self.target_language.trim().is_empty()
                || self.translation_model.trim().is_empty())
        {
            return Err(
                "Select at least one session model only after source language, target language, and translation model are set."
                    .into(),
            );
        }
        if self.enabled_models.len() > 128
            || self
                .enabled_models
                .iter()
                .any(|model| model.chars().count() > 256 || model.contains(['\r', '\n', '\0']))
        {
            return Err("Too many or invalid enabled model ids.".into());
        }
        Ok(())
    }
}

pub fn settings_path(paths: &PiRuntimePaths) -> PathBuf {
    paths.agent_dir.join(FILE_NAME)
}

pub fn load(paths: &PiRuntimePaths) -> PiTranslationSettings {
    let Some(bytes) = std::fs::read(settings_path(paths)).ok() else {
        return PiTranslationSettings::default();
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return PiTranslationSettings::default();
    };
    // Migrate the original single-model field. The old global `enabled`
    // switch intentionally does not carry over: enablement is now explicit
    // per session model.
    if let Some(object) = value.as_object_mut()
        && object.get("translationModel").is_none()
        && let Some(model) = object.remove("model")
    {
        object.insert("translationModel".into(), model);
    }
    serde_json::from_value(value).unwrap_or_default()
}

pub fn save(
    paths: &PiRuntimePaths,
    settings: PiTranslationSettings,
) -> Result<PiTranslationSettings, String> {
    settings.validate()?;
    std::fs::create_dir_all(&paths.agent_dir).map_err(|err| err.to_string())?;
    let path = settings_path(paths);
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&settings).map_err(|err| err.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|err| err.to_string())?;
    std::fs::rename(&temporary, &path).map_err(|err| err.to_string())?;
    Ok(settings)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiLanguageDetection {
    pub language: Option<String>,
    pub confidence: f64,
    pub reliable: bool,
}

pub fn detect_language(text: &str) -> PiLanguageDetection {
    match whatlang::detect(text) {
        Some(info) => PiLanguageDetection {
            language: Some(info.lang().code().into()),
            confidence: info.confidence(),
            reliable: info.is_reliable(),
        },
        None => PiLanguageDetection {
            language: None,
            confidence: 0.0,
            reliable: false,
        },
    }
}

/// Keep the file helper independently testable without requiring a runtime
/// installation. This is also useful for migration tooling.
#[allow(dead_code)]
fn _path_for_agent(agent_dir: &Path) -> PathBuf {
    agent_dir.join(FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> PiRuntimePaths {
        let current = root.join("current");
        PiRuntimePaths {
            root: root.into(),
            current: current.clone(),
            executable: current.join("bin/pi"),
            npm_executable: current.join("bin/npm"),
            package_dir: current.join("pi"),
            agent_dir: root.join("agent"),
        }
    }

    #[test]
    fn missing_settings_use_safe_defaults() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(load(&paths(temp.path())), PiTranslationSettings::default());
    }

    #[test]
    fn settings_round_trip_and_validation() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let settings = PiTranslationSettings {
            source_language: "Chinese".into(),
            target_language: "English".into(),
            translation_model: "openai/gpt-4o-mini".into(),
            enabled_models: vec!["openai/gpt-5".into()],
            output_mode: TranslationOutputMode::Append,
            ..Default::default()
        };
        save(&paths, settings.clone()).unwrap();
        assert_eq!(load(&paths), settings);
        assert!(
            save(
                &paths,
                PiTranslationSettings {
                    enabled_models: vec!["openai/gpt-5".into()],
                    ..Default::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn offline_detection_identifies_common_languages() {
        let english = detect_language("This is a short sentence written in English.");
        assert_eq!(english.language.as_deref(), Some("eng"));
        assert!(english.reliable);

        let chinese = detect_language("这是一段中文文本，用于测试离线语言识别。");
        assert_eq!(chinese.language.as_deref(), Some("cmn"));
        assert!(chinese.reliable);
    }

    #[test]
    fn ordinary_edits_and_removals_do_not_require_model_discovery() {
        let previous = PiTranslationSettings {
            translation_model: "provider/translator".into(),
            enabled_models: vec!["provider/a".into(), "provider/b".into()],
            ..Default::default()
        };
        let mut edited = previous.clone();
        edited.output_mode = TranslationOutputMode::Append;
        edited.target_language = "Chinese".into();
        edited.enabled_models.remove(0);
        assert!(edited.new_model_selections(&previous).is_empty());

        edited.enabled_models.push("provider/c".into());
        assert_eq!(edited.new_model_selections(&previous), vec!["provider/c"]);
        edited.translation_model = "provider/new-translator".into();
        assert_eq!(
            edited.new_model_selections(&previous),
            vec!["provider/c", "provider/new-translator"]
        );
    }
}
