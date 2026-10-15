//! Context recovery: when a Pi turn's answer or error contains a phrase the
//! user configured, the chat's host asks a chosen model for supplementary
//! context, restarts the conversation from before the prompt that drew the
//! failure (Session Rewind), and sends that prompt again with the context
//! ahead of it.
//!
//! This module holds the device's settings, the record the resent prompt
//! carries in the transcript, and the matching rules — pure, so the engine's
//! decision and the settings card agree on what a trigger is.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Device-scoped settings (Settings → Agents → Context recovery).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContextRecoverySettings {
    /// Phrases that mark a failed turn, matched case-insensitively anywhere
    /// in the answer's text or an error message.
    pub triggers: Vec<String>,
    /// What the recovery model is asked to do with the failed exchange.
    pub instruction: String,
    /// `provider/model` that writes the supplementary context.
    pub model: String,
    /// `provider/model` ids of the session models whose chats are watched.
    pub enabled_models: Vec<String>,
    /// How many times one request may be recovered in a row before a failure
    /// is left standing.
    pub max_attempts: u32,
}

/// The instruction a fresh device starts with.
pub const DEFAULT_RECOVERY_INSTRUCTION: &str = "A coding agent could not complete the user's request: its answer or an error below shows what went wrong. Write a short supplementary context that addresses the cause and helps the agent succeed when the request is sent again — for example, clarify what the user wants, supply missing background, or say how to avoid the error. Address the agent directly. Do not answer the request yourself.";

impl Default for ContextRecoverySettings {
    fn default() -> Self {
        Self {
            triggers: Vec::new(),
            instruction: DEFAULT_RECOVERY_INSTRUCTION.into(),
            model: String::new(),
            enabled_models: Vec::new(),
            max_attempts: 1,
        }
    }
}

impl ContextRecoverySettings {
    pub const MAX_TRIGGERS: usize = 64;
    pub const MAX_TRIGGER_CHARS: usize = 200;
    pub const MAX_INSTRUCTION_CHARS: usize = 8_000;
    pub const MAX_ATTEMPTS_LIMIT: u32 = 3;

    /// The triggers that can match: trimmed, blank lines dropped.
    pub fn active_triggers(&self) -> Vec<&str> {
        self.triggers
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect()
    }

    /// Whether chats on `model` are watched: everything a recovery needs is
    /// configured and the session model is on the list.
    pub fn enabled_for(&self, model: Option<&str>) -> bool {
        !self.active_triggers().is_empty()
            && !self.model.trim().is_empty()
            && !self.instruction.trim().is_empty()
            && model.is_some_and(|model| self.enabled_models.iter().any(|id| id == model))
    }

    /// Ids this save selects that the previous one did not — the only ones
    /// that need checking against the device's catalog.
    pub fn new_model_selections<'a>(&'a self, previous: &Self) -> Vec<&'a String> {
        let mut added: Vec<_> = self
            .enabled_models
            .iter()
            .filter(|id| !previous.enabled_models.contains(id))
            .collect();
        if !self.model.is_empty() && self.model != previous.model {
            added.push(&self.model);
        }
        added
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.triggers.len() > Self::MAX_TRIGGERS {
            return Err(format!("At most {} triggers.", Self::MAX_TRIGGERS));
        }
        for trigger in &self.triggers {
            if trigger.chars().count() > Self::MAX_TRIGGER_CHARS {
                return Err(format!(
                    "A trigger is longer than {} characters.",
                    Self::MAX_TRIGGER_CHARS
                ));
            }
            if trigger.contains(['\r', '\n', '\0']) {
                return Err("A trigger contains an invalid character.".into());
            }
        }
        if self.instruction.chars().count() > Self::MAX_INSTRUCTION_CHARS {
            return Err("The instruction is too long.".into());
        }
        if self.instruction.contains('\0') {
            return Err("The instruction contains an invalid character.".into());
        }
        if self.model.chars().count() > 256 || self.model.contains(['\r', '\n', '\0']) {
            return Err("Invalid recovery model id.".into());
        }
        if !(1..=Self::MAX_ATTEMPTS_LIMIT).contains(&self.max_attempts) {
            return Err(format!(
                "Attempts must be between 1 and {}.",
                Self::MAX_ATTEMPTS_LIMIT
            ));
        }
        if self.enabled_models.len() > 128
            || self
                .enabled_models
                .iter()
                .any(|model| model.chars().count() > 256 || model.contains(['\r', '\n', '\0']))
        {
            return Err("Too many or invalid enabled model ids.".into());
        }
        Ok(())
    }
}

/// Where a trigger matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryTriggerSource {
    /// The text the model wrote.
    Response,
    /// An error the turn ended with or reported.
    Error,
}

