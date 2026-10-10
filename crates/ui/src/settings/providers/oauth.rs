//! Subscription sign-in: starting the OAuth flow, submitting the pasted
//! code, and the sign-in dialog.

use super::*;

impl ProvidersPage {
    pub(super) fn start_oauth(&mut self, id: &str, cx: &mut Context<Self>) {
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
        self.confirm = None;
        self.error = None;
        self.notice = None;
        self.busy = Some(Busy {
            method: methods::BEGIN_PI_PROVIDER_LOGIN,
            provider: Some(id.to_string()),
        });
        let callback = cx.new(|cx| {
            ComposerInput::settings_field("Paste callback URL or authorization code", false, cx)
        });
        let events = vec![cx.subscribe(&callback, |page: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                page.submit_oauth(cx);
            } else if matches!(
                event,
                ComposerInputEvent::Edited | ComposerInputEvent::CursorMoved
            ) {
                cx.notify();
            }
        })];
        self.oauth = Some(OauthLogin {
            ticket: ticket.clone(),
            status: None,
            callback,
            submitting: false,
            error: None,
            focus_callback: false,
            _events: events,
        });
        let target = self.target.clone();
        let lease = target.update(cx, |target, cx| target.lock(cx));
        let params = ticket.params(serde_json::json!({ "id": id }));
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::BEGIN_PI_PROVIDER_LOGIN, params)
                .await;
            let mut status = match result.and_then(|value| {
                serde_json::from_value::<LoginStatus>(value)
                    .map_err(|_| cypher_rpc::RpcError::Failed("Invalid sign-in response.".into()))
            }) {
                Ok(status) => status,
                Err(_) => {
                    this.update(cx, |page, cx| {
                        if page.target.read(cx).matches(&ticket) {
                            page.oauth = None;
                            page.busy = None;
                            page.error = Some(format!(
                                "Could not start sign-in on {}. Update Cypher on that device and try again.",
                                ticket.label
                            ));
                            cx.notify();
                        }
                    })
                    .ok();
                    drop(lease);
                    target.update(cx, |_, cx| cx.notify());
                    return;
                }
            };
            let attempt = status.attempt_id.clone();
            loop {
                let terminal = matches!(status.phase.as_str(), "succeeded" | "failed" | "cancelled");
                let live = this
                    .update(cx, |page, cx| {
                        let Some(form) = page.oauth.as_mut() else {
                            return false;
                        };
                        if !page.target.read(cx).matches(&ticket)
                            || form.status.as_ref().is_some_and(|s| s.attempt_id != attempt)
                                && form.status.is_some()
                        {
                            return false;
                        }
                        let waiting = status.phase == "awaiting_callback";
                        if waiting
                            && form
                                .status
                                .as_ref()
                                .is_none_or(|s| s.phase != "awaiting_callback")
                        {
                            form.focus_callback = true;
                        }
                        form.status = Some(status.clone());
                        if terminal {
                            page.busy = None;
                            if status.phase == "succeeded" {
                                page.oauth = None;
                                page.notice = Some(format!("Signed in on {}.", ticket.label));
                                page.call(methods::LIST_PI_PROVIDERS, serde_json::json!({}), cx);
                            } else if let Some(error) = &status.error {
                                form.error = Some(error.clone());
                            }
                        }
                        cx.notify();
                        page.oauth.is_some() && !terminal
                    })
                    .unwrap_or(false);
                if !live {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(750))
                    .await;
                match engine
                    .client()
                    .call(
                        methods::PI_PROVIDER_LOGIN_STATUS,
                        ticket.params(serde_json::json!({ "attemptId": attempt })),
                    )
                    .await
                    .and_then(|value| {
                        serde_json::from_value::<LoginStatus>(value)
                            .map_err(|_| cypher_rpc::RpcError::Failed("Invalid sign-in status.".into()))
                    }) {
                    Ok(next) => status = next,
                    Err(_) => {
                        this.update(cx, |page, cx| {
                            if page.target.read(cx).matches(&ticket) {
                                page.oauth = None;
                                page.busy = None;
                                page.error = Some(
                                    "Lost connection during sign-in. The attempt expires after 10 minutes.".into(),
                                );
                                cx.notify();
                            }
                        })
                        .ok();
                        break;
                    }
                }
            }
            let _ = engine
                .client()
                .call(
                    methods::CANCEL_PI_PROVIDER_LOGIN,
                    ticket.params(serde_json::json!({ "attemptId": attempt })),
                )
                .await;
            drop(lease);
            target.update(cx, |_, cx| cx.notify());
        })
        .detach();
        cx.notify();
    }

    fn submit_oauth(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.oauth.as_mut() else {
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
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::COMPLETE_PI_PROVIDER_LOGIN,
                    ticket.params(serde_json::json!({ "attemptId": id, "callbackUrl": callback })),
                )
                .await;
            this.update(cx, |page, cx| {
                if let Some(form) = page.oauth.as_mut() {
                    if form.ticket != ticket
                        || form.status.as_ref().is_none_or(|s| s.attempt_id != id)
                    {
                        return;
                    }
                    form.submitting = false;
                    match result {
                        Ok(value) => {
                            if let Ok(status) = serde_json::from_value(value) {
                                form.status = Some(status);
                            }
                        }
                        Err(_) => {
                            form.error = Some(
                                "Callback rejected or connection lost. Paste the full URL or code for this attempt.".into(),
                            );
                        }
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_oauth(
        &mut self,
        window: &Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let form = self.oauth.as_ref().unwrap();
        let status = form.status.as_ref();
        let waiting = status.is_some_and(|s| s.phase == "awaiting_callback");
        let code = status.and_then(|s| s.user_code.clone());
        let url = status.and_then(|s| s.authorization_url.clone());
        let title = status
            .map(|s| match s.provider_id.as_str() {
                "anthropic" => "Claude",
                "openai-codex" => "ChatGPT",
                other => other,
            })
            .unwrap_or("Sign in");
        let heading = format!("Sign in · {title}");
        let description = status
            .and_then(|s| s.instructions.clone())
            .unwrap_or_else(|| {
                "Open the authorization page on this computer. Paste the callback if the browser cannot reach the selected device.".into()
            });
        let width = (f32::from(window.viewport_size().width) - 40.0).clamp(280.0, 464.0);
        let card = popover::dialog_card(theme)
            .id("provider-oauth-dialog")
            .role(gpui::Role::Dialog)
            .aria_label(title)
            .track_focus(&self.dialog_focus)
            .key_context("ProviderDialog")
            .tab_group()
            .w(px(width))
            .p_0()
            .on_key_down(cx.listener(Self::on_dialog_key))
            .child(self.dialog_heading(theme, &heading, &description, cx));
        let mut body = div()
            .px(px(24.0))
            .py(px(20.0))
            .flex()
            .flex_col()
            .gap(px(12.0));
        if let Some(url) = url.clone() {
            body = body.child(
                button(
                    theme,
                    "provider-oauth-open",
                    "Open authorization page",
                    ButtonStyle::Secondary,
                    true,
                )
                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
            );
        }
        if let Some(code) = code {
            body = body.child(caption(theme, format!("Code: {code}")).text_size(px(16.0)));
        }
        if waiting {
            body = body
                .child(
                    div()
                        .p(px(10.0))
                        .rounded(px(8.0))
                        .bg(theme.input_glass_bg())
                        .child(form.callback.clone()),
                )
                .child(
                    button(
                        theme,
                        "provider-oauth-submit",
                        if form.submitting {
                            "Submitting…"
                        } else {
                            "Complete sign-in"
                        },
                        ButtonStyle::Primary,
                        !form.submitting,
                    )
                    .track_focus(&self.submit_focus)
                    .when(!form.submitting, |el| {
                        el.on_click(cx.listener(|page, _, _, cx| page.submit_oauth(cx)))
                    }),
                );
        }
        if let Some(error) = &form.error {
            body = body.child(widgets::error_strip(theme, error.clone()));
        }
        card.child(body)
            .child(
                div()
                    .px(px(24.0))
                    .pb(px(20.0))
                    .child(
                        button(
                            theme,
                            "provider-oauth-cancel",
                            "Cancel",
                            ButtonStyle::Ghost,
                            true,
                        )
                        .track_focus(&self.cancel_focus)
                        .on_click(cx.listener(|page, _, window, cx| {
                            page.oauth = None;
                            page.busy = None;
                            page.notice =
                                Some("Cancelling sign-in on the selected runtime…".into());
                            page.close_dialog(window, cx);
                        })),
                    )
                    .into_any_element(),
            )
            .into_any_element()
    }
}
