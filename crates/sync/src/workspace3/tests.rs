use super::{
    journal::Journal,
    wire::{Page, Scope},
};
use cypher_proto::metadata::{MetadataRow as RegistryRow, OpKind, RowOp, apply_op};
use serde_json::json;
use std::collections::BTreeMap;

fn scope() -> Scope {
    Scope {
        endpoint: "https://test".into(),
        org: "org".into(),
        user: "user".into(),
        actor: "host".into(),
    }
}
fn mutate(j: &mut Journal, id: &str, title: &str, now: u64) -> String {
    j.mutate(
        "chats",
        id,
        OpKind::Upsert,
        Some(BTreeMap::from([
            ("id".into(), json!(id)),
            ("title".into(), json!(title)),
        ])),
        now,
    )
    .unwrap()
}
fn pending_row(j: &Journal, seq: u64) -> RegistryRow {
    let p = j.pending().unwrap().unwrap();
    let v: serde_json::Value = serde_json::from_str(&p.request).unwrap();
    let op: RowOp = serde_json::from_value(v["ops"][0].clone()).unwrap();
    let mut row = apply_op(None, &op).0.unwrap();
    row.seq = seq;
    row
}
#[test]
fn offline_outbox_and_exact_ack_survive_restart_without_advancing_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.sqlite");
    let mut j = Journal::open(&path, scope()).unwrap();
    let id = mutate(&mut j, "chat", "offline", 1000);
    let pending = j.pending().unwrap().unwrap();
    assert_eq!(
        j.row("chats", "chat").unwrap().unwrap().fields["title"],
        "offline"
    );
    assert_eq!(j.cursor().unwrap(), 0);
    let row = pending_row(&j, 5);
    drop(j);
    let mut j = Journal::open(&path, scope()).unwrap();
    assert_eq!(j.pending().unwrap().unwrap().request, pending.request);
    assert!(j.acknowledge(&id, "wrong", 5, &[row.clone()]).is_err());
    assert!(j.pending().unwrap().is_some());
    j.acknowledge(&id, &pending.hash, 5, &[row.clone()])
        .unwrap();
    assert!(j.pending().unwrap().is_none());
    assert_eq!(
        j.cursor().unwrap(),
        0,
        "ACK is not the contiguous pull cursor"
    );
    j.apply_page(
        0,
        &Page {
            through: 5,
            next: 5,
            done: true,
            rows: vec![row],
        },
    )
    .unwrap();
    drop(j);
    let j = Journal::open(&path, scope()).unwrap();
    assert_eq!(j.cursor().unwrap(), 5);
    assert_eq!(
        j.row("chats", "chat").unwrap().unwrap().fields["title"],
        "offline"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[test]
fn wrong_scope_and_legacy_database_are_not_reset_or_imported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.sqlite");
    let mut j = Journal::open(&path, scope()).unwrap();
    mutate(&mut j, "chat", "keep", 1);
    drop(j);
    let mut another = scope();
    another.user = "other".into();
    assert!(Journal::open(&path, another).is_err());
    assert!(
        Journal::open(&path, scope())
            .unwrap()
            .pending()
            .unwrap()
            .is_some()
    );
    let path = dir.path().join("legacy.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE snapshots(id TEXT PRIMARY KEY,body BLOB);")
        .unwrap();
    assert!(Journal::open(&path, scope()).is_err());
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='workspace3_meta'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
}
#[test]
fn rows_and_cursor_roll_back_together_and_latest_pending_overlay_survives_old_ack() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.sqlite");
    let mut j = Journal::open(&path, scope()).unwrap();
    let id = mutate(&mut j, "chat", "first", 1000);
    let pending = j.pending().unwrap().unwrap();
    let row = pending_row(&j, 1);
    mutate(&mut j, "chat", "second", 900);
    j.acknowledge(&id, &pending.hash, 1, &[row.clone()])
        .unwrap();
    assert_eq!(
        j.row("chats", "chat").unwrap().unwrap().fields["title"],
        "second"
    );
    let mut next = row.clone();
    next.id = "poison".into();
    next.fields.insert("id".into(), json!("poison"));
    next.seq = 2;
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER poison BEFORE INSERT ON workspace3_rows WHEN NEW.id='poison' BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
    assert!(
        j.apply_page(
            0,
            &Page {
                through: 2,
                next: 2,
                done: true,
                rows: vec![row.clone(), next.clone()]
            }
        )
        .is_err()
    );
    assert_eq!(j.cursor().unwrap(), 0);
    assert!(j.row("chats", "poison").unwrap().is_none());
    db.execute_batch("DROP TRIGGER poison").unwrap();
    j.apply_page(
        0,
        &Page {
            through: 2,
            next: 2,
            done: true,
            rows: vec![row, next],
        },
    )
    .unwrap();
    assert_eq!(j.cursor().unwrap(), 2);
    assert_eq!(
        j.window("chats", "", 1).unwrap().next.as_deref(),
        Some("chat")
    );
    assert_eq!(
        j.window("chats", "chat", 1).unwrap().next.as_deref(),
        Some("poison")
    );
    assert!(j.window("chats", "", 33).is_err());
}
#[test]
fn remote_clock_floor_is_observed_and_bad_pages_do_not_reseed() {
    let mut j = Journal::open(std::path::Path::new(":memory:"), scope()).unwrap();
    mutate(&mut j, "chat", "initial", 1000);
    let mut remote = pending_row(&j, 4);
    for clock in remote.clocks.values_mut() {
        *clock = "0000000009000-000005-remote".into();
    }
    j.apply_page(
        0,
        &Page {
            through: 4,
            next: 4,
            done: true,
            rows: vec![remote],
        },
    )
    .unwrap();
    mutate(&mut j, "later", "later", 100);
    let window = j.window("chats", "chat", 1).unwrap();
    assert!(
        window.rows[0]
            .clocks
            .values()
            .all(|c| c.starts_with("0000000009000-000006-"))
    );
    let before = j.pending().unwrap().unwrap().request;
    assert!(
        j.apply_page(
            4,
            &Page {
                through: 0,
                next: 0,
                done: true,
                rows: vec![]
            }
        )
        .is_err()
    );
    assert_eq!(j.cursor().unwrap(), 4);
    assert_eq!(j.pending().unwrap().unwrap().request, before);
    assert!(
        j.mutate(
            "chats",
            "a",
            OpKind::Upsert,
            Some(BTreeMap::from([("id".into(), json!("b"))])),
            1
        )
        .is_err()
    );
}

