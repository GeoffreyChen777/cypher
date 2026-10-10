//! The composer's `/` menu as data: which rows it shows, in what order, and
//! what each one says about the current chat. Rendering and keys live in the
//! composer.
//!
//! The menu has levels. Typing `/` lists Cypher's own actions for the message
//! (Attach files), then the commands turned on in Settings → Commands under
//! the same group headers as that page, each with a badge for what it
//! controls ("On" for Fast mode, a paused goal, how full the context is). A
//! command that takes named choices — `/orchestrate on|off|status`, `/goal
//! pause|resume|…`, `/provider add` — shows a chevron, and choosing it (or
//! typing its name and a space) opens the list of its choices, the one in
//! effect checked.

use std::ops::Range;

use cypher_engine::pi::session_modes::PiSessionModes;
use cypher_proto::{ContextUsage, SlashCommand};

use crate::settings::commands::{CommandGroup, placement};

/// One named choice of a command: what gets typed after the command, and
/// what it does. `takes_text` choices need more typing after them.
#[derive(Debug, PartialEq, Eq)]
pub struct Choice {
    pub value: &'static str,
    pub description: &'static str,
    pub takes_text: bool,
}

const fn choice(value: &'static str, description: &'static str) -> Choice {
    Choice {
        value,
        description,
        takes_text: false,
    }
}

const ORCHESTRATE: [Choice; 3] = [
    choice("on", "Let the main agent delegate to subagents"),
    choice("off", "Use subagents only when asked"),
    choice("status", "Show the current setting"),
];

// pi-goal's own completions and wording.
const GOAL: [Choice; 5] = [
    choice("status", "Show the current goal"),
    choice("pause", "Pause the active goal"),
    choice("resume", "Resume a stopped or budget-limited goal"),
    choice("clear", "Clear the current goal"),
    Choice {
        value: "edit",
        description: "Edit the current goal objective",
        takes_text: true,
    },
];

const PROVIDER: [Choice; 1] = [choice("add", "Add a provider in Settings")];

/// The named choices of a command, empty for commands without any.
pub fn choices(command: &str) -> &'static [Choice] {
    match command {
        "orchestrate" => &ORCHESTRATE,
        "goal" => &GOAL,
        "provider" => &PROVIDER,
        _ => &[],
    }
}

/// Something the composer itself does from the menu, rather than a command
/// sent to the agent. Choosing one removes the typed `/…`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open the file picker (the composer's former paperclip button).
    Attach,
}

impl Action {
    /// Every action, in menu order. Every composer offers them all.
    pub const ALL: [Self; 1] = [Self::Attach];

    pub fn label(self) -> &'static str {
        match self {
            Self::Attach => "Attach files",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Attach => "Add files or images to this message",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Attach => crate::icons::PAPERCLIP,
        }
    }

    /// What typing after `/` matches: the label and the words someone
    /// looking for it would type.
    fn search_text(self) -> &'static str {
        match self {
            Self::Attach => "attach files images upload",
        }
    }
}

