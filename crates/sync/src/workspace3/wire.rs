use crate::sync3::Error;
use cypher_proto::metadata::{MetadataRow as RegistryRow, OpKind, RowOp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const FRAME_BYTES: usize = 256 * 1024;
pub const ROW_BYTES: usize = 64 * 1024;
pub const MAX_OPS: usize = 3;
pub const MAX_ROWS: usize = 32;
pub const MAX_SAFE: u64 = 9_007_199_254_740_991;
pub fn error(code: &str) -> Error {
    Error::Protocol(code.into())
}
pub fn id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
pub fn kind(s: &str) -> bool {
    matches!(s, "devices" | "spaces" | "chats")
}
pub fn row_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:@/-".contains(&b))
}
pub fn clock(s: &str) -> Result<(u64, u32), Error> {
    if s.len() < 22
        || !s.is_ascii()
        || &s[13..14] != "-"
        || &s[20..21] != "-"
        || !id(&s[21..])
        || !s[..13].bytes().all(|b| b.is_ascii_digit())
        || !s[14..20].bytes().all(|b| b.is_ascii_digit())
    {
        return Err(error("invalid_clock"));
    }
    Ok((
        s[..13].parse().map_err(|_| error("invalid_clock"))?,
        s[14..20].parse().map_err(|_| error("invalid_clock"))?,
    ))
}
fn field(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.bytes().all(|b| b.is_ascii_alphanumeric())
        && !matches!(s, "constructor" | "prototype" | "__proto__")
}
pub fn value(v: &Value, depth: usize) -> Result<(), Error> {
    if depth > 32 {
        return Err(error("json_too_complex"));
    }
    match v {
        Value::Number(n) => {
            let f = n.as_f64().ok_or_else(|| error("invalid_number"))?;
            if !f.is_finite() || (f.fract() == 0.0 && f.abs() > MAX_SAFE as f64) {
                return Err(error("invalid_number"));
            }
        }
        Value::Array(a) => {
            for child in a {
                value(child, depth + 1)?;
            }
        }
        Value::Object(o) => {
            for child in o.values() {
                value(child, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub fn operation(op: &RowOp, actor: &str) -> Result<(), Error> {
    operation_policy(op, actor, false)
}
pub(super) fn local_operation(op: &RowOp, actor: &str) -> Result<(), Error> {
    operation_policy(op, actor, true)
}
fn operation_policy(op: &RowOp, actor: &str, local: bool) -> Result<(), Error> {
    clock(&op.hlc)?;
    if !kind(&op.kind)
        || !row_id(&op.id)
        || !op.hlc.ends_with(&format!("-{actor}"))
        || (op.op == OpKind::Delete) != op.set.is_none()
        || (!local
            && op.kind == "devices"
            && op.op != OpKind::Delete
            && op.id != actor
            && !(op.op == OpKind::Update
                && op
                    .set
                    .as_ref()
                    .is_some_and(|s| s.keys().all(|k| k == "name"))))
    {
        return Err(error("invalid_workspace_operation"));
    }
    if let Some(set) = &op.set {
        for (k, v) in set {
            if !field(k) {
                return Err(error("invalid_field"));
            }
            value(v, 0)?;
        }
        if set.get("id").is_some_and(|v| v.as_str() != Some(&op.id)) {
            return Err(error("row_identity_mismatch"));
        }
    }
    if serde_json::to_vec(op)?.len() > 16 * 1024 {
        return Err(error("operation_too_large"));
    }
    Ok(())
}
pub fn row(row: &RegistryRow) -> Result<(), Error> {
    if !kind(&row.kind)
        || !row_id(&row.id)
        || row.seq == 0
        || row.seq > MAX_SAFE
        || serde_json::to_vec(row)?.len() > ROW_BYTES
    {
        return Err(error("invalid_workspace_row"));
    }
    if row
        .fields
        .get("id")
        .is_some_and(|v| v.as_str() != Some(&row.id))
    {
        return Err(error("row_identity_mismatch"));
    }
    for (key, v) in &row.fields {
        if !field(key) || !row.clocks.contains_key(key) {
            return Err(error("invalid_workspace_row"));
        }
        value(v, 0)?;
    }
    for (key, c) in &row.clocks {
        if !field(key) {
            return Err(error("invalid_field"));
        }
        clock(c)?;
    }
    if let Some(c) = &row.del_hlc {
        clock(c)?;
    }
    if row.deleted && (row.del_hlc.is_none() || !row.fields.is_empty() || !row.clocks.is_empty()) {
        return Err(error("invalid_tombstone"));
    }
    Ok(())
}
pub fn rows(value: &Value) -> Result<Vec<RegistryRow>, Error> {
    let items = value
        .as_array()
        .ok_or_else(|| error("invalid_workspace_rows"))?;
    if items.len() > MAX_ROWS {
        return Err(error("invalid_workspace_rows"));
    }
    items
        .iter()
        .map(|item| {
            let o = item
                .as_object()
                .ok_or_else(|| error("invalid_workspace_row"))?;
            if ["kind", "id", "seq", "deleted", "fields", "clocks"]
                .iter()
                .any(|k| !o.contains_key(*k))
                || o.keys().any(|k| {
                    !["kind", "id", "seq", "deleted", "fields", "clocks", "delHlc"]
                        .contains(&k.as_str())
                })
            {
                return Err(error("invalid_workspace_row"));
            }
            let result: RegistryRow = serde_json::from_value(item.clone())?;
            row(&result)?;
            Ok(result)
        })
        .collect()
}
pub fn frame(v: &Value) -> Result<(), Error> {
    let o = v
        .as_object()
        .ok_or_else(|| error("invalid_workspace_frame"))?;
    let kind = o
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| error("invalid_workspace_frame"))?;
    let fields: &[&str] = match kind {
        "welcome" => &[
            "user",
            "org",
            "connection",
            "leaseMs",
            "through",
            "next",
            "done",
            "rows",
        ],
        "page" => &["through", "next", "done", "rows"],
        "pushed" => &["id", "requestHash", "through", "rows"],
        "changed" => &["through"],
        "probeOk" => &["id", "through"],
        "demand" | "watching" => &["chats"],
        "presence" => &["actor", "role", "connection", "expiresAt", "state"],
        "peerClosed" => &["actor", "connection"],
        "routed" => &["id", "token", "window"],
        "call" => &["token", "from", "method", "params", "window"],
        "reply" => &["id", "sequence", "done", "value"],
        "credit" => &["token", "through"],
        "input" => &["token", "sequence", "done", "value"],
        "inputCredit" => &["id", "token", "through"],
        "cancel" => &["token"],
        "error" => &["code"],
        _ => return Err(error("unknown_message")),
    };
    if o.get("version").and_then(Value::as_u64) != Some(3)
        || fields.iter().any(|f| !o.contains_key(*f))
        || o.keys().any(|f| {
            !["type", "version"].contains(&f.as_str())
                && !fields.contains(&f.as_str())
                && !(kind == "error" && ["id", "token"].contains(&f.as_str()))
                && !(kind == "call" && f == "input")
        })
    {
        return Err(error("invalid_workspace_frame"));
    }
    for name in ["id", "actor", "from", "connection", "org"] {
        if o.get(name).is_some_and(|v| !v.as_str().is_some_and(id)) {
            return Err(error("invalid_identity"));
        }
    }
    if o.get("token")
        .is_some_and(|v| !v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 1024))
    {
        return Err(error("invalid_route"));
    }
    for name in ["through", "next", "sequence", "expiresAt", "leaseMs"] {
        if o.get(name)
            .is_some_and(|v| !v.as_u64().is_some_and(|n| n <= MAX_SAFE))
        {
            return Err(error("invalid_number"));
        }
    }
    if o.get("window").is_some_and(|v| v.as_u64() != Some(2)) {
        return Err(error("invalid_credit_window"));
    }
    if o.get("done").is_some_and(|v| !v.is_boolean()) {
        return Err(error("invalid_workspace_frame"));
    }
    if o.get("input").is_some_and(|v| !v.is_boolean()) {
        return Err(error("invalid_workspace_frame"));
    }
    if let Some(chats) = o.get("chats") {
        let a = chats.as_array().ok_or_else(|| error("invalid_topics"))?;
        if a.len() > if kind == "watching" { 8 } else { 512 }
            || a.iter().any(|v| !v.as_str().is_some_and(id))
        {
            return Err(error("invalid_topics"));
        }
    }
    if let Some(role) = o.get("role") {
        if !matches!(role.as_str(), Some("host" | "viewer")) {
            return Err(error("invalid_role"));
        }
    }
    for name in ["params", "value", "state"] {
        if let Some(value) = o.get(name) {
            if serde_json::to_vec(value)?.len() > 64 * 1024 {
                return Err(error("rpc_value_too_large"));
            }
        }
    }
    if kind == "presence" && !v["state"].is_object() {
        return Err(error("invalid_presence"));
    }
    if kind == "call"
        && !v["method"].as_str().is_some_and(|s| {
            !s.is_empty()
                && s.len() <= 96
                && s.as_bytes()[0].is_ascii_alphabetic()
                && s.bytes().all(|b| b.is_ascii_alphanumeric())
        })
    {
        return Err(error("invalid_method"));
    }
    if kind == "error"
        && !v["code"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 128)
    {
        return Err(error("invalid_error"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scope {
    pub endpoint: String,
    pub org: String,
    pub user: String,
    pub actor: String,
}
impl Scope {
    pub fn validate(&self) -> Result<(), Error> {
        if !id(&self.org)
            || !id(&self.actor)
            || self.user.is_empty()
            || self.user.len() > 256
            || self.endpoint.is_empty()
        {
            return Err(error("invalid_workspace_scope"));
        }
        if self.user.chars().any(char::is_control)
            || (self.endpoint != "local"
                && (!(self.endpoint.starts_with("http://")
                    || self.endpoint.starts_with("https://"))
                    || self
                        .endpoint
                        .chars()
                        .any(|c| c.is_whitespace() || ['@', '?', '#'].contains(&c))))
        {
            return Err(error("invalid_workspace_scope"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub through: u64,
    pub next: u64,
    pub done: bool,
    pub rows: Vec<RegistryRow>,
}
impl Page {
    pub fn validate(&self, after: u64) -> Result<(), Error> {
        if self.through > MAX_SAFE
            || self.through < self.next
            || self.next < after
            || self.done != (self.next == self.through)
            || self.rows.len() > MAX_ROWS
            || serde_json::to_vec(self)?.len() > FRAME_BYTES - 512
        {
            return Err(error("invalid_workspace_page"));
        }
        let mut previous = after;
        let mut seen = std::collections::HashSet::new();
        for r in &self.rows {
            row(r)?;
            if r.seq <= previous || r.seq > self.next || !seen.insert((&r.kind, &r.id)) {
                return Err(error("invalid_workspace_page"));
            }
            previous = r.seq;
        }
        if previous != self.next {
            return Err(error("invalid_workspace_page"));
        }
        Ok(())
    }
}