/// What a recovered prompt carries in the transcript: the context the agent
/// received ahead of it, what set the recovery off, and where the
/// conversation restarted. Stamped on the resent user entry (`recovery`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextRecovery {
    /// The supplementary context, as the agent received it.
    pub context: String,
    /// The configured trigger that matched.
    pub trigger: String,
    pub matched_in: RecoveryTriggerSource,
    /// The failed answer or error around the match, whitespace collapsed.
    pub excerpt: String,
    /// The entry the conversation restarted after; `None` when it restarted
    /// from the beginning of the chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_after: Option<String>,
    /// That entry's opening words, so the restart point can be named without
    /// looking it up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_preview: Option<String>,
    /// 1 for the first recovery of a request, counting up while recovered
    /// prompts keep failing.
    pub attempt: u32,
    /// The model that wrote `context`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What the failed round had already done when it failed, as the agent
    /// received it ("You have already done: …"), in the chat's language.
    /// `None` when the round did no tool work, so there was nothing to carry
    /// over and the agent starts the request afresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub already_done: Option<String>,
    /// The request's earlier attempts, oldest first: each was sent with its
    /// own context and failed. The model that wrote `context` read them all,
    /// and the next attempt's record carries them on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_attempts: Vec<FailedRecoveryAttempt>,
}

/// One earlier attempt at a recovered request that failed too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedRecoveryAttempt {
    pub attempt: u32,
    /// The supplementary context it was sent with.
    pub context: String,
    /// Its failed answer or error around the trigger, whitespace collapsed.
    pub excerpt: String,
}

/// How far a recovery under way has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryStage {
    /// The recovery model is writing the supplementary context.
    WritingContext,
    /// The chat is being rewound and the prompt sent again.
    Restarting,
}

/// A recovery under way on a chat — run state on its session row
/// ([`crate::Session::recovery`]), so every device's transcript can show it
/// until the prompt is sent again or the recovery gives up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryProgress {
    pub stage: RecoveryStage,
    pub trigger: String,
    pub matched_in: RecoveryTriggerSource,
    pub attempt: u32,
    pub started_at: DateTime<Utc>,
}

impl RecoveryProgress {
    /// Longer than any recovery runs (the context has three minutes): a row
    /// still claiming one past this was left behind by an engine that died
    /// mid-recovery, and shows nothing.
    pub const STALE_AFTER_SECS: i64 = 5 * 60;

    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        now.signed_duration_since(self.started_at).num_seconds() < Self::STALE_AFTER_SECS
    }
}

/// One trigger found in a failed turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerMatch {
    pub trigger: String,
    pub source: RecoveryTriggerSource,
    pub excerpt: String,
}

/// Characters of context kept on each side of a match in its excerpt.
const EXCERPT_BEFORE: usize = 120;
const EXCERPT_AFTER: usize = 240;

/// The first trigger found in a failed turn: errors are searched before the
/// answer's text, because an error is the more specific signal. Matching is
/// case-insensitive (Unicode lowercase) on the text as written.
pub fn find_trigger(triggers: &[&str], response: &str, errors: &[&str]) -> Option<TriggerMatch> {
    let sources = errors
        .iter()
        .map(|text| (*text, RecoveryTriggerSource::Error))
        .chain(std::iter::once((response, RecoveryTriggerSource::Response)));
    for (text, source) in sources {
        if text.trim().is_empty() {
            continue;
        }
        let (lowered, origin) = lowered_with_origin(text);
        for trigger in triggers {
            let needle = trigger.trim().to_lowercase();
            if needle.is_empty() {
                continue;
            }
            if let Some(at) = lowered.find(&needle) {
                let start = origin[at];
                let end = origin.get(at + needle.len()).copied().unwrap_or(text.len());
                return Some(TriggerMatch {
                    trigger: trigger.trim().to_owned(),
                    source,
                    excerpt: excerpt(text, start, end),
                });
            }
        }
    }
    None
}

/// `text` lowercased, with the byte offset in `text` of the character each
/// lowered byte came from (lowercasing can change a character's length).
fn lowered_with_origin(text: &str) -> (String, Vec<usize>) {
    let mut lowered = String::with_capacity(text.len());
    let mut origin = Vec::with_capacity(text.len());
    for (at, ch) in text.char_indices() {
        for lower in ch.to_lowercase() {
            let before = lowered.len();
            lowered.push(lower);
            origin.extend(std::iter::repeat_n(at, lowered.len() - before));
        }
    }
    (lowered, origin)
}

