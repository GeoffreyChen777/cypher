//! The Botboard sidebar under the header: the bot picker, a notice line
//! when something is off, then the current bot's cards, top to bottom
//! (§13.1). This file holds the cards' shared shapes, the floating card,
//! and **Projects** (each granted project with its sessions in scope). A
//! card with nothing to show is not drawn; with no bots the sidebar says so and
//! offers a new one. Cards hold no state: everything is read from the main
//! state on render. A card's header opens its whole list in the detail
//! panel ([`super::panel`]); a row opens that item, and stays highlighted
//! while it is open.

use super::*;
use cypher_proto::{Chat, Space};

/// The cards' inner inset and corner radius (the project cards' own).
const CARD_PAD: f32 = 12.0;
const CARD_RADIUS: f32 = 12.0;

/// One granted project on the Projects card: its sessions in the bot's
/// scope, and how many of them run or ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProjectLoad {
    pub space_id: String,
    pub sessions: usize,
    pub running: usize,
    pub asking: usize,
}

/// The projects `scope` grants, in `spaces` order (every one under "All
/// projects"), each with its sessions in scope ([`scoped_sessions`]) counted
/// by `status`.
pub(super) fn project_loads(
    scope: &BotScope,
    spaces: &[&Space],
    chats: &[Chat],
    status: impl Fn(&Chat) -> ChatIndicator,
) -> Vec<ProjectLoad> {
    let visible = scope.visible_chats(chats);
    spaces
        .iter()
        .filter(|space| scope.sees_space(&space.id))
        .map(|space| {
            let sessions: Vec<&Chat> = visible
                .iter()
                .copied()
                .filter(|c| in_project(c, &space.id))
                .collect();
            let count = |want: ChatIndicator| sessions.iter().filter(|c| status(c) == want).count();
            ProjectLoad {
                space_id: space.id.clone(),
                sessions: sessions.len(),
                running: count(ChatIndicator::Working),
                asking: count(ChatIndicator::AwaitingInput),
            }
        })
        .collect()
}

/// A project's sessions the bot can read, newest activity first: its live
/// top-level sessions (subagent children ride their parent's row).
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

