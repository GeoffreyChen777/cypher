//! A run that ends or is lost without a live owner never leaves a question
//! that can be neither answered nor stopped (the owner's chat bc811d09,
//! desktop 0.4.9: an escalate prompt on an assistant entry still
//! `streaming` with no run behind it; every answer was rejected "no pending
//! input request", every Stop applied as a no-op, and Needs you showed the
//! question for good).
//!
//! The rule under test (`session::engine::questions`): an unresolved input
//! part with no live run behind it — on a settled entry, or on a streaming
//! entry no run is writing any more — is open and answered as the next
//! turn; Stop with no run closes it. Every path that ends or loses a run
//! stamps its streaming entry settled:
//! - a quit cut short after the run's `Done` reached the journal but before
//!   the doc stored the settle (boot recovery, whatever the journal says);
//! - kill -9 mid-question (stale journal; `restart_resume.rs` e1 too);
//! - graceful engine stop, plan stop (the `Interrupt` command), a harness
//!   child dying, and a Runtime replace while the run asks.

use crate::support;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;

use cypher_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionCommandStatus,
    SessionDoc, SessionMessageEntry,
};
use cypher_engine::{EngineCore, RunJournal};
use cypher_harness::HarnessError;
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, PermissionPrompt, PermissionPromptKind, RunRequest,
    SessionAsk, SessionStatus, UserInputAnswer, UserInputQuestion,
};
use cypher_sync::DocsStore;

use support::{entries, message_entry, wait_for};

const CHAT: &str = "chat-lost-run";
const DEVICE: &str = "dev-lost";
const REQUEST: &str = "req-escalate";

type RequestLog = Arc<Mutex<Vec<RunRequest>>>;

fn escalate_question() -> UserInputQuestion {
    UserInputQuestion {
        id: "q1".into(),
        header: "Run this command?".into(),
        question: "Run `rm -rf build` outside the sandbox?".into(),
        options: vec!["Allow".into(), "Deny and stop".into()],
        multi_select: false,
        permission: Some(PermissionPrompt {
            kind: PermissionPromptKind::Escalate,
            tool_call_id: None,
        }),
        asked_by: None,
    }
}

fn answer(label: &str) -> Vec<UserInputAnswer> {
    vec![UserInputAnswer {
        question_id: "q1".into(),
        labels: vec![label.into()],
    }]
}

/// How the asking run behaves once its question is up.
#[derive(Clone, Copy, PartialEq)]
enum OnAsk {
    /// Wait for the answer, then finish the turn with it.
    Wait,
    /// The agent child dies while the question is up (stream error).
    Die,
}

