//! Agent bots (docs/design/agent-bots.md): an owner's long-lived agent over
//! the projects and sessions it is granted, hosted on one of the owner's
//! devices. The definition rides the registry's `bots` rows; the bot's
//! conversation is an ordinary chat marked with [`BotChat`].
//!
//! [`BotScope`] is the whole permission model and is pure on purpose: the
//! host engine enforces it on every bot tool call, and the desktop previews
//! the same answer while the owner edits it.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Chat;

/// Longest bot name, in characters.
pub const BOT_NAME_MAX_CHARS: usize = 40;
/// Longest owner instructions, in characters.
pub const BOT_INSTRUCTIONS_MAX_CHARS: usize = 8 * 1024;
/// Session ids one scope may list in `includeSessions` / `excludeSessions`.
pub const BOT_SCOPE_MAX_SESSIONS: usize = 2_000;

/// The tools a bot run may call — the whole allowlist its Pi process gets.
/// The engine's bot tools extension registers exactly these, and the engine
/// answers each one through the `BotTool` RPC.
pub const BOT_TOOLS: [&str; 6] = [
    "list_projects",
    "list_sessions",
    "session_activity",
    "search_sessions",
    "read_session",
    "project_git",
];

/// Coordination tools (P2, *Coordinate*): `propose_plan` in the bot chat,
/// the rest in orchestration threads (`list_work` and `get_work` in both).
pub const BOT_WORK_TOOLS: [&str; 11] = [
    "propose_plan",
    "list_work",
    "get_work",
    "create_worktree",
    "start_session",
    "wait_for_session",
    "answer_session",
    "steer_session",
    "stop_session",
    "update_step",
    "finish_work",
];

/// Pi's own tools an owner thread gets with *Work directly*, under
/// permission control.
pub const BOT_DIRECT_TOOLS: [&str; 8] = [
    "bash",
    "read",
    "write",
    "edit",
    "grep",
    "find",
    "ls",
    "request_permission",
];

/// Notebook and schedule tools (P3): read, search, write, update and delete
/// notes, then the routines.
pub const BOT_MEMORY_TOOLS: [&str; 7] = [
    "read_notes",
    "search_notes",
    "write_note",
    "update_note",
    "delete_note",
    "list_routines",
    "save_routine",
];

/// Network tools (P4), owner threads only.
pub const BOT_NETWORK_TOOLS: [&str; 2] = ["list_connections", "ask_bot"];

/// One bot definition (a registry `bots` row).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bot {
    pub id: String,
    pub name: String,
    /// Avatar glyph key from the project-icon set; `None` = the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Avatar colour key from the project-colour set; `None` = neutral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// The engine that runs the bot. Fixed at creation.
    pub host_device_id: String,
    /// The bot's own conversation, hosted on `host_device_id`.
    pub chat_id: String,
    /// The owner's standing instructions, appended to the bot's system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default)]
    pub scope: BotScope,
    /// What its owner threads may do beyond reading.
    #[serde(default)]
    pub capabilities: crate::BotCapabilities,
    /// Kill switch: no routine runs and no new work starts while set.
    #[serde(default)]
    pub paused: bool,
    pub created_at: DateTime<Utc>,
}

impl Bot {
    /// Reject what the UI must never persist.
    pub fn validate(&self) -> Result<(), String> {
        validate_name(&self.name)?;
        if let Some(instructions) = &self.instructions
            && instructions.chars().count() > BOT_INSTRUCTIONS_MAX_CHARS
        {
            return Err(format!(
                "Instructions must be {BOT_INSTRUCTIONS_MAX_CHARS} characters or fewer."
            ));
        }
        self.scope.validate()
    }
}

/// A trimmed, non-empty name of at most [`BOT_NAME_MAX_CHARS`] characters.
pub fn validate_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the bot a name.".into());
    }
    if name.chars().count() > BOT_NAME_MAX_CHARS {
        return Err(format!(
            "Name must be {BOT_NAME_MAX_CHARS} characters or fewer."
        ));
    }
    Ok(())
}

