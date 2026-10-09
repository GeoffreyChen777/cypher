//! Row model (pure): transcript entries → block-granularity rows.

use super::*;

/// One tool invocation inside a group row.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolItem {
    pub call: ToolCall,
    pub is_error: bool,
    pub resolved: bool,
    /// Expandable detail: a code-block of output lines, or a real diff
    /// section rendered by the changes pane's component (ACP harnesses).
    /// Precomputed here because rows are cached by fingerprint — diffing and
    /// tokenizing per paint would run on every scroll frame.
    pub detail: Option<Arc<ToolDetail>>,
    /// Expandable full-invocation block: the complete tool call (whole
    /// command / pattern / URL / input JSON) that the chip header collapses
    /// to one truncated line. Rendered above `detail` in the open card.
    /// Precomputed for the same reason as `detail`.
    pub invocation: Option<Arc<ToolDetail>>,
    /// Sidecar key of the full output (chat2-sync A3) — the doc carries only
    /// a one-line summary; expanding offers a lazy "Show full output" fetch.
    pub output_ref: Option<SharedString>,
    /// Full-output size, for the affordance label ("Show full output (12 KB)").
    pub output_bytes: Option<u64>,
    /// Sidecar key of the full diff (doc carries only per-file stats).
    pub diff_ref: Option<SharedString>,
    /// Nesting under the call that made this one: 0 for a call the model
    /// made, 1 for a call a Pi codemode script made from inside its run
    /// (deeper if that call made calls of its own). See [`nest_tool_calls`].
    pub depth: u8,
}

/// A chip's expandable detail payload.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolDetail {
    /// Command/tool output as a code block: verbatim lines (indentation
    /// intact), capped at [`OUTPUT_DETAIL_MAX_LINES`] with a counted tail.
    Output {
        lines: Vec<SharedString>,
        truncated_by: usize,
    },
    /// A file diff, in the changes pane's model: hunks with 3 lines of
    /// context, dual line numbers, and (for recognized languages) syntax
    /// tokens — rendered by `changes::render_file_body`.
    Diff {
        file: Arc<crate::changes::FileDiff>,
        old_text: Option<Arc<str>>,
        new_text: Option<Arc<str>>,
    },
    /// Per-file `+N −N` stat rows — what the thin doc keeps of an edit
    /// (chat2-sync A1). The full diff upgrades this to [`ToolDetail::Diff`]
    /// via the sidecar fetch.
    Stats {
        stats: Arc<Vec<cypher_doc::ToolDiffStat>>,
    },
}

/// Max verbatim output lines per chip before the counted tail row.
pub const OUTPUT_DETAIL_MAX_LINES: usize = 24;

/// Max lines of a codemode script's invocation block. The script is the
/// whole point of the call and the doc keeps it (capped at
/// [`cypher_doc::CODEMODE_SCRIPT_MAX_CHARS`]), so it gets more room
/// than a command's echo before the counted tail.
pub const SCRIPT_DETAIL_MAX_LINES: usize = 80;

/// Max diff lines an inline tool-diff detail renders — the detail is one
/// stacked element inside its transcript row, so it must stay bounded
/// (~600 lines ≈ 12.6k px, several screens of context before the cut).
pub const DIFF_DETAIL_MAX_LINES: usize = 600;

/// Per-line height of an output detail block (diff blocks use the changes
/// pane's own [`crate::changes::DIFF_LINE_HEIGHT`]).
pub const OUTPUT_LINE_HEIGHT: f32 = 18.0;

/// Vertical padding of an output detail body (py(6) × 2).
pub(super) const OUTPUT_BODY_PAD: f32 = 12.0;

/// The hairline between an expanded chip's header row and its detail body.
pub(super) const DETAIL_SEPARATOR: f32 = 1.0;

/// Build a tool part's expandable detail. A diff wins over raw output (it is
/// the more structured record of the same action); post-strip docs carry diff
/// STATS instead of inline diff text, which win the same way.
pub fn tool_detail(
    output: Option<&str>,
    diff: Option<&cypher_proto::ToolDiff>,
    diff_stats: Option<&[cypher_doc::ToolDiffStat]>,
) -> Option<ToolDetail> {
    if let Some(diff) = diff {
        let mut file = diff_to_file(diff);
        if file.hunks.is_empty() {
            return None;
        }
        // A transcript diff renders as one stacked element inside its row —
        // cap it so a whole-file rewrite (or fetched full-diff blob) can't
        // build tens of thousands of elements per frame. The changes pane
        // has no such cap; it virtualizes per line.
        crate::changes::truncate_file_lines(&mut file, DIFF_DETAIL_MAX_LINES);
        return Some(ToolDetail::Diff {
            file: Arc::new(file),
            old_text: diff.old_text.as_deref().map(Arc::from),
            new_text: Some(Arc::from(diff.new_text.as_str())),
        });
    }
    if let Some(stats) = diff_stats.filter(|s| !s.is_empty()) {
        return Some(ToolDetail::Stats {
            stats: Arc::new(stats.to_vec()),
        });
    }
    let output = output?;
    let mut lines: Vec<SharedString> = output
        .lines()
        .map(|l| SharedString::from(l.to_owned()))
        .collect();
    // Trim trailing blank output lines so the block hugs its content.
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }
    let truncated_by = lines.len().saturating_sub(OUTPUT_DETAIL_MAX_LINES);
    lines.truncate(OUTPUT_DETAIL_MAX_LINES);
    Some(ToolDetail::Output {
        lines,
        truncated_by,
    })
}

/// Columns at which an invocation line soft-wraps into continuation lines.
/// The wrap is char-counted, not measured — block heights must be analytic —
/// so the budget is sized to fit the narrowest useful transcript pane.
pub const CALL_WRAP_COLS: usize = 80;

/// Soft-wrap one raw line into [`CALL_WRAP_COLS`]-char chunks so a long
/// single-line command stays fully readable instead of ellipsizing.
fn wrap_cols(line: &str, cols: usize) -> Vec<SharedString> {
    if line.chars().count() <= cols {
        return vec![SharedString::from(line.to_owned())];
    }
    line.chars()
        .collect::<Vec<_>>()
        .chunks(cols)
        .map(|chunk| SharedString::from(chunk.iter().collect::<String>()))
        .collect()
}

/// Build a chip's full-invocation block — the complete tool call the header
/// truncates to one line: the whole command, pattern, or URL, todo items one
/// per line, MCP/unknown input as pretty-printed JSON. Reuses the output
/// code-block payload so rendering and height stay one implementation.
pub fn call_block(call: &ToolCall) -> Option<ToolDetail> {
    let text: String = match call {
        ToolCall::Exec { command } => command.clone(),
        ToolCall::ReadFile { path } => path.clone(),
        ToolCall::WriteFile { path, content } => match content {
            Some(content) => format!("{path}\n{content}"),
            None => path.clone(),
        },
        ToolCall::EditFile { path, .. } => path.clone(),
        ToolCall::ApplyPatch { path } => path.clone().unwrap_or_else(|| "workspace".into()),
        ToolCall::Search { pattern, path } => match path {
            Some(path) => format!("{pattern} in {path}"),
            None => pattern.clone(),
        },
        ToolCall::Glob { pattern } => pattern.clone(),
        ToolCall::WebFetch { url, prompt } => match prompt {
            Some(prompt) => format!("{url}\n{prompt}"),
            None => url.clone(),
        },
        ToolCall::WebSearch { query } => query.clone(),
        ToolCall::Todo { items } => items
            .iter()
            .map(|i| format!("{} {}", if i.done { "[x]" } else { "[ ]" }, i.text))
            .collect::<Vec<_>>()
            .join("\n"),
        ToolCall::Mcp {
            server,
            tool,
            input,
        } => match input {
            Some(input) => format!(
                "{server} · {tool}\n{}",
                serde_json::to_string_pretty(input).unwrap_or_default()
            ),
            None => format!("{server} · {tool}\nInput details not retained in chat"),
        },
        // A script reads as the code it is, not as its JSON-escaped input.
        ToolCall::Unknown { name, .. } if name == cypher_proto::view::CODEMODE_TOOL => {
            match cypher_proto::view::codemode_script(call) {
                Some(code) => code.to_owned(),
                None => format!("{name}\nInput details not retained in chat"),
            }
        }
        ToolCall::Unknown { name, input } if name == cypher_proto::view::TOOL_SEARCH_TOOL => {
            match input
                .as_ref()
                .and_then(|input| input.get("query"))
                .and_then(serde_json::Value::as_str)
            {
                Some(query) => query.to_owned(),
                None => format!("{name}\nInput details not retained in chat"),
            }
        }
        ToolCall::Unknown { name, input } => match input {
            Some(input) => format!(
                "{name}\n{}",
                serde_json::to_string_pretty(input).unwrap_or_default()
            ),
            None => format!("{name}\nInput details not retained in chat"),
        },
    };
    // Blank lines around the invocation are formatting, not content (a
    // model's script routinely opens with a newline).
    let mut lines: Vec<SharedString> = text
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .flat_map(|l| wrap_cols(l, CALL_WRAP_COLS))
        .collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }
    let max_lines = if cypher_proto::view::codemode_script(call).is_some() {
        SCRIPT_DETAIL_MAX_LINES
    } else {
        OUTPUT_DETAIL_MAX_LINES
    };
    let truncated_by = lines.len().saturating_sub(max_lines);
    lines.truncate(max_lines);
    Some(ToolDetail::Output {
        lines,
        truncated_by,
    })
}

