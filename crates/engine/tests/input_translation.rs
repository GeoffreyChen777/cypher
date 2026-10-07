//! Prompt translation (pi `cypher.translation.input.v1`): an
//! `AgentEvent::InputTranslation` stamps the translation onto the USER entry
//! whose text it replaced — the transcript keeps showing the prompt as typed
//! while quotes from it can map back to what the agent read. It is not run
//! activity: no assistant segment, no status change, and it lands even while
//! the session is parked (the extension translates a prompt before the turn it
//! opens has started).

mod common;

use cypher_doc::{MessagePart, MessageRole};
use cypher_engine::EngineCore;
use cypher_proto::{AgentEvent, DoneStatus, HarnessId, SessionStatus};

use common::{FeedRig, entries, run_request, status, wait_for};

const CHAT: &str = "chat-input-translation";
const PROMPT: &str = "解释一下这个函数";

fn assemble(main_prompt: &str) -> FeedRig {
    common::feed_rig(main_prompt, false)
}

fn prompt_agent_text(core: &EngineCore) -> Option<String> {
    entries(core, CHAT)
        .into_iter()
        .find(|e| e.role == MessageRole::User)
        .and_then(|e| match e.parts.into_iter().next() {
            Some(MessagePart::Text { agent_text, .. }) => agent_text,
            _ => None,
        })
}

#[tokio::test]
async fn a_prompt_translation_stamps_the_user_entry_even_while_parked() {
    let rig = assemble(PROMPT);
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            run_request(PROMPT),
            Some("m1".into()),
        )
        .await
        .expect("dispatch");
    rig.feed
        .send(AgentEvent::TextDelta {
            text: "It parses the config.".into(),
        })
        .unwrap();
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
    let before = entries(&rig.core, CHAT).len();

    // A translation for text no recent prompt holds is dropped.
    rig.feed
        .send(AgentEvent::InputTranslation {
            source: "something else".into(),
            text: "Something else".into(),
        })
        .unwrap();
    rig.feed
        .send(AgentEvent::InputTranslation {
            source: PROMPT.into(),
            text: "Explain this function".into(),
        })
        .unwrap();
    wait_for(
        || prompt_agent_text(&rig.core).is_some(),
        "the prompt's agent version",
    )
    .await;
    assert_eq!(
        prompt_agent_text(&rig.core).as_deref(),
        Some("Explain this function")
    );
    // The displayed prompt is untouched, no entry was added, and the parked
    // session stays parked.
    let after = entries(&rig.core, CHAT);
    assert_eq!(after.len(), before);
    assert!(matches!(
        &after[0].parts[0],
        MessagePart::Text { text, .. } if text == PROMPT
    ));
    assert_eq!(status(&rig.core, CHAT), Some(SessionStatus::Idle));

    rig.core.shutdown().await;
}
