//! The Botboard sidebar under the header: the bot picker, then the current
//! bot's cards, top to bottom (§13.1). This slice draws **Status** (who the
//! bot is, where it runs, what it is doing and what it may read) and
//! **Projects** (each granted project with its sessions in scope). A card
//! with nothing to show is not drawn; with no bots the sidebar says so and
//! offers a new one. Cards hold no state: everything is read from the main
//! state on render. A card's header opens its whole list in the detail
//! panel ([`super::panel`]); a row opens that item, and stays highlighted
//! while it is open.

use super::*;
use cypher_proto::{Chat, Space};

/// The cards' inner inset and corner radius (the project cards' own).
const CARD_PAD: f32 = 12.0;
const CARD_RADIUS: f32 = 12.0;
/// The Status card's avatar.
const STATUS_AVATAR: f32 = 38.0;

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
    !chat.archived && !chat.is_child() && chat.space_id.as_deref() == Some(space_id)
}

/// The Status card's permission badges: what the bot may read. `live`
/// says whether a granted project still exists.
pub(super) fn access_badges(scope: &BotScope, live: impl Fn(&str) -> bool) -> Vec<String> {
    let projects = match &scope.projects {
        ProjectGrant::All => "Reads all projects".to_string(),
        ProjectGrant::None => "Reads no projects".to_string(),
        ProjectGrant::Selected { space_ids } => {
            match space_ids.iter().filter(|id| live(id)).count() {
                0 => "Reads no projects".to_string(),
                1 => "Reads 1 project".to_string(),
                n => format!("Reads {n} projects"),
            }
        }
    };
    let mut badges = vec![projects];
    match scope.include_sessions.len() {
        0 => {}
        1 => badges.push("+1 session".to_string()),
        n => badges.push(format!("+{n} sessions")),
    }
    badges.push(
        if scope.transcripts {
            "Reads conversations"
        } else {
            "Titles only"
        }
        .to_string(),
    );
    badges
}

/// How a part of the Status card's line reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusTone {
    Working,
    Waiting,
    Failed,
    Paused,
    Quiet,
}

/// The Status card's line, part by part: what the conversation is doing,
/// then the bot's running work and what needs the owner ("Working… · 1
/// task running · 2 need you"), and the tone its mark takes, the most
/// pressing part's. A question in the conversation counts among `needs`.
pub(super) fn status_summary(
    chat: ChatIndicator,
    running: usize,
    needs: usize,
    paused: bool,
) -> (Vec<(String, StatusTone)>, StatusTone) {
    let mut parts = Vec::new();
    if paused {
        parts.push(("Paused".to_string(), StatusTone::Paused));
    }
    match chat {
        ChatIndicator::Working => parts.push(("Working…".to_string(), StatusTone::Working)),
        ChatIndicator::Errored => {
            parts.push(("The last run failed".to_string(), StatusTone::Failed))
        }
        _ => {}
    }
    if running > 0 {
        parts.push((
            format!(
                "{running} task{} running",
                if running == 1 { "" } else { "s" }
            ),
            StatusTone::Working,
        ));
    }
    if needs > 0 {
        parts.push((
            format!("{needs} need{} you", if needs == 1 { "s" } else { "" }),
            StatusTone::Waiting,
        ));
    }
    if parts.is_empty() {
        parts.push(("Idle".to_string(), StatusTone::Quiet));
    }
    let lead = [
        StatusTone::Waiting,
        StatusTone::Working,
        StatusTone::Failed,
        StatusTone::Paused,
    ]
    .into_iter()
    .find(|tone| parts.iter().any(|(_, t)| t == tone))
    .unwrap_or(StatusTone::Quiet);
    (parts, lead)
}

