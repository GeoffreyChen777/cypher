//! The mock harness's translation demo (`[mock:translation]`), run through
//! the engine: the prompt keeps the words the user typed with the agent's
//! version stamped on, and every statement the agent made between tool calls
//! settles translated in place, as does the answer — though some step frames
//! land after later text has streamed. The demo session
//! (`scripts/dev-demo.sh --translation`) is built from this run.

mod common;

use cypher_doc::{MessagePart, MessageRole, MessageStatus};
use cypher_harness::mock::{
    MockHarness, TRANSLATION_MARKER, TRANSLATION_PROMPT, TRANSLATION_REPLACE_MARKER,
    TranslationMode, translation_settled,
};
use cypher_proto::{AgentEvent, DoneStatus, HarnessId, SessionStatus};

fn harness() -> MockHarness {
    MockHarness {
        script: vec![AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        }],
    }
}

/// `(text, agent_text)` of each text part in `role`'s entries.
fn texts(
    core: &cypher_engine::EngineCore,
    chat: &str,
    role: MessageRole,
) -> Vec<(String, Option<String>)> {
    common::entries(core, chat)
        .into_iter()
        .filter(|e| e.role == role)
        .flat_map(|e| e.parts)
        .filter_map(|part| match part {
            MessagePart::Text {
                text, agent_text, ..
            } => Some((text, agent_text)),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_statement_settles_translated_in_place() {
    let rig = common::rig(harness());
    for (chat, marker, mode) in [
        (
            "chat-translation-append",
            TRANSLATION_MARKER,
            TranslationMode::Append,
        ),
        (
            "chat-translation-replace",
            TRANSLATION_REPLACE_MARKER,
            TranslationMode::Replace,
        ),
    ] {
        rig.core
            .sessions
            .dispatch_augmented(
                chat,
                HarnessId::Mock,
                common::run_request(TRANSLATION_PROMPT),
                Some(format!("{TRANSLATION_PROMPT} {marker}")),
                None,
            )
            .await
            .expect("dispatch");
        common::wait_for(
            || {
                common::entries(&rig.core, chat).iter().any(|e| {
                    e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)
                }) && common::status(&rig.core, chat) == Some(SessionStatus::Idle)
            },
            "the run to settle",
        )
        .await;

        let prompt = texts(&rig.core, chat, MessageRole::User);
        assert_eq!(prompt.len(), 1);
        assert_eq!(prompt[0].0, TRANSLATION_PROMPT, "shown as typed");
        assert!(
            prompt[0]
                .1
                .as_deref()
                .is_some_and(|t| t.starts_with("The sync client")),
            "{prompt:?}"
        );

        let expected: Vec<_> = translation_settled(mode)
            .into_iter()
            .map(|(original, rendered)| (rendered, Some(original)))
            .collect();
        assert_eq!(texts(&rig.core, chat, MessageRole::Assistant), expected);
    }
    rig.core.sessions.shutdown().await;
}
