//! A project card's context menu and its overlays: pin, rename, delete.

use super::*;

impl Shell {
    // ---- space context menu / rename / delete overlays ----

    pub(in crate::shell) fn close_space_menu(&mut self, cx: &mut Context<Self>) {
        if self.space_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.space_menu);
            cx.notify();
        }
    }

    /// Pin/unpin a project (synced): pinned projects lead the sidebar.
    pub(in crate::shell) fn set_space_pinned(
        &mut self,
        space_id: String,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_space_menu(cx);
        self.mutate(
            serde_json::json!({ "op": "setSpacePinned", "spaceId": space_id, "pinned": pinned }),
            cx,
        );
        cx.notify();
    }

    pub(in crate::shell) fn open_rename_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.close_space_menu(cx);
        let current = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|s| s.display_name().to_string())
            .unwrap_or_default();
        let input = cx.new(|cx| TextInput::new("Project name", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted) {
                this.submit_rename_space(cx);
            }
        });
        self.rename_space_dialog = Some(RenameSpaceDialog {
            space_id,
            input,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    pub(in crate::shell) fn submit_rename_space(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_space_dialog.take() else {
            return;
        };
        let name = dialog.input.read(cx).text().trim().to_string();
        if !name.is_empty() {
            self.mutate(
                serde_json::json!({ "op": "renameSpace", "spaceId": dialog.space_id, "name": name }),
                cx,
            );
        }
        cx.notify();
    }

    pub(in crate::shell) fn delete_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.delete_space_confirm = None;
        self.mutate(
            serde_json::json!({ "op": "deleteSpace", "spaceId": space_id }),
            cx,
        );
        cx.notify();
    }

    /// Space context menu + rename dialog + delete confirm (appended to the
    /// shell's overlay list).
    pub(in crate::shell) fn render_space_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let mut overlays: Vec<AnyElement> = Vec::new();
        if let Some(dialog) = self.render_quick_chat_dialog(viewport, window, &theme, cx) {
            overlays.push(dialog);
        }
        if let Some(menu) = self.render_sidebar_view_menu(&theme, cx) {
            overlays.push(menu);
        }
        if let Some(menu) = self.render_space_style_menu(&theme, cx) {
            overlays.push(menu);
        }

        if let Some(overlay) = self.render_space_menu(&theme, cx) {
            overlays.push(overlay);
        }

        if let Some(overlay) = self.render_rename_space_dialog(viewport, &theme, window, cx) {
            overlays.push(overlay);
        }

        if let Some(overlay) = self.render_delete_space_dialog(viewport, &theme, cx) {
            overlays.push(overlay);
        }

        overlays
    }

    /// A project row's context menu: pin, rename, open in (or return from)
    /// its own window, remove.
    fn render_space_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (space_id, position) = self.space_menu.get().cloned()?;
        let closing = self.space_menu.closing_since();
        let rename_id = space_id.clone();
        let delete_id = space_id.clone();
        let pin_id = space_id.clone();
        let window_id = space_id.clone();
        let project_window = self.is_project_window();
        let pinned = self
            .state
            .read(cx)
            .space_row(&space_id)
            .is_some_and(|s| s.pinned);
        // Wide enough for the longest label ("Return to Main Window")
        // on one line.
        let menu = popover::popover_card(theme)
            .w(px(210.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_space_menu(cx);
            }))
            .flex()
            .flex_col()
            .child(
                popover::menu_row(theme, false, format!("space-menu-pin-{space_id}"))
                    .id("space-menu-pin")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_space_pinned(pin_id.clone(), !pinned, cx)
                    }))
                    .child(icon(icons::PIN).size(px(16.0)).text_color(theme.text_muted))
                    .child(SharedString::from(if pinned { "Unpin" } else { "Pin" })),
            )
            .child(
                popover::menu_row(theme, false, format!("space-menu-rename-{space_id}"))
                    .id("space-menu-rename")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_rename_space(rename_id.clone(), cx)
                    }))
                    .child(icon(icons::PEN).size(px(16.0)).text_color(theme.text_muted))
                    .child(SharedString::from("Rename…")),
            )
            // The main window pops the project out; its own window
            // hands it back (same as closing the window).
            .child(
                popover::menu_row(theme, false, format!("space-menu-window-{space_id}"))
                    .id("space-menu-window")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if project_window {
                            this.close_this_window(cx);
                        } else {
                            this.open_project_window(window_id.clone(), cx);
                        }
                    }))
                    .child(
                        icon(icons::WINDOW_FRAME)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    )
                    .child(div().whitespace_nowrap().child(SharedString::from(
                        if project_window {
                            "Return to Main Window"
                        } else {
                            "Open in New Window"
                        },
                    ))),
            )
            .child(popover::menu_separator())
            .child(
                popover::menu_row(theme, false, format!("space-menu-delete-{space_id}"))
                    .id("space-menu-delete")
                    .text_color(theme.danger)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_space_menu(cx);
                        this.delete_space_confirm = Some(delete_id.clone());
                        cx.notify();
                    }))
                    .child(
                        icon(icons::TRASH_BIN_MINIMALISTIC)
                            .size(px(16.0))
                            .text_color(theme.danger),
                    )
                    .child(SharedString::from("Remove…")),
            )
            .into_any_element();
        Some(popover::menu_at(
            "space-context-menu",
            position,
            menu,
            closing,
        ))
    }

    /// The rename-project dialog.
    fn render_rename_space_dialog(
        &mut self,
        viewport: gpui::Size<Pixels>,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.rename_space_dialog.as_mut()?;
        if std::mem::take(&mut dialog.focus_pending) {
            window.focus(&dialog.input.focus_handle(cx), cx);
        }
        let input = dialog.input.clone();
        let card = popover::dialog_card(theme)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.rename_space_dialog = None;
                    cx.notify();
                }
            }))
            .child(popover::dialog_title(theme, "Rename project"))
            .child(
                div()
                    .mt(px(12.0))
                    .child(popover::dialog_field(input.into_any_element())),
            )
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(theme, "Cancel", "rename-space-cancel")
                            .id("rename-space-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.rename_space_dialog = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_primary(theme, "Rename")
                            .id("rename-space-save")
                            .on_click(cx.listener(|this, _, _, cx| this.submit_rename_space(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal("rename-space-dialog", viewport, card))
    }

    /// Confirming a project removal, naming how many sessions go with it.
    fn render_delete_space_dialog(
        &self,
        viewport: gpui::Size<Pixels>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let space_id = self.delete_space_confirm.clone()?;
        let (name, device, count) = {
            let state = self.state.read(cx);
            let space = state.space_row(&space_id);
            (
                space
                    .map(|s| s.display_name().to_string())
                    .unwrap_or_else(|| "this project".into()),
                space
                    .and_then(|s| state.device_name(&s.device_id))
                    .unwrap_or("its device")
                    .to_string(),
                state.chats_in_space(&space_id).len(),
            )
        };
        let copy = if count == 1 {
            format!(
                "Removing “{name}” permanently deletes its 1 session on {device}. This can’t be undone."
            )
        } else {
            format!(
                "Removing “{name}” permanently deletes its {count} sessions on {device}. This can’t be undone."
            )
        };
        let card = popover::dialog_card(theme)
            .child(popover::dialog_title(theme, "Remove project?"))
            .child(div().mt(px(6.0)).child(popover::dialog_body(theme, copy)))
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(theme, "Cancel", "delete-space-cancel")
                            .id("delete-space-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.delete_space_confirm = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_danger(theme, "Remove")
                            .id("delete-space-confirm")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_space(space_id.clone(), cx)
                            })),
                    ),
            )
            .into_any_element();
        Some(popover::modal("delete-space-dialog", viewport, card))
    }
}
