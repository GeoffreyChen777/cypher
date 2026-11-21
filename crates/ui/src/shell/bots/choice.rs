//! The model, thinking level and GPT Fast mode of the sessions a workspace
//! starts ([`SessionChoice`]), as the routine form and the workspace's
//! settings choose them. Three rows in a group box: Model (the device's
//! models, searchable), Thinking (the levels that model offers) and Fast
//! mode (Default, On, Off). "Default" leaves the field out, so it follows
//! whatever the session falls back to, which the caller names.

use super::parts::CallError;
use super::schedule::ModelList;
use super::*;
use crate::kit::popover;
use cypher_proto::{ReasoningLevel, SessionChoice};

/// Which form a picker belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChoiceSlot {
    RoutineForm,
    BotDialog,
}

impl ChoiceSlot {
    fn key(self) -> &'static str {
        match self {
            ChoiceSlot::RoutineForm => "bot-form",
            ChoiceSlot::BotDialog => "bot-dialog",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    Model,
    Thinking,
}

/// One form's choice and its menus' state.
pub(super) struct ChoicePicker {
    pub(super) choice: SessionChoice,
    open: Option<Menu>,
    search: Entity<TextInput>,
    models: ModelList,
    /// The device the catalogue was asked of.
    device: Option<String>,
}

impl ChoicePicker {
    pub(super) fn new(choice: SessionChoice, cx: &mut Context<Shell>) -> Self {
        Self {
            choice,
            open: None,
            search: cx.new(|cx| TextInput::settings_field("Search models…", false, cx)),
            models: ModelList::Loading,
            device: None,
        }
    }
}

/// "High", "XHigh" for a menu.
fn level_label(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Minimal => "Minimal",
        ReasoningLevel::Low => "Low",
        ReasoningLevel::Medium => "Medium",
        ReasoningLevel::High => "High",
        ReasoningLevel::XHigh => "Extra high",
        ReasoningLevel::Max | ReasoningLevel::Ultra => "Max",
        ReasoningLevel::Ultracode | ReasoningLevel::Ultrathink => "Max",
    }
}

