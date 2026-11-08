//! The move-sessions palette: pick the project a session — or every session
//! of a project — moves to, on this device or another one. The sessions'
//! host runs the move (`MigrateChats`); the palette follows its progress.

use super::add_space::key_chip;
use super::*;
use cypher_proto::ChatMigrationEvent;

/// What the palette moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::shell) enum MoveSource {
    Chat(String),
    Space(String),
}

pub(in crate::shell) enum MovePhase {
    Picking,
    Moving {
        done: usize,
        total: usize,
        current: Option<SharedString>,
        failures: Vec<(SharedString, SharedString)>,
    },
    /// Some sessions stayed where they were (a clean move just closes).
    Finished {
        moved: usize,
        failures: Vec<(SharedString, SharedString)>,
    },
}

pub(in crate::shell) struct MoveSessionsFlow {
    source: MoveSource,
    active: usize,
    focus: FocusHandle,
    focus_pending: bool,
    list_scroll: gpui::ScrollHandle,
    phase: MovePhase,
    /// Esc during a move hides the palette; the move carries on and a
    /// failure lands in the sidebar notice.
    hidden: bool,
    task: Option<Task<()>>,
}

/// The sessions being moved, as the palette names them.
struct MoveSourceInfo {
    /// The device that runs the sessions — and the move.
    host: String,
    space_id: Option<String>,
    title: String,
    count: usize,
}

/// A project the sessions can move to.
struct MoveTarget {
    space: Space,
    device: String,
    online: bool,
    is_local: bool,
}

impl Shell {
    pub(in crate::shell) fn open_move_sessions(
        &mut self,
        source: MoveSource,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        self.close_space_menu(cx);
        if self
            .dialogs
            .move_sessions
            .as_ref()
            .is_some_and(|flow| matches!(flow.phase, MovePhase::Moving { .. }))
        {
            self.sidebar.notice = Some("Another move is still under way.".into());
            cx.notify();
            return;
        }
        let mut flow = MoveSessionsFlow {
            source,
            active: 0,
            focus: cx.focus_handle(),
            focus_pending: true,
            list_scroll: gpui::ScrollHandle::new(),
            phase: MovePhase::Picking,
            hidden: false,
            task: None,
        };
        flow.active = self
            .move_targets(&flow.source, cx)
            .iter()
            .position(|t| t.online)
            .unwrap_or(0);
        self.dialogs.move_sessions = Some(flow);
        cx.notify();
    }

    fn move_source_info(&self, source: &MoveSource, cx: &App) -> Option<MoveSourceInfo> {
        let state = self.state.read(cx);
        match source {
            MoveSource::Chat(chat_id) => {
                let chat = state.chats.iter().find(|c| &c.id == chat_id)?;
                Some(MoveSourceInfo {
                    host: chat.device_id.clone(),
                    space_id: chat.space_id.clone(),
                    title: chat
                        .title
                        .clone()
                        .map(|t| transcript::single_line(&t))
                        .unwrap_or_else(|| "New session".into()),
                    count: 1,
                })
            }
            MoveSource::Space(space_id) => {
                let space = state.space_row(space_id)?;
                Some(MoveSourceInfo {
                    host: space.device_id.clone(),
                    space_id: Some(space.id.clone()),
                    title: space.display_name().to_string(),
                    count: movable_sessions(&state.chats, space_id),
                })
            }
        }
    }

    /// Every other project, this device's first, then by device and name.
    fn move_targets(&self, source: &MoveSource, cx: &App) -> Vec<MoveTarget> {
        let from = self
            .move_source_info(source, cx)
            .and_then(|info| info.space_id);
        let now = Utc::now();
        let state = self.state.read(cx);
        let local = state.local_device_id.clone();
        let mut targets: Vec<MoveTarget> = state
            .spaces
            .iter()
            .filter(|s| Some(&s.id) != from.as_ref())
            .map(|space| MoveTarget {
                device: state
                    .device_name(&space.device_id)
                    .unwrap_or("Unknown device")
                    .to_string(),
                online: state.device_online(&space.device_id, now),
                is_local: local.as_deref() == Some(space.device_id.as_str()),
                space: space.clone(),
            })
            .collect();
        targets.sort_by_key(|t| {
            (
                !t.is_local,
                t.device.to_lowercase(),
                t.space.display_name().to_lowercase(),
                t.space.id.clone(),
            )
        });
        targets
    }