/// Reduce an inline [`cypher_proto::ToolDiff`] to the changes pane's
/// [`crate::changes::FileDiff`]: hunks grouped with 3 context lines, dual
/// 1-based line numbers, unified-diff hunk headers, and add/del counts.
pub fn diff_to_file(diff: &cypher_proto::ToolDiff) -> crate::changes::FileDiff {
    use crate::changes::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};
    let old = diff.old_text.as_deref().unwrap_or("");
    let text_diff = similar::TextDiff::from_lines(old, &diff.new_text);
    let mut hunks = Vec::new();
    let (mut additions, mut deletions) = (0u32, 0u32);
    let mut max_line = 0u32;
    for group in text_diff.grouped_ops(3) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let old_range = first.old_range().start..last.old_range().end;
        let new_range = first.new_range().start..last.new_range().end;
        let header = format!(
            "@@ -{},{} +{},{} @@",
            old_range.start + 1,
            old_range.len(),
            new_range.start + 1,
            new_range.len(),
        );
        let mut lines = Vec::new();
        for op in &group {
            for change in text_diff.iter_changes(op) {
                let kind = match change.tag() {
                    similar::ChangeTag::Delete => {
                        deletions += 1;
                        LineKind::Del
                    }
                    similar::ChangeTag::Insert => {
                        additions += 1;
                        LineKind::Add
                    }
                    similar::ChangeTag::Equal => LineKind::Context,
                };
                let old_no = change.old_index().map(|n| n as u32 + 1);
                let new_no = change.new_index().map(|n| n as u32 + 1);
                max_line = max_line.max(old_no.unwrap_or(0)).max(new_no.unwrap_or(0));
                lines.push(DiffLine {
                    kind,
                    old_no,
                    new_no,
                    text: change.value().trim_end_matches('\n').to_owned(),
                });
            }
        }
        hunks.push(Hunk { header, lines });
    }
    FileDiff {
        path: diff.path.clone(),
        old_path: None,
        status: if diff.old_text.is_none() {
            FileStatus::Added
        } else {
            FileStatus::Modified
        },
        binary: false,
        notices: Vec::new(),
        hunks,
        additions,
        deletions,
        max_line,
    }
}

#[derive(Clone)]
pub enum RowKind {
    User {
        /// Visible prompt (attachment-ref trailer already stripped). When the
        /// prompt carries file mentions this is the *projected* display text —
        /// chip labels in place of the raw Markdown links.
        text: SharedString,
        /// File-mention chips over `text`, in display-byte terms. Computed
        /// once per entry change in [`rows_for_entry`] (rows are cached by
        /// fingerprint), never per frame. Empty for ordinary prompts.
        mentions: Arc<Vec<crate::composer::SentMentionSpan>>,
        /// Attachment refs parsed out of the message text (message-attachments.ts):
        /// image thumbnails load from the owning device via
        /// ReadAttachmentChunk; other files render as tiles.
        attachments: Arc<Vec<crate::attachments::UserAttachment>>,
        /// The comments that rode this prompt, shown above the bubble.
        comments: Arc<Vec<MessageComment>>,
        /// Optimistic echo not yet confirmed by a doc frame.
        pending: bool,
        /// Sent as a Steer into a live turn: labelled above a quieter,
        /// outlined bubble (the iOS `UserBubble` treatment). Set in
        /// [`TranscriptView::rows_for`] from the command ledger.
        steer: bool,
    },
    /// One top-level markdown block of a completed message.
    Markdown {
        tree: Arc<BlockTree>,
        block_ix: usize,
    },
    /// One top-level block of a STREAMING message. Split per block like
    /// completed rows (only the tail blocks' versions change per commit, so
    /// the settled prefix is never respliced or re-rendered); rendered with
    /// the fade veil.
    LiveMarkdown {
        tree: Arc<BlockTree>,
        block_ix: usize,
    },
    ToolGroup {
        tools: Arc<Vec<ToolItem>>,
        auto_open: bool,
        /// Part of an [`RowKind::Activity`]: no header of its own, the chips
        /// always shown on the activity's rail.
        nested: bool,
        /// Leading chips its run's cap folds away ([`cap_work_runs`]); 0
        /// outside a run, where the group caps itself.
        skip: usize,
    },
    /// Stands in for the start of an open work run that the tool call cap
    /// folds away — "Show 6 earlier tool calls and 3 thoughts" — or, once
    /// the user revealed it (`tools == 0`), folds it back. Keyed by the run's
    /// [`RowKind::Activity`] row id (`run`).
    RunOverflow {
        run: SharedString,
        tools: usize,
        thoughts: usize,
    },
    /// The toggle over a run of work that mixed tool calls with thinking —
    /// one row instead of alternating "Thought" and "Ran N commands" rows.
    /// The `rows` that follow are the run in order (its tool groups, its
    /// thoughts and their blocks, all `nested`); they are dropped while the
    /// toggle is closed (see [`fold_closed_toggles`]). Open by default only
    /// while the run is the streaming tail, like a tool group.
    Activity {
        rows: usize,
        summary: SharedString,
        auto_open: bool,
    },
    /// The settled turn's work rule: a hairline labelled "Worked for 1m 32s",
    /// sitting between the turn's tool/chip activity and the answer text it
    /// produced. Only on entries that actually did work (see
    /// [`worked_rule_at`]).
    Worked {
        label: SharedString,
    },
    InputChip {
        /// First question's header (the passive chip is only rendered after
        /// the answer has resolved; the live question is rendered by the
        /// composer's wizard).
        header: SharedString,
        /// Stable request identity used to suppress a stale resolved mirror
        /// while the interactive wizard for the same request is visible.
        request_id: SharedString,
        resolved: bool,
    },
    ErrorChip {
        message: SharedString,
    },
    /// The toggle over an append-mode translation's original answer. The
    /// `blocks` rows that follow it — the original's blocks and the separator
    /// rule — are dropped from the list while the toggle is closed (the
    /// default, see [`fold_closed_toggles`]), so the answer reads like
    /// a replace-mode translation with the original one click away.
    TranslationOriginal {
        blocks: usize,
    },
    /// The toggle over a reasoning part: "Thinking…" while it streams,
    /// "Thought" once it settled. The `blocks` [`RowKind::ThoughtBlock`] rows
    /// that follow are dropped while it is closed, the default (see
    /// [`fold_closed_toggles`]).
    Thought {
        blocks: usize,
        live: bool,
        /// Inside an [`RowKind::Activity`]: drawn as a chip on its rail,
        /// labelled with `preview`.
        nested: bool,
        /// The thought's first line, markup stripped.
        preview: SharedString,
    },
    /// One top-level markdown block of a reasoning part, painted muted.
    /// `live` blocks fade in like [`RowKind::LiveMarkdown`].
    ThoughtBlock {
        tree: Arc<BlockTree>,
        block_ix: usize,
        live: bool,
        /// Inside an [`RowKind::Activity`]: indented on its rail.
        nested: bool,
    },
}

