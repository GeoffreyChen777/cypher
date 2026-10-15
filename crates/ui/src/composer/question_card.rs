//! The question card: an agent's question, or a permission request, in place
//! of the composer pill until it is answered.
//!
//! It wears the pill's own chrome (frosted fill, hairline border, soft lift,
//! the pill's rounding), so answering reads as the composer changing shape
//! rather than a dialog landing on it. Top to bottom: who is asking, the
//! question and its context, then the answer.
//!
//! A permission request ([`super::permission_card`]) lays its facts out as
//! labeled rows and its options as buttons weighted by what they grant: the
//! narrowest approval filled, broader ones outlined, Deny quiet, Deny and stop
//! in red. Every other question lists its options as rows.

use super::permission_card::{self, FactKind, OptionRole, PermissionRequest};
use super::*;

/// How long a new card ignores its shortcuts: a key typed into the composer
/// as the card appears must not answer it.
pub(crate) const SHORTCUT_GRACE: Duration = Duration::from_millis(500);

impl Composer {
    /// The current page's options, when it is a permission request (the
    /// card that answers to letter shortcuts).
    pub(super) fn permission_options(&self) -> Option<Vec<String>> {
        let wizard = self.wizard.as_ref()?;
        if wizard.slash.is_some() {
            return None;
        }
        let question = wizard.current()?;
        let prompt = if question.question.is_empty() {
            &question.header
        } else {
            &question.question
        };
        permission_card::parse(question, prompt).map(|_| question.options.clone())
    }

    /// The card, ready to stand in for the composer pill.
    pub(super) fn render_wizard(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
        let permission = wizard
            .slash
            .is_none()
            .then(|| permission_card::parse(&question, &prompt))
            .flatten();
        let chrome_title = wizard
            .slash
            .clone()
            .unwrap_or_else(|| SharedString::from("Question"));
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
            permission,
        };

        let header = self.wizard_header(&frame, cx);
        let (body, footer) = match frame.permission.clone() {
            Some(request) => (
                self.permission_body(&frame, &request),
                self.permission_actions(&frame, cx),
            ),
            None => (self.wizard_body(&frame, cx), self.wizard_footer(&frame, cx)),
        };
        let theme = &frame.theme;

