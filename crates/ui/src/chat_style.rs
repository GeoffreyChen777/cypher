//! Client-local chat typography, colors, and column layout.
//!
//! Kept in its own file: the shell's debounced pane/tab snapshot must never
//! overwrite an appearance change. No engine RPC or device targeting is involved.
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use gpui::{App, Global, Hsla, SharedString};
use serde::{Deserialize, Serialize};

use crate::theme::{Appearance, MarkdownMetrics, Theme};

pub const FILE_NAME: &str = "chat-appearance.json";
pub const CONTENT_WIDTH: f32 = 736.0;
pub const COMPOSER_WIDTH: f32 = 768.0;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChatColors {
    pub text: Option<String>,
    pub background: Option<String>,
    pub accent: Option<String>,
    pub user_bubble: Option<String>,
    pub code_block_background: Option<String>,
    pub code_block_text: Option<String>,
    pub inline_code_text: Option<String>,
    pub inline_code_background: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChatAppearance {
    pub font_size: f32,
    pub code_font_size: f32,
    pub font_family: Option<String>,
    pub code_font_family: Option<String>,
    /// Multiplier over the existing line heights (100% preserves the old look).
    pub line_spacing: f32,
    pub paragraph_spacing: f32,
    pub message_spacing: f32,
    pub wide: bool,
    pub light: ChatColors,
    pub dark: ChatColors,
}

impl Default for ChatAppearance {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            code_font_size: 12.5,
            font_family: None,
            code_font_family: None,
            line_spacing: 1.0,
            paragraph_spacing: 12.0,
            message_spacing: 14.0,
            wide: false,
            light: ChatColors::default(),
            dark: ChatColors::default(),
        }
    }
}

/// Accept full RGB hex only; empty fields mean "follow the theme".
pub fn normalize_hex(value: &str) -> Option<String> {
    let value = value.trim();
    let digits = value.strip_prefix('#').unwrap_or(value);
    (digits.len() == 6 && digits.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("#{}", digits.to_ascii_uppercase()))
}

pub fn color(value: &Option<String>) -> Option<Hsla> {
    let normalized = normalize_hex(value.as_deref()?)?;
    u32::from_str_radix(&normalized[1..], 16)
        .ok()
        .map(|rgb| gpui::rgb(rgb).into())
}

fn bounded(value: f32, min: f32, max: f32, default: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        default
    }
}

fn clean_font(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let v = v.trim();
        (!v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control)).then(|| v.to_string())
    })
}

impl ChatAppearance {
    pub fn sanitized(mut self) -> Self {
        self.font_size = bounded(self.font_size, 12.0, 32.0, 14.0);
        self.code_font_size = bounded(self.code_font_size, 10.0, 24.0, 12.5);
        self.line_spacing = bounded(self.line_spacing, 0.8, 2.0, 1.0);
        self.paragraph_spacing = bounded(self.paragraph_spacing, 0.0, 40.0, 12.0);
        self.message_spacing = bounded(self.message_spacing, 4.0, 64.0, 14.0);
        self.font_family = clean_font(self.font_family);
        self.code_font_family = clean_font(self.code_font_family);
        for palette in [&mut self.light, &mut self.dark] {
            for value in [
                &mut palette.text,
                &mut palette.background,
                &mut palette.accent,
                &mut palette.user_bubble,
                &mut palette.code_block_background,
                &mut palette.code_block_text,
                &mut palette.inline_code_text,
                &mut palette.inline_code_background,
            ] {
                *value = value.as_deref().and_then(normalize_hex);
            }
        }
        self
    }

    pub fn colors(&self, appearance: Appearance) -> &ChatColors {
        if appearance.is_dark() {
            &self.dark
        } else {
            &self.light
        }
    }

    pub fn colors_mut(&mut self, appearance: Appearance) -> &mut ChatColors {
        if appearance.is_dark() {
            &mut self.dark
        } else {
            &mut self.light
        }
    }

