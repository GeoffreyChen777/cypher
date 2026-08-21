//! PiHarness integration tests against the fake pi in
//! `tests/fixtures/fake-pi.sh` (no real `pi` binary involved).

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use cypher_harness::pi::PiHarness;
use cypher_harness::{
    CancellationToken, Harness, HarnessError, RunControls, RunHostContext, SteerMessage,
};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, ReasoningLevel, RunRequest, SandboxLevel, SteeringMode,
    ToolCall, UserInputAnswer,
};

const FIXTURE_SESSION_FILE: &str = "/tmp/pi-test/session.jsonl";

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-pi.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    path
}

fn harness() -> PiHarness {
    // The fake pi ignores --session-dir; a shared scratch path is fine
    // (PiHarness::run create_dir_all's it idempotently).
    PiHarness::new(std::env::temp_dir().join("cypher-pi-test-sessions"))
        .with_executable(fixture_path())
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        // provider/id convention — the harness splits on the first `/`.
        model: Some("anthropic/claude-sonnet-4-20250514".into()),
        reasoning: Some(ReasoningLevel::Medium),
        model_options: serde_json::Map::new(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        resume: None,
    }
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec!["tokio".into()],
                })
                .collect();
            let _ = tx.send(answers);
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
        host: RunHostContext::default(),
    };
    (controls, steer_tx, token)
}

async fn run_to_end(
    harness: &PiHarness,
    req: RunRequest,
    controls: RunControls,
) -> Vec<AgentEvent> {
    let stream = harness.run(req, controls).await.expect("run starts");
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time")
}

fn dones(events: &[AgentEvent]) -> Vec<(DoneStatus, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Done { status, error, .. } => Some((*status, error.clone())),
            _ => None,
        })
        .collect()
}

/// The engine bridge (`CYPHER_ENGINE_WS_URL`) must be injected into every pi
/// child when `with_engine_bridge` is set — and absent otherwise. The spawn
/// seam builds the exact command a run would spawn, so no child is needed.
#[test]
fn engine_bridge_url_is_injected_into_children() {
    let url = std::ffi::OsStr::new("ws://127.0.0.1:4242");
    let key = std::ffi::OsStr::new("CYPHER_ENGINE_WS_URL");

    let bridged = PiHarness::new(std::env::temp_dir().join("cypher-pi-bridge-sessions"))
        .with_executable(fixture_path())
        .with_engine_bridge(Some("ws://127.0.0.1:4242".into()));
    let cmd = bridged
        .spawn_command(None, &RunHostContext::default(), None)
        .expect("command builds");
    let envs: std::collections::HashMap<_, _> = cmd.as_std().get_envs().collect();
    assert_eq!(
        envs.get(key),
        Some(&Some(url)),
        "bridged children receive CYPHER_ENGINE_WS_URL"
    );

    let plain = PiHarness::new(std::env::temp_dir().join("cypher-pi-bridge-sessions"))
        .with_executable(fixture_path());
    let cmd = plain
        .spawn_command(None, &RunHostContext::default(), None)
        .expect("command builds");
    assert!(
        cmd.as_std().get_envs().all(|(k, _)| k != key),
        "unbridged children must not receive the engine WS URL"
    );
}

#[tokio::test]
async fn models_and_commands_are_discovered_from_the_probe() {
    let harness = harness();
    let models = harness.models().await.expect("model discovery");
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["anthropic/claude-sonnet-4-20250514", "openai/gpt-4o-mini"],
        "{models:?}"
    );
    assert_eq!(models[0].label, "Claude Sonnet 4");
    assert_eq!(
        models[0].description.as_deref(),
        Some("anthropic · 200k context")
    );
    assert_eq!(
        models[0].reasoning_levels,
        vec![
            ReasoningLevel::Minimal,
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::XHigh,
            ReasoningLevel::Max,
        ]
    );
    // Non-reasoning models get no ladder.
    assert!(models[1].reasoning_levels.is_empty());
    // Cached: a second call returns the same list without respawning.
    let again = harness.models().await.expect("cached models");
    assert_eq!(again, models);

    let commands = harness.commands().await.expect("command discovery");
    // The probe's extension `compact` wins (dedup — no synthesized twin); the
    // synthesized built-ins land at the tail, so only `export-html` is added.
    assert_eq!(commands.len(), 3, "{commands:?}");
    assert_eq!(commands[0].name, "compact");
    assert_eq!(commands[1].name, "skill:brave-search");
    assert_eq!(commands[2].name, "export-html");
    assert_eq!(
        commands[2].description,
        "Export the session to an HTML file (pi built-in)"
    );
    assert_eq!(commands[2].input_hint.as_deref(), Some("output path"));
    let again = harness.commands().await.expect("cached commands");
    assert_eq!(again, commands);
}

