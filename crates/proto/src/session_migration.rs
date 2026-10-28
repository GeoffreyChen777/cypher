//! Session migration wire types.
//!
//! A migration moves root chats, together with their Cypher child chats, from
//! one project to another — on the same device or on another device of the
//! same account. The UI asks the chats' CURRENT host (`MigrateChats`, routed
//! by `targetDeviceId`); that host drives the destination with the
//! engine-to-engine calls below, so the Pi session, the run journal and the
//! transcript's images travel host to host and never through the UI.
//!
//! Order of one migration (one root chat and its children):
//! 1. the source freezes the chats' command drains and parks their runs;
//! 2. `BeginChatMigration` — the destination checks it owns the project and
//!    opens a staging folder;
//! 3. `WriteChatMigrationChunk`* — the files, in relay-sized base64 slices;
//! 4. `CommitChatMigration` — the destination installs the files (the Pi
//!    session header names the new folder), writes each chat's [`ChatHome`]
//!    and becomes the host;
//! 5. the source writes the same homes, drops its copies and thaws.
//!
//! A failure before the commit (or `AbortChatMigration`) leaves the chats
//! exactly where they were.

use serde::{Deserialize, Serialize};

/// Where a chat runs after a migration: the registry fields that move
/// together (see `RegistryDoc::rehome_chat`). An empty `harness_session_id`
/// is the "do not resume" tombstone: the chat moved without a Pi session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatHome {
    pub device_id: String,
    pub space_id: String,
    pub cwd: String,
    pub harness_session_id: String,
    pub harness_session_cwd: String,
}

/// `MigrateChats` request (stream), answered by the chats' current host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMigrationRequest {
    /// Root chats to move. Child chats always follow their parent.
    #[serde(default)]
    pub chat_ids: Vec<String>,
    /// Move every root chat of this project instead (archived ones too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_space_id: Option<String>,
    /// The destination project; its device becomes the chats' host.
    pub to_space_id: String,
}

/// `MigrateChats` stream items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChatMigrationEvent {
    /// Emitted once, before anything moves.
    Start { total: usize },
    /// One root chat settled: moved (`error: None`) or left where it was.
    Chat {
        chat_id: String,
        title: Option<String>,
        index: usize,
        total: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Terminal summary — the stream's last item.
    Done { moved: usize, failed: usize },
}

/// `BeginChatMigration` (engine to engine).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationBegin {
    pub migration_id: String,
    pub to_space_id: String,
}

/// `BeginChatMigration` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationBegun {
    /// The folder the chats will run in: the project path, `~` expanded on
    /// the destination.
    pub cwd: String,
}

/// `WriteChatMigrationChunk` (engine to engine): one slice of a staged file.
/// `offset` must equal the bytes staged so far, so a retried slice cannot
/// double-append.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationChunk {
    pub migration_id: String,
    /// Staging key (`[A-Za-z0-9_-]`), unique within the migration.
    pub file: String,
    pub offset: u64,
    /// Base64 of the slice.
    pub data: String,
}

/// A staged file and the name it had on the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigratedFile {
    pub file: String,
    pub name: String,
}

/// One chat of a [`MigrationCommit`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigratedChat {
    pub chat_id: String,
    /// The Pi session the chat resumes, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<MigratedFile>,
    /// The run journal, when one was sent (never for same-device moves).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
    /// Folders a message queued before the move may still name (the old
    /// row's cwd, as written and as expanded on the source). The new host
    /// runs such a message in the destination folder instead.
    #[serde(default)]
    pub previous_cwds: Vec<String>,
}

/// A transcript image copied with the chat. The destination serves it under
/// its old path, which the transcript keeps naming.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigratedAttachment {
    pub file: String,
    pub source_path: String,
}

/// `CommitChatMigration` (engine to engine).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationCommit {
    pub migration_id: String,
    pub to_space_id: String,
    pub chats: Vec<MigratedChat>,
    #[serde(default)]
    pub attachments: Vec<MigratedAttachment>,
}

/// `CommitChatMigration` reply: each chat's new home, already written on the
/// destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationCommitted {
    pub homes: Vec<MigratedHome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigratedHome {
    pub chat_id: String,
    pub home: ChatHome,
}

/// `AbortChatMigration` (engine to engine): drop the staging folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationAbort {
    pub migration_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialize_camel_case() {
        let chat = ChatMigrationEvent::Chat {
            chat_id: "c1".into(),
            title: Some("T".into()),
            index: 1,
            total: 2,
            error: None,
        };
        assert_eq!(
            serde_json::to_value(&chat).unwrap(),
            serde_json::json!({
                "kind": "chat", "chatId": "c1", "title": "T", "index": 1, "total": 2
            })
        );
        let done = ChatMigrationEvent::Done {
            moved: 1,
            failed: 0,
        };
        assert_eq!(
            serde_json::to_value(&done).unwrap(),
            serde_json::json!({ "kind": "done", "moved": 1, "failed": 0 })
        );
    }

    #[test]
    fn request_accepts_a_project_without_chat_ids() {
        let request: ChatMigrationRequest = serde_json::from_value(serde_json::json!({
            "fromSpaceId": "s1", "toSpaceId": "s2"
        }))
        .unwrap();
        assert!(request.chat_ids.is_empty());
        assert_eq!(request.from_space_id.as_deref(), Some("s1"));
    }
}
