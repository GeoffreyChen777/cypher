use super::*;

#[test]
fn round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let settings = UiSettings {
        sidebar_width: 300.0,
        sidebar_collapsed: true,
        last_space_id: Some("space-1".into()),
        sound_enabled: false,
        notifications_enabled: false,
        notifications_background_only: false,
        dock_badge_enabled: false,
        right_pane_width: 700.0,
        terminal_height: 320.0,
        keymap: KeymapConfig {
            toggle_sidebar: "mod-shift-s".into(),
            ..KeymapConfig::default()
        },
        appearance: AppearanceMode::Light,
        setup_completed: true,
        pi_runtime_setup_version: 1,
        shown_slash_commands: vec!["goal".into(), "skill:x".into()],
        offered_slash_commands: slash_commands::SHOWN_BY_DEFAULT
            .iter()
            .map(|name| name.to_string())
            .collect(),
        sidebar_sort: SidebarSort::Device,
        sidebar_device_filter: Some("dev-1".into()),
        sidebar_sort_reversed: true,
        workspace: Some(sample_workspace()),
        project_workspaces: HashMap::from([("space-1".to_string(), sample_workspace())]),
        session_docks: HashMap::from([(
            "a".to_string(),
            SessionDock {
                right: Some(0.4),
                terminal: None,
                right_open: true,
                terminal_open: false,
                used_at: 7,
            },
        )]),
    };
    settings.save(dir.path()).unwrap();
    assert_eq!(UiSettings::load(dir.path()), settings);
    let json = std::fs::read_to_string(UiSettings::path(dir.path())).unwrap();
    for key in [
        "\"workspace\"",
        "\"projectWorkspaces\"",
        "\"sessionDocks\"",
        "\"rightOpen\"",
    ] {
        assert!(json.contains(key), "{key} missing: {json}");
    }
}

fn sample_workspace() -> crate::workspace::Workspace {
    use crate::workspace::{Edge, TabKey, Workspace};
    let mut ws = Workspace::new();
    let first = ws.open(TabKey::session("a"));
    ws.open_split(TabKey::session("b"), first, Edge::Right);
    ws
}

/// Files from before the workspace layout load with no saved layout and
/// no per-session docks — and don't write the keys back until used.
/// Retired keys (`openTabs`, `spaceFilter`, `sidebarGrouped`, …) are
/// ignored rather than failing the parse.
#[test]
fn legacy_files_without_a_workspace_load() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 300, "openTabs": ["a"], "spaceFilter": "space-9", "sidebarGrouped": true, "tabOrder": {"s": ["a"]}, "spaceOrder": ["s"], "rightPaneOpen": true, "terminalOpen": true, "rightPaneWidth": 700, "terminalHeight": 300}"#,
        )
        .unwrap();
    let loaded = UiSettings::load(dir.path());
    assert_eq!(loaded.sidebar_width, 300.0);
    assert_eq!(loaded.workspace, None);
    assert!(loaded.project_workspaces.is_empty() && loaded.session_docks.is_empty());
    assert_eq!(loaded.right_pane_width, 700.0, "legacy sizes still seed");
    assert_eq!(loaded.terminal_height, 300.0);
    let json = serde_json::to_string(&loaded).unwrap();
    assert!(
        !json.contains("workspace") && !json.contains("sessionDocks"),
        "{json}"
    );
}

/// The old hidden-command list loads without error and leaves every
/// command but the defaults hidden; it is not written back.
#[test]
fn legacy_hidden_slash_commands_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        UiSettings::path(dir.path()),
        r#"{"sidebarWidth": 300, "hiddenSlashCommands": ["mcp", "skill:x"]}"#,
    )
    .unwrap();
    let loaded = UiSettings::load(dir.path());
    assert_eq!(loaded.sidebar_width, 300.0, "the rest of the file loads");
    assert_eq!(
        loaded.shown_slash_commands,
        slash_commands::SHOWN_BY_DEFAULT
    );
    let json = serde_json::to_string(&loaded).unwrap();
    assert!(!json.contains("hiddenSlashCommands"), "{json}");
}

