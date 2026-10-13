//! Round changes: a settled round records the files it changed on its last
//! answer, a steer extends the round it lands in, and the recorded tree pair
//! reads the round's diff back after the checkout moved on.

mod common;

use std::path::Path;
use std::time::Duration;

use cypher_doc::MessageRole;
use cypher_engine::{EngineCore, capture_tree_diff};
use cypher_proto::{HarnessId, RunRequest, SessionStatus, TurnChanges};

use common::{entries, feed_rig, run_request, status, text, wait_for};

const CHAT: &str = "chat-round";
const PROMPT: &str = "edit the repo";

fn request_in(repo: &Path, prompt: &str) -> RunRequest {
    RunRequest {
        cwd: repo.to_string_lossy().to_string(),
        ..run_request(prompt)
    }
}

fn session_started(repo: &Path) -> cypher_proto::AgentEvent {
    common::session_started(
        HarnessId::Mock,
        "mock-1",
        &repo.to_string_lossy(),
        "hs-round",
        "a-round",
    )
}

/// The recorded changes of every assistant entry, in transcript order.
fn recorded(core: &EngineCore) -> Vec<Option<TurnChanges>> {
    entries(core, CHAT)
        .into_iter()
        .filter(|entry| entry.role == MessageRole::Assistant)
        .map(|entry| entry.turn_changes)
        .collect()
}

/// Wait until the round's start snapshot landed, so edits the test makes
/// next belong to the round rather than racing its base.
async fn wait_for_round_start(core: &EngineCore) {
    wait_for(
        || core.diff_sync.turn_snapshot(CHAT).is_some(),
        "turn-start snapshot",
    )
    .await;
}

fn paths(changes: &TurnChanges) -> Vec<(&str, &str)> {
    changes
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.status.as_str()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settled_round_records_its_changed_files() {
    let rig = feed_rig(PROMPT, true);
    let repo = rig.dir.path().join("repo");
    common::init_repo(&repo, "one\ntwo\n").await;
    // Untracked before the round: never part of it.
    std::fs::write(repo.join("pre.txt"), "before\n").expect("pre.txt");

    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, request_in(&repo, PROMPT), None)
        .await
        .expect("dispatch");
    wait_for_round_start(&rig.core).await;
    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").expect("edit a.txt");
    std::fs::write(repo.join("new.txt"), "made this round\n").expect("new.txt");
    rig.feed.send(session_started(&repo)).unwrap();
    rig.feed.send(text("Done.")).unwrap();
    rig.feed.send(common::done("hs-round")).unwrap();

    wait_for(
        || recorded(&rig.core).last().is_some_and(Option::is_some),
        "round changes recorded",
    )
    .await;
    let changes = recorded(&rig.core).pop().flatten().expect("changes");
    assert_eq!(
        paths(&changes),
        vec![("a.txt", "modified"), ("new.txt", "added")]
    );
    assert_eq!(changes.file_count, 2);
    assert_eq!(changes.additions, 2);
    assert_eq!(changes.deletions, 0);
    let root = std::fs::canonicalize(&repo).expect("canonical repo");
    assert_eq!(Path::new(&changes.cwd), root);

    // The tree pair reads the round back, whatever the checkout does next.
    std::fs::write(repo.join("a.txt"), "rewritten later\n").expect("later edit");
    let diff = capture_tree_diff(&rig.core.repos, &root, &changes.base, &changes.head)
        .await
        .expect("round diff");
    assert!(diff.patch.contains("+three"), "{}", diff.patch);
    assert!(diff.patch.contains("+made this round"), "{}", diff.patch);
    assert!(!diff.patch.contains("rewritten later"), "{}", diff.patch);
    assert!(!diff.patch.contains("before"), "{}", diff.patch);
    rig.core.sessions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steer_extends_the_round_and_a_quiet_round_records_nothing() {
    let rig = feed_rig(PROMPT, true);
    let repo = rig.dir.path().join("repo");
    common::init_repo(&repo, "one\ntwo\n").await;

    // Round one changes nothing.
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, request_in(&repo, PROMPT), None)
        .await
        .expect("dispatch");
    wait_for_round_start(&rig.core).await;
    rig.feed.send(session_started(&repo)).unwrap();
    rig.feed.send(text("Nothing to do.")).unwrap();
    rig.feed.send(common::done("hs-round")).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "round one settles",
    )
    .await;

    // Round two: an edit, a steer while it works, then another edit. Both
    // belong to the one round the steer landed in.
    let first = rig.core.diff_sync.turn_snapshot(CHAT).expect("snapshot").at;
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, request_in(&repo, "round two"), None)
        .await
        .expect("round two");
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "round two works",
    )
    .await;
    // Round one's start stays the recorded "latest turn" until round two's
    // lands; wait for the new one.
    wait_for(
        || {
            rig.core
                .diff_sync
                .turn_snapshot(CHAT)
                .is_some_and(|snapshot| snapshot.at > first)
        },
        "round two start snapshot",
    )
    .await;
    std::fs::write(repo.join("a.txt"), "one\ntwo\nbefore steer\n").expect("edit a.txt");
    rig.feed.send(text("Working on it.")).unwrap();
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, request_in(&repo, "also b"), None)
        .await
        .expect("steer");
    std::fs::write(repo.join("b.txt"), "after steer\n").expect("b.txt");
    rig.feed.send(text("Both done.")).unwrap();
    rig.feed.send(common::done("hs-round")).unwrap();

    wait_for(
        || recorded(&rig.core).last().is_some_and(Option::is_some),
        "round two changes recorded",
    )
    .await;
    let all = recorded(&rig.core);
    assert!(
        all[..all.len() - 1].iter().all(Option::is_none),
        "only the round's last answer records it: {all:?}"
    );
    let changes = all.last().cloned().flatten().expect("changes");
    assert_eq!(
        paths(&changes),
        vec![("a.txt", "modified"), ("b.txt", "added")]
    );
    // Give a (wrong) record of round one a beat to surface.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(recorded(&rig.core)[0].is_none());
    rig.core.sessions.shutdown().await;
}
