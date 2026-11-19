//! The Botboard sidebar under the header: the bot picker, a notice line
//! when something is off, then the current bot's cards, top to bottom
//! (§13.1). This file holds the cards' shared shapes, the floating card,
//! and **Projects** (each granted project with the sessions the workspace
//! runs there). A
//! card with nothing to show is not drawn; with no bots the sidebar says so and
//! offers a new one. Cards hold no state: everything is read from the main
//! state on render. A card is its list: it shows its first rows and grows
//! in place on "N more" ([`Shell::more_row`]); the detail panel
//! ([`super::panel`]) never lists a card's rows. A row opens that item
//! there, and stays highlighted while it is open.

use super::*;
use cypher_proto::{Chat, Space};

/// The cards' inner inset and corner radius (the project cards' own).
const CARD_PAD: f32 = 12.0;
const CARD_RADIUS: f32 = 12.0;

/// The sessions a project shows on the Projects card before "N more".
const PROJECT_SESSIONS: usize = 6;

/// A project's key among the expanded lists.
fn project_key(space_id: &str) -> String {
    format!("project-{space_id}")
}

/// What a card's header offers at its end, on hover.
pub(super) enum HeaderAction {
    /// The card's own page in the panel (Needs you, where its items are
    /// answered); it stays shown while that page is open.
    Page(PanelItem),
    /// Add a routine: the form, in the panel.
    NewRoutine,
}

/// One project on the Projects card with the workspace's sessions there.
struct ProjectSessions {
    space: Space,
    /// The newest [`PROJECT_SESSIONS`] of them, or all once expanded.
    sessions: Vec<SessionLine>,
    total: usize,
}

/// A workspace session's row under its project.
struct SessionLine {
    id: String,
    title: String,
    status: ChatIndicator,
    time_ago: String,
}

/// The owner's sessions in a project that the bot can read, newest activity
/// first: its live top-level sessions (subagent children ride their
/// parent's row; a workspace's own sessions are listed apart).
pub(super) fn scoped_sessions<'a>(
    scope: &BotScope,
    chats: &'a [Chat],
    space_id: &str,
) -> Vec<&'a Chat> {
    let mut sessions: Vec<&Chat> = scope
        .visible_chats(chats)
        .into_iter()
        .filter(|c| in_project(c, space_id))
        .collect();
    sessions.sort_by_key(|c| std::cmp::Reverse(c.last_message_at.unwrap_or(c.created_at)));
    sessions
}

fn in_project(chat: &Chat, space_id: &str) -> bool {
    !chat.archived
        && !chat.is_child()
        && !chat.is_workspace_session()
        && !chat.is_memory_pass()
        && chat.space_id.as_deref() == Some(space_id)
}

/// Something off about the bot, said once at the top of the board: it is
/// paused, its host is unreachable, or its last turn failed. The normal
/// case says nothing, as the project sidebar marks only an offline host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BoardNotice {
    Paused,
    Offline(String),
    Failed,
}

pub(super) fn board_notice(
    paused: bool,
    host_online: bool,
    host: &str,
    chat: ChatIndicator,
) -> Option<BoardNotice> {
    if paused {
        Some(BoardNotice::Paused)
    } else if !host_online {
        Some(BoardNotice::Offline(host.to_string()))
    } else if chat == ChatIndicator::Errored {
        Some(BoardNotice::Failed)
    } else {
        None
    }
}

/// A card's shape. Like the project sidebar's groups, cards are bare: one
/// plate floats under the focused card ([`focused_card`]) and glides when
/// focus moves.
pub(super) fn card(_theme: &Theme) -> gpui::Div {
    div().rounded(px(CARD_RADIUS)).flex().flex_col()
}

