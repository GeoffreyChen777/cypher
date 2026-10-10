//! Render pieces and the composer's `Render` impl.

use super::*;

impl Composer {
    /// Whether the current page has an answer to advance with. Empty labels
    /// are the cancel signal on the wire, so Enter and the primary button
    /// both wait for a pick or typed text (an optional comment may be blank).
    pub(super) fn wizard_ready(&self, cx: &App) -> bool {
        self.wizard.as_ref().is_some_and(|wizard| {
            let optional = wizard
                .current()
                .is_some_and(|q| optional_comment_copy(&q.header, &q.question).is_some());
            optional || wizard.page_has_pick() || !self.input.read(cx).is_empty()
        })
    }

    /// Question card, top to bottom by importance: who is asking (and the
    /// step), the question itself, its context, the answer (options and/or a
    /// text field), then a footer of keyboard hints and the page's actions.
    fn render_wizard(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(wizard) = self.wizard.clone() else {
            return gpui::Empty.into_any_element();
        };
        let Some(question) = wizard.current().cloned() else {
            return gpui::Empty.into_any_element();
        };
        let view = wizard.view();
        let page = wizard.page;
        let pages = wizard.questions.len();
        let last = page + 1 >= pages;
        let typed_empty = self.input.read(cx).is_empty();
        let pick_only = wizard_pick_only(&question);
        let optional_comment = optional_comment_copy(&question.header, &question.question);
        let can_advance = self.wizard_ready(cx);
        let prompt = if question.question.is_empty() {
            question.header.clone()
        } else {
            question.question.clone()
        };
        let chrome_title = wizard
            .slash
            .clone()
            .unwrap_or_else(|| SharedString::from("Agent question"));
        let multi = question.multi_select;
        // Card text selects + copies like transcript text. Keys carry the
        // request and page, so a selection never washes another page's copy.
        let scope = self.wizard_selection;
        let key_prefix = format!("{}:{page}", wizard.request_id);
        let frame = WizardFrame {
            theme,
            wizard,
            question,
            view,
            page,
            pages,
            last,
            typed_empty,
            pick_only,
            optional_comment,
            can_advance,
            prompt,
            chrome_title,
            multi,
            scope,
            key_prefix,
        };

        let header = self.wizard_header(&frame, cx);
        let body = self.wizard_body(&frame, cx);
        let footer = self.wizard_footer(&frame, cx);
        let theme = &frame.theme;

        div()
            .id("question-panel")
            .track_focus(&self.wizard_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_wizard_key(event, window, cx)
            }))
            .occlude()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .w_full()
            .min_w_0()
            .max_w(px(560.0))
            .mx_auto()
            .overflow_hidden()
            .rounded(px(14.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface_dialog)
            .shadow_lg()
            .flex()
            .flex_col()
            .child(crate::markdown::render::selection_frame_reset(scope))
            .child(header)
            .child(body)
            .child(footer)
            .into_any_element()
    }

    /// One answer option: its number-key (or checkbox) marker, label and
    /// description.
    fn wizard_option(
        &self,
        frame: &WizardFrame,
        ix: usize,
        label: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &frame.theme;
        let multi = frame.multi;
        let scope = frame.scope;
        let picked = frame.wizard.is_picked(ix) && frame.typed_empty;
        let custom = frame.view.custom_ix == Some(ix);
        let description = frame.view.descriptions.get(ix).cloned().flatten();
        let hover_key = format!("wizard-option-{ix}");
        let number = (ix < 9).then(|| SharedString::from(format!("{}", ix + 1)));

        let marker = wizard_option_marker(
            picked,
            custom,
            multi,
            description.is_some(),
            number.clone(),
            theme,
        );

        let text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .font_weight(if custom {
                        gpui::FontWeight::NORMAL
                    } else {
                        gpui::FontWeight::MEDIUM
                    })
                    .text_color(if custom { theme.text_muted } else { theme.text })
                    .map(|el| {
                        if custom {
                            el.child("Write a different answer…")
                        } else {
                            el.child(crate::markdown::render::selectable_plain_text(
                                scope,
                                frame.text_key(&format!("option{ix}")),
                                SharedString::from(label.to_owned()),
                                theme,
                            ))
                        }
                    }),
            )
            .children(description.map(|description| {
                div()
                    .text_size(px(12.0))
                    .line_height(px(17.0))
                    .text_color(theme.text_muted)
                    .child(crate::markdown::render::selectable_plain_text(
                        scope,
                        frame.text_key(&format!("option{ix}-description")),
                        SharedString::from(description),
                        theme,
                    ))
            }));

        div()
            .id(("wizard-option", ix))
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(10.0))
            .px(px(10.0))
            .py(px(9.0))
            .rounded(px(10.0))
            .border_1()
            .when(custom && !picked, |el| el.border_dashed())
            .border_color(if picked {
                theme.border_strong
            } else {
                theme.border
            })
            .bg(if picked {
                crate::kit::theme::ink(0.07)
            } else {
                motion::hover_blend(
                    &hover_key,
                    crate::kit::theme::ink(if custom { 0.0 } else { 0.02 }),
                    crate::kit::theme::ink(0.05),
                )
            })
            .on_hover(motion::hover_listener(hover_key))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                // A drag or double-click over the copy is a text
                // selection, not a pick.
                if let gpui::ClickEvent::Mouse(click) = event {
                    let moved = click.up.position - click.down.position;
                    if click.down.click_count > 1
                        || f32::from(moved.x).abs() > 3.0
                        || f32::from(moved.y).abs() > 3.0
                    {
                        return;
                    }
                }
                this.wizard_select(ix, cx)
            }))
            .child(marker)
            .child(text)
            // Multi-select and the custom row keep their number key on the
            // trailing edge, where it doesn't read as a checkbox state.
            .when(multi || custom, |el| {
                el.children(number.map(|number| {
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .line_height(px(18.0))
                        .text_color(theme.text_faint)
                        .child(number)
                }))
            })
    }

    /// Top row: who is asking, and where in the set this page is.
    fn wizard_header(&self, frame: &WizardFrame, cx: &mut Context<Self>) -> gpui::Div {
        let theme = &frame.theme;
        let pages = frame.pages;
        let page = frame.page;
        div()
            .flex_none()
            .h(px(40.0))
            .pl(px(16.0))
            .pr(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(
                crate::kit::icons::icon(if frame.wizard.slash.is_some() {
                    crate::kit::icons::TUNING
                } else {
                    crate::kit::icons::QUESTION_CIRCLE
                })
                .size(px(14.0))
                .flex_none()
                .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_muted)
                    .child(frame.chrome_title.clone()),
            )
            .when(pages > 1, |el| {
                el.child(
                    div()
                        .flex_none()
                        .ml(px(4.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(3.0))
                        .children((0..pages).map(|ix| {
                            div()
                                .w(px(12.0))
                                .h(px(3.0))
                                .rounded_full()
                                .bg(if ix <= page {
                                    theme.text_muted
                                } else {
                                    crate::kit::theme::ink(0.12)
                                })
                        })),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(format!("{} of {}", page + 1, pages))),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .id("wizard-cancel")
                    .flex_none()
                    .size(px(26.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(crate::kit::theme::ink(0.08)))
                    .on_click(cx.listener(|this, _, _, cx| this.wizard_cancel(cx)))
                    .child(
                        crate::kit::icons::icon(crate::kit::icons::CLOSE)
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    ),
            )
    }

    /// Body: the question, its context, then the answer.
    fn wizard_body(
        &self,
        frame: &WizardFrame,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &frame.theme;
        let scope = frame.scope;
        let question = &frame.question;
        let optional_comment = &frame.optional_comment;
        let mut body = div()
            .id("wizard-scroll")
            .min_w_0()
            .max_h(px(WIZARD_CONTENT_MAX_HEIGHT))
            .overflow_y_scroll()
            .px(px(16.0))
            .pt(px(2.0))
            .pb(px(16.0))
            .flex()
            .flex_col();
        let (question_text, context) = match optional_comment.as_ref() {
            Some(copy) => (copy.question.clone(), copy.context.clone()),
            None => split_question_context(&frame.prompt),
        };
        body = body.child(
            div()
                .text_size(px(15.0))
                .line_height(px(21.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .cursor_text()
                .child(crate::markdown::render::selectable_plain_text(
                    scope,
                    frame.text_key("question"),
                    SharedString::from(question_text),
                    theme,
                )),
        );
        if let Some(context) = context {
            body = body.child(wizard_context_block(
                &context,
                scope,
                frame.text_key("context"),
                theme,
            ));
        }
        if let Some(copy) = optional_comment.as_ref() {
            body = body.child(wizard_picked_answers(frame, copy));
        }
        if !question.options.is_empty() {
            let options = question
                .options
                .iter()
                .enumerate()
                .map(|(ix, label)| self.wizard_option(frame, ix, label, cx));
            body = body.child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_col()
                    // Single-select needs no label: the numbered rows say it.
                    .when(frame.multi, |el| {
                        el.child(wizard_section_label("Pick any that apply", theme))
                    })
                    .child(div().flex().flex_col().gap(px(6.0)).children(options)),
            );
        }
        if !frame.pick_only {
            let (label, hint) = if optional_comment.is_some() {
                (
                    "Add a comment (optional)",
                    Some("Leave it blank to send your pick as is."),
                )
            } else if question.options.is_empty() {
                ("Your answer", None)
            } else {
                (
                    "Or write your own",
                    Some("Typed text replaces the ticked options."),
                )
            };
            body = body.child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_col()
                    .child(wizard_section_label(label, theme))
                    .child(
                        div()
                            .w_full()
                            .min_h(px(56.0))
                            .rounded(px(10.0))
                            .border_1()
                            .border_color(theme.border_strong)
                            .bg(theme.surface_card)
                            .px(px(12.0))
                            .py(px(8.0))
                            .child(self.input.clone()),
                    )
                    .children(hint.map(|hint| {
                        div()
                            .mt(px(6.0))
                            .text_size(px(11.5))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(hint))
                    })),
            );
        }
        body
    }

    /// Footer: keyboard hints on the left, actions on the right.
    fn wizard_footer(&self, frame: &WizardFrame, cx: &mut Context<Self>) -> gpui::Div {
        let theme = &frame.theme;
        let page = frame.page;
        let pick_only = frame.pick_only;
        let last = frame.last;
        let can_advance = frame.can_advance;
        let option_count = frame.question.options.len().min(9);
        let esc_action = if page > 0 {
            Some("back")
        } else if pick_only {
            Some("dismiss")
        } else {
            None
        };
        let hints = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .when(option_count > 0, |el| {
                let keys = if option_count == 1 {
                    "1".to_owned()
                } else {
                    format!("1–{option_count}")
                };
                el.child(wizard_key_hint(
                    &keys,
                    if frame.multi { "toggle" } else { "choose" },
                    theme,
                ))
            })
            .when(!pick_only, |el| {
                el.child(wizard_key_hint(
                    "↵",
                    if last { "submit" } else { "next" },
                    theme,
                ))
            })
            .children(esc_action.map(|action| wizard_key_hint("esc", action, theme)));
        let back = if frame.optional_comment.is_some() {
            Some(
                crate::kit::popover::btn_ghost(theme, "Skip", "wizard-comment-skip")
                    .id("wizard-comment-skip")
                    .on_click(cx.listener(|this, _, _, cx| this.wizard_skip_comment(cx)))
                    .into_any_element(),
            )
        } else if page > 0 {
            Some(
                crate::kit::popover::btn_ghost(theme, "Back", "wizard-back")
                    .id("wizard-back")
                    .on_click(cx.listener(|this, _, _, cx| this.wizard_back(cx)))
                    .into_any_element(),
            )
        } else {
            None
        };
        // A single-select page answers on the click itself; a primary button
        // appears only where the answer needs confirming.
        let show_primary = !pick_only || frame.pages > 1;
        div()
            .flex_none()
            .h(px(48.0))
            .pl(px(16.0))
            .pr(px(10.0))
            .border_t_1()
            .border_color(theme.border)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(hints)
            .children(back)
            .when(show_primary, |el| {
                el.child(
                    crate::kit::popover::btn_primary(theme, if last { "Submit" } else { "Next" })
                        .id("wizard-submit")
                        .px(px(14.0))
                        .when(!can_advance, |el| el.opacity(0.4).cursor_default())
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.wizard_ready(cx) {
                                this.wizard_advance(cx);
                            }
                        })),
                )
            })
    }

    /// What the context gauge shows: the selected session's latest
    /// context-window reading, and whether a click compacts it (the harness
    /// has `/compact` and no turn is running). `None` — no ring at all —
    /// until the host engine has a reading (new chats, hosts on an older
    /// version), and in a Side Chat. A remote host's reading can trail a
    /// running turn by up to the session row's 20s freshness write; it
    /// catches up when the turn settles.
    fn context_ring_reading(&self, cx: &App) -> Option<context_ring::RingReading> {
        let pickers = self.pickers.read(cx);
        if pickers.is_side_chat() {
            return None;
        }
        let state = self.state.read(cx);
        let chat_id = state.selected_chat.as_deref()?;
        let usage = state.session_for(chat_id)?.context_usage?;
        let busy = matches!(
            state.indicator_for(chat_id, chrono::Utc::now()),
            crate::state::Indicator::Working | crate::state::Indicator::AwaitingInput
        );
        let compactable = matches!(pickers.effective_harness(cx), Some(HarnessId::Pi));
        Some(context_ring::RingReading {
            usage,
            compactable,
            busy,
        })
    }

    /// The context gauge: the reading along the pill's rounded right end,
    /// round the send button and held off the border (the pill clips
    /// children to its inside), on the single-line pill and the multi-line
    /// one alike. `height` is the pill's inside; `send_bottom_inset` how far
    /// up from it the send button ends. Its tooltip, hover and click ride
    /// strips over the arc that stay outside the send button, so the button
    /// keeps its own: one up the right edge, plus, where the gauge sits in
    /// the bottom corner, one along the bottom under the button.
    fn render_edge_ring(
        &self,
        reading: crate::composer::context_ring::RingReading,
        height: f32,
        send_bottom_inset: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let fraction = reading.usage.fraction();
        let enabled = reading.enabled();
        let summary = reading.summary();
        let hint = reading.hint();
        // Hover-fade keys are global: one per composer.
        let fade = format!("composer-edge-ring-{}", cx.entity_id());
        let rest = theme.text_muted.opacity(0.25);
        let track = if enabled {
            motion::hover_blend(&fade, rest, theme.text_muted.opacity(0.5))
        } else {
            rest
        };
        // Inside the 1px border the corner's radius is one less (CSS's inner
        // radius), which is the curve the stroke follows.
        let corner_radius = PILL_RADIUS - PILL_BORDER_V / 2.0;
        let arc = crate::composer::context_ring::edge_arc(
            fraction,
            corner_radius,
            track,
            crate::composer::context_ring::fill_color(fraction, theme),
        )
        .absolute()
        .inset_0();
        let semicircle = crate::composer::context_ring::edge_is_semicircle(height, corner_radius);
        let zone = |id: &'static str| {
            let summary = summary.clone();
            let hint = hint.clone();
            div()
                .id(id)
                .absolute()
                .bottom_0()
                .tooltip(move |_, cx| {
                    cx.new(|_| crate::composer::context_ring::ContextRingTooltip {
                        summary: summary.clone(),
                        hint: hint.clone(),
                    })
                    .into()
                })
                .when(enabled, |el| {
                    el.cursor_pointer()
                        .on_hover(motion::hover_listener(fade.clone()))
                        .on_click(cx.listener(|this, _, _, cx| this.compact_context(cx)))
                })
        };
        // Up the right edge: the whole end on the single line, the bottom
        // corner on a taller pill.
        let side = zone("composer-edge-ring")
            .right_0()
            .w(px(EDGE_RING_HIT_WIDTH))
            .when(semicircle, |el| el.top_0())
            .when(!semicircle, |el| el.h(px(PILL_RADIUS)));
        // Under the send button, where the corner's arc turns toward it.
        let under = (!semicircle).then(|| {
            zone("composer-edge-ring-under")
                .right(px(EDGE_RING_HIT_WIDTH))
                .w(px(PILL_RADIUS - EDGE_RING_HIT_WIDTH))
                .h(px(send_bottom_inset))
        });
        motion::fade_quick(
            "composer-edge-ring-in",
            div()
                .absolute()
                .right_0()
                .top_0()
                .bottom_0()
                .w(px(PILL_RADIUS))
                .child(arc)
                .child(side)
                .children(under),
        )
        .into_any_element()
    }

    fn render_send_button(
        &mut self,
        mode: SendButtonMode,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = Theme::of(cx);
        // Zeron composer-actions.tsx: a size-7 filled circle — up-arrow to
        // send/steer, a dark rounded square on the same light circle to stop.
        match mode {
            SendButtonMode::Stop => div()
                .id("composer-stop")
                .size(px(SEND_BUTTON_SIZE))
                .flex_none()
                .rounded_full()
                .bg(theme.text)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .on_click(cx.listener(|this, _, _, cx| this.interrupt(cx)))
                .child(div().size(px(11.0)).rounded(px(3.0)).bg(theme.bg))
                .into_any_element(),
            SendButtonMode::Send | SendButtonMode::Steer => {
                // Dimmed and inert while no project is picked (`send_blocked`
                // also gates `on_submit`, so Enter is a no-op too).
                let blocked = self.send_blocked(cx);
                div()
                    .id("composer-send")
                    .size(px(SEND_BUTTON_SIZE))
                    .flex_none()
                    .rounded_full()
                    .bg(theme.text)
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(blocked, |el| el.opacity(0.35))
                    .when(!blocked, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.opacity(0.85))
                            .on_click(cx.listener(|this, _, _, cx| this.on_submit(cx)))
                    })
                    .child(
                        crate::kit::icons::icon(crate::kit::icons::ARROW_UP)
                            .size(px(14.0))
                            .text_color(theme.bg),
                    )
                    .into_any_element()
            }
        }
    }
}

