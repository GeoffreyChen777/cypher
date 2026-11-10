//! The **Activity** card (§13.1): the last few things the bot did, each with
//! its kind's glyph and when. The panel is the full log, filterable by kind
//! (plans, sessions, memory, network); a row opens the work or session it
//! names.

use super::board::{card_corner, card_glyph, card_item};
use super::items::{ago, glyph, item_row, mark_cell, trailing};
use super::panel::{PanelHead, PanelItem};
use super::*;
use cypher_proto::{ActivityGroup, BotActivity};

/// The filter chips, in order (`None` is All).
const FILTERS: [Option<ActivityGroup>; 6] = [
    None,
    Some(ActivityGroup::Plans),
    Some(ActivityGroup::Sessions),
    Some(ActivityGroup::Routines),
    Some(ActivityGroup::Notes),
    Some(ActivityGroup::Network),
];

/// A bot's activity in `group` (all of it for `None`), newest first.
pub(super) fn activity_of<'a>(
    records: &'a cypher_proto::BotRecords,
    bot_id: &str,
    group: Option<ActivityGroup>,
) -> Vec<&'a BotActivity> {
    let mut entries: Vec<&BotActivity> = records
        .activity
        .iter()
        .filter(|a| a.bot_id == bot_id)
        .filter(|a| group.is_none_or(|g| a.kind.group() == g))
        .collect();
    entries.sort_by_key(|a| std::cmp::Reverse(a.at));
    entries
}

pub(super) fn group_label(group: Option<ActivityGroup>) -> &'static str {
    match group {
        None => "All",
        Some(ActivityGroup::Plans) => "Plans",
        Some(ActivityGroup::Sessions) => "Sessions",
        Some(ActivityGroup::Routines) => "Routines",
        Some(ActivityGroup::Notes) => "Memory",
        Some(ActivityGroup::Network) => "Connections",
    }
}

fn group_glyph(group: ActivityGroup) -> &'static str {
    match group {
        ActivityGroup::Plans => icons::CHECKLIST,
        ActivityGroup::Sessions => icons::CHAT_ROUND_LINE,
        ActivityGroup::Routines => icons::CLOCK,
        ActivityGroup::Notes => icons::BOOK,
        ActivityGroup::Network => icons::USERS,
    }
}

/// What an entry opens: its work in the panel, else its session.
enum Link {
    Work(String),
    Chat(String),
    None,
}

fn link_of(entry: &BotActivity, chat_listed: impl Fn(&str) -> bool) -> Link {
    match (&entry.work_id, &entry.chat_id) {
        (Some(work), _) => Link::Work(work.clone()),
        (None, Some(chat)) if chat_listed(chat) => Link::Chat(chat.clone()),
        _ => Link::None,
    }
}

impl Shell {
    fn activity_row(
        &self,
        key: String,
        entry: &BotActivity,
        link: Link,
        roomy: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let now = Utc::now();
        let selected = matches!(&link, Link::Work(id)
            if self.board.panel == Some(PanelItem::WorkItem(id.clone())));
        let row = if roomy {
            item_row(&key, selected)
        } else {
            card_item(
                &key,
                Some(card_glyph(
                    group_glyph(entry.kind.group()),
                    theme.text_muted.opacity(0.7),
                )),
                transcript::single_line(&entry.text),
                Some(card_corner(ago(entry.at, now), None, theme)),
                // The work item it links to is the Work card's to mark.
                false,
                theme,
            )
        };
        let row = match link {
            Link::Work(id) => row.on_click(cx.listener(move |this, _, _, cx| {
                this.open_panel(PanelItem::WorkItem(id.clone()), cx)
            })),
            Link::Chat(id) => {
                row.on_click(cx.listener(move |this, _, _, cx| this.open_chat(id.clone(), cx)))
            }
            Link::None => row.cursor_default(),
        };
        if !roomy {
            return row;
        }
        let text = div()
            .flex_1()
            .min_w_0()
            .text_size(px(12.5))
            .line_height(px(17.0))
            .text_color(theme.text.opacity(0.9));
        let text = if roomy {
            text.line_clamp(2)
        } else {
            text.truncate()
        };
        row.when(roomy, |el| el.items_start().py(px(7.0)))
            .child(
                div()
                    .when(roomy, |el| el.pt(px(1.5)))
                    .child(mark_cell(glyph(
                        group_glyph(entry.kind.group()),
                        theme.text_muted.opacity(0.85),
                    ))),
            )
            .child(text.child(SharedString::from(if roomy {
                // Two lines' worth: gpui's clamp cuts without an ellipsis.
                cypher_proto::clip(&transcript::single_line(&entry.text), 110)
            } else {
                transcript::single_line(&entry.text)
            })))
            .child(trailing(ago(entry.at, now), theme.text_faint))
    }

