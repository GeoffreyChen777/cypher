//! The provider list: subscription groups, provider rows and their menus,
//! the Add-provider trigger and menu, and the empty state.

use super::*;

impl ProvidersPage {
    pub(super) fn render_add_trigger(
        &mut self,
        theme: &Theme,
        id: &'static str,
        enabled: bool,
        align_end: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let open = self.add_menu.get().is_some();
        let mut trigger = add_provider_button(theme, id, enabled)
            .relative()
            .child(provider_icon(icons::ALT_ARROW_DOWN, 12.0, theme.on_solid))
            .when(open, |el| el.bg(theme.element_active))
            .when(enabled, |el| {
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|page, _, _, _| page.add_menu.note_trigger_press()),
                )
                .on_click(cx.listener(|page, _, window, cx| page.toggle_add_menu(window, cx)))
            });
        if open {
            trigger = trigger.child(self.render_add_menu(theme, align_end, cx));
        }
        trigger
    }

    fn render_add_menu(
        &mut self,
        theme: &Theme,
        align_end: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu = self.add_menu.get().unwrap().clone();
        let closing = self.add_menu.closing_since();
        let mut card = popover::popover_card(theme)
            .id("provider-add-menu")
            .role(gpui::Role::Menu)
            .aria_label("Add provider")
            .track_focus(&self.add_menu_focus)
            .w(px(268.0))
            .on_mouse_down_out(cx.listener(|page, _, window, cx| page.close_add_menu(window, cx)))
            .on_key_down(cx.listener(|page, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" | "tab" => {
                        page.close_add_menu(window, cx);
                        page.page_focus.focus(window, cx);
                    }
                    "up" | "down" => {
                        if let Some(menu) = page.add_menu.open_mut() {
                            let delta = if event.keystroke.key == "up" { -1 } else { 1 };
                            menu.active = popover::menu_step(
                                Some(menu.active),
                                CustomProviderKind::ALL.len(),
                                delta,
                            )
                            .unwrap_or(0);
                        }
                    }
                    "enter" | "space" => {
                        if let Some(menu) = page.add_menu.as_open().cloned()
                            && let Some(&kind) = CustomProviderKind::ALL.get(menu.active)
                        {
                            page.add_custom_kind(kind, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }));
        for (index, kind) in CustomProviderKind::ALL.iter().copied().enumerate() {
            let spec = kind.spec();
            let enabled = closing.is_none() && self.busy.is_none();
            card = card.child(
                popover::menu_row(
                    theme,
                    index == menu.active,
                    format!("provider-add-kind-{index}"),
                )
                .id(("provider-add-kind", index))
                .role(gpui::Role::MenuItem)
                .aria_label(spec.title)
                .items_start()
                .py(px(8.0))
                .when(enabled, |el| {
                    el.on_click(cx.listener(move |page, _, window, cx| {
                        page.add_custom_kind(kind, window, cx)
                    }))
                })
                .child(provider_icon(spec.icon, 16.0, theme.text_muted).mt(px(2.0)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(SharedString::from(spec.title))
                        .child(caption(theme, spec.caption)),
                ),
            );
        }
        let layer = "provider-add-menu-layer";
        if align_end {
            popover::anchored_menu_below_end(layer, card.into_any_element(), closing)
        } else {
            popover::anchored_menu_below(layer, card.into_any_element(), closing)
        }
    }

    fn menu_action(
        &mut self,
        provider: PiProviderInfo,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.page_focus.focus(window, cx);
        if is_claude_cli(&provider) {
            match index {
                0 => self.open_claude_dialog(window, cx),
                // Re-detect: the provider list re-resolves the `claude` CLI.
                1 => self.call(methods::LIST_PI_PROVIDERS, serde_json::json!({}), cx),
                2 => cx.open_url(CLAUDE_CODE_INSTALL),
                _ => {}
            }
            return;
        }
        match index {
            0 if provider.credential_saved => {
                self.call(
                    methods::REFRESH_PI_PROVIDER,
                    serde_json::json!({ "id": provider.id }),
                    cx,
                );
            }
            1 if provider.credential_saved => self.ask_remove(provider.id, false, cx),
            2 if !is_oauth(&provider) => self.ask_remove(provider.id, true, cx),
            _ => {}
        }
    }

    fn render_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let menu = self.menu.get().unwrap().clone();
        let closing = self.menu.closing_since();
        let mut card = popover::popover_card(theme)
            .id("provider-actions-menu")
            .role(gpui::Role::Menu)
            .aria_label("Provider actions")
            .track_focus(&self.menu_focus)
            .w(px(204.0))
            .on_mouse_down_out(cx.listener(|page, _, window, cx| page.close_menu(window, cx)))
            .on_key_down(cx.listener(|page, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" | "tab" => {
                        page.close_menu(window, cx);
                        page.page_focus.focus(window, cx);
                    }
                    "up" | "down" => {
                        if let Some(menu) = page.menu.open_mut() {
                            let delta = if event.keystroke.key == "up" { -1 } else { 1 };
                            menu.active =
                                popover::menu_step(Some(menu.active), 3, delta).unwrap_or(0);
                            if !menu.provider.credential_saved && !is_claude_cli(&menu.provider) {
                                menu.active = if is_oauth(&menu.provider) { 0 } else { 2 };
                            }
                        }
                    }
                    "enter" | "space" => {
                        if let Some(menu) = page.menu.as_open().cloned() {
                            page.menu_action(menu.provider, menu.active, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }));
        let claude = is_claude_cli(&menu.provider);
        let items: [(&str, &'static str); 3] = if claude {
            [
                ("Manage…", icons::SETTINGS_MINIMALISTIC),
                ("Re-detect Claude Code", icons::REFRESH),
                ("Install Claude Code…", icons::GLOBAL),
            ]
        } else {
            [
                ("Refresh models", icons::REFRESH),
                (
                    if is_oauth(&menu.provider) {
                        "Sign out"
                    } else {
                        "Remove API key"
                    },
                    icons::KEY_MINIMALISTIC,
                ),
                ("Delete provider…", icons::TRASH_BIN_MINIMALISTIC),
            ]
        };
        for (index, (label, glyph)) in items.into_iter().enumerate() {
            if index == 2 && is_oauth(&menu.provider) {
                continue;
            }
            let destructive = index == 2 && !claude;
            let enabled = self.busy.is_none()
                && (claude || index == 2 || menu.provider.credential_saved)
                && closing.is_none();
            let provider = menu.provider.clone();
            if index == 2 {
                card = card.child(popover::menu_separator());
            }
            card = card.child(
                popover::menu_row(
                    theme,
                    index == menu.active && enabled,
                    format!("provider-menu-{index}"),
                )
                .id(("provider-menu-action", index))
                .role(gpui::Role::MenuItem)
                .aria_label(label)
                .min_h(px(32.0))
                .when(destructive, |el| el.text_color(theme.danger_muted))
                .when(!enabled, |el| el.opacity(0.4))
                .when(enabled, |el| {
                    el.on_click(cx.listener(move |page, _, window, cx| {
                        page.menu_action(provider.clone(), index, window, cx)
                    }))
                })
                .child(provider_icon(
                    glyph,
                    15.0,
                    if destructive {
                        theme.danger_muted
                    } else {
                        theme.text_muted
                    },
                ))
                .child(SharedString::from(label)),
            );
        }
        popover::anchored_menu_below("provider-menu-layer", card.into_any_element(), closing)
    }

    pub(super) fn subscription_group(
        &mut self,
        theme: &Theme,
        provider: PiProviderInfo,
        index: usize,
        first_group: bool,
        current: Option<&str>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        widgets::section_card(theme)
            .mt(px(if first_group { 12.0 } else { 24.0 }))
            .child(self.provider_row(provider, index, true, current, theme, cx))
    }

    pub(super) fn provider_row(
        &mut self,
        provider: PiProviderInfo,
        index: usize,
        first: bool,
        current: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.busy.is_some() || !self.target.read(cx).can_write(cx);
        let refreshing = self.busy.as_ref().is_some_and(|b| {
            b.method == methods::REFRESH_PI_PROVIDER && b.provider.as_deref() == Some(&provider.id)
        });
        let status = provider_status_pill(&provider, refreshing, theme);
        let model_label = format!(
            "{} {}",
            provider.model_count,
            if provider.model_count == 1 {
                "model"
            } else {
                "models"
            }
        );
        let selected = current.filter(|model| model.starts_with(&format!("{}/", provider.id)));
        let more = self.provider_more_button(&provider, index, busy, theme, cx);
        let actions = provider_actions(&provider, index, busy, more, theme, cx);
        let (mark, tint) = match provider.id.as_str() {
            "anthropic" | "claude-code" => (icons::CLAUDE_MARK, Some(icons::claude_brand())),
            "openai-codex" => (icons::OPENAI_MARK, None),
            _ => (icons::GLOBAL, Some(theme.accent)),
        };
        div()
            .px(px(20.0))
            .py(px(16.0))
            .when(!first, |el| el.border_t_1().border_color(theme.border))
            .flex()
            .items_start()
            .gap(px(12.0))
            .child(
                widgets::row_tile(theme, mark)
                    .size(px(36.0))
                    .bg(tint.unwrap_or(theme.accent).opacity(0.06))
                    .when_some(tint, |el, color| el.text_color(color)),
            )
            .child(provider_details(
                provider,
                status,
                model_label,
                selected,
                theme,
            ))
            .child(actions)
            .into_any_element()
    }

    /// The row's ••• trigger and, while open, its menu.
    fn provider_more_button(
        &mut self,
        provider: &PiProviderInfo,
        index: usize,
        busy: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let menu_provider = provider.clone();
        let menu_id = provider.id.clone();
        let menu_open = self
            .menu
            .get()
            .is_some_and(|m| m.provider.id == provider.id);
        let mut more = button(
            theme,
            ("provider-more", index),
            "•••",
            ButtonStyle::Ghost,
            !busy,
        )
        .w(px(32.0))
        .relative()
        .px_0()
        .text_size(px(10.0))
        .aria_label(format!("More actions for {}", provider.id))
        .when(menu_open, |el| el.bg(theme.element_active))
        .tooltip(|_, cx| cx.new(|_| Hint("More actions".into())).into())
        .when(!busy, |el| {
            el.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |page, _, _, _| {
                    page.menu
                        .note_trigger_press_matching(|menu| menu.provider.id == menu_id);
                }),
            )
            .on_click(cx.listener(move |page, _, window, cx| {
                if page.menu.take_press_was_open() {
                    page.close_menu(window, cx);
                } else {
                    page.add_menu = popover::Popup::default();
                    page.menu.open(ProviderMenu {
                        provider: menu_provider.clone(),
                        active: if menu_provider.credential_saved
                            || is_oauth(&menu_provider)
                            || is_claude_cli(&menu_provider)
                        {
                            0
                        } else {
                            2
                        },
                    });
                    page.menu_focus.focus(window, cx);
                }
                cx.notify();
            }))
        });
        if menu_open {
            more = more.child(self.render_menu(theme, cx));
        }
        more
    }

    pub(super) fn render_empty(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        widgets::section_card(theme).mt(px(16.0)).px(px(32.0)).py(px(48.0))
            .items_center().gap(px(12.0))
            .child(div().size(px(56.0)).rounded(px(16.0)).bg(theme.accent.opacity(0.07))
                .border_1().border_color(theme.accent.opacity(0.12)).flex().items_center().justify_center()
                .child(provider_icon(icons::GLOBAL, 26.0, theme.accent)))
            .child(div().mt(px(4.0)).text_size(px(16.0)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme.text)
                .child("Connect your first provider"))
            .child(caption(theme, "Bring your own API key to use models from NewAPI\nor another OpenAI-compatible service.")
                .text_center().max_w(px(340.0)))
            .child(self.render_add_trigger(
                theme,
                "provider-empty-add",
                self.busy.is_none() && self.target.read(cx).can_write(cx),
                false,
                cx,
            )
            .mt(px(8.0))
            .h(px(36.0)))
            .into_any_element()
    }
}

