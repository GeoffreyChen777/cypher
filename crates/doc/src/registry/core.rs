//! The registry's mirrored CRDT core: HLC stamps, rows, ops and the per-op
//! merge. Pure data and functions — no doc state, no I/O.
//!
//! Mirrored 1:1 by `edge/src/registry-core.ts` (the server merge) and
//! `apps/ios/Cypher/Sync/RegistryCore.swift`; the shared test vectors live in
//! `registry/tests.rs`, `edge/src/registry-core.test.ts` and
//! `CypherTests/RegistryCoreTests.swift`. Change all three together.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── HLC ─────────────────────────────────────────────────────────────────────

/// Encode an HLC string: `{ms:013}-{counter:06}-{device}`. Fixed-width zero
/// padding makes lexicographic order = (ms, counter, device) order, and the
/// device suffix makes the order total (two writers can never tie).
pub(crate) fn encode_hlc(ms: i64, counter: u32, device: &str) -> String {
    format!("{ms:013}-{counter:06}-{device}")
}

/// `a` strictly newer than `b` (`None` = never written, loses to any).
fn hlc_newer(a: &str, b: Option<&str>) -> bool {
    match b {
        None => true,
        Some(b) => a > b,
    }
}

/// Monotonic HLC source: never emits the same or an earlier clock twice, even
/// across a wall-clock regression or restart (state persists with the doc).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct HlcClock {
    last_ms: i64,
    counter: u32,
}

impl HlcClock {
    pub(crate) fn next(&mut self, now_ms: i64, device: &str) -> String {
        if now_ms > self.last_ms {
            self.last_ms = now_ms;
            self.counter = 0;
        } else {
            self.counter += 1;
            if self.counter > 999_999 {
                self.last_ms += 1;
                self.counter = 0;
            }
        }
        encode_hlc(self.last_ms, self.counter, device)
    }
}

// ── rows and ops (wire-compatible with edge/src/registry-core.ts) ───────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryRow {
    pub kind: String,
    pub id: String,
    /// Server seq of the batch that last touched this row (0 locally).
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub deleted: bool,
    /// Tombstone clock — an upsert newer than this revives the row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub del_hlc: Option<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
    /// Per-field last-write clocks.
    #[serde(default)]
    pub clocks: BTreeMap<String, String>,
}

impl RegistryRow {
    fn tombstone(kind: &str, id: &str, hlc: String) -> Self {
        Self {
            kind: kind.to_string(),
            id: id.to_string(),
            seq: 0,
            deleted: true,
            del_hlc: Some(hlc),
            fields: BTreeMap::new(),
            clocks: BTreeMap::new(),
        }
    }

    /// The newest clock anywhere on the row (delete-vs-live comparison base).
    fn max_clock(&self) -> Option<&str> {
        let mut max = self.del_hlc.as_deref();
        for clock in self.clocks.values() {
            if max.is_none_or(|m| clock.as_str() > m) {
                max = Some(clock);
            }
        }
        max
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpKind {
    /// Creates, and revives tombstones when newer.
    Upsert,
    /// Never creates or revives ("never invent rows").
    Update,
    /// Tombstones when causally newer than the row.
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowOp {
    pub kind: String,
    pub id: String,
    pub op: OpKind,
    /// Field writes; `Value::Null` deletes the field (still a clocked write).
    /// Absent for deletes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<BTreeMap<String, Value>>,
    /// Clock for every write in `set` without an entry in `clocks`.
    pub hlc: String,
    /// Per-field clock overrides — re-seed pushes carry a row's ORIGINAL
    /// clocks so recovery never coarsens causality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clocks: Option<BTreeMap<String, String>>,
}

impl RowOp {
    pub(crate) fn clock_for<'a>(&'a self, field: &str) -> &'a str {
        self.clocks
            .as_ref()
            .and_then(|c| c.get(field))
            .map_or(self.hlc.as_str(), String::as_str)
    }
}

/// Apply one op to a row — the 1:1 mirror of `applyOp` in
/// `edge/src/registry-core.ts`. Returns the new row (`None` only for an
/// `update` on a missing row) and whether anything changed.
pub fn apply_op(row: Option<&RegistryRow>, op: &RowOp) -> (Option<RegistryRow>, bool) {
    if op.op == OpKind::Delete {
        return match row {
            // Tombstone-on-missing guards against a late create racing the delete.
            None => (
                Some(RegistryRow::tombstone(&op.kind, &op.id, op.hlc.clone())),
                true,
            ),
            Some(row) => {
                let beats = if row.deleted {
                    hlc_newer(&op.hlc, row.del_hlc.as_deref())
                } else {
                    hlc_newer(&op.hlc, row.max_clock())
                };
                if beats {
                    let mut gone = row.clone();
                    gone.deleted = true;
                    gone.del_hlc = Some(op.hlc.clone());
                    gone.fields.clear();
                    gone.clocks.clear();
                    (Some(gone), true)
                } else {
                    (Some(row.clone()), false)
                }
            }
        };
    }

    let mut base = match row {
        None => {
            if op.op == OpKind::Update {
                return (None, false);
            }
            RegistryRow {
                kind: op.kind.clone(),
                id: op.id.clone(),
                seq: 0,
                deleted: false,
                del_hlc: None,
                fields: BTreeMap::new(),
                clocks: BTreeMap::new(),
            }
        }
        Some(row) if row.deleted => {
            if op.op == OpKind::Update || !hlc_newer(&op.hlc, row.del_hlc.as_deref()) {
                return (Some(row.clone()), false);
            }
            // Revival: the tombstone loses wholesale; the upsert's fields are
            // the row.
            RegistryRow {
                kind: row.kind.clone(),
                id: row.id.clone(),
                seq: row.seq,
                deleted: false,
                del_hlc: row.del_hlc.clone(),
                fields: BTreeMap::new(),
                clocks: BTreeMap::new(),
            }
        }
        Some(row) => row.clone(),
    };

    let mut changed = row.is_none() || (row.is_some_and(|r| r.deleted) && !base.deleted);
    if let Some(set) = &op.set {
        for (key, value) in set {
            let clock = op.clock_for(key);
            if !hlc_newer(clock, base.clocks.get(key).map(String::as_str)) {
                continue;
            }
            if value.is_null() {
                base.fields.remove(key);
            } else {
                base.fields.insert(key.clone(), value.clone());
            }
            base.clocks.insert(key.clone(), clock.to_string());
            changed = true;
        }
    }
    if changed {
        base.deleted = false;
        (Some(base), true)
    } else {
        (Some(base), false)
    }
}

/// A row as a re-seed op (server-behind-client recovery): one upsert carrying
/// the row's ORIGINAL per-field clocks, or a delete for tombstones.
pub(crate) fn row_to_seed_op(row: &RegistryRow) -> RowOp {
    if row.deleted {
        return RowOp {
            kind: row.kind.clone(),
            id: row.id.clone(),
            op: OpKind::Delete,
            set: None,
            hlc: row
                .del_hlc
                .clone()
                .unwrap_or_else(|| encode_hlc(0, 0, "seed")),
            clocks: None,
        };
    }
    RowOp {
        kind: row.kind.clone(),
        id: row.id.clone(),
        op: OpKind::Upsert,
        set: Some(
            row.fields
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        hlc: row
            .max_clock()
            .map(str::to_string)
            .unwrap_or_else(|| encode_hlc(0, 0, "seed")),
        clocks: Some(row.clocks.clone()),
    }
}
