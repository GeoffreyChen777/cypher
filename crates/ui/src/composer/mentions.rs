//! Mentions: the strict local Markdown links behind file, session, issue and
//! pull-request chips, their projection onto the text input's chips, and the
//! reference limits enforced on send.

use std::collections::HashSet;
use std::ops::Range;
use std::time::Duration;

use cypher_proto::Chat;
use gpui::{Context, SharedString};

use crate::widgets::text_input::{Chip, TextInput, TextProjection};

/// The literal `@` a chip displays before its file name. Projected as TEXT so
/// it shapes, wraps, and hit-tests with the label — the earlier SVG icons
/// painted into a reserved whitespace slot never sat right at text size
/// (user report). Chips read as inline code: `@name` in the mono font over
/// the code wash.
const MENTION_PREFIX: char = '@';

const MENTION_SIDE_PAD: &str = "\u{00A0}";

/// A private URI scheme keeps file mentions distinguishable from ordinary
/// Markdown links pasted into the composer.
pub(in crate::composer) const FILE_MENTION_SCHEME: &str = "cypher-file:";

/// The private scheme for `@session` references. The target is the percent-
/// encoded chat id (the stable identity); the label is the session title at
/// insert time — re-parsing never requires the title to match anything, so
/// renamed sessions keep working in old drafts.
pub(in crate::composer) const SESSION_MENTION_SCHEME: &str = "cypher-session:";

/// The private scheme for `#` GitHub issue references: `owner/name/number`.
/// The label (`#482 Title`) is the display snapshot at insert time; the
/// target alone identifies the issue.
const ISSUE_MENTION_SCHEME: &str = "cypher-issue:";

/// The same for `#` pull request references (GitHub numbers issues and pull
/// requests in one space, so only the chip's wording differs).
const PR_MENTION_SCHEME: &str = "cypher-pr:";

/// Distinct issues and pull requests one send may reference.
pub(in crate::composer) const MAX_ISSUE_REFS: usize = 3;

/// Total character budget across ALL referenced-issue snapshots (each is
/// already bounded engine-side; this bounds the sum).
pub(in crate::composer) const MAX_ISSUE_REFERENCE_CHARS: usize = 96 * 1024;

/// Budget for one send-time issue snapshot (a `gh` round trip on the
/// project's device, possibly over the relay).
pub(in crate::composer) const ISSUE_LOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// Distinct sessions one send may reference (the 4th distinct pick is
/// rejected with a visible composer error and not inserted).
const MAX_SESSION_REFS: usize = 3;

/// Total character budget across ALL referenced-session context (the per-
/// session window is capped engine-side at 48 KiB; this bounds the sum).
pub(in crate::composer) const MAX_SESSION_REFERENCE_CHARS: usize = 96 * 1024;

/// Bounded budget to read the first `TranscriptFrame::Reset` of a referenced
/// session's `WatchDocMessages` stream at send time.
pub(in crate::composer) const SESSION_LOAD_TIMEOUT: Duration = Duration::from_secs(8);

/// Quiet window after a synced replica first shows content: backfill lands as
/// a checkpoint then its rows, so the read waits for frames to stop.
pub(in crate::composer) const SESSION_REPLICA_SETTLE: Duration = Duration::from_millis(400);

/// Session rows the mention popup lists at once; a query narrows the rest.
pub(in crate::composer) const MAX_SESSION_CANDIDATES: usize = 8;

/// Session chip/tooltip display-title cap (chars, not bytes).
pub(in crate::composer) const MAX_SESSION_TITLE_CHARS: usize = 60;

/// Cap on a referenced session's chat id (chars) — UUIDs are 36, so a pasted
/// id this long is never a real chat and the link is rejected outright.
pub(in crate::composer) const MAX_SESSION_ID_CHARS: usize = 256;

/// Cap on a session chip's display label (chars, after unescaping) — inserted
/// titles are capped at [`MAX_SESSION_TITLE_CHARS`], so a label this long is a
/// hostile paste, not a renamed session.
pub(in crate::composer) const MAX_SESSION_LABEL_CHARS: usize = 512;

