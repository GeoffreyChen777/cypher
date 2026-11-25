use super::*;

#[gpui::test]
fn unix_engine_directory_does_not_redirect_client_preferences(cx: &mut gpui::TestAppContext) {
    let ui = tempfile::tempdir().unwrap();
    let engine = tempfile::tempdir().unwrap();
    let ui_settings = UiSettings {
        sidebar_width: 333.,
        ..Default::default()
    };
    let engine_settings = UiSettings {
        sidebar_width: 222.,
        ..Default::default()
    };
    ui_settings.save(ui.path()).unwrap();
    engine_settings.save(engine.path()).unwrap();
    let engine_bytes = std::fs::read(UiSettings::path(engine.path())).unwrap();
    cx.update(|cx| {
        gpui_tokio::init(cx);
        cx.set_global(Theme::for_appearance(crate::kit::theme::Appearance::Dark));
        crate::widgets::text_input::init(cx);
        crate::terminal::panel::init(cx);
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| state.data_dir = Some(ui.path().into()));
        let boot = EngineBootConfig {
            data_dir: engine.path().into(),
            ipc_socket: cypher_env::ipc_socket(engine.path()).unwrap(),
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: None,
            default_harness: cypher_proto::HarnessId::Mock,
        };
        let shell = cx.new(|cx| Shell::new(state.clone(), boot, ui.path().into(), cx));
        assert_eq!(shell.read(cx).data_dir, ui.path());
        assert_eq!(shell.read(cx).boot.data_dir, engine.path());
        assert_eq!(shell.read(cx).settings.sidebar_width, 333.);
        assert_eq!(state.read(cx).data_dir.as_deref(), Some(ui.path()));
        shell
            .read(cx)
            .settings
            .save(&shell.read(cx).data_dir)
            .unwrap();
    });
    assert_eq!(
        std::fs::read(UiSettings::path(engine.path())).unwrap(),
        engine_bytes
    );
}

#[test]
fn every_default_shortcut_binds_on_this_platform() {
    // `apply_keymap` silently falls back on an unparseable combo, so a
    // default gpui cannot parse would ship as a dead shortcut.
    for id in crate::prefs::ShortcutId::ALL {
        let combo = platform_combo(id.default_combo());
        assert!(
            Keystroke::parse(&combo).is_ok(),
            "{} default {combo:?} does not parse",
            id.label()
        );
    }
}

#[gpui::test]
fn the_files_editor_saves_on_mod_s_despite_the_sidebar_shortcut(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        apply_keymap(cx, &crate::prefs::KeymapConfig::default());
        let keystroke = Keystroke::parse(&platform_combo("mod-s")).unwrap();
        let resolve = |contexts: &[gpui::KeyContext]| {
            cx.key_bindings()
                .borrow()
                .bindings_for_input(std::slice::from_ref(&keystroke), contexts)
                .0
                .first()
                .map(|binding| binding.action().name())
        };
        let editor = [
            gpui::KeyContext::parse("Workspace").unwrap(),
            gpui::KeyContext::parse(crate::files::editor::KEY_CONTEXT).unwrap(),
        ];
        assert_eq!(
            resolve(&editor),
            Some(gpui::Action::name(&crate::files::editor::Save))
        );
        // Elsewhere the default still toggles the sidebar.
        assert_eq!(
            resolve(&[gpui::KeyContext::parse("Workspace").unwrap()]),
            Some(gpui::Action::name(&ToggleSidebar))
        );
    });
}

#[gpui::test]
fn shift_arrows_switch_workspaces_unless_the_focus_owns_them(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        apply_keymap(cx, &crate::prefs::KeymapConfig::default());
        let resolve = |keys: &str, contexts: &[&str]| -> Vec<&'static str> {
            let keystroke = Keystroke::parse(keys).unwrap();
            let contexts: Vec<gpui::KeyContext> = contexts
                .iter()
                .map(|c| gpui::KeyContext::parse(c).unwrap())
                .collect();
            cx.key_bindings()
                .borrow()
                .bindings_for_input(std::slice::from_ref(&keystroke), &contexts)
                .0
                .iter()
                .map(|binding| binding.action().name())
                .collect()
        };
        let prev = gpui::Action::name(&PrevWorkspace);
        let next = gpui::Action::name(&NextWorkspace);
        let select_up = gpui::Action::name(&crate::widgets::text_input::SelectUp);
        let select_down = gpui::Action::name(&crate::widgets::text_input::SelectDown);
        // Focus on the root, the sidebar's or the board's chrome.
        assert_eq!(resolve("shift-up", &[SHELL_KEY_CONTEXT]), [prev]);
        assert_eq!(resolve("shift-down", &[SHELL_KEY_CONTEXT]), [next]);
        // A composer selects first; only if it passes (empty) does the
        // switch run.
        assert_eq!(
            resolve("shift-up", &[SHELL_KEY_CONTEXT, "Composer"]),
            [select_up, prev]
        );
        assert_eq!(
            resolve("shift-down", &[SHELL_KEY_CONTEXT, "Composer"]),
            [select_down, next]
        );
        // The Files editor's own selection binding comes first too.
        let editor = resolve(
            "shift-up",
            &[SHELL_KEY_CONTEXT, crate::files::editor::KEY_CONTEXT],
        );
        assert_eq!(
            editor.first().copied(),
            Some(gpui::Action::name(&crate::files::editor::SelectUp))
        );
        // The terminal and palette fields keep the keys.
        assert!(resolve("shift-up", &[SHELL_KEY_CONTEXT, "Terminal"]).is_empty());
        assert!(resolve("shift-down", &[SHELL_KEY_CONTEXT, "PaletteSearch"]).is_empty());
    });
}

/// A window whose root stands in for the shell's (its key context and the
/// two workspace actions) around one chat composer input.
struct ComposerHost {
    input: Entity<crate::widgets::text_input::TextInput>,
    switched: Vec<bool>,
}

impl Render for ComposerHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(SHELL_KEY_CONTEXT)
            .w(px(400.0))
            .on_action(cx.listener(|this, _: &NextWorkspace, _, _| this.switched.push(true)))
            .on_action(cx.listener(|this, _: &PrevWorkspace, _, _| this.switched.push(false)))
            .child(self.input.clone())
    }
}

#[gpui::test]
fn an_empty_composer_passes_shift_arrows_to_the_workspace_switch(cx: &mut gpui::TestAppContext) {
    use crate::widgets::text_input::TextInput;
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        apply_keymap(cx, &crate::prefs::KeymapConfig::default());
    });
    let window = cx.open_window(gpui::size(px(500.0), px(200.0)), |_, cx| ComposerHost {
        input: cx.new(|cx| {
            let mut input = TextInput::new("", cx);
            input.use_chat_style = true;
            input
        }),
        switched: Vec::new(),
    });
    let input = window.update(cx, |host, _, _| host.input.clone()).unwrap();
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
        let focus = input.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    });
    let switched = |visual: &mut gpui::VisualTestContext| {
        window
            .update(visual, |host, _, _| host.switched.clone())
            .unwrap()
    };

    visual.simulate_keystrokes("shift-down");
    visual.simulate_keystrokes("shift-up");
    assert_eq!(switched(&mut visual), [true, false], "empty: both switch");

    // With a draft the keys select it, and nothing switches.
    input.update(&mut visual, |input, cx| input.set_text("first\nsecond", cx));
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    let caret = input.read_with(&visual, |input, _| input.selected_range.clone());
    assert!(caret.is_empty(), "the caret, nothing selected yet");
    visual.simulate_keystrokes("shift-up");
    let selected = input.read_with(&visual, |input, _| input.selected_range.clone());
    assert!(!selected.is_empty(), "Shift+↑ selected the draft");
    visual.simulate_keystrokes("shift-down");
    assert_eq!(
        switched(&mut visual),
        [true, false],
        "a draft keeps the keys"
    );
}

#[gpui::test]
fn a_settings_field_keeps_shift_arrows_even_when_empty(cx: &mut gpui::TestAppContext) {
    use crate::widgets::text_input::TextInput;
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        apply_keymap(cx, &crate::prefs::KeymapConfig::default());
    });
    let window = cx.open_window(gpui::size(px(500.0), px(200.0)), |_, cx| ComposerHost {
        input: cx.new(|cx| TextInput::new("", cx)),
        switched: Vec::new(),
    });
    let input = window.update(cx, |host, _, _| host.input.clone()).unwrap();
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
        let focus = input.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    });
    visual.simulate_keystrokes("shift-up shift-down");
    let switched = window
        .update(&mut visual, |host, _, _| host.switched.clone())
        .unwrap();
    assert!(switched.is_empty(), "not the chat composer");
}

fn test_chat(id: &str, archived: bool) -> cypher_proto::Chat {
    cypher_proto::Chat {
        id: id.into(),
        title: Some(format!("Chat {id}")),
        archived,
        created_at: Utc::now(),
        ..crate::test_fixtures::chat()
    }
}

fn test_shell(cx: &mut gpui::TestAppContext) -> (Entity<Shell>, Entity<AppState>) {
    test_shell_in(tempfile::tempdir().unwrap().keep(), cx)
}

fn test_shell_in(ui: PathBuf, cx: &mut gpui::TestAppContext) -> (Entity<Shell>, Entity<AppState>) {
    cx.update(|cx| {
        gpui_tokio::init(cx);
        cx.set_global(Theme::for_appearance(crate::kit::theme::Appearance::Dark));
        crate::widgets::text_input::init(cx);
        crate::terminal::panel::init(cx);
        let state = cx.new(|_| AppState::new());
        let boot = EngineBootConfig {
            data_dir: ui.clone(),
            ipc_socket: cypher_env::ipc_socket(&ui).unwrap(),
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: None,
            default_harness: cypher_proto::HarnessId::Mock,
        };
        let shell = cx.new(|cx| Shell::new(state.clone(), boot, ui.clone(), cx));
        (shell, state)
    })
}

fn set_chats(
    state: &Entity<AppState>,
    chats: Vec<cypher_proto::Chat>,
    cx: &mut gpui::TestAppContext,
) {
    state.update(cx, |s, cx| {
        s.apply_chats(chats);
        cx.notify();
    });
    cx.run_until_parked();
}

fn tab_shape(shell: &Entity<Shell>, cx: &mut gpui::TestAppContext) -> Vec<Vec<String>> {
    shell.read_with(cx, |shell, _| {
        shell
            .workspace
            .groups_in_reading_order()
            .into_iter()
            .map(|id| {
                shell
                    .workspace
                    .group(id)
                    .unwrap()
                    .tabs()
                    .iter()
                    .map(|tab| match tab {
                        crate::workspace::TabKey::Session(id) => id.clone(),
                        crate::workspace::TabKey::NewSession(_) => "+".into(),
                    })
                    .collect()
            })
            .collect()
    })
}

#[gpui::test]
fn boot_lands_the_latest_chat_as_a_tab_and_main_follows_focus(cx: &mut gpui::TestAppContext) {
    let (shell, state) = test_shell(cx);
    assert!(!state.read_with(cx, |s, _| s.transcript_watches()));
    set_chats(
        &state,
        vec![test_chat("a", false), test_chat("b", false)],
        cx,
    );
    let landed = tab_shape(&shell, cx);
    assert_eq!(landed.len(), 1);
    assert_eq!(landed[0].len(), 1);
    let first = landed[0][0].clone();
    assert_eq!(
        state.read_with(cx, |s, _| s.selected_chat.clone()),
        Some(first.clone())
    );
    // Sidebar click: opens in the focused tile; ⌘-click splits right.
    let other = if first == "a" { "b" } else { "a" };
    shell.update(cx, |shell, cx| shell.open_chat(other.into(), cx));
    assert_eq!(
        tab_shape(&shell, cx),
        vec![vec![first.clone(), other.into()]]
    );
    assert_eq!(
        state
            .read_with(cx, |s, _| s.selected_chat.clone())
            .as_deref(),
        Some(other)
    );
    shell.update(cx, |shell, cx| {
        shell.open_chat_with(first.clone(), true, cx)
    });
    // Already open: focused where it is, never duplicated.
    assert_eq!(
        tab_shape(&shell, cx),
        vec![vec![first.clone(), other.into()]]
    );
    assert_eq!(
        state.read_with(cx, |s, _| s.selected_chat.clone()),
        Some(first.clone())
    );
    shell.update(cx, |shell, cx| shell.close_focused_tab(cx));
    assert_eq!(tab_shape(&shell, cx), vec![vec![other.to_string()]]);
    shell.update(cx, |shell, cx| {
        shell.open_chat_with(first.clone(), true, cx)
    });
    assert_eq!(
        tab_shape(&shell, cx),
        vec![vec![other.to_string()], vec![first.clone()]]
    );
    // One slot per tab, each pinned to its own session.
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.tiles.slots.len(), 2);
        for slot in shell.tiles.slots.values() {
            assert_eq!(
                slot.state.read(cx).selected_chat.as_deref(),
                slot.tab.chat_id()
            );
        }
    });
}

#[gpui::test]
fn a_canvas_becomes_its_session_and_vanished_or_archived_chats_close(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, state) = test_shell(cx);
    set_chats(&state, vec![test_chat("a", false)], cx);
    shell.update(cx, |shell, cx| shell.open_new_session(cx));
    assert_eq!(
        tab_shape(&shell, cx),
        vec![vec!["a".to_string(), "+".into()]]
    );
    let canvas = shell.read_with(cx, |shell, _| shell.focused_slot().unwrap());
    // The first send selects the minted chat on the canvas's context.
    let ctx = shell.read_with(cx, |shell, _| shell.tiles.slots[&canvas].state.clone());
    ctx.update(cx, |s, cx| s.select_chat(Some("new".into()), cx));
    cx.run_until_parked();
    assert_eq!(
        tab_shape(&shell, cx),
        vec![vec!["a".to_string(), "new".into()]]
    );
    // Same slot, re-keyed in place.
    shell.read_with(cx, |shell, _| {
        assert_eq!(shell.focused_slot(), Some(canvas));
    });
    assert_eq!(
        state
            .read_with(cx, |s, _| s.selected_chat.clone())
            .as_deref(),
        Some("new")
    );
    // A tile's context moving to ANOTHER chat (a subagent child) goes
    // back to its own; the other opens as a tab.
    set_chats(
        &state,
        vec![
            test_chat("a", false),
            test_chat("new", false),
            test_chat("c", false),
        ],
        cx,
    );
    ctx.update(cx, |s, cx| s.select_chat(Some("c".into()), cx));
    cx.run_until_parked();
    assert_eq!(
        tab_shape(&shell, cx),
        vec![vec!["a".to_string(), "new".into(), "c".into()]]
    );
    assert_eq!(
        ctx.read_with(cx, |s, _| s.selected_chat.clone()).as_deref(),
        Some("new")
    );
    // Deleted elsewhere / archived: the tabs close.
    set_chats(
        &state,
        vec![test_chat("a", false), test_chat("c", true)],
        cx,
    );
    assert_eq!(tab_shape(&shell, cx), vec![vec!["a".to_string()]]);
    shell.read_with(cx, |shell, _| assert_eq!(shell.tiles.slots.len(), 1));
}

