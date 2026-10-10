//! The quick-chat dialog: start a session on a device without a project.

use super::*;

impl Shell {
    pub(in crate::shell) fn open_quick_chat_dialog(&mut self, cx: &mut Context<Self>) {
        self.close_space_menu(cx);
        let active = self
            .quick_chat_devices(cx)
            .iter()
            .position(|(_, online, _)| *online)
            .unwrap_or(0);
        self.quick_chat = Some(QuickChatFlow {
            active,
            focus: cx.focus_handle(),
            focus_pending: true,
            list_scroll: gpui::ScrollHandle::new(),
        });
        cx.notify();
    }

    /// Devices in palette order — this device first, then by name — with
    /// their presence and whether each is this device.
    fn quick_chat_devices(&self, cx: &App) -> Vec<(Device, bool, bool)> {
        let now = Utc::now();
        let state = self.state.read(cx);
        let local = state.local_device_id.clone();
        let mut devices = state.devices.clone();
        devices.sort_by_key(|d| {
            (
                local.as_deref() != Some(d.id.as_str()),
                d.name.to_lowercase(),
                d.id.clone(),
            )
        });
        devices
            .into_iter()
            .map(|d| {
                let online = state.device_online(&d.id, now);
                let is_local = local.as_deref() == Some(d.id.as_str());
                (d, online, is_local)
            })
            .collect()
    }