/// What a mention chip references. Files carry their workspace-relative path;
/// sessions carry the stable chat id (identity survives title changes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::composer) enum MentionKind {
    File {
        path: String,
        is_dir: bool,
    },
    Session {
        chat_id: String,
    },
    Issue {
        repo: String,
        number: u64,
        /// A pull request (`cypher-pr:`).
        pull: bool,
    },
}

/// A strict, local-only Markdown representation of a mention (file or
/// session). The underlying prompt always contains this form; the editor
/// projects it to a chip for display without leaking a second data model into
/// submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::composer) struct MentionLink {
    pub(in crate::composer) range: Range<usize>,
    /// Display label: basename for files, session title for sessions.
    pub(in crate::composer) label: String,
    pub(in crate::composer) kind: MentionKind,
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

fn percent_decode_path(encoded: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(encoded.len());
    let raw = encoded.as_bytes();
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' {
            let hex = std::str::from_utf8(raw.get(at + 1..at + 3)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            bytes.push(raw[at]);
            at += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

fn escape_mention_label(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

/// Reverse of [`escape_mention_label`]: restores the display label from its
/// escaped Markdown form (`\[`, `\]`, `\\` → `[`, `]`, `\`). Used for session
/// titles, which are not re-validatable against a canonical target.
fn unescape_mention_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut escaped = false;
    for ch in label.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else {
            out.push(ch);
        }
    }
    if escaped {
        out.push('\\');
    }
    out
}

pub(in crate::composer) fn local_file_link(path: &str, is_dir: bool) -> String {
    let path = path.trim_end_matches('/');
    let basename = path
        .rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(path);
    format!(
        "[{}]({}{})",
        escape_mention_label(basename),
        FILE_MENTION_SCHEME,
        percent_encode_path(&format!("{path}{}", if is_dir { "/" } else { "" }))
    )
}

/// The canonical raw form of a `@session` mention chip: the escaped title as
/// the label, the percent-encoded chat id as the target.
pub(in crate::composer) fn local_session_link(title: &str, chat_id: &str) -> String {
    format!(
        "[{}]({}{})",
        escape_mention_label(title),
        SESSION_MENTION_SCHEME,
        percent_encode_path(chat_id)
    )
}

/// The display label of an issue chip: `#482 Title`, the title capped like
/// session titles.
pub(in crate::composer) fn issue_chip_label(number: u64, title: &str) -> String {
    let title = title.trim();
    let title = if title.chars().count() > MAX_SESSION_TITLE_CHARS {
        let mut out: String = title.chars().take(MAX_SESSION_TITLE_CHARS).collect();
        out.push('…');
        out
    } else {
        title.to_string()
    };
    if title.is_empty() {
        format!("#{number}")
    } else {
        format!("#{number} {title}")
    }
}

/// "issue" / "pull request", for chip tooltips and prompt text.
pub(in crate::composer) fn github_kind_noun(pull: bool) -> &'static str {
    if pull { "pull request" } else { "issue" }
}

/// The canonical raw form of a `#issue` or `#pull request` chip.
pub(in crate::composer) fn local_issue_link(
    repo: &str,
    number: u64,
    title: &str,
    pull: bool,
) -> String {
    format!(
        "[{}]({}{}/{})",
        escape_mention_label(&issue_chip_label(number, title)),
        if pull {
            PR_MENTION_SCHEME
        } else {
            ISSUE_MENTION_SCHEME
        },
        percent_encode_path(repo),
        number
    )
}

fn local_path_is_safe(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

fn label_close(text: &str, start: usize) -> Option<usize> {
    let mut escaped = false;
    for (at, ch) in text[start..].char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == ']' && text[start + at + 1..].starts_with('(') {
            return Some(start + at);
        }
    }
    None
}

