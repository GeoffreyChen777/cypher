//! Effective-prompt override (the Comment feature): a Run/Steer command with
//! `agent_prompt` delivers the AUGMENTED prompt to the harness while the doc
//! user entry keeps the VISIBLE prompt. Without the field the harness gets
//! the visible prompt (behavior unchanged), and the retry re-delivers the
//! same override.

mod common;

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use tokio::sync::mpsc;

use cypher_doc::{MessageComment, MessagePart, MessageRole};
use cypher_engine::{EngineCore, SteerOutcome};
use cypher_proto::{AgentEvent, HarnessId, SessionStatus};

use common::{entries, run_request, status, text, wait_for};

const CHAT: &str = "chat-annotated";

fn session_started() -> AgentEvent {
    common::session_started(
        HarnessId::Mock,
        "mock-1",
        "/tmp",
        "hs-annotated",
        "a-annotated",
    )
}

fn done() -> AgentEvent {
    common::done("hs-annotated")
}

struct Rig {
    core: EngineCore,
    feed: mpsc::UnboundedSender<AgentEvent>,
    prompts: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

/// Records every prompt the harness received — the main run's request AND
/// every mailbox steer (pi's parked path consumes a steer immediately and
/// confirms it with a `Steered` boundary, so the recording covers accepted
/// steers end-to-end).
fn assemble() -> Rig {
    let (feed, rx) = mpsc::unbounded_channel();
    let feed_rx = Mutex::new(Some(rx));
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let log = prompts.clone();
    let harness = common::TestHarness::new(
        HarnessId::Mock,
        "RecordingHarness",
        move |request, controls| {
            // The auto-titler's throwaway run (its template prompt) is not a user
            // turn — never recorded.
            if request.prompt.contains("concise 3-5 word title") {
                return common::script(vec![done()]);
            }
            log.lock().unwrap().push(request.prompt.clone());
            let feed = feed_rx
                .lock()
                .unwrap()
                .take()
                .expect("RecordingHarness serves the main dispatch once per test");
            let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
            let prompts = log.clone();
            let interrupt = controls.interrupt.clone();
            let mut steering = controls.steering;
            tokio::spawn(async move {
                let mut feed = futures::stream::unfold(feed, |mut feed| async move {
                    feed.recv().await.map(|event| (event, feed))
                })
                .boxed();
                loop {
                    tokio::select! {
                        event = feed.next(), if !interrupt.is_cancelled() => match event {
                            Some(event) => {
                                if tx.send(event).is_err() {
                                    break;
                                }
                            }
                            None => break,
                        },
                        steer = steering.recv(), if !interrupt.is_cancelled() => match steer {
                            Some(msg) => {
                                prompts.lock().unwrap().push(msg.prompt.clone());
                                if tx
                                    .send(AgentEvent::Steered {
                                        assistant_message_id: Some("prev-annotated".into()),
                                        next_assistant_message_id: Some("next-annotated".into()),
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            None => break,
                        },
                        _ = interrupt.cancelled() => break,
                    }
                }
            });
            Ok(common::channel_stream(rx))
        },
    )
    .steering();
    let common::Rig { core, dir } = common::rig(harness);
    Rig {
        core,
        feed,
        prompts,
        _dir: dir,
    }
}

fn user_texts(core: &EngineCore) -> Vec<String> {
    entries(core, CHAT)
        .into_iter()
        .filter(|e| e.role == MessageRole::User)
        .filter_map(|e| {
            e.parts
                .iter()
                .filter_map(|p| match p {
                    MessagePart::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .next()
        })
        .collect()
}

/// A Run carrying `agent_prompt`: the harness receives the AUGMENTED prompt
/// while the doc user entry keeps the VISIBLE prompt.
#[tokio::test]
async fn run_delivers_agent_prompt_but_keeps_visible_entry() {
    let rig = assemble();
    let visible = "check the build";
    let augmented =
        "Conversation annotations (JSON): {\"comments\":[]}\n\nUser request:\ncheck the build";
    rig.core
        .sessions
        .dispatch_augmented(
            CHAT,
            HarnessId::Mock,
            run_request(visible),
            Some(augmented.to_string()),
            None,
        )
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("Watching.")).unwrap();
    rig.feed.send(done()).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    let received = rig.prompts.lock().unwrap().clone();
    assert_eq!(
        received,
        vec![augmented],
        "the harness must receive the effective (augmented) prompt"
    );
    assert_eq!(
        user_texts(&rig.core),
        vec![visible.to_string()],
        "the doc user entry must stay the visible prompt"
    );
    rig.core.sessions.shutdown().await;
}

/// A comment itself is sufficient turn content: the harness receives a
/// non-empty annotation prompt while the document does not invent filler text
/// for the user's visible transcript.
#[tokio::test]
async fn comment_only_run_delivers_annotations_without_visible_filler() {
    let rig = assemble();
    let augmented = concat!(
        "Conversation annotations (JSON): ",
        "{\"comments\":[{\"quotedText\":\"old text\",\"comment\":\"fix this\"}]}",
        "\n\nUser request:\n"
    );
    rig.core
        .sessions
        .dispatch_augmented(
            CHAT,
            HarnessId::Mock,
            run_request(""),
            Some(augmented.to_string()),
            None,
        )
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(done()).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    assert_eq!(*rig.prompts.lock().unwrap(), vec![augmented.to_string()]);
    assert_eq!(
        user_texts(&rig.core),
        vec![String::new()],
        "the document must not synthesize visible filler text"
    );
    rig.core.sessions.shutdown().await;
}

/// Without `agent_prompt` the harness receives the visible prompt — old
/// behavior is byte-compatible.
#[tokio::test]
async fn run_without_agent_prompt_delivers_visible_prompt() {
    let rig = assemble();
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("plain request"), None)
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(done()).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;
    assert_eq!(
        *rig.prompts.lock().unwrap(),
        vec!["plain request".to_string()]
    );
    assert_eq!(user_texts(&rig.core), vec!["plain request".to_string()]);
    rig.core.sessions.shutdown().await;
}

/// An ACCEPTED steer carrying `agent_prompt` delivers the augmented prompt to
/// the harness mailbox while the doc keeps the visible steer text.
#[tokio::test]
async fn accepted_steer_delivers_agent_prompt_but_keeps_visible_entry() {
    let rig = assemble();
    let visible_run = "watch the build";
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request(visible_run), None)
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("Watching.")).unwrap();
    rig.feed.send(done()).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    let visible_steer = "follow-up";
    let augmented_steer =
        "Conversation annotations (JSON): {\"comments\":[]}\n\nUser request:\nfollow-up";
    let outcome = rig
        .core
        .sessions
        .steer_augmented(
            CHAT,
            visible_steer,
            Some(augmented_steer.to_string()),
            Some("msg-2".to_string()),
        )
        .await
        .expect("steer");
    assert_eq!(outcome, SteerOutcome::Accepted);
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "Steered boundary re-arms Working",
    )
    .await;
    // The engine sets Working optimistically on acceptance; the harness's
    // parked-path consumption is async — wait for the steer to be recorded.
    wait_for(
        || rig.prompts.lock().unwrap().len() >= 2,
        "harness records the mailbox steer",
    )
    .await;

    let received = rig.prompts.lock().unwrap().clone();
    assert_eq!(
        received,
        vec![visible_run.to_string(), augmented_steer.to_string()],
        "main run + augmented steer delivered to the harness"
    );
    assert_eq!(
        user_texts(&rig.core),
        vec![visible_run.to_string(), visible_steer.to_string()],
        "both doc user entries stay visible"
    );
    rig.core.sessions.shutdown().await;
}

/// A commented prompt with one quote taken from a displayed translation.
fn commented_prompt(request: &str) -> String {
    use cypher_proto::agent_prompt::{AgentQuote, PromptComment, QuoteAlign, comments_block, wrap};
    let align = AgentQuote::Align(QuoteAlign {
        passage: "The original passage.".into(),
        before: "译文".into(),
        selected: "选中".into(),
        after: "。".into(),
    });
    wrap(
        &[comments_block(&[
            PromptComment::new("选中", Some(&align), "why?"),
            PromptComment::new("plain", None, "ok"),
        ])],
        request,
    )
}

fn user_comments(core: &EngineCore) -> Vec<Vec<MessageComment>> {
    entries(core, CHAT)
        .into_iter()
        .filter(|e| e.role == MessageRole::User)
        .map(|e| e.comments)
        .collect()
}

fn expected_comments() -> Vec<MessageComment> {
    vec![
        MessageComment {
            quote: "选中".into(),
            comment: "why?".into(),
        },
        MessageComment {
            quote: "plain".into(),
            comment: "ok".into(),
        },
    ]
}

/// The comments that rode a Run land on its user entry, quoted as the user
/// selected them — even though a non-Pi harness receives the prompt with the
/// translation alignment stripped.
#[tokio::test]
async fn run_records_its_comments_on_the_user_entry() {
    let rig = assemble();
    let augmented = commented_prompt("look");
    rig.core
        .sessions
        .dispatch_augmented(
            CHAT,
            HarnessId::Mock,
            run_request("look"),
            Some(augmented.clone()),
            None,
        )
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(done()).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    let received = rig.prompts.lock().unwrap().clone();
    assert_eq!(
        received,
        vec![cypher_proto::agent_prompt::strip_alignment(&augmented)]
    );
    assert_eq!(user_comments(&rig.core), vec![expected_comments()]);
    rig.core.sessions.shutdown().await;
}

/// A routed steer records its comments the same way, and strips the
/// alignment for the running non-Pi harness itself.
#[tokio::test]
async fn accepted_steer_records_its_comments_and_strips_alignment() {
    let rig = assemble();
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("watch"), None)
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(done()).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    let augmented = commented_prompt("follow-up");
    let outcome = rig
        .core
        .sessions
        .steer_augmented(
            CHAT,
            "follow-up",
            Some(augmented.clone()),
            Some("msg-2".to_string()),
        )
        .await
        .expect("steer");
    assert_eq!(outcome, SteerOutcome::Accepted);
    wait_for(
        || rig.prompts.lock().unwrap().len() >= 2,
        "harness records the mailbox steer",
    )
    .await;

    let received = rig.prompts.lock().unwrap().clone();
    assert_eq!(
        received[1],
        cypher_proto::agent_prompt::strip_alignment(&augmented)
    );
    assert!(!received[1].contains(cypher_proto::agent_prompt::ALIGN_KEY));
    assert_eq!(
        user_comments(&rig.core),
        vec![Vec::new(), expected_comments()]
    );
    rig.core.sessions.shutdown().await;
}