/// The Status card's acting badges: what the bot may do beyond reading.
pub(super) fn capability_badges(caps: &cypher_proto::BotCapabilities) -> Vec<&'static str> {
    [
        (caps.work_directly, "Works directly"),
        (caps.coordinate, "Can start sessions"),
        (caps.steer, "Can steer sessions"),
    ]
    .into_iter()
    .filter_map(|(on, label)| on.then_some(label))
    .collect()
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

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
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
        PanelItem::Schedule
        | PanelItem::Routine(_)
        | PanelItem::Task(_)
        | PanelItem::NewRoutine
        | PanelItem::NewTask => "schedule",
        PanelItem::Notebook => "notebook",
        PanelItem::Activity => "activity",
        PanelItem::Network | PanelItem::Connection(_) | PanelItem::Invite => "network",
        PanelItem::Thread(chat_id) => match thread_kind(chat_id) {
            Some(cypher_proto::BotThread::Routine { .. }) => "schedule",
            Some(cypher_proto::BotThread::Peer { .. }) => "network",
            _ => "work",
        },
    }
}

/// The card that floats: the open panel item's, else Status (the bot chat
/// beside it, as the project sidebar floats the selected session's
/// project). A card not drawn (Needs you emptied) hands it back to Status.
pub(super) fn focused_card(wanted: Option<&'static str>, drawn: &[&'static str]) -> &'static str {
    wanted.filter(|key| drawn.contains(key)).unwrap_or("status")
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
        if let Some(picker) = self.render_bot_picker(bot.as_ref().map(|b| b.id.as_str()), theme, cx)
        {
            column = column.child(picker);
        }
        self.board.plate.begin_render();
        // §13.1's order: Needs you right under Status, so attention is never
        // below the fold. A card with nothing to show is `None` and not drawn.
        let cards: Vec<(&'static str, AnyElement)> = match bot {
            Some(bot) => [
                ("status", Some(self.render_status_card(&bot, theme, cx))),
                ("needs", self.render_needs_card(&bot, theme, cx)),
                ("work", self.render_work_card(&bot, theme, cx)),
                ("schedule", self.render_schedule_card(&bot, theme, cx)),
                ("projects", self.render_projects_card(&bot, theme, cx)),
                ("notebook", self.render_notebook_card(&bot, theme, cx)),
                ("activity", self.render_activity_card(&bot, theme, cx)),
                ("network", self.render_network_card(&bot, theme, cx)),
            ]
            .into_iter()
            .filter_map(|(key, card)| card.map(|card| (key, card)))
            .collect(),
            None if synced => vec![("status", render_board_empty(theme, cx))],
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
        let focused = (!drawn.is_empty()).then(|| focused_card(wanted, &drawn));
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

    /// Who the bot is, where it runs, what it is doing, and what it may
    /// read; the settings button opens its palette.
    fn render_status_card(&self, bot: &Bot, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let now = Utc::now();
        let (host, online, status, badges, running, needs) = {
            let state = self.state.read(cx);
            let status = state
                .chats
                .iter()
                .find(|c| c.id == bot.chat_id)
                .map(|chat| state.display_status_for(chat, now))
                .unwrap_or(ChatIndicator::Idle);
            (
                state
                    .device_name(&bot.host_device_id)
                    .unwrap_or("Unknown device")
                    .to_string(),
                state.device_online(&bot.host_device_id, now),
                status,
                access_badges(&bot.scope, |id| state.space_row(id).is_some()),
                state
                    .bot_records
                    .work_of(&bot.id)
                    .filter(|w| w.state == cypher_proto::WorkState::Running)
                    .count(),
                bot_needs(state, bot, now).len(),
            )
        };
        let avatar = crate::appearance::bot_style::bot_avatar(
            &bot.name,
            bot.icon.as_deref(),
            bot.color.as_deref(),
            STATUS_AVATAR,
            theme,
        );
        let presence = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(5.0))
            .text_size(px(11.5))
            .text_color(theme.text_muted)
            .child(
                div()
                    .flex_none()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(if online {
                        theme.success
                    } else {
                        theme.text_faint
                    }),
            )
            // A long device name truncates; the presence word stays.
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .gap(px(3.0))
                    .child(div().min_w_0().truncate().child(SharedString::from(host)))
                    .child(div().flex_none().child(SharedString::from(if online {
                        "· online"
                    } else {
                        "· offline"
                    }))),
            );
        let (parts, tone) = status_summary(status, running, needs, bot.paused);
        let tone_color = |tone: StatusTone| match tone {
            StatusTone::Working => theme.text,
            StatusTone::Waiting => theme.warning,
            StatusTone::Failed => theme.danger,
            StatusTone::Paused | StatusTone::Quiet => theme.text_muted,
        };
        let ticker = self.pulse_ticker.entity_id();
        let line_mark = match tone {
            StatusTone::Working => {
                super::super::render::status_mark(ChatIndicator::Working, ticker, theme)
            }
            StatusTone::Waiting => {
                super::super::render::status_mark(ChatIndicator::AwaitingInput, ticker, theme)
            }
            StatusTone::Failed => Some(
                icon(icons::DANGER_TRIANGLE)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(theme.danger)
                    .into_any_element(),
            ),
            StatusTone::Paused => Some(
                icon(icons::PAUSE)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(theme.text_muted)
                    .into_any_element(),
            ),
            StatusTone::Quiet => None,
        };
        let last = parts.len().saturating_sub(1);
        // The mark stays on the first line; a long line wraps part by part
        // beside it.
        let status_row = div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(6.0))
            .text_size(px(12.5))
            .line_height(px(17.0))
            .children(line_mark.map(|mark| {
                div()
                    .flex_none()
                    .h(px(17.0))
                    .flex()
                    .items_center()
                    .child(mark)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .children(parts.into_iter().enumerate().map(|(ix, (text, tone))| {
                        div()
                            .flex_none()
                            .flex()
                            .flex_row()
                            .text_color(tone_color(tone))
                            .child(SharedString::from(text))
                            .when(ix < last, |el| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .px(px(4.0))
                                        .text_color(theme.text_faint)
                                        .child(SharedString::from("·")),
                                )
                            })
                    })),
            );
        // What it may read in the quiet tone; what lets it act in the
        // accent's; a pause in amber.
        let badge = |label: String, fg: gpui::Hsla, bg: gpui::Hsla| {
            div()
                .px(px(7.0))
                .py(px(2.0))
                .rounded(px(6.0))
                .bg(bg)
                .text_size(px(11.0))
                .text_color(fg)
                .child(SharedString::from(label))
        };
        let badge_row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(4.0))
            .when(bot.paused, |el| {
                el.child(badge(
                    "Paused".into(),
                    theme.warning,
                    theme.warning.opacity(0.14),
                ))
            })
            .children(
                badges
                    .into_iter()
                    .map(|label| badge(label, theme.text_muted, crate::kit::theme::ink(0.06))),
            )
            .children(
                capability_badges(&bot.capabilities)
                    .into_iter()
                    .map(|label| badge(label.into(), theme.accent, theme.accent.opacity(0.12))),
            );
        let edit_id = bot.id.clone();
        let settings =
            super::items::icon_button("bot-status-settings", icons::TUNING, "Bot settings", theme)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_bot_dialog(Some(edit_id.clone()), cx)
                }));
        let (pause_glyph, pause_tip) = if bot.paused {
            (icons::PLAY, "Resume: let it run routines and start work")
        } else {
            (icons::PAUSE, "Pause: no routines run and no work starts")
        };
        let (pause_id, paused) = (bot.id.clone(), bot.paused);
        let pause = super::items::icon_button("bot-status-pause", pause_glyph, pause_tip, theme)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.mutate(
                    serde_json::json!({
                        "op": "setBotPaused", "botId": pause_id, "paused": !paused,
                    }),
                    cx,
                )
            }));
        card(theme)
            .p(px(CARD_PAD))
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(avatar)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(14.0))
                                    .line_height(px(18.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child(SharedString::from(transcript::single_line(&bot.name))),
                            )
                            .child(presence),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_row()
                            .child(pause)
                            .child(settings),
                    ),
            )
            .child(status_row)
            .child(badge_row)
            .into_any_element()
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
        let rows: Vec<(ProjectLoad, Space, String)> = {
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
        if rows.is_empty() {
            return None;
        }
        let open = self.board.panel.clone();
        let header = section_header(
            "bot-projects-header",
            icons::FOLDER,
            "Projects",
            rows.len(),
            open == Some(PanelItem::Projects),
            theme,
        )
        .on_click(cx.listener(|this, _, _, cx| this.open_panel(PanelItem::Projects, cx)));
        let rows = rows.into_iter().map(|(load, space, device)| {
            let item = PanelItem::Project(space.id.clone());
            let selected = open.as_ref() == Some(&item);
            let key = format!("bot-project-{}", space.id);
            let note = load_note(&load);
            let note_color = if load.asking > 0 {
                theme.warning
            } else if load.running > 0 {
                theme.text
            } else {
                theme.text_faint
            };
            project_row(&key, &space, selected, theme)
                .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
                .child(two_lines(
                    space.display_name().to_string(),
                    device,
                    format!("· {}", plural(load.sessions, "session")),
                    theme,
                ))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(note_color)
                        .child(SharedString::from(note)),
                )
        });
        Some(
            card(theme)
                .pb(px(4.0))
                .child(header)
                .child(div().px(px(4.0)).flex().flex_col().children(rows))
                .into_any_element(),
        )
    }
}

