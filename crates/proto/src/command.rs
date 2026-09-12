//! Storage-independent command data. Both the native executor and v3 can
//! refer to the complete payload without depending on the legacy document.
use serde::{Deserialize, Serialize};

use crate::{RunRequest, UserInputAnswer};

pub const COMMAND_DEFAULT_TTL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionCommandKind {
    Run,
    Steer,
    Interrupt,
    RespondInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionCommandStatus {
    Pending,
    Applied,
    Rejected,
    Expired,
    Superseded,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionCommandPayload {
    #[serde(rename_all = "camelCase")]
    Run {
        request: RunRequest,
        /// Client-minted optimistic message identity, retained on retry.
        message_id: String,
        /// Effective harness prompt; request.prompt remains the visible text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_prompt: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Steer {
        prompt: String,
        message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_prompt: Option<String>,
    },
    Interrupt {},
    #[serde(rename_all = "camelCase")]
    RespondInput {
        request_id: String,
        answers: Vec<UserInputAnswer>,
    },
}

impl SessionCommandPayload {
    pub fn kind(&self) -> SessionCommandKind {
        match self {
            Self::Run { .. } => SessionCommandKind::Run,
            Self::Steer { .. } => SessionCommandKind::Steer,
            Self::Interrupt {} => SessionCommandKind::Interrupt,
            Self::RespondInput { .. } => SessionCommandKind::RespondInput,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandBasedOn {
    pub turn_id: Option<String>,
    pub frontier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCommandEntry {
    pub id: String,
    pub payload: SessionCommandPayload,
    pub issued_by: String,
    pub issued_at: i64,
    #[serde(default)]
    pub based_on: Option<CommandBasedOn>,
    #[serde(default)]
    pub expires_at: Option<i64>,
    pub status: SessionCommandStatus,
    #[serde(default)]
    pub resolution: Option<String>,
    /// Original user send time, not the time a transport retried delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<i64>,
}

impl SessionCommandEntry {
    pub fn kind(&self) -> SessionCommandKind {
        self.payload.kind()
    }

    pub fn effective_expiry(&self) -> i64 {
        self.expires_at
            .unwrap_or_else(|| self.issued_at.saturating_add(COMMAND_DEFAULT_TTL_MS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_run_payload_roundtrips_without_a_document_dependency() {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/run-command.json")).unwrap();
        let command: SessionCommandEntry = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(command.kind(), SessionCommandKind::Run);
        assert_eq!(serde_json::to_value(&command).unwrap(), value);
        let SessionCommandPayload::Run {
            request,
            agent_prompt,
            message_id,
        } = command.payload
        else {
            panic!("run payload expected");
        };
        assert_eq!(request.prompt, "Visible prompt");
        assert_eq!(
            agent_prompt.as_deref(),
            Some("Selected context\nVisible prompt")
        );
        assert_eq!(message_id, "user-message");
        assert_eq!(request.pending_attachments[0].upload_id, "upload-one");
        assert!(request.worktree.is_some());
    }

    #[test]
    fn expiry_cannot_overflow_on_a_malformed_imported_timestamp() {
        let entry = SessionCommandEntry {
            id: "test".into(),
            payload: SessionCommandPayload::Interrupt {},
            issued_by: "host".into(),
            issued_at: i64::MAX,
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Pending,
            resolution: None,
            sent_at: None,
        };
        assert_eq!(entry.effective_expiry(), i64::MAX);
    }
}