/// Records every request. A prompt starting "ask" streams some text and asks
/// the escalate question; an answer finishes the turn, an empty answer (an
/// interrupt failing the resolver) waits for the token and ends the stream
/// without a Done, like a torn-down CLI. Any other prompt (the dead-run
/// answer fallback's) completes at once.
fn harness(requests: RequestLog, on_ask: OnAsk) -> support::TestHarness {
    support::TestHarness::new(HarnessId::Mock, "Escalating", move |request, controls| {
        requests.lock().unwrap().push(request.clone());
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(16);
        let asking = request.prompt.starts_with("ask");
        let prompt = request.prompt.clone();
        tokio::spawn(async move {
            if !asking {
                let _ = tx
                    .send(Ok(support::text(&format!("resumed: {prompt}"))))
                    .await;
                let _ = tx
                    .send(Ok(support::done_with(DoneStatus::Completed, Some("hs-1"))))
                    .await;
                return;
            }
            let _ = tx.send(Ok(support::text("checking…"))).await;
            let answer = (controls.request_input)(vec![escalate_question()]);
            if on_ask == OnAsk::Die {
                // Give the question time to reach the doc, then die.
                tokio::time::sleep(Duration::from_millis(2000)).await;
                let _ = tx
                    .send(Err(HarnessError::Protocol("agent child exited".into())))
                    .await;
                drop(answer);
                return;
            }
            let answers = answer.await.unwrap_or_default();
            match answers.first().and_then(|a| a.labels.first().cloned()) {
                Some(picked) => {
                    let _ = tx
                        .send(Ok(support::text(&format!(" picked {picked}"))))
                        .await;
                    let _ = tx
                        .send(Ok(support::done_with(DoneStatus::Completed, Some("hs-1"))))
                        .await;
                }
                None => {
                    controls.interrupt.cancelled().await;
                    drop(tx);
                }
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    })
    .reasoning(&[])
}

fn engine(dir: &Path, requests: &RequestLog, on_ask: OnAsk) -> EngineCore {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("device-id"), DEVICE).unwrap();
    let core = support::engine_at(dir, harness(requests.clone(), on_ask));
    assert_eq!(core.device_id, DEVICE);
    core
}

/// The chat row the dead-run fallback rebuilds its run from.
fn chat_row(core: &EngineCore) {
    if core.workspace.chat(CHAT).ok().flatten().is_none() {
        core.workspace
            .create_chat(CHAT, None, Some(DEVICE), None, Some("/tmp".into()))
            .expect("chat row");
        core.workspace
            .rename_chat(CHAT, "Pre-titled")
            .expect("title the chat");
    }
}

/// The stuck doc: the user's prompt, then the assistant entry still
/// `streaming` with the escalate question open on it.
fn stuck_entries() -> [SessionMessageEntry; 2] {
    let user = message_entry(
        "msg-user-1",
        MessageRole::User,
        "ask before deleting",
        DEVICE,
        Some(MessageStatus::Complete),
    );
    let mut asking = message_entry(
        "msg-assistant-1",
        MessageRole::Assistant,
        "checking…",
        DEVICE,
        Some(MessageStatus::Streaming),
    );
    asking.created_at = 2;
    asking.parts.push(MessagePart::Input {
        id: "0c4aa55e".into(),
        request_id: REQUEST.into(),
        questions: vec![escalate_question()],
        resolved: false,
    });
    [user, asking]
}

/// The row the previous engine left: awaiting input on the question.
fn asking_row() -> cypher_proto::Session {
    cypher_proto::Session {
        chat_id: CHAT.into(),
        device_id: DEVICE.into(),
        status: SessionStatus::AwaitingInput,
        started_at: None,
        updated_at: chrono::Utc::now() - chrono::Duration::minutes(30),
        subagents: Vec::new(),
        context_usage: None,
        throughput: None,
        recovery: None,
        progress: None,
        notice: None,
        asking: Some(SessionAsk {
            request_id: REQUEST.into(),
            asked_at: 2,
            permission: Some(PermissionPromptKind::Escalate),
            asked_by: None,
            from_chat_id: None,
        }),
    }
}

/// On-disk state of a desktop quit cut short (the app gives its quit hook
/// 200 ms): the run settled — its `Done{interrupted}` is in the journal —
/// but the doc snapshot still holds the streaming entry and the registry
/// row still says it asks.
async fn manufacture_quit_cut_short(dir: &Path) {
    let requests: RequestLog = Default::default();
    // A first engine writes the row the quit left behind.
    let core = engine(dir, &requests, OnAsk::Wait);
    chat_row(&core);
    core.workspace.record_session(&asking_row());
    core.shutdown().await;
    drop(core);

    let store = DocsStore::open(dir.join("orgs/dev-org/dev-user")).unwrap();
    let doc = SessionDoc::init(CHAT).unwrap();
    for entry in stuck_entries() {
        doc.push_message(&entry).unwrap();
    }
    store
        .save_snapshot(CHAT, &doc.export_snapshot().unwrap())
        .unwrap();
    let journal = RunJournal::open(dir.join("orgs/dev-org/dev-user/journals")).unwrap();
    for event in [
        support::session_started(HarnessId::Mock, "mock-1", "/tmp", "hs-1", "msg-assistant-1"),
        AgentEvent::InputRequested {
            request_id: REQUEST.into(),
            questions: vec![escalate_question()],
        },
        support::done_with(DoneStatus::Interrupted, None),
    ] {
        journal.append(CHAT, &event).unwrap();
    }
    assert!(
        journal.stale_sessions().unwrap().is_empty(),
        "the journal is closed: the journal sweep never sees this chat"
    );
}

/// The live-engine version of the stuck state: the doc entries pushed
/// straight into the open doc, no run, no row asking — nothing at boot
/// touched it.
fn push_stuck_doc(core: &EngineCore) {
    chat_row(core);
    let handle = core.doc_host.open(CHAT).unwrap();
    for entry in stuck_entries() {
        handle.doc().push_message(&entry).unwrap();
    }
    assert!(!core.sessions.has_run(CHAT));
}

fn queue(core: &EngineCore, id: &str, payload: SessionCommandPayload) {
    let handle = core.doc_host.open(CHAT).unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    handle
        .doc()
        .queue_command(&cypher_doc::SessionCommandEntry {
            id: id.into(),
            payload,
            issued_by: "viewer-device".into(),
            issued_at: now,
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Pending,
            resolution: None,
            sent_at: None,
            dismissed: false,
            author: None,
        })
        .expect("queue command");
}

async fn outcome(core: &EngineCore, id: &str) -> (SessionCommandStatus, Option<String>) {
    let mut found = None;
    wait_for(
        || {
            found = support::command_statuses(core, CHAT)
                .into_iter()
                .find(|(cid, status, _)| cid == id && *status != SessionCommandStatus::Pending)
                .map(|(_, status, resolution)| (status, resolution));
            found.is_some()
        },
        &format!("command {id} to be processed"),
    )
    .await;
    found.unwrap()
}

fn respond(core: &EngineCore, id: &str, request_id: &str, label: &str) {
    queue(
        core,
        id,
        SessionCommandPayload::RespondInput {
            request_id: request_id.into(),
            answers: answer(label),
        },
    );
}

fn stop(core: &EngineCore, id: &str) {
    queue(core, id, SessionCommandPayload::Interrupt {});
}

/// `(status, resolved)` of the request's input part, if the doc has it.
fn question_state(core: &EngineCore, request_id: &str) -> Option<(Option<MessageStatus>, bool)> {
    entries(core, CHAT).iter().find_map(|e| {
        e.parts.iter().find_map(|p| match p {
            MessagePart::Input {
                request_id: rid,
                resolved,
                ..
            } if rid == request_id => Some((e.status, *resolved)),
            _ => None,
        })
    })
}

fn streaming_entries(core: &EngineCore) -> usize {
    entries(core, CHAT)
        .iter()
        .filter(|e| e.status == Some(MessageStatus::Streaming))
        .count()
}

fn asking(core: &EngineCore) -> Option<String> {
    core.sessions
        .session_status(CHAT)
        .and_then(|s| s.asking)
        .map(|a| a.request_id)
}

/// The live question's request id, once the asking run has put it up.
async fn live_question(core: &EngineCore) -> String {
    let mut found = None;
    wait_for(
        || {
            found = entries(core, CHAT).iter().find_map(|e| {
                e.parts.iter().find_map(|p| match p {
                    MessagePart::Input {
                        request_id,
                        resolved: false,
                        ..
                    } if e.status == Some(MessageStatus::Streaming) => Some(request_id.clone()),
                    _ => None,
                })
            });
            found.is_some()
                && support::status(core, CHAT) == Some(SessionStatus::AwaitingInput)
                && asking(core) == found
        },
        "the run to ask its question",
    )
    .await;
    found.unwrap()
}

fn start_asking_run(core: &EngineCore) {
    chat_row(core);
    queue(
        core,
        "cmd-run",
        SessionCommandPayload::Run {
            request: support::run_request("ask before deleting"),
            message_id: "msg-user-1".into(),
            agent_prompt: None,
        },
    );
}

// --- lost runs: no run behind the question -------------------------------

/// The owner's case, from disk: a quit cut short leaves a closed journal
/// over a doc whose entry still streams. Boot recovery settles it whatever
/// the journal says; the question stays open and answerable, and the
/// answer takes effect as the next turn.
#[tokio::test(flavor = "multi_thread")]
async fn a_quit_cut_short_is_settled_on_boot_and_its_question_answered() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    manufacture_quit_cut_short(&dir).await;

    let requests: RequestLog = Default::default();
    let core = engine(&dir, &requests, OnAsk::Wait);
    assert_eq!(streaming_entries(&core), 0, "boot settled the lost run");
    assert_eq!(
        question_state(&core, REQUEST),
        Some((Some(MessageStatus::Aborted), false)),
        "the question is still open, on the aborted entry"
    );
    let row = core.sessions.session_status(CHAT).expect("row");
    assert_eq!(row.status, SessionStatus::Idle);
    assert_eq!(
        asking(&core).as_deref(),
        Some(REQUEST),
        "Needs you shows it"
    );

    respond(&core, "cmd-answer", REQUEST, "Deny and stop");
    assert_eq!(
        outcome(&core, "cmd-answer").await,
        (
            SessionCommandStatus::Applied,
            Some("answered as new turn".into())
        )
    );
    wait_for(
        || {
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.prompt.contains("Deny and stop"))
        },
        "the answer to run as the next turn",
    )
    .await;
    assert_eq!(
        question_state(&core, REQUEST).map(|(_, resolved)| resolved),
        Some(true)
    );
    wait_for(|| asking(&core).is_none(), "the row to stop asking").await;
    wait_for(|| !core.sessions.has_run(CHAT), "the answered turn to end").await;

    // Stop afterwards: no run, nothing left to settle — still applied.
    stop(&core, "cmd-stop");
    assert_eq!(
        outcome(&core, "cmd-stop").await,
        (SessionCommandStatus::Applied, Some("no live run".into()))
    );
    assert_eq!(streaming_entries(&core), 0);
    core.shutdown().await;
}

