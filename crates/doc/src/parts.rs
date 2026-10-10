//! Message parts: the event fold and the render-only privacy policy.

use serde::{Deserialize, Serialize};

use cypher_proto::{AgentEvent, ToolCall, ToolDiff, UserInputQuestion};

/// Line cap for the tool-output SUMMARY persisted into the doc. Keeping a
/// small number of complete lines makes the expandable detail useful without
/// imposing an arbitrary character limit on a single long line.
pub(crate) const TOOL_OUTPUT_SUMMARY_MAX_LINES: usize = 5;

/// Char cap for the `subagent` tool's `task` kept in the doc (privacy-safe
/// persistence, [`sanitize_tool_call`]). Cut on a Unicode-char boundary, so
/// the stored string is always valid UTF-8.
pub(crate) const SUBAGENT_TASK_MAX_CHARS: usize = 500;

/// Char cap for a Pi `codemode` script kept in the doc. The script is the
/// call's whole invocation — what a `bash` command is to `Exec`, which the doc
/// keeps uncut — so it stays, bounded: a model writes a few dozen lines, and
/// a pathological one cannot grow a tool part without limit.
pub const CODEMODE_SCRIPT_MAX_CHARS: usize = 8_000;

/// Char cap for a Pi `tool_search` query kept in the doc.
pub(crate) const TOOL_SEARCH_QUERY_MAX_CHARS: usize = 500;

