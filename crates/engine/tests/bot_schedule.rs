//! Memory and routines against a real engine (docs/design/agent-bots.md
//! §7–§8): the notebook tools by origin, routines that run in their own
//! thread and post into the bot chat, failures, run-now, the pause switch,
//! and project routines that run as plain sessions (in their own worktree
//! when asked).

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::Notify;

use cypher_doc::MessagePart;
use cypher_engine::EngineCore;
use cypher_proto::{
    self as p, Bot, BotChat, BotOrigin, BotPost, BotRoutine, BotScope, BotThread, HarnessId,
    PermissionPrompt, PermissionPromptKind, ProjectGrant, RunOutcome, RunRequest, Schedule,
    UserInputQuestion,
};

use common::{TestHarness, wait_for_within};

/// What each run reached the harness with.
type Runs = Arc<Mutex<Vec<(RunRequest, Option<cypher_harness::BotRunEnv>)>>>;

/// A stand-in Pi: asks for an approval when the prompt says ASK, holds its
/// answer until `hold` is notified when it says HOLD, else answers
/// "Report: <last line of the prompt>".
fn harness(runs: Runs, hold: Arc<Notify>) -> TestHarness {
    TestHarness::new(HarnessId::Pi, "Scheduled", move |request, controls| {
        runs.lock()
            .unwrap()
            .push((request.clone(), controls.host.profile.bot().cloned()));
        if request.prompt.contains("HOLD") {
            let hold = hold.clone();
            let started =
                common::session_started(HarnessId::Pi, "mock-1", &request.cwd, "s", "a-1");
            let rest = futures::stream::once(async move {
                hold.notified().await;
                futures::stream::iter([common::text("Held answer."), common::done("s")].map(Ok))
            })
            .flatten();
            return Ok(futures::stream::iter([Ok(started)]).chain(rest).boxed());
        }
        if request.prompt.contains("ASK") {
            let answer = (controls.request_input)(vec![UserInputQuestion {
                id: "perm-1".into(),
                header: "Approval".into(),
                question: "Run this command outside the sandbox?".into(),
                options: vec!["Run once".into(), "Deny".into()],
                multi_select: false,
                permission: Some(PermissionPrompt {
                    kind: PermissionPromptKind::Escalate,
                    tool_call_id: None,
                }),
                asked_by: None,
            }]);
            let started =
                common::session_started(HarnessId::Pi, "mock-1", &request.cwd, "s", "a-1");
            let rest = futures::stream::once(async move {
                let _ = answer.await;
                futures::stream::iter(
                    [common::text("Carried on without it."), common::done("s")].map(Ok),
                )
            })
            .flatten();
            return Ok(futures::stream::iter([Ok(started)]).chain(rest).boxed());
        }
        let first = request
            .prompt
            .lines()
            .last()
            .unwrap_or_default()
            .to_string();
        common::reply(HarnessId::Pi, &request, "sess", &format!("Report: {first}"))
    })
}

struct Rig {
    core: EngineCore,
    runs: Runs,
    hold: Arc<Notify>,
    dir: tempfile::TempDir,
}

/// An engine over `root/data` whose worktrees go under `root/worktrees`.
fn engine(root: &Path, runs: Runs, hold: Arc<Notify>) -> EngineCore {
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap();
    let previous = std::env::var_os("CYPHER_WORKTREES_DIR");
    unsafe { std::env::set_var("CYPHER_WORKTREES_DIR", root.join("worktrees")) };
    let core = common::engine_at(&root.join("data"), harness(runs, hold));
    unsafe {
        match previous {
            Some(value) => std::env::set_var("CYPHER_WORKTREES_DIR", value),
            None => std::env::remove_var("CYPHER_WORKTREES_DIR"),
        }
    }
    core
}

/// One engine with a project "web".
async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let runs: Runs = Arc::default();
    let hold = Arc::new(Notify::new());
    let core = engine(&root, runs.clone(), hold.clone());
    let web = root.join("web");
    common::init_repo(&web, "hello\n").await;
    core.workspace
        .create_space("sp-web", &core.device_id, web.to_str().unwrap(), None, true)
        .unwrap();
    Rig {
        core,
        runs,
        hold,
        dir,
    }
}

