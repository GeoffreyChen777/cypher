//! Host-side, frame-bounded producer for the native append-only event fold.
//! No full text is retained in the writer/checkpoint. Changed slot/chunk
//! metadata and the outbox frame commit together. This does not dispatch a
//! harness, transfer ownership, upload artifacts, or acknowledge execution.
use super::{Error, invalid};
use cypher_proto::{
    MessagePart, MessageStatus, SessionMessageEntry,
    parts::{continuation_id, render_parts},
    sync3::{self as wire, Event, MessageState, Operation, Request},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DELTA_BYTES: usize = 8 * 1024;
type Hash = [u8; 32];

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    scope: Hash,
    actor: String,
    owner_epoch: u64,
    run_id: Option<String>,
    entry: SessionMessageEntry,
    seed: Hash,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Header {
    version: u32,
    config: Config,
    revision: u64,
    next_op: u64,
    chunks: usize,
    slots: usize,
    seen_slots: usize,
    phase: Phase,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
enum Phase {
    Writing,
    Finishing {
        status: Option<MessageStatus>,
        remaining: usize,
    },
    Closed {
        status: Option<MessageStatus>,
    },
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    id: String,
    reserved: usize,
    parts: usize,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Slot {
    id: String,
    chunk: usize,
    index: u32,
    memo: Memo,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
enum Memo {
    Text {
        bytes: usize,
        observed_bytes: usize,
        observed_hash: Hash,
        tail_bytes: usize,
    },
    Part {
        kind: PartKind,
        hash: Hash,
        resolved: bool,
        questions: Option<Hash>,
    },
}
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum PartKind {
    Tool,
    Input,
    Error,
}
#[derive(Clone)]
struct State {
    header: Header,
    chunks: Vec<Chunk>,
    slots: Vec<Slot>,
}

/// Opaque producer frame. Only a durable sink may accept this; successful
/// network send alone is not acceptance. The journal persists its small
/// changed checkpoint rows in the same transaction as operations.
pub struct Frame {
    pub(super) root: String,
    pub(super) actor: String,
    pub(super) expected_revision: u64,
    pub(super) header: String,
    pub(super) updates: Vec<(&'static str, usize, String)>,
    pub(super) operations: Vec<Operation>,
    next: State,
    more: bool,
}
impl Frame {
    pub(super) fn execution_context(&self) -> (u64, Option<&str>) {
        (
            self.next.header.config.owner_epoch,
            self.next.header.config.run_id.as_deref(),
        )
    }
    pub(super) fn scope(&self) -> &Hash {
        &self.next.header.config.scope
    }
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }
}
#[derive(Debug, PartialEq)]
pub struct Progress {
    pub operations: usize,
    /// Call again with the current fold when backpressure permits. Each call
    /// emits at most one frame; it never drains an arbitrary backlog itself.
    pub more: bool,
}

pub struct TranscriptWriter {
    state: State,
    // Keep the exact frame on ambiguous sink failure, even if the next caller
    // supplies a longer fold. Its operation IDs/bodies must not be regenerated.
    pending: Option<Frame>,
}
impl TranscriptWriter {
    /// `entry` supplies metadata only. The fold and final status are supplied
    /// separately. No entry is emitted until there is an actual part.
    pub(super) fn new(
        scope: Hash,
        actor: String,
        owner_epoch: u64,
        run_id: Option<String>,
        entry: &SessionMessageEntry,
    ) -> Result<Self, Error> {
        let template = SessionMessageEntry {
            id: entry.id.clone(),
            role: entry.role,
            created_at: entry.created_at,
            device_id: entry.device_id.clone(),
            continuation_of: entry.continuation_of.clone(),
            parts: vec![],
            status: Some(MessageStatus::Streaming),
        };
        let mut config = Config {
            scope,
            actor,
            owner_epoch,
            run_id,
            entry: template,
            seed: [0; 32],
        };
        // Reserve enough identifier space for every possible continuation.
        if !wire::valid_entity_id(&continuation_id(&entry.id, usize::MAX)) {
            return Err(invalid("continuation_id_too_long"));
        }
        config.seed = hash(&serde_json::to_vec(&config)?);
        let state = State {
            header: Header {
                version: 2,
                config,
                revision: 0,
                next_op: 0,
                chunks: 0,
                slots: 0,
                seen_slots: 0,
                phase: Phase::Writing,
            },
            chunks: vec![],
            slots: vec![],
        };
        state
            .operation(state.created_event(0)?)?
            .validate()
            .map_err(invalid)?;
        Ok(Self {
            state,
            pending: None,
        })
    }
    pub fn root_id(&self) -> &str {
        &self.state.header.config.entry.id
    }
    pub(super) fn scope(&self) -> &Hash {
        &self.state.header.config.scope
    }
    pub(super) fn actor(&self) -> &str {
        &self.state.header.config.actor
    }
    pub(super) fn revision(&self) -> u64 {
        self.state.header.revision
    }

    pub fn sync(
        &mut self,
        folded: &[MessagePart],
        mut sink: impl FnMut(&Frame) -> Result<(), Error>,
    ) -> Result<Progress, Error> {
        if !matches!(self.state.header.phase, Phase::Writing)
            || self
                .pending
                .as_ref()
                .is_some_and(|f| !matches!(f.next.header.phase, Phase::Writing))
        {
            return Err(invalid("writer_finishing"));
        }
        if self.pending.is_some() {
            return self.flush(&mut sink, true);
        }
        let mut next = self.state.clone();
        let mut batch = Batch::new()?;
        let more = next.sync(folded, &mut batch)?;
        self.publish(next, batch.operations, more, &mut sink)
    }

    /// Freeze the fold before calling this. Finish children before the root,
    /// so root-based renderers do not become terminal while children stream.
    pub fn finish(
        &mut self,
        folded: &[MessagePart],
        status: Option<MessageStatus>,
        mut sink: impl FnMut(&Frame) -> Result<(), Error>,
    ) -> Result<Progress, Error> {
        if status == Some(MessageStatus::Streaming) {
            return Err(invalid("invalid_message_status"));
        }
        let phase = self
            .pending
            .as_ref()
            .map(|f| &f.next.header.phase)
            .unwrap_or(&self.state.header.phase);
        match phase {
            Phase::Writing => {}
            Phase::Finishing { status: old, .. } | Phase::Closed { status: old }
                if old != &status =>
            {
                return Err(invalid("finish_status_changed"));
            }
            _ => {}
        }
        if self.pending.is_some() {
            return self.flush(&mut sink, true);
        }
        if matches!(self.state.header.phase, Phase::Writing) {
            let progress = self.sync(folded, &mut sink)?;
            if progress.more || progress.operations > 0 {
                return Ok(Progress {
                    more: true,
                    ..progress
                });
            }
        } else {
            // No trailing additions or tool refreshes are legal after any
            // finalization frame. Check against the frozen fold without writes.
            let mut check = self.state.clone();
            let mut batch = Batch::new()?;
            if check.sync(folded, &mut batch)? || !batch.operations.is_empty() {
                return Err(invalid("writer_finishing"));
            }
        }
        if matches!(self.state.header.phase, Phase::Closed { .. }) {
            return Ok(Progress {
                operations: 0,
                more: false,
            });
        }
        let mut next = self.state.clone();
        let mut remaining = match next.header.phase {
            Phase::Finishing { remaining, .. } => remaining,
            _ => next.chunks.len(),
        };
        let mut batch = Batch::new()?;
        while remaining > 0 {
            let event = Event::MessageFinished {
                message_id: next.chunks[remaining - 1].id.clone(),
                status,
            };
            if !batch.emit(&mut next, event)? {
                break;
            }
            remaining -= 1;
        }
        next.header.phase = if remaining == 0 {
            Phase::Closed { status }
        } else {
            Phase::Finishing { status, remaining }
        };
        self.publish(next, batch.operations, remaining > 0, &mut sink)
    }

    fn publish(
        &mut self,
        mut next: State,
        operations: Vec<Operation>,
        more: bool,
        sink: &mut impl FnMut(&Frame) -> Result<(), Error>,
    ) -> Result<Progress, Error> {
        if operations.is_empty() && next.header.phase == self.state.header.phase {
            return Ok(Progress {
                operations: 0,
                more,
            });
        }
        next.header.revision = next
            .header
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("writer_exhausted"))?;
        next.header.chunks = next.chunks.len();
        next.header.slots = next.slots.len();
        let mut updates = Vec::new();
        for (i, chunk) in next.chunks.iter().enumerate() {
            if self.state.chunks.get(i) != Some(chunk) {
                updates.push(("chunk", i, serde_json::to_string(chunk)?));
            }
        }
        for (i, slot) in next.slots.iter().enumerate() {
            if self.state.slots.get(i) != Some(slot) {
                updates.push(("slot", i, serde_json::to_string(slot)?));
            }
        }
        self.pending = Some(Frame {
            root: self.root_id().into(),
            actor: next.header.config.actor.clone(),
            expected_revision: self.state.header.revision,
            header: serde_json::to_string(&next.header)?,
            updates,
            operations,
            next,
            more,
        });
        self.flush(sink, false)
    }
    fn flush(
        &mut self,
        sink: &mut impl FnMut(&Frame) -> Result<(), Error>,
        retry: bool,
    ) -> Result<Progress, Error> {
        let frame = self.pending.as_ref().expect("pending producer frame");
        sink(frame)?;
        let frame = self.pending.take().expect("accepted producer frame");
        let progress = Progress {
            operations: frame.operations.len(),
            more: frame.more || retry,
        };
        self.state = frame.next;
        Ok(progress)
    }

    pub(super) fn restore(header: &str, rows: Vec<(String, usize, String)>) -> Result<Self, Error> {
        let header: Header = serde_json::from_str(header)?;
        if header.version != 2 || !header.config.entry.parts.is_empty() {
            return Err(invalid("invalid_writer_checkpoint"));
        }
        let checked = Self::new(
            header.config.scope,
            header.config.actor.clone(),
            header.config.owner_epoch,
            header.config.run_id.clone(),
            &header.config.entry,
        )?;
        if checked.state.header.config != header.config {
            return Err(invalid("invalid_writer_checkpoint"));
        }
        let mut state = State {
            header,
            chunks: vec![],
            slots: vec![],
        };
        for (kind, ordinal, body) in rows {
            match kind.as_str() {
                "chunk" if ordinal == state.chunks.len() => {
                    state.chunks.push(serde_json::from_str(&body)?)
                }
                "slot" if ordinal == state.slots.len() => {
                    state.slots.push(serde_json::from_str(&body)?)
                }
                _ => return Err(invalid("invalid_writer_checkpoint")),
            }
        }
        if state.header.chunks != state.chunks.len()
            || state.header.slots != state.slots.len()
            || state.header.seen_slots < state.slots.len()
            || state.chunks.iter().enumerate().any(|(i, c)| {
                c.id != state.chunk_id(i)
                    || c.reserved > wire::MAX_MESSAGE_BYTES
                    || c.parts > wire::MAX_MESSAGE_PARTS
            })
            || state.slots.iter().any(|s| {
                !wire::valid_entity_id(&s.id)
                    || state
                        .chunks
                        .get(s.chunk)
                        .is_none_or(|c| s.index as usize >= c.parts)
                    || matches!(s.memo, Memo::Text { bytes, observed_bytes, tail_bytes, .. }
                        if bytes > observed_bytes || tail_bytes > bytes || tail_bytes > wire::MAX_MESSAGE_BYTES)
            })
            || matches!(state.header.phase, Phase::Finishing { remaining, .. } if remaining > state.chunks.len())
            || matches!(state.header.phase, Phase::Finishing { status: Some(MessageStatus::Streaming), .. } | Phase::Closed { status: Some(MessageStatus::Streaming) })
        {
            return Err(invalid("invalid_writer_checkpoint"));
        }
        Ok(Self {
            state,
            pending: None,
        })
    }
}

struct Batch {
    operations: Vec<Operation>,
    bytes: usize,
}
impl Batch {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            operations: vec![],
            bytes: serde_json::to_vec(&Request::Push {
                version: wire::VERSION,
                operations: vec![],
            })?
            .len(),
        })
    }
    fn emit(&mut self, state: &mut State, event: Event) -> Result<bool, Error> {
        let operation = state.operation(event)?;
        operation.validate().map_err(invalid)?;
        let bytes =
            serde_json::to_vec(&operation)?.len() + usize::from(!self.operations.is_empty());
        if self.operations.len() == wire::MAX_BATCH_OPS
            || self.bytes + bytes > wire::MAX_FRAME_BYTES
        {
            return Ok(false);
        }
        state.header.next_op = state
            .header
            .next_op
            .checked_add(1)
            .ok_or_else(|| invalid("writer_exhausted"))?;
        self.bytes += bytes;
        self.operations.push(operation);
        Ok(true)
    }
}

