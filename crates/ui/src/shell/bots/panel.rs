//! The Botboard's detail panel (§13.1): one item from the sidebar's cards in
//! full. It slides in from the window's right edge while the chat narrows
//! ([`super::column`]), opens and closes with the button at the right of
//! the chat's header (Esc closes it too), and opening another row replaces
//! its content; the opened row stays highlighted in the sidebar.
//!
//! An item is one framed card under its title. The Needs you page is the
//! exception: its cards stand in the column on their own.

use super::*;

/// What the panel shows: one item in full (a row's, a card's own page).
/// Never a card's list: the sidebar holds the lists, and expands them in
/// place. Ids are record ids unless named otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::shell) enum PanelItem {
    /// One project's sessions in the bot's scope, by space id.
    Project(String),
    /// Everything that needs the owner, each item answered in place.
    NeedsYou,
    /// A plan awaiting approval (work id).
    Plan(String),
    /// A peer answer drafted in Review mode (exchange id).
    Review(String),
    /// One of the workspace's sessions in a project (a session a plan
    /// started, a project routine's), live: its transcript and composer,
    /// by chat id.
    Session(String),
    /// A result notice from the chat, in full (the post's entry id).
    Post(String),
    /// One question between workspaces, either way, opened from the
    /// Network panel (exchange id); a waiting review shows as `Review`.
    Question(String),
    /// One orchestration (work id).
    WorkItem(String),
    /// One of the bot's hidden threads, read-only (its chat id): an
    /// orchestration's, a routine's or a connection's.
    Thread(String),
    /// One routine.
    Routine(String),
    /// The new-routine form.
    NewRoutine,
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
        self.board.panel_last = None;
        self.drop_panel_extras();
        self.sync_panel_session(cx);
        cx.notify();
    }

    /// The chat header's button: shut the panel, or open it on what it
    /// last showed (the Needs you page when that is nothing).
    pub(in crate::shell) fn toggle_panel(&mut self, cx: &mut Context<Self>) {
        if self.board.panel.is_some() {
            return self.close_panel(cx);
        }
        let item = self
            .board
            .panel_recent
            .clone()
            .unwrap_or(PanelItem::NeedsYou);
        self.open_panel(item, cx);
    }

    /// What an item kept while it was open (the sessions it looked into, a
    /// draft being edited) goes with it.
    pub(super) fn drop_panel_extras(&mut self) {
        match self.board.panel.as_ref() {
            // The page prunes its own, to the sessions that still ask.
            Some(PanelItem::NeedsYou) => {}
            Some(PanelItem::Thread(chat_id)) => {
                let keep = chat_id.clone();
                self.board.peeks.retain(|id, _| *id == keep);
            }
            _ => self.board.peeks.clear(),
        }
        if !matches!(
            self.board.panel,
            Some(PanelItem::Review(_) | PanelItem::Question(_))
        ) {
            self.board.review = None;
        }
        if self.board.panel != Some(PanelItem::NeedsYou) {
            self.board.needs_focus = None;
        }
    }

    /// Keep the live session slot on the session the panel shows, or showed
    /// while it slides shut: made when a session opens, dropped once the
    /// panel shows something else or is shut. Its docks start closed: the
    /// panel has room for the conversation only.
    pub(super) fn sync_panel_session(&mut self, cx: &mut Context<Self>) {
        let wanted = match self.board.panel.as_ref().or(self.board.panel_last.as_ref()) {
            Some(PanelItem::Session(chat_id)) => Some(chat_id.clone()),
            _ => None,
        };
        let bound = self
            .board
            .session_slot
            .and_then(|sid| self.tiles.slots.get(&sid))
            .and_then(|slot| slot.tab.chat_id().map(str::to_string));
        if bound == wanted && (wanted.is_none() || self.board.session_slot.is_some()) {
            return;
        }
        if let Some(sid) = self.board.session_slot.take() {
            self.dispose_slot(sid, cx);
        }
        if let Some(chat_id) = wanted {
            let sid = self.create_slot(crate::workspace::TabKey::Session(chat_id), cx);
            if let Some(slot) = self.tiles.slots.get_mut(&sid) {
                slot.dock.open = false;
                slot.terminal_open = false;
            }
            self.board.session_slot = Some(sid);
        }
        self.dismiss_hidden_comment_ui(cx);
    }

    /// Slide the panel shut, its content kept until the tween ends.
    pub(in crate::shell) fn close_panel(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.board.panel.take() else {
            return;
        };
        let from = self.eval_tween(self.board.panel_tween, self.panel_span());
        self.board.panel_tween = Some(WidthTween::new(from, 0.0));
        // A form shut is a form dropped: the button does not bring it back.
        self.board.panel_recent =
            (!matches!(item, PanelItem::NewRoutine | PanelItem::Invite)).then(|| item.clone());
        self.board.panel_last = Some(item);
        // A live session's slot stays for the slide; the rest goes now.
        self.sync_panel_session(cx);
        cx.notify();
    }

    /// Esc anywhere in the window closes an open panel, unless a bot dialog
    /// over it takes the key (or a shortcut's menu, which it closes
    /// instead). Returns whether it did.
    pub(in crate::shell) fn close_panel_on_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.close_shortcut_menu_on_escape(cx) {
            return true;
        }
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
    pub(super) fn close_panel_now(&mut self, cx: &mut Context<Self>) {
        self.board.panel = None;
        self.board.panel_last = None;
        self.board.panel_recent = None;
        self.board.panel_tween = None;
        self.drop_panel_extras();
        self.sync_panel_session(cx);
    }

    /// The panel card for `item`, `None` when it no longer resolves (the
    /// project went away, or the bot).
    pub(super) fn render_panel_card(
        &mut self,
        item: &PanelItem,
        window: &mut Window,
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
            PanelItem::Project(space_id) => {
                self.render_project_panel(&bot, space_id, &theme, window, cx)?
            }
            // The Needs you page: its cards alone, no frame or title.
            PanelItem::NeedsYou => {
                let page = self.render_needs_page(&bot, &theme, window, cx);
                return Some(motion::fade_quick("bot-panel-needs", page).into_any_element());
            }
            // A plan is a work item waiting on its approval: one page.
            PanelItem::Plan(id) => self.render_work_item(id, &theme, window, cx)?,
            PanelItem::Review(id) => self.render_question_panel(id, &theme, cx)?,
            PanelItem::WorkItem(_) | PanelItem::Thread(_) => {
                self.render_work_panel(item, &theme, window, cx)?
            }
            PanelItem::Session(chat_id) => {
                self.render_session_panel(&bot, chat_id, &theme, window, cx)?
            }
            PanelItem::Post(entry_id) => self.render_post_panel(entry_id, &theme, window, cx)?,
            PanelItem::Routine(_) | PanelItem::NewRoutine => {
                self.render_schedule_panel(&bot, item, &theme, cx)?
            }
            PanelItem::Activity => self.render_activity_panel(&bot, item, &theme, cx)?,
            PanelItem::Network | PanelItem::Connection(_) | PanelItem::Invite => {
                self.render_network_panel(&bot, item, &theme, cx)?
            }
            PanelItem::Question(id) => self.render_question_panel(id, &theme, cx)?,
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
            .children(self.board.panel_actions.take());
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
        } else if matches!(item, PanelItem::Thread(_) | PanelItem::Session(_)) {
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
