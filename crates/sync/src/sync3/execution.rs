//! Durable local dispatch gate. A committed server claim is necessary but not
//! sufficient: only its locally prepared intent may obtain a one-shot permit.
//! Unknown post-crash effects are recovery work, never an automatic redispatch.
//!
//! This does not implement payload-specific policy (expiry, based-on, artifacts,
//! target eligibility) or invoke a harness. The host must validate that policy
//! before preparation and again before consuming a permit.
use super::{Error, Journal, enqueue_into, invalid};
use cypher_proto::{
    SessionCommandEntry, SessionCommandPayload, SessionCommandStatus,
    sync3::{CommandState, Event, MessageState, Operation, Outcome, RunState},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod observation;
mod source;
pub use observation::ObservationPermit;
pub use source::{SourceEvent, SourcePage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Plan {
    /// A new semantic turn, not necessarily a new harness process.
    Run,
    /// Control an existing turn, or perform a non-run command when None.
    Control { run_id: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Prepared,
    Claimed,
    Declined,
    Settled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub command_id: String,
    pub run_id: String,
    pub state: State,
}

pub enum Progress {
    WaitingForClaim,
    WaitingForRun,
    Declined,
    /// The permit may already have caused effects. Never issue another.
    RecoveryRequired,
    Settled,
    Dispatch(DispatchPermit),
}

/// Not Clone or serializable. A lost permit cannot be re-created from SQLite.
/// Owning this only proves the durable dispatch gate; payload policy is still
/// the host's responsibility. Do not log the command's private prompt.
pub struct DispatchPermit {
    intent: Intent,
    command: SessionCommandEntry,
}
impl DispatchPermit {
    pub fn command(&self) -> &SessionCommandEntry {
        &self.command
    }
    pub fn run_id(&self) -> &str {
        &self.intent.run_id
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    scope: [u8; 32],
    actor: String,
    epoch: u64,
    owner_epoch: u64,
    command_id: String,
    command_hash: [u8; 32],
    nonce: String,
    run_id: String,
    plan: Plan,
    state: State,
    run_enqueued: bool,
    terminal: Option<Vec<Operation>>,
}
impl Intent {
    fn info(&self) -> Info {
        Info {
            command_id: self.command_id.clone(),
            run_id: self.run_id.clone(),
            state: self.state,
        }
    }
    fn operation(&self, suffix: &str, event: Event) -> Operation {
        Operation {
            id: format!("intent-{}-{suffix}", self.nonce),
            actor: self.actor.clone(),
            owner_epoch: self.owner_epoch,
            event,
        }
    }
    fn claim(&self) -> Operation {
        self.operation(
            "claim",
            Event::CommandClaimAttempted {
                command_id: self.command_id.clone(),
                run_id: self.run_id.clone(),
            },
        )
    }
    fn start(&self) -> Operation {
        self.operation(
            "start",
            Event::RunStarted {
                run_id: self.run_id.clone(),
            },
        )
    }
}

impl Journal {
    /// Recovery of a claim whose owning process no longer holds a permit.
    /// The Engine must exclude its in-process claims before calling this.
    /// Closes incomplete public messages and rejects the uncertain command;
    /// it never returns or reconstructs a dispatch permit.
    pub fn quarantine_execution(&mut self, command_id: &str, note: &str) -> Result<bool, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut intent) = load(&tx, command_id, &self.scope, &self.actor)? else {
            return Err(invalid("unknown_execution_intent"));
        };
        if intent.state == State::Settled {
            return Ok(false);
        }
        if intent.state != State::Claimed {
            return Err(invalid("execution_not_uncertain"));
        }
        check_fence(&tx, &intent)?;
        let mut operations = recovery_messages(
            &tx,
            &intent.run_id,
            &intent.actor,
            &intent.nonce,
            |suffix, event| intent.operation(suffix, event),
            note,
        )?;
        if let Some(run) = entity::<RunState>(&tx, "runs", &intent.run_id)? {
            if run.outcome.is_none() && matches!(intent.plan, Plan::Run) {
                operations.push(intent.operation(
                    "recovery-run",
                    Event::RunFinished {
                        run_id: intent.run_id.clone(),
                        outcome: Outcome::Failed,
                    },
                ));
            }
        }
        operations.push(intent.operation(
            "recovery-command",
            Event::CommandResolved {
                command_id: command_id.into(),
                status: SessionCommandStatus::Rejected,
                resolution: Some(note.into()),
            },
        ));
        for operation in &operations {
            operation.validate().map_err(invalid)?;
            enqueue_into(&tx, operation)?;
        }
        intent.state = State::Settled;
        intent.terminal = Some(operations);
        save(&tx, &intent)?;
        tx.commit()?;
        Ok(true)
    }
    /// Persist the intent and ONLY its claim attempt together. A speculative
    /// run start must not accompany a claim that could lose server arbitration.
    pub fn prepare_execution(&mut self, command_id: &str, plan: Plan) -> Result<Info, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(intent) = load(&tx, command_id, &self.scope, &self.actor)? {
            if intent.plan != plan {
                return Err(invalid("execution_intent_conflict"));
            }
            return Ok(intent.info());
        }
        let (epoch, owner_epoch) = fence(&tx, &self.actor)?;
        let command: CommandState =
            entity(&tx, "commands", command_id)?.ok_or_else(|| invalid("unknown_command"))?;
        validate_plan(&command.command.payload, &plan)?;
        if command.command.status != SessionCommandStatus::Pending
            || command.run_id.is_some()
            || command.accepted_op_id.is_some()
        {
            return Err(invalid("execution_requires_reconciliation"));
        }
        let nonce = uuid::Uuid::new_v4().to_string();
        let run_id = match &plan {
            Plan::Control { run_id: Some(run) } => {
                live(&tx, run)?;
                run.clone()
            }
            _ => format!("run-{nonce}"),
        };
        let intent = Intent {
            version: 1,
            scope: self.scope,
            actor: self.actor.clone(),
            epoch,
            owner_epoch,
            command_id: command_id.into(),
            command_hash: command_hash(command_id, &command)?,
            nonce,
            run_id,
            plan,
            state: State::Prepared,
            run_enqueued: false,
            terminal: None,
        };
        let claim = intent.claim();
        claim.validate().map_err(invalid)?;
        enqueue_into(&tx, &claim)?;
        save(&tx, &intent)?;
        tx.commit()?;
        Ok(intent.info())
    }

    pub fn execution_info(&self, command_id: &str) -> Result<Option<Info>, Error> {
        Ok(load(&self.db, command_id, &self.scope, &self.actor)?.map(|i| i.info()))
    }

    /// Poll after committed-cursor progress, not after network send/ACK alone.
    /// The Claimed state commits BEFORE the only permit is returned.
    pub fn advance_execution(&mut self, command_id: &str) -> Result<Progress, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut intent = load(&tx, command_id, &self.scope, &self.actor)?
            .ok_or_else(|| invalid("unknown_execution_intent"))?;
        match intent.state {
            State::Claimed => return Ok(Progress::RecoveryRequired),
            State::Declined => return Ok(Progress::Declined),
            State::Settled => return Ok(Progress::Settled),
            State::Prepared => {}
        }
        check_fence(&tx, &intent)?;
        if !committed(&tx, &intent.claim())? {
            require_pending(&tx, &intent.claim())?;
            return Ok(Progress::WaitingForClaim);
        }
        let command: CommandState =
            entity(&tx, "commands", command_id)?.ok_or_else(|| invalid("unknown_command"))?;
        if command.accepted_op_id.as_deref() != Some(intent.claim().id.as_str()) {
            intent.state = State::Declined;
            save(&tx, &intent)?;
            tx.commit()?;
            return Ok(Progress::Declined);
        }
        if command.run_id.as_deref() != Some(intent.run_id.as_str())
            || command.command.status != SessionCommandStatus::Pending
            || command_hash(command_id, &command)? != intent.command_hash
        {
            return Err(invalid("execution_requires_reconciliation"));
        }
        validate_plan(&command.command.payload, &intent.plan)?;
        match &intent.plan {
            Plan::Run => {
                if !committed(&tx, &intent.start())? {
                    if intent.run_enqueued {
                        require_pending(&tx, &intent.start())?;
                        return Ok(Progress::WaitingForRun);
                    }
                    let start = intent.start();
                    start.validate().map_err(invalid)?;
                    enqueue_into(&tx, &start)?;
                    intent.run_enqueued = true;
                    save(&tx, &intent)?;
                    tx.commit()?;
                    return Ok(Progress::WaitingForRun);
                }
                if !intent.run_enqueued {
                    return Err(invalid("execution_requires_reconciliation"));
                }
                live(&tx, &intent.run_id)?;
            }
            Plan::Control { run_id: Some(run) } => live(&tx, run)?,
            Plan::Control { run_id: None } => {}
        }
        intent.state = State::Claimed;
        save(&tx, &intent)?;
        tx.commit()?;
        Ok(Progress::Dispatch(DispatchPermit {
            intent,
            command: command.command,
        }))
    }

    /// Record the observed result and terminal outbox operations atomically.
    /// Run output must already be durably enqueued/finalized by the producer.
    /// Retrying this exact result is safe, but it never grants another permit.
    pub fn complete_execution(
        &mut self,
        permit: &DispatchPermit,
        outcome: Option<Outcome>,
        status: SessionCommandStatus,
        resolution: Option<String>,
    ) -> Result<(), Error> {
        if permit.intent.scope != self.scope || permit.intent.actor != self.actor {
            return Err(invalid("execution_scope_mismatch"));
        }
        if matches!(permit.intent.plan, Plan::Run) != outcome.is_some() {
            return Err(invalid("execution_outcome_mismatch"));
        }
        let mut operations = Vec::new();
        if let Some(outcome) = outcome {
            operations.push(permit.intent.operation(
                "finish",
                Event::RunFinished {
                    run_id: permit.intent.run_id.clone(),
                    outcome,
                },
            ));
        }
        operations.push(permit.intent.operation(
            "resolve",
            Event::CommandResolved {
                command_id: permit.intent.command_id.clone(),
                status,
                resolution,
            },
        ));
        for op in &operations {
            op.validate().map_err(invalid)?;
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut intent = load(&tx, &permit.intent.command_id, &self.scope, &self.actor)?
            .ok_or_else(|| invalid("unknown_execution_intent"))?;
        let mut expected = permit.intent.clone();
        if intent.state == State::Settled {
            expected.state = State::Settled;
            expected.terminal = Some(operations.clone());
            if intent != expected {
                return Err(invalid("execution_result_conflict"));
            }
            // Do not resurrect a pruned result; existing settled state is the
            // durable acknowledgement of this exact local completion.
            return Ok(());
        }
        if intent != expected || intent.state != State::Claimed {
            return Err(invalid("execution_intent_conflict"));
        }
        check_fence(&tx, &intent)?;
        if matches!(intent.plan, Plan::Run) {
            let unfinished: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sync3_writers WHERE json_extract(header,'$.config.run_id')=?
                 AND json_type(header,'$.phase.Closed') IS NULL)", [&intent.run_id], |r| r.get(0))?;
            if unfinished {
                return Err(invalid("execution_output_not_finalized"));
            }
        }
        for operation in &operations {
            enqueue_into(&tx, operation)?;
        }
        intent.state = State::Settled;
        intent.terminal = Some(operations);
        save(&tx, &intent)?;
        tx.commit()?;
        Ok(())
    }
}