/// The question card's per-frame snapshot, shared by its sections.
struct WizardFrame {
    theme: Theme,
    wizard: Wizard,
    question: UserInputQuestion,
    view: PageView,
    page: usize,
    pages: usize,
    last: bool,
    typed_empty: bool,
    pick_only: bool,
    optional_comment: Option<OptionalCommentCopy>,
    can_advance: bool,
    prompt: String,
    chrome_title: SharedString,
    multi: bool,
    scope: crate::markdown::selection::SelectionScope,
    key_prefix: String,
}

impl WizardFrame {
    /// A selectable text's key: the request and page, then `part`.
    fn text_key(&self, part: &str) -> Arc<str> {
        format!("{}:{part}", self.key_prefix).into()
    }
}

/// An option's leading marker: its number key (a checkbox on multi-select
/// pages); a solid check once picked.
fn wizard_option_marker(
    picked: bool,
    custom: bool,
    multi: bool,
    described: bool,
    number: Option<SharedString>,
    theme: &Theme,
) -> gpui::Div {
    div()
        .flex_none()
        .size(px(20.0))
        .mt(px(if described { 0.0 } else { -1.0 }))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(if multi { 5.0 } else { 6.0 }))
        .when(picked, |el| el.bg(theme.text))
        .when(!picked && multi, |el| {
            el.border_1().border_color(theme.border_strong)
        })
        .when(!picked && !multi, |el| el.bg(crate::kit::theme::ink(0.06)))
        .map(|el| {
            if picked {
                el.child(
                    crate::kit::icons::icon(crate::kit::icons::CHECK)
                        .size(px(12.0))
                        .text_color(theme.on_solid),
                )
            } else if custom {
                el.child(
                    crate::kit::icons::icon(crate::kit::icons::PEN)
                        .size(px(11.0))
                        .text_color(theme.text_muted),
                )
            } else if multi {
                el
            } else {
                el.text_size(px(11.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_muted)
                    .children(number)
            }
        })
}

