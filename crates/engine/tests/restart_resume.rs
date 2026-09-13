//! Restart round-trip + harness resume continuity (the "chats forget everything
//! after an app restart" regression): the EMBED assembly (`EngineCore::assemble`)
//! is run twice over one data dir, asserting
//! - chats + transcripts survive a graceful shutdown → relaunch;
//! - the next run in an existing chat carries the chat's stored harness-native
//!   session id as `RunRequest.resume` (engine-owned, zeron sessions.ts:736);
//! - a kill -9 style crash recovers the session id from the run journal
//!   (zeron recoverDraft, sessions.ts:538-552) and stamps streaming entries
//!   `aborted`;
//! - resume is cwd-scoped (harness session stores are keyed by cwd);
//! - neither boot recovery nor a startup crash re-dispatches an uncertain
//!   request; the stored session id remains available for explicit continuation;
//! - a steer with no live run after a restart dispatches as a new turn that
//!   still resumes the prior conversation.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry,
};
use cypher_engine::{
    EngineCore, EngineProfile, HarnessRegistry, session_replicas::SessionReplicas,
};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode,
};

const CHAT: &str = "chat-restart";

type RequestLog = Arc<Mutex<Vec<RunRequest>>>;

fn run_request(prompt: &str, cwd: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        pending_attachments: Vec::new(),
        resume: None,
        worktree: None,
    }
}

/// Records every `RunRequest` it receives (the resume-injection probe). A
/// successful run emits `SessionStarted{session_id}` … `Done{session_id}`;
/// while `fail_starts` is positive, a run dies the way a crashed agent child
/// does — an errored Done before any session starts — decrementing the
/// counter (so a transient spawn blip is one failure, a helper that is down
/// hard is `u32::MAX`).
struct RecordingHarness {
    requests: RequestLog,
    session_id: String,
    fail_starts: Arc<Mutex<u32>>,
}

#[async_trait]
impl Harness for RecordingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Recording"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.requests
            .lock()
            .expect("request log")
            .push(request.clone());
        let fail = {
            let mut left = self.fail_starts.lock().expect("fail counter");
            if *left > 0 {
                *left -= 1;
                true
            } else {
                false
            }
        };
        let events: Vec<Result<AgentEvent, HarnessError>> = if fail {
            vec![Ok(AgentEvent::Done {
                status: DoneStatus::Errored,
                result: None,
                error: Some("Recording exited unexpectedly (exit code 1): boom".into()),
                session_id: None,
            })]
        } else {
            vec![
                Ok(AgentEvent::SessionStarted {
                    harness: HarnessId::Mock,
                    model: "mock-1".into(),
                    tools: vec![],
                    cwd: request.cwd.clone(),
                    session_id: self.session_id.clone(),
                    assistant_message_id: "a-1".into(),
                }),
                Ok(AgentEvent::TextDelta {
                    text: format!("ack: {}", request.prompt),
                }),
                Ok(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: Some(self.session_id.clone()),
                }),
            ]
        };
        Ok(futures::stream::iter(events).boxed())
    }
}

fn assemble(dir: &std::path::Path, harness: RecordingHarness) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(harness));
    EngineCore::assemble(dir, Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles")
}

fn queue_run(core: &EngineCore, prompt: &str, cwd: &str, message_id: &str) {
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: run_request(prompt, cwd),
                message_id: message_id.into(),

                agent_prompt: None,
            },
        )
        .expect("queue run command");
}

async fn wait_for<F>(predicate: F, what: &str)
where
    F: FnMut() -> bool,
{
    wait_for_within(predicate, what, Duration::from_secs(10)).await;
}

async fn wait_for_within<F>(mut predicate: F, what: &str, deadline: Duration)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + deadline;
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

/// Tolerant read for hot-polling predicates (mirrors e2e.rs `entries_now`).
fn entries_now(core: &EngineCore) -> Vec<SessionMessageEntry> {
    cypher_doc::join_continuation_entries(
        core.doc_host
            .open(CHAT)
            .ok()
            .and_then(|h| h.doc().read_entries().ok())
            .unwrap_or_default(),
    )
}

