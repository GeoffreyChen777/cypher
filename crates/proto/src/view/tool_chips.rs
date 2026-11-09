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
pub(crate) fn script_tools(code: &str) -> Vec<String> {
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

/// An agent bot's own tools (`crate::BOT_TOOLS`, `BOT_WORK_TOOLS`,
/// `BOT_SCHEDULE_TOOLS`, `BOT_ACTIVITY_TOOLS`, `BOT_SHORTCUT_TOOLS`, `BOT_NETWORK_TOOLS`) and the memory tools every run
/// gets (`crate::MEMORY_TOOLS`), by what they do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotToolKind {
    ListProjects,
    ListSessions,
    SessionActivity,
    SearchSessions,
    ReadSession,
    ProjectGit,
    ProposePlan,
    ListWork,
    GetWork,
    StopWork,
    CreateWorktree,
    StartSession,
    WaitForSession,
    AnswerSession,
    SteerSession,
    StopSession,
    UpdateStep,
    FinishWork,
    ListRoutines,
    SaveRoutine,
    RunRoutine,
    DeleteRoutine,
    SaveProjectTask,
    ListActivity,
    ListShortcuts,
    SaveShortcut,
    DeleteShortcut,
    ListConnections,
    AskBot,
    /// `memory` by its command: `view`, `search`, `create`, `str_replace`
    /// and `insert`, `delete`, `rename`; `Memory` while the command is not
    /// known yet (still streaming) or is one this build does not name.
    ReadMemory,
    SearchMemory,
    CreateMemory,
    UpdateMemory,
    DeleteMemory,
    RenameMemory,
    Memory,
    SearchHistory,
    ReadHistory,
}

impl BotToolKind {
    /// Whether the call acted (started, steered, stopped, wrote) rather than
    /// read.
    pub fn acts(self) -> bool {
        !matches!(
            self,
            Self::ListProjects
                | Self::ListSessions
                | Self::SessionActivity
                | Self::SearchSessions
                | Self::ReadSession
                | Self::ProjectGit
                | Self::ListWork
                | Self::GetWork
                | Self::ListRoutines
                | Self::ListActivity
                | Self::ListShortcuts
                | Self::ListConnections
                | Self::ReadMemory
                | Self::SearchMemory
                | Self::Memory
                | Self::SearchHistory
                | Self::ReadHistory
        )
    }
}

/// The bot tool a call is, when it is one.
pub fn bot_tool_kind(call: &crate::ToolCall) -> Option<BotToolKind> {
    let crate::ToolCall::Unknown { name, input } = call else {
        return None;
    };
    Some(match name.as_str() {
        "list_projects" => BotToolKind::ListProjects,
        "list_sessions" => BotToolKind::ListSessions,
        "session_activity" => BotToolKind::SessionActivity,
        "search_sessions" => BotToolKind::SearchSessions,
        "read_session" => BotToolKind::ReadSession,
        "project_git" => BotToolKind::ProjectGit,
        "propose_plan" => BotToolKind::ProposePlan,
        "list_work" => BotToolKind::ListWork,
        "get_work" => BotToolKind::GetWork,
        "stop_work" => BotToolKind::StopWork,
        "create_worktree" => BotToolKind::CreateWorktree,
        "start_session" => BotToolKind::StartSession,
        "wait_for_session" => BotToolKind::WaitForSession,
        "answer_session" => BotToolKind::AnswerSession,
        "steer_session" => BotToolKind::SteerSession,
        "stop_session" => BotToolKind::StopSession,
        "update_step" => BotToolKind::UpdateStep,
        "finish_work" => BotToolKind::FinishWork,
        // `list_schedule` is its name before routines were one kind.
        "list_routines" | "list_schedule" => BotToolKind::ListRoutines,
        "save_routine" => BotToolKind::SaveRoutine,
        "run_routine" => BotToolKind::RunRoutine,
        "delete_routine" => BotToolKind::DeleteRoutine,
        "list_activity" => BotToolKind::ListActivity,
        "save_project_task" => BotToolKind::SaveProjectTask,
        "list_shortcuts" => BotToolKind::ListShortcuts,
        "save_shortcut" => BotToolKind::SaveShortcut,
        "delete_shortcut" => BotToolKind::DeleteShortcut,
        "list_connections" => BotToolKind::ListConnections,
        "ask_bot" => BotToolKind::AskBot,
        "memory" => match input
            .as_ref()
            .and_then(|input| input.get("command"))
            .and_then(serde_json::Value::as_str)
        {
            Some("view") => BotToolKind::ReadMemory,
            Some("search") => BotToolKind::SearchMemory,
            Some("create") => BotToolKind::CreateMemory,
            Some("str_replace" | "insert") => BotToolKind::UpdateMemory,
            Some("delete") => BotToolKind::DeleteMemory,
            Some("rename") => BotToolKind::RenameMemory,
            _ => BotToolKind::Memory,
        },
        "search_history" => BotToolKind::SearchHistory,
        "read_history" => BotToolKind::ReadHistory,
        _ => return None,
    })
}

