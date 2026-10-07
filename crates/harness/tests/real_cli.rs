//! Manual probes against the REAL agent stacks (adapters, installed and
//! authenticated CLIs, network, live models). Every test is `#[ignore]`; run
//! them explicitly, e.g.
//!
//!   cargo test -p cypher-harness --test real_cli -- --ignored --nocapture real_claude
//!
//! The probes run one at a time ([`serial`]): the quiet-settle A/B probe sets
//! a process-global env knob the others must not observe.

mod common;

use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{MutexGuard, mpsc};

use cypher_harness::{AcpHarness, CancellationToken, Harness, RunControls, SteerMessage};
use cypher_proto::{AgentEvent, DoneStatus, RunRequest};

use common::dones;

async fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    SERIAL.lock().await
}

fn request(prompt: &str) -> RunRequest {
    common::request(prompt, Some("grok-4.5"))
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    common::controls_answering("Yes")
}

/// The two-command probe turn shared by the quiet-settle A/B probe and the
/// survey.
fn probe_request(model: Option<&str>) -> RunRequest {
    RunRequest {
        cwd: std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()),
        ..common::request(
            "Use your shell tool to run `echo probe-one`. After you see its output, \
             run `echo probe-two` as a second separate command. After that, reply \
             with exactly the word PROBE-DONE.",
            model,
        )
    }
}

// ---------------------------------------------------------------------------
// Adapter smoke tests and settle checks
// ---------------------------------------------------------------------------

/// Discovery against the real installed adapters: base model rows only
/// (never one per reasoning effort), with wire-derived trait options. Free
/// (initialize + session/new, no prompt), but needs the CLIs installed and
/// authenticated. Run explicitly:
/// `cargo test -p cypher-harness --test real_cli -- --ignored real_discovery`
#[tokio::test]
#[ignore = "needs the claude + codex CLIs installed and authenticated"]
async fn real_discovery_yields_base_models_with_traits() {
    let _serial = serial().await;
    let codex = AcpHarness::codex().models().await.expect("codex discovery");
    assert!(!codex.is_empty());
    for m in &codex {
        assert!(
            !m.id.contains('[') || m.id.ends_with("[1m]"),
            "effort-variant leaked as a model row: {}",
            m.id
        );
    }
    let sol = codex.iter().find(|m| m.id == "gpt-5.6-sol").expect("sol");
    assert!(
        sol.options.iter().any(|o| o.id == "fast-mode"),
        "codex fast-mode trait missing: {:?}",
        sol.options
    );
    assert!(!sol.reasoning_levels.is_empty());

    let claude = AcpHarness::claude()
        .models()
        .await
        .expect("claude discovery");
    assert!(!claude.is_empty());
    for m in &claude {
        assert!(
            !m.reasoning_levels.is_empty(),
            "claude ladder missing on {}",
            m.id
        );
        assert!(
            m.reasoning_levels
                .contains(&cypher_proto::ReasoningLevel::Ultrathink),
            "ultrathink extra missing on {}",
            m.id
        );
    }
}

/// Real-adapter smoke: spawns the actual `claude-agent-acp` (via npx when not
/// installed) against the installed, authenticated claude CLI and burns one
/// tiny haiku prompt. Run explicitly:
/// `cargo test -p cypher-harness --test real_cli -- --ignored real_claude`
#[tokio::test]
#[ignore = "needs the claude CLI authenticated + network; costs one tiny prompt"]
async fn real_claude_adapter_end_to_end() {
    let _serial = serial().await;
    let (controls, steer_tx, _token) = controls();
    let harness = AcpHarness::claude();
    let mut req = request("Reply with exactly the word ACP-OK and nothing else.");
    req.model = Some("claude-haiku-4-5".into());
    req.reasoning = None;
    req.cwd = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let stream = harness.run(req, controls).await.expect("run starts");
    // The session parks after Done while the steering mailbox lives (the
    // engine reaps by dropping it) — release the sender at Done or the
    // stream never ends.
    let events = tokio::time::timeout(Duration::from_secs(180), async move {
        let mut stream = stream;
        let mut steer = Some(steer_tx);
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::Done { .. }) {
                steer = None;
            }
            events.push(ev);
        }
        drop(steer);
        events
    })
    .await
    .expect("real run finished in time");
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("ACP-OK"),
        "unexpected reply: {text:?}\n{events:?}"
    );
    assert_eq!(dones(&events).len(), 1, "{events:?}");
    assert_eq!(dones(&events)[0].0, DoneStatus::Completed, "{events:?}");
}

