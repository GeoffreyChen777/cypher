//! The Botboard's detail panel (§13.1): one item from the sidebar's cards in
//! full. It slides in from the window's right edge while the chat narrows
//! ([`super::column`]), closes with × or Esc, and opening another row
//! replaces its content; the opened row stays highlighted in the sidebar.

use super::board::{load_note, project_loads, project_row, scoped_sessions, two_lines};
use super::*;

/// What the panel shows: a card's whole list (its header) or one item (a
/// row). Ids are record ids unless named otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::shell) enum PanelItem {
    /// Every project the bot reads (the Projects card's header).
    Projects,
    /// One project's sessions in the bot's scope, by space id.
    Project(String),
    /// Everything that needs the owner.
    NeedsYou,
    /// A plan awaiting approval (work id).
    Plan(String),
    /// An action awaiting approval.
    Approval(String),
    /// A session the bot started, asking (session chat id, work id).
    Asking {
        chat_id: String,
        work_id: String,
    },
    /// A peer answer drafted in Review mode (exchange id).
    Review(String),
    /// Running and recent work.
    Work,
    /// One orchestration (work id).
    WorkItem(String),
    /// One of the bot's hidden threads, read-only (its chat id): an
    /// orchestration's, a routine's or a connection's.
    Thread(String),
    /// Routines, by next run.
    Schedule,
    Routine(String),
    /// The new-routine form.
    NewRoutine,
    /// Every note, editable.
    Notebook,
    /// The full audit log.
    Activity,
    /// Connections and invites.
    Network,
    Connection(String),
    /// Create an invite or redeem one.
    Invite,
}

/// The panel header's glyph, title and subtitle.
pub(super) struct PanelHead {
    pub glyph: &'static str,
    pub tint: Option<gpui::Hsla>,
    pub title: String,
    pub subtitle: String,
}

impl Shell {
    /// Open `item`; a second click on the open row closes it.
    pub(in crate::shell) fn open_panel(&mut self, item: PanelItem, cx: &mut Context<Self>) {
        if self.board.panel.as_ref() == Some(&item) {
            return self.close_panel(cx);
        }
        if self.board.panel.is_none() {
            let from = self.eval_tween(self.board.panel_tween, 0.0);
            self.board.panel_tween = Some(WidthTween::new(from, super::column::PANEL_SPAN));
        }
        self.board.panel = Some(item);
        self.drop_panel_extras();
        cx.notify();
    }

    /// What an item kept while it was open (the session it looked into, a
    /// draft being edited) goes with it.
    pub(super) fn drop_panel_extras(&mut self) {
        let keep_peek = match self.board.panel.as_ref() {
            Some(PanelItem::Asking { chat_id, .. }) => Some(chat_id.clone()),
            Some(PanelItem::Thread(_)) => self.board.peek.as_ref().map(|p| p.chat_id.clone()),
            _ => None,
        };
        if self.board.peek.as_ref().map(|p| &p.chat_id) != keep_peek.as_ref() {
            self.board.peek = None;
        }
        if !matches!(self.board.panel, Some(PanelItem::Review(_))) {
            self.board.review = None;
        }
    }

    /// Slide the panel shut, its content kept until the tween ends.
    pub(super) fn close_panel(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.board.panel.take() else {
            return;
        };
        let from = self.eval_tween(self.board.panel_tween, super::column::PANEL_SPAN);
        self.board.panel_tween = Some(WidthTween::new(from, 0.0));
        self.board.panel_last = Some(item);
        cx.notify();
    }

    /// Esc anywhere in the window closes an open panel, unless a bot dialog
    /// over it takes the key. Returns whether it did.
    pub(in crate::shell) fn close_panel_on_escape(&mut self, cx: &mut Context<Self>) -> bool {
        let shown = self.botboard() && matches!(self.route, Route::Chat);
        // A confirm over the panel takes the key first.
        if shown && self.board.confirm.take().is_some() {
            cx.notify();
            return true;
        }
        if !shown
            || self.board.panel.is_none()
            || self.dialogs.bot.is_some()
            || self.dialogs.delete_bot.is_some()
        {
            return false;
        }
        self.close_panel(cx);
        true
    }

    /// Shut at once (the bot changed under it).
    pub(super) fn close_panel_now(&mut self) {
        self.board.panel = None;
        self.board.panel_last = None;
        self.board.panel_tween = None;
        self.drop_panel_extras();
    }