fn complete_assistant_count(core: &EngineCore) -> usize {
    entries_now(core)
        .iter()
        .filter(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
        .count()
}

fn stored_harness_session(core: &EngineCore) -> Option<(String, Option<String>)> {
    let chat = core
        .workspace
        .chat(CHAT)
        .expect("read chat row")
        .expect("chat row exists");
    chat.harness_session_id
        .map(|id| (id, chat.harness_session_cwd))
}

/// Create + name the chat row up front so the auto-titler (which runs its own
/// harness request after a completed exchange on an UNTITLED chat) stays out
/// of the recorded request log.
fn pre_title(core: &EngineCore) {
    core.workspace
        .create_space("space-restart", &core.device_id, "/tmp", None, false)
        .expect("create space row");
    core.workspace
        .create_chat(CHAT, Some("space-restart"), None, None, None)
        .expect("create chat row");
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");
}

/// One full turn in a fresh engine over `dir`, then graceful shutdown — the
/// "before restart" phase shared by the tests below.
async fn run_one_turn_and_shutdown(dir: &std::path::Path, requests: &RequestLog, session: &str) {
    let core = assemble(
        dir,
        RecordingHarness {
            requests: requests.clone(),
            session_id: session.into(),
            fail_starts: Default::default(),
        },
    );
    pre_title(&core);
    queue_run(
        &core,
        "remember the codeword PINEAPPLE",
        "/tmp",
        "msg-user-1",
    );
    wait_for(
        || complete_assistant_count(&core) == 1,
        "first turn to complete",
    )
    .await;
    core.shutdown().await;
    drop(core);
}

#[tokio::test]
async fn restart_roundtrip_restores_chats_transcript_and_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));

    run_one_turn_and_shutdown(&dir, &requests, "hs-restart-1").await;
    assert_eq!(
        requests.lock().unwrap()[0].resume,
        None,
        "a chat's first run must start a fresh harness session"
    );

    // Relaunch over the same data dir (the embedded-engine restart path).
    let core = assemble(
        &dir,
        RecordingHarness {
            requests: requests.clone(),
            session_id: "hs-restart-2".into(),
            fail_starts: Default::default(),
        },
    );

    // Sidebar state survived: the chat row is back with its cwd, preview, and
    // the stored harness session (cwd-scoped).
    let chats = core.workspace.read_chats().expect("read chats");
    assert_eq!(chats.len(), 1, "chat row survives restart: {chats:#?}");
    assert_eq!(chats[0].id, CHAT);
    assert_eq!(chats[0].cwd.as_deref(), Some("/tmp"));
    assert!(chats[0].last_message_preview.is_some());
    assert_eq!(
        stored_harness_session(&core),
        Some(("hs-restart-1".into(), Some("/tmp".into())))
    );

    // Transcript survived: user + completed assistant entry, texts intact.
    let entries = entries_now(&core);
    assert_eq!(
        entries.len(),
        2,
        "transcript survives restart: {entries:#?}"
    );
    assert_eq!(entries[0].id, "msg-user-1");
    assert_eq!(entries[0].role, MessageRole::User);
    assert_eq!(entries[1].role, MessageRole::Assistant);
    assert_eq!(entries[1].status, Some(MessageStatus::Complete));
    assert!(matches!(
        &entries[1].parts[0],
        MessagePart::Text { text, .. } if text.contains("PINEAPPLE")
    ));

    // The next run resumes the SAME harness conversation: the engine injects
    // the stored session id even though the caller sent `resume: None`.
    queue_run(&core, "what was the codeword?", "/tmp", "msg-user-2");
    wait_for(
        || complete_assistant_count(&core) == 2,
        "second turn to complete",
    )
    .await;
    {
        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(
            log[1].resume.as_deref(),
            Some("hs-restart-1"),
            "post-restart dispatch must resume the stored harness session"
        );
    }
    // The fresh turn's session id replaces the stored one.
    assert_eq!(
        stored_harness_session(&core),
        Some(("hs-restart-2".into(), Some("/tmp".into())))
    );
    core.shutdown().await;
}

