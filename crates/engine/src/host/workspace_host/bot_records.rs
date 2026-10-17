//! Bot records on the registry (cypher_proto::bot_records): typed writes
//! that send only the fields their writer owns, and the records watch.

use serde::Serialize;
use serde_json::{Map, Value};

use super::*;

/// Which fields of a record a write carries.
#[derive(Debug, Clone, Copy)]
pub enum Fields<'a> {
    /// Every field.
    All,
    /// Only these (camelCase) fields.
    Only(&'a [&'a str]),
    /// Every field but these: the host leaves an owner's control fields
    /// alone, and an owner device leaves the host's state alone.
    Except(&'a [&'a str]),
}

/// `value`'s fields as a row write, filtered by `fields`. A field the value
/// serializes as absent (`None` skipped) is written as `null` when `fields`
/// names it, so clearing reaches other devices.
pub fn record_fields<T: Serialize>(
    value: &T,
    fields: Fields<'_>,
) -> Result<Map<String, Value>, EngineError> {
    let Value::Object(mut map) = serde_json::to_value(value)
        .map_err(|e| EngineError::Other(format!("record encode: {e}")))?
    else {
        return Err(EngineError::Other(
            "a record must serialize as an object".into(),
        ));
    };
    match fields {
        Fields::All => {}
        Fields::Only(keep) => {
            map.retain(|key, _| keep.contains(&key.as_str()));
            for key in keep {
                map.entry(key.to_string()).or_insert(Value::Null);
            }
        }
        Fields::Except(drop) => map.retain(|key, _| !drop.contains(&key.as_str())),
    }
    Ok(map)
}

impl WorkspaceHost {
    /// Write `value`'s `fields` to the `kind` row `id`, creating it if
    /// missing.
    pub fn put_record<T: Serialize>(
        &self,
        kind: &str,
        id: &str,
        value: &T,
        fields: Fields<'_>,
    ) -> Result<(), EngineError> {
        let map = record_fields(value, fields)?;
        Ok(self.mutate(|doc| doc.put_record(kind, id, map))?)
    }

    /// Write raw fields of an existing row; `false` when there is none.
    pub fn update_record(
        &self,
        kind: &str,
        id: &str,
        fields: Map<String, Value>,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.update_record(kind, id, fields))?)
    }

    pub fn delete_records(&self, kind: &str, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        self.mutate(|doc| doc.delete_records(kind, ids));
    }

    /// One record decoded as `T`, overlay applied.
    pub fn record<T: serde::de::DeserializeOwned>(&self, kind: &str, id: &str) -> Option<T> {
        self.read(|doc| doc.record(kind, id))
            .and_then(|value| serde_json::from_value(value).ok())
    }

    pub fn read_bot_records(&self) -> cypher_proto::BotRecords {
        self.read(|doc| doc.read_bot_records())
    }

    /// Every bot record, republished on change (`WatchBotRecords`).
    pub fn watch_bot_records(&self) -> watch::Receiver<cypher_proto::BotRecords> {
        self.inner.records_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Row {
        id: &'static str,
        state: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        decision: Option<bool>,
    }

    #[test]
    fn writes_carry_only_their_writers_fields() {
        let row = Row {
            id: "w",
            state: "running",
            decision: None,
        };
        let host = record_fields(&row, Fields::Except(&["decision"])).unwrap();
        assert_eq!(
            Value::Object(host),
            serde_json::json!({"id": "w", "state": "running"})
        );
        let owner = record_fields(&row, Fields::Only(&["decision"])).unwrap();
        assert_eq!(Value::Object(owner), serde_json::json!({"decision": null}));
    }
}
