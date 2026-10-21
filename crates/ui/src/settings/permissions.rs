//! Settings → Permissions: what agents on the selected device may touch.
//!
//! Device-scoped like Translation: the device's Runtime extension
//! (`cypher-permissions`) owns every decision and reads `permissions.json` at
//! the start of each turn; this page edits the part of that file it shows and
//! reports whether the device can sandbox at all. A master switch at the top
//! turns the extension on; it is off by default. Design:
//! `docs/design/permission-control.md`.

use cypher_engine::mcp::McpSnapshot;
use cypher_engine::pi::permissions::{
    ApprovalMode, CommandDecision, NetworkMode, PermissionApprovals, PiPermissionSettings,
    PiPermissionSnapshot, ToolDecision,
};
use cypher_proto::Model;
use cypher_rpc::methods;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, Render, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};

use super::device_target::DeviceTarget;
use super::widgets;
use crate::{
    kit::icons,
    kit::popover::{self, Loadable},
    kit::theme::{MonoStyled, Theme},
    state::AppState,
    widgets::text_input::{TextInput, TextInputEvent},
};

/// Which elevation an approval row edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ApprovalKey {
    ExternalWrite,
    ProtectedWrite,
    SensitiveRead,
    SensitiveWrite,
    Network,
    Tools,
    Escalate,
}

impl ApprovalKey {
    const ALL: [Self; 7] = [
        Self::Escalate,
        Self::ExternalWrite,
        Self::ProtectedWrite,
        Self::SensitiveRead,
        Self::SensitiveWrite,
        Self::Network,
        Self::Tools,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::ExternalWrite => "Writing outside the workspace",
            Self::ProtectedWrite => "Writing protected files",
            Self::SensitiveRead => "Reading sensitive files",
            Self::SensitiveWrite => "Writing sensitive files",
            Self::Network => "Network beyond the filter",
            Self::Tools => "Tools set to Ask",
            Self::Escalate => "Running commands outside the sandbox",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::ExternalWrite => "Other projects and folders. Reading them is always allowed.",
            Self::ProtectedWrite => {
                "Git config and hooks, shell startup files, launch agents: a write there runs code later, so the reviewing model never approves one."
            }
            Self::SensitiveRead => {
                "Keys, .env files and credentials, for the file tools. Sandboxed commands never read them."
            }
            Self::SensitiveWrite => "Keys, .env files and credentials, for the file tools.",
            Self::Network => "With the filtered network: a domain not on the list.",
            Self::Tools => "MCP and extension tools whose setting below is Ask.",
            Self::Escalate => {
                "A command that needs more than the sandbox (ssh, gh, other processes) runs as you. Inspections run without asking; dangerous commands (publishing, deleting outside the workspace, secrets) always ask you."
            }
        }
    }

    fn get(self, approvals: &PermissionApprovals) -> ApprovalMode {
        match self {
            Self::ExternalWrite => approvals.external_write,
            Self::ProtectedWrite => approvals.protected_write,
            Self::SensitiveRead => approvals.sensitive_read,
            Self::SensitiveWrite => approvals.sensitive_write,
            Self::Network => approvals.network,
            Self::Tools => approvals.tools,
            Self::Escalate => approvals.escalate,
        }
    }

    fn set(self, approvals: &mut PermissionApprovals, mode: ApprovalMode) {
        match self {
            Self::ExternalWrite => approvals.external_write = mode,
            Self::ProtectedWrite => approvals.protected_write = mode,
            Self::SensitiveRead => approvals.sensitive_read = mode,
            Self::SensitiveWrite => approvals.sensitive_write = mode,
            Self::Network => approvals.network = mode,
            Self::Tools => approvals.tools = mode,
            Self::Escalate => approvals.escalate = mode,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::ExternalWrite => "external-write",
            Self::ProtectedWrite => "protected-write",
            Self::SensitiveRead => "sensitive-read",
            Self::SensitiveWrite => "sensitive-write",
            Self::Network => "network",
            Self::Tools => "tools",
            Self::Escalate => "escalate",
        }
    }
}

fn mode_label(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Deny => "Deny",
        ApprovalMode::Ask => "Ask me",
        ApprovalMode::Assisted => "Model first",
    }
}

fn decision_label(decision: ToolDecision) -> &'static str {
    match decision {
        ToolDecision::Allow => "Allow",
        ToolDecision::Elevate => "Ask",
        ToolDecision::Deny => "Deny",
    }
}

/// The extra-pattern lists, edited as one text block each.
struct ListInputs {
    sensitive: Entity<TextInput>,
    protected: Entity<TextInput>,
    writable: Entity<TextInput>,
    sockets: Entity<TextInput>,
    domains: Entity<TextInput>,
}

