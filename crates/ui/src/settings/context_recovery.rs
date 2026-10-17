//! Settings → Agents → Context recovery.
//!
//! Device-scoped like Message translation: the selected device hosts the
//! chats it watches and runs the recovery model. Pickers and the attempt
//! count save as they change; the two text fields save with their button.

use cypher_proto::{ContextRecoverySettings, Model};
use cypher_rpc::methods;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, Render, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};
use std::sync::{Arc, Mutex};

use super::device_target::DeviceTarget;
use super::translation::{dropdown_trigger, mode_button, settings_row};
use super::widgets;
use crate::{
    kit::popover::{self, Loadable},
    kit::theme::Theme,
    state::AppState,
    widgets::text_input::{TextInput, TextInputEvent},
};

/// Which model list a popup edits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    /// The model that writes the context (single choice).
    Recovery,
    /// The session models whose chats are watched (multiple).
    Sessions,
}

pub struct ContextRecoveryControl {
    state: Entity<AppState>,
    target: Entity<DeviceTarget>,
    generation: u64,
    settings: Loadable<ContextRecoverySettings>,
    models: Loadable<Vec<Model>>,
    triggers: Entity<TextInput>,
    instruction: Entity<TextInput>,
    recovery_search: Entity<TextInput>,
    session_search: Entity<TextInput>,
    model: String,
    enabled_models: Vec<String>,
    max_attempts: u32,
    pending_save: Arc<Mutex<Option<ContextRecoverySettings>>>,
    open_picker: Option<Picker>,
    recovery_focus: FocusHandle,
    session_focus: FocusHandle,
    busy: bool,
    error: Option<String>,
    notice: Option<&'static str>,
    task: Option<Task<()>>,
    _target_observer: Subscription,
    _input_observers: Vec<Subscription>,
}

/// Trigger lines as saved: trimmed, blank lines dropped.
fn trigger_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