/// What the user already picked, shown as the answer it is.
fn wizard_picked_answers(frame: &WizardFrame, copy: &OptionalCommentCopy) -> gpui::Div {
    let theme = &frame.theme;
    let scope = frame.scope;
    div()
        .mt(px(16.0))
        .flex()
        .flex_col()
        .child(wizard_section_label(
            if copy.selected_label == "Selected options" {
                "Your picks"
            } else {
                "Your pick"
            },
            theme,
        ))
        .child(div().flex().flex_col().gap(px(6.0)).children(
            copy.selected.lines().enumerate().map(|(ix, line)| {
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(10.0))
                    .px(px(10.0))
                    .py(px(9.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(crate::kit::theme::ink(0.07))
                    .child(
                        div()
                            .flex_none()
                            .size(px(20.0))
                            .mt(px(-1.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .bg(theme.text)
                            .child(
                                crate::kit::icons::icon(crate::kit::icons::CHECK)
                                    .size(px(12.0))
                                    .text_color(theme.on_solid),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.0))
                            .line_height(px(18.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .cursor_text()
                            .child(crate::markdown::render::selectable_plain_text(
                                scope,
                                frame.text_key(&format!("picked{ix}")),
                                SharedString::from(line.to_owned()),
                                theme,
                            )),
                    )
            }),
        ))
}

/// Focus lands on the prompt input (window-level focus fallbacks — e.g. after
/// the focused terminal panel is hidden — route here).
impl Focusable for Composer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.style_relayout_passes > 0 {
            self.style_relayout_passes -= 1;
            let entity = cx.entity_id();
            window.on_next_frame(move |_, cx| cx.notify(entity));
        }
        let theme = crate::appearance::chat_style::theme(cx);
        let wide = crate::appearance::chat_style::settings(cx).wide;
        let compact_height = compact_height_for_line(
            crate::appearance::chat_style::settings(cx).input_line_height(),
        );
        let wizard_active = self.wizard.is_some();
        self.close_unfocused_popups(wizard_active, window, cx);
        let mode = self.button_mode(cx);
        let layout = self.update_layout_mode(cx);
        let container = self.render_notices(mode, wide, &theme, cx);

        if wizard_active {
            // A card that follows an answer swaps straight in: the composer
            // came back a moment ago, and fading this one over it is what the
            // eye reads as a flicker (see [`WIZARD_HANDOFF_QUIET_MS`]).
            let quiet = self
                .wizard
                .as_ref()
                .is_some_and(|wizard| wizard.quiet_entry);
            let wizard = div().w_full().min_w_0().child(self.render_wizard(cx));
            return container.child(if quiet {
                wizard.into_any_element()
            } else {
                motion::fade_quick("composer-wizard", wizard).into_any_element()
            });
        }

        // New chats always use the expanded layout: the repo/branch pickers
        // need the full-width actions row (zeron composer-actions.tsx
        // `mustExpand = isNew || …`).
        let expanded = layout.expanded || layout.new_chat;
        let frame = self.advance_pill_morph(expanded, &layout, compact_height, window);
        let body = self.render_pill(&frame, mode, &theme, cx);
        let container = container.child(self.render_pill_stack(body, &theme));
        // Branch/worktree toolbar under the pill (t3code BranchToolbar): the
        // checkout-kind selector + ref picker for new sessions, read-only
        // labels once the session exists. Git spaces only.
        let footer = self
            .pickers
            .update(cx, |pickers, cx| pickers.render_footer(cx));
        let container = match footer {
            Some(footer) => container.child(footer),
            None => container,
        };
        self.with_preview(container, window, cx)
    }
}

/// This frame's input measurements and layout mode, after the flip state
/// machine has run.
struct LayoutFrame {
    last_width: f32,
    content_height: f32,
    new_chat: bool,
    now_ms: f32,
    expanded: bool,
}

/// The pill's geometry this frame, mid-morph or settled.
struct PillFrame {
    expanded: bool,
    compact_height: f32,
    base_height: f32,
    pill_height: f32,
    morph_t: f32,
    morphing: bool,
}

/// The pieces both pill layouts arrange.
struct PillParts {
    pill: gpui::Div,
    strip: Option<gpui::Div>,
    send_button: AnyElement,
    edge_ring: Option<AnyElement>,
    cluster_dy: f32,
}

impl Composer {
    /// Completion popups never outlive the input's focus, nor show under the
    /// question card.
    fn close_unfocused_popups(
        &mut self,
        wizard_active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mention.token.is_some()
            && (wizard_active || !self.input.focus_handle(cx).is_focused(window))
        {
            self.reset_mention(None, cx);
        }
        if self.slash.token.is_some()
            && (wizard_active || !self.input.focus_handle(cx).is_focused(window))
        {
            self.reset_slash(None, cx);
        }
        if self.issue.token.is_some()
            && (wizard_active || !self.input.focus_handle(cx).is_focused(window))
        {
            self.reset_issue(None, cx);
        }
    }

    /// The compact/expanded state machine: read the input's measurements,
    /// track resizes, flip the mode when the text no longer fits, and step
    /// the height morph. At most one flip per layout pass.
    fn update_layout_mode(&mut self, cx: &mut Context<Self>) -> LayoutFrame {
        let (text_width, has_newline, content_height, last_width, epoch) = {
            let input = self.input.read(cx);
            (
                input.measured_text_width(),
                input.has_newline(),
                input.measured_content_height(),
                input.last_width,
                input.layout_epoch,
            )
        };
        // Narrow tile: the Traits chip steps aside (last frame's pill width —
        // independent of the chips, so the gate can't feed back on itself).
        let pill_width = self.pill_width.get();
        let narrow = pill_width > 0.0 && pill_width < NARROW_PILL_WIDTH;
        self.pickers
            .update(cx, |pickers, cx| pickers.set_narrow(narrow, cx));
        let now = Instant::now();
        // Only measurements taken *after* the last flip may drive the next one
        // (at most one flip per layout pass — a flip invalidates the widths).
        let measured_since_flip = epoch > self.flip_epoch && last_width > 0.0;
        if measured_since_flip {
            // A same-mode width change is an interactive window/pane resize:
            // freeze the mode until sizes settle for RESIZE_SETTLE_MS.
            if self.last_seen_width > 0.0 && (last_width - self.last_seen_width).abs() > 0.5 {
                self.width_changed_at = Some(now);
            }
            self.last_seen_width = last_width;
            if self.expanded_mode {
                if self.expanded_anchor <= 0.0 {
                    self.expanded_anchor = last_width;
                }
            } else {
                // The compact pill's content box is the layout-stable capacity
                // both thresholds measure against.
                self.compact_capacity = last_width - 8.0;
            }
        }
        let resizing = self
            .width_changed_at
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(RESIZE_SETTLE_MS));
        if resizing && self.settle_task.is_none() {
            // Re-evaluate once the settle window has passed.
            self.settle_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(RESIZE_SETTLE_MS + 20))
                    .await;
                this.update(cx, |composer, cx| {
                    composer.settle_task = None;
                    cx.notify();
                })
                .ok();
            }));
        }
        // Layout-stable compact capacity: measured directly while compact;
        // while expanded, the learned value shifted by any container resize
        // (the expanded input width tracks the container 1:1).
        let capacity = if !self.expanded_mode {
            if last_width > 0.0 {
                last_width - 8.0
            } else {
                f32::MAX // before first measure default to compact
            }
        } else if self.compact_capacity > 0.0 {
            if self.expanded_anchor > 0.0 && last_width > 0.0 {
                self.compact_capacity + (last_width - self.expanded_anchor)
            } else {
                self.compact_capacity
            }
        } else {
            f32::MAX
        };
        let next = composer_flip(
            self.expanded_mode,
            text_width,
            capacity,
            has_newline,
            resizing,
        );
        let committed_flip = next != self.expanded_mode && measured_since_flip;
        if committed_flip {
            self.expanded_mode = next;
            self.flip_epoch = epoch;
            self.expanded_anchor = 0.0;
            // The mode change moves the input width; don't read that jump as
            // an interactive resize.
            self.last_seen_width = 0.0;
        }
        // New chats render expanded regardless of `expanded_mode` (see below),
        // so a mode flip there changes nothing visible — never morph it.
        let new_chat = self.state.read(cx).selected_chat.is_none();
        // Morph clock in ms.
        let now_ms = self.morph_clock.elapsed().as_secs_f32() * 1000.0;
        let route_snap = self
            .route_snap_until
            .is_some_and(|until| Instant::now() < until);
        self.flip_morph = flip_morph_step(
            self.flip_morph,
            committed_flip && !new_chat,
            self.last_rendered_height,
            now_ms,
            motion::reduced_motion(cx),
            route_snap,
        );
        LayoutFrame {
            last_width,
            content_height,
            new_chat,
            now_ms,
            expanded: self.expanded_mode,
        }
    }

    /// The composer column with its notices: the send failure, a failed
    /// durable delivery, and the turn-boundary steering hint.
    fn render_notices(
        &self,
        mode: SendButtonMode,
        wide: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let failure = self.failure.clone();
        let failed_delivery = if matches!(self.transport, ComposerTransport::Main) {
            self.state.read(cx).failed_commands().into_iter().next()
        } else {
            None
        };
        // Centered composer column (zeron `mx-auto w-full max-w-3xl`).
        let container = div()
            .w_full()
            .when(!wide, |el| {
                el.max_w(px(crate::appearance::chat_style::COMPOSER_WIDTH))
            })
            .mx_auto()
            .flex()
            .flex_col()
            .gap(px(Theme::SPACE_SM))
            .px(px(
                if wide && matches!(self.transport, ComposerTransport::Main) {
                    48.0
                } else {
                    Theme::SPACE_LG
                },
            ))
            .pb(px(Theme::SPACE_LG))
            .when_some(failure, |el, message| {
                el.child(failure_notice(message, theme, cx))
            });
        let container = container.when_some(failed_delivery, |el, failed| {
            el.child(delivery_failure_notice(failed, theme, cx))
        });

        // Turn-boundary steering notice: for agents without mid-turn
        // injection, a "steer" is queued and applies when the current turn
        // finishes. Without this hint the queue reads as a dropped steer.
        let steer_queues = mode == SendButtonMode::Steer
            && self.pickers.read(cx).resolved_steering_mode(cx)
                == Some(cypher_proto::SteeringMode::TurnBoundary);
        container.when(steer_queues, |el| {
            el.child(
                div()
                    .mt(px(6.0))
                    .px(px(12.0))
                    .text_size(px(11.0))
                    .line_height(px(15.0))
                    .text_color(theme.text_muted.opacity(0.8))
                    .child("This agent can't be steered mid-turn — your message will be queued and sent when the current turn finishes."),
            )
        })
    }

    /// Committed-height morph: the layout below is already the NEW mode's;
    /// only the pill's height (and the entrance fade/text glide driven by
    /// `morph_t`) animates. Steady state renders exactly the target.
    /// Staged attachments add the wrap strip's height to the pill in BOTH
    /// modes (attachment-ui.tsx AttachmentStrip sits above the input row).
    fn advance_pill_morph(
        &mut self,
        expanded: bool,
        layout: &LayoutFrame,
        compact_height: f32,
        window: &mut Window,
    ) -> PillFrame {
        let now_ms = layout.now_ms;
        let staged_images = self.staged().iter().filter(|a| a.image().is_some()).count();
        let staged_files = self.staged().len() - staged_images;
        let strip_width_hint = if layout.last_width > 0.0 {
            layout.last_width
        } else {
            720.0
        };
        let strip_h = attachment_strip_height(staged_images, staged_files, strip_width_hint);
        let base_height = if expanded {
            composer_total_height(layout.content_height)
        } else {
            compact_height
        };
        let target_height = base_height + strip_h;
        let (pill_height, morph_t, morphing) = match self.flip_morph {
            Some(m) if !m.done(now_ms) => {
                (m.height(target_height, now_ms), m.progress(now_ms), true)
            }
            _ => (target_height, 1.0, false),
        };
        if !morphing {
            self.flip_morph = None;
        } else {
            // Manual tween drive: keep frames coming (shell.rs motion_active).
            window.request_animation_frame();
        }
        self.last_rendered_height = pill_height;
        PillFrame {
            expanded,
            compact_height,
            base_height,
            pill_height,
            morph_t,
            morphing,
        }
    }

    /// The pill and everything inside it, in the frame's layout mode.
    fn render_pill(
        &mut self,
        frame: &PillFrame,
        mode: SendButtonMode,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let send_button = self.render_send_button(mode, cx);
        // Attaching lives in the `/` menu (Attach files), not on the pill;
        // paste and drop feed the same strip.
        // Staged-thumbnail strip (attachment-ui.tsx AttachmentStrip), above
        // the input inside the pill in both modes.
        let strip = self.render_attachment_strip(theme, cx);

        // The pill chrome (zeron composer.tsx): `rounded-[26px] border
        // border-white/[0.08] bg-white/[0.03] shadow-xl` — a floating pill with
        // a hairline over a faint wash, never a solid grey box. Picker chips
        // and the send circle live INSIDE the pill.
        let pill_bg = theme.input_glass_bg();
        // Its shadow is not a box shadow: that paints BEHIND the translucent
        // fill and shows through as an inner glow (theme.rs's
        // card_selected_shadows lesson; user report). See `pill_shadow` below.
        let pill_width = self.pill_width.clone();
        let pill = div()
            .relative()
            .rounded(px(PILL_RADIUS))
            .bg(pill_bg)
            .border_1()
            .border_color(theme.border)
            .child(
                gpui::canvas(
                    move |bounds, window, _| {
                        let width = f32::from(bounds.size.width);
                        if (width - pill_width.get()).abs() > 0.5 {
                            pill_width.set(width);
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );
        // The pill's bottom edge is stationary on screen (the composer sits at
        // the bottom of the shell column; growth moves the TOP edge), so the
        // controls pin to the bottom and only the text glides with the reveal
        // (the send/attach/chips must not ride the height, and none of them
        // fade — the full cluster stays visible throughout).
        let cluster_dy = morph_cluster_dy(frame.morph_t);
        // The context gauge round the send button, drawn once a mode change
        // has landed: mid-morph the pill's end is still the old one's shape.
        // The send button's bottom inset: centered in the compact row, or in
        // the expanded actions row's 32px zone above its 10px bottom pad.
        let send_bottom_inset = if frame.expanded {
            10.0 + (ACTIONS_ROW_HEIGHT - 4.0 - 10.0 - SEND_BUTTON_SIZE) / 2.0
        } else {
            (frame.compact_height - PILL_BORDER_V - SEND_BUTTON_SIZE) / 2.0
        };
        let edge_ring = (!frame.morphing)
            .then(|| self.context_ring_reading(cx))
            .flatten()
            .map(|reading| {
                self.render_edge_ring(
                    reading,
                    frame.pill_height - PILL_BORDER_V,
                    send_bottom_inset,
                    theme,
                    cx,
                )
            });
        let parts = PillParts {
            pill,
            strip,
            send_button,
            edge_ring,
            cluster_dy,
        };
        if frame.expanded {
            self.render_pill_expanded(parts, frame, theme, cx)
        } else {
            self.render_pill_compact(parts, frame, theme, cx)
        }
    }

    /// Expanded: textarea on top (`px-4 pb-1 pt-4`), actions row
    /// (`px-3 pb-2.5 pt-1`, h-8 chips → 46px) ABSOLUTE at the pill's
    /// stationary bottom — constant screen-y through the morph, with
    /// the 2.5px compact↔expanded centering delta gliding out. The
    /// text container is laid out at TARGET size (committed layout
    /// never reflows mid-tween — the caret can't jump); its top pad
    /// eases 12→16 so the first line glides from its compact resting
    /// place. The whole control cluster stays at full alpha — chips,
    /// attach and send are all (near-)stationary on the bottom anchor.
    fn render_pill_expanded(
        &self,
        parts: PillParts,
        frame: &PillFrame,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let PillParts {
            pill,
            strip,
            send_button,
            edge_ring,
            cluster_dy,
        } = parts;
        let text_pt = morph_text_pad(frame.morph_t);
        pill.h(px(frame.pill_height))
            .overflow_hidden()
            .relative()
            .flex()
            .flex_col()
            .children(strip)
            .child(
                div()
                    .h(px(
                        (frame.base_height - PILL_BORDER_V - ACTIONS_ROW_HEIGHT).max(0.0)
                    ))
                    .px(px(16.0))
                    .pt(px(text_pt))
                    .pb(px(4.0))
                    .child(self.render_input_with_completion(theme, cx)),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(-cluster_dy))
                    .h(px(ACTIONS_ROW_HEIGHT))
                    .flex()
                    .flex_row()
                    .items_center()
                    // Shared cluster metrics (see CLUSTER_INSET): gap-1
                    // internals and the right inset (12) are the compact
                    // ones, so the buttons never step sideways.
                    .gap(px(4.0))
                    .pl(px(12.0))
                    .pr(px(CLUSTER_INSET))
                    .pt(px(4.0))
                    .pb(px(10.0))
                    .child(div().flex_1().min_w_0().child(self.pickers.clone()))
                    .child(send_button),
            )
            .children(edge_ring)
    }

    /// Compact pill: input and the actions cluster on one 47px line
    /// (`py-3 pl-4 pr-2` textarea, `gap-2 py-1.5 pl-1 pr-2` cluster;
    /// the 22.75px line centers to the same 12px inset as `py-3`).
    /// The row is BOTTOM-justified: during the collapse morph the pill
    /// top sweeps down over a stationary row, the text walks down from
    /// its expanded resting place via a decaying relative offset, and
    /// the whole inline cluster (chips + send) holds its spot at
    /// full alpha (2.5px centering delta gliding in).
    fn render_pill_compact(
        &self,
        parts: PillParts,
        frame: &PillFrame,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let PillParts {
            pill,
            strip,
            send_button,
            edge_ring,
            cluster_dy,
        } = parts;
        let text_glide = match self.flip_morph {
            Some(m) if frame.morphing => collapse_text_glide(m.from, frame.morph_t),
            _ => 0.0,
        };
        pill.h(px(frame.pill_height))
            .overflow_hidden()
            .flex()
            .flex_col()
            .justify_end()
            .children(strip)
            .child(
                div()
                    .h(px(frame.compact_height - PILL_BORDER_V))
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            // Narrow tiles: the cluster's chips shrink
                            // before the input loses its last word.
                            .min_w(px(96.0))
                            .pl(px(16.0))
                            .pr(px(8.0))
                            .relative()
                            .top(px(-text_glide))
                            .child(self.render_input_with_completion(theme, cx)),
                    )
                    .child(
                        div()
                            // Shrinkable (basis = content): the picker
                            // chips ellipsize under row pressure.
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            // Shared cluster metrics (`gap-1 pl-1`, zeron
                            // composer-actions.tsx): identical internals
                            // to expanded, right inset included
                            // (CLUSTER_INSET).
                            .gap(px(4.0))
                            .pl(px(4.0))
                            .pr(px(CLUSTER_INSET))
                            .relative()
                            .top(px(-cluster_dy))
                            .child(div().min_w_0().child(self.pickers.clone()))
                            .child(send_button),
                    ),
            )
            .children(edge_ring)
    }

    /// The pill frosted over the transcript, with its soft lift shadow.
    fn render_pill_stack(&self, body: gpui::Div, theme: &Theme) -> gpui::Div {
        // The file dropzone lives in the shell (the whole conversation column,
        // not just the pill — shell.rs `chat-dropzone`); drops land back here
        // via `add_paths`.
        // Frosted: the pill backdrop-blurs the transcript scrolling under it
        // (the popover glass treatment; radius matches the pill's rounding).
        let frosted = crate::kit::frost::frosted(
            PILL_RADIUS,
            16.0,
            // Handed back by an answer, the composer returns instantly — the
            // same swap, from the other side.
            if self.input_swap_instant {
                body.into_any_element()
            } else {
                motion::fade_quick("composer-input", body).into_any_element()
            },
        );
        // The pill's lift: a soft shadow around it, never under it, painted
        // after the frost so the blur never samples it. It follows the pill
        // through every height change (absolute over the same box).
        let reach = PILL_SHADOW_REACH;
        let pill_shadow = div()
            .absolute()
            .top(px(-reach.top))
            .bottom(px(-reach.bottom))
            .left(px(-reach.side))
            .right(px(-reach.side))
            .child(
                crate::kit::soft_shadow::outside_shadow(PILL_RADIUS, reach, theme.lift_shadow())
                    .size_full(),
            );
        let pill_shadow = if self.input_swap_instant {
            pill_shadow.into_any_element()
        } else {
            motion::fade_quick("composer-input-shadow", pill_shadow).into_any_element()
        };
        div()
            .relative()
            .flex()
            .flex_col()
            .child(frosted)
            .child(pill_shadow)
    }

    /// Full-size preview of a staged thumbnail (AttachmentPreviewDialog).
    fn with_preview(
        &mut self,
        container: gpui::Div,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        if let Some(preview) = self.preview.clone() {
            if std::mem::take(&mut self.preview_focus_pending) {
                window.focus(&self.preview_focus, cx);
            }
            let weak = cx.weak_entity();
            return container.child(attachments::lightbox(
                window.viewport_size(),
                &preview,
                &self.preview_focus,
                move |window, cx| {
                    // Hand focus back to the input so typing (and the next
                    // Escape) lands where it did before the lightbox opened.
                    if let Ok(input_focus) = weak.update(cx, |this, cx| {
                        this.preview = None;
                        cx.notify();
                        this.input.read(cx).focus_handle.clone()
                    }) {
                        window.focus(&input_focus, cx);
                    }
                },
            ));
        }
        container
    }
}