/// Same boot state, Stop alone: the question closes and the row stops
/// asking; nothing is dispatched.
#[tokio::test(flavor = "multi_thread")]
async fn stop_closes_the_question_a_lost_run_left_open() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    manufacture_quit_cut_short(&dir).await;

    let requests: RequestLog = Default::default();
    let core = engine(&dir, &requests, OnAsk::Wait);
    assert_eq!(asking(&core).as_deref(), Some(REQUEST));

    stop(&core, "cmd-stop");
    assert_eq!(
        outcome(&core, "cmd-stop").await,
        (
            SessionCommandStatus::Applied,
            Some("no live run; settled 0 streaming entries, closed 1 question".into())
        )
    );
    assert_eq!(
        question_state(&core, REQUEST),
        Some((Some(MessageStatus::Aborted), true))
    );
    assert_eq!(asking(&core), None, "Needs you no longer shows it");
    assert_eq!(support::status(&core, CHAT), Some(SessionStatus::Idle));
    // A late answer to the closed question is rejected, and says why.
    respond(&core, "cmd-late", REQUEST, "Allow");
    assert_eq!(
        outcome(&core, "cmd-late").await,
        (
            SessionCommandStatus::Rejected,
            Some("no pending input request".into())
        )
    );
    assert!(
        requests.lock().unwrap().is_empty(),
        "nothing was dispatched"
    );
    core.shutdown().await;
}

