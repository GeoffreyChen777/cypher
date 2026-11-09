//! Memory (docs/design/memory.md): what agents learn and keep between
//! sessions, as small trees of markdown files.
//!
//! A **store** is one Loro doc synced like a chat (its id is the room id);
//! the registry holds one small [`MemoryStore`] row per store so every
//! device can list stores, show counts and run consolidation without
//! loading the doc. The doc's files, pending writes and change log reach
//! the app as one [`MemoryView`] frame (`WatchMemory`); edits go through
//! `EditMemory` ([`MemoryEdit`]). Times are epoch milliseconds.

use serde::{Deserialize, Serialize};

use crate::bot_records::clip;

/// Registry row kind of [`MemoryStore`].
pub const KIND_MEMORY_STORES: &str = "memoryStores";

/// The always-injected index of every store.
pub const MEMORY_INDEX: &str = "/MEMORY.md";
/// The longest file a store keeps.
pub const MEMORY_FILE_MAX: usize = 64 * 1024;
/// The most a store holds, all files together.
pub const MEMORY_STORE_MAX: usize = 512 * 1024;
/// The most files one store holds.
pub const MEMORY_FILES_MAX: usize = 200;
/// What of a store's `MEMORY.md` the prompt carries: this many lines…
pub const MEMORY_INDEX_LINES: usize = 200;
/// …or this many bytes, whichever comes first.
pub const MEMORY_INDEX_BYTES: usize = 16 * 1024;
/// All stores of one run together.
pub const MEMORY_PROMPT_BYTES: usize = 32 * 1024;
/// The change log keeps this many entries per store.
pub const MEMORY_CHANGES_MAX: usize = 300;
/// The preview of `MEMORY.md` a store's row carries (sidebar cards).
pub const MEMORY_PREVIEW_MAX: usize = 1_200;
/// The longest path, root excluded.
pub const MEMORY_PATH_MAX: usize = 160;

/// Held writes a store's row carries for chat cards (newest), and the
/// characters of each one's text.
pub const MEMORY_HELD_IN_ROW: usize = 10;
pub const MEMORY_HELD_PREVIEW: usize = 400;
/// Decisions on held writes a row remembers (so a card shows its outcome).
pub const MEMORY_DECIDED_IN_ROW: usize = 30;

/// The memory tools every run but a peer's gets (one Pi extension).
pub const MEMORY_TOOLS: [&str; 3] = ["memory", "search_history", "read_history"];

/// Whose memory a store is.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MemoryScope {
    /// The user's own memory, read by every session and workspace.
    You,
    /// One project's memory, shared by every session in it.
    #[serde(rename_all = "camelCase")]
    Project { space_id: String },
    /// A workspace's own memory.
    #[serde(rename_all = "camelCase")]
    Workspace { bot_id: String },
    /// One subagent profile's memory: the user's (`space_id` none) or one
    /// project's.
    #[serde(rename_all = "camelCase")]
    Role {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space_id: Option<String>,
    },
}

impl MemoryScope {
    /// The store's doc (and sync room) id. `user_key` identifies the user
    /// (a hash of org and user), so ids of user-level stores never collide
    /// across users; project and workspace ids are unique already.
    pub fn store_id(&self, user_key: &str) -> String {
        let id = match self {
            Self::You => format!("mem-u-{user_key}"),
            Self::Project { space_id } => format!("mem-p-{space_id}"),
            Self::Workspace { bot_id } => format!("mem-w-{bot_id}"),
            Self::Role {
                agent,
                space_id: None,
            } => format!("mem-r-{user_key}-{}", safe_id(agent)),
            Self::Role {
                agent,
                space_id: Some(space),
            } => format!("mem-rp-{space}-{}", safe_id(agent)),
        };
        id.chars().take(128).collect()
    }

    /// The root this store has in a run's memory paths when it is its own
    /// kind's only store (`/you`, `/project`, `/workspace`, `/role`).
    pub fn root(&self) -> &'static str {
        match self {
            Self::You => "/you",
            Self::Project { .. } => "/project",
            Self::Workspace { .. } => "/workspace",
            Self::Role { .. } => "/role",
        }
    }
}

