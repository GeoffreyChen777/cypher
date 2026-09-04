//! Message parts: the event fold, the render-only privacy policy, and continuation splitting.
//!
//! Ports of `packages/control/src/parts.ts` (fold) and
//! `packages/session-doc/src/{render-parts,messages}.ts`.

use serde::{Deserialize, Serialize};

use cypher_proto::{AgentEvent, ToolCall, ToolDiff, UserInputQuestion};

use crate::constants::MSG_INLINE_MAX;

/// Line cap for the tool-output SUMMARY persisted into the doc. Keeping a
/// small number of complete lines makes the expandable detail useful without
/// imposing an arbitrary character limit on a single long line.
pub const TOOL_OUTPUT_SUMMARY_MAX_LINES: usize = 5;

/// Char cap for the `subagent` tool's `task` kept in the doc (privacy-safe
/// persistence, [`sanitize_tool_call`]). Cut on a Unicode-char boundary, so
/// the stored string is always valid UTF-8.
pub const SUBAGENT_TASK_MAX_CHARS: usize = 500;

/// The doc-resident form of a tool output (docs/chat2-sync.md A1; the R2
/// sidecar is PARKED as of 2026-08-10, so this IS the whole record in the
/// doc — the full text survives only in the host's local run journal):
///
/// - Markdown code fences are stripped first — ACP harnesses fence every
///   output, so the fence is transport wrapping, never content (pre-fix,
///   every summary read "```console…").
/// - Outputs keep complete lines, up to [`TOOL_OUTPUT_SUMMARY_MAX_LINES`].
/// - A long single line is kept whole; the limit is by lines, not characters.
///
/// `None` for blank output.
pub fn summarize_tool_output(text: &str) -> Option<String> {
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
pub const TOOL_PROGRESS_MAX_LINES: usize = 8;
pub const TOOL_PROGRESS_MAX_BYTES: usize = 4096;

/// Keep the tail of a live progress blob: last ≤8 lines, whole lines, within
/// 4KB (truncating by bytes would split a line mid-character; lines are the
/// UI's render unit).
pub fn tail_progress(text: &str) -> String {
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
pub fn diff_stat(diff: &ToolDiff) -> ToolDiffStat {
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MessagePart {
    Text {
        id: String,
        text: String,
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
        /// stamped by [`apply_sidecar_refs`] (the fold is chat-agnostic).
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
}

impl MessagePart {
    pub fn id(&self) -> &str {
        match self {
            MessagePart::Text { id, .. }
            | MessagePart::Tool { id, .. }
            | MessagePart::Input { id, .. }
            | MessagePart::Error { id, .. } => id,
        }
    }

    pub fn byte_len(&self) -> usize {
        match self {
            MessagePart::Text { text, .. } => text.len(),
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
                });
            }
        }
        AgentEvent::ReasoningDelta { .. } => {
            // Reasoning is not rendered as a transcript part (matches zeron).
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
        // engine consumes it before the fold) — never transcript content.
        AgentEvent::AssistantMessageCompleted { .. }
        | AgentEvent::Usage { .. }
        | AgentEvent::AvailableCommands { .. }
        | AgentEvent::SubagentStatus { .. } => {}
    }
}

/// Stamp sidecar keys onto resolved tool parts that have sidecar content.
///
/// Separate from the fold because the fold is chat-agnostic and pure; the
/// caller (who knows the chat id) runs this right after each fold step, before
/// the parts hit the doc. Idempotent. Key shape `{chatId}/{partId}` (+
/// `.diff`) matches the edge's `/blob/{chatId}/{partId}` route.
pub fn apply_sidecar_refs(chat_id: &str, parts: &mut [MessagePart]) {
    for part in parts.iter_mut() {
        if let MessagePart::Tool {
            id,
            resolved: true,
            output_ref,
            output_bytes,
            diff_ref,
            diff_stats,
            ..
        } = part
        {
            if output_ref.is_none() && output_bytes.is_some() {
                *output_ref = Some(format!("{chat_id}/{id}"));
            }
            if diff_ref.is_none() && diff_stats.is_some() {
                *diff_ref = Some(format!("{chat_id}/{id}.diff"));
            }
        }
    }
}

/// What a [`AgentEvent::ToolResult`] owes the sidecar: the full output text
/// and/or the full diff (as JSON), keyed by part id. `None` when the event
/// carries nothing worth uploading.
#[derive(Debug, Clone, PartialEq)]
pub struct SidecarPayload {
    pub part_id: String,
    pub output: Option<String>,
    pub diff: Option<ToolDiff>,
}

pub fn sidecar_payload(event: &AgentEvent) -> Option<SidecarPayload> {
    let AgentEvent::ToolResult {
        id, output, diff, ..
    } = event
    else {
        return None;
    };
    let output = output.clone().filter(|o| !o.trim().is_empty());
    if output.is_none() && diff.is_none() {
        return None;
    }
    Some(SidecarPayload {
        part_id: id.clone(),
        output,
        diff: diff.clone(),
    })
}

/// Render-only privacy policy — strip heavy/sensitive tool inputs before a call enters the doc.
///
/// Keeps: command / path / pattern / url / query / todo items / server+tool names.
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
        ToolCall::Unknown { name, .. } => ToolCall::Unknown {
            name: name.clone(),
            input: None,
        },
        other => other.clone(),
    }
}