/// The exact stuck doc state, with nothing at boot having touched it: a
/// streaming entry holding an unresolved escalate question and no run. The
/// answer is applied through the next-turn fallback (the stuck entry
/// settles as that turn starts); a Stop after it settles what is left.
#[tokio::test(flavor = "multi_thread")]
async fn an_answer_on_a_streaming_entry_with_no_run_takes_effect() {
    let tmp = tempfile::tempdir().unwrap();
    let requests: RequestLog = Default::default();
    let core = engine(tmp.path(), &requests, OnAsk::Wait);
    push_stuck_doc(&core);
    assert_eq!(streaming_entries(&core), 1);

    respond(&core, "cmd-answer", REQUEST, "Allow");
    assert_eq!(
        outcome(&core, "cmd-answer").await,
        (
            SessionCommandStatus::Applied,
            Some("answered as new turn".into())
        )
    );
    wait_for(
        || {
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.prompt.contains("Allow"))
        },
        "the answer to run as the next turn",
    )
    .await;
    wait_for(|| !core.sessions.has_run(CHAT), "the answered turn to end").await;
    assert_eq!(
        question_state(&core, REQUEST),
        Some((Some(MessageStatus::Aborted), true)),
        "the stuck entry settled, its question answered"
    );
    assert_eq!(streaming_entries(&core), 0);
    assert_eq!(asking(&core), None);

    stop(&core, "cmd-stop");
    assert_eq!(
        outcome(&core, "cmd-stop").await.0,
        SessionCommandStatus::Applied
    );
    assert_eq!(asking(&core), None);
    assert_eq!(support::status(&core, CHAT), Some(SessionStatus::Idle));
    core.shutdown().await;
}

