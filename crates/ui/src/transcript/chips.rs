//! Tool summaries / chips and the working-indicator flavour (pure).

use super::*;

/// The ToolGroup summary line — "Ran 3 commands · edited 2 files".
///
/// The rule lives in `cypher_proto::view` so the terminal viewport reports the
/// same summary; this only adapts the row model's [`ToolItem`] to it.
pub fn tool_group_summary(tools: &[ToolItem]) -> String {
    let pairs: Vec<(ToolCall, bool)> = tools.iter().map(|t| (t.call.clone(), t.is_error)).collect();
    cypher_proto::view::tool_group_summary(&pairs)
}

// `single_line` and the per-kind chip label/detail are shared with the terminal
// viewport (`cypher_proto::view`): a tool must be named identically on every
// surface, and the one-line collapse is needed for the same reason in both (a
// literal newline breaks gpui's ellipsis logic and would be a cursor move in a
// cell grid).
pub use cypher_proto::view::{single_line, tool_chip_content};

/// Analytic expanded-chips height — no measurement needed for the fold tween.
pub fn chips_height(count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    CHIPS_TOP_PAD + count as f32 * CHIP_HEIGHT + (count as f32 - 1.0) * CHIP_GAP
}

/// Analytic height an open detail adds to its chip's card (separator + body)
/// — output blocks by line count, diff blocks via the changes pane's own
/// [`crate::changes::body_height`]. The chip's own [`CHIP_HEIGHT`] is already
/// counted by [`chips_height`].
pub fn detail_height(detail: &ToolDetail) -> f32 {
    let body = match detail {
        ToolDetail::Output {
            lines,
            truncated_by,
        } => {
            let rows = lines.len() + usize::from(*truncated_by > 0);
            rows as f32 * OUTPUT_LINE_HEIGHT + OUTPUT_BODY_PAD
        }
        ToolDetail::Diff { file, .. } => crate::changes::body_height(file),
        ToolDetail::Stats { stats } => stats.len() as f32 * OUTPUT_LINE_HEIGHT + OUTPUT_BODY_PAD,
    };
    DETAIL_SEPARATOR + body
}

/// Height of the "Show full output/diff" affordance row appended below an
/// open detail whose full payload lives in the sidecar (chat2-sync A3).
pub const BLOB_AFFORDANCE_HEIGHT: f32 = 24.0;

/// Height of the "Show N earlier tool calls" row that stands in for the chips
/// a group's cap keeps folded (and for the "Show fewer" row once revealed).
pub const OVERFLOW_ROW_HEIGHT: f32 = 26.0;

/// How many leading chips a group folds away: the cap keeps the LAST `limit`
/// calls (the ones the agent just ran), `limit == 0` keeps every call, and a
/// revealed group (`revealed`) hides nothing. One chip is never worth a row of
/// its own — folding it would trade 38px of content for 26px of button.
pub fn hidden_tool_count(total: usize, limit: u32, revealed: bool) -> usize {
    if revealed || limit == 0 {
        return 0;
    }
    match total.saturating_sub(limit as usize) {
        1 => 0,
        hidden => hidden,
    }
}

/// Line cap for a FETCHED full output (a defensive ceiling, not a doc cap —
/// the harness bounds outputs at 4KiB, so this is rarely reached).
const FULL_OUTPUT_MAX_LINES: usize = 400;

/// Build the upgraded detail from a fetched sidecar blob. Diff blobs parse
/// the `ToolDiff` JSON through the same pipeline as inline diffs; output
/// blobs render (near-)uncapped — fetching past the summary was the point.
pub(super) fn blob_detail(text: &str, is_diff: bool) -> Option<ToolDetail> {
    if is_diff {
        let diff: cypher_proto::ToolDiff = serde_json::from_str(text).ok()?;
        return tool_detail(None, Some(&diff), None);
    }
    let mut lines: Vec<SharedString> = text
        .lines()
        .map(|l| SharedString::from(l.to_owned()))
        .collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }
    let truncated_by = lines.len().saturating_sub(FULL_OUTPUT_MAX_LINES);
    lines.truncate(FULL_OUTPUT_MAX_LINES);
    Some(ToolDetail::Output {
        lines,
        truncated_by,
    })
}

