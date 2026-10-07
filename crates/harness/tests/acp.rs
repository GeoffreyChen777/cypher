//! AcpHarness integration tests against the fake ACP agent in
//! `tests/fixtures/fake-acp.sh` (no real `grok` binary involved).

mod common;

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use common::{dones, run_to_end};
use cypher_harness::{
    AcpHarness, CancellationToken, Harness, HarnessError, RunControls, RunHostContext, SteerMessage,
};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, RunRequest, SteeringMode, TodoItem, ToolCall,
    UserInputAnswer,
};

fn fixture_path() -> PathBuf {
    common::fixture("fake-acp.sh")
}

fn harness() -> AcpHarness {
    AcpHarness::grok().with_executable(fixture_path())
}

fn request(prompt: &str) -> RunRequest {
    common::request(prompt, Some("grok-4.5"))
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    common::controls_answering("Yes")
}

#[tokio::test]
async fn happy_path_maps_chunks_tools_diffs_plans_and_commands() {
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), request("scenario:happy"), controls).await;

    // SessionStarted from session/new's id.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStarted { harness, session_id, cwd, .. }
                if *harness == HarnessId::Grok && session_id == "s-1" && cwd == "/tmp"
        )),
        "{events:?}"
    );

    // Initialize-advertised commands surface before the turn.
    let commands: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AvailableCommands { commands } => Some(commands.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(commands.len(), 2, "{events:?}");
    assert_eq!(commands[0][0].name, "compact");
    assert_eq!(commands[0][1].input_hint.as_deref(), Some("the goal"));
    // Mid-run advertisement replaces the list.
    assert_eq!(commands[1][0].name, "deep-research");

    // Chunks; the wrong-session and non-text chunks never surface.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Hello".into()
    }));
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "thinking".into()
    }));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("WRONG"))),
        "{events:?}"
    );

    // Execute tool: pending opens the call, the completed update resolves it
    // with capped multi-line output (newlines preserved verbatim).
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "t1".into(),
        call: ToolCall::Exec {
            command: "cargo test -p cypher-harness".into()
        },
    }));
    let exec_output = events
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
        .expect("exec output present");
    assert!(exec_output.starts_with("   Compiling cypher-harness"));
    assert_eq!(exec_output.lines().count(), 6, "{exec_output:?}");

    // Edit tool: single-shot completed call carries the inline diff.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "t2".into(),
        call: ToolCall::EditFile {
            path: "/w/src/resolve.rs".into(),
            old_string: None,
            new_string: None,
        },
    }));
    let diff = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolResult {
                id,
                diff: Some(diff),
                ..
            } if id == "t2" => Some(diff.clone()),
            _ => None,
        })
        .expect("edit diff present");
    assert_eq!(diff.path, "/w/src/resolve.rs");
    assert!(
        diff.old_text
            .as_deref()
            .is_some_and(|t| t.contains(".filter(|p| p.exists())")),
        "{diff:?}"
    );
    assert!(diff.new_text.contains("split_paths"), "{diff:?}");

    // Plan → stable todo chip.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "acp-plan".into(),
        call: ToolCall::Todo {
            items: vec![
                TodoItem {
                    text: "read".into(),
                    done: true
                },
                TodoItem {
                    text: "fix".into(),
                    done: false
                },
            ]
        },
    }));

    // usage_update is the context gauge, never per-turn tokens.
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
    assert!(events.contains(&AgentEvent::ContextUsage {
        used: 1200,
        size: 500_000,
    }));

    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn config_options_apply_requested_model_and_effort() {
    let (controls, _steer, _token) = controls();
    let mut req = request("scenario:config");
    req.reasoning = Some(cypher_proto::ReasoningLevel::Medium);
    let events = run_to_end(&harness(), req, controls).await;
    // The fixture answers refusal unless BOTH set_config_option calls
    // (model grok-4.5, effort medium) arrived before the prompt.
    assert!(
        events.contains(&AgentEvent::TextDelta {
            text: "configured".into()
        }),
        "{events:?}"
    );
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn question_shaped_requests_bridge_to_the_input_panel() {
    // The controls' bridge answers every question with its FIRST option
    // label — build controls that answer "Use tokio" specifically.
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec!["Use tokio".into()],
                })
                .collect();
            let _ = tx.send(answers);
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
        host: RunHostContext::default(),
    };
    let _keep = (steer_tx, token);
    let events = run_to_end(&harness(), request("scenario:question"), controls).await;
    // The fixture answers refusal unless the harness relayed the choice
    // (optionId opt-tokio) instead of auto-accepting.
    assert!(
        events.contains(&AgentEvent::TextDelta {
            text: "answered".into()
        }),
        "{events:?}"
    );
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn claude_and_codex_specs_drive_the_same_wire() {
    // The whole point of the conversion: every spec runs against the same
    // fake ACP agent with no per-agent protocol code. Model ids in the
    // fixture are grok-flavored, so config sets simply skip.
    for (name, h) in [
        (
            "claude",
            AcpHarness::claude().with_executable(fixture_path()),
        ),
        ("codex", AcpHarness::codex().with_executable(fixture_path())),
        (
            "hermes",
            AcpHarness::hermes().with_executable(fixture_path()),
        ),
        (
            "cursor",
            AcpHarness::cursor().with_executable(fixture_path()),
        ),
    ] {
        let (controls, _steer, _token) = controls();
        let events = run_to_end(&h, request("scenario:happy"), controls).await;
        assert!(
            events.contains(&AgentEvent::TextDelta {
                text: "Hello".into()
            }),
            "{name}: {events:?}"
        );
        assert_eq!(
            dones(&events),
            vec![(DoneStatus::Completed, None)],
            "{name}"
        );
    }
}