/// `text` with anything but ASCII letters, digits, `-` and `_` as `_`.
fn safe_id(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// When a store's consolidation pass runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Consolidation {
    /// On by default (§6): a nightly pass for a store with new activity.
    pub enabled: bool,
    /// The hour (0–23, the host device's local time) it starts.
    pub hour: u8,
}

impl Default for Consolidation {
    fn default() -> Self {
        Self {
            enabled: true,
            hour: 3,
        }
    }
}

/// How a consolidation pass ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PassOutcome {
    #[default]
    Running,
    Done,
    /// Nothing new since the last pass: no run.
    Skipped,
    Failed,
    /// Undone by the owner.
    Undone,
}

/// One consolidation pass.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MemoryPass {
    pub id: String,
    pub at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    pub outcome: PassOutcome,
    /// The run's short summary, or why it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Files it changed.
    pub changes: u32,
    /// Tokens the run used (input and output), when known.
    pub tokens: u64,
    /// The hidden chat it ran in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// Sessions and threads it read since the last pass.
    pub sources: u32,
    /// Asked for with "Run now".
    pub manual: bool,
    /// The store's version before the pass (hex): "Undo" restores it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
}

impl MemoryPass {
    pub fn is_running(&self) -> bool {
        self.outcome == PassOutcome::Running
    }
}

/// One month's consolidation cost.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MonthUsage {
    /// `YYYY-MM`.
    pub month: String,
    pub tokens: u64,
    pub passes: u32,
}

/// A store's registry row: what devices know without loading its doc.
/// The engine that changes the doc rewrites the counts; the consolidation
/// fields are written by the device that runs the pass, and `consolidation`
/// by the owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStore {
    /// The doc id ([`MemoryScope::store_id`]).
    pub id: String,
    pub scope: MemoryScope,
    /// The device that made the store: where a user-level store's pass
    /// runs. Project and workspace stores follow their project's or
    /// workspace's host instead.
    #[serde(default)]
    pub host_device_id: String,
    #[serde(default)]
    pub files: u32,
    #[serde(default)]
    pub bytes: u64,
    /// Writes waiting for the owner's review.
    #[serde(default)]
    pub pending: u32,
    /// The start of `MEMORY.md`, for cards.
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub consolidation: Consolidation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pass: Option<MemoryPass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub month: Option<MonthUsage>,
    /// A pass was asked for ("Run now") at this time; the host runs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_requested_at: Option<i64>,
    /// The newest held writes, for the approval card drawn at the tool call
    /// that made each (in the chat it was made in).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<HeldWrite>,
    /// Recent decisions on held writes, newest last: a decided card shows
    /// its outcome.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decided: Vec<HeldDecision>,
}

/// A held write as a chat card shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldWrite {
    pub id: String,
    pub chat_id: String,
    /// The `memory` tool call that made it: where the card is drawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub path: String,
    /// The file as the write would leave it, clipped (`None`: deleted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    pub reason: String,
    pub at: i64,
}

/// What the owner decided about one held write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldDecision {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub saved: bool,
    pub at: i64,
}

impl MemoryStore {
    /// The held write a `memory` tool call made, still waiting.
    pub fn held_for_call(&self, tool_call_id: &str) -> Option<&HeldWrite> {
        self.held
            .iter()
            .find(|h| h.tool_call_id.as_deref() == Some(tool_call_id))
    }

    /// The decision on the write a `memory` tool call made.
    pub fn decided_for_call(&self, tool_call_id: &str) -> Option<&HeldDecision> {
        self.decided
            .iter()
            .rev()
            .find(|d| d.tool_call_id.as_deref() == Some(tool_call_id))
    }
}

impl MemoryStore {
    pub fn new(id: String, scope: MemoryScope, host_device_id: String, now: i64) -> Self {
        Self {
            id,
            scope,
            host_device_id,
            files: 0,
            bytes: 0,
            pending: 0,
            preview: String::new(),
            created_at: now,
            updated_at: now,
            consolidation: Consolidation::default(),
            last_pass: None,
            month: None,
            pass_requested_at: None,
            held: Vec::new(),
            decided: Vec::new(),
        }
    }