/// The marker on a bot thread's chat row. Bot chats are never visible to any
/// bot, and the sidebar shows them in the bot strip instead of a project.
///
/// Only the host engine writes it, when it creates the thread: no client
/// operation sets or changes it, so a thread's origin is whatever the engine
/// stamped (docs/design/agent-bots.md §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotChat {
    pub bot_id: String,
    /// Who the thread works for. Rows written before origins existed are the
    /// bots' own chats, so a missing origin reads as the owner.
    #[serde(default)]
    pub origin: BotOrigin,
    /// Which of the bot's threads this is; `Main` for the bot chat.
    #[serde(default, skip_serializing_if = "is_main")]
    pub thread: crate::BotThread,
}

fn is_main(thread: &crate::BotThread) -> bool {
    *thread == crate::BotThread::Main
}

impl BotChat {
    /// The marker of a thread the owner instructs (the bot chat).
    pub fn owner(bot_id: impl Into<String>) -> Self {
        Self::new(bot_id, BotOrigin::Owner)
    }

    /// A thread the bot itself runs in: every marked chat except a project
    /// routine's session, which the marker only hides (it runs as a plain
    /// session).
    pub fn is_bot_thread(&self) -> bool {
        !matches!(self.thread, crate::BotThread::RoutineSession { .. })
    }

    pub fn new(bot_id: impl Into<String>, origin: BotOrigin) -> Self {
        Self {
            bot_id: bot_id.into(),
            origin,
            thread: crate::BotThread::Main,
        }
    }

    /// The marker of a hidden thread the host creates.
    pub fn thread(bot_id: impl Into<String>, origin: BotOrigin, thread: crate::BotThread) -> Self {
        Self {
            bot_id: bot_id.into(),
            origin,
            thread,
        }
    }

    /// The bot chat itself (not a hidden thread).
    pub fn is_main(&self) -> bool {
        is_main(&self.thread)
    }
}

/// Who started a bot thread, which fixes what the thread may do
/// (docs/design/agent-bots.md §4.1). It never changes for a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BotOrigin {
    /// The owner: the bot chat, and later its orchestration threads.
    #[default]
    Owner,
    /// A routine running unattended: nobody watches it, so it never waits
    /// for an answer or an approval.
    Routine,
    /// Another user's bot asking a question. An origin this engine does not
    /// know also reads as a peer, the narrowest one, so a row from a newer
    /// engine never runs with more than it was meant to.
    #[serde(other)]
    Peer,
}

/// Which projects a bot sees.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum ProjectGrant {
    /// No project — only sessions named in `includeSessions`.
    #[default]
    None,
    /// Every project, including ones created later.
    All,
    /// The listed spaces (device + folder pairs).
    #[serde(rename_all = "camelCase")]
    Selected { space_ids: Vec<String> },
}

/// What a bot may read. A granted project grants its sessions, including
/// future ones; `excludeSessions` removes single sessions and
/// `includeSessions` adds single sessions from ungranted projects. Subagent
/// child chats follow their parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotScope {
    #[serde(default)]
    pub projects: ProjectGrant,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_sessions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_sessions: Vec<String>,
    /// Archived sessions stay hidden unless this is set (or the session is
    /// named in `includeSessions`).
    #[serde(default)]
    pub include_archived: bool,
    /// Message text may be read. Off, the bot sees only titles, previews and
    /// status.
    #[serde(default = "yes")]
    pub transcripts: bool,
}

fn yes() -> bool {
    true
}

/// Matches the serde defaults: no projects, message text readable once
/// something is granted.
impl Default for BotScope {
    fn default() -> Self {
        Self {
            projects: ProjectGrant::None,
            include_sessions: Vec::new(),
            exclude_sessions: Vec::new(),
            include_archived: false,
            transcripts: true,
        }
    }
}

impl BotScope {
    pub fn validate(&self) -> Result<(), String> {
        let selected = match &self.projects {
            ProjectGrant::Selected { space_ids } => space_ids.len(),
            _ => 0,
        };
        if selected > BOT_SCOPE_MAX_SESSIONS
            || self.include_sessions.len() > BOT_SCOPE_MAX_SESSIONS
            || self.exclude_sessions.len() > BOT_SCOPE_MAX_SESSIONS
        {
            return Err("The scope lists too many projects or sessions.".into());
        }
        Ok(())
    }

