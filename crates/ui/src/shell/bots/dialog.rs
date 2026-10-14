//! The bot dialog: create a bot, or edit one. It holds the whole definition
//! (name, avatar, host, what it can see, instructions) and writes it in one
//! go: `CreateBot` on the chosen host, or a `Mutate updateBot` from here.

use super::*;

/// Which projects the bot is granted (the dialog's three-way pick).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    None,
    All,
    Selected,
}

pub(in crate::shell) struct BotDialog {
    /// `None` creates a bot; `Some` edits that bot.
    bot_id: Option<String>,
    name: Entity<TextInput>,
    instructions: Entity<TextInput>,
    icon: Option<String>,
    color: Option<String>,
    /// The host to create on; fixed (and only shown) when editing.
    host: Option<String>,
    grant: Grant,
    selected: Vec<String>,
    /// Kept as they were: the dialog does not edit single-session overrides.
    include_sessions: Vec<String>,
    exclude_sessions: Vec<String>,
    include_archived: bool,
    transcripts: bool,
    error: Option<SharedString>,
    busy: bool,
    focus_pending: bool,
    projects_scroll: gpui::ScrollHandle,
    _events: Vec<Subscription>,
}

impl BotDialog {
    fn scope(&self) -> BotScope {
        BotScope {
            projects: match self.grant {
                Grant::None => ProjectGrant::None,
                Grant::All => ProjectGrant::All,
                Grant::Selected => ProjectGrant::Selected {
                    space_ids: self.selected.clone(),
                },
            },
            include_sessions: self.include_sessions.clone(),
            exclude_sessions: self.exclude_sessions.clone(),
            include_archived: self.include_archived,
            transcripts: self.transcripts,
        }
    }
}

impl Shell {
    /// Devices that can host a bot, local first: `(id, name, can host)`.
    fn bot_hosts(&self, cx: &App) -> Vec<(String, String, bool)> {
        let now = Utc::now();
        let state = self.state.read(cx);
        let local = state.local_device_id.clone();
        let mut devices: Vec<_> = state
            .devices
            .iter()
            .filter(|d| !matches!(d.platform.as_str(), "ios" | "android"))
            .map(|d| {
                let ok = can_host(d, local.as_deref(), state.device_online(&d.id, now));
                (d.id.clone(), d.name.clone(), ok)
            })
            .collect();
        devices.sort_by_key(|(id, name, _)| {
            (local.as_deref() != Some(id.as_str()), name.to_lowercase())
        });
        devices
    }

