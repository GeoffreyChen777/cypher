//! Device-scoped automatic title model. Selection is acknowledged by the host,
//! not stored in client preferences; late replies cannot affect another device.
use cypher_proto::{Model, TitleModelSettings};
use cypher_rpc::methods;
use gpui::{
    AnyElement, Context, Entity, Render, SharedString, Subscription, Task, Window, div, prelude::*,
    px,
};

use super::{device_target::DeviceTarget, widgets};
use crate::{
    composer::{ComposerInput, ComposerInputEvent},
    icons,
    popover::Loadable,
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
        let description = id
            .clone()
            .unwrap_or_else(|| "Choose a small model automatically (current behavior)".into());
        let enabled = !self.busy && self.target.read(cx).can_write(cx);
        div()
            .id(SharedString::from(format!(
                "title-model-{}",
                id.as_deref().unwrap_or("auto")
            )))
            .px(px(14.0))
            .py(px(11.0))
            .rounded(px(10.0))
            .flex()
            .items_center()
            .gap(px(12.0))
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
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text)
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(description)),
                    ),
            )
            .child(div().w(px(16.0)).when(selected, |row| {
                row.child(
                    icons::icon(icons::CHECK)
                        .size(px(14.0))
                        .text_color(theme.text),
                )
            }))
            .into_any_element()
    }
}

impl Render for TitlesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let body: AnyElement = match self.settings.clone() {
            Loadable::Ready(settings) => {
                let mut card = widgets::section_card(&theme)
                    .p(px(8.0))
                    .child(self.model_row(None, settings.model.is_none(), &theme, cx));
                if let Some(id) = &settings.model {
                    card = card.child(
                        div()
                            .px(px(14.0))
                            .py(px(10.0))
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!("Selected: {id}"))),
                    );
                }
                card = card.child(div().px(px(6.0)).py(px(8.0)).child(self.search.clone()));
                match self.models.clone() {
                    Loadable::Ready(models) => {
                        if settings
                            .model
                            .as_ref()
                            .is_some_and(|id| !models.iter().any(|model| &model.id == id))
                        {
                            card = card.child(widgets::error_strip(&theme,
                                "The selected model is no longer in this device's catalog. It remains selected; no other LLM will be used."));
                        }
                        let query = self.search.read(cx).text().trim().to_lowercase();
                        let filtered: Vec<_> = models
                            .iter()
                            .filter(|model| {
                                model.label.to_lowercase().contains(&query)
                                    || model.id.to_lowercase().contains(&query)
                            })
                            .collect();
                        if filtered.is_empty() {
                            card = card.child(div().p(px(14.0)).text_size(px(12.0)).text_color(theme.text_muted)
                                .child(if models.is_empty() { "No Pi models available. Configure Providers on this device, then refresh." } else { "No matching models." }));
                        }
                        card = card.child(
                            div()
                                .id("title-model-list")
                                .max_h(px(380.0))
                                .overflow_y_scroll()
                                .children(filtered.into_iter().map(|model| {
                                    self.model_row(
                                        Some(model),
                                        settings.model.as_ref() == Some(&model.id),
                                        &theme,
                                        cx,
                                    )
                                })),
                        );
                    }
                    Loadable::Error(error) => {
                        card = card.child(widgets::error_strip(&theme, error))
                    }
                    _ => {
                        card = card.child(
                            div()
                                .p(px(14.0))
                                .text_size(px(12.0))
                                .text_color(theme.text_muted)
                                .child("Loading models…"),
                        )
                    }
                }
                card.into_any_element()
            }
            Loadable::Error(error) => widgets::error_strip(&theme, error).into_any_element(),
            _ => div()
                .mt(px(24.0))
                .text_size(px(13.0))
                .text_color(theme.text_muted)
                .child("Loading title settings…")
                .into_any_element(),
        };
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .child(widgets::page_header(&theme, "Automatic titles", None))
            .child(
                widgets::ghost_action(&theme)
                    .id("title-model-refresh")
                    .child(if self.busy { "Saving…" } else { "Refresh" })
                    .on_click(cx.listener(|page, _, _, cx| page.load(cx))),
            );
        let column = widgets::page_column()
            .when(self.embedded, |el| el.max_w(px(768.0)).px_0().pt_0())
            .child(header)
            .child(widgets::page_subtitle(
                &theme,
                "Choose the model used for new session titles.",
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
            .children(self.notice.clone().map(|notice| {
                div()
                    .mt(px(12.0))
                    .text_size(px(12.0))
                    .text_color(theme.success_muted)
                    .child(SharedString::from(notice))
            }))
            .child(body);
        div()
            .id("title-settings-page")
            .size_full()
            .overflow_y_scroll()
            .child(column)
    }
}