    /// Whether the project (space) is granted.
    pub fn sees_space(&self, space_id: &str) -> bool {
        match &self.projects {
            ProjectGrant::None => false,
            ProjectGrant::All => true,
            ProjectGrant::Selected { space_ids } => space_ids.iter().any(|id| id == space_id),
        }
    }

    /// The chats this scope can see, in `chats` order. Bot chats are never
    /// visible; a child chat is visible when its root parent is.
    pub fn visible_chats<'a>(&self, chats: &'a [Chat]) -> Vec<&'a Chat> {
        let by_id: HashMap<&str, &Chat> = chats.iter().map(|c| (c.id.as_str(), c)).collect();
        chats
            .iter()
            .filter(|chat| self.sees_chat_in(chat, &by_id))
            .collect()
    }

    /// One chat's visibility; `chats` resolves child chats' parents.
    pub fn sees_chat(&self, chat: &Chat, chats: &[Chat]) -> bool {
        let by_id: HashMap<&str, &Chat> = chats.iter().map(|c| (c.id.as_str(), c)).collect();
        self.sees_chat_in(chat, &by_id)
    }

    /// The scope both `self` and `other` grant (§5.2: a connection's scope
    /// only narrows the bot's). Projects are those both grant, excluded
    /// sessions add up, and archived sessions and message text need both. A
    /// session both see by different rules (one grants its project, the
    /// other names it) is named in `includeSessions`, resolved against
    /// `chats`, so the result sees exactly the chats both scopes see.
    pub fn intersect(&self, other: &BotScope, chats: &[Chat]) -> BotScope {
        let projects = match (&self.projects, &other.projects) {
            (ProjectGrant::None, _) | (_, ProjectGrant::None) => ProjectGrant::None,
            (ProjectGrant::All, grant) | (grant, ProjectGrant::All) => grant.clone(),
            (ProjectGrant::Selected { space_ids: a }, ProjectGrant::Selected { space_ids: b }) => {
                ProjectGrant::Selected {
                    space_ids: a.iter().filter(|id| b.contains(id)).cloned().collect(),
                }
            }
        };
        let mut exclude_sessions = self.exclude_sessions.clone();
        for id in &other.exclude_sessions {
            if !exclude_sessions.contains(id) {
                exclude_sessions.push(id.clone());
            }
        }
        let mut narrowed = BotScope {
            projects,
            include_sessions: Vec::new(),
            exclude_sessions,
            include_archived: self.include_archived && other.include_archived,
            transcripts: self.transcripts && other.transcripts,
        };
        let by_id: HashMap<&str, &Chat> = chats.iter().map(|c| (c.id.as_str(), c)).collect();
        let include: Vec<String> = chats
            .iter()
            .filter(|chat| chat.parent_chat_id().is_none())
            .filter(|chat| {
                self.sees_chat_in(chat, &by_id)
                    && other.sees_chat_in(chat, &by_id)
                    && !narrowed.sees_chat_in(chat, &by_id)
            })
            .map(|chat| chat.id.clone())
            .collect();
        narrowed.include_sessions = include;
        narrowed
    }

    fn sees_chat_in(&self, chat: &Chat, by_id: &HashMap<&str, &Chat>) -> bool {
        // Walk to the root of a child chain; a broken or cyclic chain hides
        // the chat rather than guessing.
        let mut root = chat;
        let mut seen = HashSet::new();
        while let Some(parent) = root.parent_chat_id() {
            if !seen.insert(root.id.as_str()) {
                return false;
            }
            match by_id.get(parent) {
                Some(next) => root = next,
                None => return false,
            }
        }
        if chat.is_bot() || root.is_bot() {
            return false;
        }
        let listed = |ids: &[String], id: &str| ids.iter().any(|x| x == id);
        if listed(&self.exclude_sessions, &chat.id) || listed(&self.exclude_sessions, &root.id) {
            return false;
        }
        if listed(&self.include_sessions, &root.id) {
            return true;
        }
        if root.archived && !self.include_archived {
            return false;
        }
        root.space_id
            .as_deref()
            .is_some_and(|space_id| self.sees_space(space_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChildAgentProfile, ChildChat, SubagentRunMode};

    fn chat(id: &str, space: Option<&str>) -> Chat {
        Chat {
            id: id.into(),
            device_id: "dev".into(),
            title: None,
            archived: false,
            pinned: false,
            cwd: None,
            branch: None,
            checkout_id: None,
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: Utc::now(),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: space.map(Into::into),
            last_seen_at: None,
            room_gen: None,
            child: None,
            bot: None,
            memory: None,
        }
    }

    fn child_of(id: &str, parent: &str) -> Chat {
        Chat {
            child: Some(ChildChat {
                parent_chat_id: parent.into(),
                parent_run_id: "run".into(),
                agent: "worker".into(),
                task: "task".into(),
                mode: SubagentRunMode::Sync,
                tool_call_id: None,
                profile: ChildAgentProfile {
                    system_prompt: String::new(),
                    tools: vec![],
                    model: None,
                    thinking: None,
                },
            }),
            ..chat(id, None)
        }
    }

    fn ids(scope: &BotScope, chats: &[Chat]) -> Vec<String> {
        scope
            .visible_chats(chats)
            .into_iter()
            .map(|c| c.id.clone())
            .collect()
    }

    #[test]
    fn a_new_scope_sees_nothing() {
        let chats = [chat("a", Some("s1")), chat("b", None)];
        assert!(ids(&BotScope::default(), &chats).is_empty());
    }

    #[test]
    fn granted_projects_grant_their_sessions() {
        let chats = [
            chat("a", Some("s1")),
            chat("b", Some("s2")),
            chat("c", None),
        ];
        let selected = BotScope {
            projects: ProjectGrant::Selected {
                space_ids: vec!["s1".into()],
            },
            ..Default::default()
        };
        assert_eq!(ids(&selected, &chats), ["a"]);
        let all = BotScope {
            projects: ProjectGrant::All,
            ..Default::default()
        };
        // A project-less chat belongs to no project, so "all projects" skips it.
        assert_eq!(ids(&all, &chats), ["a", "b"]);
    }

    #[test]
    fn session_overrides_win_over_projects() {
        let chats = [chat("a", Some("s1")), chat("b", Some("s2"))];
        let scope = BotScope {
            projects: ProjectGrant::Selected {
                space_ids: vec!["s1".into()],
            },
            include_sessions: vec!["b".into()],
            exclude_sessions: vec!["a".into()],
            ..Default::default()
        };
        assert_eq!(ids(&scope, &chats), ["b"]);
    }

    #[test]
    fn archived_sessions_need_an_opt_in() {
        let mut archived = chat("a", Some("s1"));
        archived.archived = true;
        let mut scope = BotScope {
            projects: ProjectGrant::All,
            ..Default::default()
        };
        assert!(ids(&scope, std::slice::from_ref(&archived)).is_empty());
        scope.include_archived = true;
        assert_eq!(ids(&scope, &[archived]), ["a"]);
    }

    #[test]
    fn bot_chats_are_never_visible() {
        let mut own = chat("bot-chat", Some("s1"));
        own.bot = Some(BotChat::owner("b1"));
        let scope = BotScope {
            projects: ProjectGrant::All,
            include_sessions: vec!["bot-chat".into()],
            ..Default::default()
        };
        assert!(ids(&scope, &[own]).is_empty());
    }

    #[test]
    fn child_chats_follow_their_root_parent() {
        let chats = [
            chat("parent", Some("s1")),
            child_of("child", "parent"),
            child_of("grandchild", "child"),
            child_of("orphan", "missing"),
        ];
        let scope = BotScope {
            projects: ProjectGrant::Selected {
                space_ids: vec!["s1".into()],
            },
            ..Default::default()
        };
        assert_eq!(ids(&scope, &chats), ["parent", "child", "grandchild"]);
        let excluded = BotScope {
            exclude_sessions: vec!["parent".into()],
            ..scope
        };
        assert!(ids(&excluded, &chats).is_empty());
    }

    #[test]
    fn a_cyclic_child_chain_is_hidden() {
        let chats = [child_of("a", "b"), child_of("b", "a")];
        let scope = BotScope {
            projects: ProjectGrant::All,
            include_sessions: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        assert!(ids(&scope, &chats).is_empty());
    }

    #[test]
    fn an_intersection_sees_only_what_both_scopes_see() {
        let mut archived = chat("old", Some("s1"));
        archived.archived = true;
        let chats = [
            chat("a", Some("s1")),
            chat("b", Some("s2")),
            chat("c", Some("s3")),
            chat("loose", None),
            child_of("a-child", "a"),
            child_of("b-child", "b"),
            archived,
        ];
        let bot = BotScope {
            projects: ProjectGrant::Selected {
                space_ids: vec!["s1".into(), "s2".into()],
            },
            include_sessions: vec!["loose".into()],
            exclude_sessions: vec!["b-child".into()],
            include_archived: true,
            transcripts: true,
        };
        let connection = BotScope {
            projects: ProjectGrant::Selected {
                space_ids: vec!["s1".into(), "s3".into()],
            },
            include_sessions: vec!["b".into(), "loose".into()],
            exclude_sessions: vec!["a-child".into()],
            include_archived: false,
            transcripts: false,
        };
        let both = bot.intersect(&connection, &chats);
        let seen = |scope: &BotScope| -> std::collections::BTreeSet<String> {
            ids(scope, &chats).into_iter().collect()
        };
        let expected: std::collections::BTreeSet<String> = seen(&bot)
            .intersection(&seen(&connection))
            .cloned()
            .collect();
        assert_eq!(seen(&both), expected);
        assert_eq!(
            seen(&both).into_iter().collect::<Vec<_>>(),
            ["a", "b", "loose"],
            "s3 is the connection's only, archived needs both, each side's exclusions hold"
        );
        assert_eq!(
            both.projects,
            ProjectGrant::Selected {
                space_ids: vec!["s1".into()]
            }
        );
        assert!(both.sees_space("s1") && !both.sees_space("s2") && !both.sees_space("s3"));
        assert!(!both.transcripts && !both.include_archived);
        // Commutative in what it sees, and "all" narrows to the other side.
        assert_eq!(seen(&connection.intersect(&bot, &chats)), seen(&both));
        let all = BotScope {
            projects: ProjectGrant::All,
            ..Default::default()
        };
        assert_eq!(all.intersect(&bot, &chats).projects, bot.projects);
        assert_eq!(
            BotScope::default().intersect(&all, &chats).projects,
            ProjectGrant::None
        );
        assert!(seen(&BotScope::default().intersect(&bot, &chats)).is_empty());
    }

    #[test]
    fn scope_round_trips_with_defaults() {
        let parsed: BotScope = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(parsed, BotScope::default(), "serde and Rust defaults agree");
        assert_eq!(parsed.projects, ProjectGrant::None);
        assert!(parsed.transcripts);
        let selected = BotScope {
            projects: ProjectGrant::Selected {
                space_ids: vec!["s1".into()],
            },
            ..Default::default()
        };
        let value = serde_json::to_value(&selected).unwrap();
        assert_eq!(
            value["projects"],
            serde_json::json!({"mode": "selected", "spaceIds": ["s1"]})
        );
        assert_eq!(serde_json::from_value::<BotScope>(value).unwrap(), selected);
    }

    #[test]
    fn bot_markers_read_old_rows_as_owner_and_unknown_origins_as_peer() {
        let old: BotChat = serde_json::from_value(serde_json::json!({"botId": "b1"})).unwrap();
        assert_eq!(old, BotChat::owner("b1"));
        for (origin, wire) in [
            (BotOrigin::Owner, "owner"),
            (BotOrigin::Routine, "routine"),
            (BotOrigin::Peer, "peer"),
        ] {
            let marker = BotChat::new("b1", origin);
            let value = serde_json::to_value(&marker).unwrap();
            assert_eq!(value, serde_json::json!({"botId": "b1", "origin": wire}));
            assert_eq!(serde_json::from_value::<BotChat>(value).unwrap(), marker);
        }
        let future: BotChat =
            serde_json::from_value(serde_json::json!({"botId": "b1", "origin": "auditor"}))
                .unwrap();
        assert_eq!(future.origin, BotOrigin::Peer);
    }

    #[test]
    fn names_are_bounded() {
        assert!(validate_name("  ").is_err());
        assert!(validate_name("Billing bot").is_ok());
        assert!(validate_name(&"x".repeat(BOT_NAME_MAX_CHARS + 1)).is_err());
    }
}