    pub fn metrics(&self) -> MarkdownMetrics {
        MarkdownMetrics {
            body_size: self.font_size,
            body_line_height: 22.0 * (self.font_size / 14.0) * self.line_spacing,
            code_size: self.code_font_size,
            code_line_height: 18.0 * (self.code_font_size / 12.5) * self.line_spacing,
            block_gap: self.paragraph_spacing,
        }
    }

    pub fn input_line_height(&self) -> f32 {
        22.75 * (self.font_size / 14.0) * self.line_spacing
    }

    pub fn load(dir: &Path) -> Self {
        match std::fs::read(dir.join(FILE_NAME)) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(settings) => settings.sanitized(),
                Err(error) => {
                    tracing::warn!(%error, "invalid chat appearance; using defaults");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(dir)?;
        let temp = dir.join(format!(".chat-appearance-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            let bytes = serde_json::to_vec_pretty(&self.clone().sanitized())
                .map_err(std::io::Error::other)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, dir.join(FILE_NAME))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
}

pub struct ChatAppearanceState {
    pub settings: ChatAppearance,
    pub fonts: Arc<Vec<String>>,
    revision: u64,
    dir: PathBuf,
}
impl Global for ChatAppearanceState {}

pub fn init(dir: PathBuf, cx: &mut App) {
    let settings = ChatAppearance::load(&dir);
    let mut fonts = cx.text_system().all_font_names();
    fonts.retain(|name| !name.starts_with('.') || name == ".SystemUIFont");
    fonts.sort_unstable();
    fonts.dedup();
    cx.set_global(ChatAppearanceState {
        settings,
        fonts: Arc::new(fonts),
        revision: 1,
        dir,
    });
}

pub fn settings(cx: &App) -> &ChatAppearance {
    static DEFAULT: LazyLock<ChatAppearance> = LazyLock::new(ChatAppearance::default);
    cx.try_global::<ChatAppearanceState>()
        .map(|state| &state.settings)
        .unwrap_or(&DEFAULT)
}

/// Save first; a failed write leaves the previous live configuration intact.
pub fn set(settings: ChatAppearance, cx: &mut App) -> std::io::Result<()> {
    let settings = settings.sanitized();
    let Some(state) = cx.try_global::<ChatAppearanceState>() else {
        return Err(std::io::Error::other("Chat appearance is not initialized."));
    };
    settings.save(&state.dir)?;
    if settings == state.settings {
        return Ok(());
    }
    let state = cx.global_mut::<ChatAppearanceState>();
    state.settings = settings;
    state.revision = state.revision.wrapping_add(1);
    cx.refresh_windows();
    Ok(())
}

/// A scoped theme, never installed as the app theme. Sidebar/settings/terminal
/// typography and palette stay unchanged.
pub fn theme(cx: &App) -> Theme {
    let base = Theme::of(cx);
    if let Some(state) = cx.try_global::<ChatAppearanceState>() {
        resolve(&state.settings, base, state.revision, &state.fonts)
    } else {
        base.clone()
    }
}

pub fn resolve(settings: &ChatAppearance, base: &Theme, revision: u64, fonts: &[String]) -> Theme {
    let mut theme = base.clone();
    theme.markdown = settings.metrics();
    theme.text_style_revision = base.text_style_revision.wrapping_add(revision);
    for (requested, target) in [
        (&settings.font_family, &mut theme.font_sans),
        (&settings.code_font_family, &mut theme.font_mono),
    ] {
        if let Some(name) = requested
            && fonts.iter().any(|font| font == name)
        {
            *target = SharedString::from(name.clone());
        }
    }
    let colors = settings.colors(base.appearance);
    if let Some(value) = color(&colors.background) {
        theme.bg = value;
        theme.input_bg = value.blend(base.wash(0.04));
    }
    if let Some(value) = color(&colors.text) {
        theme.text = value;
    }
    if let Some(value) = color(&colors.accent) {
        theme.accent = value;
        theme.markdown_link = Some(value);
    }
    theme.user_bubble = color(&colors.user_bubble).or(base.user_bubble);
    theme.code_block_background = color(&colors.code_block_background);
    theme.code_block_text = color(&colors.code_block_text);
    theme.inline_code_text = color(&colors.inline_code_text).or(base.inline_code_text);
    theme.inline_code_background =
        color(&colors.inline_code_background).or(base.inline_code_background);
    theme
}

pub fn bubble(theme: &Theme) -> Hsla {
    theme.user_bubble.unwrap_or_else(|| {
        theme.wash(if theme.appearance.is_dark() {
            0.08
        } else {
            0.04
        })
    })
}

/// Paint custom chat backgrounds on the rounded panel, never on rectangular
/// transcript children: GPUI overflow masks do not clip to border radii.
/// Non-chat surfaces retain their app-theme background.
pub fn panel_background(settings: &ChatAppearance, base: &Theme, is_chat: bool) -> Hsla {
    if is_chat {
        color(&settings.colors(base.appearance).background)
            .unwrap_or(base.regions.chat_background.unwrap_or(base.surface))
    } else {
        base.surface
    }
}

pub fn contrast_warnings(theme: &Theme) -> Vec<&'static str> {
    let mut warnings = Vec::new();
    for (foreground, background, label) in [
        (theme.text, theme.bg, "Message text / chat background"),
        (
            theme.markdown_link.unwrap_or(theme.text),
            theme.bg,
            "Links / chat background",
        ),
        (
            theme.text,
            theme.bg.blend(bubble(theme)),
            "Message text / user bubble",
        ),
        (
            theme.code_block_text.unwrap_or(theme.text),
            theme
                .bg
                .blend(crate::markdown::render::code_block_background(theme)),
            "Code text / code block background",
        ),
        (
            crate::markdown::render::inline_code_text(theme),
            theme
                .bg
                .blend(crate::markdown::render::inline_code_wash(theme)),
            "Inline code text / inline code background",
        ),
    ] {
        if crate::theme::contrast_ratio(foreground, background) < 4.5 {
            warnings.push(label);
        }
    }
    warnings
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorPreset {
    #[default]
    Default,
    #[serde(alias = "ocean")]
    Catppuccin,
    #[serde(alias = "forest")]
    Nord,
    #[serde(alias = "warm")]
    Gruvbox,
}
impl ColorPreset {
    pub const ALL: [Self; 4] = [Self::Default, Self::Catppuccin, Self::Nord, Self::Gruvbox];
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Catppuccin => "Catppuccin",
            Self::Nord => "Nord",
            Self::Gruvbox => "Gruvbox Soft",
        }
    }
    pub fn colors(self, appearance: Appearance) -> ChatColors {
        let dark = appearance.is_dark();
        let (background, text, accent, bubble) = match (self, dark) {
            (Self::Default, _) => return ChatColors::default(),
            // Official palette tokens; links and mapping details in docs/appearance-colors.md.
            // Catppuccin Mocha / Latte: Base, Text, Mauve, Surface0 / Mantle.
            (Self::Catppuccin, true) => ("#1E1E2E", "#CDD6F4", "#CBA6F7", "#313244"),
            (Self::Catppuccin, false) => ("#EFF1F5", "#4C4F69", "#8839EF", "#E6E9EF"),
            // Nord: Polar Night / Snow Storm. The light accent uses nord3
            // rather than pale Frost so links remain readable on nord6.
            (Self::Nord, true) => ("#2E3440", "#D8DEE9", "#88C0D0", "#3B4252"),
            (Self::Nord, false) => ("#ECEFF4", "#2E3440", "#4C566A", "#E5E9F0"),
            // Gruvbox soft dark; light uses a less yellow warm-paper
            // adaptation while retaining the original text/blue accents.
            (Self::Gruvbox, true) => ("#32302F", "#EBDBB2", "#83A598", "#3C3836"),
            (Self::Gruvbox, false) => ("#F7F5EF", "#3C3836", "#076678", "#EEEAE1"),
        };
        ChatColors {
            text: Some(text.into()),
            background: Some(background.into()),
            accent: Some(accent.into()),
            user_bubble: Some(bubble.into()),
            inline_code_text: Some(text.into()),
            inline_code_background: Some(bubble.into()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_existing_typography_and_layout() {
        let defaults = ChatAppearance::default();
        assert_eq!(defaults.metrics(), MarkdownMetrics::default());
        assert_eq!(
            defaults.input_line_height(),
            crate::composer::INPUT_LINE_HEIGHT
        );
        assert_eq!(
            defaults.paragraph_spacing,
            crate::markdown::render::MD_BLOCK_GAP
        );
        assert_eq!(defaults.message_spacing, crate::transcript::GAP_TURN);
        assert!(!defaults.wide);
        for base in [Theme::light(), Theme::dark()] {
            let resolved = resolve(&defaults, &base, 1, &[]);
            assert_eq!(resolved.bg, base.bg);
            assert_eq!(resolved.text, base.text);
            assert_eq!(resolved.font_sans, base.font_sans);
            assert_eq!(resolved.font_mono, base.font_mono);
            assert_eq!(resolved.markdown_link, None);
            assert_eq!(resolved.user_bubble, None);
            assert_eq!(resolved.code_block_background, None);
            assert_eq!(resolved.code_block_text, None);
            assert_eq!(resolved.inline_code_text, None);
            assert_eq!(resolved.inline_code_background, None);
        }
    }

    #[test]
    fn sparse_settings_and_missing_file_use_defaults() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(ChatAppearance::load(temp.path()), ChatAppearance::default());
        let sparse: ChatAppearance =
            serde_json::from_str(r##"{"wide":true,"dark":{"accent":"#aabbcc"}}"##).unwrap();
        assert!(sparse.wide);
        assert_eq!(sparse.font_size, 14.0);
        assert_eq!(sparse.sanitized().dark.accent.as_deref(), Some("#AABBCC"));
    }

    #[test]
    fn malformed_values_are_bounded_without_losing_other_preferences() {
        let settings = ChatAppearance {
            font_size: f32::NAN,
            code_font_size: -5.0,
            line_spacing: f32::INFINITY,
            paragraph_spacing: 1000.0,
            message_spacing: -9.0,
            wide: true,
            font_family: Some("  Test Font  ".into()),
            code_font_family: Some("bad\nfont".into()),
            dark: ChatColors {
                text: Some("not a color".into()),
                accent: Some("abcdef".into()),
                ..Default::default()
            },
            ..Default::default()
        }
        .sanitized();
        assert_eq!(settings.font_size, 14.0);
        assert_eq!(settings.code_font_size, 10.0);
        assert_eq!(settings.line_spacing, 1.0);
        assert_eq!(settings.paragraph_spacing, 40.0);
        assert_eq!(settings.message_spacing, 4.0);
        assert!(settings.wide);
        assert_eq!(settings.font_family.as_deref(), Some("Test Font"));
        assert_eq!(settings.code_font_family, None);
        assert_eq!(settings.dark.text, None);
        assert_eq!(settings.dark.accent.as_deref(), Some("#ABCDEF"));
    }

    #[test]
    fn local_file_round_trip_does_not_touch_shell_settings() {
        let temp = tempfile::tempdir().unwrap();
        let ui_path = crate::settings::UiSettings::path(temp.path());
        std::fs::write(&ui_path, b"existing pane/tab settings").unwrap();
        let mut settings = ChatAppearance {
            font_size: 20.0,
            line_spacing: 1.3,
            wide: true,
            ..Default::default()
        };
        settings.dark = ColorPreset::Catppuccin.colors(Appearance::Dark);
        settings.save(temp.path()).unwrap();
        assert_eq!(ChatAppearance::load(temp.path()), settings);
        assert_eq!(
            std::fs::read(&ui_path).unwrap(),
            b"existing pane/tab settings"
        );
        // A subsequent debounced shell save cannot overwrite this separate file.
        crate::settings::UiSettings::default()
            .save(temp.path())
            .unwrap();
        assert_eq!(ChatAppearance::load(temp.path()), settings);
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(temp.path().join(FILE_NAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn corrupt_chat_preferences_do_not_affect_other_settings() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(FILE_NAME), b"{broken").unwrap();
        let other = temp.path().join("ui-settings.json");
        std::fs::write(&other, b"keep this").unwrap();
        assert_eq!(ChatAppearance::load(temp.path()), ChatAppearance::default());
        assert_eq!(std::fs::read(other).unwrap(), b"keep this");
    }

    #[test]
    fn scoped_fonts_fallback_without_changing_the_ui_theme() {
        let base = Theme::dark();
        let settings = ChatAppearance {
            font_family: Some("Example Serif".into()),
            code_font_family: Some("Missing Mono".into()),
            ..Default::default()
        };
        let resolved = resolve(&settings, &base, 9, &["Example Serif".into()]);
        assert_eq!(resolved.font_sans.as_ref(), "Example Serif");
        assert_eq!(resolved.font_mono, base.font_mono);
        assert_eq!(base.font_sans.as_ref(), "Geist");
        assert_eq!(resolved.text_style_revision, 9);
    }

    #[test]
    fn palettes_are_independent_and_presets_have_readable_contrast() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            for preset in ColorPreset::ALL {
                let mut settings = ChatAppearance::default();
                *settings.colors_mut(appearance) = preset.colors(appearance);
                let opposite = if appearance.is_dark() {
                    Appearance::Light
                } else {
                    Appearance::Dark
                };
                assert_eq!(settings.colors(opposite), &ChatColors::default());
                let theme = resolve(&settings, &Theme::for_appearance(appearance), 1, &[]);
                assert!(
                    contrast_warnings(&theme).is_empty(),
                    "{preset:?} {appearance:?}: {:?}",
                    contrast_warnings(&theme)
                );
            }
        }
    }

    #[test]
    fn custom_low_contrast_is_reported_not_silently_recolored() {
        let mut settings = ChatAppearance::default();
        settings.light.text = Some("#FFFFFF".into());
        settings.light.background = Some("#FFFFFF".into());
        let theme = resolve(&settings, &Theme::light(), 1, &[]);
        assert!(!contrast_warnings(&theme).is_empty());
        assert_eq!(theme.text, theme.bg);
    }

    #[test]
    fn rounded_chat_panels_own_preset_backgrounds_but_other_surfaces_do_not() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            let base = Theme::for_appearance(appearance);
            let mut settings = ChatAppearance::default();
            assert_eq!(panel_background(&settings, &base, true), base.surface);
            for preset in ColorPreset::ALL {
                *settings.colors_mut(appearance) = preset.colors(appearance);
                assert_eq!(
                    panel_background(&settings, &base, true),
                    color(&settings.colors(appearance).background).unwrap_or(base.surface),
                );
                assert_eq!(
                    panel_background(&settings, &base, false),
                    base.surface,
                    "settings, Diff and terminal cards must not inherit chat colors",
                );
            }
            settings.colors_mut(appearance).background = Some("#123456".into());
            assert_eq!(
                panel_background(&settings, &base, true),
                gpui::rgb(0x123456).into()
            );
            settings.colors_mut(appearance).background = None;
            assert_eq!(panel_background(&settings, &base, true), base.surface);
        }
    }

    #[test]
    fn code_colors_round_trip_independently_without_changing_the_base_theme() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = ChatAppearance::default();
        settings.dark.code_block_background = Some("#101020".into());
        settings.dark.code_block_text = Some("#ccddff".into());
        settings.save(temp.path()).unwrap();
        let restored = ChatAppearance::load(temp.path());
        assert_eq!(restored.dark.code_block_text.as_deref(), Some("#CCDDFF"));
        assert_eq!(restored.light.code_block_text, None);
        let base = Theme::dark();
        let resolved = resolve(&restored, &base, 7, &[]);
        assert_eq!(
            resolved.code_block_background,
            color(&restored.dark.code_block_background)
        );
        assert_eq!(
            resolved.code_block_text,
            color(&restored.dark.code_block_text)
        );
        assert_eq!(resolved.text, base.text);
        assert_eq!(resolved.code_text, base.code_text);
        assert_eq!(resolved.syntax.variable, base.syntax.variable);
        assert_eq!(base.code_block_text, None);
        assert!(!contrast_warnings(&resolved).contains(&"Code text / code block background"));
    }