    /// The panel card for `item`, `None` when it no longer resolves (the
    /// project went away, or the bot).
    pub(super) fn render_panel_card(
        &mut self,
        item: &PanelItem,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let bot = {
            let state = self.state.read(cx);
            current_bot(&state.bots, self.settings.botboard_bot.as_deref()).cloned()
        }?;
        self.board.panel_footer = None;
        let (head, body) = match item {
            PanelItem::Projects => self.render_projects_panel(&bot, &theme, cx),
            PanelItem::Project(space_id) => {
                self.render_project_panel(&bot, space_id, &theme, cx)?
            }
            PanelItem::NeedsYou
            | PanelItem::Plan(_)
            | PanelItem::Approval(_)
            | PanelItem::Asking { .. }
            | PanelItem::Review(_) => self.render_needs_panel(&bot, item, &theme, cx)?,
            PanelItem::Work | PanelItem::WorkItem(_) | PanelItem::Thread(_) => {
                self.render_work_panel(&bot, item, &theme, cx)?
            }
            PanelItem::Schedule | PanelItem::Routine(_) | PanelItem::NewRoutine => {
                self.render_schedule_panel(&bot, item, &theme, cx)?
            }
            PanelItem::Notebook => self.render_notebook_panel(&bot, item, &theme, cx)?,
            PanelItem::Activity => self.render_activity_panel(&bot, item, &theme, cx)?,
            PanelItem::Network | PanelItem::Connection(_) | PanelItem::Invite => {
                self.render_network_panel(&bot, item, &theme, cx)?
            }
        };
        let card_bg = crate::appearance::chat_style::panel_background(
            crate::appearance::chat_style::settings(cx),
            Theme::of(cx),
            true,
        );
        let glyph = div()
            .flex_none()
            .size(px(30.0))
            .rounded(px(8.0))
            .bg(match head.tint {
                Some(tint) => tint.opacity(0.14),
                None => crate::kit::theme::ink(0.06),
            })
            .flex()
            .items_center()
            .justify_center()
            .child(
                icon(head.glyph)
                    .size(px(15.0))
                    .text_color(head.tint.unwrap_or(theme.text_muted)),
            );
        let close_key = "bot-panel-close";
        let close = div()
            .id(close_key)
            .flex_none()
            .size(px(26.0))
            .rounded(px(7.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(motion::hover_blend(
                close_key,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.12),
            ))
            .on_hover(motion::hover_listener(close_key))
            .tooltip(|_, cx| {
                cx.new(|_| super::super::session::FindTooltip("Close  esc".into()))
                    .into()
            })
            .on_click(cx.listener(|this, _, _, cx| this.close_panel(cx)))
            .child(
                icon(icons::CLOSE)
                    .size(px(11.0))
                    .text_color(theme.text_muted),
            );
        let header = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .px(px(16.0))
            .pt(px(14.0))
            .pb(px(12.0))
            .border_b_1()
            .border_color(theme.border)
            .child(glyph)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(15.0))
                            .line_height(px(20.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from(head.title)),
                    )
                    .when(!head.subtitle.is_empty(), |el| {
                        el.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(head.subtitle)),
                        )
                    }),
            )
            .child(close);
        // Replacing the content fades the new item in. A thread's
        // transcript scrolls itself, so its body fills the card unscrolled.
        let content_key = SharedString::from(format!("bot-panel-{item:?}"));
        let body = if matches!(item, PanelItem::Thread(_)) {
            div()
                .flex_1()
                .min_h_0()
                .child(motion::fade_quick(content_key, body.size_full()))
                .into_any_element()
        } else {
            div()
                .id("bot-panel-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(motion::fade_quick(content_key, body))
                .into_any_element()
        };
        // A form's buttons stay put at the foot while its fields scroll.
        let footer = self.board.panel_footer.take().map(|footer| {
            div()
                .flex_none()
                .border_t_1()
                .border_color(crate::kit::theme::hairline(0.06))
                .px(px(8.0))
                .py(px(10.0))
                .child(footer)
        });
        Some(
            div()
                .size_full()
                .rounded(px(PANEL_CORNER_RADIUS))
                .bg(card_bg)
                .shadow_sm()
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(header)
                .child(body)
                .children(footer)
                .into_any_element(),
        )
    }

    /// Every project the bot reads, with what is running and asking there;
    /// a row opens the project.
    fn render_projects_panel(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (PanelHead, gpui::Div) {
        let now = Utc::now();
        let rows: Vec<(super::board::ProjectLoad, cypher_proto::Space, String)> = {
            let state = self.state.read(cx);
            let spaces = state.spaces_sorted();
            project_loads(&bot.scope, &spaces, &state.chats, |chat| {
                state.display_status_for(chat, now)
            })
            .into_iter()
            .filter_map(|load| {
                let space = state.space_row(&load.space_id)?.clone();
                let device = state
                    .device_name(&space.device_id)
                    .unwrap_or("Unknown device")
                    .to_string();
                Some((load, space, device))
            })
            .collect()
        };
        let reach = match bot.scope.projects {
            ProjectGrant::All => "all projects".to_string(),
            _ if rows.len() == 1 => "1 project".to_string(),
            _ => format!("{} projects", rows.len()),
        };
        let head = PanelHead {
            glyph: icons::WIDGET,
            tint: None,
            title: "Projects".into(),
            subtitle: format!("{} reads {reach}", bot.name),
        };
        let list = rows.into_iter().map(|(load, space, device)| {
            let key = format!("bot-panel-project-{}", space.id);
            let item = PanelItem::Project(space.id.clone());
            let sessions = load.sessions;
            let note = load_note(&load);
            let note_color = if load.asking > 0 {
                theme.warning
            } else if load.running > 0 {
                theme.text
            } else {
                theme.text_faint
            };
            project_row(&key, &space, false, theme)
                .py(px(8.0))
                .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
                .child(two_lines(
                    space.display_name().to_string(),
                    device,
                    format!(
                        "· {sessions} session{}",
                        if sessions == 1 { "" } else { "s" }
                    ),
                    theme,
                ))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(note_color)
                        .child(SharedString::from(note)),
                )
        });
        let body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(list);
        (head, body)
    }

    /// One project's sessions in the bot's scope, newest first; a click
    /// opens the session in Projects mode.
    fn render_project_panel(
        &self,
        bot: &Bot,
        space_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let now = Utc::now();
        let (space, device, sessions) = {
            let state = self.state.read(cx);
            if !bot.scope.sees_space(space_id) {
                return None;
            }
            let space = state.space_row(space_id)?.clone();
            let device = state
                .device_name(&space.device_id)
                .unwrap_or("Unknown device")
                .to_string();
            let sessions: Vec<(String, String, ChatIndicator, String)> =
                scoped_sessions(&bot.scope, &state.chats, space_id)
                    .into_iter()
                    .map(|chat| {
                        (
                            chat.id.clone(),
                            transcript::single_line(chat.title.as_deref().unwrap_or("New session")),
                            state.display_status_for(chat, now),
                            format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), now),
                        )
                    })
                    .collect();
            (space, device, sessions)
        };
        let count = sessions.len();
        let head = PanelHead {
            glyph: crate::appearance::space_style::space_icon(space.icon.as_deref()),
            tint: crate::appearance::space_style::space_color(space.color.as_deref(), theme),
            title: space.display_name().to_string(),
            subtitle: format!(
                "{device} · {count} session{}",
                if count == 1 { "" } else { "s" }
            ),
        };
        let ticker = self.pulse_ticker.entity_id();
        let rows = sessions
            .into_iter()
            .map(|(chat_id, title, status, ago)| {
                let key = format!("bot-panel-session-{chat_id}");
                let mark =
                    super::super::render::status_mark(status, ticker, theme).unwrap_or_else(|| {
                        div()
                            .flex_none()
                            .size(px(5.0))
                            .rounded_full()
                            .bg(if status == ChatIndicator::Errored {
                                theme.danger
                            } else {
                                theme.text_faint.opacity(0.6)
                            })
                            .into_any_element()
                    });
                div()
                    .id(SharedString::from(key.clone()))
                    .px(px(8.0))
                    .h(px(34.0))
                    .rounded(px(8.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .cursor_pointer()
                    .bg(motion::hover_blend(
                        &key,
                        crate::kit::theme::wash(0.0),
                        crate::kit::theme::wash(0.05),
                    ))
                    .on_hover(motion::hover_listener(key.clone()))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.open_chat(chat_id.clone(), cx)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(14.0))
                            .flex()
                            .justify_center()
                            .child(mark),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.0))
                            .text_color(theme.text)
                            .child(SharedString::from(title)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted.opacity(0.8))
                            .child(SharedString::from(ago)),
                    )
            })
            .collect::<Vec<_>>();
        let body = div().p(px(8.0)).flex().flex_col().gap(px(1.0));
        let body = if rows.is_empty() {
            body.child(
                div()
                    .px(px(8.0))
                    .py(px(10.0))
                    .text_size(px(12.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from("No sessions here yet.")),
            )
        } else {
            body.child(
                div()
                    .px(px(8.0))
                    .pt(px(6.0))
                    .pb(px(6.0))
                    .text_size(px(10.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted.opacity(0.75))
                    .child(SharedString::from("SESSIONS")),
            )
            .children(rows)
        };
        Some((head, body))
    }
}
