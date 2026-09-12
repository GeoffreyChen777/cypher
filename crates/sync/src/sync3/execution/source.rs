//! Private raw-event retention. These rows never enter the cloud outbox.
//! Stable, caller-assigned ordinals allow retry after a lost SQLite result.
//! A source receipt proves persistence, not another execution authorization.
use super::*;
use cypher_proto::AgentEvent;

const MAX_EVENTS: usize = 32;
const PAGE_BYTES: usize = 1024 * 1024;

/// Deliberately not Debug/Serialize: this may include private file content.
pub struct SourceEvent {
    pub seq: u64,
    pub event: AgentEvent,
    /// Observed after local completion was recorded. Retain for reconciliation,
    /// never automatically reopen the closed semantic run to publish it.
    pub after_completion: bool,
}

pub struct SourcePage {
    /// The source head in the same read transaction as these events.
    pub through: u64,
    pub next: u64,
    pub events: Vec<SourceEvent>,
}

impl Journal {
    /// Build a producer for this exact dispatched run rather than accepting
    /// a caller-supplied owner epoch or run ID.
    pub fn new_execution_writer(
        &self,
        permit: &DispatchPermit,
        entry: &cypher_proto::SessionMessageEntry,
    ) -> Result<crate::sync3::writer::TranscriptWriter, Error> {
        let tx = self.db.unchecked_transaction()?;
        let intent = match_permit(&tx, &self.scope, &self.actor, permit)?;
        if intent.state != State::Claimed {
            return Err(invalid("execution_not_running"));
        }
        check_fence(&tx, &intent)?;
        live(&tx, &intent.run_id)?;
        let writer = self.new_writer(intent.owner_epoch, Some(intent.run_id), entry)?;
        tx.commit()?;
        Ok(writer)
    }

