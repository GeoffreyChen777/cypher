//! Tool summaries (pure): chip labels and tool-group summaries.

/// Collapse model-generated text onto ONE line for single-line surfaces (tool
/// chips, titles, previews): newlines, tabs and runs of whitespace become
/// single spaces, trimmed.
///
/// Both viewports need this for the same reason from opposite directions — gpui
/// breaks on a literal `\n` before its ellipsis logic, and a terminal cell grid
/// would take an embedded newline as a cursor move.
pub fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Pi's `codemode` tool: instead of calling tools one at a time, the model
/// writes a JavaScript script that calls them (`await tools.read({…})`). Pi
/// runs each call the script makes as a tool call of its own, with the id
/// `{script call id}/{n}`, so a viewport can list those calls under the
/// script that made them.
pub const CODEMODE_TOOL: &str = "codemode";

/// Pi's `tool_search`: finds tools the model was not shown up front (MCP
/// tools with `deferred` exposure) and loads the matches.
pub const TOOL_SEARCH_TOOL: &str = "tool_search";

/// The script of a `codemode` call, when the transcript kept it.
pub fn codemode_script(call: &crate::ToolCall) -> Option<&str> {
    match call {
        crate::ToolCall::Unknown { name, input } if name == CODEMODE_TOOL => {
            input.as_ref()?.get("code")?.as_str()
        }
        _ => None,
    }
}

/// The tool part of a Pi MCP tool name (`mcp__{server}__{tool}`). A script
/// names tools by identifier, which only carries Pi's sanitized server
/// (`mvp-lab` reads `mvp_lab`); the nested MCP chips under the script name
/// the server as configured, so the summary leaves it to them.
fn mcp_tool_label(name: &str) -> Option<String> {
    let (server, tool) = name.strip_prefix("mcp__")?.split_once("__")?;
    (!server.is_empty() && !tool.is_empty()).then(|| tool.to_owned())
}

/// The tools a script calls, in order of first use: `tools.read(…)` and
/// `tools["read"](…)`, an MCP tool by its tool name. Read off the source, so
/// a call the script only makes on some branch is listed too — this names
/// what the script is about, the nested chips under it say what actually ran.
pub fn script_tools(code: &str) -> Vec<String> {
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let bytes = code.as_bytes();
    let mut names: Vec<String> = Vec::new();
    let mut from = 0;
    while let Some(at) = code[from..].find("tools") {
        let start = from + at;
        let end = start + "tools".len();
        from = end;
        // `ALL_TOOLS`, `myTools.x` and `x.tools.y` are not the global.
        if start > 0 && (ident(bytes[start - 1]) || bytes[start - 1] == b'.') {
            continue;
        }
        let rest = &code[end..];
        let name = if let Some(after) = rest.strip_prefix('.') {
            &after[..after.bytes().take_while(|b| ident(*b)).count()]
        } else if let Some(after) = rest.strip_prefix('[') {
            match after.chars().next() {
                Some(quote @ ('"' | '\'' | '`')) => {
                    let inner = &after[1..];
                    inner.find(quote).map_or("", |len| &inner[..len])
                }
                _ => "",
            }
        } else {
            ""
        };
        if name.is_empty() {
            continue;
        }
        let shown = mcp_tool_label(name).unwrap_or_else(|| name.to_owned());
        if !names.contains(&shown) {
            names.push(shown);
        }
    }
    names
}

/// A script chip's one-line detail: the tools it calls, else its first line
/// of code (the `// @options:` header is configuration, not content).
fn script_summary(code: &str) -> String {
    let tools = script_tools(code);
    if !tools.is_empty() {
        return tools.join(", ");
    }
    code.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("// @options:"))
        .unwrap_or_default()
        .to_owned()
}

/// Per-kind chip label + one-line detail. Labels match zeron's `describeTool`
/// (tool-chip.tsx) exactly, so the two viewports name a tool identically.
pub fn tool_chip_content(call: &crate::ToolCall) -> (&'static str, String) {
    let (label, detail) = tool_chip_content_raw(call);
    (label, single_line(&detail))
}