#[tokio::test]
async fn ultrathink_prefixes_the_prompt_for_claude() {
    let (controls, _steer, _token) = controls();
    let h = AcpHarness::claude().with_executable(fixture_path());
    let mut req = request("scenario:echo-prompt");
    req.reasoning = Some(cypher_proto::ReasoningLevel::Ultrathink);
    let events = run_to_end(&h, req, controls).await;
    // The fixture echoes the prompt text back; the Ultrathink prefix must be
    // on the wire.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::TextDelta { text } if text.starts_with("Ultrathink:")
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn permission_requests_auto_accept_the_preferred_allow_option() {
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), request("scenario:permission"), controls).await;
    // The fixture answers refusal unless the harness selected "always".
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "approved".into()
    }));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn steering_extension_injects_mid_turn() {
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:steer-ext"), controls)
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

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Steered { .. })),
        "{events:?}"
    );
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "steered".into()
    }));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

/// The steering response racing the turn's own end: the injection landed in
/// the dying turn, and the prompt response reached the wire first. The
/// boundary must still be emitted BEFORE the Done — a Steered after Done
/// re-armed the consumer (parked session → Working) with no next turn and no
/// Done ever coming (the stranded-Working / eternal-timer bug).
#[tokio::test]
async fn steer_racing_the_turn_end_never_emits_steered_after_done() {
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:steer-race"), controls)
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

    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None)],
        "{events:?}"
    );
    let steered = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Steered { .. }))
        .expect("steer landed in the turn: a Steered boundary must exist");
    let done = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("checked above");
    assert!(
        steered < done,
        "Steered after Done strands the session: {events:?}"
    );
}

#[tokio::test]
async fn rejected_steer_queues_and_delivers_at_the_turn_boundary() {
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:steer-queue"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        let mut steer = Some(steer);
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "first")
                && let Some(steer) = &steer
            {
                steer
                    .send(SteerMessage {
                        prompt: "redirect please".into(),
                        message_id: None,
                    })
                    .await
                    .expect("steer sent");
            }
            // Close the mailbox once the boundary turn streams so the
            // persistent session winds down and the stream ends.
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "boundary") {
                steer = None;
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");

    // First turn completes, then the queued steer becomes the boundary turn.
    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None), (DoneStatus::Completed, None)],
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Steered { .. })),
        "{events:?}"
    );
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "boundary".into()
    }));
}

#[tokio::test]
async fn interrupt_sends_session_cancel_and_ends_interrupted() {
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
async fn wedged_agent_escalates_to_signals_and_still_ends_interrupted() {
    let (controls, _steer, token) = controls();
    let harness = harness().with_graces(Duration::from_millis(100), Duration::from_millis(200));
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
async fn refusal_maps_to_an_errored_done() {
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), request("scenario:refusal"), controls).await;
    let dones = dones(&events);
    assert_eq!(dones.len(), 1);
    assert_eq!(dones[0].0, DoneStatus::Errored);
    assert!(dones[0].1.as_deref().unwrap_or("").contains("refused"));
}

