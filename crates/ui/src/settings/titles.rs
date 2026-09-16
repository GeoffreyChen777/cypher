//! Device-scoped automatic title model. Selection is acknowledged by the host,
//! not stored in client preferences; late replies cannot affect another device.
use cypher_proto::{Model, TitleModelSettings};
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

pub struct TitlesPage {
    state: Entity<AppState>,
    target: Entity<DeviceTarget>,
    generation: u64,
    settings: Loadable<TitleModelSettings>,
    models: Loadable<Vec<Model>>,
    search: Entity<ComposerInput>,
    busy: bool,
    notice: Option<String>,
    error: Option<String>,
    task: Option<Task<()>>,
    menu_open: bool,
    trigger_focus: FocusHandle,
    _target_observer: Subscription,
    _search_observer: Subscription,
    embedded: bool,
}

impl TitlesPage {
    pub fn new(
        state: Entity<AppState>,
        target: Entity<DeviceTarget>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(state, target, false, cx)
    }

    pub fn new_embedded(
        state: Entity<AppState>,
        target: Entity<DeviceTarget>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(state, target, true, cx)
    }

    fn build(
        state: Entity<AppState>,
        target: Entity<DeviceTarget>,
        embedded: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let generation = target.read(cx).generation();
        let observer = cx.observe(&target, |page: &mut Self, target, cx| {
            let generation = target.read(cx).generation();
            if generation != page.generation {
                page.generation = generation;
                page.task = None;
                page.busy = false;
                page.notice = None;
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
            notice: None,
            error: None,
            task: None,
            menu_open: false,
            trigger_focus: cx.focus_handle(),
            _target_observer: observer,
            _search_observer: search_observer,
            embedded,
        };
        page.load(cx);
        page
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.notice = None;
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
            let settings = engine.client().call(
                methods::GET_TITLE_MODEL_SETTINGS, ticket.params(serde_json::json!({})),
            ).await;
            let loaded = this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) { return false; }
                page.settings = match settings {
                    Ok(value) => match serde_json::from_value(value) {
                        Ok(settings) => Loadable::Ready(settings),
                        Err(error) => Loadable::Error(error.to_string()),
                    },
                    Err(error) => Loadable::Error(format!(
                        "Couldn't load title settings on {}. Update the host engine if it doesn't support this setting. {error}", ticket.label,
                    )),
                };
                cx.notify();
                matches!(page.settings, Loadable::Ready(_))
            }).unwrap_or(false);
            if !loaded { return; }
            let models = engine.client().call(
                methods::LIST_MODELS, ticket.params(serde_json::json!({"harness":"pi"})),
            ).await;
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) { return; }
                page.models = match models {
                    Ok(value) => match serde_json::from_value::<Vec<Model>>(value) {
                        Ok(mut models) => {
                            models.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()).then(a.id.cmp(&b.id)));
                            Loadable::Ready(models)
                        }
                        Err(error) => Loadable::Error(error.to_string()),
                    },
                    Err(error) => Loadable::Error(format!("Couldn't load this device's Pi models: {error}")),
                };
                cx.notify();
            }).ok();
        }));
        cx.notify();
    }

    fn save(&mut self, model: Option<String>, cx: &mut Context<Self>) {
        if self.busy
            || !self.target.read(cx).can_write(cx)
            || self.generation != self.target.read(cx).generation()
            || !matches!(self.settings, Loadable::Ready(_))
        {
            return;
        }
        if let Some(id) = &model {
            let Loadable::Ready(models) = &self.models else {
                return;
            };
            if !models.iter().any(|model| &model.id == id) {
                return;
            }
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
        let target = self.target.clone();
        let lease = target.update(cx, |target, cx| target.lock(cx));
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SET_TITLE_MODEL_SETTINGS,
                    ticket.params(serde_json::json!({"model":model})),
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
                        Ok(settings) => {
                            page.settings = Loadable::Ready(settings);
                            page.notice = Some(format!(
                                "Saved on {}. Applies to new title generations.",
                                ticket.label
                            ));
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

    fn selected_label(&self, settings: &TitleModelSettings) -> String {
        match &settings.model {
            None => "Automatic".into(),
            Some(id) => self
                .models
                .ready()
                .and_then(|models| {
                    models
                        .iter()
                        .find(|model| &model.id == id)
                        .map(|model| model.label.clone())
                })
                .unwrap_or_else(|| id.clone()),
        }
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
            .unwrap_or_else(|| "Automatic".into());
        let enabled = !self.busy && self.target.read(cx).can_write(cx);
        div()
            .id(SharedString::from(format!(
                "title-model-{}",
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
                    page.save(id.clone(), cx);
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.0))
                    .text_color(theme.text)
                    .child(SharedString::from(label)),
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
        settings: &TitleModelSettings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut menu = div()
            .w(px(280.0))
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
                settings.model.is_none(),
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
                                "No Pi models available."
                            } else {
                                "No matching models."
                            }),
                    );
                } else {
                    menu = menu.child(
                        div()
                            .id("title-model-list")
                            .max_h(px(240.0))
                            .overflow_y_scroll()
                            .px(px(4.0))
                            .pb(px(4.0))
                            .children(filtered.into_iter().map(|model| {
                                self.model_row(
                                    Some(model),
                                    settings.model.as_ref() == Some(&model.id),
                                    theme,
                                    cx,
                                )
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
        popover::anchored_menu_below("title-model-popup", menu.into_any_element(), None)
    }
}

impl Render for TitlesPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let picker: AnyElement = match self.settings.clone() {
            Loadable::Ready(settings) => {
                let missing = settings.model.as_ref().is_some_and(|id| {
                    self.models
                        .ready()
                        .is_some_and(|models| !models.iter().any(|model| &model.id == id))
                });
                let popup = self
                    .menu_open
                    .then(|| self.model_popup(&settings, &theme, cx));
                let trigger_label = self.selected_label(&settings);
                widgets::section_card(&theme)
                    .child(
                        div()
                            .px(px(20.0))
                            .py(px(14.0))
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(widgets::row_title(&theme, "Automatic titles"))
                                    .child(
                                        div()
                                            .mt(px(3.0))
                                            .text_size(px(12.0))
                                            .text_color(theme.text_muted)
                                            .child(SharedString::from(
                                                "Model used for new session titles",
                                            )),
                                    )
                                    .when(missing, |el| {
                                        el.child(
                                            div()
                                                .mt(px(4.0))
                                                .text_size(px(11.0))
                                                .text_color(theme.warning_muted)
                                                .child(
                                                    "Selected model is no longer in the catalog.",
                                                ),
                                        )
                                    }),
                            )
                            .child(
                                widgets::ghost_action(&theme)
                                    .id("title-model-trigger")
                                    .aria_label("Title model")
                                    .track_focus(&self.trigger_focus)
                                    .relative()
                                    .w(px(220.0))
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
                                            .text_color(theme.text)
                                            .child(SharedString::from(trigger_label)),
                                    )
                                    .child(
                                        icons::icon(icons::ALT_ARROW_DOWN)
                                            .size(px(12.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .children(popup),
                            ),
                    )
                    .into_any_element()
            }
            Loadable::Error(error) => widgets::error_strip(&theme, error).into_any_element(),
            _ => widgets::section_card(&theme)
                .p(px(16.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme.text_muted)
                        .child("Loading title settings…"),
                )
                .into_any_element(),
        };
        let column = widgets::page_column()
            .when(self.embedded, |el| el.max_w(px(768.0)).px_0().pt_0().pb_0())
            .when(!self.embedded, |el| {
                el.child(widgets::page_header(&theme, "Automatic titles", None))
                    .child(widgets::page_subtitle(
                        &theme,
                        "Choose the model used for new session titles.",
                    ))
            })
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
            .children(self.notice.clone().map(|notice| {
                div()
                    .mt(px(12.0))
                    .text_size(px(12.0))
                    .text_color(theme.success_muted)
                    .child(SharedString::from(notice))
            }))
            .child(picker);
        div()
            .id("title-settings-page")
            .when(!self.embedded, |el| el.size_full().overflow_y_scroll())
            .child(column)
    }
}
