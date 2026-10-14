//! The bot dialog: create a bot, or edit one. It holds the whole definition
//! (name, avatar, host, what it can see, instructions) and writes it in one
//! go: `CreateBot` on the chosen host, or a `Mutate updateBot` from here.

use super::*;

/// The dialog's inset, and the heights of its glass header and footer.
const PAD: f32 = 22.0;
const HEADER_H: f32 = 78.0;
const FOOTER_H: f32 = 64.0;
/// Backdrop blur under the glass bars.
const GLASS_BLUR: f32 = 20.0;
/// [`popover::dialog_card`]'s rounding, which the glass bars follow.
const CARD_RADIUS: f32 = 16.0;

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

/// One device the dialog offers as a host.
struct BotHost {
    id: String,
    name: String,
    laptop: bool,
    local: bool,
    /// Online, a desktop engine, and new enough (see [`can_host`]).
    ok: bool,
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
    /// Devices that can host a bot, local first.
    fn bot_hosts(&self, cx: &App) -> Vec<BotHost> {
        let now = Utc::now();
        let state = self.state.read(cx);
        let local = state.local_device_id.clone();
        let mut devices: Vec<BotHost> = state
            .devices
            .iter()
            .filter(|d| !matches!(d.platform.as_str(), "ios" | "android"))
            .map(|d| BotHost {
                id: d.id.clone(),
                name: d.name.clone(),
                laptop: matches!(d.platform.as_str(), "macos" | "darwin"),
                local: local.as_deref() == Some(d.id.as_str()),
                ok: can_host(d, local.as_deref(), state.device_online(&d.id, now)),
            })
            .collect();
        devices.sort_by_key(|h| (!h.local, h.name.to_lowercase()));
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
        // A new bot starts on the next robot face and colour, so a few bots
        // in a row tell apart without any picking.
        let count = self.state.read(cx).bots.len();
        let palette = crate::appearance::space_style::SPACE_COLORS;
        let faces = crate::appearance::bot_style::BOT_FACES;
        let fresh_color = palette[count % palette.len()].0;
        let fresh_face = faces[count % faces.len()].0;
        let host = match &existing {
            Some(bot) => Some(bot.host_device_id.clone()),
            None => self
                .bot_hosts(cx)
                .into_iter()
                .find(|host| host.ok)
                .map(|host| host.id),
        };
        let scope = existing
            .as_ref()
            .map(|b| b.scope.clone())
            .unwrap_or_default();
        self.dialogs.bot = Some(BotDialog {
            bot_id,
            name,
            instructions,
            icon: match &existing {
                Some(bot) => bot.icon.clone(),
                None => Some(fresh_face.to_string()),
            },
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
        self.dialogs.bot.as_ref()?;
        let hosts = self.bot_hosts(cx);
        let (projects, access) = {
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
            let access = self.dialogs.bot.as_ref().map(|dialog| {
                let scope = dialog.scope();
                let sessions = scope
                    .visible_chats(&state.chats)
                    .iter()
                    .filter(|c| !c.is_child())
                    .count();
                let spaces = state
                    .spaces
                    .iter()
                    .filter(|s| scope.sees_space(&s.id))
                    .count();
                (sessions, spaces)
            });
            (projects, access)
        };
        let dialog = self.dialogs.bot.as_mut()?;
        if std::mem::take(&mut dialog.focus_pending) {
            window.focus(&dialog.name.focus_handle(cx), cx);
        }
        let editing = dialog.bot_id.is_some();
        let name_text = dialog.name.read(cx).text().trim().to_string();
        let shown_name = if name_text.is_empty() {
            "Bot".to_string()
        } else {
            name_text
        };
        let tint = crate::appearance::space_style::space_color(dialog.color.as_deref(), theme)
            .unwrap_or(theme.text);
        let hairline = crate::kit::theme::hairline(0.08);

        let section = |title: &str, caption: Option<&str>| {
            div()
                .mt(px(18.0))
                .mb(px(8.0))
                .flex()
                .flex_row()
                .items_baseline()
                .justify_between()
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(SharedString::from(title.to_string())),
                )
                .when_some(caption, |el, caption| {
                    el.child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(caption.to_string())),
                    )
                })
        };

