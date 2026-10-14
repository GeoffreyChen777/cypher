//! Row model (pure): transcript entries → block-granularity rows.

use super::*;

use std::collections::{HashMap, HashSet};

use cypher_proto::{PermissionOutcome, ToolPermission};

mod fold;
mod gates;
mod tools;

pub use fold::*;
pub use gates::*;
pub use tools::*;

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
        /// A permission prompt with no call to file it under (one asked
        /// ahead, or from a Runtime that does not say which call): shown as
        /// a permission, not a question.
        permission: bool,
    },
    ErrorChip {
        message: SharedString,
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
    /// The files a settled round changed, closing the round's last answer:
    /// a card listing them, each opening the round's diff at that file.
    TurnChanges {
        changes: Arc<cypher_proto::TurnChanges>,
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
    /// Which version of a translated text part this row shows. A message
    /// shows one side at a time — the translation unless the user
    /// Shift-clicked it to read the original — and the other side's rows are
    /// dropped ([`show_translation_side`]). `None` for everything else.
    pub translation: Option<TranslationSide>,
}

/// One version of a translated text part ([`Row::translation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslationSide {
    /// The text as the model wrote it.
    Original,
    /// The text as the user reads it.
    Translation,
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
                crate::markdown::find::count_matches(text, query)
            }
        }
        RowKind::Markdown { tree, block_ix } | RowKind::LiveMarkdown { tree, block_ix } => {
            tree.blocks.get(*block_ix).map_or(0, |top| {
                markdown::render::count_block_matches(&top.block, query)
            })
        }
        // Thinking is not searched: a hit inside a collapsed thought could
        // not be shown.
        RowKind::ToolGroup { .. }
        | RowKind::Activity { .. }
        | RowKind::RunOverflow { .. }
        | RowKind::Worked { .. }
        | RowKind::TurnChanges { .. }
        | RowKind::InputChip { .. }
        | RowKind::ErrorChip { .. }
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
    /// A round's changed-files card was clicked: open the round's diff,
    /// scrolled to `path` (the whole round when `None`).
    OpenTurnDiff {
        changes: Arc<cypher_proto::TurnChanges>,
        path: Option<String>,
    },
}