/// The provider's status pill (with a dot), or "Refreshing…".
fn provider_status_pill(provider: &PiProviderInfo, refreshing: bool, theme: &Theme) -> gpui::Div {
    let color = status_color(theme, &provider.state);
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.0))
        .rounded_full()
        .px(px(8.0))
        .py(px(3.0))
        .bg(color.opacity(0.08))
        .text_size(px(11.0))
        .text_color(color)
        .child(div().size(px(5.0)).rounded_full().bg(color))
        .child(SharedString::from(if refreshing {
            "Refreshing…"
        } else {
            status_label(provider)
        }))
}

/// A provider row's middle column: title, status and "In use", base URL,
/// kind · models · checked, and any problem message.
fn provider_details(
    provider: PiProviderInfo,
    status: gpui::Div,
    model_label: String,
    selected: Option<&str>,
    theme: &Theme,
) -> gpui::Div {
    let oauth = is_oauth(&provider);
    let claude_cli = is_claude_cli(&provider);
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.0))
                .child(widgets::row_title(theme, provider_title(&provider)).text_size(px(14.0)))
                .child(status)
                .when_some(selected, |el, _| el.child(widgets::badge(theme, "In use"))),
        )
        .when(!oauth && !claude_cli, |el| {
            el.child(caption(theme, provider.base_url.clone()).truncate())
        })
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.0))
                .child(
                    caption(
                        theme,
                        if claude_cli {
                            "Claude Code CLI"
                        } else if oauth {
                            "ChatGPT subscription"
                        } else {
                            "OpenAI-compatible"
                        },
                    )
                    .text_size(px(11.5)),
                )
                .when(claude_cli && !provider.base_url.is_empty(), |el| {
                    el.child(caption(theme, "·")).child(
                        caption(theme, provider.base_url.clone())
                            .text_size(px(11.5))
                            .truncate(),
                    )
                })
                .when(!claude_cli, |el| {
                    el.child(caption(theme, "·"))
                        .child(caption(theme, model_label).text_size(px(11.5)))
                        .child(caption(theme, "·"))
                        .child(
                            caption(
                                theme,
                                checked_label(
                                    provider.checked_at,
                                    chrono::Utc::now().timestamp_millis(),
                                ),
                            )
                            .text_size(px(11.5)),
                        )
                }),
        )
        .when_some(provider.message, |el, message| {
            el.child(
                div()
                    .mt(px(4.0))
                    .flex()
                    .items_start()
                    .gap(px(6.0))
                    .child(provider_icon(icons::DANGER_TRIANGLE, 14.0, theme.danger))
                    .child(caption(theme, message).text_color(theme.danger_muted)),
            )
        })
}

