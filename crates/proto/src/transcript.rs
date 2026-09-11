//! Rendered transcript entries, independent of the persistence/sync backend.
use serde::{Deserialize, Serialize};

use crate::{MessagePart, MessageStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageEntry {
    pub id: String,
    pub role: MessageRole,
    pub parts: Vec<MessagePart>,
    pub created_at: i64,
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<MessageStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_of: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn render_projection_strips_inputs_but_preserves_the_tool_record() {
        let parts = vec![MessagePart::Tool {
            id: "write".into(),
            call: crate::ToolCall::WriteFile {
                path: "file".into(),
                content: Some("private fixture input".into()),
            },
            is_error: false,
            resolved: true,
            output: Some("File written".into()),
            progress: None,
            diff: None,
            output_ref: Some("chat/write".into()),
            output_bytes: Some(512),
            diff_ref: Some("chat/write.diff".into()),
            diff_stats: None,
        }];
        let mut expected = parts.clone();
        if let MessagePart::Tool { call, .. } = &mut expected[0] {
            *call = crate::ToolCall::WriteFile {
                path: "file".into(),
                content: None,
            };
        }
        assert_eq!(crate::parts::render_parts(&parts), expected);
        assert_ne!(
            parts, expected,
            "rendering must not mutate the local journal's inputs"
        );
    }

    #[test]
    fn transcript_roundtrip_retains_system_role_utf8_and_continuation() {
        let entry = SessionMessageEntry {
            id: crate::parts::continuation_id("message", 1),
            role: MessageRole::System,
            parts: vec![MessagePart::Text {
                id: "text".into(),
                text: "你好".into(),
            }],
            created_at: 1000,
            device_id: "host".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: Some("message".into()),
        };
        assert_eq!(entry.parts[0].byte_len(), 6);
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["role"], "system");
        assert_eq!(value["id"], "message#c1");
        assert_eq!(value["continuationOf"], "message");
        assert_eq!(
            serde_json::from_value::<SessionMessageEntry>(value).unwrap(),
            entry
        );
    }
}