/// zeron composer.tsx `Notice` (matches the transcript ErrorChip palette):
/// `flex items-start gap-2 rounded-xl border px-3 py-2 text-[12px]
/// leading-snug` with a 14px DangerTriangle — a subtle tinted wash, not a
/// bare red stroke. Amber for the offline-ish case (engine not connected),
/// red for send/run failures. Click dismisses.
fn failure_notice(
    message: SharedString,
    theme: &Theme,
    cx: &mut Context<Composer>,
) -> gpui::Stateful<gpui::Div> {
    let offline = message.as_ref() == "Engine not connected";
    let (border_c, wash, text_c) = if offline {
        let amber = theme.warning; // amber-400
        let amber_200 = theme.warning_muted;
        (
            amber.opacity(0.16),
            amber.opacity(0.05),
            amber_200.opacity(0.9),
        )
    } else {
        let danger = theme.danger; // red-400
        let red_300 = theme.danger_muted;
        (
            danger.opacity(0.16),
            danger.opacity(0.05),
            red_300.opacity(0.9),
        )
    };
    div()
        .id("composer-failure")
        .mx(px(4.0))
        .mt(px(6.0))
        .flex()
        .items_start()
        .gap(px(8.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(border_c)
        .bg(wash)
        .px(px(12.0))
        .py(px(8.0))
        .text_size(px(12.0))
        .line_height(px(16.0))
        .text_color(text_c)
        .cursor_pointer()
        .on_click(cx.listener(|this, _, _, cx| {
            this.failure = None;
            cx.notify();
        }))
        .child(
            crate::kit::icons::icon(crate::kit::icons::DANGER_TRIANGLE)
                .size(px(14.0))
                .mt(px(2.0))
                .text_color(text_c),
        )
        .child(div().min_w_0().child(message))
}

/// A durable send the host failed: its prompt, the host's resolution, and
/// Retry.
fn delivery_failure_notice(
    failed: crate::state::FailedCommand,
    theme: &Theme,
    cx: &mut Context<Composer>,
) -> gpui::Stateful<gpui::Div> {
    let command_id = failed.command_id.clone();
    let resolution = failed
        .resolution
        .clone()
        .unwrap_or_else(|| "The host did not complete this send.".into());
    div()
        .id("composer-delivery-failure")
        .mx(px(4.0))
        .mt(px(6.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(theme.danger.opacity(0.16))
        .bg(theme.danger.opacity(0.05))
        .px(px(12.0))
        .py(px(8.0))
        .text_size(px(12.0))
        .text_color(theme.danger_muted.opacity(0.9))
        .child(
            crate::kit::icons::icon(crate::kit::icons::DANGER_TRIANGLE)
                .size(px(14.0))
                .text_color(theme.danger_muted.opacity(0.9)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .truncate()
                        .child(format!("Send failed: {}", failed.prompt)),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(11.0))
                        .text_color(theme.text_muted)
                        .child(resolution),
                ),
        )
        .child(
            div()
                .id("composer-delivery-retry")
                .flex_none()
                .rounded(px(7.0))
                .bg(theme.danger.opacity(0.12))
                .px(px(9.0))
                .py(px(5.0))
                .cursor_pointer()
                .hover(|s| s.bg(theme.danger.opacity(0.18)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.retry_failed_command(command_id.clone(), cx);
                }))
                .child("Retry"),
        )
}