fn tool_chip_content_raw(call: &crate::ToolCall) -> (&'static str, String) {
    use crate::ToolCall;
    match call {
        ToolCall::Exec { command } => ("Run", command.clone()),
        ToolCall::ReadFile { path } => ("Read", path.clone()),
        ToolCall::WriteFile { path, .. } => ("Write", path.clone()),
        ToolCall::EditFile { path, .. } => ("Edit", path.clone()),
        ToolCall::ApplyPatch { path } => {
            ("Patch", path.clone().unwrap_or_else(|| "workspace".into()))
        }
        ToolCall::Search { pattern, path } => (
            "Search",
            match path {
                Some(path) => format!("{pattern} in {path}"),
                None => pattern.clone(),
            },
        ),
        ToolCall::Glob { pattern } => ("Glob", pattern.clone()),
        ToolCall::WebFetch { url, .. } => ("Fetch", url.clone()),
        ToolCall::WebSearch { query } => ("Web", query.clone()),
        ToolCall::Todo { items } => {
            let done = items.iter().filter(|i| i.done).count();
            ("Todo", format!("{done}/{} done", items.len()))
        }
        ToolCall::Mcp { server, tool, .. } => ("MCP", format!("{server} · {tool}")),
        ToolCall::Unknown { name, .. } if name == CODEMODE_TOOL => (
            "Script",
            codemode_script(call)
                .map(script_summary)
                .unwrap_or_default(),
        ),
        ToolCall::Unknown { name, input } if name == TOOL_SEARCH_TOOL => (
            "Find tools",
            input
                .as_ref()
                .and_then(|input| input.get("query"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
        ToolCall::Unknown { name, .. } => ("Tool", name.clone()),
    }
}

/// The privacy-safe info a subagent panel needs from a doc tool part's call
/// (the pi `subagent` tool, `extensions/subagents`): agent name, task text
/// (≤500 chars after the doc-side sanitize), and whether it was launched
/// async. `None` for any other call, or a subagent call with no agent.
///
/// Shared by the transcript chip (`tool_chip_content`) and the session-level
/// subagent panel (`cypher-ui::subagents`), so both surfaces read the same
/// fields from the same source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentCallInfo {
    pub agent: String,
    pub task: String,
    pub is_async: bool,
}

pub fn subagent_call_info(call: &crate::ToolCall) -> Option<SubagentCallInfo> {
    let crate::ToolCall::Unknown { name, input } = call else {
        return None;
    };
    if name != "subagent" {
        return None;
    }
    let args = input.as_ref().and_then(serde_json::Value::as_object)?;
    let agent = args.get("agent").and_then(serde_json::Value::as_str)?;
    Some(SubagentCallInfo {
        agent: agent.to_owned(),
        task: args
            .get("task")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        is_async: args
            .get("async")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// The ToolGroup summary line — "Ran 3 commands · edited 2 files".
///
/// Takes `(call, is_error)` pairs so each viewport can keep its own row model;
/// the summary itself is one implementation for both.
pub fn tool_group_summary(tools: &[(crate::ToolCall, bool)]) -> String {
    work_summary(tools, 0)
}

/// The summary of a run of work that mixed tool calls with thinking — the
/// tool group's line with the thoughts counted before any failures:
/// "Ran 3 commands · 2 thoughts · 1 failed".
pub fn work_summary(tools: &[(crate::ToolCall, bool)], thoughts: usize) -> String {
    use crate::ToolCall;
    let mut commands = 0usize;
    let mut edited: Vec<&str> = Vec::new();
    let mut reads = 0usize;
    let mut searches = 0usize;
    let mut fetches = 0usize;
    let mut todos = 0usize;
    let mut scripts = 0usize;
    let mut other = 0usize;
    let mut failed = 0usize;
    for (call, is_error) in tools {
        if *is_error {
            failed += 1;
        }
        match call {
            ToolCall::Exec { .. } => commands += 1,
            ToolCall::WriteFile { path, .. } | ToolCall::EditFile { path, .. } => {
                if !edited.contains(&path.as_str()) {
                    edited.push(path);
                }
            }
            ToolCall::ApplyPatch { path } => {
                let p = path.as_deref().unwrap_or("patch");
                if !edited.contains(&p) {
                    edited.push(p);
                }
            }
            ToolCall::ReadFile { .. } => reads += 1,
            ToolCall::Search { .. } | ToolCall::Glob { .. } | ToolCall::WebSearch { .. } => {
                searches += 1
            }
            ToolCall::WebFetch { .. } => fetches += 1,
            ToolCall::Todo { .. } => todos += 1,
            ToolCall::Unknown { name, .. } if name == CODEMODE_TOOL => scripts += 1,
            ToolCall::Unknown { name, .. } if name == TOOL_SEARCH_TOOL => searches += 1,
            ToolCall::Mcp { .. } | ToolCall::Unknown { .. } => other += 1,
        }
    }
    let mut segments: Vec<String> = Vec::new();
    // Commands and scripts share one verb: "ran 2 commands and 1 script".
    let ran: Vec<String> = [
        (commands > 0).then(|| plural(commands, "command", "commands")),
        (scripts > 0).then(|| plural(scripts, "script", "scripts")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !ran.is_empty() {
        segments.push(format!("ran {}", ran.join(" and ")));
    }
    if !edited.is_empty() {
        segments.push(format!("edited {}", plural(edited.len(), "file", "files")));
    }
    if reads > 0 {
        segments.push(format!("read {}", plural(reads, "file", "files")));
    }
    if searches > 0 {
        segments.push(format!("searched {}", plural(searches, "time", "times")));
    }
    if fetches > 0 {
        segments.push(format!("fetched {}", plural(fetches, "page", "pages")));
    }
    if todos > 0 {
        segments.push("updated todos".to_string());
    }
    if other > 0 {
        segments.push(format!("called {}", plural(other, "tool", "tools")));
    }
    if segments.is_empty() {
        segments.push(plural(tools.len(), "tool", "tools"));
    }
    if thoughts > 0 {
        segments.push(plural(thoughts, "thought", "thoughts"));
    }
    if failed > 0 {
        segments.push(format!("{failed} failed"));
    }
    let mut summary = segments.join(" · ");
    // Capitalize the first segment only (zeron's style).
    if let Some(first) = summary.get(0..1) {
        let upper = first.to_uppercase();
        summary.replace_range(0..1, &upper);
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolCall;

    #[test]
    fn subagent_chips_use_the_generic_tool_label() {
        // The subagent tool is a generic Unknown in the transcript: it labels
        // as a plain "Tool" chip (no special agent/task head in the feed —
        // that surfaced live-card detail moved to the session-level subagent
        // panel).
        let call = ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(serde_json::json!({
                "agent": "planner",
                "task": "Plan the live tool card\n(step by step)",
                "cwd": "/repo",
                "async": false,
            })),
        };
        assert_eq!(tool_chip_content(&call), ("Tool", "subagent".to_string()));
        // Missing/blank args keep the same generic label.
        let call = ToolCall::Unknown {
            name: "subagent".into(),
            input: None,
        };
        assert_eq!(tool_chip_content(&call), ("Tool", "subagent".to_string()));
        // Other Unknown tools keep the generic "Tool" label too.
        let call = ToolCall::Unknown {
            name: "send_message".into(),
            input: None,
        };
        assert_eq!(
            tool_chip_content(&call),
            ("Tool", "send_message".to_string())
        );
    }

    fn script(code: &str) -> ToolCall {
        ToolCall::Unknown {
            name: CODEMODE_TOOL.into(),
            input: Some(serde_json::json!({ "code": code })),
        }
    }

    #[test]
    fn script_chips_name_the_tools_the_script_calls() {
        let call = script(
            "const [a, b] = await Promise.all([\n  tools.read({ path: 'x' }),\n  tools.bash({ command: 'ls' }),\n]);\nawait tools.read({ path: 'y' });",
        );
        assert_eq!(
            tool_chip_content(&call),
            ("Script", "read, bash".to_string())
        );
        // Bracket access, and MCP tools by their tool name.
        let call = script(
            "await tools[\"my-tool\"]({});\nawait tools.mcp__mvp_lab_discord__search({ q: 1 });",
        );
        assert_eq!(
            tool_chip_content(&call),
            ("Script", "my-tool, search".to_string())
        );
        // Lookalikes are not the `tools` global.
        assert!(
            script_tools("ALL_TOOLS.map(t => t.name); myTools.x(); a.tools.y(); tools.").is_empty()
        );
        // No tool calls: the first line of code that is not the options header.
        let call = script("// @options: {\"timeout_ms\": 1000}\n\nreturn 6 * 7;");
        assert_eq!(
            tool_chip_content(&call),
            ("Script", "return 6 * 7;".to_string())
        );
        // A script the doc did not keep still labels as a script.
        let call = ToolCall::Unknown {
            name: CODEMODE_TOOL.into(),
            input: None,
        };
        assert_eq!(tool_chip_content(&call), ("Script", String::new()));
        assert_eq!(codemode_script(&call), None);
    }

    #[test]
    fn tool_search_chips_show_their_query() {
        let call = ToolCall::Unknown {
            name: TOOL_SEARCH_TOOL.into(),
            input: Some(serde_json::json!({ "query": "discord messages" })),
        };
        assert_eq!(
            tool_chip_content(&call),
            ("Find tools", "discord messages".to_string())
        );
    }

    #[test]
    fn group_summaries_count_scripts_with_commands() {
        let exec = ToolCall::Exec {
            command: "ls".into(),
        };
        let read = ToolCall::ReadFile { path: "a".into() };
        assert_eq!(
            tool_group_summary(&[(script(""), false), (read.clone(), false)]),
            "Ran 1 script · read 1 file"
        );
        assert_eq!(
            tool_group_summary(&[
                (script(""), false),
                (exec.clone(), false),
                (exec, true),
                (read, false),
            ]),
            "Ran 2 commands and 1 script · read 1 file · 1 failed"
        );
        let search = ToolCall::Unknown {
            name: TOOL_SEARCH_TOOL.into(),
            input: None,
        };
        assert_eq!(
            tool_group_summary(&[(search.clone(), false)]),
            "Searched 1 time"
        );
        assert_eq!(
            work_summary(&[(search.clone(), false), (search, true)], 2),
            "Searched 2 times · 2 thoughts · 1 failed"
        );
    }

    #[test]
    fn subagent_call_info_extracts_agent_task_and_async() {
        // Complete: agent/task/async all present.
        let call = ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(serde_json::json!({
                "agent": "planner",
                "task": "Plan the panel",
                "async": true,
                "cwd": "/secret",
            })),
        };
        let info = subagent_call_info(&call).expect("extracts");
        assert_eq!(info.agent, "planner");
        assert_eq!(info.task, "Plan the panel");
        assert!(info.is_async);
        // Missing async defaults to sync (the sync tool call omitted it).
        let call = ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(serde_json::json!({ "agent": "actor", "task": "T" })),
        };
        let info = subagent_call_info(&call).expect("extracts");
        assert!(!info.is_async);
        // Missing task defaults to empty.
        let call = ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(serde_json::json!({ "agent": "actor" })),
        };
        assert_eq!(subagent_call_info(&call).unwrap().task, "");
        // Missing agent → None.
        let call = ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(serde_json::json!({ "task": "T" })),
        };
        assert!(subagent_call_info(&call).is_none());
        // Non-subagent calls (typed or other Unknown) → None.
        assert!(
            subagent_call_info(&ToolCall::Exec {
                command: "ls".into()
            })
            .is_none()
        );
        assert!(
            subagent_call_info(&ToolCall::Unknown {
                name: "send_message".into(),
                input: None,
            })
            .is_none()
        );
    }
}
