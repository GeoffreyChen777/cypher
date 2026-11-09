//! The Botboard's detail panel (§13.1): one item from the sidebar's cards in
//! full. It slides in from the window's right edge while the chat narrows
//! ([`super::column`]), closes with × or Esc, and opening another row
//! replaces its content; the opened row stays highlighted in the sidebar.

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

/// The panel header's glyph and title. Item actions join it through
/// [`Shell::set_panel_actions`]; there is no subtitle (a panel says where
/// things stand in its body).
pub(super) struct PanelHead {
    pub glyph: &'static str,
    pub tint: Option<gpui::Hsla>,
    pub title: String,
}

impl Shell {
    /// Open `item`; a second click on the open row closes it.
    pub(in crate::shell) fn open_panel(&mut self, item: PanelItem, cx: &mut Context<Self>) {
        if self.board.panel.as_ref() == Some(&item) {
            return self.close_panel(cx);
        }
        if self.board.panel.is_none() {
            let from = self.eval_tween(self.board.panel_tween, 0.0);
            self.board.panel_tween = Some(WidthTween::new(from, self.panel_span()));
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
        let from = self.eval_tween(self.board.panel_tween, self.panel_span());
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
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let bot = {
            let state = self.state.read(cx);
            current_bot(&state.bots, self.settings.botboard_bot.as_deref()).cloned()
        }?;
        self.board.panel_footer = None;
        self.board.panel_actions = None;
        self.board.panel_mark = None;
        self.board.panel_title_note = None;
        self.board.panel_empty = None;
        let (head, body) = match item {
            PanelItem::Projects => self.render_projects_panel(&bot, &theme, cx),
            PanelItem::Project(space_id) => {
                self.render_project_panel(&bot, space_id, &theme, window, cx)?
            }
            PanelItem::NeedsYou
            | PanelItem::Plan(_)
            | PanelItem::Approval(_)
            | PanelItem::Asking { .. }
            | PanelItem::Review(_) => self.render_needs_panel(&bot, item, &theme, window, cx)?,
            PanelItem::Work | PanelItem::WorkItem(_) | PanelItem::Thread(_) => {
                self.render_work_panel(&bot, item, &theme, window, cx)?
            }
            PanelItem::Schedule | PanelItem::Routine(_) | PanelItem::NewRoutine => {
                self.render_schedule_panel(&bot, item, &theme, cx)?
            }
            PanelItem::Notebook => self.render_notebook_panel(&bot, item, &theme, window, cx)?,
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
            // Inset from the card's sides, so the divider under the title
            // stops where the content does instead of running edge to edge.
            .mx(px(16.0))
            // One height for every item, whatever sits left of its title
            // (a 30px tile or a bare status mark): 14px over a tile, 12px
            // under.
            .h(px(56.0))
            .pt(px(2.0))
            .border_b_1()
            .border_color(theme.border)
            .child(match self.board.panel_mark.take() {
                Some(mark) => mark,
                None => glyph.into_any_element(),
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap(px(8.0))
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
                    .children(self.board.panel_title_note.take()),
            )
            .children(self.board.panel_actions.take())
            .child(close);
        // Replacing the content fades the new item in. A thread's
        // transcript scrolls itself, so its body fills the card unscrolled.
        let content_key = SharedString::from(format!("bot-panel-{item:?}"));
        // An empty item: what its body still shows (a filter, say) on top,
        // the message centred in the rest.
        let body = if let Some(empty) = self.board.panel_empty.take() {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(div().flex_none().child(body))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(32.0))
                        .pb(px(56.0))
                        .child(motion::fade_quick(content_key, div().child(empty))),
                )
                .into_any_element()
        } else if matches!(item, PanelItem::Thread(_)) {
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
}