/// Deterministic continuation id: `"{root}#c{n}"`.
pub fn continuation_id(root: &str, index: usize) -> String {
    format!("{root}#c{index}")
}

/// Split an oversized parts list into chunks each under `MSG_INLINE_MAX` bytes.
///
/// Splitting happens at part boundaries; an oversized text part is itself chunked at char
/// boundaries. Returns one Vec per resulting entry — the first keeps the root id, the rest are
/// continuations (`continuation_id(root, i)`), matching `splitMessageEntry` in zeron.
pub fn split_parts(parts: &[MessagePart]) -> Vec<Vec<MessagePart>> {
    let mut chunks: Vec<Vec<MessagePart>> = vec![Vec::new()];
    let mut current_bytes = 0usize;

    let push_part = |chunks: &mut Vec<Vec<MessagePart>>, current: &mut usize, part: MessagePart| {
        let len = part.byte_len();
        if *current > 0 && *current + len > MSG_INLINE_MAX {
            chunks.push(Vec::new());
            *current = 0;
        }
        *current += len;
        chunks.last_mut().unwrap().push(part);
    };

    for part in parts {
        match part {
            MessagePart::Text { id, text } if text.len() > MSG_INLINE_MAX => {
                // Chunk oversized text at char boundaries.
                let mut start = 0usize;
                let mut piece = 0usize;
                while start < text.len() {
                    let mut end = (start + MSG_INLINE_MAX).min(text.len());
                    while end < text.len() && !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    // Guard: ensure forward progress on pathological boundaries.
                    if end <= start {
                        end = text.len();
                    }
                    let sub = MessagePart::Text {
                        id: if piece == 0 {
                            id.clone()
                        } else {
                            format!("{id}~{piece}")
                        },
                        text: text[start..end].to_string(),
                    };
                    push_part(&mut chunks, &mut current_bytes, sub);
                    start = end;
                    piece += 1;
                }
            }
            other => push_part(&mut chunks, &mut current_bytes, other.clone()),
        }
    }
    chunks
}