/// Rows that belong to an [`RowKind::Activity`] run.
pub(super) fn is_nested(kind: &RowKind) -> bool {
    matches!(
        kind,
        RowKind::ToolGroup { nested: true, .. }
            | RowKind::Thought { nested: true, .. }
            | RowKind::ThoughtBlock { nested: true, .. }
            | RowKind::RunOverflow { .. }
    )
}

/// Rows whose text streams in under a fade veil.
pub(super) fn is_live_markdown(kind: &RowKind) -> bool {
    matches!(
        kind,
        RowKind::LiveMarkdown { .. } | RowKind::ThoughtBlock { live: true, .. }
    )
}

/// A transcript row: stable id + content version (diff key) + block payload.
#[derive(Clone)]
pub struct Row {
    pub id: SharedString,
    pub version: u64,
    /// First row of its message entry (gets the turn gap).
    pub turn_start: bool,
    pub kind: RowKind,
    /// The owning message entry — hover anywhere on the entry's rows reveals
    /// its timestamp strip (zeron chat-view.tsx `group`/`group-hover`).
    pub entry_id: SharedString,
    /// The owning entry's role — drives the fork affordance's role-specific
    /// tooltip AND gates System rows entirely (they never fork).
    pub role: MessageRole,
    /// Epoch-ms for the 16px hover-timestamp strip UNDER this row: set on the
    /// LAST row of a completed entry (user rows always; assistant rows only
    /// once streaming ends — "the turn isn't at a time yet", chat-view.tsx).
    pub timestamp: Option<i64>,
    /// The model(s) that wrote the answer, revealed after the timestamp in
    /// the same hover strip. Travels with `timestamp`; set only on settled
    /// assistant entries whose harness reported the model (pi).
    pub answered: Option<AnsweredLabel>,
}

/// What a settled answer's strip says about the model(s) that wrote it.
#[derive(Clone, Debug, PartialEq)]
pub struct AnsweredLabel {
    /// The answering model ids, in the order they first answered.
    pub text: SharedString,
    /// A model answered that wasn't the one requested: the strip highlights
    /// the label and this explains it on hover.
    pub substituted: Option<SharedString>,
}

/// The strip label for an entry's answering models; `None` when the entry
/// records none (another harness, or written before models were recorded).
pub fn answered_label(models: &[AnsweredModel]) -> Option<AnsweredLabel> {
    let mut served: Vec<&str> = Vec::new();
    let mut swaps: Vec<String> = Vec::new();
    for answered in models {
        if !served.contains(&answered.model.as_str()) {
            served.push(&answered.model);
        }
        if answered.substituted()
            && let Some(requested) = &answered.requested
        {
            let swap = format!("Requested {requested}, answered by {}", answered.model);
            if !swaps.contains(&swap) {
                swaps.push(swap);
            }
        }
    }
    if served.is_empty() {
        return None;
    }
    Some(AnsweredLabel {
        text: served.join(", ").into(),
        substituted: (!swaps.is_empty()).then(|| swaps.join("\n").into()),
    })
}

/// Whether a user row renders as the quiet slash-command action chip rather
/// than a prompt bubble (settings commands are not a message to the model).
/// Shared by the renderer and the find index so the two agree on which rows
/// carry searchable, highlightable bubble text.
pub(super) fn renders_as_command_chip(
    text: &str,
    mentions: &[crate::composer::SentMentionSpan],
    attachments: &[crate::attachments::UserAttachment],
) -> bool {
    crate::composer::slash_command_label(text).is_some()
        && mentions.is_empty()
        && attachments.is_empty()
}

/// How many find matches a row holds, over exactly the text elements the row
/// renders as selectable text. Rows without one (tool groups, chips, the
/// worked rule, command chips, image-only sends) contribute nothing: there is
/// no laid-out text model to wash a highlight into, and reporting hits the
/// user cannot see would break the "n of N" contract.
pub(super) fn row_match_count(row: &Row, query: &str) -> u32 {
    let count = match &row.kind {
        RowKind::User {
            text,
            mentions,
            attachments,
            ..
        } => {
            if text.is_empty() || renders_as_command_chip(text, mentions, attachments) {
                0
            } else {
                crate::find::count_matches(text, query)
            }
        }
        RowKind::Markdown { tree, block_ix } | RowKind::LiveMarkdown { tree, block_ix } => tree
            .blocks
            .get(*block_ix)
            .map_or(0, |top| render::count_block_matches(&top.block, query)),
        // Thinking is not searched: a hit inside a collapsed thought could
        // not be shown.
        RowKind::ToolGroup { .. }
        | RowKind::Activity { .. }
        | RowKind::RunOverflow { .. }
        | RowKind::Worked { .. }
        | RowKind::InputChip { .. }
        | RowKind::ErrorChip { .. }
        | RowKind::TranslationOriginal { .. }
        | RowKind::Thought { .. }
        | RowKind::ThoughtBlock { .. } => 0,
    };
    count.min(u32::MAX as usize) as u32
}

/// A resolved transcript mirror for the request currently served by the
/// interactive composer wizard. The mirror can arrive during replay before
/// the unresolved part is removed; suppress it to avoid rendering the same
/// question in two different UI surfaces.
pub(super) fn is_pending_input_duplicate(row: &Row, pending_request_id: Option<&str>) -> bool {
    let Some(pending_request_id) = pending_request_id else {
        return false;
    };
    matches!(
        &row.kind,
        RowKind::InputChip { request_id, .. }
            if request_id.as_ref() == pending_request_id
    )
}

/// Absolute hover-timestamp label, e.g. "Jul 1, 3:45 PM" — the exact
/// `formatTimestamp` shape (utils.ts: short month, numeric day, hour,
/// 2-digit minutes, no leading zero on the hour). Pure over an explicit
/// timezone so tests don't depend on the host's local time.
pub fn format_timestamp<Tz: chrono::TimeZone>(ms: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(utc) => utc
            .with_timezone(tz)
            .format("%b %-d, %-I:%M %p")
            .to_string(),
        None => String::new(),
    }
}

/// Events the transcript emits to the shell (Session Fork v1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptEvent {
    /// The user clicked a settled entry's fork affordance: fork the
    /// conversation at `anchor_message_id` (before this user / from this
    /// response) into a NEW durable root chat.
    ForkRequested {
        chat_id: String,
        anchor_message_id: String,
    },
    /// The user CONFIRMED a settled entry's rewind affordance: restart the
    /// conversation at `anchor_message_id` inside THIS chat — everything
    /// after the boundary is deleted and the chat's Pi session is re-pointed
    /// at the truncated one. Emitted only on the second (confirming) click.
    RewindRequested {
        chat_id: String,
        anchor_message_id: String,
    },
}

impl EventEmitter<TranscriptEvent> for Transcript {}

/// Session Fork affordance state for the timestamp strip's git-branch icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkGate {
    /// Shown and clickable: the source is a settled Pi root chat.
    Enabled,
    /// Shown but inert (dimmed), with the reason for the tooltip.
    Disabled(&'static str),
}

/// The fork affordance gate (pure — tests exercise the real gating without a
/// gpui App). Embedded (Side Chat) panels, offline engines, child chats,
/// non-Pi configs, live (Working/AwaitingInput) chats, and an OFFLINE SOURCE
/// HOST DEVICE are all disabled. Remote Pi chats stay ENABLED while their
/// host is online: the shell relays `ForkSession` to the source chat's host
/// device.
pub fn fork_gate(
    embedded: bool,
    chat: Option<&Chat>,
    live: bool,
    offline: bool,
    host_online: bool,
) -> ForkGate {
    if embedded {
        return ForkGate::Disabled("Side chats can't be forked.");
    }
    if offline {
        return ForkGate::Disabled("The engine is offline.");
    }
    let Some(chat) = chat else {
        return ForkGate::Disabled("No chat selected.");
    };
    if chat.is_child() {
        return ForkGate::Disabled("Subagent chats can't be forked.");
    }
    if chat.config.as_ref().map(|c| c.harness) != Some(HarnessId::Pi) {
        return ForkGate::Disabled("Only Pi chats can be forked.");
    }
    if live {
        return ForkGate::Disabled("Wait for the chat to finish before forking.");
    }
    if !host_online {
        return ForkGate::Disabled("The device hosting this chat is offline.");
    }
    ForkGate::Enabled
}

