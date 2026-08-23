//! Session Fork (v1) wire types.
//!
//! A Session Fork clones a settled prefix of one chat's transcript into a NEW
//! durable root Chat on the source chat's host device, backed by a fresh Pi
//! session (forked before a user message / cloned at the current leaf). The
//! source chat, its session, and its context are never mutated.
//!
//! The request id is CLIENT-MINTED as the target chat id: a lost-reply retry
//! with the same request id returns the SAME target chat instead of minting a
//! twin public chat (the engine checks for an existing target row first).

use serde::{Deserialize, Serialize};

use crate::Chat;

/// `ForkSession` request. `request_id` is client-minted and BECOMES the
/// target chat's id — the idempotence key for lost-reply retries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionForkRequest {
    /// Client-minted target chat id (the idempotence key; the created chat
    /// row carries exactly this id).
    pub request_id: String,
    /// The source chat whose settled transcript prefix is forked.
    pub source_chat_id: String,
    /// The settled USER/ASSISTANT message the fork anchors at.
    pub anchor_message_id: String,
}

/// The fork boundary the engine resolved for the clicked anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionForkMode {
    /// Fork BEFORE the clicked user message: the target transcript copies
    /// entries before it and the composer is prefilled with the clicked
    /// visible user text.
    EditUser,
    /// Fork AFTER the clicked assistant response: the target transcript
    /// copies through it and the composer starts blank.
    ContinueAfterAssistant,
}

/// `ForkSession` success reply: the authoritative target [`Chat`] row plus
/// the resolved boundary and the optional composer prefill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionForkCreated {
    /// The authoritative target chat row (durable, sidebar-visible, selected
    /// by the caller). Same device/space/cwd/branch/checkout/config/sandbox
    /// as the source; fresh harness session path; `roomGen: 2`.
    pub chat: Chat,
    /// Which boundary was materialized.
    pub mode: SessionForkMode,
    /// Composer prefill for `EditUser` forks (the clicked user's visible
    /// text); `None` for `ContinueAfterAssistant` (blank composer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer_text: Option<String>,
}

/// Why a fork could not be materialized — the UI renders this directly
/// instead of a generic RPC failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionForkUnavailableReason {
    /// The source chat is not a Pi chat (forking is Pi-only in v1).
    NonPi,
    /// The source chat is a Cypher child subagent chat.
    ChildChat,
    /// The source chat is a temporary Side Chat.
    TemporarySideChat,
    /// The source chat has a live/awaiting run that cannot be quiesced.
    LiveSession,
    /// The source chat has no stored harness session to fork from.
    MissingSession,
    /// The source chat is not hosted on this engine (no host).
    MissingHost,
    /// The anchor/its boundary cannot be represented in the source session.
    BoundaryUnavailable,
    /// The hosting engine is too old to serve the fork.
    Unsupported,
}

/// Typed `ForkSession` failure. The engine prefers this over an RPC `Failed`
/// string so the UI can show a precise, actionable notice; `Failed` is
/// reserved for genuinely unexpected errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionForkUnavailable {
    pub reason: SessionForkUnavailableReason,
    pub message: String,
}

/// `ForkSession` reply envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionForkResponse {
    Created(SessionForkCreated),
    Unavailable(SessionForkUnavailable),
}

/// Pi-native fork boundary (harness seam, not a wire RPC type). `BeforeUser`
/// indexes into the ordered Cypher visible USER prompts carried alongside;
/// `CloneLeaf` duplicates the active branch at its current leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PiForkBoundary {
    /// Fork BEFORE the mapped Pi user entry for `visible_user_prompts[index]`.
    BeforeUser(usize),
    /// Clone the active branch at its current leaf.
    CloneLeaf,
}

/// Pi-native session fork request (engine → [`Harness`]; Pi-only — other
/// harnesses answer Unsupported).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiSessionForkRequest {
    /// Absolute path of the source pi session file (the chat's stored
    /// `harnessSessionId`).
    pub source_session_path: String,
    /// The source chat's ordered visible USER prompt strings (image
    /// attachment trailers stripped), oldest → newest.
    pub visible_user_prompts: Vec<String>,
    pub boundary: PiForkBoundary,
}