    /// Publish only after the source event was durably retained. Ownership,
    /// intent, source position and producer checkpoint are checked in the
    /// SAME write transaction. A frame cannot cross run or ownership fences.
    pub fn enqueue_execution_frame(
        &mut self,
        permit: &DispatchPermit,
        source_seq: u64,
        frame: &crate::sync3::writer::Frame,
    ) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let intent = match_permit(&tx, &self.scope, &self.actor, permit)?;
        if intent.state != State::Claimed {
            return Err(invalid("execution_not_running"));
        }
        check_fence(&tx, &intent)?;
        live(&tx, &intent.run_id)?;
        if frame.execution_context() != (intent.owner_epoch, Some(intent.run_id.as_str())) {
            return Err(invalid("execution_frame_mismatch"));
        }
        let retained: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sync3_execution_source_events WHERE command_id=? AND seq=?)",
            params![intent.command_id, source_seq], |r| r.get(0))?;
        if !retained {
            return Err(invalid("execution_source_missing"));
        }
        Self::write_writer_frame(&tx, &self.scope, &self.actor, frame)?;
        tx.commit()?;
        Ok(())
    }

    /// Persist observed events BEFORE folding/publishing them. `first` starts
    /// at one and is retained unchanged when a caller retries an uncertain
    /// result. Exact overlap is accepted; gaps/changed bodies are errors.
    ///
    /// A batch has <=32 events and <=1 MiB, except a single event may exceed
    /// that byte budget. Raw tools are not constrained by public wire limits.
    /// This is local SQLite storage, not an artifact/publication policy.
    ///
    /// Deliberately does NOT require the current ownership fence: a previously
    /// dispatched task must retain late observations in its original scope
    /// even after losing permission to publish. No outbox rows are written.
    pub fn append_execution_events(
        &mut self,
        permit: &DispatchPermit,
        first: u64,
        events: &[AgentEvent],
    ) -> Result<u64, Error> {
        if first == 0 || events.is_empty() || events.len() > MAX_EVENTS {
            return Err(invalid("invalid_execution_source_batch"));
        }
        let last = first
            .checked_add(events.len() as u64 - 1)
            .filter(|n| *n <= cypher_proto::sync3::MAX_SAFE_INTEGER)
            .ok_or_else(|| invalid("execution_source_exhausted"))?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let intent = match_permit(&tx, &self.scope, &self.actor, permit)?;
        let head = head(&tx, &intent.command_id)?;
        if first > head + 1 {
            return Err(invalid("execution_source_gap"));
        }
        let mut bytes = 0usize;
        for (offset, event) in events.iter().enumerate() {
            let seq = first + offset as u64;
            let body = serde_json::to_string(event)?;
            bytes = bytes
                .checked_add(body.len())
                .ok_or_else(|| invalid("execution_source_exhausted"))?;
            if events.len() > 1 && bytes > PAGE_BYTES {
                return Err(invalid("execution_source_batch_too_large"));
            }
            let digest: [u8; 32] = Sha256::digest(body.as_bytes()).into();
            let old: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT digest FROM sync3_execution_source_events WHERE command_id=? AND seq=?",
                    params![intent.command_id, seq],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(old) = old {
                if old.as_slice() != digest {
                    return Err(invalid("execution_source_conflict"));
                }
            } else {
                if seq <= head {
                    return Err(invalid("execution_source_gap"));
                }
                tx.execute(
                    "INSERT INTO sync3_execution_source_events(command_id,seq,event,digest,after_completion) VALUES(?,?,?,?,?)",
                    params![intent.command_id, seq, body, digest.as_slice(), intent.state == State::Settled])?;
            }
        }
        tx.commit()?;
        Ok(last)
    }

    /// Bounded, indexed read for local recovery. No dispatch permit is minted.
    /// Returns <=32 events / <=1 MiB, except that the FIRST single oversized
    /// event is returned intact so a reader can always make progress.
    pub fn execution_events(
        &self,
        command_id: &str,
        after: u64,
        limit: usize,
    ) -> Result<SourcePage, Error> {
        if limit == 0 || limit > MAX_EVENTS {
            return Err(invalid("invalid_execution_source_window"));
        }
        let tx = self.db.unchecked_transaction()?;
        load(&tx, command_id, &self.scope, &self.actor)?
            .ok_or_else(|| invalid("unknown_execution_intent"))?;
        let through = head(&tx, command_id)?;
        if after > through {
            return Err(invalid("execution_source_cursor_ahead"));
        }
        let mut next = after;
        let mut events = Vec::new();
        let mut used = 0usize;
        {
            let mut query = tx.prepare(
                "SELECT seq,event,digest,after_completion FROM sync3_execution_source_events
                 WHERE command_id=? AND seq>? ORDER BY seq LIMIT ?",
            )?;
            let mut rows = query.query(params![command_id, after, limit])?;
            while let Some(row) = rows.next()? {
                let seq: u64 = row.get(0)?;
                if seq != next + 1 {
                    return Err(invalid("execution_source_gap"));
                }
                let body: String = row.get(1)?;
                if !events.is_empty() && body.len() > PAGE_BYTES - used {
                    break;
                }
                let digest: Vec<u8> = row.get(2)?;
                let actual: [u8; 32] = Sha256::digest(body.as_bytes()).into();
                if digest.as_slice() != actual {
                    return Err(invalid("execution_source_corrupt"));
                }
                let event: AgentEvent = serde_json::from_str(&body)?;
                if serde_json::to_value(&event)?
                    != serde_json::from_str::<serde_json::Value>(&body)?
                {
                    return Err(invalid("execution_source_schema_mismatch"));
                }
                let completed: i64 = row.get(3)?;
                if !matches!(completed, 0 | 1) {
                    return Err(invalid("execution_source_corrupt"));
                }
                let after_completion = completed == 1;
                events.push(SourceEvent {
                    seq,
                    event,
                    after_completion,
                });
                used += body.len();
                next = seq;
                if used >= PAGE_BYTES {
                    break;
                }
            }
        }
        tx.commit()?;
        Ok(SourcePage {
            through,
            next,
            events,
        })
    }
}

fn head(db: &Connection, command_id: &str) -> Result<u64, Error> {
    Ok(db.query_row(
        "SELECT COALESCE(MAX(seq),0) FROM sync3_execution_source_events WHERE command_id=?",
        [command_id],
        |r| r.get(0),
    )?)
}

fn match_permit(
    db: &Connection,
    scope: &[u8; 32],
    actor: &str,
    permit: &DispatchPermit,
) -> Result<Intent, Error> {
    if &permit.intent.scope != scope || permit.intent.actor != actor {
        return Err(invalid("execution_scope_mismatch"));
    }
    let intent = load(db, &permit.intent.command_id, scope, actor)?
        .ok_or_else(|| invalid("unknown_execution_intent"))?;
    let mut expected = permit.intent.clone();
    expected.state = intent.state;
    expected.terminal = intent.terminal.clone();
    if intent != expected || !matches!(intent.state, State::Claimed | State::Settled) {
        return Err(invalid("execution_intent_conflict"));
    }
    Ok(intent)
}
