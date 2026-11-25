//! Extension notices: an extension's `notify` during a model turn (a provider
//! bridge's rate-limit warning, a stuck-tool hint). Never reply text, never
//! a run's result, never sent to the model: the harness emits
//! [`crate::AgentEvent::Notice`], the engine keeps the latest one per chat
//! on its session row ([`crate::Session::notice`], transient run state like
//! `throughput`), and the clients show it as a quiet line in the session's
//! status strip for [`NOTICE_TTL_SECS`].
//!
//! Also the compact reading of the notices clients know the shape of, and
//! the matcher that drops such notices from transcripts written before the
//! split (they landed in the reply text). Both are table-driven and shared
//! with the iPhone through `protocol/vectors/notice-text-v1.json`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// How loud an extension notice is. `error` notifies stay
/// [`crate::AgentEvent::Error`]; only these two become notices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    Info,
    Warning,
}

/// The latest notice of a chat, on its session row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotice {
    pub level: NoticeLevel,
    /// The extension's text as it sent it.
    pub text: String,
    /// When the host received it: the clients stop showing it
    /// [`NOTICE_TTL_SECS`] later.
    pub at: DateTime<Utc>,
}

/// How long a notice shows after it arrived (clients), and after which the
/// host drops it from the row when the next turn starts.
pub const NOTICE_TTL_SECS: i64 = 90;

impl SessionNotice {
    /// Whether the notice still shows at `now`.
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        (now - self.at).num_seconds() < NOTICE_TTL_SECS
    }

    /// What the status strip shows: the compact form when the text has a
    /// known shape, else the text's first line.
    pub fn display_text(&self) -> String {
        compact_notice(&self.text)
    }
}

/// The prefix of pi-claude-bridge's warning (`index.ts`, `allowed_warning`):
/// `Claude rate limit warning: 58% used (seven_day)`.
const RATE_LIMIT_PREFIX: &str = "Claude rate limit warning: ";

/// Rate-limit window names as the Claude SDK reports them
/// (`rateLimitType`) → how the strip names them. An unknown window shows
/// its raw name.
const RATE_LIMIT_WINDOWS: &[(&str, &str)] = &[
    ("five_hour", "5-hour"),
    ("seven_day", "weekly"),
    ("seven_day_opus", "weekly Opus"),
    ("seven_day_sonnet", "weekly Sonnet"),
];

/// One parsed rate-limit warning.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RateLimitWarning<'a> {
    percent: &'a str,
    /// `None`: no parenthesised window; `Some("")`: an empty one.
    window: Option<&'a str>,
}

/// Parse `text` as exactly one rate-limit warning — the whole pattern
/// `^Claude rate limit warning: \d+% used( \([^)]*\))?\s*$` — returning it
/// and how many bytes of `text` the warning itself spans (without the
/// trailing whitespace).
fn parse_rate_limit_prefix(text: &str) -> Option<(RateLimitWarning<'_>, usize)> {
    let rest = text.strip_prefix(RATE_LIMIT_PREFIX)?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let percent = &rest[..digits];
    let rest = rest[digits..].strip_prefix("% used")?;
    let mut len = RATE_LIMIT_PREFIX.len() + digits + "% used".len();
    let window = match rest.strip_prefix(" (") {
        Some(inner) => {
            let close = inner.find(')')?;
            let window = &inner[..close];
            // `[^)]*` may hold anything but `)`: a line break inside is
            // not a single-line notice.
            if window.contains('\n') {
                return None;
            }
            len += 2 + close + 1;
            Some(window)
        }
        None => None,
    };
    Some((RateLimitWarning { percent, window }, len))
}

fn parse_rate_limit(text: &str) -> Option<RateLimitWarning<'_>> {
    let (warning, len) = parse_rate_limit_prefix(text)?;
    text[len..]
        .chars()
        .all(char::is_whitespace)
        .then_some(warning)
}

/// The compact form of a notice: `Claude rate limit warning: 58% used
/// (seven_day)` → `Claude: 58% of weekly limit`. Text of no known shape
/// keeps its first line, trimmed.
pub fn compact_notice(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(warning) = parse_rate_limit(trimmed) {
        let window = warning.window.filter(|w| !w.trim().is_empty()).map(|w| {
            let w = w.trim();
            RATE_LIMIT_WINDOWS
                .iter()
                .find(|(raw, _)| *raw == w)
                .map_or(w, |(_, label)| label)
        });
        return match window {
            Some(window) => format!("Claude: {}% of {window} limit", warning.percent),
            None => format!("Claude: {}% of limit", warning.percent),
        };
    }
    trimmed.lines().next().unwrap_or_default().trim().to_owned()
}

/// Whether `text` is, as a whole, a notice an older harness streamed into
/// the reply (the transcript then drops the whole part).
pub fn is_reply_notice(text: &str) -> bool {
    parse_rate_limit(text.trim_start()).is_some()
}