#[gpui::test]
fn background_tabs_of_deleted_chats_close_on_the_chats_frame(cx: &mut gpui::TestAppContext) {
    use crate::workspace::TabKey;
    let (shell, state) = test_shell(cx);
    // The boot landing may open either chat first: compare sorted.
    let tabs = |cx: &mut gpui::TestAppContext| {
        let mut shape = tab_shape(&shell, cx);
        shape.iter_mut().for_each(|group| group.sort());
        shape
    };
    set_chats(
        &state,
        vec![test_chat("a", false), test_chat("b", false)],
        cx,
    );
    shell.update(cx, |shell, cx| {
        shell.open_chat("a".into(), cx);
        shell.open_chat("b".into(), cx);
        shell.open_chat("a".into(), cx);
        // A fork this window just created, opened in the background.
        shell.expect_chat("fork");
        let group = shell.workspace.focused();
        shell.workspace.open_in(group, TabKey::session("fork"));
        shell.open_chat("a".into(), cx);
    });
    shell.read_with(cx, |shell, _| {
        assert!(shell.slot_for_tab(&TabKey::session("fork")).is_none());
    });
    // A notify that isn't a chats frame judges nothing missing.
    state.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    assert_eq!(tabs(cx), vec![vec!["a", "b", "fork"]]);
    // "b" was deleted elsewhere: its background tab (with its slot)
    // closes; the fork's row just hasn't landed yet.
    set_chats(&state, vec![test_chat("a", false)], cx);
    assert_eq!(tabs(cx), vec![vec!["a", "fork"]]);
    // Listed once: no longer expected, so a later frame without it
    // closes it.
    set_chats(
        &state,
        vec![test_chat("a", false), test_chat("fork", false)],
        cx,
    );
    shell.read_with(cx, |shell, _| assert!(!shell.chat_expected("fork")));
    set_chats(&state, vec![test_chat("a", false)], cx);
    assert_eq!(tabs(cx), vec![vec!["a"]]);
}

#[gpui::test]
fn boot_restores_the_saved_layout_and_docks(cx: &mut gpui::TestAppContext) {
    use crate::workspace::{Edge, TabKey, Workspace};
    let ui = tempfile::tempdir().unwrap().keep();
    let mut saved = Workspace::new();
    let left = saved.open(TabKey::session("a"));
    saved.open(TabKey::session("x")); // deleted since
    let canvas = saved.new_session_tab();
    saved.open(canvas);
    saved.open_split(TabKey::session("b"), left, Edge::Right);
    saved.open(TabKey::session("c"));
    saved.activate(saved.focused(), 0); // "b" active and focused
    UiSettings {
        workspace: Some(saved),
        session_docks: std::collections::HashMap::from([(
            "b".to_string(),
            crate::prefs::SessionDock {
                right: Some(0.5),
                right_open: true,
                used_at: 1,
                ..Default::default()
            },
        )]),
        ..Default::default()
    }
    .save(&ui)
    .unwrap();
    let (shell, state) = test_shell_in(ui.clone(), cx);
    set_chats(
        &state,
        vec![
            test_chat("a", false),
            test_chat("b", false),
            test_chat("c", false),
            test_chat("latest", false),
        ],
        cx,
    );
    assert_eq!(
        tab_shape(&shell, cx),
        vec![vec!["a".to_string()], vec!["b".into(), "c".into()]],
        "the saved layout wins over the boot landing"
    );
    assert_eq!(
        state
            .read_with(cx, |s, _| s.selected_chat.clone())
            .as_deref(),
        Some("b"),
        "main follows the restored focus"
    );
    shell.read_with(cx, |shell, _| {
        // Slots only for what's on screen; "c" gets one when shown.
        let mut open: Vec<_> = shell
            .tiles
            .slots
            .values()
            .map(|slot| slot.tab.clone())
            .collect();
        open.sort_by_key(|tab| tab.chat_id().map(str::to_string));
        assert_eq!(open, vec![TabKey::session("a"), TabKey::session("b")]);
        let b = shell.focused_slot().unwrap();
        let slot = &shell.tiles.slots[&b];
        assert!(slot.dock.open, "b's dock was open");
        assert_eq!(slot.right_fraction, Some(0.5));
        // A session without an entry starts from the latest sizes, closed.
        let a = shell.slot_for_tab(&TabKey::session("a")).unwrap();
        assert!(!shell.tiles.slots[&a].dock.open);
        assert_eq!(shell.tiles.slots[&a].right_fraction, Some(0.5));
    });
    // Showing "c" creates its slot; the change persists.
    shell.update(cx, |shell, cx| shell.open_chat("c".into(), cx));
    let c = shell.read_with(cx, |shell, _| shell.focused_slot().unwrap());
    shell.update(cx, |shell, cx| shell.toggle_dock(c, cx));
    cx.executor()
        .advance_clock(Duration::from_millis(SAVE_DEBOUNCE_MS + 50));
    cx.run_until_parked();
    let written = UiSettings::load(&ui);
    let ws = written.workspace.expect("layout saved");
    assert_eq!(ws.focused_tab(), Some(&TabKey::session("c")));
    assert_eq!(ws.tabs().count(), 3);
    assert!(written.session_docks["c"].right_open);
}

/// Closing a session's last right-dock tab shuts the dock; closing one of
/// several leaves it open.
#[gpui::test]
fn closing_the_last_dock_tab_shuts_the_dock(cx: &mut gpui::TestAppContext) {
    use crate::markdown::artifact::ArtifactOpen;
    use crate::markdown::parser::ArtifactKind;
    use dock::DockSurface;
    let (shell, state) = test_shell(cx);
    set_chats(&state, vec![test_chat("a", false)], cx);
    shell.update(cx, |shell, cx| shell.open_chat("a".into(), cx));
    let sid = shell.read_with(cx, |shell, _| shell.focused_slot().unwrap());
    let page = |title: &str| ArtifactOpen {
        key: format!("row-{title}").into(),
        kind: ArtifactKind::Svg,
        title: title.to_string().into(),
        named: true,
        code: "<svg xmlns=\"http://www.w3.org/2000/svg\"/>".into(),
    };
    let open =
        |cx: &mut gpui::TestAppContext| shell.read_with(cx, |s, _| s.tiles.slots[&sid].dock.open);
    shell.update(cx, |shell, cx| {
        shell.open_artifact(sid, page("one"), cx);
        shell.open_artifact(sid, page("two"), cx);
    });
    let tabs = shell.read_with(cx, |s, _| s.tiles.slots[&sid].dock.surfaces.clone());
    assert_eq!(tabs.len(), 2);
    shell.update(cx, |shell, cx| shell.close_dock_surface(sid, tabs[0], cx));
    assert!(open(cx), "one tab is left");
    shell.update(cx, |shell, cx| shell.close_dock_surface(sid, tabs[1], cx));
    assert!(!open(cx), "the last tab closed shuts the dock");
    shell.read_with(cx, |s, _| {
        let slot = &s.tiles.slots[&sid];
        assert!(slot.dock.surfaces.is_empty());
        assert_eq!(slot.dock.active, DockSurface::Picker);
    });
    // Reopening lands on the picker.
    shell.update(cx, |shell, cx| shell.toggle_dock(sid, cx));
    assert!(open(cx));
}

/// A ⌘N before the first chats frame joins the restored layout instead
/// of replacing it, and the next save keeps the saved tabs.
#[gpui::test]
fn a_presync_canvas_joins_the_restored_layout(cx: &mut gpui::TestAppContext) {
    use crate::workspace::{Edge, TabKey, Workspace};
    let ui = tempfile::tempdir().unwrap().keep();
    let mut saved = Workspace::new();
    let left = saved.open(TabKey::session("a"));
    saved.open_split(TabKey::session("b"), left, Edge::Right);
    UiSettings {
        workspace: Some(saved),
        ..Default::default()
    }
    .save(&ui)
    .unwrap();
    let (shell, state) = test_shell_in(ui.clone(), cx);
    shell.update(cx, |shell, cx| shell.open_new_session(cx));
    let canvas = shell.read_with(cx, |shell, _| shell.focused_slot().unwrap());
    set_chats(
        &state,
        vec![test_chat("a", false), test_chat("b", false)],
        cx,
    );
    assert_eq!(
        tab_shape(&shell, cx),
        vec![vec!["a".to_string()], vec!["b".into(), "+".into()]]
    );
    shell.read_with(cx, |shell, _| {
        // The same canvas slot, re-keyed into the restored layout.
        assert_eq!(shell.focused_slot(), Some(canvas));
        assert!(matches!(
            shell.tiles.slots[&canvas].tab,
            TabKey::NewSession(_)
        ));
    });
    shell.update(cx, |shell, cx| shell.save_layout(cx));
    cx.executor()
        .advance_clock(Duration::from_millis(SAVE_DEBOUNCE_MS + 50));
    cx.run_until_parked();
    let ws = UiSettings::load(&ui).workspace.expect("layout saved");
    assert!(ws.contains(&TabKey::session("a")) && ws.contains(&TabKey::session("b")));
}

/// Zoomed: only the zoomed tile gets a slot; the hidden ones get theirs
/// when shown.
#[gpui::test]
fn hidden_tiles_start_no_context(cx: &mut gpui::TestAppContext) {
    use crate::workspace::{Edge, TabKey, Workspace};
    let ui = tempfile::tempdir().unwrap().keep();
    let mut saved = Workspace::new();
    let left = saved.open(TabKey::session("a"));
    let right = saved
        .open_split(TabKey::session("b"), left, Edge::Right)
        .unwrap();
    saved.toggle_zoom(right);
    UiSettings {
        workspace: Some(saved),
        ..Default::default()
    }
    .save(&ui)
    .unwrap();
    let (shell, state) = test_shell_in(ui, cx);
    set_chats(
        &state,
        vec![test_chat("a", false), test_chat("b", false)],
        cx,
    );
    shell.read_with(cx, |shell, _| {
        assert_eq!(shell.tiles.slots.len(), 1);
        assert!(shell.slot_for_tab(&TabKey::session("b")).is_some());
    });
    shell.update(cx, |shell, cx| shell.toggle_zoom_focused(cx));
    shell.read_with(cx, |shell, _| assert_eq!(shell.tiles.slots.len(), 2));
}

/// Any workspace change that takes a session off screen (here a sidebar
/// pick into its tile) closes the shared comment popup; one that hides
/// nothing leaves it.
#[gpui::test]
fn a_session_leaving_the_screen_drops_the_comment_popup(cx: &mut gpui::TestAppContext) {
    let (shell, state) = test_shell(cx);
    set_chats(
        &state,
        vec![test_chat("a", false), test_chat("b", false)],
        cx,
    );
    shell.update(cx, |shell, cx| {
        shell.open_chat("a".into(), cx);
        shell.split_focused(crate::workspace::Edge::Right, cx);
    });
    let popup = shell.read_with(cx, |shell, _| shell.comments.popup.clone());
    let offer = |cx: &mut gpui::TestAppContext| {
        popup.update(cx, |popup, cx| {
            popup.offer(
                "a".into(),
                "quote".into(),
                None,
                gpui::point(px(10.0), px(10.0)),
                crate::comment_popup::CommentOwner::next_terminal(),
                None,
                std::rc::Rc::new(|_| {}),
                None,
                cx,
            )
        });
    };
    // Focusing the other (empty) tile hides nothing.
    offer(cx);
    let groups = shell.read_with(cx, |shell, _| shell.workspace.groups_in_reading_order());
    shell.update(cx, |shell, cx| shell.focus_group(groups[0], cx));
    assert!(popup.read_with(cx, |popup, _| popup.is_active()));
    // A sidebar pick replaces "a" in its tile: "a" leaves the screen.
    shell.update(cx, |shell, cx| shell.open_chat("b".into(), cx));
    let mut first = tab_shape(&shell, cx)[0].clone();
    first.sort();
    assert_eq!(first, vec!["a".to_string(), "b".into()]);
    assert!(!popup.read_with(cx, |popup, _| popup.is_active()));
}

/// A slow (remote) canvas send outliving the pending-send overlay: while
/// its composer is still sending, a chats frame without the minted row
/// neither closes the tab nor leaves its context deselected.
#[gpui::test]
fn a_tab_stays_while_its_first_send_is_in_flight(cx: &mut gpui::TestAppContext) {
    let (shell, state) = test_shell(cx);
    set_chats(&state, vec![test_chat("a", false)], cx);
    shell.update(cx, |shell, cx| shell.open_new_session(cx));
    let sid = shell.read_with(cx, |shell, _| shell.focused_slot().unwrap());
    let (ctx, composer) = shell.read_with(cx, |shell, _| {
        let slot = &shell.tiles.slots[&sid];
        (slot.state.clone(), slot.composer.clone())
    });
    composer.update(cx, |composer, _| composer.set_sending_for_test(true));
    ctx.update(cx, |s, cx| s.select_chat(Some("new".into()), cx));
    cx.run_until_parked();
    assert_eq!(
        tab_shape(&shell, cx),
        vec![vec!["a".to_string(), "new".into()]]
    );
    // No pending-send overlay (expired): only the composer holds it.
    set_chats(&state, vec![test_chat("a", false)], cx);
    assert_eq!(
        tab_shape(&shell, cx),
        vec![vec!["a".to_string(), "new".into()]]
    );
    assert_eq!(
        ctx.read_with(cx, |s, _| s.selected_chat.clone()).as_deref(),
        Some("new")
    );
    // The send settled and the row never came: the next frame closes it.
    composer.update(cx, |composer, _| composer.set_sending_for_test(false));
    set_chats(&state, vec![test_chat("a", false)], cx);
    assert_eq!(tab_shape(&shell, cx), vec![vec!["a".to_string()]]);
}