/// The card a panel item belongs to (its row or header opened it). Hidden
/// threads go to what owns them; `thread_kind` reads a thread chat's kind.
pub(super) fn card_of(
    item: &PanelItem,
    thread_kind: impl Fn(&str) -> Option<cypher_proto::BotThread>,
) -> &'static str {
    match item {
        PanelItem::Project(_) | PanelItem::Session(_) => "projects",
        PanelItem::NeedsYou | PanelItem::Plan(_) | PanelItem::Review(_) => "needs",
        PanelItem::WorkItem(_) => "work",
        // A result notice names a routine's run or an ended plan.
        PanelItem::Post(id) if id.starts_with("routine-") => "schedule",
        PanelItem::Post(_) => "work",
        PanelItem::Routine(_) | PanelItem::NewRoutine => "schedule",
        // The log opens from the Work panel and the chat header.
        PanelItem::Activity => "work",
        PanelItem::Network
        | PanelItem::Connection(_)
        | PanelItem::Invite
        | PanelItem::Question(_) => "network",
        PanelItem::Thread(chat_id) => match thread_kind(chat_id) {
            Some(
                cypher_proto::BotThread::Routine { .. }
                | cypher_proto::BotThread::RoutineSession { .. },
            ) => "schedule",
            Some(cypher_proto::BotThread::Peer { .. }) => "network",
            _ => "work",
        },
    }
}

/// The card that floats: the open panel item's, while it is drawn. With
/// the panel shut nothing floats, as no project does without a selected
/// session; only the no-bots card stands on its own.
pub(super) fn focused_card(
    wanted: Option<&'static str>,
    drawn: &[&'static str],
) -> Option<&'static str> {
    wanted
        .filter(|key| drawn.contains(key))
        .or_else(|| drawn.contains(&"empty").then_some("empty"))
}

