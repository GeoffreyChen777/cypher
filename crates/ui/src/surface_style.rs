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
        use crate::terminal::view;
        match self.key {
            "terminalBackground" => view::background(theme),
            "terminalText" | "gitText" | "sidebarText" => theme.text,
            "terminalCursor" => theme.cursor,
            "terminalSelection" => view::selection(theme),
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
            _ => view::resolve_color(
                crate::terminal::emulator::CellColor::Indexed(
                    self.ansi_index().expect("known color field") as u8,
                ),
                theme,
            ),
        }
    }
}

/// Runtime-only tokens. Region overrides are applied to cloned Themes, never
/// to the global theme or to another region's renderers.
#[derive(Clone, Debug, Default)]
pub struct RegionTokens {
    pub chat_background: Option<Hsla>,
    pub terminal_background: Option<Hsla>,
    pub terminal_selection: Option<Hsla>,
    pub terminal_ansi: [Option<Hsla>; 16],
    pub git_background: Option<Hsla>,
    pub git_line_number: Option<Hsla>,
    pub git_added: Option<Hsla>,
    pub git_deleted: Option<Hsla>,
    pub sidebar_selected: Option<Hsla>,
    pub sidebar_hover: Option<Hsla>,
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
        use std::io::Write;
        std::fs::create_dir_all(dir)?;
        let temp = dir.join(format!(".appearance-colors-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            file.write_all(
                &serde_json::to_vec_pretty(&self.clone().sanitized())
                    .map_err(std::io::Error::other)?,
            )?;
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
    Theme::install(Theme::of(cx).appearance, cx);
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
    Theme::install(Theme::of(cx).appearance, cx);
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
                let bg = crate::terminal::view::background(&t);
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
            let bg = crate::terminal::view::background(t);
            pairs.push(("Default text".into(), t.text, bg));
            pairs.push((
                "Selected text".into(),
                t.text,
                bg.blend(crate::terminal::view::selection(t)),
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
mod tests {
    use super::*;
    use crate::terminal::{emulator::CellColor, view};

    #[test]
    fn default_content_backgrounds_match_for_every_color_theme() {
        let chat = crate::chat_style::ChatAppearance::default();
        for appearance in [Appearance::Light, Appearance::Dark] {
            for preset in ColorPreset::ALL {
                let p = Palette {
                    preset,
                    ..Default::default()
                };
                let global = apply_preset(Theme::for_appearance(appearance), preset);
                let sidebar = resolve(&p, &global, Region::Sidebar);
                let chat_bg = crate::chat_style::panel_background(&chat, &global, true);
                assert_eq!(
                    global.surface, sidebar.surface,
                    "settings / sidebar: {preset:?}"
                );
                assert_eq!(global.surface, chat_bg, "settings / Chat: {preset:?}");
            }
        }
    }

    #[test]
    fn old_preset_names_load_without_discarding_overrides() {
        for (old, new) in [
            ("ocean", ColorPreset::Catppuccin),
            ("forest", ColorPreset::Nord),
            ("warm", ColorPreset::Gruvbox),
        ] {
            let p: Palette = serde_json::from_value(serde_json::json!({
                "preset": old, "overrides": {"sidebarCard": "#123456"}
            }))
            .unwrap();
            assert_eq!(p.preset, new);
            assert_eq!(p.overrides["sidebarCard"], "#123456");
            assert_ne!(serde_json::to_value(p).unwrap()["preset"], old);
        }
    }

    #[test]
    fn every_preset_keeps_the_original_window_and_sidebar_material() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            let base = Theme::for_appearance(appearance);
            #[cfg(target_os = "macos")]
            assert!(base.glass().a > 0.0 && base.glass().a < 1.0);
            for preset in ColorPreset::ALL {
                let palette = Palette {
                    preset,
                    ..Default::default()
                };
                let overall = apply_preset(base.clone(), preset);
                assert_eq!(overall.glass(), base.glass(), "{appearance:?} {preset:?}");
                assert_eq!(overall.is_glass(), base.is_glass());
                for region in Region::ALL {
                    let mut theme = resolve(&palette, &base, region);
                    assert_eq!(theme.glass(), base.glass());
                    theme.surface = gpui::rgb(0xff00ff).into();
                    assert_eq!(
                        theme.glass(),
                        base.glass(),
                        "card colors must not affect frost"
                    );
                }
                if preset != ColorPreset::Default {
                    assert_ne!(
                        overall.surface, base.surface,
                        "content cards still follow the preset"
                    );
                }
            }
        }
    }

    #[test]
    fn legacy_sidebar_backing_overrides_are_ignored_without_losing_card_colors() {
        let mut settings = SurfaceAppearance::default();
        settings.dark.preset = ColorPreset::Catppuccin;
        settings
            .dark
            .overrides
            .insert("sidebarBackground".into(), "#FF0000".into());
        settings
            .dark
            .overrides
            .insert("sidebarCard".into(), "#123456".into());
        settings
            .dark
            .overrides
            .insert("terminalText".into(), "#ABCDEF".into());
        let base = Theme::dark();
        let raw = resolve(&settings.dark, &base, Region::Sidebar);
        assert_eq!(raw.glass(), base.glass());
        let sanitized = settings.sanitized();
        assert!(!sanitized.dark.overrides.contains_key("sidebarBackground"));
        assert_eq!(sanitized.dark.overrides["sidebarCard"], "#123456");
        assert_eq!(sanitized.dark.overrides["terminalText"], "#ABCDEF");
        assert_eq!(
            resolve(&sanitized.dark, &base, Region::Sidebar).surface,
            gpui::rgb(0x123456).into()
        );
        assert!(!FIELDS.iter().any(|field| field.key == "sidebarBackground"));
    }

    #[test]
    fn default_regions_preserve_the_existing_theme_and_geometry() {
        for a in [Appearance::Dark, Appearance::Light] {
            let base = Theme::for_appearance(a);
            for region in Region::ALL {
                let t = resolve(&Palette::default(), &base, region);
                assert_eq!(t.bg, base.bg);
                assert_eq!(t.surface, base.surface);
                assert_eq!(t.text, base.text);
                assert_eq!(t.cursor, base.cursor);
                assert_eq!(t.font_mono, base.font_mono);
                assert_eq!(t.markdown, base.markdown);
                assert_eq!(t.regions.git_added, None);
                assert_eq!(view::background(&t), view::terminal_bg_for(a));
                assert_eq!(view::selection(&t), view::terminal_selection_for(a));
                for i in 0..=255 {
                    assert_eq!(
                        view::resolve_color(CellColor::Indexed(i), &t),
                        view::resolve_color(CellColor::Indexed(i), &base)
                    );
                }
            }
        }
    }

    #[test]
    fn field_keys_are_unique_and_ansi_slots_are_complete() {
        let keys: std::collections::BTreeSet<_> = FIELDS.iter().map(|f| f.key).collect();
        assert_eq!(keys.len(), FIELDS.len());
        assert_eq!(FIELDS.len(), 30);
        assert_eq!(
            FIELDS
                .iter()
                .filter_map(|f| f.ansi_index())
                .collect::<Vec<_>>(),
            (0..16).collect::<Vec<_>>()
        );
        for f in FIELDS {
            assert!(!f.label.is_empty());
        }
    }

    #[test]
    fn palettes_round_trip_privately_without_touching_chat_or_ui_settings() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["chat-appearance.json", "ui-settings.json"] {
            std::fs::write(dir.path().join(name), b"untouched existing preferences").unwrap();
        }
        let mut settings = SurfaceAppearance::default();
        settings.dark.preset = ColorPreset::Catppuccin;
        settings
            .dark
            .overrides
            .insert("terminalText".into(), "#aabbcc".into());
        settings
            .light
            .overrides
            .insert("gitBackground".into(), "#ffffff".into());
        settings.save(dir.path()).unwrap();
        let restored = SurfaceAppearance::load(dir.path());
        assert_eq!(restored, settings.sanitized());
        assert_eq!(restored.dark.overrides["terminalText"], "#AABBCC");
        assert!(!restored.light.overrides.contains_key("terminalText"));
        for name in ["chat-appearance.json", "ui-settings.json"] {
            assert_eq!(
                std::fs::read(dir.path().join(name)).unwrap(),
                b"untouched existing preferences"
            );
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join(FILE_NAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn missing_old_and_invalid_preferences_fall_back_without_losing_valid_keys() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            SurfaceAppearance::load(dir.path()),
            SurfaceAppearance::default()
        );
        std::fs::write(dir.path().join(FILE_NAME), b"{broken").unwrap();
        assert_eq!(
            SurfaceAppearance::load(dir.path()),
            SurfaceAppearance::default()
        );
        let parsed: SurfaceAppearance = serde_json::from_str(
            r##"{"dark":{"overrides":{"terminalText":"#123","sidebarCard":"abcdef","futureKey":"#112233","terminalAnsi16":"#112233"}}}"##
        ).unwrap();
        let parsed = parsed.sanitized();
        assert_eq!(parsed.dark.overrides.len(), 1);
        assert_eq!(parsed.dark.overrides["sidebarCard"], "#ABCDEF");
        assert_eq!(parsed.dark.preset, ColorPreset::Default);
    }

    #[test]
    fn preset_changes_and_region_resets_preserve_other_overrides() {
        let mut settings = SurfaceAppearance::default();
        for f in FIELDS {
            settings
                .dark
                .overrides
                .insert(f.key.into(), "#112233".into());
            settings
                .light
                .overrides
                .insert(f.key.into(), "#AABBCC".into());
        }
        let light = settings.light.clone();
        let original = settings.dark.overrides.clone();
        settings.dark.preset = ColorPreset::Nord;
        assert_eq!(settings.dark.overrides, original);
        for region in Region::ALL {
            let mut p = settings.dark.clone();
            p.reset_region(region);
            assert_eq!(p.preset, ColorPreset::Nord);
            for f in FIELDS {
                assert_eq!(p.overrides.contains_key(f.key), f.region != region);
            }
        }
        assert_eq!(settings.light, light);
    }

    #[test]
    fn overrides_are_region_scoped_and_clearing_restores_inheritance() {
        let mut p = Palette {
            preset: ColorPreset::Gruvbox,
            ..Default::default()
        };
        for (key, value) in [
            ("terminalText", "#AABBCC"),
            ("terminalBackground", "#010203"),
            ("gitText", "#DDEEFF"),
            ("gitBackground", "#040506"),
            ("sidebarText", "#CCDDEE"),
            ("sidebarCard", "#070809"),
        ] {
            p.overrides.insert(key.into(), value.into());
        }
        let base = Theme::dark();
        let terminal = resolve(&p, &base, Region::Terminal);
        let git = resolve(&p, &base, Region::Git);
        let sidebar = resolve(&p, &base, Region::Sidebar);
        assert_eq!(terminal.text, p.get("terminalText").unwrap());
        assert_eq!(git.text, p.get("gitText").unwrap());
        assert_eq!(sidebar.text, p.get("sidebarText").unwrap());
        assert_eq!(sidebar.surface, p.get("sidebarCard").unwrap());
        assert_ne!(git.regions.terminal_background, p.get("terminalBackground"));
        assert_ne!(terminal.regions.git_background, p.get("gitBackground"));
        assert_eq!(base.regions.terminal_background, None);
        p.overrides.remove("terminalText");
        assert_eq!(
            resolve(&p, &base, Region::Terminal).text,
            apply_preset(base, p.preset).text
        );
    }

    #[test]
    fn ansi_overrides_do_not_rewrite_truecolor_or_extended_indices() {
        let mut p = Palette::default();
        p.overrides
            .insert("terminalBackground".into(), "#102030".into());
        p.overrides.insert("terminalText".into(), "#FFEEDD".into());
        p.overrides
            .insert("terminalCursor".into(), "#ABCDEF".into());
        p.overrides
            .insert("terminalSelection".into(), "#778899".into());
        for i in 0..16 {
            p.overrides
                .insert(format!("terminalAnsi{i}"), "#123456".into());
        }
        for a in [Appearance::Light, Appearance::Dark] {
            let base = Theme::for_appearance(a);
            let t = resolve(&p, &base, Region::Terminal);
            assert_eq!(
                view::resolve_color(CellColor::Background, &t),
                p.get("terminalBackground").unwrap()
            );
            assert_eq!(
                view::resolve_color(CellColor::Foreground, &t),
                p.get("terminalText").unwrap()
            );
            for i in 0..16 {
                assert_eq!(
                    view::resolve_color(CellColor::Indexed(i), &t),
                    p.get("terminalAnsi0").unwrap()
                );
            }
            for i in 16..=255 {
                assert_eq!(
                    view::resolve_color(CellColor::Indexed(i), &t),
                    view::resolve_color(CellColor::Indexed(i), &base)
                );
            }
            assert_eq!(
                view::resolve_color(CellColor::Rgb(1, 2, 3), &t),
                view::resolve_color(CellColor::Rgb(1, 2, 3), &base)
            );
            assert_eq!(t.cursor, p.get("terminalCursor").unwrap().opacity(0.55));
            assert_eq!(
                view::selection(&t),
                p.get("terminalSelection").unwrap().opacity(0.25)
            );
        }
    }

    #[test]
    fn git_custom_base_text_preserves_semantic_highlights() {
        use cypher_syntax::HighlightKind;
        let base = Theme::dark();
        let mut p = Palette::default();
        p.overrides.insert("gitText".into(), "#ABCDEF".into());
        p.overrides
            .insert("gitAddedBackground".into(), "#123456".into());
        p.overrides
            .insert("gitDeletedBackground".into(), "#654321".into());
        let t = resolve(&p, &base, Region::Git);
        for kind in [
            HighlightKind::Keyword,
            HighlightKind::String,
            HighlightKind::Comment,
            HighlightKind::Number,
            HighlightKind::Function,
        ] {
            assert_eq!(t.syntax.color(kind), base.syntax.color(kind));
        }
        assert_eq!(
            t.syntax.color(HighlightKind::Variable),
            p.get("gitText").unwrap()
        );
        assert_eq!(t.regions.git_added, p.get("gitAddedBackground"));
        assert_eq!(t.regions.git_deleted, p.get("gitDeletedBackground"));
        assert_eq!(t.diff_add, base.diff_add);
        assert_eq!(t.diff_del, base.diff_del);
    }

    #[test]
    fn overall_theme_preserves_existing_chat_colors_and_invalidates_cached_runs() {
        let mut chat = crate::chat_style::ChatAppearance::default();
        chat.dark.text = Some("#FEEDAA".into());
        chat.dark.background = Some("#112244".into());
        chat.dark.inline_code_text = Some("#FFCCBB".into());
        for preset in ColorPreset::ALL {
            let mut base = apply_preset(Theme::dark(), preset);
            base.text_style_revision = 20;
            let t = crate::chat_style::resolve(&chat, &base, 7, &[]);
            assert_eq!(t.text, color(&chat.dark.text).unwrap());
            assert_eq!(t.bg, color(&chat.dark.background).unwrap());
            assert_eq!(t.inline_code_text, color(&chat.dark.inline_code_text));
            assert_eq!(t.text_style_revision, 27);
        }
    }

    #[test]
    fn low_contrast_is_warned_not_silently_recolored() {
        for (region, fg, bg) in [
            (Region::Terminal, "terminalText", "terminalBackground"),
            (Region::Git, "gitText", "gitBackground"),
            (Region::Sidebar, "sidebarText", "sidebarCard"),
        ] {
            let mut p = Palette::default();
            p.overrides.insert(fg.into(), "#FFFFFF".into());
            p.overrides.insert(bg.into(), "#FFFFFF".into());
            let t = resolve(&p, &Theme::dark(), region);
            assert_eq!(t.text, gpui::rgb(0xffffff).into());
            assert!(!contrast_warnings(region, &t).is_empty());
        }
    }

    #[test]
    fn custom_region_text_also_keeps_its_header_readable() {
        for (region, base, fg_key, bg_key, fg, bg) in [
            (
                Region::Terminal,
                Theme::light(),
                "terminalText",
                "terminalBackground",
                "#F0F0F0",
                "#111111",
            ),
            (
                Region::Git,
                Theme::dark(),
                "gitText",
                "gitBackground",
                "#161616",
                "#FAFAFA",
            ),
        ] {
            let mut p = Palette::default();
            p.overrides.insert(fg_key.into(), fg.into());
            p.overrides.insert(bg_key.into(), bg.into());
            let t = resolve(&p, &base, region);
            let background = p.get(bg_key).unwrap();
            assert!(crate::theme::contrast_ratio(t.text_muted, background) >= 4.5);
            assert_eq!(t.text, p.get(fg_key).unwrap());
        }
    }

    #[test]
    fn failed_atomic_write_keeps_the_destination_and_cleans_staging() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join(FILE_NAME);
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("keep"), b"original").unwrap();
        let mut settings = SurfaceAppearance::default();
        settings.dark.preset = ColorPreset::Catppuccin;
        assert!(settings.save(dir.path()).is_err());
        assert_eq!(
            std::fs::read(destination.join("keep")).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
