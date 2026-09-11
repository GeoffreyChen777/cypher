//! Shared, closed command shape descriptor (not a general JSON Schema engine).
//! The canonical JSON is also an iOS bundle resource and an Edge import.
use serde::{Deserialize, Deserializer, de::Error};
use serde_json::Value;
use std::sync::LazyLock;

use super::{MAX_SAFE_INTEGER, canonical_json, valid_entity_id, valid_id, validate_json};
use crate::{SessionCommandEntry, SessionCommandPayload, SessionCommandStatus};

static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../apps/ios/Cypher/Sync/Sync3CommandSchema.json"
    ))
    .expect("checked-in command shape descriptor")
});

fn matches(value: &Value, schema: &Value) -> bool {
    if let Some(kind) = schema.as_str() {
        return match kind {
            "id" => value.as_str().is_some_and(valid_id),
            "entityId" => value.as_str().is_some_and(valid_entity_id),
            "string" => value.is_string(),
            "nonemptyString" => value.as_str().is_some_and(|s| !s.is_empty()),
            "uint" => value.as_u64().is_some_and(|n| n <= MAX_SAFE_INTEGER),
            "bool" => value.is_boolean(),
            "null" => value.is_null(),
            "json" => true, // Whole-value depth, number and byte checks are separate.
            _ => false,
        };
    }
    if let Some(inner) = schema.get("nullable") {
        return value.is_null() || matches(value, inner);
    }
    if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
        return variants.iter().filter(|s| matches(value, s)).count() == 1;
    }
    if let Some(variants) = schema.get("enum").and_then(Value::as_array) {
        return variants.contains(value);
    }
    let min = schema["min"].as_u64().unwrap_or(0) as usize;
    let max = schema["max"].as_u64().unwrap_or(256) as usize;
    if let Some(inner) = schema.get("array") {
        return value.as_array().is_some_and(|a| {
            a.len() >= min && a.len() <= max && a.iter().all(|v| matches(v, inner))
        });
    }
    if let Some(inner) = schema.get("map") {
        return value
            .as_object()
            .is_some_and(|m| m.len() <= max && m.values().all(|v| matches(v, inner)));
    }
    if let Some(fields) = schema.get("object").and_then(Value::as_object) {
        let optional = schema["optional"].as_array();
        return value.as_object().is_some_and(|m| {
            m.keys().all(|k| fields.contains_key(k))
                && fields.iter().all(|(k, s)| match m.get(k) {
                    Some(v) => matches(v, s),
                    None => optional.is_some_and(|a| a.contains(&Value::String(k.clone()))),
                })
        });
    }
    false
}

pub(super) fn validate(command: &SessionCommandEntry) -> Result<(), &'static str> {
    let value = serde_json::to_value(command).map_err(|_| "invalid_command")?;
    validate_json(&value, 3)?;
    if !matches(&value, &SCHEMA) {
        return Err("invalid_command");
    }
    if command.status != SessionCommandStatus::Pending || command.resolution.is_some() {
        return Err("command_not_pending");
    }
    if matches!(command.payload, SessionCommandPayload::Interrupt {})
        && command
            .based_on
            .as_ref()
            .and_then(|b| b.turn_id.as_ref())
            .is_none()
    {
        return Err("interrupt_requires_target");
    }
    Ok(())
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<SessionCommandEntry, D::Error> {
    let mut value = Value::deserialize(d)?;
    validate_json(&value, 3).map_err(D::Error::custom)?;
    canonical_json(&mut value);
    // Check BEFORE serde can discard unknown properties or default omitted
    // fields. Optional fields must have their canonical non-null form.
    if !matches(&value, &SCHEMA) {
        return Err(D::Error::custom("invalid_command"));
    }
    let command = serde_json::from_value(value).map_err(D::Error::custom)?;
    validate(&command).map_err(D::Error::custom)?;
    Ok(command)
}