#[tokio::test]
async fn resume_loads_the_session_and_drops_replayed_history() {
    let (controls, _steer, _token) = controls();
    let mut req = request("scenario:resumed");
    req.resume = Some("s-loaded".into());
    let events = run_to_end(&harness(), req, controls).await;
    // The 600-update replay is drained without surfacing…
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("old reply"))),
        "{events:?}"
    );
    // …the loaded session id sticks, and the live turn still streams.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "s-loaded"
    )));
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "back again".into()
    }));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn failed_load_falls_back_to_a_fresh_session() {
    let (controls, _steer, _token) = controls();
    let mut req = request("scenario:resumed");
    req.resume = Some("load-fail".into());
    let events = run_to_end(&harness(), req, controls).await;
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "s-fresh"
    )));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn commands_discovery_scans_the_initialize_response() {
    let harness = harness();
    let commands = harness.commands().await.expect("discovery");
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert_eq!(commands[0].name, "compact");
    assert_eq!(commands[1].name, "goal");
    assert_eq!(commands[1].input_hint.as_deref(), Some("the goal"));
    // Cached: a second call must not respawn (same result, instant).
    let again = harness.commands().await.expect("cached");
    assert_eq!(again, commands);
}

#[tokio::test]
async fn missing_binary_surfaces_not_installed_with_install_hint() {
    let harness = AcpHarness::grok().with_executable("/nonexistent/definitely-not-grok");
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

#[test]
fn descriptor_surface_matches_registry_expectations() {
    let harness = AcpHarness::grok();
    assert_eq!(harness.id(), HarnessId::Grok);
    assert_eq!(harness.display_name(), "Grok");
    assert!(harness.supports_steering());
    assert_eq!(harness.steering_mode(), SteeringMode::TurnBoundary);
    assert_eq!(
        harness.reasoning_levels(),
        &[
            cypher_proto::ReasoningLevel::Low,
            cypher_proto::ReasoningLevel::Medium,
            cypher_proto::ReasoningLevel::High,
        ]
    );
}

#[tokio::test]
async fn models_are_discovered_from_the_acp_session() {
    // ACP is the source of truth: the fixture advertises a model config
    // option, so the picker list comes from the wire, not the static catalog.
    let harness = AcpHarness::hermes().with_executable(fixture_path());
    let models = harness.models().await.expect("discovery");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["grok-4-fast", "grok-4.5"], "{models:?}");
    // Unmatched ids inherit the probe session's thought_level ladder.
    assert_eq!(
        models[0].reasoning_levels,
        vec![
            cypher_proto::ReasoningLevel::Low,
            cypher_proto::ReasoningLevel::Medium,
            cypher_proto::ReasoningLevel::High,
        ],
        "{models:?}"
    );
    assert_eq!(models[0].description.as_deref(), Some("Fast tier"));
    // Cached: a second call returns the same list without respawning.
    let again = harness.models().await.expect("cached");
    assert_eq!(again, models);
}

#[tokio::test]
async fn models_enrich_from_the_static_catalog_on_id_match() {
    // grok's static catalog knows "grok-4.5" — the discovered entry keeps the
    // wire label but inherits the curated description and ladder.
    let harness = AcpHarness::grok().with_executable(fixture_path());
    let models = harness.models().await.expect("discovery");
    let grok45 = models
        .iter()
        .find(|m| m.id == "grok-4.5")
        .expect("grok-4.5");
    assert_eq!(
        grok45.description.as_deref(),
        Some("xAI's coding model — 500k context"),
        "{grok45:?}"
    );
}

#[tokio::test]
async fn models_fall_back_to_the_static_catalog_when_the_probe_fails() {
    // Hermes's static catalog answers when the agent can't be probed.
    let harness = AcpHarness::hermes().with_executable("/nonexistent/never-a-hermes");
    let models = harness.models().await.expect("static fallback");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["hermes-4-405b", "hermes-4-70b"], "{models:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn hung_handshake_errors_instead_of_spinning_forever() {
    // An agent that consumes stdin and never answers initialize — the
    // "thinking for minutes, then nothing" startup class (issue #93). The
    // run must end with a Done that names the timeout, not hang.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("hung-agent.sh");
    // sleep inherits the stdio pipes and holds them open without ever
    // answering — a true wedge, not a crash.
    std::fs::write(&script, "#!/bin/sh\nexec sleep 1000\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let harness = AcpHarness::grok()
        .with_executable(&script)
        .with_handshake_timeout(Duration::from_millis(300));
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness, request("hi"), controls).await;
    let dones = dones(&events);
    assert_eq!(dones.len(), 1, "{events:?}");
    let (status, error) = &dones[0];
    assert_eq!(*status, DoneStatus::Errored);
    let error = error.as_deref().unwrap_or_default();
    assert!(
        error.contains("did not complete the ACP handshake"),
        "{error}"
    );
}