/// A bot tool chip's label and detail: "Search sessions" · `"invoice" in
/// Billing`. A read names the session by id; viewports that know the title
/// show that instead.
fn bot_chip_content(
    kind: BotToolKind,
    input: Option<&serde_json::Value>,
) -> (&'static str, String) {
    let arg = |key: &str| {
        input
            .and_then(|input| input.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let in_project = |text: String| match arg("project") {
        Some(project) if text.is_empty() => format!("in {project}"),
        Some(project) => format!("{text} in {project}"),
        None => text,
    };
    let quoted = |key: &str| arg(key).map(|q| format!("\"{q}\"")).unwrap_or_default();
    // A memory path names its file: `/project/testing.md` reads as
    // `testing.md`.
    let file = |key: &str| {
        arg(key)
            .map(|path| {
                let path = path.trim_end_matches('/');
                path.rsplit('/').next().unwrap_or(path).to_string()
            })
            .unwrap_or_default()
    };
    let since_hours = input
        .and_then(|input| input.get("since_hours"))
        .and_then(serde_json::Value::as_f64)
        .filter(|h| h.is_finite() && *h > 0.0);
    let last = |hours: f64| {
        if hours >= 48.0 && hours % 24.0 == 0.0 {
            format!("last {}d", (hours / 24.0) as u64)
        } else {
            format!("last {}h", hours.ceil() as u64)
        }
    };
    match kind {
        BotToolKind::ListProjects => ("List projects", String::new()),
        BotToolKind::ListSessions => {
            let mut filters: Vec<String> = Vec::new();
            if arg("status").as_deref() == Some("active") {
                filters.push("active".into());
            }
            if let Some(hours) = since_hours {
                filters.push(last(hours));
            }
            let filters = if filters.is_empty() && arg("project").is_none() {
                "all".to_string()
            } else {
                filters.join(", ")
            };
            ("List sessions", in_project(filters))
        }
        BotToolKind::SearchSessions => ("Search sessions", in_project(quoted("query"))),
        BotToolKind::SessionActivity => (
            "Checked recent activity",
            // The tool's default window is a day.
            in_project(last(since_hours.unwrap_or(24.0).min(336.0))),
        ),
        BotToolKind::ReadSession => ("Read session", arg("session_id").unwrap_or_default()),
        BotToolKind::ProjectGit => ("Read git state", arg("project").unwrap_or_default()),
        BotToolKind::ProposePlan => ("Proposed a plan", arg("title").unwrap_or_default()),
        BotToolKind::ListWork => ("Checked its tasks", String::new()),
        BotToolKind::GetWork => ("Read task", arg("work_id").unwrap_or_default()),
        BotToolKind::StopWork => ("Stopped task", String::new()),
        BotToolKind::CreateWorktree => (
            "Created worktree",
            in_project(arg("branch").unwrap_or_default()),
        ),
        BotToolKind::StartSession => (
            "Started session",
            in_project(arg("title").unwrap_or_default()),
        ),
        BotToolKind::WaitForSession => ("Waited on session", arg("session_id").unwrap_or_default()),
        BotToolKind::AnswerSession => ("Answered session", arg("session_id").unwrap_or_default()),
        BotToolKind::SteerSession => ("Steered session", arg("session_id").unwrap_or_default()),
        BotToolKind::StopSession => ("Stopped session", arg("session_id").unwrap_or_default()),
        BotToolKind::UpdateStep => ("Updated step", arg("step").unwrap_or_default()),
        BotToolKind::FinishWork => ("Finished task", arg("outcome").unwrap_or_default()),
        BotToolKind::ListRoutines => ("Checked routines", String::new()),
        BotToolKind::SaveRoutine => ("Saved routine", arg("name").unwrap_or_default()),
        BotToolKind::RunRoutine => ("Ran routine", String::new()),
        BotToolKind::DeleteRoutine => ("Deleted routine", String::new()),
        BotToolKind::ListActivity => ("Checked activity", String::new()),
        BotToolKind::SaveProjectTask => (
            "Saved project task",
            in_project(arg("name").unwrap_or_default()),
        ),
        BotToolKind::ListShortcuts => ("Checked shortcuts", String::new()),
        BotToolKind::SaveShortcut => ("Saved a shortcut", arg("label").unwrap_or_default()),
        BotToolKind::DeleteShortcut => ("Removed a shortcut", String::new()),
        BotToolKind::ListConnections => ("Checked connections", String::new()),
        BotToolKind::AskBot => ("Asked workspace", arg("bot").unwrap_or_default()),
        BotToolKind::ReadMemory => ("Read memory", file("path")),
        BotToolKind::SearchMemory => ("Searched memory", quoted("query")),
        BotToolKind::CreateMemory => ("Created memory", file("path")),
        BotToolKind::UpdateMemory => ("Updated memory", file("path")),
        BotToolKind::DeleteMemory => ("Deleted memory", file("path")),
        BotToolKind::RenameMemory => (
            "Renamed memory",
            match (file("old_path"), file("new_path")) {
                (old, new) if old.is_empty() || new.is_empty() => format!("{old}{new}"),
                (old, new) => format!("{old} → {new}"),
            },
        ),
        BotToolKind::Memory => ("Memory", file("path")),
        BotToolKind::SearchHistory => ("Searched history", quoted("query")),
        BotToolKind::ReadHistory => ("Read a past session", String::new()),
    }
}

/// What a bot's acting tool did, for a turn's summary line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BotAction {
    Plan,
    Worktree,
    Session,
    Answer,
    Steer,
    Stop,
    Step,
    Finish,
    Memory,
    Routine,
    RunRoutine,
    DeleteRoutine,
    StopWork,
    Task,
    Shortcut,
    Unshortcut,
    Ask,
}

impl BotAction {
    fn phrase(self, n: usize) -> String {
        let (one, many): (&str, &str) = match self {
            Self::Plan => ("proposed a plan", "proposed {n} plans"),
            Self::Worktree => ("created a worktree", "created {n} worktrees"),
            Self::Session => ("started a session", "started {n} sessions"),
            Self::Answer => ("answered a session", "answered {n} questions"),
            Self::Steer => ("steered a session", "steered sessions {n} times"),
            Self::Stop => ("stopped a session", "stopped {n} sessions"),
            Self::Step => ("updated a step", "updated {n} steps"),
            Self::Finish => ("finished the task", "finished {n} tasks"),
            Self::Memory => ("updated memory", "updated memory {n} times"),
            Self::Routine => ("saved a routine", "saved {n} routines"),
            Self::RunRoutine => ("ran a routine", "ran {n} routines"),
            Self::DeleteRoutine => ("deleted a routine", "deleted {n} routines"),
            Self::StopWork => ("stopped work", "stopped {n} pieces of work"),
            Self::Task => ("saved a project task", "saved {n} project tasks"),
            Self::Shortcut => ("saved a shortcut", "saved {n} shortcuts"),
            Self::Unshortcut => ("removed a shortcut", "removed {n} shortcuts"),
            Self::Ask => ("asked a workspace", "asked workspaces {n} times"),
        };
        if n == 1 {
            one.to_string()
        } else {
            many.replace("{n}", &n.to_string())
        }
    }
}

fn bot_action(kind: BotToolKind) -> Option<BotAction> {
    Some(match kind {
        BotToolKind::ProposePlan => BotAction::Plan,
        BotToolKind::CreateWorktree => BotAction::Worktree,
        BotToolKind::StartSession => BotAction::Session,
        BotToolKind::AnswerSession => BotAction::Answer,
        BotToolKind::SteerSession => BotAction::Steer,
        BotToolKind::StopSession => BotAction::Stop,
        BotToolKind::UpdateStep => BotAction::Step,
        BotToolKind::FinishWork => BotAction::Finish,
        BotToolKind::CreateMemory
        | BotToolKind::UpdateMemory
        | BotToolKind::DeleteMemory
        | BotToolKind::RenameMemory => BotAction::Memory,
        BotToolKind::SaveRoutine => BotAction::Routine,
        BotToolKind::RunRoutine => BotAction::RunRoutine,
        BotToolKind::DeleteRoutine => BotAction::DeleteRoutine,
        BotToolKind::StopWork => BotAction::StopWork,
        BotToolKind::SaveProjectTask => BotAction::Task,
        BotToolKind::SaveShortcut => BotAction::Shortcut,
        BotToolKind::DeleteShortcut => BotAction::Unshortcut,
        BotToolKind::AskBot => BotAction::Ask,
        _ => return None,
    })
}

/// Per-kind chip label + one-line detail. Labels match zeron's `describeTool`
/// (tool-chip.tsx) exactly, so the two viewports name a tool identically.
pub fn tool_chip_content(call: &crate::ToolCall) -> (&'static str, String) {
    let (label, detail) = tool_chip_content_raw(call);
    (label, single_line(&detail))
}

fn tool_chip_content_raw(call: &crate::ToolCall) -> (&'static str, String) {
    use crate::ToolCall;
    if let (Some(kind), ToolCall::Unknown { input, .. }) = (bot_tool_kind(call), call) {
        return bot_chip_content(kind, input.as_ref());
    }
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
    let mut listings = 0usize;
    let mut sessions_read = 0usize;
    let mut memory_reads = 0usize;
    let mut git_reads = 0usize;
    let mut other = 0usize;
    let mut acted: std::collections::BTreeMap<BotAction, usize> = Default::default();
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
            ToolCall::Unknown { .. } if let Some(kind) = bot_tool_kind(call) => match kind {
                BotToolKind::SearchSessions
                | BotToolKind::SearchMemory
                | BotToolKind::SearchHistory => searches += 1,
                BotToolKind::ReadSession | BotToolKind::ReadHistory => sessions_read += 1,
                BotToolKind::ReadMemory | BotToolKind::Memory => memory_reads += 1,
                BotToolKind::ListProjects
                | BotToolKind::ListSessions
                | BotToolKind::SessionActivity => listings += 1,
                BotToolKind::ProjectGit => git_reads += 1,
                BotToolKind::ListWork
                | BotToolKind::GetWork
                | BotToolKind::ListRoutines
                | BotToolKind::ListActivity
                | BotToolKind::ListShortcuts
                | BotToolKind::ListConnections => listings += 1,
                kind => match bot_action(kind) {
                    Some(verb) => *acted.entry(verb).or_default() += 1,
                    None => other += 1,
                },
            },
            ToolCall::Mcp { .. } | ToolCall::Unknown { .. } => other += 1,
        }
    }
    let mut segments: Vec<String> = Vec::new();
    // A bot's acts read as what it did: "proposed a plan", "started 2
    // sessions", in the order of the vocabulary.
    for (verb, count) in &acted {
        segments.push(verb.phrase(*count));
    }
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
    if listings > 0 {
        segments.push("looked over your work".to_string());
    }
    if reads > 0 {
        segments.push(format!("read {}", plural(reads, "file", "files")));
    }
    if memory_reads > 0 {
        segments.push("read memory".to_string());
    }
    if sessions_read > 0 {
        segments.push(format!(
            "read {}",
            plural(sessions_read, "session", "sessions")
        ));
    }
    if git_reads > 0 {
        segments.push(format!(
            "read git state of {}",
            plural(git_reads, "project", "projects")
        ));
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

    fn bot_call(name: &str, input: serde_json::Value) -> ToolCall {
        ToolCall::Unknown {
            name: name.into(),
            input: Some(input),
        }
    }

    #[test]
    fn a_bots_acts_read_as_what_it_did() {
        let call = |name: &str| {
            (
                crate::ToolCall::Unknown {
                    name: name.into(),
                    input: Some(serde_json::json!({})),
                },
                false,
            )
        };
        assert_eq!(
            work_summary(&[call("propose_plan"), call("read_session")], 0),
            "Proposed a plan · read 1 session"
        );
        let memory = |command: &str| {
            (
                bot_call(
                    "memory",
                    serde_json::json!({"command": command, "path": "/project/testing.md"}),
                ),
                false,
            )
        };
        assert_eq!(
            work_summary(
                &[
                    memory("create"),
                    memory("str_replace"),
                    call("save_routine")
                ],
                1
            ),
            "Updated memory 2 times · saved a routine · 1 thought"
        );
        assert_eq!(
            work_summary(
                &[
                    memory("view"),
                    memory("view"),
                    memory("search"),
                    call("read_history")
                ],
                0
            ),
            "Read memory · read 1 session · searched 1 time"
        );
    }

    #[test]
    fn memory_tools_name_the_file() {
        let chip = |name, input| tool_chip_content(&bot_call(name, input));
        let memory = |command: &str, path: &str| {
            chip(
                "memory",
                serde_json::json!({"command": command, "path": path}),
            )
        };
        assert_eq!(
            memory("view", "/you/MEMORY.md"),
            ("Read memory", "MEMORY.md".to_string())
        );
        assert_eq!(
            memory("str_replace", "/project/testing.md"),
            ("Updated memory", "testing.md".to_string())
        );
        assert_eq!(
            memory("insert", "/workspace/projects/invoice-demo.md"),
            ("Updated memory", "invoice-demo.md".to_string())
        );
        assert_eq!(
            memory("create", "/project/build.md"),
            ("Created memory", "build.md".to_string())
        );
        assert_eq!(
            memory("delete", "/project/old.md"),
            ("Deleted memory", "old.md".to_string())
        );
        assert_eq!(
            memory("view", "/project/"),
            ("Read memory", "project".to_string())
        );
        assert_eq!(
            chip(
                "memory",
                serde_json::json!({
                    "command": "rename",
                    "old_path": "/you/a.md",
                    "new_path": "/you/notes/b.md",
                })
            ),
            ("Renamed memory", "a.md → b.md".to_string())
        );
        assert_eq!(
            chip(
                "memory",
                serde_json::json!({"command": "search", "query": " rounding "})
            ),
            ("Searched memory", "\"rounding\"".to_string())
        );
        assert_eq!(
            chip("memory", serde_json::json!({})),
            ("Memory", String::new()),
            "a call still streaming its arguments"
        );
        assert_eq!(
            chip("search_history", serde_json::json!({"query": "flaky test"})),
            ("Searched history", "\"flaky test\"".to_string())
        );
        assert_eq!(
            chip("read_history", serde_json::json!({"session_id": "c1"})),
            ("Read a past session", String::new())
        );
        let acts = |command: &str| {
            bot_tool_kind(&bot_call("memory", serde_json::json!({"command": command})))
                .is_some_and(BotToolKind::acts)
        };
        assert!(acts("create") && acts("str_replace") && acts("rename"));
        assert!(!acts("view") && !acts("search"));
        for retired in ["read_notes", "write_note", "delete_note"] {
            assert_eq!(
                bot_tool_kind(&bot_call(retired, serde_json::json!({}))),
                None,
                "{retired}"
            );
        }
    }

    #[test]
    fn bot_tools_read_as_what_they_do() {
        let chip = |name, input| tool_chip_content(&bot_call(name, input));
        assert_eq!(
            chip("list_projects", serde_json::json!({})),
            ("List projects", String::new())
        );
        for name in ["list_routines", "list_schedule"] {
            assert_eq!(
                chip(name, serde_json::json!({})),
                ("Checked routines", String::new()),
                "{name}: old transcripts keep their chip"
            );
        }
        assert_eq!(
            chip(
                "list_sessions",
                serde_json::json!({"project": "Billing", "status": "active", "since_hours": 48})
            ),
            ("List sessions", "active, last 2d in Billing".to_string())
        );
        assert_eq!(
            chip("list_sessions", serde_json::json!({"since_hours": 0.5})),
            ("List sessions", "last 1h".to_string())
        );
        assert_eq!(
            chip("list_sessions", serde_json::json!({})),
            ("List sessions", "all".to_string())
        );
        assert_eq!(
            chip("list_sessions", serde_json::json!({"project": "Web"})),
            ("List sessions", "in Web".to_string())
        );
        assert_eq!(
            chip("search_sessions", serde_json::json!({"query": " invoice "})),
            ("Search sessions", "\"invoice\"".to_string())
        );
        assert_eq!(
            chip("read_session", serde_json::json!({"session_id": "c1"})),
            ("Read session", "c1".to_string())
        );
        assert_eq!(
            chip("session_activity", serde_json::json!({})),
            ("Checked recent activity", "last 24h".to_string())
        );
        assert_eq!(
            chip(
                "session_activity",
                serde_json::json!({"since_hours": 72, "project": "Billing"})
            ),
            ("Checked recent activity", "last 3d in Billing".to_string())
        );
        assert_eq!(
            chip("project_git", serde_json::json!({"project": " Billing "})),
            ("Read git state", "Billing".to_string())
        );
        let summary = tool_group_summary(&[
            (bot_call("list_sessions", serde_json::json!({})), false),
            (bot_call("search_sessions", serde_json::json!({})), false),
            (bot_call("read_session", serde_json::json!({})), false),
            (bot_call("read_session", serde_json::json!({})), true),
        ]);
        assert_eq!(
            summary,
            "Looked over your work · read 2 sessions · searched 1 time · 1 failed"
        );
        let summary = tool_group_summary(&[
            (bot_call("session_activity", serde_json::json!({})), false),
            (bot_call("project_git", serde_json::json!({})), false),
        ]);
        assert_eq!(
            summary,
            "Looked over your work · read git state of 1 project"
        );
    }

    #[test]
    fn shortcut_tools_name_the_button() {
        let chip = |name, input| tool_chip_content(&bot_call(name, input));
        assert_eq!(
            chip("list_shortcuts", serde_json::json!({})),
            ("Checked shortcuts", String::new())
        );
        assert_eq!(
            chip(
                "save_shortcut",
                serde_json::json!({"label": " Fix tests ", "icon": "bug", "request": "Fix {input}"})
            ),
            ("Saved a shortcut", "Fix tests".to_string())
        );
        assert_eq!(
            chip("delete_shortcut", serde_json::json!({"id": "s1"})),
            ("Removed a shortcut", String::new())
        );
        let kind = |name| bot_tool_kind(&bot_call(name, serde_json::json!({}))).unwrap();
        assert!(!kind("list_shortcuts").acts());
        assert!(kind("save_shortcut").acts() && kind("delete_shortcut").acts());
        let call = |name: &str| (bot_call(name, serde_json::json!({})), false);
        assert_eq!(
            work_summary(
                &[
                    call("list_shortcuts"),
                    call("save_shortcut"),
                    call("save_shortcut"),
                    call("delete_shortcut")
                ],
                0
            ),
            "Saved 2 shortcuts · removed a shortcut · looked over your work"
        );
    }

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
