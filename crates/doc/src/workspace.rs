//! Registry row shapes shared with [`crate::registry`]: the materialized
//! state (`read_all`), delete-cascade results, and the doc-resident row
//! decoders (epoch-millis timestamps) that turn stored rows into
//! `cypher_proto` entities.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use cypher_proto::{
    Chat, ChatConfig, ChildChat, Device, Session, SessionStatus, Space, SubagentRun,
};

/// Everything in the registry, materialized (`read_all`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceState {
    pub devices: Vec<Device>,
    pub spaces: Vec<Space>,
    pub chats: Vec<Chat>,
    pub sessions: Vec<Session>,
}

/// Result of a `delete_space` cascade — the chat ids removed alongside the
/// space so the engine can drop local run state / doc-host handles.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedSpace {
    pub existed: bool,
    pub chat_ids: Vec<String>,
}

/// Result of unpairing a device: the registry row is tombstoned. Spaces and
/// chats stay put — the machine is kicked out of sync and continues locally.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedDevice {
    pub existed: bool,
}

fn decode_chat_config(value: serde_json::Value) -> Option<ChatConfig> {
    match serde_json::from_value(value) {
        Ok(config) => Some(config),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "unknown or malformed chat config; retaining chat row"
            );
            None
        }
    }
}

fn dt(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or(DateTime::UNIX_EPOCH)
}

// ── doc-resident row shapes (epoch-millis timestamps) ───────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawDevice {
    id: String,
    name: String,
    platform: String,
    #[serde(default)]
    last_seen_at: Option<i64>,
    #[serde(default)]
    created_at: Option<i64>,
    #[serde(default)]
    version: Option<String>,
}

impl From<RawDevice> for Device {
    fn from(raw: RawDevice) -> Self {
        Device {
            id: raw.id,
            name: raw.name,
            platform: raw.platform,
            last_seen_at: raw.last_seen_at.map(dt),
            created_at: raw.created_at.map(dt),
            version: raw.version,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawSpace {
    id: String,
    device_id: String,
    path: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    git_detected: bool,
    #[serde(default)]
    git_checked_at: Option<i64>,
    #[serde(default)]
    checkout_id: Option<String>,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    color: Option<String>,
}

impl From<RawSpace> for Space {
    fn from(raw: RawSpace) -> Self {
        Space {
            icon: raw.icon,
            color: raw.color,
            pinned: raw.pinned,
            id: raw.id,
            device_id: raw.device_id,
            path: raw.path,
            name: raw.name,
            git_detected: raw.git_detected,
            git_checked_at: raw.git_checked_at.map(dt),
            checkout_id: raw.checkout_id,
            created_at: dt(raw.created_at),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawChat {
    id: String,
    device_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    checkout_id: Option<String>,
    #[serde(default)]
    /// Kept as raw JSON so an unknown future harness/config value does not
    /// make the entire chat row undecodable. `Chat::from` degrades only this
    /// optional field to `None`.
    config: Option<serde_json::Value>,
    #[serde(default)]
    last_message_preview: Option<String>,
    #[serde(default)]
    last_message_at: Option<i64>,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    harness_session_id: Option<String>,
    #[serde(default)]
    harness_session_cwd: Option<String>,
    #[serde(default)]
    space_id: Option<String>,
    #[serde(default)]
    last_seen_at: Option<i64>,
    #[serde(default)]
    room_gen: Option<u32>,
    #[serde(default)]
    child: Option<ChildChat>,
    #[serde(default)]
    pinned: bool,
}

impl From<RawChat> for Chat {
    fn from(raw: RawChat) -> Self {
        Chat {
            pinned: raw.pinned,
            id: raw.id,
            device_id: raw.device_id,
            title: raw.title,
            archived: raw.archived,
            cwd: raw.cwd,
            branch: raw.branch,
            checkout_id: raw.checkout_id,
            config: raw.config.and_then(decode_chat_config),
            last_message_preview: raw.last_message_preview,
            last_message_at: raw.last_message_at.map(dt),
            created_at: dt(raw.created_at),
            harness_session_id: raw.harness_session_id,
            harness_session_cwd: raw.harness_session_cwd,
            space_id: raw.space_id,
            last_seen_at: raw.last_seen_at.map(dt),
            room_gen: raw.room_gen,
            child: raw.child,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawSession {
    chat_id: String,
    device_id: String,
    status: SessionStatus,
    #[serde(default)]
    started_at: Option<i64>,
    #[serde(default)]
    updated_at: i64,
    /// Live subagent runs (pi `cypher.subagents.v1` projection); absent on old
    /// rows and old writers → empty.
    #[serde(default)]
    subagents: Option<Vec<SubagentRun>>,
    /// Context-window gauge (`{used, size}`); absent on old rows and old
    /// writers. Parsed leniently: a malformed value reads as no reading
    /// instead of dropping the whole status row.
    #[serde(default)]
    context_usage: Option<serde_json::Value>,
    /// The running turn's latest throughput reading; absent on settled rows,
    /// old rows, and old writers. Parsed leniently like `context_usage`.
    #[serde(default)]
    throughput: Option<serde_json::Value>,
}

impl From<RawSession> for Session {
    fn from(raw: RawSession) -> Self {
        Session {
            chat_id: raw.chat_id,
            device_id: raw.device_id,
            status: raw.status,
            started_at: raw.started_at.map(dt),
            updated_at: dt(raw.updated_at),
            subagents: raw.subagents.unwrap_or_default(),
            context_usage: raw
                .context_usage
                .and_then(|value| serde_json::from_value(value).ok()),
            throughput: raw
                .throughput
                .and_then(|value| serde_json::from_value(value).ok()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_future_harness_keeps_chat_row_readable() {
        // Raw workspace rows store timestamps as epoch milliseconds (the
        // typed Chat serde representation uses RFC3339 strings), so construct
        // the row at the wire shape rather than round-tripping Chat directly.
        let value = serde_json::json!({
            "id": "future-chat",
            "deviceId": "dev-a",
            "title": "Future chat",
            "archived": false,
            "cwd": "/tmp/repo",
            "createdAt": 2_000,
            "config": {
                "harness": "future-harness",
                "model": "future-model",
                "reasoning": null,
                "modelOptions": {},
                "sandbox": "workspace-write"
            }
        });
        let raw: RawChat = serde_json::from_value(value).expect("row envelope remains readable");
        let decoded = Chat::from(raw);
        assert_eq!(decoded.id, "future-chat");
        assert_eq!(decoded.device_id, "dev-a");
        assert!(
            decoded.config.is_none(),
            "only the unknown config should degrade"
        );
    }
}
