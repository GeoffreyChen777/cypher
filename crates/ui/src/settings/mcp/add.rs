use super::*;
use crate::composer::{ComposerInput, ComposerInputEvent};
use gpui::{Focusable, KeyDownEvent};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Http,
    Stdio,
    Json,
}
#[derive(Clone, Copy, PartialEq)]
enum Auth {
    None,
    Bearer,
    Oauth,
}

pub(super) struct AddForm {
    mode: Mode,
    auth: Auth,
    ticket: super::super::device_target::DeviceTicket,
    name: Entity<ComposerInput>,
    endpoint: Entity<ComposerInput>,
    token: Entity<ComposerInput>,
    args: Entity<ComposerInput>,
    env: Entity<ComposerInput>,
    cwd: Entity<ComposerInput>,
    headers: Entity<ComposerInput>,
    json: Entity<ComposerInput>,
    _events: Vec<Subscription>,
}

fn parse_json(text: &str) -> Result<serde_json::Value, String> {
    if text.len() > 65_536 {
        return Err("JSON input must not exceed 64 KiB.".into());
    }
    serde_json::from_str(text).map_err(|_| {
        "Invalid JSON. Check quotes, commas and brackets; the input has not been saved.".into()
    })
}

fn import_servers(name: &str, text: &str) -> Result<BTreeMap<String, serde_json::Value>, String> {
    let mut value = parse_json(text)?;
    let object = value
        .as_object_mut()
        .ok_or("Paste a JSON object containing MCP server configuration.")?;
    if object.contains_key("command") || object.contains_key("url") {
        return Ok(BTreeMap::from([(name.trim().into(), value)]));
    }
    let servers = if object.contains_key("mcpServers") {
        if object.len() != 1 {
            return Err("Import only the mcpServers object; global settings and imports are not changed here.".into());
        }
        object.remove("mcpServers").unwrap()
    } else {
        value
    };
    serde_json::from_value(servers)
        .map_err(|_| "mcpServers must be an object mapping server names to configurations.".into())
}