impl Shell {
    /// Everything under the sidebar header on the Botboard.
    pub(in crate::shell) fn render_board_sidebar(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (synced, bot) = {
            let state = self.state.read(cx);
            (
                state.bots_synced,
                current_bot(&state.bots, self.settings.botboard_bot.as_deref()).cloned(),
            )
        };
        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(GROUP_CARD_GAP))
            .pb(px(Theme::SPACE_SM));
        if let Some(notice) = bot
            .as_ref()
            .and_then(|bot| self.render_board_notice(bot, theme, cx))
        {
            column = column.child(notice);
        }
        self.board.plate.begin_render();
        // §13.1's order: Needs you first, so attention is never below the
        // fold. A card with nothing to show is `None` and not drawn.
        let cards: Vec<(&'static str, AnyElement)> = match bot {
            Some(bot) => [
                ("needs", self.render_needs_card(&bot, theme, cx)),
                ("work", self.render_work_card(&bot, theme, cx)),
                ("schedule", self.render_schedule_card(&bot, theme, cx)),
                ("projects", self.render_projects_card(&bot, theme, cx)),
                ("network", self.render_network_card(&bot, theme, cx)),
            ]
            .into_iter()
            .filter_map(|(key, card)| card.map(|card| (key, card)))
            .collect(),
            None if synced => vec![("empty", render_board_empty(theme, cx))],
            None => Vec::new(),
        };
        let wanted = self.board.panel.as_ref().map(|item| {
            let state = self.state.read(cx);
            card_of(item, |chat_id| {
                state
                    .chats
                    .iter()
                    .find(|c| c.id == chat_id)
                    .and_then(|c| c.bot.as_ref())
                    .map(|marker| marker.thread.clone())
            })
        });
        let drawn: Vec<&'static str> = cards.iter().map(|(key, _)| *key).collect();
        let focused = focused_card(wanted, &drawn);
        if focused != self.board.focused_card {
            let previous = std::mem::replace(&mut self.board.focused_card, focused);
            // Never on first paint: the plate appears where it belongs.
            if previous.is_some() {
                self.board.focus_tween = Some(WidthTween::new(0.0, 1.0));
                self.board.plate.begin_glide();
            }
        }
        let plate = self.board.plate.element(
            self.board.focused_card.map(str::to_string),
            self.eval_tween_with(self.board.focus_tween, 1.0, super::super::RESORT),
            theme.clone(),
        );
        let list = div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(GROUP_CARD_GAP))
            // First, so it paints under every card after they all
            // prepainted.
            .child(plate)
            .children(cards.into_iter().map(|(key, card)| {
                div()
                    .relative()
                    .child(self.board.plate.probe(key.to_string()))
                    .child(card)
                    .into_any_element()
            }));
        column.child(list).into_any_element()
    }

    /// The notice line under the picker ([`board_notice`]), `None` while
    /// all is well. A pause offers Resume in place.
    fn render_board_notice(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let notice = {
            let state = self.state.read(cx);
            let now = Utc::now();
            let chat = state
                .chats
                .iter()
                .find(|c| c.id == bot.chat_id)
                .map(|chat| state.display_status_for(chat, now))
                .unwrap_or(ChatIndicator::Idle);
            board_notice(
                bot.paused,
                state.device_online(&bot.host_device_id, now),
                state
                    .device_name(&bot.host_device_id)
                    .unwrap_or("Its device"),
                chat,
            )
        }?;
        let (glyph, text, tint) = match &notice {
            BoardNotice::Paused => (
                icons::PAUSE,
                format!("{} is paused", bot.name),
                theme.warning,
            ),
            BoardNotice::Offline(host) => (
                icons::CLOUD_OFF,
                format!("{host} is offline"),
                theme.text_muted,
            ),
            BoardNotice::Failed => (
                icons::DANGER_TRIANGLE,
                "The last run failed".to_string(),
                theme.danger,
            ),
        };
        let resume = (notice == BoardNotice::Paused).then(|| {
            let bot_id = bot.id.clone();
            let key = "bot-notice-resume";
            div()
                .id(key)
                .flex_none()
                .cursor_pointer()
                .text_color(motion::hover_blend(key, theme.text_muted, theme.text))
                .on_hover(motion::hover_listener(key))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.mutate(
                        serde_json::json!({
                            "op": "setBotPaused", "botId": bot_id, "paused": false,
                        }),
                        cx,
                    )
                }))
                .child(SharedString::from("Resume"))
        });
        Some(
            div()
                .h(px(28.0))
                .px(px(10.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(7.0))
                .text_size(px(12.0))
                .child(
                    div()
                        .flex_none()
                        .size(px(20.0))
                        .ml(px(-3.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(glyph).size(px(13.0)).text_color(tint)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(tint)
                        .child(SharedString::from(text)),
                )
                .children(resume)
                .into_any_element(),
        )
    }

    /// The projects the workspace covers, each with the sessions the
    /// workspace runs there (the ones its plans started, its project
    /// routines'), the way the Sessions view lists a project's sessions:
    /// they are listed nowhere else. A project shows its newest few and
    /// the rest in place on "N more". A project's row opens its page, a
    /// session's the session, live, in the panel. `None` with nothing
    /// granted.
    fn render_projects_card(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = Utc::now();
        let ticker = self.pulse_ticker.entity_id();
        let projects: Vec<ProjectSessions> = {
            let state = self.state.read(cx);
            state
                .spaces_sorted()
                .into_iter()
                .filter(|space| bot.scope.sees_space(&space.id))
                .map(|space| {
                    let sessions = state.workspace_sessions(&bot.id, &space.id);
                    // Its newest few, or all of them once expanded.
                    let shown = if self.board.expanded.contains(&project_key(&space.id)) {
                        sessions.len()
                    } else {
                        PROJECT_SESSIONS
                    };
                    ProjectSessions {
                        space: space.clone(),
                        total: sessions.len(),
                        sessions: sessions
                            .into_iter()
                            .take(shown)
                            .map(|chat| SessionLine {
                                id: chat.id.clone(),
                                title: transcript::single_line(&super::items::plain_markdown(
                                    chat.title.as_deref().unwrap_or("New session"),
                                )),
                                status: state.display_status_for(chat, now),
                                time_ago: format_time_ago(
                                    chat.last_message_at.unwrap_or(chat.created_at),
                                    now,
                                ),
                            })
                            .collect(),
                    }
                })
                .collect()
        };
        if projects.is_empty() {
            return None;
        }
        let open = self.board.panel.clone();
        let header = self.card_header(
            "projects",
            "bot-projects-header",
            "Projects",
            projects.len(),
            None,
            None,
            theme,
            cx,
        );
        let mut rows: Vec<AnyElement> = Vec::new();
        for project in projects {
            let space = project.space;
            let item = PanelItem::Project(space.id.clone());
            let selected = open.as_ref() == Some(&item);
            let tint = crate::appearance::space_style::space_color(space.color.as_deref(), theme)
                .unwrap_or(theme.text_muted);
            let glyph = crate::appearance::space_style::space_icon(space.icon.as_deref());
            let project_item = item.clone();
            rows.push(
                card_item(
                    &format!("bot-project-{}", space.id),
                    Some(card_glyph(glyph, tint.opacity(0.82))),
                    space.display_name().to_string(),
                    None,
                    selected,
                    theme,
                )
                .font_weight(gpui::FontWeight::MEDIUM)
                .on_click(
                    cx.listener(move |this, _, _, cx| this.open_panel(project_item.clone(), cx)),
                )
                .into_any_element(),
            );
            let more = project
                .total
                .saturating_sub(project.sessions.len().min(PROJECT_SESSIONS));
            for session in project.sessions {
                let item = PanelItem::Session(session.id.clone());
                let menu_id = session.id.clone();
                let selected = open.as_ref() == Some(&item);
                let corner = super::super::render::status_mark(session.status, ticker, theme)
                    .unwrap_or_else(|| card_corner(session.time_ago, None, theme));
                rows.push(
                    card_item(
                        &format!("bot-session-{}", session.id),
                        None,
                        session.title,
                        Some(corner),
                        selected,
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
                    // A session's own menu (rename, archive, delete), as on
                    // its row in the Sessions view.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.menus.chat.open((menu_id.clone(), event.position));
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
                );
            }
            rows.extend(self.more_row(project_key(&space.id), more, theme, cx));
        }
        Some(self.fold_card("projects", header, rows, theme))
    }

    /// A full card's header, the project card header's twin: 36px tall, its
    /// title in text (semibold only on the focused card, medium elsewhere,
    /// as the project sidebar weighs its projects), and a count only when
    /// `count_tint` gives it one (Needs you). A click folds the card to this
    /// row and back, as a project header does. `action` is the one small
    /// button at its end, shown on hover. A header never opens the card's
    /// list in the panel: the list is the card, which grows in place
    /// ([`Self::more_row`]).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn card_header(
        &self,
        card: &'static str,
        key: &'static str,
        title: &'static str,
        count: usize,
        action: Option<HeaderAction>,
        count_tint: Option<gpui::Hsla>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let button = action.map(|action| {
            let (glyph, tip, open) = match &action {
                HeaderAction::Page(page) => (
                    icons::ALT_ARROW_RIGHT,
                    "Open",
                    self.board.panel.as_ref() == Some(page),
                ),
                HeaderAction::NewRoutine => (icons::PLUS, "New routine", false),
            };
            let shown = if open { 1.0 } else { motion::hover_t(key) };
            let button_key = SharedString::from(format!("{key}-action"));
            div()
                .id(button_key.clone())
                .flex_none()
                .size(px(22.0))
                .mr(px(-5.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(motion::hover_blend(
                    &button_key,
                    crate::kit::theme::wash(0.0),
                    crate::kit::theme::wash(0.12),
                ))
                .on_hover(motion::hover_listener(button_key.clone()))
                .tooltip(move |_, cx| {
                    cx.new(|_| super::super::session::FindTooltip(tip.into()))
                        .into()
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    match &action {
                        HeaderAction::Page(page) => this.open_panel(page.clone(), cx),
                        HeaderAction::NewRoutine => this.open_job_form(
                            super::schedule::FormTarget::NewRoutine,
                            Some(window),
                            cx,
                        ),
                    }
                }))
                .child(icon(glyph).size(px(11.0)).text_color(motion::hover_blend(
                    &button_key,
                    theme.text_muted.opacity(0.8 * shown),
                    theme.text,
                )))
        });
        header_frame(key, title, self.card_focused(card, cx), theme)
            .pr(px(7.0))
            .cursor_pointer()
            .on_hover(motion::hover_listener(key))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_card(card, cx)))
            // Only a count that asks for attention (Needs you's amber); the
            // rows already show how many there are.
            .children(count_tint.map(|tint| {
                div()
                    .flex_none()
                    .text_size(px(11.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(tint)
                    .child(SharedString::from(count.to_string()))
            }))
            .child(div().flex_1())
            .children(button)
    }

    /// The last row of a list that holds more than it shows: "N more"
    /// brings the rest in, in place, and "Show less" puts them away again.
    /// `None` when the list is whole and was never expanded. `key` names
    /// the list among the expanded ones; `hidden` is what the short list
    /// leaves out.
    pub(super) fn more_row(
        &self,
        key: String,
        hidden: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let expanded = self.board.expanded.contains(&key);
        if hidden == 0 {
            return None;
        }
        let label = if expanded {
            "Show less".to_string()
        } else {
            format!("{hidden} more")
        };
        // Each of its two faces is a row of its own (id and hover fade).
        let row_key = format!("bot-{}-{key}", if expanded { "less" } else { "more" });
        let fade_key = row_key.clone();
        Some(
            more_label(&row_key, label, theme)
                .on_click(cx.listener(move |this, _, _, cx| {
                    // The click moves the row out from under the pointer
                    // (the list grows or shrinks), and nothing tells a row
                    // that the pointer left it that way: without this its
                    // hover tint stays on, here and when it comes back.
                    motion::set_hover(&fade_key, false, true);
                    if !this.board.expanded.remove(&key) {
                        this.board.expanded.insert(key.clone());
                    }
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// Whether the list `key` shows every row ([`Self::more_row`]).
    pub(super) fn list_expanded(&self, key: &str) -> bool {
        self.board.expanded.contains(key)
    }

    /// A full card: its header over its rows, or the header alone while it
    /// is folded.
    pub(super) fn fold_card(
        &self,
        card: &'static str,
        header: impl IntoElement,
        rows: impl IntoIterator<Item = impl IntoElement>,
        theme: &Theme,
    ) -> AnyElement {
        if self.board.collapsed.contains(card) {
            return super::board::card(theme).child(header).into_any_element();
        }
        card_with_rows(header, rows, theme)
    }

    fn toggle_card(&mut self, card: &'static str, cx: &mut Context<Self>) {
        if !self.board.collapsed.remove(card) {
            self.board.collapsed.insert(card);
        }
        cx.notify();
    }

    /// Whether `card` is the one floating: the open panel item's card
    /// ([`focused_card`]).
    pub(super) fn card_focused(&self, card: &str, cx: &App) -> bool {
        let wanted = self.board.panel.as_ref().map(|item| {
            let state = self.state.read(cx);
            card_of(item, |chat_id| {
                state
                    .chats
                    .iter()
                    .find(|c| c.id == chat_id)
                    .and_then(|c| c.bot.as_ref())
                    .map(|marker| marker.thread.clone())
            })
        });
        wanted == Some(card)
    }
}

/// The header row both full and empty cards start from: glyph and title on
/// the project header's grid.
/// A card row's margin inside the card, and where its mark starts inside
/// the row: together the card title's inset, 11px.
pub(super) const ROW_MARGIN: f32 = 4.0;
pub(super) const ROW_MARK_INSET: f32 = CARD_TITLE_INSET - ROW_MARGIN;
/// The gap between a row's mark and its title: the title then starts where
/// the switcher's workspace name does.
const ROW_MARK_GAP: f32 = 9.0;

/// Where a card's title starts: under the workspace switcher's icon (19px
/// from the window edge; the card itself starts at 8), its rows indented
/// below it.
const CARD_TITLE_INSET: f32 = 11.0;

pub(super) fn header_frame(
    key: &'static str,
    title: &'static str,
    focused: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(key)
        .h(px(GROUP_CARD_HEADER_HEIGHT))
        .flex_none()
        // Text only (the icons stay on the rows): a heading flush with the
        // switcher's icon, its rows indented below it.
        .pl(px(CARD_TITLE_INSET))
        .pr(px(10.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(7.0))
        .child(
            div()
                .flex_none()
                .text_size(px(12.5))
                .line_height(px(18.0))
                .font_weight(if focused {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::MEDIUM
                })
                .text_color(theme.text)
                .child(SharedString::from(title)),
        )
}

/// One item on a card, drawn like a session row under its project: a
/// single line, its small mark under the card's title and the switcher's
/// icon (or, without one, its title on the titles' column), the title at
/// the session rows' 12.5 and 80% ink until hovered or open, and a quiet
/// corner for its time or state. Detail lives in the panel, not here.
pub(super) fn card_item(
    key: &str,
    mark: Option<AnyElement>,
    title: impl Into<SharedString>,
    corner: Option<AnyElement>,
    selected: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    card_item_detail(key, mark, title, corner, None, selected, theme)
}

/// [`card_item`] with `detail` under its title, on the titles' column: a
/// running item's current step over its small progress bar. The mark and
/// the corner stay on the title's line.
pub(super) fn card_item_detail(
    key: &str,
    mark: Option<AnyElement>,
    title: impl Into<SharedString>,
    corner: Option<AnyElement>,
    detail: Option<AnyElement>,
    selected: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let hover = crate::appearance::surface_style::sidebar_hover(theme);
    let selected_wash = crate::appearance::surface_style::sidebar_selected(theme);
    let rest_bg = if selected {
        selected_wash
    } else {
        crate::kit::theme::wash(0.0)
    };
    let hover_bg = if selected { selected_wash } else { hover };
    let rest_text = if selected {
        theme.text
    } else {
        theme.text.opacity(0.8)
    };
    let has_mark = mark.is_some();
    let line = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(ROW_MARK_GAP))
                .when_some(mark, |el, mark| {
                    el.child(
                        div()
                            .flex_none()
                            .size(px(14.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(mark),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.5))
                        .line_height(px(16.0))
                        .child(title.into()),
                ),
        )
        .children(corner);
    div()
        .id(SharedString::from(key.to_string()))
        .flex()
        .flex_col()
        .rounded(px(8.0))
        .mx(px(ROW_MARGIN))
        .mb(px(2.0))
        // The mark under the switcher's icon (and the card's title), the
        // title under the workspace's name; a row without a mark keeps its
        // title on that column.
        .pl(px(if has_mark {
            ROW_MARK_INSET
        } else {
            ROW_MARK_INSET + 14.0 + ROW_MARK_GAP
        }))
        .pr(px(8.0))
        .py(px(6.0))
        .cursor_pointer()
        .text_color(motion::hover_blend(key, rest_text, theme.text))
        .bg(motion::hover_blend(key, rest_bg, hover_bg))
        .on_hover(motion::hover_listener(key.to_string()))
        .child(line)
        .children(detail.map(|detail| {
            div()
                .pl(px(if has_mark { 14.0 + ROW_MARK_GAP } else { 0.0 }))
                .child(detail)
        }))
}

/// The quiet last line of a list that holds more than it shows ("2 more",
/// "Show less"): a small label under the rows' titles, in the corners'
/// faint ink, with no wash of its own. Hovering it only firms the ink up.
pub(super) fn more_label(
    key: &str,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(key.to_string()))
        .flex()
        .flex_row()
        .mx(px(ROW_MARGIN))
        .mb(px(2.0))
        // On the titles' column, like a row without a mark.
        .pl(px(ROW_MARK_INSET + 14.0 + ROW_MARK_GAP))
        .pr(px(8.0))
        .py(px(4.0))
        .cursor_pointer()
        .text_size(px(10.5))
        .line_height(px(14.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(motion::hover_blend(
            key,
            theme.text_muted.opacity(0.5),
            theme.text_muted,
        ))
        .on_hover(motion::hover_listener(key.to_string()))
        .child(label.into())
}

/// A card row's corner: the session rows' 10px time-and-state note, in the
/// sub-line ink unless `color` says otherwise (amber when something asks).
pub(super) fn card_corner(
    text: impl Into<SharedString>,
    color: Option<gpui::Hsla>,
    theme: &Theme,
) -> AnyElement {
    div()
        .flex_none()
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(color.unwrap_or(theme.text_muted.opacity(0.5)))
        .child(text.into())
        .into_any_element()
}

/// A card's mark glyph at the session rows' agent-mark size.
pub(super) fn card_glyph(path: &'static str, tint: gpui::Hsla) -> AnyElement {
    icon(path)
        .size(px(13.0))
        .flex_none()
        .text_color(tint)
        .into_any_element()
}

/// A card: its header over its rows, with the project card's body padding.
pub(super) fn card_with_rows(
    header: impl IntoElement,
    rows: impl IntoIterator<Item = impl IntoElement>,
    theme: &Theme,
) -> AnyElement {
    card(theme)
        .child(header)
        .child(
            div()
                .flex()
                .flex_col()
                .pb(px(GROUP_CARD_BODY_PADDING))
                .children(rows),
        )
        .into_any_element()
}

/// A row's title over its muted sub-line, taking the row's free width. The
/// sub-line's lead (a device name) truncates before its `tail` does.
pub(super) fn two_lines(title: String, sub: String, tail: String, theme: &Theme) -> gpui::Div {
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
                .text_size(px(13.0))
                .line_height(px(17.0))
                .text_color(theme.text)
                .child(SharedString::from(title)),
        )
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_row()
                .gap(px(3.0))
                .text_size(px(11.0))
                .line_height(px(14.0))
                .text_color(theme.text_muted.opacity(0.85))
                .child(div().min_w_0().truncate().child(SharedString::from(sub)))
                .when(!tail.is_empty(), |el| {
                    el.child(div().flex_none().child(SharedString::from(tail)))
                }),
        )
}

/// No bots yet: one line and the way to make one.
fn render_board_empty(theme: &Theme, cx: &mut Context<Shell>) -> AnyElement {
    card(theme)
        .p(px(CARD_PAD))
        .gap(px(4.0))
        .child(
            div()
                .text_size(px(13.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(SharedString::from("No workspaces yet")),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(
                    "A workspace looks after the projects you give it: it answers, plans and runs work.",
                )),
        )
        .child(
            div().mt(px(8.0)).flex().child(
                popover::btn_primary(theme, "New workspace")
                    .id("board-empty-new-bot")
                    .on_click(cx.listener(|this, _, _, cx| this.open_bot_dialog(None, cx))),
            ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures;

    fn chat(id: &str, space: &str, minutes: i64) -> Chat {
        Chat {
            id: id.into(),
            space_id: Some(space.into()),
            created_at: chrono::DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::minutes(minutes),
            ..test_fixtures::chat()
        }
    }

    fn selected(ids: &[&str]) -> BotScope {
        BotScope {
            projects: ProjectGrant::Selected {
                space_ids: ids.iter().map(|id| (*id).into()).collect(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn one_card_floats_the_open_items_else_status() {
        let none = |_: &str| None;
        assert_eq!(card_of(&PanelItem::Project("a".into()), none), "projects");
        assert_eq!(card_of(&PanelItem::Plan("w".into()), none), "needs");
        assert_eq!(card_of(&PanelItem::Routine("r".into()), none), "schedule");
        let routine = |_: &str| {
            Some(cypher_proto::BotThread::Routine {
                routine_id: "r".into(),
            })
        };
        assert_eq!(card_of(&PanelItem::Thread("t".into()), routine), "schedule");
        let session = |_: &str| {
            Some(cypher_proto::BotThread::RoutineSession {
                routine_id: "r".into(),
            })
        };
        assert_eq!(card_of(&PanelItem::Thread("t".into()), session), "schedule");
        assert_eq!(card_of(&PanelItem::Thread("t".into()), none), "work");
        let drawn = ["needs", "work", "projects"];
        assert_eq!(focused_card(None, &drawn), None, "nothing floats");
        assert_eq!(focused_card(Some("work"), &drawn), Some("work"));
        assert_eq!(focused_card(Some("schedule"), &drawn), None);
        assert_eq!(focused_card(None, &["empty"]), Some("empty"));
    }

    #[test]
    fn scoped_sessions_are_newest_first() {
        let mut newer = chat("old", "a", 1);
        newer.last_message_at =
            Some(chrono::DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::hours(1));
        let chats = vec![newer, chat("mid", "a", 30), chat("b", "b", 40)];
        let ids: Vec<&str> = scoped_sessions(&selected(&["a", "b"]), &chats, "a")
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, ["old", "mid"]);
    }

    #[test]
    fn only_something_off_earns_a_notice() {
        let ok = ChatIndicator::Idle;
        assert_eq!(board_notice(false, true, "Mac", ok), None);
        assert_eq!(
            board_notice(false, false, "Mac", ok),
            Some(BoardNotice::Offline("Mac".into()))
        );
        assert_eq!(
            board_notice(true, false, "Mac", ChatIndicator::Errored),
            Some(BoardNotice::Paused),
            "a pause says the most"
        );
        assert_eq!(
            board_notice(false, true, "Mac", ChatIndicator::Errored),
            Some(BoardNotice::Failed)
        );
    }
}