/// A layout of the wrong shape (a newer build's tab kind after a
/// downgrade, a negative index) drops only itself: every other setting
/// survives the load.
#[test]
fn a_type_invalid_workspace_drops_only_itself() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
            UiSettings::path(dir.path()),
            r#"{
                "keymap": {"toggleSidebar": "mod-shift-s"},
                "appearance": "light",
                "workspace": {"root": {"group": 1}, "groups": {"1": {"tabs": [{"futureKind": 3}], "active": -1}}},
                "projectWorkspaces": {"p": {"root": {"group": 1}, "groups": {"1": {"active": -2}}}, "q": {}},
                "sessionDocks": {"a": {"right": "wide"}, "b": {"rightOpen": true, "usedAt": 5}}
            }"#,
        )
        .unwrap();
    let loaded = UiSettings::load(dir.path());
    assert_eq!(loaded.keymap.toggle_sidebar, "mod-shift-s");
    assert_eq!(loaded.appearance, AppearanceMode::Light);
    assert_eq!(loaded.workspace, None);
    assert_eq!(
        loaded.project_workspaces.keys().collect::<Vec<_>>(),
        vec!["q"]
    );
    assert_eq!(loaded.session_docks.keys().collect::<Vec<_>>(), vec!["b"]);
    assert!(loaded.session_docks["b"].right_open);
    // A map of the wrong type altogether is dropped the same way.
    let loaded: UiSettings =
        serde_json::from_str(r#"{"soundEnabled": false, "sessionDocks": [1, 2]}"#).unwrap();
    assert!(!loaded.sound_enabled);
    assert!(loaded.session_docks.is_empty());
}

#[test]
fn a_corrupt_saved_workspace_is_repaired_not_fatal() {
    let loaded: UiSettings = serde_json::from_str(
        r#"{"soundEnabled": false, "workspace": {"root": {"group": 9}, "groups": {}}}"#,
    )
    .unwrap();
    assert!(!loaded.sound_enabled);
    let ws = loaded.workspace.expect("repaired, not dropped");
    assert_eq!(ws.group_count(), 1);
    assert_eq!(ws.tabs().count(), 0);
}

#[test]
fn session_docks_heal_and_stay_bounded() {
    let mut settings = UiSettings::default();
    for i in 0..SESSION_DOCKS_CAP + 5 {
        settings.remember_session_dock(&format!("c{i}"), SessionDock::default(), i as i64);
    }
    assert_eq!(settings.session_docks.len(), SESSION_DOCKS_CAP);
    // The oldest went first; the newest is the default for new sessions.
    assert!(!settings.session_docks.contains_key("c0"));
    assert!(!settings.session_docks.contains_key("c4"));
    assert!(settings.session_docks.contains_key("c5"));
    let latest = settings.latest_session_dock().unwrap();
    assert_eq!(latest.used_at, (SESSION_DOCKS_CAP + 4) as i64);
    settings.prune_session_docks(|id| id == "c10");
    assert_eq!(settings.session_docks.len(), 1);

    let healed: UiSettings = serde_json::from_str(
        r#"{"sessionDocks": {"a": {"right": 3.0, "terminal": -1.0, "terminalOpen": true}}}"#,
    )
    .unwrap();
    let a = healed.clamped().session_docks["a"];
    assert_eq!(a.right, Some(DOCK_FRACTION_MAX));
    assert_eq!(a.terminal, Some(DOCK_FRACTION_MIN));
    assert!(a.terminal_open && !a.right_open);
    assert_eq!(clamp_fraction(f32::NAN), None);
}

#[test]
fn a_new_session_starts_from_the_latest_sizes_with_docks_closed() {
    let latest = SessionDock {
        right: Some(0.3),
        terminal: Some(0.25),
        right_open: true,
        terminal_open: true,
        used_at: 5,
    };
    let seeded = SessionDock::seeded_from(Some(&latest));
    assert_eq!((seeded.right, seeded.terminal), (Some(0.3), Some(0.25)));
    assert!(!seeded.right_open && !seeded.terminal_open);
    assert_eq!(SessionDock::seeded_from(None), SessionDock::default());
}

/// A settings file written before light mode existed has no `appearance`
/// key; it must load as "follow the OS" rather than failing the whole parse
/// and resetting every other preference to defaults.
#[test]
fn settings_without_appearance_default_to_system() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        UiSettings::path(dir.path()),
        r#"{"sidebarWidth": 300, "soundEnabled": false}"#,
    )
    .unwrap();
    let loaded = UiSettings::load(dir.path());
    assert_eq!(loaded.appearance, AppearanceMode::System);
    assert_eq!(loaded.sidebar_width, 300.0);
    assert!(!loaded.sound_enabled, "other keys still parse");
    assert!(
        loaded.notifications_enabled,
        "pre-banner files default banners on"
    );
    assert!(
        loaded.notifications_background_only,
        "pre-banner files default background-only on"
    );
    assert!(
        loaded.dock_badge_enabled,
        "pre-badge files default the Dock badge on"
    );
    assert!(
        !loaded.setup_completed,
        "pre-setup files still show first-run setup"
    );
    let pre_runtime: UiSettings = serde_json::from_str(r#"{"setupCompleted":true}"#).unwrap();
    assert_eq!(
        pre_runtime.pi_runtime_setup_version, 0,
        "users who completed the old system-Pi setup see the runtime prompt once"
    );
}