/// The Cursor slot against the real `cursor-agent acp` server: discovery
/// (free) plus one tiny prompt. Run explicitly:
/// `cargo test -p cypher-harness --test real_cli -- --ignored real_cursor`
#[tokio::test]
#[ignore = "needs the cursor-agent CLI authenticated + network; costs one tiny prompt"]
async fn real_cursor_adapter_end_to_end() {
    let _serial = serial().await;
    // `session/new` is the source of truth for models — the static catalog is
    // a fallback, so a live account must produce more than it.
    let models = AcpHarness::cursor().models().await.expect("discovery");
    assert!(models.len() > 5, "{models:?}");
    assert!(
        models
            .iter()
            .any(|m| m.id == "composer-2.5" || m.id.starts_with("composer-2.5")),
        "{models:?}"
    );
    // Parameterized picker: base ids, no raw HTML, Mode on every row, Auto
    // exposes Intelligence / Balance / Cost.
    assert!(
        models.iter().all(|m| !m.label.contains('<')),
        "html leaked into labels: {:?}",
        models
            .iter()
            .filter(|m| m.label.contains('<'))
            .map(|m| &m.label)
            .collect::<Vec<_>>()
    );
    assert!(
        models
            .iter()
            .all(|m| m.options.iter().any(|o| o.id == "mode")),
        "Mode trait missing"
    );
    let auto = models
        .iter()
        .find(|m| m.id == "auto-smart")
        .expect("parameterized Auto id");
    let optimize = auto
        .options
        .iter()
        .find(|o| o.id == "optimize_for")
        .expect("Optimize For");
    let tiers: Vec<&str> = optimize.choices.iter().map(|c| c.id.as_str()).collect();
    assert!(tiers.contains(&"intelligence"), "{tiers:?}");
    assert!(tiers.contains(&"balanced"), "{tiers:?}");
    assert!(tiers.contains(&"cost"), "{tiers:?}");

    let (controls, steer_tx, _token) = controls();
    let harness = AcpHarness::cursor();
    let mut req = request("Reply with exactly the word ACP-OK and nothing else.");
    req.model = models
        .iter()
        .find(|m| m.id == "composer-2.5" || m.id.starts_with("composer-2.5"))
        .map(|m| m.id.clone());
    req.reasoning = None;
    req.cwd = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let stream = harness.run(req, controls).await.expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(180), async move {
        let mut stream = stream;
        let mut steer = Some(steer_tx);
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::Done { .. }) {
                steer = None;
            }
            events.push(ev);
        }
        drop(steer);
        events
    })
    .await
    .expect("real run finished in time");
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("ACP-OK"), "unexpected reply: {text:?}");
    assert_eq!(dones(&events).len(), 1, "{events:?}");
    assert_eq!(dones(&events)[0].0, DoneStatus::Completed, "{events:?}");
}

