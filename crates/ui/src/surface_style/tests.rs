use super::*;
use crate::terminal::{emulator::CellColor, view};
use crate::theme::terminal;

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
            assert_eq!(terminal::background(&t), terminal::terminal_bg_for(a));
            assert_eq!(terminal::selection(&t), terminal::terminal_selection_for(a));
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
            terminal::selection(&t),
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
