use serde::{Deserialize, Deserializer, de::Error};
use serde_json::Value;
use std::sync::LazyLock;

use super::{canonical_json, command, validate_json};
use crate::MessagePart;

static PART_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../apps/ios/Cypher/Sync/Sync3PartSchema.json"
    ))
    .expect("checked-in part descriptor")
});

pub(super) fn validate_part(part: &MessagePart) -> Result<(), &'static str> {
    let value = serde_json::to_value(part).map_err(|_| "invalid_part")?;
    validate_json(&value, 3)?;
    if !command::matches(&value, &PART_SCHEMA) {
        return Err("invalid_part");
    }
    if let MessagePart::Tool { call, .. } = part
        && crate::parts::sanitize_tool_call(call) != *call
    {
        return Err("private_tool_input");
    }
    Ok(())
}

pub(super) fn deserialize_part<'de, D: Deserializer<'de>>(d: D) -> Result<MessagePart, D::Error> {
    let mut value = Value::deserialize(d)?;
    validate_json(&value, 3).map_err(D::Error::custom)?;
    canonical_json(&mut value);
    if !command::matches(&value, &PART_SCHEMA) {
        return Err(D::Error::custom("invalid_part"));
    }
    let part = serde_json::from_value(value).map_err(D::Error::custom)?;
    validate_part(&part).map_err(D::Error::custom)?;
    Ok(part)
}

pub(super) fn validate_replacement(
    old: &MessagePart,
    new: &MessagePart,
) -> Result<(), &'static str> {
    if old.id() != new.id() || std::mem::discriminant(old) != std::mem::discriminant(new) {
        return Err("part_identity_mismatch");
    }
    match (old, new) {
        (MessagePart::Text { text: a, .. }, MessagePart::Text { text: b, .. }) if a != b => {
            Err("text_requires_delta")
        }
        (
            MessagePart::Tool { resolved: true, .. },
            MessagePart::Tool {
                resolved: false, ..
            },
        ) => Err("part_resolved"),
        (
            MessagePart::Input {
                request_id: a,
                questions: aq,
                resolved: ar,
                ..
            },
            MessagePart::Input {
                request_id: b,
                questions: bq,
                resolved: br,
                ..
            },
        ) => {
            if a != b || aq != bq {
                Err("question_changed")
            } else if *ar && !*br {
                Err("part_resolved")
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}
