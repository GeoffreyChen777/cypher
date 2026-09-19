use super::super::device_target::DeviceTicket;
use super::*;
use crate::composer::ComposerInput;
use cypher_engine::mcp::login::LoginStatus;

async fn login_call(
    engine: &crate::state::EngineHandle,
    method: &str,
    params: serde_json::Value,
    timer: impl std::future::Future<Output = ()>,
) -> Result<serde_json::Value, cypher_rpc::RpcError> {
    use futures::FutureExt;
    let call = engine.client().call(method, params).fuse();
    let timer = timer.fuse();
    futures::pin_mut!(call, timer);
    futures::select_biased! {
        result = call => result,
        _ = timer => Err(cypher_rpc::RpcError::Failed("MCP login request timed out.".into())),
    }
}

pub(super) struct LoginForm {
    ticket: DeviceTicket,
    status: Option<LoginStatus>,
    callback: Entity<ComposerInput>,
    submitting: bool,
    error: Option<String>,
}

impl McpPage {
    pub(super) fn start_mcp_login(&mut self, name: String, cx: &mut Context<Self>) {
        if self.busy.is_some() || !self.target.read(cx).can_write(cx) {
            return;
        }
        let Ok(ticket) = self.target.read(cx).ticket(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.form = None;
        self.delete = None;
        self.error = None;
        self.notice = None;
        self.busy = Some(name.clone());
        self.load_task = None;
        self.login = Some(LoginForm {
            ticket: ticket.clone(),
            status: None,
            callback: cx.new(|cx| {
                ComposerInput::settings_field("Paste full callback URL (masked)", true, cx)
            }),
            submitting: false,
            error: None,
        });
        let target = self.target.clone();
        let lease = target.update(cx, |t, cx| t.lock(cx));
        // Detached so closing the page still cancels the remote attempt. This
        // task owns the device lease and only ever uses its original ticket.
        cx.spawn(async move |this, cx| {
            let result = login_call(&engine, methods::BEGIN_MCP_LOGIN, ticket.params(serde_json::json!({"name":name})), cx.background_executor().timer(std::time::Duration::from_secs(20))).await;
            let mut status = match result.and_then(|v| serde_json::from_value::<LoginStatus>(v).map_err(|_| cypher_rpc::RpcError::Failed("Invalid MCP login response.".into()))) {
                Ok(status) => status,
                Err(_) => {
                    this.update(cx, |page,cx| {
                        if page.target.read(cx).matches(&ticket) {
                            page.login = None; page.busy = None;
                            page.error = Some(format!("Could not start MCP sign-in on {}. Update both Cypher engines to support interactive MCP login, and check the connection and OAuth configuration.", ticket.label));
                            cx.notify();
                        }
                    }).ok();
                    drop(lease);
                    target.update(cx, |_,cx| cx.notify());
                    return;
                }
            };
            let id = status.attempt_id.clone();
            loop {
                let terminal = matches!(status.phase.as_str(), "succeeded" | "failed" | "cancelled");
                let live = this.update(cx, |page,cx| {
                    if !page.target.read(cx).matches(&ticket) || page.login.as_ref().is_none_or(|f| f.ticket != ticket) { return false; }
                    if terminal {
                        page.login = None; page.busy = None;
                        page.error = status.error.clone();
                        if status.phase == "succeeded" {
                            page.notice = Some(format!("Signed in on {}.", ticket.label));
                            crate::pickers::bump_harness_catalog(cx);
                        }
                        page.load(cx);
                    } else if let Some(form) = &mut page.login { form.status = Some(status.clone()); }
                    cx.notify();
                    true
                }).unwrap_or(false);
                if !live || terminal { break; }
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                match login_call(&engine, methods::MCP_LOGIN_STATUS, ticket.params(serde_json::json!({"attemptId":id})), cx.background_executor().timer(std::time::Duration::from_secs(20))).await
                    .and_then(|v| serde_json::from_value(v).map_err(|_| cypher_rpc::RpcError::Failed("Invalid MCP login status.".into()))) {
                    Ok(s) => status = s,
                    Err(_) => {
                        this.update(cx, |page,cx| {
                            if page.target.read(cx).matches(&ticket) {
                                page.login = None; page.busy = None;
                                page.error = Some("Lost connection during MCP sign-in. The remote attempt expires after 10 minutes.".into());
                                cx.notify();
                            }
                        }).ok();
                        break;
                    }
                }
            }
            // Idempotent, best-effort cleanup, also on navigation/disconnect.
            let _ = login_call(&engine, methods::CANCEL_MCP_LOGIN, ticket.params(serde_json::json!({"attemptId":id})), cx.background_executor().timer(std::time::Duration::from_secs(5))).await;
            this.update(cx, |page,cx| {
                if page.target.read(cx).matches(&ticket) && page.notice.as_deref() == Some("Cancelling sign-in on the selected runtime…") {
                    page.notice = Some("Sign-in cancellation requested.".into());
                    cx.notify();
                }
            }).ok();
            drop(lease);
            target.update(cx, |_,cx| cx.notify());
        }).detach();
        cx.notify();
    }

    fn submit_mcp_callback(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.login else {
            return;
        };
        if form.submitting {
            return;
        }
        let Some(status) = &form.status else {
            return;
        };
        if status.phase != "awaiting_callback" {
            return;
        }
        let callback = form.callback.read(cx).text().trim().to_owned();
        if callback.is_empty() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let id = status.attempt_id.clone();
        let ticket = form.ticket.clone();
        form.callback.update(cx, |input, cx| input.set_text("", cx));
        form.submitting = true;
        form.error = None;
        cx.spawn(async move |this,cx| {
            let result = login_call(&engine, methods::COMPLETE_MCP_LOGIN, ticket.params(serde_json::json!({"attemptId":id,"callbackUrl":callback})), cx.background_executor().timer(std::time::Duration::from_secs(20))).await;
            this.update(cx, |page,cx| {
                if let Some(form) = &mut page.login {
                    if form.ticket != ticket || form.status.as_ref().is_none_or(|s| s.attempt_id != id) { return; }
                    form.submitting = false;
                    match result {
                        Ok(v) => if let Ok(status) = serde_json::from_value(v) { form.status = Some(status); },
                        Err(_) => form.error = Some("Callback rejected or connection lost. Paste the full callback URL for this attempt, or cancel and retry.".into()),
                    }
                    cx.notify();
                }
            }).ok();
        }).detach();
        cx.notify();
    }

    pub(super) fn render_mcp_login(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let form = self.login.as_ref().unwrap();
        let url = form
            .status
            .as_ref()
            .and_then(|s| s.authorization_url.clone());
        let waiting = form
            .status
            .as_ref()
            .is_some_and(|s| s.phase == "awaiting_callback");
        let mut card = widgets::section_card(theme).p(px(16.0)).flex().flex_col().gap(px(12.0))
            .child(widgets::field_label(theme, format!("MCP sign-in · {}",form.ticket.label)))
            .child(widgets::page_subtitle(theme, if waiting {
                "Open the authorization page and approve access. If the browser cannot load localhost, copy its full address anyway and paste it below. The callback belongs to the selected runtime."
            } else { "Waiting for the selected runtime to finish authentication…" }));
        if let Some(url) = url {
            card = card.child(
                widgets::ghost_action(theme)
                    .id("mcp-login-open")
                    .child("Open authorization page")
                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
            );
        }
        if waiting {
            card = card
                .child(
                    div()
                        .p(px(10.0))
                        .rounded(px(8.0))
                        .bg(theme.input_glass_bg())
                        .child(form.callback.clone()),
                )
                .child(
                    widgets::ghost_action(theme)
                        .id("mcp-login-submit")
                        .child(if form.submitting {
                            "Submitting…"
                        } else {
                            "Complete sign-in"
                        })
                        .when(!form.submitting, |el| {
                            el.on_click(cx.listener(|page, _, _, cx| page.submit_mcp_callback(cx)))
                        }),
                );
        }
        if let Some(error) = &form.error {
            card = card.child(widgets::error_strip(theme, error.clone()));
        }
        card.child(
            widgets::ghost_action(theme)
                .id("mcp-login-cancel")
                .child("Cancel sign-in")
                .on_click(cx.listener(|page, _, _, cx| {
                    page.login = None;
                    // Keep busy until the polling task sends cancellation. The
                    // device lease prevents racing a second attempt meanwhile.
                    page.busy = None;
                    page.notice = Some("Cancelling sign-in on the selected runtime…".into());
                    cx.notify();
                })),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::setup::tests::pump_until;
    use gpui::AppContext;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Fixture(Mutex<Vec<(String, serde_json::Value)>>);
    #[async_trait::async_trait]
    impl cypher_rpc::RpcService for Fixture {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<cypher_rpc::RpcReply, cypher_rpc::RpcError> {
            let mut calls = self.0.lock().unwrap();
            calls.push((method.into(), params));
            let completed = calls.iter().any(|(m, _)| m == methods::COMPLETE_MCP_LOGIN);
            let value = match method {
                methods::ENGINE_INFO => {
                    serde_json::json!({"deviceId":"viewer","workspaceScope":"local"})
                }
                methods::ENGINE_READY => serde_json::json!({}),
                methods::LIST_MCP_SERVERS => {
                    serde_json::json!({"adapterInstalled":true,"servers":[]})
                }
                methods::BEGIN_MCP_LOGIN
                | methods::MCP_LOGIN_STATUS
                | methods::COMPLETE_MCP_LOGIN
                | methods::CANCEL_MCP_LOGIN => serde_json::json!({
                    "attemptId":"remote-attempt", "phase":if completed {"succeeded"} else {"awaiting_callback"},
                    "authorizationUrl":if completed { None } else {Some("https://auth.example/authorize?state=fixture")},"error":null
                }),
                _ => return Err(cypher_rpc::RpcError::UnknownMethod(method.into())),
            };
            Ok(cypher_rpc::RpcReply::Value(value))
        }
    }

    #[gpui::test]
    fn remote_mcp_login_keeps_callback_on_original_device_and_clears_input(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.background_executor.allow_parking();
        let data = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let fixture = Arc::new(Fixture::default());
        let dir = data.path().join("engine");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("device-id"), "viewer").unwrap();
        let socket = cypher_env::ipc_socket(&dir).unwrap();
        let listener = runtime
            .block_on(cypher_rpc::LocalListener::bind(&socket))
            .unwrap();
        runtime.spawn(listener.serve(fixture.clone()));
        let state = cx.update(|cx| {
            gpui_tokio::init(cx);
            cx.set_global(Theme::for_appearance(crate::theme::Appearance::Dark));
            crate::composer::init(cx);
            let state = cx.new(|_| AppState::new());
            AppState::bootstrap(
                state.clone(),
                data.path().join("preferences"),
                crate::state::EngineBootConfig {
                    data_dir: dir,
                    ipc_socket: socket,
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
        let (target,page) = cx.update(|cx| {
            state.update(cx, |s,_| s.devices.push(serde_json::from_value(serde_json::json!({"id":"remote","name":"Remote host","platform":"linux","lastSeenAt":chrono::Utc::now()})).unwrap()));
            let target = cx.new(|cx| DeviceTarget::new(state.clone(),cx));
            target.update(cx, |t,cx| t.select(Some("remote".into()),cx).unwrap());
            let page = cx.new(|cx| McpPage::new(state,target.clone(),cx));
            (target,page)
        });
        pump_until(cx, || {
            cx.update(|cx| matches!(page.read(cx).snapshot, Loadable::Ready(_)))
        });
        page.update(cx, |page, cx| page.start_mcp_login("wiki".into(), cx));
        pump_until(cx, || {
            cx.update(|cx| {
                page.read(cx)
                    .login
                    .as_ref()
                    .is_some_and(|f| f.status.is_some())
            })
        });
        cx.update(|cx| assert!(target.read(cx).locked()));
        page.update(cx, |page, cx| {
            page.login.as_ref().unwrap().callback.update(cx, |i, cx| {
                i.set_text(
                    "http://localhost:8976/callback?state=fixture&code=fixture-code",
                    cx,
                )
            });
            page.submit_mcp_callback(cx);
            assert!(
                page.login
                    .as_ref()
                    .unwrap()
                    .callback
                    .read(cx)
                    .text()
                    .is_empty()
            );
        });
        pump_until(cx, || {
            cx.update(|cx| page.read(cx).login.as_ref().is_some_and(|f| !f.submitting))
        });
        cx.background_executor
            .advance_clock(std::time::Duration::from_secs(1));
        pump_until(cx, || {
            cx.update(|cx| page.read(cx).login.is_none() && !target.read(cx).locked())
        });
        cx.update(|cx| {
            assert_eq!(
                page.read(cx).notice.as_deref(),
                Some("Signed in on Remote host.")
            )
        });
        let calls = fixture.0.lock().unwrap();
        for (method, params) in calls.iter().filter(|(m, _)| {
            matches!(
                m.as_str(),
                methods::BEGIN_MCP_LOGIN
                    | methods::MCP_LOGIN_STATUS
                    | methods::COMPLETE_MCP_LOGIN
                    | methods::CANCEL_MCP_LOGIN
            )
        }) {
            assert_eq!(params["targetDeviceId"], "remote", "{method}");
            if method != methods::BEGIN_MCP_LOGIN {
                assert_eq!(params["attemptId"], "remote-attempt");
            }
        }
        assert!(calls.iter().any(|(m, p)| m == methods::COMPLETE_MCP_LOGIN
            && p["callbackUrl"].as_str().unwrap().contains("fixture-code")));
    }
}
