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
        crate::composer::init(cx);
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
        crate::composer::init(cx);
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
        assert_eq!(shell.slots.len(), 2);
        for slot in shell.slots.values() {
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
    let ctx = shell.read_with(cx, |shell, _| shell.slots[&canvas].state.clone());
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
    shell.read_with(cx, |shell, _| assert_eq!(shell.slots.len(), 1));
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
        let mut open: Vec<_> = shell.slots.values().map(|slot| slot.tab.clone()).collect();
        open.sort_by_key(|tab| tab.chat_id().map(str::to_string));
        assert_eq!(open, vec![TabKey::session("a"), TabKey::session("b")]);
        let b = shell.focused_slot().unwrap();
        let slot = &shell.slots[&b];
        assert!(slot.dock.open, "b's dock was open");
        assert_eq!(slot.right_fraction, Some(0.5));
        // A session without an entry starts from the latest sizes, closed.
        let a = shell.slot_for_tab(&TabKey::session("a")).unwrap();
        assert!(!shell.slots[&a].dock.open);
        assert_eq!(shell.slots[&a].right_fraction, Some(0.5));
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
        assert!(matches!(shell.slots[&canvas].tab, TabKey::NewSession(_)));
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
        assert_eq!(shell.slots.len(), 1);
        assert!(shell.slot_for_tab(&TabKey::session("b")).is_some());
    });
    shell.update(cx, |shell, cx| shell.toggle_zoom_focused(cx));
    shell.read_with(cx, |shell, _| assert_eq!(shell.slots.len(), 2));
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
    let popup = shell.read_with(cx, |shell, _| shell.comment_popup.clone());
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
        let slot = &shell.slots[&sid];
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
        crate::composer::init(cx);
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
        shell.setup_dismissed = true;
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
        assert_eq!(shell.slots.len(), 3);
        let widths: Vec<f32> = shell
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
    use cypher_engine::pi_packages::{PiPackageUpdate, PiUpdateStatus};

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
    use cypher_engine::pi_packages::{PiPackageUpdate, PiUpdateStatus};

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
    // zeron window-controls.tsx: `left: fullscreen ? 12 : 88` — the
    // cluster clears the {14,15} traffic lights, and reclaims the inset
    // when fullscreen hides them.
    assert_eq!(titlebar_cluster_start(false), 88.0);
    assert_eq!(titlebar_cluster_start(true), 12.0);
}

#[test]
fn windows_caption_controls_reserve_titlebar_space() {
    assert_eq!(titlebar_right_padding(true, 16.0), 124.0);
    assert_eq!(titlebar_right_padding(false, 16.0), 16.0);
}

#[test]
fn cluster_buttons_start_per_platform() {
    // Linux: buttons at 10..86.
    assert_eq!(cluster_buttons_start(false, false), 10.0);
    assert_eq!(CLUSTER_BUTTONS_WIDTH, 76.0);
    // macOS: buttons start at the 88px traffic-light cluster start…
    assert_eq!(cluster_buttons_start(true, false), 88.0);
    // …and reclaim the inset in fullscreen (starts at 12).
    assert_eq!(cluster_buttons_start(true, true), 12.0);
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
        [SettingsSection::Devices, SettingsSection::Archived,]
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