#[tokio::test]
async fn compact_builtin_is_intercepted_and_dones_immediately() {
    // The harness synthesizes /compact (pi's built-in TUI command has an RPC
    // equivalent) and dispatches it; the run ends right after the response —
    // no agent stream, so even the default 2s no-activity grace never applies.
    let harness = harness();
    let (controls, _steer, _token) = controls();
    let started = std::time::Instant::now();
    let events = run_to_end(&harness, request("/compact focus on api"), controls).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "intercepted built-in must not wait out the no-activity grace"
    );

    // The fixture refused unless customInstructions reached it, so its
    // success response proves the relay; the delta + Done carry the compact
    // summary text with the token fields from the response data.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Context compacted: 150000 → 32000 tokens".into()
    }));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
    let done = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Done {
                result, session_id, ..
            } => Some((result.clone(), session_id.clone())),
            _ => None,
        })
        .expect("done present");
    assert_eq!(
        done,
        (
            Some("Context compacted: 150000 → 32000 tokens".into()),
            Some(FIXTURE_SESSION_FILE.into()),
        )
    );
}

#[tokio::test]
async fn export_html_builtin_is_intercepted_and_dones_completed() {
    // /export-html is synthesized and dispatched over RPC; the fixture refused
    // unless outputPath reached it, so the success response proves the relay.
    let harness = harness();
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness, request("/export-html /tmp/x.html"), controls).await;

    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Exported to /tmp/x.html".into()
    }));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
    let done = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Done { result, .. } => Some(result.clone()),
            _ => None,
        })
        .expect("done present");
    assert_eq!(done, Some("Exported to /tmp/x.html".into()));
}

#[tokio::test]
async fn happy_path_maps_deltas_tools_errors_and_settles_completed() {
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), request("scenario:happy"), controls).await;

    // SessionStarted from get_state's sessionFile, with the current model name.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStarted { harness, session_id, cwd, model, .. }
                if *harness == HarnessId::Pi
                    && session_id == FIXTURE_SESSION_FILE
                    && cwd == "/tmp"
                    && model == "Claude Sonnet 4"
        )),
        "{events:?}"
    );

    // Thinking + text deltas (each delta its own event).
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "thinking".into()
    }));
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Hello".into()
    }));
    assert!(events.contains(&AgentEvent::TextDelta {
        text: " world".into()
    }));
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Done.".into()
    }));

    // Tool call + capped result.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "t1".into(),
        call: ToolCall::Exec {
            command: "cargo test -p cypher-harness".into()
        },
    }));
    let output = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolResult {
                id,
                is_error: false,
                output: Some(output),
                ..
            } if id == "t1" => Some(output.clone()),
            _ => None,
        })
        .expect("tool output present");
    assert!(
        output.starts_with("   Compiling cypher-harness"),
        "{output:?}"
    );

    // Two assistant messages → two journal boundaries; the toolResult
    // messages must NOT emit one.
    let completed = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::AssistantMessageCompleted { .. }))
        .count();
    assert_eq!(completed, 2, "{events:?}");

    // Extension error surfaces; the fire-and-forget notify never reaches the
    // input bridge.
    assert!(events.contains(&AgentEvent::Error {
        message: "boom".into()
    }));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::InputRequested { .. })),
        "{events:?}"
    );
    // No steer → no boundary.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Steered { .. }))
    );

    // Exactly one Completed Done with the last assistant text and the same
    // session id.
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
    let done = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Done {
                result, session_id, ..
            } => Some((result.clone(), session_id.clone())),
            _ => None,
        })
        .expect("done present");
    assert_eq!(
        done,
        (Some("Done.".into()), Some(FIXTURE_SESSION_FILE.into()))
    );
}

