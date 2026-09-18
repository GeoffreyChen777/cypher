//! Web-search fallback for Claude Code sessions — the compact control under
//! the Claude row in Settings → Providers. Device-scoped like the title
//! model: state is read from and acknowledged by the target host, and a
//! stale reply for another device is dropped.
use cypher_proto::{Model, WebSearchFallbackSettings};
use cypher_rpc::methods;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, Render, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};

use super::{device_target::DeviceTarget, widgets};
use crate::{
    composer::{ComposerInput, ComposerInputEvent},
    icons,
    popover::{self, Loadable},
    state::AppState,
    theme::Theme,
};

pub struct WebSearchFallbackControl {
    state: Entity<AppState>,
    target: Entity<DeviceTarget>,
    generation: u64,
    settings: Loadable<WebSearchFallbackSettings>,
    models: Loadable<Vec<Model>>,
    search: Entity<ComposerInput>,
    busy: bool,
    error: Option<String>,
    task: Option<Task<()>>,
    menu_open: bool,
    trigger_focus: FocusHandle,
    _target_observer: Subscription,
    _search_observer: Subscription,
}

impl WebSearchFallbackControl {
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
                page.task = None;
                page.busy = false;
                page.error = None;
                page.search.update(cx, |input, cx| input.set_text("", cx));
                page.menu_open = false;
                page.load(cx);
            }
            cx.notify();
        });
        let search = cx.new(|cx| ComposerInput::settings_field("Search models…", false, cx));
        let search_observer = cx.subscribe(&search, |_: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                cx.notify();
            }
        });
        let mut page = Self {
            state,
            target,
            generation,
            settings: Loadable::Idle,
            models: Loadable::Idle,
            search,
            busy: false,
            error: None,
            task: None,
            menu_open: false,
            trigger_focus: cx.focus_handle(),
            _target_observer: observer,
            _search_observer: search_observer,
        };
        page.load(cx);
        page
    }

    /// Providers re-loads this control whenever its own snapshot reloads
    /// (a provider was added/removed, so the catalog may have changed).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.error = None;
        self.settings = Loadable::Loading;
        self.models = Loadable::Loading;
        let ticket = match self.target.read(cx).ticket(cx) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.settings = Loadable::Error(error);
                self.models = Loadable::Idle;
                cx.notify();
                return;
            }
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.task = Some(cx.spawn(async move |this, cx| {
            let settings = engine
                .client()
                .call(
                    methods::GET_WEB_SEARCH_FALLBACK,
                    ticket.params(serde_json::json!({})),
                )
                .await;
            let loaded = this
                .update(cx, |page, cx| {
                    if !page.target.read(cx).matches(&ticket) {
                        return false;
                    }
                    page.settings = match settings {
                        Ok(value) => match serde_json::from_value(value) {
                            Ok(settings) => Loadable::Ready(settings),
                            Err(error) => Loadable::Error(error.to_string()),
                        },
                        // Older engines lack the method: the control simply
                        // hides (rendered as nothing on Error).
                        Err(error) => Loadable::Error(error.to_string()),
                    };
                    cx.notify();
                    matches!(page.settings, Loadable::Ready(_))
                })
                .unwrap_or(false);
            if !loaded {
                return;
            }
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
                            // The fallback runs through pi-web-search, which
                            // cannot call a claude-bridge API — so a Claude
                            // Code model is never offered as the fallback.
                            models.retain(|model| !model.id.starts_with("claude-bridge/"));
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
                    Err(error) => {
                        Loadable::Error(format!("Couldn't load this device's Pi models: {error}"))
                    }
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// `model: None` selects automatic (the package ranks the catalog).
    fn save(&mut self, enabled: bool, model: Option<String>, cx: &mut Context<Self>) {
        if self.busy
            || !self.target.read(cx).can_write(cx)
            || self.generation != self.target.read(cx).generation()
            || !matches!(self.settings, Loadable::Ready(_))
        {
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
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SET_WEB_SEARCH_FALLBACK,
                    ticket.params(serde_json::json!({"enabled": enabled, "model": model})),
                )
                .await;
            drop(lease);
            target.update(cx, |_, cx| cx.notify());
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.busy = false;
                page.menu_open = false;
                match result {
                    Ok(value) => match serde_json::from_value(value) {
                        Ok(settings) => page.settings = Loadable::Ready(settings),
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

    fn toggle_enabled(&mut self, cx: &mut Context<Self>) {
        let Loadable::Ready(settings) = &self.settings else {
            return;
        };
        let (enabled, model) = (!settings.enabled, settings.model.clone());
        self.save(enabled, model, cx);
    }

    fn pick_model(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        let Loadable::Ready(settings) = &self.settings else {
            return;
        };
        let enabled = settings.enabled;
        self.save(enabled, id, cx);
    }

    fn model_label(&self, id: &str) -> String {
        self.models
            .ready()
            .and_then(|models| models.iter().find(|model| model.id == id))
            .map(|model| model.label.clone())
            .unwrap_or_else(|| id.to_string())
    }

    fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.target.read(cx).can_write(cx) {
            return;
        }
        self.menu_open = !self.menu_open;
        if self.menu_open {
            self.search.focus_handle(cx).focus(window, cx);
        } else {
            self.trigger_focus.focus(window, cx);
        }
        cx.notify();
    }

    /// One picker row. `model: None` is the Automatic row, whose subline
    /// names what the package last resolved to on this device.
    fn model_row(
        &self,
        model: Option<&Model>,
        resolved: Option<&str>,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = model.map(|model| model.id.clone());
        let enabled = !self.busy && self.target.read(cx).can_write(cx);
        let (title, subtitle) = match model {
            Some(model) => (
                model.label.clone(),
                model
                    .id
                    .split_once('/')
                    .map(|(provider, _)| provider.to_string())
                    .unwrap_or_default(),
            ),
            None => (
                "Automatic".to_string(),
                match resolved {
                    Some(resolved) => format!("Best in this catalog · now {resolved}"),
                    None => "Best search model in this device's catalog".to_string(),
                },
            ),
        };
        div()
            .id(SharedString::from(format!(
                "web-search-model-{}",
                id.as_deref().unwrap_or("auto")
            )))
            .px(px(10.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .flex()
            .items_center()
            .gap(px(8.0))
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
                    page.pick_model(id.clone(), cx);
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
            .child(div().w(px(16.0)).when(selected, |row| {
                row.child(
                    icons::icon(icons::CHECK)
                        .size(px(12.0))
                        .text_color(theme.accent),
                )
            }))
            .into_any_element()
    }

    fn model_popup(
        &mut self,
        settings: &WebSearchFallbackSettings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut menu = div()
            .w(px(300.0))
            .rounded(px(10.0))
            .bg(theme.surface_overlay)
            .border_1()
            .border_color(theme.border_strong)
            .on_mouse_down_out(cx.listener(|page, _, window, cx| {
                page.menu_open = false;
                page.trigger_focus.focus(window, cx);
                cx.notify();
            }))
            .child(div().p(px(8.0)).child(self.search.clone()))
            .child(div().px(px(4.0)).child(self.model_row(
                None,
                settings.resolved.as_deref(),
                settings.automatic,
                theme,
                cx,
            )));
        match self.models.clone() {
            Loadable::Ready(models) => {
                let query = self.search.read(cx).text().trim().to_lowercase();
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
                                "No other providers on this device — Automatic has nothing to pick."
                            } else {
                                "No matching models."
                            }),
                    );
                } else {
                    menu = menu.child(
                        div()
                            .id("web-search-model-list")
                            .max_h(px(260.0))
                            .overflow_y_scroll()
                            .px(px(4.0))
                            .pb(px(4.0))
                            .children(filtered.into_iter().map(|model| {
                                let selected = settings.model.as_deref() == Some(model.id.as_str());
                                self.model_row(Some(model), None, selected, theme, cx)
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
        // Mounted inside the Claude settings dialog: draw above it.
        popover::anchored_menu_below_in_dialog(
            "web-search-model-popup",
            menu.into_any_element(),
            None,
        )
    }
}

impl Render for WebSearchFallbackControl {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let section = |theme: &Theme| {
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(widgets::field_label(theme, "Web search"))
        };
        let settings = match self.settings.clone() {
            Loadable::Ready(settings) => settings,
            Loadable::Error(error) => {
                return section(&theme)
                    .child(
                        div()
                            .text_size(px(12.0))
                            .line_height(px(17.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!(
                                "Web search fallback is unavailable on this device: {error}"
                            ))),
                    )
                    .into_any_element();
            }
            _ => {
                return section(&theme)
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child("Loading…"),
                    )
                    .into_any_element();
            }
        };
        if !settings.available {
            return section(&theme)
                .child(
                    div()
                        .text_size(px(12.0))
                        .line_height(px(17.0))
                        .text_color(theme.text_muted)
                        .child(
                            "This device's Pi Runtime doesn't include the web search fallback yet. Update Runtime in Settings → Agents.",
                        ),
                )
                .into_any_element();
        }
        let writable = !self.busy && self.target.read(cx).can_write(cx);
        // A pinned model must exist HERE; automatic ranks the catalog itself
        // and is always usable, so it is the default and needs no warning.
        let pinned_missing = settings.model.as_deref().is_some_and(|pinned| {
            self.models
                .ready()
                .is_some_and(|models| !models.iter().any(|model| model.id == pinned))
        });
        // Turning it ON with a model this device can't run would break search;
        // turning it OFF is always allowed.
        let can_toggle = writable && (settings.enabled || !pinned_missing);
        let popup = self
            .menu_open
            .then(|| self.model_popup(&settings, &theme, cx));
        let trigger_label = match settings.model.as_deref() {
            None => "Automatic".to_string(),
            Some(pinned) => self.model_label(pinned),
        };
        let hint: Option<String> = if pinned_missing {
            Some("This model isn't in the device's catalog — pick another, or Automatic.".into())
        } else if settings.automatic {
            match settings.resolved.as_deref() {
                Some(resolved) => Some(format!("Currently searching with {resolved}.")),
                None => Some(
                    "The best search model in this device's catalog is picked for each search."
                        .into(),
                ),
            }
        } else {
            None
        };
        section(&theme)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(theme.text)
                                    .child("Route web search through a fallback model"),
                            )
                            .child(
                                div()
                                    .mt(px(2.0))
                                    .text_size(px(12.0))
                                    .line_height(px(17.0))
                                    .text_color(theme.text_muted)
                                    .child(
                                        "Cypher can't drive Claude Code's own web search yet. While a Claude Code model is selected, web search runs through the model below instead.",
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("web-search-fallback-toggle")
                            .flex_none()
                            .when(can_toggle, |el| el.cursor_pointer())
                            .opacity(if can_toggle { 1.0 } else { 0.5 })
                            .on_click(cx.listener(move |page, _, _, cx| {
                                if can_toggle {
                                    page.toggle_enabled(cx);
                                }
                            }))
                            .child(widgets::toggle_switch(&theme, settings.enabled)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(widgets::field_label(&theme, "Search model"))
                    .child(
                        widgets::ghost_action(&theme)
                            .id("web-search-model-trigger")
                            .aria_label("Web search fallback model")
                            .track_focus(&self.trigger_focus)
                            .relative()
                            .w_full()
                            .h(px(40.0))
                            .px(px(12.0))
                            .border_1()
                            .border_color(theme.border)
                            .hover(|s| widgets::ghost_hover(&theme, s))
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.toggle_menu(window, cx);
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.0))
                                    .text_color(if pinned_missing {
                                        theme.text_muted
                                    } else {
                                        theme.text
                                    })
                                    .child(SharedString::from(trigger_label)),
                            )
                            .child(
                                icons::icon(icons::ALT_ARROW_DOWN)
                                    .size(px(12.0))
                                    .text_color(theme.text_muted),
                            )
                            .children(popup),
                    )
                    .when_some(hint, |el, hint| {
                        el.child(
                            div()
                                .text_size(px(11.0))
                                .line_height(px(15.0))
                                .text_color(if pinned_missing {
                                    theme.warning_muted
                                } else {
                                    theme.text_muted
                                })
                                .child(SharedString::from(hint)),
                        )
                    }),
            )
            .children(
                self.error
                    .clone()
                    .map(|error| widgets::error_strip(&theme, error)),
            )
            .into_any_element()
    }
}