/// The doc-resident form of a tool output (docs/chat2-sync.md A1). There is no
/// sidecar, so this IS the whole record in the doc — the full text survives
/// only in the host's local run journal:
///
/// - Markdown code fences are stripped first: a fence around a whole output
///   is transport wrapping, never content.
/// - Outputs keep complete lines, up to [`TOOL_OUTPUT_SUMMARY_MAX_LINES`].
/// - A long single line is kept whole; the limit is by lines, not characters.
///
/// `None` for blank output.
pub(crate) fn summarize_tool_output(text: &str) -> Option<String> {
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect();
    let stripped = kept.join("\n");
    let stripped = stripped.trim();
    if stripped.is_empty() {
        return None;
    }
    Some(
        stripped
            .lines()
            .take(TOOL_OUTPUT_SUMMARY_MAX_LINES)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Tail-cap for the transient `progress` column: keep the LAST at most
/// [`TOOL_PROGRESS_MAX_LINES`] lines within [`TOOL_PROGRESS_MAX_BYTES`] — a
/// live tail, not a summary. The harness already caps each progress tick at
/// its output cap; this is the doc's own defense so a chatty tool (a subagent
/// streaming full transcripts) can never grow the transient column unbounded.
/// Cutting the head (not the middle) is deliberate: the live card shows what
/// is happening RIGHT NOW; the settled output summary owns the beginning.
pub(crate) const TOOL_PROGRESS_MAX_LINES: usize = 8;
pub(crate) const TOOL_PROGRESS_MAX_BYTES: usize = 4096;

/// Keep the tail of a live progress blob: last ≤8 lines, whole lines, within
/// 4KB (truncating by bytes would split a line mid-character; lines are the
/// UI's render unit).
pub(crate) fn tail_progress(text: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut bytes = 0usize;
    // Walk from the LAST line backwards, keeping as many of the last 8 as fit
    // in 4KB; the first (most recent) line always rides so a stream never
    // degrades to an empty tail.
    for line in text.lines().rev().take(TOOL_PROGRESS_MAX_LINES) {
        let add = line.len() + usize::from(!kept.is_empty()); // + the join newline
        if !kept.is_empty() && bytes + add > TOOL_PROGRESS_MAX_BYTES {
            break;
        }
        kept.push(line);
        bytes += add;
    }
    kept.reverse();
    let mut out = kept.join("\n");
    // Preserve a trailing newline so a partial last line still reads as
    // "still streaming" — and never grow past the byte budget on re-join.
    if text.ends_with('\n') && !kept.is_empty() && out.len() < TOOL_PROGRESS_MAX_BYTES {
        out.push('\n');
    }
    out
}

/// Per-file diff stats persisted in place of inline diff text (t3's shape).
/// The inline diff was the bigger bomb than outputs — 32KB/edit, unexercised
/// only because the claude harness emits none. Full diff text lives in the
/// sidecar behind `diff_ref`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiffStat {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

/// Line-level add/delete counts for one file's diff.
pub(crate) fn diff_stat(diff: &ToolDiff) -> ToolDiffStat {
    let (additions, deletions) = match &diff.old_text {
        None => (diff.new_text.lines().count() as u64, 0),
        Some(old) => {
            let text_diff = similar::TextDiff::from_lines(old.as_str(), diff.new_text.as_str());
            let mut additions = 0u64;
            let mut deletions = 0u64;
            for change in text_diff.iter_all_changes() {
                match change.tag() {
                    similar::ChangeTag::Insert => additions += 1,
                    similar::ChangeTag::Delete => deletions += 1,
                    similar::ChangeTag::Equal => {}
                }
            }
            (additions, deletions)
        }
    };
    ToolDiffStat {
        path: diff.path.clone(),
        additions,
        deletions,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageStatus {
    Streaming,
    Complete,
    Aborted,
}

/// One rendered part of an assistant message.
// `Tool` is much wider than the other variants, but this type is constructed
// and matched in ~200 places and is the doc's persisted shape. Boxing that one
// variant would ripple through every fold, render and test for a stack-size
// win that never showed up in a profile.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MessagePart {
    #[serde(rename_all = "camelCase")]
    Text {
        id: String,
        text: String,
        /// The text as the AGENT has it, when that differs from what the
        /// transcript displays — set only while a translation stands between
        /// the two: an answer's original before its display translation
        /// replaced it, or the translated prompt the agent received for a
        /// user message shown as typed. Quotes taken from the displayed text
        /// map back through it, so the agent is shown its own words. Additive
        /// (absent on old rows, old writers, and untranslated text).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_text: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Tool {
        id: String,
        call: ToolCall,
        #[serde(default)]
        is_error: bool,
        /// True once a ToolResult arrived.
        #[serde(default)]
        resolved: bool,
        /// Bounded tool output summary ([`summarize_tool_output`]): up to five
        /// complete lines. Old entries (pre-strip) still carry up to 4KB here;
        /// old app versions render this field either way.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// Live progress for an UNRESOLVED tool: the tail of the streamed
        /// partial output while the tool is still running (pi
        /// `tool_execution_update` → [`AgentEvent::ToolProgress`]). Transient
        /// run state, not content — the fold overwrites it on every progress
        /// tick and CLEARS it on resolve (`ToolResult`), so a resolved chip
        /// collapses back to its plain form. Tailed by [`tail_progress`]
        /// before persisting (last ≤8 lines within 4KB).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<String>,
        /// Inline file diff — written by pre-strip app versions only; new
        /// folds persist [`Self::Tool::diff_stats`] + `diff_ref` instead.
        /// Kept so old docs render their diffs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<ToolDiff>,
        /// Sidecar key (`{chatId}/{partId}`) of the full output — additive;
        /// the fold never writes it (docs from earlier builds may carry it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_ref: Option<String>,
        /// Full-output byte length, so the UI can say "Show full output (12 KB)".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_bytes: Option<u64>,
        /// Sidecar key (`{chatId}/{partId}.diff`) of the full diff JSON.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_ref: Option<String>,
        /// Per-file diff stats (additive replacement for inline `diff`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_stats: Option<Vec<ToolDiffStat>>,
    },
    #[serde(rename_all = "camelCase")]
    Input {
        id: String,
        request_id: String,
        questions: Vec<UserInputQuestion>,
        #[serde(default)]
        resolved: bool,
    },
    Error {
        id: String,
        message: String,
    },
    /// The model's thinking as it streamed ([`AgentEvent::ReasoningDelta`]),
    /// kept so every device can show it. In the doc its body sits under
    /// `reasoning`, never `text`: readers older than this kind decode an
    /// unknown kind as a text part from `text`, so they get an empty one —
    /// which they skip — instead of the thinking shown as the answer.
    Reasoning {
        id: String,
        text: String,
    },
}

impl MessagePart {
    pub fn id(&self) -> &str {
        match self {
            MessagePart::Text { id, .. }
            | MessagePart::Tool { id, .. }
            | MessagePart::Input { id, .. }
            | MessagePart::Error { id, .. }
            | MessagePart::Reasoning { id, .. } => id,
        }
    }

    pub fn byte_len(&self) -> usize {
        match self {
            MessagePart::Text {
                text, agent_text, ..
            } => text.len() + agent_text.as_ref().map_or(0, String::len),
            MessagePart::Tool {
                call,
                output,
                progress,
                diff,
                diff_stats,
                ..
            } => {
                serde_json::to_vec(call).map_or(0, |v| v.len())
                    + output.as_ref().map_or(0, String::len)
                    + progress.as_ref().map_or(0, String::len)
                    + diff
                        .as_ref()
                        .map_or(0, |d| serde_json::to_vec(d).map_or(0, |v| v.len()))
                    + diff_stats
                        .as_ref()
                        .map_or(0, |s| serde_json::to_vec(s).map_or(0, |v| v.len()))
            }
            MessagePart::Input { questions, .. } => {
                serde_json::to_vec(questions).map_or(0, |v| v.len())
            }
            MessagePart::Error { message, .. } => message.len(),
            MessagePart::Reasoning { text, .. } => text.len(),
        }
    }
}

/// Fold one agent event into a parts accumulator, in place.
///
/// In place because the fold runs once per streamed event: rebuilding the
/// accumulator each time made long turns O(n²) in allocations.
///
/// Semantics from zeron `foldEventIntoParts`:
/// - `SessionStarted` / `Steered` reset the accumulator (turn boundary — makes replay safe).
/// - `TextDelta` appends to the trailing text part, or starts a new one if the trail is not text
///   (a tool call in between breaks the text block).
/// - `ToolCall` appends, or refreshes in place when the id already exists (SDK retry idempotence).
/// - `ToolResult` marks the matching tool part resolved / errored in place.
/// - `InputRequested` appends an input part; `InputResolved` marks it resolved.
/// - `Error` and `Done{error}` become visible error parts.
pub fn fold_event_into_parts(out: &mut Vec<MessagePart>, event: &AgentEvent) {
    match event {
        AgentEvent::SessionStarted { .. } | AgentEvent::Steered { .. } => {
            out.clear();
        }
        AgentEvent::TextDelta { text } => {
            if let Some(MessagePart::Text { text: tail, .. }) = out.last_mut() {
                tail.push_str(text);
            } else {
                let id = format!("t{}", out.len());
                out.push(MessagePart::Text {
                    id,
                    text: text.clone(),
                    agent_text: None,
                });
            }
        }
        AgentEvent::ReasoningDelta { text } => {
            // Thinking grows its trailing part the way text does; anything
            // else in between (text, a tool call) starts a new one. Empty
            // deltas are heartbeats and fold to nothing.
            if text.is_empty() {
                return;
            }
            if let Some(MessagePart::Reasoning { text: tail, .. }) = out.last_mut() {
                tail.push_str(text);
            } else {
                let id = format!("r{}", out.len());
                out.push(MessagePart::Reasoning {
                    id,
                    text: text.clone(),
                });
            }
        }
        AgentEvent::ToolCall { id, call } => {
            if let Some(existing) = out.iter_mut().find_map(|p| match p {
                MessagePart::Tool {
                    id: pid, call: c, ..
                } if pid == id => Some(c),
                _ => None,
            }) {
                *existing = call.clone();
            } else {
                out.push(MessagePart::Tool {
                    id: id.clone(),
                    call: call.clone(),
                    is_error: false,
                    resolved: false,
                    output: None,
                    progress: None,
                    diff: None,
                    output_ref: None,
                    output_bytes: None,
                    diff_ref: None,
                    diff_stats: None,
                });
            }
        }
        AgentEvent::ToolProgress { id, output } => {
            // Live tail onto an UNRESOLVED tool part only: an unknown id (the
            // fold reset at a steer/park since the call) or a resolved tool
            // (result already folded) ignores the tick — the transient column
            // exists only while the tool is actually in flight.
            for p in out.iter_mut() {
                if let MessagePart::Tool {
                    id: pid,
                    resolved,
                    progress,
                    ..
                } = p
                    && pid == id
                    && !*resolved
                {
                    *progress = Some(tail_progress(output));
                }
            }
        }
        AgentEvent::Translation { text } => {
            // Assignment, not accumulation: the publisher sends the whole
            // rendering it wants (already carrying its append/replace mode),
            // so a stream of frames grows the answer in place and any one of
            // them can be the last. Folding a delta here instead would make
            // the result depend on how many frames happened to arrive, and a
            // replayed or repeated frame would duplicate the answer.
            //
            // A frame whose text is unchanged is a keepalive: it proves the
            // turn is alive, and diffs to nothing here and in the doc writer.
            //
            // The text being overwritten is the answer as the model wrote it,
            // and it exists nowhere else in the doc — kept as the part's
            // `agent_text` the first time a frame changes it. A last frame
            // that restores the original (a failed or unchanged translation)
            // leaves nothing translated, so nothing is kept.
            if let Some(MessagePart::Text {
                text: current,
                agent_text,
                ..
            }) = out
                .iter_mut()
                .rev()
                .find(|part| matches!(part, MessagePart::Text { .. }))
            {
                if agent_text.is_none() && current != text {
                    *agent_text = Some(current.clone());
                }
                current.clone_from(text);
                if agent_text.as_deref() == Some(current.as_str()) {
                    *agent_text = None;
                }
            }
        }
        AgentEvent::ToolResult {
            id,
            is_error,
            output,
            diff,
        } => {
            for p in out.iter_mut() {
                if let MessagePart::Tool {
                    id: pid,
                    is_error: e,
                    resolved,
                    output: out_slot,
                    progress,
                    diff: diff_slot,
                    output_bytes,
                    diff_stats,
                    ..
                } = p
                    && pid == id
                {
                    *e = *is_error;
                    *resolved = true;
                    // Resolve clears the live tail: the transient progress
                    // column is a running-state artifact, gone the moment the
                    // tool settles (the chip collapses back to its plain
                    // form).
                    *progress = None;
                    // Keep the bounded output summary in the doc so expanding
                    // a tool chip actually shows what the command returned.
                    // Full output remains out of the doc budget; a sidecar can
                    // be added later for a "show full output" affordance.
                    *out_slot = output.as_deref().and_then(summarize_tool_output);
                    *output_bytes = None;
                    *diff_slot = None;
                    *diff_stats = diff.as_ref().map(|d| vec![diff_stat(d)]);
                }
            }
        }
        AgentEvent::InputRequested {
            request_id,
            questions,
        } => {
            let id = format!("in-{request_id}");
            if !out.iter().any(|p| p.id() == id) {
                out.push(MessagePart::Input {
                    id,
                    request_id: request_id.clone(),
                    questions: questions.clone(),
                    resolved: false,
                });
            }
        }
        AgentEvent::InputResolved { request_id } => {
            for p in out.iter_mut() {
                if let MessagePart::Input {
                    request_id: rid,
                    resolved,
                    ..
                } = p
                    && rid == request_id
                {
                    *resolved = true;
                }
            }
        }
        AgentEvent::Error { message } => {
            let id = format!("e{}", out.len());
            out.push(MessagePart::Error {
                id,
                message: message.clone(),
            });
        }
        AgentEvent::Done { error, .. } => {
            if let Some(message) = error {
                let id = format!("e{}", out.len());
                out.push(MessagePart::Error {
                    id,
                    message: message.clone(),
                });
            }
        }
        // AvailableCommands feeds the engine's per-harness command cache, not
        // the transcript. SubagentStatus is a live session projection (the
        // engine consumes it before the fold) — never transcript content, and
        // so are ContextUsage and Throughput.
        // InputTranslation belongs to the USER entry the engine stamps
        // directly, never to the assistant segment being folded.
        AgentEvent::AssistantMessageCompleted { .. }
        | AgentEvent::Usage { .. }
        | AgentEvent::AvailableCommands { .. }
        | AgentEvent::SubagentStatus { .. }
        | AgentEvent::ContextUsage { .. }
        | AgentEvent::Throughput { .. }
        | AgentEvent::InputTranslation { .. } => {}
    }
}

/// Render-only privacy policy — strip heavy/sensitive tool inputs before a call enters the doc.
///
/// Keeps: command / path / pattern / url / query / todo items / server+tool names, and
/// the script of a Pi `codemode` call and the query of a `tool_search` (both capped).
/// Drops: WriteFile content, EditFile old/new strings, WebFetch prompt, Mcp/Unknown input.
/// Full inputs remain only in the host's local run journal. Idempotent.
pub fn sanitize_tool_call(call: &ToolCall) -> ToolCall {
    match call {
        ToolCall::WriteFile { path, .. } => ToolCall::WriteFile {
            path: path.clone(),
            content: None,
        },
        ToolCall::EditFile { path, .. } => ToolCall::EditFile {
            path: path.clone(),
            old_string: None,
            new_string: None,
        },
        ToolCall::WebFetch { url, .. } => ToolCall::WebFetch {
            url: url.clone(),
            prompt: None,
        },
        ToolCall::Mcp { server, tool, .. } => ToolCall::Mcp {
            server: server.clone(),
            tool: tool.clone(),
            input: None,
        },
        // The pi subagent tool (`extensions/subagents` registers `agent` /
        // `task` / `cwd` / `async` / `timeoutSeconds`): keep ONLY the
        // privacy-safe fields the transcript chip and the subagent panel need
        // — agent, task (≤500 Unicode chars, cut on a char boundary so the
        // stored string is always valid UTF-8), async. Everything else (cwd,
        // timeoutSeconds, …) is dropped; the full input lives only in the
        // run journal.
        ToolCall::Unknown { name, input } if name == "subagent" => {
            let args = input.as_ref().and_then(serde_json::Value::as_object);
            let agent = args
                .and_then(|a| a.get("agent"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let task: String = args
                .and_then(|a| a.get("task"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(SUBAGENT_TASK_MAX_CHARS)
                .collect();
            let is_async = args
                .and_then(|a| a.get("async"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let mut kept = serde_json::Map::new();
            kept.insert("agent".into(), serde_json::Value::String(agent));
            kept.insert("task".into(), serde_json::Value::String(task));
            kept.insert("async".into(), serde_json::Value::Bool(is_async));
            ToolCall::Unknown {
                name: name.clone(),
                input: Some(serde_json::Value::Object(kept)),
            }
        }
        // Pi's codemode script and tool_search query: the one field each
        // invocation is about, capped on a char boundary. Nothing else rides.
        ToolCall::Unknown { name, input }
            if name == cypher_proto::view::CODEMODE_TOOL
                || name == cypher_proto::view::TOOL_SEARCH_TOOL =>
        {
            let (key, cap) = if name == cypher_proto::view::CODEMODE_TOOL {
                ("code", CODEMODE_SCRIPT_MAX_CHARS)
            } else {
                ("query", TOOL_SEARCH_QUERY_MAX_CHARS)
            };
            let value = input
                .as_ref()
                .and_then(|input| input.get(key))
                .and_then(serde_json::Value::as_str)
                .map(|value| value.chars().take(cap).collect::<String>());
            ToolCall::Unknown {
                name: name.clone(),
                input: value.map(|value| serde_json::json!({ key: value })),
            }
        }
        ToolCall::Unknown { name, .. } => ToolCall::Unknown {
            name: name.clone(),
            input: None,
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests;