/// Render smoke test: the Ready page with a split workspace, docks and
/// a terminal paints without panicking, and each tile measures its own
/// session area.
#[gpui::test]
fn a_split_workspace_renders(cx: &mut gpui::TestAppContext) {
    let ui = tempfile::tempdir().unwrap().keep();
    cx.update(|cx| {
        gpui_tokio::init(cx);
        cx.set_global(Theme::for_appearance(crate::kit::theme::Appearance::Dark));
        crate::widgets::text_input::init(cx);
        crate::terminal::panel::init(cx);
    });
    let state = cx.new(|_| AppState::new());
    let boot = EngineBootConfig {
        data_dir: ui.clone(),
        ipc_socket: cypher_env::ipc_socket(&ui).unwrap(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: cypher_proto::HarnessId::Mock,
    };
    let (shell, vcx) = cx.add_window_view({
        let state = state.clone();
        move |_, cx| Shell::new(state, boot, ui, cx)
    });
    state.update(vcx, |s, cx| {
        s.connection = ConnectionStatus::Ready;
        s.workspace_scope = Some(WorkspaceScope::Local);
        s.auth = Some(AuthState::SignedOut);
        s.apply_chats(vec![test_chat("a", false), test_chat("b", false)]);
        cx.notify();
    });
    vcx.run_until_parked();
    shell.update(vcx, |shell, cx| {
        shell.pages.setup_dismissed = true;
        // Boot landed one of them; the other splits off to the right.
        shell.open_chat_with("a".into(), true, cx);
        shell.open_chat_with("b".into(), true, cx);
        let sid = shell.focused_slot().unwrap();
        shell.toggle_dock(sid, cx);
        shell.split_focused(crate::workspace::Edge::Bottom, cx);
    });
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
    let focused = shell.read_with(vcx, |shell, _| shell.focused_slot());
    assert_eq!(focused, None, "the new split is empty and focused");
    vcx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_new_session(cx);
            let sid = shell.focused_slot().unwrap();
            shell.toggle_terminal(sid, window, cx);
            shell
                .workspace
                .apply_preset(crate::workspace::Preset::Grid2x2);
            shell.workspace_changed(cx);
        });
        window.refresh();
    });
    vcx.run_until_parked();
    shell.read_with(vcx, |shell, _| {
        assert_eq!(shell.tiles.slots.len(), 3);
        let widths: Vec<f32> = shell
            .tiles
            .slots
            .values()
            .map(|slot| f32::from(slot.area.get().size.width))
            .collect();
        assert!(
            widths.iter().all(|w| *w > 0.0 && *w < 1000.0),
            "every tile measured its own area: {widths:?}"
        );
    });
}

#[test]
fn workspace_shortcuts_parse() {
    let defaults = crate::prefs::ShortcutId::ALL
        .into_iter()
        .map(|id| id.default_combo());
    for combo in defaults.chain(FOCUS_TILE_KEYS) {
        let keystroke = Keystroke::parse(&platform_combo(combo));
        assert!(keystroke.is_ok(), "{combo:?} does not parse");
    }
    assert_eq!(
        Keystroke::parse(&platform_combo("mod-\\")).unwrap().key,
        "\\"
    );
    // One tile action per key, in order.
    assert_eq!(focus_tile_action(0).name(), gpui::Action::name(&FocusTile1));
    assert_eq!(focus_tile_action(8).name(), gpui::Action::name(&FocusTile9));
}

#[test]
fn safe_avatar_url_guards_https_and_bounds() {
    let ok = SharedString::from("https://avatars.example.com/a.png");
    assert_eq!(safe_avatar_url(Some(ok.clone())), Some(ok));
    // Non-HTTPS, hostless, malformed, and oversized values never reach
    // gpui's `img` (never a local-file interpretation either).
    for bad in [
        "http://avatars.example.com/a.png",
        "file:///etc/passwd",
        "not a url",
        "https://",
        "https:///a.png",
        // Malformed ports / hosts and embedded credentials: real URL
        // parsing rejects what a prefix check would accept.
        "https://host:99999/a.png",
        "https://host:abc/a.png",
        "https://user@host/a.png",
        "https://user:pass@host/a.png",
        &format!("https://host/{}", "a".repeat(2048)),
    ] {
        assert_eq!(
            safe_avatar_url(Some(SharedString::from(bad))),
            None,
            "{bad}"
        );
    }
    assert_eq!(safe_avatar_url(None), None);
    // Exactly 2048 chars is allowed.
    let at_cap = format!("https://h/{}", "a".repeat(2048 - "https://h/".len()));
    assert!(safe_avatar_url(Some(SharedString::from(at_cap))).is_some());
}

#[test]
fn side_chat_start_error_text_is_actionable_for_unknown_method() {
    use cypher_rpc::RpcError;
    // Old engine on the hosting device: no `StartSideChat` method →
    // the message says what's wrong and what to do, not just an error.
    let unknown = Shell::side_chat_start_error_text(&RpcError::UnknownMethod(
        methods::START_SIDE_CHAT.to_string(),
    ));
    assert!(unknown.contains("newer Cypher engine"), "{unknown}");
    assert!(unknown.contains("device hosting this session"), "{unknown}");
    assert!(unknown.contains("Update that device"), "{unknown}");
    // A DIFFERENT unknown method is not the Side Chat feature gap.
    let other = Shell::side_chat_start_error_text(&RpcError::UnknownMethod("Nope".into()));
    assert_eq!(other, "Could not open Side Chat: unknown method: Nope");
    // Engine-side failures keep the generic prefix + raw error.
    assert_eq!(
        Shell::side_chat_start_error_text(&RpcError::Failed("engine exploded".into())),
        "Could not open Side Chat: engine exploded"
    );
    for generic in [
        RpcError::BadParams("bad".into()),
        RpcError::Transport("down".into()),
        RpcError::Closed,
    ] {
        assert!(
            Shell::side_chat_start_error_text(&generic).starts_with("Could not open Side Chat:"),
            "{generic}"
        );
    }
}

/// Session Fork v1: an old hosting engine answers UnknownMethod for
/// `ForkSession` — the notice must say what's wrong and how to fix it.
#[test]
fn fork_session_error_text_is_actionable_for_unknown_method() {
    use cypher_rpc::RpcError;
    let unknown =
        Shell::fork_session_error_text(&RpcError::UnknownMethod(methods::FORK_SESSION.to_string()));
    assert!(unknown.contains("newer Cypher engine"), "{unknown}");
    assert!(unknown.contains("device hosting this session"), "{unknown}");
    assert!(unknown.contains("Update that device"), "{unknown}");
    // A DIFFERENT unknown method is not the Session Fork feature gap.
    assert_eq!(
        Shell::fork_session_error_text(&RpcError::UnknownMethod("Nope".into())),
        "Could not fork: unknown method: Nope"
    );
    // Engine-side failures keep the generic prefix + raw error.
    assert_eq!(
        Shell::fork_session_error_text(&RpcError::Failed("engine exploded".into())),
        "Could not fork: engine exploded"
    );
}

/// Session Fork idempotence: the `(sourceChatId, anchorMessageId) →
/// requestId` mapping survives RPC errors / lost replies (so a retry
/// reuses the SAME target id) and is dropped on a definitive reply
/// (Created or typed Unavailable).
#[test]
fn fork_request_id_is_retained_after_errors_but_not_definitive_replies() {
    use cypher_rpc::RpcError;
    // Lost replies / transport errors: retain — the retry reuses the id.
    assert!(Shell::fork_request_id_retained(&Err(RpcError::Transport(
        "connection lost".into()
    ))));
    assert!(Shell::fork_request_id_retained(&Err(RpcError::Closed)));
    assert!(Shell::fork_request_id_retained(&Err(RpcError::Failed(
        "engine exploded".into()
    ))));
    assert!(Shell::fork_request_id_retained(&Err(
        RpcError::UnknownMethod(methods::FORK_SESSION.to_string())
    )));
    // A definitive reply (Created / typed Unavailable) drops the mapping.
    let chat = cypher_proto::Chat {
        id: "fork-x".into(),
        title: Some("Fork".into()),
        created_at: chrono::Utc::now(),
        room_gen: Some(2),
        ..crate::test_fixtures::chat()
    };
    assert!(!Shell::fork_request_id_retained(&Ok(
        cypher_proto::SessionForkResponse::Created(cypher_proto::SessionForkCreated {
            chat,
            mode: cypher_proto::SessionForkMode::EditUser,
            composer_text: None,
        })
    )));
    assert!(!Shell::fork_request_id_retained(&Ok(
        cypher_proto::SessionForkResponse::Unavailable(cypher_proto::SessionForkUnavailable {
            reason: cypher_proto::SessionForkUnavailableReason::LiveSession,
            message: "still running".into(),
        })
    )));
}

#[tokio::test]
async fn remote_shutdown_waits_for_ipc_release() {
    let dir = tempfile::tempdir().unwrap();
    let port = cypher_env::ipc_socket(dir.path()).unwrap();
    let listener = cypher_rpc::LocalListener::bind(&port).await.unwrap();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(listener);
    });

    wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_secs(2))
        .await
        .unwrap();
    release.await.unwrap();
}

#[test]
fn about_check_uses_app_version_and_last_check() {
    use cypher_update::UpdateStatus;
    let status = |latest: Option<&str>, checked: bool, error: Option<&str>| UpdateStatus {
        current_version: "0.1.0".into(),
        latest_version: latest.map(str::to_string),
        update_available: latest.is_some(),
        checked_at: checked.then_some(1),
        error: error.map(str::to_string),
        relaunch_pending: false,
    };
    assert_eq!(about_check_from_status(None, "0.1.0"), AboutCheck::Idle);
    assert_eq!(
        about_check_from_status(Some(&status(None, false, None)), "0.1.0"),
        AboutCheck::Idle
    );
    assert_eq!(
        about_check_from_status(Some(&status(Some("0.1.0"), true, None)), "0.1.0"),
        AboutCheck::Current
    );
    assert_eq!(
        about_check_from_status(Some(&status(Some("0.1.1"), true, None)), "0.1.0"),
        AboutCheck::Available {
            latest: "0.1.1".into()
        }
    );
    assert!(matches!(
        about_check_from_status(Some(&status(None, false, Some("offline"))), "0.1.0"),
        AboutCheck::Failed { .. }
    ));
    assert_eq!(
        install_kind_label(&cypher_update::InstallKind::Unmanaged),
        "Development build"
    );
}

#[test]
fn update_strip_presentation() {
    use cypher_update::UpdateStatus;
    // `engine_current` = the attached daemon's compiled version (which can
    // differ from the UI process's), `app` = the version running in THIS
    // process.
    let status = |engine_current: &str, latest: Option<&str>, available: bool| UpdateStatus {
        current_version: engine_current.into(),
        latest_version: latest.map(str::to_string),
        update_available: available,
        checked_at: None,
        error: None,
        relaunch_pending: false,
    };
    // App-version source of truth: both table cases are decided against the
    // UI process's version, never the engine's `update_available` boolean.
    let mac_visible = |engine_current: &str, available: bool, latest: &str, app: &str| {
        update_strip_view(
            Some(&status(engine_current, Some(latest), available)),
            app,
            None,
            true,
            &UpdateFlow::Idle,
        )
        .is_some()
    };

    // Table: app 0.1.0 + engine 0.1.1/latest 0.1.1 → SHOW. The engine is
    // up to date (its boolean is false) but this UI process is older.
    assert!(mac_visible("0.1.1", false, "0.1.1", "0.1.0"));
    // Table: app 0.1.1 + old engine boolean true/latest 0.1.1 → HIDE. The
    // engine daemon is merely older; this UI process is already at latest.
    assert!(!mac_visible("0.1.0", true, "0.1.1", "0.1.1"));
    // Same version from either side never shows.
    assert!(!mac_visible("0.1.1", true, "0.1.1", "0.1.1"));
    assert!(!mac_visible("0.1.0", false, "0.1.1", "0.1.1"));
    // A genuinely newer release than this process still shows, whatever the
    // engine says.
    assert!(mac_visible("0.1.1", false, "0.1.2", "0.1.1"));

    // Hidden: no status, no latest, or the dismissed version.
    assert!(update_strip_view(None, "0.1.0", None, true, &UpdateFlow::Idle).is_none());
    assert!(
        update_strip_view(
            Some(&status("0.1.0", None, false)),
            "0.1.0",
            None,
            true,
            &UpdateFlow::Idle
        )
        .is_none()
    );
    assert!(
        update_strip_view(
            Some(&status("0.1.0", Some("0.1.1"), true)),
            "0.1.0",
            Some("0.1.1"),
            true,
            &UpdateFlow::Idle
        )
        .is_none()
    );
    // A newer release than the dismissed one shows again.
    assert!(
        update_strip_view(
            Some(&status("0.1.0", Some("0.1.2"), true)),
            "0.1.0",
            Some("0.1.1"),
            true,
            &UpdateFlow::Idle
        )
        .is_some()
    );

    // Advisory (non-mac) install also follows this process's version, not
    // the attached engine's boolean.
    let view = update_strip_view(
        Some(&status("0.1.1", Some("0.1.1"), false)),
        "0.1.0",
        None,
        false,
        &UpdateFlow::Idle,
    )
    .unwrap();
    assert_eq!(
        view.label,
        "Update available — v0.1.1 · run `cypher update`"
    );
    assert!(view.clickable && !view.failed);
    assert!(
        update_strip_view(
            Some(&status("0.1.0", Some("0.1.1"), true)),
            "0.1.1",
            None,
            false,
            &UpdateFlow::Idle,
        )
        .is_none()
    );

    // Mac bundle flow: Idle / Downloading / Ready / Failed labels + flags.
    let view = update_strip_view(
        Some(&status("0.1.0", Some("0.1.1"), true)),
        "0.1.0",
        None,
        true,
        &UpdateFlow::Idle,
    )
    .unwrap();
    assert_eq!(view.label, "Update available — v0.1.1");
    assert!(view.clickable && !view.failed);

    let view = update_strip_view(
        Some(&status("0.1.0", Some("0.1.1"), true)),
        "0.1.0",
        None,
        true,
        &UpdateFlow::Downloading,
    )
    .unwrap();
    assert_eq!(view.label, "Downloading v0.1.1…");
    assert!(!view.clickable && !view.failed);

    let view = update_strip_view(
        Some(&status("0.1.0", Some("0.1.1"), true)),
        "0.1.0",
        None,
        true,
        &UpdateFlow::Ready(PathBuf::from("/tmp/staged")),
    )
    .unwrap();
    assert_eq!(view.label, "Update ready — restart to apply");
    assert!(view.clickable && !view.failed);

    let view = update_strip_view(
        Some(&status("0.1.0", Some("0.1.1"), true)),
        "0.1.0",
        None,
        true,
        &UpdateFlow::Failed("checksum mismatch".into()),
    )
    .unwrap();
    assert_eq!(view.label, "Update failed: checksum mismatch");
    assert!(view.clickable && view.failed);
}