#[test]
fn missing_and_corrupt_files_yield_defaults() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(UiSettings::load(dir.path()), UiSettings::default());
    std::fs::write(UiSettings::path(dir.path()), "{not json").unwrap();
    assert_eq!(UiSettings::load(dir.path()), UiSettings::default());
}

#[test]
fn loaded_values_are_clamped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        UiSettings::path(dir.path()),
        r#"{"sidebarWidth": 10000, "rightPaneWidth": 1}"#,
    )
    .unwrap();
    let loaded = UiSettings::load(dir.path());
    assert_eq!(loaded.sidebar_width, SIDEBAR_MAX);
    assert_eq!(loaded.right_pane_width, RIGHT_PANE_MIN);
}

#[test]
fn nan_heals_to_default() {
    let healed = UiSettings {
        sidebar_width: f32::NAN,
        ..Default::default()
    }
    .clamped();
    assert_eq!(healed.sidebar_width, SIDEBAR_DEFAULT);
}

#[test]
fn defaults_match_zeron() {
    let d = UiSettings::default();
    assert_eq!(d.sidebar_width, 256.0);
    assert_eq!(d.right_pane_width, 520.0);
    assert_eq!(d.terminal_height, 280.0);
    assert!(!d.sidebar_collapsed);
}

#[test]
fn keymap_defaults_and_reset() {
    let mut keymap = KeymapConfig::default();
    assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-s");
    assert_eq!(keymap.get(ShortcutId::ToggleChanges), "mod-b");
    assert_eq!(keymap.get(ShortcutId::ToggleTerminal), "mod-j");
    let ctrl = if cfg!(target_os = "macos") {
        "ctrl"
    } else {
        "mod"
    };
    assert_eq!(keymap.get(ShortcutId::NextSession), format!("{ctrl}-tab"));
    assert_eq!(
        keymap.get(ShortcutId::PrevSession),
        format!("{ctrl}-shift-tab")
    );
    keymap.set(ShortcutId::ToggleSidebar, "mod-shift-x".into());
    assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-shift-x");
    keymap.reset(ShortcutId::ToggleSidebar);
    assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-s");
}

#[test]
fn combo_recording() {
    // How this platform spells a recorded ctrl (see `combo_from_keystroke_on`).
    let ctrl_combo = |suffix: &str| {
        if cfg!(target_os = "macos") {
            format!("ctrl-{suffix}")
        } else {
            format!("mod-{suffix}")
        }
    };
    assert_eq!(
        combo_from_keystroke(true, false, false, false, "s"),
        Some(ctrl_combo("s"))
    );
    assert_eq!(
        combo_from_keystroke(false, false, false, true, "s"),
        Some("mod-s".into())
    );
    assert_eq!(
        combo_from_keystroke(true, false, true, false, "tab"),
        Some(ctrl_combo("shift-tab"))
    );
    assert_eq!(
        combo_from_keystroke(true, true, true, false, "K"),
        Some(ctrl_combo("alt-shift-k"))
    );
    // Plain keys record without modifiers (Esc is filtered by the caller).
    assert_eq!(
        combo_from_keystroke(false, false, false, false, "f5"),
        Some("f5".into())
    );
    // Bare modifier presses record nothing.
    assert_eq!(
        combo_from_keystroke(true, false, false, false, "ctrl"),
        None
    );
    assert_eq!(
        combo_from_keystroke(false, false, true, false, "shift"),
        None
    );
    assert_eq!(combo_from_keystroke(false, false, false, false, ""), None);
}

