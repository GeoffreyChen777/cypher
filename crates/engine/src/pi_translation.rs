//! Device-scoped settings for the Pi translation extension.
//!
//! The extension itself runs inside Pi and owns the translation requests. The
//! engine only persists the small, non-secret configuration that the Settings
//! → Agents page edits.

use lingua::{Language, LanguageDetector, LanguageDetectorBuilder};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::pi_runtime::PiRuntimePaths;

const FILE_NAME: &str = "translation.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TranslationOutputMode {
    /// Replace the original final response once translation is complete.
    Replace,
    /// Keep the original response and append the translated response below it.
    /// The transcript folds the original away behind a collapsed toggle, so
    /// this reads like `Replace` with the original one click away.
    #[default]
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
            output_mode: TranslationOutputMode::Append,
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

/// The only languages the detector may answer with, each paired with the ISO
/// 639-3 code the extension compares against.
///
/// This list IS the contract with `LANGUAGE_OPTIONS` in the settings card and
/// `LANGUAGE_ALIASES` in `dist/pi-runtime/extensions/cypher-translation.ts`.
/// Gating compares a detected code against the code a configured language name
/// maps to, so the three lists have to name the same languages: a detector
/// asked to judge a language it was not built with does not answer "unknown",
/// it answers with whichever of these two fits better, and a confident wrong
/// answer is what licenses a wrong skip.
const LANGUAGES: [(Language, &str); 2] = [
    (Language::English, "eng"),
    // Lingua's ISO 639-3 for Chinese is the `zho` macrolanguage, but the alias
    // table maps every spelling of Chinese onto Mandarin's `cmn`, which is what
    // the detector reported before and what stored settings compare against.
    (Language::Chinese, "cmn"),
];

/// Built once and shared: the models are memory-mapped FSTs and the builder
/// walks every one of them.
fn detector() -> &'static LanguageDetector {
    static DETECTOR: OnceLock<LanguageDetector> = OnceLock::new();
    DETECTOR.get_or_init(|| {
        let languages: Vec<Language> = LANGUAGES.iter().map(|(language, _)| *language).collect();
        // High accuracy is the default; `with_low_accuracy_mode` is the opt-out
        // and it degrades exactly the texts this feature sees — chat messages
        // well under the ~120 characters where the cheap mode holds up.
        LanguageDetectorBuilder::from_languages(&languages).build()
    })
}

fn code_of(language: Language) -> &'static str {
    LANGUAGES
        .iter()
        .find(|(candidate, _)| *candidate == language)
        .map(|(_, code)| *code)
        .unwrap_or_default()
}

/// A detection may only skip a paid translation when it is this sure, and this
/// far clear of the runner-up.
///
/// Both halves earn their place. The floor alone would trust a three-way split
/// that happens to lean one way; the margin alone would trust a confident-
/// looking 0.4 against 0.1. Skipping is the irreversible half of the decision —
/// a message wrongly judged "already in the target language" is delivered
/// untranslated with no second chance — so anything short of both falls through
/// to the translation model, which costs a request and gets it right.
const RELIABLE_CONFIDENCE: f64 = 0.70;
const RELIABLE_MARGIN: f64 = 0.40;