/// A Projects row's right-hand note: "1 asking · 2 running", else "idle".
pub(super) fn load_note(load: &ProjectLoad) -> String {
    let mut parts = Vec::new();
    if load.asking > 0 {
        parts.push(format!("{} asking", load.asking));
    }
    if load.running > 0 {
        parts.push(format!("{} running", load.running));
    }
    if parts.is_empty() {
        "idle".to_string()
    } else {
        parts.join(" · ")
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
        PanelItem::Projects | PanelItem::Project(_) => "projects",
        PanelItem::NeedsYou
        | PanelItem::Plan(_)
        | PanelItem::Approval(_)
        | PanelItem::Asking { .. }
        | PanelItem::Review(_) => "needs",
        PanelItem::Work | PanelItem::WorkItem(_) => "work",
        PanelItem::Schedule | PanelItem::Routine(_) | PanelItem::NewRoutine => "schedule",
        // The log opens from the Work panel and the chat header.
        PanelItem::Activity => "work",
        PanelItem::Network | PanelItem::Connection(_) | PanelItem::Invite => "network",
        // Pinned under the cards, so nothing floats for them.
        PanelItem::NewShortcut | PanelItem::Shortcut(_) => "shortcuts",
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

    /// Each granted project with its sessions in scope; the header opens
    /// the full list, a row opens the project. `None` with nothing granted.
    fn render_projects_card(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = Utc::now();
        let rows: Vec<(ProjectLoad, Space)> = {
            let state = self.state.read(cx);
            let spaces = state.spaces_sorted();
            project_loads(&bot.scope, &spaces, &state.chats, |chat| {
                state.display_status_for(chat, now)
            })
            .into_iter()
            .filter_map(|load| {
                let space = state.space_row(&load.space_id)?.clone();
                Some((load, space))
            })
            .collect()
        };
        if rows.is_empty() {
            return None;
        }
        let open = self.board.panel.clone();
        let header = self.card_header(
            "projects",
            "bot-projects-header",
            "Projects",
            rows.len(),
            PanelItem::Projects,
            None,
            theme,
            cx,
        );
        let rows = rows.into_iter().map(|(load, space)| {
            let item = PanelItem::Project(space.id.clone());
            let selected = open.as_ref() == Some(&item);
            let key = format!("bot-project-{}", space.id);
            let tint = crate::appearance::space_style::space_color(space.color.as_deref(), theme)
                .unwrap_or(theme.text_muted);
            let glyph = crate::appearance::space_style::space_icon(space.icon.as_deref());
            // What runs or asks there, else how many sessions it holds.
            let corner = if load.asking > 0 || load.running > 0 {
                card_corner(
                    load_note(&load),
                    (load.asking > 0).then_some(theme.warning),
                    theme,
                )
            } else {
                card_corner(load.sessions.to_string(), None, theme)
            };
            card_item(
                &key,
                Some(card_glyph(glyph, tint.opacity(0.82))),
                space.display_name().to_string(),
                Some(corner),
                selected,
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
        });
        Some(self.fold_card("projects", header, rows, theme))
    }

    /// Whether `card` is the one floating: the open panel item's card
    /// ([`focused_card`]).
    /// A full card's header, the project card header's twin: 36px tall, its
    /// title in text (semibold only on the focused card, medium elsewhere,
    /// as the project sidebar weighs its projects), and a count only when
    /// `count_tint` gives it one (Needs you). A click folds the card to this row and
    /// back, as a project header does; the arrow at its end, shown on hover
    /// and while the list is open, opens the whole list.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn card_header(
        &self,
        card: &'static str,
        key: &'static str,
        title: &'static str,
        count: usize,
        list: PanelItem,
        count_tint: Option<gpui::Hsla>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let open = self.board.panel.as_ref() == Some(&list);
        let shown = if open { 1.0 } else { motion::hover_t(key) };
        let arrow_key = SharedString::from(format!("{key}-open"));
        let arrow =
            div()
                .id(arrow_key.clone())
                .flex_none()
                .size(px(22.0))
                .mr(px(-5.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(motion::hover_blend(
                    &arrow_key,
                    crate::kit::theme::wash(0.0),
                    crate::kit::theme::wash(0.12),
                ))
                .on_hover(motion::hover_listener(arrow_key.clone()))
                .tooltip(|_, cx| {
                    cx.new(|_| super::super::session::FindTooltip("Show all".into()))
                        .into()
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_panel(list.clone(), cx);
                }))
                .child(icon(icons::ALT_ARROW_RIGHT).size(px(11.0)).text_color(
                    motion::hover_blend(
                        &arrow_key,
                        theme.text_muted.opacity(0.8 * shown),
                        theme.text,
                    ),
                ));
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
            .child(arrow)
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
pub(super) const CARD_TITLE_INSET: f32 = 11.0;

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
    div()
        .id(SharedString::from(key.to_string()))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
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
        .children(corner)
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

    fn space(id: &str) -> Space {
        Space {
            id: id.into(),
            name: Some(id.to_uppercase()),
            ..test_fixtures::space()
        }
    }

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
    fn project_loads_count_sessions_in_scope() {
        let spaces = [space("a"), space("b"), space("c")];
        let spaces: Vec<&Space> = spaces.iter().collect();
        let mut archived = chat("a3", "a", 3);
        archived.archived = true;
        let mut child = chat("a4", "a", 4);
        child.child = Some(cypher_proto::ChildChat {
            parent_chat_id: "a1".into(),
            parent_run_id: "run".into(),
            agent: "worker".into(),
            task: "task".into(),
            mode: cypher_proto::SubagentRunMode::Sync,
            tool_call_id: None,
            profile: cypher_proto::ChildAgentProfile {
                system_prompt: String::new(),
                tools: vec![],
                model: None,
                thinking: None,
            },
        });
        let excluded = chat("b2", "b", 2);
        let mut bot_chat = chat("bot", "a", 5);
        bot_chat.bot = Some(cypher_proto::BotChat::owner("x"));
        let chats = vec![
            chat("a1", "a", 1),
            chat("a2", "a", 2),
            archived,
            child,
            chat("b1", "b", 1),
            excluded,
            chat("c1", "c", 1),
            bot_chat,
        ];
        let status = |c: &Chat| match c.id.as_str() {
            "a1" => ChatIndicator::Working,
            "a2" | "b1" => ChatIndicator::AwaitingInput,
            _ => ChatIndicator::Idle,
        };
        let scope = BotScope {
            exclude_sessions: vec!["b2".into()],
            ..selected(&["a", "b", "gone"])
        };
        let loads = project_loads(&scope, &spaces, &chats, status);
        assert_eq!(
            loads,
            vec![
                ProjectLoad {
                    space_id: "a".into(),
                    sessions: 2,
                    running: 1,
                    asking: 1,
                },
                ProjectLoad {
                    space_id: "b".into(),
                    sessions: 1,
                    running: 0,
                    asking: 1,
                },
            ]
        );
        let all = BotScope {
            projects: ProjectGrant::All,
            ..Default::default()
        };
        let ids: Vec<String> = project_loads(&all, &spaces, &chats, status)
            .into_iter()
            .map(|l| l.space_id)
            .collect();
        assert_eq!(ids, ["a", "b", "c"], "all projects lists every project");
        assert!(project_loads(&BotScope::default(), &spaces, &chats, status).is_empty());
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
    fn project_notes_read_plainly() {
        let load = |running, asking| ProjectLoad {
            space_id: "a".into(),
            sessions: 3,
            running,
            asking,
        };
        assert_eq!(load_note(&load(0, 0)), "idle");
        assert_eq!(load_note(&load(2, 0)), "2 running");
        assert_eq!(load_note(&load(1, 1)), "1 asking · 1 running");
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