/// The fork affordance's tooltip: role-specific while enabled, the disabled
/// reason otherwise. Enabled text is exactly `Fork before this message` for
/// User and `Fork after this response` for Assistant.
pub fn fork_tooltip(role: MessageRole, gate: &ForkGate) -> &'static str {
    match gate {
        ForkGate::Disabled(reason) => reason,
        ForkGate::Enabled => match role {
            MessageRole::User => "Fork before this message",
            MessageRole::Assistant => "Fork after this response",
            // System rows never emit a fork affordance; keep a fallback text
            // so a misroute is still coherent.
            MessageRole::System => "Fork after this message",
        },
    }
}

/// The rewind affordance gate (pure, like [`fork_gate`]). Restarting the
/// conversation from a message runs the SAME pi machinery as a fork — it just
/// lands in place — so the prerequisites match, worded for a restart. One
/// extra rule: the NEWEST entry has nothing after it, so restarting there
/// would delete nothing.
pub fn rewind_gate(
    embedded: bool,
    chat: Option<&Chat>,
    live: bool,
    offline: bool,
    host_online: bool,
    is_last_entry: bool,
) -> ForkGate {
    if embedded {
        return ForkGate::Disabled("Side chats can't be restarted from a message.");
    }
    if offline {
        return ForkGate::Disabled("The engine is offline.");
    }
    let Some(chat) = chat else {
        return ForkGate::Disabled("No chat selected.");
    };
    if chat.is_child() {
        return ForkGate::Disabled("Subagent chats can't be restarted from a message.");
    }
    if chat.config.as_ref().map(|c| c.harness) != Some(HarnessId::Pi) {
        return ForkGate::Disabled("Only Pi chats can be restarted from a message.");
    }
    if live {
        return ForkGate::Disabled("Wait for the chat to finish before restarting it.");
    }
    if !host_online {
        return ForkGate::Disabled("The device hosting this chat is offline.");
    }
    if is_last_entry {
        return ForkGate::Disabled("Nothing to remove after the last message.");
    }
    ForkGate::Enabled
}

/// The rewind affordance's tooltip. The ARMED text (after the first click)
/// spells out what the confirming click deletes — the removal is permanent,
/// so the count is never left implicit.
pub fn rewind_tooltip(role: MessageRole, gate: &ForkGate, armed: bool, later: usize) -> String {
    match gate {
        ForkGate::Disabled(reason) => (*reason).to_string(),
        ForkGate::Enabled if armed => match role {
            MessageRole::User => format!(
                "Click again to delete this message and {} after it",
                plural_messages(later)
            ),
            _ => format!(
                "Click again to delete {} after this response",
                plural_messages(later)
            ),
        },
        ForkGate::Enabled => match role {
            MessageRole::User => {
                "Restart from here — deletes this message and everything after it".to_string()
            }
            _ => "Restart from here — deletes everything after this response".to_string(),
        },
    }
}

fn plural_messages(count: usize) -> String {
    if count == 1 {
        "1 message".to_string()
    } else {
        format!("{count} messages")
    }
}

/// Text copied by the entry-level action, not by a virtualized markdown row.
/// Keep Markdown intact; tool payloads and input-wizard internals aren't prose.
pub(super) fn message_copy_text(entry: &SessionMessageEntry) -> Option<String> {
    let text = entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            MessagePart::Error { message, .. } if entry.role != MessageRole::User => {
                Some(message.as_str())
            }
            _ => None,
        })
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let text = if entry.role == MessageRole::User {
        // Match the visible user bubble: no attachment transport paths or
        // internal file/session mention URLs on the clipboard.
        let parsed = crate::attachments::parse_user_message_attachments(&text);
        crate::composer::sent_mention_display(&parsed.text)
            .map(|(display, _)| display)
            .unwrap_or(parsed.text)
    } else {
        text
    };
    (!text.trim().is_empty()).then_some(text)
}

pub(super) fn message_for_copy<'a>(
    entries: &'a [SessionMessageEntry],
    echoes: &'a [SessionMessageEntry],
    entry_id: &str,
) -> Option<&'a SessionMessageEntry> {
    // A durable message wins over an optimistic mirror with the same id.
    entries
        .iter()
        .chain(echoes)
        .find(|entry| entry.id == entry_id)
}

pub(super) fn message_copy_icon(copied: bool, enabled: bool, theme: &Theme) -> gpui::Svg {
    // Svg::paint reads its own text.color, not the enclosing div's color.
    let color = if !enabled {
        theme.text_muted.opacity(0.3)
    } else if copied {
        theme.success
    } else {
        theme.text_muted
    };
    crate::icons::icon(if copied {
        crate::icons::CHECK
    } else {
        crate::icons::COPY
    })
    .size(px(10.0))
    .text_color(color)
}

/// Shared styling for the message strip's action tooltips.
pub(super) struct MessageActionTooltip {
    pub(super) text: SharedString,
}

impl Render for MessageActionTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        motion::fade_quick(
            "message-action-tooltip",
            div()
                .max_w(px(280.0))
                .px(px(8.0))
                .py(px(5.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.surface_raised)
                .text_size(px(11.0))
                .text_color(theme.text_muted)
                .child(self.text.clone()),
        )
    }
}

pub(super) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x1_0000_01b3);
    }
    hash
}

pub(super) fn tool_fingerprint(tools: &[ToolItem], auto_open: bool) -> u64 {
    let mut acc = Vec::with_capacity(tools.len() * 8 + 1);
    for t in tools {
        let (label, detail) = tool_chip_content(&t.call);
        acc.extend_from_slice(label.as_bytes());
        acc.extend_from_slice(&(detail.len() as u32).to_le_bytes());
        acc.push(t.is_error as u8 | (t.resolved as u8) << 1);
        // A call nesting under its caller once the caller's part arrives
        // re-indents the chip.
        acc.push(t.depth);
        // Detail payload arriving (or growing) must re-splice the row even
        // when the resolved bit didn't change.
        match t.detail.as_deref() {
            None => acc.push(0),
            Some(ToolDetail::Output {
                lines,
                truncated_by,
            }) => {
                acc.push(1);
                acc.extend_from_slice(&(lines.len() as u32).to_le_bytes());
                acc.extend_from_slice(&(*truncated_by as u32).to_le_bytes());
                let bytes: usize = lines.iter().map(|l| l.len()).sum();
                acc.extend_from_slice(&(bytes as u32).to_le_bytes());
            }
            Some(ToolDetail::Diff { file, .. }) => {
                acc.push(2);
                acc.extend_from_slice(file.path.as_bytes());
                acc.extend_from_slice(&file.additions.to_le_bytes());
                acc.extend_from_slice(&file.deletions.to_le_bytes());
                acc.extend_from_slice(&(file.hunks.len() as u32).to_le_bytes());
            }
            Some(ToolDetail::Stats { stats }) => {
                acc.push(3);
                for stat in stats.iter() {
                    acc.extend_from_slice(stat.path.as_bytes());
                    acc.extend_from_slice(&stat.additions.to_le_bytes());
                    acc.extend_from_slice(&stat.deletions.to_le_bytes());
                }
            }
        }
        // The invocation block is pure over `call`, which the one-line hash
        // above only covers by length — hash its bytes so an in-place call
        // update (a streaming MCP input, a growing todo list) re-splices.
        if let Some(ToolDetail::Output {
            lines,
            truncated_by,
        }) = t.invocation.as_deref()
        {
            for line in lines {
                acc.extend_from_slice(line.as_bytes());
            }
            acc.extend_from_slice(&(*truncated_by as u32).to_le_bytes());
        }
        // Sidecar refs arriving after the resolve tick must re-splice too —
        // they add the fetch affordance without changing the detail payload.
        acc.push(t.output_ref.is_some() as u8 | (t.diff_ref.is_some() as u8) << 1);
    }
    acc.push(auto_open as u8);
    fnv1a(&acc)
}