#[test]
fn cursor_descriptor_surface_matches_registry_expectations() {
    let cursor = AcpHarness::cursor();
    assert_eq!(cursor.id(), HarnessId::Cursor);
    assert_eq!(cursor.display_name(), "Cursor");
    assert!(cursor.supports_steering());
    assert_eq!(cursor.steering_mode(), SteeringMode::TurnBoundary);
    // Cursor carries effort in the model id's bracket suffix, so there is no
    // separate ladder for the Reasoning dropdown to drive.
    assert!(cursor.reasoning_levels().is_empty());
}

#[test]
fn hermes_descriptor_surface_matches_registry_expectations() {
    let hermes = AcpHarness::hermes();
    assert_eq!(hermes.id(), HarnessId::Hermes);
    assert_eq!(hermes.display_name(), "Hermes");
    assert!(hermes.supports_steering());
    assert_eq!(hermes.steering_mode(), SteeringMode::TurnBoundary);
    assert!(hermes.reasoning_levels().is_empty());
}

/// The 2026-08-12 stuck-Working wedge, end to end: a prompt whose turn was
/// consumed by CLI-side self-continuation never gets its response. A steer's
/// `noRunningTurn` steering outcome is the protocol evidence the pending
/// prompt can never settle; after the grace the harness closes the dead turn
/// (Done — never a stranded Working) and promotes the steer to a fresh
/// prompt, which settles normally.
#[tokio::test]
async fn starved_prompt_recovers_via_no_running_turn_evidence() {
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:starve"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(15), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "working") {
                steer
                    .send(SteerMessage {
                        prompt: "what about now".into(),
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

    // Two settled turns: the synthesized close of the starved prompt, then
    // the promoted steer's real turn.
    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None), (DoneStatus::Completed, None)],
        "{events:?}"
    );
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "promoted".into()
    }));
    let steered = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Steered { .. }))
        .expect("the queued steer must be promoted through a Steered boundary");
    let first_done = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("dones asserted above");
    assert!(
        first_done < steered,
        "the dead turn settles before the promoted boundary: {events:?}"
    );
}

/// The dropped-reply turn end with no steer involved: the adapter emits the
/// turn's terminal cost frame (`usage_update` with `cost`) but never the
/// prompt response. The claude spec must settle the turn off that evidence
/// within its 1s grace — Working clears in about a second, not never (and
/// not only after a watchdog window).
#[tokio::test]
async fn dropped_reply_settles_fast_off_the_turn_end_cost_frame() {
    let (controls, _steer, _token) = controls();
    let harness = AcpHarness::claude().with_executable(fixture_path());
    let started = std::time::Instant::now();
    let stream = harness
        .run(request("scenario:cost-starve"), controls)
        .await
        .expect("run starts");
    let (events, done_at) = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut done_at = None;
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::Done { .. }) && done_at.is_none() {
                done_at = Some(started.elapsed());
            }
            events.push(ev);
        }
        (events, done_at)
    })
    .await
    .expect("run finished in time");

    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None)],
        "{events:?}"
    );
    // The fixture holds its stream open for 6s after the cost frame; the
    // Done must come from the 1s cost-hint grace, not stream EOF (margin
    // sized for parallel-suite load).
    let done_at = done_at.expect("dones asserted above");
    assert!(
        done_at < Duration::from_secs(4),
        "Done at {done_at:?} — should be ~1s after the cost frame, well \
         before the fixture's 6s exit"
    );
}

/// Prevention for the interactive starve: a steer arriving while the agent
/// is mid SELF-CONTINUED turn (open tool call, no prompt outstanding) must
/// not become a session/prompt — the adapter drops that reply. The harness
/// cancels the unowned turn first (the fixture asserts session/cancel is on
/// the wire before any prompt), then dispatches the steer as a fresh prompt
/// after the flush window.
#[tokio::test]
async fn steer_into_self_continuation_cancels_before_prompting() {
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:busy-steer"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(15), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        let mut steer = Some(steer);
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            // The self-continued tool call is the busy signal: steer now.
            if matches!(&ev, AgentEvent::ToolCall { id, .. } if id == "sc-1")
                && let Some(tx) = steer.take()
            {
                tx.send(SteerMessage {
                    prompt: "what about now".into(),
                    message_id: None,
                })
                .await
                .expect("steer sent");
                // Sender dropped here; the mailbox closes so the run can end.
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");

    // Two clean turns: the first prompt's, then the promoted steer's —
    // and the fixture exits with `refusal` if a prompt ever arrives
    // without the preceding session/cancel.
    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None), (DoneStatus::Completed, None)],
        "{events:?}"
    );
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "fresh answer".into()
    }));
    let steered = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Steered { .. }))
        .expect("promoted steer must carry a boundary");
    let first_done = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("dones asserted above");
    assert!(first_done < steered, "{events:?}");
}

