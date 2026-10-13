//! A permission request from the Runtime's permission extension
//! (`pi-runtime/extensions/cypher-permissions/core.ts` `approvalTitle`), read
//! out of the plain question it arrives as so the question card can lay it out:
//! the question, then one `Label: value` line per fact, a `Change:` or `Input:`
//! block last. A request is recognised by its "Deny and stop" option, which
//! only the extension offers; anything that does not parse stays an ordinary
//! question.

use cypher_proto::UserInputQuestion;

use super::split_question_context;

const STOP_OPTION: &str = "Deny and stop";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactKind {
    Path,
    Matches,
    Domain,
    Tool,
    Command,
    /// What the agent says a command outside the sandbox is for.
    Purpose,
    /// A command pattern the approval would allow from now on.
    Pattern,
    Reason,
    /// A sentence about the network for this command.
    Network,
    /// A sentence worth knowing before answering.
    Note,
    /// Why the reviewing model asked instead of approving.
    Review,
}

impl FactKind {
    fn parse(label: &str) -> Option<Self> {
        Some(match label {
            "Path" => Self::Path,
            "Matches" => Self::Matches,
            "Domain" => Self::Domain,
            "Tool" => Self::Tool,
            "Command" => Self::Command,
            "Purpose" => Self::Purpose,
            "Pattern" => Self::Pattern,
            "Reason" => Self::Reason,
            "Network" => Self::Network,
            "Note" => Self::Note,
            "Review" => Self::Review,
            _ => return None,
        })
    }

    /// The row label; sentences (network, notes, the review) have none.
    pub fn label(self) -> Option<&'static str> {
        Some(match self {
            Self::Path => "Path",
            Self::Matches => "Matches",
            Self::Domain => "Domain",
            Self::Tool => "Tool",
            Self::Command => "Command",
            Self::Purpose => "Purpose",
            Self::Pattern => "Pattern",
            Self::Reason => "Reason",
            Self::Network | Self::Note | Self::Review => return None,
        })
    }

    /// Paths, patterns, domains and commands read best in the code face.
    pub fn monospace(self) -> bool {
        matches!(
            self,
            Self::Path | Self::Matches | Self::Domain | Self::Tool | Self::Command | Self::Pattern
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub kind: FactKind,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    pub question: String,
    pub facts: Vec<Fact>,
    /// `("Change", diff)` or `("Input", tool input)`.
    pub block: Option<(String, String)>,
    /// A secret or Cypher's own data is at stake.
    pub serious: bool,
}

impl PermissionRequest {
    pub fn rows(&self) -> impl Iterator<Item = &Fact> {
        self.facts.iter().filter(|fact| fact.kind.label().is_some())
    }

    pub fn sentences(&self) -> impl Iterator<Item = &Fact> {
        self.facts.iter().filter(|fact| fact.kind.label().is_none())
    }
}

/// How much an option weighs on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionRole {
    /// The narrowest approval: the filled button.
    Primary,
    /// A broader approval.
    Secondary,
    Deny,
    /// Deny and stop the turn.
    Stop,
}

/// Each option's role. The first approval is the primary one: the extension
/// lists the narrowest first.
pub fn option_roles(options: &[String]) -> Vec<OptionRole> {
    let mut primary_taken = false;
    options
        .iter()
        .map(|label| match label.as_str() {
            STOP_OPTION => OptionRole::Stop,
            "Deny" => OptionRole::Deny,
            _ if !primary_taken => {
                primary_taken = true;
                OptionRole::Primary
            }
            _ => OptionRole::Secondary,
        })
        .collect()
}

/// The letter that picks each option: Y the narrowest approval, C an
/// approval for this chat, A always, N deny, S deny and stop. ⌘↵ also picks
/// the narrowest approval; number keys still pick by position.
pub fn shortcut_keys(options: &[String]) -> Vec<Option<&'static str>> {
    option_roles(options)
        .into_iter()
        .zip(options)
        .map(|(role, label)| match role {
            OptionRole::Primary => Some("y"),
            OptionRole::Secondary if label.starts_with("Always") => Some("a"),
            OptionRole::Secondary if label.ends_with("this chat") => Some("c"),
            OptionRole::Secondary => None,
            OptionRole::Deny => Some("n"),
            OptionRole::Stop => Some("s"),
        })
        .collect()
}