fn recovery_messages(
    db: &Connection,
    run: &str,
    actor: &str,
    nonce: &str,
    operation: impl Fn(&str, Event) -> Operation,
    note: &str,
) -> Result<Vec<Operation>, Error> {
    let mut operations = Vec::new();
    let mut roots = std::collections::BTreeMap::new();
    let mut query = db.prepare(
        "SELECT body FROM sync3_entities WHERE kind='messages' AND run_id=?
         AND json_extract(body,'$.entry.status')='streaming' ORDER BY created_seq,id",
    )?;
    for (index, body) in query
        .query_map([run], |r| r.get::<_, String>(0))?
        .enumerate()
    {
        let message: MessageState = serde_json::from_str(&body?)?;
        roots.insert(
            message
                .entry
                .continuation_of
                .clone()
                .unwrap_or_else(|| message.entry.id.clone()),
            message.entry.created_at,
        );
        operations.push(operation(
            &format!("recovery-message-{index}"),
            Event::MessageFinished {
                message_id: message.entry.id,
                status: Some(cypher_proto::MessageStatus::Aborted),
            },
        ));
    }
    // A dedicated bounded continuation remains safe when the old chunk is
    // full or has retained writer frames still awaiting acknowledgement.
    for (index, (root, created_at)) in roots.into_iter().enumerate() {
        let id = format!("recovery-{nonce}-{index}");
        operations.push(operation(
            &format!("recovery-note-{index}"),
            Event::MessageCreated {
                message_id: id.clone(),
                run_id: Some(run.into()),
                role: cypher_proto::MessageRole::Assistant,
                device_id: actor.into(),
                created_at: created_at as u64,
                continuation_of: Some(root),
            },
        ));
        operations.push(operation(
            &format!("recovery-note-part-{index}"),
            Event::PartPut {
                message_id: id.clone(),
                index: 0,
                part: cypher_proto::MessagePart::Error {
                    id: "engine-recovery".into(),
                    message: note.into(),
                },
            },
        ));
        operations.push(operation(
            &format!("recovery-note-end-{index}"),
            Event::MessageFinished {
                message_id: id,
                status: Some(cypher_proto::MessageStatus::Aborted),
            },
        ));
    }
    Ok(operations)
}