    pub(in crate::shell) fn open_bot_dialog(
        &mut self,
        bot_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let existing = bot_id.as_deref().and_then(|id| self.bot_row(id, cx));
        if bot_id.is_some() && existing.is_none() {
            return;
        }
        let name = cx.new(|cx| TextInput::new("Bot name", cx));
        let instructions =
            cx.new(|cx| TextInput::new("Anything it should always keep in mind (optional)", cx));
        let (grant, selected) = match existing.as_ref().map(|b| &b.scope.projects) {
            Some(ProjectGrant::All) => (Grant::All, Vec::new()),
            Some(ProjectGrant::Selected { space_ids }) => (Grant::Selected, space_ids.clone()),
            Some(ProjectGrant::None) => (Grant::None, Vec::new()),
            // A new bot starts on "choose", with nothing chosen yet.
            None => (Grant::Selected, Vec::new()),
        };
        if let Some(bot) = &existing {
            name.update(cx, |input, cx| input.set_text(bot.name.clone(), cx));
            let text = bot.instructions.clone().unwrap_or_default();
            instructions.update(cx, |input, cx| input.set_text(text, cx));
        }
        let events = vec![cx.subscribe(&name, |this: &mut Shell, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted) {
                this.submit_bot_dialog(cx);
            }
        })];
        let palette = crate::appearance::space_style::SPACE_COLORS;
        let fresh_color = palette[self.state.read(cx).bots.len() % palette.len()].0;
        let host = match &existing {
            Some(bot) => Some(bot.host_device_id.clone()),
            None => self
                .bot_hosts(cx)
                .into_iter()
                .find(|(_, _, ok)| *ok)
                .map(|(id, _, _)| id),
        };
        let scope = existing
            .as_ref()
            .map(|b| b.scope.clone())
            .unwrap_or_default();
        self.dialogs.bot = Some(BotDialog {
            bot_id,
            name,
            instructions,
            icon: existing.as_ref().and_then(|b| b.icon.clone()),
            color: match &existing {
                Some(bot) => bot.color.clone(),
                None => Some(fresh_color.to_string()),
            },
            host,
            grant,
            selected,
            include_sessions: scope.include_sessions,
            exclude_sessions: scope.exclude_sessions,
            include_archived: scope.include_archived,
            transcripts: scope.transcripts,
            error: None,
            busy: false,
            focus_pending: true,
            projects_scroll: gpui::ScrollHandle::new(),
            _events: events,
        });
        cx.notify();
    }

    fn close_bot_dialog(&mut self, cx: &mut Context<Self>) {
        self.dialogs.bot = None;
        self.bot_task = None;
        cx.notify();
    }

    fn submit_bot_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialogs.bot.as_ref() else {
            return;
        };
        if dialog.busy {
            return;
        }
        let name = dialog.name.read(cx).text().trim().to_string();
        let instructions = dialog.instructions.read(cx).text().trim().to_string();
        let scope = dialog.scope();
        let fail = |this: &mut Shell, message: String, cx: &mut Context<Shell>| {
            if let Some(dialog) = this.dialogs.bot.as_mut() {
                dialog.error = Some(message.into());
                dialog.busy = false;
            }
            cx.notify();
        };
        if let Err(message) = cypher_proto::validate_name(&name) {
            return fail(self, message, cx);
        }
        let mut body = serde_json::json!({
            "name": name,
            "icon": dialog.icon,
            "color": dialog.color,
            "instructions": (!instructions.is_empty()).then_some(instructions),
            "scope": scope,
        });
        if let Some(bot_id) = dialog.bot_id.clone() {
            body["op"] = "updateBot".into();
            body["botId"] = bot_id.into();
            self.mutate(body, cx);
            return self.close_bot_dialog(cx);
        }
        let Some(host) = dialog.host.clone() else {
            return fail(self, "Pick the device the bot runs on.".into(), cx);
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return fail(self, "Engine not connected".into(), cx);
        };
        body["targetDeviceId"] = host.into();
        if let Some(dialog) = self.dialogs.bot.as_mut() {
            dialog.busy = true;
            dialog.error = None;
        }
        cx.notify();
        self.bot_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::CREATE_BOT, body).await;
            this.update(cx, |this, cx| {
                match result.map_err(|err| err.to_string()).and_then(|value| {
                    serde_json::from_value::<Bot>(value).map_err(|e| e.to_string())
                }) {
                    Ok(bot) => {
                        this.close_bot_dialog(cx);
                        this.expect_chat(&bot.chat_id);
                        this.open_chat(bot.chat_id, cx);
                    }
                    Err(message) => fail(this, message, cx),
                }
            })
            .ok();
        }));
    }

    pub(super) fn render_bot_dialog(
        &mut self,
        viewport: gpui::Size<Pixels>,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let hosts = self.bot_hosts(cx);
        let (projects, preview) = {
            let state = self.state.read(cx);
            let projects: Vec<(String, String, String)> = state
                .spaces_sorted()
                .into_iter()
                .map(|s| {
                    let device = state
                        .devices
                        .iter()
                        .find(|d| d.id == s.device_id)
                        .map_or_else(String::new, |d| d.name.clone());
                    (s.id.clone(), s.display_name().to_string(), device)
                })
                .collect();
            let preview = self.dialogs.bot.as_ref().map(|dialog| {
                let scope = dialog.scope();
                let visible = scope.visible_chats(&state.chats);
                let sessions = visible.iter().filter(|c| !c.is_child()).count();
                let spaces = state
                    .spaces
                    .iter()
                    .filter(|s| scope.sees_space(&s.id))
                    .count();
                (sessions, spaces)
            });
            (projects, preview)
        };
        let dialog = self.dialogs.bot.as_mut()?;
        if std::mem::take(&mut dialog.focus_pending) {
            window.focus(&dialog.name.focus_handle(cx), cx);
        }
        let editing = dialog.bot_id.is_some();
        let name_text = dialog.name.read(cx).text().to_string();
        let avatar = crate::appearance::bot_style::bot_avatar(
            if name_text.trim().is_empty() {
                "Bot"
            } else {
                &name_text
            },
            dialog.icon.as_deref(),
            dialog.color.as_deref(),
            44.0,
            theme,
        );

        let heading = |label: &str| {
            div()
                .mt(px(14.0))
                .mb(px(6.0))
                .text_size(px(11.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text_muted)
                .child(SharedString::from(label.to_string()))
        };

        // Avatar: the initial, then the glyphs; then the colours.
        let tint = crate::appearance::space_style::space_color(dialog.color.as_deref(), theme)
            .unwrap_or(theme.text);
        let glyph_cell = |id: String, on: bool| {
            div()
                .id(SharedString::from(id))
                .size(px(26.0))
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
        };
        let mut glyphs = div().flex().flex_row().flex_wrap().gap(px(2.0)).child(
            glyph_cell("bot-icon-initial".into(), dialog.icon.is_none())
                .text_size(px(12.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(tint)
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(d) = this.dialogs.bot.as_mut() {
                        d.icon = None;
                    }
                    cx.notify();
                }))
                .child(SharedString::from(crate::appearance::bot_style::initial(
                    if name_text.trim().is_empty() {
                        "Bot"
                    } else {
                        &name_text
                    },
                ))),
        );
        for (key, asset) in crate::appearance::bot_style::BOT_ICONS {
            let on = dialog.icon.as_deref() == Some(*key);
            let pick = (*key).to_string();
            glyphs = glyphs.child(
                glyph_cell(format!("bot-icon-{key}"), on)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(d) = this.dialogs.bot.as_mut() {
                            d.icon = Some(pick.clone());
                        }
                        cx.notify();
                    }))
                    .child(icon(asset).size(px(14.0)).text_color(tint)),
            );
        }
        let swatch = |id: String, on: bool| {
            div()
                .id(SharedString::from(id))
                .size(px(18.0))
                .rounded_full()
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .when(on, |el| el.border_2().border_color(theme.text))
        };
        let mut colors = div()
            .mt(px(6.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .child(
                swatch("bot-color-none".into(), dialog.color.is_none())
                    .border_1()
                    .border_color(theme.text_muted.opacity(0.5))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(d) = this.dialogs.bot.as_mut() {
                            d.color = None;
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(10.0))
                            .h(px(1.0))
                            .bg(theme.text_muted.opacity(0.6)),
                    ),
            );
        for (key, hue) in crate::appearance::space_style::SPACE_COLORS {
            let on = dialog.color.as_deref() == Some(*key);
            let pick = (*key).to_string();
            colors = colors.child(
                swatch(format!("bot-color-{key}"), on)
                    .bg(crate::appearance::space_style::swatch(*hue, theme))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(d) = this.dialogs.bot.as_mut() {
                            d.color = Some(pick.clone());
                        }
                        cx.notify();
                    })),
            );
        }

        // Host: chosen once, at creation.
        let host_section = if editing {
            let host_name = hosts
                .iter()
                .find(|(id, _, _)| Some(id) == dialog.host.as_ref())
                .map_or("an unknown device", |(_, name, _)| name.as_str())
                .to_string();
            div()
                .text_size(px(13.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(format!("Runs on {host_name}")))
                .into_any_element()
        } else if hosts.iter().all(|(_, _, ok)| !ok) {
            popover::dialog_body(theme, "No device can host a bot right now.").into_any_element()
        } else {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0))
                .children(hosts.iter().map(|(id, name, ok)| {
                    let on = dialog.host.as_deref() == Some(id.as_str());
                    let pick = id.clone();
                    let ok = *ok;
                    div()
                        .id(SharedString::from(format!("bot-host-{id}")))
                        .px(px(10.0))
                        .py(px(5.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(if on {
                            theme.text.opacity(0.6)
                        } else {
                            crate::kit::theme::hairline(0.10)
                        })
                        .text_size(px(12.5))
                        .text_color(if ok { theme.text } else { theme.text_faint })
                        .when(on, |el| el.bg(crate::kit::theme::ink(0.06)))
                        .when(ok, |el| {
                            el.cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(d) = this.dialogs.bot.as_mut() {
                                        d.host = Some(pick.clone());
                                    }
                                    cx.notify();
                                }))
                        })
                        .child(SharedString::from(name.clone()))
                }))
                .into_any_element()
        };

        // What it can see.
        let grant_chip = |grant: Grant, label: &str| {
            let on = dialog.grant == grant;
            div()
                .id(SharedString::from(format!("bot-grant-{label}")))
                .flex_1()
                .py(px(5.0))
                .rounded(px(7.0))
                .flex()
                .justify_center()
                .text_size(px(12.5))
                .cursor_pointer()
                .text_color(if on { theme.text } else { theme.text_muted })
                .when(on, |el| {
                    el.bg(crate::kit::theme::card_selected_bg())
                        .shadow(crate::kit::theme::card_selected_shadows())
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(d) = this.dialogs.bot.as_mut() {
                        d.grant = grant;
                    }
                    cx.notify();
                }))
                .child(SharedString::from(label.to_string()))
        };
        let grants = div()
            .p(px(2.0))
            .rounded(px(9.0))
            .bg(crate::kit::theme::ink(0.04))
            .flex()
            .flex_row()
            .gap(px(2.0))
            .child(grant_chip(Grant::Selected, "Choose projects"))
            .child(grant_chip(Grant::All, "All projects"))
            .child(grant_chip(Grant::None, "Nothing"));
        let project_list = (dialog.grant == Grant::Selected).then(|| {
            let rows = projects.iter().map(|(id, name, device)| {
                let on = dialog.selected.iter().any(|s| s == id);
                let pick = id.clone();
                div()
                    .id(SharedString::from(format!("bot-project-{id}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(7.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(d) = this.dialogs.bot.as_mut() {
                            if let Some(ix) = d.selected.iter().position(|s| *s == pick) {
                                d.selected.remove(ix);
                            } else {
                                d.selected.push(pick.clone());
                            }
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(15.0))
                            .flex_none()
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(if on {
                                theme.text
                            } else {
                                theme.text_muted.opacity(0.5)
                            })
                            .when(on, |el| el.bg(theme.text))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(on, |el| {
                                el.child(
                                    icon(icons::CHECK).size(px(11.0)).text_color(theme.on_solid),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.0))
                            .child(SharedString::from(name.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.5))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(device.clone())),
                    )
            });
            div()
                .id("bot-projects")
                .mt(px(6.0))
                .max_h(px(180.0))
                .overflow_y_scroll()
                .track_scroll(&dialog.projects_scroll)
                .flex()
                .flex_col()
                .when(projects.is_empty(), |el| {
                    el.child(popover::dialog_body(theme, "No projects yet."))
                })
                .children(rows)
        });
        let toggle_row = |id: &str, label: &str, on: bool, flip: fn(&mut BotDialog)| {
            div()
                .id(SharedString::from(id.to_string()))
                .mt(px(8.0))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(d) = this.dialogs.bot.as_mut() {
                        flip(d);
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .text_size(px(13.0))
                        .child(SharedString::from(label.to_string())),
                )
                .child(crate::settings::widgets::toggle_switch(theme, on))
        };
        let summary = preview.map(|(sessions, spaces)| {
            div()
                .mt(px(10.0))
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(match (sessions, spaces) {
                    (0, 0) => "It can't see any sessions yet.".to_string(),
                    (sessions, spaces) => format!(
                        "It can see {sessions} session{} in {spaces} project{}.",
                        if sessions == 1 { "" } else { "s" },
                        if spaces == 1 { "" } else { "s" }
                    ),
                }))
        });

        let mut actions = div()
            .mt(px(18.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0));
        if let Some(bot_id) = dialog.bot_id.clone() {
            actions = actions.child(
                popover::btn_ghost(theme, "Delete…", "bot-dialog-delete")
                    .id("bot-dialog-delete")
                    .text_color(theme.danger)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_bot_dialog(cx);
                        this.dialogs.delete_bot = Some(bot_id.clone());
                        cx.notify();
                    })),
            );
        }
        let busy = dialog.busy;
        actions = actions
            .child(div().flex_1())
            .child(
                popover::btn_ghost(theme, "Cancel", "bot-dialog-cancel")
                    .id("bot-dialog-cancel")
                    .on_click(cx.listener(|this, _, _, cx| this.close_bot_dialog(cx))),
            )
            .child(
                popover::btn_primary(
                    theme,
                    match (editing, busy) {
                        (true, _) => "Save",
                        (false, false) => "Create",
                        (false, true) => "Creating…",
                    },
                )
                .id("bot-dialog-submit")
                .when(busy, |el| el.opacity(0.6))
                .on_click(cx.listener(|this, _, _, cx| this.submit_bot_dialog(cx))),
            );

        let card = popover::dialog_card(theme)
            .w(px(440.0))
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.close_bot_dialog(cx);
                }
            }))
            .child(popover::dialog_title(
                theme,
                if editing { "Edit bot" } else { "New bot" },
            ))
            .child(
                div()
                    .mt(px(14.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .child(avatar)
                    .child(div().flex_1().min_w_0().child(popover::dialog_field(
                        dialog.name.clone().into_any_element(),
                    ))),
            )
            .child(heading("Avatar"))
            .child(glyphs)
            .child(colors)
            .child(heading("Runs on"))
            .child(host_section)
            .child(heading("Can see"))
            .child(grants)
            .children(project_list)
            .child(toggle_row(
                "bot-toggle-text",
                "Read message text",
                dialog.transcripts,
                |d| d.transcripts = !d.transcripts,
            ))
            .child(toggle_row(
                "bot-toggle-archived",
                "Include archived sessions",
                dialog.include_archived,
                |d| d.include_archived = !d.include_archived,
            ))
            .children(summary)
            .child(heading("Instructions"))
            .child(popover::dialog_field(
                dialog.instructions.clone().into_any_element(),
            ))
            .when_some(dialog.error.clone(), |el, error| {
                el.child(
                    div()
                        .mt(px(10.0))
                        .text_size(px(12.0))
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .child(actions)
            .into_any_element();
        Some(popover::modal("bot-dialog", viewport, card))
    }
}
