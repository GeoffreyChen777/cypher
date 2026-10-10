use super::*;
use crate::settings::setup::tests::pump_until;
use cypher_engine::pi_runtime::PiRuntimePaths;
use cypher_engine::pi_subagents;
use gpui::AppContext;
use std::sync::Arc;

/// A stand-in engine that serves the subagent methods out of a real temp
/// runtime, so the page is exercised against the ACTUAL file format rather
/// than a hand-written fake reply.
struct SubagentFixture {
    paths: PiRuntimePaths,
}

#[async_trait::async_trait]
impl cypher_rpc::RpcService for SubagentFixture {
    async fn handle(
        &self,
        method: &str,
        mut params: serde_json::Value,
    ) -> Result<cypher_rpc::RpcReply, cypher_rpc::RpcError> {
        let value = match method {
            methods::ENGINE_INFO => {
                serde_json::json!({"deviceId":"viewer", "workspaceScope":"local"})
            }
            methods::ENGINE_READY => serde_json::json!({}),
            methods::LIST_MODELS => serde_json::json!([
                {
                    "id": "claude-bridge/claude-fable-5-1",
                    "label": "Fable 5.1",
                    "description": "claude-bridge · 200k context",
                    "reasoningLevels": ["minimal","low","medium","high","xhigh","max"],
                },
                {
                    "id": "claude-bridge/claude-opus-4-6",
                    "label": "Opus 4.6",
                    "description": "claude-bridge · 1M context",
                    "reasoningLevels": ["minimal","low","medium","high","max"],
                },
                {
                    "id": "openai/gpt-tiny",
                    "label": "GPT Tiny",
                    "description": "openai · 8k context",
                    "reasoningLevels": [],
                },
            ]),
            methods::LIST_PI_SUBAGENTS => {
                serde_json::to_value(pi_subagents::list(&self.paths)).unwrap()
            }
            methods::SAVE_PI_SUBAGENT => {
                assert_eq!(params["targetDeviceId"], "agent-host");
                params.as_object_mut().unwrap().remove("targetDeviceId");
                let original = params["originalName"].as_str().map(str::to_string);
                let agent: pi_subagents::PiSubagent = serde_json::from_value(params).unwrap();
                pi_subagents::save(&self.paths, &agent, original.as_deref())
                    .map_err(cypher_rpc::RpcError::Failed)?;
                serde_json::to_value(pi_subagents::list(&self.paths)).unwrap()
            }
            methods::DELETE_PI_SUBAGENT => {
                params.as_object_mut().unwrap().remove("targetDeviceId");
                pi_subagents::delete(&self.paths, params["name"].as_str().unwrap())
                    .map_err(cypher_rpc::RpcError::Failed)?;
                serde_json::to_value(pi_subagents::list(&self.paths)).unwrap()
            }
            other => return Err(cypher_rpc::RpcError::UnknownMethod(other.into())),
        };
        Ok(cypher_rpc::RpcReply::Value(value))
    }
}

struct Fixture {
    page: Entity<SubagentsPage>,
    paths: PiRuntimePaths,
    target: Entity<DeviceTarget>,
    _data: tempfile::TempDir,
    _runtime: tokio::runtime::Runtime,
}