/// The comments a prompt carried, right-aligned above its bubble: each quote
/// (one muted line) over what the user wrote about it. Both texts select
/// like the bubble's, keyed `{row}:{n}` in paint order ahead of its `:u`.
pub(super) fn user_comments(
    row_id: &SharedString,
    comments: &[MessageComment],
    wide: bool,
    pending: bool,
    theme: &Theme,
    scope: crate::markdown::selection::SelectionScope,
    selection: Option<render::SelectionUi>,
) -> gpui::Div {
    let mut list = div()
        .min_w_0()
        .when(!wide, |el| el.max_w(px(MAX_CONTENT_WIDTH * 0.8)))
        .when(wide, |el| el.max_w(gpui::relative(0.8)))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .when(pending, |el| el.opacity(0.65));
    for (ix, comment) in comments.iter().enumerate() {
        let quote = selectable_text(
            format!("{row_id}:{}", ix * 2).into(),
            crate::composer::comment_quote_preview(&comment.quote).into(),
            theme.text_muted,
            theme,
            scope,
            selection.clone(),
        );
        let text = selectable_text(
            format!("{row_id}:{}", ix * 2 + 1).into(),
            comment.comment.clone().into(),
            theme.text,
            theme,
            scope,
            selection.clone(),
        );
        list = list.child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .pl(px(10.0))
                .border_l_2()
                .border_color(theme.border)
                .child(div().text_size(px(11.5)).line_height(px(16.0)).child(quote))
                .child(div().text_size(px(13.0)).line_height(px(18.0)).child(text)),
        );
    }
    div().w_full().flex().justify_end().pb(px(6.0)).child(list)
}

/// One plain run of transcript text registered for drag selection under
/// `key` (see [`user_bubble_text`]).
fn selectable_text(
    key: std::sync::Arc<str>,
    text: SharedString,
    color: gpui::Hsla,
    theme: &Theme,
    scope: crate::markdown::selection::SelectionScope,
    selection: Option<render::SelectionUi>,
) -> AnyElement {
    let styled = StyledText::new(text.clone()).with_runs(vec![TextRun {
        len: text.len(),
        font: gpui::font(theme.font_sans.clone()),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }]);
    let layout = styled.layout().clone();
    let sel_theme = theme.clone();
    let underlay = canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            render::paint_text_selection(
                window, scope, &key, &text, &layout, &sel_theme, selection,
            );
        },
    )
    .absolute()
    .size_full();
    div()
        .relative()
        .child(underlay)
        .child(styled)
        .into_any_element()
}

/// Flag an entry's user row as a steer. The version bit keeps the row diff
/// honest when the ledger confirms a steer after the row first rendered.
pub(super) fn mark_steer_rows(rows: &mut [Row]) {
    for row in rows {
        if let RowKind::User { steer, .. } = &mut row.kind {
            *steer = true;
            row.version ^= 1 << 63;
        }
    }
}

