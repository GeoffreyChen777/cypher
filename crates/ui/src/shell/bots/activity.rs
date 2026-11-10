//! The **Activity** card (§13.1): the last few things the bot did, each with
//! its kind's glyph and when. The panel is the full log, filterable by kind
//! (plans, sessions, memory, network); a row opens the work or session it
//! names.

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
/// An entry split for reading: what was done (`verb`, muted), what it was
/// done to (`subject`, full strength) and the rest (`detail`: where, why,
/// how much; a quieter second line). Parsed from the sentence the engine
/// wrote, so old entries read the same; one that fits no pattern is all
/// `verb`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Parts {
    pub verb: String,
    pub subject: Option<String>,
    pub detail: Option<String>,
}

/// Longest text taken as a subject; a longer one is a detail.
const SUBJECT_MAX: usize = 80;

/// `"Name" (Owner's workspace)` at the start of `text`: the subject
/// ("Name · Owner") and what follows.
fn quoted(text: &str) -> Option<(String, &str)> {
    let inner = text.strip_prefix('"')?;
    let end = inner.find('"')?;
    let mut name = inner[..end].to_string();
    let mut rest = &inner[end + 1..];
    if let Some(after) = rest.strip_prefix(" (")
        && let Some(close) = after.find(')')
        && let Some(owner) = after[..close].strip_suffix("'s workspace")
    {
        name = format!("{name} · {owner}");
        rest = &after[close + 1..];
    }
    Some((name, rest))
}