/// The heading over [`Action`]s: they act on the message being written.
pub const ACTIONS_HEADING: &str = "Message";

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A group's heading; never selected.
    Header(&'static str),
    /// One of the composer's own actions.
    Action(Action),
    /// A command, by its index in the agent's command list.
    Command(usize),
    /// One choice of the command whose choices are open.
    Choice(&'static Choice),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Menu {
    pub rows: Vec<Row>,
    /// Positions in `rows` the keyboard moves between, top to bottom.
    pub selectable: Vec<usize>,
    /// The row to highlight first, as a position in `selectable`: the best
    /// match for what was typed.
    pub preferred: Option<usize>,
}

/// The command list: the `actions` and the commands `shown` lets through
/// that match `query`, the actions first under their own heading, then the
/// commands grouped under headers in the Settings page's group order. Within
/// a group the best matches come first (catalog order with nothing typed).
/// Once something is typed, the overall best match is preferred even when
/// its group is not the first; with nothing typed, the top row is.
pub fn command_level(
    actions: &[Action],
    commands: &[SlashCommand],
    shown: impl Fn(&str) -> bool,
    query: &str,
) -> Menu {
    let visible: Vec<usize> = (0..commands.len())
        .filter(|&ix| shown(&commands[ix].name))
        .collect();
    // Actions and commands rank together, so the best match wins whichever
    // it is.
    let candidates: Vec<Row> = actions
        .iter()
        .map(|&action| Row::Action(action))
        .chain(visible.iter().map(|&ix| Row::Command(ix)))
        .collect();
    let labels: Vec<&str> = candidates
        .iter()
        .map(|row| match row {
            Row::Action(action) => action.search_text(),
            Row::Command(ix) => commands[*ix].name.as_str(),
            Row::Header(_) | Row::Choice(_) => "",
        })
        .collect();
    let ranked: Vec<&Row> = crate::popover::filter_indices(query, &labels)
        .into_iter()
        .map(|position| &candidates[position])
        .collect();
    fn push_group(menu: &mut Menu, heading: &'static str, members: Vec<Row>) {
        if members.is_empty() {
            return;
        }
        menu.rows.push(Row::Header(heading));
        for row in members {
            menu.selectable.push(menu.rows.len());
            menu.rows.push(row);
        }
    }
    let mut menu = Menu::default();
    let actions_found: Vec<Row> = ranked
        .iter()
        .filter(|row| matches!(row, Row::Action(_)))
        .map(|row| (*row).clone())
        .collect();
    push_group(&mut menu, ACTIONS_HEADING, actions_found);
    for group in CommandGroup::ALL {
        let members: Vec<Row> = ranked
            .iter()
            .filter(|row| {
                matches!(row, Row::Command(ix) if placement(&commands[*ix].name).group == group)
            })
            .map(|row| (*row).clone())
            .collect();
        push_group(&mut menu, group.title(), members);
    }
    menu.preferred = if query.trim().is_empty() {
        (!menu.selectable.is_empty()).then_some(0)
    } else {
        ranked.first().and_then(|best| {
            menu.selectable
                .iter()
                .position(|&row| menu.rows[row] == **best)
        })
    };
    menu
}

/// The choices of `command` that start with `query`.
pub fn choice_level(command: &str, query: &str) -> Menu {
    let query = query.to_lowercase();
    let rows: Vec<Row> = choices(command)
        .iter()
        .filter(|choice| choice.value.starts_with(&query))
        .map(Row::Choice)
        .collect();
    Menu {
        selectable: (0..rows.len()).collect(),
        preferred: (!rows.is_empty()).then_some(0),
        rows,
    }
}

/// `/command partial` with the cursor in the first word after the command:
/// the choice level's token. `range` covers that word.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceToken {
    pub command: String,
    pub range: Range<usize>,
    pub query: String,
}

/// The choice token under the cursor, if the prompt is a command followed by
/// at most one word that the cursor is in (or right after the space).
/// Whether `command` has choices at all is the caller's question.
pub fn choice_token(text: &str, cursor: usize) -> Option<ChoiceToken> {
    if !text.starts_with('/') || cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let command_end = text.find(char::is_whitespace)?;
    let command = &text[1..command_end];
    if command.is_empty() || command.contains('/') {
        return None;
    }
    let rest = &text[command_end..];
    // The argument starts after the spaces; a line break ends the command.
    let gap = rest.len() - rest.trim_start_matches(' ').len();
    if gap == 0 {
        return None;
    }
    let start = command_end + gap;
    let end = text[start..]
        .find(char::is_whitespace)
        .map_or(text.len(), |at| start + at);
    if cursor < start || cursor > end || !text[end..].trim().is_empty() {
        return None;
    }
    Some(ChoiceToken {
        command: command.to_string(),
        range: start..end,
        query: text[start..cursor].to_string(),
    })
}

/// What the menu knows about the current chat.
#[derive(Debug, Clone, Copy, Default)]
pub struct Facts<'a> {
    /// The Pi plugins' switches (`None` until the host answered, or for an
    /// agent other than Pi).
    pub modes: Option<&'a PiSessionModes>,
    pub context: Option<ContextUsage>,
    pub running_subagents: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Something is switched on or running.
    On,
    /// Switched off.
    Off,
    /// A plain reading.
    Neutral,
    /// A reading worth acting on (a nearly full context).
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub label: String,
    pub tone: Tone,
}

fn badge(label: impl Into<String>, tone: Tone) -> Badge {
    Badge {
        label: label.into(),
        tone,
    }
}

fn on_off(on: bool) -> Badge {
    if on {
        badge("On", Tone::On)
    } else {
        badge("Off", Tone::Off)
    }
}

