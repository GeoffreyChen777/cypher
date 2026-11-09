//! The **Projects** panels (§13.1), in the work page's layout: sections
//! under small-caps headings, hairlines between them, rows that wash on
//! hover and open what they name.
//!
//! The list groups the projects the bot reads by device. A project's page
//! names its device beside the title (the folder on hover), then lists its
//! sessions, the work that ran there and the routines that run there.

use super::board::{ProjectLoad, project_loads, scoped_sessions};
use super::items::{
    arrow_in, head_note, hover_row, section_head, section_rule, session_mark, session_word,
    trailing, work_mark,
};
use super::panel::{PanelHead, PanelItem};
use super::*;
use cypher_proto::{BotRoutine, BotWork, WorkState};

/// Work items the project page lists, newest first.
const PROJECT_WORK: usize = 5;

/// One project on the list: its load and last activity.
struct ProjectEntry {
    space: cypher_proto::Space,
    load: ProjectLoad,
    last: Option<i64>,
}

impl Shell {
    /// Every project the bot reads, grouped by the device it lives on; a
    /// row opens the project.
    pub(super) fn render_projects_panel(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (PanelHead, gpui::Div) {
        let now = Utc::now();
        // (device name, its projects), devices in the order their first
        // project comes.
        let groups: Vec<(String, Vec<ProjectEntry>)> = {
            let state = self.state.read(cx);
            let spaces = state.spaces_sorted();
            let mut groups: Vec<(String, Vec<ProjectEntry>)> = Vec::new();
            let loads = project_loads(&bot.scope, &spaces, &state.chats, |chat| {
                state.display_status_for(chat, now)
            });
            for load in loads {
                let Some(space) = state.space_row(&load.space_id).cloned() else {
                    continue;
                };
                let device = state
                    .device_name(&space.device_id)
                    .unwrap_or("Unknown device")
                    .to_string();
                let last = scoped_sessions(&bot.scope, &state.chats, &space.id)
                    .first()
                    .map(|c| c.last_message_at.unwrap_or(c.created_at).timestamp_millis());
                let entry = ProjectEntry { space, load, last };
                match groups.iter_mut().find(|(d, _)| *d == device) {
                    Some((_, list)) => list.push(entry),
                    None => groups.push((device, vec![entry])),
                }
            }
            groups
        };
        let head = PanelHead {
            glyph: icons::WIDGET,
            tint: None,
            title: "Projects".into(),
        };
        let mut body = div().p(px(8.0)).flex().flex_col();
        if groups.is_empty() {
            return (
                head,
                body.child(parts::quiet_line(
                    theme,
                    "It reads no projects yet. Choose some in its settings.",
                )),
            );
        }
        for (ix, (device, entries)) in groups.into_iter().enumerate() {
            let n = entries.len();
            body = body
                .when(ix > 0, |el| el.child(section_rule()))
                .child(
                    section_head(&device, ix == 0, theme)
                        .child(div().flex_1())
                        .child(head_note(
                            format!("{n} project{}", if n == 1 { "" } else { "s" }),
                            theme,
                        )),
                )
                .children(
                    entries
                        .into_iter()
                        .map(|entry| self.project_entry_row(entry, now, theme, cx)),
                );
        }
        (head, body)
    }

    /// A project on the list: its glyph in its colour, its name, how many
    /// sessions it has and what runs or asks there, when it was last
    /// active, and an arrow that slides in on hover.
    fn project_entry_row(
        &self,
        entry: ProjectEntry,
        now: chrono::DateTime<Utc>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let ProjectEntry { space, load, last } = entry;
        let key = format!("bot-panel-project-{}", space.id);
        let item = PanelItem::Project(space.id.clone());
        let glyph = crate::appearance::space_style::space_icon(space.icon.as_deref());
        let tint = crate::appearance::space_style::space_color(space.color.as_deref(), theme)
            .unwrap_or(theme.text_muted);
        // "4 sessions · 1 asking · 2 running", the live counts in their tone.
        let mut line = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(4.0))
            .text_size(px(11.5))
            .line_height(px(16.0))
            .text_color(theme.text_muted.opacity(0.85))
            .child(SharedString::from(match load.sessions {
                0 => "No sessions yet".to_string(),
                1 => "1 session".to_string(),
                n => format!("{n} sessions"),
            }));
        if load.asking > 0 {
            line = line.child("·").child(
                div()
                    .text_color(theme.warning)
                    .child(SharedString::from(format!("{} asking", load.asking))),
            );
        }
        if load.running > 0 {
            line = line.child("·").child(
                div()
                    .text_color(theme.text)
                    .child(SharedString::from(format!("{} running", load.running))),
            );
        }
        hover_row(&key)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
            .child(
                div()
                    .pt(px(2.0))
                    .child(icon(glyph).size(px(14.0)).flex_none().text_color(tint)),
            )
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
                            .text_size(px(13.0))
                            .line_height(px(18.0))
                            .text_color(theme.text)
                            .child(SharedString::from(space.display_name().to_string())),
                    )
                    .child(line),
            )
            .children(last.map(|at| {
                div()
                    .pt(px(2.0))
                    .child(trailing(super::items::ago(at, now), theme.text_faint))
            }))
            .child(div().pt(px(5.0)).child(arrow_in(&key, theme)))
    }

    /// One project: its device after the title (the folder on hover, a
    /// click copies it), then its sessions in the bot's scope, the bot's
    /// work there and its routines there.
    pub(super) fn render_project_panel(
        &mut self,
        bot: &Bot,
        space_id: &str,
        theme: &Theme,
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let now = Utc::now();
        let (space, device, sessions, work, routines) = {
            let state = self.state.read(cx);
            if !bot.scope.sees_space(space_id) {
                return None;
            }
            let space = state.space_row(space_id)?.clone();
            let device = state
                .device_name(&space.device_id)
                .unwrap_or("Unknown device")
                .to_string();
            let sessions: Vec<(String, String, ChatIndicator, i64)> =
                scoped_sessions(&bot.scope, &state.chats, space_id)
                    .into_iter()
                    .map(|chat| {
                        (
                            chat.id.clone(),
                            transcript::single_line(&super::items::plain_markdown(
                                chat.title.as_deref().unwrap_or("New session"),
                            )),
                            state.display_status_for(chat, now),
                            chat.last_message_at
                                .unwrap_or(chat.created_at)
                                .timestamp_millis(),
                        )
                    })
                    .collect();
            let records = &state.bot_records;
            let mut work: Vec<BotWork> = records
                .work_of(&bot.id)
                .filter(|w| w.state != WorkState::Cancelled)
                .filter(|w| {
                    w.steps
                        .iter()
                        .any(|s| s.project.as_deref() == Some(space_id))
                })
                .cloned()
                .collect();
            work.sort_by_key(|w| std::cmp::Reverse(work_at(w)));
            work.truncate(PROJECT_WORK);
            let mut routines: Vec<BotRoutine> = records
                .routines
                .iter()
                .filter(|r| r.bot_id == bot.id && r.target.space_id() == Some(space_id))
                .cloned()
                .collect();
            routines.sort_by_key(|r| r.name.to_lowercase());
            (space, device, sessions, work, routines)
        };
        let head = PanelHead {
            glyph: crate::appearance::space_style::space_icon(space.icon.as_deref()),
            tint: crate::appearance::space_style::space_color(space.color.as_deref(), theme),
            title: space.display_name().to_string(),
        };
        let ticker = self.pulse_ticker.entity_id();

        // ── where it lives: its device after the title; hovering it shows
        // the folder, and a click copies the folder's path ──
        let device_key = format!("bot-project-device-{}", space.id);
        let copied = self.board.copied.as_deref() == Some(device_key.as_str());
        let (path, tip_path) = (space.path.clone(), space.path.clone());
        let click_key = device_key.clone();
        self.board.panel_title_note = Some(
            div()
                .id(SharedString::from(device_key.clone()))
                .min_w_0()
                .flex_shrink(1.0)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .cursor_pointer()
                .on_hover(motion::hover_listener(device_key.clone()))
                .tooltip(move |_, cx| {
                    let tip = FolderTip {
                        path: tip_path.clone(),
                        copied,
                    };
                    cx.new(|_| tip).into()
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.copy_text(click_key.clone(), path.clone(), cx)
                }))
                .text_size(px(12.5))
                .text_color(if copied {
                    theme.success
                } else {
                    motion::hover_blend(&device_key, theme.text_muted, theme.text)
                })
                .child(
                    icon(if copied { icons::CHECK } else { icons::LAPTOP })
                        .size(px(12.0))
                        .flex_none()
                        .text_color(if copied {
                            theme.success
                        } else {
                            motion::hover_blend(&device_key, theme.text_muted, theme.text)
                        }),
                )
                .child(div().min_w_0().truncate().child(SharedString::from(device)))
                .into_any_element(),
        );
        let mut body = div().p(px(8.0)).flex().flex_col();

        // ── its sessions ──
        body = body.child(section_head("Sessions", true, theme));
        if sessions.is_empty() {
            body = body.child(parts::quiet_line(theme, "No sessions here yet."));
        }
        for (chat_id, title, status, at) in sessions {
            let key = format!("bot-panel-session-{chat_id}");
            body = body.child(
                hover_row(&key)
                    .items_center()
                    .py(px(6.0))
                    .cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.open_chat(chat_id.clone(), cx)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(14.0))
                            .flex()
                            .justify_center()
                            .child(session_mark(status, ticker, theme)),
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
                    .children(session_word(status, theme).map(|(word, color)| {
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(color)
                            .child(word)
                    }))
                    .child(trailing(super::items::ago(at, now), theme.text_faint))
                    .child(arrow_in(&key, theme)),
            );
        }

        // ── the bot's work there ──
        if !work.is_empty() {
            body = body
                .child(section_rule())
                .child(section_head("Work", false, theme));
            for w in work {
                let key = format!("bot-project-work-{}", w.id);
                let item = if w.awaiting_approval() {
                    PanelItem::Plan(w.id.clone())
                } else {
                    PanelItem::WorkItem(w.id.clone())
                };
                let sub = if w.state == WorkState::Running {
                    super::work::current_step_line(&w)
                } else if w.state == WorkState::Proposed {
                    crate::transcript::bot_plan::phase_line(&w, now.timestamp_millis())
                } else {
                    super::work::result_line(&w)
                };
                body = body.child(
                    hover_row(&key)
                        .cursor_pointer()
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)),
                        )
                        .child(
                            div()
                                .pt(px(3.0))
                                .flex_none()
                                .w(px(14.0))
                                .flex()
                                .justify_center()
                                .child(work_mark(&w, ticker, theme)),
                        )
                        .child(two_line(transcript::single_line(&w.title), sub, theme))
                        .child(div().pt(px(2.0)).child(trailing(
                            super::items::ago(work_at(&w), now),
                            theme.text_faint,
                        )))
                        .child(div().pt(px(5.0)).child(arrow_in(&key, theme))),
                );
            }
        }

        // ── its routines ──
        if !routines.is_empty() {
            body = body
                .child(section_rule())
                .child(section_head("Routines", false, theme));
            for r in routines {
                let key = format!("bot-project-routine-{}", r.id);
                let item = PanelItem::Routine(r.id.clone());
                let (glyph, when) = super::schedule::routine_brief(&r);
                let tint =
                    crate::appearance::space_style::space_color(space.color.as_deref(), theme)
                        .unwrap_or(theme.text_muted);
                body = body.child(
                    hover_row(&key)
                        .cursor_pointer()
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)),
                        )
                        .child(
                            div()
                                .pt(px(3.0))
                                .flex_none()
                                .w(px(14.0))
                                .flex()
                                .justify_center()
                                .child(icon(glyph).size(px(12.0)).text_color(if r.enabled {
                                    tint
                                } else {
                                    theme.text_faint
                                })),
                        )
                        .child(two_line(r.name.clone(), when, theme))
                        .child(div().pt(px(5.0)).child(arrow_in(&key, theme))),
                );
            }
        }
        Some((head, body))
    }
}