/// A card's header row: glyph, title and count; a click opens the whole
/// list (highlighted while that list is open).
pub(super) fn section_header(
    key: &'static str,
    glyph: &'static str,
    title: &'static str,
    count: usize,
    open: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    section_header_tinted(key, glyph, title, count, open, None, theme)
}

/// [`section_header`] with its glyph and count in `tint` (the Needs you
/// card's amber).
pub(super) fn section_header_tinted(
    key: &'static str,
    glyph: &'static str,
    title: &'static str,
    count: usize,
    open: bool,
    tint: Option<gpui::Hsla>,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(key)
        .h(px(GROUP_CARD_HEADER_HEIGHT))
        .mx(px(4.0))
        .mt(px(4.0))
        .px(px(8.0))
        .rounded(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .cursor_pointer()
        .bg(if open {
            crate::kit::theme::wash(0.08)
        } else {
            motion::hover_blend(
                key,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.05),
            )
        })
        .on_hover(motion::hover_listener(key))
        .child(
            icon(glyph)
                .size(px(14.0))
                .flex_none()
                .text_color(tint.unwrap_or(theme.text_muted)),
        )
        .child(
            div()
                .text_size(px(13.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(SharedString::from(title)),
        )
        .child(
            div()
                .text_size(px(12.0))
                .when(tint.is_some(), |el| {
                    el.font_weight(gpui::FontWeight::MEDIUM)
                })
                .text_color(tint.unwrap_or(theme.text_muted.opacity(0.75)))
                .child(SharedString::from(count.to_string())),
        )
        .child(div().flex_1())
        .child(
            icon(icons::ALT_ARROW_RIGHT)
                .size(px(11.0))
                .flex_none()
                .text_color(theme.text_muted.opacity(if open { 0.8 } else { 0.0 }))
                .when(!open, |el| {
                    el.text_color(theme.text_muted.opacity(0.8 * motion::hover_t(key)))
                }),
        )
}

/// A project row (the Projects card and the panel's list): the project's
/// own glyph in its colour, then whatever the caller adds.
pub(super) fn project_row(
    key: &str,
    space: &Space,
    selected: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let glyph = crate::appearance::space_style::space_icon(space.icon.as_deref());
    let tint = crate::appearance::space_style::space_color(space.color.as_deref(), theme)
        .unwrap_or(theme.text_muted);
    div()
        .id(SharedString::from(key.to_string()))
        .px(px(8.0))
        .py(px(6.0))
        .rounded(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .cursor_pointer()
        .bg(if selected {
            crate::kit::theme::wash(0.08)
        } else {
            motion::hover_blend(
                key,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.05),
            )
        })
        .on_hover(motion::hover_listener(key.to_string()))
        .child(icon(glyph).size(px(14.0)).flex_none().text_color(tint))
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
                .child(SharedString::from("No bots yet")),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(
                    "A bot answers questions about the projects you give it.",
                )),
        )
        .child(
            div().mt(px(8.0)).flex().child(
                popover::btn_primary(theme, "New bot")
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
        assert_eq!(card_of(&PanelItem::Thread("t".into()), none), "work");
        let drawn = ["status", "work", "projects"];
        assert_eq!(focused_card(None, &drawn), "status");
        assert_eq!(focused_card(Some("work"), &drawn), "work");
        assert_eq!(
            focused_card(Some("needs"), &drawn),
            "status",
            "a card no longer drawn hands focus back"
        );
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
    fn notes_and_badges_read_plainly() {
        let load = |running, asking| ProjectLoad {
            space_id: "a".into(),
            sessions: 3,
            running,
            asking,
        };
        assert_eq!(load_note(&load(0, 0)), "idle");
        assert_eq!(load_note(&load(2, 0)), "2 running");
        assert_eq!(load_note(&load(1, 1)), "1 asking · 1 running");
        let live = |id: &str| id != "gone";
        assert_eq!(
            access_badges(&selected(&["a", "b", "gone"]), live),
            ["Reads 2 projects", "Reads conversations"]
        );
        assert_eq!(
            access_badges(
                &BotScope {
                    transcripts: false,
                    include_sessions: vec!["x".into()],
                    ..selected(&["a"])
                },
                live
            ),
            ["Reads 1 project", "+1 session", "Titles only"]
        );
        assert_eq!(
            access_badges(
                &BotScope {
                    projects: ProjectGrant::All,
                    ..Default::default()
                },
                live
            )[0],
            "Reads all projects"
        );
        assert_eq!(
            access_badges(&BotScope::default(), live)[0],
            "Reads no projects"
        );
    }

    #[test]
    fn the_status_line_combines_the_chat_work_and_needs() {
        let text = |chat, running, needs, paused| {
            let (parts, tone) = status_summary(chat, running, needs, paused);
            let line: Vec<String> = parts.into_iter().map(|(t, _)| t).collect();
            (line.join(" · "), tone)
        };
        assert_eq!(
            text(ChatIndicator::Completed, 0, 0, false),
            ("Idle".to_string(), StatusTone::Quiet)
        );
        assert_eq!(
            text(ChatIndicator::Idle, 1, 2, false),
            (
                "1 task running · 2 need you".to_string(),
                StatusTone::Waiting
            )
        );
        assert_eq!(
            text(ChatIndicator::Working, 2, 0, false),
            (
                "Working… · 2 tasks running".to_string(),
                StatusTone::Working
            )
        );
        assert_eq!(
            text(ChatIndicator::AwaitingInput, 0, 1, true),
            ("Paused · 1 needs you".to_string(), StatusTone::Waiting)
        );
        assert_eq!(
            text(ChatIndicator::Errored, 0, 0, true),
            (
                "Paused · The last run failed".to_string(),
                StatusTone::Failed
            )
        );
        assert_eq!(text(ChatIndicator::Idle, 0, 0, true).1, StatusTone::Paused);
    }

    #[test]
    fn acting_badges_name_each_capability() {
        assert!(capability_badges(&Default::default()).is_empty());
        assert_eq!(
            capability_badges(&cypher_proto::BotCapabilities::new_bot()),
            ["Works directly"]
        );
        assert_eq!(
            capability_badges(&cypher_proto::BotCapabilities {
                work_directly: false,
                coordinate: true,
                steer: true,
            }),
            ["Can start sessions", "Can steer sessions"]
        );
    }
}