/// Context use at or above this share warns.
const CONTEXT_WARNING: f32 = 0.8;

/// The badge a command's row carries, if the menu knows something about
/// what it controls.
pub fn command_badge(command: &str, facts: &Facts) -> Option<Badge> {
    match command {
        "fast" => facts.modes.map(|modes| on_off(modes.fast)),
        "scripts" => facts.modes.map(|modes| on_off(modes.codemode)),
        "orchestrate" => facts.modes.map(|modes| on_off(modes.orchestrate)),
        "goal" => facts.modes?.goal.as_ref().map(|goal| {
            let tone = if goal.status == "active" {
                Tone::On
            } else {
                Tone::Neutral
            };
            badge(goal_status(&goal.status), tone)
        }),
        "compact" => facts.context.filter(|usage| usage.size > 0).map(|usage| {
            let fraction = usage.fraction();
            let tone = if fraction >= CONTEXT_WARNING {
                Tone::Warning
            } else {
                Tone::Neutral
            };
            badge(format!("{}% used", (fraction * 100.0).round()), tone)
        }),
        "subagent-status" => (facts.running_subagents > 0)
            .then(|| badge(format!("{} running", facts.running_subagents), Tone::On)),
        _ => None,
    }
}

/// Whether `choice` is the one in effect (the check on its row).
pub fn choice_in_effect(command: &str, choice: &Choice, facts: &Facts) -> bool {
    match (command, facts.modes) {
        ("orchestrate", Some(modes)) => {
            choice.value == if modes.orchestrate { "on" } else { "off" }
        }
        _ => false,
    }
}

/// The line above a command's choices: what is in effect now.
pub fn choice_summary(command: &str, facts: &Facts) -> Option<String> {
    let modes = facts.modes?;
    match command {
        "orchestrate" => Some(format!(
            "Adaptive orchestration is {}",
            if modes.orchestrate { "on" } else { "off" }
        )),
        "goal" => Some(match &modes.goal {
            Some(goal) => format!("{}: {}", goal_status(&goal.status), goal.text),
            None => "No goal yet. Type one to start it.".into(),
        }),
        _ => None,
    }
}