/// The exact stuck doc state, Stop alone: it settles the streaming entry and
/// closes the question instead of applying as a no-op.
#[tokio::test(flavor = "multi_thread")]
async fn stop_alone_settles_a_streaming_entry_with_no_run() {
    let tmp = tempfile::tempdir().unwrap();
    let requests: RequestLog = Default::default();
    let core = engine(tmp.path(), &requests, OnAsk::Wait);
    push_stuck_doc(&core);

    stop(&core, "cmd-stop");
    assert_eq!(
        outcome(&core, "cmd-stop").await,
        (
            SessionCommandStatus::Applied,
            Some("no live run; settled 1 streaming entry, closed 1 question".into())
        )
    );
    assert_eq!(
        question_state(&core, REQUEST),
        Some((Some(MessageStatus::Aborted), true))
    );
    assert_eq!(asking(&core), None);
    assert!(requests.lock().unwrap().is_empty());
    core.shutdown().await;
}

// --- runs that end while their question is up ------------------------------

/// Graceful engine stop while the run asks: the run settles (entry aborted,
/// question closed with it), the settle is stored before the rest of the
/// teardown, and the next boot finds nothing streaming or asking.
#[tokio::test(flavor = "multi_thread")]
async fn a_graceful_stop_while_asking_settles_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let requests: RequestLog = Default::default();
    let core = engine(&dir, &requests, OnAsk::Wait);
    start_asking_run(&core);
    let request_id = live_question(&core).await;
    core.shutdown().await;
    drop(core);

    let core = engine(&dir, &requests, OnAsk::Wait);
    assert_eq!(streaming_entries(&core), 0);
    assert_eq!(
        question_state(&core, &request_id),
        Some((Some(MessageStatus::Aborted), true))
    );
    assert_eq!(asking(&core), None);
    core.shutdown().await;
}

/// kill -9 while the run asks (stale journal): the entry is stamped aborted
/// on boot and the question stays answerable; the answer takes effect.
#[tokio::test(flavor = "multi_thread")]
async fn a_kill_while_asking_leaves_an_answerable_question() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let requests: RequestLog = Default::default();
    {
        // The state a kill -9 leaves: the doc and row as the run wrote them,
        // the journal ending on the question.
        let core = engine(&dir, &requests, OnAsk::Wait);
        chat_row(&core);
        core.workspace.record_session(&asking_row());
        core.shutdown().await;
    }
    let store = DocsStore::open(dir.join("orgs/dev-org/dev-user")).unwrap();
    let doc = SessionDoc::init(CHAT).unwrap();
    for entry in stuck_entries() {
        doc.push_message(&entry).unwrap();
    }
    store
        .save_snapshot(CHAT, &doc.export_snapshot().unwrap())
        .unwrap();
    let journal = RunJournal::open(dir.join("orgs/dev-org/dev-user/journals")).unwrap();
    journal
        .append(
            CHAT,
            &AgentEvent::InputRequested {
                request_id: REQUEST.into(),
                questions: vec![escalate_question()],
            },
        )
        .unwrap();
    drop(journal);

    let core = engine(&dir, &requests, OnAsk::Wait);
    assert_eq!(streaming_entries(&core), 0);
    assert_eq!(
        question_state(&core, REQUEST),
        Some((Some(MessageStatus::Aborted), false))
    );
    assert_eq!(asking(&core).as_deref(), Some(REQUEST));
    respond(&core, "cmd-answer", REQUEST, "Allow");
    assert_eq!(
        outcome(&core, "cmd-answer").await.0,
        SessionCommandStatus::Applied
    );
    wait_for(|| asking(&core).is_none(), "the row to stop asking").await;
    core.shutdown().await;
}

