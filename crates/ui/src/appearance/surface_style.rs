//! Client-local overall palettes and independent workbench color overrides.
//! Chat overrides remain in chat-appearance.json and are never rewritten here.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use gpui::{App, Global, Hsla};
use serde::{Deserialize, Serialize};

use crate::chat_style::{ColorPreset, color, normalize_hex};
use crate::theme::{Appearance, Theme};

pub const FILE_NAME: &str = "appearance-colors.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Terminal,
    Git,
    Sidebar,
}
impl Region {
    pub const ALL: [Self; 3] = [Self::Terminal, Self::Git, Self::Sidebar];
    pub fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Git => "Git / Diff",
            Self::Sidebar => "Sidebar",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Field {
    pub region: Region,
    pub key: &'static str,
    pub label: &'static str,
}
macro_rules! fields {
    ($($region:ident: $key:literal => $label:literal),* $(,)?) => {
        pub const FIELDS: &[Field] = &[$(Field { region: Region::$region, key: $key, label: $label }),*];
    };
}
fields! {
    Terminal: "terminalBackground" => "Background",
    Terminal: "terminalText" => "Default text",
    Terminal: "terminalCursor" => "Cursor",
    Terminal: "terminalSelection" => "Selection",
    Git: "gitBackground" => "Background",
    Git: "gitText" => "Default text",
    Git: "gitLineNumber" => "Line numbers",
    Git: "gitAddedBackground" => "Added line background",
    Git: "gitDeletedBackground" => "Deleted line background",
    Sidebar: "sidebarCard" => "Project card background",
    Sidebar: "sidebarText" => "Primary text",
    Sidebar: "sidebarSecondary" => "Secondary text",
    Sidebar: "sidebarSelected" => "Selected row background",
    Sidebar: "sidebarHover" => "Hovered row background",
    Terminal: "terminalAnsi0" => "ANSI 0 · Black",
    Terminal: "terminalAnsi1" => "ANSI 1 · Red",
    Terminal: "terminalAnsi2" => "ANSI 2 · Green",
    Terminal: "terminalAnsi3" => "ANSI 3 · Yellow",
    Terminal: "terminalAnsi4" => "ANSI 4 · Blue",
    Terminal: "terminalAnsi5" => "ANSI 5 · Magenta",
    Terminal: "terminalAnsi6" => "ANSI 6 · Cyan",
    Terminal: "terminalAnsi7" => "ANSI 7 · White",
    Terminal: "terminalAnsi8" => "ANSI 8 · Bright black",
    Terminal: "terminalAnsi9" => "ANSI 9 · Bright red",
    Terminal: "terminalAnsi10" => "ANSI 10 · Bright green",
    Terminal: "terminalAnsi11" => "ANSI 11 · Bright yellow",
    Terminal: "terminalAnsi12" => "ANSI 12 · Bright blue",
    Terminal: "terminalAnsi13" => "ANSI 13 · Bright magenta",
    Terminal: "terminalAnsi14" => "ANSI 14 · Bright cyan",
    Terminal: "terminalAnsi15" => "ANSI 15 · Bright white",
}
impl Field {
    pub fn ansi_index(self) -> Option<usize> {
        self.key.strip_prefix("terminalAnsi")?.parse().ok()
    }
    pub fn value(self, theme: &Theme) -> Hsla {
        use crate::theme::terminal;
        match self.key {
            "terminalBackground" => terminal::background(theme),
            "terminalText" | "gitText" | "sidebarText" => theme.text,
            "terminalCursor" => theme.cursor,
            "terminalSelection" => terminal::selection(theme),
            "gitBackground" => theme.regions.git_background.unwrap_or(theme.surface),
            "gitLineNumber" => theme
                .regions
                .git_line_number
                .unwrap_or(theme.text_faint.opacity(0.8)),
            "gitAddedBackground" => theme
                .regions
                .git_added
                .unwrap_or(theme.diff_add.opacity(0.055)),
            "gitDeletedBackground" => theme
                .regions
                .git_deleted
                .unwrap_or(theme.diff_del.opacity(0.055)),
            "sidebarCard" => theme.surface,
            "sidebarSecondary" => theme.text_muted,
            "sidebarSelected" => sidebar_selected(theme),
            "sidebarHover" => sidebar_hover(theme),
            _ => terminal::ansi(theme, self.ansi_index().expect("known color field") as u8),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Palette {
    pub preset: ColorPreset,
    pub overrides: BTreeMap<String, String>,
}
impl Palette {
    pub fn reset_region(&mut self, region: Region) {
        self.overrides
            .retain(|key, _| !FIELDS.iter().any(|f| f.region == region && f.key == key));
    }
    pub fn get(&self, key: &str) -> Option<Hsla> {
        color(&self.overrides.get(key).cloned())
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SurfaceAppearance {
    pub light: Palette,
    pub dark: Palette,
}
impl SurfaceAppearance {
    pub fn palette(&self, a: Appearance) -> &Palette {
        if a.is_dark() { &self.dark } else { &self.light }
    }
    pub fn palette_mut(&mut self, a: Appearance) -> &mut Palette {
        if a.is_dark() {
            &mut self.dark
        } else {
            &mut self.light
        }
    }
    pub fn sanitized(mut self) -> Self {
        for p in [&mut self.light, &mut self.dark] {
            p.overrides.retain(|key, value| {
                if FIELDS.iter().any(|f| f.key == key)
                    && let Some(hex) = normalize_hex(value)
                {
                    *value = hex;
                    true
                } else {
                    false
                }
            });
        }
        self
    }
    pub fn load(dir: &Path) -> Self {
        match std::fs::read(dir.join(FILE_NAME)) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(settings) => settings.sanitized(),
                Err(error) => {
                    tracing::warn!(%error, "invalid workbench colors; using defaults");
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

pub struct SurfaceAppearanceState {
    pub settings: SurfaceAppearance,
    pub revision: u64,
    dir: PathBuf,
}
impl Global for SurfaceAppearanceState {}
pub fn init(dir: PathBuf, cx: &mut App) {
    cx.set_global(SurfaceAppearanceState {
        settings: SurfaceAppearance::load(&dir),
        revision: 1,
        dir,
    });
    crate::appearance::install_theme(Theme::of(cx).appearance, cx);
}
pub fn settings(cx: &App) -> &SurfaceAppearance {
    static DEFAULT: LazyLock<SurfaceAppearance> = LazyLock::new(SurfaceAppearance::default);
    cx.try_global::<SurfaceAppearanceState>()
        .map(|s| &s.settings)
        .unwrap_or(&DEFAULT)
}
pub fn set(settings: SurfaceAppearance, cx: &mut App) -> std::io::Result<()> {
    let settings = settings.sanitized();
    let Some(state) = cx.try_global::<SurfaceAppearanceState>() else {
        return Err(std::io::Error::other(
            "Color preferences are not initialized.",
        ));
    };
    settings.save(&state.dir)?;
    if settings == state.settings {
        return Ok(());
    }
    let state = cx.global_mut::<SurfaceAppearanceState>();
    state.settings = settings;
    state.revision = state.revision.wrapping_add(1);
    crate::appearance::install_theme(Theme::of(cx).appearance, cx);
    cx.refresh_windows();
    Ok(())
}

fn base_text(theme: &mut Theme, text: Hsla) {
    theme.text = text;
    theme.syntax.variable = text;
    theme.syntax.parameter = text;
    theme.syntax.operator = text;
    theme.syntax.punctuation = text;
}

fn secondary_text(theme: &mut Theme, text: Hsla, background: Hsla) {
    theme.text_muted = background.blend(text.opacity(0.72));
    theme.text_dim = background.blend(text.opacity(0.65));
    theme.text_faint = background.blend(text.opacity(0.52));
}

/// Overall preset only. Explicit Chat/region overrides never enter this layer.
pub fn apply_preset(mut theme: Theme, preset: ColorPreset) -> Theme {
    let colors = preset.colors(theme.appearance);
    let Some(bg) = color(&colors.background) else {
        return theme;
    };
    let text = color(&colors.text).unwrap();
    let accent = color(&colors.accent).unwrap();
    let card = color(&colors.user_bubble).unwrap();
    base_text(&mut theme, text);
    theme.text_muted = bg.blend(text.opacity(0.72));
    theme.text_dim = theme.text_muted;
    theme.text_faint = bg.blend(text.opacity(0.52));
    theme.bg = bg;
    // Settings panel, sidebar project cards and Chat share one base.
    // The secondary tone is reserved for bubbles and raised controls.
    theme.surface = bg;
    theme.surface_card = bg;
    theme.surface_dialog = card;
    theme.surface_overlay = card;
    theme.surface_raised = card;
    theme.surface_raised_hover = card.blend(theme.ink(0.08));
    theme.input_bg = card;
    theme.accent = accent;
    theme.markdown_link = Some(accent);
    theme.user_bubble = Some(card);
    theme.inline_code_text = color(&colors.inline_code_text);
    theme.inline_code_background = color(&colors.inline_code_background);
    theme.element_hover = accent.opacity(0.10);
    theme.element_active = accent.opacity(0.18);
    theme.diff_hunk_bg = accent.opacity(0.08);
    theme.regions.chat_background = Some(bg);
    theme.regions.sidebar_selected = Some(bg.blend(accent.opacity(0.15)));
    theme.regions.sidebar_hover = Some(bg.blend(accent.opacity(0.08)));
    theme.regions.terminal_background = Some(bg);
    theme.regions.git_background = Some(bg);
    theme
}

pub fn resolve(palette: &Palette, base: &Theme, region: Region) -> Theme {
    let mut t = apply_preset(base.clone(), palette.preset);
    let c = |key: &str| palette.get(key);
    match region {
        Region::Terminal => {
            if let Some(v) = c("terminalBackground") {
                t.regions.terminal_background = Some(v);
            }
            if let Some(v) = c("terminalText") {
                t.text = v;
                let bg = crate::theme::terminal::background(&t);
                secondary_text(&mut t, v, bg);
            }
            if let Some(v) = c("terminalCursor") {
                t.cursor = v.opacity(0.55);
            }
            if let Some(v) = c("terminalSelection") {
                t.regions.terminal_selection = Some(v.opacity(0.25));
            }
            for i in 0..16 {
                t.regions.terminal_ansi[i] = c(&format!("terminalAnsi{i}"));
            }
        }
        Region::Git => {
            if let Some(v) = c("gitBackground") {
                t.regions.git_background = Some(v);
            }
            if let Some(v) = c("gitText") {
                base_text(&mut t, v);
                let bg = t.regions.git_background.unwrap_or(t.surface);
                secondary_text(&mut t, v, bg);
            }
            t.regions.git_line_number = c("gitLineNumber");
            t.regions.git_added = c("gitAddedBackground");
            t.regions.git_deleted = c("gitDeletedBackground");
        }
        Region::Sidebar => {
            if let Some(v) = c("sidebarCard") {
                t.surface = v;
            }
            if let Some(v) = c("sidebarText") {
                t.text = v;
            }
            if let Some(v) = c("sidebarSecondary") {
                t.text_muted = v;
                t.text_dim = v;
                t.text_faint = v;
            }
            if let Some(v) = c("sidebarSelected") {
                t.regions.sidebar_selected = Some(v);
            }
            if let Some(v) = c("sidebarHover") {
                t.regions.sidebar_hover = Some(v);
            }
        }
    }
    t
}
pub fn theme(region: Region, cx: &App) -> Theme {
    let base = Theme::of(cx);
    resolve(settings(cx).palette(base.appearance), base, region)
}
pub fn sidebar_selected(t: &Theme) -> Hsla {
    t.regions
        .sidebar_selected
        .unwrap_or_else(|| t.wash(if t.appearance.is_dark() { 0.11 } else { 0.06 }))
}
pub fn sidebar_hover(t: &Theme) -> Hsla {
    t.regions.sidebar_hover.unwrap_or_else(|| t.glass_hover())
}

pub fn contrast_warnings(region: Region, t: &Theme) -> Vec<String> {
    let mut pairs = Vec::new();
    match region {
        Region::Terminal => {
            let bg = crate::theme::terminal::background(t);
            pairs.push(("Default text".into(), t.text, bg));
            pairs.push((
                "Selected text".into(),
                t.text,
                bg.blend(crate::theme::terminal::selection(t)),
            ));
            for i in 0..16 {
                // Report only explicitly configured ANSI colors: black/dim
                // slots intentionally have low contrast in the stock palette.
                if let Some(c) = t.regions.terminal_ansi[i] {
                    pairs.push((format!("ANSI {i}"), c, bg));
                }
            }
        }
        Region::Git => {
            let bg = t.regions.git_background.unwrap_or(t.surface);
            pairs.push(("Default text".into(), t.text, bg));
            pairs.push((
                "Line numbers".into(),
                t.regions
                    .git_line_number
                    .unwrap_or(t.text_faint.opacity(0.8)),
                bg,
            ));
            pairs.push((
                "Added lines".into(),
                t.text,
                bg.blend(t.regions.git_added.unwrap_or(t.diff_add.opacity(0.055))),
            ));
            pairs.push((
                "Deleted lines".into(),
                t.text,
                bg.blend(t.regions.git_deleted.unwrap_or(t.diff_del.opacity(0.055))),
            ));
        }
        Region::Sidebar => {
            // Glass is desktop-dependent; use the fixed neutral backing for
            // the indicative sidebar-text contrast calculation.
            let bg = Theme::for_appearance(t.appearance).surface;
            pairs.push(("Primary text".into(), t.text, t.surface));
            pairs.push(("Secondary text".into(), t.text_muted, t.surface));
            pairs.push(("Sidebar text".into(), t.text, bg));
            pairs.push((
                "Selected row".into(),
                t.text,
                t.surface.blend(sidebar_selected(t)),
            ));
            pairs.push((
                "Hovered row".into(),
                t.text,
                t.surface.blend(sidebar_hover(t)),
            ));
        }
    }
    pairs
        .into_iter()
        .filter_map(|(label, fg, bg)| {
            (crate::theme::contrast_ratio(bg.blend(fg), bg) < 4.5).then_some(label)
        })
        .collect()
}

#[cfg(test)]
mod tests;