/// Manufacture actual private v3 SQLite state at the boundary after source
/// retention/publication but before a Done, with no engine/workspace row save.
async fn seed_native_crash(dir: &std::path::Path, created_at: i64, observed: bool) {
    let replicas = SessionReplicas::new(
        &EngineProfile::development(dir, "dev-org", "dev-user"),
        "dev-crash".into(),
        None,
    )
    .unwrap();
    let mut output = replicas
        .prepare(
            CHAT,
            SessionCommandPayload::Run {
                message_id: "msg-user-1".into(),
                agent_prompt: None,
                request: run_request("long task", "/tmp"),
            },
            "msg-assistant-1",
        )
        .await
        .unwrap();
    output.set_cwd("/tmp").unwrap();
    let occupancy = if observed {
        Some(output.occupy().await.unwrap())
    } else {
        None
    };
    let user = SessionMessageEntry {
        id: "msg-user-1".into(),
        role: MessageRole::User,
        device_id: "dev-crash".into(),
        created_at: created_at - 1,
        status: Some(MessageStatus::Complete),
        continuation_of: None,
        parts: vec![MessagePart::Text {
            id: "t0".into(),
            text: "long task".into(),
        }],
    };
    let replica = output.replica();
    let mut writer = replica.read(|j| j.new_writer(1, None, &user)).unwrap();
    while writer
        .finish(&user.parts, user.status, |frame| {
            replica.write(|j| j.enqueue_writer_frame(frame))
        })
        .unwrap()
        .more
    {}
    output
        .observe(&AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "mock-1".into(),
            tools: vec![],
            cwd: "/tmp".into(),
            session_id: "hs-crash".into(),
            assistant_message_id: "msg-assistant-1".into(),
        })
        .unwrap();
    output
        .observe(&AgentEvent::TextDelta {
            text: "partial…".into(),
        })
        .unwrap();
    if let Some(occupancy) = occupancy {
        output
            .observe(&AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            })
            .unwrap();
        output
            .finish(cypher_proto::sync3::Outcome::Completed)
            .unwrap();
        output
            .retain(&AgentEvent::TextDelta {
                text: "background partial".into(),
            })
            .unwrap();
        output
            .begin_observation(&occupancy, "background-reply".into())
            .await
            .unwrap();
        output
            .sync_parts(&[MessagePart::Text {
                id: "t0".into(),
                text: "background partial".into(),
            }])
            .unwrap();
    }
    drop(output); // intentionally no completion, quarantine or synthetic Done
    replicas.shutdown().await;
}