/// A choice in words for a page: "Opus 5.5 · High thinking · Fast mode";
/// `None` when it chooses nothing. `label` names a model id.
pub(super) fn choice_words(
    choice: &SessionChoice,
    label: impl Fn(&str) -> String,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(model) = &choice.model {
        parts.push(label(model));
    }
    if let Some(level) = choice.thinking {
        parts.push(format!("{} thinking", level_label(level)));
    }
    match choice.fast {
        Some(true) => parts.push("Fast mode".into()),
        Some(false) => parts.push("No Fast mode".into()),
        None => {}
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

impl Shell {
    fn choice_picker(&mut self, slot: ChoiceSlot) -> Option<&mut ChoicePicker> {
        match slot {
            ChoiceSlot::RoutineForm => self.routine_form_choice(),
            ChoiceSlot::BotDialog => self.dialogs.bot.as_mut().map(|d| &mut d.choice),
        }
    }

    /// Ask `device` for its models, once per device.
    fn load_choice_models(&mut self, slot: ChoiceSlot, device: String, cx: &mut Context<Self>) {
        let Some(picker) = self.choice_picker(slot) else {
            return;
        };
        if picker.device.as_deref() == Some(device.as_str()) {
            return;
        }
        picker.device = Some(device.clone());
        picker.models = ModelList::Loading;
        let asked = device.clone();
        self.board_call(
            methods::LIST_MODELS,
            serde_json::json!({ "harness": "pi", "targetDeviceId": device }),
            cx,
            move |this, result, cx| {
                let models = match result {
                    Ok(value) => match serde_json::from_value::<Vec<cypher_proto::Model>>(value) {
                        Ok(models) => {
                            let models = crate::pickers::normalize_model_rows(models);
                            this.remember_model_labels(&models);
                            ModelList::Ready(models)
                        }
                        Err(err) => ModelList::Failed(err.to_string()),
                    },
                    Err(CallError::Unknown) => {
                        ModelList::Failed("This device can't list its models.".into())
                    }
                    Err(CallError::Failed(text)) => ModelList::Failed(text),
                };
                let Some(picker) = this.choice_picker(slot) else {
                    return;
                };
                if picker.device.as_deref() == Some(asked.as_str()) {
                    picker.models = models;
                }
                cx.notify();
            },
        );
    }

    /// The Model, Thinking and Fast mode rows. `device` is where the
    /// sessions run (its models are offered); `fallback` says what
    /// "Default" follows.
    pub(super) fn render_choice(
        &mut self,
        slot: ChoiceSlot,
        device: Option<String>,
        fallback: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use crate::settings::widgets;
        if let Some(device) = device {
            self.load_choice_models(slot, device, cx);
        }
        let Some(picker) = self.choice_picker(slot) else {
            return div();
        };
        let key = slot.key();
        let choice = picker.choice.clone();
        let open = picker.open;
        let search = picker.search.clone();
        let models = picker.models.clone();
        let model = match (&choice.model, &models) {
            (Some(id), ModelList::Ready(models)) => models.iter().find(|m| &m.id == id),
            _ => None,
        };
        // The levels the chosen model offers; every level until it is known.
        let levels: Vec<ReasoningLevel> = match model {
            Some(m) if !m.reasoning_levels.is_empty() => ReasoningLevel::CHOOSABLE
                .into_iter()
                .filter(|l| m.reasoning_levels.contains(l))
                .collect(),
            _ => ReasoningLevel::CHOOSABLE.to_vec(),
        };
        let model_text = match (&choice.model, model) {
            (None, _) => "Default".to_string(),
            (Some(_), Some(m)) => m.label.clone(),
            (Some(id), None) => id.clone(),
        };
        let thinking_text = choice.thinking.map_or("Default", level_label).to_string();

        let toggle = move |menu: Menu| {
            move |this: &mut Shell,
                  _: &gpui::ClickEvent,
                  window: &mut Window,
                  cx: &mut Context<Shell>| {
                if let Some(picker) = this.choice_picker(slot) {
                    picker.open = (picker.open != Some(menu)).then_some(menu);
                    if picker.open == Some(Menu::Model) {
                        let search = picker.search.clone();
                        window.focus(&search.focus_handle(cx), cx);
                    }
                }
                cx.notify();
            }
        };
        let row = |id: String,
                   label: String,
                   selected: bool,
                   pick: Box<dyn Fn(&mut SessionChoice)>,
                   cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(id))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .cursor_pointer()
                .bg(if selected {
                    theme.ink(0.07)
                } else {
                    gpui::transparent_black()
                })
                .hover(|style| style.bg(theme.ink(0.05)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(picker) = this.choice_picker(slot) {
                        pick(&mut picker.choice);
                        picker.open = None;
                    }
                    cx.notify();
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
                    row.child(icon(icons::CHECK).size(px(12.0)).text_color(theme.accent))
                }))
        };
        let close_out = move |this: &mut Shell,
                              _: &gpui::MouseDownEvent,
                              _: &mut Window,
                              cx: &mut Context<Shell>| {
            if let Some(picker) = this.choice_picker(slot) {
                picker.open = None;
            }
            cx.notify();
        };
        let menu_frame = || {
            div()
                .w(px(280.0))
                .rounded(px(10.0))
                .bg(theme.surface_overlay)
                .border_1()
                .border_color(theme.border_strong)
        };

        let model_menu = (open == Some(Menu::Model)).then(|| {
            let mut menu = menu_frame()
                .on_mouse_down_out(cx.listener(close_out))
                .child(div().p(px(8.0)).child(search.clone()))
                .child(div().px(px(4.0)).child(row(
                    format!("{key}-model-default"),
                    "Default".into(),
                    choice.model.is_none(),
                    Box::new(|c: &mut SessionChoice| c.model = None),
                    cx,
                )));
            match &models {
                ModelList::Ready(list) => {
                    let query = search.read(cx).text().trim().to_lowercase();
                    let matching: Vec<_> = list
                        .iter()
                        .filter(|m| {
                            query.is_empty()
                                || m.label.to_lowercase().contains(&query)
                                || m.id.to_lowercase().contains(&query)
                        })
                        .cloned()
                        .collect();
                    if matching.is_empty() {
                        menu = menu.child(
                            div()
                                .p(px(12.0))
                                .text_size(px(12.0))
                                .text_color(theme.text_muted)
                                .child("No matching models."),
                        );
                    } else {
                        let mut list = div()
                            .id(SharedString::from(format!("{key}-model-list")))
                            .max_h(px(240.0))
                            .overflow_y_scroll()
                            .px(px(4.0))
                            .pb(px(4.0));
                        for m in matching {
                            let on = choice.model.as_deref() == Some(m.id.as_str());
                            let id = m.id.clone();
                            list = list.child(row(
                                format!("{key}-model-{}", m.id),
                                m.label,
                                on,
                                Box::new(move |c: &mut SessionChoice| c.model = Some(id.clone())),
                                cx,
                            ));
                        }
                        menu = menu.child(list);
                    }
                }
                ModelList::Failed(error) => {
                    menu = menu.child(widgets::error_strip(theme, error).mt(px(0.0)))
                }
                ModelList::Loading => {
                    menu = menu.child(
                        div()
                            .p(px(12.0))
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child("Loading models…"),
                    )
                }
            }
            popover::anchored_menu_above_end(
                format!("{key}-model-popup"),
                menu.into_any_element(),
                None,
            )
        });
        let thinking_menu = (open == Some(Menu::Thinking)).then(|| {
            let mut list = div().p(px(4.0)).child(row(
                format!("{key}-thinking-default"),
                "Default".into(),
                choice.thinking.is_none(),
                Box::new(|c: &mut SessionChoice| c.thinking = None),
                cx,
            ));
            for level in &levels {
                let level = *level;
                list = list.child(row(
                    format!("{key}-thinking-{}", level.word()),
                    level_label(level).into(),
                    choice.thinking == Some(level),
                    Box::new(move |c: &mut SessionChoice| c.thinking = Some(level)),
                    cx,
                ));
            }
            popover::anchored_menu_above_end(
                format!("{key}-thinking-popup"),
                menu_frame()
                    .w(px(200.0))
                    .on_mouse_down_out(cx.listener(close_out))
                    .child(list)
                    .into_any_element(),
                None,
            )
        });
        let trigger = |id: String,
                       text: String,
                       menu: Menu,
                       popup: Option<AnyElement>,
                       cx: &mut Context<Self>| {
            widgets::ghost_action(theme)
                .id(SharedString::from(id))
                .relative()
                .w(px(180.0))
                .flex_none()
                .border_1()
                .border_color(theme.border)
                .hover(|s| widgets::ghost_hover(theme, s))
                .on_click(cx.listener(toggle(menu)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.text)
                        .child(SharedString::from(text)),
                )
                .child(
                    icon(icons::ALT_ARROW_DOWN)
                        .size(px(12.0))
                        .text_color(theme.text_muted),
                )
                .children(popup)
        };
        let line = |first: bool, title: &str, detail: Option<&str>, control: AnyElement| {
            div()
                .when(!first, |el| {
                    el.border_t_1()
                        .border_color(crate::kit::theme::hairline(0.06))
                })
                .px(px(12.0))
                .py(px(9.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child(SharedString::from(title.to_string())),
                        )
                        .children(detail.map(|detail| {
                            div()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(detail.to_string()))
                        })),
                )
                .child(control)
        };
        let fast = div()
            .w(px(180.0))
            .flex_none()
            .p(px(2.0))
            .rounded(px(8.0))
            .bg(crate::kit::theme::ink(0.05))
            .flex()
            .flex_row()
            .gap(px(2.0))
            .children(
                [(None, "Default"), (Some(true), "On"), (Some(false), "Off")]
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (value, label))| {
                        let on = choice.fast == value;
                        div()
                            .id(SharedString::from(format!("{key}-fast-{ix}")))
                            .flex_1()
                            .py(px(3.0))
                            .rounded(px(6.0))
                            .flex()
                            .justify_center()
                            .text_size(px(12.0))
                            .cursor_pointer()
                            .text_color(if on { theme.text } else { theme.text_muted })
                            .when(on, |el| {
                                el.bg(crate::kit::theme::card_selected_bg())
                                    .shadow(crate::kit::theme::card_selected_shadows())
                                    .font_weight(gpui::FontWeight::MEDIUM)
                            })
                            .when(!on, |el| el.hover(|s| s.text_color(theme.text)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(picker) = this.choice_picker(slot) {
                                    picker.choice.fast = value;
                                }
                                cx.notify();
                            }))
                            .child(SharedString::from(label))
                    }),
            );
        parts::group_box()
            .child(line(
                true,
                "Model",
                Some(fallback),
                trigger(
                    format!("{key}-model-trigger"),
                    model_text,
                    Menu::Model,
                    model_menu,
                    cx,
                )
                .into_any_element(),
            ))
            .child(line(
                false,
                "Thinking",
                None,
                trigger(
                    format!("{key}-thinking-trigger"),
                    thinking_text,
                    Menu::Thinking,
                    thinking_menu,
                    cx,
                )
                .into_any_element(),
            ))
            .child(line(
                false,
                "Fast mode",
                Some("GPT models from 5.4 on."),
                fast.into_any_element(),
            ))
    }
}
