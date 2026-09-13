//! Disposable native metadata view and mutation draft.
//!
//! This is not a replica or an outbox. Only a clone owned by a single local
//! transaction may stage operations; callers publish rows returned by their
//! durable journal after commit. There are no ACK, snapshot or reseed methods.
use super::{MetadataRow, OpKind, RowOp};
use crate::{Chat, ChatConfig, Device, Session, Space};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct MetadataError(pub String);
impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for MetadataError {}
impl From<serde_json::Error> for MetadataError {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceState {
    pub devices: Vec<Device>,
    pub spaces: Vec<Space>,
    pub chats: Vec<Chat>,
    pub sessions: Vec<Session>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedSpace {
    pub existed: bool,
    pub chat_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedDevice {
    pub existed: bool,
}

#[derive(Clone)]
pub struct MetadataView {
    actor: String,
    rows: BTreeMap<(String, String), MetadataRow>,
    staged: Vec<RowOp>,
}

const DATES: &[&str] = &["createdAt", "lastSeenAt", "gitCheckedAt", "lastMessageAt"];
fn fields<T: Serialize>(
    entity: &T,
    optional: &[&str],
) -> Result<BTreeMap<String, Value>, MetadataError> {
    let Value::Object(mut value) = serde_json::to_value(entity)? else {
        return Err(MetadataError("invalid_metadata_entity".into()));
    };
    for field in optional {
        value.entry((*field).to_owned()).or_insert(Value::Null);
    }
    for field in DATES {
        if let Some(Value::String(time)) = value.get(*field) {
            let millis = DateTime::parse_from_rfc3339(time)
                .map_err(|_| MetadataError("invalid_metadata_date".into()))?
                .timestamp_millis();
            value.insert((*field).into(), json!(millis));
        }
    }
    Ok(value.into_iter().collect())
}
fn decode<T: DeserializeOwned>(row: &MetadataRow) -> Result<T, MetadataError> {
    let mut fields = row.fields.clone();
    if row.kind == "chats" {
        fields.entry("archived".into()).or_insert(json!(false));
    }
    if matches!(row.kind.as_str(), "chats" | "spaces") {
        fields.entry("createdAt".into()).or_insert(json!(0));
    }
    for field in DATES {
        if let Some(time) = fields.get(*field).and_then(Value::as_i64) {
            let date = DateTime::<Utc>::from_timestamp_millis(time)
                .ok_or_else(|| MetadataError("invalid_metadata_date".into()))?;
            fields.insert((*field).into(), serde_json::to_value(date)?);
        }
    }
    Ok(serde_json::from_value(Value::Object(
        fields.into_iter().collect(),
    ))?)
}
impl MetadataView {
    pub fn new(actor: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            rows: BTreeMap::new(),
            staged: vec![],
        }
    }
    pub fn device_id(&self) -> &str {
        &self.actor
    }
    pub fn replace_rows(&mut self, rows: impl IntoIterator<Item = MetadataRow>) {
        for row in rows {
            self.rows.insert((row.kind.clone(), row.id.clone()), row);
        }
    }
    pub fn take_operations(&mut self) -> Vec<RowOp> {
        std::mem::take(&mut self.staged)
    }
    fn row(&self, kind: &str, id: &str) -> Option<&MetadataRow> {
        self.rows
            .get(&(kind.into(), id.into()))
            .filter(|r| !r.deleted)
    }
    fn read<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>, MetadataError> {
        self.rows
            .range((kind.into(), String::new())..)
            .take_while(|((k, _), _)| k == kind)
            .filter(|(_, row)| !row.deleted)
            .map(|(_, row)| decode(row))
            .collect()
    }
    fn write(&mut self, kind: &str, id: &str, op: OpKind, set: BTreeMap<String, Value>) {
        if op == OpKind::Update && self.row(kind, id).is_none() {
            return;
        }
        let key = (kind.into(), id.into());
        let row = self.rows.entry(key).or_insert_with(|| MetadataRow {
            kind: kind.into(),
            id: id.into(),
            seq: 0,
            deleted: false,
            del_hlc: None,
            fields: BTreeMap::new(),
            clocks: BTreeMap::new(),
        });
        if row.deleted {
            row.fields.clear();
        }
        row.deleted = false;
        for (field, value) in &set {
            if value.is_null() {
                row.fields.remove(field);
            } else {
                row.fields.insert(field.clone(), value.clone());
            }
        }
        // The journal allocates the HLC in the same transaction as its outbox.
        self.staged.push(RowOp {
            kind: kind.into(),
            id: id.into(),
            op,
            set: Some(set),
            hlc: String::new(),
        });
    }
    fn update(&mut self, kind: &str, id: &str, set: Value) -> Result<bool, MetadataError> {
        if self.row(kind, id).is_none() {
            return Ok(false);
        }
        let Value::Object(set) = set else {
            return Err(MetadataError("invalid_metadata_update".into()));
        };
        self.write(kind, id, OpKind::Update, set.into_iter().collect());
        Ok(true)
    }
    fn delete(&mut self, kind: &str, id: &str) -> bool {
        let existed = self.row(kind, id).is_some();
        let row = self
            .rows
            .entry((kind.into(), id.into()))
            .or_insert_with(|| MetadataRow {
                kind: kind.into(),
                id: id.into(),
                seq: 0,
                deleted: true,
                del_hlc: None,
                fields: BTreeMap::new(),
                clocks: BTreeMap::new(),
            });
        row.deleted = true;
        row.fields.clear();
        row.clocks.clear();
        self.staged.push(RowOp {
            kind: kind.into(),
            id: id.into(),
            op: OpKind::Delete,
            set: None,
            hlc: String::new(),
        });
        existed
    }
    pub fn device_is_tombstoned(&self, id: &str) -> bool {
        self.rows
            .get(&("devices".into(), id.into()))
            .is_some_and(|r| r.deleted)
    }
    pub fn upsert_device(&mut self, value: &Device) -> Result<(), MetadataError> {
        self.write(
            "devices",
            &value.id,
            OpKind::Upsert,
            fields(value, &["lastSeenAt", "createdAt", "version"])?,
        );
        Ok(())
    }
    pub fn upsert_space(&mut self, value: &Space) -> Result<(), MetadataError> {
        self.write(
            "spaces",
            &value.id,
            OpKind::Upsert,
            fields(value, &["name", "gitCheckedAt", "checkoutId"])?,
        );
        Ok(())
    }
    pub fn upsert_chat(&mut self, value: &Chat) -> Result<(), MetadataError> {
        self.write(
            "chats",
            &value.id,
            OpKind::Upsert,
            fields(
                value,
                &[
                    "title",
                    "cwd",
                    "branch",
                    "checkoutId",
                    "config",
                    "lastMessagePreview",
                    "lastMessageAt",
                    "harnessSessionId",
                    "harnessSessionCwd",
                    "spaceId",
                    "lastSeenAt",
                    "roomGen",
                    "child",
                ],
            )?,
        );
        Ok(())
    }
    pub fn claim_chat(
        &mut self,
        id: &str,
        cwd: Option<&str>,
        space: Option<&str>,
        at: DateTime<Utc>,
    ) {
        let mut set = BTreeMap::from([
            ("id".into(), json!(id)),
            ("deviceId".into(), json!(self.actor)),
            ("createdAt".into(), json!(at.timestamp_millis())),
        ]);
        if let Some(cwd) = cwd {
            set.insert("cwd".into(), json!(cwd));
        }
        if let Some(space) = space {
            set.insert("spaceId".into(), json!(space));
        }
        self.write("chats", id, OpKind::Upsert, set);
    }
    pub fn chat(&self, id: &str) -> Result<Option<Chat>, MetadataError> {
        self.row("chats", id).map(decode).transpose()
    }
    pub fn space(&self, id: &str) -> Result<Option<Space>, MetadataError> {
        self.row("spaces", id).map(decode).transpose()
    }
    pub fn read_devices(&self) -> Result<Vec<Device>, MetadataError> {
        self.read("devices")
    }
    pub fn read_spaces(&self) -> Result<Vec<Space>, MetadataError> {
        self.read("spaces")
    }
    pub fn read_chats(&self) -> Result<Vec<Chat>, MetadataError> {
        self.read("chats")
    }
    pub fn read_all(&self) -> Result<WorkspaceState, MetadataError> {
        Ok(WorkspaceState {
            devices: self.read_devices()?,
            spaces: self.read_spaces()?,
            chats: self.read_chats()?,
            sessions: vec![],
        })
    }
    pub fn rename_device(&mut self, id: &str, name: &str) -> Result<bool, MetadataError> {
        self.update("devices", id, json!({"name":name}))
    }
    pub fn set_device_last_seen(
        &mut self,
        id: &str,
        at: DateTime<Utc>,
    ) -> Result<bool, MetadataError> {
        self.update("devices", id, json!({"lastSeenAt":at.timestamp_millis()}))
    }
    pub fn rename_space(&mut self, id: &str, name: Option<&str>) -> Result<bool, MetadataError> {
        self.update("spaces", id, json!({"name":name}))
    }
    pub fn set_space_git(
        &mut self,
        id: &str,
        detected: bool,
        checkout: Option<&str>,
        checked: DateTime<Utc>,
    ) -> Result<bool, MetadataError> {
        self.update("spaces", id, json!({"gitDetected":detected,"gitCheckedAt":checked.timestamp_millis(),"checkoutId":checkout}))
    }
    pub fn rename_chat(&mut self, id: &str, title: &str) -> Result<bool, MetadataError> {
        self.update("chats", id, json!({"title":title}))
    }
    pub fn set_chat_archived(&mut self, id: &str, archived: bool) -> Result<bool, MetadataError> {
        self.update("chats", id, json!({"archived":archived}))
    }
    pub fn set_chat_room_gen(&mut self, id: &str, generation: u32) -> Result<bool, MetadataError> {
        self.update("chats", id, json!({"roomGen":generation}))
    }
    pub fn set_chat_seen(&mut self, id: &str, at: DateTime<Utc>) -> Result<bool, MetadataError> {
        self.update("chats", id, json!({"lastSeenAt":at.timestamp_millis()}))
    }
    pub fn set_chat_branch(&mut self, id: &str, branch: &str) -> Result<bool, MetadataError> {
        self.update("chats", id, json!({"branch":branch}))
    }
    pub fn set_chat_cwd(&mut self, id: &str, cwd: &str) -> Result<bool, MetadataError> {
        self.update("chats", id, json!({"cwd":cwd}))
    }
    pub fn set_chat_checkout(&mut self, id: &str, checkout: &str) -> Result<bool, MetadataError> {
        self.update("chats", id, json!({"checkoutId":checkout}))
    }
    pub fn set_chat_config(
        &mut self,
        id: &str,
        config: &ChatConfig,
    ) -> Result<bool, MetadataError> {
        self.update("chats", id, json!({"config":config}))
    }
    pub fn set_chat_harness_session(
        &mut self,
        id: &str,
        session: &str,
        cwd: &str,
    ) -> Result<bool, MetadataError> {
        self.update(
            "chats",
            id,
            json!({"harnessSessionId":session,"harnessSessionCwd":cwd}),
        )
    }
    pub fn set_chat_last_message(
        &mut self,
        id: &str,
        preview: &str,
        at: DateTime<Utc>,
    ) -> Result<bool, MetadataError> {
        self.update(
            "chats",
            id,
            json!({"lastMessagePreview":preview,"lastMessageAt":at.timestamp_millis()}),
        )
    }
    pub fn delete_chat(&mut self, id: &str) -> Result<bool, MetadataError> {
        Ok(self.delete("chats", id))
    }
    pub fn delete_device(&mut self, id: &str) -> Result<DeletedDevice, MetadataError> {
        Ok(DeletedDevice {
            existed: self.delete("devices", id),
        })
    }
    pub fn delete_space(&mut self, id: &str) -> Result<DeletedSpace, MetadataError> {
        let chat_ids: Vec<_> = self
            .read_chats()?
            .into_iter()
            .filter(|c| c.space_id.as_deref() == Some(id))
            .map(|c| c.id)
            .collect();
        for chat in &chat_ids {
            self.delete("chats", chat);
        }
        Ok(DeletedSpace {
            existed: self.delete("spaces", id),
            chat_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_is_disposable_and_claim_does_not_erase_late_fields() {
        let mut view = MetadataView::new("host");
        view.claim_chat("chat", Some("/work"), Some("space"), DateTime::UNIX_EPOCH);
        let operations = view.take_operations();
        assert_eq!(operations.len(), 1);
        assert!(
            operations[0].hlc.is_empty(),
            "only the journal allocates clocks"
        );
        let set = operations[0].set.as_ref().unwrap();
        assert!(!set.contains_key("title") && !set.contains_key("config"));
        assert_eq!(
            view.chat("chat").unwrap().unwrap().created_at,
            DateTime::UNIX_EPOCH
        );
        let mut draft = view.clone();
        draft.rename_chat("chat", "not committed").unwrap();
        assert_eq!(
            draft.chat("chat").unwrap().unwrap().title.as_deref(),
            Some("not committed")
        );
        assert!(view.chat("chat").unwrap().unwrap().title.is_none());
        assert!(view.take_operations().is_empty());
    }

    #[test]
    fn typed_rows_roundtrip_dates_optional_clears_and_configuration() {
        let mut view = MetadataView::new("host");
        view.claim_chat("chat", Some("/work"), None, Utc::now());
        let mut chat = view.chat("chat").unwrap().unwrap();
        chat.title = Some("title".into());
        chat.config = Some(ChatConfig {
            harness: crate::HarnessId::Mock,
            model: Some("model".into()),
            reasoning: None,
            model_options: serde_json::from_value(json!({"temperature":0.5})).unwrap(),
            sandbox: crate::SandboxLevel::ReadOnly,
        });
        view.upsert_chat(&chat).unwrap();
        assert_eq!(view.chat("chat").unwrap().unwrap(), chat);
        chat.title = None;
        chat.config = None;
        view.upsert_chat(&chat).unwrap();
        assert_eq!(view.chat("chat").unwrap().unwrap(), chat);
        let operations = view.take_operations();
        let last = operations.last().unwrap().set.as_ref().unwrap();
        assert_eq!(last["title"], Value::Null);
        assert_eq!(last["config"], Value::Null);
        assert!(last["createdAt"].is_i64());
    }

    #[test]
    fn native_cascades_contain_no_session_operations_or_transport_receipts() {
        let mut view = MetadataView::new("host");
        view.upsert_space(&Space {
            id: "space".into(),
            device_id: "host".into(),
            path: "/work".into(),
            name: None,
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: DateTime::UNIX_EPOCH,
        })
        .unwrap();
        for id in ["a", "b"] {
            view.claim_chat(id, Some("/work"), Some("space"), DateTime::UNIX_EPOCH);
        }
        view.claim_chat("other", None, None, DateTime::UNIX_EPOCH);
        view.take_operations();
        let deleted = view.delete_space("space").unwrap();
        assert!(deleted.existed);
        assert_eq!(deleted.chat_ids, ["a", "b"]);
        let operations = view.take_operations();
        assert_eq!(operations.len(), 3);
        assert!(
            operations
                .iter()
                .all(|op| op.op == OpKind::Delete && op.kind != "sessions")
        );
        assert_eq!(view.read_chats().unwrap().len(), 1);
        assert!(view.read_all().unwrap().sessions.is_empty());
    }

    #[test]
    fn malformed_owned_metadata_is_an_error_not_an_absent_claimable_chat() {
        let mut view = MetadataView::new("host");
        view.replace_rows([MetadataRow {
            kind: "chats".into(),
            id: "bad".into(),
            seq: 1,
            deleted: false,
            del_hlc: None,
            fields: BTreeMap::from([("id".into(), json!("bad")), ("deviceId".into(), json!(42))]),
            clocks: BTreeMap::new(),
        }]);
        assert!(view.chat("bad").is_err());
        assert!(view.read_all().is_err());
        assert_eq!(view.chat("absent").unwrap(), None);
        view.delete_device("absent").unwrap();
        assert!(view.device_is_tombstoned("absent"));
    }
}
