//! Settings → Agents → System prompt.
//!
//! Device-scoped like Context recovery: the text is the selected device's
//! `APPEND_SYSTEM.md`, which every Pi chat on it appends to its system prompt.
//! It saves with its button; a blank save removes the file.

use cypher_proto::AppendSystemPrompt;
use cypher_rpc::methods;
use gpui::{
    Context, Entity, Render, SharedString, Subscription, Task, Window, div, prelude::*, px,
};

use super::device_target::DeviceTarget;
use super::widgets;
use crate::{
    kit::popover::{self, Loadable},
    kit::theme::Theme,
    state::AppState,
    widgets::text_input::{TextInput, TextInputEvent},
};

pub struct SystemPromptControl {
    state: Entity<AppState>,
    target: Entity<DeviceTarget>,
    generation: u64,
    saved: Loadable<AppendSystemPrompt>,
    input: Entity<TextInput>,
    busy: bool,
    error: Option<String>,
    notice: Option<&'static str>,
    task: Option<Task<()>>,
    _target_observer: Subscription,
    _input_observer: Subscription,
}

impl SystemPromptControl {
    pub fn new(
        state: Entity<AppState>,
        target: Entity<DeviceTarget>,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextInput::settings_prompt_field("Instructions every chat on this device follows…", cx)
        });
        let input_observer = cx.subscribe(&input, |page: &mut Self, _, event, cx| {
            if matches!(event, TextInputEvent::Edited) {
                page.notice = None;
                cx.notify();
            }
        });
        let generation = target.read(cx).generation();
        let target_observer = cx.observe(&target, |page: &mut Self, target, cx| {
            let generation = target.read(cx).generation();
            if generation != page.generation {
                page.generation = generation;
                page.saved = Loadable::Idle;
                page.busy = false;
                page.error = None;
                page.notice = None;
                page.task = None;
                page.load(cx);
            }
            cx.notify();
        });
        let mut page = Self {
            state,
            target,
            generation,
            saved: Loadable::Idle,
            input,
            busy: false,
            error: None,
            notice: None,
            task: None,
            _target_observer: target_observer,
            _input_observer: input_observer,
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
        self.saved = Loadable::Loading;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::GET_APPEND_SYSTEM_PROMPT,
                    ticket.params(serde_json::json!({})),
                )
                .await;
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.saved = match result {
                    Ok(value) => match serde_json::from_value::<AppendSystemPrompt>(value) {
                        Ok(saved) => {
                            page.set_input(&saved, cx);
                            Loadable::Ready(saved)
                        }
                        Err(error) => Loadable::Error(error.to_string()),
                    },
                    Err(error) => Loadable::Error(format!("{}: {error}", ticket.label)),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_input(&mut self, saved: &AppendSystemPrompt, cx: &mut Context<Self>) {
        let text = saved.text.clone();
        self.input.update(cx, |input, cx| input.set_text(text, cx));
    }

    /// Whether the field differs from what the device has.
    fn dirty(&self, cx: &Context<Self>) -> bool {
        self.saved
            .ready()
            .is_some_and(|saved| self.input.read(cx).text().trim() != saved.text)
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy || !self.target.read(cx).can_write(cx) || !self.dirty(cx) {
            return;
        }
        let Ok(ticket) = self.target.read(cx).ticket(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let text = self.input.read(cx).text().trim().to_owned();
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SET_APPEND_SYSTEM_PROMPT,
                    ticket.params(serde_json::json!({ "text": text })),
                )
                .await;
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.busy = false;
                match result {
                    Ok(value) => match serde_json::from_value::<AppendSystemPrompt>(value) {
                        Ok(saved) => {
                            page.notice = Some(if saved.text.is_empty() {
                                "Cleared. Each chat drops it from its next message."
                            } else {
                                "Saved. Each chat uses it from its next message."
                            });
                            // Only when the field still holds what was sent:
                            // typing during the save must not be overwritten.
                            if page.input.read(cx).text().trim() == saved.text {
                                page.set_input(&saved, cx);
                            }
                            page.saved = Loadable::Ready(saved);
                        }
                        Err(error) => page.error = Some(error.to_string()),
                    },
                    Err(error) => page.error = Some(format!("{}: {error}", ticket.label)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for SystemPromptControl {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let Loadable::Ready(saved) = self.saved.clone() else {
            return widgets::section_card(&theme)
                .mt(px(24.0))
                .p(px(16.0))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(match &self.saved {
                            Loadable::Error(error) => SharedString::from(error.clone()),
                            _ => "Loading the system prompt…".into(),
                        }),
                )
                .into_any_element();
        };
        let writable = self.target.read(cx).can_write(cx);
        let can_save = writable && !self.busy && self.dirty(cx);
        widgets::section_card(&theme)
            .mt(px(24.0))
            .child(header(!saved.text.is_empty(), &theme))
            .child(
                div()
                    .border_t_1()
                    .border_color(theme.border)
                    .px(px(20.0))
                    .py(px(18.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .line_height(px(16.0))
                            .text_color(theme.text_muted)
                            .child(
                                "Pi appends this to every chat\u{2019}s system prompt on this device, \
                                 on every model. On Claude it follows Claude Code\u{2019}s own prompt. \
                                 Subagents with a prompt of their own use theirs instead.",
                            ),
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
                            .child(self.input.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(saved.path.clone())),
                            )
                            .when_some(self.notice, |el, notice| {
                                el.child(
                                    div()
                                        .flex_none()
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
                                .id("system-prompt-save")
                                .flex_none()
                                .when(!can_save, |el| el.opacity(0.45))
                                .when(can_save, |el| {
                                    el.cursor_pointer()
                                        .on_click(cx.listener(|page, _, _, cx| page.save(cx)))
                                }),
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

/// The card's header: icon, title and caption, and whether a prompt is set.
fn header(set: bool, theme: &Theme) -> gpui::Div {
    div()
        .px(px(20.0))
        .py(px(18.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(widgets::row_tile(theme, crate::kit::icons::DOCUMENT))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(widgets::row_title(theme, "System prompt"))
                .child(
                    div()
                        .mt(px(3.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child("Your own instructions, added to every Pi chat on this device"),
                ),
        )
        .child(if set {
            widgets::badge_active(theme, "Set")
        } else {
            widgets::badge(theme, "Empty")
        })
}
