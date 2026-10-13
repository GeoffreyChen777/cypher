//! The mock harness's turn changes demo (`[mock:changes:N]`), run through the
//! engine on the demo project: each round records the files it changed on
//! its answer, the way a real agent's rounds do. The demo session
//! (`scripts/dev-demo.sh --changes`) is built from these rounds.

mod common;

use std::path::Path;

use cypher_doc::{MessageRole, MessageStatus};
use cypher_harness::mock::{CHANGES_MARKER, CHANGES_ROUNDS, MockHarness};
use cypher_proto::{AgentEvent, DoneStatus, HarnessId, RunRequest, SessionStatus, TurnChanges};

const CHAT: &str = "chat-changes-demo";

fn harness() -> MockHarness {
    MockHarness {
        script: vec![AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        }],
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == "__pycache__" {
            continue;
        }
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// `(status, path)` per file, in git's order.
fn files(changes: &TurnChanges) -> Vec<(&str, &str)> {
    changes
        .files
        .iter()
        .map(|file| (file.status.as_str(), file.path.as_str()))
        .collect()
}

/// Settled answers and what each recorded.
fn answers(core: &cypher_engine::EngineCore) -> Vec<Option<TurnChanges>> {
    common::entries(core, CHAT)
        .into_iter()
        .filter(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
        .map(|e| e.turn_changes)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_round_records_what_it_changed() {
    let rig = common::rig(harness());
    let repo = rig.dir.path().join("tasks-app");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/demo/tasks-app"),
        &repo,
    );
    common::git(&repo, &["init", "-q", "-b", "main"]).await;
    common::git(&repo, &["add", "."]).await;
    common::git(&repo, &["commit", "-q", "-m", "tasks 0.4.0"]).await;

    for round in 1..=CHANGES_ROUNDS {
        let request = RunRequest {
            cwd: repo.to_string_lossy().to_string(),
            ..common::run_request(&format!("Round {round} {CHANGES_MARKER}{round}]"))
        };
        rig.core
            .sessions
            .dispatch(CHAT, HarnessId::Mock, request, None)
            .await
            .expect("dispatch");
        common::wait_for(
            || {
                answers(&rig.core).len() == round
                    && common::status(&rig.core, CHAT) == Some(SessionStatus::Idle)
            },
            "the round to settle",
        )
        .await;
        if round < CHANGES_ROUNDS {
            common::wait_for(
                || answers(&rig.core).last().is_some_and(Option::is_some),
                "the round's changes",
            )
            .await;
        }
    }
    let rounds = answers(&rig.core);
    let round = |n: usize| rounds[n - 1].as_ref().expect("recorded");
    assert_eq!(
        files(round(1)),
        [
            ("modified", "README.md"),
            ("modified", "tasks/cli.py"),
            ("added", "tasks/dates.py"),
            ("modified", "tasks/model.py"),
            ("modified", "tasks/render.py"),
            ("modified", "tasks/store.py"),
        ]
    );
    assert_eq!(
        files(round(2)),
        [
            ("renamed", "CHANGELOG.md"),
            ("modified", "pyproject.toml"),
            ("modified", "tasks/cli.py"),
            ("deleted", "tasks/legacy_import.py"),
            ("added", "tests/test_dates.py"),
        ]
    );
    assert_eq!(round(3).file_count, 8);
    assert!(
        rounds[3].is_none(),
        "a round that changed nothing shows no card"
    );
    rig.core.sessions.shutdown().await;
}
