//! Settings → Subagents: the `agents/*.md` profiles the Pi subagents extension
//! offers when a chat spawns a child — name, description, model, thinking,
//! tool allowlist and system prompt, edited here instead of by hand.
//!
//! Profiles belong to the SELECTED DEVICE (they are files in that host's Pi
//! runtime), so this page is device-targeted like Agents/MCP/Commands.
//!
//! Two facts from the extension shape the whole surface and are surfaced
//! rather than hidden (see `cypher_engine::pi_subagents`):
//!
//! - built-ins ship inside the extension package, and a user profile of the
//!   same name overrides one. So editing a built-in writes an override, and
//!   deleting that override restores the built-in — the delete confirmation
//!   says which of the two is about to happen.
//! - `description` is not decoration: a profile without one is silently
//!   ignored by the extension. The editor treats it as required.
//!
//! **Each field is edited as its own type, not as free text**, because every
//! one of these values is a string the extension matches EXACTLY and a typo
//! produces a subagent that launches wrong rather than an error:
//!
//! - **model** is picked from the target device's own `ListModels` catalog,
//!   whose `id` is already the `provider/model` string the file stores.
//! - **thinking** is Pi's fixed `--thinking` ladder ([`THINKING_LEVELS`]),
//!   further narrowed by the chosen model — a model that does not support
//!   reasoning has no levels to offer.
//! - **tools** are toggles over Pi's built-in set ([`BUILTIN_TOOLS`], its
//!   `allToolNames`). Extension and MCP tools stay free-form additions
//!   because Pi has no RPC that enumerates them — `get_available_models` and
//!   `get_available_thinking_levels` exist, a tools equivalent does not.
//!
//! A value already in a file that is not in the catalog is never silently
//! dropped: it stays selected and is flagged, so opening an agent whose
//! provider is offline and pressing Save cannot quietly retarget it.

use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent, Render,
    SharedString, Subscription, Task, Window, div, prelude::*, px,
};

use cypher_engine::pi_subagents::PiSubagent;
use cypher_proto::Model;
use cypher_rpc::methods;

use super::device_target::{DeviceTarget, DeviceTicket};
use crate::kit::icons;
use crate::kit::popover::{self, Loadable};
use crate::kit::theme::Theme;
use crate::settings::widgets;
use crate::state::AppState;
use crate::widgets::text_input::{TextInput, TextInputEvent};

/// Pi's built-in tools (`core/tools/index.js` `allToolNames`). Anything else a
/// profile lists — an extension or MCP tool — is kept as a custom chip.
pub const BUILTIN_TOOLS: [&str; 8] = [
    "read",
    "grep",
    "find",
    "ls",
    "edit",
    "write",
    "bash",
    "powershell",
];

/// Pi's `--thinking` ladder. `off` is Pi's own extra tier and has no
/// `ReasoningLevel` equivalent, so it is spelled here rather than derived.
pub const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The messaging trio the harness appends to every child regardless of the
/// allowlist (`cypher_harness::pi::MESSAGING_TOOLS`).
const MESSAGING_TOOLS: &str = "send_message, read_inbox, reply_message";

/// Editor dialog width. Wider than [`popover::dialog_card`]'s 360px default
/// because this form carries chip rows and a prompt document, not a field or
/// two.
const EDITOR_WIDTH: f32 = 560.0;
/// Confirmation dialog width — a question, so the default reads better.
const CONFIRM_WIDTH: f32 = 400.0;
/// Chrome above and below the dialog's scrolling body (heading + footer +
/// window margin), subtracted from the viewport to bound that scroll.
const DIALOG_CHROME: f32 = 260.0;

/// Dialog caption line (providers.rs `caption`).
fn caption(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(12.0))
        .line_height(px(18.0))
        .text_color(theme.text_muted)
        .child(text.into())
}

/// The open editor. `original` is the profile it was opened on: `None` means
/// "create", and a changed name means "rename" — both decided by the engine,
/// which owns the file moves.
struct Editor {
    original: Option<String>,
    /// Opened from a built-in: saving writes a user-level override of it.
    from_builtin: bool,
    ticket: DeviceTicket,
    read_only: bool,
    /// `None` = inherit the parent chat's model.
    model: Option<String>,
    /// `None` = inherit; otherwise one of [`THINKING_LEVELS`], or a value the
    /// file already carried.
    thinking: Option<String>,
    /// Empty = every tool.
    tools: Vec<String>,
    name: Entity<TextInput>,
    description: Entity<TextInput>,
    prompt: Entity<TextInput>,
    /// Free-form entry for an extension/MCP tool Pi cannot enumerate.
    tool_entry: Entity<TextInput>,
    model_menu_open: bool,
    model_search: Entity<TextInput>,
    model_trigger: FocusHandle,
    _events: Vec<Subscription>,
}

impl Editor {
    /// Tools the profile lists that are not Pi built-ins.
    fn custom_tools(&self) -> Vec<String> {
        self.tools
            .iter()
            .filter(|tool| !BUILTIN_TOOLS.contains(&tool.as_str()))
            .cloned()
            .collect()
    }
}

struct DeleteConfirmation {
    name: String,
    /// Deleting this restores a built-in of the same name instead of removing
    /// the agent outright.
    restores_builtin: bool,
    ticket: DeviceTicket,
}

pub struct SubagentsPage {
    state: Entity<AppState>,
    target: Entity<DeviceTarget>,
    generation: u64,
    _target_observer: Subscription,
    agents: Loadable<Vec<PiSubagent>>,
    /// The target device's Pi catalog, for the model picker. A failure here
    /// never blocks editing — a stored model id stands on its own.
    models: Loadable<Vec<Model>>,
    error: Option<String>,
    notice: Option<String>,
    busy: Option<String>,
    load_task: Option<Task<()>>,
    editor: Option<Editor>,
    delete: Option<DeleteConfirmation>,
    /// Focus owner of the open dialog, so Escape reaches [`Self::on_dialog_key`]
    /// even when no field inside it holds focus.
    dialog_focus: FocusHandle,
    /// Where focus returns when a dialog closes.
    page_focus: FocusHandle,
}