/// Scan raw text for mention links — both `cypher-file:` and
/// `cypher-session:` — in document order. Each is validated strictly so
/// hostile or non-canonical Markdown never becomes a chip.
pub(in crate::composer) fn mention_links(text: &str) -> Vec<MentionLink> {
    let mut links = Vec::new();
    let mut search = 0;
    while let Some(relative_start) = text[search..].find('[') {
        let start = search + relative_start;
        let Some(label_end) = label_close(text, start + 1) else {
            search = start + 1;
            continue;
        };
        let target_start = label_end + 2;
        let Some(relative_end) = text[target_start..].find(')') else {
            search = start + 1;
            continue;
        };
        let end = target_start + relative_end + 1;
        let raw_label = &text[start + 1..label_end];
        let target = &text[target_start..end - 1];
        let kind = if let Some(encoded) = target.strip_prefix(FILE_MENTION_SCHEME) {
            percent_decode_path(encoded).and_then(|decoded| {
                let is_dir = decoded.ends_with('/');
                let path = decoded.strip_suffix('/').unwrap_or(&decoded);
                (local_path_is_safe(path)
                    && percent_encode_path(&decoded) == encoded
                    && path
                        .rsplit('/')
                        .next()
                        .is_some_and(|basename| escape_mention_label(basename) == raw_label))
                .then(|| MentionKind::File {
                    path: path.to_string(),
                    is_dir,
                })
            })
        } else if let Some(encoded) = target.strip_prefix(SESSION_MENTION_SCHEME) {
            percent_decode_path(encoded).and_then(|chat_id| {
                let safe = !chat_id.is_empty()
                    && chat_id.chars().count() <= MAX_SESSION_ID_CHARS
                    && !chat_id.chars().any(|c| c.is_control() || c.is_whitespace());
                (safe && percent_encode_path(&chat_id) == encoded)
                    .then_some(MentionKind::Session { chat_id })
            })
        } else if let Some((target, pull)) = target
            .strip_prefix(ISSUE_MENTION_SCHEME)
            .map(|target| (target, false))
            .or_else(|| target.strip_prefix(PR_MENTION_SCHEME).map(|t| (t, true)))
        {
            target.rsplit_once('/').and_then(|(repo, number)| {
                let parsed = number.parse::<u64>().ok().filter(|n| *n > 0)?;
                // Canonical only: no leading zeros, and the chip must read
                // as the issue it points at.
                (cypher_engine::github::valid_repo(repo)
                    && parsed.to_string() == number
                    && (raw_label == format!("#{parsed}")
                        || raw_label.starts_with(&format!("#{parsed} "))))
                .then(|| MentionKind::Issue {
                    repo: repo.to_string(),
                    number: parsed,
                    pull,
                })
            })
        } else {
            search = end;
            continue;
        };
        let Some(kind) = kind else {
            search = end;
            continue;
        };
        let label = match &kind {
            MentionKind::File { path, .. } => {
                path.rsplit('/').next().unwrap_or_default().to_string()
            }
            MentionKind::Session { .. } | MentionKind::Issue { .. } => {
                unescape_mention_label(raw_label)
            }
        };
        // Hardened session and issue display labels: control chars/newlines and absurd
        // lengths in a pasted link never become a chip (stable ids survive
        // rename, so the label is never checked against the title).
        if matches!(
            kind,
            MentionKind::Session { .. } | MentionKind::Issue { .. }
        ) && (label.chars().count() > MAX_SESSION_LABEL_CHARS
            || label
                .chars()
                .any(|c| c.is_control() || c == '\n' || c == '\r'))
        {
            search = end;
            continue;
        }
        links.push(MentionLink {
            range: start..end,
            label,
            kind,
        });
        search = end;
    }
    links
}

/// Distinct session chat ids referenced in `text`, in mention order (deduped).
pub(in crate::composer) fn session_ref_chat_ids(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for link in mention_links(text) {
        if let MentionKind::Session { chat_id } = link.kind
            && seen.insert(chat_id.clone())
        {
            out.push(chat_id);
        }
    }
    out
}