    fn close_move_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.dialogs.move_sessions.as_mut() else {
            return;
        };
        if matches!(flow.phase, MovePhase::Moving { .. }) {
            flow.hidden = true;
        } else {
            self.dialogs.move_sessions = None;
        }
        cx.notify();
    }

    fn move_sessions_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        let picking = self
            .dialogs
            .move_sessions
            .as_ref()
            .is_some_and(|f| matches!(f.phase, MovePhase::Picking));
        match key {
            popover::MenuKey::Escape => self.close_move_sessions(cx),
            popover::MenuKey::Up | popover::MenuKey::Down if picking => {
                let Some(source) = self
                    .dialogs
                    .move_sessions
                    .as_ref()
                    .map(|f| f.source.clone())
                else {
                    return;
                };
                let count = self.move_targets(&source, cx).len();
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(flow) = self.dialogs.move_sessions.as_mut() {
                    flow.active = popover::menu_step(Some(flow.active), count, delta).unwrap_or(0);
                    // Row 0 of the scroll container is the section label.
                    flow.list_scroll.scroll_to_item(flow.active + 1);
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let Some(flow) = self.dialogs.move_sessions.as_ref() else {
                    return;
                };
                if !picking {
                    if matches!(flow.phase, MovePhase::Finished { .. }) {
                        self.close_move_sessions(cx);
                    }
                    return;
                }
                let (source, active) = (flow.source.clone(), flow.active);
                let host_online = self.move_host_online(&source, cx);
                if let Some(target) = self.move_targets(&source, cx).get(active)
                    && target.online
                    && host_online
                {
                    let to = target.space.id.clone();
                    self.start_move(to, cx);
                }
            }
            _ => {}
        }
    }

    fn move_host_online(&self, source: &MoveSource, cx: &App) -> bool {
        self.move_source_info(source, cx)
            .is_some_and(|info| self.state.read(cx).device_online(&info.host, Utc::now()))
    }

    /// Ask the sessions' host to move them to `to_space_id`, and follow.
    fn start_move(&mut self, to_space_id: String, cx: &mut Context<Self>) {
        let Some(source) = self
            .dialogs
            .move_sessions
            .as_ref()
            .map(|f| f.source.clone())
        else {
            return;
        };
        let Some(info) = self.move_source_info(&source, cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar.notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let params = match &source {
            MoveSource::Chat(chat_id) => serde_json::json!({
                "chatIds": [chat_id], "toSpaceId": to_space_id, "targetDeviceId": info.host,
            }),
            MoveSource::Space(space_id) => serde_json::json!({
                "fromSpaceId": space_id, "toSpaceId": to_space_id, "targetDeviceId": info.host,
            }),
        };
        let host_name = self
            .state
            .read(cx)
            .device_name(&info.host)
            .unwrap_or("The device running these sessions")
            .to_string();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
        let stream = Tokio::spawn(cx, async move {
            let mut items = engine
                .client()
                .subscribe(methods::MIGRATE_CHATS, params)
                .await
                .map_err(|error| match error.to_string() {
                    e if e.contains("unknown method") => {
                        format!("{host_name} runs an older Cypher. Update it, then try again.")
                    }
                    e => e,
                })?;
            while let Some(item) = items.recv().await {
                let _ = tx.send(item);
            }
            Ok::<(), String>(())
        });
        let source_title: SharedString = info.title.clone().into();
        let task = cx.spawn(async move |this, cx| {
            while let Some(item) = rx.recv().await {
                let Ok(event) = serde_json::from_value::<ChatMigrationEvent>(item) else {
                    continue;
                };
                this.update(cx, |shell, cx| shell.apply_move_event(event, cx))
                    .ok();
            }
            let error = match stream.await {
                Ok(Err(error)) => error,
                _ => "The move stopped before it finished.".into(),
            };
            // A stream that ends without its summary never reported Done.
            this.update(cx, |shell, cx| {
                let still_moving = shell
                    .dialogs
                    .move_sessions
                    .as_ref()
                    .is_some_and(|f| matches!(f.phase, MovePhase::Moving { .. }));
                if still_moving {
                    shell.finish_move(0, vec![(source_title, error.into())], cx);
                }
            })
            .ok();
        });
        if let Some(flow) = self.dialogs.move_sessions.as_mut() {
            flow.phase = MovePhase::Moving {
                done: 0,
                total: info.count,
                current: None,
                failures: Vec::new(),
            };
            flow.task = Some(task);
        }
        cx.notify();
    }

    fn apply_move_event(&mut self, event: ChatMigrationEvent, cx: &mut Context<Self>) {
        let Some(MovePhase::Moving {
            done,
            total,
            current,
            failures,
        }) = self.dialogs.move_sessions.as_mut().map(|f| &mut f.phase)
        else {
            return;
        };
        match event {
            ChatMigrationEvent::Start { total: n } => *total = n,
            ChatMigrationEvent::Chat {
                title,
                index,
                total: n,
                error,
                ..
            } => {
                *done = index;
                *total = n;
                let title: SharedString = title
                    .map(|t| transcript::single_line(&t))
                    .unwrap_or_else(|| "New session".into())
                    .into();
                if let Some(error) = error {
                    failures.push((title.clone(), error.into()));
                }
                *current = Some(title);
            }
            ChatMigrationEvent::Done { moved, .. } => {
                let failures = std::mem::take(failures);
                self.finish_move(moved, failures, cx);
                return;
            }
        }
        cx.notify();
    }

    /// The move settled: a clean one closes the palette; failures stay on
    /// screen, or reach the sidebar notice when the palette was hidden.
    fn finish_move(
        &mut self,
        moved: usize,
        failures: Vec<(SharedString, SharedString)>,
        cx: &mut Context<Self>,
    ) {
        let hidden = self
            .dialogs
            .move_sessions
            .as_ref()
            .is_some_and(|f| f.hidden);
        if failures.is_empty() {
            self.dialogs.move_sessions = None;
        } else if hidden {
            let (title, error) = &failures[0];
            self.sidebar.notice = Some(if failures.len() == 1 {
                format!("“{title}” couldn’t move: {error}").into()
            } else {
                format!(
                    "{} sessions couldn’t move. “{title}”: {error}",
                    failures.len()
                )
                .into()
            });
            self.dialogs.move_sessions = None;
        } else if let Some(flow) = self.dialogs.move_sessions.as_mut() {
            flow.phase = MovePhase::Finished { moved, failures };
            flow.task = None;
        }
        cx.notify();
    }

    /// The palette: the quick-chat palette's card, header band and footer
    /// legend around the project list, the move's progress, or what stayed
    /// behind.
    pub(super) fn render_move_sessions_dialog(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (source, active, focus, list_scroll) = {
            let flow = self.dialogs.move_sessions.as_mut()?;
            if flow.hidden {
                return None;
            }
            if std::mem::take(&mut flow.focus_pending) {
                window.focus(&flow.focus, cx);
            }
            (
                flow.source.clone(),
                flow.active,
                flow.focus.clone(),
                flow.list_scroll.clone(),
            )
        };
        let Some(info) = self.move_source_info(&source, cx) else {
            // The session or project went away meanwhile.
            self.dialogs.move_sessions = None;
            return None;
        };
        let title: SharedString = match &source {
            MoveSource::Chat(_) => format!("Move “{}”", info.title).into(),
            MoveSource::Space(_) if info.count == 1 => {
                format!("Move 1 session from “{}”", info.title).into()
            }
            MoveSource::Space(_) => {
                format!("Move {} sessions from “{}”", info.count, info.title).into()
            }
        };
        let hairline = crate::kit::theme::hairline(0.06);
        let band = popover::band();
        let picking = self
            .dialogs
            .move_sessions
            .as_ref()
            .is_some_and(|f| matches!(f.phase, MovePhase::Picking));
        let body = if picking {
            let host_online = self.move_host_online(&source, cx);
            let host_name = self
                .state
                .read(cx)
                .device_name(&info.host)
                .unwrap_or("The device running these sessions")
                .to_string();
            self.move_target_list(
                &source,
                active,
                host_online,
                &host_name,
                list_scroll,
                theme,
                cx,
            )
        } else {
            self.move_progress(theme)
        };
        let footer = move_footer(picking, hairline, band, theme);
        let card = div()
            .id("move-sessions-palette")
            .w(px(440.0))
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
                this.move_sessions_key(event, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_move_sessions(cx)))
            .child(move_header(title, hairline, band, theme, cx))
            .child(body)
            .child(footer)
            .into_any_element();
        Some(popover::modal_glass(
            "move-sessions-dialog",
            viewport,
            card,
            14.0,
        ))
    }

    #[allow(clippy::too_many_arguments)] // one render pass's inputs
    fn move_target_list(
        &self,
        source: &MoveSource,
        active: usize,
        host_online: bool,
        host_name: &str,
        list_scroll: gpui::ScrollHandle,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let targets = self.move_targets(source, cx);
        let label = |text: &str| {
            div()
                .px(px(8.0))
                .pt(px(2.0))
                .pb(px(4.0))
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text_muted.opacity(0.6))
                .child(SharedString::from(text.to_string()))
        };
        let note = |text: String| {
            div()
                .mx(px(8.0))
                .mb(px(6.0))
                .text_size(px(12.0))
                .line_height(px(17.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(text))
        };
        let mut list = div()
            .id("move-sessions-targets")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&list_scroll)
            .px(px(8.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(label("Move to project"));
        if !host_online {
            list = list.child(note(format!(
                "{host_name} is offline. Sessions move from the device that runs them — try again once it’s back."
            )));
        }
        if targets.is_empty() {
            list = list.child(note(
                "There’s no other project yet. Add one, then move sessions into it.".into(),
            ));
        }
        list = list.children(targets.into_iter().enumerate().map(|(ix, target)| {
            let enabled = host_online && target.online;
            move_target_row(ix, target, ix == active, enabled, theme, cx)
        }));
        div()
            .h(px(260.0))
            .flex_none()
            .py(px(6.0))
            .child(list)
            .into_any_element()
    }

    fn move_progress(&self, theme: &Theme) -> AnyElement {
        let Some(flow) = self.dialogs.move_sessions.as_ref() else {
            return div().into_any_element();
        };
        let body = div()
            .h(px(260.0))
            .flex_none()
            .px(px(16.0))
            .py(px(14.0))
            .flex()
            .flex_col()
            .gap(px(10.0));
        match &flow.phase {
            MovePhase::Picking => body.into_any_element(),
            MovePhase::Moving {
                done,
                total,
                current,
                ..
            } => {
                let step = if *total == 0 {
                    "Getting ready…".to_string()
                } else {
                    format!("Moving {} of {total}…", (*done + 1).min(*total))
                };
                body.items_center()
                    .justify_center()
                    .child(loaders::gradient_spinner(
                        3.0,
                        self.pulse_ticker.entity_id(),
                    ))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text)
                            .child(SharedString::from(step)),
                    )
                    .child(
                        div()
                            .max_w_full()
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child(current.clone().unwrap_or_default()),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(
                                "History and agent context move with each session.",
                            )),
                    )
                    .into_any_element()
            }
            MovePhase::Finished { moved, failures } => {
                let headline = match moved {
                    0 => "Nothing moved.".to_string(),
                    1 => "1 session moved.".to_string(),
                    n => format!("{n} sessions moved."),
                };
                body.child(div().text_size(px(13.0)).text_color(theme.text).child(
                    SharedString::from(format!("{headline} These stayed where they were:")),
                ))
                .child(
                    div()
                        .id("move-sessions-failures")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .children(failures.iter().map(|(title, error)| {
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(12.5))
                                        .text_color(theme.text)
                                        .child(title.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .line_height(px(17.0))
                                        .text_color(theme.text_muted)
                                        .child(error.clone()),
                                )
                        })),
                )
                .into_any_element()
            }
        }
    }
}

