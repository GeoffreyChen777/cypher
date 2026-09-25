//! Settings → GitHub: the selected device's GitHub sign-in.
//!
//! Each device signs in on its own (its engine answers `#` issue lookups for
//! the projects it hosts). Sign-in is GitHub's device flow: the TARGET
//! device's engine asks GitHub for a code and waits for approval; this page
//! shows the code, copies it, and opens the approval page in this machine's
//! browser. The token lands on the target device and never passes through
//! this client.
use std::time::Duration;

use cypher_proto::{
    GithubAccountStatus, GithubCredentialSource, GithubLoginPoll, GithubLoginStart,
    GithubLoginState,
};
use cypher_rpc::methods;
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, Render, SharedString, Subscription, Task, Window,
    div, prelude::*, px,
};

use super::{
    device_target::{DeviceTarget, DeviceTicket},
    widgets,
};
use crate::{
    icons,
    popover::Loadable,
    state::AppState,
    theme::{MonoStyled, Theme},
};

/// How often the page asks the device whether GitHub approved the code (the
/// device itself polls GitHub at GitHub's pace).
const LOGIN_POLL: Duration = Duration::from_secs(2);

/// How long "Copied" stays up after copying the code.
const COPIED_FLASH: Duration = Duration::from_millis(1600);

/// Where approvals and grants live on github.com.
const AUTHORIZATIONS_URL: &str = "https://github.com/settings/apps/authorizations";

struct LoginFlow {
    ticket: DeviceTicket,
    start: GithubLoginStart,
    error: Option<String>,
    copied: bool,
    /// A copy made this recently shows "Copied" on the code and the button.
    copied_flash: Option<std::time::Instant>,
}

pub struct GithubPage {
    state: Entity<AppState>,
    target: Entity<DeviceTarget>,
    generation: u64,
    status: Loadable<GithubAccountStatus>,
    login: Option<LoginFlow>,
    busy: bool,
    error: Option<String>,
    task: Option<Task<()>>,
    poll_task: Option<Task<()>>,
    flash_task: Option<Task<()>>,
    _target_observer: Subscription,
}

fn source_label(source: GithubCredentialSource) -> &'static str {
    match source {
        GithubCredentialSource::Cypher => "Cypher",
        GithubCredentialSource::GhCli => "the GitHub CLI (gh) login",
        GithubCredentialSource::GitCredential => "git's saved GitHub credential",
    }
}

