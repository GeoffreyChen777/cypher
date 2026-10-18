//! The bot palette: create a bot, or edit one. It holds the whole definition
//! (name, avatar, host, permissions, instructions) and writes it in one go:
//! `CreateBot` on the chosen host, or a `Mutate updateBot` from here.
//!
//! It is drawn like the Quick Chat and Add project palettes: a glass card
//! with a header band and a key-hint footer, and the host device as a
//! devices rail on the right. The form scrolls under the header and footer,
//! which are frosted so rows read through them.

use super::*;
use crate::kit::theme::MonoStyled as _;

/// The form's inset, and the heights of the glass header and footer bands.
const PAD: f32 = 18.0;
const HEADER_H: f32 = 46.0;
const FOOTER_H: f32 = 48.0;
/// Backdrop blur under the glass bands.
const GLASS_BLUR: f32 = 20.0;
/// The palette card's rounding (the Quick Chat palette's), which the glass
/// bands follow.
const CARD_RADIUS: f32 = 14.0;
const CARD_W: f32 = 760.0;
const RAIL_W: f32 = 228.0;

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
    /// What it may do beyond reading (§4.2).
    capabilities: cypher_proto::BotCapabilities,
    error: Option<SharedString>,
    busy: bool,
    focus_pending: bool,
    _events: Vec<Subscription>,
}

