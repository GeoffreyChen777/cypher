//! The `BotTool` RPC: the read tools a bot's Pi run calls.
//!
//! Every call builds a [`View`] from the current registry: the calling chat
//! must be a bot chat hosted here, and the bot row's scope decides which
//! projects and sessions exist for this call. Answers are plain text the
//! model reads directly, bounded so one call cannot flood its context.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use cypher_doc::{MessagePart, MessageRole, SessionMessageEntry};
use cypher_proto::view::{Indicator, effective_indicator};
use cypher_proto::{Bot, Chat, Session, Space, ToolCall};

use super::Bots;
use super::prompt::space_name;
use crate::EngineError;
use crate::util::off_runtime;

/// Most sessions `list_sessions` returns.
const LIST_MAX: usize = 200;
const LIST_DEFAULT: usize = 50;
/// Unindexed sessions `search_sessions` scans, newest first.
pub(super) const SEARCH_SCAN_MAX: usize = 400;
const SEARCH_DEFAULT: usize = 20;
const SEARCH_MAX: usize = 50;
/// Excerpts shown per matching session.
const SEARCH_EXCERPTS: usize = 3;
const EXCERPT_RADIUS: usize = 120;
const READ_DEFAULT: usize = 20;
const READ_MAX: usize = 60;
/// One message's text in `read_session`, and one whole answer.
const MESSAGE_MAX_CHARS: usize = 6_000;
const ANSWER_MAX_CHARS: usize = 48_000;
const PREVIEW_MAX_CHARS: usize = 160;
/// `session_activity`'s window, in hours, and the sessions it lists.
const ACTIVITY_DEFAULT_HOURS: f64 = 24.0;
const ACTIVITY_MAX_HOURS: f64 = 336.0;
const ACTIVITY_MAX: usize = 60;
/// How long `read_session` waits for another device's chat to catch up.
const REMOTE_CATCH_UP: Duration = Duration::from_secs(8);

/// One call's world: the bot and what its scope lets it see right now.
pub(super) struct View {
    pub bot: Bot,
    pub spaces: Vec<Space>,
    /// Visible chats, children included, newest activity first.
    pub chats: Vec<Chat>,
    pub sessions: HashMap<String, Session>,
    pub devices: HashMap<String, String>,
    pub now: DateTime<Utc>,
}

impl View {
    pub(super) fn build(
        bot: Bot,
        spaces: Vec<Space>,
        chats: Vec<Chat>,
        sessions: Vec<Session>,
        devices: Vec<cypher_proto::Device>,
        now: DateTime<Utc>,
    ) -> Self {
        let mut visible: Vec<Chat> = bot
            .scope
            .visible_chats(&chats)
            .into_iter()
            .cloned()
            .collect();
        visible.sort_by(|a, b| activity(b).cmp(&activity(a)).then(a.id.cmp(&b.id)));
        let spaces = spaces
            .into_iter()
            .filter(|s| bot.scope.sees_space(&s.id))
            .collect();
        Self {
            spaces,
            chats: visible,
            sessions: sessions
                .into_iter()
                .map(|s| (s.chat_id.clone(), s))
                .collect(),
            devices: devices.into_iter().map(|d| (d.id, d.name)).collect(),
            bot,
            now,
        }
    }

    pub(super) fn device(&self, id: &str) -> &str {
        self.devices
            .get(id)
            .map_or("an unknown device", String::as_str)
    }

    pub(super) fn space(&self, id: Option<&str>) -> Option<&Space> {
        let id = id?;
        self.spaces.iter().find(|s| s.id == id)
    }

    /// "project on device" for a chat (a child reads its root's project).
    fn place(&self, chat: &Chat) -> String {
        let root = self.root(chat);
        match self.space(root.space_id.as_deref()) {
            Some(space) => format!("{} on {}", space_name(space), self.device(&space.device_id)),
            None => format!("no project, on {}", self.device(&chat.device_id)),
        }
    }

    fn root<'a>(&'a self, chat: &'a Chat) -> &'a Chat {
        let mut root = chat;
        for _ in 0..16 {
            match root
                .parent_chat_id()
                .and_then(|id| self.chats.iter().find(|c| c.id == id))
            {
                Some(parent) => root = parent,
                None => break,
            }
        }
        root
    }