/// Compact byte size for the fetch affordance label ("812 B", "12 KB").
pub(super) fn format_kb(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

// ---------------------------------------------------------------------------
// Working indicator flavour (pure; rendered by the shell strip)
// ---------------------------------------------------------------------------

/// Rotating flavour vocabulary (20 words / 7s, seeded per chat).
pub const FLAVOUR_WORDS: [&str; 20] = [
    "Thinking",
    "Pondering",
    "Scheming",
    "Brewing",
    "Weaving",
    "Tinkering",
    "Musing",
    "Composing",
    "Sifting",
    "Untangling",
    "Distilling",
    "Sketching",
    "Plotting",
    "Riffing",
    "Combobulating",
    "Percolating",
    "Marinating",
    "Noodling",
    "Puzzling",
    "Conjuring",
];
pub const FLAVOUR_ROTATE_SECS: i64 = 7;

/// The flavour word for a seed at an elapsed time.
pub fn flavour_word(seed: u64, elapsed_secs: i64) -> &'static str {
    let step = (elapsed_secs.max(0) / FLAVOUR_ROTATE_SECS) as u64;
    FLAVOUR_WORDS[((seed.wrapping_add(step)) % FLAVOUR_WORDS.len() as u64) as usize]
}

/// A stable per-chat seed.
pub fn flavour_seed(chat_id: &str) -> u64 {
    fnv1a(chat_id.as_bytes())
}

/// The working trailer's "Sending…" bridge: true while an in-flight send is
/// fresher than the session row's turn start — the row still carries the
/// PREVIOUS turn (or none), so a timer would count the send round-trip and
/// restart when the turn actually begins.
pub fn sending_bridge(
    send_started: Option<chrono::DateTime<chrono::Utc>>,
    turn_started: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    match (send_started, turn_started) {
        (Some(send), Some(turn)) => turn <= send,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// "1m 32s"-style elapsed formatting.
pub fn format_elapsed(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// A throughput reading older than this has stopped describing the stream:
/// the host publishes only while deltas arrive, so a stalled provider leaves
/// the last rate standing.
const THROUGHPUT_STALE_MS: i64 = 3_000;

/// The working trailer's throughput tail: `↓ 3.4k tokens · 52 tok/s`. The
/// live rate shows only while fresh; otherwise (a tool running, a stalled or
/// bursty stream, another device's seconds-old copy) the last finished
/// message's average stands in, and before there is one just the count.
/// Nothing at all before the turn's first output token.
pub fn throughput_label(
    throughput: &cypher_proto::Throughput,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let fresh = now
        .signed_duration_since(throughput.sampled_at)
        .num_milliseconds()
        <= THROUGHPUT_STALE_MS;
    let rate = throughput
        .tokens_per_second
        .filter(|_| fresh)
        .or(throughput.average_tokens_per_second);
    if throughput.output_tokens == 0 && rate.is_none() {
        return None;
    }
    let tokens = crate::context_ring::format_tokens(throughput.output_tokens);
    Some(match rate {
        Some(rate) => format!("↓ {tokens} tokens · {rate} tok/s"),
        None => format!("↓ {tokens} tokens"),
    })
}

/// Turns shorter than this carry no work rule: the label is a record of time
/// spent, and sub-second work reads as noise ("Worked for 0s").
const WORKED_MIN_SECS: i64 = 1;

/// The settled turn's "Worked for …" label, from the entry's own span
/// (`createdAt` → `completedAt`, the latter stamped when the segment reached a
/// terminal status). `None` while a turn is still open, on entries written
/// before the stamp existed, on a clock that ran backwards, and on turns under
/// [`WORKED_MIN_SECS`].
pub fn worked_label(created_at: i64, completed_at: Option<i64>) -> Option<SharedString> {
    let secs = completed_at?.checked_sub(created_at)?.div_euclid(1000);
    (secs >= WORKED_MIN_SECS)
        .then(|| SharedString::from(format!("Worked for {}", format_elapsed(secs))))
}

/// Where the work rule goes: the start of the entry's TRAILING run of answer
/// text. `None` when the turn ended on a tool/chip (no answer to separate) or
/// never left the text (a plain reply is not a work log).
pub(super) fn worked_rule_at(rows: &[Row]) -> Option<usize> {
    // A folded original is part of the answer, not work before it.
    let is_md = |r: &Row| {
        matches!(
            r.kind,
            RowKind::Markdown { .. }
                | RowKind::LiveMarkdown { .. }
                | RowKind::TranslationOriginal { .. }
        )
    };
    if !rows.last().is_some_and(is_md) {
        return None;
    }
    let at = rows.iter().rposition(|r| !is_md(r))? + 1;
    // Thinking is work done before the answer, but a reply that only thought
    // first is still a plain reply, not a work log.
    let is_thought = |r: &Row| {
        matches!(
            r.kind,
            RowKind::Thought { .. } | RowKind::ThoughtBlock { .. }
        )
    };
    rows[..at].iter().any(|r| !is_thought(r)).then_some(at)
}
