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
//! - a startup crash retries once with the resume kept, and a helper that is
//!   down hard never tombstones the stored session id;
//! - a steer with no live run after a restart dispatches as a new turn that
//!   still resumes the prior conversation.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;

use cypher_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionDoc, SessionMessageEntry,
};
use cypher_engine::{EngineCore, RunJournal};
use cypher_harness::HarnessError;
use cypher_proto::{AgentEvent, DoneStatus, HarnessId, RunRequest};
use cypher_sync::DocsStore;

use common::{complete_assistant_count, entries, message_entry, wait_for};

const CHAT: &str = "chat-restart";

type RequestLog = Arc<Mutex<Vec<RunRequest>>>;

fn run_request(prompt: &str, cwd: &str) -> RunRequest {
    RunRequest {
        cwd: cwd.into(),
        ..common::run_request(prompt)
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

fn assemble(dir: &Path, harness: RecordingHarness) -> EngineCore {
    let RecordingHarness {
        requests,
        session_id,
        fail_starts,
    } = harness;
    let harness = common::TestHarness::new(HarnessId::Mock, "Recording", move |request, _| {
        requests.lock().expect("request log").push(request.clone());
        let fail = {
            let mut left = fail_starts.lock().expect("fail counter");
            if *left > 0 {
                *left -= 1;
                true
            } else {
                false
            }
        };
        if fail {
            return common::script(vec![AgentEvent::Done {
                status: DoneStatus::Errored,
                result: None,
                error: Some("Recording exited unexpectedly (exit code 1): boom".into()),
                session_id: None,
            }]);
        }
        common::reply(
            HarnessId::Mock,
            &request,
            &session_id,
            &format!("ack: {}", request.prompt),
        )
    });
    common::engine_at(dir, harness)
}

/// Manufacture the on-disk state a kill -9 mid-run leaves behind:
/// - a chat doc snapshot whose assistant entry is still `streaming`;
/// - a journal whose last event is NOT `Done` (run died mid-stream), holding
///   the only copy of the harness session id (the debounced workspace-row
///   write never landed).
fn manufacture_crash(dir: &Path, user_at: i64, assistant_at: i64) {
    let store = DocsStore::open(dir.join("orgs/dev-org/dev-user")).unwrap();
    let doc = SessionDoc::init(CHAT).unwrap();
    doc.push_message(&SessionMessageEntry {
        created_at: user_at,
        ..message_entry(
            "msg-user-1",
            MessageRole::User,
            "long task",
            "dev-crash",
            Some(MessageStatus::Complete),
        )
    })
    .unwrap();
    doc.push_message(&SessionMessageEntry {
        created_at: assistant_at,
        ..message_entry(
            "msg-assistant-1",
            MessageRole::Assistant,
            "partial…",
            "dev-crash",
            Some(MessageStatus::Streaming),
        )
    })
    .unwrap();
    store
        .save_snapshot(CHAT, &doc.export_snapshot().unwrap())
        .unwrap();

    let journal = RunJournal::open(dir.join("orgs/dev-org/dev-user/journals")).unwrap();
    journal
        .append(
            CHAT,
            &common::session_started(
                HarnessId::Mock,
                "mock-1",
                "/tmp",
                "hs-crash",
                "msg-assistant-1",
            ),
        )
        .unwrap();
    journal.append(CHAT, &common::text("partial…")).unwrap();
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
        || complete_assistant_count(&core, CHAT) == 1,
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
    let entries = entries(&core, CHAT);
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
        || complete_assistant_count(&core, CHAT) == 2,
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

#[tokio::test]
async fn kill_crash_recovers_resume_from_journal_and_stamps_aborted() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    std::fs::create_dir_all(&dir).unwrap();
    // Pin the device id so the manufactured streaming entry counts as OURS.
    std::fs::write(dir.join("device-id"), "dev-crash").unwrap();

    manufacture_crash(&dir, 1, 2);

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

    // Boot recovery stamped the abandoned streaming entry `aborted`, keeping
    // its partial text …
    let entries = entries(&core, CHAT);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].status, Some(MessageStatus::Aborted));
    match &entries[1].parts[0] {
        MessagePart::Text { text, .. } => assert_eq!(text, "partial…"),
        other => panic!("unexpected part {other:?}"),
    }
    // … closed the stale journal with a synthetic Done{interrupted} …
    let journal = RunJournal::open(dir.join("orgs/dev-org/dev-user/journals")).unwrap();
    assert!(journal.stale_sessions().unwrap().is_empty());
    assert!(matches!(
        journal.last_event(CHAT).unwrap(),
        Some((
            _,
            AgentEvent::Done {
                status: DoneStatus::Interrupted,
                ..
            }
        ))
    ));
    // … and left the session idle.
    assert_eq!(
        common::status(&core, CHAT),
        Some(cypher_proto::SessionStatus::Idle)
    );

    // The next run resumes the crashed conversation: the session id was
    // recovered from the journal (its only surviving home).
    pre_title(&core);
    queue_run(&core, "keep going", "/tmp", "msg-user-2");
    wait_for(
        || complete_assistant_count(&core, CHAT) == 1,
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
async fn persistent_session_serves_multiple_turns_on_one_child() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    std::fs::create_dir_all(&dir).unwrap();

    // A harness whose stream stays OPEN after each turn's Done, serving follow-up
    // turns from the steering mailbox — the persistent-session shape (codex; and
    // claude's stream-json stdin). Counts `run()` calls to prove the engine
    // reuses one child across turns instead of respawning.
    let runs_started = Arc::new(Mutex::new(0usize));
    let started = runs_started.clone();
    let harness = common::TestHarness::new(HarnessId::Mock, "Persistent", move |_, controls| {
        *started.lock().unwrap() += 1;
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(32);
        let mut steering = controls.steering;
        tokio::spawn(async move {
            let turn = |n: usize, prompt: &str| {
                vec![
                    common::text(&format!("turn {n} ack: {prompt}")),
                    common::done("hs-persist"),
                ]
            };
            let first = vec![common::session_started(
                HarnessId::Mock,
                "mock-1",
                "/tmp",
                "hs-persist",
                "a-1",
            )];
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
    })
    .steering();
    let core = common::engine_at(&dir, harness);
    pre_title(&core);

    queue_run(&core, "first", "/tmp", "msg-user-1");
    wait_for(
        || complete_assistant_count(&core, CHAT) == 1,
        "first turn to complete",
    )
    .await;

    // The session PARKS (zeron runsBySession): the second message routes into
    // the live child instead of spawning a new one.
    queue_run(&core, "second", "/tmp", "msg-user-2");
    wait_for(
        || complete_assistant_count(&core, CHAT) == 2,
        "second turn to complete on the same child",
    )
    .await;

    assert_eq!(
        *runs_started.lock().unwrap(),
        1,
        "one harness child must serve both turns"
    );
    let entries = entries(&core, CHAT);
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
async fn fresh_crash_auto_resumes_and_notes_the_interruption() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("device-id"), "dev-crash").unwrap();

    // Same manufactured kill -9 state as above, but FRESH: the streaming entry
    // crashed moments ago, inside the 12h revival window.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    manufacture_crash(&dir, now - 60_000, now - 30_000);

    let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        &dir,
        RecordingHarness {
            requests: requests.clone(),
            session_id: "hs-after-crash".into(),
            fail_starts: Default::default(),
        },
    );

    // The run is PICKED BACK UP without any user action (zeron: "not just
    // eulogized"): recovery re-dispatches the crashed prompt itself.
    wait_for(
        || complete_assistant_count(&core, CHAT) == 1,
        "auto-resumed turn to complete",
    )
    .await;

    let entries = entries(&core, CHAT);
    // The aborted entry SAYS why it ended — and that the run is resuming.
    let aborted = entries
        .iter()
        .find(|e| e.status == Some(MessageStatus::Aborted))
        .expect("crashed entry stays, stamped aborted");
    assert!(
        aborted.parts.iter().any(|p| matches!(
            p,
            MessagePart::Error { message, .. }
                if message.contains("engine restart") && message.contains("resuming")
        )),
        "aborted entry carries the visible interruption note"
    );
    // Re-dispatch reuses the original user message id — never a duplicate.
    assert_eq!(
        entries
            .iter()
            .filter(|e| e.role == MessageRole::User)
            .count(),
        1
    );
    // The revived run continues the journal-recovered harness conversation.
    // (An auto-title request may precede it — titling fires at dispatch.)
    let recorded = requests.lock().unwrap().clone();
    let revived = recorded
        .iter()
        .find(|r| r.prompt == "long task")
        .expect("auto-resumed dispatch reached the harness");
    assert_eq!(
        revived.resume.as_deref(),
        Some("hs-crash"),
        "auto-resume must reattach the crashed harness session"
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
        || complete_assistant_count(&core, CHAT) == 2,
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
async fn startup_crash_retries_once_with_resume_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let requests: RequestLog = Arc::new(Mutex::new(Vec::new()));

    run_one_turn_and_shutdown(&dir, &requests, "hs-live").await;

    // Relaunch with a harness whose child dies at startup ONCE (a transient
    // spawn blip). Since the ACP conversion a stale id falls back inside the
    // harness (`session/load` → `session/new`), so a startup death never
    // indicts the stored id: the retry must carry the SAME session id, not
    // start fresh — and never tombstone it.
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
        || complete_assistant_count(&core, CHAT) == 2,
        "retried turn to complete",
    )
    .await;

    // The crashed attempt, then exactly one retry — resume kept both times.
    {
        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 3, "one crashed attempt + one retry");
        assert_eq!(log[1].resume.as_deref(), Some("hs-live"));
        assert_eq!(
            log[2].resume.as_deref(),
            Some("hs-live"),
            "the retry must keep the stored conversation"
        );
        assert_eq!(log[2].prompt, "second turn");
    }
    // The retry reused the same user entry — no duplicates, no error turn.
    let entries = entries(&core, CHAT);
    let users: Vec<_> = entries
        .iter()
        .filter(|e| e.role == MessageRole::User)
        .collect();
    assert_eq!(users.len(), 2, "retry must not duplicate the user entry");
    assert_eq!(entries.len(), 4, "user+assistant per turn: {entries:#?}");
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
    // Crashed attempt + its single retry — then it must STOP (no spawn loop).
    wait_for(
        || requests.lock().unwrap().len() == 3,
        "crashed attempt and retry to be recorded",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    {
        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 3, "exactly one retry, no revival loop");
        assert_eq!(log[1].resume.as_deref(), Some("hs-live"));
        assert_eq!(log[2].resume.as_deref(), Some("hs-live"));
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
        || complete_assistant_count(&core, CHAT) == 2,
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