#[test]
fn pi_update_strip_presentation() {
    use cypher_engine::pi::packages::{PiPackageUpdate, PiUpdateStatus};

    let status = |pi: bool, packages: usize, applying: bool, error: Option<&str>| PiUpdateStatus {
        pi_installed: true,
        current_pi_version: Some("0.84.0".into()),
        latest_pi_version: Some("0.85.0".into()),
        pi_update_available: pi,
        package_updates: (0..packages)
            .map(|index| PiPackageUpdate {
                name: format!("plugin-{index}"),
                current_version: "1.0.0".into(),
                latest_version: "1.1.0".into(),
            })
            .collect(),
        applying,
        checked_at: None,
        error: error.map(str::to_string),
    };

    assert!(pi_update_strip_view(None, false).is_none());
    assert!(pi_update_strip_view(Some(&status(false, 0, false, None)), false).is_none());

    let pi_only = pi_update_strip_view(Some(&status(true, 0, false, None)), false).unwrap();
    assert_eq!(pi_only.label, "Pi update available — v0.85.0");
    assert!(pi_only.clickable && !pi_only.failed);

    let combined = pi_update_strip_view(Some(&status(true, 2, false, None)), false).unwrap();
    assert_eq!(combined.label, "Pi and 2 plugin updates available");

    let busy = pi_update_strip_view(Some(&status(false, 1, true, None)), false).unwrap();
    assert_eq!(busy.label, "Updating Pi and plugins…");
    assert!(!busy.clickable);

    let failed =
        pi_update_strip_view(Some(&status(false, 1, false, Some("network"))), false).unwrap();
    assert!(failed.failed && failed.clickable);
}

#[test]
fn about_runtime_line_reports_the_manual_runtime_check() {
    use cypher_engine::pi::packages::{PiPackageUpdate, PiUpdateStatus};

    let status = |pi: bool, packages: usize, applying: bool, error: Option<&str>| PiUpdateStatus {
        pi_installed: true,
        current_pi_version: Some("0.85.1".into()),
        latest_pi_version: Some("0.85.2".into()),
        pi_update_available: pi,
        package_updates: (0..packages)
            .map(|index| PiPackageUpdate {
                name: format!("plugin-{index}"),
                current_version: "1.0.0".into(),
                latest_version: "1.1.0".into(),
            })
            .collect(),
        applying,
        checked_at: None,
        error: error.map(str::to_string),
    };

    // No Runtime facts yet, or no Runtime at all: the dialog stays quiet.
    assert!(about_runtime_line(None, true).is_none());
    let missing = PiUpdateStatus {
        pi_installed: false,
        ..status(false, 0, false, None)
    };
    assert!(about_runtime_line(Some(&missing), false).is_none());

    assert_eq!(
        about_runtime_line(Some(&status(false, 0, false, None)), true).unwrap(),
        "Checking the Pi Runtime…"
    );
    assert_eq!(
        about_runtime_line(Some(&status(false, 0, false, None)), false).unwrap(),
        "Pi Runtime is up to date."
    );
    // An install found by the check outranks the checking label: it is the
    // newer, more specific truth about the same sweep.
    assert_eq!(
        about_runtime_line(Some(&status(false, 1, true, None)), true).unwrap(),
        "Updating the Pi Runtime…"
    );
    assert_eq!(
        about_runtime_line(Some(&status(false, 1, false, None)), false).unwrap(),
        "Pi Runtime update available — 1 plugin."
    );
    assert_eq!(
        about_runtime_line(Some(&status(false, 3, false, None)), false).unwrap(),
        "Pi Runtime update available — 3 plugins."
    );
    assert_eq!(
        about_runtime_line(Some(&status(true, 0, false, None)), false).unwrap(),
        "Pi Runtime update available — Pi 0.85.2"
    );
    assert_eq!(
        about_runtime_line(Some(&status(false, 0, false, Some("offline"))), false).unwrap(),
        "Pi Runtime check failed: offline"
    );
}

#[tokio::test]
async fn signed_out_synced_runtime_stops_and_reboots_local() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
            dir.path().join("session.json"),
            r#"{"refreshToken":"still-valid","user":{"id":"user_1","email":"u@example.com"},"orgId":"org_1"}"#,
        )
        .unwrap();
    let port = cypher_env::ipc_socket(dir.path()).unwrap();
    let listener = cypher_rpc::LocalListener::bind(&port).await.unwrap();
    drop(listener);
    let boot = EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_socket: port.clone(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: Some("client_test".into()),
        default_harness: cypher_proto::HarnessId::Mock,
    };
    let synced = crate::state::EngineHandle::bootstrap(boot.clone())
        .await
        .expect("saved session opens its synced profile");
    assert_eq!(synced.engine_info().workspace_scope, WorkspaceScope::Synced);

    synced
        .client()
        .call(methods::SIGN_OUT, serde_json::json!({}))
        .await
        .expect("sign out clears credentials");
    stop_synced_runtime(synced, port, dir.path())
        .await
        .expect("synced runtime drains and releases ownership");

    assert!(!dir.path().join("session.json").exists());
    let local = crate::state::EngineHandle::bootstrap(boot)
        .await
        .expect("same process can continue locally");
    assert_eq!(local.engine_info().workspace_scope, WorkspaceScope::Local);
    local.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn remote_shutdown_waits_for_engine_lock_release() {
    let dir = tempfile::tempdir().unwrap();
    let lock = InstanceLock::acquire(dir.path()).unwrap();
    let port = cypher_env::ipc_socket(dir.path()).unwrap();
    let listener = cypher_rpc::LocalListener::bind(&port).await.unwrap();
    drop(listener);
    let lock_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let released_by_task = lock_released.clone();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(lock);
        released_by_task.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_secs(2))
        .await
        .unwrap();
    assert!(lock_released.load(std::sync::atomic::Ordering::SeqCst));
    release.await.unwrap();
}

#[tokio::test]
async fn remote_shutdown_times_out_while_ipc_remains_open() {
    let dir = tempfile::tempdir().unwrap();
    let port = cypher_env::ipc_socket(dir.path()).unwrap();
    let listener = cypher_rpc::LocalListener::bind(&port).await.unwrap();

    let error = wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_millis(100))
        .await
        .unwrap_err();

    assert!(error.contains("did not finish stopping"));
    drop(listener);
}

#[test]
fn account_actions_follow_the_attached_workspace_scope() {
    assert_eq!(
        account_menu_action(Some(WorkspaceScope::Local), SyncFlow::Idle),
        Some(AccountMenuAction::EnableSync)
    );
    assert_eq!(
        account_menu_action(Some(WorkspaceScope::Synced), SyncFlow::Idle),
        Some(AccountMenuAction::SignOut)
    );
    assert_eq!(
        account_menu_action(Some(WorkspaceScope::Development), SyncFlow::Idle),
        None
    );
}

#[test]
fn local_sign_in_offers_the_in_place_switch() {
    let signed_in = AuthState::SignedIn {
        user: cypher_proto::UserProfile {
            id: "user-1".into(),
            email: "user@example.com".into(),
            name: None,
            avatar_url: None,
        },
        org_id: Some("org-1".into()),
    };

    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::Enabling,
            Some(WorkspaceScope::Local),
            Some(&signed_in),
        ),
        SyncFlow::SwitchOffer { notice_open: true }
    );
    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::Idle,
            Some(WorkspaceScope::Local),
            Some(&signed_in),
        ),
        SyncFlow::SwitchOffer { notice_open: true },
        "another viewport derives the pending switch from AuthStatus"
    );
    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::SwitchOffer { notice_open: false },
            Some(WorkspaceScope::Local),
            Some(&signed_in),
        ),
        SyncFlow::SwitchOffer { notice_open: false },
        "shared auth updates do not reopen a postponed wizard"
    );
    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::RestartPending { notice_open: false },
            Some(WorkspaceScope::Local),
            Some(&signed_in),
        ),
        SyncFlow::RestartPending { notice_open: false },
        "the quit fallback survives shared auth updates too"
    );
    assert_eq!(
        account_menu_action(
            Some(WorkspaceScope::Local),
            SyncFlow::SwitchOffer { notice_open: false },
        ),
        Some(AccountMenuAction::RestartPending)
    );
    for notice_open in [true, false] {
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::SwitchOffer { notice_open },
                Some(WorkspaceScope::Local),
                Some(&AuthState::SignedOut),
            ),
            SyncFlow::Idle,
            "revoked credentials cancel the pending switch"
        );
    }
}

#[test]
fn import_summary_errors_are_a_failure_not_a_success() {
    // Clean summary → done with counts.
    let clean = serde_json::json!({
        "kind": "summary", "importedChats": 2, "skippedChats": 1, "errors": []
    });
    assert_eq!(import_summary_outcome(&clean), Ok((2, 1)));

    // Any error means the wizard must NOT say "all set" — partial
    // migrations surface as an explicit failure with the first cause.
    let partial = serde_json::json!({
        "kind": "summary", "importedChats": 1, "skippedChats": 0,
        "errors": ["chat c2: journal copy failed"]
    });
    let message = import_summary_outcome(&partial).expect_err("errors must fail");
    assert!(message.contains("journal copy failed"), "{message}");
    assert!(message.contains("1 imported"), "{message}");

    let many = serde_json::json!({
        "kind": "summary", "importedChats": 0, "skippedChats": 0,
        "errors": ["a", "b", "c"]
    });
    let message = import_summary_outcome(&many).expect_err("errors must fail");
    assert!(message.contains("3 failures"), "{message}");

    // A summary missing the errors field entirely (older engine) is
    // treated as clean rather than failing every import.
    let legacy = serde_json::json!({ "kind": "summary", "importedChats": 4 });
    assert_eq!(import_summary_outcome(&legacy), Ok((4, 0)));
}

#[test]
fn spaces_only_local_work_still_gets_the_import_offer() {
    assert_eq!(local_work_phrase(0, 0), None, "nothing to bring");
    assert_eq!(local_work_phrase(2, 0).as_deref(), Some("the 2 sessions"));
    assert_eq!(
        local_work_phrase(0, 1).as_deref(),
        Some("the 1 project"),
        "a projects-only profile must be offered the import, not a bare switch"
    );
    assert_eq!(
        local_work_phrase(1, 2).as_deref(),
        Some("the 1 session and 2 projects")
    );
}

#[test]
fn dismissed_import_failure_stays_reachable_on_a_synced_runtime() {
    let signed_in = AuthState::SignedIn {
        user: cypher_proto::UserProfile {
            id: "user-1".into(),
            email: "user@example.com".into(),
            name: None,
            avatar_url: None,
        },
        org_id: Some("org-1".into()),
    };

    // "Later" postpones the failure notice; it must not evaporate.
    let dismissed = SyncFlow::ImportFailed { notice_open: false };
    assert_eq!(
        sync_flow_after_auth(dismissed, Some(WorkspaceScope::Synced), Some(&signed_in)),
        dismissed,
        "a postponed import failure survives auth/scope updates"
    );

    // …and the account menu on the SYNCED runtime still exposes the
    // re-entry point. This is the whole point: after the switch there is
    // no local runtime left to re-derive an offer from, so this menu row
    // is the only path back to the retry dialog.
    assert_eq!(
        account_menu_action(Some(WorkspaceScope::Synced), dismissed),
        Some(AccountMenuAction::RestartPending),
        "retry must remain reachable after dismissal"
    );
    assert_eq!(
        account_menu_action(
            Some(WorkspaceScope::Synced),
            SyncFlow::ImportFailed { notice_open: true },
        ),
        Some(AccountMenuAction::RestartPending)
    );

    // Resolving the failure restores the normal synced menu.
    assert_eq!(
        account_menu_action(Some(WorkspaceScope::Synced), SyncFlow::Idle),
        Some(AccountMenuAction::SignOut)
    );
}

#[test]
fn switch_lifecycle_survives_the_runtime_replacement_window() {
    let signed_in = AuthState::SignedIn {
        user: cypher_proto::UserProfile {
            id: "user-1".into(),
            email: "user@example.com".into(),
            name: None,
            avatar_url: None,
        },
        org_id: Some("org-1".into()),
    };
    for flow in [
        SyncFlow::Switching { import: true },
        SyncFlow::Importing { done: 1, total: 3 },
        SyncFlow::ImportDone {
            imported: 3,
            skipped: 0,
        },
        SyncFlow::ImportFailed { notice_open: true },
        SyncFlow::ImportFailed { notice_open: false },
    ] {
        // Local (before the stop), detached (mid-replacement), and synced
        // (replacement runtime up): the driver owns these states — auth
        // and scope edges must never reset them.
        assert_eq!(
            sync_flow_after_auth(flow, Some(WorkspaceScope::Local), Some(&signed_in)),
            flow
        );
        assert_eq!(sync_flow_after_auth(flow, None, None), flow);
        assert_eq!(
            sync_flow_after_auth(flow, Some(WorkspaceScope::Synced), Some(&signed_in)),
            flow
        );
    }
}

#[test]
fn synced_sign_out_blocks_every_viewport_and_cannot_switch_accounts() {
    let signed_in_as_another_user = AuthState::SignedIn {
        user: cypher_proto::UserProfile {
            id: "user-2".into(),
            email: "other@example.com".into(),
            name: None,
            avatar_url: None,
        },
        org_id: Some("org-2".into()),
    };

    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::SigningOut,
            Some(WorkspaceScope::Synced),
            Some(&AuthState::SignedOut),
        ),
        SyncFlow::SignedOutRestartRequired,
        "the viewport that requested sign-out is blocked by AuthStatus"
    );
    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::Idle,
            Some(WorkspaceScope::Synced),
            Some(&AuthState::SignedOut),
        ),
        SyncFlow::SignedOutRestartRequired,
        "another viewport observing the same runtime is also blocked"
    );
    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::SignedOutRestartRequired,
            Some(WorkspaceScope::Synced),
            Some(&signed_in_as_another_user),
        ),
        SyncFlow::SignedOutRestartRequired,
        "new credentials cannot reopen the previous account's store"
    );
}