    pub fn clamp(&mut self) {
        self.preview = clip(&self.preview, MEMORY_PREVIEW_MAX);
        let over = self.held.len().saturating_sub(MEMORY_HELD_IN_ROW);
        self.held.drain(..over);
        for held in &mut self.held {
            held.preview = held
                .preview
                .as_deref()
                .map(|p| clip(p, MEMORY_HELD_PREVIEW));
            held.reason = clip(&held.reason, crate::bot_records::LINE_MAX);
        }
        let over = self.decided.len().saturating_sub(MEMORY_DECIDED_IN_ROW);
        self.decided.drain(..over);
        self.consolidation.hour = self.consolidation.hour.min(23);
        if let Some(pass) = &mut self.last_pass {
            pass.summary = pass
                .summary
                .as_deref()
                .map(|s| clip(s, crate::bot_records::RESULT_MAX));
        }
    }
}

/// Who wrote a file, a pending write or a change.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MemoryAuthor {
    /// The user, in the app.
    #[default]
    Owner,
    /// An agent in a session (or a subagent's chat).
    #[serde(rename_all = "camelCase")]
    Session {
        chat_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// A workspace's agent, in one of its threads.
    #[serde(rename_all = "camelCase")]
    Workspace {
        bot_id: String,
        chat_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// A consolidation pass.
    #[serde(rename_all = "camelCase")]
    Consolidation { pass_id: String },
    /// Moved from the workspace notebook.
    Notebook,
}

impl MemoryAuthor {
    /// A short "who" for lists: "You", "a session", "consolidation".
    pub fn label(&self) -> String {
        match self {
            Self::Owner => "You".into(),
            Self::Session { title, .. } => title
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .map_or_else(|| "A session".into(), |t| t.to_string()),
            Self::Workspace { title, .. } => title
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .map_or_else(|| "The workspace".into(), |t| t.to_string()),
            Self::Consolidation { .. } => "Consolidation".into(),
            Self::Notebook => "Notebook".into(),
        }
    }

    /// The chat an agent author wrote from.
    pub fn chat_id(&self) -> Option<&str> {
        match self {
            Self::Session { chat_id, .. } | Self::Workspace { chat_id, .. } => Some(chat_id),
            _ => None,
        }
    }
}

/// One file of a store. `path` starts with `/` and is relative to the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFile {
    pub path: String,
    pub text: String,
    #[serde(default)]
    pub author: MemoryAuthor,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

/// What a change did to its file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MemoryAction {
    Create,
    Edit,
    Delete,
    #[serde(rename_all = "camelCase")]
    Rename {
        from: String,
    },
    /// An earlier change, or a whole pass, put back.
    #[serde(rename_all = "camelCase")]
    Undo {
        change_id: String,
    },
}

/// One entry of a store's change log, newest last in the doc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryChange {
    pub id: String,
    pub at: i64,
    pub author: MemoryAuthor,
    pub path: String,
    pub action: MemoryAction,
    /// One line: what changed ("Added 2 lines", "Removed the file").
    #[serde(default)]
    pub summary: String,
    /// The file's text before the change (clipped), for the log and undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// It replaced text the owner wrote.
    #[serde(default)]
    pub owner_text: bool,
    /// The doc version before the change (hex), to undo it exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
    /// Already undone.
    #[serde(default)]
    pub undone: bool,
}

/// A write held for review: made by a run that had read untrusted content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryPending {
    pub id: String,
    pub at: i64,
    pub author: MemoryAuthor,
    pub path: String,
    /// The memory command (`create`, `str_replace`, …) and its arguments,
    /// applied as given when accepted.
    pub command: String,
    #[serde(default)]
    pub args: serde_json::Value,
    /// Why it is held ("the session fetched a web page").
    pub reason: String,
    /// The file as the write would leave it (`None`: deleted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// The `memory` tool call that made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Everything the app shows of one store (`WatchMemory`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryView {
    pub store: MemoryStore,
    /// `MEMORY.md` first, then by path.
    pub files: Vec<MemoryFile>,
    pub pending: Vec<MemoryPending>,
    /// Newest first.
    pub changes: Vec<MemoryChange>,
}