    fn status(&self, chat: &Chat) -> &'static str {
        match effective_indicator(self.sessions.get(&chat.id), self.now) {
            Indicator::Working => "working",
            Indicator::AwaitingInput => "waiting for input",
            Indicator::Errored => "errored",
            Indicator::None => "idle",
        }
    }

    /// The spaces a `project` argument names: an id, or a name (any case).
    /// An unknown name answers like an invisible one.
    pub(super) fn resolve_project(
        &self,
        project: Option<&str>,
    ) -> Result<Option<Vec<String>>, EngineError> {
        let Some(project) = project.map(str::trim).filter(|p| !p.is_empty()) else {
            return Ok(None);
        };
        let matches: Vec<String> = self
            .spaces
            .iter()
            .filter(|s| s.id == project || space_name(s).eq_ignore_ascii_case(project))
            .map(|s| s.id.clone())
            .collect();
        if matches.is_empty() {
            return Err(EngineError::Other(format!(
                "No project \"{project}\" that you can see. Use list_projects."
            )));
        }
        Ok(Some(matches))
    }

    fn in_projects(&self, chat: &Chat, projects: &Option<Vec<String>>) -> bool {
        match projects {
            None => true,
            Some(ids) => self
                .root(chat)
                .space_id
                .as_deref()
                .is_some_and(|id| ids.iter().any(|x| x == id)),
        }
    }

    pub(super) fn header(&self, chat: &Chat) -> String {
        format!(
            "{} · \"{}\" · {} · {} · {}",
            chat.id,
            title(chat),
            self.place(chat),
            self.status(chat),
            when(activity(chat), self.now)
        )
    }
}

fn activity(chat: &Chat) -> DateTime<Utc> {
    chat.last_message_at.unwrap_or(chat.created_at)
}

fn title(chat: &Chat) -> &str {
    chat.title
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or("(untitled)")
}

/// `2026-10-14 09:12 UTC (3h ago)`.
pub(super) fn when(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - at).num_seconds().max(0);
    let ago = match secs {
        0..60 => "just now".to_string(),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    };
    format!("{} ({ago})", at.format("%Y-%m-%d %H:%M UTC"))
}

fn clip(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}… [{} more characters]", count - max)
}

fn one_line(text: &str, max: usize) -> String {
    clip(&text.split_whitespace().collect::<Vec<_>>().join(" "), max)
}

