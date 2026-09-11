//! Experimental Sync v3 local journal and protocol state.
//!
//! ACK retires an outbox item; only a committed page advances the projection.
//! It is intentionally not selected by the production doc host yet.
use cypher_proto::sync3::{self as wire, Operation, Projection, Reply, Request, Row};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

mod projection_store;
pub mod transport;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("v3 file I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("v3 storage: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("v3 JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("v3 protocol: {0}")]
    Protocol(String),
}
fn invalid(code: &str) -> Error {
    Error::Protocol(code.into())
}

pub struct Journal {
    db: Connection,
    actor: String,
}

impl Journal {
    /// The account+room binding is persisted and checked even if a caller
    /// accidentally opens another account's filename. Never clear on mismatch.
    pub fn open(path: &Path, account: &str, room: &str, actor: &str) -> Result<Self, Error> {
        if !wire::valid_id(actor) || account.is_empty() || room.is_empty() {
            return Err(invalid("invalid_identity"));
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        let format: u32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let prototype: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='sync3_projection')",
            [], |r| r.get(0))?;
        if prototype || ![0, 3].contains(&format) {
            // Never silently reopen a different storage format as empty.
            return Err(invalid("unsupported_journal_format"));
        }
        #[cfg(unix)]
        if path != Path::new(":memory:") {
            use std::os::unix::fs::PermissionsExt;
            // SQLite derives new WAL/SHM permissions from the database file.
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            for suffix in ["-wal", "-shm"] {
                let mut sidecar = path.as_os_str().to_os_string();
                sidecar.push(suffix);
                let sidecar = std::path::PathBuf::from(sidecar);
                match std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS sync3_meta(
              singleton INTEGER PRIMARY KEY CHECK(singleton=1),
              account TEXT NOT NULL,room TEXT NOT NULL,actor TEXT NOT NULL,
              epoch INTEGER NOT NULL DEFAULT 0, owner TEXT NOT NULL DEFAULT '',
              owner_epoch INTEGER NOT NULL DEFAULT 0,cursor INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS sync3_outbox(
              ordinal INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,
              operation TEXT NOT NULL,acked_seq INTEGER);
            CREATE TABLE IF NOT EXISTS sync3_events(
              seq INTEGER PRIMARY KEY,id TEXT UNIQUE NOT NULL,operation TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS sync3_entities(
              kind TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,run_id TEXT,seq INTEGER NOT NULL,
              PRIMARY KEY(kind,id));
            CREATE INDEX IF NOT EXISTS sync3_entity_run ON sync3_entities(kind,run_id);
            CREATE INDEX IF NOT EXISTS sync3_entity_seq ON sync3_entities(seq);",
        )?;
        let tx = db.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO sync3_meta(singleton,account,room,actor) VALUES(1,?,?,?)",
            params![account, room, actor],
        )?;
        let identity: (String, String, String) = tx.query_row(
            "SELECT account,room,actor FROM sync3_meta WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if identity != (account.into(), room.into(), actor.into()) {
            return Err(invalid("scope_mismatch"));
        }
        tx.pragma_update(None, "user_version", 3)?;
        tx.commit()?;
        Ok(Self {
            db,
            actor: actor.into(),
        })
    }
    pub fn actor(&self) -> &str {
        &self.actor
    }
    pub fn cursor(&self) -> Result<u64, Error> {
        Ok(self
            .db
            .query_row("SELECT cursor FROM sync3_meta WHERE singleton=1", [], |r| {
                r.get(0)
            })?)
    }
    pub fn epoch(&self) -> Result<u64, Error> {
        Ok(self
            .db
            .query_row("SELECT epoch FROM sync3_meta WHERE singleton=1", [], |r| {
                r.get(0)
            })?)
    }
    pub fn projection(&self) -> Result<Projection, Error> {
        projection_store::read(&self.db)
    }
    pub fn hello(&self) -> Result<Request, Error> {
        Ok(Request::Hello {
            version: wire::VERSION,
            actor: self.actor.clone(),
            epoch: self.epoch()?,
            after: self.cursor()?,
        })
    }
    pub fn accept_state(&mut self, state: &Reply) -> Result<u64, Error> {
        let Reply::State {
            version,
            epoch,
            owner,
            owner_epoch,
            head,
        } = state
        else {
            return Err(invalid("expected_state"));
        };
        if *version != wire::VERSION
            || *epoch == 0
            || *owner_epoch == 0
            || [*epoch, *owner_epoch, *head]
                .iter()
                .any(|n| *n > wire::MAX_SAFE_INTEGER)
            || !wire::valid_id(owner)
        {
            return Err(invalid("invalid_state"));
        }
        let tx = self.db.transaction()?;
        let (old_epoch, old_owner_epoch, cursor, old_owner): (u64, u64, u64, String) = tx
            .query_row(
                "SELECT epoch,owner_epoch,cursor,owner FROM sync3_meta WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        if old_epoch != 0 && old_epoch != *epoch {
            return Err(invalid("epoch_mismatch"));
        }
        if *head < cursor {
            return Err(invalid("server_behind"));
        }
        if *owner_epoch < old_owner_epoch {
            return Err(invalid("owner_epoch_regressed"));
        }
        if old_epoch != 0 && *owner_epoch == old_owner_epoch && old_owner != *owner {
            return Err(invalid("owner_changed_without_epoch"));
        }
        tx.execute(
            "UPDATE sync3_meta SET epoch=?,owner=?,owner_epoch=? WHERE singleton=1",
            params![epoch, owner, owner_epoch],
        )?;
        tx.commit()?;
        Ok(*head)
    }
    /// Persist before the UI reports "queued". No optimistic mutation to the
    /// authoritative projection and no automatic regeneration of retry IDs.
    pub fn enqueue(&mut self, operation: &Operation) -> Result<(), Error> {
        operation.validate().map_err(invalid)?;
        let operation = operation.canonicalized();
        if operation.actor != self.actor {
            return Err(invalid("actor_mismatch"));
        }
        let body = serde_json::to_string(&operation)?;
        let tx = self.db.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT operation FROM sync3_outbox WHERE id=?",
                [&operation.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if serde_json::from_str::<Operation>(&existing)? != *operation {
                return Err(invalid("operation_id_conflict"));
            }
        } else {
            // An applied ID may have already been removed from the outbox.
            let committed: Option<String> = tx
                .query_row(
                    "SELECT operation FROM sync3_events WHERE id=?",
                    [&operation.id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(committed) = committed {
                if serde_json::from_str::<Operation>(&committed)? != *operation {
                    return Err(invalid("operation_id_conflict"));
                }
            } else {
                tx.execute(
                    "INSERT INTO sync3_outbox(id,operation) VALUES(?,?)",
                    params![operation.id, body],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn pending(&self) -> Result<Vec<Operation>, Error> {
        let mut query = self.db.prepare(
            "SELECT operation FROM sync3_outbox WHERE acked_seq IS NULL ORDER BY ordinal LIMIT 64",
        )?;
        let mut result = Vec::new();
        let mut bytes = serde_json::to_vec(&Request::Push {
            version: wire::VERSION,
            operations: Vec::new(),
        })?
        .len();
        for row in query.query_map([], |r| r.get::<_, String>(0))? {
            let operation: Operation = serde_json::from_str(&row?)?;
            operation.validate().map_err(invalid)?;
            let count = serde_json::to_vec(&operation)?.len() + usize::from(!result.is_empty());
            if bytes + count > wire::MAX_FRAME_BYTES {
                break;
            }
            bytes += count;
            result.push(operation);
        }
        Ok(result)
    }
    /// An ACK is not a page. It cannot advance the applied cursor across
    /// other actors' missing events. Receipt validation is transactional.
    pub fn acknowledge(&mut self, ack: &Reply) -> Result<(), Error> {
        let Reply::Ack {
            version,
            epoch,
            receipts,
        } = ack
        else {
            return Err(invalid("expected_ack"));
        };
        if *version != wire::VERSION
            || *epoch == 0
            || self.epoch()? != *epoch
            || receipts.len() > wire::MAX_BATCH_OPS
        {
            return Err(invalid("invalid_ack"));
        }
        let tx = self.db.transaction()?;
        for receipt in receipts {
            if !wire::valid_id(&receipt.id)
                || receipt.seq == 0
                || receipt.seq > wire::MAX_SAFE_INTEGER
            {
                return Err(invalid("invalid_receipt"));
            }
            let old: Option<Option<u64>> = tx
                .query_row(
                    "SELECT acked_seq FROM sync3_outbox WHERE id=?",
                    [&receipt.id],
                    |r| r.get(0),
                )
                .optional()?;
            match old {
                Some(Some(seq)) if seq != receipt.seq => return Err(invalid("receipt_conflict")),
                Some(_) => {
                    tx.execute(
                        "UPDATE sync3_outbox SET acked_seq=? WHERE id=?",
                        params![receipt.seq, receipt.id],
                    )?;
                }
                None => {
                    let applied: Option<u64> = tx
                        .query_row(
                            "SELECT seq FROM sync3_events WHERE id=?",
                            [&receipt.id],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if applied != Some(receipt.seq) {
                        return Err(invalid("unknown_receipt"));
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
    /// Replay + cursor + outbox retirement are ONE durable transaction. A bad
    /// row anywhere rolls back the complete page, including earlier rows.
    pub fn apply_page(&mut self, page: &Reply) -> Result<(), Error> {
        let Reply::Page {
            version,
            epoch,
            through,
            next,
            rows,
            done,
        } = page
        else {
            return Err(invalid("expected_page"));
        };
        if *version != wire::VERSION
            || *epoch == 0
            || self.epoch()? != *epoch
            || *through > wire::MAX_SAFE_INTEGER
            || *next > *through
            || (*done != (*next == *through))
            || rows.len() > wire::MAX_BATCH_OPS
            || serde_json::to_vec(page)?.len() > wire::MAX_FRAME_BYTES
        {
            return Err(invalid("invalid_page"));
        }
        let tx = self.db.transaction()?;
        let mut cursor: u64 =
            tx.query_row("SELECT cursor FROM sync3_meta WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        if *next < cursor {
            return Err(invalid("stale_page"));
        }
        let mut previous = None;
        for Row { seq, operation } in rows {
            operation.validate().map_err(invalid)?;
            let operation = operation.canonicalized();
            if *seq == 0 || *seq > *next || previous.is_some_and(|p| *seq != p + 1) {
                return Err(invalid("cursor_gap"));
            }
            previous = Some(*seq);
            if *seq <= cursor {
                let existing: Option<(String, String)> = tx
                    .query_row(
                        "SELECT id,operation FROM sync3_events WHERE seq=?",
                        [seq],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                match existing {
                    Some((id, body))
                        if id == operation.id
                            && serde_json::from_str::<Operation>(&body)? == *operation =>
                    {
                        continue;
                    }
                    _ => return Err(invalid("history_conflict")),
                }
            }
            if *seq != cursor + 1 {
                return Err(invalid("cursor_gap"));
            }
            // Historical owner epochs are legal here: the authenticated
            // server fenced writes at commit time. Current-owner checks on
            // old committed rows would make ownership migration unreplayable.
            projection_store::apply(&tx, &operation, *seq)?;
            let pending: Option<(String, Option<u64>)> = tx
                .query_row(
                    "SELECT operation,acked_seq FROM sync3_outbox WHERE id=?",
                    [&operation.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((pending, receipt)) = pending {
                if serde_json::from_str::<Operation>(&pending)? != *operation
                    || receipt.is_some_and(|s| s != *seq)
                {
                    return Err(invalid("receipt_conflict"));
                }
                tx.execute("DELETE FROM sync3_outbox WHERE id=?", [&operation.id])?;
            }
            tx.execute(
                "INSERT INTO sync3_events VALUES(?,?,?)",
                params![seq, operation.id, serde_json::to_string(&operation)?],
            )?;
            cursor = *seq;
        }
        if cursor != *next {
            return Err(invalid("cursor_gap"));
        }
        if rows.is_empty() && !*done {
            return Err(invalid("empty_page"));
        }
        tx.execute("UPDATE sync3_meta SET cursor=? WHERE singleton=1", [cursor])?;
        tx.commit()?;
        Ok(())
    }
}

/// Transport lifecycle. Health comes from business progress, not ping/pong.
/// A callback from a retired connection must pass this generation check
/// BEFORE it can change persistent state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Offline,
    Connecting,
    CatchingUp,
    Live,
    Suspect,
    Repairing,
}

#[derive(Debug)]
pub struct Lifecycle {
    pub phase: Phase,
    generation: u64,
}
impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            phase: Phase::Offline,
            generation: 0,
        }
    }
}
impl Lifecycle {
    pub fn connect(&mut self) -> u64 {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("connection generation exhausted");
        self.phase = Phase::Connecting;
        self.generation
    }
    pub fn current(&self, generation: u64) -> bool {
        self.generation == generation && self.phase != Phase::Offline
    }
    pub fn transition(&mut self, generation: u64, phase: Phase) -> bool {
        if !self.current(generation) {
            return false;
        }
        self.phase = phase;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::{Command, Event, Receipt};
    fn operation() -> Operation {
        Operation {
            id: "queued".into(),
            actor: "phone".into(),
            owner_epoch: 1,
            event: Event::CommandQueued {
                command_id: "cmd".into(),
                command: Command::Send {
                    text: "hello".into(),
                },
            },
        }
    }
    fn state(head: u64) -> Reply {
        Reply::State {
            version: 3,
            epoch: 1,
            owner: "host".into(),
            owner_epoch: 1,
            head,
        }
    }
    fn page(rows: Vec<Row>, next: u64) -> Reply {
        Reply::Page {
            version: 3,
            epoch: 1,
            through: next,
            next,
            done: true,
            rows,
        }
    }

    #[test]
    fn queued_operation_survives_restart_and_ack_does_not_skip_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sqlite");
        let mut j = Journal::open(&path, "account", "room", "phone").unwrap();
        j.enqueue(&operation()).unwrap();
        drop(j);
        let mut j = Journal::open(&path, "account", "room", "phone").unwrap();
        assert_eq!(j.pending().unwrap(), vec![operation()]);
        j.accept_state(&state(1)).unwrap();
        j.acknowledge(&Reply::Ack {
            version: 3,
            epoch: 1,
            receipts: vec![Receipt {
                id: "queued".into(),
                seq: 1,
            }],
        })
        .unwrap();
        assert_eq!(j.cursor().unwrap(), 0);
        assert!(j.pending().unwrap().is_empty());
        drop(j);
        let mut j = Journal::open(&path, "account", "room", "phone").unwrap();
        j.apply_page(&page(
            vec![Row {
                seq: 1,
                operation: operation(),
            }],
            1,
        ))
        .unwrap();
        assert_eq!(j.cursor().unwrap(), 1);
        assert_eq!(j.projection().unwrap().commands.len(), 1);
        j.enqueue(&operation()).unwrap();
        assert!(j.pending().unwrap().is_empty());
    }
    #[test]
    fn invalid_page_rolls_back_content_cursor_and_outbox() {
        let mut j = Journal::open(Path::new(":memory:"), "account", "room", "phone").unwrap();
        j.accept_state(&state(2)).unwrap();
        j.enqueue(&operation()).unwrap();
        let bad = Operation {
            id: "bad".into(),
            event: Event::RunStarted {
                run_id: "missing".into(),
            },
            ..operation()
        };
        assert!(
            j.apply_page(&page(
                vec![
                    Row {
                        seq: 1,
                        operation: operation()
                    },
                    Row {
                        seq: 2,
                        operation: bad
                    }
                ],
                2
            ))
            .is_err()
        );
        assert_eq!(j.cursor().unwrap(), 0);
        assert_eq!(j.projection().unwrap(), Projection::default());
        assert_eq!(j.pending().unwrap().len(), 1);
    }
    #[test]
    fn gap_conflicting_id_and_account_reuse_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sqlite");
        let mut j = Journal::open(&path, "account", "room", "phone").unwrap();
        j.accept_state(&state(2)).unwrap();
        j.enqueue(&operation()).unwrap();
        assert!(
            j.apply_page(&page(
                vec![Row {
                    seq: 2,
                    operation: operation()
                }],
                2
            ))
            .is_err()
        );
        let mut conflict = operation();
        conflict.event = Event::CommandQueued {
            command_id: "other".into(),
            command: Command::Interrupt {},
        };
        assert!(j.enqueue(&conflict).is_err());
        drop(j);
        assert!(Journal::open(&path, "other-account", "room", "phone").is_err());
        assert!(Journal::open(&path, "account", "other-room", "phone").is_err());
        let mut j = Journal::open(&path, "account", "room", "phone").unwrap();
        assert_eq!(j.pending().unwrap(), vec![operation()]);
        let mut wrong = state(2);
        if let Reply::State { epoch, .. } = &mut wrong {
            *epoch = 2;
        }
        assert!(j.accept_state(&wrong).is_err());
        let mut wrong_owner = state(2);
        if let Reply::State { owner, .. } = &mut wrong_owner {
            *owner = "different-owner".into();
        }
        assert!(j.accept_state(&wrong_owner).is_err());
    }
    #[test]
    fn golden_projection_commits_atomically_and_replays_idempotently() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
        let ops: Vec<Operation> = serde_json::from_value(fixture["operations"].clone()).unwrap();
        let mut j = Journal::open(Path::new(":memory:"), "account", "room", "phone").unwrap();
        j.accept_state(&state(ops.len() as u64)).unwrap();
        let page = page(
            ops.into_iter()
                .enumerate()
                .map(|(i, operation)| Row {
                    seq: i as u64 + 1,
                    operation,
                })
                .collect(),
            10,
        );
        j.apply_page(&page).unwrap();
        j.apply_page(&page).unwrap();
        assert_eq!(
            serde_json::to_value(j.projection().unwrap()).unwrap(),
            fixture["projection"]
        );
        assert!(j.accept_state(&state(9)).is_err());
    }
    #[test]
    fn old_transport_callback_cannot_change_phase() {
        let mut life = Lifecycle::default();
        let old = life.connect();
        let new = life.connect();
        assert!(!life.transition(old, Phase::Repairing));
        assert!(life.transition(new, Phase::Live));
        assert!(life.transition(new, Phase::Offline));
        assert!(!life.transition(new, Phase::Live));
    }
    #[test]
    fn sparse_projection_never_rewrites_unrelated_history() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
        let ops: Vec<Operation> = serde_json::from_value(fixture["operations"].clone()).unwrap();
        let mut j = Journal::open(Path::new(":memory:"), "account", "room", "phone").unwrap();
        j.accept_state(&state(1006)).unwrap();
        for chunk in (0..1000).collect::<Vec<_>>().chunks(64) {
            let rows = chunk
                .iter()
                .map(|i| Row {
                    seq: i + 1,
                    operation: Operation {
                        id: format!("history-{i}"),
                        actor: "phone".into(),
                        owner_epoch: 1,
                        event: Event::CommandQueued {
                            command_id: format!("history-{i}"),
                            command: Command::Interrupt {},
                        },
                    },
                })
                .collect();
            j.apply_page(&page(rows, chunk.last().unwrap() + 1))
                .unwrap();
        }
        j.apply_page(&page(
            ops[..5]
                .iter()
                .enumerate()
                .map(|(i, op)| Row {
                    seq: 1001 + i as u64,
                    operation: op.clone(),
                })
                .collect(),
            1005,
        ))
        .unwrap();
        let writes = j.db.total_changes();
        j.apply_page(&page(
            vec![Row {
                seq: 1006,
                operation: ops[5].clone(),
            }],
            1006,
        ))
        .unwrap();
        // One message row, one event, one cursor; no whole-chat rewrite.
        assert_eq!(j.db.total_changes() - writes, 3);
        let changed: u64 =
            j.db.query_row(
                "SELECT count(*) FROM sync3_entities WHERE seq=1006",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(changed, 1);
        assert_eq!(j.projection().unwrap().messages["message"].text, "你好!");
    }
    #[test]
    fn javascript_number_normalization_does_not_poison_own_receipt() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/numbers.json")).unwrap();
        let source: Operation = serde_json::from_value(fixture["source"].clone()).unwrap();
        let canonical: Operation = serde_json::from_value(fixture["canonical"].clone()).unwrap();
        let mut j = Journal::open(Path::new(":memory:"), "account", "room", "phone").unwrap();
        j.enqueue(&source).unwrap();
        j.enqueue(&canonical).unwrap();
        assert_eq!(j.pending().unwrap(), vec![canonical.clone()]);
        j.accept_state(&state(1)).unwrap();
        j.apply_page(&page(
            vec![Row {
                seq: 1,
                operation: canonical,
            }],
            1,
        ))
        .unwrap();
        j.enqueue(&source).unwrap();
        assert!(j.pending().unwrap().is_empty());
        assert_eq!(
            serde_json::to_value(&j.projection().unwrap().commands["numeric-command"].command)
                .unwrap(),
            fixture["canonical"]["event"]["command"]
        );
    }
}