impl ContextRecoveryControl {
    pub fn new(
        state: Entity<AppState>,
        target: Entity<DeviceTarget>,
        cx: &mut Context<Self>,
    ) -> Self {
        let triggers = cx.new(|cx| {
            TextInput::settings_prompt_field(
                "e.g. I can't help with that\ncontext_length_exceeded",
                cx,
            )
        });
        let instruction = cx.new(|cx| {
            TextInput::settings_prompt_field("What the recovery model should write…", cx)
        });
        let recovery_search =
            cx.new(|cx| TextInput::settings_field("Search available models…", false, cx));
        let session_search =
            cx.new(|cx| TextInput::settings_field("Search available models…", false, cx));
        let mut input_observers = Vec::new();
        for input in [&triggers, &instruction, &recovery_search, &session_search] {
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
                page.settings = Loadable::Idle;
                page.busy = false;
                // The old device's worker keeps its own queue and finishes
                // accepted edits even after the user moves to another device.
                page.pending_save = Arc::default();
                page.error = None;
                page.notice = None;
                page.task = None;
                page.open_picker = None;
                page.load(cx);
            }
            cx.notify();
        });
        let mut page = Self {
            state,
            target,
            generation,
            settings: Loadable::Idle,
            models: Loadable::Idle,
            triggers,
            instruction,
            recovery_search,
            session_search,
            model: String::new(),
            enabled_models: Vec::new(),
            max_attempts: 1,
            pending_save: Arc::default(),
            open_picker: None,
            recovery_focus: cx.focus_handle(),
            session_focus: cx.focus_handle(),
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

    fn load(&mut self, cx: &mut Context<Self>) {
        let Ok(ticket) = self.target.read(cx).ticket(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.settings = Loadable::Loading;
        self.models = Loadable::Loading;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::GET_CONTEXT_RECOVERY_SETTINGS,
                    ticket.params(serde_json::json!({})),
                )
                .await;
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
                match result {
                    Ok(value) => match serde_json::from_value::<ContextRecoverySettings>(value) {
                        Ok(settings) => {
                            page.set_inputs(&settings, cx);
                            page.settings = Loadable::Ready(settings);
                        }
                        Err(error) => page.settings = Loadable::Error(error.to_string()),
                    },
                    Err(error) => {
                        page.settings = Loadable::Error(format!("{}: {error}", ticket.label));
                    }
                }
                page.models = match models {
                    Ok(value) => serde_json::from_value(value)
                        .map(Loadable::Ready)
                        .unwrap_or_else(|error| Loadable::Error(error.to_string())),
                    Err(error) => {
                        Loadable::Error(format!("Couldn't load this device's Pi models: {error}"))
                    }
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_inputs(&mut self, settings: &ContextRecoverySettings, cx: &mut Context<Self>) {
        let triggers = settings.triggers.join("\n");
        self.triggers
            .update(cx, |input, cx| input.set_text(triggers, cx));
        let instruction = settings.instruction.clone();
        self.instruction
            .update(cx, |input, cx| input.set_text(instruction, cx));
        self.model = settings.model.clone();
        self.enabled_models = settings.enabled_models.clone();
        self.max_attempts = settings.max_attempts;
    }

    /// The settings as the card shows them now, unsaved text included.
    fn draft(&self, cx: &Context<Self>) -> ContextRecoverySettings {
        ContextRecoverySettings {
            triggers: trigger_lines(self.triggers.read(cx).text()),
            instruction: self.instruction.read(cx).text().trim().to_owned(),
            model: self.model.clone(),
            enabled_models: self.enabled_models.clone(),
            max_attempts: self.max_attempts,
        }
    }

    /// Whether the text fields differ from what the device has.
    fn text_dirty(&self, cx: &Context<Self>) -> bool {
        let Some(saved) = self.settings.ready() else {
            return false;
        };
        let draft = self.draft(cx);
        draft.triggers != saved.triggers || draft.instruction != saved.instruction.trim()
    }

    fn save(&mut self, notice: Option<&'static str>, cx: &mut Context<Self>) {
        if !self.target.read(cx).can_write(cx) || self.settings.ready().is_none() {
            return;
        }
        let draft = self.draft(cx);
        self.call_set(draft, notice, cx);
    }

    fn call_set(
        &mut self,
        settings: ContextRecoverySettings,
        notice: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        // One in-flight save and one latest draft, as on the translation
        // card: rapid picker changes never land out of order.
        if self.busy {
            *crate::kit::lock(&self.pending_save) = Some(settings);
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
        let pending = self.pending_save.clone();
        cx.spawn(async move |this, cx| {
            let mut settings = settings;
            loop {
                let result = engine
                    .client()
                    .call(
                        methods::SET_CONTEXT_RECOVERY_SETTINGS,
                        ticket.params(serde_json::to_value(&settings).unwrap_or_default()),
                    )
                    .await;
                let next = crate::kit::lock(&pending).take();
                this.update(cx, |page, cx| {
                    if !page.target.read(cx).matches(&ticket) {
                        return;
                    }
                    page.busy = next.is_some();
                    match result {
                        Ok(value) => match serde_json::from_value(value) {
                            Ok(settings) => {
                                page.settings = Loadable::Ready(settings);
                                page.error = None;
                                if next.is_none() {
                                    page.notice = notice;
                                }
                            }
                            Err(error) => page.error = Some(error.to_string()),
                        },
                        Err(error) => page.error = Some(format!("{}: {error}", ticket.label)),
                    }
                    cx.notify();
                })
                .ok();
                match next {
                    Some(next) => settings = next,
                    None => break,
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn choose_model(&mut self, model: String, cx: &mut Context<Self>) {
        self.model = model;
        self.open_picker = None;
        self.save(None, cx);
    }

    fn toggle_session_model(&mut self, model: String, cx: &mut Context<Self>) {
        if let Some(index) = self.enabled_models.iter().position(|id| id == &model) {
            self.enabled_models.remove(index);
        } else {
            self.enabled_models.push(model);
        }
        self.save(None, cx);
    }

    fn set_attempts(&mut self, attempts: u32, cx: &mut Context<Self>) {
        if !self.target.read(cx).can_write(cx) {
            return;
        }
        self.max_attempts = attempts;
        self.save(None, cx);
    }

    fn toggle_picker(&mut self, picker: Picker, window: &mut Window, cx: &mut Context<Self>) {
        if !self.target.read(cx).can_write(cx) {
            return;
        }
        self.open_picker = (self.open_picker != Some(picker)).then_some(picker);
        let (search, trigger) = match picker {
            Picker::Recovery => (&self.recovery_search, &self.recovery_focus),
            Picker::Sessions => (&self.session_search, &self.session_focus),
        };
        if self.open_picker.is_some() {
            search.focus_handle(cx).focus(window, cx);
        } else {
            trigger.focus(window, cx);
        }
        cx.notify();
    }

    fn model_label(&self, id: &str) -> String {
        self.models
            .ready()
            .and_then(|models| models.iter().find(|model| model.id == id))
            .map(|model| model.label.clone())
            .unwrap_or_else(|| id.to_string())
    }
}

impl ContextRecoveryControl {
    fn model_row(
        &self,
        model: &Model,
        selected: bool,
        picker: Picker,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = model.id.clone();
        let enabled = self.target.read(cx).can_write(cx);
        div()
            .id(SharedString::from(format!(
                "recovery-model-{}-{}",
                match picker {
                    Picker::Recovery => "recovery",
                    Picker::Sessions => "session",
                },
                id
            )))
            .px(px(10.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .bg(if selected {
                theme.ink(0.07)
            } else {
                gpui::transparent_black()
            })
            .when(enabled, |row| {
                row.cursor_pointer()
                    .hover(|style| style.bg(theme.ink(0.05)))
            })
            .opacity(if enabled { 1.0 } else { 0.5 })
            .on_click(cx.listener(move |page, _, _, cx| {
                if !enabled {
                    return;
                }
                match picker {
                    Picker::Recovery => page.choose_model(id.clone(), cx),
                    Picker::Sessions => page.toggle_session_model(id.clone(), cx),
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.0))
                            .text_color(theme.text)
                            .child(SharedString::from(model.label.clone())),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(model.id.clone())),
                    ),
            )
            .child(div().w(px(16.0)).when(selected, |row| {
                row.child(
                    crate::kit::icons::icon(crate::kit::icons::CHECK)
                        .size(px(12.0))
                        .text_color(theme.accent),
                )
            }))
            .into_any_element()
    }

    fn model_popup(&mut self, picker: Picker, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let search = match picker {
            Picker::Recovery => self.recovery_search.clone(),
            Picker::Sessions => self.session_search.clone(),
        };
        let query = search.read(cx).text().trim().to_lowercase();
        let mut menu = div()
            .w(px(340.0))
            .rounded(px(10.0))
            .bg(theme.surface_overlay)
            .border_1()
            .border_color(theme.border_strong)
            .on_mouse_down_out(cx.listener(|page, _, window, cx| {
                page.open_picker = None;
                page.recovery_focus.focus(window, cx);
                cx.notify();
            }))
            .child(div().p(px(8.0)).child(search));
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
                                "No Pi models available."
                            } else {
                                "No matching models."
                            }),
                    );
                } else {
                    menu = menu.child(
                        div()
                            .id(match picker {
                                Picker::Recovery => "recovery-model-list",
                                Picker::Sessions => "recovery-session-model-list",
                            })
                            .max_h(px(280.0))
                            .overflow_y_scroll()
                            .px(px(4.0))
                            .pb(px(4.0))
                            .children(filtered.into_iter().map(|model| {
                                let selected = match picker {
                                    Picker::Recovery => self.model == model.id,
                                    Picker::Sessions => {
                                        self.enabled_models.iter().any(|id| id == &model.id)
                                    }
                                };
                                self.model_row(model, selected, picker, theme, cx)
                            })),
                    );
                }
            }
            Loadable::Error(error) => {
                menu = menu.child(widgets::error_strip(theme, error).mt(px(0.0)))
            }
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
        popover::anchored_menu_below(
            match picker {
                Picker::Recovery => "recovery-model-popup",
                Picker::Sessions => "recovery-session-popup",
            },
            menu.into_any_element(),
            None,
        )
    }

    /// A labelled multi-line field, in the Permissions page's style.
    fn text_field(
        theme: &Theme,
        label: &'static str,
        copy: &'static str,
        input: &Entity<TextInput>,
    ) -> gpui::Div {
        div()
            .child(widgets::field_label(theme, label))
            .child(
                div()
                    .mt(px(2.0))
                    .mb(px(6.0))
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .text_color(theme.text_muted)
                    .child(copy),
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
}

impl Render for ContextRecoveryControl {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let Loadable::Ready(saved) = self.settings.clone() else {
            return widgets::section_card(&theme)
                .mt(px(24.0))
                .p(px(16.0))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(match &self.settings {
                            Loadable::Error(error) => SharedString::from(error.clone()),
                            _ => "Loading context recovery settings…".into(),
                        }),
                )
                .into_any_element();
        };
        let model_count = self.models.ready().map_or(0, Vec::len);
        let writable = self.target.read(cx).can_write(cx);
        // What the device acts on: the saved settings, not unsaved text.
        let active = saved
            .enabled_models
            .iter()
            .any(|model| saved.enabled_for(Some(model)));
        let recovery_label = if self.model.is_empty() {
            "Choose a model".to_string()
        } else {
            self.model_label(&self.model)
        };
        let session_label = match self.enabled_models.len() {
            0 => "No models selected".to_string(),
            1 => self.model_label(&self.enabled_models[0]),
            count => format!("{count} models selected"),
        };
        let recovery_popup = (self.open_picker == Some(Picker::Recovery))
            .then(|| self.model_popup(Picker::Recovery, &theme, cx));
        let session_popup = (self.open_picker == Some(Picker::Sessions))
            .then(|| self.model_popup(Picker::Sessions, &theme, cx));
        let dirty = self.text_dirty(cx);
        let attempts = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.0))
            .p(px(3.0))
            .rounded(px(8.0))
            .bg(theme.surface_raised)
            .children((1..=ContextRecoverySettings::MAX_ATTEMPTS_LIMIT).map(|n| {
                mode_button(
                    &theme,
                    match n {
                        1 => "recovery-attempts-1",
                        2 => "recovery-attempts-2",
                        _ => "recovery-attempts-3",
                    },
                    match n {
                        1 => "1",
                        2 => "2",
                        _ => "3",
                    },
                    self.max_attempts == n,
                )
                .on_click(cx.listener(move |page, _, _, cx| page.set_attempts(n, cx)))
            }));
        widgets::section_card(&theme)
            .mt(px(24.0))
            .child(recovery_header(active, &theme))
            .child(
                div()
                    .border_t_1()
                    .border_color(theme.border)
                    .px(px(20.0))
                    .py(px(18.0))
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(
                        settings_row(
                            &theme,
                            "Enable for session models",
                            Some(
                                format!(
                                    "{} of {} models selected",
                                    self.enabled_models.len(),
                                    model_count
                                )
                                .into(),
                            ),
                        )
                        .child(
                            dropdown_trigger(
                                &theme,
                                "recovery-session-trigger",
                                &self.session_focus,
                                220.0,
                                SharedString::from(session_label),
                                !self.enabled_models.is_empty(),
                                writable,
                            )
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.toggle_picker(Picker::Sessions, window, cx)
                            }))
                            .children(session_popup),
                        ),
                    )
                    .child(
                        settings_row(
                            &theme,
                            "Write context with",
                            Some("A model from this device\u{2019}s Pi catalog".into()),
                        )
                        .child(
                            dropdown_trigger(
                                &theme,
                                "recovery-model-trigger",
                                &self.recovery_focus,
                                220.0,
                                SharedString::from(recovery_label),
                                false,
                                writable,
                            )
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.toggle_picker(Picker::Recovery, window, cx)
                            }))
                            .children(recovery_popup),
                        ),
                    )
                    .child(
                        settings_row(
                            &theme,
                            "Attempts per message",
                            Some("How many times one message is restarted before a failure is left standing".into()),
                        )
                        .child(attempts),
                    )
                    .child(
                        div()
                            .border_t_1()
                            .border_color(theme.border)
                            .pt(px(16.0))
                            .flex()
                            .flex_col()
                            .gap(px(14.0))
                            .child(Self::text_field(
                                &theme,
                                "Trigger phrases",
                                "One per line. A turn whose answer or error contains one, in any letter case, is restarted.",
                                &self.triggers,
                            ))
                            .child(Self::text_field(
                                &theme,
                                "Context instruction",
                                "What the model is asked to write. It also receives your message, the agent\u{2019}s answer and any error.",
                                &self.instruction,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .gap(px(10.0))
                                    .when_some(self.notice, |el, notice| {
                                        el.child(
                                            div()
                                                .text_size(px(12.0))
                                                .text_color(theme.text_muted)
                                                .child(notice),
                                        )
                                    })
                                    .child(
                                        popover::btn_primary(
                                            &theme,
                                            if self.busy { "Saving…" } else { "Save" },
                                        )
                                        .id("recovery-save-text")
                                        .when(!(writable && dirty), |el| el.opacity(0.45))
                                        .when(writable && dirty, |el| {
                                            el.cursor_pointer().on_click(cx.listener(
                                                |page, _, _, cx| {
                                                    page.save(
                                                        Some("Saved. The next failed turn uses these."),
                                                        cx,
                                                    )
                                                },
                                            ))
                                        }),
                                    ),
                            ),
                    )
                    .children(
                        self.error
                            .clone()
                            .map(|error| widgets::error_strip(&theme, error)),
                    ),
            )
            .into_any_element()
    }
}

/// The card's header: icon, title and caption, and the Enabled badge.
fn recovery_header(active: bool, theme: &Theme) -> gpui::Div {
    div()
        .px(px(20.0))
        .py(px(18.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(widgets::row_tile(theme, crate::kit::icons::RESTART))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(widgets::row_title(theme, "Context recovery"))
                .child(
                    div()
                        .mt(px(3.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(
                            "When a turn fails with a trigger phrase, restart it from before your message with context a model writes",
                        ),
                ),
        )
        .child(if active {
            widgets::badge_active(theme, "Enabled")
        } else {
            widgets::badge(theme, "Disabled")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_lines_drop_blanks_and_outer_space() {
        assert_eq!(
            trigger_lines("  I can't help \n\n\tcontext_length_exceeded\n   "),
            vec![
                "I can't help".to_owned(),
                "context_length_exceeded".to_owned()
            ]
        );
    }
}