/// Plan stop queues the `Interrupt` command into each session it started:
/// a session asking settles aborted, its question closes, the row stops
/// asking.
#[tokio::test(flavor = "multi_thread")]
async fn stopping_an_asking_run_closes_its_question() {
    let tmp = tempfile::tempdir().unwrap();
    let requests: RequestLog = Default::default();
    let core = engine(tmp.path(), &requests, OnAsk::Wait);
    start_asking_run(&core);
    let request_id = live_question(&core).await;

    stop(&core, "cmd-stop");
    assert_eq!(
        outcome(&core, "cmd-stop").await,
        (SessionCommandStatus::Applied, None)
    );
    wait_for(|| !core.sessions.has_run(CHAT), "the run to end").await;
    assert_eq!(
        question_state(&core, &request_id),
        Some((Some(MessageStatus::Aborted), true))
    );
    assert_eq!(streaming_entries(&core), 0);
    assert_eq!(asking(&core), None);
    assert_eq!(support::status(&core, CHAT), Some(SessionStatus::Idle));
    core.shutdown().await;
}

/// The agent child dies while its question is up (a harness restart): the
/// run settles its entry and closes the question; nothing streams or asks.
#[tokio::test(flavor = "multi_thread")]
async fn a_harness_dying_while_asking_settles_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let requests: RequestLog = Default::default();
    let core = engine(tmp.path(), &requests, OnAsk::Die);
    start_asking_run(&core);
    let request_id = live_question(&core).await;
    wait_for(|| !core.sessions.has_run(CHAT), "the run to end").await;
    assert_eq!(streaming_entries(&core), 0);
    assert_eq!(
        question_state(&core, &request_id).map(|(_, resolved)| resolved),
        Some(true)
    );
    assert_eq!(asking(&core), None);
    core.shutdown().await;
}

/// A Runtime replace recycles parked sessions only: a run asking keeps its
/// question, and the answer still reaches it.
#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_replace_leaves_an_asking_run_answerable() {
    let tmp = tempfile::tempdir().unwrap();
    let requests: RequestLog = Default::default();
    let core = engine(tmp.path(), &requests, OnAsk::Wait);
    start_asking_run(&core);
    let request_id = live_question(&core).await;

    core.sessions.recycle_idle_sessions().await;
    assert!(core.sessions.has_run(CHAT));
    assert_eq!(asking(&core).as_deref(), Some(request_id.as_str()));

    respond(&core, "cmd-answer", &request_id, "Allow");
    assert_eq!(
        outcome(&core, "cmd-answer").await,
        (SessionCommandStatus::Applied, None)
    );
    wait_for(
        || {
            entries(&core, CHAT).iter().any(|e| {
                e.status == Some(MessageStatus::Complete)
                    && e.parts.iter().any(
                        |p| matches!(p, MessagePart::Text { text, .. } if text.contains("picked Allow")),
                    )
            })
        },
        "the answered turn to complete",
    )
    .await;
    assert_eq!(asking(&core), None);
    core.shutdown().await;
}

/// Two answers to a live question in one go: the run takes the first, the
/// second is rejected — never the next-turn fallback (no duplicate turn).
#[tokio::test(flavor = "multi_thread")]
async fn a_second_answer_to_a_live_question_never_starts_a_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let requests: RequestLog = Default::default();
    let core = engine(tmp.path(), &requests, OnAsk::Wait);
    start_asking_run(&core);
    let request_id = live_question(&core).await;

    respond(&core, "cmd-answer-1", &request_id, "Allow");
    respond(&core, "cmd-answer-2", &request_id, "Deny and stop");
    assert_eq!(
        outcome(&core, "cmd-answer-1").await,
        (SessionCommandStatus::Applied, None)
    );
    let (status, reason) = outcome(&core, "cmd-answer-2").await;
    assert_eq!(status, SessionCommandStatus::Rejected, "{reason:?}");
    wait_for(|| !core.sessions.has_run(CHAT), "the answered turn to end").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(requests.lock().unwrap().len(), 1, "no duplicate turn");
    assert_eq!(asking(&core), None);
    core.shutdown().await;
}