/// Cursor's todo extension reaches the stream as a chip, and its tools land
/// through the standard `session/update` path. Worth pinning live: the docs
/// call `cursor/update_todos` a fire-and-forget notification, but the CLI
/// sends it as a REQUEST — an unanswered one would stall the turn. Run:
/// `cargo test -p cypher-harness --test real_cli -- --ignored real_cursor_todos`
#[tokio::test]
#[ignore = "needs the cursor-agent CLI authenticated + network; costs one small prompt"]
async fn real_cursor_todos_and_tools_reach_the_stream() {
    let _serial = serial().await;
    let (controls, steer_tx, _token) = controls();
    let harness = AcpHarness::cursor();
    let mut req = request(
        "Use your todo tool to record 3 steps, then run `echo hi` in the shell. \
         Keep it brief.",
    );
    req.reasoning = None;
    req.cwd = std::env::temp_dir().to_string_lossy().into_owned();
    let stream = harness.run(req, controls).await.expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(240), async move {
        let mut stream = stream;
        let mut steer = Some(steer_tx);
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::Done { .. }) {
                steer = None;
            }
            events.push(ev);
        }
        drop(steer);
        events
    })
    .await
    .expect("real run finished in time");
    let todos: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::ToolCall {
                    call: cypher_proto::ToolCall::Todo { .. },
                    ..
                }
            )
        })
        .collect();
    assert!(!todos.is_empty(), "no todo chip: {events:?}");
    // Repeated updates refresh one chip rather than stacking new ones.
    assert!(
        todos.iter().all(|e| matches!(
            e,
            AgentEvent::ToolCall { id, .. } if id == "cursor-todos"
        )),
        "todo chips must share the stable id: {todos:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCall {
                call: cypher_proto::ToolCall::Exec { .. },
                ..
            }
        )),
        "no exec tool call: {events:?}"
    );
    assert_eq!(dones(&events)[0].0, DoneStatus::Completed, "{events:?}");
}

/// Full-stack verification of the 2026-08-12 starve fix against the REAL
/// adapter + CLI: prompt#1 backgrounds a task and ends; the CLI
/// self-continues on its notification and runs a 20s foreground command; a
/// steer lands mid-way. With prevention in place the harness cancels the
/// unowned turn and prompts fresh (no starve to recover from); the turn
/// must settle promptly either way — never strand, never wait for a
/// watchdog. Costs a few small prompts. Run explicitly:
/// `cargo test -p cypher-harness --test real_cli -- --ignored real_claude_starve`
#[tokio::test]
#[ignore = "needs the claude CLI authenticated + network; costs a few small prompts"]
async fn real_claude_starve_settles_off_the_cost_frame() {
    let _serial = serial().await;
    let (controls, steer_tx, _token) = controls();
    let harness = AcpHarness::claude();
    let mut req = request(
        "Use the Bash tool exactly twice, then stop.\n\
         First call: run the command `sleep 8; echo task-finished` with \
         run_in_background set to true.\n\
         Then reply with exactly the word: started\n\
         IMPORTANT: later, when a task notification about that background \
         task arrives, make one FOREGROUND Bash call: `sleep 20; echo waited` \
         (no run_in_background), then reply with exactly: done waiting",
    );
    req.model = None;
    req.reasoning = None;
    req.cwd = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let stream = harness.run(req, controls).await.expect("run starts");

    let collected = tokio::time::timeout(Duration::from_secs(240), async move {
        let mut stream = stream;
        let mut events: Vec<(std::time::Instant, AgentEvent)> = Vec::new();
        let mut dones_seen = 0usize;
        let mut steer = Some(steer_tx);
        let mut steer_sent_at: Option<std::time::Instant> = None;
        let mut steer_task: Option<tokio::task::JoinHandle<std::time::Instant>> = None;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            let now = std::time::Instant::now();
            if matches!(ev, AgentEvent::Done { .. }) {
                dones_seen += 1;
                if dones_seen == 1 {
                    // Steer 16s after the first turn settles: the background
                    // task (8s) has exited and the CLI is inside its
                    // self-continued turn's 20s foreground command.
                    let tx = steer.take().expect("one steer");
                    steer_task = Some(tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(16)).await;
                        let at = std::time::Instant::now();
                        let _ = tx
                            .send(SteerMessage {
                                prompt: "what about now".into(),
                                message_id: None,
                            })
                            .await;
                        at
                    }));
                }
            }
            events.push((now, ev));
            if dones_seen == 2 {
                if let Some(task) = steer_task.take() {
                    steer_sent_at = task.await.ok();
                }
                break;
            }
        }
        (events, steer_sent_at)
    })
    .await
    .expect("run should settle without any watchdog — before the fix this timed out");

    let (events, steer_sent_at) = collected;
    let evs: Vec<&AgentEvent> = events.iter().map(|(_, e)| e).collect();
    let done_times: Vec<std::time::Instant> = events
        .iter()
        .filter(|(_, e)| matches!(e, AgentEvent::Done { .. }))
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(done_times.len(), 2, "{evs:?}");
    for (_, e) in events.iter() {
        if let AgentEvent::Done { status, .. } = e {
            assert_eq!(*status, DoneStatus::Completed, "{evs:?}");
        }
    }
    // Prevention path: the steer landed mid self-continued turn, was
    // preceded by a cancel, and its fresh prompt settled promptly — well
    // under the quiet/watchdog windows it used to need.
    let steer_sent_at = steer_sent_at.expect("steer timer ran");
    let steer_turn = done_times[1].duration_since(steer_sent_at);
    assert!(
        steer_turn < Duration::from_secs(25),
        "steer took {steer_turn:?} to settle — prevention should cancel the \
         unowned turn and prompt fresh, not starve into a settle window"
    );
    // And the settle tracked the agent's actual finish (last streamed text),
    // not a watchdog window: cost-frame grace is 1s, allow scheduling slack.
    let last_text_at = events
        .iter()
        .filter(|(_, e)| matches!(e, AgentEvent::TextDelta { .. }))
        .map(|(t, _)| *t)
        .next_back()
        .expect("streamed text exists");
    let settle_gap = done_times[1].duration_since(last_text_at);
    assert!(
        settle_gap < Duration::from_secs(5),
        "final Done lagged the last content by {settle_gap:?} — the settle \
         should ride the turn-end cost frame (~1s)"
    );
}