/// One `#` issue or pull request reference in a draft, and its chip label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::composer) struct IssueRef {
    pub(in crate::composer) repo: String,
    pub(in crate::composer) number: u64,
    pub(in crate::composer) pull: bool,
    label: String,
}

/// Distinct issues referenced in `text`, in mention order (deduped).
pub(in crate::composer) fn issue_refs(text: &str) -> Vec<IssueRef> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for link in mention_links(text) {
        if let MentionKind::Issue { repo, number, pull } = link.kind
            && seen.insert((repo.clone(), number))
        {
            out.push(IssueRef {
                repo,
                number,
                pull,
                label: link.label,
            });
        }
    }
    out
}

/// Worktree name hint for a new session started from an issue: its number
/// and title (`482 Login loops…` → `cypher/482-login-loops-…-<hash>`).
pub(in crate::composer) fn issue_worktree_hint(issue: &IssueRef) -> String {
    issue.label.trim_start_matches('#').to_string()
}

/// Pure guard for the "up to [`MAX_SESSION_REFS`] distinct sessions" rule:
/// `true` when accepting `candidate_id` would exceed the cap (the draft
/// already references `MAX_SESSION_REFS` DISTINCT sessions and the candidate
/// is a new one). Duplicates of an already-referenced session pass.
pub(in crate::composer) fn session_cap_reached(
    existing_ids: &[String],
    candidate_id: &str,
) -> bool {
    existing_ids.len() >= MAX_SESSION_REFS && !existing_ids.iter().any(|id| id == candidate_id)
}

/// Send-time cap guard: `true` when the raw prompt references more than
/// [`MAX_SESSION_REFS`] DISTINCT sessions. The picker guards insertion, but
/// manually pasted/private markup can carry 4+ refs and must be rejected at
/// send too (before anything is cleared).
pub(in crate::composer) fn session_send_cap_exceeded(text: &str) -> bool {
    session_ref_chat_ids(text).len() > MAX_SESSION_REFS
}

/// Authoritative send-time validation of session references against the
/// current `AppState.chats` snapshot: `None` when every ref is loadable, or
/// an actionable error naming the first offending ref. The current chat and
/// temporary child (Side Chat) chats can never be referenced legitimately —
/// they are rejected synchronously so the draft/comments/attachments survive
/// untouched. Sessions from any project or device of the active profile are
/// valid. Unknown ids still fail later in async loading (a row may exist that
/// this snapshot hasn't synced yet).
pub(in crate::composer) fn session_refs_authoritative_error(
    refs: &[String],
    current_chat: Option<&str>,
    chats: &[Chat],
) -> Option<String> {
    for id in refs {
        if current_chat == Some(id.as_str()) {
            return Some(
                "You can't reference the current chat — remove the @session reference and try again."
                    .into(),
            );
        }
        let Some(chat) = chats.iter().find(|c| &c.id == id) else {
            continue; // unknown id — still fails later in async loading
        };
        if chat.is_child() {
            return Some(
                "You can't reference a temporary side chat — remove the @session reference and try again."
                    .into(),
            );
        }
    }
    None
}

/// The mention list's child index for candidate `active`: the "Sessions" and
/// "Files" headers are also direct children of the scroll container, and
/// both render only when session candidates exist.
pub(in crate::composer) fn mention_scroll_child(active: usize, n_sessions: usize) -> usize {
    if n_sessions == 0 {
        active
    } else if active < n_sessions {
        active + 1
    } else {
        active + 2
    }
}

/// Reconcile the mention popup's ONE active index after the candidate list
/// changes. Session candidates are local and deterministic, so a non-empty
/// list must seed `active = Some(0)` immediately — never waiting on the
/// (debounced, possibly engine-less) file RPC — while an index the shrunken
/// list has outgrown is cleared. An empty list has no active row.
pub(in crate::composer) fn reconcile_mention_active(
    active: Option<usize>,
    count: usize,
) -> Option<usize> {
    if count == 0 {
        return None;
    }
    match active {
        None => Some(0),
        Some(ix) if ix >= count => None,
        Some(ix) => Some(ix),
    }
}

