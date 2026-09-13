//! Selected-text Side Chat wire types (round 21).
//!
//! A Side Chat is a TEMPORARY engine-hosted chat opened from a settled
//! selection (transcript / git diff / terminal). Until promoted it lives only
//! in engine memory — no workspace Chat row, no public WatchSessions entry,
//! no SQLite snapshot, no chat2 room, no run journal. Promotion turns it into
//! a normal root chat with the same id and transcript.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::SessionStatus;

/// Where a Side Chat was opened from — the surface metadata labels and the
/// engine's source-context window. Carried from the offering surface to
/// `StartSideChat`; never instructions, only context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SideChatSource {
    /// A settled markdown selection in the source chat's transcript. The
    /// anchor message id names the selected message; the engine includes the
    /// source transcript's newest whole messages through that anchor.
    #[serde(rename_all = "camelCase")]
    Transcript {
        /// The id of the message whose text the selection settles in. `None`
        /// (a row-level key that no longer resolves) falls back to the source
        /// transcript tail with safe labeling.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        anchor_message_id: Option<String>,
    },
    /// A settled selection in a Git-diff pane.
    #[serde(rename_all = "camelCase")]
    GitDiff {
        /// The diff pane's scope label (e.g. "Working tree", "main").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        /// The selected file's path, when the selection resolves to one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_path: Option<String>,
    },
    /// A settled selection in a terminal.
    #[serde(rename_all = "camelCase")]
    Terminal {
        /// The active terminal tab's title, when one is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
}

impl SideChatSource {
    /// A short human label for the panel's context preview header.
    pub fn label(&self) -> &'static str {
        match self {
            SideChatSource::Transcript { .. } => "Transcript selection",
            SideChatSource::GitDiff { .. } => "Diff selection",
            SideChatSource::Terminal { .. } => "Terminal selection",
        }
    }
}

/// `StartSideChat` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideChatCreated {
    pub side_chat_id: String,
    /// The source chat the side chat was opened from (its host device owns
    /// the side chat; all side-chat RPCs carry `targetDeviceId`).
    pub parent_chat_id: String,
    /// The device hosting the side chat — the `targetDeviceId` every
    /// side-chat RPC must carry.
    pub target_device_id: String,
}

/// `WatchSideChatStatus` frame: the side chat's PRIVATE live status. Temporary
/// side chats never appear in the public `WatchSessions` stream; this watch is
/// the only status channel for a temp panel. After promotion the same watch
/// keeps streaming until the panel is replaced by the normal chat surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideChatStatus {
    pub side_chat_id: String,
    pub status: SessionStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// `PromoteSideChat` reply — the promoted chat's id (idempotent: a lost reply
/// followed by a retry returns the same chat without double-promoting).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideChatPromoted {
    pub chat_id: String,
}