/// pi-goal's status in words.
fn goal_status(status: &str) -> String {
    match status {
        "active" => "Running".into(),
        "paused" => "Paused".into(),
        "blocked" => "Blocked".into(),
        "usage_limited" => "Usage limit".into(),
        "budget_limited" => "Budget limit".into(),
        "complete" => "Done".into(),
        other => other.replace('_', " "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_engine::pi::session_modes::PiGoal;

    fn command(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: String::new(),
            input_hint: None,
        }
    }

    fn catalog() -> Vec<SlashCommand> {
        [
            "goal",
            "fast",
            "orchestrate",
            "subagents",
            "subagent-status",
            "skill:wiki",
            "compact",
            "mcp",
        ]
        .into_iter()
        .map(command)
        .collect()
    }

    fn names(menu: &Menu, commands: &[SlashCommand]) -> Vec<String> {
        menu.rows
            .iter()
            .map(|row| match row {
                Row::Header(title) => format!("# {title}"),
                Row::Action(action) => action.label().to_string(),
                Row::Command(ix) => commands[*ix].name.clone(),
                Row::Choice(choice) => choice.value.to_string(),
            })
            .collect()
    }

    #[test]
    fn commands_group_under_headers_in_page_order() {
        let commands = catalog();
        let menu = command_level(&Action::ALL, &commands, |name| name != "mcp", "");
        assert_eq!(
            names(&menu, &commands),
            [
                "# Message",
                "Attach files",
                "# Conversation",
                "compact",
                "# Agent modes",
                "goal",
                "fast",
                "orchestrate",
                "# Subagents",
                "subagents",
                "subagent-status",
                "# Skills",
                "skill:wiki",
            ]
        );
        // Headers are skipped by the keyboard; the top row is preferred.
        assert_eq!(menu.selectable, [1, 3, 5, 6, 7, 9, 10, 12]);
        assert_eq!(menu.preferred, Some(0));
    }

    #[test]
    fn attach_is_offered_with_no_command_turned_on() {
        let commands = catalog();
        let menu = command_level(&Action::ALL, &commands, |_| false, "");
        assert_eq!(names(&menu, &commands), ["# Message", "Attach files"]);
        // Found by what someone looking for it would type.
        for query in ["att", "files", "image", "upload"] {
            let menu = command_level(&Action::ALL, &commands, |_| false, query);
            assert_eq!(
                menu.rows,
                [Row::Header(ACTIONS_HEADING), Row::Action(Action::Attach)],
                "{query}"
            );
        }
        assert!(
            command_level(&Action::ALL, &commands, |_| false, "fast")
                .rows
                .is_empty()
        );
    }

    #[test]
    fn the_best_match_is_preferred_even_in_a_later_group() {
        let commands = catalog();
        let menu = command_level(&Action::ALL, &commands, |_| true, "fa");
        assert_eq!(names(&menu, &commands), ["# Agent modes", "fast"]);
        // Prefix matches beat substrings: `subagents` (Subagents group) wins
        // over `fast` and `orchestrate`, which only contain an "s" and sit in
        // the earlier Agent modes group.
        let menu = command_level(&[], &commands, |_| true, "s");
        assert_eq!(menu.rows[0], Row::Header(CommandGroup::AgentModes.title()));
        let preferred = menu.selectable[menu.preferred.unwrap()];
        assert_eq!(menu.rows[preferred], Row::Command(3));
    }

    #[test]
    fn choices_filter_by_prefix() {
        let menu = choice_level("orchestrate", "o");
        assert_eq!(names(&menu, &[]), ["on", "off"]);
        assert_eq!(menu.preferred, Some(0));
        assert!(choice_level("orchestrate", "x").selectable.is_empty());
        assert!(choice_level("fast", "").rows.is_empty());
    }

    #[test]
    fn choice_tokens_follow_the_first_argument_word() {
        let token = choice_token("/orchestrate of", 15).unwrap();
        assert_eq!(token.command, "orchestrate");
        assert_eq!(token.range, 13..15);
        assert_eq!(token.query, "of");
        // Right after the space: an empty query over an empty word.
        let token = choice_token("/goal ", 6).unwrap();
        assert_eq!((token.range, token.query.as_str()), (6..6, ""));
        // Still in the command name, past the first word, a second word, or
        // a line break: not a choice.
        assert!(choice_token("/goal", 5).is_none());
        assert!(choice_token("/goal pause now", 15).is_none());
        assert!(choice_token("/goal edit ship it", 8).is_none());
        assert!(choice_token("/goal\npause", 11).is_none());
        assert!(choice_token("goal pause", 10).is_none());
    }

    #[test]
    fn badges_say_what_is_in_effect() {
        let modes = PiSessionModes {
            fast: true,
            codemode: false,
            orchestrate: false,
            goal: Some(PiGoal {
                status: "paused".into(),
                text: "Ship the menu".into(),
            }),
        };
        let facts = Facts {
            modes: Some(&modes),
            context: Some(ContextUsage {
                used: 170_000,
                size: 200_000,
            }),
            running_subagents: 2,
        };
        assert_eq!(command_badge("fast", &facts), Some(badge("On", Tone::On)));
        assert_eq!(
            command_badge("scripts", &facts),
            Some(badge("Off", Tone::Off))
        );
        assert_eq!(
            command_badge("orchestrate", &facts),
            Some(badge("Off", Tone::Off))
        );
        assert_eq!(
            command_badge("goal", &facts),
            Some(badge("Paused", Tone::Neutral))
        );
        assert_eq!(
            command_badge("compact", &facts),
            Some(badge("85% used", Tone::Warning))
        );
        assert_eq!(
            command_badge("subagent-status", &facts),
            Some(badge("2 running", Tone::On))
        );
        assert_eq!(command_badge("subagents", &facts), None);
        // Nothing known yet: no badges rather than guesses.
        let unknown = Facts::default();
        assert_eq!(command_badge("fast", &unknown), None);
        assert_eq!(command_badge("compact", &unknown), None);

        assert!(choice_in_effect("orchestrate", &ORCHESTRATE[1], &facts));
        assert!(!choice_in_effect("orchestrate", &ORCHESTRATE[0], &facts));
        assert_eq!(
            choice_summary("goal", &facts).as_deref(),
            Some("Paused: Ship the menu")
        );
        assert_eq!(
            choice_summary("orchestrate", &facts).as_deref(),
            Some("Adaptive orchestration is off")
        );
    }
}
