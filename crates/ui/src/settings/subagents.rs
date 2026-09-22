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

use gpui::{
    Context, Entity, Focusable, IntoElement, KeyDownEvent, Render, SharedString, Subscription,
    Task, Window, div, prelude::*, px,
};

use cypher_engine::pi_subagents::PiSubagent;
use cypher_rpc::methods;

use super::device_target::{DeviceTarget, DeviceTicket};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons;
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// The open editor. `original` is the profile it was opened on: `None` means
/// "create", and a changed name means "rename" — both decided by the engine,
/// which owns the file moves.
struct Editor {
    original: Option<String>,
    /// Opened from a built-in: saving writes a user-level override of it.
    from_builtin: bool,
    ticket: DeviceTicket,
    read_only: bool,
    name: Entity<ComposerInput>,
    description: Entity<ComposerInput>,
    model: Entity<ComposerInput>,
    thinking: Entity<ComposerInput>,
    tools: Entity<ComposerInput>,
    prompt: Entity<ComposerInput>,
    _events: Vec<Subscription>,
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
    error: Option<String>,
    notice: Option<String>,
    busy: Option<String>,
    load_task: Option<Task<()>>,
    editor: Option<Editor>,
    delete: Option<DeleteConfirmation>,
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
            error: None,
            notice: None,
            busy: None,
            load_task: None,
            editor: None,
            delete: None,
        };
        page.load(cx);
        page
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
            let input = cx.new(|cx| ComposerInput::settings_field(placeholder, false, cx));
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
        let model = field(
            "Inherit the parent's model",
            existing
                .and_then(|a| a.model.as_deref())
                .unwrap_or_default(),
            cx,
        );
        let thinking = field(
            "Inherit — e.g. low, high, xhigh, max",
            existing
                .and_then(|a| a.thinking.as_deref())
                .unwrap_or_default(),
            cx,
        );
        let tools = field(
            "All tools — or a list like read, grep, find, ls",
            &existing.map(|a| a.tools.join(", ")).unwrap_or_default(),
            cx,
        );
        let prompt = cx.new(|cx| {
            let mut input = ComposerInput::settings_prompt_field(
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
        for input in [&name, &description, &model, &thinking, &tools, &prompt] {
            events.push(
                cx.subscribe(input, |page: &mut Self, _, event, cx| match event {
                    // Enter in a value field saves; the prompt field binds
                    // Enter to a newline and never emits this.
                    ComposerInputEvent::Submitted => page.save(cx),
                    ComposerInputEvent::Edited => {
                        page.error = None;
                        cx.notify();
                    }
                    _ => {}
                }),
            );
        }

        self.notice = None;
        self.error = None;
        self.delete = None;
        self.editor = Some(Editor {
            original: existing.map(|a| a.name.clone()),
            from_builtin: existing.is_some_and(|a| a.builtin),
            ticket,
            read_only: existing.is_some_and(|a| a.read_only),
            name,
            description,
            model,
            thinking,
            tools,
            prompt,
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

    /// Build the save request from the open editor.
    fn save_request(&self, cx: &Context<Self>) -> Option<serde_json::Value> {
        let editor = self.editor.as_ref()?;
        let text = |input: &Entity<ComposerInput>| input.read(cx).text().trim().to_string();
        let optional = |input: &Entity<ComposerInput>| {
            let value = text(input);
            (!value.is_empty()).then_some(value)
        };
        let tools: Vec<String> = text(&editor.tools)
            .split(',')
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
            .map(str::to_string)
            .collect();
        let mut params = serde_json::json!({
            "name": text(&editor.name),
            "description": text(&editor.description),
            "systemPrompt": editor.prompt.read(cx).text().trim(),
            "tools": tools,
            "readOnly": editor.read_only,
        });
        if let Some(object) = params.as_object_mut() {
            if let Some(model) = optional(&editor.model) {
                object.insert("model".into(), model.into());
            }
            if let Some(thinking) = optional(&editor.thinking) {
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
    ) -> gpui::AnyElement {
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
        // how wide the tool allowlist is.
        let mut facts: Vec<gpui::AnyElement> = Vec::new();
        if let Some(model) = agent.model.as_deref() {
            facts.push(
                div()
                    .child(SharedString::from(model.to_string()))
                    .into_any_element(),
            );
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

    fn render_editor(&mut self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let editor = self.editor.as_ref().unwrap();
        let busy = self.busy.is_some();
        let creating = editor.original.is_none();
        let from_builtin = editor.from_builtin;
        let read_only = editor.read_only;
        let label = editor.ticket.label.clone();

        let input_row =
            |label: &'static str, hint: Option<&'static str>, input: &Entity<ComposerInput>| {
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
            };

        let mut fields = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(input_row("Name", None, &editor.name))
            .child(input_row(
                "Description",
                Some("Required: the extension ignores a profile without one, and the model reads it to pick an agent."),
                &editor.description,
            ))
            .child(input_row("Model", None, &editor.model))
            .child(input_row("Thinking", None, &editor.thinking))
            .child(input_row(
                "Tools",
                Some("Comma-separated. Leave empty to allow every tool. The messaging tools are always added."),
                &editor.tools,
            ))
            .child(input_row("System prompt", None, &editor.prompt));

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

        if from_builtin {
            fields = fields.child(widgets::page_subtitle(
                theme,
                format!(
                    "Saving writes your own copy on {label}. The built-in stays untouched, and deleting your copy brings it back."
                ),
            ));
        } else {
            fields = fields.child(widgets::page_subtitle(
                theme,
                format!(
                    "Saved to {label}. Runs already in flight keep the profile they started with."
                ),
            ));
        }

        let save_label = if busy {
            "Saving…"
        } else if creating {
            "Add subagent"
        } else {
            "Save subagent"
        };
        fields = fields.child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    widgets::ghost_action(theme)
                        .id("subagent-cancel")
                        .text_color(theme.text)
                        .hover(|s| widgets::ghost_hover(theme, s))
                        .child("Cancel")
                        .when(busy, |el| el.opacity(0.45))
                        .when(!busy, |el| {
                            el.on_click(cx.listener(|page, _, _, cx| {
                                page.editor = None;
                                page.error = None;
                                cx.notify();
                            }))
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
                ),
        );

        widgets::section_card(theme)
            .id("subagent-editor")
            .mt(px(16.0))
            .p(px(20.0))
            .on_key_down(cx.listener(|page, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && page.busy.is_none() {
                    page.editor = None;
                    page.error = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(fields)
            .into_any_element()
    }

    fn render_delete_confirmation(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
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
        widgets::section_card(theme)
            .id("subagent-delete-confirmation")
            .mt(px(16.0))
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(widgets::row_title(theme, title))
            .child(widgets::page_subtitle(theme, body))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        widgets::ghost_action(theme)
                            .id("subagent-delete-cancel")
                            .text_color(theme.text)
                            .hover(|s| widgets::ghost_hover(theme, s))
                            .child("Cancel")
                            .when(busy, |el| el.opacity(0.45))
                            .when(!busy, |el| {
                                el.on_click(cx.listener(|page, _, _, cx| {
                                    page.delete = None;
                                    page.error = None;
                                    cx.notify();
                                }))
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

impl Render for SubagentsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let body: gpui::AnyElement = match self.agents.clone() {
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
        let editor = editing.then(|| self.render_editor(&theme, cx));
        let delete = self
            .delete
            .as_ref()
            .map(|_| self.render_delete_confirmation(&theme, cx));
        let count = match &self.agents {
            Loadable::Ready(agents) => Some(agents.len()),
            _ => None,
        };

        div()
            .id("subagents-page")
            .size_full()
            .overflow_y_scroll()
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
                    .children(editor)
                    .children(delete)
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::setup::tests::pump_until;
    use cypher_engine::pi_runtime::PiRuntimePaths;
    use cypher_engine::pi_subagents;
    use gpui::AppContext;
    use std::sync::Arc;

    /// A stand-in engine that serves the three subagent methods out of a real
    /// temp runtime, so the page is exercised against the ACTUAL file format
    /// rather than a hand-written fake reply.
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

    #[gpui::test]
    fn subagents_page_creates_customizes_and_resets_profiles(cx: &mut gpui::TestAppContext) {
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
        // One built-in, exactly as the extension ships it.
        std::fs::create_dir_all(pi_subagents::builtin_dir(&paths)).unwrap();
        std::fs::write(
            pi_subagents::builtin_dir(&paths).join("reviewer.md"),
            "---\nname: reviewer\ndescription: Verify completed work\ntools: read, grep\n---\nYou verify.\n",
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
            cx.set_global(Theme::for_appearance(crate::theme::Appearance::Dark));
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

        let window = cx.open_window(gpui::size(px(1100.0), px(1400.0)), |_, cx| {
            SubagentsPage::new(state, target.clone(), cx)
        });
        let page = window.root(cx).unwrap();
        pump_until(cx, || {
            cx.update(|cx| matches!(page.read(cx).agents, Loadable::Ready(_)))
        });

        // The built-in is listed and marked as one.
        cx.update(|cx| {
            let agents = page.read(cx).agents.ready().unwrap().clone();
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0].name, "reviewer");
            assert!(agents[0].builtin);
            assert!(!agents[0].overrides_builtin);
        });

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|w, cx| {
            w.refresh();
            w.draw(cx).clear();
        });

        // Create a brand-new profile.
        let new_button = visual.debug_bounds("subagent-new").unwrap();
        visual.simulate_click(new_button.center(), Default::default());
        page.update(cx, |page, cx| {
            let editor = page.editor.as_ref().expect("editor opens");
            assert!(editor.original.is_none());
            editor.name.update(cx, |v, cx| v.set_text("planner", cx));
            editor
                .description
                .update(cx, |v, cx| v.set_text("Resolve a design decision", cx));
            editor
                .tools
                .update(cx, |v, cx| v.set_text("read, bash , grep", cx));
            editor.thinking.update(cx, |v, cx| v.set_text("xhigh", cx));
            editor.prompt.update(cx, |v, cx| {
                v.set_text("You are the planner.\n\nSecond line.", cx)
            });
            cx.notify();
        });
        visual.update(|w, cx| {
            w.refresh();
            w.draw(cx).clear();
        });
        let save = visual.debug_bounds("subagent-save").unwrap();
        visual.simulate_click(save.center(), Default::default());
        pump_until(cx, || cx.update(|cx| page.read(cx).editor.is_none()));

        // It reached disk in the extension's own format, prompt newlines and
        // comma-separated tools included.
        let written =
            std::fs::read_to_string(pi_subagents::user_dir(&paths).join("planner.md")).unwrap();
        assert!(written.contains("name: planner"), "{written}");
        assert!(written.contains("tools: read, bash, grep"), "{written}");
        assert!(written.contains("thinking: xhigh"), "{written}");
        assert!(
            written.contains("You are the planner.\n\nSecond line."),
            "{written}"
        );
        cx.update(|cx| {
            let agents = page.read(cx).agents.ready().unwrap().clone();
            assert_eq!(agents.len(), 2);
            assert_eq!(agents[0].name, "planner");
            assert_eq!(agents[0].tools, ["read", "bash", "grep"]);
        });

        // Customizing the BUILT-IN writes an override, leaving the package
        // file alone.
        let builtin = cx.update(|cx| {
            page.read(cx)
                .agents
                .ready()
                .unwrap()
                .iter()
                .find(|a| a.builtin)
                .cloned()
                .unwrap()
        });
        cx.update(|cx| {
            page.update(cx, |page, cx| {
                page.agents = Loadable::Ready(page.agents.ready().unwrap().clone());
                cx.notify();
            })
        });
        visual.update(|w, cx| {
            page.update(cx, |page, cx| page.open_editor(Some(builtin), w, cx));
        });
        page.update(cx, |page, cx| {
            let editor = page.editor.as_ref().expect("editor opens");
            assert!(editor.from_builtin);
            editor
                .description
                .update(cx, |v, cx| v.set_text("My stricter reviewer", cx));
            page.save(cx);
        });
        pump_until(cx, || cx.update(|cx| page.read(cx).editor.is_none()));
        cx.update(|cx| {
            let reviewer = page
                .read(cx)
                .agents
                .ready()
                .unwrap()
                .iter()
                .find(|a| a.name == "reviewer")
                .cloned()
                .unwrap();
            assert!(!reviewer.builtin);
            assert!(reviewer.overrides_builtin);
            assert_eq!(reviewer.description, "My stricter reviewer");
        });
        assert!(
            std::fs::read_to_string(pi_subagents::builtin_dir(&paths).join("reviewer.md"))
                .unwrap()
                .contains("Verify completed work"),
            "the shipped built-in must not be edited"
        );

        // Resetting the override restores the built-in instead of removing it.
        let override_row = cx.update(|cx| {
            page.read(cx)
                .agents
                .ready()
                .unwrap()
                .iter()
                .find(|a| a.overrides_builtin)
                .cloned()
                .unwrap()
        });
        page.update(cx, |page, cx| page.request_delete(&override_row, cx));
        cx.update(|cx| {
            let confirmation = page.read(cx).delete.as_ref().unwrap();
            assert!(confirmation.restores_builtin);
        });
        visual.update(|w, cx| {
            w.refresh();
            w.draw(cx).clear();
        });
        let confirm = visual.debug_bounds("subagent-delete-confirm").unwrap();
        visual.simulate_click(confirm.center(), Default::default());
        pump_until(cx, || cx.update(|cx| page.read(cx).delete.is_none()));
        cx.update(|cx| {
            let reviewer = page
                .read(cx)
                .agents
                .ready()
                .unwrap()
                .iter()
                .find(|a| a.name == "reviewer")
                .cloned()
                .unwrap();
            assert!(reviewer.builtin, "the built-in returns");
            assert_eq!(reviewer.description, "Verify completed work");
        });

        // A device switch drops the open editor rather than retargeting it.
        visual.update(|w, cx| {
            page.update(cx, |page, cx| page.open_editor(None, w, cx));
        });
        cx.update(|cx| assert!(page.read(cx).editor.is_some()));
        target.update(cx, |t, cx| t.select(None, cx).unwrap());
        cx.run_until_parked();
        cx.update(|cx| assert!(page.read(cx).editor.is_none()));
    }
}