#[tokio::test]
async fn kill_crash_recovers_resume_from_journal_and_stamps_aborted() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    std::fs::create_dir_all(&dir).unwrap();
    // Pin the device id so the manufactured streaming entry counts as OURS.
    std::fs::write(dir.join("device-id"), "dev-crash").unwrap();

    seed_native_crash(&dir, 2, false).await;

    let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        &dir,
        RecordingHarness {
            requests: requests.clone(),
            session_id: "hs-after-crash".into(),
            fail_starts: Default::default(),
        },
    );
    assert_eq!(core.device_id, "dev-crash");

    wait_for(
        || {
            entries_now(&core)
                .iter()
                .any(|e| e.status == Some(MessageStatus::Aborted))
        },
        "native crash quarantine",
    )
    .await;
    // Recovery closes the public message without falsifying harness source.
    let entries = entries_now(&core);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].status, Some(MessageStatus::Aborted));
    let replica = core.sessions.session_replica(CHAT).await.unwrap();
    let command = replica
        .read(|j| Ok(j.projection()?.commands.keys().next().unwrap().clone()))
        .unwrap();
    let raw = replica
        .read(|j| j.execution_events(&command, 0, 32))
        .unwrap();
    assert!(matches!(
        raw.events.last().unwrap().event,
        AgentEvent::TextDelta { .. }
    ));

    // No workspace-row write existed: native source is the sole surviving
    // home of the provider session reference.
    pre_title(&core);
    queue_run(&core, "keep going", "/tmp", "msg-user-2");
    wait_for(
        || complete_assistant_count(&core) == 1,
        "post-crash turn to complete",
    )
    .await;
    assert_eq!(
        requests.lock().unwrap()[0].resume.as_deref(),
        Some("hs-crash"),
        "journal-recovered session id must ride the next dispatch"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn orphaned_autonomous_run_is_quarantined_without_a_new_command_or_process_release() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("device-id"), "dev-crash").unwrap();
    seed_native_crash(dir.path(), 2, true).await;
    let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        dir.path(),
        RecordingHarness {
            requests: requests.clone(),
            session_id: "must-not-run".into(),
            fail_starts: Default::default(),
        },
    );
    wait_for(
        || {
            entries_now(&core)
                .iter()
                .any(|e| e.id == "background-reply" && e.status == Some(MessageStatus::Aborted))
        },
        "autonomous crash quarantine",
    )
    .await;
    assert!(requests.lock().unwrap().is_empty());
    let replica = core.sessions.session_replica(CHAT).await.unwrap();
    let p = replica.read(|j| j.projection()).unwrap();
    assert_eq!(p.commands.len(), 1);
    assert_eq!(
        p.commands.values().next().unwrap().command.status,
        cypher_proto::SessionCommandStatus::Applied
    );
    assert_eq!(p.runs.len(), 2);
    assert_eq!(
        p.runs
            .values()
            .filter(|r| r.outcome == Some(cypher_proto::sync3::Outcome::Failed))
            .count(),
        1
    );
    assert!(
        !p.executions.values().next().unwrap().closed,
        "unknown persistent-process closure must not be invented"
    );
    assert!(
        replica
            .read(|j| j.open_observations("", 32))
            .unwrap()
            .is_empty()
    );
    let raw = replica
        .read(|j| j.execution_events(p.commands.keys().next().unwrap(), 0, 32))
        .unwrap();
    assert!(
        matches!(&raw.events.last().unwrap().event, AgentEvent::TextDelta { text } if text == "background partial")
    );
    let joined = entries_now(&core);
    assert!(joined.iter().find(|e| e.id == "background-reply").unwrap().parts.iter()
        .any(|p| matches!(p, MessagePart::Error { message, .. } if message.contains("No new command was dispatched"))));
    core.shutdown().await;
}

/// A harness whose stream stays OPEN after each turn's Done, serving follow-up
/// turns from the steering mailbox — the persistent-session shape (codex; and
/// claude's stream-json stdin). Counts `run()` calls to prove the engine
/// reuses one child across turns instead of respawning.
struct PersistentHarness {
    runs_started: Arc<Mutex<usize>>,
}

#[async_trait]
impl Harness for PersistentHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Persistent"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        _request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        *self.runs_started.lock().unwrap() += 1;
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(32);
        let mut steering = controls.steering;
        tokio::spawn(async move {
            let turn = |n: usize, prompt: &str| {
                vec![
                    AgentEvent::TextDelta {
                        text: format!("turn {n} ack: {prompt}"),
                    },
                    AgentEvent::Done {
                        status: DoneStatus::Completed,
                        result: None,
                        error: None,
                        session_id: Some("hs-persist".into()),
                    },
                ]
            };
            let first = vec![AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: vec![],
                cwd: "/tmp".into(),
                session_id: "hs-persist".into(),
                assistant_message_id: "a-1".into(),
            }];
            for ev in first.into_iter().chain(turn(1, "first")) {
                if tx.send(Ok(ev)).await.is_err() {
                    return;
                }
            }
            // Parked: serve follow-up turns from the mailbox until the
            // engine hangs up (idle reap / interrupt / shutdown).
            let mut n = 1usize;
            while let Some(steer) = steering.recv().await {
                n += 1;
                let boundary = AgentEvent::Steered {
                    assistant_message_id: None,
                    next_assistant_message_id: steer.message_id.map(|id| format!("a-{id}")),
                };
                if tx.send(Ok(boundary)).await.is_err() {
                    return;
                }
                for ev in turn(n, &steer.prompt) {
                    if tx.send(Ok(ev)).await.is_err() {
                        return;
                    }
                }
            }
        });
        Ok(futures::stream::unfold(
            rx,
            |mut rx| async move { rx.recv().await.map(|ev| (ev, rx)) },
        )
        .boxed())
    }
}

