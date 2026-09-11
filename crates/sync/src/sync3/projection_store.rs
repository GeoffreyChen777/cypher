//! Sparse transactional projection: a token touches its message and run, not
//! a serialized copy of the entire conversation.
use super::{Error, invalid};
use cypher_proto::sync3::{Event, Operation, Projection};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

const KINDS: [&str; 5] = ["commands", "runs", "messages", "tools", "inputs"];

fn load(db: &Connection, projection: &mut Projection, kind: &str, id: &str) -> Result<(), Error> {
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM sync3_entities WHERE kind=? AND id=?",
            params![kind, id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(body) = body else {
        return Ok(());
    };
    match kind {
        "commands" => {
            projection
                .commands
                .insert(id.into(), serde_json::from_str(&body)?);
        }
        "runs" => {
            projection
                .runs
                .insert(id.into(), serde_json::from_str(&body)?);
        }
        "messages" => {
            projection
                .messages
                .insert(id.into(), serde_json::from_str(&body)?);
        }
        "tools" => {
            projection
                .tools
                .insert(id.into(), serde_json::from_str(&body)?);
        }
        "inputs" => {
            projection
                .inputs
                .insert(id.into(), serde_json::from_str(&body)?);
        }
        _ => return Err(invalid("invalid_entity_kind")),
    }
    Ok(())
}

pub(super) fn read(db: &Connection) -> Result<Projection, Error> {
    let mut value =
        serde_json::json!({"commands":{},"runs":{},"messages":{},"tools":{},"inputs":{}});
    let mut query = db.prepare("SELECT kind,id,body FROM sync3_entities ORDER BY kind,id")?;
    for row in query.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        let (kind, id, body) = row?;
        if !KINDS.contains(&kind.as_str()) {
            return Err(invalid("invalid_entity_kind"));
        }
        value[&kind][&id] = serde_json::from_str(&body)?;
    }
    Ok(serde_json::from_value(value)?)
}

/// Called only inside Journal's page transaction. Unchanged dependencies are
/// read for reducer validation but never rewritten.
pub(super) fn apply(db: &Connection, operation: &Operation, seq: u64) -> Result<(), Error> {
    operation.validate().map_err(invalid)?;
    let mut projection = Projection::default();
    match &operation.event {
        Event::CommandQueued { command_id, .. } | Event::CommandAccepted { command_id, .. } => {
            load(db, &mut projection, "commands", command_id)?;
        }
        Event::RunStarted { run_id } => {
            load(db, &mut projection, "runs", run_id)?;
            let command: Option<String> = db
                .query_row(
                    "SELECT id FROM sync3_entities WHERE kind='commands' AND run_id=? LIMIT 1",
                    [run_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(command) = command {
                load(db, &mut projection, "commands", &command)?;
            }
        }
        Event::RunFinished { run_id, .. } => load(db, &mut projection, "runs", run_id)?,
        Event::MessageCreated {
            run_id, message_id, ..
        } => {
            load(db, &mut projection, "runs", run_id)?;
            load(db, &mut projection, "messages", message_id)?;
        }
        Event::TextAppended { message_id, .. } => {
            load(db, &mut projection, "messages", message_id)?;
            if let Some(message) = projection.messages.get(message_id) {
                let run = message.run_id.clone();
                load(db, &mut projection, "runs", &run)?;
            }
        }
        Event::ToolStarted {
            run_id, tool_id, ..
        } => {
            load(db, &mut projection, "runs", run_id)?;
            load(db, &mut projection, "tools", tool_id)?;
        }
        Event::ToolFinished { tool_id, .. } => {
            load(db, &mut projection, "tools", tool_id)?;
            if let Some(tool) = projection.tools.get(tool_id) {
                let run = tool.run_id.clone();
                load(db, &mut projection, "runs", &run)?;
            }
        }
        Event::InputRequested {
            run_id, request_id, ..
        } => {
            load(db, &mut projection, "runs", run_id)?;
            load(db, &mut projection, "inputs", request_id)?;
        }
    }
    let before = serde_json::to_value(&projection)?;
    // Historical writes were fenced by the server at commit time.
    projection
        .apply(operation, &operation.actor, operation.owner_epoch)
        .map_err(invalid)?;
    let after = serde_json::to_value(&projection)?;
    for kind in KINDS {
        for (id, record) in after[kind]
            .as_object()
            .ok_or_else(|| invalid("invalid_projection"))?
        {
            if before[kind].get(id) == Some(record) {
                continue;
            }
            let body = serde_json::to_string(record)?;
            if body.len() > cypher_proto::sync3::MAX_FRAME_BYTES * 4 {
                return Err(invalid("entity_too_large"));
            }
            let run_id = record.get("runId").and_then(Value::as_str);
            db.execute(
                "INSERT INTO sync3_entities(kind,id,body,run_id,seq) VALUES(?,?,?,?,?)
                 ON CONFLICT(kind,id) DO UPDATE SET body=excluded.body,run_id=excluded.run_id,seq=excluded.seq",
                params![kind, id, body, run_id, seq])?;
        }
    }
    Ok(())
}