/// What follows a subject: its detail, without the joining colon.
fn tail(rest: &str) -> Option<String> {
    let rest = rest.trim().trim_start_matches(':').trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// A path's last part and the folder it is in.
fn path_parts(path: &str) -> (String, Option<String>) {
    let path = path.trim_end_matches('/');
    match path.rsplit_once('/') {
        Some((dir, name)) if !name.is_empty() => {
            (name.to_string(), (!dir.is_empty()).then(|| dir.to_string()))
        }
        _ => (path.to_string(), None),
    }
}

pub(super) fn parts_of(text: &str) -> Parts {
    let text = transcript::single_line(text).replace('`', "");
    let text = text.trim();
    // `"Morning brief" failed: why`, `"Ledger" (Dana's workspace) disconnected`
    if let Some((subject, rest)) = quoted(text) {
        let rest = rest.trim();
        let (word, more) = rest.split_once(' ').unwrap_or((rest, ""));
        return Parts {
            verb: capitalized(word.trim_end_matches(':')),
            subject: Some(subject),
            detail: tail(more),
        };
    }
    let quote = text.find('"');
    let colon = text.find(": ");
    // `Ran "Nightly tests"`, `Started "Fix it" in web on Mac mini`
    if let Some(at) = quote.filter(|q| colon.is_none_or(|c| *q < c))
        && let Some((subject, rest)) = quoted(&text[at..])
    {
        return Parts {
            verb: text[..at].trim().trim_end_matches(':').to_string(),
            subject: Some(subject),
            detail: tail(rest),
        };
    }
    if let Some(at) = colon {
        let verb = text[..at].trim();
        let rest = text[at + 2..].trim();
        // `Declined after review: "Ledger" (…) asked …`
        if let Some((subject, more)) = quoted(rest) {
            return Parts {
                verb: verb.to_string(),
                subject: Some(subject),
                detail: tail(more),
            };
        }
        // A memory file: `Created: /workspace/x.md`,
        // `Added 3 lines, removed 1: /workspace/MEMORY.md`.
        if rest.starts_with('/') && !rest.contains(' ') {
            let (name, dir) = path_parts(rest);
            let edit = ["Added ", "Removed ", "Changed "]
                .iter()
                .any(|p| verb.starts_with(p));
            return Parts {
                verb: if edit {
                    "Edited".into()
                } else {
                    verb.to_string()
                },
                subject: Some(name),
                detail: match (edit, dir) {
                    (true, Some(dir)) => Some(format!("{verb} · {dir}")),
                    (true, None) => Some(verb.to_string()),
                    (false, dir) => dir,
                },
            };
        }
        let short = rest.chars().count() <= SUBJECT_MAX && !rest.contains(". ");
        return Parts {
            verb: verb.to_string(),
            subject: short.then(|| rest.to_string()),
            detail: (!short).then(|| rest.to_string()),
        };
    }
    // `Created worktree /…/swift-onyx (cypher/swift-onyx) in invoice-demo`
    if let Some(at) = text.find(" /") {
        let path_end = text[at + 1..]
            .find(' ')
            .map_or(text.len(), |end| at + 1 + end);
        let (name, _) = path_parts(&text[at + 1..path_end]);
        return Parts {
            verb: text[..at].to_string(),
            subject: Some(name),
            detail: tail(&text[path_end..]),
        };
    }
    Parts {
        verb: text.to_string(),
        ..Parts::default()
    }
}

/// An entry's text in the panel: the muted verb and the subject at full
/// strength on one line (wrapping to two), then the detail, quieter, on one.
/// A failure's verb is red.
fn entry_text(entry: &BotActivity, theme: &Theme) -> gpui::Div {
    let parts = parts_of(&entry.text);
    let failed = entry.kind == cypher_proto::ActivityKind::WorkFailed
        || parts.verb.to_lowercase().contains("failed");
    let font = gpui::font(theme.font_sans.clone());
    let run = |len: usize, color: gpui::Hsla, weight: gpui::FontWeight| gpui::TextRun {
        len,
        font: gpui::Font {
            weight,
            ..font.clone()
        },
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let verb_color = if failed {
        theme.danger
    } else {
        theme.text_muted
    };
    let (line, runs) = match &parts.subject {
        Some(subject) if !parts.verb.is_empty() => {
            let head = format!("{} ", parts.verb);
            let line = format!("{head}{subject}");
            let runs = vec![
                run(head.len(), verb_color, gpui::FontWeight::NORMAL),
                run(subject.len(), theme.text, gpui::FontWeight::MEDIUM),
            ];
            (line, runs)
        }
        Some(subject) => (
            subject.clone(),
            vec![run(subject.len(), theme.text, gpui::FontWeight::MEDIUM)],
        ),
        // No subject: the verb is the whole entry, at full strength.
        None => (
            parts.verb.clone(),
            vec![run(
                parts.verb.len(),
                if failed { theme.danger } else { theme.text },
                gpui::FontWeight::NORMAL,
            )],
        ),
    };
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(1.0))
        .child(
            div()
                .min_w_0()
                .text_size(px(12.5))
                .line_height(px(17.0))
                .line_clamp(2)
                .child(gpui::StyledText::new(line).with_runs(runs)),
        )
        .children(parts.detail.map(|detail| {
            div()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .line_height(px(15.0))
                .text_color(theme.text_muted.opacity(0.85))
                .child(SharedString::from(detail))
        }))
}

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
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let now = Utc::now();
        let selected = matches!(&link, Link::Work(id)
            if self.board.panel == Some(PanelItem::WorkItem(id.clone())));
        let row = item_row(&key, selected);
        let row = match link {
            Link::Work(id) => row.on_click(cx.listener(move |this, _, _, cx| {
                this.open_panel(PanelItem::WorkItem(id.clone()), cx)
            })),
            Link::Chat(id) => {
                row.on_click(cx.listener(move |this, _, _, cx| this.open_chat(id.clone(), cx)))
            }
            Link::None => row.cursor_default(),
        };
        let text = entry_text(entry, theme);
        row.items_start()
            .py(px(7.0))
            .child(div().pt(px(1.5)).child(mark_cell(glyph(
                group_glyph(entry.kind.group()),
                theme.text_muted.opacity(0.85),
            ))))
            .child(text)
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

    fn parts(verb: &str, subject: Option<&str>, detail: Option<&str>) -> Parts {
        Parts {
            verb: verb.into(),
            subject: subject.map(String::from),
            detail: detail.map(String::from),
        }
    }

    #[test]
    fn entries_split_into_verb_subject_and_detail() {
        for (text, want) in [
            (
                "Ran \"Nightly tests\"",
                parts("Ran", Some("Nightly tests"), None),
            ),
            (
                "\"Morning brief\" failed: harness protocol error: pi exited",
                parts(
                    "Failed",
                    Some("Morning brief"),
                    Some("harness protocol error: pi exited"),
                ),
            ),
            (
                "Started \"Add currency field\" in invoice-demo on Ada’s Mac mini",
                parts(
                    "Started",
                    Some("Add currency field"),
                    Some("in invoice-demo on Ada’s Mac mini"),
                ),
            ),
            (
                "The plan failed: Add currency support to invoices",
                parts(
                    "The plan failed",
                    Some("Add currency support to invoices"),
                    None,
                ),
            ),
            (
                "Connected with \"Ledger\" (Dana's workspace)",
                parts("Connected with", Some("Ledger · Dana"), None),
            ),
            (
                "\"Ledger\" (Dana's workspace) disconnected",
                parts("Disconnected", Some("Ledger · Dana"), None),
            ),
            (
                "Asked \"Ledger\" (Dana's workspace): Is the fix out?",
                parts("Asked", Some("Ledger · Dana"), Some("Is the fix out?")),
            ),
            (
                "Declined after review: \"Ledger\" (Dana's workspace) asked Can you share it?",
                parts(
                    "Declined after review",
                    Some("Ledger · Dana"),
                    Some("asked Can you share it?"),
                ),
            ),
            (
                "Created: /workspace/routines.md",
                parts("Created", Some("routines.md"), Some("/workspace")),
            ),
            (
                "Added 15 lines, removed 12: /workspace/MEMORY.md",
                parts(
                    "Edited",
                    Some("MEMORY.md"),
                    Some("Added 15 lines, removed 12 · /workspace"),
                ),
            ),
            (
                "Memory consolidated: I reorganized `/workspace/MEMORY.md` into an index. It links two files.",
                parts(
                    "Memory consolidated",
                    None,
                    Some("I reorganized /workspace/MEMORY.md into an index. It links two files."),
                ),
            ),
            (
                "Created worktree /Users/a/worktrees/demo/swift-onyx (cypher/swift-onyx) in invoice-demo",
                parts(
                    "Created worktree",
                    Some("swift-onyx"),
                    Some("(cypher/swift-onyx) in invoice-demo"),
                ),
            ),
            (
                "Something else entirely",
                parts("Something else entirely", None, None),
            ),
        ] {
            assert_eq!(parts_of(text), want, "{text}");
        }
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