#[test]
fn titlebar_cluster_matches_cypher_window_controls() {
    // The cluster clears the traffic lights by 14px (open: lights 14..74 →
    // 88, zeron window-controls.tsx; collapsed: 21..81 → 95), and reclaims
    // the inset when fullscreen hides them.
    assert_eq!(
        titlebar_cluster_start(false, TRAFFIC_LIGHT_INSET_OPEN),
        88.0
    );
    assert_eq!(
        titlebar_cluster_start(false, TRAFFIC_LIGHT_INSET_COLLAPSED),
        95.0
    );
    assert_eq!(titlebar_cluster_start(true, TRAFFIC_LIGHT_INSET_OPEN), 12.0);
}

#[test]
fn collapsed_chrome_controls_share_the_tab_strip_center_line() {
    // Tabs: a 40px header in a card inset 8px → centre 28. Collapsed, the
    // 14px traffic lights sit at {21,21} (centre 28, square in the corner)
    // and the cluster centres on the same line; open, both centre on 21.
    assert_eq!(CHROME_CENTER_Y, 28.0);
    assert_eq!(TRAFFIC_LIGHT_INSET_COLLAPSED, 21.0);
    assert_eq!(
        chrome_center_y(TRAFFIC_LIGHT_INSET_COLLAPSED),
        CHROME_CENTER_Y
    );
    assert_eq!(chrome_center_y(TRAFFIC_LIGHT_INSET_OPEN), 21.0);
}

#[test]
fn windows_caption_controls_reserve_titlebar_space() {
    assert_eq!(titlebar_right_padding(true, 16.0), 124.0);
    assert_eq!(titlebar_right_padding(false, 16.0), 16.0);
}

#[test]
fn cluster_buttons_start_per_platform() {
    // Linux: buttons at 10..86.
    assert_eq!(
        cluster_buttons_start(false, false, TRAFFIC_LIGHT_INSET_OPEN),
        10.0
    );
    assert_eq!(CLUSTER_BUTTONS_WIDTH, 76.0);
    // macOS: buttons start at the traffic-light cluster start…
    assert_eq!(
        cluster_buttons_start(true, false, TRAFFIC_LIGHT_INSET_OPEN),
        88.0
    );
    // …and reclaim the inset in fullscreen (starts at 12).
    assert_eq!(
        cluster_buttons_start(true, true, TRAFFIC_LIGHT_INSET_OPEN),
        12.0
    );
}

// ---- sidebar resort FLIP diff ----

fn keys(list: &[(&str, f32)]) -> Vec<(String, f32)> {
    list.iter().map(|(k, h)| (k.to_string(), *h)).collect()
}

#[test]
fn resort_offsets_empty_when_order_unchanged() {
    let order = keys(&[("a", 29.0), ("b", 29.0), ("c", 45.0)]);
    assert!(resort_offsets(&order, &order, 2.0).is_empty());
}

#[test]
fn resort_offsets_activity_moves_row_to_top() {
    // c (bottom, y=62) jumps to top: c glides down-from-above? No — c's
    // old y is 62, new y is 0 → starts +62 below… offset = old - new = +62,
    // painted at +62 decaying to 0 (a glide UP into place). a and b shift
    // down by c's height + gap (31).
    let old = keys(&[("a", 29.0), ("b", 29.0), ("c", 29.0)]);
    let new = keys(&[("c", 29.0), ("a", 29.0), ("b", 29.0)]);
    let offsets = resort_offsets(&old, &new, 2.0);
    assert_eq!(offsets.get("c"), Some(&62.0));
    assert_eq!(offsets.get("a"), Some(&-31.0));
    assert_eq!(offsets.get("b"), Some(&-31.0));
}

#[test]
fn resort_offsets_respect_heights_and_gap() {
    // Tall row (45px) swaps with a short one (29px).
    let old = keys(&[("tall", 45.0), ("short", 29.0)]);
    let new = keys(&[("short", 29.0), ("tall", 45.0)]);
    let offsets = resort_offsets(&old, &new, 2.0);
    // short: old y 47 → new y 0; tall: old y 0 → new y 31.
    assert_eq!(offsets.get("short"), Some(&47.0));
    assert_eq!(offsets.get("tall"), Some(&-31.0));
}

#[test]
fn resort_offsets_ignore_added_and_removed_keys() {
    let old = keys(&[("a", 29.0), ("gone", 29.0), ("b", 29.0)]);
    let new = keys(&[("new", 29.0), ("a", 29.0), ("b", 29.0)]);
    let offsets = resort_offsets(&old, &new, 2.0);
    // "new" has no old position (fades in instead); "gone" just goes.
    assert!(!offsets.contains_key("new"));
    assert!(!offsets.contains_key("gone"));
    // a: old 0 → new 31 (pushed down by the insert); b: 62 → 62 (gone's
    // slot replaced by "new" of equal height — no move, no entry).
    assert_eq!(offsets.get("a"), Some(&-31.0));
    assert_eq!(offsets.get("b"), None);
}

#[test]
fn resort_glide_spec_matches_original() {
    // 260ms cubic-bezier(0.22, 1, 0.36, 1).
    assert_eq!(RESORT.duration_ms, 260);
    assert_eq!(RESORT.curve, motion::EASE_RESORT);
}

// ---- navigation history (titlebar back/forward) ----

fn chat(id: &str) -> NavEntry {
    NavEntry::Chat(id.to_string())
}

#[test]
fn settings_navigation_groups_are_disjoint_and_complete() {
    let listed = SettingsSection::NAV_GROUPS
        .iter()
        .flat_map(|(_, sections)| sections.iter().copied())
        .collect::<Vec<_>>();
    assert_eq!(listed.len(), SettingsSection::ALL.len());
    for section in SettingsSection::ALL {
        assert_eq!(
            listed.iter().filter(|s| **s == section).count(),
            1,
            "{section:?}"
        );
    }
    assert_eq!(
        SettingsSection::DEVICE_SETTINGS,
        [
            SettingsSection::Harnesses,
            SettingsSection::Providers,
            SettingsSection::Subagents,
            SettingsSection::Commands,
            SettingsSection::Mcp,
            SettingsSection::Permissions,
            SettingsSection::Github,
        ]
    );
    assert_eq!(
        SettingsSection::CLIENT_SETTINGS,
        [
            SettingsSection::Appearance,
            SettingsSection::Notifications,
            SettingsSection::Shortcuts,
        ]
    );
    assert_eq!(
        SettingsSection::WORKSPACE_SETTINGS,
        [
            SettingsSection::Devices,
            SettingsSection::Memory,
            SettingsSection::Archived,
        ]
    );
}

#[test]
fn nav_history_starts_with_nothing_to_walk() {
    let nav = NavHistory::new(chat(""));
    assert!(!nav.can_back());
    assert!(!nav.can_forward());
    assert_eq!(*nav.current(), chat(""));
}

#[test]
fn nav_push_then_back_and_forward() {
    let mut nav = NavHistory::new(chat("a"));
    nav.push(chat("b"));
    nav.push(NavEntry::Settings(SettingsSection::Devices));
    assert!(nav.can_back());
    assert!(!nav.can_forward());

    // Back walks toward the oldest entry without dropping anything.
    assert_eq!(
        nav.back(),
        Some(chat("b")),
        "back lands on the previous route"
    );
    assert_eq!(nav.back(), Some(chat("a")));
    assert!(!nav.can_back());
    assert!(nav.can_forward());
    assert_eq!(nav.back(), None, "past the oldest entry is a no-op");

    // Forward retraces the same path.
    assert_eq!(nav.forward(), Some(chat("b")));
    assert_eq!(
        nav.forward(),
        Some(NavEntry::Settings(SettingsSection::Devices))
    );
    assert!(!nav.can_forward());
    assert_eq!(nav.forward(), None);
}

#[test]
fn nav_push_dedups_the_current_route() {
    let mut nav = NavHistory::new(chat("a"));
    nav.push(chat("a"));
    nav.push(chat("a"));
    assert_eq!(nav.len(), 1, "re-selecting the current route never stacks");
    nav.push(NavEntry::Settings(SettingsSection::Providers));
    nav.push(NavEntry::Settings(SettingsSection::Providers));
    assert_eq!(nav.len(), 2);
}

#[test]
fn nav_canvas_push_from_a_chat_is_one_entry_and_dedups_on_repeat() {
    // `open_new_session` / `land_in_space` push the empty canvas route
    // explicitly; `on_state_changed` follows with the same entry after
    // the chat clears. Both must land as ONE entry (the push dedups),
    // and a second `+` while already on the canvas must not stack either.
    let mut nav = NavHistory::new(chat("a"));
    nav.push(chat("")); // open_new_session: chat "a" → canvas
    nav.push(chat("")); // on_state_changed after select_chat(None)
    nav.push(chat("")); // another `+` on the canvas
    assert_eq!(nav.len(), 2, "canvas push dedups against itself");
    assert_eq!(*nav.current(), chat(""));
    assert_eq!(nav.back(), Some(chat("a")), "Back returns to the session");

    // Already on the canvas: pushing the canvas is a strict no-op.
    let mut again = NavHistory::new(chat(""));
    again.push(chat(""));
    assert_eq!(again.len(), 1);
    assert!(!again.can_back());
}

#[test]
fn nav_push_truncates_the_forward_branch() {
    // a → b → c, back to a, then push d: the b/c branch is gone (browser
    // semantics — zeron's memory history PUSH truncates entries ahead).
    let mut nav = NavHistory::new(chat("a"));
    nav.push(chat("b"));
    nav.push(chat("c"));
    nav.back();
    nav.back();
    assert_eq!(*nav.current(), chat("a"));
    assert!(nav.can_forward());
    nav.push(chat("d"));
    assert!(!nav.can_forward(), "the old branch is unreachable");
    assert_eq!(nav.len(), 2);
    assert_eq!(nav.back(), Some(chat("a")));
    assert_eq!(nav.forward(), Some(chat("d")));
}

#[test]
fn nav_replace_swaps_in_place() {
    // The boot auto-select replaces the untouched canvas entry, so Back
    // stays disabled after landing in the last-used chat.
    let mut nav = NavHistory::new(chat(""));
    nav.replace(chat("boot"));
    assert_eq!(nav.len(), 1);
    assert_eq!(*nav.current(), chat("boot"));
    assert!(!nav.can_back());
}

#[test]
fn nav_settings_sections_are_distinct_entries() {
    let mut nav = NavHistory::new(chat("a"));
    nav.push(NavEntry::Settings(SettingsSection::Devices));
    nav.push(NavEntry::Settings(SettingsSection::Shortcuts));
    assert_eq!(nav.len(), 3, "section changes are navigations");
    assert_eq!(
        nav.back(),
        Some(NavEntry::Settings(SettingsSection::Devices))
    );
    assert_eq!(nav.back(), Some(chat("a")));
}

// ---- Botboard (docs/design/agent-bots.md §13.1) ----

pub(super) fn test_bot(id: &str) -> cypher_proto::Bot {
    cypher_proto::Bot {
        id: id.into(),
        name: id.to_uppercase(),
        icon: None,
        color: None,
        host_device_id: "dev".into(),
        chat_id: format!("{id}-chat"),
        instructions: None,
        scope: Default::default(),
        capabilities: Default::default(),
        paused: false,
        created_at: Utc::now(),
        plans: Default::default(),
        session_choice: Default::default(),
    }
}

fn test_bot_chat(bot_id: &str) -> cypher_proto::Chat {
    cypher_proto::Chat {
        bot: Some(cypher_proto::BotChat::owner(bot_id)),
        ..test_chat(&format!("{bot_id}-chat"), false)
    }
}

fn set_bots(state: &Entity<AppState>, bots: &[&str], cx: &mut gpui::TestAppContext) {
    state.update(cx, |s, cx| {
        s.apply_bots(bots.iter().map(|id| test_bot(id)).collect());
        cx.notify();
    });
    cx.run_until_parked();
}

fn mode(shell: &Entity<Shell>, cx: &mut gpui::TestAppContext) -> crate::prefs::WindowMode {
    shell.read_with(cx, |shell, _| shell.settings.window_mode)
}

fn board_chat(shell: &Entity<Shell>, cx: &mut gpui::TestAppContext) -> Option<String> {
    shell.read_with(cx, |shell, _| shell.board_chat().map(str::to_string))
}

#[gpui::test]
fn a_bot_chat_opens_on_the_botboard_and_the_tabs_wait(cx: &mut gpui::TestAppContext) {
    use crate::prefs::WindowMode;
    let (shell, state) = test_shell(cx);
    set_bots(&state, &["bob"], cx);
    set_chats(
        &state,
        vec![
            test_chat("a", false),
            test_chat("b", false),
            test_bot_chat("bob"),
        ],
        cx,
    );
    shell.update(cx, |shell, cx| shell.open_chat("b".into(), cx));
    let tabs = tab_shape(&shell, cx);
    assert!(tabs.iter().flatten().all(|tab| tab != "bob-chat"));
    assert_eq!(mode(&shell, cx), WindowMode::Projects);
    assert_eq!(board_chat(&shell, cx), None, "no board slot until it shows");

    // Opening the bot's chat (its menu, a notification, a jump) shows it on
    // the Botboard; the tab layout is left as it was.
    shell.update(cx, |shell, cx| shell.open_chat("bob-chat".into(), cx));
    assert_eq!(mode(&shell, cx), WindowMode::Botboard);
    assert_eq!(board_chat(&shell, cx).as_deref(), Some("bob-chat"));
    assert_eq!(tab_shape(&shell, cx), tabs);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.settings.botboard_bot.as_deref(), Some("bob"));
        assert_eq!(shell.focused_slot(), shell.board.slot);
        let slot = &shell.tiles.slots[&shell.board.slot.unwrap()];
        assert_eq!(
            slot.state.read(cx).selected_chat.as_deref(),
            Some("bob-chat")
        );
    });
    assert_eq!(
        state
            .read_with(cx, |s, _| s.selected_chat.clone())
            .as_deref(),
        Some("bob-chat"),
        "main follows the conversation in front"
    );

    // Any other session goes back to Projects mode, layout intact.
    shell.update(cx, |shell, cx| shell.open_chat("b".into(), cx));
    assert_eq!(mode(&shell, cx), WindowMode::Projects);
    assert_eq!(tab_shape(&shell, cx), tabs);
    assert_eq!(
        state
            .read_with(cx, |s, _| s.selected_chat.clone())
            .as_deref(),
        Some("b")
    );
    // Tile verbs are Projects-mode only; the logo brings the bot back.
    shell.update(cx, |shell, cx| {
        shell.set_window_mode(WindowMode::Botboard, cx);
        shell.split_focused(crate::workspace::Edge::Right, cx);
        shell.close_focused_tab(cx);
    });
    assert_eq!(tab_shape(&shell, cx), tabs);
    assert_eq!(board_chat(&shell, cx).as_deref(), Some("bob-chat"));
    // Back from the board lands on the tab it sat over.
    shell.update(cx, |shell, cx| shell.navigate_back(cx));
    assert_eq!(mode(&shell, cx), WindowMode::Projects);
    assert_eq!(
        state
            .read_with(cx, |s, _| s.selected_chat.clone())
            .as_deref(),
        Some("b")
    );
}

