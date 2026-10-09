//! Wire parsing for pi's RPC stream: the Cypher extension status frames (subagents,
//! translation) and the mapping of pi tool calls/results onto typed calls.

use super::*;

/// Status key of the cypher subagent status protocol: `extensions/subagents`
/// publishes `setStatus("cypher.subagents.v1", JSON.stringify({version:1,
/// runs:[…]}))`. Every other key stays ignored transient TUI furniture.
pub(crate) const SUBAGENTS_STATUS_KEY: &str = "cypher.subagents.v1";
/// Final-answer translation emitted by the Cypher translation extension.
pub(crate) const TRANSLATION_STATUS_KEY: &str = "cypher.translation.v1";
/// Prompt translation emitted by the same extension: the user's own words and
/// the translation the agent received instead, so the transcript can keep the
/// pair (`{version:1, source, text}`; capped like a final-answer frame).
pub(crate) const INPUT_TRANSLATION_STATUS_KEY: &str = "cypher.translation.input.v1";
/// Whole-snapshot byte cap for one translation frame.
///
/// A frame carries the full replacement for the message's text, so append mode
/// pays for the original AND the translation, and the extension caps only its
/// SOURCE (24k chars). 24k chars of English plus a Chinese translation of them
/// already clears 64KiB — the old cap silently dropped exactly the long answers
/// that are hardest to read untranslated, and with streaming it would have
/// dropped a run of frames and left the answer stuck half-translated. Sized to
/// hold any frame that cap can produce, with room for 4-byte characters on both
/// sides.
pub(super) const TRANSLATION_STATUS_MAX_BYTES: usize = 256 * 1024;
/// Whole-snapshot byte cap (the extension caps at 64KiB; the harness
/// re-checks so a misbehaving publisher can't smuggle an unbounded blob).
const SUBAGENTS_STATUS_MAX_BYTES: usize = 64 * 1024;
/// Max runs in one snapshot.
const SUBAGENTS_MAX_RUNS: usize = 32;
/// Max task chars per run (cut on Unicode boundaries by the publisher).
const SUBAGENTS_TASK_MAX_CHARS: usize = 500;
/// Max progress lines / bytes per run.
const SUBAGENTS_PROGRESS_MAX_LINES: usize = 8;
const SUBAGENTS_PROGRESS_MAX_BYTES: usize = 4096;
/// Max chars for the Cypher child chat id a run may carry (a child chat id is
/// an engine-minted uuid string; a longer value is a publisher bug and is
/// rejected with the rest of the strict per-run parse).
const SUBAGENTS_CHILD_CHAT_ID_MAX_CHARS: usize = 256;

/// Parse a `cypher.subagents.v1` `statusText` into runs.
///
/// - missing/blank text → `Some(vec![])` (a clear snapshot).
/// - anything failing strict validation (not version 1, invalid JSON, an
///   oversize snapshot, more than [`SUBAGENTS_MAX_RUNS`] runs, an over-cap
///   task, an over-cap progress tail, an unknown enum, or a missing required
///   field) → `None` with a warning. The caller must ignore it and keep the
///   run going — a bad status frame is never a reason to interrupt the agent.
pub(super) fn parse_subagent_status(text: &str) -> Option<Vec<SubagentRun>> {
    if text.trim().is_empty() {
        return Some(Vec::new());
    }
    if text.len() > SUBAGENTS_STATUS_MAX_BYTES {
        tracing::warn!(
            target: "cypher_harness::pi",
            bytes = text.len(),
            "subagent status snapshot over 64KiB; ignoring"
        );
        return None;
    }
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                target: "cypher_harness::pi",
                error = %err,
                "subagent status: invalid JSON; ignoring"
            );
            return None;
        }
    };
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        tracing::warn!(
            target: "cypher_harness::pi",
            "subagent status: unsupported snapshot version; ignoring"
        );
        return None;
    }
    let runs = value.get("runs").and_then(Value::as_array)?;
    if runs.len() > SUBAGENTS_MAX_RUNS {
        tracing::warn!(
            target: "cypher_harness::pi",
            count = runs.len(),
            "subagent status: too many runs; ignoring"
        );
        return None;
    }
    let mut out = Vec::with_capacity(runs.len());
    for run in runs {
        match parse_subagent_run(run) {
            Some(parsed) => out.push(parsed),
            None => {
                tracing::warn!(
                    target: "cypher_harness::pi",
                    "subagent status: malformed run; ignoring snapshot"
                );
                return None;
            }
        }
    }
    Some(out)
}

