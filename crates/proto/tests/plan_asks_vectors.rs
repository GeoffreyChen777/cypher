//! The plan-question rule (`crates/proto/src/view/plan_asks.rs`) against
//! `protocol/vectors/plan-asks-v1.json`, which the iPhone twin and the
//! Worker run too. Every case of docs/design/plan-asks.md §2 is one entry.

use std::collections::HashMap;

use chrono::{DateTime, TimeZone, Utc};
use cypher_proto::view::{ChatLink, ChatTree, Indicator, effective_indicator, plan_question};
use cypher_proto::{BotRecords, BotWork, NeedsYou, Session};
use serde_json::{Value, json};

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../../protocol/vectors/plan-asks-v1.json")).unwrap()
}

fn ms(value: &Value) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(value.as_i64().unwrap()).unwrap()
}

/// A registry session row (times in epoch ms) as the typed `Session`.
fn session(row: &Value) -> Session {
    let mut row = row.clone();
    for key in ["updatedAt", "startedAt"] {
        if let Some(at) = row.get(key).filter(|v| !v.is_null()) {
            row[key] = json!(ms(at).to_rfc3339());
        }
    }
    serde_json::from_value(row).unwrap()
}

fn indicator(i: Indicator) -> &'static str {
    match i {
        Indicator::None => "none",
        Indicator::Working => "working",
        Indicator::AwaitingInput => "awaitingInput",
        Indicator::Errored => "errored",
    }
}

#[test]
fn plan_asks_vectors() {
    let vectors = vectors();
    let now = ms(&vectors["now"]);
    let cases = vectors["cases"].as_array().unwrap();
    assert!(cases.len() >= 15);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let online: Vec<&str> = case["online"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d.as_str().unwrap())
            .collect();
        let chats = case["chats"].as_array().unwrap();
        let tree = ChatTree::new(chats.iter().map(|c| ChatLink {
            id: c["id"].as_str().unwrap(),
            parent_chat_id: c["child"]["parentChatId"].as_str(),
            archived: c["archived"].as_bool().unwrap_or(false),
        }));
        let sessions: HashMap<String, Session> = case["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(session)
            .map(|s| (s.chat_id.clone(), s))
            .collect();
        let work: BotWork = serde_json::from_value(case["work"].clone()).unwrap();
        let question = |chat: &str| {
            plan_question(
                &tree,
                chat,
                |id| sessions.get(id),
                |device| online.contains(&device),
                now,
            )
        };
        let records = BotRecords {
            work: vec![work.clone()],
            ..Default::default()
        };
        let needs: Vec<Value> = records
            .needs_you("b", |chat| question(chat).is_some())
            .into_iter()
            .map(|item| {
                let NeedsYou::Asking { chat_id, work_id } = item else {
                    panic!("{name}: only questions here, got {item:?}");
                };
                let q = question(&chat_id).unwrap();
                let request_id = sessions
                    .get(&q.chat_id)
                    .and_then(|s| s.asking.as_ref())
                    .map(|a| a.request_id.clone());
                json!({
                    "chatId": chat_id,
                    "workId": work_id,
                    "askingChatId": q.chat_id,
                    "requestId": request_id,
                    "offline": q.offline,
                })
            })
            .collect();
        let expect = &case["expect"];
        assert_eq!(Value::Array(needs), expect["needsYou"], "{name}: needsYou");
        let steps =
            serde_json::to_value(work.settled_steps(|chat| question(chat).is_some())).unwrap();
        assert_eq!(steps, expect["steps"], "{name}: steps");
        for (chat, want) in expect["indicators"].as_object().unwrap() {
            let got = indicator(effective_indicator(sessions.get(chat), now));
            assert_eq!(got, want.as_str().unwrap(), "{name}: indicator of {chat}");
        }
        // The serialized row reads back the same (the wire shape is the
        // typed one): `asking` survives a round trip.
        for s in sessions.values() {
            let back: Session = serde_json::from_value(serde_json::to_value(s).unwrap()).unwrap();
            assert_eq!(&back, s, "{name}: round trip");
        }
    }
}