impl SubagentsPage {
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
                page.load_task = None;
                page.agents = Loadable::Idle;
                page.models = Loadable::Idle;
                page.busy = None;
                page.error = None;
                page.notice = None;
                page.editor = None;
                page.delete = None;
                page.load(cx);
            }
            cx.notify();
        });
        let mut page = Self {
            state,
            target,
            generation,
            _target_observer: observer,
            agents: Loadable::Idle,
            models: Loadable::Idle,
            error: None,
            notice: None,
            busy: None,
            load_task: None,
            editor: None,
            delete: None,
            dialog_focus: cx.focus_handle(),
            page_focus: cx.focus_handle(),
        };
        page.load(cx);
        page
    }

    /// Dismiss whichever dialog is open and hand focus back to the page.
    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.editor = None;
        self.delete = None;
        self.error = None;
        self.page_focus.clone().focus(window, cx);
        cx.notify();
    }

    /// Escape closes the model dropdown first, then the dialog — one layer per
    /// press, so dismissing a menu never discards a half-filled form.
    fn on_dialog_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" {
            return;
        }
        cx.stop_propagation();
        if let Some(editor) = &mut self.editor
            && editor.model_menu_open
        {
            editor.model_menu_open = false;
            cx.notify();
            return;
        }
        self.close_dialog(window, cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let ticket = match self.target.read(cx).ticket(cx) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.agents = Loadable::Error(error);
                cx.notify();
                return;
            }
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.agents = Loadable::Loading;
        self.models = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::LIST_PI_SUBAGENTS,
                    ticket.params(serde_json::json!({})),
                )
                .await;
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.agents = match result {
                    Ok(value) => match serde_json::from_value::<Vec<PiSubagent>>(value) {
                        Ok(agents) => Loadable::Ready(agents),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(Self::explain(&ticket, &err.to_string())),
                };
                cx.notify();
            })
            .ok();

            // The catalog is a convenience for the picker, fetched second so a
            // slow or broken model probe never delays the profile list.
            let models = engine
                .client()
                .call(
                    methods::LIST_MODELS,
                    ticket.params(serde_json::json!({"harness":"pi"})),
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
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => {
                        Loadable::Error(format!("Couldn't load this device's Pi models: {err}"))
                    }
                };
                cx.notify();
            })
            .ok();
        }));
    }

    /// An engine that predates this page answers `unknown method`, which is a
    /// version mismatch rather than a failure the user can retry into.
    fn explain(ticket: &DeviceTicket, error: &str) -> String {
        if error.contains("unknown method") {
            format!(
                "{}: update Cypher on this device to manage subagents from Settings.",
                ticket.label
            )
        } else {
            format!("{}: {error}", ticket.label)
        }
    }

    /// The catalog entry for an id, when the device knows it.
    fn model_entry(&self, id: &str) -> Option<&Model> {
        self.models
            .ready()
            .and_then(|models| models.iter().find(|model| model.id == id))
    }

    /// Levels the editor may offer. A model the device knows decides: no
    /// reasoning support means no ladder at all, so the UI stops offering a
    /// setting Pi would ignore, and otherwise only `off` plus the model's own
    /// levels (Pi clamps anything else). An unknown or inherited model keeps
    /// the full ladder rather than guessing.
    fn available_levels(&self, editor: &Editor) -> Vec<&'static str> {
        match editor.model.as_deref().and_then(|id| self.model_entry(id)) {
            Some(model) if model.reasoning_levels.is_empty() => Vec::new(),
            Some(model) => THINKING_LEVELS
                .into_iter()
                .filter(|&level| {
                    level == "off"
                        || model.reasoning_levels.iter().any(|offered| {
                            serde_json::to_value(offered)
                                .is_ok_and(|value| value.as_str() == Some(level))
                        })
                })
                .collect(),
            None => THINKING_LEVELS.to_vec(),
        }
    }

    fn open_editor(
        &mut self,
        agent: Option<PiSubagent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() || !self.target.read(cx).can_write(cx) {
            return;
        }
        let Ok(ticket) = self.target.read(cx).ticket(cx) else {
            return;
        };
        let field = |placeholder: &'static str, value: &str, cx: &mut Context<Self>| {
            let input = cx.new(|cx| TextInput::settings_field(placeholder, false, cx));
            if !value.is_empty() {
                input.update(cx, |input, cx| input.set_text(value, cx));
            }
            input
        };

        let existing = agent.as_ref();
        let name = field(
            "e.g. reviewer",
            existing.map(|a| a.name.as_str()).unwrap_or_default(),
            cx,
        );
        let description = field(
            "What this agent is for — the extension needs one",
            existing.map(|a| a.description.as_str()).unwrap_or_default(),
            cx,
        );
        let tool_entry = field("Add an extension or MCP tool…", "", cx);
        let model_search = field("Search models…", "", cx);
        let prompt = cx.new(|cx| {
            let mut input = TextInput::settings_prompt_field(
                "The system prompt appended for this agent's runs.",
                cx,
            );
            if let Some(agent) = existing
                && !agent.system_prompt.is_empty()
            {
                input.set_text(&agent.system_prompt, cx);
            }
            input
        });

        let mut events = Vec::new();
        for input in [&name, &description, &prompt] {
            events.push(
                cx.subscribe(input, |page: &mut Self, _, event, cx| match event {
                    // Enter in a value field saves; the prompt field binds
                    // Enter to a newline and never emits this.
                    TextInputEvent::Submitted => page.save(cx),
                    TextInputEvent::Edited => {
                        page.error = None;
                        cx.notify();
                    }
                    _ => {}
                }),
            );
        }
        // Enter in the tool box adds that tool instead of saving the form.
        events.push(
            cx.subscribe(&tool_entry, |page: &mut Self, _, event, cx| match event {
                TextInputEvent::Submitted => page.add_custom_tool(cx),
                TextInputEvent::Edited => {
                    page.error = None;
                    cx.notify();
                }
                _ => {}
            }),
        );
        events.push(cx.subscribe(&model_search, |_: &mut Self, _, event, cx| {
            if matches!(event, TextInputEvent::Edited) {
                cx.notify();
            }
        }));

        self.notice = None;
        self.error = None;
        self.delete = None;
        self.editor = Some(Editor {
            original: existing.map(|a| a.name.clone()),
            from_builtin: existing.is_some_and(|a| a.builtin),
            ticket,
            read_only: existing.is_some_and(|a| a.read_only),
            model: existing.and_then(|a| a.model.clone()),
            thinking: existing.and_then(|a| a.thinking.clone()),
            tools: existing.map(|a| a.tools.clone()).unwrap_or_default(),
            name,
            description,
            prompt,
            tool_entry,
            model_menu_open: false,
            model_search,
            model_trigger: cx.focus_handle(),
            _events: events,
        });
        if let Some(editor) = &self.editor {
            let focus = if editor.original.is_some() {
                editor.description.focus_handle(cx)
            } else {
                editor.name.focus_handle(cx)
            };
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn toggle_tool(&mut self, tool: String, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        if let Some(editor) = &mut self.editor {
            if let Some(index) = editor.tools.iter().position(|t| *t == tool) {
                editor.tools.remove(index);
            } else {
                editor.tools.push(tool);
            }
            self.error = None;
        }
        cx.notify();
    }

    /// Add whatever is in the tool box. Kept permissive on purpose: Pi cannot
    /// list extension or MCP tools, so the user is the only source for them.
    fn add_custom_tool(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some(editor) = &self.editor else {
            return;
        };
        let raw = editor.tool_entry.read(cx).text().trim().to_string();
        if raw.is_empty() {
            return;
        }
        // A pasted "a, b" is two tools, not one name with a comma in it — and
        // a comma could not survive the file's comma-separated list anyway.
        let added: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
            .map(str::to_string)
            .collect();
        if let Some(editor) = &mut self.editor {
            for tool in added {
                if !editor.tools.contains(&tool) {
                    editor.tools.push(tool);
                }
            }
        }
        if let Some(editor) = &self.editor {
            editor
                .tool_entry
                .update(cx, |input, cx| input.set_text("", cx));
        }
        self.error = None;
        cx.notify();
    }

    fn set_model(&mut self, model: Option<String>, cx: &mut Context<Self>) {
        // Resolved before the editor is borrowed: a model the device knows to
        // have no reasoning ladder cannot carry a thinking level, and an
        // unknown or inherited one is given the benefit of the doubt.
        let supports_thinking = match model.as_deref() {
            None => true,
            Some(id) => self
                .model_entry(id)
                .is_none_or(|entry| !entry.reasoning_levels.is_empty()),
        };
        if let Some(editor) = &mut self.editor {
            editor.model = model;
            editor.model_menu_open = false;
            // Dropping the level here keeps the file honest instead of
            // writing a setting Pi would ignore.
            if !supports_thinking {
                editor.thinking = None;
            }
        }
        self.error = None;
        cx.notify();
    }

    /// Build the save request from the open editor.
    fn save_request(&self, cx: &Context<Self>) -> Option<serde_json::Value> {
        let editor = self.editor.as_ref()?;
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_string();
        let mut params = serde_json::json!({
            "name": text(&editor.name),
            "description": text(&editor.description),
            "systemPrompt": editor.prompt.read(cx).text().trim(),
            "tools": editor.tools,
            "readOnly": editor.read_only,
        });
        if let Some(object) = params.as_object_mut() {
            if let Some(model) = editor.model.clone() {
                object.insert("model".into(), model.into());
            }
            if let Some(thinking) = editor.thinking.clone() {
                object.insert("thinking".into(), thinking.into());
            }
            // A built-in is not a file we may edit: saving always creates a
            // user override, so the engine must treat it as a fresh write
            // under that name rather than a rename of the package file.
            if !editor.from_builtin
                && let Some(original) = editor.original.clone()
            {
                object.insert("originalName".into(), original.into());
            }
        }
        Some(params)
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        if !self.target.read(cx).matches(&editor.ticket) {
            self.editor = None;
            self.error =
                Some("The selected device changed. Open the subagent again before saving.".into());
            cx.notify();
            return;
        }
        let Some(params) = self.save_request(cx) else {
            return;
        };
        let name = params["name"].as_str().unwrap_or_default().to_string();
        let created = editor.original.is_none();
        let override_written = editor.from_builtin;
        self.mutate(methods::SAVE_PI_SUBAGENT, name.clone(), params, cx, {
            move |page, ticket| {
                page.editor = None;
                page.notice = Some(if override_written {
                    format!(
                        "Saved “{name}” on {} as your own copy. The built-in is unchanged and returns if you delete this one.",
                        ticket.label
                    )
                } else if created {
                    format!("Added “{name}” on {}.", ticket.label)
                } else {
                    format!("Saved “{name}” on {}.", ticket.label)
                });
            }
        });
    }

    fn request_delete(&mut self, agent: &PiSubagent, cx: &mut Context<Self>) {
        if self.busy.is_some() || !self.target.read(cx).can_write(cx) {
            return;
        }
        let Ok(ticket) = self.target.read(cx).ticket(cx) else {
            return;
        };
        self.editor = None;
        self.error = None;
        self.notice = None;
        self.delete = Some(DeleteConfirmation {
            name: agent.name.clone(),
            restores_builtin: agent.overrides_builtin,
            ticket,
        });
        cx.notify();
    }

    fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Some(confirmation) = &self.delete else {
            return;
        };
        if !self.target.read(cx).matches(&confirmation.ticket) {
            self.delete = None;
            self.error = Some(
                "The selected device changed. Select the subagent again before deleting.".into(),
            );
            cx.notify();
            return;
        }
        let name = confirmation.name.clone();
        let restores = confirmation.restores_builtin;
        self.mutate(
            methods::DELETE_PI_SUBAGENT,
            name.clone(),
            serde_json::json!({ "name": name }),
            cx,
            move |page, ticket| {
                page.delete = None;
                page.notice = Some(if restores {
                    format!(
                        "Removed your copy of “{name}” on {}. The built-in subagent is active again.",
                        ticket.label
                    )
                } else {
                    format!("Deleted “{name}” from {}.", ticket.label)
                });
            },
        );
    }

    /// Shared write path: every mutation replies with the device's fresh list,
    /// so success is applied from the reply rather than guessed locally.
    fn mutate(
        &mut self,
        method: &'static str,
        name: String,
        params: serde_json::Value,
        cx: &mut Context<Self>,
        on_success: impl FnOnce(&mut Self, &DeviceTicket) + 'static,
    ) {
        if self.busy.is_some()
            || !self.target.read(cx).can_write(cx)
            || self.generation != self.target.read(cx).generation()
        {
            return;
        }
        let Ok(ticket) = self.target.read(cx).ticket(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.error = None;
        self.notice = None;
        self.busy = Some(name);
        self.load_task = None;
        let target = self.target.clone();
        let lease = target.update(cx, |target, cx| target.lock(cx));
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(method, ticket.params(params))
                .await
                .map_err(|err| Self::explain(&ticket, &err.to_string()));
            drop(lease);
            target.update(cx, |_, cx| cx.notify());
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.busy = None;
                match result {
                    Ok(value) => match serde_json::from_value::<Vec<PiSubagent>>(value) {
                        Ok(agents) => {
                            page.agents = Loadable::Ready(agents);
                            on_success(page, &ticket);
                        }
                        Err(err) => page.error = Some(err.to_string()),
                    },
                    Err(err) => page.error = Some(err),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn agent_row(
        &mut self,
        theme: &Theme,
        agent: PiSubagent,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let blocked = self.busy.is_some()
            || self.editor.is_some()
            || self.delete.is_some()
            || !self.target.read(cx).can_write(cx);

        let mut title = div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(widgets::row_title(theme, agent.name.clone()));
        if agent.builtin {
            title = title.child(widgets::badge(theme, "built-in"));
        } else if agent.overrides_builtin {
            title = title.child(widgets::badge_active(theme, "your copy"));
        }
        if agent.read_only {
            title = title.child(widgets::badge(theme, "read-only"));
        }

        // The quiet line is what actually governs a run: model, thinking and
        // how wide the tool allowlist is. The model shows its catalog LABEL
        // when the device knows it, because `provider/id` is unreadable.
        let mut facts: Vec<AnyElement> = Vec::new();
        if let Some(model) = agent.model.as_deref() {
            let label = self
                .model_entry(model)
                .map(|entry| entry.label.clone())
                .unwrap_or_else(|| model.to_string());
            facts.push(div().child(SharedString::from(label)).into_any_element());
        }
        if let Some(thinking) = agent.thinking.as_deref() {
            facts.push(
                div()
                    .child(SharedString::from(format!("thinking {thinking}")))
                    .into_any_element(),
            );
        }
        facts.push(
            div()
                .child(SharedString::from(if agent.tools.is_empty() {
                    "all tools".to_string()
                } else {
                    format!("{} tools", agent.tools.len())
                }))
                .into_any_element(),
        );

        let edit = agent.clone();
        let remove = agent.clone();
        let mut row = widgets::card_row(theme, index == 0)
            .id(("subagent-row", index))
            .items_start()
            .child(
                div()
                    .mt(px(2.0))
                    .child(widgets::row_tile(theme, icons::DOCUMENT)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(title)
                    .child(
                        div()
                            .mt(px(4.0))
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(agent.description.clone())),
                    )
                    .child(widgets::meta_line(theme, facts)),
            );

        row = row.child(
            widgets::ghost_action(theme)
                .flex_none()
                .id(("subagent-edit", index))
                .role(gpui::Role::Button)
                .aria_label(format!("Edit {}", agent.name))
                .text_color(theme.text)
                .hover(|s| widgets::ghost_hover(theme, s))
                .child(SharedString::from(if agent.builtin {
                    "Customize"
                } else {
                    "Edit"
                }))
                .when(blocked, |el| el.opacity(0.45))
                .when(!blocked, |el| {
                    el.on_click(cx.listener(move |page, _, window, cx| {
                        page.open_editor(Some(edit.clone()), window, cx);
                    }))
                }),
        );
        // A built-in has no file of ours to delete; the row offers nothing.
        if !agent.builtin {
            row = row.child(
                widgets::ghost_action(theme)
                    .flex_none()
                    .id(("subagent-delete", index))
                    .role(gpui::Role::Button)
                    .aria_label(format!("Delete {}", agent.name))
                    .text_color(theme.danger_muted)
                    .child(SharedString::from(if agent.overrides_builtin {
                        "Reset"
                    } else {
                        "Delete"
                    }))
                    .when(blocked, |el| el.opacity(0.45))
                    .when(!blocked, |el| {
                        el.on_click(cx.listener(move |page, _, _, cx| {
                            page.request_delete(&remove, cx);
                        }))
                    }),
            );
        }
        row.into_any_element()
    }

    /// One selectable chip. Used for every enumerable value on this page so a
    /// thinking level and a tool read as the same kind of choice — and it
    /// carries the 8px corner of [`widgets::ghost_action`] /
    /// [`popover::btn_primary`], because it is a button among buttons, not a
    /// [`widgets::badge`] (those are the round ones, and they are read-only).
    fn chip(
        theme: &Theme,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        selected: bool,
        enabled: bool,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id.into())
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(if selected {
                theme.accent.opacity(0.55)
            } else {
                theme.border
            })
            .bg(if selected {
                theme.accent.opacity(0.14)
            } else {
                gpui::transparent_black()
            })
            .text_size(px(12.0))
            .text_color(if selected {
                theme.text
            } else {
                theme.text_muted
            })
            .when(enabled, |el| {
                el.cursor_pointer()
                    .hover(|s| s.bg(crate::kit::theme::ink(0.06)))
            })
            .when(!enabled, |el| el.opacity(0.45))
            .child(label.into())
    }

    fn model_row(
        &self,
        model: Option<&Model>,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = model.map(|model| model.id.clone());
        let label = model
            .map(|model| model.label.clone())
            .unwrap_or_else(|| "Inherit from the parent chat".into());
        let description = model.and_then(|model| model.description.clone());
        div()
            .id(SharedString::from(format!(
                "subagent-model-{}",
                id.as_deref().unwrap_or("inherit")
            )))
            .px(px(10.0))
            .py(px(7.0))
            .rounded(px(8.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .bg(if selected {
                theme.ink(0.07)
            } else {
                gpui::transparent_black()
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme.ink(0.05)))
            .on_click(cx.listener(move |page, _, _, cx| page.set_model(id.clone(), cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.0))
                            .text_color(theme.text)
                            .child(SharedString::from(label)),
                    )
                    .when_some(description, |el, description| {
                        el.child(
                            div()
                                .truncate()
                                .text_size(px(11.0))
                                .text_color(theme.text_muted.opacity(0.75))
                                .child(SharedString::from(description)),
                        )
                    }),
            )
            .child(div().w(px(16.0)).when(selected, |row| {
                row.child(
                    icons::icon(icons::CHECK)
                        .size(px(12.0))
                        .text_color(theme.accent),
                )
            }))
            .into_any_element()
    }

    fn model_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let editor = self.editor.as_ref().unwrap();
        let selected = editor.model.clone();
        let query = editor.model_search.read(cx).text().trim().to_lowercase();
        let mut menu = div()
            .w(px(320.0))
            .rounded(px(10.0))
            .bg(theme.surface_overlay)
            .border_1()
            .border_color(theme.border_strong)
            .on_mouse_down_out(cx.listener(|page, _, window, cx| {
                if let Some(editor) = &mut page.editor {
                    editor.model_menu_open = false;
                    editor.model_trigger.clone().focus(window, cx);
                }
                cx.notify();
            }))
            .child(div().p(px(8.0)).child(editor.model_search.clone()))
            .child(
                div()
                    .px(px(4.0))
                    .child(self.model_row(None, selected.is_none(), theme, cx)),
            );

        match self.models.clone() {
            Loadable::Ready(models) => {
                let filtered: Vec<_> = models
                    .iter()
                    .filter(|model| {
                        query.is_empty()
                            || model.label.to_lowercase().contains(&query)
                            || model.id.to_lowercase().contains(&query)
                    })
                    .collect();
                if filtered.is_empty() {
                    menu = menu.child(
                        div()
                            .p(px(12.0))
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child(if models.is_empty() {
                                "No Pi models available on this device."
                            } else {
                                "No matching models."
                            }),
                    );
                } else {
                    menu = menu.child(
                        div()
                            .id("subagent-model-list")
                            .max_h(px(260.0))
                            .overflow_y_scroll()
                            .px(px(4.0))
                            .pb(px(4.0))
                            .children(filtered.into_iter().map(|model| {
                                self.model_row(
                                    Some(model),
                                    selected.as_deref() == Some(model.id.as_str()),
                                    theme,
                                    cx,
                                )
                            })),
                    );
                }
            }
            Loadable::Error(error) => menu = menu.child(widgets::error_strip(theme, error)),
            _ => {
                menu = menu.child(
                    div()
                        .p(px(12.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child("Loading models…"),
                )
            }
        }
        // In-dialog layer: the modal defers at priority 2, so the default
        // priority-1 menu would draw UNDER the card that owns its trigger.
        popover::anchored_menu_below_in_dialog(
            "subagent-model-popup",
            menu.into_any_element(),
            None,
        )
    }

    /// Label for the model trigger, preferring the catalog's human name.
    fn model_label(&self, editor: &Editor) -> String {
        match editor.model.as_deref() {
            None => "Inherit from the parent chat".into(),
            Some(id) => self
                .model_entry(id)
                .map(|model| model.label.clone())
                .unwrap_or_else(|| id.to_string()),
        }
    }

    fn render_editor(
        &mut self,
        window: &mut Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editor = self.editor.as_ref().unwrap();
        let busy = self.busy.is_some();
        let creating = editor.original.is_none();
        let from_builtin = editor.from_builtin;
        let read_only = editor.read_only;
        let original_label = editor.original.clone();
        let label = editor.ticket.label.clone();
        let selected_tools = editor.tools.clone();
        let custom_tools = editor.custom_tools();
        let thinking = editor.thinking.clone();
        let levels = self.available_levels(editor);
        let model_unknown = editor.model.as_deref().is_some_and(|id| {
            matches!(self.models, Loadable::Ready(_)) && self.model_entry(id).is_none()
        });
        let model_trigger_label = self.model_label(editor);
        let menu = editor.model_menu_open.then(|| self.model_menu(theme, cx));
        let editor = self.editor.as_ref().unwrap();
        let view = EditorView {
            busy,
            creating,
            from_builtin,
            original_label,
            label,
            selected_tools,
            custom_tools,
            thinking,
            levels,
            model_unknown,
            model_trigger_label,
        };
        let label = &view.label;

        let mut fields = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(editor_input_row(theme, "Name", None, &editor.name))
            .child(editor_input_row(
                theme,
                "Description",
                Some("Required: the extension ignores a profile without one, and the model reads it to pick an agent."),
                &editor.description,
            ));
        fields = fields.child(editor_model_row(editor, &view, menu, theme, cx));
        fields = fields.child(editor_thinking_row(&view, theme, cx));
        fields = fields.child(editor_tools_section(editor, &view, theme, cx));
        fields = fields.child(editor_input_row(
            theme,
            "System prompt",
            None,
            &editor.prompt,
        ));

        fields = fields.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(
                    widgets::toggle_switch(theme, read_only)
                        .id("subagent-readonly")
                        .cursor_pointer()
                        .when(busy, |el| el.opacity(0.5))
                        .when(!busy, |el| {
                            el.on_click(cx.listener(|page, _, _, cx| {
                                if let Some(editor) = &mut page.editor {
                                    editor.read_only = !editor.read_only;
                                }
                                cx.notify();
                            }))
                        }),
                )
                .child(widgets::field_label(theme, "Read-only agent")),
        );

        fields = fields.child(caption(
            theme,
            if from_builtin {
                format!(
                    "Saving writes your own copy on {label}. The built-in stays untouched, and deleting your copy brings it back."
                )
            } else {
                format!(
                    "Saved to {label}. Runs already in flight keep the profile they started with."
                )
            },
        ));

        let footer = editor_footer(&view, theme, cx);
        let heading = editor_heading(&view, theme, cx);

        popover::dialog_card(theme)
            .id("subagent-editor-dialog")
            .role(gpui::Role::Dialog)
            .aria_label("Subagent settings")
            .track_focus(&self.dialog_focus)
            .key_context("SubagentDialog")
            .tab_group()
            .p_0()
            .w(px(EDITOR_WIDTH))
            .overflow_hidden()
            .on_key_down(cx.listener(Self::on_dialog_key))
            .child(heading)
            .child(
                div()
                    .id("subagent-editor-scroll")
                    .max_h(px((f32::from(window.viewport_size().height)
                        - DIALOG_CHROME)
                        .max(160.0)))
                    .overflow_y_scroll()
                    .px(px(20.0))
                    .py(px(16.0))
                    .child(fields),
            )
            .child(footer)
            .into_any_element()
    }

    fn render_delete_confirmation(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let confirmation = self.delete.as_ref().unwrap();
        let busy = self.busy.is_some();
        let enabled = !busy && self.target.read(cx).can_write(cx);
        let restores = confirmation.restores_builtin;
        let title = if restores {
            format!(
                "Reset “{}” on “{}” to the built-in?",
                confirmation.name, confirmation.ticket.label
            )
        } else {
            format!(
                "Delete “{}” from “{}”?",
                confirmation.name, confirmation.ticket.label
            )
        };
        let body = if restores {
            "Your edited copy is removed and the built-in subagent of the same name takes over again. Nothing else changes."
        } else {
            "The profile file is removed from this device. Chats that already spawned this subagent are unaffected; new spawns will not find it."
        };
        popover::dialog_card(theme)
            .id("subagent-delete-confirmation")
            .role(gpui::Role::Dialog)
            .aria_label("Confirm")
            .track_focus(&self.dialog_focus)
            .key_context("SubagentDialog")
            .tab_group()
            .w(px(CONFIRM_WIDTH))
            .gap(px(12.0))
            .on_key_down(cx.listener(Self::on_dialog_key))
            .child(popover::dialog_title(theme, &title))
            .child(popover::dialog_body(theme, body))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        widgets::ghost_action(theme)
                            .id("subagent-delete-cancel")
                            .debug_selector(|| "subagent-delete-cancel".into())
                            .text_color(theme.text)
                            .hover(|s| widgets::ghost_hover(theme, s))
                            .child("Cancel")
                            .when(busy, |el| el.opacity(0.45))
                            .when(!busy, |el| {
                                el.on_click(
                                    cx.listener(|page, _, window, cx| {
                                        page.close_dialog(window, cx)
                                    }),
                                )
                            }),
                    )
                    .child(
                        widgets::ghost_action(theme)
                            .id("subagent-delete-confirm")
                            .debug_selector(|| "subagent-delete-confirm".into())
                            .text_color(theme.danger)
                            .child(if busy {
                                "Working…"
                            } else if restores {
                                "Reset to built-in"
                            } else {
                                "Delete subagent"
                            })
                            .when(!enabled, |el| el.opacity(0.45))
                            .when(enabled, |el| {
                                el.on_click(cx.listener(|page, _, _, cx| page.confirm_delete(cx)))
                            }),
                    ),
            )
            .into_any_element()
    }
}