impl State {
    fn chunk_id(&self, index: usize) -> String {
        if index == 0 {
            self.header.config.entry.id.clone()
        } else {
            continuation_id(&self.header.config.entry.id, index)
        }
    }
    fn created_event(&self, index: usize) -> Result<Event, Error> {
        let c = &self.header.config;
        let parent = if index == 0 {
            c.entry.continuation_of.clone()
        } else {
            c.entry
                .continuation_of
                .clone()
                .or_else(|| Some(c.entry.id.clone()))
        };
        Ok(Event::MessageCreated {
            run_id: c.run_id.clone(),
            message_id: self.chunk_id(index),
            role: c.entry.role,
            device_id: c.entry.device_id.clone(),
            created_at: c
                .entry
                .created_at
                .try_into()
                .map_err(|_| invalid("invalid_timestamp"))?,
            continuation_of: parent,
        })
    }
    fn operation(&self, event: Event) -> Result<Operation, Error> {
        let mut digest = Sha256::new();
        digest.update(self.header.config.seed);
        digest.update(self.header.next_op.to_be_bytes());
        Ok(Operation {
            id: format!("{:x}", digest.finalize()),
            actor: self.header.config.actor.clone(),
            owner_epoch: self.header.config.owner_epoch,
            event,
        })
    }
    fn new_chunk(&mut self, batch: &mut Batch) -> Result<bool, Error> {
        let i = self.chunks.len();
        let event = self.created_event(i)?;
        let Event::MessageCreated {
            continuation_of, ..
        } = &event
        else {
            unreachable!()
        };
        let mut entry = self.header.config.entry.clone();
        entry.id = self.chunk_id(i);
        entry.continuation_of = continuation_of.clone();
        let reserved = serde_json::to_vec(&MessageState {
            created_seq: wire::MAX_SAFE_INTEGER,
            run_id: self.header.config.run_id.clone(),
            entry,
        })?
        .len();
        if !batch.emit(self, event)? {
            return Ok(false);
        }
        self.chunks.push(Chunk {
            id: self.chunk_id(i),
            reserved,
            parts: 0,
        });
        Ok(true)
    }
    fn place(
        &mut self,
        part: &MessagePart,
        reserve: usize,
        batch: &mut Batch,
    ) -> Result<Option<(usize, u32)>, Error> {
        let need_chunk = self.chunks.last().is_none_or(|c| {
            c.parts == wire::MAX_MESSAGE_PARTS
                || c.reserved + reserve + usize::from(c.parts > 0) > wire::MAX_MESSAGE_BYTES
                || self
                    .slots
                    .iter()
                    .any(|s| s.chunk == self.chunks.len() - 1 && s.id == part.id())
        });
        if need_chunk && !self.new_chunk(batch)? {
            return Ok(None);
        }
        let chunk = self.chunks.len() - 1;
        let c = &self.chunks[chunk];
        if c.reserved + reserve + usize::from(c.parts > 0) > wire::MAX_MESSAGE_BYTES {
            return Err(invalid("part_too_large"));
        }
        let index = c.parts as u32;
        if !batch.emit(
            self,
            Event::PartPut {
                message_id: c.id.clone(),
                index,
                part: part.clone(),
            },
        )? {
            return Ok(None);
        }
        let c = &mut self.chunks[chunk];
        c.reserved += reserve + usize::from(c.parts > 0);
        c.parts += 1;
        Ok(Some((chunk, index)))
    }
    fn sync(&mut self, folded: &[MessagePart], batch: &mut Batch) -> Result<bool, Error> {
        if folded.len() < self.slots.len() || folded.len() < self.header.seen_slots {
            return Err(invalid("fold_shrank"));
        }
        self.header.seen_slots = folded.len();
        for (i, source) in folded.iter().enumerate() {
            if !wire::valid_entity_id(source.id()) {
                return Err(invalid("invalid_id"));
            }
            if let Some(old) = self.slots.get(i) {
                if old.id != source.id() {
                    return Err(invalid("part_identity_mismatch"));
                }
            }
            match source {
                MessagePart::Text { id, text } => {
                    if i == self.slots.len() {
                        let part = MessagePart::Text {
                            id: id.clone(),
                            text: String::new(),
                        };
                        let Some((chunk, index)) =
                            self.place(&part, serde_json::to_vec(&part)?.len(), batch)?
                        else {
                            return Ok(true);
                        };
                        self.slots.push(Slot {
                            id: id.clone(),
                            chunk,
                            index,
                            memo: Memo::Text {
                                bytes: 0,
                                observed_bytes: 0,
                                observed_hash: hash(b""),
                                tail_bytes: 0,
                            },
                        });
                    }
                    let mut slot = self.slots[i].clone();
                    let Memo::Text {
                        mut bytes,
                        observed_bytes,
                        observed_hash,
                        mut tail_bytes,
                    } = slot.memo
                    else {
                        return Err(invalid("part_identity_mismatch"));
                    };
                    if text
                        .get(..observed_bytes)
                        .is_none_or(|prefix| hash(prefix.as_bytes()) != observed_hash)
                    {
                        return Err(invalid("text_rewritten"));
                    }
                    if bytes < text.len() && i + 1 < self.slots.len() {
                        return Err(invalid("text_not_trailing"));
                    }
                    while bytes < text.len() {
                        let capacity = wire::MAX_MESSAGE_BYTES - self.chunks[slot.chunk].reserved;
                        let (end, growth) = text_piece(text, bytes, capacity);
                        if end == bytes {
                            // A previous frame may have ended after creating
                            // the empty successor but before putting its part.
                            if slot.chunk + 1 == self.chunks.len() && !self.new_chunk(batch)? {
                                break;
                            }
                            let part = MessagePart::Text {
                                id: id.clone(),
                                text: String::new(),
                            };
                            let Some((chunk, index)) =
                                self.place(&part, serde_json::to_vec(&part)?.len(), batch)?
                            else {
                                break;
                            };
                            slot.chunk = chunk;
                            slot.index = index;
                            tail_bytes = 0;
                            self.slots[i] = slot.clone();
                            continue;
                        }
                        let event = Event::TextAppended {
                            message_id: self.chunks[slot.chunk].id.clone(),
                            part_id: id.clone(),
                            offset: tail_bytes as u64,
                            text: text[bytes..end].into(),
                        };
                        if !batch.emit(self, event)? {
                            break;
                        }
                        self.chunks[slot.chunk].reserved += growth;
                        tail_bytes += end - bytes;
                        bytes = end;
                    }
                    slot.memo = Memo::Text {
                        bytes,
                        observed_bytes: text.len(),
                        observed_hash: hash(text.as_bytes()),
                        tail_bytes,
                    };
                    self.slots[i] = slot;
                    if bytes < text.len() {
                        return Ok(true);
                    }
                }
                _ => {
                    guard_public_size(source)?;
                    let part = render_parts(std::slice::from_ref(source))
                        .pop()
                        .expect("one rendered part");
                    let memo = part_memo(&part)?;
                    if let Some(old) = self.slots.get(i) {
                        match (&old.memo, &memo) {
                            (
                                Memo::Part {
                                    kind: a,
                                    resolved: ar,
                                    questions: aq,
                                    ..
                                },
                                Memo::Part {
                                    kind: b,
                                    resolved: br,
                                    questions: bq,
                                    ..
                                },
                            ) => {
                                if a != b {
                                    return Err(invalid("part_identity_mismatch"));
                                }
                                if aq != bq {
                                    return Err(invalid("question_changed"));
                                }
                                if *ar && !*br {
                                    return Err(invalid("part_resolved"));
                                }
                            }
                            _ => return Err(invalid("part_identity_mismatch")),
                        }
                        if old.memo == memo {
                            continue;
                        }
                        let event = Event::PartPut {
                            message_id: self.chunks[old.chunk].id.clone(),
                            index: old.index,
                            part,
                        };
                        if !batch.emit(self, event)? {
                            return Ok(true);
                        }
                        self.slots[i].memo = memo;
                    } else {
                        // Tool/error payloads can grow later. Reserve the full
                        // operation budget, so resolution never needs to move a
                        // published part. Immutable questions need only their
                        // current bytes (resolved=true is shorter than false).
                        let reserve = if matches!(part, MessagePart::Input { .. }) {
                            serde_json::to_vec(&part)?.len()
                        } else {
                            wire::MAX_OPERATION_BYTES
                        };
                        let Some((chunk, index)) = self.place(&part, reserve, batch)? else {
                            return Ok(true);
                        };
                        self.slots.push(Slot {
                            id: part.id().into(),
                            chunk,
                            index,
                            memo,
                        });
                    }
                }
            }
        }
        Ok(false)
    }
}
fn hash(bytes: &[u8]) -> Hash {
    Sha256::digest(bytes).into()
}
// Bound the public material before render_parts clones it. Heavy private
// write/edit/MCP inputs are deliberately not counted: they will be stripped.
// Oversized public fields need an artifact policy, not silent truncation.
fn guard_public_size(part: &MessagePart) -> Result<(), Error> {
    use cypher_proto::ToolCall;
    let mut total = 0usize;
    let mut add = |s: &str| {
        if s.len() > 64 * 1024 || s.len() + 2 > wire::MAX_OPERATION_BYTES - total {
            return Err(invalid("part_requires_artifact"));
        }
        total += s.len() + 2;
        Ok(())
    };
    match part {
        MessagePart::Tool {
            call,
            output,
            progress,
            diff,
            output_ref,
            diff_ref,
            diff_stats,
            ..
        } => {
            for text in [output, progress, output_ref, diff_ref]
                .into_iter()
                .flatten()
            {
                add(text)?;
            }
            if let Some(diff) = diff {
                add(&diff.path)?;
                add(&diff.new_text)?;
                if let Some(old) = &diff.old_text {
                    add(old)?;
                }
            }
            if let Some(stats) = diff_stats {
                if stats.len() > 256 {
                    return Err(invalid("invalid_part"));
                }
                for stat in stats {
                    add(&stat.path)?;
                }
            }
            match call {
                ToolCall::Exec { command } => add(command)?,
                ToolCall::ReadFile { path }
                | ToolCall::WriteFile { path, .. }
                | ToolCall::EditFile { path, .. } => add(path)?,
                ToolCall::ApplyPatch { path } => {
                    if let Some(path) = path {
                        add(path)?;
                    }
                }
                ToolCall::Search { pattern, path } => {
                    add(pattern)?;
                    if let Some(path) = path {
                        add(path)?;
                    }
                }
                ToolCall::Glob { pattern } => add(pattern)?,
                ToolCall::WebFetch { url, .. } => add(url)?,
                ToolCall::WebSearch { query } => add(query)?,
                ToolCall::Todo { items } => {
                    if items.len() > 256 {
                        return Err(invalid("invalid_part"));
                    }
                    for item in items {
                        add(&item.text)?;
                    }
                }
                ToolCall::Mcp { server, tool, .. } => {
                    add(server)?;
                    add(tool)?;
                }
                ToolCall::Unknown { name, input } => {
                    add(name)?;
                    if name == "subagent" {
                        if let Some(agent) = input
                            .as_ref()
                            .and_then(|v| v.get("agent"))
                            .and_then(|v| v.as_str())
                        {
                            add(agent)?;
                        }
                    }
                }
            }
        }
        MessagePart::Input {
            request_id,
            questions,
            ..
        } => {
            add(request_id)?;
            if questions.len() > 256 {
                return Err(invalid("invalid_part"));
            }
            for q in questions {
                add(&q.id)?;
                add(&q.header)?;
                add(&q.question)?;
                if q.options.len() > 256 {
                    return Err(invalid("invalid_part"));
                }
                for option in &q.options {
                    add(option)?;
                }
            }
        }
        MessagePart::Error { message, .. } => add(message)?,
        MessagePart::Text { .. } => {}
    }
    Ok(())
}
fn part_memo(part: &MessagePart) -> Result<Memo, Error> {
    let (kind, resolved, questions) = match part {
        MessagePart::Tool { resolved, .. } => (PartKind::Tool, *resolved, None),
        MessagePart::Input {
            resolved,
            request_id,
            questions,
            ..
        } => (
            PartKind::Input,
            *resolved,
            Some(hash(&serde_json::to_vec(&(request_id, questions))?)),
        ),
        MessagePart::Error { .. } => (PartKind::Error, false, None),
        _ => return Err(invalid("not_structured_part")),
    };
    Ok(Memo::Part {
        kind,
        resolved,
        questions,
        hash: hash(&serde_json::to_vec(part)?),
    })
}
fn text_piece(text: &str, start: usize, capacity: usize) -> (usize, usize) {
    let mut end = start;
    let mut growth = 0;
    for c in text[start..].chars() {
        let encoded = match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            c if c < '\u{20}' => 6,
            _ => c.len_utf8(),
        };
        if end - start + c.len_utf8() > DELTA_BYTES || growth + encoded > capacity {
            break;
        }
        end += c.len_utf8();
        growth += encoded;
    }
    (end, growth)
}

#[cfg(test)]
mod tests;
