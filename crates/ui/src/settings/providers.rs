//! Settings → Providers: scan-friendly connections, focused modal forms.
//! The presentation shares the existing device-scoped provider RPCs. Secrets
//! stay in ephemeral masked inputs and never enter a chat or synced document.
use cypher_engine::pi_providers::{LoginStatus, PiProviderInfo, PiProvidersSnapshot};
use cypher_rpc::methods;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, KeyDownEvent, MouseButton, SharedString,
    Subscription, Task, Window, div, prelude::*, px,
};

use super::device_target::DeviceTarget;
use super::device_target::DeviceTicket;
use super::web_search::WebSearchFallbackControl;
use super::widgets;
use crate::prefs::slash_commands::ProviderIntent;
use crate::{
    kit::icons,
    kit::popover::{self, Loadable},
    kit::theme::Theme,
    state::AppState,
    widgets::text_input::{TextInput, TextInputEvent},
};

mod dialogs;
mod model;
mod oauth;
mod rows;

use model::*;

const CLAUDE_CODE_INSTALL: &str = "https://code.claude.com/docs/en/quickstart";

fn status_color(theme: &Theme, state: &str) -> gpui::Hsla {
    match state {
        "connected" => theme.success_muted,
        "error" => theme.danger_muted,
        "signed_out" => theme.warning_muted,
        _ => theme.text_muted,
    }
}

struct Form {
    id: Entity<TextInput>,
    url: Entity<TextInput>,
    key: Entity<TextInput>,
    original: Option<PiProviderInfo>,
    kind: CustomProviderKind,
    errors: FieldErrors,
    focus: Option<Field>,
    _events: Vec<Subscription>,
}

impl Form {
    fn input(&self, field: Field) -> &Entity<TextInput> {
        match field {
            Field::Name => &self.id,
            Field::Url => &self.url,
            Field::Key => &self.key,
        }
    }
}

struct Busy {
    method: &'static str,
    provider: Option<String>,
}

struct OauthLogin {
    ticket: DeviceTicket,
    status: Option<LoginStatus>,
    callback: Entity<TextInput>,
    submitting: bool,
    error: Option<String>,
    focus_callback: bool,
    _events: Vec<Subscription>,
}

#[derive(Clone)]
struct ProviderMenu {
    provider: PiProviderInfo,
    active: usize,
}

#[derive(Clone)]
struct AddProviderMenu {
    active: usize,
}