/// Every installed real agent, through the one shared loop all the
/// starve/settle changes live in: a short live turn with a mid-turn steer —
/// injection on StepBoundary agents, boundary delivery on TurnBoundary ones,
/// busy-path handling where it applies. Contract per agent that starts:
/// every Done is Completed and the stream ENDS (no stranding) inside the
/// budget. Agents that fail auth/startup are reported and skipped. Run:
/// `cargo test -p cypher-harness --test real_cli -- --ignored --nocapture real_all_harnesses`
#[tokio::test]
#[ignore = "runs every installed+authenticated agent CLI; costs a few small prompts"]
async fn real_all_harnesses_settle_with_a_mid_turn_steer() {
    let _serial = serial().await;
    let agents: Vec<(&str, AcpHarness)> = vec![
        ("claude", AcpHarness::claude()),
        ("codex", AcpHarness::codex()),
        ("cursor", AcpHarness::cursor()),
        ("grok", AcpHarness::grok()),
    ];
    let mut failures: Vec<String> = Vec::new();
    for (name, h) in agents {
        let (controls, steer_tx, _token) = controls();
        let mut req = request(
            "Write the numbers 1 2 3 4 5, one per line, then stop. \
             If another instruction arrives, follow it too.",
        );
        req.model = None;
        req.reasoning = None;
        req.cwd = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let stream = match h.run(req, controls).await {
            Ok(s) => s,
            Err(e) => {
                println!("[{name}] SKIP — did not start: {e}");
                continue;
            }
        };
        let outcome = tokio::time::timeout(Duration::from_secs(120), async move {
            let mut stream = stream;
            let mut events = Vec::new();
            let mut steer = Some(steer_tx);
            while let Some(ev) = stream.next().await {
                let ev = ev.expect("stream event");
                if matches!(ev, AgentEvent::TextDelta { .. })
                    && let Some(tx) = steer.take()
                {
                    let _ = tx
                        .send(SteerMessage {
                            prompt: "Also write the word EXTRA on its own line.".into(),
                            message_id: None,
                        })
                        .await;
                    // Sender drops: the mailbox closes, so once every turn
                    // settles the stream must end — stranding shows as the
                    // 120s timeout.
                }
                events.push(ev);
            }
            events
        })
        .await;
        match outcome {
            Err(_) => failures.push(format!("[{name}] STRANDED: stream still open after 120s")),
            Ok(events) => {
                let ds = dones(&events);
                let auth_failure = ds.iter().any(|(s, e)| {
                    *s == DoneStatus::Errored
                        && e.as_deref().is_some_and(|e| {
                            let e = e.to_lowercase();
                            e.contains("auth") || e.contains("login") || e.contains("not installed")
                        })
                });
                if auth_failure {
                    println!("[{name}] SKIP — needs auth: {ds:?}");
                } else if ds.is_empty() || ds.iter().any(|(s, _)| *s != DoneStatus::Completed) {
                    failures.push(format!("[{name}] BAD DONES: {ds:?}"));
                } else {
                    let texts = events
                        .iter()
                        .filter(|e| matches!(e, AgentEvent::TextDelta { .. }))
                        .count();
                    println!(
                        "[{name}] OK — {} turn(s) settled, {texts} text deltas",
                        ds.len()
                    );
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Debug variant of the multi-harness sweep, claude only, printing every
/// event with a timestamp — for diagnosing strands the sweep can only name.
/// `cargo test -p cypher-harness --test real_cli -- --ignored --nocapture real_claude_debug`
#[tokio::test]
#[ignore = "debug harness; needs the claude CLI; costs one small prompt"]
async fn real_claude_debug_steer_trace() {
    let _serial = serial().await;
    let (controls, steer_tx, _token) = controls();
    let h = AcpHarness::claude();
    let mut req = request(
        "Write the numbers 1 2 3 4 5, one per line, then stop. \
         If another instruction arrives, follow it too.",
    );
    req.model = None;
    req.reasoning = None;
    req.cwd = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let started = std::time::Instant::now();
    let stream = h.run(req, controls).await.expect("run starts");
    let _ = tokio::time::timeout(Duration::from_secs(60), async move {
        let mut stream = stream;
        let mut steer = Some(steer_tx);
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            let t = started.elapsed();
            match &ev {
                AgentEvent::TextDelta { text } => {
                    println!("{t:?} TEXT {:?}", &text[..text.len().min(40)])
                }
                other => println!("{t:?} {other:?}"),
            }
            if matches!(ev, AgentEvent::TextDelta { .. })
                && let Some(tx) = steer.take()
            {
                println!("{:?} >>> sending steer", started.elapsed());
                let _ = tx
                    .send(SteerMessage {
                        prompt: "Also write the word EXTRA on its own line.".into(),
                        message_id: None,
                    })
                    .await;
            }
        }
        println!("{:?} <<< stream ended", started.elapsed());
    })
    .await;
    println!(
        "{:?} === test done (timeout means strand)",
        started.elapsed()
    );
}

// ---------------------------------------------------------------------------
// Claude quiet-settle A/B probe
// ---------------------------------------------------------------------------

/// A/B evidence probe for the Claude quiet-settle exemption, against the
/// REAL stack (claude-agent-acp + claude CLI + live model). Run explicitly:
///
///   cargo test -p cypher-harness --test real_cli -- --ignored --nocapture real_claude_quiet_ab
///
/// The knob is set to 800ms — far below routine inference-gap silence
/// (tool result → next API roundtrip), the same structural ratio as a 30s
/// window against 30–120s thinking stretches. Where Claude honors the
/// blanket quiet settle, the probe false-settles mid-turn: a premature
/// Done{completed} with the turn's real tail (tool calls / text) streaming
/// AFTER it — the orphan signature. With the exemption, the SAME knob
/// setting must produce exactly one Done, ordered after all content.
///
/// The test asserts nothing: it prints a timestamped trace and a VERDICT
/// line, so traces from two trees can be compared.
const AB_QUIET_MS: u64 = 800;
/// How long to keep observing after the FIRST Done — on unfixed code the
/// orphaned turn's tail lands in this window.
const AB_POST_DONE_WINDOW: Duration = Duration::from_secs(25);

/// Sets `CYPHER_ACP_QUIET_SETTLE_MS` for one probe and clears it on drop.
/// Only taken while holding [`serial`], so no other probe sees it.
struct QuietSettleKnob;

impl QuietSettleKnob {
    fn set(ms: u64) -> Self {
        // SAFETY: probes are serialized; no other harness run reads the env
        // while the knob is set.
        unsafe { std::env::set_var("CYPHER_ACP_QUIET_SETTLE_MS", ms.to_string()) };
        Self
    }
}

impl Drop for QuietSettleKnob {
    fn drop(&mut self) {
        // SAFETY: as in `set`.
        unsafe { std::env::remove_var("CYPHER_ACP_QUIET_SETTLE_MS") };
    }
}

fn brief(ev: &AgentEvent) -> String {
    match ev {
        AgentEvent::SessionStarted { session_id, .. } => format!("SessionStarted({session_id})"),
        AgentEvent::TextDelta { text } => {
            format!("TextDelta({:?})", text.chars().take(40).collect::<String>())
        }
        AgentEvent::ToolCall { id, .. } => format!("ToolCall({id})"),
        AgentEvent::ToolResult { id, .. } => format!("ToolResult({id})"),
        AgentEvent::AssistantMessageCompleted { .. } => "AssistantMessageCompleted".into(),
        AgentEvent::Done { status, .. } => format!("*** DONE({status:?}) ***"),
        other => {
            let dbg = format!("{other:?}");
            dbg.chars().take(60).collect()
        }
    }
}

#[tokio::test]
#[ignore = "real claude CLI + network; run explicitly for the A/B evidence probe"]
async fn real_claude_quiet_ab_probe() {
    let _serial = serial().await;
    let _knob = QuietSettleKnob::set(AB_QUIET_MS);
    let runs: usize = std::env::var("AB_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let mut clean = 0usize;
    let mut orphaned = 0usize;
    for run in 1..=runs {
        let (dones, after, finished) = ab_probe_once(run == 1).await;
        println!(
            "RUN {run}/{runs} VERDICT: dones={dones} content_events_after_first_done={after} \
             finished_text_contains_PROBE_DONE={finished}"
        );
        if after == 0 && dones == 1 {
            clean += 1;
        } else {
            orphaned += 1;
        }
    }
    println!(
        "SUMMARY: runs={runs} clean={clean} orphaned={orphaned} \
         (clean = one Done, ordered last; orphaned = premature Done, tail after it)"
    );
}

async fn ab_probe_once(print_trace: bool) -> (usize, usize, bool) {
    let (controls, steer_tx, _token) = controls();
    let harness = AcpHarness::claude();
    let req = probe_request(Some("claude-haiku-4-5"));
    let started = std::time::Instant::now();
    let mut stream = harness.run(req, controls).await.expect("run starts");

    let mut events: Vec<(Duration, AgentEvent)> = Vec::new();
    let mut steer = Some(steer_tx);
    let mut first_done_at: Option<std::time::Instant> = None;
    loop {
        let budget = match first_done_at {
            // Post-Done observation: wait out the window, then release the
            // mailbox so the run can end.
            Some(at) => {
                let left = AB_POST_DONE_WINDOW.saturating_sub(at.elapsed());
                if left.is_zero() && steer.is_some() {
                    steer = None;
                }
                left.max(Duration::from_secs(10))
            }
            None => Duration::from_secs(90),
        };
        match tokio::time::timeout(budget, stream.next()).await {
            Ok(Some(ev)) => {
                let ev = ev.expect("stream event");
                if matches!(ev, AgentEvent::Done { .. }) && first_done_at.is_none() {
                    first_done_at = Some(std::time::Instant::now());
                }
                events.push((started.elapsed(), ev));
            }
            Ok(None) => break,
            Err(_) => {
                if steer.is_some() {
                    steer = None; // release mailbox, drain to stream end
                } else {
                    break;
                }
            }
        }
        if events.len() > 400 {
            break;
        }
    }

    if print_trace {
        println!("--- TRACE (quiet knob = {AB_QUIET_MS}ms) ---");
        for (at, ev) in &events {
            println!("{:>8.3}s  {}", at.as_secs_f64(), brief(ev));
        }
    }
    let first_done_idx = events
        .iter()
        .position(|(_, e)| matches!(e, AgentEvent::Done { .. }));
    let dones = events
        .iter()
        .filter(|(_, e)| matches!(e, AgentEvent::Done { .. }))
        .count();
    let after = first_done_idx
        .map(|i| {
            events[i + 1..]
                .iter()
                .filter(|(_, e)| {
                    matches!(
                        e,
                        AgentEvent::TextDelta { .. }
                            | AgentEvent::ToolCall { .. }
                            | AgentEvent::ToolResult { .. }
                    )
                })
                .count()
        })
        .unwrap_or(0);
    let text: String = events
        .iter()
        .filter_map(|(_, e)| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (dones, after, text.contains("PROBE-DONE"))
}

// ---------------------------------------------------------------------------
// Production-settings quiet survey
// ---------------------------------------------------------------------------

/// Production-settings survey across ALL harnesses: does a real multi-tool
/// turn settle exactly once, at the end, and how close do its silent gaps
/// come to the 30s blanket quiet-settle window? Run explicitly:
///
///   SURVEY_RUNS=3 cargo test -p cypher-harness --test real_cli -- --ignored --nocapture real_all_harnesses_quiet_survey
///
/// No env knob is set here — this runs the DEFAULTS the app ships: Claude
/// exempt from the blanket settle, every other adapter on the 30s window.
/// For each installed+authenticated agent CLI it reports, per run: the Done
/// count, content events after the first Done (orphan signature), and the
/// maximum inter-event silent gap — the safety margin against the window.
/// Uninstalled/unauthenticated agents are skipped by name.
const SURVEY_POST_DONE_WINDOW: Duration = Duration::from_secs(20);

struct ProbeOutcome {
    dones: Vec<(DoneStatus, Option<String>)>,
    after_first_done: usize,
    finished: bool,
    max_gap: Duration,
    started_err: Option<String>,
}

async fn survey_probe_once(harness: AcpHarness) -> ProbeOutcome {
    let (controls, steer_tx, _token) = controls();
    let req = probe_request(None);
    let mut stream = match harness.run(req, controls).await {
        Ok(s) => s,
        Err(e) => {
            return ProbeOutcome {
                dones: Vec::new(),
                after_first_done: 0,
                finished: false,
                max_gap: Duration::ZERO,
                started_err: Some(e.to_string()),
            };
        }
    };
    let started = std::time::Instant::now();
    let mut events: Vec<(Duration, AgentEvent)> = Vec::new();
    let mut steer = Some(steer_tx);
    let mut first_done_at: Option<std::time::Instant> = None;
    loop {
        let budget = match first_done_at {
            Some(at) => {
                let left = SURVEY_POST_DONE_WINDOW.saturating_sub(at.elapsed());
                if left.is_zero() && steer.is_some() {
                    steer = None;
                }
                left.max(Duration::from_secs(10))
            }
            None => Duration::from_secs(120),
        };
        match tokio::time::timeout(budget, stream.next()).await {
            Ok(Some(Ok(ev))) => {
                if matches!(ev, AgentEvent::Done { .. }) && first_done_at.is_none() {
                    first_done_at = Some(std::time::Instant::now());
                }
                events.push((started.elapsed(), ev));
            }
            Ok(Some(Err(e))) => {
                events.push((
                    started.elapsed(),
                    AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(e.to_string()),
                        session_id: None,
                    },
                ));
                break;
            }
            Ok(None) => break,
            Err(_) => {
                if steer.is_some() {
                    steer = None;
                } else {
                    break;
                }
            }
        }
        if events.len() > 600 {
            break;
        }
    }
    let first_done_idx = events
        .iter()
        .position(|(_, e)| matches!(e, AgentEvent::Done { .. }));
    // Max silent gap while the turn is live: consecutive-event gaps from the
    // first event through the first Done (post-Done observation excluded).
    let live_end = first_done_idx.unwrap_or(events.len().saturating_sub(1));
    let max_gap = events[..=live_end.min(events.len().saturating_sub(1))]
        .windows(2)
        .map(|w| w[1].0 - w[0].0)
        .max()
        .unwrap_or(Duration::ZERO);
    let after_first_done = first_done_idx
        .map(|i| {
            events[i + 1..]
                .iter()
                .filter(|(_, e)| {
                    matches!(
                        e,
                        AgentEvent::TextDelta { .. }
                            | AgentEvent::ToolCall { .. }
                            | AgentEvent::ToolResult { .. }
                    )
                })
                .count()
        })
        .unwrap_or(0);
    let text: String = events
        .iter()
        .filter_map(|(_, e)| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    ProbeOutcome {
        dones: events
            .iter()
            .filter_map(|(_, e)| match e {
                AgentEvent::Done { status, error, .. } => Some((*status, error.clone())),
                _ => None,
            })
            .collect(),
        after_first_done,
        finished: text.contains("PROBE-DONE"),
        max_gap,
        started_err: None,
    }
}

fn is_auth_or_missing(o: &ProbeOutcome) -> bool {
    let msg = o
        .started_err
        .clone()
        .or_else(|| o.dones.iter().find_map(|(_, e)| e.clone()))
        .unwrap_or_default()
        .to_lowercase();
    msg.contains("auth")
        || msg.contains("login")
        || msg.contains("not installed")
        || msg.contains("not found")
        || msg.contains("no such file")
}

#[tokio::test]
#[ignore = "runs every installed+authenticated agent CLI; costs a few small prompts each"]
async fn real_all_harnesses_quiet_survey() {
    let _serial = serial().await;
    let runs: usize = std::env::var("SURVEY_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    type AgentFactory = (&'static str, fn() -> AcpHarness);
    let agents: Vec<AgentFactory> = vec![
        ("claude", AcpHarness::claude),
        ("codex", AcpHarness::codex),
        ("cursor", AcpHarness::cursor),
        ("grok", AcpHarness::grok),
        ("hermes", AcpHarness::hermes),
    ];
    let mut failures: Vec<String> = Vec::new();
    for (name, ctor) in agents {
        for i in 1..=runs {
            let o = survey_probe_once(ctor()).await;
            if is_auth_or_missing(&o) {
                println!("[{name}] SKIP — not installed / not authenticated");
                break;
            }
            let clean = o.dones.len() == 1
                && o.dones[0].0 == DoneStatus::Completed
                && o.after_first_done == 0
                && o.finished;
            println!(
                "[{name}] run {i}/{runs}: dones={:?} after_first_done={} finished={} \
                 max_live_gap={:.3}s (window margin {:.1}x) → {}",
                o.dones
                    .iter()
                    .map(|(s, _)| format!("{s:?}"))
                    .collect::<Vec<_>>(),
                o.after_first_done,
                o.finished,
                o.max_gap.as_secs_f64(),
                30.0 / o.max_gap.as_secs_f64().max(0.001),
                if clean { "CLEAN" } else { "VIOLATION" }
            );
            if !clean {
                failures.push(format!(
                    "[{name}] run {i}: dones={:?} after={} finished={}",
                    o.dones, o.after_first_done, o.finished
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
