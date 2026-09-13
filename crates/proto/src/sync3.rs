//! Sync v3 wire contract. Independent of Loro and transport implementation.
//! This module is experimental; normal chat2 clients do not negotiate it.

use std::collections::BTreeMap;

use crate::{MessagePart, MessageStatus, SessionMessageEntry};
use crate::{SessionCommandEntry, SessionCommandPayload, SessionCommandStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;
mod command;
mod transcript;

pub const VERSION: u8 = 3;
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_OPERATION_BYTES: usize = MAX_FRAME_BYTES / 2;
pub const MAX_BATCH_OPS: usize = 64;
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
pub const MAX_MESSAGE_PARTS: usize = 256;
fn required_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operation {
    pub id: String,
    pub actor: String,
    pub owner_epoch: u64,
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Event {
    CommandQueued {
        command_id: String,
        #[serde(deserialize_with = "command::deserialize")]
        command: Command,
    },
    CommandClaimAttempted {
        command_id: String,
        run_id: String,
    },
    CommandResolved {
        command_id: String,
        status: SessionCommandStatus,
        #[serde(deserialize_with = "required_option")]
        resolution: Option<String>,
    },
    CommandCancelAttempted {
        command_id: String,
    },
    ExecutionStarted {
        execution_id: String,
        command_id: String,
    },
    ExecutionFinished {
        execution_id: String,
    },
    RunStarted {
        run_id: String,
    },
    /// Output produced autonomously by an already-admitted persistent process.
    /// No command is created and this event never authorizes another dispatch.
    RunObserved {
        run_id: String,
        execution_id: String,
    },
    MessageCreated {
        #[serde(deserialize_with = "required_option")]
        run_id: Option<String>,
        message_id: String,
        role: Role,
        device_id: String,
        created_at: u64,
        #[serde(deserialize_with = "required_option")]
        continuation_of: Option<String>,
    },
    PartPut {
        message_id: String,
        index: u32,
        #[serde(deserialize_with = "transcript::deserialize_part")]
        part: MessagePart,
    },
    TextAppended {
        message_id: String,
        part_id: String,
        offset: u64,
        text: String,
    },
    MessageFinished {
        message_id: String,
        #[serde(deserialize_with = "required_option")]
        status: Option<MessageStatus>,
    },
    AttachmentSealed {
        upload_id: String,
        path: String,
        file_name: String,
    },
    RunFinished {
        run_id: String,
        outcome: Outcome,
    },
}

pub type Command = SessionCommandEntry;

pub type Role = crate::MessageRole;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Row {
    pub seq: u64,
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Request {
    Hello {
        version: u8,
        actor: String,
        epoch: u64,
        after: u64,
    },
    Push {
        version: u8,
        operations: Vec<Operation>,
    },
    Pull {
        version: u8,
        epoch: u64,
        after: u64,
        through: u64,
    },
    Probe {
        version: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
    pub id: String,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Reply {
    State {
        version: u8,
        epoch: u64,
        owner: String,
        owner_epoch: u64,
        head: u64,
    },
    Ack {
        version: u8,
        epoch: u64,
        receipts: Vec<Receipt>,
    },
    Page {
        version: u8,
        epoch: u64,
        through: u64,
        next: u64,
        rows: Vec<Row>,
        done: bool,
    },
    Error {
        version: u8,
        code: String,
    },
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn valid_entity_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:#~".contains(&b))
}
fn entity_id(value: &str) -> Result<(), &'static str> {
    if valid_entity_id(value) {
        Ok(())
    } else {
        Err("invalid_id")
    }
}

fn id(id: &str) -> Result<(), &'static str> {
    if valid_id(id) {
        Ok(())
    } else {
        Err("invalid_id")
    }
}
fn validate_json(value: &Value, depth: usize) -> Result<(), &'static str> {
    if depth > 32 {
        return Err("json_too_deep");
    }
    match value {
        Value::Array(values) => {
            for v in values {
                validate_json(v, depth + 1)?;
            }
        }
        Value::Object(values) => {
            for v in values.values() {
                validate_json(v, depth + 1)?;
            }
        }
        Value::Number(number) => {
            let n = number.as_f64().ok_or("invalid_number")?;
            if !n.is_finite() || (n.fract() == 0.0 && n.abs() > MAX_SAFE_INTEGER as f64) {
                return Err("invalid_number");
            }
        }
        _ => {}
    }
    Ok(())
}

impl Operation {
    /// JSON has one number domain. JS serializes 1.0/-0.0 as 1/0;
    /// canonicalize model options before persisting or comparing receipts.
    pub fn canonicalized(&self) -> std::borrow::Cow<'_, Self> {
        if let Event::CommandQueued {
            command:
                Command {
                    payload: SessionCommandPayload::Run { .. },
                    ..
                },
            ..
        } = &self.event
        {
            let mut result = self.clone();
            if let Event::CommandQueued {
                command:
                    Command {
                        payload: SessionCommandPayload::Run { request, .. },
                        ..
                    },
                ..
            } = &mut result.event
            {
                request.model_options.values_mut().for_each(canonical_json);
            }
            std::borrow::Cow::Owned(result)
        } else {
            std::borrow::Cow::Borrowed(self)
        }
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        id(&self.id)?;
        id(&self.actor)?;
        if self.owner_epoch == 0 || self.owner_epoch > MAX_SAFE_INTEGER {
            return Err("invalid_epoch");
        }
        match &self.event {
            Event::CommandQueued {
                command_id,
                command,
            } => {
                id(command_id)?;
                command::validate(command)?;
                if command.id != *command_id || command.issued_by != self.actor {
                    return Err("command_identity_mismatch");
                }
            }
            Event::CommandClaimAttempted { command_id, run_id } => {
                id(command_id)?;
                id(run_id)?;
            }
            Event::CommandResolved {
                command_id, status, ..
            } => {
                id(command_id)?;
                if !matches!(
                    status,
                    SessionCommandStatus::Applied
                        | SessionCommandStatus::Rejected
                        | SessionCommandStatus::Expired
                        | SessionCommandStatus::Superseded
                ) {
                    return Err("invalid_command_resolution");
                }
            }
            Event::CommandCancelAttempted { command_id } => id(command_id)?,
            Event::ExecutionStarted {
                execution_id,
                command_id,
            } => {
                id(execution_id)?;
                id(command_id)?;
            }
            Event::ExecutionFinished { execution_id } => id(execution_id)?,
            Event::RunStarted { run_id } | Event::RunFinished { run_id, .. } => id(run_id)?,
            Event::RunObserved {
                run_id,
                execution_id,
            } => {
                id(run_id)?;
                id(execution_id)?;
            }
            Event::MessageCreated {
                run_id,
                message_id,
                device_id,
                created_at,
                continuation_of,
                ..
            } => {
                if let Some(run) = run_id {
                    id(run)?;
                }
                entity_id(message_id)?;
                id(device_id)?;
                if *created_at > MAX_SAFE_INTEGER {
                    return Err("invalid_timestamp");
                }
                if let Some(parent) = continuation_of {
                    entity_id(parent)?;
                    if parent == message_id {
                        return Err("invalid_continuation");
                    }
                }
            }
            Event::PartPut {
                message_id,
                index,
                part,
            } => {
                entity_id(message_id)?;
                if *index as usize >= MAX_MESSAGE_PARTS {
                    return Err("too_many_parts");
                }
                transcript::validate_part(part)?;
            }
            Event::TextAppended {
                message_id,
                part_id,
                offset,
                ..
            } => {
                entity_id(message_id)?;
                entity_id(part_id)?;
                if *offset > MAX_SAFE_INTEGER {
                    return Err("invalid_offset");
                }
            }
            Event::MessageFinished { message_id, status } => {
                entity_id(message_id)?;
                if *status == Some(MessageStatus::Streaming) {
                    return Err("invalid_message_status");
                }
            }
            Event::AttachmentSealed {
                upload_id,
                path,
                file_name,
            } => {
                id(upload_id)?;
                if path.is_empty() || file_name.is_empty() {
                    return Err("invalid_attachment");
                }
            }
        }
        if serde_json::to_vec(self).map_err(|_| "invalid_json")?.len() > MAX_OPERATION_BYTES {
            return Err("operation_too_large");
        }
        Ok(())
    }
}

fn canonical_json(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(canonical_json),
        Value::Object(values) => values.values_mut().for_each(canonical_json),
        Value::Number(number) if number.is_f64() => {
            if let Some(n) = number.as_f64()
                && n.is_finite()
                && n.fract() == 0.0
                && n.abs() <= MAX_SAFE_INTEGER as f64
            {
                *value = Value::from(n as i64);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Projection {
    pub commands: BTreeMap<String, CommandState>,
    #[serde(default)]
    pub executions: BTreeMap<String, ExecutionState>,
    pub runs: BTreeMap<String, RunState>,
    pub messages: BTreeMap<String, MessageState>,
    pub attachments: BTreeMap<String, AttachmentState>,
}

/// Durable, non-expiring occupancy of a persistent harness instance.
/// Semantic run completion never closes this record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionState {
    pub command_id: String,
    pub actor: String,
    pub owner_epoch: u64,
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandState {
    pub command: Command,
    pub actor: String,
    pub run_id: Option<String>,
    /// Only this committed attempt won dispatch authority. Matching a run ID
    /// alone is insufficient: two attempts may target the same running turn.
    #[serde(deserialize_with = "required_option")]
    pub accepted_op_id: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunState {
    pub outcome: Option<Outcome>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageState {
    /// Immutable position of messageCreated in the committed room log.
    pub created_seq: u64,
    pub run_id: Option<String>,
    pub entry: SessionMessageEntry,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentState {
    pub path: String,
    pub file_name: String,
}

impl Projection {
    fn writable_message(&self, id: &str) -> Result<&MessageState, &'static str> {
        let message = self.messages.get(id).ok_or("unknown_message")?;
        if let Some(run) = &message.run_id {
            self.live(run)?;
        }
        if message.entry.status != Some(MessageStatus::Streaming) {
            return Err("message_finished");
        }
        Ok(message)
    }
    fn put_message(&mut self, id: &str, message: MessageState) -> Result<(), &'static str> {
        if serde_json::to_vec(&message)
            .map_err(|_| "invalid_message")?
            .len()
            > MAX_MESSAGE_BYTES
        {
            return Err("message_too_large");
        }
        self.messages.insert(id.into(), message);
        Ok(())
    }
    fn live(&self, run: &str) -> Result<(), &'static str> {
        match self.runs.get(run) {
            Some(RunState { outcome: None }) => Ok(()),
            _ => Err("run_not_live"),
        }
    }

    /// Caller supplies the authoritative owner fence. Apply only committed,
    /// ordered events; optimistic outbox state is a separate overlay.
    pub fn apply(
        &mut self,
        op: &Operation,
        owner: &str,
        owner_epoch: u64,
        seq: u64,
    ) -> Result<(), &'static str> {
        op.validate()?;
        if seq == 0 || seq > MAX_SAFE_INTEGER {
            return Err("invalid_sequence");
        }
        let op = op.canonicalized();
        if op.owner_epoch != owner_epoch {
            return Err("stale_owner_epoch");
        }
        if !matches!(
            op.event,
            Event::CommandQueued { .. } | Event::CommandCancelAttempted { .. }
        ) && op.actor != owner
        {
            return Err("not_owner");
        }
        match &op.event {
            Event::CommandQueued {
                command_id,
                command,
            } => {
                if self.commands.contains_key(command_id) {
                    return Err("command_exists");
                }
                self.commands.insert(
                    command_id.clone(),
                    CommandState {
                        command: command.clone(),
                        actor: op.actor.clone(),
                        run_id: None,
                        accepted_op_id: None,
                    },
                );
            }
            Event::CommandClaimAttempted { command_id, run_id } => {
                let cmd = self.commands.get_mut(command_id).ok_or("unknown_command")?;
                if cmd.command.status == SessionCommandStatus::Pending && cmd.run_id.is_none() {
                    cmd.run_id = Some(run_id.clone());
                    cmd.accepted_op_id = Some(op.id.clone());
                }
            }
            Event::CommandResolved {
                command_id,
                status,
                resolution,
            } => {
                let cmd = self.commands.get_mut(command_id).ok_or("unknown_command")?;
                if cmd.command.status != SessionCommandStatus::Pending {
                    return Err("command_resolved");
                }
                if *status == SessionCommandStatus::Applied && cmd.run_id.is_none() {
                    return Err("command_not_accepted");
                }
                cmd.command.status = *status;
                cmd.command.resolution = resolution.clone();
            }
            Event::CommandCancelAttempted { command_id } => {
                let cmd = self.commands.get_mut(command_id).ok_or("unknown_command")?;
                if cmd.actor != op.actor {
                    return Err("not_command_author");
                }
                if cmd.run_id.is_none() && cmd.command.status == SessionCommandStatus::Pending {
                    cmd.command.status = SessionCommandStatus::Cancelled;
                }
            }
            Event::RunStarted { run_id } => {
                if self.runs.contains_key(run_id) {
                    return Err("run_exists");
                }
                if !self.commands.values().any(|c| {
                    c.run_id.as_ref() == Some(run_id)
                        && matches!(
                            c.command.status,
                            SessionCommandStatus::Pending | SessionCommandStatus::Applied
                        )
                }) {
                    return Err("run_not_accepted");
                }
                self.runs.insert(run_id.clone(), RunState::default());
            }
            Event::RunObserved {
                run_id,
                execution_id,
            } => {
                if self.runs.contains_key(run_id) {
                    return Err("run_exists");
                }
                let execution = self
                    .executions
                    .get(execution_id)
                    .ok_or("unknown_execution")?;
                if execution.closed {
                    return Err("execution_closed");
                }
                if execution.actor != op.actor || execution.owner_epoch != op.owner_epoch {
                    return Err("execution_owner_mismatch");
                }
                self.runs.insert(run_id.clone(), RunState::default());
            }
            Event::ExecutionStarted {
                execution_id,
                command_id,
            } => {
                if self.executions.contains_key(execution_id) {
                    return Err("execution_exists");
                }
                let command = self.commands.get(command_id).ok_or("unknown_command")?;
                if command.accepted_op_id.is_none()
                    || command.command.status != SessionCommandStatus::Pending
                {
                    return Err("command_not_accepted");
                }
                if !matches!(
                    command.command.payload,
                    crate::SessionCommandPayload::Run { .. }
                        | crate::SessionCommandPayload::Steer { .. }
                ) {
                    return Err("invalid_execution_command");
                }
                if self
                    .executions
                    .values()
                    .any(|e| e.command_id == *command_id)
                {
                    return Err("command_has_execution");
                }
                if self.executions.values().any(|e| !e.closed) {
                    return Err("execution_busy");
                }
                self.executions.insert(
                    execution_id.clone(),
                    ExecutionState {
                        command_id: command_id.clone(),
                        actor: op.actor.clone(),
                        owner_epoch: op.owner_epoch,
                        closed: false,
                    },
                );
            }
            Event::ExecutionFinished { execution_id } => {
                let execution = self
                    .executions
                    .get(execution_id)
                    .ok_or("unknown_execution")?;
                if execution.closed {
                    return Err("execution_closed");
                }
                if execution.actor != op.actor || execution.owner_epoch != op.owner_epoch {
                    return Err("execution_owner_mismatch");
                }
                if self.runs.values().any(|r| r.outcome.is_none())
                    || self.commands.values().any(|c| {
                        c.accepted_op_id.is_some()
                            && (c.command.status == SessionCommandStatus::Pending
                                || (c.command.status == SessionCommandStatus::Applied
                                    && c.run_id.as_ref().is_some_and(|id| {
                                        self.runs.get(id).is_none_or(|r| r.outcome.is_none())
                                    })))
                    })
                {
                    return Err("execution_unresolved");
                }
                self.executions
                    .get_mut(execution_id)
                    .ok_or("unknown_execution")?
                    .closed = true;
            }
            Event::MessageCreated {
                run_id,
                message_id,
                role,
                device_id,
                created_at,
                continuation_of,
            } => {
                if let Some(run) = run_id {
                    self.live(run)?;
                }
                if self.messages.contains_key(message_id) {
                    return Err("message_exists");
                }
                self.messages.insert(
                    message_id.clone(),
                    MessageState {
                        created_seq: seq,
                        run_id: run_id.clone(),
                        entry: SessionMessageEntry {
                            id: message_id.clone(),
                            role: *role,
                            parts: vec![],
                            device_id: device_id.clone(),
                            created_at: *created_at as i64,
                            status: Some(MessageStatus::Streaming),
                            continuation_of: continuation_of.clone(),
                        },
                    },
                );
            }
            Event::PartPut {
                message_id,
                index,
                part,
            } => {
                let mut message = self.writable_message(message_id)?.clone();
                let index = *index as usize;
                if index > message.entry.parts.len() {
                    return Err("part_gap");
                }
                if let Some(old) = message.entry.parts.get(index) {
                    transcript::validate_replacement(old, part)?;
                    message.entry.parts[index] = part.clone();
                } else {
                    if message.entry.parts.iter().any(|p| p.id() == part.id()) {
                        return Err("part_exists");
                    }
                    message.entry.parts.push(part.clone());
                }
                self.put_message(message_id, message)?;
            }
            Event::TextAppended {
                message_id,
                part_id,
                offset,
                text,
            } => {
                let mut m = self.writable_message(message_id)?.clone();
                let part = m
                    .entry
                    .parts
                    .iter_mut()
                    .find(|p| p.id() == part_id)
                    .ok_or("unknown_part")?;
                let MessagePart::Text { text: current, .. } = part else {
                    return Err("not_text");
                };
                if current.len() as u64 != *offset {
                    return Err("text_offset_mismatch");
                }
                current.push_str(text);
                self.put_message(message_id, m)?;
            }
            Event::MessageFinished { message_id, status } => {
                self.writable_message(message_id)?;
                self.messages.get_mut(message_id).unwrap().entry.status = *status;
            }
            Event::AttachmentSealed {
                upload_id,
                path,
                file_name,
            } => {
                let value = AttachmentState {
                    path: path.clone(),
                    file_name: file_name.clone(),
                };
                if let Some(old) = self.attachments.get(upload_id)
                    && old != &value
                {
                    return Err("attachment_conflict");
                }
                self.attachments.insert(upload_id.clone(), value);
            }
            Event::RunFinished { run_id, outcome } => {
                self.live(run_id)?;
                if self.messages.values().any(|m| {
                    m.run_id.as_ref() == Some(run_id)
                        && m.entry.status == Some(MessageStatus::Streaming)
                }) {
                    return Err("unfinished_messages");
                }
                self.runs.get_mut(run_id).unwrap().outcome = Some(*outcome);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_deltas_can_exceed_one_frame_but_never_the_message_budget() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
        let mut projection = Projection::default();
        for (i, value) in fixture["operations"]
            .as_array()
            .unwrap()
            .iter()
            .take(5)
            .enumerate()
        {
            projection
                .apply(
                    &serde_json::from_value(value.clone()).unwrap(),
                    "host",
                    1,
                    i as u64 + 1,
                )
                .unwrap();
        }
        for i in 0..5 {
            let before = projection.clone();
            let op = Operation {
                id: format!("budget-{i}"),
                actor: "host".into(),
                owner_epoch: 1,
                event: Event::TextAppended {
                    message_id: "message".into(),
                    part_id: "text".into(),
                    offset: 6 + i * 60 * 1024,
                    text: "x".repeat(60 * 1024),
                },
            };
            let result = projection.apply(&op, "host", 1, i + 6);
            if i < 4 {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err(), "message_too_large");
                assert_eq!(projection, before);
            }
        }
    }
    #[test]
    fn shared_render_part_shapes_are_lossless_and_private_inputs_are_rejected() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/part-validation.json"))
                .unwrap();
        for (group, valid) in [("valid", true), ("invalid", false)] {
            for part in fixture[group].as_array().unwrap() {
                let raw = serde_json::json!({"id":"part-op","actor":"host","ownerEpoch":1,
                    "event":{"type":"partPut","messageId":"message","index":0,"part":part}});
                let result = serde_json::from_value::<Operation>(raw.clone());
                if valid {
                    let op = result.unwrap();
                    op.validate().unwrap();
                    assert_eq!(serde_json::to_value(op).unwrap(), raw);
                } else {
                    assert!(result.map_or(true, |op| op.validate().is_err()), "{part}");
                }
            }
        }
    }
    #[test]
    fn shared_complete_command_validation() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
        let cases: Value = serde_json::from_str(include_str!(
            "../../../fixtures/sync3/command-validation.json"
        ))
        .unwrap();
        for payload in cases["payloads"].as_array().unwrap() {
            let mut op = fixture["operations"][0].clone();
            op["event"]["command"]["payload"] = payload.clone();
            serde_json::from_value::<Operation>(op)
                .unwrap()
                .validate()
                .unwrap();
        }
        for edit in cases["invalid"].as_array().unwrap() {
            let mut op = fixture["operations"][0].clone();
            let path: Vec<&str> = edit["path"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap())
                .collect();
            let (key, parent) = path.split_last().unwrap();
            let command = &mut op["event"]["command"];
            let parent = if parent.is_empty() {
                command
            } else {
                command
                    .pointer_mut(&format!("/{}", parent.join("/")))
                    .unwrap()
            };
            if edit["remove"] == true {
                parent.as_object_mut().unwrap().remove(*key);
            } else {
                parent[*key] = edit["value"].clone();
            }
            assert!(
                serde_json::from_value::<Operation>(op).map_or(true, |o| o.validate().is_err()),
                "{edit}"
            );
        }
        let mut interrupt = fixture["operations"][0].clone();
        interrupt["event"]["command"]["payload"] = serde_json::json!({"kind":"interrupt"});
        interrupt["event"]["command"]["basedOn"] = Value::Null;
        assert!(serde_json::from_value::<Operation>(interrupt).is_err());
    }

    #[test]
    fn shared_command_lifecycle_is_fenced_and_failures_are_atomic() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
        let initial: Vec<Operation> =
            serde_json::from_value(fixture["operations"].clone()).unwrap();
        let cases: Vec<Value> = serde_json::from_str(include_str!(
            "../../../fixtures/sync3/command-lifecycle.json"
        ))
        .unwrap();
        for case in cases {
            let mut projection = Projection::default();
            let mut seq = 1;
            for op in initial
                .iter()
                .take(case["initialPrefix"].as_u64().unwrap_or(1) as usize)
            {
                projection.apply(op, "host", 1, seq).unwrap();
                seq += 1;
            }
            for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                let op: Operation = serde_json::from_value(serde_json::json!({
                    "id":format!("step-{i}"),"actor":step["actor"],
                    "ownerEpoch":step.get("ownerEpoch").unwrap_or(&Value::from(1)),
                    "event":step["event"]
                }))
                .unwrap();
                let before = projection.clone();
                let result = projection.apply(&op, "host", 1, seq);
                if let Some(error) = step["error"].as_str() {
                    assert_eq!(result, Err(error), "{case}");
                    assert_eq!(projection, before);
                } else {
                    result.unwrap();
                    seq += 1;
                }
            }
            assert_eq!(
                serde_json::to_value(projection.commands["command"].command.status).unwrap(),
                case["status"]
            );
            if let Some(expected) = case.get("acceptedOpId") {
                assert_eq!(
                    serde_json::to_value(&projection.commands["command"].accepted_op_id).unwrap(),
                    *expected,
                    "{case}"
                );
            }
        }
    }

    #[test]
    fn golden_events_reduce_and_reject_late_output() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
        let ops: Vec<Operation> = serde_json::from_value(fixture["operations"].clone()).unwrap();
        let system: Operation =
            serde_json::from_str(include_str!("../../../fixtures/sync3/system-message.json"))
                .unwrap();
        let mut system_projection = Projection::default();
        for (i, op) in ops[..3].iter().enumerate() {
            system_projection
                .apply(op, "host", 1, i as u64 + 1)
                .unwrap();
        }
        system_projection.apply(&system, "host", 1, 4).unwrap();
        assert_eq!(
            system_projection.messages["system-message#c1"].entry.role,
            Role::System
        );
        let mut p = Projection::default();
        for (i, op) in ops.iter().enumerate() {
            p.apply(op, "host", 1, i as u64 + 1).unwrap();
        }
        assert_eq!(serde_json::to_value(&p).unwrap(), fixture["projection"]);
        assert_eq!(
            p.apply(
                &Operation {
                    id: "late".into(),
                    actor: "host".into(),
                    owner_epoch: 1,
                    event: Event::TextAppended {
                        message_id: "message".into(),
                        part_id: "text".into(),
                        offset: 7,
                        text: "!".into()
                    },
                },
                "host",
                1,
                ops.len() as u64 + 1
            ),
            Err("run_not_live")
        );
    }
    #[test]
    fn rejects_shared_invalid_vectors() {
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("../../../fixtures/sync3/invalid.json")).unwrap();
        for value in fixtures {
            let rejected = match serde_json::from_value::<Operation>(value.clone()) {
                Ok(op) => op.validate().is_err(),
                Err(_) => true,
            };
            assert!(rejected, "accepted invalid operation: {value}");
        }
    }
}