#[tokio::test]
async fn persistent_session_serves_multiple_turns_on_one_child() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    std::fs::create_dir_all(&dir).unwrap();

    let runs_started = Arc::new(Mutex::new(0usize));
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(PersistentHarness {
        runs_started: runs_started.clone(),
    }));
    let core = EngineCore::assemble(&dir, Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles");
    pre_title(&core);

    queue_run(&core, "first", "/tmp", "msg-user-1");
    wait_for(
        || complete_assistant_count(&core) == 1,
        "first turn to complete",
    )
    .await;

    // The session PARKS (zeron runsBySession): the second message routes into
    // the live child instead of spawning a new one.
    queue_run(&core, "second", "/tmp", "msg-user-2");
    wait_for(
        || complete_assistant_count(&core) == 2,
        "second turn to complete on the same child",
    )
    .await;

    assert_eq!(
        *runs_started.lock().unwrap(),
        1,
        "one harness child must serve both turns"
    );
    let entries = entries_now(&core);
    assert_eq!(
        entries
            .iter()
            .filter(|e| e.role == MessageRole::User)
            .count(),
        2
    );
    core.shutdown().await;
}

#[tokio::test]
async fn fresh_crash_requires_review_and_preserves_explicit_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("device-id"), "dev-crash").unwrap();

    // Same manufactured kill -9 state as above, but FRESH: the streaming entry
    // crashed moments ago. Freshness is not evidence of unexecuted work.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    seed_native_crash(&dir, now - 30_000, false).await;

    let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        &dir,
        RecordingHarness {
            requests: requests.clone(),
            session_id: "hs-after-crash".into(),
            fail_starts: Default::default(),
        },
    );

    wait_for(
        || {
            entries_now(&core)
                .iter()
                .any(|e| e.status == Some(MessageStatus::Aborted))
        },
        "native crash quarantine",
    )
    .await;
    assert_eq!(complete_assistant_count(&core), 0);
    assert!(
        requests.lock().unwrap().is_empty(),
        "boot must not launch a harness"
    );

    let entries = entries_now(&core);
    // The original output and user identity stay, with explicit uncertainty.
    let aborted = entries
        .iter()
        .find(|e| e.status == Some(MessageStatus::Aborted))
        .expect("crashed entry stays, stamped aborted");
    assert!(
        aborted.parts.iter().any(|p| matches!(
            p,
            MessagePart::Error { message, .. }
                if message.contains("engine restart") && message.contains("not automatically retried")
        )),
        "aborted entry carries the visible interruption note"
    );
    assert!(
        aborted
            .parts
            .iter()
            .any(|p| matches!(p, MessagePart::Text { text, .. } if text == "partial…"))
    );
    assert_eq!(
        entries
            .iter()
            .filter(|e| e.role == MessageRole::User)
            .count(),
        1
    );
    // Only an explicit new request may continue the preserved conversation.
    queue_run(&core, "reviewed effects; continue", "/tmp", "msg-reviewed");
    wait_for(
        || complete_assistant_count(&core) == 1,
        "explicit continuation",
    )
    .await;
    let recorded = requests.lock().unwrap().clone();
    assert!(
        !recorded.iter().any(|r| r.prompt == "long task"),
        "the crashed request was never resent"
    );
    let resumed = recorded
        .iter()
        .find(|r| r.prompt == "reviewed effects; continue")
        .expect("explicit continuation reached the harness");
    assert_eq!(
        resumed.resume.as_deref(),
        Some("hs-crash"),
        "explicit continuation retains the crashed harness session"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn resume_is_cwd_scoped() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));

    run_one_turn_and_shutdown(&dir, &requests, "hs-cwd-1").await;

    let core = assemble(
        &dir,
        RecordingHarness {
            requests: requests.clone(),
            session_id: "hs-cwd-2".into(),
            fail_starts: Default::default(),
        },
    );
    // Same chat, different launch directory: claude session stores are keyed
    // by cwd, so the stored id must NOT be injected.
    queue_run(
        &core,
        "now from another project",
        "/elsewhere",
        "msg-user-2",
    );
    wait_for(
        || complete_assistant_count(&core) == 2,
        "cross-cwd turn to complete",
    )
    .await;
    assert_eq!(
        requests.lock().unwrap()[1].resume,
        None,
        "a session created under /tmp must not resume from /elsewhere"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn startup_crash_is_not_retried_but_explicit_continuation_keeps_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));

    run_one_turn_and_shutdown(&dir, &requests, "hs-live").await;

    // Relaunch with a harness whose child dies at startup ONCE (a transient
    // spawn blip). No SessionStarted does not prove no external effects.
    let core = assemble(
        &dir,
        RecordingHarness {
            requests: requests.clone(),
            session_id: "hs-next".into(),
            fail_starts: Arc::new(Mutex::new(1)),
        },
    );
    queue_run(&core, "second turn", "/tmp", "msg-user-2");
    wait_for(
        || {
            core.sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == cypher_proto::SessionStatus::Errored)
        },
        "failed turn to settle without a retry",
    )
    .await;

    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "only the explicit second turn ran"
    );
    assert_eq!(
        stored_harness_session(&core),
        Some(("hs-live".into(), Some("/tmp".into())))
    );
    assert!(
        entries_now(&core)
            .iter()
            .any(|entry| entry.parts.iter().any(|p| matches!(
                p, MessagePart::Error { message, .. } if message.contains("exited unexpectedly")
            ))),
        "the original failure must remain visible"
    );
    queue_run(&core, "reviewed; third turn", "/tmp", "msg-user-3");
    wait_for(
        || {
            entries_now(&core)
                .iter()
                .filter(|e| {
                    e.role == MessageRole::Assistant
                        && matches!(
                            e.status,
                            Some(MessageStatus::Complete | MessageStatus::Aborted)
                        )
                })
                .count()
                == 3
        },
        "explicit continuation completes",
    )
    .await;
    {
        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 3, "one harness call per explicit request");
        assert_eq!(log[1].resume.as_deref(), Some("hs-live"));
        assert_eq!(
            log[2].resume.as_deref(),
            Some("hs-live"),
            "explicit continuation must keep the stored conversation"
        );
        assert_eq!(log[2].prompt, "reviewed; third turn");
    }
    // The failed turn stays; a new user action has its own identity.
    let entries = entries_now(&core);
    let users: Vec<_> = entries
        .iter()
        .filter(|e| e.role == MessageRole::User)
        .collect();
    assert_eq!(users.len(), 3);
    assert_eq!(
        entries.len(),
        6,
        "user+assistant per explicit turn: {entries:#?}"
    );
    // The successful turn's session id replaces the stored one as usual.
    assert_eq!(
        stored_harness_session(&core),
        Some(("hs-next".into(), Some("/tmp".into())))
    );
    core.shutdown().await;
}

