//! Mock harness prompt markers: `[mock:ask]` parks on a question until it is
//! answered, `[mock:hold]` keeps working until interrupted, and
//! `[mock:changes:N]` edits the checkout — only when the run may write.

mod common;

use std::path::Path;
use std::time::Duration;

use futures::StreamExt;

use common::{controls_answering, dones, request, run_to_end};
use cypher_harness::Harness;
use cypher_harness::mock::{ASK_MARKER, CHANGES_MARKER, HOLD_MARKER, HOLD_TOOL_ID, MockHarness};
use cypher_proto::{AgentEvent, DoneStatus, RunRequest, SandboxLevel};

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

/// Every file under `root`, relative path → content.
fn tree(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// The auto-titler sends a chat's first prompt, marker and all, through the
/// harness read-only in the chat's checkout. The changes demo must not edit
/// the checkout then: a second, concurrent copy of round 1's edits would land
/// in the next round's changes.
#[tokio::test]
async fn the_changes_demo_edits_only_when_the_run_may_write() {
    let demo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/demo/tasks-app");
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&demo, dir.path());
    let before = tree(dir.path());
    let round_one = |sandbox| RunRequest {
        cwd: dir.path().to_string_lossy().into_owned(),
        sandbox,
        ..request(&format!("Add due dates {CHANGES_MARKER}1]"), None)
    };
    let tool_calls = |events: &[AgentEvent]| {
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCall { .. }))
            .count()
    };

    let (controls, _steer, _token) = controls_answering("unused");
    let read_only = run_to_end(&harness(), round_one(SandboxLevel::ReadOnly), controls).await;
    assert_eq!(tool_calls(&read_only), 0);
    assert_eq!(dones(&read_only), [(DoneStatus::Completed, None)]);
    assert!(
        tree(dir.path()) == before,
        "a read-only run edited the checkout"
    );

    let (controls, _steer, _token) = controls_answering("unused");
    let writing = run_to_end(
        &harness(),
        round_one(SandboxLevel::WorkspaceWrite),
        controls,
    )
    .await;
    assert!(tool_calls(&writing) > 0);
    let after = tree(dir.path());
    assert!(after.contains_key("tasks/dates.py"));
    assert_ne!(after["README.md"], before["README.md"]);
}