    /// The detail panel for this card's items; `None` when the item no
    /// longer resolves.
    pub(super) fn render_activity_panel(
        &mut self,
        bot: &Bot,
        item: &PanelItem,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        if *item != PanelItem::Activity {
            return None;
        }
        let filter = self.board.activity_filter;
        let entries = {
            let state = self.state.read(cx);
            let entries: Vec<(BotActivity, Link)> =
                activity_of(&state.bot_records, &bot.id, filter)
                    .into_iter()
                    .map(|a| {
                        let link = link_of(a, |id| state.chats.iter().any(|c| c.id == id));
                        (a.clone(), link)
                    })
                    .collect();
            entries
        };
        let head = PanelHead {
            glyph: icons::PULSE,
            tint: None,
            title: "Activity".into(),
        };
        // The palette's segmented control, one segment per kind.
        let chips = div()
            .mx(px(8.0))
            .mt(px(6.0))
            .mb(px(6.0))
            .p(px(2.0))
            .rounded(px(8.0))
            .bg(crate::kit::theme::ink(0.05))
            .flex()
            .flex_row()
            .gap(px(2.0))
            .children(FILTERS.into_iter().map(|group| {
                let on = filter == group;
                let label = group_label(group);
                div()
                    .id(SharedString::from(format!("bot-activity-filter-{label}")))
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
                        this.board.activity_filter = group;
                        cx.notify();
                    }))
                    .child(SharedString::from(label))
            }));
        let mut body = div().p(px(8.0)).flex().flex_col().gap(px(1.0)).child(chips);
        if entries.is_empty() {
            // Under the filter, so another can be picked.
            self.board.panel_empty = Some(
                parts::panel_empty(
                    theme,
                    icons::PULSE,
                    if filter.is_some() {
                        "Nothing of this kind yet"
                    } else {
                        "No activity yet"
                    },
                    "What the workspace plans, starts, notes and runs is logged here.",
                )
                .into_any_element(),
            );
        }
        for (entry, link) in entries {
            body = body.child(self.activity_row(
                format!("bot-panel-activity-{}", entry.id),
                &entry,
                link,
                true,
                theme,
                cx,
            ));
        }
        Some((head, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::ActivityKind;

    fn entry(id: &str, kind: ActivityKind, at: i64) -> BotActivity {
        BotActivity {
            id: id.into(),
            bot_id: "b".into(),
            at,
            kind,
            text: id.into(),
            chat_id: None,
            work_id: None,
            space_id: None,
        }
    }

    #[test]
    fn activity_filters_by_group_newest_first() {
        let mut records = cypher_proto::BotRecords::default();
        let mut other = entry("other", ActivityKind::NoteWritten, 9);
        other.bot_id = "x".into();
        records.activity = vec![
            entry("plan", ActivityKind::PlanProposed, 1),
            entry("session", ActivityKind::SessionStarted, 3),
            entry("note", ActivityKind::NoteWritten, 2),
            entry("routine", ActivityKind::RoutineRan, 0),
            entry("peer", ActivityKind::PeerAsked, 4),
            entry("done", ActivityKind::WorkFinished, 5),
            other,
        ];
        let ids = |group| {
            activity_of(&records, "b", group)
                .into_iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(None),
            ["done", "peer", "session", "note", "plan", "routine"]
        );
        assert_eq!(ids(Some(ActivityGroup::Plans)), ["done", "plan"]);
        assert_eq!(ids(Some(ActivityGroup::Sessions)), ["session"]);
        assert_eq!(ids(Some(ActivityGroup::Notes)), ["note"]);
        assert_eq!(ids(Some(ActivityGroup::Routines)), ["routine"]);
        assert_eq!(ids(Some(ActivityGroup::Network)), ["peer"]);
        assert_eq!(group_label(Some(ActivityGroup::Routines)), "Routines");
        assert_eq!(group_label(Some(ActivityGroup::Notes)), "Memory");
    }

    #[test]
    fn entries_link_to_their_work_before_their_session() {
        let mut a = entry("a", ActivityKind::SessionStarted, 0);
        a.chat_id = Some("chat".into());
        assert!(matches!(link_of(&a, |_| true), Link::Chat(id) if id == "chat"));
        assert!(matches!(link_of(&a, |_| false), Link::None));
        a.work_id = Some("w".into());
        assert!(matches!(link_of(&a, |_| true), Link::Work(id) if id == "w"));
    }
}