#[gpui::test]
fn a_restored_layout_drops_its_bot_tabs(cx: &mut gpui::TestAppContext) {
    use crate::workspace::{TabKey, Workspace};
    let ui = tempfile::tempdir().unwrap().keep();
    let mut saved = Workspace::new();
    saved.open(TabKey::session("a"));
    saved.open(TabKey::session("bob-chat"));
    UiSettings {
        workspace: Some(saved),
        ..Default::default()
    }
    .save(&ui)
    .unwrap();
    let (shell, state) = test_shell_in(ui, cx);
    set_bots(&state, &["bob"], cx);
    set_chats(
        &state,
        vec![test_chat("a", false), test_bot_chat("bob")],
        cx,
    );
    assert_eq!(tab_shape(&shell, cx), vec![vec!["a".to_string()]]);
    // …and lands on the Botboard with that bot.
    assert_eq!(mode(&shell, cx), crate::prefs::WindowMode::Botboard);
    assert_eq!(board_chat(&shell, cx).as_deref(), Some("bob-chat"));
}

#[gpui::test]
fn a_saved_botboard_survives_the_boot_landing(cx: &mut gpui::TestAppContext) {
    use crate::prefs::WindowMode;
    let ui = tempfile::tempdir().unwrap().keep();
    UiSettings {
        window_mode: WindowMode::Botboard,
        botboard_bot: Some("bob".into()),
        ..Default::default()
    }
    .save(&ui)
    .unwrap();
    let (shell, state) = test_shell_in(ui, cx);
    set_bots(&state, &["bob"], cx);
    // No sessions: the landing opens a canvas in Projects mode's layout,
    // which waits behind the board.
    set_chats(&state, vec![test_bot_chat("bob")], cx);
    assert_eq!(mode(&shell, cx), WindowMode::Botboard);
    assert_eq!(board_chat(&shell, cx).as_deref(), Some("bob-chat"));
    shell.read_with(cx, |shell, _| {
        assert!(
            shell
                .workspace
                .tabs()
                .any(|tab| matches!(tab, crate::workspace::TabKey::NewSession(_)))
        );
    });
}

#[gpui::test]
fn the_botboard_survives_its_bot_being_deleted(cx: &mut gpui::TestAppContext) {
    use crate::prefs::WindowMode;
    let ui = tempfile::tempdir().unwrap().keep();
    UiSettings {
        window_mode: WindowMode::Botboard,
        botboard_bot: Some("ann".into()),
        ..Default::default()
    }
    .save(&ui)
    .unwrap();
    let (shell, state) = test_shell_in(ui, cx);
    set_chats(
        &state,
        vec![
            test_chat("a", false),
            test_bot_chat("bob"),
            test_bot_chat("ann"),
        ],
        cx,
    );
    // Nothing moves before the bots frame.
    shell.read_with(cx, |shell, _| {
        assert_eq!(shell.settings.botboard_bot.as_deref(), Some("ann"));
        assert!(shell.board.slot.is_none());
    });
    set_bots(&state, &["bob", "ann"], cx);
    assert_eq!(board_chat(&shell, cx).as_deref(), Some("ann-chat"));
    // Deleting the current bot moves to another one…
    set_bots(&state, &["bob"], cx);
    set_chats(
        &state,
        vec![test_chat("a", false), test_bot_chat("bob")],
        cx,
    );
    assert_eq!(board_chat(&shell, cx).as_deref(), Some("bob-chat"));
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.settings.botboard_bot.clone()),
        Some("bob".into())
    );
    // …and the last one leaves the empty state, still on the Botboard.
    set_bots(&state, &[], cx);
    set_chats(&state, vec![test_chat("a", false)], cx);
    assert_eq!(board_chat(&shell, cx), None);
    assert_eq!(mode(&shell, cx), WindowMode::Botboard);
    shell.read_with(cx, |shell, _| {
        assert_eq!(shell.settings.botboard_bot, None);
        assert_eq!(shell.focused_slot(), None);
    });
}

#[gpui::test]
fn the_detail_panel_opens_replaces_and_closes(cx: &mut gpui::TestAppContext) {
    use crate::prefs::WindowMode;
    use bots::PanelItem;
    let (shell, state) = test_shell(cx);
    set_bots(&state, &["bob", "ann"], cx);
    set_chats(
        &state,
        vec![
            test_chat("a", false),
            test_bot_chat("bob"),
            test_bot_chat("ann"),
        ],
        cx,
    );
    let panel = |cx: &mut gpui::TestAppContext| shell.read_with(cx, |s, _| s.board.panel.clone());
    shell.update(cx, |shell, cx| {
        shell.open_bot("bob", cx);
        shell.open_panel(PanelItem::Activity, cx);
    });
    assert_eq!(panel(cx), Some(PanelItem::Activity));
    // Another row replaces it; the open row again closes it.
    let project = PanelItem::Project("sp".into());
    shell.update(cx, |shell, cx| shell.open_panel(project.clone(), cx));
    assert_eq!(panel(cx), Some(project.clone()));
    shell.update(cx, |shell, cx| shell.open_panel(project.clone(), cx));
    assert_eq!(panel(cx), None);
    // Esc closes it, unless a bot dialog over it takes the key.
    shell.update(cx, |shell, cx| {
        shell.open_panel(PanelItem::Activity, cx);
        shell.dialogs.delete_bot = Some("bob".into());
        assert!(!shell.close_panel_on_escape(cx));
        shell.dialogs.delete_bot = None;
        assert!(shell.close_panel_on_escape(cx));
        assert!(!shell.close_panel_on_escape(cx), "nothing left to close");
    });
    assert_eq!(panel(cx), None);
    // Switching bots shuts it at once; so does Projects mode for Esc.
    shell.update(cx, |shell, cx| {
        shell.open_panel(PanelItem::Activity, cx);
        shell.open_bot("ann", cx);
    });
    assert_eq!(panel(cx), None);
    shell.update(cx, |shell, cx| {
        shell.open_panel(PanelItem::Activity, cx);
        shell.set_window_mode(WindowMode::Projects, cx);
        assert!(!shell.close_panel_on_escape(cx), "the board is not shown");
    });
}

#[gpui::test]
fn the_needs_you_panel_shuts_once_its_last_item_is_decided(cx: &mut gpui::TestAppContext) {
    use bots::PanelItem;
    use cypher_proto::{ApprovalState, BotApproval};
    let (shell, state) = test_shell(cx);
    set_bots(&state, &["bob"], cx);
    set_chats(&state, vec![test_bot_chat("bob")], cx);
    let approval = |id: &str| BotApproval {
        id: id.into(),
        bot_id: "bob".into(),
        thread_chat_id: "t".into(),
        work_id: None,
        action: "Start a session".into(),
        reason: "It read a peer's answer".into(),
        created_at: 0,
        state: ApprovalState::Pending,
        decision: None,
    };
    let set_approvals = |ids: &[&str], cx: &mut gpui::TestAppContext| {
        let approvals = ids.iter().map(|id| approval(id)).collect();
        state.update(cx, |s, cx| {
            s.apply_bot_records(cypher_proto::BotRecords {
                approvals,
                ..Default::default()
            });
            cx.notify();
        });
        cx.run_until_parked();
    };
    let panel = |cx: &mut gpui::TestAppContext| shell.read_with(cx, |s, _| s.board.panel.clone());
    let decide = |id: &str, cx: &mut gpui::TestAppContext| {
        let params = serde_json::json!({
            "op": "resolveBotApproval",
            "approvalId": id,
            "approve": true,
        });
        shell.update(cx, |shell, cx| shell.decide_need(params, cx));
    };
    set_approvals(&["a", "b"], cx);
    shell.update(cx, |shell, cx| {
        shell.open_bot("bob", cx);
        shell.open_panel(PanelItem::NeedsYou, cx);
    });
    // A decision with more left keeps the page open…
    decide("a", cx);
    set_approvals(&["b"], cx);
    assert_eq!(panel(cx), Some(PanelItem::NeedsYou));
    // …and the last one shuts it once it lands, not when it is sent.
    decide("b", cx);
    assert_eq!(panel(cx), Some(PanelItem::NeedsYou));
    set_approvals(&[], cx);
    assert_eq!(panel(cx), None);
    // Emptied without a decision here (another device), it stays open.
    set_approvals(&["c"], cx);
    shell.update(cx, |shell, cx| shell.open_panel(PanelItem::NeedsYou, cx));
    set_approvals(&[], cx);
    assert_eq!(panel(cx), Some(PanelItem::NeedsYou));
    // A decision made before the owner moved to another item is forgotten.
    set_approvals(&["d"], cx);
    decide("d", cx);
    shell.update(cx, |shell, cx| {
        shell.open_panel(PanelItem::Activity, cx);
        shell.open_panel(PanelItem::NeedsYou, cx);
    });
    set_approvals(&[], cx);
    assert_eq!(panel(cx), Some(PanelItem::NeedsYou));
}

/// A plan whose row was cut (docs/design/plan-body.md §5): its page
/// watches the workspace chat's body, shows the preview with one
/// "Loading the full plan…" line until a body of the row's rev arrives
/// (a stale one is ignored), and lets the owner approve meanwhile. A row
/// without revs reads as before and watches nothing.
#[gpui::test]
fn a_cut_plans_page_shows_the_preview_until_its_body_arrives(cx: &mut gpui::TestAppContext) {
    use bots::PanelItem;
    use cypher_proto::{BotWork, PlanBody, StepState, TextSource, WorkBodies, WorkState, WorkStep};
    let ui = tempfile::tempdir().unwrap().keep();
    cx.update(|cx| {
        gpui_tokio::init(cx);
        cx.set_global(Theme::for_appearance(crate::kit::theme::Appearance::Dark));
        crate::widgets::text_input::init(cx);
        crate::terminal::panel::init(cx);
        // The panel stands at its width at once, so its buttons are where
        // a click lands.
        cx.set_reduce_motion(true);
    });
    UiSettings {
        window_mode: crate::prefs::WindowMode::Botboard,
        botboard_bot: Some("bob".into()),
        ..Default::default()
    }
    .save(&ui)
    .unwrap();
    let state = cx.new(|_| AppState::new());
    let boot = EngineBootConfig {
        data_dir: ui.clone(),
        ipc_socket: cypher_env::ipc_socket(&ui).unwrap(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: cypher_proto::HarnessId::Mock,
    };
    let (shell, vcx) = cx.add_window_view({
        let state = state.clone();
        move |_, cx| Shell::new(state, boot, ui, cx)
    });

    let body = PlanBody::new(
        "Rewrite the billing exports, every one of them.".into(),
        [("1".to_string(), "Rewrite each export by hand.".to_string())].into(),
        0,
    );
    let plan = |id: &str, rev: Option<String>| BotWork {
        steered: Vec::new(),
        id: id.into(),
        bot_id: "bob".into(),
        title: "Billing exports".into(),
        instruction: "Rewrite the billing exports, every…".into(),
        summary: None,
        steps: vec![WorkStep {
            id: "1".into(),
            title: "Rewrite".into(),
            project: None,
            detail: Some("Rewrite each export…".into()),
            state: StepState::Pending,
            sessions: vec![],
            note: None,
            started_at: None,
            ended_at: None,
            session_choice: Default::default(),
        }],
        state: WorkState::Proposed,
        chat_id: "bob-chat".into(),
        tool_call_id: None,
        thread_chat_id: None,
        created_at: 0,
        started_at: None,
        ended_at: None,
        result: None,
        decision: None,
        stop_requested_at: None,
        notify: false,
        time_limit_minutes: None,
        time_added_minutes: None,
        warned_at: None,
        coordinator_choice: Default::default(),
        body_rev: rev,
        result_rev: None,
    };
    state.update(vcx, |s, cx| {
        s.connection = ConnectionStatus::Ready;
        s.workspace_scope = Some(WorkspaceScope::Local);
        s.auth = Some(AuthState::SignedOut);
        s.apply_bots(vec![test_bot("bob")]);
        s.apply_chats(vec![test_chat("a", false), test_bot_chat("bob")]);
        s.apply_bot_records(cypher_proto::BotRecords {
            work: vec![plan("cut", Some(body.rev.clone())), plan("whole", None)],
            ..Default::default()
        });
        cx.notify();
    });
    vcx.run_until_parked();
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
    };
    let text = |vcx: &mut gpui::VisualTestContext, id: &str| {
        shell.read_with(vcx, |shell, cx| {
            let work = shell
                .state
                .read(cx)
                .bot_records
                .work
                .iter()
                .find(|w| w.id == id)
                .cloned()
                .unwrap();
            shell.plan_text(&work)
        })
    };
    shell.update(vcx, |shell, cx| {
        shell.pages.setup_dismissed = true;
        shell.open_bot("bob", cx);
        shell.open_panel(PanelItem::WorkItem("cut".into()), cx);
    });
    draw(vcx);

    // The page watches the workspace chat's body (no engine here, so no
    // stream yet) and shows the preview, marked, with the loading line.
    assert_eq!(
        shell.read_with(vcx, |shell, _| shell.plan_body_watch()),
        Some(("cut".into(), "bob-chat".into(), false))
    );
    assert_eq!(text(vcx, "cut").plan, TextSource::Preview);
    assert!(
        vcx.debug_bounds("bot-work-loading").is_some(),
        "the loading line"
    );

    // Approve does not wait for the body: a click sends the decision (here
    // it finds no engine, so the sidebar says so).
    let approve = vcx.debug_bounds("bot-plan-approve").expect("Approve");
    vcx.simulate_click(approve.center(), gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(vcx, |shell, _| shell.sidebar.notice.clone()),
        Some("Engine not connected".into()),
        "Approve ran while the full plan was loading"
    );

    // A body of another rev is ignored.
    let mut stale = body.clone();
    stale.rev = "0000000000000000".into();
    shell.update(vcx, |shell, cx| {
        shell.apply_plan_body(
            "cut",
            WorkBodies {
                plan: Some(stale),
                result: None,
            },
            cx,
        )
    });
    draw(vcx);
    assert_eq!(text(vcx, "cut").plan, TextSource::Preview);
    assert!(vcx.debug_bounds("bot-work-loading").is_some());

    // The row's body arrives: the full text, and the line goes.
    shell.update(vcx, |shell, cx| {
        shell.apply_plan_body(
            "cut",
            WorkBodies {
                plan: Some(body.clone()),
                result: None,
            },
            cx,
        )
    });
    draw(vcx);
    let full = text(vcx, "cut");
    assert_eq!(full.plan, TextSource::Body);
    assert_eq!(full.detail(0), Some("Rewrite each export by hand."));
    assert!(vcx.debug_bounds("bot-work-loading").is_none());
    assert!(vcx.debug_bounds("bot-plan-approve").is_some());

    // A row without revs reads as it is, and nothing is watched.
    shell.update(vcx, |shell, cx| {
        shell.open_panel(PanelItem::WorkItem("whole".into()), cx)
    });
    draw(vcx);
    assert_eq!(
        shell.read_with(vcx, |shell, _| shell.plan_body_watch()),
        None
    );
    let whole = text(vcx, "whole");
    assert_eq!(whole.plan, TextSource::Row);
    assert_eq!(whole.detail(0), Some("Rewrite each export…"));
    assert!(vcx.debug_bounds("bot-work-loading").is_none());
    assert!(vcx.debug_bounds("bot-plan-approve").is_some());
}

