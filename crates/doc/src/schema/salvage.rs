//! Decoding doc entries: the strict shape first, then field-level salvage so a
//! malformed entry degrades instead of vanishing.

use super::*;

pub(super) fn entry_from_json(v: serde_json::Value) -> Result<SessionMessageEntry, DocError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawEntry {
        id: String,
        role: MessageRole,
        #[serde(default)]
        parts: Vec<DocPartJson>,
        created_at: i64,
        device_id: String,
        #[serde(default)]
        status: Option<MessageStatus>,
        #[serde(default)]
        continuation_of: Option<String>,
        #[serde(default)]
        completed_at: Option<i64>,
        #[serde(default)]
        comments: Vec<MessageComment>,
        #[serde(default)]
        models: Vec<AnsweredModel>,
    }
    match serde_json::from_value::<RawEntry>(v.clone()) {
        Ok(raw) => Ok(SessionMessageEntry {
            id: raw.id,
            role: raw.role,
            parts: raw.parts.into_iter().map(from_doc_part).collect(),
            created_at: raw.created_at,
            device_id: raw.device_id,
            status: raw.status,
            continuation_of: raw.continuation_of,
            completed_at: raw.completed_at,
            comments: raw.comments,
            models: raw.models,
        }),
        // A missing field must cost AT MOST what the field carried — never
        // the entry, never the transcript. Rooms merge writes from every
        // device and app version.
        Err(strict_err) => salvage_entry(v, strict_err),
    }
}

/// Field-level salvage for entries the strict shape rejects. Missing
/// identity/attribution fields get deterministic stand-ins (content-hashed
/// id, so repeated reads and continuation joins stay stable); parts are
/// salvaged individually — a part missing `kind` is inferred from its
/// content shape, and only truly contentless parts are dropped.
fn salvage_entry(
    v: serde_json::Value,
    strict_err: serde_json::Error,
) -> Result<SessionMessageEntry, DocError> {
    let Some(obj) = v.as_object() else {
        return Err(DocError::Json(strict_err));
    };
    let stable_hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        v.to_string().hash(&mut hasher);
        hasher.finish()
    };
    let str_field = |key: &str| obj.get(key).and_then(|x| x.as_str()).map(str::to_owned);
    let id = str_field("id").unwrap_or_else(|| format!("recovered-{stable_hash:016x}"));
    let role = obj
        .get("role")
        .and_then(|r| serde_json::from_value::<MessageRole>(r.clone()).ok())
        .unwrap_or(MessageRole::Assistant);
    let mut parts = Vec::new();
    let mut dropped_parts = 0usize;
    if let Some(raw_parts) = obj.get("parts").and_then(|p| p.as_array()) {
        for (ix, part) in raw_parts.iter().enumerate() {
            match serde_json::from_value::<DocPartJson>(part.clone()) {
                Ok(p) => parts.push(from_doc_part(p)),
                Err(_) => match salvage_part(part, &id, ix) {
                    Some(p) => parts.push(p),
                    None => dropped_parts += 1,
                },
            }
        }
    }
    tracing::warn!(
        entry = %id,
        error = %strict_err,
        salvaged_parts = parts.len(),
        dropped_parts,
        "transcript entry failed strict parse; salvaged"
    );
    Ok(SessionMessageEntry {
        id,
        role,
        parts,
        created_at: obj.get("createdAt").and_then(|x| x.as_i64()).unwrap_or(0),
        device_id: str_field("deviceId").unwrap_or_default(),
        status: obj
            .get("status")
            .and_then(|s| serde_json::from_value(s.clone()).ok()),
        continuation_of: str_field("continuationOf"),
        completed_at: obj.get("completedAt").and_then(|x| x.as_i64()),
        comments: obj
            .get("comments")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            .unwrap_or_default(),
        models: obj
            .get("models")
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default(),
    })
}

/// Salvage one part whose strict `DocPartJson` parse failed: infer the kind
/// from the content shape (`text` → text part, parseable `call` → tool
/// part). `None` only when nothing renderable survives.
fn salvage_part(part: &serde_json::Value, entry_id: &str, ix: usize) -> Option<MessagePart> {
    let obj = part.as_object()?;
    let id = obj
        .get("id")
        .and_then(|x| x.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{entry_id}#recovered-{ix}"));
    if let Some(text) = obj.get("reasoning").and_then(|x| x.as_str()) {
        return Some(MessagePart::Reasoning {
            id,
            text: text.to_owned(),
        });
    }
    if let Some(text) = obj.get("text").and_then(|x| x.as_str()) {
        return Some(MessagePart::Text {
            id,
            text: text.to_owned(),
            agent_text: obj
                .get("agentText")
                .and_then(|x| x.as_str())
                .map(str::to_owned),
        });
    }
    if let Some(call) = obj
        .get("call")
        .and_then(|c| serde_json::from_value(c.clone()).ok())
    {
        return Some(MessagePart::Tool {
            id,
            call,
            is_error: obj
                .get("isError")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            resolved: obj
                .get("resolved")
                .and_then(|x| x.as_bool())
                .unwrap_or(true),
            output: obj
                .get("output")
                .and_then(|x| x.as_str())
                .map(str::to_owned),
            progress: obj
                .get("progress")
                .and_then(|x| x.as_str())
                .map(str::to_owned),
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
        });
    }
    if let Some(message) = obj.get("message").and_then(|x| x.as_str()) {
        return Some(MessagePart::Error {
            id,
            message: message.to_owned(),
        });
    }
    None
}