pub struct PermissionsPage {
    state: Entity<AppState>,
    target: Entity<DeviceTarget>,
    generation: u64,
    snapshot: Loadable<PiPermissionSnapshot>,
    models: Loadable<Vec<Model>>,
    /// MCP servers configured on the device, for per-server overrides.
    servers: Vec<String>,
    lists: ListInputs,
    model_search: Entity<TextInput>,
    model_menu_open: bool,
    model_trigger: FocusHandle,
    busy: bool,
    error: Option<String>,
    notice: Option<String>,
    task: Option<Task<()>>,
    _target_observer: Subscription,
    _input_observers: Vec<Subscription>,
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

impl PermissionsPage {
    pub fn new(
        state: Entity<AppState>,
        target: Entity<DeviceTarget>,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = |placeholder: &str, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| TextInput::settings_prompt_field(placeholder, cx))
        };
        let lists = ListInputs {
            sensitive: field("e.g. **/secrets.yaml", cx),
            protected: field("e.g. <root>/Makefile", cx),
            writable: field("e.g. ~/.m2/repository/**", cx),
            sockets: field("e.g. ~/.docker/run/docker.sock", cx),
            domains: field("e.g. api.example.com", cx),
        };
        let model_search = cx.new(|cx| TextInput::settings_field("Search models…", false, cx));
        let mut input_observers = Vec::new();
        for input in [
            &lists.sensitive,
            &lists.protected,
            &lists.writable,
            &lists.sockets,
            &lists.domains,
            &model_search,
        ] {
            input_observers.push(cx.subscribe(input, |page: &mut Self, _, event, cx| {
                if matches!(event, TextInputEvent::Edited) {
                    page.notice = None;
                    cx.notify();
                }
            }));
        }
        let generation = target.read(cx).generation();
        let observer = cx.observe(&target, |page: &mut Self, target, cx| {
            let generation = target.read(cx).generation();
            if generation != page.generation {
                page.generation = generation;
                page.snapshot = Loadable::Idle;
                page.models = Loadable::Idle;
                page.servers.clear();
                page.busy = false;
                page.error = None;
                page.notice = None;
                page.task = None;
                page.model_menu_open = false;
                page.load(cx);
            }
            cx.notify();
        });
        let mut page = Self {
            state,
            target,
            generation,
            snapshot: Loadable::Idle,
            models: Loadable::Idle,
            servers: Vec::new(),
            lists,
            model_search,
            model_menu_open: false,
            model_trigger: cx.focus_handle(),
            busy: false,
            error: None,
            notice: None,
            task: None,
            _target_observer: observer,
            _input_observers: input_observers,
        };
        page.load(cx);
        page
    }

    fn fill_lists(&mut self, settings: &PiPermissionSettings, cx: &mut Context<Self>) {
        for (input, list) in [
            (&self.lists.sensitive, &settings.sensitive),
            (&self.lists.protected, &settings.protected),
            (&self.lists.writable, &settings.writable_support),
            (&self.lists.sockets, &settings.unix_sockets),
            (&self.lists.domains, &settings.allow_domains),
        ] {
            let text = list.join("\n");
            input.update(cx, |input, cx| input.set_text(text, cx));
        }
    }

    fn lists_dirty(&self, cx: &Context<Self>) -> bool {
        let Some(snapshot) = self.snapshot.ready() else {
            return false;
        };
        let settings = &snapshot.settings;
        [
            (&self.lists.sensitive, &settings.sensitive),
            (&self.lists.protected, &settings.protected),
            (&self.lists.writable, &settings.writable_support),
            (&self.lists.sockets, &settings.unix_sockets),
            (&self.lists.domains, &settings.allow_domains),
        ]
        .iter()
        .any(|(input, list)| lines(input.read(cx).text()) != **list)
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let ticket = match self.target.read(cx).ticket(cx) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.snapshot = Loadable::Error(error);
                cx.notify();
                return;
            }
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.snapshot = Loadable::Loading;
        self.error = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::GET_PI_PERMISSION_SETTINGS,
                    ticket.params(serde_json::json!({})),
                )
                .await;
            let loaded = this
                .update(cx, |page, cx| {
                    if !page.target.read(cx).matches(&ticket) {
                        return false;
                    }
                    page.snapshot = match result {
                        Ok(value) => match serde_json::from_value::<PiPermissionSnapshot>(value) {
                            Ok(snapshot) => {
                                page.fill_lists(&snapshot.settings, cx);
                                Loadable::Ready(snapshot)
                            }
                            Err(error) => Loadable::Error(error.to_string()),
                        },
                        Err(error) => Loadable::Error(format!("{}: {error}", ticket.label)),
                    };
                    cx.notify();
                    matches!(page.snapshot, Loadable::Ready(_))
                })
                .unwrap_or(false);
            if !loaded {
                return;
            }
            // The judge picker and the per-server rows are extras: a device
            // without models or MCP still shows the rest.
            let models = engine
                .client()
                .call(
                    methods::LIST_MODELS,
                    ticket.params(serde_json::json!({"harness": "pi"})),
                )
                .await;
            let servers = engine
                .client()
                .call(
                    methods::LIST_MCP_SERVERS,
                    ticket.params(serde_json::json!({})),
                )
                .await;
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.models = match models {
                    Ok(value) => match serde_json::from_value::<Vec<Model>>(value) {
                        Ok(mut models) => {
                            models.sort_by(|a, b| {
                                a.label
                                    .to_lowercase()
                                    .cmp(&b.label.to_lowercase())
                                    .then(a.id.cmp(&b.id))
                            });
                            Loadable::Ready(models)
                        }
                        Err(error) => Loadable::Error(error.to_string()),
                    },
                    Err(error) => Loadable::Error(error.to_string()),
                };
                page.servers = servers
                    .ok()
                    .and_then(|value| serde_json::from_value::<McpSnapshot>(value).ok())
                    .map(|snapshot| snapshot.servers.into_iter().map(|s| s.name).collect())
                    .unwrap_or_default();
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn editable(&self, cx: &Context<Self>) -> bool {
        !self.busy
            && self.target.read(cx).can_write(cx)
            && matches!(self.snapshot, Loadable::Ready(_))
    }

    /// Save a change to the current settings. `notice` is shown on success.
    fn update(
        &mut self,
        change: impl FnOnce(&mut PiPermissionSettings),
        notice: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if !self.editable(cx) || self.generation != self.target.read(cx).generation() {
            return;
        }
        let Some(snapshot) = self.snapshot.ready() else {
            return;
        };
        let mut settings = snapshot.settings.clone();
        change(&mut settings);
        if let Err(error) = settings.validate() {
            self.error = Some(error);
            cx.notify();
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
        self.notice = None;
        let notice = notice.map(str::to_owned);
        let target = self.target.clone();
        let lease = target.update(cx, |target, cx| target.lock(cx));
        let body = match serde_json::to_value(&settings) {
            Ok(body) => body,
            Err(error) => {
                self.busy = false;
                self.error = Some(error.to_string());
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::SET_PI_PERMISSION_SETTINGS, ticket.params(body))
                .await;
            drop(lease);
            target.update(cx, |_, cx| cx.notify());
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.busy = false;
                page.model_menu_open = false;
                match result {
                    Ok(value) => match serde_json::from_value::<PiPermissionSnapshot>(value) {
                        Ok(snapshot) => {
                            page.fill_lists(&snapshot.settings, cx);
                            page.snapshot = Loadable::Ready(snapshot);
                            page.notice = notice;
                        }
                        Err(error) => page.error = Some(error.to_string()),
                    },
                    Err(error) => {
                        page.error = Some(format!("Couldn't save on {}: {error}", ticket.label))
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn save_lists(&mut self, cx: &mut Context<Self>) {
        let sensitive = lines(self.lists.sensitive.read(cx).text());
        let protected = lines(self.lists.protected.read(cx).text());
        let writable = lines(self.lists.writable.read(cx).text());
        let sockets = lines(self.lists.sockets.read(cx).text());
        let domains = lines(self.lists.domains.read(cx).text());
        self.update(
            move |settings| {
                settings.sensitive = sensitive;
                settings.protected = protected;
                settings.writable_support = writable;
                settings.unix_sockets = sockets;
                settings.allow_domains = domains;
            },
            Some("Saved. New turns use these rules."),
            cx,
        );
    }

    fn toggle_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable(cx) {
            return;
        }
        self.model_menu_open = !self.model_menu_open;
        if self.model_menu_open {
            self.model_search
                .update(cx, |input, cx| input.set_text("", cx));
            self.model_search.focus_handle(cx).focus(window, cx);
        } else {
            self.model_trigger.focus(window, cx);
        }
        cx.notify();
    }

    fn model_label(&self, id: &str) -> String {
        if id.is_empty() {
            return "The chat's model".into();
        }
        self.models
            .ready()
            .and_then(|models| models.iter().find(|model| model.id == id))
            .map(|model| model.label.clone())
            .unwrap_or_else(|| id.to_string())
    }

    // --- Rendering -------------------------------------------------------------

    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        id: String,
        options: Vec<(&'static str, T)>,
        selected: T,
        theme: &Theme,
        cx: &mut Context<Self>,
        pick: impl Fn(&mut Self, T, &mut Context<Self>) + Clone + 'static,
    ) -> AnyElement {
        let enabled = self.editable(cx);
        let mut row = div()
            .flex_none()
            .flex()
            .flex_row()
            .p(px(2.0))
            .gap(px(2.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.ink(0.02));
        for (label, value) in options {
            let active = value == selected;
            let pick = pick.clone();
            row = row.child(
                div()
                    .id(SharedString::from(format!("{id}-{label}")))
                    .debug_selector({
                        let selector = format!("{id}-{label}");
                        move || selector.clone()
                    })
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .text_size(px(12.0))
                    .text_color(if active { theme.text } else { theme.text_muted })
                    .when(active, |el| el.bg(theme.ink(0.08)))
                    .when(enabled && !active, |el| {
                        el.cursor_pointer().hover(|s| s.bg(theme.ink(0.04)))
                    })
                    .when(!enabled, |el| el.opacity(0.5))
                    .when(enabled && !active, |el| {
                        el.on_click(cx.listener(move |page, _, _, cx| pick(page, value, cx)))
                    })
                    .child(label),
            );
        }
        row.into_any_element()
    }

    fn setting_row(
        theme: &Theme,
        first: bool,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        control: AnyElement,
    ) -> gpui::Div {
        widgets::card_row(theme, first)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(widgets::row_title(theme, title))
                    .child(
                        div()
                            .mt(px(3.0))
                            .text_size(px(12.0))
                            .line_height(px(17.0))
                            .text_color(theme.text_muted)
                            .child(detail.into()),
                    ),
            )
            .child(control)
    }

    fn section_heading(theme: &Theme, title: &str, copy: &str) -> gpui::Div {
        div()
            .mt(px(28.0))
            .child(widgets::field_label(theme, title.to_string()))
            .child(
                widgets::page_subtitle(theme, copy.to_string())
                    .max_w(px(600.0))
                    .line_height(px(19.0)),
            )
    }

    /// The master switch, with what it means on this device.
    fn status_card(
        &self,
        snapshot: &PiPermissionSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let status = &snapshot.status;
        let on = snapshot.settings.enabled;
        let (title, detail, badge) = if !status.installed {
            (
                "Permission control is not installed",
                status
                    .error
                    .clone()
                    .unwrap_or_else(|| "Update this device's Pi Runtime.".into()),
                Some(widgets::badge(theme, "Off")),
            )
        } else if !on {
            (
                "Permission control is off",
                "Agents read, write and run commands without checks or approval requests. Turn it on to sandbox shell commands and check every file access. Applies from the next turn.".into(),
                None,
            )
        } else if !status.sandbox_available {
            (
                "Permission control is unavailable on this device",
                format!(
                    "{} Commands and file tools run without restrictions.",
                    status
                        .error
                        .clone()
                        .unwrap_or_else(|| "The OS sandbox does not work here.".into())
                ),
                None,
            )
        } else {
            let mechanism = match status.implementation.as_deref() {
                Some("seatbelt") => "macOS Seatbelt",
                Some(other) if other.starts_with("landlock") => "Linux Landlock",
                Some(other) => other,
                None => "the OS sandbox",
            };
            (
                "Permission control is on",
                format!(
                    "Shell commands run in {mechanism}. Each chat's level is set with /permissions in the composer."
                ),
                None,
            )
        };
        let can_toggle = status.installed && self.editable(cx);
        let control = match badge {
            Some(badge) => badge.into_any_element(),
            None => div()
                .id("permissions-enabled-toggle")
                .debug_selector(|| "permissions-enabled-toggle".into())
                .flex_none()
                .when(can_toggle, |el| el.cursor_pointer())
                .opacity(if can_toggle { 1.0 } else { 0.5 })
                .on_click(cx.listener(move |page, _, _, cx| {
                    if can_toggle {
                        page.update(move |settings| settings.enabled = !on, None, cx);
                    }
                }))
                .child(widgets::toggle_switch(theme, on))
                .into_any_element(),
        };
        widgets::section_card(theme)
            .child(
                div()
                    .px(px(20.0))
                    .py(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(widgets::row_tile(theme, icons::SHIELD_CHECK))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::row_title(theme, title))
                            .child(
                                div()
                                    .mt(px(3.0))
                                    .text_size(px(12.0))
                                    .line_height(px(17.0))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(detail)),
                            ),
                    )
                    .child(control),
            )
            .into_any_element()
    }

    fn approvals_card(
        &self,
        settings: &PiPermissionSettings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut card = widgets::section_card(theme).mt(px(12.0));
        for (index, key) in ApprovalKey::ALL.into_iter().enumerate() {
            let mut options = vec![
                (mode_label(ApprovalMode::Deny), ApprovalMode::Deny),
                (mode_label(ApprovalMode::Ask), ApprovalMode::Ask),
                (mode_label(ApprovalMode::Assisted), ApprovalMode::Assisted),
            ];
            // A protected write is a human's: the extension reads "Model first" as "Ask me".
            let mut current = key.get(&settings.approval);
            if key == ApprovalKey::ProtectedWrite {
                options.pop();
                if current == ApprovalMode::Assisted {
                    current = ApprovalMode::Ask;
                }
            }
            let control = self.segmented(
                format!("permissions-approval-{}", key.id()),
                options,
                current,
                theme,
                cx,
                move |page, mode, cx| {
                    page.update(
                        move |settings| key.set(&mut settings.approval, mode),
                        None,
                        cx,
                    )
                },
            );
            card = card.child(Self::setting_row(
                theme,
                index == 0,
                key.title(),
                key.detail(),
                control,
            ));
        }
        card = card.child(Self::setting_row(
            theme,
            false,
            "Cypher's own settings and data",
            "Writes always ask you: the reviewing model never approves changes to its own rules.",
            widgets::badge(theme, "Ask me").into_any_element(),
        ));
        card.into_any_element()
    }

    fn judge_card(
        &mut self,
        settings: &PiPermissionSettings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let enabled = self.editable(cx);
        let label = self.model_label(&settings.judge_model);
        let trigger = div()
            .id("permissions-judge-model")
            .debug_selector(|| "permissions-judge-model".into())
            .track_focus(&self.model_trigger)
            .flex_none()
            .max_w(px(260.0))
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.input_glass_bg())
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(px(12.0))
            .text_color(theme.text)
            .when(enabled, |el| el.cursor_pointer())
            .when(!enabled, |el| el.opacity(0.5))
            .on_click(cx.listener(|page, _, window, cx| page.toggle_model_menu(window, cx)))
            .child(div().truncate().child(SharedString::from(label)))
            .child(
                icons::icon(icons::ALT_ARROW_DOWN)
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            );
        let mut control = div().relative().child(trigger);
        if self.model_menu_open {
            control = control.child(
                gpui::deferred(
                    gpui::anchored()
                        .snap_to_window_with_margin(px(8.0))
                        .child(self.model_popup(&settings.judge_model, theme, cx)),
                )
                .with_priority(1),
            );
        }
        widgets::section_card(theme)
            .mt(px(12.0))
            .child(Self::setting_row(
                theme,
                true,
                "Reviewing model",
                "\"Model first\" asks this model whether a request plainly serves what you asked for. It has two answers: approve it once, or ask you. It never rejects; if it is unsure or fails, you are asked.",
                control.into_any_element(),
            ))
            .into_any_element()
    }

    fn model_popup(&mut self, current: &str, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let row = |id: String,
                   title: String,
                   subtitle: String,
                   selected: bool,
                   cx: &mut Context<Self>| {
            let pick = id.clone();
            div()
                .id(SharedString::from(format!(
                    "permissions-judge-{}",
                    if id.is_empty() { "chat" } else { id.as_str() }
                )))
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .cursor_pointer()
                .when(selected, |el| el.bg(theme.ink(0.07)))
                .hover(|s| s.bg(theme.ink(0.05)))
                .on_click(cx.listener(move |page, _, _, cx| {
                    let model = pick.clone();
                    page.update(move |settings| settings.judge_model = model, None, cx);
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(13.0))
                                .text_color(theme.text)
                                .child(SharedString::from(title)),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(11.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(subtitle)),
                        ),
                )
                .child(div().w(px(16.0)).when(selected, |el| {
                    el.child(
                        icons::icon(icons::CHECK)
                            .size(px(12.0))
                            .text_color(theme.accent),
                    )
                }))
                .into_any_element()
        };
        let mut list = div()
            .id("permissions-judge-list")
            .max_h(px(280.0))
            .overflow_y_scroll()
            .px(px(4.0))
            .pb(px(4.0))
            .child(row(
                String::new(),
                "The chat's model".into(),
                "Whatever model the chat runs on".into(),
                current.is_empty(),
                cx,
            ));
        match self.models.clone() {
            Loadable::Ready(models) => {
                let query = self.model_search.read(cx).text().trim().to_lowercase();
                for model in models.iter().filter(|model| {
                    query.is_empty()
                        || model.label.to_lowercase().contains(&query)
                        || model.id.to_lowercase().contains(&query)
                }) {
                    let provider = model
                        .id
                        .split_once('/')
                        .map(|(provider, _)| provider.to_string())
                        .unwrap_or_default();
                    list = list.child(row(
                        model.id.clone(),
                        model.label.clone(),
                        provider,
                        model.id == current,
                        cx,
                    ));
                }
            }
            Loadable::Error(error) => {
                list = list.child(
                    div()
                        .p(px(10.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(format!("Couldn't load models: {error}"))),
                );
            }
            Loadable::Idle | Loadable::Loading => {
                list = list.child(
                    div()
                        .p(px(10.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child("Loading models…"),
                );
            }
        }
        div()
            .mt(px(4.0))
            .w(px(300.0))
            .rounded(px(10.0))
            .bg(theme.surface_overlay)
            .border_1()
            .border_color(theme.border_strong)
            .on_mouse_down_out(cx.listener(|page, _, window, cx| {
                page.model_menu_open = false;
                page.model_trigger.focus(window, cx);
                cx.notify();
            }))
            .child(div().p(px(8.0)).child(self.model_search.clone()))
            .child(list)
            .into_any_element()
    }

    fn tools_card(
        &self,
        settings: &PiPermissionSettings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let decisions = || {
            vec![
                (decision_label(ToolDecision::Allow), ToolDecision::Allow),
                (decision_label(ToolDecision::Elevate), ToolDecision::Elevate),
                (decision_label(ToolDecision::Deny), ToolDecision::Deny),
            ]
        };
        let default = self.segmented(
            "permissions-tools-default".into(),
            decisions(),
            settings.tools_default,
            theme,
            cx,
            |page, decision, cx| {
                page.update(move |settings| settings.tools_default = decision, None, cx)
            },
        );
        let mut card = widgets::section_card(theme)
            .mt(px(12.0))
            .child(Self::setting_row(
                theme,
                true,
                "All MCP and extension tools",
                "Their internals are not sandboxed; Ask puts each first use in this chat through approval.",
                default,
            ));
        let mut names: Vec<String> = self.servers.clone();
        for key in settings.tool_overrides.keys() {
            if let Some(server) = key.strip_prefix("mcp:")
                && !names.iter().any(|name| name == server)
            {
                names.push(server.to_string());
            }
        }
        for server in names {
            let key = format!("mcp:{server}");
            let current = settings.tool_overrides.get(&key).copied();
            let mut options: Vec<(&'static str, Option<ToolDecision>)> = vec![("Default", None)];
            options.extend(
                decisions()
                    .into_iter()
                    .map(|(label, decision)| (label, Some(decision))),
            );
            let pick_key = key.clone();
            let control = self.segmented(
                format!("permissions-server-{server}"),
                options,
                current,
                theme,
                cx,
                move |page, decision, cx| {
                    let key = pick_key.clone();
                    page.update(
                        move |settings| match decision {
                            Some(decision) => {
                                settings.tool_overrides.insert(key, decision);
                            }
                            None => {
                                settings.tool_overrides.remove(&key);
                            }
                        },
                        None,
                        cx,
                    )
                },
            );
            card = card.child(Self::setting_row(
                theme,
                false,
                format!("MCP: {server}"),
                "Overrides the default for this server's tools.",
                control,
            ));
        }
        card.into_any_element()
    }

    fn list_field(theme: &Theme, label: &str, copy: &str, input: &Entity<TextInput>) -> gpui::Div {
        div()
            .mt(px(14.0))
            .child(widgets::field_label(theme, label.to_string()))
            .child(
                div()
                    .mt(px(2.0))
                    .mb(px(6.0))
                    .text_size(px(12.0))
                    .line_height(px(17.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(copy.to_string())),
            )
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
    }

    fn rules_card(
        &self,
        settings: &PiPermissionSettings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let enabled = self.editable(cx);
        let local = self.segmented(
            "permissions-local-binding".into(),
            vec![("Allow", true), ("Block", false)],
            settings.local_binding,
            theme,
            cx,
            |page, on, cx| page.update(move |settings| settings.local_binding = on, None, cx),
        );
        let network = self.segmented(
            "permissions-network-mode".into(),
            vec![
                ("Open", NetworkMode::Open),
                ("Filtered", NetworkMode::Filtered),
            ],
            settings.network_mode,
            theme,
            cx,
            |page, mode, cx| page.update(move |settings| settings.network_mode = mode, None, cx),
        );
        let dirty = self.lists_dirty(cx);
        widgets::section_card(theme)
            .mt(px(12.0))
            .child(Self::setting_row(
                theme,
                true,
                "Network",
                "Open: commands reach the internet directly. Filtered: only package registries, code hosts and the domains below pass; anything else asks.",
                network,
            ))
            .child(Self::setting_row(
                theme,
                false,
                "Local servers",
                "Let commands listen on and connect to localhost: dev servers and test suites.",
                local,
            ))
            .child(
                div()
                    .px(px(20.0))
                    .pb(px(16.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(Self::list_field(
                        theme,
                        "More sensitive files",
                        "Neither read nor written without your approval. Keys, .env files, ~/.ssh and cloud and browser credentials are built in. One per line: ~/path, <root>/path or **/name.",
                        &self.lists.sensitive,
                    ))
                    .child(Self::list_field(
                        theme,
                        "More protected files",
                        "Readable, but writing them needs approval (the reviewing model may approve). Git config and hooks, shell startup files and launch agents are built in.",
                        &self.lists.protected,
                    ))
                    .child(Self::list_field(
                        theme,
                        "More writable folders",
                        "Folders shell commands may write besides the workspace and temp. The npm, Cargo, Go and Gradle caches are built in.",
                        &self.lists.writable,
                    ))
                    .child(Self::list_field(
                        theme,
                        "More allowed domains",
                        "Reachable without asking when the network is Filtered. GitHub, npm, PyPI, crates.io, Go, Maven, RubyGems and Docker Hub are built in; *.example.com covers subdomains.",
                        &self.lists.domains,
                    ))
                    .child(Self::list_field(
                        theme,
                        "Unix sockets",
                        "Commands can't use sockets unless listed here. Docker, ssh-agent and GPG agents each need one.",
                        &self.lists.sockets,
                    ))
                    .child(
                        div()
                            .mt(px(14.0))
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(px(10.0))
                            .when_some(self.notice.clone(), |el, notice| {
                                el.child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme.text_muted)
                                        .child(SharedString::from(notice)),
                                )
                            })
                            .child(
                                popover::btn_primary(theme, if self.busy { "Saving…" } else { "Save" })
                                    .id("permissions-save-lists")
                                    .debug_selector(|| "permissions-save-lists".into())
                                    .when(!(enabled && dirty), |el| el.opacity(0.45))
                                    .when(enabled && dirty, |el| {
                                        el.on_click(cx.listener(|page, _, _, cx| page.save_lists(cx)))
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// Command rules: what runs outside the sandbox without asking, what
    /// always asks, what never runs.
    fn commands_card(
        &self,
        settings: &PiPermissionSettings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let enabled = self.editable(cx);
        let mut card = widgets::section_card(theme).mt(px(12.0));
        if settings.commands.is_empty() {
            return card
                .child(
                    div()
                        .px(px(20.0))
                        .py(px(14.0))
                        .text_size(px(12.5))
                        .text_color(theme.text_muted)
                        .child(
                            "Nothing yet. Choosing Always in a project when the agent asks to run a command outside the sandbox adds it here.",
                        ),
                )
                .into_any_element();
        }
        for (index, rule) in settings.commands.iter().enumerate() {
            let pattern = command_pattern_text(&rule.pattern);
            let title = rule.label.clone().unwrap_or_else(|| pattern.clone());
            let decision = match rule.decision {
                CommandDecision::Allow => "Runs outside the sandbox",
                CommandDecision::Ask => "Always asks",
                CommandDecision::Deny => "Never runs",
            };
            let mut notes = String::new();
            if let Some(project) = &rule.project {
                let name = project
                    .rsplit('/')
                    .find(|part| !part.is_empty())
                    .unwrap_or(project);
                notes.push_str(&format!(" · in {name}"));
            }
            if !rule.env.is_empty() {
                notes.push_str(&format!(" · with {}", rule.env.join(", ")));
            }
            if rule.sha256.is_some() {
                notes.push_str(" · pinned to its current contents");
            }
            if rule.acknowledge_danger {
                notes.push_str(" · even when dangerous");
            }
            let target = rule.clone();
            card = card.child(
                widgets::card_row(theme, index == 0)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::row_title(theme, title).truncate())
                            .child(
                                div()
                                    .mt(px(2.0))
                                    .text_size(px(12.0))
                                    .mono(theme)
                                    .text_color(theme.text_muted)
                                    .truncate()
                                    .child(SharedString::from(pattern)),
                            )
                            .child(
                                div()
                                    .mt(px(2.0))
                                    .text_size(px(12.0))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(format!("{decision}{notes}"))),
                            ),
                    )
                    .child(
                        widgets::ghost_action(theme)
                            .id(SharedString::from(format!(
                                "permissions-command-remove-{index}"
                            )))
                            .hover(|s| widgets::ghost_hover(theme, s))
                            .child("Remove")
                            .when(!enabled, |el| el.opacity(0.45))
                            .when(enabled, |el| {
                                el.on_click(cx.listener(move |page, _, _, cx| {
                                    let target = target.clone();
                                    page.update(
                                        move |settings| {
                                            settings.commands.retain(|rule| *rule != target)
                                        },
                                        None,
                                        cx,
                                    )
                                }))
                            }),
                    ),
            );
        }
        card.into_any_element()
    }

    fn always_card(
        &self,
        settings: &PiPermissionSettings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let enabled = self.editable(cx);
        let mut card = widgets::section_card(theme).mt(px(12.0));
        if settings.rules.is_empty() {
            return card
                .child(
                    div()
                        .px(px(20.0))
                        .py(px(14.0))
                        .text_size(px(12.5))
                        .text_color(theme.text_muted)
                        .child(
                            "Nothing yet. Choosing Always in a project, or Always allow, in an approval adds the path here.",
                        ),
                )
                .into_any_element();
        }
        for (index, rule) in settings.rules.iter().enumerate() {
            let (title, detail) = match (
                rule.path.strip_prefix("tool:"),
                rule.path.strip_prefix("net:"),
            ) {
                (Some(tool), _) => (format!("Use {tool}"), "Tool".to_string()),
                (_, Some(domain)) => (domain.to_string(), "Network".to_string()),
                _ => (
                    rule.path.clone(),
                    if rule.capability == "write" {
                        "Read and write".to_string()
                    } else {
                        "Read".to_string()
                    },
                ),
            };
            let detail = match &rule.project {
                Some(project) => {
                    let name = project
                        .rsplit('/')
                        .find(|part| !part.is_empty())
                        .unwrap_or(project);
                    format!("{detail} · in {name}")
                }
                None => detail,
            };
            let target = rule.clone();
            card = card.child(
                widgets::card_row(theme, index == 0)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::row_title(theme, title).truncate())
                            .child(
                                div()
                                    .mt(px(2.0))
                                    .text_size(px(12.0))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(detail)),
                            ),
                    )
                    .child(
                        widgets::ghost_action(theme)
                            .id(SharedString::from(format!(
                                "permissions-rule-remove-{index}"
                            )))
                            .hover(|s| widgets::ghost_hover(theme, s))
                            .child("Remove")
                            .when(!enabled, |el| el.opacity(0.45))
                            .when(enabled, |el| {
                                el.on_click(cx.listener(move |page, _, _, cx| {
                                    let target = target.clone();
                                    page.update(
                                        move |settings| {
                                            settings.rules.retain(|rule| *rule != target)
                                        },
                                        None,
                                        cx,
                                    )
                                }))
                            }),
                    ),
            );
        }
        card.into_any_element()
    }
}

impl Render for PermissionsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let body: AnyElement = match self.snapshot.clone() {
            Loadable::Idle | Loadable::Loading => widgets::section_card(&theme)
                .p(px(16.0))
                .child(popover::skeleton_rows(
                    "permissions-skeleton",
                    &theme,
                    3,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element(),
            Loadable::Error(message) => div()
                .child(widgets::error_strip(&theme, message))
                .child(
                    widgets::ghost_action(&theme)
                        .id("permissions-retry")
                        .mt(px(8.0))
                        .text_color(theme.text)
                        .hover(|s| widgets::ghost_hover(&theme, s))
                        .child("Retry")
                        .on_click(cx.listener(|page, _, _, cx| page.load(cx))),
                )
                .into_any_element(),
            Loadable::Ready(snapshot) => {
                let settings = snapshot.settings.clone();
                let page = div()
                    .child(self.status_card(&snapshot, &theme, cx))
                    .when(snapshot.unreadable, |el| {
                        el.child(widgets::warning_strip(
                            &theme,
                            "permissions.json on this device is not valid JSON, so the defaults apply. Saving here replaces it.",
                        ))
                    });
                if !settings.enabled {
                    // Off: nothing below applies, so only the switch shows.
                    page.into_any_element()
                } else {
                    page.child(Self::section_heading(
                        &theme,
                        "Approvals",
                        "Inside the workspace, agents read and write freely; they read everything else except sensitive files. Anything more is a request, decided here.",
                    ))
                    .child(self.approvals_card(&settings, &theme, cx))
                    .child(self.judge_card(&settings, &theme, cx))
                    .child(Self::section_heading(
                        &theme,
                        "Tools",
                        "MCP servers and other extension tools.",
                    ))
                    .child(self.tools_card(&settings, &theme, cx))
                    .child(Self::section_heading(
                        &theme,
                        "Files and network",
                        "Applies to file tools and to shell commands, which run in the OS sandbox. A command that can read a sensitive file runs offline unless you approve the network with it.",
                    ))
                    .child(self.rules_card(&settings, &theme, cx))
                    .child(Self::section_heading(
                        &theme,
                        "Commands outside the sandbox",
                        "Commands you allowed to run as you, with your files, keys and network. A command the agent's own changes could alter is pinned to the contents you approved.",
                    ))
                    .child(self.commands_card(&settings, &theme, cx))
                    .child(Self::section_heading(
                        &theme,
                        "Always allowed",
                        "Paths and tools you approved with Always allow.",
                    ))
                    .child(self.always_card(&settings, &theme, cx))
                    .into_any_element()
                }
            }
        };
        div()
            .id("permissions-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_header(&theme, "Permissions", None))
                    .child(
                        widgets::page_subtitle(
                            &theme,
                            "What agents on this device may touch. Changes apply from the next turn.",
                        )
                        .max_w(px(560.0))
                        .line_height(px(20.0)),
                    )
                    .children(
                        self.error
                            .clone()
                            .map(|message| widgets::error_strip(&theme, message).into_any_element()),
                    )
                    .when_some(self.target.read(cx).unavailable(cx), |el, error| {
                        el.child(widgets::warning_strip(&theme, error))
                    })
                    .child(body),
            )
    }
}

/// A command pattern as the extension shows it: `**` reads as `…`.
fn command_pattern_text(pattern: &[String]) -> String {
    pattern
        .iter()
        .map(|word| {
            if word == "**" {
                "…".to_string()
            } else if word.contains([' ', '\t', '\'', '"']) {
                serde_json::to_string(word).unwrap_or_else(|_| word.clone())
            } else {
                word.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_patterns_read_as_the_extension_shows_them() {
        let words = |list: &[&str]| list.iter().map(|word| word.to_string()).collect::<Vec<_>>();
        assert_eq!(
            command_pattern_text(&words(&["git", "push", "**"])),
            "git push …"
        );
        assert_eq!(
            command_pattern_text(&words(&["ssh", "web", "uptime -p"])),
            "ssh web \"uptime -p\""
        );
    }

    #[test]
    fn list_text_becomes_trimmed_non_empty_lines() {
        assert_eq!(
            lines("  **/a.yaml \n\n~/.x\r\n"),
            vec!["**/a.yaml".to_string(), "~/.x".to_string()]
        );
    }
}