async fn create_bot(core: &EngineCore, coordinate: bool) -> Bot {
    let client = cypher_rpc::memory_client(core.rpc_service());
    let value = client
        .call(
            cypher_rpc::methods::CREATE_BOT,
            serde_json::json!({
                "targetDeviceId": core.device_id,
                "name": "Bob",
                "scope": BotScope {
                    projects: ProjectGrant::Selected { space_ids: vec!["sp-web".into()] },
                    ..Default::default()
                },
                "capabilities": { "coordinate": coordinate },
            }),
        )
        .await
        .expect("CreateBot");
    serde_json::from_value(value).expect("a Bot")
}

async fn mutate(core: &EngineCore, op: serde_json::Value) {
    cypher_rpc::memory_client(core.rpc_service())
        .call(cypher_rpc::methods::MUTATE, op)
        .await
        .expect("Mutate");
}

async fn tool(core: &EngineCore, chat_id: &str, name: &str, args: serde_json::Value) -> String {
    let reply = cypher_rpc::memory_client(core.rpc_service())
        .call(
            cypher_rpc::methods::BOT_TOOL,
            serde_json::json!({ "chatId": chat_id, "tool": name, "args": args }),
        )
        .await
        .expect("BotTool answers");
    reply["text"].as_str().expect("text").to_string()
}

fn routine_def(id: &str, bot: &Bot, prompt: &str, schedule: Schedule) -> BotRoutine {
    BotRoutine {
        id: id.into(),
        bot_id: bot.id.clone(),
        name: "Morning brief".into(),
        prompt: prompt.into(),
        target: p::RoutineTarget::Workspace,
        model: None,
        schedule,
        enabled: true,
        write_notes: false,
        work_directly: false,
        created_at: 0,
        updated_at: 0,
        run_requested_at: None,
        thread_chat_id: None,
        manual_chat_id: None,
        worktree_path: None,
        last_run: None,
        next_run_at: None,
    }
}

async fn save_routine(core: &EngineCore, routine: &BotRoutine) {
    mutate(
        core,
        serde_json::json!({ "op": "saveBotRoutine", "routine": routine }),
    )
    .await;
}

fn routine(core: &EngineCore, id: &str) -> BotRoutine {
    core.workspace
        .record(p::KIND_BOT_ROUTINES, id)
        .expect("the routine row")
}

async fn wait_for(predicate: impl FnMut() -> bool, what: &str) {
    wait_for_within(predicate, what, Duration::from_secs(20)).await;
}

fn last_outcome(core: &EngineCore, id: &str) -> Option<RunOutcome> {
    routine(core, id).last_run.map(|run| run.outcome)
}

