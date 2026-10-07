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
fn tool_call_limit_defaults_to_five_and_stays_bounded() {
    let defaults = ChatAppearance::default();
    assert_eq!(defaults.tool_call_limit, 5);
    assert_eq!(defaults.clone().sanitized().tool_call_limit, 5);
    let unbounded = ChatAppearance {
        tool_call_limit: u32::MAX,
        ..Default::default()
    };
    assert_eq!(unbounded.sanitized().tool_call_limit, MAX_TOOL_CALL_LIMIT);
    // 0 is the deliberate "show all" setting, not an invalid value.
    let uncapped = ChatAppearance {
        tool_call_limit: 0,
        ..Default::default()
    };
    assert_eq!(uncapped.sanitized().tool_call_limit, 0);
}

#[test]
fn sparse_settings_and_missing_file_use_defaults() {
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(ChatAppearance::load(temp.path()), ChatAppearance::default());
    let sparse: ChatAppearance =
        serde_json::from_str(r##"{"wide":true,"dark":{"accent":"#aabbcc"}}"##).unwrap();
    assert!(sparse.wide);
    assert_eq!(sparse.font_size, 14.0);
    // A file written before this setting existed keeps the new default.
    assert_eq!(sparse.tool_call_limit, DEFAULT_TOOL_CALL_LIMIT);
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
        serde_json::from_str(r##"{"dark":{"text":"#F0F0F0","background":"#101010"}}"##).unwrap();
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