fn validate_plan(payload: &SessionCommandPayload, plan: &Plan) -> Result<(), Error> {
    let valid = matches!(
        (payload, plan),
        (SessionCommandPayload::Run { .. }, Plan::Run)
            | (
                SessionCommandPayload::Steer { .. },
                Plan::Run | Plan::Control { run_id: Some(_) }
            )
            | (SessionCommandPayload::Interrupt {}, Plan::Control { .. })
            | (
                SessionCommandPayload::RespondInput { .. },
                Plan::Control { run_id: Some(_) }
            )
    );
    if !valid {
        return Err(invalid("execution_plan_mismatch"));
    }
    Ok(())
}

fn fence(db: &Connection, actor: &str) -> Result<(u64, u64), Error> {
    let (epoch, owner, owner_epoch): (u64, String, u64) = db.query_row(
        "SELECT epoch,owner,owner_epoch FROM sync3_meta WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if epoch == 0 || owner_epoch == 0 {
        return Err(invalid("execution_not_synced"));
    }
    if owner != actor {
        return Err(invalid("not_owner"));
    }
    Ok((epoch, owner_epoch))
}
fn check_fence(db: &Connection, intent: &Intent) -> Result<(), Error> {
    if fence(db, &intent.actor)? != (intent.epoch, intent.owner_epoch) {
        return Err(invalid("execution_fence_changed"));
    }
    Ok(())
}
fn load(db: &Connection, id: &str, scope: &[u8; 32], actor: &str) -> Result<Option<Intent>, Error> {
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM sync3_execution_intents WHERE command_id=?",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(body) = body else { return Ok(None) };
    let intent: Intent = serde_json::from_str(&body)?;
    if intent.version != 1
        || &intent.scope != scope
        || intent.actor != actor
        || intent.command_id != id
        || (intent.state == State::Settled) != intent.terminal.is_some()
        || match &intent.plan {
            Plan::Control { run_id: Some(run) } => run != &intent.run_id,
            _ => intent.run_id != format!("run-{}", intent.nonce),
        }
    {
        return Err(invalid("invalid_execution_intent"));
    }
    intent.claim().validate().map_err(invalid)?;
    Ok(Some(intent))
}
fn save(db: &Connection, intent: &Intent) -> Result<(), Error> {
    db.execute(
        "INSERT INTO sync3_execution_intents(command_id,body) VALUES(?,?)
        ON CONFLICT(command_id) DO UPDATE SET body=excluded.body",
        params![intent.command_id, serde_json::to_string(intent)?],
    )?;
    Ok(())
}
fn entity<T: serde::de::DeserializeOwned>(
    db: &Connection,
    kind: &str,
    id: &str,
) -> Result<Option<T>, Error> {
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM sync3_entities WHERE kind=? AND id=?",
            params![kind, id],
            |r| r.get(0),
        )
        .optional()?;
    body.map(|body| serde_json::from_str(&body).map_err(Error::from))
        .transpose()
}
fn live(db: &Connection, run: &str) -> Result<(), Error> {
    if !matches!(
        entity::<RunState>(db, "runs", run)?,
        Some(RunState { outcome: None })
    ) {
        return Err(invalid("run_not_live"));
    }
    Ok(())
}
fn committed(db: &Connection, op: &Operation) -> Result<bool, Error> {
    let body: Option<String> = db
        .query_row(
            "SELECT operation FROM sync3_events WHERE id=?",
            [&op.id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(body) = body else { return Ok(false) };
    if serde_json::from_str::<Operation>(&body)? != *op {
        return Err(invalid("operation_id_conflict"));
    }
    Ok(true)
}
fn require_pending(db: &Connection, op: &Operation) -> Result<(), Error> {
    let body: Option<String> = db
        .query_row(
            "SELECT operation FROM sync3_outbox WHERE id=?",
            [&op.id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(body) = body else {
        return Err(invalid("execution_receipt_missing"));
    };
    if serde_json::from_str::<Operation>(&body)? != *op {
        return Err(invalid("operation_id_conflict"));
    }
    Ok(())
}
fn command_hash(id: &str, command: &CommandState) -> Result<[u8; 32], Error> {
    let op = Operation {
        id: "intent-source".into(),
        actor: command.actor.clone(),
        owner_epoch: 1,
        event: Event::CommandQueued {
            command_id: id.into(),
            command: command.command.clone(),
        },
    };
    op.validate().map_err(invalid)?;
    Ok(Sha256::digest(serde_json::to_vec(&op.canonicalized())?).into())
}

#[cfg(test)]
mod tests;