/// Render-time inverse of splitting: concatenate continuation entries' parts in list order.
pub fn join_continuations(entries: Vec<Vec<MessagePart>>) -> Vec<MessagePart> {
    entries.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_delta(s: &str) -> AgentEvent {
        AgentEvent::TextDelta { text: s.into() }
    }

    #[test]
    fn text_deltas_merge_until_broken_by_tool() {
        let mut parts = Vec::new();
        fold_event_into_parts(&mut parts, &text_delta("Hello "));
        fold_event_into_parts(&mut parts, &text_delta("world"));
        assert_eq!(parts.len(), 1);
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "tool-1".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
            },
        );
        fold_event_into_parts(&mut parts, &text_delta("after"));
        assert_eq!(parts.len(), 3);
        match &parts[2] {
            MessagePart::Text { text, .. } => assert_eq!(text, "after"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn session_started_resets_accumulator() {
        let mut parts = Vec::new();
        fold_event_into_parts(&mut parts, &text_delta("junk"));
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::SessionStarted {
                harness: cypher_proto::HarnessId::Mock,
                model: "m".into(),
                tools: vec![],
                cwd: "/".into(),
                session_id: "s".into(),
                assistant_message_id: "a".into(),
            },
        );
        assert!(parts.is_empty());
    }

    #[test]
    fn tool_call_refresh_is_idempotent() {
        let call = AgentEvent::ToolCall {
            id: "t".into(),
            call: ToolCall::Exec {
                command: "ls".into(),
            },
        };
        let mut once = Vec::new();
        fold_event_into_parts(&mut once, &call);
        let mut twice = once.clone();
        fold_event_into_parts(&mut twice, &call);
        assert_eq!(once, twice);
    }

    #[test]
    fn tool_result_marks_resolution() {
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "t".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
            },
        );
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolResult {
                id: "t".into(),
                is_error: true,
                output: None,
                diff: None,
            },
        );
        match &parts[0] {
            MessagePart::Tool {
                is_error, resolved, ..
            } => {
                assert!(*is_error);
                assert!(*resolved);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn sanitize_strips_heavy_inputs_and_is_idempotent() {
        let call = ToolCall::WriteFile {
            path: "/x".into(),
            content: Some("secret".into()),
        };
        let clean = sanitize_tool_call(&call);
        assert_eq!(
            clean,
            ToolCall::WriteFile {
                path: "/x".into(),
                content: None
            }
        );
        assert_eq!(sanitize_tool_call(&clean), clean);
    }

    /// The subagent tool keeps ONLY the panel/chip fields (agent, task,
    /// async) — cwd/timeout/anything else never enter the doc. Idempotent.
    #[test]
    fn sanitize_subagent_keeps_only_privacy_safe_fields() {
        let call = ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(serde_json::json!({
                "agent": "planner",
                "task": "Plan the panel\n(step by step)",
                "cwd": "/secret/repo",
                "async": true,
                "timeoutSeconds": 600,
            })),
        };
        let clean = sanitize_tool_call(&call);
        let ToolCall::Unknown { name, input } = &clean else {
            panic!("stays an unknown tool");
        };
        assert_eq!(name, "subagent");
        let args = input
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .expect("kept input");
        assert_eq!(args.len(), 3, "only agent/task/async survive");
        assert_eq!(args["agent"], "planner");
        assert_eq!(args["task"], "Plan the panel\n(step by step)");
        assert_eq!(args["async"], true);
        assert!(args.get("cwd").is_none());
        assert!(args.get("timeoutSeconds").is_none());
        // Idempotent: sanitizing the sanitized call is a no-op.
        assert_eq!(sanitize_tool_call(&clean), clean);
    }

    /// A task longer than [`SUBAGENT_TASK_MAX_CHARS`] is cut on a Unicode
    /// boundary so the stored string stays valid UTF-8.
    #[test]
    fn sanitize_subagent_truncates_task_on_char_boundary() {
        let long = "é".repeat(SUBAGENT_TASK_MAX_CHARS + 40);
        let call = ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(serde_json::json!({ "agent": "actor", "task": long })),
        };
        let clean = sanitize_tool_call(&call);
        let ToolCall::Unknown { input, .. } = clean else {
            panic!("stays unknown");
        };
        let args = input
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .expect("kept input");
        let task = args["task"].as_str().expect("task string");
        assert_eq!(task.chars().count(), SUBAGENT_TASK_MAX_CHARS);
        assert!(std::str::from_utf8(task.as_bytes()).is_ok(), "valid UTF-8");
    }

    /// Ordinary Unknown tools still clear their input wholesale.
    #[test]
    fn sanitize_other_unknown_still_clears_input() {
        let call = ToolCall::Unknown {
            name: "send_message".into(),
            input: Some(serde_json::json!({ "to": "main", "content": "secret" })),
        };
        let clean = sanitize_tool_call(&call);
        assert_eq!(
            clean,
            ToolCall::Unknown {
                name: "send_message".into(),
                input: None
            }
        );
    }

    #[test]
    fn split_and_join_round_trip() {
        let big = "x".repeat(MSG_INLINE_MAX * 2 + 100);
        let parts = vec![
            MessagePart::Text {
                id: "t0".into(),
                text: big.clone(),
            },
            MessagePart::Tool {
                id: "tool-1".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
                is_error: false,
                resolved: true,
                output: None,
                progress: None,
                diff: None,
                output_ref: None,
                output_bytes: None,
                diff_ref: None,
                diff_stats: None,
            },
        ];
        let chunks = split_parts(&parts);
        assert!(
            chunks.len() >= 3,
            "expected >=3 chunks, got {}",
            chunks.len()
        );
        for chunk in &chunks {
            let bytes: usize = chunk.iter().map(|p| p.byte_len()).sum();
            assert!(bytes <= MSG_INLINE_MAX, "chunk over cap: {bytes}");
        }
        let joined = join_continuations(chunks);
        let text: String = joined
            .iter()
            .filter_map(|p| match p {
                MessagePart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, big);
        assert!(matches!(joined.last().unwrap(), MessagePart::Tool { .. }));
    }

    #[test]
    fn continuation_ids_are_deterministic() {
        assert_eq!(continuation_id("m1", 1), "m1#c1");
    }

    // ── A1 strip (docs/chat2-sync.md) ───────────────────────────────────────

    #[test]
    fn summarize_keeps_five_lines_without_a_character_cap() {
        assert_eq!(summarize_tool_output(""), None);
        assert_eq!(summarize_tool_output("  \n\t\n"), None);
        assert_eq!(summarize_tool_output("one line"), Some("one line".into()));
        // Small multi-line outputs ride whole — no summary, no "…".
        assert_eq!(
            summarize_tool_output("\n\nfirst real\nsecond"),
            Some("first real\nsecond".into())
        );
        assert_eq!(
            summarize_tool_output("only line\n\n  \n"),
            Some("only line".into())
        );
        // Markdown fences are transport wrapping, never content: stripped
        // even when they'd otherwise be the first line, and a fence-only
        // output is blank.
        assert_eq!(
            summarize_tool_output("```console\nreal content\n```"),
            Some("real content".into())
        );
        assert_eq!(summarize_tool_output("```\n```"), None);
        // Big outputs keep the first five complete lines, with no character
        // cap on any individual line.
        let big = format!(
            "```console\nline one {}\nline two\nline three\nline four\nline five\nline six\n```",
            "x".repeat(400)
        );
        let summary = summarize_tool_output(&big).unwrap();
        assert_eq!(summary.lines().count(), TOOL_OUTPUT_SUMMARY_MAX_LINES);
        assert!(summary.contains(&"x".repeat(400)));
        assert!(!summary.contains("line six"));
    }

    #[test]
    fn diff_stat_counts_line_changes() {
        let stat = diff_stat(&ToolDiff {
            path: "/w/a.rs".into(),
            old_text: Some("a\nb\nc\n".into()),
            new_text: "a\nB\nc\nd\n".into(),
        });
        assert_eq!(stat.path, "/w/a.rs");
        assert_eq!(stat.additions, 2); // B + d
        assert_eq!(stat.deletions, 1); // b
        // New file: every line is an addition.
        let stat = diff_stat(&ToolDiff {
            path: "/w/new.rs".into(),
            old_text: None,
            new_text: "one\ntwo\n".into(),
        });
        assert_eq!((stat.additions, stat.deletions), (2, 0));
    }

    #[test]
    fn fold_strips_output_to_summary_and_diff_to_stats() {
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "t".into(),
                call: ToolCall::Exec {
                    command: "cargo test".into(),
                },
            },
        );
        let full = "running 42 tests\n".repeat(300); // ~5KB, was 4KB inline pre-strip
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolResult {
                id: "t".into(),
                is_error: false,
                output: Some(full.clone()),
                diff: Some(ToolDiff {
                    path: "/w/a.rs".into(),
                    old_text: Some("a\n".into()),
                    new_text: "b\n".into(),
                }),
            },
        );
        match &parts[0] {
            MessagePart::Tool {
                output,
                output_bytes,
                diff,
                diff_stats,
                ..
            } => {
                // The bounded summary is doc-resident and powers the
                // expandable output body; diff text still becomes stats.
                assert_eq!(
                    output.as_deref(),
                    Some(
                        "running 42 tests\nrunning 42 tests\nrunning 42 tests\nrunning 42 tests\nrunning 42 tests"
                    )
                );
                assert_eq!(*output_bytes, None);
                assert!(diff.is_none(), "inline diff text must not enter the doc");
                let stats = diff_stats.as_ref().unwrap();
                assert_eq!(stats.len(), 1);
                assert_eq!((stats[0].additions, stats[0].deletions), (1, 1));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn tool_progress_tails_onto_unresolved_parts_and_resolve_clears() {
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "t".into(),
                call: ToolCall::Unknown {
                    name: "subagent".into(),
                    input: None,
                },
            },
        );
        // A long stream keeps the LAST 8 lines (cut head, keep tail).
        let long = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolProgress {
                id: "t".into(),
                output: long.clone(),
            },
        );
        match &parts[0] {
            MessagePart::Tool { progress, .. } => {
                let progress = progress.as_deref().expect("progress column set");
                assert_eq!(progress.lines().count(), 8);
                assert!(
                    progress.starts_with("line 12"),
                    "tail keeps the LAST lines: {progress}"
                );
                assert!(progress.ends_with("line 19"));
            }
            other => panic!("unexpected {other:?}"),
        }
        // A later tick overwrites the tail.
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolProgress {
                id: "t".into(),
                output: "fresh".into(),
            },
        );
        assert!(matches!(
            &parts[0],
            MessagePart::Tool { progress: Some(p), .. } if p == "fresh"
        ));
        // Resolve clears the transient tail but keeps the bounded result
        // summary for the expandable chip body.
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolResult {
                id: "t".into(),
                is_error: false,
                output: Some("final".into()),
                diff: None,
            },
        );
        match &parts[0] {
            MessagePart::Tool {
                resolved: true,
                progress: None,
                output: Some(output),
                ..
            } => assert_eq!(output, "final"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn tool_progress_ignores_unknown_and_resolved_ids() {
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "t".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
            },
        );
        // Unknown id: no tool part matches — ignored, parts untouched.
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolProgress {
                id: "ghost".into(),
                output: "noise".into(),
            },
        );
        assert_eq!(parts.len(), 1);
        assert!(matches!(
            &parts[0],
            MessagePart::Tool { progress: None, .. }
        ));
        // Resolve, then a late tick for the same id: ignored.
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolResult {
                id: "t".into(),
                is_error: false,
                output: None,
                diff: None,
            },
        );
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolProgress {
                id: "t".into(),
                output: "late".into(),
            },
        );
        assert!(matches!(
            &parts[0],
            MessagePart::Tool {
                resolved: true,
                progress: None,
                ..
            }
        ));
    }

    #[test]
    fn tail_progress_caps_lines_and_bytes_without_splitting_lines() {
        assert_eq!(tail_progress(""), "");
        assert_eq!(tail_progress("a\nb"), "a\nb");
        // Line cap: last 8.
        let many = (0..30)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = tail_progress(&many);
        assert_eq!(tail.lines().count(), 8);
        assert!(tail.ends_with("l29"));
        // Byte cap: whole lines only (never a mid-line cut).
        let huge_line = "x".repeat(TOOL_PROGRESS_MAX_BYTES + 100);
        let single = format!("head\n{huge_line}");
        let tail = tail_progress(&single);
        assert_eq!(
            tail, huge_line,
            "the oversized last line rides whole (single line stays readable)"
        );
        // Both caps: last lines within 4KB.
        let wide = (0..200)
            .map(|i| format!("line {i} {}", "y".repeat(60)))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = tail_progress(&wide);
        assert!(
            tail.len() <= TOOL_PROGRESS_MAX_BYTES,
            "{} bytes",
            tail.len()
        );
        assert!(
            tail.starts_with("line "),
            "kept the TAIL, not the head: {tail}"
        );
        // Trailing newline is preserved when budget allows.
        let tail = tail_progress("a\nb\n");
        assert_eq!(tail, "a\nb\n");
    }

    #[test]
    fn sidecar_refs_stamp_once_and_only_where_content_exists() {
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "t1".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
            },
        );
        // Unresolved: no refs yet.
        apply_sidecar_refs("chat-9", &mut parts);
        assert!(matches!(
            &parts[0],
            MessagePart::Tool {
                output_ref: None,
                diff_ref: None,
                ..
            }
        ));
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolResult {
                id: "t1".into(),
                is_error: false,
                output: Some("hello".into()),
                diff: Some(ToolDiff {
                    path: "/w/a".into(),
                    old_text: None,
                    new_text: "x\n".into(),
                }),
            },
        );
        apply_sidecar_refs("chat-9", &mut parts);
        apply_sidecar_refs("chat-9", &mut parts); // idempotent
        match &parts[0] {
            MessagePart::Tool {
                output_ref,
                diff_ref,
                ..
            } => {
                // The output summary is already doc-resident. Full output
                // sidecar refs remain absent until sidecar storage is enabled;
                // diff STATS still get their ref shape.
                assert_eq!(output_ref.as_deref(), None);
                assert_eq!(diff_ref.as_deref(), Some("chat-9/t1.diff"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn sidecar_payload_carries_full_texts() {
        assert_eq!(
            sidecar_payload(&AgentEvent::TextDelta { text: "x".into() }),
            None
        );
        assert_eq!(
            sidecar_payload(&AgentEvent::ToolResult {
                id: "t".into(),
                is_error: false,
                output: Some("   \n".into()),
                diff: None,
            }),
            None,
            "blank output uploads nothing"
        );
        let payload = sidecar_payload(&AgentEvent::ToolResult {
            id: "t".into(),
            is_error: true,
            output: Some("full output".into()),
            diff: None,
        })
        .unwrap();
        assert_eq!(payload.part_id, "t");
        assert_eq!(payload.output.as_deref(), Some("full output"));
    }
}