/// One device the dialog offers as a host.
struct BotHost {
    id: String,
    name: String,
    laptop: bool,
    local: bool,
    online: bool,
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
                online: state.device_online(&d.id, now),
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
        // Enter in either field creates (or saves), like the palette footer
        // says.
        let submit = |this: &mut Shell,
                      _: Entity<TextInput>,
                      event: &TextInputEvent,
                      cx: &mut Context<Shell>| {
            if matches!(event, TextInputEvent::Submitted) {
                this.submit_bot_dialog(cx);
            }
        };
        let events = vec![
            cx.subscribe(&name, submit),
            cx.subscribe(&instructions, submit),
        ];
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
            // A new bot may work in its local projects; it coordinates only
            // once the owner turns that on.
            capabilities: existing
                .as_ref()
                .map_or_else(cypher_proto::BotCapabilities::new_bot, |b| b.capabilities),
            error: None,
            busy: false,
            focus_pending: true,
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
            "capabilities": dialog.capabilities,
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
                        this.show_new_bot(bot, cx);
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
        let hairline = crate::kit::theme::hairline(0.06);
        let band = popover::band();
        // The palette's small section label ("Run on device", "Devices").
        let label = |text: &str| {
            div()
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text_muted.opacity(0.6))
                .child(SharedString::from(text.to_string()))
        };
        let section = |text: &str| div().mt(px(18.0)).mb(px(8.0)).child(label(text));

        // ── Avatar: robot faces, then symbols and the initial; colours ──────
        // Glyphs rest neutral; the pick wears the bot's colour, as the
        // avatar will.
        let cell = |id: String, on: bool| {
            div()
                .id(SharedString::from(id))
                .size(px(32.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(on, |el| el.bg(tint.opacity(0.16)))
                .when(!on, |el| el.hover(|s| s.bg(theme.element_hover)))
        };
        let glyph_tint = |on: bool| if on { tint } else { theme.text_muted };
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
                    let on = dialog.icon.as_deref() == Some(*key);
                    cell(format!("bot-face-{key}"), on)
                        .on_click(cx.listener(pick_icon(Some(key))))
                        .child(icon(asset).size(px(19.0)).text_color(glyph_tint(on)))
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
                    .text_color(glyph_tint(dialog.icon.is_none()))
                    .on_click(cx.listener(pick_icon(None)))
                    .child(SharedString::from(crate::appearance::bot_style::initial(
                        &shown_name,
                    ))),
            )
            .children(
                crate::appearance::bot_style::BOT_SYMBOLS
                    .iter()
                    .map(|(key, asset)| {
                        let on = dialog.icon.as_deref() == Some(*key);
                        cell(format!("bot-symbol-{key}"), on)
                            .on_click(cx.listener(pick_icon(Some(key))))
                            .child(icon(asset).size(px(15.0)).text_color(glyph_tint(on)))
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

        // ── Permissions ─────────────────────────────────────────────────────
        let grant_option = |grant: Grant, text: &str| {
            let on = dialog.grant == grant;
            div()
                .id(SharedString::from(format!("bot-grant-{text}")))
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
                .child(SharedString::from(text.to_string()))
        };
        let permission_text = |title: &str, detail: &str| {
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
            .child(permission_text(
                "Project access",
                "Which projects' sessions it may read.",
            ))
            .child(
                div()
                    .w(px(204.0))
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
                    .h(px(28.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(9.0))
                    .px(px(8.0))
                    .rounded(px(8.0))
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
                            .text_color(theme.text_muted.opacity(0.8)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .child(SharedString::from(name.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(device.clone())),
                    )
            });
            div()
                .border_t_1()
                .border_color(hairline)
                .p(px(4.0))
                .flex()
                .flex_col()
                .gap(px(1.0))
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
                .children(rows)
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
                .child(permission_text(title, detail))
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
        let caps = dialog.capabilities;
        let read_only = !(caps.work_directly || caps.coordinate || caps.steer);
        let summary = access.map(|(sessions, spaces)| {
            let text = match (sessions, spaces) {
                (0, 0) => "No access to any sessions yet.".to_string(),
                (sessions, spaces) => format!(
                    "Access to {sessions} session{} in {spaces} project{}.{}",
                    if sessions == 1 { "" } else { "s" },
                    if spaces == 1 { "" } else { "s" },
                    if read_only { " Read-only." } else { "" }
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

        // ── Capabilities: what it may do beyond reading. Each lets the bot
        //    act, so each wears the accent's bolt (lit while on).
        let capability = |id: &str,
                          title: &str,
                          detail: &str,
                          on: bool,
                          first: bool,
                          flip: fn(&mut cypher_proto::BotCapabilities)| {
            div()
                .id(SharedString::from(id.to_string()))
                .when(!first, |el| el.border_t_1())
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
                        flip(&mut d.capabilities);
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_none()
                        .size(px(24.0))
                        .rounded(px(7.0))
                        .bg(if on {
                            theme.accent.opacity(0.14)
                        } else {
                            crate::kit::theme::ink(0.05)
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(icons::BOLT).size(px(13.0)).text_color(if on {
                            theme.accent
                        } else {
                            theme.text_muted.opacity(0.7)
                        })),
                )
                .child(permission_text(title, detail))
                .child(crate::settings::widgets::toggle_switch(theme, on))
        };
        let capabilities = div()
            .rounded(px(10.0))
            .border_1()
            .border_color(hairline)
            .bg(crate::kit::theme::ink(0.02))
            .flex()
            .flex_col()
            .child(capability(
                "bot-cap-direct",
                "Work directly",
                "Run commands and edit files in its projects on this device, under your \
                 permission rules.",
                caps.work_directly,
                true,
                |c| c.work_directly = !c.work_directly,
            ))
            .child(capability(
                "bot-cap-coordinate",
                "Coordinate",
                "Plan work and start sessions in its projects, after you approve.",
                caps.coordinate,
                false,
                |c| c.coordinate = !c.coordinate,
            ))
            .child(capability(
                "bot-cap-steer",
                "Steer others' sessions",
                "Steer, answer or stop sessions it didn't start, each after you confirm.",
                caps.steer,
                false,
                |c| c.steer = !c.steer,
            ));

        // ── The form column: scrolls the card's full height, under both
        //    glass bands; its padding keeps every row clear of them at rest.
        let avatar = crate::appearance::bot_style::bot_avatar(
            &shown_name,
            dialog.icon.as_deref(),
            dialog.color.as_deref(),
            48.0,
            theme,
        );
        let form = div()
            .id("bot-dialog-form")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .pt(px(HEADER_H + 14.0))
            .pb(px(FOOTER_H + 18.0))
            .px(px(PAD))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(14.0))
                    .child(avatar)
                    .child(div().flex_1().min_w_0().child(popover::dialog_field(
                        dialog.name.clone().into_any_element(),
                    ))),
            )
            .child(section("Avatar"))
            .child(faces)
            .child(symbols)
            .child(colors)
            .child(section("Permissions"))
            .child(permissions)
            .children(summary)
            .child(section("What it may do"))
            .child(capabilities)
            .child(section("Instructions"))
            .child(popover::dialog_field(
                dialog.instructions.clone().into_any_element(),
            ));

        // ── The devices rail: where the bot runs, fixed once created ────────
        let host_row = |ix: usize, host: &BotHost, active: bool, pickable: bool| {
            let pick = host.id.clone();
            let caption = match (host.local, host.online, host.ok) {
                (true, _, _) => "This device",
                (false, false, _) => "Offline",
                (false, true, false) => "Needs Cypher 0.3.57 or newer",
                (false, true, true) => "Online",
            };
            div()
                .id(("bot-host", ix))
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(8.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(9.0))
                .when(!host.ok && !active, |el| el.opacity(0.55))
                .when(active, |el| {
                    el.bg(crate::kit::theme::card_selected_bg())
                        .shadow(crate::kit::theme::card_selected_shadows())
                })
                .when(pickable && !active, |el| {
                    el.hover(|s| s.bg(theme.element_hover))
                })
                .when(pickable, |el| {
                    el.cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(d) = this.dialogs.bot.as_mut() {
                                d.host = Some(pick.clone());
                            }
                            cx.notify();
                        }))
                })
                .child(
                    icon(if host.laptop {
                        icons::LAPTOP
                    } else {
                        icons::MONITOR
                    })
                    .size(px(15.0))
                    .flex_none()
                    .text_color(if active {
                        theme.text
                    } else {
                        theme.text_muted
                    }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(12.5))
                                .text_color(if active { theme.text } else { theme.text_muted })
                                .child(SharedString::from(host.name.clone())),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(5.0))
                                .text_size(px(11.0))
                                .text_color(theme.text_faint)
                                .when(host.online, |el| {
                                    el.child(
                                        div()
                                            .size(px(5.0))
                                            .rounded_full()
                                            .bg(theme.success.opacity(0.9)),
                                    )
                                })
                                .child(SharedString::from(caption)),
                        ),
                )
        };
        let rail = div()
            .w(px(RAIL_W))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(hairline)
            .pt(px(HEADER_H + 8.0))
            .pb(px(FOOTER_H + 8.0))
            .px(px(8.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .px(px(8.0))
                    .pt(px(2.0))
                    .pb(px(4.0))
                    .child(label("Run on device")),
            )
            .children(hosts.iter().enumerate().map(|(ix, host)| {
                let active = dialog.host.as_deref() == Some(host.id.as_str());
                host_row(ix, host, active, !editing && host.ok)
            }))
            .when(hosts.is_empty(), |el| {
                el.child(
                    div()
                        .px(px(8.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from("No device can host a bot.")),
                )
            })
            .child(
                div()
                    .mt(px(6.0))
                    .px(px(8.0))
                    .text_size(px(11.0))
                    .line_height(px(15.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(if editing {
                        "A bot stays on the device it was created on."
                    } else {
                        "Mac and Linux devices that are online. Fixed once created."
                    })),
            );

        // ── Glass bands: the header and footer over the scrolling form ──────
        // A thin tint of the card's own fill under the palette band, so the
        // form reads through them, blurred.
        let card_fill = if theme.is_glass() {
            theme.glass_overlay()
        } else {
            theme.surface_overlay
        };
        let glass = card_fill.opacity(0.4);
        let glass_band = |top: bool, content: AnyElement| {
            let r = px(CARD_RADIUS);
            let corners = if top {
                gpui::Corners {
                    top_left: r,
                    top_right: r,
                    bottom_left: px(0.0),
                    bottom_right: px(0.0),
                }
            } else {
                gpui::Corners {
                    top_left: px(0.0),
                    top_right: px(0.0),
                    bottom_left: r,
                    bottom_right: r,
                }
            };
            div()
                .absolute()
                .left_0()
                .right_0()
                .when(top, |el| el.top_0().h(px(HEADER_H)))
                .when(!top, |el| el.bottom_0().h(px(FOOTER_H)))
                .child(crate::kit::frost::frosted_bar(
                    corners,
                    GLASS_BLUR,
                    div()
                        .size_full()
                        .occlude()
                        .bg(glass)
                        .when(top, |el| el.rounded_t(r).border_b_1())
                        .when(!top, |el| el.rounded_b(r).border_t_1())
                        .border_color(hairline)
                        .child(div().size_full().bg(band).child(content)),
                ))
        };
        let header = div()
            .size_full()
            .pl(px(14.0))
            .pr(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(
                icon(icons::BOT)
                    .size(px(15.0))
                    .flex_none()
                    .text_color(theme.text_muted.opacity(0.8)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(14.0))
                    .text_color(theme.text)
                    .child(SharedString::from(if editing {
                        "Bot settings"
                    } else {
                        "New bot"
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(
                        "Answers using only the sessions you allow",
                    )),
            )
            .child(
                popover::key_cap(theme)
                    .mono(theme)
                    .id("bot-dialog-esc")
                    .px(px(6.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_muted.opacity(0.7))
                    .cursor_pointer()
                    .hover(|s| s.bg(crate::kit::theme::ink(0.09)))
                    .on_click(cx.listener(|this, _, _, cx| this.close_bot_dialog(cx)))
                    .child(SharedString::from("esc")),
            );
        let busy = dialog.busy;
        let mut footer = div()
            .size_full()
            .px(px(12.0))
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
                    .py(px(5.0))
                    .rounded(px(7.0))
                    .text_size(px(12.5))
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
                            .size(px(13.0))
                            .text_color(theme.danger),
                    )
                    .child(SharedString::from("Delete")),
            );
        }
        footer = footer
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(theme.danger)
                    .children(dialog.error.clone()),
            )
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
                .py(px(5.0))
                .when(busy, |el| el.opacity(0.6))
                .on_click(cx.listener(|this, _, _, cx| this.submit_bot_dialog(cx))),
            );

        // FIXED height, like the palettes: a longer project list scrolls
        // instead of growing the card.
        let card_h = (f32::from(viewport.height) - 120.0).clamp(420.0, 620.0);
        let card = div()
            .id("bot-palette")
            .relative()
            .w(px(CARD_W))
            .h(px(card_h))
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(crate::kit::theme::hairline(0.10))
            .bg(card_fill)
            .shadow_lg()
            .overflow_hidden()
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.close_bot_dialog(cx);
                }
            }))
            .child(div().size_full().flex().flex_row().child(form).child(rail))
            .child(glass_band(true, header.into_any_element()))
            .child(glass_band(false, footer.into_any_element()))
            .into_any_element();
        Some(popover::modal_glass(
            "bot-dialog",
            viewport,
            card,
            CARD_RADIUS,
        ))
    }
}