    fn quick_chat_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.quick_chat = None;
                cx.notify();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = self.quick_chat_devices(cx).len();
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(flow) = self.quick_chat.as_mut() {
                    flow.active = popover::menu_step(Some(flow.active), count, delta).unwrap_or(0);
                    // Row 0 of the scroll container is the section label.
                    flow.list_scroll.scroll_to_item(flow.active + 1);
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.quick_chat.as_ref().map(|f| f.active).unwrap_or(0);
                if let Some((device, true, _)) = self.quick_chat_devices(cx).get(active).cloned() {
                    self.start_quick_chat(device.id, cx);
                }
            }
            _ => {}
        }
    }

    /// The quick-chat palette: the add-project palette's card, header band,
    /// and footer legend, with the devices rail as its only content. Enter
    /// or a click on an online device starts the throwaway session there;
    /// offline devices are listed but inert (the host mints the folder).
    pub(super) fn render_quick_chat_dialog(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (active, focus, list_scroll) = {
            let flow = self.quick_chat.as_mut()?;
            if std::mem::take(&mut flow.focus_pending) {
                window.focus(&flow.focus, cx);
            }
            (flow.active, flow.focus.clone(), flow.list_scroll.clone())
        };
        let rows = self.quick_chat_devices(cx);
        let hairline = crate::kit::theme::hairline(0.06);
        let band = popover::band();
        let key_chip = |theme: &Theme| {
            div()
                .h(px(22.0))
                .px(px(6.0))
                .rounded(px(5.0))
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .bg(crate::kit::theme::ink(0.05))
                .text_size(px(11.0))
                .mono(theme)
                .text_color(theme.text_muted.opacity(0.7))
        };
        let header = div()
            .h(px(46.0))
            .flex_none()
            .pl(px(14.0))
            .pr(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .bg(band)
            .border_b_1()
            .border_color(hairline)
            .child(
                icon(icons::CHAT_ROUND_LINE)
                    .size(px(14.0))
                    .flex_none()
                    .text_color(theme.text_muted.opacity(0.8)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(14.0))
                    .text_color(theme.text)
                    .child(SharedString::from("Quick chat")),
            )
            .child(
                key_chip(theme)
                    .id("quick-chat-esc")
                    .cursor_pointer()
                    .hover(|s| s.bg(crate::kit::theme::ink(0.09)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.quick_chat = None;
                        cx.notify();
                    }))
                    .child(SharedString::from("esc")),
            );
        // FIXED height like the palette body: the list fills and scrolls,
        // so the card never resizes with the device count. The gutters live
        // on the wrapper, outside the scroll viewport.
        let list = div().h(px(240.0)).flex_none().py(px(6.0)).child(
            div()
                .id("quick-chat-devices")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&list_scroll)
                .px(px(8.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .px(px(8.0))
                        .pt(px(2.0))
                        .pb(px(4.0))
                        .text_size(px(11.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text_muted.opacity(0.6))
                        .child(SharedString::from("Run on device")),
                )
                .children(
                    rows.into_iter()
                        .enumerate()
                        .map(|(ix, (dev, online, is_local))| {
                            let is_active = ix == active;
                            let platform_icon = match dev.platform.as_str() {
                                "macos" | "darwin" => icons::LAPTOP,
                                "web" => icons::GLOBAL,
                                "ios" | "android" => icons::SMARTPHONE,
                                _ => icons::MONITOR,
                            };
                            let name: SharedString = dev.name.clone().into();
                            let device_id = dev.id.clone();
                            div()
                                .id(("quick-chat-device", ix))
                                .h(px(28.0))
                                .px(px(8.0))
                                .rounded(px(8.0))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .text_size(px(12.5))
                                .when(online, |el| el.cursor_pointer())
                                .when(!online, |el| el.opacity(0.55))
                                .when(is_active, |el| {
                                    el.bg(crate::kit::theme::card_selected_bg())
                                        .shadow(crate::kit::theme::card_selected_shadows())
                                        .text_color(theme.text)
                                })
                                .when(!is_active, |el| {
                                    el.text_color(theme.text_muted.opacity(0.7))
                                        .hover(|s| s.bg(theme.element_hover))
                                })
                                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                    if let Some(flow) = this.quick_chat.as_mut()
                                        && flow.active != ix
                                    {
                                        flow.active = ix;
                                        cx.notify();
                                    }
                                }))
                                .when(online, |el| {
                                    el.on_click(cx.listener(move |this, _, _, cx| {
                                        this.start_quick_chat(device_id.clone(), cx);
                                    }))
                                })
                                .child(
                                    icon(platform_icon)
                                        .size(px(14.0))
                                        .flex_none()
                                        .text_color(theme.text_muted.opacity(0.8)),
                                )
                                .child(div().flex_1().min_w_0().truncate().child(name))
                                .when(is_local, |el| {
                                    el.child(
                                        div()
                                            .flex_none()
                                            .text_size(px(11.0))
                                            .text_color(theme.text_faint)
                                            .child(SharedString::from("this device")),
                                    )
                                })
                                .child(
                                    div()
                                        .size(px(5.0))
                                        .rounded_full()
                                        .flex_none()
                                        .when(online, |el| {
                                            let emerald = theme.success;
                                            el.bg(emerald.opacity(0.9)).shadow(vec![
                                                gpui::BoxShadow {
                                                    color: emerald.opacity(0.55),
                                                    offset: gpui::point(px(0.0), px(0.0)),
                                                    blur_radius: px(6.0),
                                                    spread_radius: px(0.0),
                                                    inset: false,
                                                },
                                            ])
                                        })
                                        .when(!online, |el| el.bg(crate::kit::theme::ink(0.22))),
                                )
                        }),
                ),
        );
        let footer = div()
            .flex_none()
            .bg(band)
            .border_t_1()
            .border_color(hairline)
            .px(px(12.0))
            .py(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .child(popover::key_hint_pair(
                theme,
                icons::ARROW_UP,
                icons::ARROW_DOWN,
                "Navigate",
            ))
            .child(popover::key_hint(theme, icons::RETURN, "Start"));
        let card =
            div()
                .id("quick-chat-palette")
                .w(px(420.0))
                .rounded(px(14.0))
                .border_1()
                .border_color(crate::kit::theme::hairline(0.10))
                .bg(if theme.is_glass() {
                    theme.glass_overlay()
                } else {
                    theme.surface_overlay
                })
                .shadow_lg()
                .overflow_hidden()
                .flex()
                .flex_col()
                .text_color(theme.text)
                .track_focus(&focus)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    this.quick_chat_key(event, cx)
                }))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.quick_chat = None;
                    cx.notify();
                }))
                .child(header)
                .child(list)
                .child(footer)
                .into_any_element();
        Some(popover::modal_glass(
            "quick-chat-dialog",
            viewport,
            card,
            14.0,
        ))
    }
}
