//! Mock harness prompt markers: `[mock:ask]` parks on a question until it is
//! answered, `[mock:hold]` keeps working until interrupted.

mod common;

use std::time::Duration;

use futures::StreamExt;

use common::{controls_answering, dones, request, run_to_end};
use cypher_harness::Harness;
use cypher_harness::mock::{ASK_MARKER, HOLD_MARKER, HOLD_TOOL_ID, MockHarness};
use cypher_proto::{AgentEvent, DoneStatus};

fn harness() -> MockHarness {
    MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: "first".into(),
            },
            AgentEvent::TextDelta {
                text: "second".into(),
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

fn texts(events: &[AgentEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn ask_marker_replays_the_script_once_answered() {
    let (controls, _steer, _token) = controls_answering("Patch it in place");
    let events = run_to_end(
        &harness(),
        request(&format!("fix it {ASK_MARKER}"), None),
        controls,
    )
    .await;
    assert_eq!(texts(&events), ["first", "second"]);
    assert_eq!(dones(&events), [(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn hold_marker_works_until_interrupted() {
    let (controls, _steer, token) = controls_answering("unused");
    let mut stream = harness()
        .run(request(&format!("fix it {HOLD_MARKER}"), None), controls)
        .await
        .expect("run starts");
    let first = stream.next().await.expect("opening event").expect("event");
    assert_eq!(texts(&[first]), ["first"]);
    // An open command keeps the engine's quiesce watchdog disarmed.
    let command = stream.next().await.expect("command").expect("event");
    assert!(matches!(command, AgentEvent::ToolCall { ref id, .. } if id == HOLD_TOOL_ID));
    // Still working: nothing more arrives until Stop.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err()
    );
    token.cancel();
    let rest: Vec<AgentEvent> = stream.map(|e| e.expect("event")).collect().await;
    assert_eq!(dones(&rest), [(DoneStatus::Interrupted, None)]);
}

#[tokio::test]
async fn unmarked_prompts_replay_the_whole_script() {
    let (controls, _steer, _token) = controls_answering("unused");
    let events = run_to_end(&harness(), request("fix it", None), controls).await;
    assert_eq!(texts(&events), ["first", "second"]);
    assert_eq!(dones(&events), [(DoneStatus::Completed, None)]);
}