/// The match with some text either side, whitespace collapsed, ellipsized
/// where it was cut.
fn excerpt(text: &str, start: usize, end: usize) -> String {
    let before: Vec<char> = text[..start].chars().collect();
    let after: Vec<char> = text[end..].chars().collect();
    let head: String = before[before.len().saturating_sub(EXCERPT_BEFORE)..]
        .iter()
        .collect();
    let tail: String = after[..after.len().min(EXCERPT_AFTER)].iter().collect();
    let mut out = String::new();
    if before.len() > EXCERPT_BEFORE {
        out.push('…');
    }
    out.push_str(&head);
    out.push_str(&text[start..end]);
    out.push_str(&tail);
    if after.len() > EXCERPT_AFTER {
        out.push('…');
    }
    collapse_whitespace(&out)
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_searched_before_the_answer() {
        let found = find_trigger(
            &["rate limit", "cannot help"],
            "I cannot help with that.",
            &["429: Rate Limit exceeded"],
        )
        .unwrap();
        assert_eq!(found.trigger, "rate limit");
        assert_eq!(found.source, RecoveryTriggerSource::Error);
        assert_eq!(found.excerpt, "429: Rate Limit exceeded");
    }

    #[test]
    fn matching_ignores_case_and_keeps_the_text_as_written() {
        let found = find_trigger(&["  I CAN'T  "], "Sorry — I can't do that here.", &[]).unwrap();
        assert_eq!(found.trigger, "I CAN'T");
        assert_eq!(found.source, RecoveryTriggerSource::Response);
        assert_eq!(found.excerpt, "Sorry — I can't do that here.");
    }

    #[test]
    fn a_match_after_a_character_that_lowercases_longer_maps_back() {
        // 'İ' lowercases to two characters, shifting every later offset.
        let found = find_trigger(&["拒绝"], "İİİ 我拒绝执行", &[]).unwrap();
        assert_eq!(found.excerpt, "İİİ 我拒绝执行");
    }

    #[test]
    fn blank_triggers_and_texts_never_match() {
        assert!(find_trigger(&["", "   "], "anything", &[""]).is_none());
        assert!(find_trigger(&["x"], "   ", &[]).is_none());
    }

    #[test]
    fn a_long_answer_is_cut_around_the_match() {
        let text = format!("{} TRIGGER {}", "a ".repeat(400), "b ".repeat(400));
        let found = find_trigger(&["trigger"], &text, &[]).unwrap();
        assert!(found.excerpt.starts_with('…') && found.excerpt.ends_with('…'));
        assert!(found.excerpt.contains("TRIGGER"));
        assert!(found.excerpt.chars().count() < EXCERPT_BEFORE + EXCERPT_AFTER + 10);
    }

    #[test]
    fn a_chat_is_watched_only_when_everything_is_configured() {
        let mut settings = ContextRecoverySettings {
            triggers: vec!["refuse".into()],
            model: "p/fixer".into(),
            enabled_models: vec!["p/agent".into()],
            ..Default::default()
        };
        assert!(settings.enabled_for(Some("p/agent")));
        assert!(!settings.enabled_for(Some("p/other")));
        assert!(!settings.enabled_for(None));
        settings.triggers = vec!["  ".into()];
        assert!(!settings.enabled_for(Some("p/agent")));
        settings.triggers = vec!["refuse".into()];
        settings.model.clear();
        assert!(!settings.enabled_for(Some("p/agent")));
    }

    #[test]
    fn validation_bounds_every_field() {
        assert!(ContextRecoverySettings::default().validate().is_ok());
        let bad = [
            ContextRecoverySettings {
                triggers: vec!["a\nb".into()],
                ..Default::default()
            },
            ContextRecoverySettings {
                max_attempts: 0,
                ..Default::default()
            },
            ContextRecoverySettings {
                max_attempts: ContextRecoverySettings::MAX_ATTEMPTS_LIMIT + 1,
                ..Default::default()
            },
            ContextRecoverySettings {
                instruction: "x".repeat(ContextRecoverySettings::MAX_INSTRUCTION_CHARS + 1),
                ..Default::default()
            },
        ];
        for settings in bad {
            assert!(settings.validate().is_err(), "{settings:?}");
        }
    }

    #[test]
    fn only_newly_selected_models_need_the_catalog() {
        let previous = ContextRecoverySettings {
            model: "p/fixer".into(),
            enabled_models: vec!["p/a".into()],
            ..Default::default()
        };
        let mut next = previous.clone();
        next.triggers = vec!["x".into()];
        assert!(next.new_model_selections(&previous).is_empty());
        next.enabled_models.push("p/b".into());
        next.model = "p/other".into();
        assert_eq!(next.new_model_selections(&previous), vec!["p/b", "p/other"]);
    }

    #[test]
    fn the_record_round_trips_without_optional_fields() {
        let record = ContextRecovery {
            context: "ctx".into(),
            trigger: "t".into(),
            matched_in: RecoveryTriggerSource::Error,
            excerpt: "e".into(),
            restart_after: None,
            restart_preview: None,
            attempt: 1,
            model: None,
            already_done: None,
            failed_attempts: Vec::new(),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(
            json,
            r#"{"context":"ctx","trigger":"t","matchedIn":"error","excerpt":"e","attempt":1}"#
        );
        assert_eq!(
            serde_json::from_str::<ContextRecovery>(&json).unwrap(),
            record
        );
    }
}
