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

/// How the Status card's line reads for the bot conversation's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusTone {
    Working,
    Waiting,
    Failed,
    Quiet,
}

pub(super) fn status_line(status: ChatIndicator) -> (&'static str, StatusTone) {
    match status {
        ChatIndicator::Working => ("Working…", StatusTone::Working),
        ChatIndicator::AwaitingInput => ("Waiting for you", StatusTone::Waiting),
        ChatIndicator::Errored => ("The last run failed", StatusTone::Failed),
        ChatIndicator::Completed | ChatIndicator::Idle => ("Idle", StatusTone::Quiet),
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

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// The cards' shared surface: the sidebar's focused-project card.
fn card(theme: &Theme) -> gpui::Div {
    div()
        .rounded(px(CARD_RADIUS))
        .bg(theme.sidebar_card_fill(1.0))
        .shadow(theme.sidebar_card_shadows())
        .flex()
        .flex_col()
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
        match bot {
            Some(bot) => {
                column = column.child(self.render_status_card(&bot, theme, cx));
                if let Some(projects) = self.render_projects_card(&bot, theme, cx) {
                    column = column.child(projects);
                }
            }
            None if synced => column = column.child(render_board_empty(theme, cx)),
            None => {}
        }
        column.into_any_element()
    }

    /// Who the bot is, where it runs, what it is doing, and what it may
    /// read; the settings button opens its palette.
    fn render_status_card(&self, bot: &Bot, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let now = Utc::now();
        let (host, online, status, badges) = {
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
        let (line, tone) = status_line(status);
        let line_color = match tone {
            StatusTone::Working => theme.text,
            StatusTone::Waiting => theme.warning,
            StatusTone::Failed => theme.danger,
            StatusTone::Quiet => theme.text_muted,
        };
        let line_mark = match tone {
            StatusTone::Working | StatusTone::Waiting => {
                super::super::render::status_mark(status, self.pulse_ticker.entity_id(), theme)
            }
            StatusTone::Failed => Some(
                icon(icons::DANGER_TRIANGLE)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(theme.danger)
                    .into_any_element(),
            ),
            StatusTone::Quiet => None,
        };
        let status_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .text_size(px(12.5))
            .text_color(line_color)
            .children(line_mark)
            .child(SharedString::from(line));
        let badge_row =
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(4.0))
                .children(badges.into_iter().map(|label| {
                    div()
                        .px(px(7.0))
                        .py(px(2.0))
                        .rounded(px(6.0))
                        .bg(crate::kit::theme::ink(0.06))
                        .text_size(px(11.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(label))
                }));
        let edit_id = bot.id.clone();
        let settings_key = "bot-status-settings";
        let settings = div()
            .id(settings_key)
            .flex_none()
            .size(px(28.0))
            .rounded(px(8.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(motion::hover_blend(
                settings_key,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.14),
            ))
            .on_hover(motion::hover_listener(settings_key))
            .tooltip(|_, cx| {
                cx.new(|_| super::super::session::FindTooltip("Bot settings".into()))
                    .into()
            })
            .on_click(
                cx.listener(move |this, _, _, cx| this.open_bot_dialog(Some(edit_id.clone()), cx)),
            )
            .child(
                icon(icons::TUNING)
                    .size(px(14.0))
                    .text_color(motion::hover_blend(
                        settings_key,
                        theme.text_muted.opacity(0.8),
                        theme.text,
                    )),
            );
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
                    .child(settings),
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
fn section_header(
    key: &'static str,
    glyph: &'static str,
    title: &'static str,
    count: usize,
    open: bool,
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
                .text_color(theme.text_muted),
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
                .text_color(theme.text_muted.opacity(0.75))
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
        assert_eq!(
            status_line(ChatIndicator::AwaitingInput),
            ("Waiting for you", StatusTone::Waiting)
        );
        assert_eq!(status_line(ChatIndicator::Completed).0, "Idle");
    }
}