// ── arguments ───────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct ListSessions {
    project: Option<String>,
    status: Option<String>,
    since_hours: Option<f64>,
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct SessionActivity {
    since_hours: Option<f64>,
    project: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SearchSessions {
    query: String,
    project: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ReadSession {
    session_id: String,
    from: Option<usize>,
    limit: Option<usize>,
}

fn args<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, EngineError> {
    serde_json::from_value(value)
        .map_err(|err| EngineError::Other(format!("Invalid arguments: {err}")))
}

// ── the tools ───────────────────────────────────────────────────────────────

impl Bots {
    /// Answer one bot tool call from the chat `chat_id`.
    pub async fn tool(
        &self,
        chat_id: &str,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<String, EngineError> {
        // The tracks' own tools first (coordination, memory, network); each
        // checks the thread and the bot's capabilities per call (§4.4).
        if let Some((chat, marker)) = self.thread(chat_id)? {
            if let Some(answer) = self.work_tool(&chat, &marker, tool, &arguments).await {
                return answer;
            }
            if let Some(answer) = self.memory_tool(&chat, &marker, tool, &arguments).await {
                return answer;
            }
            if let Some(answer) = self.network_tool(&chat, &marker, tool, &arguments).await {
                return answer;
            }
            if let Some(answer) = self.shortcut_tool(&chat, &marker, tool, &arguments) {
                return answer;
            }
        }
        let view = self.view(chat_id)?;
        match tool {
            "list_projects" => Ok(list_projects(&view)),
            "list_sessions" => list_sessions(&view, args(arguments)?),
            "session_activity" => session_activity(&view, args(arguments)?),
            "project_git" => self.project_git(&view, args(arguments)?).await,
            "search_sessions" => self.search_sessions(view, args(arguments)?).await,
            "read_session" => self.read_session(view, args(arguments)?).await,
            other => Err(EngineError::Other(format!(
                "Unknown workspace tool: {other}"
            ))),
        }
    }

    /// The calling thread's chat row and marker: a bot thread hosted here,
    /// `None` for anything else.
    pub(super) fn thread(
        &self,
        chat_id: &str,
    ) -> Result<Option<(Chat, cypher_proto::BotChat)>, EngineError> {
        let workspace = &self.inner.workspace;
        Ok(workspace.chat(chat_id)?.and_then(|chat| {
            let marker = chat.bot.clone()?;
            (chat.device_id == workspace.device_id()).then_some((chat, marker))
        }))
    }

    /// The caller's view, refusing any chat that is not a bot thread hosted
    /// on this device or whose bot is gone.
    pub(super) fn view(&self, chat_id: &str) -> Result<View, EngineError> {
        let workspace = &self.inner.workspace;
        let not_a_bot = || EngineError::Other("This chat is not a workspace's agent.".into());
        let chat = workspace.chat(chat_id)?.ok_or_else(not_a_bot)?;
        let marker = chat
            .bot
            .as_ref()
            .filter(|m| m.is_bot_thread())
            .ok_or_else(not_a_bot)?;
        if chat.device_id != workspace.device_id() {
            return Err(not_a_bot());
        }
        let mut bot = workspace
            .bot(&marker.bot_id)?
            // The bot chat itself, or one of its hidden threads (only the
            // host writes a thread's marker).
            .filter(|bot| bot.chat_id == chat.id || !marker.is_main())
            .ok_or_else(|| EngineError::Other("This workspace has been deleted.".into()))?;
        let chats = workspace.read_chats()?;
        // A peer thread reads through the bot scope ∩ the connection scope.
        if let Some(scope) = super::network::peer_scope(workspace, &bot, marker, &chats) {
            bot.scope = scope;
        }
        Ok(View::build(
            bot,
            workspace.read_spaces()?,
            chats,
            workspace.read_sessions()?,
            workspace.read_devices()?,
            Utc::now(),
        ))
    }

    async fn search_sessions(
        &self,
        view: View,
        query: SearchSessions,
    ) -> Result<String, EngineError> {
        let words: Vec<String> = query
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        if words.is_empty() {
            return Err(EngineError::Other(
                "Give search_sessions some words to find.".into(),
            ));
        }
        let projects = view.resolve_project(query.project.as_deref())?;
        let limit = query.limit.unwrap_or(SEARCH_DEFAULT).clamp(1, SEARCH_MAX);
        let candidates: Vec<Chat> = view
            .chats
            .iter()
            .filter(|c| view.in_projects(c, &projects))
            .cloned()
            .collect();
        let transcripts = view.bot.scope.transcripts;
        let index = self.inner.index.clone();
        let docs = self.inner.doc_host.clone();
        let find_words = words.clone();
        let found = off_runtime(move || {
            find(
                &candidates,
                &find_words,
                transcripts,
                limit,
                |words, ids| index.search(words, ids),
                |id| docs.read_transcript(id),
            )
        })
        .await
        .map_err(EngineError::Other)?;

        let mut out = vec![if found.hits.is_empty() {
            format!("No sessions match \"{}\".", words.join(" "))
        } else {
            format!(
                "Sessions matching \"{}\" (best matches first):",
                words.join(" ")
            )
        }];
        for (chat, excerpts) in &found.hits {
            out.push(format!("- {}", view.header(chat)));
            for line in excerpts {
                out.push(format!("  {line}"));
            }
        }
        if !transcripts {
            out.push(
                "Only titles and previews were searched: this workspace may not read message text."
                    .into(),
            );
        }
        if found.unstored > 0 {
            out.push(format!(
                "{} session(s) have no copy on this device yet and were matched by title only.",
                found.unstored
            ));
        }
        if found.not_searched > 0 {
            out.push(format!(
                "{} older session(s) are not indexed yet, so only their titles were searched; \
                 try again shortly or narrow with `project`.",
                found.not_searched
            ));
        }
        Ok(clip(&out.join("\n"), ANSWER_MAX_CHARS))
    }

    async fn read_session(&self, view: View, request: ReadSession) -> Result<String, EngineError> {
        let id = request.session_id.trim();
        let chat = view
            .chats
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| EngineError::Other(format!("No session \"{id}\" that you can see.")))?;
        if !view.bot.scope.transcripts {
            return Err(EngineError::Other(
                "This workspace may not read message text: its owner allowed titles, previews and \
                 status only."
                    .into(),
            ));
        }
        let entries = self.transcript(&chat).await?;
        Ok(render_session(
            &view,
            &chat,
            &entries,
            request.from,
            request.limit,
            "read_session",
        ))
    }

    /// A chat's transcript, as fresh as this device can make it. A chat
    /// another device hosts is opened (joining its room) and given a moment
    /// to catch up; a local one is read from its stored doc.
    pub(super) async fn transcript(
        &self,
        chat: &Chat,
    ) -> Result<Vec<SessionMessageEntry>, EngineError> {
        let docs = self.inner.doc_host.clone();
        let hosted_here = chat.device_id == docs.device_id();
        if !hosted_here {
            let handle = docs.open(&chat.id)?;
            let deadline = tokio::time::Instant::now() + REMOTE_CATCH_UP;
            while !handle.connected() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            if handle.connected() {
                // The join imports the room's rows right after connecting.
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        let id = chat.id.clone();
        let entries = off_runtime(move || docs.read_transcript(&id))
            .await
            .map_err(EngineError::Other)??;
        match entries {
            Some(entries) => Ok(entries),
            None if hosted_here => Ok(Vec::new()),
            None => Err(EngineError::Other(
                "That session is not available on this device right now (its device may be \
                 offline)."
                    .into(),
            )),
        }
    }
}

pub(super) fn list_projects(view: &View) -> String {
    let mut rows: Vec<(DateTime<Utc>, String)> = view
        .spaces
        .iter()
        .map(|space| {
            let chats: Vec<&Chat> = view
                .chats
                .iter()
                .filter(|c| !c.is_child() && c.space_id.as_deref() == Some(space.id.as_str()))
                .collect();
            let last = chats.iter().map(|c| activity(c)).max();
            let working = chats.iter().filter(|c| view.status(c) == "working").count();
            let mut line = format!(
                "- {} (id {}) on {}: {}{} · {} session(s)",
                space_name(space),
                space.id,
                view.device(&space.device_id),
                space.path,
                if space.git_detected { " · git" } else { "" },
                chats.len()
            );
            if working > 0 {
                line.push_str(&format!(", {working} working"));
            }
            if let Some(last) = last {
                line.push_str(&format!(" · last activity {}", when(last, view.now)));
            }
            (last.unwrap_or(space.created_at), line)
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    let loose = view
        .chats
        .iter()
        .filter(|c| !c.is_child() && view.space(c.space_id.as_deref()).is_none())
        .count();
    let mut out = vec![if rows.is_empty() {
        "You can see no projects.".to_string()
    } else {
        format!("{} project(s), most recently active first:", rows.len())
    }];
    out.extend(rows.into_iter().map(|(_, line)| line));
    if loose > 0 {
        out.push(format!(
            "Plus {loose} individually shared session(s) outside these projects (list_sessions shows them)."
        ));
    }
    out.join("\n")
}

pub(super) fn list_sessions(view: &View, query: ListSessions) -> Result<String, EngineError> {
    let projects = view.resolve_project(query.project.as_deref())?;
    let active_only = match query.status.as_deref() {
        None | Some("all") => false,
        Some("active") => true,
        Some(other) => {
            return Err(EngineError::Other(format!(
                "Unknown status filter \"{other}\": use active or all."
            )));
        }
    };
    let since = query
        .since_hours
        .filter(|h| h.is_finite() && *h >= 0.0)
        .map(|h| view.now - chrono::Duration::seconds((h * 3_600.0) as i64));
    let limit = query.limit.unwrap_or(LIST_DEFAULT).clamp(1, LIST_MAX);
    let matching: Vec<&Chat> = view
        .chats
        .iter()
        .filter(|c| !c.is_child())
        .filter(|c| view.in_projects(c, &projects))
        .filter(|c| !active_only || matches!(view.status(c), "working" | "waiting for input"))
        .filter(|c| since.is_none_or(|since| activity(c) >= since))
        .collect();
    if matching.is_empty() {
        return Ok("No sessions match.".into());
    }
    let mut out = vec![format!(
        "{} session(s), newest activity first{}:",
        matching.len(),
        if matching.len() > limit {
            format!(" (showing {limit})")
        } else {
            String::new()
        }
    )];
    for chat in matching.into_iter().take(limit) {
        let children = view
            .chats
            .iter()
            .filter(|c| c.parent_chat_id() == Some(chat.id.as_str()))
            .count();
        let mut line = format!("- {}", view.header(chat));
        if children > 0 {
            line.push_str(&format!(" · {children} subagent chat(s)"));
        }
        out.push(line);
        if let Some(preview) = chat
            .last_message_preview
            .as_deref()
            .filter(|p| !p.trim().is_empty())
        {
            out.push(format!(
                "  latest: {}",
                one_line(preview, PREVIEW_MAX_CHARS)
            ));
        }
    }
    Ok(clip(&out.join("\n"), ANSWER_MAX_CHARS))
}

/// What changed in the window: sessions started, sessions with new messages
/// (with their status now and latest preview), and runs that ended. Grouped
/// by project, the most recently active project first.
pub(super) fn session_activity(view: &View, query: SessionActivity) -> Result<String, EngineError> {
    let projects = view.resolve_project(query.project.as_deref())?;
    let hours = query
        .since_hours
        .filter(|h| h.is_finite() && *h > 0.0)
        .unwrap_or(ACTIVITY_DEFAULT_HOURS)
        .min(ACTIVITY_MAX_HOURS);
    let since = view.now - chrono::Duration::seconds((hours * 3_600.0) as i64);
    let window = format!("the last {}", hours_label(hours));
    let changed: Vec<&Chat> = view
        .chats
        .iter()
        .filter(|c| !c.is_child() && view.in_projects(c, &projects))
        .filter(|c| {
            c.created_at >= since
                || activity(c) >= since
                || matches!(view.status(c), "working" | "waiting for input")
                || (view.status(c) == "errored"
                    && view
                        .sessions
                        .get(&c.id)
                        .is_some_and(|s| s.updated_at >= since))
        })
        .collect();
    if changed.is_empty() {
        return Ok(format!(
            "Nothing changed in {window} (since {}).",
            since.format("%Y-%m-%d %H:%M UTC")
        ));
    }

    // Groups keyed by the root's space; `view.chats` is newest first, so the
    // first session seen in a group is its newest.
    let mut groups: Vec<(Option<&str>, Vec<&Chat>)> = Vec::new();
    for chat in &changed {
        let key = view.space(chat.space_id.as_deref()).map(|s| s.id.as_str());
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, chats)) => chats.push(chat),
            None => groups.push((key, vec![chat])),
        }
    }
    let count = |status: &str| changed.iter().filter(|c| view.status(c) == status).count();
    let started = changed.iter().filter(|c| c.created_at >= since).count();
    let mut out = vec![format!(
        "Activity in {window} (since {}): {} session(s) in {} group(s) · {started} new · {} \
         working · {} waiting for input · {} errored. Newest first:",
        since.format("%Y-%m-%d %H:%M UTC"),
        changed.len(),
        groups.len(),
        count("working"),
        count("waiting for input"),
        count("errored"),
    )];
    let mut shown = 0usize;
    for (key, chats) in &groups {
        if shown >= ACTIVITY_MAX {
            break;
        }
        out.push(String::new());
        out.push(match key.and_then(|id| view.space(Some(id))) {
            Some(space) => format!(
                "{} (id {}) on {}:",
                space_name(space),
                space.id,
                view.device(&space.device_id)
            ),
            None => "Individually shared sessions:".to_string(),
        });
        for chat in chats.iter().take(ACTIVITY_MAX - shown) {
            shown += 1;
            let mut line = format!(
                "- {} · \"{}\" · {} · last activity {}",
                chat.id,
                title(chat),
                view.status(chat),
                when(activity(chat), view.now)
            );
            if chat.created_at >= since {
                line.push_str(&format!(
                    " · new, started {}",
                    when(chat.created_at, view.now)
                ));
            }
            let subagents = view
                .chats
                .iter()
                .filter(|c| c.parent_chat_id() == Some(chat.id.as_str()) && activity(c) >= since)
                .count();
            if subagents > 0 {
                line.push_str(&format!(" · {subagents} active subagent chat(s)"));
            }
            out.push(line);
            if let Some(preview) = chat
                .last_message_preview
                .as_deref()
                .filter(|p| !p.trim().is_empty())
            {
                out.push(format!(
                    "  latest: {}",
                    one_line(preview, PREVIEW_MAX_CHARS)
                ));
            }
            // A settled session row stamps when its last run ended; trust it
            // only when the run left a message inside the window.
            if let Some(session) = view.sessions.get(&chat.id)
                && session.updated_at >= since
                && activity(chat) >= since
            {
                match view.status(chat) {
                    "idle" => out.push(format!(
                        "  run finished {}",
                        when(session.updated_at, view.now)
                    )),
                    "errored" => out.push(format!(
                        "  run failed {}",
                        when(session.updated_at, view.now)
                    )),
                    _ => {}
                }
            }
        }
    }
    if changed.len() > shown {
        out.push(String::new());
        out.push(format!(
            "{} more session(s) not shown; narrow with `project` or a shorter `since_hours`.",
            changed.len() - shown
        ));
    }
    Ok(clip(&out.join("\n"), ANSWER_MAX_CHARS))
}

/// `24 hours`, `1.5 hours`, `14 days`.
fn hours_label(hours: f64) -> String {
    if hours >= 48.0 && hours % 24.0 == 0.0 {
        format!("{} days", (hours / 24.0) as u64)
    } else if hours == 1.0 {
        "hour".to_string()
    } else if hours.fract() == 0.0 {
        format!("{} hours", hours as u64)
    } else {
        format!("{hours:.1} hours")
    }
}

/// What `search_sessions` found, best first, and what it could not search.
pub(super) struct Found {
    pub hits: Vec<(Chat, Vec<String>)>,
    /// Sessions this device holds no transcript of.
    pub unstored: usize,
    /// Unindexed sessions past the scan bound: titles only.
    pub not_searched: usize,
}

/// Match `candidates` (newest first) against `words`: titles and previews
/// from the rows; message text from the index (`lookup`), and by scanning
/// transcripts (`read`) for the newest [`SEARCH_SCAN_MAX`] candidates the
/// index does not hold yet. Sessions with matching messages rank by their
/// best message's relevance (title matches first), then by recency; the
/// title-only ones follow, newest first. Blocking.
pub(super) fn find(
    candidates: &[Chat],
    words: &[String],
    transcripts: bool,
    limit: usize,
    lookup: impl FnOnce(&[String], &[String]) -> Result<super::index::Lookup, EngineError>,
    mut read: impl FnMut(&str) -> Result<Option<Vec<SessionMessageEntry>>, EngineError>,
) -> Found {
    // chat id → (best score, excerpts by message index)
    let mut matched: HashMap<String, (f64, Vec<(usize, String)>)> = HashMap::new();
    let mut unstored = 0usize;
    let mut not_searched = 0usize;
    if transcripts {
        let ids: Vec<String> = candidates.iter().map(|c| c.id.clone()).collect();
        let lookup = lookup(words, &ids).unwrap_or_else(|err| {
            tracing::debug!(error = %err, "bot search without the index");
            Default::default()
        });
        let mut unindexed: Vec<&Chat> = Vec::new();
        for chat in candidates {
            match lookup.indexed.get(&chat.id) {
                Some(true) => {}
                Some(false) => unstored += 1,
                None => unindexed.push(chat),
            }
        }
        for hit in lookup.hits {
            let (_, excerpts) = matched
                .entry(hit.chat_id)
                .or_insert_with(|| (hit.score, Vec::new()));
            if excerpts.len() < SEARCH_EXCERPTS {
                let lower = hit.text.to_lowercase();
                excerpts.push((
                    hit.index,
                    format!(
                        "#{} {}: {}",
                        hit.index,
                        hit.role,
                        excerpt(&hit.text, &lower, &words[0])
                    ),
                ));
            }
        }
        not_searched = unindexed.len().saturating_sub(SEARCH_SCAN_MAX);
        for chat in unindexed.into_iter().take(SEARCH_SCAN_MAX) {
            match read(&chat.id) {
                Ok(Some(entries)) => {
                    let mut excerpts = Vec::new();
                    for (index, entry) in entries.iter().enumerate() {
                        if excerpts.len() >= SEARCH_EXCERPTS {
                            break;
                        }
                        let text = entry_text(entry);
                        let lower = text.to_lowercase();
                        if words.iter().all(|w| lower.contains(w)) {
                            excerpts.push((
                                index,
                                format!(
                                    "#{index} {}: {}",
                                    role(entry),
                                    excerpt(&text, &lower, &words[0])
                                ),
                            ));
                        }
                    }
                    if !excerpts.is_empty() {
                        // Unranked: after every indexed match of equal standing.
                        matched.insert(chat.id.clone(), (0.0, excerpts));
                    }
                }
                Ok(None) => unstored += 1,
                Err(err) => {
                    tracing::debug!(chat = %chat.id, error = %err, "bot search skipped a chat");
                    unstored += 1;
                }
            }
        }
    }
    let mut ranked: Vec<(u8, f64, usize, &Chat, Vec<String>)> = Vec::new();
    for (position, chat) in candidates.iter().enumerate() {
        let haystack = format!(
            "{} {}",
            chat.title.as_deref().unwrap_or(""),
            chat.last_message_preview.as_deref().unwrap_or("")
        )
        .to_lowercase();
        let title_hit = words.iter().all(|w| haystack.contains(w));
        let (tier, score, excerpts) = match matched.remove(&chat.id) {
            Some((score, mut excerpts)) => {
                excerpts.sort_by_key(|(index, _)| *index);
                let lines = excerpts.into_iter().map(|(_, line)| line).collect();
                (if title_hit { 0 } else { 1 }, score, lines)
            }
            None if title_hit => (2, 0.0, Vec::new()),
            None => continue,
        };
        ranked.push((tier, score, position, chat, excerpts));
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
    Found {
        hits: ranked
            .into_iter()
            .take(limit)
            .map(|(_, _, _, chat, excerpts)| (chat.clone(), excerpts))
            .collect(),
        unstored,
        not_searched,
    }
}

pub(super) fn role(entry: &SessionMessageEntry) -> &'static str {
    match entry.role {
        MessageRole::User => "user",
        MessageRole::Assistant => "agent",
        MessageRole::System => "system",
    }
}

/// The message's readable text: its text parts, in order.
pub(super) fn entry_text(entry: &SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// About [`EXCERPT_RADIUS`] characters either side of the first `word`.
fn excerpt(text: &str, lower: &str, word: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    // Lowercasing can change byte lengths; locate by character index.
    let at = lower
        .find(word)
        .map(|byte| lower[..byte].chars().count())
        .unwrap_or(0)
        .min(chars.len());
    let start = at.saturating_sub(EXCERPT_RADIUS);
    let end = (at + word.chars().count() + EXCERPT_RADIUS).min(chars.len());
    let body: String = chars[start..end].iter().collect();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        body.split_whitespace().collect::<Vec<_>>().join(" "),
        if end < chars.len() { "…" } else { "" }
    )
}

fn tool_summary(call: &ToolCall) -> String {
    let short = |s: &str| one_line(s, 120);
    match call {
        ToolCall::Exec { command } => format!("ran `{}`", short(command)),
        ToolCall::ReadFile { path } => format!("read {path}"),
        ToolCall::WriteFile { path, .. } => format!("wrote {path}"),
        ToolCall::EditFile { path, .. } => format!("edited {path}"),
        ToolCall::ApplyPatch { path } => match path {
            Some(path) => format!("patched {path}"),
            None => "applied a patch".into(),
        },
        ToolCall::Search { pattern, path } => match path {
            Some(path) => format!("searched {path} for {}", short(pattern)),
            None => format!("searched for {}", short(pattern)),
        },
        ToolCall::Glob { pattern } => format!("listed {}", short(pattern)),
        ToolCall::WebFetch { url, .. } => format!("fetched {url}"),
        ToolCall::WebSearch { query } => format!("searched the web for {}", short(query)),
        ToolCall::Todo { items } => format!(
            "updated the plan ({} of {} done)",
            items.iter().filter(|i| i.done).count(),
            items.len()
        ),
        ToolCall::Mcp { server, tool, .. } => format!("called {server}/{tool}"),
        ToolCall::Unknown { name, .. } => format!("used {name}"),
    }
}

fn render_entry(index: usize, entry: &SessionMessageEntry, now: DateTime<Utc>) -> String {
    let at = DateTime::from_timestamp_millis(entry.created_at).unwrap_or(now);
    let mut lines = vec![format!("[#{index}] {} · {}", role(entry), when(at, now))];
    let mut text = String::new();
    for part in &entry.parts {
        match part {
            MessagePart::Text { text: body, .. } | MessagePart::Post { text: body, .. } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(body);
            }
            MessagePart::Tool { call, is_error, .. } => {
                if !text.is_empty() {
                    lines.push(clip(text.trim(), MESSAGE_MAX_CHARS));
                    text.clear();
                }
                lines.push(format!(
                    "  ↳ {}{}",
                    tool_summary(call),
                    if *is_error { " (failed)" } else { "" }
                ));
            }
            MessagePart::Input { questions, .. } => {
                let asked: Vec<String> = questions
                    .iter()
                    .map(|q| one_line(&q.question, 200))
                    .collect();
                lines.push(format!("  ↳ asked the user: {}", asked.join(" / ")));
            }
            MessagePart::Error { message, .. } => {
                lines.push(format!("  ↳ error: {}", one_line(message, 300)));
            }
            MessagePart::Reasoning { .. } => {}
        }
    }
    if !text.trim().is_empty() {
        lines.push(clip(text.trim(), MESSAGE_MAX_CHARS));
    }
    lines.join("\n")
}

/// `read_tool` names the tool that pages on (`read_session`, or the
/// memory tools' `read_history`).
pub(super) fn render_session(
    view: &View,
    chat: &Chat,
    entries: &[SessionMessageEntry],
    from: Option<usize>,
    limit: Option<usize>,
    read_tool: &str,
) -> String {
    let total = entries.len();
    let limit = limit.unwrap_or(READ_DEFAULT).clamp(1, READ_MAX);
    let start = from
        .unwrap_or_else(|| total.saturating_sub(limit))
        .min(total);
    let end = (start + limit).min(total);
    let mut out = vec![format!(
        "Session {} · {total} message(s){}",
        view.header(chat),
        if total == 0 {
            String::new()
        } else {
            format!(" · showing #{start}–#{}", end.saturating_sub(1).max(start))
        }
    )];
    if start > 0 {
        out.push(format!(
            "Earlier messages: {read_tool} with from={}.",
            start.saturating_sub(limit)
        ));
    }
    let mut used: usize = out.iter().map(|l| l.chars().count()).sum();
    let mut shown_to = start;
    for (index, entry) in entries.iter().enumerate().take(end).skip(start) {
        let block = render_entry(index, entry, view.now);
        let size = block.chars().count();
        if used + size > ANSWER_MAX_CHARS && index > start {
            break;
        }
        used += size;
        out.push(String::new());
        out.push(block);
        shown_to = index + 1;
    }
    if shown_to < total {
        out.push(String::new());
        out.push(format!("More messages: {read_tool} with from={shown_to}."));
    }
    out.join("\n")
}

#[cfg(test)]
pub(super) fn list_sessions_args(value: serde_json::Value) -> ListSessions {
    serde_json::from_value(value).expect("list_sessions arguments")
}

#[cfg(test)]
pub(super) fn session_activity_args(value: serde_json::Value) -> SessionActivity {
    serde_json::from_value(value).expect("session_activity arguments")
}