/// Project composer text for display: every mention link becomes an atomic
/// chip of the text input (`@label`, with the link's hover tooltip).
pub fn mention_projection(raw: &str) -> TextProjection {
    project_mentions(raw).0
}

/// [`mention_projection`] plus the links behind its chips, in the same order.
fn project_mentions(raw: &str) -> (TextProjection, Vec<MentionLink>) {
    let links = mention_links(raw);
    let labels = mention_display_labels(&links);
    let mut display = String::new();
    let mut chips = Vec::new();
    let mut raw_at = 0;
    for (link, label) in links.iter().zip(labels) {
        display.push_str(&raw[raw_at..link.range.start]);
        let display_start = display.len();
        // The chip is plain projected text — `@` plus the label between
        // non-breaking side bearings; the rounded code wash beneath it is
        // painted by the text input's element. Every character here must
        // exist in Geist (no exotic whitespace — U+2003/U+202F shape at
        // fallback width and collapsed the chip once already).
        display.push_str(MENTION_SIDE_PAD);
        // Issue labels already lead with their own `#`.
        if !matches!(link.kind, MentionKind::Issue { .. }) {
            display.push(MENTION_PREFIX);
        }
        for ch in label.chars() {
            display.push(if ch == ' ' { '\u{00A0}' } else { ch });
        }
        display.push('\u{00A0}');
        let display_end = display.len();
        let chip = Chip {
            range: link.range.clone(),
            tooltip: link.tooltip_label(),
        };
        chips.push((chip, display_start..display_end));
        raw_at = link.range.end;
    }
    display.push_str(&raw[raw_at..]);
    (TextProjection::with_chips(display, chips), links)
}

impl MentionLink {
    /// What the chip's hover tooltip says: a file's workspace-relative path,
    /// a session's title, an issue or pull request's reference.
    fn tooltip_label(&self) -> SharedString {
        match &self.kind {
            MentionKind::File { path, is_dir } => {
                format!("{path}{}", if *is_dir { "/" } else { "" }).into()
            }
            MentionKind::Session { .. } => session_tooltip_label(&self.label),
            MentionKind::Issue { repo, number, pull } => {
                format!("GitHub {} {repo}#{number}", github_kind_noun(*pull)).into()
            }
        }
    }
}

/// The tooltip text for a session reference, on its chip and in the mention
/// menu.
pub(in crate::composer) fn session_tooltip_label(title: &str) -> SharedString {
    format!("Session: {title}").into()
}

/// Chips keep compact labels in the common case. File basenames are
/// disambiguated to the shortest unique path suffix when the same basename
/// appears more than once; session labels are their titles as-is.
pub(in crate::composer) fn mention_display_labels(links: &[MentionLink]) -> Vec<String> {
    links
        .iter()
        .map(|link| match &link.kind {
            MentionKind::Session { .. } | MentionKind::Issue { .. } => link.label.clone(),
            MentionKind::File { path, .. } => {
                let files: Vec<&MentionLink> = links
                    .iter()
                    .filter(|l| matches!(l.kind, MentionKind::File { .. }))
                    .collect();
                if files
                    .iter()
                    .filter(|other| other.label == link.label)
                    .count()
                    == 1
                {
                    return link.label.clone();
                }
                let parts: Vec<_> = path.split('/').collect();
                (1..=parts.len())
                    .map(|count| parts[parts.len() - count..].join("/"))
                    .find(|suffix| {
                        let suffix: Vec<_> = suffix.split('/').collect();
                        files.iter().all(|other| {
                            let MentionKind::File {
                                path: other_path, ..
                            } = &other.kind
                            else {
                                return true;
                            };
                            !other_path
                                .split('/')
                                .rev()
                                .take(suffix.len())
                                .eq(suffix.iter().rev().copied())
                        })
                    })
                    .unwrap_or_else(|| path.clone())
            }
        })
        .collect()
}

