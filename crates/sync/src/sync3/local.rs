//! Local-only v3 authority. This is not HTTP repair or a way to ACK cloud
//! commands locally. A durable mode marker prevents a remote replica from
//! becoming an authority merely because its network connection disappeared.
use super::{Error, Journal, invalid, projection_store};
use cypher_proto::sync3::{self as wire, Event, Reply};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::path::Path;

pub struct LocalAuthority {
    journal: Journal,
}

impl LocalAuthority {
    pub fn open(path: &Path, account: &str, room: &str, actor: &str) -> Result<Self, Error> {
        let mut journal = Journal::open(path, account, room, actor)?;
        let tx = journal
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS sync3_local_authority(
                singleton INTEGER PRIMARY KEY CHECK(singleton=1), actor TEXT NOT NULL);",
        )?;
        let owner: Option<String> = tx
            .query_row(
                "SELECT actor FROM sync3_local_authority WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let (epoch, cursor, stored_owner, owner_epoch): (u64, u64, String, u64) = tx.query_row(
            "SELECT epoch,cursor,owner,owner_epoch FROM sync3_meta WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        match owner {
            Some(owner)
                if owner == actor && stored_owner == actor && epoch == 1 && owner_epoch == 1 => {}
            Some(_) => return Err(invalid("local_authority_mismatch")),
            None => {
                let used: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sync3_events) OR EXISTS(SELECT 1 FROM sync3_outbox)",
                    [], |r| r.get(0),
                )?;
                if epoch != 0 || cursor != 0 || used {
                    return Err(invalid("replica_is_not_local_authority"));
                }
                tx.execute("INSERT INTO sync3_local_authority VALUES(1,?)", [actor])?;
                tx.execute(
                    "UPDATE sync3_meta SET epoch=1,owner=?,owner_epoch=1 WHERE singleton=1",
                    [actor],
                )?;
            }
        }
        tx.commit()?;
        Ok(Self { journal })
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }
    pub fn journal_mut(&mut self) -> &mut Journal {
        &mut self.journal
    }

    /// Commit at most one wire-sized page. Sequence assignment, validation,
    /// sparse projection, receipt and outbox retirement are a single SQLite
    /// transaction. A later invalid operation rolls back the entire batch.
    pub fn commit_pending(&mut self) -> Result<usize, Error> {
        let operations = self.journal.pending()?;
        if operations.is_empty() {
            return Ok(0);
        }
        let tx = self
            .journal
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (owner, epoch, owner_epoch, mut head): (String, u64, u64, u64) = tx.query_row(
            "SELECT owner,epoch,owner_epoch,cursor FROM sync3_meta WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        if epoch != 1 || owner_epoch != 1 || owner != self.journal.actor {
            return Err(invalid("local_authority_mismatch"));
        }
        for operation in &operations {
            operation.validate().map_err(invalid)?;
            if operation.owner_epoch != owner_epoch {
                return Err(invalid("stale_owner_epoch"));
            }
            if !matches!(
                operation.event,
                Event::CommandQueued { .. } | Event::CommandCancelAttempted { .. }
            ) && operation.actor != owner
            {
                return Err(invalid("not_owner"));
            }
            let prior: Option<(u64, String)> = tx
                .query_row(
                    "SELECT seq,operation FROM sync3_events WHERE id=?",
                    [&operation.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((_, body)) = prior {
                if serde_json::from_str::<wire::Operation>(&body)? != *operation.canonicalized() {
                    return Err(invalid("operation_id_conflict"));
                }
            } else {
                head = head
                    .checked_add(1)
                    .filter(|v| *v <= wire::MAX_SAFE_INTEGER)
                    .ok_or_else(|| invalid("sequence_exhausted"))?;
                projection_store::apply(&tx, operation, head)?;
                tx.execute(
                    "INSERT INTO sync3_events(seq,id,operation) VALUES(?,?,?)",
                    params![
                        head,
                        operation.id,
                        serde_json::to_string(&operation.canonicalized())?
                    ],
                )?;
            }
            tx.execute("DELETE FROM sync3_outbox WHERE id=?", [&operation.id])?;
        }
        tx.execute("UPDATE sync3_meta SET cursor=? WHERE singleton=1", [head])?;
        tx.commit()?;
        Ok(operations.len())
    }

    pub fn state(&self) -> Result<Reply, Error> {
        let (owner, owner_epoch) = self.journal.owner()?;
        Ok(Reply::State {
            version: wire::VERSION,
            epoch: self.journal.epoch()?,
            owner,
            owner_epoch,
            head: self.journal.cursor()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync3::execution::{Plan, Progress};

    fn queued() -> wire::Operation {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../../fixtures/sync3/golden.json")).unwrap();
        serde_json::from_value(fixture["operations"][0].clone()).unwrap()
    }

    #[test]
    fn durable_local_decision_has_same_one_shot_gate_as_cloud() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.sqlite");
        let mut local = LocalAuthority::open(&path, "account", "room", "host").unwrap();
        let mut op = queued();
        op.actor = "host".into();
        if let Event::CommandQueued { command, .. } = &mut op.event {
            command.issued_by = "host".into();
        }
        local.journal_mut().enqueue(&op).unwrap();
        local.commit_pending().unwrap();
        local
            .journal_mut()
            .prepare_execution("command", Plan::Run)
            .unwrap();
        assert!(matches!(
            local.journal_mut().advance_execution("command").unwrap(),
            Progress::WaitingForClaim
        ));
        local.commit_pending().unwrap();
        assert!(matches!(
            local.journal_mut().advance_execution("command").unwrap(),
            Progress::WaitingForRun
        ));
        local.commit_pending().unwrap();
        assert!(matches!(
            local.journal_mut().advance_execution("command").unwrap(),
            Progress::Dispatch(_)
        ));
        let head = local.journal().cursor().unwrap();
        drop(local);
        let mut reopened = LocalAuthority::open(&path, "account", "room", "host").unwrap();
        assert_eq!(reopened.journal().cursor().unwrap(), head);
        assert!(matches!(
            reopened.journal_mut().advance_execution("command").unwrap(),
            Progress::RecoveryRequired
        ));
    }

    #[test]
    fn cloud_replica_cannot_become_local_authority_on_reconnect_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote.sqlite");
        let mut remote = Journal::open(&path, "account", "room", "host").unwrap();
        remote
            .accept_state(&Reply::State {
                version: 3,
                epoch: 1,
                owner: "host".into(),
                owner_epoch: 1,
                head: 0,
            })
            .unwrap();
        drop(remote);
        assert!(
            matches!(LocalAuthority::open(&path, "account", "room", "host"),
            Err(Error::Protocol(code)) if code == "replica_is_not_local_authority")
        );
    }

    #[test]
    fn failed_local_batch_retains_outbox_and_commits_no_partial_projection() {
        let dir = tempfile::tempdir().unwrap();
        let mut local =
            LocalAuthority::open(&dir.path().join("local.sqlite"), "account", "room", "host")
                .unwrap();
        let mut op = queued();
        op.actor = "host".into();
        if let Event::CommandQueued { command, .. } = &mut op.event {
            command.issued_by = "host".into();
        }
        let bad = wire::Operation {
            id: "bad".into(),
            actor: "host".into(),
            owner_epoch: 1,
            event: Event::RunStarted {
                run_id: "unclaimed".into(),
            },
        };
        local.journal_mut().enqueue_batch(&[op, bad]).unwrap();
        assert!(local.commit_pending().is_err());
        assert_eq!(local.journal().cursor().unwrap(), 0);
        assert!(local.journal().projection().unwrap().commands.is_empty());
        assert_eq!(local.journal().pending().unwrap().len(), 2);
    }
}