/// One edit from the app (`EditMemory {scope, edit}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum MemoryEdit {
    /// Create or replace a file.
    Write {
        path: String,
        text: String,
    },
    Delete {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
    /// Apply a pending write.
    Accept {
        id: String,
    },
    Discard {
        id: String,
    },
    /// Put back what one change (or, with a pass id, a whole pass) did.
    #[serde(rename_all = "camelCase")]
    Undo {
        change_id: String,
    },
    #[serde(rename_all = "camelCase")]
    UndoPass {
        pass_id: String,
    },
    SetConsolidation {
        enabled: bool,
        hour: u8,
    },
    /// Ask the store's host to run a pass now.
    RunPass,
}

/// A consolidation pass's hidden chat: run by the engine with only the
/// memory tools, never shown in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryChat {
    pub store_id: String,
    pub scope: MemoryScope,
    pub pass_id: String,
}

/// A subagent profile's memory (`memory:` in its frontmatter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RoleMemory {
    /// One store for the agent, across projects.
    User,
    /// One store per project the agent works in.
    Project,
}

impl RoleMemory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "user" => Some(Self::User),
            "project" => Some(Self::Project),
            _ => None,
        }
    }
}

/// Whether `path` (store-relative) is a usable memory path: starts with
/// `/`, ends in `.md`, no empty, `.` or `..` segments, printable, within
/// [`MEMORY_PATH_MAX`].
pub fn valid_memory_path(path: &str) -> bool {
    path.starts_with('/')
        && path.ends_with(".md")
        && path.len() <= MEMORY_PATH_MAX
        && path[1..]
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != ".." && !seg.starts_with('.'))
        && path
            .chars()
            .all(|c| !c.is_control() && c != '\\' && c != '|')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_ids_are_room_ids() {
        let id_ok = |id: &str| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        };
        for scope in [
            MemoryScope::You,
            MemoryScope::Project {
                space_id: "6f1c".into(),
            },
            MemoryScope::Workspace {
                bot_id: "b1".into(),
            },
            MemoryScope::Role {
                agent: "code reviewer".into(),
                space_id: None,
            },
            MemoryScope::Role {
                agent: "planner".into(),
                space_id: Some("s1".into()),
            },
        ] {
            let id = scope.store_id("ab12");
            assert!(id_ok(&id), "{id}");
        }
        assert_eq!(MemoryScope::You.store_id("ab12"), "mem-u-ab12");
        assert_ne!(
            MemoryScope::You.store_id("ab12"),
            MemoryScope::You.store_id("cd34")
        );
    }

    #[test]
    fn memory_paths_are_markdown_without_escapes() {
        for ok in ["/MEMORY.md", "/build-and-test.md", "/decisions/rounding.md"] {
            assert!(valid_memory_path(ok), "{ok}");
        }
        for bad in [
            "MEMORY.md",
            "/notes.txt",
            "/../x.md",
            "/a//b.md",
            "/.hidden.md",
            "/a\\b.md",
            "/a|b.md",
        ] {
            assert!(!valid_memory_path(bad), "{bad}");
        }
    }

    #[test]
    fn scope_and_edit_wire_shapes() {
        let scope: MemoryScope =
            serde_json::from_value(serde_json::json!({"kind": "project", "spaceId": "s"})).unwrap();
        assert_eq!(
            scope,
            MemoryScope::Project {
                space_id: "s".into()
            }
        );
        let edit: MemoryEdit = serde_json::from_value(
            serde_json::json!({"op": "write", "path": "/MEMORY.md", "text": "x"}),
        )
        .unwrap();
        assert!(matches!(edit, MemoryEdit::Write { .. }));
        assert!(Consolidation::default().enabled);
    }
}