/// Pi-native session fork result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiSessionForkResult {
    /// Absolute path of the NEW pi session file (from `get_state.sessionFile`),
    /// verified non-empty and different from the source — `None` for an
    /// EMPTY-CONTEXT fork BEFORE THE FIRST USER, which real pi does not
    /// persist until the first user message lands (the target chat then
    /// starts its own fresh session from empty context on first send).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatConfig, HarnessId, SandboxLevel, SessionForkMode};

    fn sample_chat() -> Chat {
        Chat {
            id: "fork-1".into(),
            device_id: "dev-1".into(),
            title: Some("My chat — Fork".into()),
            archived: false,
            cwd: Some("/tmp/proj".into()),
            branch: Some("main".into()),
            checkout_id: Some("ck-1".into()),
            config: Some(ChatConfig {
                harness: HarnessId::Pi,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            }),
            last_message_preview: None,
            last_message_at: None,
            created_at: chrono::Utc::now(),
            harness_session_id: Some("sessions/new.jsonl".into()),
            harness_session_cwd: Some("/tmp/proj".into()),
            space_id: Some("sp-1".into()),
            last_seen_at: None,
            room_gen: Some(2),
            child: None,
        }
    }

    #[test]
    fn request_round_trips_camel_cased() {
        let req = SessionForkRequest {
            request_id: "fork-1".into(),
            source_chat_id: "chat-9".into(),
            anchor_message_id: "msg-42".into(),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["requestId"], "fork-1");
        assert_eq!(json["sourceChatId"], "chat-9");
        assert_eq!(json["anchorMessageId"], "msg-42");
        let back: SessionForkRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn created_response_round_trips() {
        let created = SessionForkCreated {
            chat: sample_chat(),
            mode: SessionForkMode::EditUser,
            composer_text: Some("fix the bug".into()),
        };
        let resp = SessionForkResponse::Created(created);
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["kind"], "created");
        assert_eq!(json["mode"], "editUser");
        assert_eq!(json["composerText"], "fix the bug");
        assert_eq!(json["chat"]["id"], "fork-1");
        let back: SessionForkResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn continue_after_assistant_has_no_composer_text() {
        let created = SessionForkCreated {
            chat: sample_chat(),
            mode: SessionForkMode::ContinueAfterAssistant,
            composer_text: None,
        };
        let json = serde_json::to_value(&created).unwrap();
        // The optional field is skipped, not null.
        assert!(json.get("composerText").is_none());
        assert_eq!(json["mode"], "continueAfterAssistant");
        let back: SessionForkCreated = serde_json::from_value(json).unwrap();
        assert_eq!(back.composer_text, None);
    }

    #[test]
    fn unavailable_round_trips() {
        let unavailable = SessionForkResponse::Unavailable(SessionForkUnavailable {
            reason: SessionForkUnavailableReason::LiveSession,
            message: "The source chat is still running.".into(),
        });
        let json = serde_json::to_value(&unavailable).unwrap();
        assert_eq!(json["kind"], "unavailable");
        assert_eq!(json["reason"], "liveSession");
        let back: SessionForkResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back, unavailable);
    }

    #[test]
    fn pi_result_round_trips_with_optional_session_path() {
        let with = PiSessionForkResult {
            session_path: Some("/sessions/new.jsonl".into()),
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["sessionPath"], "/sessions/new.jsonl");
        let back: PiSessionForkResult = serde_json::from_value(json).unwrap();
        assert_eq!(back, with);

        // Empty-context first-user forks carry no session path; the field is
        // skipped (not null) so the wire shape stays unambiguous.
        let none = PiSessionForkResult { session_path: None };
        let json = serde_json::to_value(&none).unwrap();
        assert!(json.get("sessionPath").is_none(), "{json}");
        let back: PiSessionForkResult = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_path, None);
    }

    #[test]
    fn pi_request_round_trips() {
        let req = PiSessionForkRequest {
            source_session_path: "/sessions/a.jsonl".into(),
            visible_user_prompts: vec!["one".into(), "two".into()],
            boundary: PiForkBoundary::BeforeUser(1),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["sourceSessionPath"], "/sessions/a.jsonl");
        assert_eq!(json["boundary"]["beforeUser"], 1);
        let back: PiSessionForkRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.boundary, PiForkBoundary::BeforeUser(1));

        let clone = PiSessionForkRequest {
            boundary: PiForkBoundary::CloneLeaf,
            ..req
        };
        let json = serde_json::to_value(&clone).unwrap();
        assert_eq!(json["boundary"]["cloneLeaf"], serde_json::Value::Null);
        let back: PiSessionForkRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.boundary, PiForkBoundary::CloneLeaf);
    }
}