    #[test]
    fn legacy_colors_and_invalid_code_colors_fall_back_safely() {
        let mut settings: ChatAppearance =
            serde_json::from_str(r##"{"dark":{"text":"#F0F0F0","background":"#101010"}}"##)
                .unwrap();
        assert_eq!(settings.dark.code_block_background, None);
        assert_eq!(settings.dark.code_block_text, None);
        settings.dark.code_block_background = Some("invalid".into());
        settings.dark.code_block_text = Some("#123".into());
        let settings = settings.sanitized();
        assert_eq!(settings.dark.code_block_text, None);
        assert_eq!(settings.dark.code_block_background, None);
        assert_eq!(settings.dark.text.as_deref(), Some("#F0F0F0"));
    }

    #[test]
    fn code_color_contrast_uses_the_code_background_not_chat_background() {
        let mut settings = ChatAppearance::default();
        settings.dark.code_block_background = Some("#FFFFFF".into());
        settings.dark.code_block_text = Some("#FFFFFF".into());
        let resolved = resolve(&settings, &Theme::dark(), 1, &[]);
        assert!(contrast_warnings(&resolved).contains(&"Code text / code block background"));
    }

    #[test]
    fn inline_colors_round_trip_without_recoloring_mentions_or_code_blocks() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = ChatAppearance::default();
        settings.dark.inline_code_text = Some("#ffe08a".into());
        settings.dark.inline_code_background = Some("#47391d".into());
        settings.dark.code_block_background = Some("#101020".into());
        settings.save(temp.path()).unwrap();
        let restored = ChatAppearance::load(temp.path());
        assert_eq!(restored.dark.inline_code_text.as_deref(), Some("#FFE08A"));
        assert_eq!(
            restored.dark.inline_code_background.as_deref(),
            Some("#47391D")
        );
        assert_eq!(restored.light.inline_code_text, None);
        assert_eq!(restored.light.inline_code_background, None);
        let base = Theme::dark();
        let theme = resolve(&restored, &base, 9, &[]);
        assert_eq!(
            theme.inline_code_text,
            color(&restored.dark.inline_code_text)
        );
        assert_eq!(
            theme.inline_code_background,
            color(&restored.dark.inline_code_background)
        );
        assert_eq!(
            theme.code_block_background,
            color(&restored.dark.code_block_background)
        );
        assert_eq!(theme.code_block_text, None);
        assert_eq!(
            theme.code_text, base.code_text,
            "mention text must keep its own color"
        );
        assert_eq!(
            theme.code_wash, base.code_wash,
            "mention backgrounds must stay unchanged"
        );
        assert_eq!(theme.text, base.text);
        assert!(!contrast_warnings(&theme).contains(&"Inline code text / inline code background"));
    }

    #[test]
    fn inline_colors_are_optional_validated_and_checked_for_contrast() {
        let mut settings: ChatAppearance =
            serde_json::from_str(r##"{"dark":{"codeBlockText":"#FFFFFF"}}"##).unwrap();
        assert_eq!(settings.dark.inline_code_text, None);
        settings.dark.inline_code_text = Some("invalid".into());
        settings.dark.inline_code_background = Some("#12".into());
        settings = settings.sanitized();
        assert_eq!(settings.dark.inline_code_text, None);
        assert_eq!(settings.dark.inline_code_background, None);
        assert_eq!(settings.dark.code_block_text.as_deref(), Some("#FFFFFF"));
        settings.dark.inline_code_text = Some("#101010".into());
        settings.dark.inline_code_background = Some("#101010".into());
        let theme = resolve(&settings, &Theme::dark(), 1, &[]);
        assert!(contrast_warnings(&theme).contains(&"Inline code text / inline code background"));
    }
}