#[tokio::test]
async fn persistent_startup_crash_keeps_stored_session_id() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));

    run_one_turn_and_shutdown(&dir, &requests, "hs-live").await;

    // A helper that is down hard: every spawn dies at startup. The old guess
    // logic read this as "bad resume id" and tombstoned a perfectly good
    // session — permanently, surviving restarts (user incident 2026-08-13).
    let core = assemble(
        &dir,
        RecordingHarness {
            requests: requests.clone(),
            session_id: "hs-unused".into(),
            fail_starts: Arc::new(Mutex::new(u32::MAX)),
        },
    );
    queue_run(&core, "second turn", "/tmp", "msg-user-2");
    // One failed invocation; a broken helper cannot cause an automatic retry.
    wait_for(
        || {
            core.sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == cypher_proto::SessionStatus::Errored)
        },
        "crashed attempt to settle",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    {
        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 2, "no automatic retry or revival loop");
        assert_eq!(log[1].resume.as_deref(), Some("hs-live"));
    }
    // THE fix: the stored id survives the startup failures — the next send
    // (say, after the restart that heals the helper) resumes the same
    // conversation.
    assert_eq!(
        stored_harness_session(&core),
        Some(("hs-live".into(), Some("/tmp".into())))
    );
    core.shutdown().await;
}

