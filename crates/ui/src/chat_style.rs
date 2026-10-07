//! Client-local chat typography, colors, and column layout.
//!
//! Kept in its own file: the shell's debounced pane/tab snapshot must never
//! overwrite an appearance change. No engine RPC or device targeting is involved.
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use gpui::{App, Global, Hsla, SharedString, hsla};
use serde::{Deserialize, Serialize};

use crate::theme::{Appearance, MarkdownMetrics, Theme};

pub const FILE_NAME: &str = "chat-appearance.json";
pub const CONTENT_WIDTH: f32 = 736.0;
pub const COMPOSER_WIDTH: f32 = 768.0;
/// Most chips an open tool group shows before the older ones fold behind a
/// "Show N earlier tool calls" row. `0` means no cap.
pub const DEFAULT_TOOL_CALL_LIMIT: u32 = 5;
pub const MAX_TOOL_CALL_LIMIT: u32 = 50;

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
    /// Tool chips kept visible per open tool group — the most recent ones.
    /// `0` shows every call.
    pub tool_call_limit: u32,
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
            tool_call_limit: DEFAULT_TOOL_CALL_LIMIT,
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
        self.tool_call_limit = self.tool_call_limit.min(MAX_TOOL_CALL_LIMIT);
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
        let bytes =
            serde_json::to_vec_pretty(&self.clone().sanitized()).map_err(std::io::Error::other)?;
        crate::fs_util::write_atomic(dir, FILE_NAME, &bytes, 0o600)
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

/// The user message plate. Unset, it is a translucent muted blue (~#2C333E on
/// the dark panel, ~#E2E9F4 on white). User reports drove each choice: the
/// old neutral wash sat too close to every other surface, a full-chroma
/// indigo tint read too loud, and warm sand was not colour-blind friendly.
/// Blue survives red-green colour blindness, and the plate also steps in
/// lightness from the panel so hue is not the only cue. Translucent because
/// an opaque plate reads as a slab over glass.
pub fn bubble(theme: &Theme) -> Hsla {
    theme.user_bubble.unwrap_or_else(|| {
        if theme.appearance.is_dark() {
            hsla(217.0 / 360.0, 0.82, 0.76, 0.18)
        } else {
            hsla(217.0 / 360.0, 0.57, 0.44, 0.14)
        }
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
mod tests;