#[test]
fn explicit_local_authority_needs_no_cloud_outbox_and_cannot_be_rebound() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.sqlite");
    let mut local = scope();
    local.endpoint = "local".into();
    let mut j = Journal::open(&path, local.clone()).unwrap();
    mutate(&mut j, "chat", "local", 1);
    assert!(j.pending().unwrap().is_none());
    assert_eq!(j.cursor().unwrap(), 1);
    assert_eq!(j.row("chats", "chat").unwrap().unwrap().seq, 1);
    drop(j);
    assert!(
        Journal::open(&path, scope()).is_err(),
        "local authority is not a cloud seed"
    );
    assert_eq!(
        Journal::open(&path, local)
            .unwrap()
            .row("chats", "chat")
            .unwrap()
            .unwrap()
            .fields["title"],
        "local"
    );
}

#[test]
fn a_domain_cascade_is_atomic_when_a_later_operation_is_invalid() {
    let mut local = scope();
    local.endpoint = "local".into();
    let mut j = Journal::open(std::path::Path::new(":memory:"), local).unwrap();
    let op = RowOp {
        kind: "chats".into(),
        id: "a".into(),
        op: OpKind::Upsert,
        set: Some(BTreeMap::from([("id".into(), json!("a"))])),
        hlc: String::new(),
    };
    let mut bad = op.clone();
    bad.id = "b".into();
    assert!(j.mutate_many(&[op.clone(), bad], 1).is_err());
    assert!(j.row("chats", "a").unwrap().is_none());
    assert_eq!(j.cursor().unwrap(), 0);
    j.mutate_many(&[op], 1).unwrap();
    assert_eq!(j.cursor().unwrap(), 1);
}