/// The option a key press picks on a permission card, if any. Letters only
/// without modifiers (⌘C must stay Copy); Enter only with ⌘, so an Enter
/// typed as the card appears never approves anything.
pub fn option_for_key(
    options: &[String],
    key: &str,
    platform: bool,
    other_modifiers: bool,
) -> Option<usize> {
    if other_modifiers {
        return None;
    }
    if key == "enter" {
        return platform
            .then(|| {
                option_roles(options)
                    .iter()
                    .position(|role| *role == OptionRole::Primary)
            })
            .flatten();
    }
    if platform {
        return None;
    }
    shortcut_keys(options)
        .iter()
        .position(|shortcut| *shortcut == Some(key))
}

pub fn parse(question: &UserInputQuestion, prompt: &str) -> Option<PermissionRequest> {
    // A permission prompt says so (`question.permission`); a Runtime from
    // before that is still recognised by its "Deny and stop" option.
    let permission = question.permission.is_some()
        || question.options.iter().any(|option| option == STOP_OPTION);
    if question.multi_select || !permission {
        return None;
    }
    let (question_text, context) = split_question_context(prompt);
    let context = context?;
    let mut facts = Vec::new();
    let mut block: Option<(String, Vec<&str>)> = None;
    for line in context.lines() {
        if let Some((_, lines)) = block.as_mut() {
            lines.push(line);
            continue;
        }
        if matches!(line.trim_end(), "Change:" | "Input:") {
            block = Some((line.trim_end().trim_end_matches(':').to_owned(), Vec::new()));
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let (label, value) = line.split_once(": ")?;
        let kind = FactKind::parse(label)?;
        facts.push(Fact {
            kind,
            value: value.trim().to_owned(),
        });
    }
    if facts.is_empty() {
        return None;
    }
    let serious = question_text.contains("sensitive")
        || question_text.contains("Cypher's own")
        || question_text.contains("outside the sandbox");
    Some(PermissionRequest {
        question: question_text,
        facts,
        block: block
            .map(|(label, lines)| (label, lines.join("\n").trim_end().to_owned()))
            .filter(|(_, text)| !text.is_empty()),
        serious,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(prompt: &str, options: &[&str]) -> UserInputQuestion {
        UserInputQuestion {
            id: "q".into(),
            header: prompt.into(),
            question: prompt.into(),
            options: options.iter().map(|option| option.to_string()).collect(),
            multi_select: false,
            permission: None,
        }
    }

    const OPTIONS: [&str; 4] = ["Allow once", "Allow for this chat", "Deny", "Deny and stop"];

    #[test]
    fn a_prompt_that_says_it_is_a_permission_needs_no_stop_option() {
        let prompt = "Allow the agent to reach example.com?\n\nContext:\nDomain: example.com";
        let mut marked = question(prompt, &["Allow for this command", "Deny"]);
        assert!(parse(&marked, prompt).is_none(), "unmarked, no stop option");
        marked.permission = Some(cypher_proto::PermissionPrompt {
            kind: cypher_proto::PermissionPromptKind::Network,
            tool_call_id: Some("call-1".into()),
        });
        assert!(parse(&marked, prompt).is_some());
    }

    #[test]
    fn a_request_reads_as_facts_sentences_and_a_block() {
        let prompt = "Allow the agent to write a protected file?\n\nContext:\nPath: ~/app/.git/config\nMatches: **/.git/config\nTool: edit\nReason: Set the remote\nReview: The reviewing model couldn't approve this automatically. Unclear.\nChange:\n- [core]\n+ [core]\n+ \tfsmonitor = x\n";
        let request = parse(&question(prompt, &OPTIONS), prompt).expect("a permission request");
        assert_eq!(
            request.question,
            "Allow the agent to write a protected file?"
        );
        let rows: Vec<_> = request
            .rows()
            .map(|fact| (fact.kind.label().unwrap(), fact.value.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Path", "~/app/.git/config"),
                ("Matches", "**/.git/config"),
                ("Tool", "edit"),
                ("Reason", "Set the remote"),
            ]
        );
        assert_eq!(request.sentences().count(), 1);
        assert_eq!(
            request.block,
            Some((
                "Change".into(),
                "- [core]\n+ [core]\n+ \tfsmonitor = x".into()
            ))
        );
        assert!(!request.serious);
    }

    #[test]
    fn ordinary_questions_stay_questions() {
        let prompt = "Which source?\n\nContext:\nThe catalog is stale.";
        assert_eq!(
            parse(&question(prompt, &OPTIONS), prompt),
            None,
            "unlabeled context"
        );
        let prompt = "Allow the agent to read a sensitive file?\n\nContext:\nPath: ~/.env";
        assert_eq!(
            parse(&question(prompt, &["Yes", "No"]), prompt),
            None,
            "no stop option"
        );
        assert!(parse(&question(prompt, &OPTIONS), prompt).unwrap().serious);
    }

    #[test]
    fn a_request_to_leave_the_sandbox_shows_purpose_and_patterns() {
        let options = [
            "Run once",
            "Allow for this chat",
            "Always allow",
            "Deny",
            "Deny and stop",
        ];
        let prompt = "Run this command outside the sandbox?\n\nContext:\nPurpose: Deploy to the server web\nCommand: scp app.tgz deploy@web:/srv/\nPattern: scp … deploy@web:*\nReason: the user asked\nNote: It runs as you, with your files, keys, keychain and network.";
        let request = parse(&question(prompt, &options), prompt).expect("a permission request");
        assert!(request.serious);
        let rows: Vec<_> = request
            .rows()
            .map(|fact| (fact.kind.label().unwrap(), fact.kind.monospace()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Purpose", false),
                ("Command", true),
                ("Pattern", true),
                ("Reason", false)
            ]
        );
        assert_eq!(request.sentences().count(), 1);
        assert_eq!(
            shortcut_keys(&labels(&options)),
            [Some("y"), Some("c"), Some("a"), Some("n"), Some("s")]
        );
    }

    fn labels(options: &[&str]) -> Vec<String> {
        options.iter().map(|option| option.to_string()).collect()
    }

    #[test]
    fn letters_pick_options_by_meaning() {
        let file = labels(&[
            "Allow once",
            "Allow for this chat",
            "Always allow",
            "Deny",
            "Deny and stop",
        ]);
        assert_eq!(
            shortcut_keys(&file),
            [Some("y"), Some("c"), Some("a"), Some("n"), Some("s")]
        );
        let command = labels(&[
            "Allow this command once",
            "Allow this command for this chat",
            "Deny",
            "Deny and stop",
        ]);
        assert_eq!(
            shortcut_keys(&command),
            [Some("y"), Some("c"), Some("n"), Some("s")]
        );
        let live = labels(&[
            "Allow for this command",
            "Allow for this chat",
            "Always allow",
            "Deny",
            "Deny and stop",
        ]);
        assert_eq!(option_for_key(&live, "y", false, false), Some(0));
        assert_eq!(option_for_key(&live, "a", false, false), Some(2));
        assert_eq!(
            option_for_key(&command, "a", false, false),
            None,
            "no Always here"
        );
    }

    #[test]
    fn enter_needs_command_and_letters_need_no_modifier() {
        let options = labels(&OPTIONS);
        assert_eq!(option_for_key(&options, "enter", false, false), None);
        assert_eq!(option_for_key(&options, "enter", true, false), Some(0));
        assert_eq!(
            option_for_key(&options, "c", true, false),
            None,
            "⌘C stays Copy"
        );
        assert_eq!(option_for_key(&options, "n", false, true), None);
        assert_eq!(option_for_key(&options, "s", false, false), Some(3));
    }

    #[test]
    fn the_narrowest_approval_is_primary() {
        let options: Vec<String> = [
            "Allow once",
            "Allow for this chat",
            "Always allow",
            "Deny",
            "Deny and stop",
        ]
        .iter()
        .map(|option| option.to_string())
        .collect();
        assert_eq!(
            option_roles(&options),
            [
                OptionRole::Primary,
                OptionRole::Secondary,
                OptionRole::Secondary,
                OptionRole::Deny,
                OptionRole::Stop
            ]
        );
    }
}
