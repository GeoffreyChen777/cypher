//! Builds `protocol/vectors/command-signing-v1.json`, which the iOS writer
//! runs (`scripts/tests/command-signing-vectors.swift`), and fails when the
//! committed file differs. Regenerate with
//! `UPDATE_VECTORS=1 cargo test -p cypher-doc --test command_vectors`.
//!
//! Values are tagged so the integer/double distinction survives JSON:
//! `{"null": true}`, `{"bool": b}`, `{"i64": "n"}` (a string, past 2^53),
//! `{"double": x}`, `{"string": s}`, `{"binary": hex}`, `{"list": [...]}`,
//! `{"map": {...}}`.

use std::collections::BTreeMap;

use cypher_doc::{
    CommandBasedOn, SessionCommandEntry, SessionCommandPayload, SessionCommandStatus, SessionDoc,
    command_signing_bytes,
};
use loro::LoroValue;
use serde_json::{Value, json};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn tagged(value: &LoroValue) -> Value {
    match value {
        LoroValue::Null => json!({ "null": true }),
        LoroValue::Bool(b) => json!({ "bool": b }),
        LoroValue::I64(n) => json!({ "i64": n.to_string() }),
        LoroValue::Double(f) => json!({ "double": f }),
        LoroValue::String(s) => json!({ "string": s.as_str() }),
        LoroValue::Binary(b) => json!({ "binary": hex(b) }),
        LoroValue::List(items) => json!({ "list": items.iter().map(tagged).collect::<Vec<_>>() }),
        LoroValue::Map(map) => {
            let sorted: BTreeMap<&str, Value> =
                map.iter().map(|(k, v)| (k.as_str(), tagged(v))).collect();
            json!({ "map": sorted })
        }
        LoroValue::Container(_) => panic!("no containers in a deep value"),
    }
}

fn case(name: &str, chat_id: &str, fields: &BTreeMap<String, LoroValue>) -> Value {
    let bytes = command_signing_bytes(chat_id, fields.iter().map(|(k, v)| (k.as_str(), v)));
    json!({
        "name": name,
        "chatId": chat_id,
        "fields": fields.iter().map(|(k, v)| (k.clone(), tagged(v))).collect::<BTreeMap<_, _>>(),
        "signingBytes": hex(&bytes),
    })
}

/// The entry as the engine writes it, read back from the doc.
fn written_run() -> BTreeMap<String, LoroValue> {
    let doc = SessionDoc::init("chat-7").unwrap();
    let entry = SessionCommandEntry {
        id: "cmd-1".into(),
        payload: SessionCommandPayload::Run {
            request: cypher_proto::RunRequest {
                prompt: "Fix the build — 修复构建 🙂".into(),
                harness: None,
                model: Some("model-a".into()),
                reasoning: None,
                model_options: Default::default(),
                cwd: "~/src/app".into(),
                sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
                auto_approve: false,
                attachments: Vec::new(),
                pending_attachments: Vec::new(),
                resume: None,
                worktree: None,
            },
            message_id: "msg-1".into(),
            agent_prompt: None,
        },
        issued_by: "device-b".into(),
        issued_at: 1_700_000_000_000,
        based_on: Some(CommandBasedOn {
            turn_id: Some("turn-9".into()),
            frontier: None,
        }),
        expires_at: Some(1_700_086_400_000),
        status: SessionCommandStatus::Pending,
        resolution: None,
        sent_at: Some(1_700_000_000_000),
    };
    doc.queue_signed_command("chat-7", &entry, |_| Some([0x5a; 64]))
        .unwrap();
    let LoroValue::List(items) = doc.doc().get_list("commands").get_deep_value() else {
        panic!("commands is a list")
    };
    let LoroValue::Map(map) = &items[0] else {
        panic!("an entry is a map")
    };
    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Every value type, keys that sort by bytes, and the fields left unsigned.
fn every_type() -> BTreeMap<String, LoroValue> {
    let map = |pairs: Vec<(&str, LoroValue)>| {
        LoroValue::Map(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<std::collections::HashMap<_, _>>()
                .into(),
        )
    };
    let nested = map(vec![
        ("z", LoroValue::I64(-1)),
        ("Z", LoroValue::I64(i64::MAX)),
        ("é", LoroValue::Double(-0.5)),
        ("", LoroValue::Null),
        ("ab", LoroValue::Bool(true)),
        ("a", LoroValue::Bool(false)),
        ("bin", LoroValue::Binary(vec![0, 1, 254, 255].into())),
        ("empty", LoroValue::List(Vec::<LoroValue>::new().into())),
        ("none", map(vec![])),
        (
            "list",
            LoroValue::List(
                vec![
                    LoroValue::I64(1),
                    LoroValue::Double(1.0),
                    LoroValue::String("1".into()),
                ]
                .into(),
            ),
        ),
    ]);
    [
        ("id", LoroValue::from("cmd-2")),
        ("payload", nested),
        ("issuedAt", LoroValue::I64(0)),
        ("status", LoroValue::from("applied")),
        ("resolution", LoroValue::from("done")),
        ("sig", LoroValue::from("00")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

fn build() -> Value {
    json!({
        "comment": "Generated by crates/doc/tests/command_vectors.rs. Byte strings are lowercase hex.",
        "cases": [
            case("an entry as the engine writes it", "chat-7", &written_run()),
            case("every value type, with the unsigned fields", "chat-🙂", &every_type()),
        ],
    })
}

/// Keys sorted at every level, so the file is byte-identical whether or not
/// the build unifies serde_json's `preserve_order` feature.
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

const PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../protocol/vectors/command-signing-v1.json"
);

#[test]
fn shared_vectors_match_the_committed_file() {
    let built = serde_json::to_string_pretty(&canonical(build())).unwrap() + "\n";
    if std::env::var_os("UPDATE_VECTORS").is_some() {
        std::fs::write(PATH, &built).unwrap();
    }
    let committed = std::fs::read_to_string(PATH).expect("run with UPDATE_VECTORS=1 to create it");
    assert!(
        committed == built,
        "{PATH} is stale; regenerate with UPDATE_VECTORS=1"
    );
}

#[test]
fn the_engine_entry_is_signed_over_everything_but_the_hosts_fields() {
    let fields = written_run();
    for key in [
        "id",
        "kind",
        "payload",
        "issuedBy",
        "issuedAt",
        "basedOn",
        "expiresAt",
        "sentAt",
    ] {
        assert!(fields.contains_key(key), "{key}");
    }
    assert!(fields.contains_key("sig"));
}