/// One labelled text field of the editor, with an optional hint under it.
fn editor_input_row(
    theme: &Theme,
    label: &'static str,
    hint: Option<&'static str>,
    input: &Entity<TextInput>,
) -> gpui::Div {
    let mut column = div()
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
        );
    if let Some(hint) = hint {
        column = column.child(widgets::page_subtitle(theme, hint));
    }
    column
}

/// Model — picked from the device's own catalog.
fn editor_model_row(
    editor: &Editor,
    view: &EditorView,
    menu: Option<AnyElement>,
    theme: &Theme,
    cx: &mut Context<SubagentsPage>,
) -> gpui::Div {
    let EditorView {
        busy,
        model_unknown,
        model_trigger_label,
        ..
    } = view;
    let busy = *busy;
    div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .child(widgets::field_label(theme, "Model"))
            .child(
                widgets::ghost_action(theme)
                    .id("subagent-model-trigger")
                    .debug_selector(|| "subagent-model-trigger".into())
                    .role(gpui::Role::Button)
                    .aria_label("Subagent model")
                    .track_focus(&editor.model_trigger)
                    .relative()
                    .w_full()
                    .px(px(10.0))
                    .py(px(8.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.input_glass_bg())
                    .hover(|s| widgets::ghost_hover(theme, s))
                    .when(busy, |el| el.opacity(0.45))
                    .when(!busy, |el| {
                        el.on_click(cx.listener(|page, _, window, cx| {
                            if let Some(editor) = &mut page.editor {
                                editor.model_menu_open = !editor.model_menu_open;
                                if editor.model_menu_open {
                                    editor.model_search.focus_handle(cx).focus(window, cx);
                                }
                            }
                            cx.notify();
                        }))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text)
                            .child(SharedString::from(model_trigger_label.clone())),
                    )
                    .child(
                        icons::icon(icons::ALT_ARROW_DOWN)
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    )
                    .children(menu),
            )
            .when(*model_unknown, |el| {
                el.child(widgets::warning_strip(
                    theme,
                    "This model is not in the device's Pi catalog right now. It is kept as written until you pick another.",
                ))
            })
}

/// Thinking — Pi's ladder, narrowed by what the model supports.
fn editor_thinking_row(
    view: &EditorView,
    theme: &Theme,
    cx: &mut Context<SubagentsPage>,
) -> gpui::Div {
    let EditorView {
        busy,
        thinking,
        levels,
        ..
    } = view;
    let busy = *busy;
    let mut thinking_row = div().flex().flex_row().flex_wrap().gap(px(6.0)).child(
        SubagentsPage::chip(
            theme,
            "subagent-thinking-inherit",
            "Inherit",
            thinking.is_none(),
            !busy,
        )
        .on_click(cx.listener(|page, _, _, cx| {
            if let Some(editor) = &mut page.editor {
                editor.thinking = None;
            }
            cx.notify();
        })),
    );
    for level in levels {
        let level = *level;
        let selected = thinking.as_deref() == Some(level);
        thinking_row = thinking_row.child(
            SubagentsPage::chip(
                theme,
                SharedString::from(format!("subagent-thinking-{level}")),
                level,
                selected,
                !busy,
            )
            .on_click(cx.listener(move |page, _, _, cx| {
                if let Some(editor) = &mut page.editor {
                    editor.thinking = Some(level.to_string());
                }
                cx.notify();
            })),
        );
    }
    // A level the file carries that is not on the ladder stays selectable
    // rather than vanishing on the next save.
    if let Some(current) = thinking.as_deref()
        && !levels.contains(&current)
    {
        thinking_row = thinking_row.child(SubagentsPage::chip(
            theme,
            "subagent-thinking-custom",
            format!("{current} (from the file)"),
            true,
            false,
        ));
    }
    let mut thinking_column = div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(widgets::field_label(theme, "Thinking"))
        .child(thinking_row);
    if levels.is_empty() {
        thinking_column = thinking_column.child(widgets::page_subtitle(
            theme,
            "The selected model has no reasoning levels, so Pi ignores this setting.",
        ));
    }
    thinking_column
}

/// Tools — built-in toggles plus free-form extension/MCP names.
fn editor_tools_section(
    editor: &Editor,
    view: &EditorView,
    theme: &Theme,
    cx: &mut Context<SubagentsPage>,
) -> gpui::Div {
    let EditorView {
        busy,
        selected_tools,
        custom_tools,
        ..
    } = view;
    let busy = *busy;
    let mut tool_row = div().flex().flex_row().flex_wrap().gap(px(6.0));
    for tool in BUILTIN_TOOLS {
        let selected = selected_tools.iter().any(|t| t == tool);
        tool_row = tool_row.child(
            SubagentsPage::chip(
                theme,
                SharedString::from(format!("subagent-tool-{tool}")),
                tool,
                selected,
                !busy,
            )
            .on_click(cx.listener(move |page, _, _, cx| {
                page.toggle_tool(tool.to_string(), cx);
            })),
        );
    }
    for tool in custom_tools {
        let name = tool.clone();
        tool_row = tool_row.child(
            SubagentsPage::chip(
                theme,
                SharedString::from(format!("subagent-custom-tool-{name}")),
                format!("{name}  ×"),
                true,
                !busy,
            )
            .on_click(cx.listener(move |page, _, _, cx| {
                page.toggle_tool(name.clone(), cx);
            })),
        );
    }
    div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .child(widgets::field_label(theme, "Tools"))
            .child(tool_row)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px(px(10.0))
                            .py(px(8.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(theme.border_strong)
                            .bg(theme.input_glass_bg())
                            .child(editor.tool_entry.clone()),
                    )
                    .child(
                        widgets::ghost_action(theme)
                            .id("subagent-tool-add")
                            .debug_selector(|| "subagent-tool-add".into())
                            .flex_none()
                            .text_color(theme.text)
                            .hover(|s| widgets::ghost_hover(theme, s))
                            .child("Add")
                            .when(busy, |el| el.opacity(0.45))
                            .when(!busy, |el| {
                                el.on_click(
                                    cx.listener(|page, _, _, cx| page.add_custom_tool(cx)),
                                )
                            }),
                    ),
            )
            .child(widgets::page_subtitle(
                theme,
                if selected_tools.is_empty() {
                    format!(
                        "Nothing selected — the agent gets every tool. {MESSAGING_TOOLS} are always added."
                    )
                } else {
                    format!(
                        "{} selected. {MESSAGING_TOOLS} are always added.",
                        selected_tools.len()
                    )
                },
            ))
}

/// The editor's Cancel / Save footer.
fn editor_footer(view: &EditorView, theme: &Theme, cx: &mut Context<SubagentsPage>) -> gpui::Div {
    let EditorView { busy, creating, .. } = view;
    let (busy, creating) = (*busy, *creating);
    let save_label = if busy {
        "Saving…"
    } else if creating {
        "Add subagent"
    } else {
        "Save subagent"
    };
    div()
        .px(px(20.0))
        .py(px(14.0))
        .border_t_1()
        .border_color(theme.border)
        .flex()
        .justify_end()
        .gap(px(8.0))
        .child(
            widgets::ghost_action(theme)
                .id("subagent-cancel")
                .debug_selector(|| "subagent-cancel".into())
                .text_color(theme.text)
                .hover(|s| widgets::ghost_hover(theme, s))
                .child("Cancel")
                .when(busy, |el| el.opacity(0.45))
                .when(!busy, |el| {
                    el.on_click(cx.listener(|page, _, window, cx| page.close_dialog(window, cx)))
                }),
        )
        .child(
            popover::btn_primary(theme, save_label)
                .id("subagent-save")
                .debug_selector(|| "subagent-save".into())
                .when(busy, |el| el.opacity(0.45))
                .when(!busy, |el| {
                    el.on_click(cx.listener(|page, _, _, cx| page.save(cx)))
                }),
        )
}

/// The editor's title, its copy and target device, and the close button.
fn editor_heading(view: &EditorView, theme: &Theme, cx: &mut Context<SubagentsPage>) -> gpui::Div {
    let EditorView {
        busy,
        creating,
        from_builtin,
        original_label,
        label,
        ..
    } = view;
    let (busy, creating, from_builtin) = (*busy, *creating, *from_builtin);
    let heading_title = match (creating, from_builtin, original_label.as_deref()) {
        (true, _, _) => "New subagent".to_string(),
        (false, true, Some(name)) => format!("Customize “{name}”"),
        (false, _, Some(name)) => format!("Edit “{name}”"),
        (false, _, None) => "Subagent".to_string(),
    };
    let heading_copy = if from_builtin {
        "Saving keeps the built-in and adds your own copy of it."
    } else {
        "How this specialist runs when a chat spawns it."
    };

    div()
        .px(px(20.0))
        .pt(px(20.0))
        .flex()
        .items_start()
        .gap(px(12.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(popover::dialog_title(theme, &heading_title).text_size(px(17.0)))
                .child(caption(theme, heading_copy).mt(px(6.0)))
                .child(caption(theme, format!("Target device: {label}")).mt(px(4.0))),
        )
        .child(
            widgets::ghost_action(theme)
                .id("subagent-dialog-close")
                .debug_selector(|| "subagent-dialog-close".into())
                .role(gpui::Role::Button)
                .aria_label("Close")
                .flex_none()
                .hover(|s| widgets::ghost_hover(theme, s))
                .child(
                    icons::icon(icons::CLOSE)
                        .size(px(16.0))
                        .text_color(theme.text_muted),
                )
                .when(busy, |el| el.opacity(0.45))
                .when(!busy, |el| {
                    el.on_click(cx.listener(|page, _, window, cx| page.close_dialog(window, cx)))
                }),
        )
}

/// The editor's per-frame snapshot, taken before the model menu re-borrows
/// the page.
struct EditorView {
    busy: bool,
    creating: bool,
    from_builtin: bool,
    original_label: Option<String>,
    label: String,
    selected_tools: Vec<String>,
    custom_tools: Vec<String>,
    thinking: Option<String>,
    levels: Vec<&'static str>,
    model_unknown: bool,
    model_trigger_label: String,
}

impl Render for SubagentsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let body: AnyElement = match self.agents.clone() {
            Loadable::Idle | Loadable::Loading => widgets::section_card(&theme)
                .p(px(16.0))
                .child(popover::skeleton_rows(
                    "subagents-skeleton",
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
                        .id("subagents-retry")
                        .mt(px(8.0))
                        .text_color(theme.text)
                        .hover(|s| widgets::ghost_hover(&theme, s))
                        .on_click(cx.listener(|page, _, _, cx| page.load(cx)))
                        .child(SharedString::from("Retry")),
                )
                .into_any_element(),
            Loadable::Ready(agents) if agents.is_empty() => widgets::section_card(&theme)
                .p(px(16.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(
                            "No subagents on this device yet. Add one to give a chat a specialist it can spawn.",
                        )),
                )
                .into_any_element(),
            Loadable::Ready(agents) => widgets::section_card(&theme)
                .children(
                    agents
                        .into_iter()
                        .enumerate()
                        .map(|(index, agent)| self.agent_row(&theme, agent, index, cx)),
                )
                .into_any_element(),
        };

        let editing = self.editor.is_some();
        let can_add = self.target.read(cx).can_write(cx)
            && self.busy.is_none()
            && !editing
            && self.delete.is_none();
        // One focused surface at a time, over a scrim that swallows clicks —
        // the provider dialogs' shape. Deliberately NOT dismissed by clicking
        // the scrim: this form holds a prompt someone just typed.
        let modal = if editing {
            Some(popover::modal(
                "subagent-editor-modal",
                window.viewport_size(),
                self.render_editor(window, &theme, cx),
            ))
        } else if self.delete.is_some() {
            Some(popover::modal(
                "subagent-delete-modal",
                window.viewport_size(),
                self.render_delete_confirmation(&theme, cx),
            ))
        } else {
            None
        };
        let count = match &self.agents {
            Loadable::Ready(agents) => Some(agents.len()),
            _ => None,
        };

        div()
            .id("subagents-page")
            .track_focus(&self.page_focus)
            .size_full()
            .relative()
            .overflow_y_scroll()
            .children(modal)
            .child(
                widgets::page_column()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(widgets::page_header(&theme, "Subagents", count))
                            .child(
                                popover::btn_primary(&theme, "New subagent")
                                    .id("subagent-new")
                                    .debug_selector(|| "subagent-new".into())
                                    .when(!can_add, |el| el.opacity(0.45))
                                    .when(can_add, |el| {
                                        el.on_click(cx.listener(|page, _, window, cx| {
                                            page.open_editor(None, window, cx)
                                        }))
                                    }),
                            ),
                    )
                    .child(
                        widgets::page_subtitle(
                            &theme,
                            "Subagents are the specialists a chat can spawn. Each one is a profile on the selected device: its own system prompt, tool allowlist and model.",
                        )
                        .max_w(px(560.0))
                        .line_height(px(20.0)),
                    )
                    .when_some(self.target.read(cx).unavailable(cx), |el, error| {
                        el.child(widgets::warning_strip(&theme, error))
                    })
                    .children(
                        self.error
                            .clone()
                            .map(|message| widgets::error_strip(&theme, message).into_any_element()),
                    )
                    .children(
                        self.notice
                            .clone()
                            .map(|notice| widgets::page_subtitle(&theme, notice)),
                    )
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests;