impl McpPage {
    pub(super) fn open_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.target.read(cx).can_write(cx) || self.busy.is_some() {
            return;
        }
        self.set_add_mode(Mode::Http, cx);
        if let Some(form) = &self.form {
            form.name.focus_handle(cx).focus(window, cx);
        }
    }

    fn set_add_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Ok(ticket) = self.target.read(cx).ticket(cx) else {
            return;
        };
        let name = cx.new(|cx| ComposerInput::settings_field("e.g. docs-server", false, cx));
        let endpoint = cx.new(|cx| {
            ComposerInput::settings_field(
                if mode == Mode::Stdio {
                    "Executable, e.g. npx or /absolute/path/server"
                } else {
                    "https://example.com/mcp"
                },
                false,
                cx,
            )
        });
        let token = cx.new(|cx| ComposerInput::settings_field("Bearer token", true, cx));
        let args =
            cx.new(|cx| ComposerInput::settings_field("[\"-y\", \"your-mcp-package\"]", true, cx));
        let env = cx.new(|cx| ComposerInput::settings_field("{\"API_KEY\": \"…\"}", true, cx));
        let cwd = cx.new(|cx| {
            ComposerInput::settings_field(
                "Optional absolute working directory on the selected host",
                false,
                cx,
            )
        });
        let headers =
            cx.new(|cx| ComposerInput::settings_field("{\"X-API-Key\": \"…\"}", true, cx));
        let json = cx.new(|cx| {
            ComposerInput::settings_field(
                "Paste MCP JSON here (masked to protect credentials)",
                true,
                cx,
            )
        });
        let mut events = Vec::new();
        for input in [&name, &endpoint, &token, &args, &env, &cwd, &headers, &json] {
            events.push(
                cx.subscribe(input, |page: &mut Self, _, event, cx| match event {
                    ComposerInputEvent::Submitted => page.save_add(cx),
                    ComposerInputEvent::Edited => {
                        page.error = None;
                        cx.notify();
                    }
                    _ => {}
                }),
            );
        }
        self.form = Some(AddForm {
            mode,
            auth: Auth::Oauth,
            ticket,
            name,
            endpoint,
            token,
            args,
            env,
            cwd,
            headers,
            json,
            _events: events,
        });
        self.error = None;
        self.notice = None;
        cx.notify();
    }

    fn add_request(&self, cx: &gpui::App) -> Result<serde_json::Value, String> {
        let form = self.form.as_ref().ok_or("Open Add MCP first.")?;
        if !self.target.read(cx).matches(&form.ticket) {
            return Err(
                "The selected device changed. Reopen Add MCP on the intended device.".into(),
            );
        }
        let name = form.name.read(cx).text().trim();
        let servers = if form.mode == Mode::Json {
            import_servers(name, form.json.read(cx).text())?
        } else {
            let mut entry = serde_json::Map::new();
            entry.insert(
                if form.mode == Mode::Http {
                    "url"
                } else {
                    "command"
                }
                .into(),
                form.endpoint.read(cx).text().trim().into(),
            );
            if form.mode == Mode::Http {
                match form.auth {
                    Auth::None => {
                        entry.insert("auth".into(), false.into());
                    }
                    Auth::Oauth => {
                        entry.insert("auth".into(), "oauth".into());
                    }
                    Auth::Bearer => {
                        if form.token.read(cx).text().trim().is_empty() {
                            return Err(
                                "Enter a bearer token or choose another authentication method."
                                    .into(),
                            );
                        }
                        entry.insert("auth".into(), "bearer".into());
                        entry.insert("bearerToken".into(), form.token.read(cx).text().into());
                    }
                }
                let headers = form.headers.read(cx).text();
                if !headers.trim().is_empty() {
                    entry.insert("headers".into(), parse_json(headers)?);
                }
            } else {
                for (key, input) in [("args", &form.args), ("env", &form.env)] {
                    let text = input.read(cx).text();
                    if !text.trim().is_empty() {
                        entry.insert(key.into(), parse_json(text)?);
                    }
                }
                let cwd = form.cwd.read(cx).text().trim();
                if !cwd.is_empty() {
                    entry.insert("cwd".into(), cwd.into());
                }
            }
            BTreeMap::from([(name.into(), serde_json::Value::Object(entry))])
        };
        let request = cypher_engine::mcp::AddMcpServers { servers };
        request.validate()?;
        if let Loadable::Ready(snapshot) = &self.snapshot
            && snapshot
                .servers
                .iter()
                .any(|s| request.servers.contains_key(&s.name))
        {
            return Err(
                "A server with this name already exists. Existing servers will not be overwritten."
                    .into(),
            );
        }
        Ok(serde_json::json!({ "servers": request.servers }))
    }

    fn save_add(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        match self.add_request(cx) {
            Ok(request) => self.mutate(methods::ADD_MCP_SERVERS, "$add".into(), request, cx),
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    pub(super) fn render_add_form(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let form = self.form.as_ref().unwrap();
        if self.busy.is_some() {
            return widgets::section_card(theme)
                .mt(px(16.0))
                .p(px(20.0))
                .child(SharedString::from(format!(
                    "Saving MCP configuration to {}…",
                    form.ticket.label
                )))
                .into_any_element();
        }
        let mode = form.mode;
        let auth = form.auth;
        let label = form.ticket.label.clone();
        let mut fields = div().flex().flex_col().gap(px(14.0));
        let mut tabs = div().flex().gap(px(8.0));
        for (ix, candidate, text) in [
            (0, Mode::Http, "HTTP"),
            (1, Mode::Stdio, "stdio"),
            (2, Mode::Json, "Import JSON"),
        ] {
            tabs = tabs.child(
                widgets::ghost_action(theme)
                    .id(("mcp-add-mode", ix as usize))
                    .text_color(theme.text)
                    .when(candidate == mode, |el| el.bg(theme.element_active))
                    .child(text)
                    .on_click(cx.listener(move |page, _, _, cx| page.set_add_mode(candidate, cx))),
            );
        }
        fields = fields.child(tabs);
        let input_row = |label: &'static str, input: &Entity<ComposerInput>| {
            div()
                .flex()
                .flex_col()
                .gap(px(5.0))
                .child(widgets::field_label(theme, label))
                .child(
                    div()
                        .w_full()
                        .px(px(10.0))
                        .py(px(8.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border_strong)
                        .bg(theme.input_glass_bg())
                        .child(input.clone()),
                )
        };
        fields = fields.child(input_row(
            if mode == Mode::Json {
                "Name (only for a single unnamed server)"
            } else {
                "Server name"
            },
            &form.name,
        ));
        if mode == Mode::Json {
            fields = fields.child(input_row("MCP JSON (masked)", &form.json))
                .child(widgets::page_subtitle(theme, "Paste {\"mcpServers\": {…}}, a named server map, or one server object with a name above. Up to 32 servers; existing names are never overwritten."));
            if !form.json.read(cx).text().trim().is_empty()
                && let Ok(request) = self.add_request(cx)
                && let Some(servers) = request["servers"].as_object()
            {
                fields = fields.child(widgets::page_subtitle(
                    theme,
                    format!(
                        "Ready to add: {}",
                        servers.keys().cloned().collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
        } else {
            fields = fields.child(input_row(
                if mode == Mode::Http {
                    "MCP URL"
                } else {
                    "Executable"
                },
                &form.endpoint,
            ));
            if mode == Mode::Http {
                let mut choices = div().flex().gap(px(8.0));
                for (ix, candidate, text) in [
                    (0, Auth::Oauth, "OAuth"),
                    (1, Auth::Bearer, "Bearer token"),
                    (2, Auth::None, "No auth"),
                ] {
                    choices = choices.child(
                        widgets::ghost_action(theme)
                            .id(("mcp-add-auth", ix as usize))
                            .text_color(theme.text)
                            .when(candidate == auth, |el| el.bg(theme.element_active))
                            .child(text)
                            .on_click(cx.listener(move |page, _, _, cx| {
                                if let Some(form) = &mut page.form {
                                    form.auth = candidate;
                                    form.token.update(cx, |input, cx| input.set_text("", cx));
                                }
                                cx.notify();
                            })),
                    );
                }
                fields = fields.child(choices);
                if auth == Auth::Bearer {
                    fields = fields.child(input_row("Bearer token (masked)", &form.token));
                }
                fields = fields.child(input_row("Headers JSON (optional, masked)", &form.headers))
                    .child(widgets::page_subtitle(theme, "HTTPS required except for loopback. After saving an OAuth server, use Sign in in the server list."));
            } else {
                fields = fields
                    .child(input_row(
                        "Arguments JSON array (optional, masked)",
                        &form.args,
                    ))
                    .child(input_row(
                        "Environment JSON object (optional, masked)",
                        &form.env,
                    ))
                    .child(input_row("Working directory (optional)", &form.cwd));
            }
        }
        fields = fields.child(widgets::page_subtitle(theme,
            format!("Target: {label}. Only add trusted servers: commands and secret helpers may execute when Pi loads the configuration, including model/tool discovery. Saving does not verify connectivity. Switching input mode clears the form.")))
            .child(div().flex().justify_end().gap(px(8.0))
                .child(widgets::ghost_action(theme).id("mcp-add-cancel").text_color(theme.text)
                    .child("Cancel").on_click(cx.listener(|page, _, _, cx| {
                        page.form = None; page.error = None; cx.notify();
                    })))
                .child(popover::btn_primary(theme, if mode == Mode::Json { "Import servers" } else { "Add server" })
                    .id("mcp-add-save").debug_selector(|| "mcp-add-save".into())
                    .on_click(cx.listener(|page, _, _, cx| page.save_add(cx)))));
        widgets::section_card(theme)
            .id("mcp-add-form")
            .mt(px(16.0))
            .p(px(20.0))
            .on_key_down(cx.listener(|page, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && page.busy.is_none() {
                    page.form = None;
                    page.error = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(fields)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::setup::tests::pump_until;
    use gpui::AppContext;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn mcp_json_import_accepts_common_shapes_and_rejects_global_settings() {
        for text in [
            r#"{"mcpServers":{"docs":{"url":"https://example.com/mcp"}}}"#,
            r#"{"docs":{"url":"https://example.com/mcp"}}"#,
        ] {
            let servers = import_servers("", text).unwrap();
            assert_eq!(servers.len(), 1);
            cypher_engine::mcp::AddMcpServers { servers }
                .validate()
                .unwrap();
        }
        assert_eq!(
            import_servers("local", r#"{"command":"node","args":["server.js"]}"#).unwrap()["local"]
                ["command"],
            "node"
        );
        for text in [
            r#"{"mcpServers":{},"settings":{"secret":"fixture-key"}}"#,
            "fixture-key not JSON",
            "[]",
        ] {
            assert!(
                !import_servers("", text)
                    .unwrap_err()
                    .contains("fixture-key")
            );
        }
        assert!(parse_json(&"x".repeat(65_537)).is_err());
    }

    struct McpFixture {
        agent: std::path::PathBuf,
        additions: AtomicUsize,
        removals: AtomicUsize,
        release: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl cypher_rpc::RpcService for McpFixture {
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
                methods::LIST_MCP_SERVERS => {
                    serde_json::to_value(cypher_engine::mcp::list(&self.agent)).unwrap()
                }
                methods::ADD_MCP_SERVERS => {
                    assert_eq!(params["targetDeviceId"], "mcp-host");
                    params.as_object_mut().unwrap().remove("targetDeviceId");
                    let request =
                        serde_json::from_value::<cypher_engine::mcp::AddMcpServers>(params)
                            .unwrap();
                    request.validate().unwrap();
                    self.additions.fetch_add(1, Ordering::SeqCst);
                    self.release.notified().await;
                    serde_json::to_value(
                        cypher_engine::mcp::add_servers(&self.agent, request)
                            .map_err(cypher_rpc::RpcError::Failed)?,
                    )
                    .unwrap()
                }
                methods::REMOVE_MCP_SERVER => {
                    assert_eq!(params["targetDeviceId"], "mcp-host");
                    params.as_object_mut().unwrap().remove("targetDeviceId");
                    let request =
                        serde_json::from_value::<cypher_engine::mcp::RemoveMcpServer>(params)
                            .unwrap();
                    self.removals.fetch_add(1, Ordering::SeqCst);
                    self.release.notified().await;
                    serde_json::to_value(
                        cypher_engine::mcp::remove_server(&self.agent, request)
                            .map_err(cypher_rpc::RpcError::Failed)?,
                    )
                    .unwrap()
                }
                other => return Err(cypher_rpc::RpcError::UnknownMethod(other.into())),
            };
            Ok(cypher_rpc::RpcReply::Value(value))
        }
    }

    #[gpui::test]
    fn mcp_add_form_and_json_import_save_to_the_selected_device(cx: &mut gpui::TestAppContext) {
        cx.background_executor.allow_parking();
        let data = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let fixture = Arc::new(McpFixture {
            agent: data.path().join("agent"),
            additions: AtomicUsize::new(0),
            removals: AtomicUsize::new(0),
            release: tokio::sync::Notify::new(),
        });
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        runtime.spawn(cypher_rpc::serve_ws_listener(listener, fixture.clone()));
        let state = cx.update(|cx| {
            gpui_tokio::init(cx);
            cx.set_global(Theme::for_appearance(crate::theme::Appearance::Dark));
            crate::composer::init(cx);
            let state = cx.new(|_| AppState::new());
            AppState::bootstrap(
                state.clone(),
                crate::state::EngineBootConfig {
                    data_dir: data.path().join("ui"),
                    ipc_port: port,
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
            state.update(cx, |state, _| state.devices.push(serde_json::from_value(serde_json::json!({
                "id":"mcp-host", "name":"Remote MCP host", "platform":"linux", "lastSeenAt":chrono::Utc::now()
            })).unwrap()));
            let target = cx.new(|cx| super::super::super::device_target::DeviceTarget::new(state.clone(), cx));
            target.update(cx, |t,cx| t.select(Some("mcp-host".into()),cx).unwrap());
            target
        });
        let window = cx.open_window(gpui::size(px(1100.0), px(1200.0)), |_, cx| {
            McpPage::new(state, target.clone(), cx)
        });
        let page = window.root(cx).unwrap();
        pump_until(cx, || {
            cx.update(|cx| matches!(page.read(cx).snapshot, Loadable::Ready(_)))
        });
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|w, cx| {
            w.refresh();
            w.draw(cx).clear();
        });
        let add_button = visual.debug_bounds("mcp-add").unwrap();
        visual.simulate_click(add_button.center(), Default::default());
        page.update(cx, |page, cx| {
            let form = page.form.as_mut().unwrap();
            form.auth = Auth::Bearer;
            form.name.update(cx, |v, cx| v.set_text("web", cx));
            form.endpoint
                .update(cx, |v, cx| v.set_text("https://example.com/mcp", cx));
            form.token
                .update(cx, |v, cx| v.set_text("fixture-secret", cx));
            cx.notify();
        });
        visual.update(|w, cx| {
            w.refresh();
            w.draw(cx).clear();
        });
        let save = visual.debug_bounds("mcp-add-save").unwrap();
        visual.simulate_click(save.center(), Default::default());
        pump_until(cx, || fixture.additions.load(Ordering::SeqCst) == 1);
        assert!(target.update(cx, |t, cx| t.select(None, cx)).is_err());
        fixture.release.notify_one();
        pump_until(cx, || cx.update(|cx| page.read(cx).form.is_none()));
        cx.update(|cx| {
            let snapshot = page.read(cx).snapshot.ready().unwrap();
            assert_eq!(snapshot.servers[0].name, "web");
            assert!(
                !serde_json::to_string(snapshot)
                    .unwrap()
                    .contains("fixture-secret")
            );
        });
        page.update(cx,|page,cx| {
            page.set_add_mode(Mode::Json,cx);
            page.form.as_ref().unwrap().json.update(cx,|v,cx|v.set_text(
                r#"{"mcpServers":{"tool":{"command":"node","args":["--key","fixture-secret"]},"docs":{"url":"https://example.com/docs","auth":false}}}"#,cx));
            page.save_add(cx);
        });
        pump_until(cx, || fixture.additions.load(Ordering::SeqCst) == 2);
        fixture.release.notify_one();
        pump_until(cx, || cx.update(|cx| page.read(cx).form.is_none()));
        assert_eq!(cypher_engine::mcp::list(&fixture.agent).servers.len(), 3);
        page.update(cx, |page, cx| page.request_delete("web".into(), cx));
        assert_eq!(
            fixture.removals.load(Ordering::SeqCst),
            0,
            "opening confirmation must not delete"
        );
        visual.update(|w, cx| {
            w.refresh();
            w.draw(cx).clear();
        });
        let cancel = visual.debug_bounds("mcp-delete-cancel").unwrap();
        visual.simulate_click(cancel.center(), Default::default());
        cx.update(|cx| assert!(page.read(cx).delete.is_none()));
        assert_eq!(fixture.removals.load(Ordering::SeqCst), 0);
        page.update(cx, |page, cx| page.request_delete("web".into(), cx));
        visual.update(|w, cx| {
            w.refresh();
            w.draw(cx).clear();
        });
        let confirm = visual.debug_bounds("mcp-delete-confirm").unwrap();
        visual.simulate_click(confirm.center(), Default::default());
        pump_until(cx, || fixture.removals.load(Ordering::SeqCst) == 1);
        assert!(target.update(cx, |t, cx| t.select(None, cx)).is_err());
        fixture.release.notify_one();
        pump_until(cx, || cx.update(|cx| page.read(cx).delete.is_none()));
        assert_eq!(cypher_engine::mcp::list(&fixture.agent).servers.len(), 2);
        page.update(cx, |page, cx| page.request_delete("docs".into(), cx));
        target.update(cx, |t, cx| t.select(None, cx).unwrap());
        cx.run_until_parked();
        page.update(cx, |page, cx| {
            assert!(page.delete.is_none());
            page.confirm_delete(cx);
        });
        assert_eq!(fixture.removals.load(Ordering::SeqCst), 1);
        target.update(cx, |t, cx| t.select(Some("mcp-host".into()), cx).unwrap());
        cx.run_until_parked();
        page.update(cx, |page, cx| {
            page.set_add_mode(Mode::Stdio, cx);
            let form = page.form.as_ref().unwrap();
            form.name.update(cx, |v, cx| v.set_text("another-tool", cx));
            form.endpoint.update(cx, |v, cx| v.set_text("node", cx));
            form.args
                .update(cx, |v, cx| v.set_text("[\"server.js\"]", cx));
            form.env
                .update(cx, |v, cx| v.set_text("{\"KEY\":\"fixture-secret\"}", cx));
            let request = page.add_request(cx).unwrap();
            assert_eq!(request["servers"]["another-tool"]["args"][0], "server.js");
            // A device switch drops the form instead of reusing its credentials.
        });
        target.update(cx, |t, cx| t.select(None, cx).unwrap());
        cx.run_until_parked();
        cx.update(|cx| assert!(page.read(cx).form.is_none()));
    }
}