impl EventEmitter<TranscriptEvent> for Transcript {}

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
    crate::kit::icons::icon(if copied {
        crate::kit::icons::CHECK
    } else {
        crate::kit::icons::COPY
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
        acc.push(t.is_error as u8 | (t.resolved as u8) << 1 | (t.blocked_first as u8) << 2);
        // A call nesting under its caller once the caller's part arrives
        // re-indents the chip.
        acc.push(t.depth);
        // The permission badge arriving or changing redraws the row.
        if let Some(permission) = &t.permission {
            acc.extend_from_slice(&serde_json::to_vec(permission).unwrap_or_default());
        }
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
    selection: Option<markdown::render::SelectionUi>,
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
    selection: Option<markdown::render::SelectionUi>,
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
            markdown::render::paint_text_selection(
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
    if entry.role == MessageRole::User {
        return vec![user_row(entry, pending)];
    }

    // Assistant/system: split parts into block rows, folding consecutive tools.
    let cx = EntryRows {
        entry,
        entry_id: entry.id.clone().into(),
        streaming: entry.status == Some(MessageStatus::Streaming),
        last_part_ix: entry.parts.len().saturating_sub(1),
    };
    let fold = PermissionFold::of(entry);
    let mut rows: Vec<Row> = Vec::new();
    let mut group_ix = 0usize;
    // Each tool with its part id, which carries the nesting
    // ([`nest_tool_calls`]).
    let mut pending_group: Vec<(String, ToolItem)> = Vec::new();
    let mut group_last_part_ix = 0usize;
    // The open work run: where its rows start and the part that opened it
    // (the Activity row's id). Tool calls and thoughts extend it; anything
    // the reader sees between them (answer text, a chip) closes it.
    let mut run: Option<(usize, String)> = None;

    for (part_ix, part) in entry.parts.iter().enumerate() {
        // A blocked attempt folded into its retry, and an answered prompt
        // filed under its call, render nothing and split nothing.
        match part {
            MessagePart::Tool { id, .. } if fold.folded.contains(id) => continue,
            MessagePart::Input { request_id, .. } if fold.filed.contains(request_id) => continue,
            _ => {}
        }
        if let Some((part_id, item)) = tool_item(part, &fold) {
            // Nothing is pushed until the group flushes, so the run's
            // rows start here.
            run.get_or_insert_with(|| (rows.len(), part_id.clone()));
            pending_group.push((part_id, item));
            group_last_part_ix = part_ix;
            continue;
        }
        // A part that renders nothing (an empty text or thought, a
        // question still in the composer) splits nothing either.
        if part_is_silent(part) {
            continue;
        }
        cx.flush_tool_group(
            &mut rows,
            &mut pending_group,
            &mut group_ix,
            group_last_part_ix,
        );
        if !matches!(part, MessagePart::Reasoning { .. })
            && let Some((start, first_part)) = run.take()
        {
            fold_work_run(&mut rows, start, &first_part, entry, false);
        }
        match part {
            MessagePart::Text {
                id: part_id,
                text,
                agent_text,
            } => cx.push_text_rows(&mut rows, part_id, text, agent_text.as_deref(), parse),
            MessagePart::Reasoning { id: part_id, text } => {
                run.get_or_insert_with(|| (rows.len(), part_id.clone()));
                // Still thinking: the reasoning is the live tail.
                let live = cx.streaming && part_ix == cx.last_part_ix;
                cx.push_thought_rows(&mut rows, part_id, text, live, parse);
            }
            MessagePart::Input {
                id: part_id,
                request_id,
                questions,
                resolved,
                ..
            } => rows.push(cx.input_chip_row(part_id, request_id, questions, *resolved)),
            MessagePart::Error {
                id: part_id,
                message,
            } => rows.push(cx.row(
                format!("{}#{}", entry.id, part_id),
                message.len() as u64,
                RowKind::ErrorChip {
                    // Harness-generated; the chip is one line.
                    message: single_line(message).into(),
                },
            )),
            // Tools are grouped above; nothing reaches here.
            MessagePart::Tool { .. } => {}
        }
    }
    cx.flush_tool_group(
        &mut rows,
        &mut pending_group,
        &mut group_ix,
        group_last_part_ix,
    );
    if let Some((start, first_part)) = run.take() {
        // Still the tail of a streaming reply: open, like a live tool group.
        fold_work_run(&mut rows, start, &first_part, entry, cx.streaming);
    }

    // The work rule goes in BEFORE the turn-start/timestamp bookkeeping: it is
    // never the entry's first or last row (it separates work from the answer
    // that followed it), so neither marker can land on it.
    if !cx.streaming
        && let Some(at) = worked_rule_at(&rows)
        && let Some(label) = worked_label(entry.created_at, entry.completed_at)
    {
        rows.insert(
            at,
            cx.row(
                format!("{}#worked", entry.id),
                fnv1a(label.as_bytes()),
                RowKind::Worked { label },
            ),
        );
    }

    // The round's changed files close its last answer; the hover strip
    // rides under them.
    if !cx.streaming
        && let Some(changes) = &entry.turn_changes
    {
        rows.push(cx.row(
            format!("{}#changes", entry.id),
            turn_changes_version(changes),
            RowKind::TurnChanges {
                changes: Arc::new(changes.clone()),
            },
        ));
    }

    if let Some(first) = rows.first_mut() {
        first.turn_start = true;
    }
    // Timestamp strip under the entry's LAST row once the turn has settled
    // (chat-view.tsx: "No timestamp hover mid-stream"). The version bit keeps
    // the diff key honest for last-row kinds whose own version wouldn't
    // change when streaming flips off (chips).
    if !cx.streaming
        && let Some(last) = rows.last_mut()
    {
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

/// A round's card version: its tree pair names its content.
pub(super) fn turn_changes_version(changes: &cypher_proto::TurnChanges) -> u64 {
    fnv1a(format!("{}:{}:{}", changes.base, changes.head, changes.file_count).as_bytes())
}

/// The single row of a user entry.
fn user_row(entry: &SessionMessageEntry, pending: bool) -> Row {
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
    Row {
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
        entry_id: entry.id.clone().into(),
        role: entry.role,
        // User rows always carry the strip (chat-view.tsx: whenever
        // `createdAt` exists — the optimistic echo included).
        timestamp: Some(entry.created_at),
        answered: None,
        translation: None,
    }
}

/// The "Deny and stop" option a permission prompt from a Runtime that does
/// not mark its prompts is recognised by (`composer::permission_card`).
const PERMISSION_STOP_OPTION: &str = "Deny and stop";

/// Whether a question is a permission prompt rather than one the agent asks.
fn is_permission_prompt(questions: &[cypher_proto::UserInputQuestion]) -> bool {
    questions.first().is_some_and(|question| {
        question.permission.is_some()
            || question
                .options
                .iter()
                .any(|option| option == PERMISSION_STOP_OPTION)
    })
}

/// How an entry's permission prompts and sandbox retries fold into its tool
/// rows. An answered prompt files under the call it gated instead of a row
/// of its own, and a command the sandbox blocked folds into the run of the
/// same command that followed (outside the sandbox, or refused).
#[derive(Default)]
struct PermissionFold {
    /// Answered prompts filed under a call of this entry (request ids).
    filed: HashSet<String>,
    /// Calls an answered prompt gated (part ids), for a call the extension
    /// keeps no record of.
    prompted: HashSet<String>,
    /// Blocked attempts folded into a later run (part ids): no row.
    folded: HashSet<String>,
    /// The later run (part id) → the folded attempt's output.
    attempts: HashMap<String, Option<String>>,
}

impl PermissionFold {
    fn of(entry: &SessionMessageEntry) -> Self {
        let mut fold = Self::default();
        let tool_ids: HashSet<&str> = entry
            .parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::Tool { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        for part in &entry.parts {
            if let MessagePart::Input {
                request_id,
                questions,
                resolved: true,
                ..
            } = part
                && let Some(call_id) = questions
                    .first()
                    .and_then(|question| question.permission.as_ref())
                    .and_then(|permission| permission.tool_call_id.as_deref())
                && tool_ids.contains(call_id)
            {
                fold.filed.insert(request_id.clone());
                fold.prompted.insert(call_id.to_owned());
            }
        }
        let command = |part: &MessagePart| match part {
            MessagePart::Tool {
                call: ToolCall::Exec { command },
                permission: Some(permission),
                resolved: true,
                ..
            } => Some((command.clone(), permission.outcome)),
            _ => None,
        };
        for (ix, part) in entry.parts.iter().enumerate() {
            let Some((blocked, PermissionOutcome::Blocked)) = command(part) else {
                continue;
            };
            let retry = entry.parts[ix + 1..].iter().find(|later| {
                command(later).is_some_and(|(again, outcome)| {
                    again == blocked
                        && matches!(
                            outcome,
                            PermissionOutcome::Outside | PermissionOutcome::Denied
                        )
                })
            });
            if let (
                Some(MessagePart::Tool { id: retry_id, .. }),
                MessagePart::Tool { id, output, .. },
            ) = (retry, part)
            {
                fold.folded.insert(id.clone());
                fold.attempts.insert(retry_id.clone(), output.clone());
            }
        }
        fold
    }
}

/// A tool part as its group item, keyed by part id; `None` for other parts.
fn tool_item(part: &MessagePart, fold: &PermissionFold) -> Option<(String, ToolItem)> {
    let MessagePart::Tool {
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
        permission,
        ..
    } = part
    else {
        return None;
    };
    // A call the extension keeps no record of (an MCP tool) that a prompt
    // gated: it ran if it did not fail.
    let permission = permission.clone().or_else(|| {
        (fold.prompted.contains(part_id) && *resolved).then_some(ToolPermission {
            outcome: if *is_error {
                PermissionOutcome::Denied
            } else {
                PermissionOutcome::Allowed
            },
            scope: None,
            rule: None,
            purpose: None,
        })
    });
    let attempt = fold.attempts.get(part_id);
    let output = match &permission {
        Some(permission) if attempt.is_some() || permission.purpose.is_some() => {
            Some(permission_output(permission, attempt, output.as_deref()))
        }
        _ => output.clone(),
    };
    Some((
        part_id.clone(),
        ToolItem {
            call: call.clone(),
            is_error: *is_error,
            resolved: *resolved,
            detail: tool_detail(output.as_deref(), diff.as_ref(), diff_stats.as_deref())
                .map(Arc::new),
            invocation: call_block(call).map(Arc::new),
            output_ref: output_ref.clone().map(SharedString::from),
            output_bytes: *output_bytes,
            diff_ref: diff_ref.clone().map(SharedString::from),
            depth: 0,
            permission,
            blocked_first: attempt.is_some(),
        },
    ))
}

/// A permission-handled call's detail: what it was for, the blocked attempt
/// folded into it, then its own output.
fn permission_output(
    permission: &ToolPermission,
    attempt: Option<&Option<String>>,
    output: Option<&str>,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(purpose) = &permission.purpose {
        lines.push(format!("Purpose: {purpose}"));
    }
    if let Some(rule) = &permission.rule {
        lines.push(format!("Allowed by: {rule}"));
    }
    if let Some(attempt) = attempt {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("In the sandbox, blocked:".into());
        lines.push(
            attempt
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or("(no output)")
                .to_owned(),
        );
        lines.push(String::new());
        lines.push(match permission.outcome {
            PermissionOutcome::Denied => "Not run outside the sandbox:".into(),
            _ => "Outside the sandbox:".into(),
        });
    } else if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(output.unwrap_or("(no output)").to_owned());
    lines.join("\n")
}

/// Whether a part renders no row: an empty text or thought, or a question
/// still pending in the composer.
fn part_is_silent(part: &MessagePart) -> bool {
    match part {
        MessagePart::Text { text, .. } | MessagePart::Reasoning { text, .. } => {
            text.trim().is_empty()
        }
        MessagePart::Input { resolved, .. } => !*resolved,
        MessagePart::Error { .. } | MessagePart::Tool { .. } => false,
    }
}

/// The per-entry facts every assistant/system row is built from.
struct EntryRows<'a> {
    entry: &'a SessionMessageEntry,
    entry_id: SharedString,
    streaming: bool,
    last_part_ix: usize,
}

impl EntryRows<'_> {
    /// A plain row of this entry (no turn-start, timestamp or answer label).
    fn row(&self, id: impl Into<SharedString>, version: u64, kind: RowKind) -> Row {
        Row {
            id: id.into(),
            version,
            turn_start: false,
            kind,
            entry_id: self.entry_id.clone(),
            role: self.entry.role,
            timestamp: None,
            answered: None,
            translation: None,
        }
    }

    /// Close the pending run of consecutive tools into one group row.
    fn flush_tool_group(
        &self,
        rows: &mut Vec<Row>,
        group: &mut Vec<(String, ToolItem)>,
        group_ix: &mut usize,
        last_ix: usize,
    ) {
        if group.is_empty() {
            return;
        }
        let tools = nest_tool_calls(std::mem::take(group));
        let auto_open = self.streaming && last_ix == self.last_part_ix;
        rows.push(self.row(
            format!("{}#g{}", self.entry.id, group_ix),
            tool_fingerprint(&tools, auto_open),
            RowKind::ToolGroup {
                tools: Arc::new(tools),
                auto_open,
                nested: false,
                skip: 0,
            },
        ));
        *group_ix += 1;
    }

    /// One row per top-level block of `tree`. The version is a content hash
    /// of the block's bytes (LSB = `live`), so a commit only splices rows
    /// whose bytes actually changed — the settled prefix of a live part is
    /// untouched (and its render caches stay valid).
    fn push_block_rows(
        &self,
        rows: &mut Vec<Row>,
        key: &str,
        tree: &Arc<BlockTree>,
        text: &str,
        live: bool,
        kind: impl Fn(usize) -> RowKind,
    ) {
        for block_ix in 0..tree.blocks.len() {
            let range = &tree.blocks[block_ix].range;
            let end = range.end.min(text.len());
            let bytes = text
                .as_bytes()
                .get(range.start.min(end)..end)
                .unwrap_or_default();
            rows.push(self.row(
                format!("{key}.{block_ix}"),
                (fnv1a(bytes) << 1) | live as u64,
                kind(block_ix),
            ));
        }
    }

    fn push_text_rows(
        &self,
        rows: &mut Vec<Row>,
        part_id: &str,
        text: &str,
        agent_text: Option<&str>,
        parse: &mut dyn FnMut(&str, &str) -> Arc<BlockTree>,
    ) {
        let key = format!("{}#{}", self.entry.id, part_id);
        let tree = parse(&key, text);
        let streaming = self.streaming;
        let markdown = |tree: &Arc<BlockTree>, block_ix| {
            // Live and completed parts split identically — one row per
            // top-level block, same ids, so the live→complete handoff
            // never changes row identity.
            if streaming {
                RowKind::LiveMarkdown {
                    tree: tree.clone(),
                    block_ix,
                }
            } else {
                RowKind::Markdown {
                    tree: tree.clone(),
                    block_ix,
                }
            }
        };
        let start = rows.len();
        self.push_block_rows(rows, &key, &tree, text, streaming, |ix| markdown(&tree, ix));
        let Some(agent) = agent_text else {
            return;
        };
        if let Some(blocks) = appended_original_blocks(text, agent, &tree) {
            // Append mode already holds both versions: the original's blocks,
            // the separator rule and the translation's. They keep their block
            // ids either way (quotes map back through them); the rule only
            // ever separated the two, and a message shows one at a time.
            let rule = start + blocks - 1;
            rows.remove(rule);
            for (ix, row) in rows[start..].iter_mut().enumerate() {
                row.translation = Some(if start + ix < rule {
                    TranslationSide::Original
                } else {
                    TranslationSide::Translation
                });
            }
        } else if crate::quote_origin::appended_translation(text, agent).is_none() {
            // Replace mode: the original's own rows go first, under their own
            // key — a quote from them is the agent's text and stands as
            // selected, which is what a key outside the part's gives it.
            for row in &mut rows[start..] {
                row.translation = Some(TranslationSide::Translation);
            }
            let original_key = format!("{key}~original");
            let original = parse(&original_key, agent);
            let mut original_rows = Vec::new();
            self.push_block_rows(
                &mut original_rows,
                &original_key,
                &original,
                agent,
                false,
                |ix| RowKind::Markdown {
                    tree: original.clone(),
                    block_ix: ix,
                },
            );
            for row in &mut original_rows {
                row.translation = Some(TranslationSide::Original);
            }
            rows.splice(start..start, original_rows);
        }
        // An append rendering whose translation has not begun shows whole.
    }

    fn push_thought_rows(
        &self,
        rows: &mut Vec<Row>,
        part_id: &str,
        text: &str,
        live: bool,
        parse: &mut dyn FnMut(&str, &str) -> Arc<BlockTree>,
    ) {
        let key = format!("{}#{}", self.entry.id, part_id);
        let tree = parse(&key, text);
        let preview = thought_preview(text);
        rows.push(self.row(
            format!("{key}.thought"),
            ((tree.blocks.len() as u64) << 1 | live as u64) ^ fnv1a(preview.as_bytes()) << 8,
            RowKind::Thought {
                blocks: tree.blocks.len(),
                live,
                nested: false,
                preview: preview.into(),
            },
        ));
        // Same ids and content-hash versions as answer blocks,
        // so a streaming thought only splices its tail.
        self.push_block_rows(rows, &key, &tree, text, live, |block_ix| {
            RowKind::ThoughtBlock {
                tree: tree.clone(),
                block_ix,
                live,
                nested: false,
            }
        });
    }

    fn input_chip_row(
        &self,
        part_id: &str,
        request_id: &str,
        questions: &[cypher_proto::UserInputQuestion],
        resolved: bool,
    ) -> Row {
        // Only resolved questions get here (`part_is_silent`): the composer
        // wizard is the interaction, and a pending "Awaiting your answer…"
        // chip made slash-command settings read as the model asking a
        // question. Model-generated header onto the one-line chip: the
        // question alone, without its `Context:` part (a permission
        // request's paths and notes).
        let header: SharedString = single_line(
            &questions
                .first()
                .map(|q| crate::composer::split_question_context(&q.header).0)
                .unwrap_or_else(|| "Question".to_string()),
        )
        .into();
        let permission = is_permission_prompt(questions);
        self.row(
            format!("{}#{}", self.entry.id, part_id),
            (fnv1a(header.as_bytes()) << 2 | (permission as u64) << 1) | resolved as u64,
            RowKind::InputChip {
                header,
                request_id: request_id.to_string().into(),
                resolved,
                permission,
            },
        )
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
    top_gap_for_style(prev, row, GAP_TURN, markdown::render::MD_BLOCK_GAP)
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
