//! Storage-independent Workspace v3 metadata.
//!
//! One operation clock stamps all its field writes. There is deliberately no
//! per-field clock override, snapshot import, reseed, session row or transport
//! state in this model. Admission limits/account policy live at the boundary;
//! the deterministic merge is shared by durable and optimistic native views.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
pub mod view;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetadataRow {
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub del_hlc: Option<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
    #[serde(default)]
    pub clocks: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpKind {
    Upsert,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RowOp {
    pub kind: String,
    pub id: String,
    pub op: OpKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<BTreeMap<String, Value>>,
    pub hlc: String,
}

fn newer(a: &str, b: Option<&str>) -> bool {
    b.is_none_or(|b| a > b)
}

impl MetadataRow {
    pub fn max_clock(&self) -> Option<&str> {
        self.clocks
            .values()
            .map(String::as_str)
            .chain(self.del_hlc.as_deref())
            .max()
    }
}

/// Replay an admitted operation. Equal clocks are immutable: retransmission
/// cannot change a field. Deletes compare against the newest field, updates
/// cannot invent or revive rows, and only newer upserts can revive tombstones.
pub fn apply_op(row: Option<&MetadataRow>, op: &RowOp) -> (Option<MetadataRow>, bool) {
    if op.op == OpKind::Delete {
        if let Some(row) = row {
            let previous = if row.deleted {
                row.del_hlc.as_deref()
            } else {
                row.max_clock()
            };
            if !newer(&op.hlc, previous) {
                return (Some(row.clone()), false);
            }
        }
        return (
            Some(MetadataRow {
                kind: op.kind.clone(),
                id: op.id.clone(),
                seq: row.map_or(0, |r| r.seq),
                deleted: true,
                del_hlc: Some(op.hlc.clone()),
                fields: BTreeMap::new(),
                clocks: BTreeMap::new(),
            }),
            true,
        );
    }
    let mut base = match row {
        None if op.op == OpKind::Update => return (None, false),
        Some(row)
            if row.deleted
                && (op.op == OpKind::Update || !newer(&op.hlc, row.del_hlc.as_deref())) =>
        {
            return (Some(row.clone()), false);
        }
        Some(row) if !row.deleted => row.clone(),
        _ => MetadataRow {
            kind: op.kind.clone(),
            id: op.id.clone(),
            seq: row.map_or(0, |r| r.seq),
            deleted: false,
            del_hlc: row.and_then(|r| r.del_hlc.clone()),
            fields: BTreeMap::new(),
            clocks: BTreeMap::new(),
        },
    };
    let mut changed = row.is_none_or(|r| r.deleted);
    if let Some(set) = &op.set {
        for (key, value) in set {
            if newer(&op.hlc, base.clocks.get(key).map(String::as_str)) {
                if value.is_null() {
                    base.fields.remove(key);
                } else {
                    base.fields.insert(key.clone(), value.clone());
                }
                base.clocks.insert(key.clone(), op.hlc.clone());
                changed = true;
            }
        }
    }
    (Some(base), changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn op(kind: OpKind, clock: u32, set: Value) -> RowOp {
        RowOp {
            kind: "chats".into(),
            id: "chat".into(),
            op: kind,
            hlc: format!("0000000000001-{clock:06}-host"),
            set: set.as_object().map(|s| s.clone().into_iter().collect()),
        }
    }
    #[test]
    fn writes_and_deletes_preserve_causality_and_do_not_invent_rows() {
        assert_eq!(
            apply_op(None, &op(OpKind::Update, 1, json!({"title":"missing"}))),
            (None, false)
        );
        let (first, changed) = apply_op(
            None,
            &op(OpKind::Upsert, 1, json!({"title":"first","cwd":"/work"})),
        );
        assert!(changed);
        let (same, changed) = apply_op(
            first.as_ref(),
            &op(OpKind::Update, 1, json!({"title":"forged"})),
        );
        assert!(!changed);
        assert_eq!(same, first);
        let (edited, _) = apply_op(
            first.as_ref(),
            &op(OpKind::Update, 3, json!({"title":"new","cwd":null})),
        );
        assert_eq!(edited.as_ref().unwrap().fields["title"], "new");
        assert!(!edited.as_ref().unwrap().fields.contains_key("cwd"));
        assert_eq!(
            apply_op(edited.as_ref(), &op(OpKind::Delete, 2, Value::Null)),
            (edited.clone(), false)
        );
        let (gone, _) = apply_op(edited.as_ref(), &op(OpKind::Delete, 4, Value::Null));
        assert!(gone.as_ref().unwrap().deleted);
        assert_eq!(
            apply_op(
                gone.as_ref(),
                &op(OpKind::Update, 5, json!({"title":"wrong"}))
            ),
            (gone.clone(), false)
        );
        assert_eq!(
            apply_op(
                gone.as_ref(),
                &op(OpKind::Upsert, 4, json!({"title":"old"}))
            ),
            (gone.clone(), false)
        );
        let (revived, _) = apply_op(
            gone.as_ref(),
            &op(OpKind::Upsert, 5, json!({"title":"revived"})),
        );
        let revived = revived.unwrap();
        assert!(!revived.deleted);
        assert_eq!(revived.fields.len(), 1);
        assert_eq!(revived.del_hlc, gone.unwrap().del_hlc);
    }
    #[test]
    fn native_operations_do_not_admit_reseed_fields_or_unknown_members() {
        let original =
            serde_json::to_value(op(OpKind::Upsert, 1, json!({"title":"hello"}))).unwrap();
        assert!(serde_json::from_value::<RowOp>(original.clone()).is_ok());
        for (field, value) in [
            ("clocks", json!({"title":"0000000000000-000000-old"})),
            ("reseed", json!(true)),
            ("clocks", Value::Null),
        ] {
            let mut invalid = original.clone();
            invalid[field] = value;
            assert!(serde_json::from_value::<RowOp>(invalid).is_err(), "{field}");
        }
    }
    #[test]
    fn fieldwise_concurrent_edits_converge_in_either_order() {
        let a = op(OpKind::Upsert, 1, json!({"title":"A"}));
        let b = op(OpKind::Upsert, 2, json!({"archived":true}));
        let a_first = apply_op(None, &a).0;
        let b_first = apply_op(None, &b).0;
        assert_eq!(
            apply_op(a_first.as_ref(), &b).0,
            apply_op(b_first.as_ref(), &a).0
        );
    }
}
