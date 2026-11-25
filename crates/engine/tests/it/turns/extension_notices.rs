//! Extension notices (`AgentEvent::Notice` — a provider bridge's rate-limit
//! warning): the chat's latest one lands on its SESSION row, transient run
//! state like throughput. Never a transcript entry, never reply text, never
//! the run's status; the same notice again is dropped, a new one replaces it.

use crate::support;

use cypher_doc::{MessagePart, MessageRole};
use cypher_engine::EngineCore;
use cypher_proto::{AgentEvent, DoneStatus, HarnessId, NoticeLevel, SessionNotice, SessionStatus};

use support::{FeedRig, entries, run_request, status, wait_for};

const CHAT: &str = "chat-extension-notices";
const PROMPT: &str = "look at the build";

fn assemble() -> FeedRig {
    support::feed_rig(PROMPT, false)
}

fn notice(core: &EngineCore) -> Option<SessionNotice> {
    core.sessions.session_status(CHAT).and_then(|s| s.notice)
}

fn warning(text: &str) -> AgentEvent {
    AgentEvent::Notice {
        level: NoticeLevel::Warning,
        text: text.into(),
    }
}

fn reply_text(core: &EngineCore) -> String {
    entries(core, CHAT)
        .into_iter()
        .filter(|e| e.role == MessageRole::Assistant)
        .flat_map(|e| e.parts)
        .filter_map(|p| match p {
            MessagePart::Text { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_notice_is_session_state_not_reply_text_and_dedupes() {
    let rig = assemble();
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request(PROMPT), None)
        .await
        .expect("dispatch");
    rig.feed
        .send(AgentEvent::TextDelta {
            text: "Looking.".into(),
        })
        .unwrap();
    rig.feed
        .send(warning("Claude rate limit warning: 58% used (seven_day)"))
        .unwrap();
    wait_for(
        || notice(&rig.core).is_some(),
        "the notice lands on the row",
    )
    .await;
    let first = notice(&rig.core).unwrap();
    assert_eq!(first.level, NoticeLevel::Warning);
    assert_eq!(
        first.text,
        "Claude rate limit warning: 58% used (seven_day)"
    );

    // The same notice again: dropped (its arrival time stays put).
    rig.feed
        .send(warning("Claude rate limit warning: 58% used (seven_day)"))
        .unwrap();
    rig.feed
        .send(AgentEvent::TextDelta {
            text: " Done.".into(),
        })
        .unwrap();
    wait_for(
        || reply_text(&rig.core) == "Looking. Done.",
        "the reply streams on",
    )
    .await;
    assert_eq!(
        notice(&rig.core).unwrap().at,
        first.at,
        "a repeat is dropped"
    );

    // A new one replaces it.
    rig.feed
        .send(warning("Claude rate limit warning: 60% used (seven_day)"))
        .unwrap();
    wait_for(
        || notice(&rig.core).is_some_and(|n| n.text.contains("60%")),
        "a newer notice replaces it",
    )
    .await;
    rig.feed
        .send(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        })
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    // Never reply text, and it outlives the turn while it shows.
    assert_eq!(reply_text(&rig.core), "Looking. Done.");
    assert!(
        !entries(&rig.core, CHAT)
            .iter()
            .any(|e| e.parts.iter().any(
                |p| matches!(p, MessagePart::Text { text, .. } if text.contains("rate limit"))
            )),
        "no transcript entry carries the notice"
    );
    assert!(notice(&rig.core).is_some_and(|n| n.text.contains("60%")));
}