#[tokio::test]
async fn tool_progress_is_throttled_and_stops_after_end() {
    // Three tool_execution_update events: the first two 100ms apart (< the
    // 500ms throttle), the third 600ms after the first (>= throttle). Only
    // TWO ToolProgress forward (first + the post-throttle one); a late update
    // after tool_execution_end never forwards.
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), request("scenario:progress"), controls).await;

    let progress: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolProgress { id, output } if id == "t1" => Some(output.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        progress,
        vec!["step one", "step three"],
        "first always forwards; the <500ms tick is throttled; the post-throttle one lands"
    );
    // The tool resolved, and no ToolProgress rides after its ToolResult.
    let end_ix = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolResult { id, .. } if id == "t1"))
        .expect("tool result present");
    assert!(
        events[end_ix + 1..]
            .iter()
            .all(|e| !matches!(e, AgentEvent::ToolProgress { .. })),
        "no ToolProgress after tool_execution_end: {events:?}"
    );
    // The subagent call folds as a typed Unknown (ToolCall), and the turn
    // still completes cleanly.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "t1".into(),
        call: ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(serde_json::json!({ "agent": "planner", "task": "plan the live card" })),
        },
    }));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn no_agent_activity_terminates_with_done_completed_carrying_notify_text() {
    // An extension command whose handler only notifies: pi relays the notify
    // requests, accepts the prompt, then goes silent forever. The harness
    // must end the run itself (Done Completed, result = notify text) instead
    // of sitting "Working" indefinitely, and reap the child.
    let harness = harness().with_no_activity_grace(Duration::from_millis(200));
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness, request("scenario:noagent"), controls).await;

    // info notify → TextDelta (escaped multi-line text passes through as-is),
    // error notify → Error event.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Available subagents:\n- actor: does things\n- coder: writes code".into()
    }));
    assert!(events.contains(&AgentEvent::Error {
        message: "command failed: no token".into()
    }));
    // The run terminates itself: Done Completed whose result is the notify
    // text, and the stream closes cleanly (no Errored).
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
    let done = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Done {
                result, session_id, ..
            } => Some((result.clone(), session_id.clone())),
            _ => None,
        })
        .expect("done present");
    assert_eq!(
        done,
        (
            Some("Available subagents:\n- actor: does things\n- coder: writes code".into()),
            Some(FIXTURE_SESSION_FILE.into()),
        )
    );
}

#[tokio::test]
async fn resume_switches_to_the_injected_session_path() {
    let mut req = request("scenario:resumed");
    let resume_path = "/tmp/pi-test/resumed-session.jsonl";
    req.resume = Some(resume_path.into());

    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), req, controls).await;

    // The fixture adopts the switched-to path as its sessionFile, so the
    // resumed session id must BE the resume path — proving switch_session
    // carried it and the session stayed on it for the whole run.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::SessionStarted { session_id, .. }
            if session_id == "/tmp/pi-test/resumed-session.jsonl"
    )));
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "back again".into()
    }));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
    // Done keeps the resumed session id.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Done { session_id, .. }
            if session_id.as_deref() == Some("/tmp/pi-test/resumed-session.jsonl")
    )));
}

#[tokio::test]
async fn failed_resume_is_a_loud_errored_done_naming_the_path() {
    let mut req = request("scenario:resumed");
    req.resume = Some("/tmp/pi-test/missing-session.jsonl".into());
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), req, controls).await;

    // Never a silent fresh session: Done Errored whose message names the
    // failure AND the path.
    let dones = dones(&events);
    assert_eq!(dones.len(), 1, "{events:?}");
    assert_eq!(dones[0].0, DoneStatus::Errored);
    let error = dones[0].1.as_deref().expect("resume error message");
    assert!(error.contains("resume failed"), "{error}");
    assert!(
        error.contains("/tmp/pi-test/missing-session.jsonl"),
        "resume error must name the path: {error}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionStarted { .. })),
        "a failed resume must never start a session: {events:?}"
    );
}

#[tokio::test]
async fn steering_sends_a_steer_command_and_emits_the_boundary_before_content() {
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:steer"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "first") {
                steer
                    .send(SteerMessage {
                        prompt: "redirect please".into(),
                        message_id: None,
                    })
                    .await
                    .expect("steer sent");
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");

    // The fixture refuses (and exits non-zero) unless a `steer` command with
    // the text arrived — its response gates everything here. The Steered
    // boundary lands BEFORE the steered content.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "steered".into()
    }));
    let steered = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Steered { .. }))
        .expect("steer boundary must exist: {events:?}");
    let steered_text = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "steered"))
        .expect("steered content exists");
    assert!(steered < steered_text, "{events:?}");
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

/// Number of `Done` events already observed at the time a `Done` arrives.
fn dones_so_far(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Done { .. }))
        .count()
}

