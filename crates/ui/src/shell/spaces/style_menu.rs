//! A project card's icon and colour menu.

use super::*;

impl Shell {
    fn close_space_style_menu(&mut self, cx: &mut Context<Self>) {
        if self.space_style_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.space_style_menu);
            cx.notify();
        }
    }

    /// Write the project's glyph/colour keys (synced). Both keys are sent
    /// every time so one pick never clears the other.
    fn set_space_appearance(
        &mut self,
        space_id: String,
        icon: Option<String>,
        color: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.mutate(
            serde_json::json!({
                "op": "setSpaceAppearance",
                "spaceId": space_id,
                "icon": icon,
                "color": color,
            }),
            cx,
        );
        cx.notify();
    }

    /// The glyph/colour picker: a grid of glyphs, a row of colour swatches
    /// (plus "none"), and a Reset row. Picks apply immediately and keep the
    /// menu open so several tries in a row are one gesture.
    pub(super) fn render_space_style_menu(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (space_id, position) = self.space_style_menu.get().cloned()?;
        let closing = self.space_style_menu.closing_since();
        let (icon_key, color_key) = {
            let state = self.state.read(cx);
            let space = state.space_row(&space_id);
            (
                space.and_then(|s| s.icon.clone()),
                space.and_then(|s| s.color.clone()),
            )
        };
        let tint = crate::appearance::space_style::space_color(color_key.as_deref(), theme)
            .unwrap_or(theme.text);
        let glyphs =
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(2.0))
                .px(px(4.0))
                .children(crate::appearance::space_style::SPACE_ICONS.iter().map(
                    |(key, asset)| {
                        let on = icon_key.as_deref() == Some(*key)
                            || (icon_key.is_none()
                                && *key == crate::appearance::space_style::SPACE_ICONS[0].0);
                        let pick_space = space_id.clone();
                        let pick_color = color_key.clone();
                        let pick_icon = (*key != crate::appearance::space_style::SPACE_ICONS[0].0)
                            .then(|| (*key).to_string());
                        div()
                            .id(SharedString::from(format!("space-style-icon-{key}")))
                            .size(px(28.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .when(on, |el| {
                                el.bg(crate::kit::theme::card_selected_bg())
                                    .shadow(crate::kit::theme::card_selected_shadows())
                            })
                            .when(!on, |el| el.hover(|s| s.bg(theme.element_hover)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_space_appearance(
                                    pick_space.clone(),
                                    pick_icon.clone(),
                                    pick_color.clone(),
                                    cx,
                                )
                            }))
                            .child(icon(asset).size(px(15.0)).text_color(tint))
                    },
                ));
        let none_on = color_key.is_none();
        let none_space = space_id.clone();
        let none_icon = icon_key.clone();
        let colors = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .py(px(4.0))
            .child(
                div()
                    .id("space-style-color-none")
                    .size(px(18.0))
                    .rounded_full()
                    .border_1()
                    .border_color(theme.text_muted.opacity(0.5))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(none_on, |el| {
                        el.shadow(crate::kit::theme::card_selected_shadows())
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_space_appearance(none_space.clone(), none_icon.clone(), None, cx)
                    }))
                    .child(
                        div()
                            .w(px(10.0))
                            .h(px(1.0))
                            .bg(theme.text_muted.opacity(0.6)),
                    ),
            )
            .children(
                crate::appearance::space_style::SPACE_COLORS
                    .iter()
                    .map(|(key, hue)| {
                        let on = color_key.as_deref() == Some(*key);
                        let color = crate::appearance::space_style::swatch(*hue, theme);
                        let pick_space = space_id.clone();
                        let pick_icon = icon_key.clone();
                        let pick_color = Some((*key).to_string());
                        div()
                            .id(SharedString::from(format!("space-style-color-{key}")))
                            .size(px(18.0))
                            .rounded_full()
                            .bg(color)
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(on, |el| {
                                el.border_2().border_color(theme.text).child(
                                    icon(icons::CHECK).size(px(10.0)).text_color(theme.on_solid),
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_space_appearance(
                                    pick_space.clone(),
                                    pick_icon.clone(),
                                    pick_color.clone(),
                                    cx,
                                )
                            }))
                    }),
            );
        let reset_space = space_id.clone();
        let menu = popover::popover_card(theme)
            .w(px(232.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_space_style_menu(cx);
            }))
            .flex()
            .flex_col()
            .child(popover::menu_heading(theme, "Icon"))
            .child(glyphs)
            .child(popover::menu_separator())
            .child(popover::menu_heading(theme, "Color"))
            .child(colors)
            .child(popover::menu_separator())
            .child(
                popover::menu_row(theme, false, "space-style-reset")
                    .id("space-style-reset")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_space_appearance(reset_space.clone(), None, None, cx);
                        this.close_space_style_menu(cx);
                    }))
                    .child(
                        icon(icons::RESTART)
                            .size(px(15.0))
                            .text_color(theme.text_muted),
                    )
                    .child(SharedString::from("Reset to default")),
            )
            .into_any_element();
        Some(popover::menu_at(
            "space-style-menu",
            position,
            menu,
            closing,
        ))
    }
}