#[derive(Clone, Copy)]
enum ButtonStyle {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// A 32px desktop control, with a visible keyboard focus ring and a real
/// disabled state (callers attach handlers only when enabled).
fn button(
    theme: &Theme,
    id: impl Into<gpui::ElementId>,
    label: &str,
    style: ButtonStyle,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let (bg, text, border) = match style {
        ButtonStyle::Primary => (theme.solid, theme.on_solid, theme.solid),
        ButtonStyle::Secondary => (theme.surface_card, theme.text, theme.border_strong),
        ButtonStyle::Ghost => (
            gpui::transparent_black(),
            theme.text_muted,
            gpui::transparent_black(),
        ),
        ButtonStyle::Danger => (theme.danger_strong, gpui::white(), theme.danger_strong),
    };
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label.to_string())
        .tab_index(0)
        .tab_stop(enabled)
        .h(px(32.0))
        .px(px(12.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(text)
        .text_size(px(12.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .focus_visible(|s| s.border_color(theme.accent))
        .when(enabled, |el| el.cursor_pointer().hover(|s| s.opacity(0.8)))
        .when(!enabled, |el| el.opacity(0.4))
        .when(!label.is_empty(), |el| {
            el.child(SharedString::from(label.to_string()))
        })
}

struct Hint(SharedString);
impl Render for Hint {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_overlay)
            .text_color(theme.text)
            .text_size(px(12.0))
            .child(self.0.clone())
    }
}

/// This GPUI revision skips Svg::paint without a color on the SVG itself;
/// a parent's text_color is not enough. Require a tint at every call site.
fn provider_icon(path: &'static str, size: f32, color: gpui::Hsla) -> gpui::Svg {
    icons::icon(path).size(px(size)).text_color(color)
}

fn add_provider_button(
    theme: &Theme,
    id: &'static str,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    button(theme, id, "", ButtonStyle::Primary, enabled)
        .aria_label("Add provider")
        .child(provider_icon(icons::PLUS, 14.0, theme.on_solid))
        .child("Add provider")
}

fn icon_button(
    theme: &Theme,
    id: impl Into<gpui::ElementId>,
    glyph: &'static str,
    label: &'static str,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    button(theme, id, "", ButtonStyle::Ghost, enabled)
        .w(px(32.0))
        .px_0()
        .aria_label(label)
        .child(provider_icon(glyph, 16.0, theme.text_muted))
        .tooltip(move |_, cx| cx.new(|_| Hint(label.into())).into())
}

fn caption(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(12.0))
        .line_height(px(18.0))
        .text_color(theme.text_muted)
        .child(text.into())
}

pub struct ProvidersPage {
    state: Entity<AppState>,
    target: Entity<DeviceTarget>,
    generation: u64,
    observed_device: Option<String>,
    _target_observer: Subscription,
    snapshot: Loadable<PiProvidersSnapshot>,
    form: Option<Form>,
    confirm: Option<(String, bool)>, // remove; otherwise log out
    confirm_focus: bool,
    intent: Option<ProviderIntent>,
    busy: Option<Busy>,
    oauth: Option<OauthLogin>,
    error: Option<String>,
    notice: Option<String>,
    menu: popover::Popup<ProviderMenu>,
    add_menu: popover::Popup<AddProviderMenu>,
    menu_focus: FocusHandle,
    add_menu_focus: FocusHandle,
    dialog_focus: FocusHandle,
    cancel_focus: FocusHandle,
    submit_focus: FocusHandle,
    close_focus: FocusHandle,
    return_focus: Option<FocusHandle>,
    restore_focus: bool,
    page_focus: FocusHandle,
    scroll: gpui::ScrollHandle,
    task: Option<Task<()>>,
    /// The Claude dialog's web-search fallback control (device-scoped).
    web_search: Entity<WebSearchFallbackControl>,
    /// The Claude row's Manage dialog is open.
    claude_dialog: bool,
}

impl ProvidersPage {
    pub fn new(
        state: Entity<AppState>,
        target: Entity<DeviceTarget>,
        intent: ProviderIntent,
        cx: &mut Context<Self>,
    ) -> Self {
        let generation = target.read(cx).generation();
        let observed_device = target.read(cx).id().map(str::to_string);
        let observer = cx.observe(&target, |page: &mut Self, target, cx| {
            let generation = target.read(cx).generation();
            if generation != page.generation {
                // Initial engine identification is not a user-requested device
                // switch; keep an onboarding/command intent waiting for that load.
                let intent = if page.observed_device.is_none() {
                    page.intent.take()
                } else {
                    None
                };
                page.observed_device = target.read(cx).id().map(str::to_string);
                page.generation = generation;
                page.dismiss(cx);
                page.intent = intent;
                page.task = None;
                page.snapshot = Loadable::Idle;
                page.busy = None;
                page.error = None;
                page.notice = None;
                page.call(methods::LIST_PI_PROVIDERS, serde_json::json!({}), cx);
            }
            cx.notify();
        });
        // Existing development capture convention; this never writes credentials.
        let intent = if cypher_env::var("OPEN_DIALOG").as_deref() == Some("provider-add") {
            ProviderIntent::Add
        } else {
            intent
        };
        let web_search =
            cx.new(|cx| WebSearchFallbackControl::new(state.clone(), target.clone(), cx));
        let mut page = Self {
            state,
            target,
            generation,
            observed_device,
            _target_observer: observer,
            web_search,
            claude_dialog: false,
            snapshot: Loadable::Idle,
            form: None,
            confirm: None,
            confirm_focus: false,
            intent: Some(intent),
            busy: None,
            oauth: None,
            error: None,
            notice: None,
            menu: popover::Popup::default(),
            add_menu: popover::Popup::default(),
            menu_focus: cx.focus_handle(),
            add_menu_focus: cx.focus_handle(),
            dialog_focus: cx.focus_handle(),
            cancel_focus: cx.focus_handle(),
            submit_focus: cx.focus_handle(),
            close_focus: cx.focus_handle(),
            return_focus: None,
            restore_focus: false,
            page_focus: cx.focus_handle(),
            scroll: gpui::ScrollHandle::new(),
            task: None,
        };
        page.call(methods::LIST_PI_PROVIDERS, serde_json::json!({}), cx);
        page
    }

    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        let changed = self.form.take().is_some()
            | self.confirm.take().is_some()
            | self.oauth.take().is_some()
            | self.intent.take().is_some()
            | self.menu.get().is_some()
            | self.add_menu.get().is_some()
            | std::mem::take(&mut self.claude_dialog);
        self.menu = popover::Popup::default();
        self.add_menu = popover::Popup::default();
        self.return_focus = None;
        self.restore_focus = false;
        if changed {
            cx.notify();
        }
    }

    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() && self.oauth.is_none() {
            return;
        }
        self.form = None;
        self.confirm = None;
        self.oauth = None;
        self.claude_dialog = false;
        self.error = None;
        self.restore_focus = false;
        self.return_focus
            .take()
            .unwrap_or_else(|| self.page_focus.clone())
            .focus(window, cx);
        cx.notify();
    }

    fn apply_intent(&mut self, cx: &mut Context<Self>) {
        match self.intent.take() {
            Some(ProviderIntent::Add) => self.edit(None, cx),
            Some(ProviderIntent::Edit(id)) => {
                let id = if id == "anthropic" {
                    "claude-code".to_string()
                } else {
                    id
                };
                let provider = self
                    .snapshot
                    .ready()
                    .and_then(|s| s.providers.iter().find(|p| p.id == id))
                    .cloned();
                if let Some(provider) = provider {
                    if is_claude_cli(&provider) {
                        if !provider.credential_saved {
                            cx.open_url(CLAUDE_CODE_INSTALL);
                        }
                    } else if is_oauth(&provider) {
                        self.start_oauth(&provider.id, cx);
                    } else {
                        self.edit(Some(provider), cx);
                    }
                } else {
                    self.error = Some(format!(
                        "Provider \"{id}\" is not configured. Add it first."
                    ));
                }
            }
            Some(ProviderIntent::Logout(id)) => self.ask_remove(id, false, cx),
            _ => {}
        }
    }

    fn call(&mut self, method: &'static str, params: serde_json::Value, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let writing = method != methods::LIST_PI_PROVIDERS;
        if writing
            && (!self.target.read(cx).can_write(cx)
                || self.generation != self.target.read(cx).generation())
        {
            return;
        }
        let ticket = match self.target.read(cx).ticket(cx) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some("Engine not connected. Reconnect and try again.".into());
            cx.notify();
            return;
        };
        let provider = params
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        self.busy = Some(Busy {
            method,
            provider: provider.clone(),
        });
        self.error = None;
        self.notice = None;
        self.menu = popover::Popup::default();
        self.add_menu = popover::Popup::default();
        self.task = None;
        let target = self.target.clone();
        let lease = writing.then(|| target.update(cx, |target, cx| target.lock(cx)));
        let task = cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, ticket.params(params)).await;
            drop(lease);
            target.update(cx, |_, cx| {
                cx.notify();
                if writing {
                    crate::pickers::bump_harness_catalog(cx);
                }
            });
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.busy = None;
                match result {
                    Ok(value) => match serde_json::from_value::<PiProvidersSnapshot>(value) {
                        Ok(snapshot) => {
                            if method == methods::SAVE_PI_PROVIDER
                                && let Some(p) = snapshot
                                    .providers
                                    .iter()
                                    .find(|p| Some(&p.id) == provider.as_ref())
                            {
                                page.notice = Some(format!(
                                    "{} connected. {} models available.",
                                    p.id, p.model_count
                                ));
                            }
                            page.snapshot = Loadable::Ready(snapshot);
                            if writing {
                                page.web_search.update(cx, |control, cx| control.reload(cx));
                            } else {
                                crate::pickers::bump_harness_catalog(cx);
                            }
                            if method != methods::LIST_PI_PROVIDERS {
                                page.restore_focus = page.form.is_some() || page.confirm.is_some();
                                page.form = None;
                                page.confirm = None;
                            }
                            page.apply_intent(cx);
                        }
                        Err(_) => {
                            page.error =
                                Some("Could not read the provider response. Please retry.".into())
                        }
                    },
                    Err(error) => page.error = Some(format!("{}: {error}", ticket.label)),
                }
                cx.notify();
            })
            .ok();
        });
        if writing {
            task.detach();
        } else {
            self.task = Some(task);
        }
        cx.notify();
    }

    fn edit(&mut self, provider: Option<PiProviderInfo>, cx: &mut Context<Self>) {
        if self.busy.is_some() || !self.target.read(cx).can_write(cx) {
            return;
        }
        self.error = None;
        self.notice = None;
        self.confirm = None;
        self.menu = popover::Popup::default();
        self.add_menu = popover::Popup::default();
        let kind = CustomProviderKind::from_provider(provider.as_ref());
        let id = cx.new(|cx| TextInput::settings_field("e.g. my-gateway", false, cx));
        let url = cx.new(|cx| TextInput::settings_field("https://api.example.com", false, cx));
        let key = cx.new(|cx| {
            TextInput::settings_field(
                if provider.as_ref().is_some_and(|p| p.credential_saved) {
                    "Leave empty to keep the saved key"
                } else {
                    "Paste your API key"
                },
                true,
                cx,
            )
        });
        if let Some(p) = &provider {
            id.update(cx, |input, cx| input.set_text(p.id.clone(), cx));
            url.update(cx, |input, cx| input.set_text(p.base_url.clone(), cx));
        }
        let mut events = Vec::new();
        for (field, input) in [(Field::Name, &id), (Field::Url, &url), (Field::Key, &key)] {
            events.push(cx.subscribe(input, move |page: &mut Self, _, event, cx| {
                if matches!(event, TextInputEvent::Submitted) {
                    page.save(cx);
                } else if matches!(event, TextInputEvent::Edited | TextInputEvent::CursorMoved) {
                    if matches!(event, TextInputEvent::Edited)
                        && let Some(form) = &mut page.form
                    {
                        match field {
                            Field::Name => form.errors.name = None,
                            Field::Url => form.errors.url = None,
                            Field::Key => form.errors.key = None,
                        }
                    }
                    cx.notify();
                }
            }));
        }
        let focus = Some(if provider.is_some() {
            Field::Key
        } else {
            Field::Name
        });
        self.form = Some(Form {
            id,
            url,
            key,
            original: provider,
            kind,
            errors: FieldErrors::default(),
            focus,
            _events: events,
        });
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || !self.target.read(cx).can_write(cx) {
            return;
        }
        let Some(form) = &mut self.form else {
            return;
        };
        let id = form.id.read(cx).text().trim().to_string();
        let url = form.url.read(cx).text().trim().to_string();
        let key = form.key.read(cx).text().trim().to_string();
        form.errors = validate_form(&id, &url, &key, form.original.as_ref());
        if let Some(field) = form.errors.first() {
            form.focus = Some(field);
            cx.notify();
            return;
        }
        let params = serde_json::json!({
            "id": id, "baseUrl": url, "apiKey": key, "edit": form.original.is_some(),
        });
        form.key.update(cx, |input, cx| input.set_text("", cx));
        self.call(methods::SAVE_PI_PROVIDER, params, cx);
    }

    fn ask_remove(&mut self, id: String, remove: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() || !self.target.read(cx).can_write(cx) {
            return;
        }
        self.menu = popover::Popup::default();
        self.form = None;
        self.confirm = Some((id, remove));
        self.confirm_focus = true;
        self.error = None;
        cx.notify();
    }

    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.begin_close() {
            if self.menu_focus.is_focused(window) {
                self.page_focus.focus(window, cx);
            }
            popover::reap_popup(cx, |page: &mut Self| &mut page.menu);
            cx.notify();
        }
    }

    fn close_add_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_menu.begin_close() {
            if self.add_menu_focus.is_focused(window) {
                self.page_focus.focus(window, cx);
            }
            popover::reap_popup(cx, |page: &mut Self| &mut page.add_menu);
            cx.notify();
        }
    }

    fn toggle_add_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_menu.take_press_was_open() {
            self.close_add_menu(window, cx);
            return;
        }
        self.menu = popover::Popup::default();
        self.add_menu.open(AddProviderMenu { active: 0 });
        self.add_menu_focus.focus(window, cx);
        cx.notify();
    }

    fn add_custom_kind(
        &mut self,
        kind: CustomProviderKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_menu = popover::Popup::default();
        self.page_focus.focus(window, cx);
        match kind {
            CustomProviderKind::NewApi => self.edit(None, cx),
        }
    }
}