/// One chip in a *sent* message: its byte range over the projected display
/// string (`@label` between side bearings). The transcript renders these
/// read-only — no editing state, no tooltip machinery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentMentionSpan {
    pub range: Range<usize>,
    /// Full workspace-relative path (labels can be shortened to basenames).
    /// Empty for session references.
    pub path: SharedString,
    pub is_dir: bool,
    /// The stable chat id when this chip is a `@session` reference; `None`
    /// for file mentions.
    pub session: Option<SharedString>,
}

/// Project a sent message's raw Markdown for transcript display: mention links
/// collapse to the same chip labels the composer shows, everything else passes
/// through untouched. `None` when the text has no valid mention — the
/// substring probe keeps ordinary prompts on the zero-allocation path, so this
/// is safe to call for every user row.
pub fn sent_mention_display(raw: &str) -> Option<(String, Vec<SentMentionSpan>)> {
    if !raw.contains(FILE_MENTION_SCHEME)
        && !raw.contains(SESSION_MENTION_SCHEME)
        && !raw.contains(ISSUE_MENTION_SCHEME)
        && !raw.contains(PR_MENTION_SCHEME)
    {
        return None;
    }
    let (projection, links) = project_mentions(raw);
    if links.is_empty() {
        return None;
    }
    let spans = links
        .iter()
        .zip(&projection.chips)
        .map(|(link, (_, display))| match &link.kind {
            MentionKind::File { path, is_dir } => SentMentionSpan {
                range: display.clone(),
                path: SharedString::from(format!("{path}{}", if *is_dir { "/" } else { "" })),
                is_dir: *is_dir,
                session: None,
            },
            MentionKind::Session { chat_id } => SentMentionSpan {
                range: display.clone(),
                path: SharedString::from(""),
                is_dir: false,
                session: Some(SharedString::from(chat_id.clone())),
            },
            MentionKind::Issue { .. } => SentMentionSpan {
                range: display.clone(),
                path: SharedString::from(""),
                is_dir: false,
                session: None,
            },
        })
        .collect();
    Some((projection.display, spans))
}

/// The composer's mention edits on its text input: each replaces the
/// completed `@query` / `#query` token with a chip's link as one
/// non-coalescing undo step.
pub(in crate::composer) trait MentionInput {
    /// A file or folder mention.
    fn replace_mention(
        &mut self,
        range: Range<usize>,
        path: &str,
        is_dir: bool,
        cx: &mut Context<TextInput>,
    );
    /// A session reference (same atomicity as file mentions).
    fn replace_session_mention(
        &mut self,
        range: Range<usize>,
        title: &str,
        chat_id: &str,
        cx: &mut Context<TextInput>,
    );
    /// A GitHub issue or pull request.
    fn replace_issue_mention(
        &mut self,
        range: Range<usize>,
        repo: &str,
        number: u64,
        title: &str,
        pull: bool,
        cx: &mut Context<TextInput>,
    );
}

impl MentionInput for TextInput {
    fn replace_mention(
        &mut self,
        range: Range<usize>,
        path: &str,
        is_dir: bool,
        cx: &mut Context<TextInput>,
    ) {
        self.replace_with_chip(range, local_file_link(path, is_dir), cx);
    }

    fn replace_session_mention(
        &mut self,
        range: Range<usize>,
        title: &str,
        chat_id: &str,
        cx: &mut Context<TextInput>,
    ) {
        self.replace_with_chip(range, local_session_link(title, chat_id), cx);
    }

    fn replace_issue_mention(
        &mut self,
        range: Range<usize>,
        repo: &str,
        number: u64,
        title: &str,
        pull: bool,
        cx: &mut Context<TextInput>,
    ) {
        self.replace_with_chip(range, local_issue_link(repo, number, title, pull), cx);
    }
}