#[tokio::test]
async fn parked_mailbox_restarts_via_prompt_not_idle_steer() {
    // The first turn settles and the child PERSISTS (a real pi keeps the
    // session open across turns). The second mailbox send must restart the
    // session via `prompt` carrying `streamingBehavior:"steer"` — the
    // fixture rejects an idle `steer` (a parked pi only queues steers, so
    // the harness would strand the message forever) AND a plain prompt
    // without streamingBehavior (pi rejects one while streaming — the
    // confirmed parked-session wedge; the routed prompt is atomic across
    // pi's real idle/active state). One SessionStarted, two Completed Dones,
    // and the Steered boundary precedes the second turn's output — including
    // a notify that lands BEFORE the prompt response and must fold into the
    // second segment.
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:parked"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            // Send the second mailbox message once the FIRST turn is Done
            // (the harness is parked — a live-turn steer would be wrong).
            if let AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            } = ev
                && dones_so_far(&events) == 0
            {
                steer
                    .send(SteerMessage {
                        prompt: "second message".into(),
                        message_id: None,
                    })
                    .await
                    .expect("second mailbox send");
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");

    // One persistent session across both turns.
    let sessions = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::SessionStarted { .. }))
        .count();
    assert_eq!(sessions, 1, "{events:?}");
    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None), (DoneStatus::Completed, None),],
        "{events:?}"
    );
    // The boundary precedes the second turn's output AND the pre-response
    // notify that landed before the prompt response.
    let boundary = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Steered { .. }))
        .expect("second-turn boundary must exist");
    let note = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "pre-response note"))
        .expect("pre-response notify must stream");
    let second = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "second"))
        .expect("second turn text");
    assert!(boundary < note, "{events:?}");
    assert!(boundary < second, "{events:?}");
}

#[tokio::test]
async fn parked_notify_only_turn_rearms_no_activity_and_resets_result() {
    // A second (parked) turn whose prompt produces ZERO agent-lifecycle
    // events — only a notify, delayed past the prompt response. The harness
    // must re-arm the no-activity grace AFTER the response (a stale turn-1
    // timer would fire immediately with empty/old text) and Done Completed
    // with the SECOND turn's notify text (per-turn result reset). The parked
    // prompt must carry `streamingBehavior:"steer"` (the fixture rejects a
    // plain parked prompt).
    let harness = harness().with_no_activity_grace(Duration::from_millis(200));
    let (controls, steer, _token) = controls();
    let stream = harness
        .run(request("scenario:parked-noagent"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if let AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            } = ev
                && dones_so_far(&events) == 0
            {
                steer
                    .send(SteerMessage {
                        prompt: "notify only".into(),
                        message_id: None,
                    })
                    .await
                    .expect("second mailbox send");
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");

    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None), (DoneStatus::Completed, None),],
        "{events:?}"
    );
    // Second Done's result is the SECOND turn's notify text — never turn 1's
    // "first turn", never empty (proving the grace was re-armed, not stale).
    let results: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Done { result, .. } => Some(result.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "{events:?}");
    assert_eq!(results[0].as_deref(), Some("first turn"), "{events:?}");
    assert_eq!(
        results[1].as_deref(),
        Some("second turn note"),
        "{events:?}"
    );
}

#[tokio::test]
async fn rapid_double_mailbox_queues_two_parked_turns() {
    // Two mailbox messages sent back-to-back while parked: both are honored
    // as sequential parked prompts (turn 2 then turn 3) — never dropped,
    // never sent as idle steers, never concurrent with each other.
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:parked-double"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        let mut sent = false;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if let AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            } = ev
                && !sent
            {
                // Fire both rapidly while the harness is parked.
                for prompt in ["second turn", "third turn"] {
                    steer
                        .send(SteerMessage {
                            prompt: prompt.into(),
                            message_id: None,
                        })
                        .await
                        .expect("mailbox send");
                }
                sent = true;
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");

    assert_eq!(
        dones(&events),
        vec![
            (DoneStatus::Completed, None),
            (DoneStatus::Completed, None),
            (DoneStatus::Completed, None),
        ],
        "{events:?}"
    );
    // Two parked boundaries, one per extra turn, each before its output.
    let boundaries: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, AgentEvent::Steered { .. }).then_some(i))
        .collect();
    assert_eq!(boundaries.len(), 2, "{events:?}");
    for text in ["second", "third"] {
        let pos = events
            .iter()
            .position(|e| matches!(e, AgentEvent::TextDelta { text: t } if t == text))
            .expect(text);
        assert!(
            boundaries.iter().any(|&b| b < pos),
            "{text} must follow a boundary: {events:?}"
        );
    }
}