/// Root sessions of a project — archived ones too; subagent chats move with
/// their parent.
fn movable_sessions(chats: &[Chat], space_id: &str) -> usize {
    chats
        .iter()
        .filter(|c| c.space_id.as_deref() == Some(space_id) && !c.is_child())
        .count()
}

/// Does the project have anything to move? (Gates the card menu entry.)
pub(in crate::shell) fn space_has_sessions(chats: &[Chat], space_id: &str) -> bool {
    movable_sessions(chats, space_id) > 0
}

fn move_header(
    title: SharedString,
    hairline: gpui::Hsla,
    band: gpui::Hsla,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> gpui::Div {
    div()
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
            icon(icons::FOLDER)
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
                .child(title),
        )
        .child(
            key_chip(theme)
                .id("move-sessions-esc")
                .cursor_pointer()
                .hover(|s| s.bg(crate::kit::theme::ink(0.09)))
                .on_click(cx.listener(|this, _, _, cx| this.close_move_sessions(cx)))
                .child(SharedString::from("esc")),
        )
}

/// One destination project: its glyph, name, the device it runs on, and
/// that device's presence. Projects on offline devices are listed but inert.
fn move_target_row(
    ix: usize,
    target: MoveTarget,
    is_active: bool,
    enabled: bool,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> gpui::Stateful<gpui::Div> {
    let glyph = crate::appearance::space_style::space_icon(target.space.icon.as_deref());
    let tint = crate::appearance::space_style::space_color(target.space.color.as_deref(), theme)
        .unwrap_or(theme.text_muted.opacity(0.8));
    let name: SharedString = target.space.display_name().to_string().into();
    let device: SharedString = if target.is_local {
        "This device".into()
    } else {
        target.device.clone().into()
    };
    let space_id = target.space.id.clone();
    let online = target.online;
    div()
        .id(("move-sessions-target", ix))
        .h(px(30.0))
        .px(px(8.0))
        .rounded(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .text_size(px(12.5))
        .when(enabled, |el| el.cursor_pointer())
        .when(!enabled, |el| el.opacity(0.55))
        .when(is_active, |el| {
            el.bg(crate::kit::theme::card_selected_bg())
                .shadow(crate::kit::theme::card_selected_shadows())
                .text_color(theme.text)
        })
        .when(!is_active, |el| {
            el.text_color(theme.text_muted.opacity(0.75))
                .hover(|s| s.bg(theme.element_hover))
        })
        .on_mouse_move(cx.listener(move |this, _, _, cx| {
            if let Some(flow) = this.dialogs.move_sessions.as_mut()
                && flow.active != ix
            {
                flow.active = ix;
                cx.notify();
            }
        }))
        .when(enabled, |el| {
            el.on_click(cx.listener(move |this, _, _, cx| {
                this.start_move(space_id.clone(), cx);
            }))
        })
        .child(icon(glyph).size(px(14.0)).flex_none().text_color(tint))
        .child(div().flex_1().min_w_0().truncate().child(name))
        .child(
            div()
                .flex_none()
                .max_w(px(150.0))
                .truncate()
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child(device),
        )
        .child(
            div()
                .size(px(5.0))
                .rounded_full()
                .flex_none()
                .when(online, |el| el.bg(theme.success.opacity(0.9)))
                .when(!online, |el| el.bg(crate::kit::theme::ink(0.22))),
        )
}

/// The footer's key-cap legend.
fn move_footer(picking: bool, hairline: gpui::Hsla, band: gpui::Hsla, theme: &Theme) -> gpui::Div {
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
        .gap(px(12.0));
    if picking {
        footer
            .child(popover::key_hint_pair(
                theme,
                icons::ARROW_UP,
                icons::ARROW_DOWN,
                "Navigate",
            ))
            .child(popover::key_hint(theme, icons::RETURN, "Move"))
    } else {
        footer.child(popover::key_hint_text(theme, "esc", "Close"))
    }
}