#[test]
fn every_default_is_spelled_the_way_the_recorder_spells_it() {
    // The invariant `default_combo_on` documents. Checked for BOTH
    // platforms because the hazard only exists off macOS, so a single-OS
    // CI run would never see it.
    for mac in [true, false] {
        for id in ShortcutId::ALL {
            let combo = id.default_combo_on(mac);
            // Via the platform spelling, where modifier names are
            // unambiguous, so the decode can't inherit the bug it checks.
            let bound = platform_combo_on(mac, combo);
            let mut parts: Vec<&str> = bound.split('-').collect();
            let key = parts.pop().expect("a combo always ends in a key");
            let recorded = combo_from_keystroke_on(
                mac,
                parts.contains(&"ctrl"),
                parts.contains(&"alt"),
                parts.contains(&"shift"),
                parts.contains(&"cmd"),
                key,
            );
            assert_eq!(
                recorded.as_deref(),
                Some(combo),
                "{} default {combo:?} is unreachable from the recorder (mac={mac})",
                id.label()
            );
        }
    }
}

#[test]
fn defaults_are_distinct_physical_keys() {
    // Distinct STRINGS is not enough — two defaults could still resolve to
    // the same keystroke through `platform_combo`.
    let mut seen = std::collections::HashSet::new();
    for id in ShortcutId::ALL {
        let bound = platform_combo(id.default_combo());
        assert!(seen.insert(bound.clone()), "{bound:?} bound twice");
    }
}

#[test]
fn combo_translation() {
    let primary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    assert_eq!(platform_combo("mod-s"), format!("{primary}-s"));
    assert_eq!(platform_combo("alt-f4"), "alt-f4");
    let display_primary = if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    };
    assert_eq!(
        display_combo("mod-shift-s"),
        format!("{display_primary}+Shift+S")
    );
    assert_eq!(display_combo("f5"), "F5");
    // Literal ctrl passes through untouched — the macOS spelling of
    // session cycling.
    assert_eq!(platform_combo("ctrl-shift-tab"), "ctrl-shift-tab");
    assert_eq!(display_combo("ctrl-shift-tab"), "Ctrl+Shift+Tab");
}

#[test]
fn a_keymap_missing_newer_shortcuts_keeps_its_customizations() {
    // Upgrade path: a file from a build that predates session cycling
    // carries the user's rebinds and defaults only the new rows.
    let dir = tempfile::tempdir().unwrap();
    // Files written before the keymap existed load with defaults.
    std::fs::write(UiSettings::path(dir.path()), r#"{"sidebarWidth": 300}"#).unwrap();
    assert_eq!(UiSettings::load(dir.path()).keymap, KeymapConfig::default());
    std::fs::write(
        UiSettings::path(dir.path()),
        r#"{"keymap": {"toggleSidebar": "mod-shift-x"}}"#,
    )
    .unwrap();
    let keymap = UiSettings::load(dir.path()).keymap;
    assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-shift-x");
    assert_eq!(keymap.get(ShortcutId::ToggleTerminal), "mod-j");
    assert_eq!(
        keymap.get(ShortcutId::NextSession),
        ShortcutId::NextSession.default_combo()
    );
    // The workspace rows arrived later still: defaults, customizations
    // intact.
    assert_eq!(keymap.get(ShortcutId::SplitRight), "mod-\\");
    assert_eq!(keymap.get(ShortcutId::ToggleZoom), "mod-shift-enter");
}

#[test]
fn workspace_shortcuts_are_grouped_and_round_trip() {
    let workspace: Vec<ShortcutId> = ShortcutId::ALL
        .into_iter()
        .filter(|id| id.group() == ShortcutGroup::Workspace)
        .collect();
    assert_eq!(workspace.len(), 8);
    assert_eq!(ShortcutId::NewSession.group(), ShortcutGroup::General);
    let mut keymap = KeymapConfig::default();
    keymap.set(ShortcutId::FocusLeft, "mod-shift-left".into());
    let json = serde_json::to_string(&keymap).unwrap();
    assert!(json.contains(r#""focusLeft":"mod-shift-left""#), "{json}");
    let back: KeymapConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back, keymap);
}

#[test]
fn terminal_height_clamps_on_load() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(UiSettings::path(dir.path()), r#"{"terminalHeight": 5}"#).unwrap();
    assert_eq!(
        UiSettings::load(dir.path()).terminal_height,
        TERMINAL_MIN_HEIGHT
    );
    std::fs::write(UiSettings::path(dir.path()), r#"{"terminalHeight": 99999}"#).unwrap();
    assert_eq!(
        UiSettings::load(dir.path()).terminal_height,
        TERMINAL_ABS_MAX_HEIGHT
    );
}
