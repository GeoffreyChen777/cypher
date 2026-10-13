//! The mock harness's permission control demos (`[mock:permissions]`,
//! `[mock:permission-card]`) leave in the chat doc what the Pi harness and
//! Cypher's permission extension do: each prompt answered beside the call it
//! gated, and on each call how permission control treated it. The demo
//! sessions (`scripts/dev-demo.sh --permissions`) are built from these.

mod common;

use cypher_doc::{MessagePart, MessageRole, MessageStatus};
use cypher_harness::mock::{MockHarness, PERMISSION_CARD_MARKER, PERMISSIONS_MARKER};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, PermissionOutcome, PermissionPromptKind, PermissionScope,
    UserInputAnswer,
};

const CHAT: &str = "chat-permission-demo";

fn harness() -> MockHarness {
    MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: "Title".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            },
        ],
    }
}

fn reply(core: &cypher_engine::EngineCore) -> Option<Vec<MessagePart>> {
    common::entries(core, CHAT)
        .into_iter()
        .find(|entry| entry.role == MessageRole::Assistant)
        .filter(|entry| entry.status == Some(MessageStatus::Complete))
        .map(|entry| entry.parts)
}

/// `(part id, outcome, scope)` for every tool part.
fn outcomes(
    parts: &[MessagePart],
) -> Vec<(String, Option<PermissionOutcome>, Option<PermissionScope>)> {
    parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Tool { id, permission, .. } => Some((
                id.clone(),
                permission.as_ref().map(|p| p.outcome),
                permission.as_ref().and_then(|p| p.scope),
            )),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_story_settles_with_each_prompt_answered_beside_its_call() {
    let rig = common::rig(harness());
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            common::run_request(PERMISSIONS_MARKER),
            None,
        )
        .await
        .expect("dispatch");
    common::wait_for(|| reply(&rig.core).is_some(), "the story to settle").await;
    let parts = reply(&rig.core).unwrap();

    use PermissionOutcome::*;
    use PermissionScope::*;
    assert_eq!(
        outcomes(&parts),
        [
            ("perm-1".into(), Some(Blocked), None),
            ("perm-2".into(), Some(Outside), Some(Chat)),
            ("perm-3".into(), Some(Outside), Some(Rule)),
            ("perm-4".into(), Some(Allowed), Some(Chat)),
            ("perm-5".into(), Some(Blocked), None),
            ("perm-6".into(), Some(Blocked), None),
            ("perm-7".into(), Some(Denied), None),
            ("perm-8".into(), None, None),
        ]
    );
    let position = |wanted: &str| {
        parts
            .iter()
            .position(|part| matches!(part, MessagePart::Tool { id, .. } if id == wanted))
            .unwrap()
    };
    let mut prompts = Vec::new();
    for (ix, part) in parts.iter().enumerate() {
        let MessagePart::Input {
            questions,
            resolved,
            ..
        } = part
        else {
            continue;
        };
        assert!(*resolved, "every prompt in the story is answered");
        let permission = questions[0].permission.clone();
        if let Some(call) = permission.as_ref().and_then(|p| p.tool_call_id.as_deref()) {
            assert!(position(call) < ix, "the prompt follows the call it gates");
        }
        prompts.push(permission.map(|p| (p.kind, p.tool_call_id)));
    }
    assert_eq!(
        prompts,
        [
            Some((PermissionPromptKind::Escalate, Some("perm-2".into()))),
            Some((PermissionPromptKind::Access, Some("perm-4".into()))),
            Some((PermissionPromptKind::Escalate, Some("perm-7".into()))),
            Some((PermissionPromptKind::Tool, Some("perm-8".into()))),
            // An older Runtime's prompt and an ordinary question: unmarked.
            None,
            None,
        ]
    );
    rig.core.sessions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_card_waits_for_the_answer_and_finishes_by_it() {
    let rig = common::rig(harness());
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            common::run_request(PERMISSION_CARD_MARKER),
            None,
        )
        .await
        .expect("dispatch");
    let pending = || {
        common::entries(&rig.core, CHAT)
            .into_iter()
            .flat_map(|entry| entry.parts)
            .find_map(|part| match part {
                MessagePart::Input {
                    request_id,
                    questions,
                    resolved: false,
                    ..
                } => Some((request_id, questions)),
                _ => None,
            })
    };
    common::wait_for(|| pending().is_some(), "the live card").await;
    let (request_id, questions) = pending().unwrap();
    assert_eq!(
        questions[0].permission.as_ref().map(|p| p.kind),
        Some(PermissionPromptKind::Escalate)
    );
    assert!(
        rig.core
            .sessions
            .respond_input(
                CHAT,
                &request_id,
                vec![UserInputAnswer {
                    question_id: questions[0].id.clone(),
                    labels: vec!["Allow for this chat".into()],
                }],
            )
            .expect("respond")
    );
    common::wait_for(|| reply(&rig.core).is_some(), "the run to finish").await;
    assert_eq!(
        outcomes(&reply(&rig.core).unwrap()),
        [
            ("card-1".into(), Some(PermissionOutcome::Blocked), None),
            (
                "card-2".into(),
                Some(PermissionOutcome::Outside),
                Some(PermissionScope::Chat)
            ),
        ]
    );
    rig.core.sessions.shutdown().await;
}