/// When a work item last moved: its end, else its start, else its proposal.
fn work_at(work: &BotWork) -> i64 {
    work.ended_at.or(work.started_at).unwrap_or(work.created_at)
}

/// A row's title over its one-line detail.
fn two_line(title: String, detail: String, theme: &Theme) -> gpui::Div {
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
                .text_size(px(13.0))
                .line_height(px(18.0))
                .text_color(theme.text)
                .child(SharedString::from(title)),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .line_height(px(16.0))
                .text_color(theme.text_muted.opacity(0.85))
                .child(SharedString::from(detail)),
        )
}

/// The folder glyph in a project's hover card, and the gap after it.
const FOLDER_TIP_ICON: f32 = 12.0;
const FOLDER_TIP_GAP: f32 = 6.0;

/// The most characters on one line of the folder card's path (12px mono
/// is about 7.2px a character: some 330px).
const FOLDER_TIP_CHARS: usize = 46;

/// `path` in lines of at most `max` characters, broken after a slash where
/// one falls in reach (a longer name is cut where it must).
fn path_lines(path: &str, max: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    // Its pieces, each ending in its slash.
    let pieces = path.split_inclusive('/');
    for piece in pieces {
        if !line.is_empty() && line.chars().count() + piece.chars().count() > max {
            lines.push(std::mem::take(&mut line));
        }
        line.push_str(piece);
        while line.chars().count() > max {
            let cut: String = line.chars().take(max).collect();
            line = line.chars().skip(max).collect();
            lines.push(cut);
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// The hover card of a project's device: the folder it lives in, and that
/// a click copies it.
struct FolderTip {
    path: String,
    copied: bool,
}

impl Render for FolderTip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        motion::fade_quick(
            "bot-project-folder-tip",
            div()
                // The work status card's padding.
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.surface_raised)
                .shadow_md()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .items_start()
                        .gap(px(FOLDER_TIP_GAP))
                        .child(
                            icon(icons::FOLDER)
                                .size(px(FOLDER_TIP_ICON))
                                .mt(px(2.5))
                                .flex_none()
                                .text_color(theme.text_muted),
                        )
                        // Broken into lines here, at its slashes: a tooltip
                        // sizes itself before its width is capped, so text
                        // left to wrap overflowed the card.
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .text_size(px(12.0))
                                .line_height(px(17.0))
                                .font_family(theme.font_mono.clone())
                                .text_color(theme.text)
                                .children(
                                    path_lines(&self.path, FOLDER_TIP_CHARS).into_iter().map(
                                        |line| {
                                            div()
                                                .whitespace_nowrap()
                                                .child(SharedString::from(line))
                                        },
                                    ),
                                ),
                        ),
                )
                // Under the path, not the folder glyph.
                .child(
                    div()
                        .pl(px(FOLDER_TIP_ICON + FOLDER_TIP_GAP))
                        .text_size(px(11.0))
                        .text_color(if self.copied {
                            theme.success
                        } else {
                            theme.text_faint
                        })
                        .child(if self.copied {
                            "Copied"
                        } else {
                            "Click to copy the path"
                        }),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::path_lines;

    #[test]
    fn a_short_path_is_one_line() {
        assert_eq!(path_lines("/Users/me/demo", 46), ["/Users/me/demo"]);
    }

    #[test]
    fn long_paths_break_after_a_slash() {
        let path = "/Users/administrator/.cypher/worktrees/cypher/4a8247ff-7a86-478e-bc0a-1af73c26-6dfa1f50";
        let lines = path_lines(path, 46);
        assert_eq!(lines.concat(), path);
        assert!(lines.iter().all(|l| l.chars().count() <= 46), "{lines:?}");
        assert_eq!(lines[0], "/Users/administrator/.cypher/worktrees/cypher/");
    }

    #[test]
    fn a_name_longer_than_a_line_is_cut() {
        let lines = path_lines(&format!("/a/{}", "x".repeat(10)), 6);
        assert_eq!(lines.concat(), format!("/a/{}", "x".repeat(10)));
        assert!(lines.iter().all(|l| l.chars().count() <= 6), "{lines:?}");
    }
}