pub(super) fn user_entry_is_slash_command(entry: &SessionMessageEntry) -> bool {
    let text: String = entry
        .parts
        .iter()
        .filter_map(|p| match p {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    crate::composer::slash_command_label(&text).is_some()
}

/// Build the block rows of one (already continuation-joined) entry.
///
/// `parse` maps `(part_key, text)` to a block tree — the entity supplies
/// incremental parsers for live parts and a cache for complete ones; tests pass
/// a plain `parse_full`.
pub fn rows_for_entry(
    entry: &SessionMessageEntry,
    pending: bool,
    parse: &mut dyn FnMut(&str, &str) -> Arc<BlockTree>,
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let streaming = entry.status == Some(MessageStatus::Streaming);
    let entry_id: SharedString = entry.id.clone().into();

    if entry.role == MessageRole::User {
        let raw: String = entry
            .parts
            .iter()
            .filter_map(|p| match p {
                MessagePart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        // Attachment refs ride the plain text (the `withAttachments`
        // transport); split them back out for the thumbnail strip.
        let parsed = crate::attachments::parse_user_message_attachments(&raw);
        // File mentions render as chips here too, not just in the composer.
        // The projection is pure over the text, so the raw-length row version
        // below stays a valid cache/diff key.
        let (text, mentions) = match crate::composer::sent_mention_display(&parsed.text) {
            Some((display, spans)) => (display, spans),
            None => (parsed.text, Vec::new()),
        };
        // Comments are fixed at send, but the echo and the doc frame must
        // still agree on them for the row cache. Uncommented prompts keep the
        // plain raw-length key.
        let comments_key = (!entry.comments.is_empty()).then(|| {
            let mut acc = Vec::new();
            for comment in &entry.comments {
                acc.extend_from_slice(&fnv1a(comment.quote.as_bytes()).to_le_bytes());
                acc.extend_from_slice(&fnv1a(comment.comment.as_bytes()).to_le_bytes());
            }
            fnv1a(&acc)
        });
        return vec![Row {
            id: entry.id.clone().into(),
            version: ((raw.len() as u64) ^ comments_key.unwrap_or(0)) << 1 | pending as u64,
            turn_start: true,
            kind: RowKind::User {
                text: text.into(),
                mentions: Arc::new(mentions),
                attachments: Arc::new(parsed.attachments),
                comments: Arc::new(entry.comments.clone()),
                pending,
                steer: false,
            },
            entry_id,
            role: entry.role,
            // User rows always carry the strip (chat-view.tsx: whenever
            // `createdAt` exists — the optimistic echo included).
            timestamp: Some(entry.created_at),
            answered: None,
        }];
    }

    // Assistant/system: split parts into block rows, folding consecutive tools.
    let last_part_ix = entry.parts.len().saturating_sub(1);
    let mut group_ix = 0usize;
    // Each tool with its part id, which carries the nesting
    // ([`nest_tool_calls`]).
    let mut pending_group: Vec<(String, ToolItem)> = Vec::new();
    let mut group_last_part_ix = 0usize;

    let flush_group = |rows: &mut Vec<Row>,
                       group: &mut Vec<(String, ToolItem)>,
                       group_ix: &mut usize,
                       last_ix: usize| {
        if group.is_empty() {
            return;
        }
        let tools = nest_tool_calls(std::mem::take(group));
        let auto_open = streaming && last_ix == last_part_ix;
        rows.push(Row {
            id: format!("{}#g{}", entry.id, group_ix).into(),
            version: tool_fingerprint(&tools, auto_open),
            turn_start: false,
            kind: RowKind::ToolGroup {
                tools: Arc::new(tools),
                auto_open,
                nested: false,
                skip: 0,
            },
            entry_id: entry.id.clone().into(),
            role: entry.role,
            timestamp: None,
            answered: None,
        });
        *group_ix += 1;
    };
    // The open work run: where its rows start and the part that opened it
    // (the Activity row's id). Tool calls and thoughts extend it; anything
    // the reader sees between them (answer text, a chip) closes it.
    let mut run: Option<(usize, String)> = None;

    for (part_ix, part) in entry.parts.iter().enumerate() {
        match part {
            MessagePart::Tool {
                id: part_id,
                call,
                is_error,
                resolved,
                output,
                diff,
                output_ref,
                output_bytes,
                diff_ref,
                diff_stats,
                ..
            } => {
                // Nothing is pushed until the group flushes, so the run's
                // rows start here.
                run.get_or_insert_with(|| (rows.len(), part_id.clone()));
                pending_group.push((
                    part_id.clone(),
                    ToolItem {
                        call: call.clone(),
                        is_error: *is_error,
                        resolved: *resolved,
                        detail: tool_detail(
                            output.as_deref(),
                            diff.as_ref(),
                            diff_stats.as_deref(),
                        )
                        .map(Arc::new),
                        invocation: call_block(call).map(Arc::new),
                        output_ref: output_ref.clone().map(SharedString::from),
                        output_bytes: *output_bytes,
                        diff_ref: diff_ref.clone().map(SharedString::from),
                        depth: 0,
                    },
                ));
                group_last_part_ix = part_ix;
            }
            other => {
                // A part that renders nothing (an empty text or thought, a
                // question still in the composer) splits nothing either.
                let silent = match other {
                    MessagePart::Text { text, .. } | MessagePart::Reasoning { text, .. } => {
                        text.trim().is_empty()
                    }
                    MessagePart::Input { resolved, .. } => !*resolved,
                    MessagePart::Error { .. } | MessagePart::Tool { .. } => false,
                };
                if silent {
                    continue;
                }
                flush_group(
                    &mut rows,
                    &mut pending_group,
                    &mut group_ix,
                    group_last_part_ix,
                );
                if !matches!(other, MessagePart::Reasoning { .. })
                    && let Some((start, first_part)) = run.take()
                {
                    fold_work_run(&mut rows, start, &first_part, entry, false);
                }
                match other {
                    MessagePart::Text {
                        id: part_id,
                        text,
                        agent_text,
                    } => {
                        let key = format!("{}#{}", entry.id, part_id);
                        let tree = parse(&key, text);
                        // Block rows keep their ids either way (quotes map
                        // back through them), so the toggle only ever hides
                        // or shows rows — see `fold_closed_toggles`.
                        if let Some(blocks) = agent_text
                            .as_deref()
                            .and_then(|agent| appended_original_blocks(text, agent, &tree))
                        {
                            rows.push(Row {
                                id: format!("{key}.original").into(),
                                version: (blocks as u64) << 1,
                                turn_start: false,
                                entry_id: entry_id.clone(),
                                role: entry.role,
                                timestamp: None,
                                answered: None,
                                kind: RowKind::TranslationOriginal { blocks },
                            });
                        }
                        // Live and completed parts split identically — one row
                        // per top-level block, same ids, so the live→complete
                        // handoff never changes row identity. The version is a
                        // content hash of the block's bytes (LSB = streaming),
                        // so a commit only splices rows whose bytes actually
                        // changed — the settled prefix of a live reply is
                        // untouched (and its render caches stay valid).
                        for block_ix in 0..tree.blocks.len() {
                            let range = &tree.blocks[block_ix].range;
                            let end = range.end.min(text.len());
                            let bytes = text
                                .as_bytes()
                                .get(range.start.min(end)..end)
                                .unwrap_or_default();
                            let version = (fnv1a(bytes) << 1) | streaming as u64;
                            rows.push(Row {
                                id: format!("{key}.{block_ix}").into(),
                                version,
                                turn_start: false,
                                entry_id: entry_id.clone(),
                                role: entry.role,
                                timestamp: None,
                                answered: None,
                                kind: if streaming {
                                    RowKind::LiveMarkdown {
                                        tree: tree.clone(),
                                        block_ix,
                                    }
                                } else {
                                    RowKind::Markdown {
                                        tree: tree.clone(),
                                        block_ix,
                                    }
                                },
                            });
                        }
                    }
                    MessagePart::Reasoning { id: part_id, text } => {
                        run.get_or_insert_with(|| (rows.len(), part_id.clone()));
                        let key = format!("{}#{}", entry.id, part_id);
                        let tree = parse(&key, text);
                        // Still thinking: the reasoning is the live tail.
                        let live = streaming && part_ix == last_part_ix;
                        let preview = thought_preview(text);
                        rows.push(Row {
                            id: format!("{key}.thought").into(),
                            version: ((tree.blocks.len() as u64) << 1 | live as u64)
                                ^ fnv1a(preview.as_bytes()) << 8,
                            turn_start: false,
                            entry_id: entry_id.clone(),
                            role: entry.role,
                            timestamp: None,
                            answered: None,
                            kind: RowKind::Thought {
                                blocks: tree.blocks.len(),
                                live,
                                nested: false,
                                preview: preview.into(),
                            },
                        });
                        // Same ids and content-hash versions as answer blocks,
                        // so a streaming thought only splices its tail.
                        for block_ix in 0..tree.blocks.len() {
                            let range = &tree.blocks[block_ix].range;
                            let end = range.end.min(text.len());
                            let bytes = text
                                .as_bytes()
                                .get(range.start.min(end)..end)
                                .unwrap_or_default();
                            rows.push(Row {
                                id: format!("{key}.{block_ix}").into(),
                                version: (fnv1a(bytes) << 1) | live as u64,
                                turn_start: false,
                                entry_id: entry_id.clone(),
                                role: entry.role,
                                timestamp: None,
                                answered: None,
                                kind: RowKind::ThoughtBlock {
                                    tree: tree.clone(),
                                    block_ix,
                                    live,
                                    nested: false,
                                },
                            });
                        }
                    }
                    MessagePart::Input {
                        id: part_id,
                        request_id,
                        questions,
                        resolved,
                        ..
                    } => {
                        // Only resolved questions get here (`silent`): the
                        // composer wizard is the interaction, and a pending
                        // "Awaiting your answer…" chip made slash-command
                        // settings read as the model asking a question.
                        // Model-generated header onto the one-line chip.
                        let header: SharedString = single_line(
                            &questions
                                .first()
                                .map(|q| q.header.clone())
                                .unwrap_or_else(|| "Question".to_string()),
                        )
                        .into();
                        rows.push(Row {
                            id: format!("{}#{}", entry.id, part_id).into(),
                            version: fnv1a(header.as_bytes()) << 1 | *resolved as u64,
                            turn_start: false,
                            kind: RowKind::InputChip {
                                header,
                                request_id: request_id.clone().into(),
                                resolved: *resolved,
                            },
                            entry_id: entry_id.clone(),
                            role: entry.role,
                            timestamp: None,
                            answered: None,
                        });
                    }
                    MessagePart::Error {
                        id: part_id,
                        message,
                    } => {
                        rows.push(Row {
                            id: format!("{}#{}", entry.id, part_id).into(),
                            version: message.len() as u64,
                            turn_start: false,
                            kind: RowKind::ErrorChip {
                                // Harness-generated; the chip is one line.
                                message: single_line(message).into(),
                            },
                            entry_id: entry_id.clone(),
                            role: entry.role,
                            timestamp: None,
                            answered: None,
                        });
                    }
                    // Tools are grouped by the outer arm; nothing reaches here.
                    MessagePart::Tool { .. } => {}
                }
            }
        }
    }
    flush_group(
        &mut rows,
        &mut pending_group,
        &mut group_ix,
        group_last_part_ix,
    );
    if let Some((start, first_part)) = run.take() {
        // Still the tail of a streaming reply: open, like a live tool group.
        fold_work_run(&mut rows, start, &first_part, entry, streaming);
    }

    // The work rule goes in BEFORE the turn-start/timestamp bookkeeping: it is
    // never the entry's first or last row (it separates work from the answer
    // that followed it), so neither marker can land on it.
    if !streaming
        && let Some(at) = worked_rule_at(&rows)
        && let Some(label) = worked_label(entry.created_at, entry.completed_at)
    {
        rows.insert(
            at,
            Row {
                id: format!("{}#worked", entry.id).into(),
                version: fnv1a(label.as_bytes()),
                turn_start: false,
                kind: RowKind::Worked { label },
                entry_id: entry_id.clone(),
                role: entry.role,
                timestamp: None,
                answered: None,
            },
        );
    }

    if let Some(first) = rows.first_mut() {
        first.turn_start = true;
    }
    // Timestamp strip under the entry's LAST row once the turn has settled
    // (chat-view.tsx: "No timestamp hover mid-stream"). The version bit keeps
    // the diff key honest for last-row kinds whose own version wouldn't
    // change when streaming flips off (chips).
    if !streaming && let Some(last) = rows.last_mut() {
        last.timestamp = Some(entry.created_at);
        last.version ^= 1 << 62;
        if entry.role == MessageRole::Assistant
            && let Some(label) = answered_label(&entry.models)
        {
            last.version ^= fnv1a(label.text.as_bytes()).rotate_left(1)
                ^ u64::from(label.substituted.is_some());
            last.answered = Some(label);
        }
    }
    rows
}

/// Fold a closed work run — `rows[start..]`, opened by part `first_part` —
/// behind one [`RowKind::Activity`] row when it mixed tool calls with
/// thinking. A run of only tools is one group already and a lone thought its
/// own toggle; both stay as they are.
fn fold_work_run(
    rows: &mut Vec<Row>,
    start: usize,
    first_part: &str,
    entry: &SessionMessageEntry,
    auto_open: bool,
) {
    let mut thoughts = 0usize;
    let mut tools: Vec<(ToolCall, bool)> = Vec::new();
    for row in &rows[start..] {
        match &row.kind {
            RowKind::Thought { .. } => thoughts += 1,
            RowKind::ToolGroup { tools: group, .. } => {
                tools.extend(group.iter().map(|t| (t.call.clone(), t.is_error)));
            }
            _ => {}
        }
    }
    if thoughts == 0 || tools.is_empty() {
        return;
    }
    for row in &mut rows[start..] {
        if let RowKind::ToolGroup { nested, .. }
        | RowKind::Thought { nested, .. }
        | RowKind::ThoughtBlock { nested, .. } = &mut row.kind
        {
            *nested = true;
            // A thought that gains its first tool call redraws as a chip.
            row.version ^= 1 << 61;
        }
    }
    let summary = cypher_proto::view::work_summary(&tools, thoughts);
    let count = rows.len() - start;
    rows.insert(
        start,
        Row {
            id: format!("{}#{}.activity", entry.id, first_part).into(),
            version: (fnv1a(summary.as_bytes()) ^ count as u64) << 1 | auto_open as u64,
            turn_start: false,
            kind: RowKind::Activity {
                rows: count,
                summary: summary.into(),
                auto_open,
            },
            entry_id: entry.id.clone().into(),
            role: entry.role,
            timestamp: None,
            answered: None,
        },
    );
}

/// A thought's label in a work run: its first line, without the heading or
/// emphasis markers models often title a thought with ("**Planning**").
pub(super) fn thought_preview(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut line = line.trim_start_matches('#').trim_start();
    for marker in ["**", "__", "*", "_"] {
        if line.len() > marker.len() * 2 && line.starts_with(marker) && line.ends_with(marker) {
            line = &line[marker.len()..line.len() - marker.len()];
            break;
        }
    }
    single_line(line)
}

/// Order a tool group so each call made from inside another call's run
/// follows that call, one level deeper. Pi runs the calls a codemode script
/// makes (`await tools.read(…)`) through its own tool pipeline and reports
/// each as a tool call whose id is `{caller id}/{n}`; the doc keeps those ids,
/// so the nesting needs no field of its own — old transcripts nest too, and
/// viewers that predate this list the same calls flat.
///
/// A call whose caller is not in the group (a different group, or an id that
/// merely contains `/`) stays where it is at depth 0. Siblings keep their
/// arrival order.
pub(super) fn nest_tool_calls(items: Vec<(String, ToolItem)>) -> Vec<ToolItem> {
    let index: HashMap<&str, usize> = items
        .iter()
        .enumerate()
        .map(|(ix, (id, _))| (id.as_str(), ix))
        .collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    let mut roots: Vec<usize> = Vec::new();
    for (ix, (id, _)) in items.iter().enumerate() {
        // A caller's id is a strict prefix of its calls' ids, so this never
        // cycles.
        match id
            .rsplit_once('/')
            .and_then(|(caller, _)| index.get(caller).copied())
        {
            Some(caller) => children[caller].push(ix),
            None => roots.push(ix),
        }
    }
    if roots.len() == items.len() {
        return items.into_iter().map(|(_, item)| item).collect();
    }
    let mut order: Vec<(usize, u8)> = Vec::with_capacity(items.len());
    let mut stack: Vec<(usize, u8)> = roots.iter().rev().map(|&ix| (ix, 0)).collect();
    while let Some((ix, depth)) = stack.pop() {
        order.push((ix, depth));
        for &child in children[ix].iter().rev() {
            stack.push((child, depth.saturating_add(1)));
        }
    }
    let mut slots: Vec<Option<ToolItem>> = items.into_iter().map(|(_, item)| Some(item)).collect();
    order
        .into_iter()
        .filter_map(|(ix, depth)| {
            let mut item = slots[ix].take()?;
            item.depth = depth;
            Some(item)
        })
        .collect()
}

/// How many top-level blocks of an append-mode translation's `tree` belong to
/// the folded original: the original's own blocks plus the separator rule.
/// `None` unless `text` is `agent`, the separator and a translation that has
/// begun — until then (and whenever the rendering doesn't parse as expected,
/// e.g. an original ending inside an open code fence) the text shows whole.
fn appended_original_blocks(text: &str, agent: &str, tree: &BlockTree) -> Option<usize> {
    crate::quote_origin::appended_translation(text, agent)?;
    let rule = tree
        .blocks
        .iter()
        .position(|top| top.range.start >= agent.len())?;
    (rule > 0 && rule + 1 < tree.blocks.len() && matches!(tree.blocks[rule].block, Block::Rule))
        .then_some(rule + 1)
}

/// The rows a toggle covers and whether it starts open: a translation's
/// original and a thought start closed, a work run only while it streams.
fn toggle_span(kind: &RowKind) -> Option<(usize, bool)> {
    match kind {
        RowKind::TranslationOriginal { blocks } | RowKind::Thought { blocks, .. } => {
            Some((*blocks, false))
        }
        RowKind::Activity {
            rows, auto_open, ..
        } => Some((*rows, *auto_open)),
        _ => None,
    }
}

/// Whether the toggle `row` is open: the user's click (`pins`, by row id)
/// wins, else the toggle's default.
pub(super) fn toggle_open(row: &Row, pins: &HashMap<SharedString, bool>) -> bool {
    toggle_span(&row.kind).is_some_and(|(_, default)| pins.get(&row.id).copied().unwrap_or(default))
}

/// Drop the rows each CLOSED toggle covers — a translation's original, a
/// thought, a work run. `pins` holds the toggles the user clicked, by row id;
/// every other toggle keeps its default ([`toggle_span`]). A folded row's
/// timestamp (a reply that ended while thinking) moves onto its toggle, so
/// the entry keeps its strip.
pub fn fold_closed_toggles(rows: &mut Vec<Row>, pins: &HashMap<SharedString, bool>) {
    let mut hide = 0usize;
    let mut folded: Vec<Row> = Vec::with_capacity(rows.len());
    for row in rows.drain(..) {
        if hide > 0 {
            hide -= 1;
            if let (Some(stamp), Some(toggle)) = (row.timestamp, folded.last_mut()) {
                toggle.timestamp = Some(stamp);
                toggle.version ^= 1 << 62;
                if let Some(label) = row.answered {
                    toggle.version ^= fnv1a(label.text.as_bytes()).rotate_left(1)
                        ^ u64::from(label.substituted.is_some());
                    toggle.answered = Some(label);
                }
            }
            continue;
        }
        if let Some((covered, _)) = toggle_span(&row.kind)
            && !toggle_open(&row, pins)
        {
            hide = covered;
        }
        folded.push(row);
    }
    *rows = folded;
}

/// Cap each open work run the way a tool group caps its chips: keep the
/// run's LAST `limit` tool calls ([`hidden_tool_count`]) and fold everything
/// before them — earlier calls and thoughts alike — behind one
/// [`RowKind::RunOverflow`] row. The cut lands right after the last hidden
/// call, so the thinking that led into the first kept one stays; a cut inside
/// a tool group hides that group's leading chips (`skip`). `revealed` holds
/// the runs the user unfolded, by [`RowKind::Activity`] row id: they show
/// whole, under a row that folds them back.
///
/// Runs after [`fold_closed_toggles`]: a closed run has no rows left to cap.
pub fn cap_work_runs(
    rows: &mut Vec<Row>,
    limit: u32,
    revealed: &std::collections::HashSet<SharedString>,
) {
    let mut ix = 0;
    while ix < rows.len() {
        if !matches!(rows[ix].kind, RowKind::Activity { .. }) {
            ix += 1;
            continue;
        }
        let run = rows[ix].id.clone();
        let start = ix + 1;
        let end = start
            + rows[start..]
                .iter()
                .take_while(|row| is_nested(&row.kind))
                .count();
        let total: usize = rows[start..end]
            .iter()
            .map(|row| match &row.kind {
                RowKind::ToolGroup { tools, .. } => tools.len(),
                _ => 0,
            })
            .sum();
        let is_revealed = revealed.contains(&run);
        let hidden = hidden_tool_count(total, limit, is_revealed);
        if hidden == 0 && !(is_revealed && hidden_tool_count(total, limit, false) > 0) {
            ix = end;
            continue;
        }
        // `rows[start..cut]` fold away whole; `skip` chips of `rows[cut]` too.
        let (mut cut, mut left, mut skip, mut thoughts) = (start, hidden, 0, 0);
        while left > 0 {
            match &rows[cut].kind {
                RowKind::ToolGroup { tools, .. } if tools.len() > left => {
                    skip = left;
                    break;
                }
                RowKind::ToolGroup { tools, .. } => left -= tools.len(),
                RowKind::Thought { .. } => thoughts += 1,
                _ => {}
            }
            cut += 1;
        }
        let group = &mut rows[cut];
        if let RowKind::ToolGroup {
            skip: group_skip, ..
        } = &mut group.kind
            && skip > 0
        {
            *group_skip = skip;
            group.version ^= (skip as u64) << 48;
        }
        let overflow = Row {
            id: format!("{run}.overflow").into(),
            version: (hidden as u64) << 32 | thoughts as u64,
            turn_start: false,
            kind: RowKind::RunOverflow {
                run,
                tools: hidden,
                thoughts,
            },
            entry_id: rows[ix].entry_id.clone(),
            role: rows[ix].role,
            timestamp: None,
            answered: None,
        };
        rows.splice(start..cut, [overflow]);
        ix = end - (cut - start) + 1;
    }
}

/// The label of a work run's [`RowKind::RunOverflow`] row: what it folds
/// away, or — for a revealed run (`tools == 0`) — that it folds it back.
pub(super) fn run_overflow_label(tools: usize, thoughts: usize) -> String {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    match (tools, thoughts) {
        (0, _) => "Show fewer tool calls".to_string(),
        (tools, 0) => format!("Show {tools} earlier tool call{}", plural(tools)),
        (tools, thoughts) => format!(
            "Show {tools} earlier tool call{} and {thoughts} thought{}",
            plural(tools),
            plural(thoughts)
        ),
    }
}

/// How [`parse_for_row`] produced its tree — carries the incremental parser's
/// work counters so callers (and tests) can see that per-append parse work is
/// bounded by the reparsed tail, never the whole accumulated reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseOutcome {
    /// Streaming row: the live [`IncrementalParser`] advanced by one commit.
    Incremental {
        /// Bytes fed through `parse_full` for this commit (the reparse tail).
        parsed_bytes: usize,
        /// Leading top-level blocks left untouched (render caches stay valid).
        stable_prefix_blocks: usize,
    },
    /// Completed row served from the settled tree cache (no parse at all).
    Cached,
    /// Live→complete handoff: the live parser's exact tree was adopted.
    Handoff,
    /// Completed row parsed from scratch.
    Full,
}

/// The transcript's markdown parse wiring, extracted for testability: one call
/// per text part per sync. Streaming parts keep one [`IncrementalParser`] per
/// row key and advance it with the full accumulated text (`set_text` takes the
/// O(tail) append path for the prefix-extensions the doc watch delivers);
/// completed parts hit the settled cache, adopt the live parser's tree on the
/// live→complete flip (flicker-free handoff), or do one full parse.
pub fn parse_for_row(
    streaming: bool,
    key: &str,
    text: &str,
    live_parsers: &mut HashMap<String, IncrementalParser>,
    tree_cache: &mut HashMap<String, (usize, Arc<BlockTree>)>,
) -> (Arc<BlockTree>, ParseOutcome) {
    if streaming {
        let parser = live_parsers.entry(key.to_string()).or_default();
        parser.set_text(text);
        (
            // Display tree: hanging inline markers mended so closers arriving
            // later never reflow painted text (markdown/mend.rs). Completed
            // rows below use the canonical tree — the honest settle.
            Arc::new(parser.display_tree()),
            ParseOutcome::Incremental {
                parsed_bytes: parser.last_parse_bytes(),
                stable_prefix_blocks: parser.stable_prefix_blocks(),
            },
        )
    } else {
        if let Some((len, tree)) = tree_cache.get(key)
            && *len == text.len()
        {
            return (tree.clone(), ParseOutcome::Cached);
        }
        // On the live→complete flip reuse the live parser's tree when
        // the sources match — the split rows then share the exact tree
        // the unsplit row painted, guaranteeing a flicker-free handoff.
        let (tree, outcome) = match live_parsers.remove(key) {
            Some(parser) if parser.source() == text => {
                (Arc::new(parser.tree().clone()), ParseOutcome::Handoff)
            }
            _ => (Arc::new(parse_full(text)), ParseOutcome::Full),
        };
        tree_cache.insert(key.to_string(), (text.len(), tree.clone()));
        (tree, outcome)
    }
}

/// Markdown row ids are `{entry}#{part}.{blockIx}` — the part prefix is
/// everything before the block index.
fn part_prefix(id: &str) -> &str {
    id.rsplit_once('.').map(|(p, _)| p).unwrap_or(id)
}

/// Vertical gap opening `row` given its predecessor: turn gap at turn starts;
/// the markdown block gap between sibling block rows split from the same text
/// part — matching the live row's internal spacing exactly, so the
/// live→split handoff cannot shift a pixel; the block gap otherwise.
#[cfg(test)]
pub fn top_gap_for(prev: Option<&Row>, row: &Row) -> f32 {
    top_gap_for_style(prev, row, GAP_TURN, render::MD_BLOCK_GAP)
}

pub(super) fn top_gap_for_style(
    prev: Option<&Row>,
    row: &Row,
    message_gap: f32,
    paragraph_gap: f32,
) -> f32 {
    if row.turn_start {
        return message_gap;
    }
    if is_nested(&row.kind) {
        return nested_gap(prev, row, paragraph_gap);
    }
    let is_md = |k: &RowKind| {
        matches!(
            k,
            RowKind::Markdown { .. } | RowKind::LiveMarkdown { .. } | RowKind::ThoughtBlock { .. }
        )
    };
    let same_part_markdown = prev.is_some_and(|p| {
        is_md(&p.kind) && is_md(&row.kind) && part_prefix(&p.id) == part_prefix(&row.id)
    });
    if same_part_markdown {
        paragraph_gap
    } else {
        GAP_BLOCK
    }
}

/// The gap above a row of a work run (drawn on the run's rail): chips stack
/// like a tool group's — their cards carry their own margins — a thought's
/// text sits just under its chip, and the chip after it gets some air.
fn nested_gap(prev: Option<&Row>, row: &Row, paragraph_gap: f32) -> f32 {
    let Some(prev) = prev else {
        return 0.0;
    };
    match (&row.kind, &prev.kind) {
        (_, RowKind::Activity { .. }) => CHIPS_TOP_PAD,
        (RowKind::ThoughtBlock { .. }, RowKind::ThoughtBlock { .. })
            if part_prefix(&prev.id) == part_prefix(&row.id) =>
        {
            paragraph_gap
        }
        (RowKind::ThoughtBlock { .. }, _) => 2.0,
        (_, RowKind::ThoughtBlock { .. }) => 6.0,
        _ => 0.0,
    }
}

/// Minimal splice for a row-set change: `Some((old_range, new_count))`, or
/// `None` when the sets are identical by (id, version).
pub fn diff_rows(old: &[Row], new: &[Row]) -> Option<(Range<usize>, usize)> {
    let eq = |a: &Row, b: &Row| a.id == b.id && a.version == b.version;
    let mut prefix = 0usize;
    let max_prefix = old.len().min(new.len());
    while prefix < max_prefix && eq(&old[prefix], &new[prefix]) {
        prefix += 1;
    }
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let mut suffix = 0usize;
    let max_suffix = (old.len() - prefix).min(new.len() - prefix);
    while suffix < max_suffix && eq(&old[old.len() - 1 - suffix], &new[new.len() - 1 - suffix]) {
        suffix += 1;
    }
    Some((prefix..old.len() - suffix, new.len() - suffix - prefix))
}