fn start(cx: &mut gpui::TestAppContext, builtin: &str) -> (Fixture, gpui::VisualTestContext) {
    cx.background_executor.allow_parking();
    let data = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let current = data.path().join("current");
    let paths = PiRuntimePaths {
        root: data.path().into(),
        current: current.clone(),
        executable: current.join("bin/pi"),
        npm_executable: current.join("bin/npm"),
        package_dir: current.join("pi"),
        agent_dir: data.path().join("agent"),
    };
    std::fs::create_dir_all(pi_subagents::builtin_dir(&paths)).unwrap();
    std::fs::write(
        pi_subagents::builtin_dir(&paths).join("reviewer.md"),
        builtin,
    )
    .unwrap();

    let fixture = Arc::new(SubagentFixture {
        paths: paths.clone(),
    });
    let engine_dir = data.path().join("ui");
    std::fs::create_dir_all(&engine_dir).unwrap();
    std::fs::write(engine_dir.join("device-id"), "viewer").unwrap();
    let port = cypher_env::ipc_socket(&engine_dir).unwrap();
    let listener = runtime
        .block_on(cypher_rpc::LocalListener::bind(&port))
        .unwrap();
    runtime.spawn(listener.serve(fixture.clone()));

    let state = cx.update(|cx| {
        gpui_tokio::init(cx);
        cx.set_global(Theme::for_appearance(crate::kit::theme::Appearance::Dark));
        crate::composer::init(cx);
        let state = cx.new(|_| AppState::new());
        AppState::bootstrap(
            state.clone(),
            data.path().join("preferences"),
            crate::state::EngineBootConfig {
                data_dir: engine_dir.clone(),
                ipc_socket: port.clone(),
                edge_url: "http://127.0.0.1:1".into(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: cypher_proto::HarnessId::Mock,
            },
            cx,
        );
        state
    });
    pump_until(cx, || cx.update(|cx| state.read(cx).engine().is_some()));

    let target = cx.update(|cx| {
        state.update(cx, |state, _| {
            state.devices.push(
                serde_json::from_value(serde_json::json!({
                    "id":"agent-host", "name":"Remote host", "platform":"linux",
                    "lastSeenAt": chrono::Utc::now()
                }))
                .unwrap(),
            )
        });
        let target = cx.new(|cx| DeviceTarget::new(state.clone(), cx));
        target.update(cx, |t, cx| t.select(Some("agent-host".into()), cx).unwrap());
        target
    });

    let window = cx.open_window(gpui::size(px(1100.0), px(1600.0)), |_, cx| {
        SubagentsPage::new(state, target.clone(), cx)
    });
    let page = window.root(cx).unwrap();
    pump_until(cx, || {
        cx.update(|cx| {
            matches!(page.read(cx).agents, Loadable::Ready(_))
                && matches!(page.read(cx).models, Loadable::Ready(_))
        })
    });
    let visual = gpui::VisualTestContext::from_window(window.into(), cx);
    (
        Fixture {
            page,
            paths,
            target,
            _data: data,
            _runtime: runtime,
        },
        visual,
    )
}

fn draw(visual: &mut gpui::VisualTestContext) {
    visual.update(|w, cx| {
        w.refresh();
        w.draw(cx).clear();
    });
}

#[gpui::test]
fn typed_controls_write_catalog_values(cx: &mut gpui::TestAppContext) {
    let (f, mut visual) = start(
        cx,
        "---\nname: reviewer\ndescription: Verify completed work\ntools: read, grep\n---\nYou verify.\n",
    );
    let page = f.page.clone();

    draw(&mut visual);
    let new_button = visual.debug_bounds("subagent-new").unwrap();
    visual.simulate_click(new_button.center(), Default::default());
    page.update(cx, |page, cx| {
        let editor = page.editor.as_ref().expect("editor opens");
        editor.name.update(cx, |v, cx| v.set_text("planner", cx));
        editor
            .description
            .update(cx, |v, cx| v.set_text("Resolve a design decision", cx));
        editor.prompt.update(cx, |v, cx| {
            v.set_text("You are the planner.\n\nSecond line.", cx)
        });
        cx.notify();
    });

    // Model: the picker offers the device's catalog, and the trigger shows
    // the human label rather than the provider/id string.
    page.update(cx, |page, cx| {
        page.set_model(Some("claude-bridge/claude-fable-5-1".into()), cx);
        let editor = page.editor.as_ref().unwrap();
        assert_eq!(page.model_label(editor), "Fable 5.1");
        // That model has a ladder, so levels are offered.
        assert_eq!(page.available_levels(editor).len(), THINKING_LEVELS.len());
        page.editor.as_mut().unwrap().thinking = Some("xhigh".into());
    });

    // Tools: built-in toggles plus a free-form extension tool.
    page.update(cx, |page, cx| {
        for tool in ["read", "grep", "bash"] {
            page.toggle_tool(tool.into(), cx);
        }
        // Toggling twice clears it again.
        page.toggle_tool("bash".into(), cx);
        page.editor
            .as_ref()
            .unwrap()
            .tool_entry
            .update(cx, |v, cx| v.set_text("web_search, ask_user", cx));
        page.add_custom_tool(cx);
        let editor = page.editor.as_ref().unwrap();
        assert_eq!(editor.tools, ["read", "grep", "web_search", "ask_user"]);
        assert_eq!(editor.custom_tools(), ["web_search", "ask_user"]);
        // The entry box clears after adding.
        assert!(editor.tool_entry.read(cx).text().is_empty());
    });

    draw(&mut visual);
    let save = visual.debug_bounds("subagent-save").unwrap();
    visual.simulate_click(save.center(), Default::default());
    pump_until(cx, || cx.update(|cx| page.read(cx).editor.is_none()));

    let written =
        std::fs::read_to_string(pi_subagents::user_dir(&f.paths).join("planner.md")).unwrap();
    assert!(
        written.contains("model: claude-bridge/claude-fable-5-1"),
        "{written}"
    );
    assert!(written.contains("thinking: xhigh"), "{written}");
    assert!(
        written.contains("tools: read, grep, web_search, ask_user"),
        "{written}"
    );
}

/// Escape peels one layer per press: the model dropdown, then the dialog.
/// A form holding a freshly typed prompt must not vanish because someone
/// dismissed a menu.
#[gpui::test]
fn escape_closes_the_dropdown_before_the_dialog(cx: &mut gpui::TestAppContext) {
    let (f, mut visual) = start(cx, "---\nname: reviewer\ndescription: Verify\n---\nBody\n");
    let page = f.page.clone();
    draw(&mut visual);

    visual.update(|w, cx| {
        page.update(cx, |page, cx| page.open_editor(None, w, cx));
    });
    page.update(cx, |page, cx| {
        page.editor.as_mut().unwrap().model_menu_open = true;
        cx.notify();
    });

    let escape = || gpui::KeyDownEvent {
        keystroke: gpui::Keystroke::parse("escape").unwrap(),
        is_held: false,
        prefer_character_input: false,
    };
    // First press: only the dropdown closes.
    visual.update(|w, cx| {
        page.update(cx, |page, cx| page.on_dialog_key(&escape(), w, cx));
    });
    cx.update(|cx| {
        let page = page.read(cx);
        let editor = page.editor.as_ref().expect("dialog stays open");
        assert!(!editor.model_menu_open, "dropdown closed");
    });
    // Second press: the dialog goes.
    visual.update(|w, cx| {
        page.update(cx, |page, cx| page.on_dialog_key(&escape(), w, cx));
    });
    cx.update(|cx| assert!(page.read(cx).editor.is_none()));

    // The same key closes a confirmation outright.
    let agent = cx.update(|cx| page.read(cx).agents.ready().unwrap()[0].clone());
    page.update(cx, |page, cx| page.request_delete(&agent, cx));
    cx.update(|cx| assert!(page.read(cx).delete.is_some()));
    visual.update(|w, cx| {
        page.update(cx, |page, cx| page.on_dialog_key(&escape(), w, cx));
    });
    cx.update(|cx| assert!(page.read(cx).delete.is_none()));
}

#[gpui::test]
fn a_model_without_reasoning_drops_the_thinking_level(cx: &mut gpui::TestAppContext) {
    let (f, mut visual) = start(
        cx,
        "---\nname: reviewer\ndescription: Verify\nmodel: claude-bridge/claude-fable-5-1\nthinking: max\n---\nBody\n",
    );
    let page = f.page.clone();
    draw(&mut visual);

    let builtin = cx.update(|cx| {
        page.read(cx)
            .agents
            .ready()
            .unwrap()
            .iter()
            .find(|a| a.name == "reviewer")
            .cloned()
            .unwrap()
    });
    visual.update(|w, cx| {
        page.update(cx, |page, cx| page.open_editor(Some(builtin), w, cx));
    });
    cx.update(|cx| {
        let page = page.read(cx);
        let editor = page.editor.as_ref().unwrap();
        assert_eq!(editor.thinking.as_deref(), Some("max"));
        assert!(!page.available_levels(editor).is_empty());
    });

    // A model with a narrower ladder offers only its own levels (plus
    // Pi's `off`), never the tiers Pi would clamp away.
    page.update(cx, |page, cx| {
        page.set_model(Some("claude-bridge/claude-opus-4-6".into()), cx);
        let editor = page.editor.as_ref().unwrap();
        assert_eq!(
            page.available_levels(editor),
            ["off", "minimal", "low", "medium", "high", "max"]
        );
    });

    // Switching to a model with no reasoning ladder clears the level and
    // stops offering one, instead of writing a setting Pi ignores.
    page.update(cx, |page, cx| {
        page.set_model(Some("openai/gpt-tiny".into()), cx);
        let editor = page.editor.as_ref().unwrap();
        assert_eq!(editor.thinking, None);
        assert!(page.available_levels(editor).is_empty());
    });

    // Inheriting the model again restores the full ladder.
    page.update(cx, |page, cx| {
        page.set_model(None, cx);
        let editor = page.editor.as_ref().unwrap();
        assert_eq!(page.available_levels(editor).len(), THINKING_LEVELS.len());
    });
}

#[gpui::test]
fn an_unknown_model_survives_being_opened_and_saved(cx: &mut gpui::TestAppContext) {
    let (f, mut visual) = start(
        cx,
        "---\nname: reviewer\ndescription: Verify\nmodel: offline/ghost-model\nthinking: ludicrous\n---\nBody\n",
    );
    let page = f.page.clone();
    draw(&mut visual);

    let builtin = cx.update(|cx| {
        page.read(cx)
            .agents
            .ready()
            .unwrap()
            .iter()
            .find(|a| a.name == "reviewer")
            .cloned()
            .unwrap()
    });
    visual.update(|w, cx| {
        page.update(cx, |page, cx| page.open_editor(Some(builtin), w, cx));
    });
    cx.update(|cx| {
        let page = page.read(cx);
        let editor = page.editor.as_ref().unwrap();
        // Not in the catalog, so the raw id is shown rather than a label.
        assert_eq!(page.model_label(editor), "offline/ghost-model");
        assert!(page.model_entry("offline/ghost-model").is_none());
    });

    page.update(cx, |page, cx| page.save(cx));
    pump_until(cx, || cx.update(|cx| page.read(cx).editor.is_none()));

    let written =
        std::fs::read_to_string(pi_subagents::user_dir(&f.paths).join("reviewer.md")).unwrap();
    assert!(written.contains("model: offline/ghost-model"), "{written}");
    assert!(written.contains("thinking: ludicrous"), "{written}");
}

#[gpui::test]
fn builtins_are_customized_reset_and_the_editor_drops_on_device_switch(
    cx: &mut gpui::TestAppContext,
) {
    let (f, mut visual) = start(
        cx,
        "---\nname: reviewer\ndescription: Verify completed work\ntools: read, grep\n---\nYou verify.\n",
    );
    let page = f.page.clone();

    cx.update(|cx| {
        let agents = page.read(cx).agents.ready().unwrap().clone();
        assert_eq!(agents.len(), 1);
        assert!(agents[0].builtin);
    });

    let builtin = cx.update(|cx| page.read(cx).agents.ready().unwrap()[0].clone());
    visual.update(|w, cx| {
        page.update(cx, |page, cx| page.open_editor(Some(builtin), w, cx));
    });
    page.update(cx, |page, cx| {
        let editor = page.editor.as_ref().unwrap();
        assert!(editor.from_builtin);
        // The built-in's tools arrive as selected chips, not as text.
        assert_eq!(editor.tools, ["read", "grep"]);
        editor
            .description
            .update(cx, |v, cx| v.set_text("My stricter reviewer", cx));
        page.save(cx);
    });
    pump_until(cx, || cx.update(|cx| page.read(cx).editor.is_none()));
    cx.update(|cx| {
        let reviewer = page.read(cx).agents.ready().unwrap()[0].clone();
        assert!(!reviewer.builtin && reviewer.overrides_builtin);
        assert_eq!(reviewer.description, "My stricter reviewer");
    });
    assert!(
        std::fs::read_to_string(pi_subagents::builtin_dir(&f.paths).join("reviewer.md"))
            .unwrap()
            .contains("Verify completed work"),
        "the shipped built-in must not be edited"
    );

    let override_row = cx.update(|cx| page.read(cx).agents.ready().unwrap()[0].clone());
    page.update(cx, |page, cx| page.request_delete(&override_row, cx));
    cx.update(|cx| assert!(page.read(cx).delete.as_ref().unwrap().restores_builtin));
    draw(&mut visual);
    let confirm = visual.debug_bounds("subagent-delete-confirm").unwrap();
    visual.simulate_click(confirm.center(), Default::default());
    pump_until(cx, || cx.update(|cx| page.read(cx).delete.is_none()));
    cx.update(|cx| {
        let reviewer = page.read(cx).agents.ready().unwrap()[0].clone();
        assert!(reviewer.builtin, "the built-in returns");
        assert_eq!(reviewer.description, "Verify completed work");
    });

    visual.update(|w, cx| {
        page.update(cx, |page, cx| page.open_editor(None, w, cx));
    });
    cx.update(|cx| assert!(page.read(cx).editor.is_some()));
    f.target.update(cx, |t, cx| t.select(None, cx).unwrap());
    cx.run_until_parked();
    cx.update(|cx| assert!(page.read(cx).editor.is_none()));
}