#[test]
fn header_buttons_follow_the_mode() {
    use crate::prefs::WindowMode;
    use spaces::{HeaderAdd, header_buttons};
    let projects = header_buttons(false, WindowMode::Projects);
    assert!(projects.view && projects.quick_chat && projects.account);
    assert_eq!(projects.add, Some(HeaderAdd::Project));
    let board = header_buttons(false, WindowMode::Botboard);
    assert!(!board.view && !board.quick_chat && board.account);
    assert_eq!(board.add, Some(HeaderAdd::Bot));
    for mode in [WindowMode::Projects, WindowMode::Botboard] {
        let window = header_buttons(true, mode);
        assert!(!window.view && !window.quick_chat && !window.account);
        assert_eq!(window.add, None, "a project window has no Botboard");
    }
}

#[test]
fn needs_you_counts_bots_waiting_on_their_owner() {
    let now = Utc::now();
    let mut state = AppState::new();
    state.apply_bots(vec![test_bot("bob"), test_bot("ann"), test_bot("cat")]);
    state.apply_chats(vec![
        test_bot_chat("bob"),
        test_bot_chat("ann"),
        test_bot_chat("cat"),
        test_chat("plain", false),
    ]);
    let session = |chat: &str, status| cypher_proto::Session {
        chat_id: chat.into(),
        status,
        updated_at: now,
        ..crate::test_fixtures::session()
    };
    use cypher_proto::SessionStatus;
    state.apply_sessions(vec![
        session("bob-chat", SessionStatus::AwaitingInput),
        session("ann-chat", SessionStatus::Working),
        // Only bots' conversations count.
        session("plain", SessionStatus::AwaitingInput),
    ]);
    assert_eq!(bots::needs_you_count(&state, now), 1);
    state.apply_sessions(vec![
        session("bob-chat", SessionStatus::AwaitingInput),
        session("cat-chat", SessionStatus::AwaitingInput),
    ]);
    assert_eq!(bots::needs_you_count(&state, now), 2);
    state.apply_sessions(Vec::new());
    assert_eq!(bots::needs_you_count(&state, now), 0);
}

#[test]
fn needs_you_counts_records_and_worker_questions_across_bots() {
    use cypher_proto::{
        ApprovalState, BotApproval, BotWork, SessionStatus, StepState, WorkState, WorkStep,
    };
    let now = Utc::now();
    let mut state = AppState::new();
    state.apply_bots(vec![test_bot("bob"), test_bot("ann")]);
    state.apply_chats(vec![
        test_bot_chat("bob"),
        test_bot_chat("ann"),
        test_chat("worker", false),
        test_chat("plain", false),
    ]);
    let work = |id: &str, bot: &str, state: WorkState, sessions: Vec<String>| BotWork {
        steered: Vec::new(),
        id: id.into(),
        bot_id: bot.into(),
        title: id.into(),
        instruction: String::new(),
        summary: None,
        steps: vec![WorkStep {
            id: "1".into(),
            title: "Run the tests".into(),
            project: None,
            detail: None,
            state: StepState::Running,
            sessions,
            note: None,
            started_at: None,
            ended_at: None,
            session_choice: Default::default(),
        }],
        state,
        chat_id: format!("{bot}-chat"),
        tool_call_id: None,
        thread_chat_id: None,
        created_at: 0,
        started_at: None,
        ended_at: None,
        result: None,
        decision: None,
        stop_requested_at: None,
        notify: false,
        time_limit_minutes: None,
        time_added_minutes: None,
        warned_at: None,
        coordinator_choice: Default::default(),
        body_rev: None,
        result_rev: None,
    };
    let records = cypher_proto::BotRecords {
        work: vec![
            work("plan", "bob", WorkState::Proposed, vec![]),
            work("run", "ann", WorkState::Running, vec!["worker".into()]),
        ],
        approvals: vec![BotApproval {
            id: "a".into(),
            bot_id: "bob".into(),
            thread_chat_id: "t".into(),
            work_id: None,
            action: "Start a session".into(),
            reason: "It read a peer's answer".into(),
            created_at: 0,
            state: ApprovalState::Pending,
            decision: None,
        }],
        ..Default::default()
    };
    state.apply_bot_records(records);
    let bob = state.bots.iter().find(|b| b.id == "bob").cloned();
    let ann = state.bots.iter().find(|b| b.id == "ann").cloned();
    let (bob, ann) = (bob.expect("bob"), ann.expect("ann"));
    assert_eq!(
        bots::bot_needs(&state, &bob, now).len(),
        2,
        "plan + approval"
    );
    assert!(bots::bot_needs(&state, &ann, now).is_empty());
    assert_eq!(bots::needs_you_count(&state, now), 2);
    // A worker asking, and Bob's own conversation asking, both count.
    let session = |chat: &str| cypher_proto::Session {
        chat_id: chat.into(),
        status: SessionStatus::AwaitingInput,
        updated_at: now,
        ..crate::test_fixtures::session()
    };
    state.apply_sessions(vec![
        session("worker"),
        session("bob-chat"),
        session("plain"),
    ]);
    assert_eq!(
        bots::bot_needs(&state, &bob, now).first(),
        Some(&bots::Need::Chat),
        "the bot's own question leads"
    );
    assert_eq!(bots::bot_needs(&state, &ann, now).len(), 1);
    assert_eq!(bots::needs_you_count(&state, now), 4);
    // Sessions a bot started say so; others don't.
    assert_eq!(
        bots::started_by_mark(&state, "worker").as_deref(),
        Some("by ANN")
    );
    assert_eq!(bots::started_by_mark(&state, "plain"), None);
}

// ---- The workspace chat's dock (docs/features/files.md "Workspace chats") ----

use crate::shell::bots::PanelItem;
use crate::shell::session::SlotId;

/// A windowed shell on the Botboard showing workspace "bob", whose chat
/// runs in its folder and is granted projects "api" and "web" (as the
/// engine lists them); returns the board slot.
fn workspace_board(
    cx: &mut gpui::TestAppContext,
) -> (Entity<Shell>, &mut gpui::VisualTestContext, SlotId) {
    let ui = tempfile::tempdir().unwrap().keep();
    workspace_board_in(ui, cx)
}

/// [`workspace_board`] over the UI data in `ui`.
fn workspace_board_in(
    ui: PathBuf,
    cx: &mut gpui::TestAppContext,
) -> (Entity<Shell>, &mut gpui::VisualTestContext, SlotId) {
    let (shell, vcx) = windowed_board(ui, &["bob"], cx);
    let sid = shell.read_with(vcx, |shell, _| shell.board.slot.unwrap());
    let root = |space: Option<&str>, name: &str, path: &str| cypher_proto::ChatFileRoot {
        space_id: space.map(str::to_owned),
        name: name.into(),
        path: path.into(),
        checkout_id: space.map(|id| format!("co-{id}")),
    };
    let slot_state = shell.read_with(vcx, |shell, _| shell.tiles.slots[&sid].state.clone());
    slot_state.update(vcx, |s, cx| {
        assert_eq!(s.file_roots(), crate::state::FileRoots::Loading);
        s.set_file_roots_for_test(vec![
            root(None, "BOB", "/data/bots/bob"),
            root(Some("api"), "api", "/src/api"),
            root(Some("web"), "web", "/src/web"),
        ]);
        cx.notify();
    });
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
    (shell, vcx, sid)
}

/// A windowed shell on the Botboard with workspaces `bots` (each chat in
/// its folder) and session "a", showing the first workspace.
fn windowed_board<'a>(
    ui: PathBuf,
    bots: &[&str],
    cx: &'a mut gpui::TestAppContext,
) -> (Entity<Shell>, &'a mut gpui::VisualTestContext) {
    cx.update(|cx| {
        gpui_tokio::init(cx);
        cx.set_global(Theme::for_appearance(crate::kit::theme::Appearance::Dark));
        crate::widgets::text_input::init(cx);
        crate::terminal::panel::init(cx);
    });
    let state = cx.new(|_| AppState::new());
    let boot = EngineBootConfig {
        data_dir: ui.clone(),
        ipc_socket: cypher_env::ipc_socket(&ui).unwrap(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: cypher_proto::HarnessId::Mock,
    };
    let (shell, vcx) = cx.add_window_view({
        let state = state.clone();
        move |_, cx| Shell::new(state, boot, ui, cx)
    });
    state.update(vcx, |s, cx| {
        s.connection = ConnectionStatus::Ready;
        s.workspace_scope = Some(WorkspaceScope::Local);
        s.auth = Some(AuthState::SignedOut);
        s.local_device_id = Some("dev".into());
        s.apply_bots(bots.iter().map(|id| test_bot(id)).collect());
        s.apply_chats(
            std::iter::once(test_chat("a", false))
                .chain(bots.iter().map(|id| cypher_proto::Chat {
                    cwd: Some(format!("/data/bots/{id}")),
                    ..test_bot_chat(id)
                }))
                .collect(),
        );
        cx.notify();
    });
    vcx.run_until_parked();
    let first = format!("{}-chat", bots[0]);
    shell.update(vcx, |shell, cx| {
        shell.pages.setup_dismissed = true;
        shell.open_chat(first, cx);
    });
    vcx.run_until_parked();
    (shell, vcx)
}

fn board_side(
    shell: &Entity<Shell>,
    sid: SlotId,
    cx: &mut gpui::VisualTestContext,
) -> (bool, bool) {
    shell.read_with(cx, |shell, _| {
        (
            shell.tiles.slots[&sid].dock.open,
            shell.board.panel.is_some(),
        )
    })
}

fn open_link(shell: &Entity<Shell>, sid: SlotId, url: &str, cx: &mut gpui::VisualTestContext) {
    let link = crate::files::links::parse(url).unwrap();
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.open_file_link(sid, link, window, cx));
    });
    cx.run_until_parked();
}

fn shown_root(
    shell: &Entity<Shell>,
    sid: SlotId,
    cx: &mut gpui::VisualTestContext,
) -> Option<String> {
    shell.read_with(cx, |shell, cx| {
        crate::files::context_for(shell.tiles.slots[&sid].state.read(cx))
            .unwrap()
            .root_space_id
    })
}

#[gpui::test]
fn the_workspace_chat_has_a_dock_and_a_terminal(cx: &mut gpui::TestAppContext) {
    let (shell, vcx, sid) = workspace_board(cx);
    shell.read_with(vcx, |shell, cx| {
        assert!(!shell.slot_is_hidden_bot_thread(sid, cx));
        let slot = &shell.tiles.slots[&sid];
        assert!(shell.files_available(slot, cx));
        assert!(
            shell.slot_git_detected(slot, cx),
            "its projects are checkouts"
        );
    });
    shell.update(vcx, |shell, cx| shell.toggle_dock(sid, cx));
    assert_eq!(board_side(&shell, sid, vcx), (true, false));
    vcx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.toggle_terminal(sid, window, cx));
    });
    shell.read_with(vcx, |shell, _| {
        assert!(shell.tiles.slots[&sid].terminal_open);
    });
    // The board renders its chat with the rail and the open docks.
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
}

#[gpui::test]
fn the_dock_and_the_detail_panel_take_turns_on_the_right(cx: &mut gpui::TestAppContext) {
    let (shell, vcx, sid) = workspace_board(cx);
    shell.update(vcx, |shell, cx| shell.open_panel(PanelItem::NeedsYou, cx));
    assert_eq!(board_side(&shell, sid, vcx), (false, true));
    // The dock opening shuts the panel…
    shell.update(vcx, |shell, cx| shell.toggle_dock(sid, cx));
    assert_eq!(board_side(&shell, sid, vcx), (true, false));
    // …and the panel opening shuts the dock.
    shell.update(vcx, |shell, cx| shell.open_panel(PanelItem::Activity, cx));
    assert_eq!(board_side(&shell, sid, vcx), (false, true));
    // The rail's side panel button: shut what is open, then reopen it.
    shell.update(vcx, |shell, cx| shell.toggle_board_side(sid, cx));
    assert_eq!(board_side(&shell, sid, vcx), (false, false));
    shell.update(vcx, |shell, cx| shell.toggle_board_side(sid, cx));
    assert_eq!(board_side(&shell, sid, vcx), (false, true));
    shell.read_with(vcx, |shell, _| {
        assert_eq!(shell.board.panel, Some(PanelItem::Activity));
    });
    // A file reference opens Files in the dock, and the panel shuts.
    open_link(&shell, sid, "/src/web/app/page.tsx:12", vcx);
    assert_eq!(board_side(&shell, sid, vcx), (true, false));
    // The button shuts the dock, and brings the dock (with its Files tab)
    // back.
    shell.update(vcx, |shell, cx| shell.toggle_board_side(sid, cx));
    assert_eq!(board_side(&shell, sid, vcx), (false, false));
    shell.update(vcx, |shell, cx| shell.toggle_board_side(sid, cx));
    assert_eq!(board_side(&shell, sid, vcx), (true, false));
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
}

