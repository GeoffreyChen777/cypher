//! Settings → Agents → Message translation.
//!
//! The control is device-scoped: the selected device owns both the Pi
//! extension and the translation model credentials.

use cypher_engine::pi_translation::{PiTranslationSettings, TranslationOutputMode};
use cypher_proto::Model;
use cypher_rpc::methods;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, Render, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};
use std::sync::{Arc, Mutex};

use super::device_target::DeviceTarget;
use super::widgets;
use crate::{
    composer::{ComposerInput, ComposerInputEvent},
    popover::{self, Loadable},
    state::AppState,
    theme::Theme,
};

/// Offering a language the offline detector cannot judge is worse than not
/// offering it: asked about a language it was not built with, the detector does
/// not answer "unknown", it answers with whichever language it does know, and a
/// confident wrong answer is what lets a message be skipped as "already in the
/// destination language". This list therefore tracks `LANGUAGES` in
/// `cypher_engine::pi_translation` and `LANGUAGE_ALIASES` in the extension.
///
/// A language saved before this list was trimmed still shows its stored name
/// here (the label falls back to the raw value) and still translates — the
/// extension never lets an unrecognized language skip the model.
const LANGUAGE_OPTIONS: &[(&str, &str)] = &[
    ("Auto detect", "auto"),
    ("English", "English"),
    ("Chinese", "Chinese"),
];

pub struct TranslationSettings {
    state: Entity<AppState>,
    target: Entity<DeviceTarget>,
    generation: u64,
    settings: Loadable<PiTranslationSettings>,
    models: Loadable<Vec<Model>>,
    source: Entity<ComposerInput>,
    target_language: Entity<ComposerInput>,
    translation_search: Entity<ComposerInput>,
    session_search: Entity<ComposerInput>,
    translation_model: String,
    enabled_models: Vec<String>,
    output_mode: TranslationOutputMode,
    pending_save: Arc<Mutex<Option<PiTranslationSettings>>>,
    translation_menu_open: bool,
    session_menu_open: bool,
    source_menu_open: bool,
    target_menu_open: bool,
    translation_focus: FocusHandle,
    session_focus: FocusHandle,
    source_focus: FocusHandle,
    target_focus: FocusHandle,
    busy: bool,
    error: Option<String>,
    task: Option<Task<()>>,
    _target_observer: Subscription,
    _input_observers: Vec<Subscription>,
}