/// `text` without the notices an older harness streamed into it: the doc
/// fold appended a notify straight onto the reply's text part, with no
/// separator, so a part can open or close with one (or several in a row).
/// Only a notice at the very start or end that matches the WHOLE pattern
/// goes — text inside a reply is never touched. Returns `None` when nothing
/// was stripped.
pub fn strip_reply_notices(text: &str) -> Option<String> {
    if !text.contains(RATE_LIMIT_PREFIX) {
        return None;
    }
    let mut rest = text;
    // Leading: one notice after another, each up to its closing `)` (or
    // `% used` when it has no window).
    loop {
        let body = rest.trim_start();
        match parse_rate_limit_prefix(body) {
            Some((_, len)) => rest = &body[len..],
            None => break,
        }
    }
    // Trailing: the last occurrence whose remainder is exactly a notice.
    while let Some(at) = rest.rfind(RATE_LIMIT_PREFIX) {
        if parse_rate_limit(&rest[at..]).is_none() {
            break;
        }
        rest = &rest[..at];
    }
    if rest.len() == text.len() {
        return None;
    }
    // A stripped notice took its line break with it only when it had one;
    // the reply keeps its own leading/trailing shape otherwise.
    if rest.trim().is_empty() {
        return Some(String::new());
    }
    Some(
        rest.trim_matches(|c: char| c == '\n' || c == '\r')
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_rate_limit_windows() {
        let cases = [
            (
                "Claude rate limit warning: 58% used (seven_day)",
                "Claude: 58% of weekly limit",
            ),
            (
                "Claude rate limit warning: 80% used (five_hour)",
                "Claude: 80% of 5-hour limit",
            ),
            (
                "Claude rate limit warning: 91% used (seven_day_opus)",
                "Claude: 91% of weekly Opus limit",
            ),
            (
                "Claude rate limit warning: 5% used (overage)",
                "Claude: 5% of overage limit",
            ),
            (
                "Claude rate limit warning: 58% used ()",
                "Claude: 58% of limit",
            ),
            (
                "Claude rate limit warning: 58% used",
                "Claude: 58% of limit",
            ),
            (
                "Claude rate limit warning: 58% used (seven_day)\n",
                "Claude: 58% of weekly limit",
            ),
            (
                "Claude bridge: 2 tool handler(s) still waiting\nmore",
                "Claude bridge: 2 tool handler(s) still waiting",
            ),
            (
                "Claude rate limit warning: lots used",
                "Claude rate limit warning: lots used",
            ),
        ];
        for (text, want) in cases {
            assert_eq!(compact_notice(text), want, "{text:?}");
        }
    }

    #[test]
    fn whole_part_matches_only_the_full_pattern() {
        assert!(is_reply_notice(
            "Claude rate limit warning: 57% used (seven_day)"
        ));
        assert!(is_reply_notice(
            "Claude rate limit warning: 57% used (seven_day)  \n"
        ));
        assert!(is_reply_notice("Claude rate limit warning: 57% used"));
        assert!(!is_reply_notice(
            "Claude rate limit warning: 57% used (seven_day) ok"
        ));
        assert!(!is_reply_notice(
            "Note: Claude rate limit warning: 57% used"
        ));
        assert!(!is_reply_notice("Claude rate limit warning: % used"));
    }

    #[test]
    fn strips_only_leading_and_trailing_notices() {
        let n = "Claude rate limit warning: 57% used (seven_day)";
        assert_eq!(
            strip_reply_notices(&format!("{n}Let me look.")).as_deref(),
            Some("Let me look.")
        );
        assert_eq!(
            strip_reply_notices(&format!("{n}\nLet me look.")).as_deref(),
            Some("Let me look.")
        );
        assert_eq!(
            strip_reply_notices(&format!("Done.{n}")).as_deref(),
            Some("Done.")
        );
        assert_eq!(
            strip_reply_notices(&format!("Done.\n{n}\n")).as_deref(),
            Some("Done.")
        );
        assert_eq!(
            strip_reply_notices(&format!(
                "{n}Claude rate limit warning: 60% used (seven_day)Hi"
            ))
            .as_deref(),
            Some("Hi")
        );
        assert_eq!(strip_reply_notices(n).as_deref(), Some(""));
        // Inside the reply: untouched.
        assert_eq!(
            strip_reply_notices(&format!("The bridge says \"{n}\" when close.")),
            None
        );
        assert_eq!(strip_reply_notices(&format!("A {n} B")), None);
        assert_eq!(strip_reply_notices("Plain reply."), None);
    }

    #[test]
    fn notice_expires() {
        let at = Utc::now();
        let notice = SessionNotice {
            level: NoticeLevel::Warning,
            text: "x".into(),
            at,
        };
        assert!(notice.is_live(at));
        assert!(notice.is_live(at + chrono::Duration::seconds(NOTICE_TTL_SECS - 1)));
        assert!(!notice.is_live(at + chrono::Duration::seconds(NOTICE_TTL_SECS)));
    }
}