/// A provider row's buttons: Manage / Install / Sign in / Connect, then •••.
fn provider_actions(
    provider: &PiProviderInfo,
    index: usize,
    busy: bool,
    more: gpui::Stateful<gpui::Div>,
    theme: &Theme,
    cx: &mut Context<ProvidersPage>,
) -> gpui::Div {
    let oauth = is_oauth(provider);
    let claude_cli = is_claude_cli(provider);
    let edit = provider.clone();
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.0))
        .when(claude_cli, |el| {
            el.child(
                button(
                    theme,
                    ("provider-claude-manage", index),
                    "Manage",
                    ButtonStyle::Secondary,
                    !busy,
                )
                .when(!busy, |el| {
                    el.on_click(
                        cx.listener(|page, _, window, cx| page.open_claude_dialog(window, cx)),
                    )
                }),
            )
        })
        .when(claude_cli && !provider.credential_saved, |el| {
            el.child(
                button(
                    theme,
                    ("provider-manage", index),
                    "Install Claude Code",
                    ButtonStyle::Secondary,
                    true,
                )
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.open_url(CLAUDE_CODE_INSTALL);
                })),
            )
        })
        .when(
            !(claude_cli || (oauth && provider.credential_saved)),
            |el| {
                el.child(
                    button(
                        theme,
                        ("provider-manage", index),
                        if oauth {
                            "Sign in"
                        } else if provider.credential_saved {
                            "Manage"
                        } else {
                            "Connect"
                        },
                        ButtonStyle::Secondary,
                        !busy,
                    )
                    .when(!busy, |el| {
                        el.on_click(cx.listener(move |page, _, _, cx| {
                            if is_oauth(&edit) {
                                page.start_oauth(&edit.id, cx);
                            } else {
                                page.edit(Some(edit.clone()), cx);
                            }
                        }))
                    }),
                )
            },
        )
        .child(more)
}