impl Render for ProvidersPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let dialog_open = self.form.is_some() || self.confirm.is_some() || self.oauth.is_some();
        if self.restore_focus {
            self.restore_focus = false;
            self.return_focus
                .take()
                .unwrap_or_else(|| self.page_focus.clone())
                .focus(window, cx);
        }
        if dialog_open {
            if self.return_focus.is_none() {
                self.return_focus = Some(
                    window
                        .focused(cx)
                        .filter(|focus| {
                            focus != &self.menu_focus
                                && focus != &self.add_menu_focus
                                && focus != &self.dialog_focus
                        })
                        .unwrap_or_else(|| self.page_focus.clone()),
                );
            }
            if let Some(form) = &mut self.form
                && let Some(field) = form.focus.take()
            {
                form.input(field).focus_handle(cx).focus(window, cx);
            }
            if let Some(oauth) = &mut self.oauth
                && std::mem::take(&mut oauth.focus_callback)
            {
                oauth.callback.focus_handle(cx).focus(window, cx);
            }
            if std::mem::take(&mut self.confirm_focus) {
                self.cancel_focus.focus(window, cx);
            }
            let steal_dialog = self.busy.is_some()
                && self.oauth.as_ref().is_none_or(|oauth| {
                    !oauth
                        .status
                        .as_ref()
                        .is_some_and(|s| s.phase == "awaiting_callback")
                });
            if steal_dialog {
                self.dialog_focus.focus(window, cx);
            }
        }
        let count = self.snapshot.ready().map(|s| s.providers.len());
        let busy = self.busy.is_some();
        let blocked = busy || !self.target.read(cx).can_write(cx);
        let loaded = count.is_some();
        let current = self
            .state
            .read(cx)
            .selected_chat_row()
            .filter(|chat| Some(chat.device_id.as_str()) == self.target.read(cx).id())
            .and_then(|chat| chat.config.as_ref())
            .filter(|c| c.harness == cypher_proto::HarnessId::Pi)
            .and_then(|c| c.model.clone());
        let header = div()
            .w_full()
            .flex()
            .flex_wrap()
            .items_start()
            .justify_between()
            .gap(px(16.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(200.0))
                    .child(
                        div()
                            .text_size(px(20.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("Providers"),
                    )
                    .child(
                        caption(&theme, "Connect the models you want to work with.").mt(px(8.0)),
                    ),
            )
            .children(
                count
                    .is_some_and(|n| n > 0)
                    .then(|| self.render_add_trigger(&theme, "provider-add", !blocked, true, cx)),
            );
        let mut body = widgets::page_column().pt(px(36.0)).child(header).child(
            div()
                .w_full()
                .mt(px(32.0))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(widgets::field_label(&theme, "Connections"))
                        .when_some(count, |el, n| {
                            el.child(widgets::badge(&theme, n.to_string()))
                        }),
                )
                .child(
                    div()
                        .ml_auto()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .child(
                            icon_button(
                                &theme,
                                "provider-reload",
                                icons::REFRESH,
                                "Reload providers",
                                !busy,
                            )
                            .when(!busy, |el| {
                                el.on_click(cx.listener(|page, _, _, cx| {
                                    page.call(methods::LIST_PI_PROVIDERS, serde_json::json!({}), cx)
                                }))
                            }),
                        ),
                ),
        );
        if let Some(error) = self.target.read(cx).unavailable(cx) {
            body = body.child(widgets::warning_strip(&theme, error));
        }
        if !dialog_open {
            if let Some(error) = &self.error {
                body = body.child(widgets::error_strip(&theme, error.clone()).mt(px(16.0)));
            }
            if let Some(notice) = &self.notice {
                body = body.child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(provider_icon(icons::CHECK, 14.0, theme.success))
                        .child(caption(&theme, notice.clone()).text_color(theme.success_muted)),
                );
            }
        }
        if !loaded && busy {
            body = body.child(
                widgets::section_card(&theme)
                    .mt(px(12.0))
                    .p(px(20.0))
                    .child(popover::skeleton_rows(
                        "providers-loading",
                        &theme,
                        3,
                        cx.entity_id(),
                        cx,
                    )),
            );
        } else if count == Some(0) {
            body = body.child(self.render_empty(&theme, cx));
        } else if let Some(snapshot) = self.snapshot.ready().cloned() {
            let claude = snapshot
                .providers
                .iter()
                .find(|provider| is_claude_cli(provider))
                .cloned();
            let chatgpt = snapshot
                .providers
                .iter()
                .find(|provider| provider.id == "openai-codex")
                .cloned();
            let gateways: Vec<_> = snapshot
                .providers
                .iter()
                .filter(|provider| !is_subscription(provider))
                .cloned()
                .collect();
            let mut first_group = true;
            if let Some(provider) = claude {
                body = body.child(self.subscription_group(
                    &theme,
                    provider,
                    0,
                    first_group,
                    current.as_deref(),
                    cx,
                ));
                first_group = false;
            }
            if let Some(provider) = chatgpt {
                body = body.child(self.subscription_group(
                    &theme,
                    provider,
                    1,
                    first_group,
                    current.as_deref(),
                    cx,
                ));
                first_group = false;
            }
            if !gateways.is_empty() {
                let rows = gateways
                    .into_iter()
                    .enumerate()
                    .map(|(index, provider)| {
                        self.provider_row(
                            provider,
                            index + 8,
                            index == 0,
                            current.as_deref(),
                            &theme,
                            cx,
                        )
                    })
                    .collect::<Vec<_>>();
                body = body.child(
                    widgets::section_card(&theme)
                        .when(first_group, |el| el.mt(px(12.0)))
                        .children(rows),
                );
            }
        }
        if count.is_some_and(|n| n > 0) {
            body = body.child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(provider_icon(
                        icons::KEY_MINIMALISTIC,
                        14.0,
                        theme.text_muted,
                    ))
                    .child(caption(
                        &theme,
                        format!(
                            "API keys stay on {}, separate from your chats.",
                            self.target.read(cx).label(cx)
                        ),
                    )),
            );
        }
        let modal = if self.form.is_some() {
            Some(popover::modal(
                "provider-form-modal",
                window.viewport_size(),
                self.render_form(window, &theme, cx),
            ))
        } else if self.confirm.is_some() {
            Some(popover::modal(
                "provider-confirm-modal",
                window.viewport_size(),
                self.render_confirmation(window, &theme, cx),
            ))
        } else if self.oauth.is_some() {
            Some(popover::modal(
                "provider-oauth-modal",
                window.viewport_size(),
                self.render_oauth(window, &theme, cx),
            ))
        } else if self.claude_dialog {
            Some(popover::modal(
                "provider-claude-modal",
                window.viewport_size(),
                self.render_claude_dialog(window, &theme, cx),
            ))
        } else {
            None
        };
        div()
            .id("providers-page")
            .key_context("ProviderPage")
            .track_focus(&self.page_focus)
            .tab_group()
            .size_full()
            .relative()
            .on_key_down(cx.listener(|page, event: &KeyDownEvent, window, cx| {
                if page.form.is_some() || page.confirm.is_some() {
                    return;
                }
                if event.keystroke.key == "tab" {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .id("provider-list-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(body),
            )
            .children(modal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> PiProviderInfo {
        PiProviderInfo {
            id: "mvp-lab".into(),
            title: None,
            base_url: "https://api.example.com".into(),
            provider_type: "newapi".into(),
            credential_saved: true,
            state: "unverified".into(),
            model_count: 0,
            checked_at: None,
            message: None,
        }
    }

    #[test]
    fn add_menu_kinds_start_with_openai_compatible() {
        assert_eq!(CustomProviderKind::ALL, &[CustomProviderKind::NewApi]);
        let spec = CustomProviderKind::NewApi.spec();
        assert_eq!(spec.title, "OpenAI-compatible");
        assert!(spec.caption.contains("NewAPI"));
    }

    #[test]
    fn field_validation_names_the_first_problem_without_returning_secrets() {
        let errors = validate_form("", "invalid", "", None);
        assert_eq!(errors.first(), Some(Field::Name));
        assert_eq!(
            validate_form("gateway", "invalid", "", None).first(),
            Some(Field::Url)
        );
        assert_eq!(
            validate_form("gateway", "https://api.example.com", "", None).first(),
            Some(Field::Key)
        );
        assert!(
            validate_form(
                "gateway",
                "https://api.example.com/v1/",
                "fixture-key",
                None
            )
            .first()
            .is_none()
        );
        for name in ["a/b", "constructor", "white space", "🚀"] {
            assert!(
                validate_form(name, "https://api.example.com", "fixture-key", None)
                    .name
                    .is_some()
            );
        }
    }

    #[test]
    fn saved_key_can_only_be_kept_for_the_same_endpoint() {
        let p = provider();
        assert!(
            validate_form(&p.id, "https://api.example.com/v1", "", Some(&p))
                .first()
                .is_none()
        );
        assert!(
            validate_form(&p.id, "https://other.example.com", "", Some(&p))
                .key
                .is_some()
        );
        let mut signed_out = p.clone();
        signed_out.credential_saved = false;
        assert!(
            validate_form(&p.id, &p.base_url, "", Some(&signed_out))
                .key
                .is_some()
        );
        for url in [
            "http://example.com",
            "https://user:key@example.com",
            "https://example.com?key=x",
            "https://example.com/#key",
            "file:///tmp/x",
        ] {
            assert!(normalized_url(url).is_none(), "{url}");
        }
        assert!(normalized_url("http://127.0.0.1:8080").is_some());
    }

    #[test]
    fn saved_credentials_are_not_presented_as_verified() {
        let mut p = provider();
        assert_eq!(status_label(&p), "Not verified");
        p.state = "connected".into();
        assert_eq!(status_label(&p), "Verified");
        p.state = "signed_out".into();
        assert_eq!(status_label(&p), "Needs API key");
        p.provider_type = "oauth".into();
        assert_eq!(status_label(&p), "Needs sign-in");
        p.provider_type = "claude-cli".into();
        assert_eq!(status_label(&p), "Needs Claude Code");
        p.state = "connected".into();
        assert_eq!(status_label(&p), "Installed");
        p.state = "error".into();
        assert_eq!(status_label(&p), "Connection failed");
        assert_eq!(checked_label(None, 0), "Not checked yet");
        assert_eq!(checked_label(Some(10), 0), "Checked just now");
        assert_eq!(checked_label(Some(0), 60_000), "Checked 1m ago");
    }

    #[test]
    fn status_labels_remain_readable_in_both_appearances() {
        for theme in [Theme::dark(), Theme::light()] {
            for state in ["connected", "signed_out", "error", "unverified"] {
                let color = status_color(&theme, state);
                let background =
                    crate::kit::theme::flatten(color.opacity(0.08), theme.surface_card);
                assert!(
                    crate::kit::theme::contrast_ratio(color, background) >= 4.5,
                    "{state} status text: {:?}",
                    theme.appearance
                );
            }
        }
    }

    #[test]
    fn provider_svg_icons_have_their_own_paint_color() {
        for theme in [Theme::dark(), Theme::light()] {
            for (path, color) in [
                (icons::PLUS, theme.on_solid),
                (icons::REFRESH, theme.text_muted),
                (icons::CLOSE, theme.text_muted),
                (icons::CHECK, theme.success),
                (icons::TRASH_BIN_MINIMALISTIC, theme.danger_muted),
            ] {
                let mut glyph = provider_icon(path, 14.0, color);
                // Svg::paint checks this exact field, not its parent's color.
                assert_eq!(glyph.style().text.color, Some(color));
                assert_eq!(glyph.style().size.width, Some(px(14.0).into()));
                assert_eq!(glyph.style().size.height, Some(px(14.0).into()));
            }
            assert!(crate::kit::theme::contrast_ratio(theme.on_solid, theme.solid) >= 4.5);
        }
    }
}
