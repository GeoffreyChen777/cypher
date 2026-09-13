//! Settings → Providers: scan-friendly connections, focused modal forms.
//! The presentation shares the existing device-scoped provider RPCs. Secrets
//! stay in ephemeral masked inputs and never enter a chat or synced document.
use cypher_engine::pi_providers::{PiProviderInfo, PiProvidersSnapshot};
use cypher_rpc::methods;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, KeyDownEvent, MouseButton, SharedString,
    Subscription, Task, Window, div, prelude::*, px,
};

use super::device_target::DeviceTarget;
use super::widgets;
use crate::{
    composer::{ComposerInput, ComposerInputEvent},
    icons,
    popover::{self, Loadable},
    state::AppState,
    theme::Theme,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderIntent {
    List,
    Add,
    Edit(String),
    Logout(String),
}

pub fn command_intent(text: &str) -> Option<ProviderIntent> {
    let parts: Vec<_> = text.split_whitespace().collect();
    match parts.as_slice() {
        ["/provider"] | ["/login"] | ["/logout"] => Some(ProviderIntent::List),
        ["/provider", "add"] => Some(ProviderIntent::Add),
        ["/login", id, ..] => Some(ProviderIntent::Edit((*id).into())),
        ["/logout", id] => Some(ProviderIntent::Logout((*id).into())),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Url,
    Key,
}

#[derive(Default)]
struct FieldErrors {
    name: Option<&'static str>,
    url: Option<&'static str>,
    key: Option<&'static str>,
}

impl FieldErrors {
    fn first(&self) -> Option<Field> {
        if self.name.is_some() {
            Some(Field::Name)
        } else if self.url.is_some() {
            Some(Field::Url)
        } else if self.key.is_some() {
            Some(Field::Key)
        } else {
            None
        }
    }
}

/// Match the service's URL policy so avoidable mistakes stay next to the field.
/// The engine remains authoritative; client validation is not a security gate.
fn normalized_url(value: &str) -> Option<String> {
    if value.contains(['\r', '\n']) {
        return None;
    }
    let mut url = url::Url::parse(value.trim()).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        return None;
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(path.strip_suffix("/v1").unwrap_or(&path));
    Some(url.to_string().trim_end_matches('/').into())
}

fn validate_form(
    name: &str,
    url: &str,
    key: &str,
    original: Option<&PiProviderInfo>,
) -> FieldErrors {
    let mut errors = FieldErrors::default();
    let name_ok = !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        && !matches!(name, "constructor" | "prototype" | "__proto__");
    if !name_ok {
        errors.name = Some("Use 1–64 letters, numbers, dots, hyphens or underscores.");
    }
    let normalized = normalized_url(url);
    if normalized.is_none() {
        errors.url = Some(
            "Enter an HTTPS service URL without credentials or query parameters. Localhost may use HTTP.",
        );
    }
    let can_keep_key = original.is_some_and(|p| {
        p.credential_saved && normalized.is_some() && normalized_url(&p.base_url) == normalized
    });
    if key.contains(['\r', '\n']) {
        errors.key = Some("Paste a single API key, without line breaks.");
    } else if key.is_empty() && !can_keep_key {
        errors.key = Some(if original.is_some_and(|p| p.credential_saved) {
            "Enter the API key again when changing the service URL."
        } else {
            "Enter an API key to connect this provider."
        });
    }
    errors
}

fn status_label(provider: &PiProviderInfo) -> &'static str {
    match provider.state.as_str() {
        "connected" => "Verified",
        "error" => "Connection failed",
        "signed_out" => "Needs API key",
        _ => "Not verified",
    }
}

fn status_color(theme: &Theme, state: &str) -> gpui::Hsla {
    match state {
        "connected" => theme.success_muted,
        "error" => theme.danger_muted,
        "signed_out" => theme.warning_muted,
        _ => theme.text_muted,
    }
}

fn checked_label(at: Option<i64>, now: i64) -> String {
    let Some(at) = at else {
        return "Not checked yet".into();
    };
    let minutes = now.saturating_sub(at).max(0) / 60_000;
    match minutes {
        0 => "Checked just now".into(),
        1..=59 => format!("Checked {minutes}m ago"),
        60..=1439 => format!("Checked {}h ago", minutes / 60),
        _ => chrono::DateTime::from_timestamp_millis(at)
            .map(|d| {
                format!(
                    "Checked {}",
                    d.with_timezone(&chrono::Local).format("%b %-d")
                )
            })
            .unwrap_or_else(|| "Not checked yet".into()),
    }
}

struct Form {
    id: Entity<ComposerInput>,
    url: Entity<ComposerInput>,
    key: Entity<ComposerInput>,
    original: Option<PiProviderInfo>,
    errors: FieldErrors,
    focus: Option<Field>,
    _events: Vec<Subscription>,
}

impl Form {
    fn input(&self, field: Field) -> &Entity<ComposerInput> {
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

#[derive(Clone)]
struct ProviderMenu {
    provider: PiProviderInfo,
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
    error: Option<String>,
    notice: Option<String>,
    menu: popover::Popup<ProviderMenu>,
    menu_focus: FocusHandle,
    dialog_focus: FocusHandle,
    cancel_focus: FocusHandle,
    submit_focus: FocusHandle,
    close_focus: FocusHandle,
    return_focus: Option<FocusHandle>,
    restore_focus: bool,
    page_focus: FocusHandle,
    scroll: gpui::ScrollHandle,
    task: Option<Task<()>>,
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
        let mut page = Self {
            state,
            target,
            generation,
            observed_device,
            _target_observer: observer,
            snapshot: Loadable::Idle,
            form: None,
            confirm: None,
            confirm_focus: false,
            intent: Some(intent),
            busy: None,
            error: None,
            notice: None,
            menu: popover::Popup::default(),
            menu_focus: cx.focus_handle(),
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
            | self.intent.take().is_some()
            | self.menu.get().is_some();
        self.menu = popover::Popup::default();
        self.return_focus = None;
        self.restore_focus = false;
        if changed {
            cx.notify();
        }
    }

    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.form = None;
        self.confirm = None;
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
                let provider = self
                    .snapshot
                    .ready()
                    .and_then(|s| s.providers.iter().find(|p| p.id == id))
                    .cloned();
                if let Some(provider) = provider {
                    self.edit(Some(provider), cx);
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
                            if method == methods::SAVE_PI_PROVIDER {
                                if let Some(p) = snapshot
                                    .providers
                                    .iter()
                                    .find(|p| Some(&p.id) == provider.as_ref())
                                {
                                    page.notice = Some(format!(
                                        "{} connected. {} models available.",
                                        p.id, p.model_count
                                    ));
                                }
                            }
                            page.snapshot = Loadable::Ready(snapshot);
                            if !writing {
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
        let id = cx.new(|cx| ComposerInput::settings_field("e.g. my-gateway", false, cx));
        let url = cx.new(|cx| ComposerInput::settings_field("https://api.example.com", false, cx));
        let key = cx.new(|cx| {
            ComposerInput::settings_field(
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
                if matches!(event, ComposerInputEvent::Submitted) {
                    page.save(cx);
                } else if matches!(
                    event,
                    ComposerInputEvent::Edited | ComposerInputEvent::CursorMoved
                ) {
                    if matches!(event, ComposerInputEvent::Edited)
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

    fn menu_action(
        &mut self,
        provider: PiProviderInfo,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.page_focus.focus(window, cx);
        match index {
            0 if provider.credential_saved => {
                self.call(
                    methods::REFRESH_PI_PROVIDER,
                    serde_json::json!({ "id": provider.id }),
                    cx,
                );
            }
            1 if provider.credential_saved => self.ask_remove(provider.id, false, cx),
            2 => self.ask_remove(provider.id, true, cx),
            _ => {}
        }
    }

    fn on_dialog_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            self.close_dialog(window, cx);
            cx.stop_propagation();
        } else if event.keystroke.key == "tab" {
            let mut handles = vec![self.close_focus.clone()];
            if let Some(form) = &self.form {
                if form.original.is_none() {
                    handles.push(form.id.focus_handle(cx));
                }
                handles.push(form.url.focus_handle(cx));
                handles.push(form.key.focus_handle(cx));
            }
            handles.push(self.cancel_focus.clone());
            handles.push(self.submit_focus.clone());
            if self.busy.is_some() {
                self.dialog_focus.focus(window, cx);
            } else {
                let active = handles.iter().position(|h| h.is_focused(window));
                let delta = if event.keystroke.modifiers.shift {
                    -1
                } else {
                    1
                };
                if let Some(next) = popover::menu_step(active, handles.len(), delta) {
                    handles[next].focus(window, cx);
                }
            }
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn field(
        &self,
        field: Field,
        label: &'static str,
        hint: &str,
        theme: &Theme,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let form = self.form.as_ref().unwrap();
        let input = form.input(field);
        let error = match field {
            Field::Name => form.errors.name,
            Field::Url => form.errors.url,
            Field::Key => form.errors.key,
        };
        let focused = input.focus_handle(cx).is_focused(window);
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(widgets::field_label(theme, label))
            .child(
                div()
                    .id(("provider-field", field as usize))
                    .role(gpui::Role::Group)
                    .aria_label(label)
                    .w_full()
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(if error.is_some() {
                        theme.danger
                    } else if focused {
                        theme.accent
                    } else {
                        theme.border_strong
                    })
                    .bg(theme.input_glass_bg())
                    .child(input.clone()),
            )
            .child(
                caption(theme, error.unwrap_or(hint).to_string())
                    .when(error.is_some(), |el| el.text_color(theme.danger_muted)),
            )
    }

    fn render_form(
        &mut self,
        window: &Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let form = self.form.as_ref().unwrap();
        let editing = form.original.is_some();
        let saved = form.original.as_ref().is_some_and(|p| p.credential_saved);
        let busy = self.busy.is_some();
        let width = (f32::from(window.viewport_size().width) - 40.0).clamp(280.0, 464.0);
        let mut fields = div()
            .px(px(24.0))
            .py(px(24.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(widgets::row_tile(theme, icons::GLOBAL).size(px(32.0)))
                    .child(
                        div()
                            .flex_1()
                            .child(widgets::row_title(theme, "OpenAI-compatible"))
                            .child(caption(theme, "NewAPI and compatible gateways")),
                    )
                    .child(widgets::badge(theme, "API key")),
            );
        if let Some(p) = &form.original {
            fields = fields.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(widgets::field_label(theme, "Provider name"))
                    .child(
                        div()
                            .h(px(40.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.ink(0.025))
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(p.id.clone())),
                    ),
            );
        } else {
            fields = fields.child(self.field(
                Field::Name,
                "Provider name",
                "A short name to identify this connection.",
                theme,
                window,
                cx,
            ));
        }
        fields = fields
            .child(self.field(
                Field::Url,
                "Base URL",
                "Use the service root or its /v1 endpoint.",
                theme,
                window,
                cx,
            ))
            .child(self.field(
                Field::Key,
                "API key",
                if saved {
                    "Leave empty to keep your key. A new URL requires entering it again."
                } else {
                    "Get a key from your provider's dashboard."
                },
                theme,
                window,
                cx,
            ));
        if let Some(error) = &self.error {
            fields = fields.child(widgets::error_strip(theme, error.clone()).mt_0());
        }
        let footer = div()
            .px(px(24.0))
            .py(px(16.0))
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.ink(0.02))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(provider_icon(
                        icons::KEY_MINIMALISTIC,
                        14.0,
                        theme.text_muted,
                    ))
                    .child(caption(
                        theme,
                        if self.target.read(cx).is_local() {
                            format!(
                                "Saved only on {}. Never added to chats.",
                                self.target.read(cx).label(cx)
                            )
                        } else {
                            format!(
                                "Sent via your account's relay; saved only on {}.",
                                self.target.read(cx).label(cx)
                            )
                        },
                    )),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        button(
                            theme,
                            "provider-cancel",
                            "Cancel",
                            ButtonStyle::Ghost,
                            !busy,
                        )
                        .track_focus(&self.cancel_focus)
                        .when(!busy, |el| {
                            el.on_click(
                                cx.listener(|page, _, window, cx| page.close_dialog(window, cx)),
                            )
                        }),
                    )
                    .child(
                        button(
                            theme,
                            "provider-save",
                            if busy {
                                "Connecting…"
                            } else if editing {
                                "Save connection"
                            } else {
                                "Connect provider"
                            },
                            ButtonStyle::Primary,
                            !busy && self.target.read(cx).can_write(cx),
                        )
                        .h(px(36.0))
                        .track_focus(&self.submit_focus)
                        .when(busy, |el| {
                            el.child(crate::loaders::mini_gradient_spinner(
                                "provider-saving",
                                2.0,
                                cx.entity_id(),
                                cx,
                            ))
                        })
                        .when(!busy && self.target.read(cx).can_write(cx), |el| {
                            el.on_click(cx.listener(|page, _, _, cx| page.save(cx)))
                        }),
                    ),
            );
        popover::dialog_card(theme)
            .id("provider-form-dialog")
            .role(gpui::Role::Dialog)
            .aria_label(if editing {
                "Provider settings"
            } else {
                "Add provider"
            })
            .track_focus(&self.dialog_focus)
            .key_context("ProviderDialog")
            .tab_group()
            .p_0()
            .w(px(width))
            .overflow_hidden()
            .on_key_down(cx.listener(Self::on_dialog_key))
            .child(self.dialog_heading(
                theme,
                if editing {
                    "Provider settings"
                } else {
                    "Add provider"
                },
                "Connect models to your workspace.",
                cx,
            ))
            .child(
                div()
                    .id("provider-form-scroll")
                    .max_h(px(
                        (f32::from(window.viewport_size().height) - 252.0).max(120.0)
                    ))
                    .overflow_y_scroll()
                    .child(fields),
            )
            .child(footer)
            .into_any_element()
    }

    fn dialog_heading(
        &self,
        theme: &Theme,
        title: &str,
        description: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let busy = self.busy.is_some();
        div()
            .px(px(24.0))
            .pt(px(24.0))
            .flex()
            .items_start()
            .gap(px(12.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(popover::dialog_title(theme, title).text_size(px(18.0)))
                    .child(caption(theme, description.to_string()).mt(px(6.0)))
                    .child(
                        caption(
                            theme,
                            format!("Target device: {}", self.target.read(cx).label(cx)),
                        )
                        .mt(px(4.0)),
                    ),
            )
            .child(
                icon_button(
                    theme,
                    "provider-dialog-close",
                    icons::CLOSE,
                    "Close dialog",
                    !busy,
                )
                .track_focus(&self.close_focus)
                .when(!busy, |el| {
                    el.on_click(cx.listener(|page, _, window, cx| page.close_dialog(window, cx)))
                }),
            )
    }

    fn render_confirmation(
        &mut self,
        window: &Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (id, remove) = self.confirm.clone().unwrap();
        let busy = self.busy.is_some();
        let title = if remove {
            "Delete provider?"
        } else {
            "Remove saved API key?"
        };
        let copy = if remove {
            format!(
                "“{id}” and its saved API key will be removed from {}.",
                self.target.read(cx).label(cx)
            )
        } else {
            format!("You'll need to enter an API key again to use models from “{id}”.")
        };
        let width = (f32::from(window.viewport_size().width) - 40.0).clamp(280.0, 420.0);
        popover::dialog_card(theme)
            .id("provider-confirm-dialog")
            .role(gpui::Role::AlertDialog)
            .aria_label(title)
            .track_focus(&self.dialog_focus)
            .key_context("ProviderDialog")
            .tab_group()
            .w(px(width))
            .p_0()
            .on_key_down(cx.listener(Self::on_dialog_key))
            .child(self.dialog_heading(
                theme,
                title,
                "Your existing conversations will be kept.",
                cx,
            ))
            .child(
                div()
                    .px(px(24.0))
                    .py(px(20.0))
                    .child(popover::dialog_body(theme, copy))
                    .when_some(self.error.clone(), |el, error| {
                        el.child(widgets::error_strip(theme, error))
                    }),
            )
            .child(
                div()
                    .px(px(24.0))
                    .py(px(16.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        button(
                            theme,
                            "provider-confirm-cancel",
                            "Cancel",
                            ButtonStyle::Secondary,
                            !busy,
                        )
                        .track_focus(&self.cancel_focus)
                        .when(!busy, |el| {
                            el.on_click(
                                cx.listener(|page, _, window, cx| page.close_dialog(window, cx)),
                            )
                        }),
                    )
                    .child(
                        button(
                            theme,
                            "provider-confirm",
                            if busy {
                                "Removing…"
                            } else if remove {
                                "Delete provider"
                            } else {
                                "Remove API key"
                            },
                            ButtonStyle::Danger,
                            !busy && self.target.read(cx).can_write(cx),
                        )
                        .track_focus(&self.submit_focus)
                        .when(!busy, |el| {
                            el.on_click(cx.listener(move |page, _, _, cx| {
                                page.call(
                                    if remove {
                                        methods::REMOVE_PI_PROVIDER
                                    } else {
                                        methods::LOGOUT_PI_PROVIDER
                                    },
                                    serde_json::json!({ "id": id }),
                                    cx,
                                )
                            }))
                        }),
                    ),
            )
            .into_any_element()
    }

    fn render_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let menu = self.menu.get().unwrap().clone();
        let closing = self.menu.closing_since();
        let mut card = popover::popover_card(theme)
            .id("provider-actions-menu")
            .role(gpui::Role::Menu)
            .aria_label("Provider actions")
            .track_focus(&self.menu_focus)
            .w(px(204.0))
            .on_mouse_down_out(cx.listener(|page, _, window, cx| page.close_menu(window, cx)))
            .on_key_down(cx.listener(|page, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" | "tab" => {
                        page.close_menu(window, cx);
                        page.page_focus.focus(window, cx);
                    }
                    "up" | "down" => {
                        if let Some(menu) = page.menu.open_mut() {
                            let delta = if event.keystroke.key == "up" { -1 } else { 1 };
                            menu.active =
                                popover::menu_step(Some(menu.active), 3, delta).unwrap_or(0);
                            if !menu.provider.credential_saved {
                                menu.active = 2;
                            }
                        }
                    }
                    "enter" | "space" => {
                        if let Some(menu) = page.menu.as_open().cloned() {
                            page.menu_action(menu.provider, menu.active, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }));
        for (index, (label, glyph)) in [
            ("Refresh models", icons::REFRESH),
            ("Remove API key", icons::KEY_MINIMALISTIC),
            ("Delete provider…", icons::TRASH_BIN_MINIMALISTIC),
        ]
        .into_iter()
        .enumerate()
        {
            let enabled = self.busy.is_none()
                && (index == 2 || menu.provider.credential_saved)
                && closing.is_none();
            let provider = menu.provider.clone();
            if index == 2 {
                card = card.child(popover::menu_separator());
            }
            card = card.child(
                popover::menu_row(
                    theme,
                    index == menu.active && enabled,
                    format!("provider-menu-{index}"),
                )
                .id(("provider-menu-action", index))
                .role(gpui::Role::MenuItem)
                .aria_label(label)
                .min_h(px(32.0))
                .when(index == 2, |el| el.text_color(theme.danger_muted))
                .when(!enabled, |el| el.opacity(0.4))
                .when(enabled, |el| {
                    el.on_click(cx.listener(move |page, _, window, cx| {
                        page.menu_action(provider.clone(), index, window, cx)
                    }))
                })
                .child(provider_icon(
                    glyph,
                    15.0,
                    if index == 2 {
                        theme.danger_muted
                    } else {
                        theme.text_muted
                    },
                ))
                .child(SharedString::from(label)),
            );
        }
        popover::anchored_menu_below("provider-menu-layer", card.into_any_element(), closing)
    }

    fn provider_row(
        &mut self,
        provider: PiProviderInfo,
        index: usize,
        current: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.busy.is_some() || !self.target.read(cx).can_write(cx);
        let refreshing = self.busy.as_ref().is_some_and(|b| {
            b.method == methods::REFRESH_PI_PROVIDER && b.provider.as_deref() == Some(&provider.id)
        });
        let color = status_color(theme, &provider.state);
        let status = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(5.0))
            .rounded_full()
            .px(px(8.0))
            .py(px(3.0))
            .bg(color.opacity(0.08))
            .text_size(px(11.0))
            .text_color(color)
            .child(div().size(px(5.0)).rounded_full().bg(color))
            .child(SharedString::from(if refreshing {
                "Refreshing…"
            } else {
                status_label(&provider)
            }));
        let model_label = format!(
            "{} {}",
            provider.model_count,
            if provider.model_count == 1 {
                "model"
            } else {
                "models"
            }
        );
        let edit = provider.clone();
        let menu_provider = provider.clone();
        let menu_id = provider.id.clone();
        let menu_open = self
            .menu
            .get()
            .is_some_and(|m| m.provider.id == provider.id);
        let selected = current.filter(|model| model.starts_with(&format!("{}/", provider.id)));
        let mut more = button(
            theme,
            ("provider-more", index),
            "•••",
            ButtonStyle::Ghost,
            !busy,
        )
        .w(px(32.0))
        .relative()
        .px_0()
        .text_size(px(10.0))
        .aria_label(format!("More actions for {}", provider.id))
        .when(menu_open, |el| el.bg(theme.element_active))
        .tooltip(|_, cx| cx.new(|_| Hint("More actions".into())).into())
        .when(!busy, |el| {
            el.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |page, _, _, _| {
                    page.menu
                        .note_trigger_press_matching(|menu| menu.provider.id == menu_id);
                }),
            )
            .on_click(cx.listener(move |page, _, window, cx| {
                if page.menu.take_press_was_open() {
                    page.close_menu(window, cx);
                } else {
                    page.menu.open(ProviderMenu {
                        provider: menu_provider.clone(),
                        active: if menu_provider.credential_saved { 0 } else { 2 },
                    });
                    page.menu_focus.focus(window, cx);
                }
                cx.notify();
            }))
        });
        if menu_open {
            more = more.child(self.render_menu(theme, cx));
        }
        div()
            .px(px(20.0))
            .py(px(16.0))
            .when(index > 0, |el| el.border_t_1().border_color(theme.border))
            .flex()
            .items_start()
            .gap(px(12.0))
            .child(
                widgets::row_tile(theme, icons::GLOBAL)
                    .size(px(36.0))
                    .bg(theme.accent.opacity(0.06)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                widgets::row_title(theme, provider.id.clone()).text_size(px(14.0)),
                            )
                            .child(status)
                            .when_some(selected, |el, _| el.child(widgets::badge(theme, "In use"))),
                    )
                    .child(caption(theme, provider.base_url.clone()).truncate())
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(6.0))
                            .child(caption(theme, "OpenAI-compatible").text_size(px(11.5)))
                            .child(caption(theme, "·"))
                            .child(caption(theme, model_label).text_size(px(11.5)))
                            .child(caption(theme, "·"))
                            .child(
                                caption(
                                    theme,
                                    checked_label(
                                        provider.checked_at,
                                        chrono::Utc::now().timestamp_millis(),
                                    ),
                                )
                                .text_size(px(11.5)),
                            ),
                    )
                    .when_some(provider.message, |el, message| {
                        el.child(
                            div()
                                .mt(px(4.0))
                                .flex()
                                .items_start()
                                .gap(px(6.0))
                                .child(provider_icon(icons::DANGER_TRIANGLE, 14.0, theme.danger))
                                .child(caption(theme, message).text_color(theme.danger_muted)),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        button(
                            theme,
                            ("provider-manage", index),
                            if provider.credential_saved {
                                "Manage"
                            } else {
                                "Connect"
                            },
                            ButtonStyle::Secondary,
                            !busy,
                        )
                        .when(!busy, |el| {
                            el.on_click(
                                cx.listener(move |page, _, _, cx| {
                                    page.edit(Some(edit.clone()), cx)
                                }),
                            )
                        }),
                    )
                    .child(more),
            )
            .into_any_element()
    }

    fn render_empty(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        widgets::section_card(theme).mt(px(16.0)).px(px(32.0)).py(px(48.0))
            .items_center().gap(px(12.0))
            .child(div().size(px(56.0)).rounded(px(16.0)).bg(theme.accent.opacity(0.07))
                .border_1().border_color(theme.accent.opacity(0.12)).flex().items_center().justify_center()
                .child(provider_icon(icons::GLOBAL, 26.0, theme.accent)))
            .child(div().mt(px(4.0)).text_size(px(16.0)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme.text)
                .child("Connect your first provider"))
            .child(caption(theme, "Bring your own API key to use models from NewAPI\nor another OpenAI-compatible service.")
                .text_center().max_w(px(340.0)))
            .child(add_provider_button(theme, "provider-empty-add", self.busy.is_none() && self.target.read(cx).can_write(cx))
                .mt(px(8.0)).h(px(36.0))
                .when(self.busy.is_none(), |el| el.on_click(cx.listener(|page, _, _, cx| page.edit(None, cx)))))
            .into_any_element()
    }
}

impl Render for ProvidersPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let dialog_open = self.form.is_some() || self.confirm.is_some();
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
                        .filter(|focus| focus != &self.menu_focus && focus != &self.dialog_focus)
                        .unwrap_or_else(|| self.page_focus.clone()),
                );
            }
            if let Some(form) = &mut self.form
                && let Some(field) = form.focus.take()
            {
                form.input(field).focus_handle(cx).focus(window, cx);
            }
            if std::mem::take(&mut self.confirm_focus) {
                self.cancel_focus.focus(window, cx);
            }
            if self.busy.is_some() {
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
            .when(count.is_some_and(|n| n > 0), |el| {
                el.child(
                    add_provider_button(&theme, "provider-add", !blocked).when(!blocked, |el| {
                        el.on_click(cx.listener(|page, _, _, cx| page.edit(None, cx)))
                    }),
                )
            });
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
            let rows = snapshot
                .providers
                .into_iter()
                .enumerate()
                .map(|(index, provider)| {
                    self.provider_row(provider, index, current.as_deref(), &theme, cx)
                })
                .collect::<Vec<_>>();
            body = body.child(widgets::section_card(&theme).mt(px(12.0)).children(rows));
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
    fn only_exact_management_commands_open_settings() {
        assert_eq!(command_intent("/provider add"), Some(ProviderIntent::Add));
        assert_eq!(
            command_intent("/login mvp-lab"),
            Some(ProviderIntent::Edit("mvp-lab".into()))
        );
        assert_eq!(
            command_intent("/logout mvp-lab"),
            Some(ProviderIntent::Logout("mvp-lab".into()))
        );
        assert_eq!(
            command_intent("/login x secret"),
            Some(ProviderIntent::Edit("x".into()))
        );
        assert_eq!(command_intent("explain /provider"), None);
        assert_eq!(command_intent("/newapi-provider-add"), None);
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
                let background = crate::theme::flatten(color.opacity(0.08), theme.surface_card);
                assert!(
                    crate::theme::contrast_ratio(color, background) >= 4.5,
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
            assert!(crate::theme::contrast_ratio(theme.on_solid, theme.solid) >= 4.5);
        }
    }
}
