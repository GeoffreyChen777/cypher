//! Sync v3 wire contract. Independent of Loro and transport implementation.
//! This module is experimental; normal chat2 clients do not negotiate it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u8 = 3;
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_BATCH_OPS: usize = 64;
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

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
        command: Command,
    },
    CommandAccepted {
        command_id: String,
        run_id: String,
    },
    RunStarted {
        run_id: String,
    },
    MessageCreated {
        run_id: String,
        message_id: String,
        role: Role,
    },
    TextAppended {
        message_id: String,
        offset: u64,
        text: String,
    },
    ToolStarted {
        run_id: String,
        tool_id: String,
        name: String,
    },
    ToolFinished {
        tool_id: String,
        failed: bool,
        summary: String,
    },
    InputRequested {
        run_id: String,
        request_id: String,
        prompt: String,
    },
    RunFinished {
        run_id: String,
        outcome: Outcome,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Command {
    Send { text: String },
    Steer { text: String },
    Interrupt {},
    RespondInput { request_id: String, answer: Value },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    User,
    Assistant,
}

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
    /// canonicalize input answers before persisting or comparing receipts.
    pub fn canonicalized(&self) -> std::borrow::Cow<'_, Self> {
        if let Event::CommandQueued {
            command: Command::RespondInput { .. },
            ..
        } = &self.event
        {
            let mut result = self.clone();
            if let Event::CommandQueued {
                command: Command::RespondInput { answer, .. },
                ..
            } = &mut result.event
            {
                canonical_json(answer);
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
                if let Command::RespondInput { request_id, answer } = command {
                    id(request_id)?;
                    validate_json(answer, 3)?;
                }
            }
            Event::CommandAccepted { command_id, run_id } => {
                id(command_id)?;
                id(run_id)?;
            }
            Event::RunStarted { run_id } | Event::RunFinished { run_id, .. } => id(run_id)?,
            Event::MessageCreated {
                run_id, message_id, ..
            } => {
                id(run_id)?;
                id(message_id)?;
            }
            Event::TextAppended {
                message_id, offset, ..
            } => {
                id(message_id)?;
                if *offset > MAX_SAFE_INTEGER {
                    return Err("invalid_offset");
                }
            }
            Event::ToolStarted {
                run_id, tool_id, ..
            } => {
                id(run_id)?;
                id(tool_id)?;
            }
            Event::ToolFinished { tool_id, .. } => id(tool_id)?,
            Event::InputRequested {
                run_id, request_id, ..
            } => {
                id(run_id)?;
                id(request_id)?;
            }
        }
        if serde_json::to_vec(self).map_err(|_| "invalid_json")?.len() > MAX_FRAME_BYTES / 2 {
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
    pub runs: BTreeMap<String, RunState>,
    pub messages: BTreeMap<String, MessageState>,
    pub tools: BTreeMap<String, ToolState>,
    pub inputs: BTreeMap<String, InputState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandState {
    pub command: Command,
    pub actor: String,
    pub run_id: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunState {
    pub outcome: Option<Outcome>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageState {
    pub run_id: String,
    pub role: Role,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolState {
    pub run_id: String,
    pub name: String,
    pub failed: Option<bool>,
    pub summary: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputState {
    pub run_id: String,
    pub prompt: String,
}

impl Projection {
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
    ) -> Result<(), &'static str> {
        op.validate()?;
        let op = op.canonicalized();
        if op.owner_epoch != owner_epoch {
            return Err("stale_owner_epoch");
        }
        if !matches!(op.event, Event::CommandQueued { .. }) && op.actor != owner {
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
                    },
                );
            }
            Event::CommandAccepted { command_id, run_id } => {
                let cmd = self.commands.get_mut(command_id).ok_or("unknown_command")?;
                if cmd.run_id.is_some() {
                    return Err("command_already_accepted");
                }
                cmd.run_id = Some(run_id.clone());
            }
            Event::RunStarted { run_id } => {
                if self.runs.contains_key(run_id) {
                    return Err("run_exists");
                }
                if !self
                    .commands
                    .values()
                    .any(|c| c.run_id.as_ref() == Some(run_id))
                {
                    return Err("run_not_accepted");
                }
                self.runs.insert(run_id.clone(), RunState::default());
            }
            Event::MessageCreated {
                run_id,
                message_id,
                role,
            } => {
                self.live(run_id)?;
                if self.messages.contains_key(message_id) {
                    return Err("message_exists");
                }
                self.messages.insert(
                    message_id.clone(),
                    MessageState {
                        run_id: run_id.clone(),
                        role: *role,
                        text: String::new(),
                    },
                );
            }
            Event::TextAppended {
                message_id,
                offset,
                text,
            } => {
                let m = self.messages.get(message_id).ok_or("unknown_message")?;
                self.live(&m.run_id)?;
                if m.text.len() as u64 != *offset {
                    return Err("text_offset_mismatch");
                }
                self.messages
                    .get_mut(message_id)
                    .unwrap()
                    .text
                    .push_str(text);
            }
            Event::ToolStarted {
                run_id,
                tool_id,
                name,
            } => {
                self.live(run_id)?;
                if self.tools.contains_key(tool_id) {
                    return Err("tool_exists");
                }
                self.tools.insert(
                    tool_id.clone(),
                    ToolState {
                        run_id: run_id.clone(),
                        name: name.clone(),
                        failed: None,
                        summary: None,
                    },
                );
            }
            Event::ToolFinished {
                tool_id,
                failed,
                summary,
            } => {
                let tool = self.tools.get(tool_id).ok_or("unknown_tool")?;
                self.live(&tool.run_id)?;
                if tool.failed.is_some() {
                    return Err("tool_finished");
                }
                let tool = self.tools.get_mut(tool_id).unwrap();
                tool.failed = Some(*failed);
                tool.summary = Some(summary.clone());
            }
            Event::InputRequested {
                run_id,
                request_id,
                prompt,
            } => {
                self.live(run_id)?;
                if self.inputs.contains_key(request_id) {
                    return Err("input_exists");
                }
                self.inputs.insert(
                    request_id.clone(),
                    InputState {
                        run_id: run_id.clone(),
                        prompt: prompt.clone(),
                    },
                );
            }
            Event::RunFinished { run_id, outcome } => {
                self.live(run_id)?;
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
    fn golden_events_reduce_and_reject_late_output() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
        let ops: Vec<Operation> = serde_json::from_value(fixture["operations"].clone()).unwrap();
        let mut p = Projection::default();
        for op in &ops {
            p.apply(op, "host", 1).unwrap();
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
                        offset: 7,
                        text: "!".into()
                    },
                },
                "host",
                1
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