#[tokio::test]
async fn steer_accepted_around_settle_is_retried_as_an_idle_prompt() {
    // The settle race: a steer ACCEPTED mid-turn whose reply never streams
    // (the turn settles first). The accepted steer must NOT strand forever —
    // the harness retries it as an idle prompt after the park.
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:parked-stranded"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            // Send mid-turn (right after the tool call) so the harness routes
            // it as an ACTIVE steer — the fixture accepts it but settles
            // without ever delivering the reply.
            if matches!(&ev, AgentEvent::ToolResult { id, .. } if id == "t1") {
                steer
                    .send(SteerMessage {
                        prompt: "redirect".into(),
                        message_id: None,
                    })
                    .await
                    .expect("steer sent");
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");

    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None), (DoneStatus::Completed, None),],
        "{events:?}"
    );
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "redirected".into()
    }));
    // The retried idle prompt emits its Steered boundary before the output.
    let boundary = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Steered { .. }))
        .expect("boundary must exist");
    let redirected = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "redirected"))
        .expect("retried output");
    assert!(boundary < redirected, "{events:?}");
}

#[tokio::test]
async fn interrupt_sends_abort_and_settles_interrupted() {
    let (controls, _steer, token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:interrupt"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "working") {
                token.cancel();
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");
    assert_eq!(dones(&events), vec![(DoneStatus::Interrupted, None)]);
}

#[tokio::test]
async fn wedged_pi_escalates_to_signals_and_still_ends_interrupted() {
    // The fixture never answers the abort (it blocks on read forever); the
    // SIGTERM/SIGKILL escalation must reap the child and still end Interrupted.
    let script = fixture_path().parent().unwrap().join("wedged-pi.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
    }
    let harness = PiHarness::new(std::env::temp_dir().join("cypher-pi-wedge-sessions"))
        .with_executable(&script)
        .with_graces(Duration::from_millis(100), Duration::from_millis(200));
    let (controls, _steer, token) = controls();
    let stream = harness
        .run(request("scenario:wedge"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "working") {
                token.cancel();
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("escalation reaped the child in time");
    let dones = dones(&events);
    assert_eq!(dones.len(), 1, "{events:?}");
    assert_eq!(dones[0].0, DoneStatus::Interrupted);
}

#[tokio::test]
async fn extension_select_round_trips_through_the_input_bridge() {
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), request("scenario:ui-select"), controls).await;
    // The fixture refuses unless the harness relayed the user's "tokio" pick
    // as an extension_ui_response — the answer it streams proves the round
    // trip.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "answered".into()
    }));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn missing_binary_surfaces_not_installed() {
    let harness = PiHarness::new(std::env::temp_dir().join("cypher-pi-missing-sessions"))
        .with_executable("/nonexistent/definitely-not-pi");
    let err = harness
        .run(request("x"), controls().0)
        .await
        .err()
        .expect("missing binary must fail");
    assert!(matches!(
        err,
        HarnessError::NotInstalled(_) | HarnessError::Io(_)
    ));
}

#[tokio::test]
async fn hung_handshake_errors_instead_of_spinning_forever() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("hung-pi.sh");
    std::fs::write(&script, "#!/bin/sh\nexec sleep 1000\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let harness = PiHarness::new(std::env::temp_dir().join("cypher-pi-hung-sessions"))
        .with_executable(&script)
        .with_handshake_timeout(Duration::from_millis(300));
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness, request("hi"), controls).await;
    let dones = dones(&events);
    assert_eq!(dones.len(), 1, "{events:?}");
    let (status, error) = &dones[0];
    assert_eq!(*status, DoneStatus::Errored);
    let error = error.as_deref().unwrap_or_default();
    assert!(error.contains("handshake"), "{error}");
}

#[test]
fn descriptor_surface_matches_registry_expectations() {
    let harness = harness();
    assert_eq!(harness.id(), HarnessId::Pi);
    assert_eq!(harness.display_name(), "Pi");
    assert!(harness.supports_steering());
    // Native mid-run steer: step boundary, not turn boundary.
    assert_eq!(harness.steering_mode(), SteeringMode::StepBoundary);
    assert_eq!(
        harness.reasoning_levels(),
        &[
            ReasoningLevel::Minimal,
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::XHigh,
            ReasoningLevel::Max,
        ]
    );
}