/// The post parts in a bot chat, with who posted them.
fn posts(core: &EngineCore, chat_id: &str) -> Vec<(String, BotPost)> {
    common::entries(core, chat_id)
        .into_iter()
        .flat_map(|e| e.parts)
        .filter_map(|part| match part {
            MessagePart::Post { text, from, .. } => Some((text, from)),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_due_routine_runs_in_its_thread_and_posts_to_the_bot_chat() {
    let Rig {
        core,
        runs,
        dir: _dir,
        ..
    } = rig().await;
    let bot = create_bot(&core, false).await;
    let mut def = routine_def(
        "r-brief",
        &bot,
        "Tell me what's stuck.",
        Schedule::Interval { minutes: 60 },
    );
    def.model = Some("pi/routine-model".into());
    save_routine(&core, &def).await;

    // The host plans the next run as soon as the routine exists.
    wait_for(
        || routine(&core, "r-brief").next_run_at.is_some(),
        "the next run to be planned",
    )
    .await;
    let planned = routine(&core, "r-brief").next_run_at.unwrap();
    assert!(planned > p::now_ms() + 50 * 60_000, "an hour from now");

    // Due now (a slot the device slept through).
    let due = serde_json::json!({ "nextRunAt": p::now_ms() - 60_000 });
    core.workspace
        .update_record(
            p::KIND_BOT_ROUTINES,
            "r-brief",
            due.as_object().unwrap().clone(),
        )
        .unwrap();
    wait_for(
        || last_outcome(&core, "r-brief") == Some(RunOutcome::Done),
        "the routine run to finish",
    )
    .await;

    let done = routine(&core, "r-brief");
    let thread_id = done.thread_chat_id.clone().expect("its thread");
    let run = done.last_run.clone().unwrap();
    assert_eq!(run.chat_id.as_deref(), Some(thread_id.as_str()));
    assert_eq!(
        run.summary.as_deref(),
        Some("Report: Tell me what's stuck.")
    );
    assert!(
        done.next_run_at.unwrap() > p::now_ms() + 50 * 60_000,
        "the rest is skipped: the next run is planned from now"
    );

    // The thread: hidden, origin routine, in the bot's folder.
    let thread = core.workspace.chat(&thread_id).unwrap().unwrap();
    assert_eq!(
        thread.bot,
        Some(BotChat::thread(
            &bot.id,
            BotOrigin::Routine,
            BotThread::Routine {
                routine_id: "r-brief".into()
            }
        ))
    );
    assert_eq!(thread.title.as_deref(), Some("Morning brief"));
    let bot_chat = core.workspace.chat(&bot.chat_id).unwrap().unwrap();
    assert_eq!(thread.cwd, bot_chat.cwd);
    assert_eq!(thread.device_id, core.device_id);
    let (request, env) = runs
        .lock()
        .unwrap()
        .iter()
        .find(|(r, _)| r.prompt.contains("Tell me what's stuck."))
        .cloned()
        .expect("the run reached the harness");
    assert!(
        request.prompt.starts_with("Routine \"Morning brief\" · "),
        "{}",
        request.prompt
    );
    assert_eq!(
        request.model.as_deref(),
        Some("pi/routine-model"),
        "the routine's model"
    );
    let env = env.expect("a bot run");
    assert_eq!(env.origin, BotOrigin::Routine);
    assert!(env.tools.iter().any(|t| t == "read_notes"));
    assert!(!env.tools.iter().any(|t| t == "save_routine"));
    assert!(env.system_prompt.contains("Your notebook:"));

    // Its output lands in the bot chat as a post attributed to the routine.
    wait_for(|| !posts(&core, &bot.chat_id).is_empty(), "the post").await;
    let posted = posts(&core, &bot.chat_id);
    assert_eq!(
        posted,
        vec![(
            "Report: Tell me what's stuck.".to_string(),
            BotPost::Routine {
                routine_id: "r-brief".into(),
                name: "Morning brief".into(),
                outcome: RunOutcome::Done,
                run_at: run.at,
                thread_chat_id: Some(thread_id.clone()),
            }
        )]
    );
    assert_eq!(
        core.workspace
            .chat(&bot.chat_id)
            .unwrap()
            .unwrap()
            .last_message_preview
            .as_deref(),
        Some("Morning brief: Report: Tell me what's stuck.")
    );
    let activity = core.workspace.read_bot_records().activity;
    assert!(
        activity
            .iter()
            .any(|a| a.kind == p::ActivityKind::RoutineRan
                && a.bot_id == bot.id
                && a.chat_id.as_deref() == Some(thread_id.as_str())),
        "{activity:?}"
    );
    // One post per run, however many passes look at it.
    core.bots.schedule_now().await;
    assert_eq!(posts(&core, &bot.chat_id).len(), 1);
    // No bot-chat turn was started for it.
    assert!(
        !runs.lock().unwrap().iter().any(
            |(r, _)| r.cwd == bot_chat.cwd.clone().unwrap() && !r.prompt.starts_with("Routine")
        ),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_now_failures_and_the_pause_switch() {
    let Rig {
        core, dir: _dir, ..
    } = rig().await;
    let bot = create_bot(&core, false).await;
    // An unattended run that asks for approval stops failed, with why.
    let asking = routine_def("r-ask", &bot, "ASK before cleaning up", Schedule::Manual);
    save_routine(&core, &asking).await;
    mutate(
        &core,
        serde_json::json!({ "op": "runBotRoutine", "routineId": "r-ask" }),
    )
    .await;
    wait_for(
        || last_outcome(&core, "r-ask") == Some(RunOutcome::Failed),
        "the asking run to fail",
    )
    .await;
    let failed = routine(&core, "r-ask").last_run.unwrap();
    let reason = "This run is unattended, so it couldn't ask for approval to run a command \
                  outside the sandbox. It stopped.";
    assert_eq!(failed.summary.as_deref(), Some(reason));
    assert_eq!(routine(&core, "r-ask").next_run_at, None, "manual");
    let records = core.workspace.read_bot_records();
    assert_eq!(
        records.needs_you(&bot.id, |_| false),
        vec![p::NeedsYou::RoutineFailed("r-ask".into())]
    );
    wait_for(
        || {
            posts(&core, &bot.chat_id).iter().any(|(text, from)| {
                text == reason
                    && matches!(
                        from,
                        BotPost::Routine {
                            outcome: RunOutcome::Failed,
                            ..
                        }
                    )
            })
        },
        "the failure posted to the workspace chat",
    )
    .await;

    // Paused: a requested run is recorded as skipped and never starts.
    mutate(
        &core,
        serde_json::json!({ "op": "setBotPaused", "botId": bot.id, "paused": true }),
    )
    .await;
    let quiet = routine_def("r-quiet", &bot, "Summarize the week", Schedule::Manual);
    save_routine(&core, &quiet).await;
    mutate(
        &core,
        serde_json::json!({ "op": "runBotRoutine", "routineId": "r-quiet" }),
    )
    .await;
    wait_for(
        || last_outcome(&core, "r-quiet") == Some(RunOutcome::Skipped),
        "the paused run to be skipped",
    )
    .await;
    let skipped = routine(&core, "r-quiet");
    assert_eq!(
        skipped.last_run.unwrap().summary.as_deref(),
        Some("Skipped: the workspace is paused.")
    );
    assert!(
        skipped.thread_chat_id.is_none() && skipped.manual_chat_id.is_none(),
        "no session, no run"
    );

    // Resumed, a new request runs.
    mutate(
        &core,
        serde_json::json!({ "op": "setBotPaused", "botId": bot.id, "paused": false }),
    )
    .await;
    mutate(
        &core,
        serde_json::json!({ "op": "runBotRoutine", "routineId": "r-quiet" }),
    )
    .await;
    wait_for(
        || last_outcome(&core, "r-quiet") == Some(RunOutcome::Done),
        "the resumed run",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_routine_runs_as_a_plain_session_in_its_own_worktree() {
    let Rig {
        core, runs, dir, ..
    } = rig().await;
    let bot = create_bot(&core, true).await;
    let saved = tool(
        &core,
        &bot.chat_id,
        "save_routine",
        serde_json::json!({
            "project": "web",
            "name": "Nightly tests",
            "prompt": "Run the tests and fix anything flaky",
            "schedule": { "kind": "interval", "minutes": 60 },
            "worktree": true,
        }),
    )
    .await;
    assert!(saved.contains("an ordinary session in web"), "{saved}");
    let id = core.workspace.read_bot_records().routines[0].id.clone();
    assert_eq!(
        routine(&core, &id).target,
        p::RoutineTarget::Project {
            space_id: "sp-web".into(),
            worktree: true,
            model: None,
        }
    );

    // Run now: a fresh session in the project, hidden, run as a plain one.
    mutate(
        &core,
        serde_json::json!({ "op": "runBotRoutine", "routineId": id }),
    )
    .await;
    wait_for(
        || last_outcome(&core, &id) == Some(RunOutcome::Done),
        "the manual run",
    )
    .await;
    let first = routine(&core, &id);
    let run = first.last_run.clone().unwrap();
    assert!(run.manual);
    let manual = first.manual_chat_id.clone().expect("its manual session");
    assert_eq!(run.chat_id.as_deref(), Some(manual.as_str()));
    assert!(first.thread_chat_id.is_none(), "no scheduled run yet");
    let session = core.workspace.chat(&manual).unwrap().unwrap();
    assert_eq!(session.space_id.as_deref(), Some("sp-web"));
    assert_eq!(
        session.bot,
        Some(BotChat::thread(
            &bot.id,
            BotOrigin::Routine,
            BotThread::RoutineSession {
                routine_id: id.clone()
            }
        ))
    );
    let worktrees = dir.path().canonicalize().unwrap().join("worktrees");
    let cwd = session.cwd.clone().unwrap();
    assert!(Path::new(&cwd).starts_with(&worktrees), "{cwd}");
    let (request, env) = runs
        .lock()
        .unwrap()
        .iter()
        .find(|(r, _)| r.prompt.contains("Run the tests"))
        .cloned()
        .expect("the run reached the harness");
    assert!(env.is_none(), "a plain session, not a workspace thread");
    assert_eq!(request.cwd, cwd);
    assert!(
        request
            .prompt
            .starts_with("This is a run of the routine \"Nightly tests\""),
        "the agent is told nobody watches: {}",
        request.prompt
    );
    let shown = common::entries(&core, &manual)
        .into_iter()
        .flat_map(|e| e.parts)
        .find_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text),
            _ => None,
        })
        .expect("the run's prompt in the session");
    assert!(shown.starts_with("Routine \"Nightly tests\" · "), "{shown}");
    wait_for(
        || routine(&core, &id).worktree_path.as_deref() == Some(cwd.as_str()),
        "the routine to keep its worktree",
    )
    .await;
    wait_for(
        || {
            posts(&core, &bot.chat_id).iter().any(|(_, from)| {
                matches!(from, BotPost::Routine { thread_chat_id: Some(chat), .. } if *chat == manual)
            })
        },
        "the result posted to the workspace chat",
    )
    .await;

    // Run now again: another fresh session in the same worktree; the last
    // manual one is deleted.
    tokio::time::sleep(Duration::from_millis(5)).await;
    mutate(
        &core,
        serde_json::json!({ "op": "runBotRoutine", "routineId": id }),
    )
    .await;
    wait_for(
        || {
            routine(&core, &id)
                .manual_chat_id
                .is_some_and(|chat| chat != manual)
                && last_outcome(&core, &id) == Some(RunOutcome::Done)
        },
        "the second manual run",
    )
    .await;
    let second = routine(&core, &id).manual_chat_id.unwrap();
    assert!(
        core.workspace.chat(&manual).unwrap().is_none(),
        "the last manual run's session is replaced"
    );
    let again = core.workspace.chat(&second).unwrap().unwrap();
    assert_eq!(
        again.cwd.as_deref(),
        Some(cwd.as_str()),
        "the same worktree"
    );

    // Scheduled runs continue one standing session.
    let due = || {
        let fields = serde_json::json!({ "nextRunAt": p::now_ms() - 60_000 });
        core.workspace
            .update_record(
                p::KIND_BOT_ROUTINES,
                &id,
                fields.as_object().unwrap().clone(),
            )
            .unwrap();
    };
    due();
    wait_for(
        || {
            let r = routine(&core, &id);
            r.thread_chat_id.is_some()
                && r.last_run
                    .is_some_and(|run| !run.manual && run.outcome == RunOutcome::Done)
        },
        "the scheduled run",
    )
    .await;
    let standing = routine(&core, &id).thread_chat_id.unwrap();
    assert_ne!(standing, second);
    let at = routine(&core, &id).last_run.unwrap().at;
    tokio::time::sleep(Duration::from_millis(5)).await;
    due();
    wait_for(
        || {
            routine(&core, &id)
                .last_run
                .is_some_and(|run| run.at > at && run.outcome == RunOutcome::Done)
        },
        "the next scheduled run",
    )
    .await;
    let after = routine(&core, &id);
    assert_eq!(after.thread_chat_id.as_deref(), Some(standing.as_str()));
    assert_eq!(
        after.last_run.unwrap().chat_id.as_deref(),
        Some(standing.as_str())
    );
    assert_eq!(
        core.workspace
            .chat(&standing)
            .unwrap()
            .unwrap()
            .cwd
            .as_deref(),
        Some(cwd.as_str())
    );
    assert!(
        core.workspace.chat(&second).unwrap().is_some(),
        "scheduled runs leave the manual one"
    );
    let schedule = tool(&core, &bot.chat_id, "list_routines", serde_json::json!({})).await;
    assert!(
        schedule.contains("a session in web on") && schedule.contains("its own worktree"),
        "{schedule}"
    );

    // Deleting the routine deletes its sessions.
    mutate(
        &core,
        serde_json::json!({ "op": "deleteBotRoutine", "routineId": id }),
    )
    .await;
    assert!(core.workspace.chat(&standing).unwrap().is_none());
    assert!(core.workspace.chat(&second).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn project_routines_need_coordinate_and_run_unattended() {
    let Rig {
        core, dir: _dir, ..
    } = rig().await;
    let client = cypher_rpc::memory_client(core.rpc_service());

    // Without Coordinate, neither the agent nor the owner can make one.
    let reader = create_bot(&core, false).await;
    let refused = client
        .call(
            cypher_rpc::methods::BOT_TOOL,
            serde_json::json!({
                "chatId": reader.chat_id,
                "tool": "save_routine",
                "args": {
                    "project": "web",
                    "name": "Cleanup",
                    "prompt": "Delete the build folder",
                    "schedule": { "kind": "manual" },
                },
            }),
        )
        .await;
    let text = match refused {
        Ok(reply) => reply["text"].as_str().unwrap_or_default().to_string(),
        Err(err) => err.to_string(),
    };
    assert!(text.contains("Coordinate"), "{text}");
    let mut def = routine_def(
        "r-clean",
        &reader,
        "ASK to delete the build folder",
        Schedule::Manual,
    );
    def.target = p::RoutineTarget::Project {
        space_id: "sp-web".into(),
        worktree: false,
        model: None,
    };
    let owner = client
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({ "op": "saveBotRoutine", "routine": def }),
        )
        .await;
    assert!(
        owner.is_err_and(|err| err.to_string().contains("Coordinate")),
        "the owner op refuses it too"
    );

    // With it, a run that asks fails unattended in the project's checkout.
    let bot = create_bot(&core, true).await;
    def.bot_id = bot.id.clone();
    save_routine(&core, &def).await;
    mutate(
        &core,
        serde_json::json!({ "op": "runBotRoutine", "routineId": "r-clean" }),
    )
    .await;
    wait_for(
        || last_outcome(&core, "r-clean") == Some(RunOutcome::Failed),
        "the asking run to fail",
    )
    .await;
    let run = routine(&core, "r-clean").last_run.unwrap();
    assert!(
        run.summary
            .as_deref()
            .is_some_and(|s| s.starts_with("This run is unattended")),
        "{run:?}"
    );
    let session = run.chat_id.unwrap();
    let web = core.workspace.space("sp-web").unwrap().unwrap();
    assert_eq!(
        core.workspace.chat(&session).unwrap().unwrap().cwd,
        Some(web.path.clone()),
        "the project's checkout"
    );
    let parts: Vec<MessagePart> = common::entries(&core, &session)
        .into_iter()
        .flat_map(|e| e.parts)
        .collect();
    assert!(
        !parts.iter().any(|p| matches!(p, MessagePart::Input { .. })),
        "no approval card waits in the session: {parts:?}"
    );

    // Pausing the workspace skips its project routines too.
    mutate(
        &core,
        serde_json::json!({ "op": "setBotPaused", "botId": bot.id, "paused": true }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    mutate(
        &core,
        serde_json::json!({ "op": "runBotRoutine", "routineId": "r-clean" }),
    )
    .await;
    wait_for(
        || last_outcome(&core, "r-clean") == Some(RunOutcome::Skipped),
        "the paused run to be skipped",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_follow_the_thread_that_writes_them() {
    let Rig {
        core, dir: _dir, ..
    } = rig().await;
    let bot = create_bot(&core, false).await;

    let empty = tool(
        &core,
        &bot.chat_id,
        "write_note",
        serde_json::json!({ "text": "  " }),
    )
    .await;
    assert_eq!(empty, "Give write_note some text.");
    let saved = tool(
        &core,
        &bot.chat_id,
        "write_note",
        serde_json::json!({ "text": "Alice owns auth" }),
    )
    .await;
    assert!(saved.starts_with("Saved to your notebook"), "{saved}");
    let notes = core.workspace.read_bot_records().notes;
    assert_eq!(notes.len(), 1);
    assert_eq!(
        notes[0].source,
        p::NoteSource::Bot {
            chat_id: Some(bot.chat_id.clone()),
            title: Some("Bob".into())
        }
    );
    let long = tool(
        &core,
        &bot.chat_id,
        "write_note",
        serde_json::json!({ "text": "x".repeat(3_000) }),
    )
    .await;
    assert!(long.contains("cut to 1000 characters"), "{long}");
    mutate(
        &core,
        serde_json::json!({
            "op": "saveBotNote", "noteId": "n-owner", "botId": bot.id,
            "text": "The release is blocked on the migration",
        }),
    )
    .await;
    let read = tool(
        &core,
        &bot.chat_id,
        "read_notes",
        serde_json::json!({ "query": "RELEASE" }),
    )
    .await;
    assert!(
        read.contains("- The release is blocked on the migration (from your owner"),
        "{read}"
    );
    assert!(!read.contains("Alice"), "{read}");

    // A routine thread reads; it writes only when its routine allows it.
    let def = routine_def("r-notes", &bot, "Check in", Schedule::Manual);
    save_routine(&core, &def).await;
    let mut thread = core.workspace.chat(&bot.chat_id).unwrap().unwrap();
    thread.id = "routine-thread".into();
    thread.title = Some("Check in".into());
    thread.bot = Some(BotChat::thread(
        &bot.id,
        BotOrigin::Routine,
        BotThread::Routine {
            routine_id: "r-notes".into(),
        },
    ));
    core.workspace.import_chat_row(&thread).unwrap();
    let all = tool(&core, "routine-thread", "read_notes", serde_json::json!({})).await;
    assert!(all.contains("Alice owns auth"), "{all}");
    let refused = tool(
        &core,
        "routine-thread",
        "write_note",
        serde_json::json!({ "text": "from a routine" }),
    )
    .await;
    assert_eq!(
        refused,
        "This session may read the notebook but not write to it."
    );
    let mut writer = def.clone();
    writer.write_notes = true;
    save_routine(&core, &writer).await;
    let written = tool(
        &core,
        "routine-thread",
        "write_note",
        serde_json::json!({ "text": "from a routine" }),
    )
    .await;
    assert!(written.starts_with("Saved"), "{written}");
    let refused = tool(
        &core,
        "routine-thread",
        "save_routine",
        serde_json::json!({ "name": "x", "prompt": "y", "schedule": { "kind": "manual" } }),
    )
    .await;
    assert!(
        refused.starts_with("Routines are made in the workspace chat"),
        "{refused}"
    );

    // A peer thread never sees the notebook.
    let mut peer = thread.clone();
    peer.id = "peer-thread".into();
    peer.bot = Some(BotChat::thread(
        &bot.id,
        BotOrigin::Peer,
        BotThread::Peer {
            connection_id: "c1".into(),
        },
    ));
    core.workspace.import_chat_row(&peer).unwrap();
    for name in ["read_notes", "write_note", "list_routines"] {
        let refused = tool(
            &core,
            "peer-thread",
            name,
            serde_json::json!({ "text": "x" }),
        )
        .await;
        assert!(
            refused.contains("isn't available") || refused.contains("not write"),
            "{name}: {refused}"
        );
        assert!(!refused.contains("Alice"), "{refused}");
    }

    // save_routine in the bot chat makes a routine and says what it saved.
    let saved = tool(
        &core,
        &bot.chat_id,
        "save_routine",
        serde_json::json!({
            "name": "Morning brief",
            "prompt": "Every morning send me what's stuck",
            "schedule": { "kind": "daily", "time": "09:00", "days": [1, 2, 3, 4, 5] },
        }),
    )
    .await;
    assert!(
        saved.starts_with("Saved routine \"Morning brief\"") && saved.contains("Weekdays 09:00"),
        "{saved}"
    );
    let bad = tool(
        &core,
        &bot.chat_id,
        "save_routine",
        serde_json::json!({
            "name": "Too often", "prompt": "x", "schedule": { "kind": "interval", "minutes": 5 },
        }),
    )
    .await;
    assert_eq!(bad, "Run at most every 15 minutes.");
    // A routine in a project needs Coordinate.
    let refused = tool(
        &core,
        &bot.chat_id,
        "save_routine",
        serde_json::json!({
            "project": "web", "name": "n", "prompt": "p", "schedule": { "kind": "manual" },
        }),
    )
    .await;
    assert!(refused.contains("Coordinate"), "{refused}");
    let activity = core.workspace.read_bot_records().activity;
    assert!(
        activity
            .iter()
            .filter(|a| a.kind == p::ActivityKind::NoteWritten)
            .count()
            >= 3
    );
}

/// A routine that finishes while the owner's turn is running posts after
/// that turn, never into it.
#[tokio::test(flavor = "multi_thread")]
async fn a_post_waits_for_the_bot_chats_turn() {
    let Rig {
        core,
        hold,
        dir: _dir,
        ..
    } = rig().await;
    let bot = create_bot(&core, false).await;
    let cwd = core
        .workspace
        .chat(&bot.chat_id)
        .unwrap()
        .unwrap()
        .cwd
        .unwrap();
    core.doc_host
        .queue_command(
            &bot.chat_id,
            cypher_doc::SessionCommandPayload::Run {
                request: RunRequest {
                    cwd,
                    ..common::run_request("HOLD while I think")
                },
                message_id: "owner-1".into(),
                agent_prompt: None,
            },
        )
        .unwrap();
    wait_for(
        || common::status(&core, &bot.chat_id) == Some(p::SessionStatus::Working),
        "the owner's turn",
    )
    .await;

    save_routine(
        &core,
        &routine_def("r-busy", &bot, "Anything new?", Schedule::Manual),
    )
    .await;
    mutate(
        &core,
        serde_json::json!({ "op": "runBotRoutine", "routineId": "r-busy" }),
    )
    .await;
    wait_for(
        || last_outcome(&core, "r-busy") == Some(RunOutcome::Done),
        "the routine run",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(posts(&core, &bot.chat_id).is_empty(), "held for the turn");

    hold.notify_one();
    wait_for(|| posts(&core, &bot.chat_id).len() == 1, "the post").await;
    let entries = common::entries(&core, &bot.chat_id);
    let answer = entries
        .iter()
        .position(|e| {
            e.parts
                .iter()
                .any(|p| matches!(p, MessagePart::Text { text, .. } if text == "Held answer."))
        })
        .expect("the owner's answer");
    let post = entries
        .iter()
        .position(|e| {
            e.parts
                .iter()
                .any(|p| matches!(p, MessagePart::Post { .. }))
        })
        .unwrap();
    assert!(answer < post, "the post follows the turn");
}

/// A run cut off by an engine shutdown is settled by the next engine
/// instead of staying "running" forever.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_cut_off_by_a_restart_settles_on_the_next_start() {
    let Rig {
        core,
        runs,
        hold,
        dir,
    } = rig().await;
    let root = dir.path().canonicalize().unwrap();
    let bot = create_bot(&core, false).await;
    save_routine(
        &core,
        &routine_def("r-cut", &bot, "HOLD the summary", Schedule::Manual),
    )
    .await;
    mutate(
        &core,
        serde_json::json!({ "op": "runBotRoutine", "routineId": "r-cut" }),
    )
    .await;
    wait_for(
        || {
            routine(&core, "r-cut")
                .manual_chat_id
                .is_some_and(|t| common::status(&core, &t) == Some(p::SessionStatus::Working))
        },
        "the routine run to start",
    )
    .await;
    core.shutdown().await;
    drop(core);
    let core = engine(&root, runs.clone(), hold.clone());
    core.bots.schedule_now().await;
    assert_eq!(wait_for_settled(&core, "r-cut").await, RunOutcome::Failed);
    // The old engine may have settled it while shutting down; a crash
    // leaves the row saying "running". The new engine settles that too.
    let mut crashed = routine(&core, "r-cut").last_run.unwrap();
    crashed.outcome = RunOutcome::Running;
    crashed.ended_at = None;
    crashed.summary = None;
    let fields = serde_json::json!({ "lastRun": crashed });
    core.workspace
        .update_record(
            p::KIND_BOT_ROUTINES,
            "r-cut",
            fields.as_object().unwrap().clone(),
        )
        .unwrap();
    assert_eq!(wait_for_settled(&core, "r-cut").await, RunOutcome::Failed);
    let run = routine(&core, "r-cut").last_run.unwrap();
    assert!(run.summary.is_some_and(|s| !s.is_empty()));
    core.shutdown().await;
}

async fn wait_for_settled(core: &EngineCore, id: &str) -> RunOutcome {
    let mut outcome = None;
    wait_for(
        || {
            outcome = last_outcome(core, id).filter(|o| *o != RunOutcome::Running);
            outcome.is_some()
        },
        "the cut-off run to settle",
    )
    .await;
    outcome.unwrap()
}

/// A routine granted Work directly gets Pi's own tools and the bot's local
/// checkouts as roots, but only while the bot itself works directly.
#[tokio::test(flavor = "multi_thread")]
async fn a_routine_granted_work_directly_runs_with_pi_tools() {
    let rig = rig().await;
    let core = &rig.core;
    let bot: Bot = serde_json::from_value(
        cypher_rpc::memory_client(core.rpc_service())
            .call(
                cypher_rpc::methods::CREATE_BOT,
                serde_json::json!({
                    "targetDeviceId": core.device_id,
                    "name": "Tester",
                    "scope": BotScope {
                        projects: ProjectGrant::Selected { space_ids: vec!["sp-web".into()] },
                        ..Default::default()
                    },
                    "capabilities": { "workDirectly": true },
                }),
            )
            .await
            .expect("CreateBot"),
    )
    .unwrap();
    let mut routine = routine_def("r-tests", &bot, "Run the tests", Schedule::Manual);
    routine.work_directly = true;
    save_routine(core, &routine).await;
    let thread = cypher_proto::Chat {
        bot: Some(cypher_proto::BotChat::thread(
            bot.id.clone(),
            cypher_proto::BotOrigin::Routine,
            cypher_proto::BotThread::Routine {
                routine_id: "r-tests".into(),
            },
        )),
        ..core.workspace.chat(&bot.chat_id).unwrap().unwrap()
    };
    let env = cypher_engine::bots::profile::bot_env(&core.workspace, &thread).unwrap();
    assert!(env.tools.iter().any(|t| t == "bash"), "{:?}", env.tools);
    assert!(!env.workspace_roots.is_empty());
    assert!(env.system_prompt.contains("This routine may run commands"));
    // Not granted: read tools only.
    routine.work_directly = false;
    save_routine(core, &routine).await;
    let env = cypher_engine::bots::profile::bot_env(&core.workspace, &thread).unwrap();
    assert!(!env.tools.iter().any(|t| t == "bash"), "{:?}", env.tools);
    assert!(env.workspace_roots.is_empty());
}