        // ── Avatar: robot faces, then symbols and the initial; colours ──────
        let cell = |id: String, on: bool| {
            div()
                .id(SharedString::from(id))
                .size(px(32.0))
                .rounded(px(9.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(if on {
                    tint.opacity(0.16)
                } else {
                    crate::kit::theme::ink(0.0)
                })
                .border_1()
                .border_color(if on {
                    tint.opacity(0.55)
                } else {
                    gpui::transparent_black()
                })
                .when(!on, |el| el.hover(|s| s.bg(theme.element_hover)))
        };
        let pick_icon = |key: Option<&'static str>| {
            move |this: &mut Shell,
                  _: &gpui::ClickEvent,
                  _: &mut Window,
                  cx: &mut Context<Shell>| {
                if let Some(d) = this.dialogs.bot.as_mut() {
                    d.icon = key.map(str::to_string);
                }
                cx.notify();
            }
        };
        let faces = div().flex().flex_row().flex_wrap().gap(px(4.0)).children(
            crate::appearance::bot_style::BOT_FACES
                .iter()
                .map(|(key, asset)| {
                    cell(
                        format!("bot-face-{key}"),
                        dialog.icon.as_deref() == Some(*key),
                    )
                    .on_click(cx.listener(pick_icon(Some(key))))
                    .child(icon(asset).size(px(19.0)).text_color(tint))
                }),
        );
        let symbols = div()
            .mt(px(4.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(4.0))
            .child(
                cell("bot-symbol-initial".into(), dialog.icon.is_none())
                    .text_size(px(13.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(tint)
                    .on_click(cx.listener(pick_icon(None)))
                    .child(SharedString::from(crate::appearance::bot_style::initial(
                        &shown_name,
                    ))),
            )
            .children(
                crate::appearance::bot_style::BOT_SYMBOLS
                    .iter()
                    .map(|(key, asset)| {
                        cell(
                            format!("bot-symbol-{key}"),
                            dialog.icon.as_deref() == Some(*key),
                        )
                        .on_click(cx.listener(pick_icon(Some(key))))
                        .child(icon(asset).size(px(15.0)).text_color(tint))
                    }),
            );
        let swatch = |id: String, on: bool| {
            div()
                .id(SharedString::from(id))
                .size(px(22.0))
                .rounded_full()
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .border_2()
                .border_color(if on {
                    theme.text
                } else {
                    gpui::transparent_black()
                })
        };
        let pick_color = |key: Option<&'static str>| {
            move |this: &mut Shell,
                  _: &gpui::ClickEvent,
                  _: &mut Window,
                  cx: &mut Context<Shell>| {
                if let Some(d) = this.dialogs.bot.as_mut() {
                    d.color = key.map(str::to_string);
                }
                cx.notify();
            }
        };
        let colors = div()
            .mt(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(
                swatch("bot-color-none".into(), dialog.color.is_none())
                    .on_click(cx.listener(pick_color(None)))
                    .child(
                        div()
                            .size(px(16.0))
                            .rounded_full()
                            .bg(crate::kit::theme::ink(0.10)),
                    ),
            )
            .children(
                crate::appearance::space_style::SPACE_COLORS
                    .iter()
                    .map(|(key, hue)| {
                        swatch(
                            format!("bot-color-{key}"),
                            dialog.color.as_deref() == Some(*key),
                        )
                        .on_click(cx.listener(pick_color(Some(key))))
                        .child(
                            div()
                                .size(px(16.0))
                                .rounded_full()
                                .bg(crate::appearance::space_style::swatch(*hue, theme)),
                        )
                    }),
            );

        // ── Host device: chosen once, at creation ───────────────────────────
        let host_chip = |host: &BotHost, on: bool| {
            div()
                .id(SharedString::from(format!("bot-host-{}", host.id)))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(7.0))
                .pl(px(10.0))
                .pr(px(12.0))
                .py(px(7.0))
                .rounded(px(9.0))
                .border_1()
                .border_color(if on {
                    theme.text.opacity(0.5)
                } else {
                    hairline
                })
                .when(on, |el| el.bg(crate::kit::theme::ink(0.05)))
                .text_size(px(12.5))
                .text_color(if host.ok {
                    theme.text
                } else {
                    theme.text_faint
                })
                .child(
                    icon(if host.laptop {
                        icons::LAPTOP
                    } else {
                        icons::MONITOR
                    })
                    .size(px(14.0))
                    .text_color(if host.ok {
                        theme.text_muted
                    } else {
                        theme.text_faint
                    }),
                )
                .child(SharedString::from(host.name.clone()))
                .when(host.local, |el| {
                    el.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from("This device")),
                    )
                })
        };
        let host_body = if editing {
            let host = hosts.iter().find(|h| Some(&h.id) == dialog.host.as_ref());
            match host {
                Some(host) => div()
                    .flex()
                    .child(host_chip(host, false))
                    .into_any_element(),
                None => popover::dialog_body(theme, "Runs on a device that is no longer paired.")
                    .into_any_element(),
            }
        } else if hosts.iter().all(|h| !h.ok) {
            popover::dialog_body(theme, "No device can host a bot right now.").into_any_element()
        } else {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0))
                .children(hosts.iter().map(|host| {
                    let pick = host.id.clone();
                    host_chip(host, dialog.host.as_deref() == Some(host.id.as_str())).when(
                        host.ok,
                        |el| {
                            el.cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(d) = this.dialogs.bot.as_mut() {
                                        d.host = Some(pick.clone());
                                    }
                                    cx.notify();
                                }))
                        },
                    )
                }))
                .into_any_element()
        };

        // ── Permissions ─────────────────────────────────────────────────────
        let grant_option = |grant: Grant, label: &str| {
            let on = dialog.grant == grant;
            div()
                .id(SharedString::from(format!("bot-grant-{label}")))
                .flex_1()
                .py(px(4.0))
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
                    if let Some(d) = this.dialogs.bot.as_mut() {
                        d.grant = grant;
                    }
                    cx.notify();
                }))
                .child(SharedString::from(label.to_string()))
        };
        let permission_row = |title: &str, detail: &str| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme.text)
                        .child(SharedString::from(title.to_string())),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(detail.to_string())),
                )
        };
        let projects_row = div()
            .px(px(12.0))
            .py(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .child(permission_row(
                "Project access",
                "Which projects' sessions it may read.",
            ))
            .child(
                div()
                    .w(px(222.0))
                    .flex_none()
                    .p(px(2.0))
                    .rounded(px(8.0))
                    .bg(crate::kit::theme::ink(0.05))
                    .flex()
                    .flex_row()
                    .gap(px(2.0))
                    .child(grant_option(Grant::Selected, "Selected"))
                    .child(grant_option(Grant::All, "All"))
                    .child(grant_option(Grant::None, "None")),
            );
        let project_list = (dialog.grant == Grant::Selected).then(|| {
            let rows = projects.iter().map(|(id, name, device)| {
                let on = dialog.selected.iter().any(|s| s == id);
                let pick = id.clone();
                div()
                    .id(SharedString::from(format!("bot-project-{id}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(9.0))
                    .px(px(8.0))
                    .py(px(6.0))
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
                            .size(px(16.0))
                            .flex_none()
                            .rounded(px(5.0))
                            .border_1()
                            .border_color(if on {
                                theme.text
                            } else {
                                theme.text_muted.opacity(0.45)
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
                        icon(icons::FOLDER)
                            .size(px(14.0))
                            .text_color(theme.text_muted),
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
                .border_t_1()
                .border_color(hairline)
                .px(px(4.0))
                .py(px(4.0))
                .child(
                    div()
                        .id("bot-projects")
                        .max_h(px(168.0))
                        .overflow_y_scroll()
                        .track_scroll(&dialog.projects_scroll)
                        .flex()
                        .flex_col()
                        .when(projects.is_empty(), |el| {
                            el.child(
                                div()
                                    .px(px(8.0))
                                    .py(px(6.0))
                                    .text_size(px(12.0))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(
                                        "No projects yet. Add one from the sidebar.",
                                    )),
                            )
                        })
                        .children(rows),
                )
        });
        let toggle = |id: &str, title: &str, detail: &str, on: bool, flip: fn(&mut BotDialog)| {
            div()
                .id(SharedString::from(id.to_string()))
                .border_t_1()
                .border_color(hairline)
                .px(px(12.0))
                .py(px(10.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(d) = this.dialogs.bot.as_mut() {
                        flip(d);
                    }
                    cx.notify();
                }))
                .child(permission_row(title, detail))
                .child(crate::settings::widgets::toggle_switch(theme, on))
        };
        let permissions = div()
            .rounded(px(10.0))
            .border_1()
            .border_color(hairline)
            .bg(crate::kit::theme::ink(0.02))
            .flex()
            .flex_col()
            .child(projects_row)
            .children(project_list)
            .child(toggle(
                "bot-toggle-text",
                "Read conversations",
                "Message text, not only titles and status.",
                dialog.transcripts,
                |d| d.transcripts = !d.transcripts,
            ))
            .child(toggle(
                "bot-toggle-archived",
                "Archived sessions",
                "Include sessions you have archived.",
                dialog.include_archived,
                |d| d.include_archived = !d.include_archived,
            ));
        let summary = access.map(|(sessions, spaces)| {
            let text = match (sessions, spaces) {
                (0, 0) => "No access to any sessions yet.".to_string(),
                (sessions, spaces) => format!(
                    "Access to {sessions} session{} in {spaces} project{}. Read-only.",
                    if sessions == 1 { "" } else { "s" },
                    if spaces == 1 { "" } else { "s" }
                ),
            };
            div()
                .mt(px(8.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.5))
                .text_color(theme.text_muted)
                .child(
                    icon(icons::SHIELD_CHECK)
                        .size(px(13.0))
                        .text_color(theme.text_muted),
                )
                .child(SharedString::from(text))
        });

        // ── Footer ──────────────────────────────────────────────────────────
        let busy = dialog.busy;
        let mut footer = div()
            .size_full()
            .px(px(PAD))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0));
        if let Some(bot_id) = dialog.bot_id.clone() {
            footer = footer.child(
                div()
                    .id("bot-dialog-delete")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .text_size(px(13.0))
                    .text_color(theme.danger)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.danger.opacity(0.08)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_bot_dialog(cx);
                        this.dialogs.delete_bot = Some(bot_id.clone());
                        cx.notify();
                    }))
                    .child(
                        icon(icons::TRASH_BIN_MINIMALISTIC)
                            .size(px(14.0))
                            .text_color(theme.danger),
                    )
                    .child(SharedString::from("Delete")),
            );
        }
        footer = footer
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
                        (false, false) => "Create bot",
                        (false, true) => "Creating…",
                    },
                )
                .id("bot-dialog-submit")
                .when(busy, |el| el.opacity(0.6))
                .on_click(cx.listener(|this, _, _, cx| this.submit_bot_dialog(cx))),
            );

        let avatar = crate::appearance::bot_style::bot_avatar(
            &shown_name,
            dialog.icon.as_deref(),
            dialog.color.as_deref(),
            52.0,
            theme,
        );
        // The body scrolls the card's full height, under both glass bars;
        // its padding keeps every row clear of them at either end.
        let body = div()
            .id("bot-dialog-body")
            .max_h(px((f32::from(viewport.height) - 96.0).max(320.0)))
            .overflow_y_scroll()
            .pt(px(HEADER_H - 4.0))
            .pb(px(FOOTER_H + 16.0))
            .px(px(PAD))
            .flex()
            .flex_col()
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(14.0))
                    .child(avatar)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from("Name")),
                            )
                            .child(popover::dialog_field(
                                dialog.name.clone().into_any_element(),
                            )),
                    ),
            )
            .child(section("Avatar", None))
            .child(faces)
            .child(symbols)
            .child(colors)
            .child(section(
                "Host device",
                Some(if editing {
                    "Where it runs"
                } else {
                    "Where it runs. Fixed once created."
                }),
            ))
            .child(host_body)
            .child(section("Permissions", None))
            .child(permissions)
            .children(summary)
            .child(section("Instructions", Some("Optional")))
            .child(popover::dialog_field(
                dialog.instructions.clone().into_any_element(),
            ))
            .when_some(dialog.error.clone(), |el, error| {
                el.child(
                    div()
                        .mt(px(12.0))
                        .px(px(10.0))
                        .py(px(8.0))
                        .rounded(px(8.0))
                        .bg(theme.danger.opacity(0.08))
                        .text_size(px(12.0))
                        .text_color(theme.danger)
                        .child(error),
                )
            });

        // Header and footer: glass over the body, so rows scrolling beneath
        // read through them, blurred. A thin tint of the card's own surface
        // and a lighter blur than a menu's keep the rows visible under it.
        let glass = theme.surface_dialog.opacity(0.4);
        let header = div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(HEADER_H))
            .child(crate::kit::frost::frosted_bar(
                gpui::Corners {
                    top_left: px(CARD_RADIUS),
                    top_right: px(CARD_RADIUS),
                    bottom_left: px(0.0),
                    bottom_right: px(0.0),
                },
                GLASS_BLUR,
                div()
                    .size_full()
                    .occlude()
                    .rounded_t(px(CARD_RADIUS))
                    .bg(glass)
                    .border_b_1()
                    .border_color(hairline)
                    .px(px(PAD))
                    .pt(px(20.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(popover::dialog_title(
                        theme,
                        if editing { "Bot settings" } else { "New bot" },
                    ))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(theme.text_muted)
                            .truncate()
                            .child(SharedString::from(
                                "Answers questions using only the sessions you allow.",
                            )),
                    ),
            ));
        let footer = div()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .h(px(FOOTER_H))
            .child(crate::kit::frost::frosted_bar(
                gpui::Corners {
                    top_left: px(0.0),
                    top_right: px(0.0),
                    bottom_left: px(CARD_RADIUS),
                    bottom_right: px(CARD_RADIUS),
                },
                GLASS_BLUR,
                div()
                    .size_full()
                    .occlude()
                    .rounded_b(px(CARD_RADIUS))
                    .bg(glass)
                    .border_t_1()
                    .border_color(hairline)
                    .child(footer),
            ));
        let card = popover::dialog_card(theme)
            .w(px(480.0))
            .p(px(0.0))
            .relative()
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.close_bot_dialog(cx);
                }
            }))
            .child(body)
            .child(header)
            .child(footer)
            .into_any_element();
        Some(popover::modal("bot-dialog", viewport, card))
    }
}
