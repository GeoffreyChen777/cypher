//! The add/edit provider form, the Claude Code dialog and the removal
//! confirmation, with their shared key handling and field rows.

use super::*;

impl ProvidersPage {
    pub(super) fn on_dialog_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" {
            self.close_dialog(window, cx);
            cx.stop_propagation();
        } else if event.keystroke.key == "tab" {
            let mut handles = vec![self.close_focus.clone()];
            if let Some(form) = &self.form {
                if form.original.is_none() {
                    handles.push(form.id.focus_handle(cx));
                }
                handles.push(form.url.focus_handle(cx));
                handles.push(form.key.focus_handle(cx));
            }
            if let Some(oauth) = &self.oauth
                && oauth
                    .status
                    .as_ref()
                    .is_some_and(|s| s.phase == "awaiting_callback")
            {
                handles.push(oauth.callback.focus_handle(cx));
            }
            handles.push(self.cancel_focus.clone());
            handles.push(self.submit_focus.clone());
            let steal_dialog = self.busy.is_some()
                && self.oauth.as_ref().is_none_or(|oauth| {
                    !oauth
                        .status
                        .as_ref()
                        .is_some_and(|s| s.phase == "awaiting_callback")
                });
            if steal_dialog {
                self.dialog_focus.focus(window, cx);
            } else {
                let active = handles.iter().position(|h| h.is_focused(window));
                let delta = if event.keystroke.modifiers.shift {
                    -1
                } else {
                    1
                };
                if let Some(next) = popover::menu_step(active, handles.len(), delta) {
                    handles[next].focus(window, cx);
                }
            }
            cx.notify();
            cx.stop_propagation();
        }
    }

    pub(super) fn field(
        &self,
        field: Field,
        label: &'static str,
        hint: &str,
        theme: &Theme,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let form = self.form.as_ref().unwrap();
        let input = form.input(field);
        let error = match field {
            Field::Name => form.errors.name,
            Field::Url => form.errors.url,
            Field::Key => form.errors.key,
        };
        let focused = input.focus_handle(cx).is_focused(window);
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(widgets::field_label(theme, label))
            .child(
                div()
                    .id(("provider-field", field as usize))
                    .role(gpui::Role::Group)
                    .aria_label(label)
                    .w_full()
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(if error.is_some() {
                        theme.danger
                    } else if focused {
                        theme.accent
                    } else {
                        theme.border_strong
                    })
                    .bg(theme.input_glass_bg())
                    .child(input.clone()),
            )
            .child(
                caption(theme, error.unwrap_or(hint).to_string())
                    .when(error.is_some(), |el| el.text_color(theme.danger_muted)),
            )
    }

    pub(super) fn render_form(
        &mut self,
        window: &Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let form = self.form.as_ref().unwrap();
        let editing = form.original.is_some();
        let busy = self.busy.is_some();
        let width = (f32::from(window.viewport_size().width) - 40.0).clamp(280.0, 464.0);
        let fields = self.render_form_fields(window, theme, cx);
        let footer = self.render_form_footer(editing, busy, theme, cx);
        popover::dialog_card(theme)
            .id("provider-form-dialog")
            .role(gpui::Role::Dialog)
            .aria_label(if editing {
                "Provider settings"
            } else {
                "Add provider"
            })
            .track_focus(&self.dialog_focus)
            .key_context("ProviderDialog")
            .tab_group()
            .p_0()
            .w(px(width))
            .overflow_hidden()
            .on_key_down(cx.listener(Self::on_dialog_key))
            .child(self.dialog_heading(
                theme,
                if editing {
                    "Provider settings"
                } else {
                    "Add provider"
                },
                "Connect models to your workspace.",
                cx,
            ))
            .child(
                div()
                    .id("provider-form-scroll")
                    .max_h(px(
                        (f32::from(window.viewport_size().height) - 252.0).max(120.0)
                    ))
                    .overflow_y_scroll()
                    .child(fields),
            )
            .child(footer)
            .into_any_element()
    }

    /// The form's body: kind header, provider name (read-only once saved),
    /// base URL, API key, and any error.
    fn render_form_fields(
        &self,
        window: &Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let form = self.form.as_ref().unwrap();
        let saved = form.original.as_ref().is_some_and(|p| p.credential_saved);
        let spec = form.kind.spec();
        let mut fields = div()
            .px(px(24.0))
            .py(px(24.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(widgets::row_tile(theme, spec.icon).size(px(32.0)))
                    .child(
                        div()
                            .flex_1()
                            .child(widgets::row_title(theme, spec.title))
                            .child(caption(theme, spec.caption)),
                    )
                    .child(widgets::badge(theme, "API key")),
            );
        if let Some(p) = &form.original {
            fields = fields.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(widgets::field_label(theme, "Provider name"))
                    .child(
                        div()
                            .h(px(40.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.ink(0.025))
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(p.id.clone())),
                    ),
            );
        } else {
            fields = fields.child(self.field(
                Field::Name,
                "Provider name",
                "A short name to identify this connection.",
                theme,
                window,
                cx,
            ));
        }
        fields = fields
            .child(self.field(
                Field::Url,
                "Base URL",
                "Use the service root or its /v1 endpoint.",
                theme,
                window,
                cx,
            ))
            .child(self.field(
                Field::Key,
                "API key",
                if saved {
                    "Leave empty to keep your key. A new URL requires entering it again."
                } else {
                    "Get a key from your provider's dashboard."
                },
                theme,
                window,
                cx,
            ));
        if let Some(error) = &self.error {
            fields = fields.child(widgets::error_strip(theme, error.clone()).mt_0());
        }
        fields
    }

    /// Where the key is kept, then Cancel and Save.
    fn render_form_footer(
        &self,
        editing: bool,
        busy: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .px(px(24.0))
            .py(px(16.0))
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.ink(0.02))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(provider_icon(
                        icons::KEY_MINIMALISTIC,
                        14.0,
                        theme.text_muted,
                    ))
                    .child(caption(
                        theme,
                        if self.target.read(cx).is_local() {
                            format!(
                                "Saved only on {}. Never added to chats.",
                                self.target.read(cx).label(cx)
                            )
                        } else {
                            format!(
                                "Sent via your account's relay; saved only on {}.",
                                self.target.read(cx).label(cx)
                            )
                        },
                    )),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        button(
                            theme,
                            "provider-cancel",
                            "Cancel",
                            ButtonStyle::Ghost,
                            !busy,
                        )
                        .track_focus(&self.cancel_focus)
                        .when(!busy, |el| {
                            el.on_click(
                                cx.listener(|page, _, window, cx| page.close_dialog(window, cx)),
                            )
                        }),
                    )
                    .child(
                        button(
                            theme,
                            "provider-save",
                            if busy {
                                "Connecting…"
                            } else if editing {
                                "Save connection"
                            } else {
                                "Connect provider"
                            },
                            ButtonStyle::Primary,
                            !busy && self.target.read(cx).can_write(cx),
                        )
                        .h(px(36.0))
                        .track_focus(&self.submit_focus)
                        .when(busy, |el| {
                            el.child(crate::kit::loaders::mini_gradient_spinner(
                                "provider-saving",
                                2.0,
                                cx.entity_id(),
                                cx,
                            ))
                        })
                        .when(!busy && self.target.read(cx).can_write(cx), |el| {
                            el.on_click(cx.listener(|page, _, _, cx| page.save(cx)))
                        }),
                    ),
            )
    }

    pub(super) fn dialog_heading(
        &self,
        theme: &Theme,
        title: &str,
        description: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let busy = self.busy.is_some();
        div()
            .px(px(24.0))
            .pt(px(24.0))
            .flex()
            .items_start()
            .gap(px(12.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(popover::dialog_title(theme, title).text_size(px(18.0)))
                    .child(caption(theme, description.to_string()).mt(px(6.0)))
                    .child(
                        caption(
                            theme,
                            format!("Target device: {}", self.target.read(cx).label(cx)),
                        )
                        .mt(px(4.0)),
                    ),
            )
            .child(
                icon_button(
                    theme,
                    "provider-dialog-close",
                    icons::CLOSE,
                    "Close dialog",
                    !busy,
                )
                .track_focus(&self.close_focus)
                .when(!busy, |el| {
                    el.on_click(cx.listener(|page, _, window, cx| page.close_dialog(window, cx)))
                }),
            )
    }

    /// The Claude row's Manage dialog: the Claude Code CLI facts plus the
    /// device-scoped web-search fallback (the control reloads on open so it
    /// reflects the device's current catalog).
    pub(super) fn open_claude_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.dismiss(cx);
        self.claude_dialog = true;
        self.error = None;
        self.return_focus = Some(self.page_focus.clone());
        self.web_search.update(cx, |control, cx| control.reload(cx));
        self.dialog_focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn render_claude_dialog(
        &mut self,
        window: &Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let width = (f32::from(window.viewport_size().width) - 40.0).clamp(280.0, 464.0);
        let cli = self
            .snapshot
            .ready()
            .and_then(|snapshot| snapshot.providers.iter().find(|p| is_claude_cli(p)))
            .cloned();
        let installed = cli.as_ref().is_some_and(|p| p.credential_saved);
        let path = cli.map(|p| p.base_url).filter(|path| !path.is_empty());
        let body = div()
            .px(px(24.0))
            .py(px(24.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        widgets::row_tile(theme, icons::CLAUDE_MARK)
                            .size(px(32.0))
                            .bg(icons::claude_brand().opacity(0.06))
                            .text_color(icons::claude_brand()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::row_title(theme, "Claude Code CLI"))
                            .child(
                                caption(
                                    theme,
                                    path.unwrap_or_else(|| {
                                        "Not installed on this device.".to_string()
                                    }),
                                )
                                .truncate(),
                            ),
                    )
                    .child(widgets::badge(
                        theme,
                        if installed { "Installed" } else { "Missing" },
                    )),
            )
            .when(!installed, |el| {
                el.child(
                    button(
                        theme,
                        "provider-claude-install",
                        "Install Claude Code",
                        ButtonStyle::Secondary,
                        true,
                    )
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.open_url(CLAUDE_CODE_INSTALL);
                    })),
                )
            })
            .child(self.web_search.clone());
        let footer = div().px(px(24.0)).pb(px(24.0)).flex().justify_end().child(
            button(
                theme,
                "provider-claude-done",
                "Done",
                ButtonStyle::Primary,
                true,
            )
            .on_click(cx.listener(|page, _, window, cx| page.close_dialog(window, cx))),
        );
        popover::dialog_card(theme)
            .id("provider-claude-dialog")
            .role(gpui::Role::Dialog)
            .aria_label("Claude settings")
            .track_focus(&self.dialog_focus)
            .key_context("ProviderDialog")
            .tab_group()
            .p_0()
            .w(px(width))
            .overflow_hidden()
            .on_key_down(cx.listener(Self::on_dialog_key))
            .child(self.dialog_heading(
                theme,
                "Claude settings",
                "Claude Code on the target device, and where its web searches run.",
                cx,
            ))
            .child(
                div()
                    .id("provider-claude-scroll")
                    .max_h(px(
                        (f32::from(window.viewport_size().height) - 252.0).max(120.0)
                    ))
                    .overflow_y_scroll()
                    .child(body),
            )
            .child(footer)
            .into_any_element()
    }

    pub(super) fn render_confirmation(
        &mut self,
        window: &Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (id, remove) = self.confirm.clone().unwrap();
        let busy = self.busy.is_some();
        let oauth = matches!(id.as_str(), "anthropic" | "openai-codex");
        let title = if remove {
            "Delete provider?"
        } else if oauth {
            "Sign out?"
        } else {
            "Remove saved API key?"
        };
        let copy = if remove {
            format!(
                "“{id}” and its saved API key will be removed from {}.",
                self.target.read(cx).label(cx)
            )
        } else if oauth {
            format!(
                "You'll need to sign in again to use {} models on {}.",
                if id == "anthropic" {
                    "Claude"
                } else {
                    "ChatGPT"
                },
                self.target.read(cx).label(cx)
            )
        } else {
            format!("You'll need to enter an API key again to use models from “{id}”.")
        };
        let width = (f32::from(window.viewport_size().width) - 40.0).clamp(280.0, 420.0);
        popover::dialog_card(theme)
            .id("provider-confirm-dialog")
            .role(gpui::Role::AlertDialog)
            .aria_label(title)
            .track_focus(&self.dialog_focus)
            .key_context("ProviderDialog")
            .tab_group()
            .w(px(width))
            .p_0()
            .on_key_down(cx.listener(Self::on_dialog_key))
            .child(self.dialog_heading(
                theme,
                title,
                "Your existing conversations will be kept.",
                cx,
            ))
            .child(
                div()
                    .px(px(24.0))
                    .py(px(20.0))
                    .child(popover::dialog_body(theme, copy))
                    .when_some(self.error.clone(), |el, error| {
                        el.child(widgets::error_strip(theme, error))
                    }),
            )
            .child(
                div()
                    .px(px(24.0))
                    .py(px(16.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        button(
                            theme,
                            "provider-confirm-cancel",
                            "Cancel",
                            ButtonStyle::Secondary,
                            !busy,
                        )
                        .track_focus(&self.cancel_focus)
                        .when(!busy, |el| {
                            el.on_click(
                                cx.listener(|page, _, window, cx| page.close_dialog(window, cx)),
                            )
                        }),
                    )
                    .child(
                        button(
                            theme,
                            "provider-confirm",
                            if busy {
                                "Removing…"
                            } else if remove {
                                "Delete provider"
                            } else if oauth {
                                "Sign out"
                            } else {
                                "Remove API key"
                            },
                            ButtonStyle::Danger,
                            !busy && self.target.read(cx).can_write(cx),
                        )
                        .track_focus(&self.submit_focus)
                        .when(!busy, |el| {
                            el.on_click(cx.listener(move |page, _, _, cx| {
                                page.call(
                                    if remove {
                                        methods::REMOVE_PI_PROVIDER
                                    } else {
                                        methods::LOGOUT_PI_PROVIDER
                                    },
                                    serde_json::json!({ "id": id }),
                                    cx,
                                )
                            }))
                        }),
                    ),
            )
            .into_any_element()
    }
}
