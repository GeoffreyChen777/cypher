//! Observation-only continuations of an admitted persistent process. These
//! permits cannot be passed to any dispatch API and do not mint commands.
use super::source::match_permit;
use super::*;
use crate::sync3::writer::{Frame, TranscriptWriter};
use cypher_proto::{SessionMessageEntry, sync3::ExecutionState};

pub struct ObservationPermit {
    marker: Marker,
}
impl ObservationPermit {
    pub fn run_id(&self) -> &str {
        &self.marker.run_id
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u32,
    scope: [u8; 32],
    actor: String,
    epoch: u64,
    owner_epoch: u64,
    command_id: String,
    execution_id: String,
    run_id: String,
    first_seq: u64,
}
impl Marker {
    fn operation(&self, suffix: &str, event: Event) -> Operation {
        Operation {
            id: format!("observed-{}-{suffix}", self.run_id),
            actor: self.actor.clone(),
            owner_epoch: self.owner_epoch,
            event,
        }
    }
    fn start(&self) -> Operation {
        self.operation(
            "start",
            Event::RunObserved {
                run_id: self.run_id.clone(),
                execution_id: self.execution_id.clone(),
            },
        )
    }
}

impl Journal {
    /// Bounded crash-recovery worklist, separate from command dispatch.
    pub fn open_observations(&self, after: &str, limit: usize) -> Result<Vec<String>, Error> {
        if limit == 0 || limit > 32 {
            return Err(invalid("invalid_observation_window"));
        }
        let mut query = self.db.prepare("SELECT run_id FROM sync3_observations WHERE last_seq IS NULL AND run_id>? ORDER BY run_id LIMIT ?")?;
        Ok(query
            .query_map(params![after, limit], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Caller excludes its live observation reservations. This closes only
    /// the semantic observation; process occupancy is deliberately retained.
    pub fn quarantine_observation(&mut self, run: &str, note: &str) -> Result<bool, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(String, Option<u64>)> = tx
            .query_row(
                "SELECT body,last_seq FROM sync3_observations WHERE run_id=?",
                [run],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((body, last)) = row else {
            return Err(invalid("unknown_observation"));
        };
        if last.is_some() {
            return Ok(false);
        }
        let marker: Marker = serde_json::from_str(&body)?;
        if marker.run_id != run {
            return Err(invalid("observation_conflict"));
        }
        check(&tx, &self.scope, &self.actor, &marker)?;
        if !committed(&tx, &marker.start())? {
            return Ok(false);
        }
        live(&tx, run)?;
        let mut operations = recovery_messages(
            &tx,
            run,
            &marker.actor,
            &marker.run_id,
            |suffix, event| marker.operation(suffix, event),
            note,
        )?;
        operations.push(marker.operation(
            "recover-end",
            Event::RunFinished {
                run_id: run.into(),
                outcome: Outcome::Failed,
            },
        ));
        for op in operations {
            op.validate().map_err(invalid)?;
            enqueue_into(&tx, &op)?;
        }
        let last: u64 = tx.query_row(
            "SELECT MAX(seq) FROM sync3_execution_source_events WHERE command_id=?",
            [&marker.command_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE sync3_observations SET last_seq=?,outcome=? WHERE run_id=?",
            params![last, serde_json::to_string(&Outcome::Failed)?, run],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Retained source must precede the observation marker. Returning this
    /// permit only permits checking its commit; no writer is created early.
    pub fn prepare_observation(
        &mut self,
        source: &DispatchPermit,
        execution_id: &str,
        run_id: &str,
        first_seq: u64,
    ) -> Result<ObservationPermit, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let intent = match_permit(&tx, &self.scope, &self.actor, source)?;
        if intent.state != State::Settled {
            return Err(invalid("source_turn_not_settled"));
        }
        check_fence(&tx, &intent)?;
        let execution = entity::<ExecutionState>(&tx, "executions", execution_id)?
            .ok_or_else(|| invalid("unknown_execution"))?;
        if execution.closed {
            return Err(invalid("execution_closed"));
        }
        if execution.actor != intent.actor || execution.owner_epoch != intent.owner_epoch {
            return Err(invalid("execution_owner_mismatch"));
        }
        retained(&tx, &intent.command_id, first_seq)?;
        let marker = Marker {
            version: 1,
            scope: self.scope,
            actor: self.actor.clone(),
            epoch: intent.epoch,
            owner_epoch: intent.owner_epoch,
            command_id: intent.command_id,
            execution_id: execution_id.into(),
            run_id: run_id.into(),
            first_seq,
        };
        marker.start().validate().map_err(invalid)?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT body FROM sync3_observations WHERE run_id=?",
                [run_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(previous) = previous {
            if serde_json::from_str::<Marker>(&previous)? != marker {
                return Err(invalid("observation_conflict"));
            }
        } else {
            let overlap: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sync3_observations WHERE command_id=? AND (last_seq IS NULL OR last_seq>=?))",
                params![marker.command_id, first_seq], |r| r.get(0),
            )?;
            if overlap {
                return Err(invalid("observation_source_overlap"));
            }
            tx.execute(
                "INSERT INTO sync3_observations(run_id,command_id,body,first_seq) VALUES(?,?,?,?)",
                params![
                    run_id,
                    marker.command_id,
                    serde_json::to_string(&marker)?,
                    first_seq
                ],
            )?;
            enqueue_into(&tx, &marker.start())?;
        }
        tx.commit()?;
        Ok(ObservationPermit { marker })
    }

    pub fn observation_ready(&self, permit: &ObservationPermit) -> Result<bool, Error> {
        check(&self.db, &self.scope, &self.actor, &permit.marker)?;
        committed(&self.db, &permit.marker.start())
    }

    pub fn new_observation_writer(
        &self,
        permit: &ObservationPermit,
        entry: &SessionMessageEntry,
    ) -> Result<TranscriptWriter, Error> {
        let tx = self.db.unchecked_transaction()?;
        ready(&tx, &self.scope, &self.actor, &permit.marker)?;
        let writer = self.new_writer(
            permit.marker.owner_epoch,
            Some(permit.marker.run_id.clone()),
            entry,
        )?;
        tx.commit()?;
        Ok(writer)
    }

    pub fn enqueue_observation_frame(
        &mut self,
        permit: &ObservationPermit,
        source_seq: u64,
        frame: &Frame,
    ) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ready(&tx, &self.scope, &self.actor, &permit.marker)?;
        let marker = &permit.marker;
        if source_seq < marker.first_seq {
            return Err(invalid("observation_source_mismatch"));
        }
        retained(&tx, &marker.command_id, source_seq)?;
        if frame.execution_context() != (marker.owner_epoch, Some(marker.run_id.as_str())) {
            return Err(invalid("execution_frame_mismatch"));
        }
        Self::write_writer_frame(&tx, &self.scope, &self.actor, frame)?;
        tx.commit()?;
        Ok(())
    }

    pub fn complete_observation(
        &mut self,
        permit: &ObservationPermit,
        source_seq: u64,
        outcome: Outcome,
    ) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let marker = &permit.marker;
        // Match immutable local identity before accepting any idempotent result.
        identity(&tx, &self.scope, &self.actor, marker)?;
        let previous: (Option<u64>, Option<String>) = tx.query_row(
            "SELECT last_seq,outcome FROM sync3_observations WHERE run_id=?",
            [&marker.run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let result = serde_json::to_string(&outcome)?;
        if let Some(last) = previous.0 {
            if last != source_seq || previous.1.as_deref() != Some(result.as_str()) {
                return Err(invalid("observation_result_conflict"));
            }
            return Ok(());
        }
        ready(&tx, &self.scope, &self.actor, marker)?;
        if source_seq < marker.first_seq {
            return Err(invalid("observation_source_mismatch"));
        }
        retained(&tx, &marker.command_id, source_seq)?;
        let unfinished: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sync3_writers WHERE json_extract(header,'$.config.run_id')=?
             AND json_type(header,'$.phase.Closed') IS NULL)", [&marker.run_id], |r| r.get(0),
        )?;
        if unfinished {
            return Err(invalid("execution_output_not_finalized"));
        }
        enqueue_into(
            &tx,
            &marker.operation(
                "finish",
                Event::RunFinished {
                    run_id: marker.run_id.clone(),
                    outcome,
                },
            ),
        )?;
        tx.execute(
            "UPDATE sync3_observations SET last_seq=?,outcome=? WHERE run_id=?",
            params![source_seq, result, marker.run_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn identity(db: &Connection, scope: &[u8; 32], actor: &str, marker: &Marker) -> Result<(), Error> {
    if marker.version != 1 || marker.scope != *scope || marker.actor != actor {
        return Err(invalid("observation_scope_mismatch"));
    }
    if marker.first_seq == 0 || marker.first_seq > cypher_proto::sync3::MAX_SAFE_INTEGER {
        return Err(invalid("invalid_observation_source"));
    }
    marker.start().validate().map_err(invalid)?;
    let (body, command, first): (String, String, u64) = db.query_row(
        "SELECT body,command_id,first_seq FROM sync3_observations WHERE run_id=?",
        [&marker.run_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if serde_json::from_str::<Marker>(&body)? != *marker
        || command != marker.command_id
        || first != marker.first_seq
    {
        return Err(invalid("observation_conflict"));
    }
    Ok(())
}
fn check(db: &Connection, scope: &[u8; 32], actor: &str, marker: &Marker) -> Result<(), Error> {
    identity(db, scope, actor, marker)?;
    let complete: bool = db.query_row(
        "SELECT last_seq IS NOT NULL FROM sync3_observations WHERE run_id=?",
        [&marker.run_id],
        |r| r.get(0),
    )?;
    if complete {
        return Err(invalid("observation_complete"));
    }
    if fence(db, actor)? != (marker.epoch, marker.owner_epoch) {
        return Err(invalid("execution_fence_changed"));
    }
    let execution = entity::<ExecutionState>(db, "executions", &marker.execution_id)?
        .ok_or_else(|| invalid("unknown_execution"))?;
    if execution.closed {
        return Err(invalid("execution_closed"));
    }
    if execution.actor != actor || execution.owner_epoch != marker.owner_epoch {
        return Err(invalid("execution_owner_mismatch"));
    }
    Ok(())
}
fn ready(db: &Connection, scope: &[u8; 32], actor: &str, marker: &Marker) -> Result<(), Error> {
    check(db, scope, actor, marker)?;
    if !committed(db, &marker.start())? {
        return Err(invalid("observation_not_committed"));
    }
    live(db, &marker.run_id)
}
fn retained(db: &Connection, command: &str, seq: u64) -> Result<(), Error> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync3_execution_source_events WHERE command_id=? AND seq=?)",
        params![command, seq],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(invalid("execution_source_missing"));
    }
    Ok(())
}