        let card = div()
            .id("question-panel")
            .track_focus(&self.wizard_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_wizard_key(event, window, cx)
            }))
            .occlude()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .w_full()
            .min_w_0()
            .rounded(px(PILL_RADIUS))
            .bg(theme.input_glass_bg())
            .border_1()
            .border_color(theme.border)
            .px(px(20.0))
            .pt(px(12.0))
            .pb(px(16.0))
            .flex()
            .flex_col()
            .child(crate::markdown::render::selection_frame_reset(scope))
            .child(header)
            .child(body)
            .child(footer);
        self.render_pill_stack(div().w_full().child(card), theme)
            .into_any_element()
    }

    /// Who is asking (a question, a slash command's dialog, a permission
    /// request), where in a set this page is, and the dismiss button.
    fn wizard_header(&self, frame: &WizardFrame, cx: &mut Context<Self>) -> gpui::Div {
        let theme = &frame.theme;
        let (icon, tint, title) = match &frame.permission {
            Some(request) => (
                crate::kit::icons::SHIELD_CHECK,
                if request.serious {
                    theme.warning
                } else {
                    theme.text_muted
                },
                SharedString::from("Permission request"),
            ),
            None if frame.wizard.slash.is_some() => (
                crate::kit::icons::TUNING,
                theme.text_muted,
                frame.chrome_title.clone(),
            ),
            None => (
                crate::kit::icons::QUESTION_CIRCLE,
                theme.text_muted,
                frame.chrome_title.clone(),
            ),
        };
        div()
            .flex_none()
            .h(px(28.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(7.0))
            .child(
                crate::kit::icons::icon(icon)
                    .size(px(14.0))
                    .flex_none()
                    .text_color(tint),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_muted)
                    .child(title),
            )
            .when(frame.pages > 1, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(format!(
                            "{} of {}",
                            frame.page + 1,
                            frame.pages
                        ))),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .id("wizard-cancel")
                    .flex_none()
                    .size(px(24.0))
                    .mr(px(-6.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(crate::kit::theme::ink(0.07)))
                    .on_click(cx.listener(|this, _, _, cx| this.wizard_cancel(cx)))
                    .child(
                        crate::kit::icons::icon(crate::kit::icons::CLOSE)
                            .size(px(11.0))
                            .text_color(theme.text_faint),
                    ),
            )
    }

    /// The question's headline.
    fn wizard_question(frame: &WizardFrame, text: String) -> gpui::Div {
        let theme = &frame.theme;
        div()
            .mt(px(4.0))
            .text_size(px(15.0))
            .line_height(px(21.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.text)
            .cursor_text()
            .child(crate::markdown::render::selectable_plain_text(
                frame.scope,
                frame.text_key("question"),
                SharedString::from(text),
                theme,
            ))
    }

    /// An ordinary question: the question, its context, then the answer.
    fn wizard_body(
        &self,
        frame: &WizardFrame,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &frame.theme;
        let scope = frame.scope;
        let question = &frame.question;
        let optional_comment = &frame.optional_comment;
        let (question_text, context) = match optional_comment.as_ref() {
            Some(copy) => (copy.question.clone(), copy.context.clone()),
            None => split_question_context(&frame.prompt),
        };
        let mut body = self
            .scroll_body()
            .child(Self::wizard_question(frame, question_text));
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
                    .mt(px(14.0))
                    .flex()
                    .flex_col()
                    // Single-select needs no label: the numbered rows say it.
                    .when(frame.multi, |el| {
                        el.child(wizard_section_label("Pick any that apply", theme))
                    })
                    .child(div().flex().flex_col().gap(px(4.0)).children(options)),
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
                    .mt(px(14.0))
                    .flex()
                    .flex_col()
                    .child(wizard_section_label(label, theme))
                    .child(
                        div()
                            .w_full()
                            .min_h(px(44.0))
                            .rounded(px(14.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(crate::kit::theme::ink(0.03))
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

    /// One answer row: its number key (a checkbox on multi-select pages, a
    /// check once picked), label and description.
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
        let marker = wizard_option_marker(picked, custom, multi, number.clone(), theme);

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
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(14.0))
            .when(custom && !picked, |el| {
                el.border_1().border_dashed().border_color(theme.border)
            })
            .bg(if picked {
                crate::kit::theme::ink(0.08)
            } else {
                motion::hover_blend(
                    &hover_key,
                    crate::kit::theme::ink(if custom { 0.0 } else { 0.03 }),
                    crate::kit::theme::ink(0.065),
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

    /// Keyboard hints on the left, Back/Submit on the right. A single-select
    /// page answers on the click itself, so it gets no Submit.
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
                    .rounded(px(10.0))
                    .on_click(cx.listener(|this, _, _, cx| this.wizard_skip_comment(cx)))
                    .into_any_element(),
            )
        } else if page > 0 {
            Some(
                crate::kit::popover::btn_ghost(theme, "Back", "wizard-back")
                    .id("wizard-back")
                    .rounded(px(10.0))
                    .on_click(cx.listener(|this, _, _, cx| this.wizard_back(cx)))
                    .into_any_element(),
            )
        } else {
            None
        };
        let show_primary = !pick_only || frame.pages > 1;
        div()
            .flex_none()
            .mt(px(14.0))
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
                        .rounded(px(10.0))
                        .when(!can_advance, |el| el.opacity(0.4).cursor_default())
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.wizard_ready(cx) {
                                this.wizard_advance(cx);
                            }
                        })),
                )
            })
    }

    /// A permission request: the question, its facts as labeled rows, the
    /// sentences worth reading before answering, and the change or tool input.
    fn permission_body(
        &self,
        frame: &WizardFrame,
        request: &PermissionRequest,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &frame.theme;
        let scope = frame.scope;
        let mut body = self
            .scroll_body()
            .child(Self::wizard_question(frame, request.question.clone()));
        let rows: Vec<gpui::Div> = request
            .rows()
            .enumerate()
            .map(|(ix, fact)| {
                let value = div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(if fact.kind.monospace() { 12.0 } else { 13.0 }))
                    .line_height(px(18.0))
                    .text_color(theme.text)
                    .cursor_text()
                    .when(fact.kind.monospace(), |el| el.mono(theme))
                    .child(crate::markdown::render::selectable_plain_text(
                        scope,
                        frame.text_key(&format!("fact{ix}")),
                        SharedString::from(fact.value.clone()),
                        theme,
                    ));
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_none()
                            .w(px(64.0))
                            .text_size(px(12.0))
                            .line_height(px(18.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(fact.kind.label().unwrap_or_default())),
                    )
                    .child(value)
            })
            .collect();
        if !rows.is_empty() {
            body = body.child(
                div()
                    .mt(px(12.0))
                    .px(px(14.0))
                    .py(px(10.0))
                    .rounded(px(14.0))
                    .bg(crate::kit::theme::ink(0.035))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .children(rows),
            );
        }
        let sentences: Vec<gpui::Div> = request
            .sentences()
            .enumerate()
            .map(|(ix, fact)| {
                let icon = match fact.kind {
                    FactKind::Review => crate::kit::icons::LIGHTBULB,
                    FactKind::Network => crate::kit::icons::GLOBAL,
                    _ => crate::kit::icons::INFO_CIRCLE,
                };
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(8.0))
                    .child(
                        crate::kit::icons::icon(icon)
                            .size(px(13.0))
                            .flex_none()
                            .mt(px(2.5))
                            .text_color(theme.text_faint),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.5))
                            .line_height(px(18.0))
                            .text_color(theme.text_muted)
                            .cursor_text()
                            .child(crate::markdown::render::selectable_plain_text(
                                scope,
                                frame.text_key(&format!("note{ix}")),
                                SharedString::from(fact.value.clone()),
                                theme,
                            )),
                    )
            })
            .collect();
        if !sentences.is_empty() {
            body = body.child(
                div()
                    .mt(px(10.0))
                    .px(px(2.0))
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .children(sentences),
            );
        }
        if let Some((label, text)) = &request.block {
            body = body.child(
                div()
                    .mt(px(12.0))
                    .flex()
                    .flex_col()
                    .child(wizard_section_label(label, theme))
                    .child(
                        div()
                            .id("permission-block")
                            .max_h(px(180.0))
                            .overflow_y_scroll()
                            .px(px(12.0))
                            .py(px(8.0))
                            .rounded(px(12.0))
                            .bg(theme.code_wash)
                            .text_size(px(12.0))
                            .line_height(px(17.0))
                            .text_color(theme.code_text)
                            .mono(theme)
                            .cursor_text()
                            .child(crate::markdown::render::selectable_plain_text(
                                scope,
                                frame.text_key("block"),
                                SharedString::from(text.clone()),
                                theme,
                            )),
                    ),
            );
        }
        body
    }

    /// The scrolling body every card shares, capped so the transcript stays
    /// in view above it and the whole card fits the column it sits in.
    fn scroll_body(&self) -> gpui::Stateful<gpui::Div> {
        let chrome = self.wizard_total_height.get() - self.wizard_body_height.get();
        let measured = self.wizard_body_height.clone();
        div()
            .id("wizard-scroll")
            .relative()
            .min_w_0()
            .max_h(px(wizard_content_max_height(self.available_height, chrome)))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(measure_height(measured))
    }

    /// The answers as buttons, weighted by what they grant: Deny and Deny and
    /// stop on the left, broader approvals then the narrowest (filled) on the
    /// right. Number keys still pick them in the order the request lists them.
    fn permission_actions(&self, frame: &WizardFrame, cx: &mut Context<Self>) -> gpui::Div {
        let theme = &frame.theme;
        let roles = permission_card::option_roles(&frame.question.options);
        let keys = permission_card::shortcut_keys(&frame.question.options);
        let button = |ix: usize,
                      label: &str,
                      role: OptionRole,
                      key: Option<&'static str>,
                      cx: &mut Context<Self>| {
            let fade = format!("permission-option-{ix}");
            // The button's letter, a keycap after its label; on the filled
            // button it is drawn in the button's own ink.
            let keycap = key.map(|key| {
                let on_fill = role == OptionRole::Primary;
                div()
                    .flex_none()
                    .min_w(px(16.0))
                    .h(px(16.0))
                    .px(px(3.0))
                    .rounded(px(4.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(if on_fill {
                        theme.on_solid.opacity(0.16)
                    } else {
                        crate::kit::theme::ink(0.06)
                    })
                    .text_size(px(10.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(if on_fill {
                        theme.on_solid.opacity(0.75)
                    } else {
                        theme.text_faint
                    })
                    .child(SharedString::from(key.to_uppercase()))
            });
            let base = div()
                .id(("permission-option", ix))
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .pl(px(14.0))
                .pr(px(if keycap.is_some() { 8.0 } else { 14.0 }))
                .py(px(7.0))
                .rounded(px(10.0))
                .text_size(px(13.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .cursor_pointer()
                .on_hover(motion::hover_listener(fade.clone()))
                .on_click(cx.listener(move |this, _, _, cx| this.wizard_select(ix, cx)))
                .child(SharedString::from(label.to_owned()))
                .children(keycap);
            match role {
                OptionRole::Primary => base
                    .bg(theme.text)
                    .text_color(theme.on_solid)
                    .hover(|s| s.opacity(0.88)),
                OptionRole::Secondary => base
                    .border_1()
                    .border_color(theme.border)
                    .text_color(theme.text)
                    .bg(motion::hover_blend(
                        &fade,
                        crate::kit::theme::ink(0.02),
                        crate::kit::theme::ink(0.07),
                    )),
                OptionRole::Deny => base
                    .text_color(motion::hover_blend(&fade, theme.text_muted, theme.text))
                    .bg(motion::hover_blend(
                        &fade,
                        crate::kit::theme::wash(0.0),
                        crate::kit::theme::ink(0.06),
                    )),
                OptionRole::Stop => {
                    base.text_color(theme.danger.opacity(0.85))
                        .bg(motion::hover_blend(
                            &fade,
                            theme.danger.opacity(0.0),
                            theme.danger.opacity(0.08),
                        ))
                }
            }
        };
        let options = frame.question.options.clone();
        let mut left = Vec::new();
        let mut broader = Vec::new();
        let mut primary = None;
        for (ix, ((label, role), key)) in options.iter().zip(roles).zip(keys).enumerate() {
            let element = button(ix, label, role, key, cx).into_any_element();
            match role {
                OptionRole::Deny | OptionRole::Stop => left.push(element),
                OptionRole::Secondary => broader.push(element),
                OptionRole::Primary => primary = Some(element),
            }
        }
        // Broader approvals read outward from the primary one.
        broader.reverse();
        div()
            .flex_none()
            .mt(px(16.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .children(left)
            .child(div().flex_1())
            .children(broader)
            .children(primary)
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
    /// The page read as a permission request, when it is one.
    permission: Option<PermissionRequest>,
}

impl WizardFrame {
    /// A selectable text's key: the request and page, then `part`.
    fn text_key(&self, part: &str) -> Arc<str> {
        format!("{}:{part}", self.key_prefix).into()
    }
}

/// An option's leading marker: its number key, a checkbox on multi-select
/// pages, a pen on the write-your-own row; a solid check once picked.
fn wizard_option_marker(
    picked: bool,
    custom: bool,
    multi: bool,
    number: Option<SharedString>,
    theme: &Theme,
) -> gpui::Div {
    div()
        .flex_none()
        .size(px(18.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(if multi { 5.0 } else { 9.0 }))
        .when(picked, |el| el.bg(theme.text))
        .when(!picked && multi, |el| {
            el.border_1().border_color(theme.border_strong)
        })
        .map(|el| {
            if picked {
                el.child(
                    crate::kit::icons::icon(crate::kit::icons::CHECK)
                        .size(px(11.0))
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
                el.text_size(px(11.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_faint)
                    .children(number)
            }
        })
}

/// What the user already picked (the optional-comment step), shown as the
/// answer it is.
fn wizard_picked_answers(frame: &WizardFrame, copy: &OptionalCommentCopy) -> gpui::Div {
    let theme = &frame.theme;
    let scope = frame.scope;
    div()
        .mt(px(14.0))
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
        .child(div().flex().flex_col().gap(px(4.0)).children(
            copy.selected.lines().enumerate().map(|(ix, line)| {
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(10.0))
                    .px(px(12.0))
                    .py(px(9.0))
                    .rounded(px(14.0))
                    .bg(crate::kit::theme::ink(0.08))
                    .child(wizard_option_marker(true, false, false, None, theme))
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

/// A layer that records its parent's laid-out height, re-rendering when it
/// changes so the card's caps follow.
pub(super) fn measure_height(cell: Rc<std::cell::Cell<f32>>) -> gpui::Canvas<()> {
    gpui::canvas(
        move |bounds, window, _| {
            let height = f32::from(bounds.size.height);
            if (height - cell.get()).abs() > 0.5 {
                cell.set(height);
                window.refresh();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}