impl GithubPage {
    pub fn new(
        state: Entity<AppState>,
        target: Entity<DeviceTarget>,
        cx: &mut Context<Self>,
    ) -> Self {
        let generation = target.read(cx).generation();
        let observer = cx.observe(&target, |page: &mut Self, target, cx| {
            let generation = target.read(cx).generation();
            if generation != page.generation {
                page.generation = generation;
                page.cancel_login(cx);
                page.busy = false;
                page.error = None;
                page.load(cx);
            }
            cx.notify();
        });
        let mut page = Self {
            state,
            target,
            generation,
            status: Loadable::Idle,
            login: None,
            busy: false,
            error: None,
            task: None,
            poll_task: None,
            flash_task: None,
            _target_observer: observer,
        };
        page.load(cx);
        page
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.status = Loadable::Loading;
        let ticket = match self.target.read(cx).ticket(cx) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.status = Loadable::Error(error);
                cx.notify();
                return;
            }
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::GITHUB_ACCOUNT_STATUS,
                    ticket.params(serde_json::json!({})),
                )
                .await;
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.status = match result {
                    Ok(value) => match serde_json::from_value(value) {
                        Ok(status) => Loadable::Ready(status),
                        Err(error) => Loadable::Error(error.to_string()),
                    },
                    Err(error) => Loadable::Error(format!(
                        "Couldn't read the GitHub sign-in on {}. Update Cypher on that device if it doesn't support GitHub yet. {error}",
                        ticket.label
                    )),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn sign_in(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Ok(ticket) = self.target.read(cx).ticket(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.cancel_login(cx);
        self.busy = true;
        self.error = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::START_GITHUB_LOGIN,
                    ticket.params(serde_json::json!({})),
                )
                .await
                .and_then(|value| {
                    serde_json::from_value::<GithubLoginStart>(value)
                        .map_err(|e| cypher_rpc::RpcError::Failed(e.to_string()))
                });
            this.update(cx, |page, cx| {
                page.busy = false;
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                match result {
                    Ok(start) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(start.user_code.clone()));
                        cx.open_url(&start.verification_uri);
                        page.login = Some(LoginFlow {
                            ticket,
                            start,
                            error: None,
                            copied: true,
                            copied_flash: None,
                        });
                        page.spawn_poll(cx);
                    }
                    Err(err) => page.error = Some(format!("Couldn't start the sign-in: {err}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn spawn_poll(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = &self.login else { return };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let ticket = flow.ticket.clone();
        let login_id = flow.start.login_id.clone();
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(LOGIN_POLL).await;
                let result = engine
                    .client()
                    .call(
                        methods::POLL_GITHUB_LOGIN,
                        ticket.params(serde_json::json!({ "loginId": login_id })),
                    )
                    .await;
                let settled = this
                    .update(cx, |page, cx| {
                        let Some(flow) = page.login.as_mut() else {
                            return true;
                        };
                        if flow.start.login_id != login_id {
                            return true;
                        }
                        let poll = match result.map(serde_json::from_value::<GithubLoginPoll>) {
                            Ok(Ok(poll)) => poll,
                            // A dropped relay frame is not a verdict.
                            _ => return false,
                        };
                        match poll.state {
                            GithubLoginState::Pending => false,
                            GithubLoginState::Done => {
                                page.login = None;
                                page.load(cx);
                                true
                            }
                            GithubLoginState::Error => {
                                flow.error = Some(
                                    poll.message
                                        .unwrap_or_else(|| "The sign-in didn't finish".into()),
                                );
                                cx.notify();
                                true
                            }
                        }
                    })
                    .unwrap_or(true);
                if settled {
                    break;
                }
            }
        }));
    }

    /// Copy the code again, confirmed by a short "Copied" on the code chip
    /// and the button.
    fn copy_code(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.login.as_mut() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(flow.start.user_code.clone()));
        flow.copied = true;
        flow.copied_flash = Some(std::time::Instant::now());
        self.flash_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FLASH).await;
            this.update(cx, |page, cx| {
                if let Some(flow) = page.login.as_mut() {
                    flow.copied_flash = None;
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Drop an unfinished sign-in, telling its device to stop waiting.
    fn cancel_login(&mut self, cx: &mut Context<Self>) {
        self.poll_task = None;
        let Some(flow) = self.login.take() else {
            return;
        };
        if flow.error.is_some() {
            return;
        }
        if let Some(engine) = self.state.read(cx).engine().cloned() {
            let params = flow
                .ticket
                .params(serde_json::json!({ "loginId": flow.start.login_id }));
            cx.background_spawn(async move {
                let _ = engine
                    .client()
                    .call(methods::CANCEL_GITHUB_LOGIN, params)
                    .await;
            })
            .detach();
        }
        cx.notify();
    }

    fn sign_out(&mut self, cx: &mut Context<Self>) {
        if self.busy || !self.target.read(cx).can_write(cx) {
            return;
        }
        let Ok(ticket) = self.target.read(cx).ticket(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let target = self.target.clone();
        let lease = target.update(cx, |target, cx| target.lock(cx));
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SIGN_OUT_GITHUB,
                    ticket.params(serde_json::json!({})),
                )
                .await;
            drop(lease);
            target.update(cx, |_, cx| cx.notify());
            this.update(cx, |page, cx| {
                page.busy = false;
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                match result {
                    Ok(_) => page.load(cx),
                    Err(err) => page.error = Some(format!("Couldn't sign out: {err}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn primary_button(
        theme: &Theme,
        id: &'static str,
        label: &'static str,
        enabled: bool,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .rounded(px(8.0))
            .px(px(12.0))
            .py(px(6.0))
            .bg(theme.solid)
            .text_color(theme.on_solid)
            .text_size(px(12.5))
            .font_weight(gpui::FontWeight::MEDIUM)
            .opacity(if enabled { 1.0 } else { 0.5 })
            .when(enabled, |el| {
                el.cursor_pointer().hover(|style| style.opacity(0.88))
            })
            .child(
                icons::icon(icons::GITHUB_MARK)
                    .size(px(13.0))
                    .text_color(theme.on_solid),
            )
            .child(label)
    }

    fn ghost(
        theme: &Theme,
        id: &'static str,
        label: impl Into<SharedString>,
    ) -> gpui::Stateful<gpui::Div> {
        let hover_theme = theme.clone();
        widgets::ghost_action(theme)
            .id(id)
            .flex_none()
            .hover(move |s| widgets::ghost_hover(&hover_theme, s))
            .child(label.into())
    }

    fn meta(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
        div()
            .mt(px(4.0))
            .text_size(px(12.0))
            .text_color(theme.text_muted)
            .child(text.into())
    }

    fn render_login_flow(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let flow = self.login.as_ref()?;
        let code = flow.start.user_code.clone();
        let url = flow.start.verification_uri.clone();
        let flashing = flow.copied_flash.is_some();
        let hover_bg = theme.ink(0.05);
        let code_chip = div()
            .id("github-login-code")
            .mt(px(8.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(12.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(move |style| style.bg(hover_bg))
            .on_click(cx.listener(|page, _, _, cx| page.copy_code(cx)))
            .child(
                div()
                    .mono(theme)
                    .text_size(px(22.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from(code)),
            )
            .child(
                icons::icon(if flashing { icons::CHECK } else { icons::COPY })
                    .size(px(14.0))
                    .text_color(if flashing {
                        theme.success_muted
                    } else {
                        theme.text_muted
                    }),
            );
        let mut body = div()
            .flex_1()
            .min_w_0()
            .child(widgets::row_title(theme, "Enter this code on GitHub"))
            .child(div().flex().child(code_chip));
        body = match &flow.error {
            Some(error) => body.child(
                div()
                    .mt(px(6.0))
                    .text_size(px(12.0))
                    .text_color(theme.danger_muted)
                    .child(SharedString::from(error.clone())),
            ),
            None => body.child(Self::meta(
                theme,
                format!(
                    "{}Approve Cypher in the browser, then come back — {} finishes the sign-in on its own.",
                    if flow.copied { "Copied to the clipboard. " } else { "" },
                    flow.ticket.label
                ),
            )),
        };
        let actions = if flow.error.is_some() {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    Self::ghost(theme, "github-login-dismiss", "Dismiss").on_click(cx.listener(
                        |page, _, _, cx| {
                            page.login = None;
                            cx.notify();
                        },
                    )),
                )
                .child(
                    Self::primary_button(theme, "github-login-retry", "Try again", !self.busy)
                        .on_click(cx.listener(|page, _, _, cx| page.sign_in(cx))),
                )
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    Self::ghost(
                        theme,
                        "github-login-copy",
                        if flashing { "Copied" } else { "Copy code" },
                    )
                    .on_click(cx.listener(|page, _, _, cx| page.copy_code(cx))),
                )
                .child(
                    Self::ghost(theme, "github-login-open", "Open GitHub")
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
                .child(
                    Self::ghost(theme, "github-login-cancel", "Cancel")
                        .on_click(cx.listener(|page, _, _, cx| page.cancel_login(cx))),
                )
        };
        Some(
            widgets::card_row(theme, true)
                .items_start()
                .child(widgets::row_tile(theme, icons::GITHUB_MARK))
                .child(body)
                .child(actions)
                .into_any_element(),
        )
    }

    fn render_status(
        &self,
        status: &GithubAccountStatus,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let device = self.target.read(cx).label(cx);
        let can_write = self.target.read(cx).can_write(cx);
        let row =
            widgets::card_row(theme, true).child(widgets::row_tile(theme, icons::GITHUB_MARK));
        let row = match &status.login {
            Some(login) => row
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(widgets::row_title(theme, format!("@{login}")))
                        .child(Self::meta(
                            theme,
                            format!("Signed in with the Cypher GitHub App on {device}"),
                        )),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .children(status.install_url.clone().map(|url| {
                            Self::ghost(theme, "github-manage-repos", "Manage repositories")
                                .on_click(move |_, _, cx| cx.open_url(&url))
                        }))
                        .when(can_write, |el| {
                            el.child(
                                Self::ghost(theme, "github-sign-out", "Sign out")
                                    .opacity(if self.busy { 0.5 } else { 1.0 })
                                    .on_click(cx.listener(|page, _, _, cx| page.sign_out(cx))),
                            )
                        }),
                ),
            None => {
                let detail = match (&status.fallback, status.sign_in_available) {
                    (Some(fallback), _) => format!(
                        "Using {} as @{} for now. Sign in so issue access doesn't depend on it.",
                        source_label(fallback.source),
                        fallback.login
                    ),
                    (None, true) => {
                        format!("Sign in so {device} can read the issues you reference with #.")
                    }
                    (None, false) => format!(
                        "No GitHub login on {device}. Run `gh auth login` there, or use a Cypher build with GitHub sign-in."
                    ),
                };
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(widgets::row_title(theme, "Not signed in"))
                        .child(Self::meta(theme, detail)),
                )
                .when(status.sign_in_available && can_write, |el| {
                    el.child(
                        Self::primary_button(
                            theme,
                            "github-sign-in",
                            "Sign in with GitHub",
                            !self.busy,
                        )
                        .on_click(cx.listener(|page, _, _, cx| page.sign_in(cx))),
                    )
                })
            }
        };
        row.into_any_element()
    }
}

impl Render for GithubPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let device = self.target.read(cx).label(cx);
        let card: AnyElement = if let Some(flow) = self.render_login_flow(&theme, cx) {
            widgets::section_card(&theme).child(flow).into_any_element()
        } else {
            match self.status.clone() {
                Loadable::Ready(status) => {
                    let signed_in = status.login.is_some();
                    div()
                        .child(
                            widgets::section_card(&theme)
                                .child(self.render_status(&status, &theme, cx)),
                        )
                        .when(signed_in, |el| {
                            el.child(
                                div()
                                    .mt(px(12.0))
                                    .text_size(px(11.5))
                                    .text_color(theme.text_muted.opacity(0.75))
                                    .child(SharedString::from(format!(
                                        "Signing out removes the token from {device}. To revoke Cypher's access everywhere, use {AUTHORIZATIONS_URL}."
                                    ))),
                            )
                        })
                        .into_any_element()
                }
                Loadable::Error(error) => widgets::error_strip(&theme, error).into_any_element(),
                _ => widgets::section_card(&theme)
                    .p(px(16.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child("Checking GitHub sign-in…"),
                    )
                    .into_any_element(),
            }
        };
        let column = widgets::page_column()
            .child(widgets::page_header(&theme, "GitHub", None))
            .child(widgets::page_subtitle(
                &theme,
                format!(
                    "Sign in on each device that hosts your projects. {device} uses it to read the issues you reference with #; the token stays on {device}."
                ),
            ))
            .children(
                self.target
                    .read(cx)
                    .unavailable(cx)
                    .map(|error| widgets::error_strip(&theme, error)),
            )
            .children(
                self.error
                    .clone()
                    .map(|error| widgets::error_strip(&theme, error)),
            )
            .child(card);
        div()
            .id("github-settings-page")
            .size_full()
            .overflow_y_scroll()
            .child(column)
    }
}