impl TranslationSettings {
    pub fn new(
        state: Entity<AppState>,
        target: Entity<DeviceTarget>,
        cx: &mut Context<Self>,
    ) -> Self {
        let source = cx.new(|cx| ComposerInput::settings_field("auto or Chinese", false, cx));
        let target_language = cx.new(|cx| ComposerInput::settings_field("English", false, cx));
        let translation_search =
            cx.new(|cx| ComposerInput::settings_field("Search available models…", false, cx));
        let session_search =
            cx.new(|cx| ComposerInput::settings_field("Search available models…", false, cx));
        let mut input_observers = Vec::new();
        for input in [&translation_search, &session_search] {
            input_observers.push(cx.subscribe(input, |_: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Edited) {
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
                // The old device's worker retains its own queue and finishes
                // accepted edits even if the user navigates to another device.
                page.pending_save = Arc::default();
                page.error = None;
                page.task = None;
                page.translation_menu_open = false;
                page.session_menu_open = false;
                page.source_menu_open = false;
                page.target_menu_open = false;
                page.clear_inputs(cx);
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
            source,
            target_language,
            translation_search,
            session_search,
            translation_model: String::new(),
            enabled_models: Vec::new(),
            output_mode: TranslationOutputMode::default(),
            pending_save: Arc::default(),
            translation_menu_open: false,
            session_menu_open: false,
            source_menu_open: false,
            target_menu_open: false,
            translation_focus: cx.focus_handle(),
            session_focus: cx.focus_handle(),
            source_focus: cx.focus_handle(),
            target_focus: cx.focus_handle(),
            busy: false,
            error: None,
            task: None,
            _target_observer: observer,
            _input_observers: input_observers,
        };
        page.load(cx);
        page
    }

    fn clear_inputs(&mut self, cx: &mut Context<Self>) {
        self.source.update(cx, |input, cx| input.set_text("", cx));
        self.target_language
            .update(cx, |input, cx| input.set_text("", cx));
        self.translation_search
            .update(cx, |input, cx| input.set_text("", cx));
        self.session_search
            .update(cx, |input, cx| input.set_text("", cx));
        self.translation_model.clear();
        self.enabled_models.clear();
        self.source_menu_open = false;
        self.target_menu_open = false;
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
                    methods::GET_PI_TRANSLATION_SETTINGS,
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
                    Ok(value) => match serde_json::from_value::<PiTranslationSettings>(value) {
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

    fn set_inputs(&mut self, settings: &PiTranslationSettings, cx: &mut Context<Self>) {
        self.source.update(cx, |input, cx| {
            input.set_text(settings.source_language.clone(), cx)
        });
        self.target_language.update(cx, |input, cx| {
            input.set_text(settings.target_language.clone(), cx)
        });
        self.translation_model = settings.translation_model.clone();
        self.enabled_models = settings.enabled_models.clone();
        self.output_mode = settings.output_mode;
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if !self.target.read(cx).can_write(cx) {
            return;
        }
        let current = match &self.settings {
            Loadable::Ready(settings) => settings.clone(),
            _ => return,
        };
        let settings = PiTranslationSettings {
            source_language: self.source.read(cx).text().trim().to_string(),
            target_language: self.target_language.read(cx).text().trim().to_string(),
            translation_model: self.translation_model.clone(),
            enabled_models: self.enabled_models.clone(),
            output_mode: self.output_mode,
            translate_user_messages: current.translate_user_messages,
            translate_final_responses: current.translate_final_responses,
        };
        self.call_set(settings, cx);
    }

    fn set_output_mode(&mut self, mode: TranslationOutputMode, cx: &mut Context<Self>) {
        if !self.target.read(cx).can_write(cx) {
            return;
        }
        self.output_mode = mode;
        self.save(cx);
    }

    fn choose_translation_model(&mut self, model: String, cx: &mut Context<Self>) {
        self.translation_model = model;
        self.save(cx);
    }

    fn toggle_session_model(&mut self, model: String, cx: &mut Context<Self>) {
        if let Some(index) = self.enabled_models.iter().position(|id| id == &model) {
            self.enabled_models.remove(index);
        } else {
            self.enabled_models.push(model);
        }
        self.save(cx);
    }

    fn call_set(&mut self, settings: PiTranslationSettings, cx: &mut Context<Self>) {
        // One in-flight save and one latest draft: rapid multi-select changes
        // stay interactive without out-of-order writes overwriting newer edits.
        if self.busy {
            *self.pending_save.lock().unwrap() = Some(settings);
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
        let pending = self.pending_save.clone();
        // The immutable ticket routes this write; generation checks discard
        // old replies if the user switches device. No global device lock.
        cx.spawn(async move |this, cx| {
            let mut settings = settings;
            loop {
                let result = engine
                    .client()
                    .call(
                        methods::SET_PI_TRANSLATION_SETTINGS,
                        ticket.params(serde_json::to_value(&settings).unwrap_or_default()),
                    )
                    .await;
                let next = pending.lock().unwrap().take();
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
}

impl TranslationSettings {
    fn toggle_language_menu(&mut self, source: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.target.read(cx).can_write(cx) {
            return;
        }
        if source {
            self.source_menu_open = !self.source_menu_open;
            self.target_menu_open = false;
            self.translation_menu_open = false;
            self.session_menu_open = false;
            if self.source_menu_open {
                self.source_focus.focus(window, cx);
            }
        } else {
            self.target_menu_open = !self.target_menu_open;
            self.source_menu_open = false;
            self.translation_menu_open = false;
            self.session_menu_open = false;
            if self.target_menu_open {
                self.target_focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    fn choose_language(&mut self, source: bool, value: &'static str, cx: &mut Context<Self>) {
        let input = if source {
            &self.source
        } else {
            &self.target_language
        };
        input.update(cx, |input, cx| input.set_text(value, cx));
        self.source_menu_open = false;
        self.target_menu_open = false;
        self.save(cx);
    }

    fn language_row(
        &self,
        source: bool,
        label: &'static str,
        value: &'static str,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(SharedString::from(format!(
                "translation-language-{}-{value}",
                if source { "source" } else { "target" }
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
            .cursor_pointer()
            .hover(|style| style.bg(theme.ink(0.05)))
            .on_click(cx.listener(move |page, _, _, cx| {
                page.choose_language(source, value, cx);
            }))
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.0))
                    .text_color(theme.text)
                    .child(label),
            )
            .child(div().w(px(16.0)).when(selected, |row| {
                row.child(
                    crate::icons::icon(crate::icons::CHECK)
                        .size(px(12.0))
                        .text_color(theme.accent),
                )
            }))
            .into_any_element()
    }

    fn language_popup(
        &mut self,
        source: bool,
        selected: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut menu = div()
            .w(px(220.0))
            .rounded(px(10.0))
            .bg(theme.surface_overlay)
            .border_1()
            .border_color(theme.border_strong)
            .on_mouse_down_out(cx.listener(|page, _, window, cx| {
                page.source_menu_open = false;
                page.target_menu_open = false;
                page.source_focus.focus(window, cx);
                cx.notify();
            }))
            .child(
                div()
                    .px(px(12.0))
                    .pt(px(10.0))
                    .pb(px(6.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_muted)
                    .child(if source {
                        "Translate from"
                    } else {
                        "Translate to"
                    }),
            );
        menu = menu.child(
            div()
                .id(if source {
                    "translation-source-language-list"
                } else {
                    "translation-target-language-list"
                })
                .px(px(4.0))
                .pb(px(4.0))
                .children(LANGUAGE_OPTIONS.iter().map(|(label, value)| {
                    self.language_row(source, label, value, *value == selected, theme, cx)
                })),
        );
        popover::anchored_menu_below(
            if source {
                "translation-source-language-popup"
            } else {
                "translation-target-language-popup"
            },
            menu.into_any_element(),
            None,
        )
    }

    fn toggle_translation_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.target.read(cx).can_write(cx) {
            return;
        }
        self.translation_menu_open = !self.translation_menu_open;
        self.session_menu_open = false;
        if self.translation_menu_open {
            self.translation_search.focus_handle(cx).focus(window, cx);
        } else {
            self.translation_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn toggle_session_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.target.read(cx).can_write(cx) {
            return;
        }
        self.session_menu_open = !self.session_menu_open;
        self.translation_menu_open = false;
        if self.session_menu_open {
            self.session_search.focus_handle(cx).focus(window, cx);
        } else {
            self.session_focus.focus(window, cx);
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

    fn model_row(
        &self,
        model: &Model,
        selected: bool,
        single: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = model.id.clone();
        let enabled = self.target.read(cx).can_write(cx);
        div()
            .id(SharedString::from(format!(
                "translation-model-{}-{}",
                if single { "translation" } else { "session" },
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
                if enabled {
                    if single {
                        page.choose_translation_model(id.clone(), cx);
                        page.translation_menu_open = false;
                    } else {
                        page.toggle_session_model(id.clone(), cx);
                    }
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
                    crate::icons::icon(crate::icons::CHECK)
                        .size(px(12.0))
                        .text_color(theme.accent),
                )
            }))
            .into_any_element()
    }

    fn model_popup(&mut self, single: bool, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let search = if single {
            self.translation_search.clone()
        } else {
            self.session_search.clone()
        };
        let query = search.read(cx).text().trim().to_lowercase();
        let mut menu = div()
            .w(px(340.0))
            .rounded(px(10.0))
            .bg(theme.surface_overlay)
            .border_1()
            .border_color(theme.border_strong)
            .on_mouse_down_out(cx.listener(|page, _, window, cx| {
                page.translation_menu_open = false;
                page.session_menu_open = false;
                page.translation_focus.focus(window, cx);
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
                            .id(if single {
                                "translation-model-list"
                            } else {
                                "translation-session-model-list"
                            })
                            .max_h(px(280.0))
                            .overflow_y_scroll()
                            .px(px(4.0))
                            .pb(px(4.0))
                            .children(filtered.into_iter().map(|model| {
                                let selected = if single {
                                    self.translation_model == model.id
                                } else {
                                    self.enabled_models.iter().any(|id| id == &model.id)
                                };
                                self.model_row(model, selected, single, theme, cx)
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
            if single {
                "translation-model-popup"
            } else {
                "translation-session-popup"
            },
            menu.into_any_element(),
            None,
        )
    }
}

/// One settings row: label (+ optional caption) on the left, control on the right.
/// Rows are built through this helper so every row keeps the same rhythm and the
/// control can never escape the column's padding.
fn settings_row(theme: &Theme, label: &'static str, caption: Option<SharedString>) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(16.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(widgets::field_label(theme, label))
                .children(caption.map(|caption| {
                    div()
                        .mt(px(3.0))
                        .text_size(px(11.5))
                        .text_color(theme.text_muted)
                        .child(caption)
                })),
        )
}

/// A fixed-width dropdown trigger. `flex_none` keeps it at its declared width so a
/// long label on the left can never squeeze or push it past the card edge. The
/// caller adds `.on_click(..)` and the anchored popup.
fn dropdown_trigger(
    theme: &Theme,
    id: &'static str,
    focus: &FocusHandle,
    width: f32,
    label: SharedString,
    accent: bool,
    writable: bool,
) -> gpui::Stateful<gpui::Div> {
    let hover_theme = theme.clone();
    widgets::ghost_action(theme)
        .id(id)
        .track_focus(focus)
        .relative()
        .flex_none()
        .w(px(width))
        .border_1()
        .border_color(if accent {
            theme.accent.opacity(0.65)
        } else {
            theme.border
        })
        .hover(move |s| widgets::ghost_hover(&hover_theme, s))
        .when(!writable, |el| el.opacity(0.45))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.text)
                .child(label),
        )
        .child(
            crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                .size(px(12.0))
                .text_color(theme.text_muted),
        )
}

/// One segment of the Replace/Append control. The caller adds `.on_click(..)`.
fn mode_button(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    let hover_theme = theme.clone();
    widgets::ghost_action(theme)
        .id(id)
        .flex_none()
        .rounded(px(6.0))
        .bg(if selected {
            theme.element_active
        } else {
            gpui::transparent_black()
        })
        .text_color(if selected {
            theme.text
        } else {
            theme.text_muted
        })
        .hover(move |s| widgets::ghost_hover(&hover_theme, s))
        .child(label)
}

impl Render for TranslationSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let Loadable::Ready(_) = self.settings.clone() else {
            return widgets::section_card(&theme)
                .p(px(16.0))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(match &self.settings {
                            Loadable::Error(error) => SharedString::from(error.clone()),
                            _ => "Loading translation settings…".into(),
                        }),
                )
                .into_any_element();
        };
        let models = match self.models.clone() {
            Loadable::Ready(models) => models,
            Loadable::Error(error) => {
                return widgets::section_card(&theme)
                    .p(px(16.0))
                    .child(widgets::error_strip(&theme, error))
                    .into_any_element();
            }
            _ => {
                return widgets::section_card(&theme)
                    .p(px(16.0))
                    .child("Loading available Pi models…")
                    .into_any_element();
            }
        };
        let writable = self.target.read(cx).can_write(cx);
        let translation_label = if self.translation_model.is_empty() {
            "Choose a model".to_string()
        } else {
            self.model_label(&self.translation_model)
        };
        let source_value = self.source.read(cx).text().to_string();
        let target_value = self.target_language.read(cx).text().to_string();
        let source_label = LANGUAGE_OPTIONS
            .iter()
            .find(|(_, value)| *value == source_value)
            .map(|(label, _)| *label)
            .unwrap_or(source_value.as_str());
        let target_label = LANGUAGE_OPTIONS
            .iter()
            .find(|(_, value)| *value == target_value)
            .map(|(label, _)| *label)
            .unwrap_or(target_value.as_str());
        let session_label = match self.enabled_models.len() {
            0 => "No models selected".to_string(),
            1 => self.model_label(&self.enabled_models[0]),
            count => format!("{count} models selected"),
        };
        let status = if self.enabled_models.is_empty() {
            "Disabled"
        } else {
            "Enabled"
        };
        let translation_popup = self
            .translation_menu_open
            .then(|| self.model_popup(true, &theme, cx));
        let session_popup = self
            .session_menu_open
            .then(|| self.model_popup(false, &theme, cx));
        let source_popup = self
            .source_menu_open
            .then(|| self.language_popup(true, &source_value, &theme, cx));
        let target_popup = self
            .target_menu_open
            .then(|| self.language_popup(false, &target_value, &theme, cx));
        widgets::section_card(&theme)
            .child(
                div()
                    .px(px(20.0))
                    .py(px(18.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(widgets::row_tile(&theme, crate::icons::GLOBAL))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::row_title(&theme, "Message translation"))
                            .child(
                                div()
                                    .mt(px(3.0))
                                    .text_size(px(12.0))
                                    .text_color(theme.text_muted)
                                    .child(
                                        "Translate selected conversations without changing model context",
                                    ),
                            ),
                    )
                    .child(if self.enabled_models.is_empty() {
                        widgets::badge(&theme, status)
                    } else {
                        widgets::badge_active(&theme, status)
                    }),
            )
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
                            "Language direction",
                            Some("Requests go out in the second language; answers come back in the first".into()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    dropdown_trigger(
                                        &theme,
                                        "translation-source-trigger",
                                        &self.source_focus,
                                        140.0,
                                        SharedString::from(source_label.to_string()),
                                        false,
                                        writable,
                                    )
                                    .on_click(cx.listener(|page, _, window, cx| {
                                        page.toggle_language_menu(true, window, cx)
                                    }))
                                    .children(source_popup),
                                )
                                .child(
                                    crate::icons::icon(crate::icons::ALT_ARROW_RIGHT)
                                        .size(px(14.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(
                                    dropdown_trigger(
                                        &theme,
                                        "translation-target-trigger",
                                        &self.target_focus,
                                        140.0,
                                        SharedString::from(target_label.to_string()),
                                        false,
                                        writable,
                                    )
                                    .on_click(cx.listener(|page, _, window, cx| {
                                        page.toggle_language_menu(false, window, cx)
                                    }))
                                    .children(target_popup),
                                ),
                        ),
                    )
                    .child(
                        settings_row(
                            &theme,
                            "Translate with",
                            Some("A model from this device\u{2019}s Pi catalog".into()),
                        )
                        .child(
                            dropdown_trigger(
                                &theme,
                                "translation-model-trigger",
                                &self.translation_focus,
                                220.0,
                                SharedString::from(translation_label),
                                false,
                                writable,
                            )
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.toggle_translation_menu(window, cx)
                            }))
                            .children(translation_popup),
                        ),
                    )
                    .child(
                        settings_row(
                            &theme,
                            "Enable for session models",
                            Some(
                                format!(
                                    "{} of {} models selected",
                                    self.enabled_models.len(),
                                    models.len()
                                )
                                .into(),
                            ),
                        )
                        .child(
                            dropdown_trigger(
                                &theme,
                                "translation-session-trigger",
                                &self.session_focus,
                                220.0,
                                SharedString::from(session_label),
                                !self.enabled_models.is_empty(),
                                writable,
                            )
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.toggle_session_menu(window, cx)
                            }))
                            .children(session_popup),
                        ),
                    )
                    .child(
                        settings_row(&theme, "Final response", None)
                            .border_t_1()
                            .border_color(theme.border)
                            .pt(px(16.0))
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.0))
                                    .p(px(3.0))
                                    .rounded(px(8.0))
                                    .bg(theme.surface_raised)
                                    .child(
                                        mode_button(
                                            &theme,
                                            "translation-mode-replace",
                                            "Replace",
                                            self.output_mode == TranslationOutputMode::Replace,
                                        )
                                        .on_click(cx.listener(|page, _, _, cx| {
                                            page.set_output_mode(TranslationOutputMode::Replace, cx)
                                        })),
                                    )
                                    .child(
                                        mode_button(
                                            &theme,
                                            "translation-mode-append",
                                            "Append",
                                            self.output_mode == TranslationOutputMode::Append,
                                        )
                                        .on_click(cx.listener(|page, _, _, cx| {
                                            page.set_output_mode(TranslationOutputMode::Append, cx)
                                        })),
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