#[gpui::test]
fn a_reference_opens_in_the_root_it_resolves_in(cx: &mut gpui::TestAppContext) {
    use dock::DockSurface;
    let (shell, vcx, sid) = workspace_board(cx);
    // Nothing opened yet: Files shows the first granted project.
    assert_eq!(shown_root(&shell, sid, vcx).as_deref(), Some("api"));
    // An absolute path inside a project opens there, and Files and Git
    // follow that project from then on.
    open_link(&shell, sid, "/src/web/app/page.tsx:12", vcx);
    assert_eq!(shown_root(&shell, sid, vcx).as_deref(), Some("web"));
    shell.read_with(vcx, |shell, cx| {
        let slot = &shell.tiles.slots[&sid];
        assert!(slot.dock.open);
        assert!(matches!(slot.dock.active, DockSurface::Files(_)));
        let state = slot.state.read(cx);
        let git = state.git_chat_row().unwrap();
        assert_eq!(git.cwd.as_deref(), Some("/src/web"));
        assert_eq!(git.checkout_id.as_deref(), Some("co-web"));
    });
    // Inside the workspace's own folder: that folder.
    open_link(&shell, sid, "/data/bots/bob/notes.md", vcx);
    assert_eq!(shown_root(&shell, sid, vcx), None);
    // Git stays on a checkout (the folder is none).
    shell.read_with(vcx, |shell, cx| {
        let state = shell.tiles.slots[&sid].state.read(cx);
        assert_eq!(
            state.git_chat_row().unwrap().cwd.as_deref(),
            Some("/src/api")
        );
    });
    // Outside every root: never in Files; the pick stays.
    open_link(&shell, sid, "/src/website/a.rs", vcx);
    open_link(&shell, sid, "/no/such/folder/hosts", vcx);
    assert_eq!(shown_root(&shell, sid, vcx), None);
    let files_tabs = shell.read_with(vcx, |shell, _| shell.tiles.slots[&sid].files.len());
    assert_eq!(files_tabs, 1, "one Files tab, reused");
    // A relative path tries the workspace's folder first (with no engine
    // to ask, it opens there).
    open_link(&shell, sid, "/src/api/lib.rs", vcx);
    assert_eq!(shown_root(&shell, sid, vcx).as_deref(), Some("api"));
    open_link(&shell, sid, "src/lib.rs", vcx);
    assert_eq!(shown_root(&shell, sid, vcx), None);
}

#[gpui::test]
fn git_names_its_project_whenever_the_chat_has_several_roots(cx: &mut gpui::TestAppContext) {
    let (shell, vcx, sid) = workspace_board(cx);
    let slot_state = shell.read_with(vcx, |shell, _| shell.tiles.slots[&sid].state.clone());
    let root = |space: Option<&str>, git: bool| cypher_proto::ChatFileRoot {
        space_id: space.map(str::to_owned),
        name: space.unwrap_or("BOB").into(),
        path: format!("/src/{}", space.unwrap_or("bob")),
        checkout_id: git.then(|| format!("co-{}", space.unwrap_or("bob"))),
    };
    let names = |choice: Option<(Vec<cypher_proto::ChatFileRoot>, cypher_proto::ChatFileRoot)>| {
        choice.map(|(choices, shown)| {
            let names: Vec<String> = choices.into_iter().map(|root| root.name).collect();
            (names, shown.name)
        })
    };
    // Two checkouts: a menu of both.
    let two = slot_state.read_with(vcx, |s, _| names(s.git_root_choice()));
    assert_eq!(two, Some((vec!["api".into(), "web".into()], "api".into())));
    // One checkout among several roots, with Files on the other project:
    // Git still names the checkout it shows (a label, one choice).
    slot_state.update(vcx, |s, _| {
        s.set_file_roots_for_test(vec![
            root(None, false),
            root(Some("api"), true),
            root(Some("web"), false),
        ]);
        s.chat_roots_pick_for_test(Some("web"));
        assert_eq!(s.files_root().map(|r| r.name.as_str()), Some("web"));
    });
    let one = slot_state.read_with(vcx, |s, _| names(s.git_root_choice()));
    assert_eq!(one, Some((vec!["api".into()], "api".into())));
    // The workspace's folder alone: nothing to name.
    slot_state.update(vcx, |s, _| {
        s.set_file_roots_for_test(vec![root(None, false)])
    });
    assert_eq!(slot_state.read_with(vcx, |s, _| s.git_root_choice()), None);
}

// ---- Workspace switches keep each one's dock ----

fn a_turn(head: &str) -> std::sync::Arc<cypher_proto::TurnChanges> {
    std::sync::Arc::new(cypher_proto::TurnChanges {
        cwd: "/data/bots/x".into(),
        base: "t0".into(),
        head: head.into(),
        files: Vec::new(),
        file_count: 1,
        additions: 1,
        deletions: 0,
    })
}

fn dock_of(
    shell: &Entity<Shell>,
    sid: SlotId,
    cx: &mut gpui::VisualTestContext,
) -> (bool, Vec<dock::DockSurface>, dock::DockSurface) {
    shell.read_with(cx, |shell, _| {
        let slot = &shell.tiles.slots[&sid];
        (slot.dock.open, slot.dock.surfaces.clone(), slot.dock.active)
    })
}

#[gpui::test]
fn switching_workspaces_keeps_each_ones_dock(cx: &mut gpui::TestAppContext) {
    use crate::prefs::WindowMode;
    let ui = tempfile::tempdir().unwrap().keep();
    let (shell, vcx) = windowed_board(ui, &["bob", "ann"], cx);
    let bob = shell.read_with(vcx, |shell, _| shell.board.slot.unwrap());
    shell.update(vcx, |shell, cx| {
        shell.toggle_dock(bob, cx);
        shell.open_turn_diff(bob, a_turn("t1"), None, cx);
    });
    let left = dock_of(&shell, bob, vcx);
    assert!(left.0 && left.1.len() == 1);

    // Shift+↓ to the next workspace: bob's slot stays, hidden.
    shell.update(vcx, |shell, cx| shell.step_workspace(true, cx));
    vcx.run_until_parked();
    let ann = shell.read_with(vcx, |shell, _| shell.board.slot.unwrap());
    assert_ne!(ann, bob);
    assert_eq!(board_chat(&shell, vcx).as_deref(), Some("ann-chat"));
    shell.read_with(vcx, |shell, _| {
        assert!(shell.tiles.slots.contains_key(&bob));
        assert!(shell.board.kept.holds(bob));
        // Only the shown one takes keys, follow and seen-marking.
        assert_eq!(shell.focused_slot(), Some(ann));
        assert_eq!(shell.shown_chats(), vec!["ann-chat".to_string()]);
        assert!(!shell.tiles.slots[&ann].dock.open);
    });
    assert_eq!(
        shell
            .read_with(vcx, |shell, cx| shell.state.read(cx).selected_chat.clone())
            .as_deref(),
        Some("ann-chat"),
        "main follows the one in front"
    );
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();

    // Shift+↑ back: the same slot, its dock as left.
    shell.update(vcx, |shell, cx| shell.step_workspace(false, cx));
    vcx.run_until_parked();
    assert_eq!(shell.read_with(vcx, |shell, _| shell.board.slot), Some(bob));
    assert_eq!(dock_of(&shell, bob, vcx), left);
    // The switcher (a picker row) does the same.
    shell.update(vcx, |shell, cx| shell.open_bot("ann", cx));
    shell.update(vcx, |shell, cx| shell.open_bot("bob", cx));
    assert_eq!(shell.read_with(vcx, |shell, _| shell.board.slot), Some(bob));
    assert_eq!(dock_of(&shell, bob, vcx), left);
    // The Sessions view and back.
    shell.update(vcx, |shell, cx| {
        shell.set_window_mode(WindowMode::Projects, cx);
        shell.set_window_mode(WindowMode::Botboard, cx);
    });
    assert_eq!(shell.read_with(vcx, |shell, _| shell.board.slot), Some(bob));
    assert_eq!(dock_of(&shell, bob, vcx), left);
    // Coming back with the dock open, the detail panel stays shut.
    shell.update(vcx, |shell, cx| shell.open_bot("ann", cx));
    shell.update(vcx, |shell, cx| shell.open_panel(PanelItem::Activity, cx));
    shell.update(vcx, |shell, cx| shell.open_bot("bob", cx));
    assert_eq!(board_side(&shell, bob, vcx), (true, false));
    assert_eq!(
        shell.read_with(vcx, |shell, _| shell.board.side),
        bots::BoardSide::Dock
    );

    // A kept workspace deleted while hidden lets its slot go.
    shell.read_with(vcx, |shell, _| assert!(shell.board.kept.holds(ann)));
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |s, cx| {
            s.apply_bots(vec![test_bot("bob")]);
            s.apply_chats(vec![
                test_chat("a", false),
                cypher_proto::Chat {
                    cwd: Some("/data/bots/bob".into()),
                    ..test_bot_chat("bob")
                },
            ]);
            cx.notify();
        });
    });
    vcx.run_until_parked();
    shell.read_with(vcx, |shell, _| {
        assert!(!shell.tiles.slots.contains_key(&ann));
        assert!(!shell.board.kept.holds(ann));
        assert_eq!(shell.board.slot, Some(bob));
    });
}

#[gpui::test]
fn the_oldest_kept_workspace_goes_and_comes_back_from_prefs(cx: &mut gpui::TestAppContext) {
    let names: Vec<String> = (0..=bots::KEPT_BOARD_SLOTS + 1)
        .map(|ix| format!("w{ix}"))
        .collect();
    let ids: Vec<&str> = names.iter().map(String::as_str).collect();
    let ui = tempfile::tempdir().unwrap().keep();
    let (shell, vcx) = windowed_board(ui.clone(), &ids, cx);
    let first = shell.read_with(vcx, |shell, _| shell.board.slot.unwrap());
    shell.update(vcx, |shell, cx| {
        shell.toggle_dock(first, cx);
        shell.open_turn_diff(first, a_turn("t1"), None, cx);
        shell.open_turn_diff(first, a_turn("t2"), None, cx);
    });
    // Visit every other workspace: one more than the kept ones fit.
    for id in &ids[1..] {
        shell.update(vcx, |shell, cx| shell.open_bot(id, cx));
    }
    shell.read_with(vcx, |shell, _| {
        assert!(!shell.tiles.slots.contains_key(&first), "the oldest went");
        let held = shell
            .tiles
            .slots
            .keys()
            .filter(|sid| shell.board.holds(**sid))
            .count();
        assert_eq!(held, bots::KEPT_BOARD_SLOTS + 1);
    });
    // Its docks were saved as it left; a new slot brings them back.
    shell.update(vcx, |shell, cx| shell.open_bot(ids[0], cx));
    let again = shell.read_with(vcx, |shell, _| shell.board.slot.unwrap());
    assert_ne!(again, first);
    let (open, surfaces, active) = dock_of(&shell, again, vcx);
    assert!(open);
    assert_eq!(surfaces.len(), 2);
    assert_eq!(active, surfaces[1], "the tab in front stays in front");
    shell.read_with(vcx, |shell, cx| {
        let docks = shell.slot_docks(&shell.tiles.slots[&again], cx);
        let heads: Vec<String> = docks
            .surfaces
            .iter()
            .map(|saved| match saved {
                crate::prefs::SavedSurface::Turn { turn } => turn.head.clone(),
                other => panic!("not a turn: {other:?}"),
            })
            .collect();
        assert_eq!(heads, vec!["t1".to_string(), "t2".into()]);
    });
    // …and it is written to disk.
    vcx.executor()
        .advance_clock(Duration::from_millis(SAVE_DEBOUNCE_MS + 50));
    vcx.run_until_parked();
    let written = UiSettings::load(&ui);
    let saved = &written.session_docks["w0-chat"];
    assert!(saved.right_open);
    assert_eq!(saved.surfaces.len(), 2);
    assert_eq!(saved.active_surface, Some(1));
}

/// A restart: a session's saved Files and Git tabs come back on their
/// views; the workspace chat's root pick too, and a Files tab saved on a
/// root no longer shown drops its file quietly.
#[gpui::test]
fn saved_dock_tabs_come_back_after_a_restart(cx: &mut gpui::TestAppContext) {
    use crate::prefs::{SavedGitScope, SavedRoot, SavedSurface, SessionDock};
    let ui = tempfile::tempdir().unwrap().keep();
    let files = |root: Option<&str>, file: &str, line| SavedSurface::Files {
        root: root.map(str::to_owned),
        file: Some(file.into()),
        line,
    };
    UiSettings {
        session_docks: std::collections::HashMap::from([(
            "bob-chat".to_string(),
            SessionDock {
                right_open: true,
                surfaces: vec![
                    SavedSurface::Git {
                        scope: SavedGitScope::Branch,
                        base_ref: Some("main".into()),
                    },
                    files(Some("web"), "app/page.tsx", Some(12)),
                    files(Some("gone"), "x.rs", None),
                    SavedSurface::Git {
                        scope: SavedGitScope::History,
                        base_ref: None,
                    },
                ],
                active_surface: Some(1),
                root_pick: Some(SavedRoot {
                    space_id: Some("web".into()),
                }),
                used_at: 1,
                ..Default::default()
            },
        )]),
        ..Default::default()
    }
    .save(&ui)
    .unwrap();
    let (shell, vcx, sid) = workspace_board_in(ui, cx);
    let (open, surfaces, active) = dock_of(&shell, sid, vcx);
    assert!(open);
    assert_eq!(surfaces.len(), 4);
    assert_eq!(active, surfaces[1]);
    assert_eq!(shown_root(&shell, sid, vcx).as_deref(), Some("web"));
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
    shell.read_with(vcx, |shell, cx| {
        let docks = shell.slot_docks(&shell.tiles.slots[&sid], cx);
        assert_eq!(
            docks.surfaces,
            vec![
                SavedSurface::Git {
                    scope: SavedGitScope::Branch,
                    base_ref: Some("main".into()),
                },
                // Back on its file (its line waits for the text).
                files(Some("web"), "app/page.tsx", Some(12)),
                // Saved on a root not shown now: the tree alone.
                SavedSurface::Files {
                    root: Some("web".into()),
                    file: None,
                    line: None,
                },
                SavedSurface::Git {
                    scope: SavedGitScope::History,
                    base_ref: None,
                },
            ]
        );
        assert_eq!(docks.active_surface, Some(1));
        assert_eq!(
            docks.root_pick,
            Some(SavedRoot {
                space_id: Some("web".into())
            })
        );
        // Restored Files tabs got their composer hookup with the frame.
        let slot = &shell.tiles.slots[&sid];
        assert_eq!(slot.files_subs.len(), slot.files.len());
    });
}
