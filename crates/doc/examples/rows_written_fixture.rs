//! Deterministic P0 workload: real SegmentWriter/Loro deltas, no network.
use cypher_doc::{
    MessageStatus, SegmentWriter, SessionDoc, fold_event_into_parts, materialize_tail,
};
use cypher_proto::{AgentEvent, ToolCall};
use loro::{ExportMode, LoroDoc, VersionVector};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tools = std::env::args().any(|arg| arg == "tools");
    let raw = LoroDoc::new();
    raw.set_peer_id(1)?;
    raw.get_map("meta").insert("chatId", "p0-stream")?;
    raw.get_map("meta")
        .insert("schemaVersion", cypher_doc::SESSION_SCHEMA_VERSION as i64)?;
    raw.commit();
    let doc = SessionDoc::from_doc(raw);
    let mut vv = VersionVector::default();
    let mut steps = Vec::new();
    let mut capture = |at: u64, checkpoint: bool| -> Result<(), Box<dyn std::error::Error>> {
        let update = doc.doc().export(ExportMode::updates(&vv))?;
        vv = doc.doc().oplog_vv();
        steps.push(json!({"at":at,"update":update,
            "tail": materialize_tail(&doc, at as i64, 64)?,
            "checkpoint": if checkpoint {Some(doc.export_snapshot()?)} else {None}}));
        Ok(())
    };
    capture(0, false)?;
    let mut writer = SegmentWriter::begin(&doc, "assistant", "host", 1)?;
    capture(1, false)?;
    let mut text = String::new();
    let mut folded = Vec::new();
    for index in 1..=240 {
        let delta = "流式 text — abcdefghijklmnopqrstuvwxyz.\n";
        text.push_str(delta);
        fold_event_into_parts(&mut folded, &AgentEvent::TextDelta { text: delta.into() });
        writer.sync(&folded)?;
        capture(index * 120, index == 200)?;
        if tools && index == 80 {
            for event in [
                AgentEvent::ToolCall {
                    id: "tool".into(),
                    call: ToolCall::Exec {
                        command: "echo fixture".into(),
                    },
                },
                AgentEvent::ToolResult {
                    id: "tool".into(),
                    is_error: false,
                    output: None,
                    diff: None,
                },
            ] {
                fold_event_into_parts(&mut folded, &event);
                writer.sync(&folded)?;
                capture(index * 120, false)?;
            }
        }
    }
    writer.finish(&folded, MessageStatus::Complete)?;
    capture(28_801, true)?;
    // Prove exported updates are genuine cumulative document history.
    let reader = SessionDoc::from_doc(LoroDoc::new());
    for step in &steps {
        let bytes: Vec<u8> = serde_json::from_value(step["update"].clone())?;
        reader.doc().import(&bytes)?;
    }
    assert_eq!(reader.read_entries()?, doc.read_entries()?);
    println!(
        "{}",
        json!({"kind":if tools {"text-tools-240x120ms"} else {"text-240x120ms"}, "textBytes":text.len(),
        "steps":steps, "finalEntries":doc.read_entries()?})
    );
    Ok(())
}