/// Real-CLI proof of the whole regression fix: tell claude a codeword, restart
/// the engine (fresh `EngineCore::assemble` over the same data dir), ask for
/// the codeword back — the reply can only contain it if the second run resumed
/// the first run's harness session. Ignored by default: needs an installed,
/// authenticated `claude` CLI and spends real tokens (haiku, two tiny turns).
/// Run with: `cargo test -p cypher-engine --test restart_resume -- --ignored`
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires installed+authenticated claude CLI; spends tokens"]
async fn real_claude_remembers_codeword_across_engine_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.to_string_lossy().to_string();

    let real_request = |prompt: &str| RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: Some("haiku".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.clone(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        attachments: Vec::new(),
        pending_attachments: Vec::new(),
        resume: None,
        worktree: None,
    };
    let assemble_real = || {
        EngineCore::assemble(
            &dir,
            Arc::new(cypher_engine::default_registry(dir.join("agent-sessions"))),
            HarnessId::ClaudeCode,
            None,
        )
        .expect("engine core assembles")
    };

    let core = assemble_real();
    pre_title(&core); // keep the auto-titler from spending a second model call
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: real_request(
                    "Remember the codeword: PINEAPPLE. Reply with exactly: stored",
                ),
                message_id: "msg-user-1".into(),

                agent_prompt: None,
            },
        )
        .expect("queue first real run");
    wait_for_within(
        || complete_assistant_count(&core) == 1,
        "first real claude turn",
        Duration::from_secs(120),
    )
    .await;
    assert!(
        stored_harness_session(&core).is_some(),
        "claude session id must be stored on the chat row"
    );
    core.shutdown().await;
    drop(core);

    // "App restart": a brand-new engine over the same data dir.
    let core = assemble_real();
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: real_request(
                    "What was the codeword I told you earlier? Reply with just the codeword.",
                ),
                message_id: "msg-user-2".into(),

                agent_prompt: None,
            },
        )
        .expect("queue second real run");
    wait_for_within(
        || complete_assistant_count(&core) == 2,
        "post-restart real claude turn",
        Duration::from_secs(120),
    )
    .await;

    let entries = entries_now(&core);
    let last_assistant_text: String = entries
        .iter()
        .rev()
        .find(|e| e.role == MessageRole::Assistant)
        .into_iter()
        .flat_map(|e| {
            e.parts.iter().filter_map(|p| match p {
                MessagePart::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
        })
        .collect();
    assert!(
        last_assistant_text.to_uppercase().contains("PINEAPPLE"),
        "post-restart reply must recall the codeword (got: {last_assistant_text:?})"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn steer_after_restart_dispatches_new_turn_with_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));

    run_one_turn_and_shutdown(&dir, &requests, "hs-steer").await;

    // Relaunch: no live run, no in-process `last_request`. A steer must fall
    // back to a new turn built from the chat's workspace row, resuming the
    // prior harness conversation.
    let core = assemble(
        &dir,
        RecordingHarness {
            requests: requests.clone(),
            session_id: "hs-steer-2".into(),
            fail_starts: Default::default(),
        },
    );
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Steer {
                prompt: "actually, also add tests".into(),
                message_id: Some("msg-user-2".into()),

                agent_prompt: None,
            },
        )
        .expect("queue steer command");
    wait_for(
        || complete_assistant_count(&core) == 2,
        "steer-as-new-turn to complete",
    )
    .await;

    {
        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[1].prompt, "actually, also add tests");
        assert_eq!(log[1].cwd, "/tmp", "run config rebuilt from the chat row");
        assert_eq!(
            log[1].resume.as_deref(),
            Some("hs-steer"),
            "steer-turned-run must resume the stored harness session"
        );
    }
    core.shutdown().await;
}