pub fn detect_language(text: &str) -> PiLanguageDetection {
    let values = detector().compute_language_confidence_values(text);
    // Sorted by confidence, and normalized to sum to 1 — except for input with
    // no usable tokens (punctuation, digits, an emoji), which comes back as all
    // zeroes rather than as an empty vector.
    match values.first() {
        Some(&(language, confidence)) if confidence > 0.0 => {
            let runner_up = values.get(1).map_or(0.0, |&(_, value)| value);
            PiLanguageDetection {
                language: Some(code_of(language).to_owned()),
                confidence,
                reliable: confidence >= RELIABLE_CONFIDENCE
                    && confidence - runner_up >= RELIABLE_MARGIN,
            }
        }
        _ => PiLanguageDetection {
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
            output_mode: TranslationOutputMode::Replace,
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
        edited.output_mode = TranslationOutputMode::Replace;
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

    /// The motivating case for moving off whatlang: a Chinese sentence carrying
    /// English technical words. whatlang scored these as Turkish, Portuguese,
    /// Estonian and French — always unreliable, so every one of them spent a
    /// translation request and none of them could ever teach the extension
    /// which language the user writes in.
    #[test]
    fn a_chinese_message_holding_english_terms_is_reliably_chinese() {
        for text in [
            "帮我 fix 一下这个 bug",
            "这个 function 的 return value 不对",
            "把 interface 改成 endpoint，然后 commit",
            "在 parser.rs 里加一个 test，跑一下 cargo test 看看",
            "commit 一下",
        ] {
            let detected = detect_language(text);
            assert_eq!(detected.language.as_deref(), Some("cmn"), "{text}");
            assert!(detected.reliable, "{text}");
        }
    }

    #[test]
    fn every_supported_language_is_detected_under_its_own_code() {
        for (text, code) in [
            ("帮我修一下这个解析器的错误", "cmn"),
            ("这是一段中文文本，用于测试离线语言识别。", "cmn"),
            ("Why doesn't the adapter need to inherit?", "eng"),
            (
                "Fix the crash in the markdown parser before the release.",
                "eng",
            ),
        ] {
            let detected = detect_language(text);
            assert_eq!(detected.language.as_deref(), Some(code), "{text}");
            assert!(detected.reliable, "{text}");
        }
    }

    /// The contract with `LANGUAGE_ALIASES` in the extension. Chinese is the
    /// one that would silently break gating: Lingua's own ISO 639-3 for it is
    /// `zho`, which no configured language name maps to.
    #[test]
    fn detected_codes_are_the_codes_the_extension_compares_against() {
        let codes: Vec<&str> = LANGUAGES.iter().map(|(_, code)| *code).collect();
        assert_eq!(codes, ["eng", "cmn"]);
        assert_eq!(code_of(Language::Chinese), "cmn");
    }

    /// Text in a script neither language uses is filtered out by Lingua before
    /// scoring, so it arrives here as all-zero rather than as a confident
    /// wrong answer. That is the behaviour that makes a two-language detector
    /// safe for everyone else: Japanese, Korean, Russian and Arabic carry no
    /// decision at all, so the extension sends them to the translation model
    /// instead of skipping them.
    #[test]
    fn a_script_neither_language_uses_carries_no_decision() {
        for text in [
            "パーサーのバグを直して",
            "파서의 버그를 고쳐줘",
            "исправь ошибку в парсере",
            "أصلح الخطأ في المحلل",
        ] {
            let detected = detect_language(text);
            assert_eq!(detected.language, None, "{text}");
            assert!(!detected.reliable, "{text}");
        }
    }

    /// The known limitation of trimming to two languages, pinned so it reads as
    /// a decision rather than a bug: another Latin-script language has nothing
    /// to lose to, so it comes back as confident English. Nothing downstream
    /// can recover from that, which is why the settings card and the
    /// extension's alias table must offer exactly the languages named in
    /// [`LANGUAGES`] — and why a configured language the table does not know
    /// never skips the model (`translationDecision`).
    #[test]
    fn an_unsupported_latin_language_reads_as_english() {
        let detected = detect_language("corrige le bug dans le parseur de markdown");
        assert_eq!(detected.language.as_deref(), Some("eng"));
        assert!(detected.reliable);
    }

    #[test]
    fn input_with_no_words_carries_no_decision() {
        for text in ["400", "", "   ", "!!!"] {
            let detected = detect_language(text);
            assert_eq!(detected.language, None, "{text:?}");
            assert!(!detected.reliable, "{text:?}");
            assert_eq!(detected.confidence, 0.0, "{text:?}");
        }
    }
}