/// Claude's native busy-steer path: a steer into a self-continued turn goes
/// out as a PLAIN prompt (the fixture hard-fails on any session/cancel —
/// cancelling would kill the agent's in-flight work). The CLI folds the
/// message into the running turn natively; the adapter drops the prompt's
/// reply; the cost-frame settle closes the turn ~1s after the merged turn
/// really ends — well before the fixture's held-open stream EOF.
#[tokio::test]
async fn claude_busy_steer_rides_native_queueing_and_the_cost_frame() {
    let (controls, steer, _token) = controls();
    let harness = AcpHarness::claude().with_executable(fixture_path());
    let started = std::time::Instant::now();
    let stream = harness
        .run(request("scenario:native-busy-steer"), controls)
        .await
        .expect("run starts");
    let (events, done2_at) = tokio::time::timeout(Duration::from_secs(15), async move {
        let mut events = Vec::new();
        let mut done2_at = None;
        let mut dones_seen = 0usize;
        let mut stream = stream;
        let mut steer = Some(steer);
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(&ev, AgentEvent::ToolCall { id, .. } if id == "sc-2")
                && let Some(tx) = steer.take()
            {
                tx.send(SteerMessage {
                    prompt: "what about now".into(),
                    message_id: None,
                })
                .await
                .expect("steer sent");
            }
            if matches!(ev, AgentEvent::Done { .. }) {
                dones_seen += 1;
                if dones_seen == 2 {
                    done2_at = Some(started.elapsed());
                }
            }
            events.push(ev);
        }
        (events, done2_at)
    })
    .await
    .expect("run finished in time");

    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None), (DoneStatus::Completed, None)],
        "{events:?}"
    );
    // The merged turn's folded output streamed through (nothing cancelled)…
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "merged reply".into()
    }));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("CANCELLED"))),
        "the native path must never cancel self-continued work: {events:?}"
    );
    // …and the settle rode the cost frame, not the 6s stream EOF.
    let done2_at = done2_at.expect("second done asserted above");
    assert!(
        done2_at < Duration::from_secs(5),
        "settle at {done2_at:?} — should ride the cost frame (~1s), not EOF"
    );
}

/// The injection cost frame must never settle a steered turn: the fixture
/// stamps a mid-turn cost frame right after the injection (real adapter
/// behavior, indistinguishable in shape from the terminal frame), holds the
/// turn open past the 1s cost grace + 2s slack, then finishes normally.
/// Exactly one Done; the post-injection text folds into the same turn.
#[tokio::test]
async fn injection_cost_frame_never_settles_a_steered_turn() {
    let (controls, steer, _token) = controls();
    let harness = AcpHarness::claude().with_executable(fixture_path());
    let stream = harness
        .run(request("scenario:steer-cost-noise"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(15), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        let mut steer = Some(steer);
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "first")
                && let Some(tx) = steer.take()
            {
                tx.send(SteerMessage {
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

    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None)],
        "a premature cost-frame settle would double-Done: {events:?}"
    );
    // The post-injection text arrives BEFORE the single Done — a false
    // settle would flip that order.
    let tail = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "steered tail"))
        .expect("steered tail must fold into the live turn: {events:?}");
    let done = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("done asserted above");
    assert!(tail < done, "{events:?}");
}

#[tokio::test]
async fn autonomous_turn_ended_extension_settles_between_prompts() {
    // A background-task wake makes the agent stream a turn no prompt started;
    // its SDK-side turn-end has no `session/prompt` to settle, so the adapter
    // forwards the `_session/turn_ended` extension instead — which must map
    // to a completed Done exactly once, only for this session, and only
    // between prompts (the engine's quiesce watchdog then stays a backstop,
    // not the settle path).
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), request("scenario:autonomous-end"), controls).await;

    let d = dones(&events);
    assert_eq!(
        d.len(),
        2,
        "prompt turn + autonomous turn, no third: {events:?}"
    );
    assert!(
        d.iter()
            .all(|(s, e)| *s == DoneStatus::Completed && e.is_none()),
        "{events:?}"
    );

    // The self-continued output sits BETWEEN the two Dones.
    let first_done = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .unwrap();
    let last_done = events
        .iter()
        .rposition(|e| matches!(e, AgentEvent::Done { .. }))
        .unwrap();
    let background = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "background finished"))
        .expect("self-continued output surfaces: {events:?}");
    assert!(
        first_done < background && background < last_done,
        "{events:?}"
    );
}