/// Parse a `cypher.translation.v1` `statusText` into the rendered replacement.
///
/// One frame of a streaming translation: `text` is the whole replacement for
/// the message's rendered text, already carrying the publisher's append/replace
/// mode. The whole-snapshot byte cap is the only size bound — a translation is
/// routinely longer than its source (CJK expands severalfold into English), so
/// a second cap measured against the extension's *source* limit would reject
/// well-formed frames.
pub(super) fn parse_translation_status(text: &str) -> Option<String> {
    if text.len() > TRANSLATION_STATUS_MAX_BYTES {
        tracing::warn!(
            target: "cypher_harness::pi",
            bytes = text.len(),
            "translation status snapshot over cap; ignoring"
        );
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let translated = value.get("text").and_then(Value::as_str)?.trim();
    if translated.is_empty() {
        return None;
    }
    Some(translated.to_owned())
}

/// Parse a `cypher.translation.input.v1` `statusText` into `(source, text)`:
/// the user's words and the translation the agent read in their place. Both
/// must be non-empty; the whole-snapshot byte cap bounds the pair.
pub(super) fn parse_input_translation_status(text: &str) -> Option<(String, String)> {
    if text.len() > TRANSLATION_STATUS_MAX_BYTES {
        tracing::warn!(
            target: "cypher_harness::pi",
            bytes = text.len(),
            "input translation status over cap; ignoring"
        );
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let source = value.get("source").and_then(Value::as_str)?.trim();
    let translated = value.get("text").and_then(Value::as_str)?.trim();
    if source.is_empty() || translated.is_empty() {
        return None;
    }
    Some((source.to_owned(), translated.to_owned()))
}

/// Strict per-run parse of one `cypher.subagents.v1` run object. `None` on
/// any malformed field (missing runId/agent, unknown mode/status enum, or an
/// over-cap task/progress).
fn parse_subagent_run(v: &Value) -> Option<SubagentRun> {
    let run_id = v.get("runId").and_then(Value::as_str)?.to_owned();
    let agent = v.get("agent").and_then(Value::as_str)?.to_owned();
    let task = v.get("task").and_then(Value::as_str).unwrap_or_default();
    if task.chars().count() > SUBAGENTS_TASK_MAX_CHARS {
        return None;
    }
    let mode = match v.get("mode").and_then(Value::as_str)? {
        "sync" => SubagentRunMode::Sync,
        "async" => SubagentRunMode::Async,
        "message" => SubagentRunMode::Message,
        _ => return None,
    };
    let status = match v.get("status").and_then(Value::as_str)? {
        "running" => SubagentRunStatus::Running,
        "done" => SubagentRunStatus::Done,
        "error" => SubagentRunStatus::Error,
        _ => return None,
    };
    let progress = v.get("progress").and_then(Value::as_str);
    if let Some(progress) = progress
        && (progress.lines().count() > SUBAGENTS_PROGRESS_MAX_LINES
            || progress.len() > SUBAGENTS_PROGRESS_MAX_BYTES)
    {
        return None;
    }
    // Bounded child chat id: a too-long value is a publisher bug — reject the
    // whole snapshot like the other over-cap fields.
    let child_chat_id = match v.get("childChatId").and_then(Value::as_str) {
        Some(id) if id.chars().count() > SUBAGENTS_CHILD_CHAT_ID_MAX_CHARS => return None,
        Some(id) => Some(id.to_owned()),
        None => None,
    };
    Some(SubagentRun {
        run_id,
        tool_call_id: v
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        agent,
        model: v.get("model").and_then(Value::as_str).map(str::to_owned),
        task: task.to_owned(),
        mode,
        status,
        progress: progress.map(str::to_owned),
        started_at: v.get("startedAt").and_then(Value::as_i64)?,
        updated_at: v.get("updatedAt").and_then(Value::as_i64)?,
        ended_at: v.get("endedAt").and_then(Value::as_i64),
        // The extension publishes the Cypher child chat id when the engine
        // hosts the run (`StartSubagent` bridge). Absent on standalone runs
        // and on old publishers.
        child_chat_id,
    })
}

/// The MCP server names a run can see, for naming MCP tool calls: the
/// agent's `mcp.json` and the project's `.pi/mcp.json` (Pi reads the latter
/// only for trusted projects; listing it regardless names nothing that never
/// runs). Best effort — a missing or unreadable file lists nothing.
pub(super) fn mcp_server_names(agent_dir: Option<&std::path::Path>, cwd: &str) -> Vec<String> {
    let files = agent_dir
        .map(|dir| dir.join("mcp.json"))
        .into_iter()
        .chain((!cwd.is_empty()).then(|| std::path::Path::new(cwd).join(".pi/mcp.json")));
    let mut names: Vec<String> = Vec::new();
    for file in files {
        // The engine's own bound on an mcp.json it will parse.
        if std::fs::metadata(&file).is_ok_and(|meta| meta.len() > 1_048_576) {
            continue;
        }
        let Some(value) = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        for server in value
            .get("mcpServers")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|servers| servers.keys())
        {
            if !names.contains(server) {
                names.push(server.clone());
            }
        }
    }
    names
}

/// Split a Pi MCP tool name into `(server, tool)`. Pi names every MCP tool
/// `mcp__{server}__{tool}` with each character outside `[A-Za-z0-9_]`
/// replaced by `_`, so a configured server whose sanitized name prefixes the
/// tool gives back the name Settings shows (`mvp-lab`, not `mvp_lab`), and
/// the longest such match wins. A server the list does not know (added
/// mid-run) splits at the first `__`.
pub(super) fn mcp_tool_parts(name: &str, servers: &[String]) -> Option<(String, String)> {
    let rest = name.strip_prefix("mcp__")?;
    let sanitize = |server: &str| -> String {
        server
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    servers
        .iter()
        .filter_map(|server| {
            let tool = rest.strip_prefix(&sanitize(server))?.strip_prefix("__")?;
            (!tool.is_empty()).then(|| (server.clone(), tool.to_owned()))
        })
        .max_by_key(|(server, _)| server.len())
        .or_else(|| {
            let (server, tool) = rest.split_once("__")?;
            (!server.is_empty() && !tool.is_empty()).then(|| (server.to_owned(), tool.to_owned()))
        })
}

/// pi's built-in tool set (`read`/`bash`/`write`/`edit`/`grep`/`find`/`ls`)
/// maps onto the typed [`ToolCall`] cypher renders, extracting the known arg
/// names; so do Pi's MCP tools (`mcp__{server}__{tool}`, named against the
/// run's `mcp_servers`) and pi-web-search's `web_search`. Other extension
/// tools fall through to [`ToolCall::Unknown`] with the raw args.
pub(super) fn pi_typed_call(name: &str, args: &Value, mcp_servers: &[String]) -> ToolCall {
    let arg = |key: &str| -> Option<String> {
        args.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    match name {
        "bash" => ToolCall::Exec {
            command: arg("command").unwrap_or_default(),
        },
        "read" => ToolCall::ReadFile {
            path: arg("path").unwrap_or_default(),
        },
        "write" => ToolCall::WriteFile {
            path: arg("path").unwrap_or_default(),
            content: None,
        },
        "edit" => ToolCall::EditFile {
            path: arg("path").unwrap_or_default(),
            old_string: None,
            new_string: None,
        },
        "grep" => ToolCall::Search {
            pattern: arg("pattern").unwrap_or_default(),
            path: arg("path"),
        },
        "find" => ToolCall::Glob {
            pattern: arg("pattern").unwrap_or_default(),
        },
        "ls" => ToolCall::Search {
            pattern: String::new(),
            path: arg("path"),
        },
        "web_search" if arg("query").is_some() => ToolCall::WebSearch {
            query: arg("query").unwrap_or_default(),
        },
        _ => match mcp_tool_parts(name, mcp_servers) {
            Some((server, tool)) => ToolCall::Mcp {
                server,
                tool,
                input: Some(args.clone()),
            },
            None => ToolCall::Unknown {
                name: name.to_owned(),
                input: Some(args.clone()),
            },
        },
    }
}

/// A codemode result opens with a status block (`Script completed` or
/// `Script failed`, `Wall time … seconds`, `Output:`) as its first text
/// content. The chip already shows the status and the doc keeps only the
/// first lines of an output, so the block would crowd out the script's own
/// output: drop it, keep everything after it (a failure's `Script error:`
/// text included).
pub(super) fn without_codemode_header(result: &Value) -> Value {
    let mut result = result.clone();
    if let Some(content) = result.get_mut("content").and_then(Value::as_array_mut)
        && content.first().is_some_and(|block| {
            block
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| {
                    (text.starts_with("Script completed\n") || text.starts_with("Script failed\n"))
                        && text.ends_with("Output:\n")
                })
        })
    {
        content.remove(0);
    }
    result
}

/// The joined text of a pi tool result's `content` blocks (`{type: "text",
/// text}`), capped at [`OUTPUT_CAP`](crate::OUTPUT_CAP).
pub(super) fn tool_output_text(result: &Value) -> Option<String> {
    let parts: Vec<&str> = result
        .get("content")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|c| c.get("text").and_then(Value::as_str))
        .filter(|t| !t.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(cap_text(&parts.join("\n"), OUTPUT_CAP))
}
