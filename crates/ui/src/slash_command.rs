//! Whether a message is a slash command — the one rule the transcript (the
//! command chip, find, the quiet working trailer), the composer (send gates,
//! which command a question belongs to) and iOS share
//! (`protocol/vectors/slash-commands-v1.json`).
//!
//! A message runs as a command only when its first token is `/name` and the
//! chat's host offers `name` for its harness: pi runs a registered command
//! (or its built-ins, which the harness lists too) and hands anything else to
//! the model as text. Until the host's `ListCommands` has answered (or for a
//! harness that lists none) the strict shape rule of [`command_shaped`]
//! stands in, so a path, a URL or a lone `/` is never a command.
//!
//! Every `ListCommands` answer the app receives is kept in [`KnownCommands`],
//! keyed by host device and harness, for views that never fetch it
//! themselves (the transcript).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cypher_proto::{HarnessId, SlashCommand};
use gpui::{App, Global};

/// The command names one host offers for one harness, without the slash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandNames(HashSet<String>);

impl CommandNames {
    pub fn of(commands: &[SlashCommand]) -> Self {
        Self(commands.iter().map(|c| c.name.clone()).collect())
    }

    #[cfg(test)]
    pub fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        Self(names.into_iter().map(str::to_owned).collect())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The leading `/command` token of `text` when the message runs as a slash
/// command, else `None`. `known` is the chat's host catalog for its harness
/// (`None`: not loaded, or no chat); an empty catalog counts as none.
pub fn slash_command<'a>(text: &'a str, known: Option<&CommandNames>) -> Option<&'a str> {
    let text = text.trim_start();
    let token = text.split_whitespace().next()?;
    let name = token.strip_prefix('/')?;
    match known.filter(|names| !names.is_empty()) {
        Some(names) => names.contains(name),
        None => command_shaped(name),
    }
    .then_some(token)
}

/// The fallback rule: an ASCII letter, then ASCII letters, digits, `_`, `:`
/// or `-` — what pi's commands, skills (`skill:name`) and prompt templates
/// look like, and never a path, a URL or prose.
fn command_shaped(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '-'))
}

/// Every host catalog the app has fetched (`ListCommands`), by host device
/// and harness. A catalog is replaced only when its names change (and
/// observers are notified only then), so views can key caches on its
/// address.
#[derive(Default)]
pub struct KnownCommands {
    by_host: HashMap<(String, HarnessId), Arc<CommandNames>>,
}

impl Global for KnownCommands {}

impl KnownCommands {
    /// Keep a host's answer. A host never seen yet has no entry: a send
    /// there reads with the shape rule.
    pub fn record(cx: &mut App, device_id: &str, harness: HarnessId, commands: &[SlashCommand]) {
        let names = CommandNames::of(commands);
        let key = (device_id.to_owned(), harness);
        if cx
            .try_global::<Self>()
            .and_then(|known| known.by_host.get(&key))
            .is_some_and(|kept| **kept == names)
        {
            return;
        }
        // (Notifies observers: only on a change.)
        cx.default_global::<Self>()
            .by_host
            .insert(key, Arc::new(names));
    }

    /// The catalog of `device_id`'s `harness`, if it was ever fetched.
    pub fn of(cx: &App, device_id: &str, harness: HarnessId) -> Option<Arc<CommandNames>> {
        cx.try_global::<Self>()?
            .by_host
            .get(&(device_id.to_owned(), harness))
            .cloned()
    }

    /// The catalog a chat's messages are read with: its host's, for its
    /// harness.
    pub fn of_chat(cx: &App, chat: &cypher_proto::Chat) -> Option<Arc<CommandNames>> {
        let harness = chat.config.as_ref()?.harness;
        Self::of(cx, &chat.device_id, harness)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Vectors {
        version: u32,
        cases: Vec<Case>,
    }

    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        text: String,
        commands: Option<Vec<String>>,
        expect: Option<String>,
    }

    #[test]
    fn shared_vectors() {
        let vectors: Vectors = serde_json::from_str(include_str!(
            "../../../protocol/vectors/slash-commands-v1.json"
        ))
        .unwrap();
        assert_eq!(vectors.version, 1);
        assert!(vectors.cases.len() >= 30);
        for case in &vectors.cases {
            let known = case
                .commands
                .as_ref()
                .map(|names| CommandNames::from_names(names.iter().map(String::as_str)));
            assert_eq!(
                slash_command(&case.text, known.as_ref()),
                case.expect.as_deref(),
                "{}",
                case.name
            );
        }
    }

    #[gpui::test]
    fn catalogs_are_kept_per_host_and_harness(cx: &mut gpui::TestAppContext) {
        let goal = SlashCommand {
            name: "goal".into(),
            description: String::new(),
            input_hint: None,
        };
        cx.update(|cx| {
            assert!(KnownCommands::of(cx, "mac", HarnessId::Pi).is_none());
            KnownCommands::record(cx, "mac", HarnessId::Pi, std::slice::from_ref(&goal));
            let names = KnownCommands::of(cx, "mac", HarnessId::Pi).unwrap();
            assert_eq!(slash_command("/goal x", Some(&names)), Some("/goal"));
            assert_eq!(slash_command("/fast on", Some(&names)), None);
            assert!(KnownCommands::of(cx, "linux", HarnessId::Pi).is_none());
            assert!(KnownCommands::of(cx, "mac", HarnessId::Mock).is_none());
            // The same answer again keeps the catalog (and views' caches
            // keyed on it); a different one replaces it.
            KnownCommands::record(cx, "mac", HarnessId::Pi, std::slice::from_ref(&goal));
            let again = KnownCommands::of(cx, "mac", HarnessId::Pi).unwrap();
            assert!(Arc::ptr_eq(&names, &again));
            KnownCommands::record(cx, "mac", HarnessId::Pi, &[]);
            let emptied = KnownCommands::of(cx, "mac", HarnessId::Pi).unwrap();
            assert!(!Arc::ptr_eq(&names, &emptied));
            assert_eq!(slash_command("/fast on", Some(&emptied)), Some("/fast"));
        });
    }
}
