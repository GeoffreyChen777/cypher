//! A bot's coordination against a real engine (docs/design/agent-bots.md
//! §6, P2): plans proposed and approved over `Mutate`, the orchestration
//! thread started from a brief, worker sessions driven through the command
//! plane, Stop, limits, capability refusals, approvals, and *Work directly*'s
//! roots and permission level.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;

use cypher_doc::{SessionCommandPayload, SessionCommandStatus};
use cypher_engine::EngineCore;
use cypher_proto::{
    ActivityKind, ApprovalState, Bot, BotApproval, BotChat, BotOrigin, BotScope, BotThread,
    BotWork, DoneStatus, HarnessId, ProjectGrant, RunRequest, SandboxLevel, SessionStatus,
    StepState, UserInputQuestion, WorkState,
};

use common::{run_request, wait_for, wait_for_some};

type Runs = Arc<Mutex<Vec<(RunRequest, Option<cypher_harness::BotRunEnv>)>>>;

/// A stand-in Pi. By prompt: the orchestration brief and `long` run until
/// interrupted (`finish` makes the brief end at once with "All done."),
/// `ask` asks which table to migrate and reports the answer, anything else
/// answers "ok".
fn harness(runs: Runs, finish: bool) -> common::TestHarness {
    common::TestHarness::new(HarnessId::Pi, "Stand-in", move |request, controls| {
        runs.lock()
            .unwrap()
            .push((request.clone(), controls.host.profile.bot().cloned()));
        let started = common::session_started(HarnessId::Pi, "mock-1", &request.cwd, "sess", "a-1");
        let brief = request.prompt.starts_with("Carry out this approved plan");
        if brief && finish {
            return common::reply(HarnessId::Pi, &request, "sess", "All done.");
        }
        if brief || request.prompt == "long" {
            let interrupt = controls.interrupt.clone();
            let rest = futures::stream::once(async move {
                interrupt.cancelled().await;
                futures::stream::iter([Ok(common::done_with(
                    DoneStatus::Interrupted,
                    Some("sess"),
                ))])
            })
            .flatten();
            return Ok(futures::stream::iter([Ok(started)]).chain(rest).boxed());
        }
        if request.prompt == "ask" {
            let answer = (controls.request_input)(vec![UserInputQuestion {
                id: "q1".into(),
                header: "Table".into(),
                question: "Migrate invoices or credits?".into(),
                options: vec!["invoices".into(), "credits".into()],
                multi_select: false,
                permission: None,
            }]);
            let rest = futures::stream::once(async move {
                let picked = answer
                    .await
                    .ok()
                    .and_then(|answers| answers.into_iter().next())
                    .map(|a| a.labels.join(","))
                    .unwrap_or_default();
                futures::stream::iter([
                    Ok(common::text(&format!("Migrated {picked}."))),
                    Ok(common::done("sess")),
                ])
            })
            .flatten();
            return Ok(futures::stream::iter([Ok(started)]).chain(rest).boxed());
        }
        common::reply(HarnessId::Pi, &request, "sess", "ok")
    })
}

struct Rig {
    core: EngineCore,
    runs: Runs,
    root: tempfile::TempDir,
}

fn rig(finish: bool) -> Rig {
    let root = tempfile::tempdir().unwrap();
    let runs = Runs::default();
    let core = common::engine_at(&root.path().join("data"), harness(runs.clone(), finish));
    for (space, chat) in [("sp-billing", "chat-billing"), ("sp-web", "chat-web")] {
        let path = root.path().join(space);
        std::fs::create_dir_all(&path).unwrap();
        core.workspace
            .create_space(space, &core.device_id, path.to_str().unwrap(), None, false)
            .unwrap();
        core.workspace
            .create_chat(chat, Some(space), None, None, None)
            .unwrap();
        core.workspace
            .rename_chat(chat, &format!("{space} work"))
            .unwrap();
    }
    Rig { core, runs, root }
}

fn billing_only() -> BotScope {
    BotScope {
        projects: ProjectGrant::Selected {
            space_ids: vec!["sp-billing".into()],
        },
        ..Default::default()
    }
}

impl Rig {
    fn client(&self) -> cypher_rpc::RpcClient {
        cypher_rpc::memory_client(self.core.rpc_service())
    }

    async fn bot(&self, caps: serde_json::Value) -> Bot {
        let value = self
            .client()
            .call(
                cypher_rpc::methods::CREATE_BOT,
                serde_json::json!({
                    "targetDeviceId": self.core.device_id,
                    "name": "Bob",
                    "scope": billing_only(),
                    "capabilities": caps,
                }),
            )
            .await
            .expect("CreateBot");
        serde_json::from_value(value).expect("a Bot")
    }

    async fn tool(&self, chat_id: &str, name: &str, args: serde_json::Value) -> String {
        tool(&self.client(), chat_id, name, args, None).await
    }

    async fn mutate(&self, op: serde_json::Value) {
        self.client()
            .call(cypher_rpc::methods::MUTATE, op.clone())
            .await
            .unwrap_or_else(|err| panic!("{op}: {err}"));
    }

    fn work(&self, id: &str) -> BotWork {
        self.core
            .workspace
            .record(cypher_proto::KIND_BOT_WORK, id)
            .expect("the work row")
    }

    fn activity(&self, bot: &Bot) -> Vec<ActivityKind> {
        let mut rows: Vec<_> = self
            .core
            .workspace
            .read_bot_records()
            .activity
            .into_iter()
            .filter(|a| a.bot_id == bot.id)
            .collect();
        rows.sort_by_key(|a| a.at);
        rows.into_iter().map(|a| a.kind).collect()
    }

    fn approvals(&self, bot: &Bot) -> Vec<BotApproval> {
        self.core
            .workspace
            .read_bot_records()
            .approvals
            .into_iter()
            .filter(|a| a.bot_id == bot.id)
            .collect()
    }

    /// The bot chat's owner message, then a proposed plan; its work id.
    async fn propose(&self, bot: &Bot, title: &str) -> String {
        self.core
            .doc_host
            .open(&bot.chat_id)
            .unwrap()
            .write_user_message(&format!("m-{title}"), "Fix the invoice rounding", 1_000)
            .unwrap();
        let text = tool(
            &self.client(),
            &bot.chat_id,
            "propose_plan",
            serde_json::json!({
                "title": title,
                "summary": "Fix it, then test it",
                "steps": [
                    { "title": "Fix rounding", "project": "sp-billing", "detail": "in a worktree" },
                    { "title": "Run the tests", "project": "sp-billing" },
                ],
            }),
            Some("call-1"),
        )
        .await;
        assert!(text.contains("waiting for your owner's approval"), "{text}");
        self.core
            .workspace
            .read_bot_records()
            .work
            .into_iter()
            .find(|w| w.title == title)
            .expect("a work row")
            .id
    }

    /// Approve a plan and wait for its thread; the thread's chat id.
    async fn approve(&self, work_id: &str) -> String {
        self.mutate(
            serde_json::json!({ "op": "resolveBotPlan", "workId": work_id, "approve": true }),
        )
        .await;
        wait_for_some("the orchestration thread", || {
            let work = self.work(work_id);
            (work.state == WorkState::Running)
                .then_some(work.thread_chat_id)
                .flatten()
        })
        .await
    }

    fn session_status(&self, chat: &str) -> Option<SessionStatus> {
        self.core.sessions.session_status(chat).map(|s| s.status)
    }

    fn run_of(
        &self,
        prompt_start: &str,
    ) -> Option<(RunRequest, Option<cypher_harness::BotRunEnv>)> {
        self.runs
            .lock()
            .unwrap()
            .iter()
            .find(|(r, _)| r.prompt.starts_with(prompt_start))
            .cloned()
    }
}

async fn tool(
    client: &cypher_rpc::RpcClient,
    chat_id: &str,
    name: &str,
    args: serde_json::Value,
    tool_call_id: Option<&str>,
) -> String {
    let reply = client
        .call(
            cypher_rpc::methods::BOT_TOOL,
            serde_json::json!({
                "chatId": chat_id, "tool": name, "args": args, "toolCallId": tool_call_id,
            }),
        )
        .await
        .expect("BotTool answers");
    reply["text"].as_str().expect("text").to_string()
}

fn coordinate() -> serde_json::Value {
    serde_json::json!({ "workDirectly": false, "coordinate": true })
}

fn session_id_of(text: &str) -> String {
    text.strip_prefix("Started session ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no session id in {text}"))
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approved_plan_runs_in_its_own_thread_from_a_brief() {
    let rig = rig(true);
    let bot = rig.bot(coordinate()).await;
    let work_id = rig.propose(&bot, "Fix rounding").await;

    let work = rig.work(&work_id);
    assert_eq!(work.state, WorkState::Proposed);
    assert_eq!(work.chat_id, bot.chat_id);
    assert_eq!(work.tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(work.instruction, "Fix the invoice rounding");
    assert_eq!(work.steps.len(), 2);
    assert_eq!(work.steps[0].project.as_deref(), Some("sp-billing"));
    assert_eq!(work.steps[0].id, "1");
    assert!(work.awaiting_approval());

    let thread_id = rig.approve(&work_id).await;
    let thread = rig
        .core
        .workspace
        .chat(&thread_id)
        .unwrap()
        .expect("thread");
    assert_eq!(
        thread.bot,
        Some(BotChat::thread(
            &bot.id,
            BotOrigin::Owner,
            BotThread::Work {
                work_id: work_id.clone()
            }
        ))
    );
    let bot_chat = rig.core.workspace.chat(&bot.chat_id).unwrap().unwrap();
    assert_eq!(thread.cwd, bot_chat.cwd, "the bot's folder");
    assert_eq!(thread.title.as_deref(), Some("Fix rounding"));
    assert_eq!(thread.space_id, None);

    // The thread starts on the brief, not the bot chat's history, and
    // without its own work-directly tools.
    let (request, env) = wait_for_some("the brief run", || {
        rig.run_of("Carry out this approved plan")
    })
    .await;
    assert!(
        request.prompt.contains("Plan: \"Fix rounding\""),
        "{}",
        request.prompt
    );
    assert!(
        request
            .prompt
            .contains("\"\"\"\nFix the invoice rounding\n\"\"\"")
    );
    assert!(
        request
            .prompt
            .contains("1. Fix rounding — in sp-billing (project sp-billing)")
    );
    assert_eq!(request.sandbox, SandboxLevel::ReadOnly);
    let env = env.expect("a bot run");
    assert!(env.tools.iter().any(|t| t == "start_session"));
    assert!(env.tools.iter().any(|t| t == "finish_work"));
    assert!(!env.tools.iter().any(|t| t == "propose_plan"));
    assert!(!env.tools.iter().any(|t| t == "bash"));
    assert!(env.system_prompt.contains("carrying out one plan"));

    // Its run ends without finish_work: done, with its last message.
    wait_for_some("the work to settle", || {
        let work = rig.work(&work_id);
        (work.state == WorkState::Done).then_some(())
    })
    .await;
    let work = rig.work(&work_id);
    assert_eq!(work.result.as_deref(), Some("All done."));
    assert!(work.ended_at.is_some());
    assert_eq!(
        rig.activity(&bot),
        [
            ActivityKind::PlanProposed,
            ActivityKind::PlanApproved,
            ActivityKind::WorkFinished
        ]
    );

    // The bot chat follows up with list_work and get_work.
    let listed = rig
        .tool(&bot.chat_id, "list_work", serde_json::json!({}))
        .await;
    assert!(
        listed.contains(&work_id) && listed.contains("done"),
        "{listed}"
    );
    let got = rig
        .tool(
            &bot.chat_id,
            "get_work",
            serde_json::json!({ "work_id": work_id }),
        )
        .await;
    assert!(got.contains("Result:\nAll done."), "{got}");

    // A cancelled proposal never starts.
    let cancelled = rig.propose(&bot, "Second").await;
    rig.mutate(
        serde_json::json!({ "op": "resolveBotPlan", "workId": cancelled, "approve": false }),
    )
    .await;
    wait_for(
        || rig.work(&cancelled).state == WorkState::Cancelled,
        "the plan cancelled",
    )
    .await;
    assert!(rig.work(&cancelled).thread_chat_id.is_none());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_work_thread_drives_its_sessions_through_commands() {
    let rig = rig(false);
    let bot = rig.bot(coordinate()).await;
    let work_id = rig.propose(&bot, "Migrate").await;
    let thread = rig.approve(&work_id).await;

    let started = rig
        .tool(
            &thread,
            "start_session",
            serde_json::json!({
                "step": 1, "project": "sp-billing", "prompt": "ask", "title": "Migrate tables",
            }),
        )
        .await;
    assert!(started.contains("cypher://session/"), "{started}");
    let session = session_id_of(&started);
    let chat = rig
        .core
        .workspace
        .chat(&session)
        .unwrap()
        .expect("a session chat");
    assert_eq!(chat.space_id.as_deref(), Some("sp-billing"));
    assert_eq!(chat.title.as_deref(), Some("Migrate tables"));
    assert_eq!(chat.bot, None, "a worker is an ordinary session");
    let work = rig.work(&work_id);
    assert_eq!(work.steps[0].sessions, std::slice::from_ref(&session));
    assert!(matches!(
        work.steps[0].state,
        StepState::Running | StepState::Asking
    ));

    // It asks; the step follows, and the thread reads the question.
    wait_for(
        || rig.session_status(&session) == Some(SessionStatus::AwaitingInput),
        "the session to ask",
    )
    .await;
    wait_for(
        || rig.work(&work_id).steps[0].state == StepState::Asking,
        "the step to show it is asking",
    )
    .await;
    let waited = rig
        .tool(
            &thread,
            "wait_for_session",
            serde_json::json!({ "session_id": session, "timeout_s": 5 }),
        )
        .await;
    assert!(waited.contains("is asking a question"), "{waited}");
    assert!(waited.contains("options: invoices | credits"), "{waited}");

    let answered = rig
        .tool(
            &thread,
            "answer_session",
            serde_json::json!({ "session_id": session, "answers": ["invoices"] }),
        )
        .await;
    assert!(answered.starts_with("Answered session"), "{answered}");
    let waited = rig
        .tool(
            &thread,
            "wait_for_session",
            serde_json::json!({ "session_id": session, "timeout_s": 10 }),
        )
        .await;
    assert!(waited.contains("finished its run"), "{waited}");
    assert!(waited.contains("Migrated invoices."), "{waited}");
    wait_for(
        || rig.work(&work_id).steps[0].state == StepState::Running,
        "the step back to running",
    )
    .await;

    // Steer and stop a long one.
    let long = session_id_of(
        &rig.tool(
            &thread,
            "start_session",
            serde_json::json!({ "step": "2", "project": "sp-billing", "prompt": "long" }),
        )
        .await,
    );
    wait_for(
        || rig.session_status(&long) == Some(SessionStatus::Working),
        "the long session to run",
    )
    .await;
    let steered = rig
        .tool(
            &thread,
            "steer_session",
            serde_json::json!({ "session_id": long, "message": "Use the v2 schema" }),
        )
        .await;
    assert!(steered.starts_with("Sent your message"), "{steered}");
    let stopped = rig
        .tool(
            &thread,
            "stop_session",
            serde_json::json!({ "session_id": long }),
        )
        .await;
    assert!(stopped.starts_with("Stopped session"), "{stopped}");
    wait_for(
        || rig.session_status(&long) == Some(SessionStatus::Idle),
        "the long session stopped",
    )
    .await;

    let updated = rig
        .tool(
            &thread,
            "update_step",
            serde_json::json!({ "step": 1, "state": "done", "note": "migrated" }),
        )
        .await;
    assert_eq!(updated, "Step 1 is done.");
    let finished = rig
        .tool(
            &thread,
            "finish_work",
            serde_json::json!({ "outcome": "done", "result": "Migrated invoices." }),
        )
        .await;
    assert!(finished.starts_with("Recorded."), "{finished}");
    let work = rig.work(&work_id);
    assert_eq!(work.state, WorkState::Done);
    assert_eq!(work.steps[0].state, StepState::Done);
    assert_eq!(work.steps[0].note.as_deref(), Some("migrated"));
    assert_eq!(work.result.as_deref(), Some("Migrated invoices."));
    let refused = rig
        .tool(
            &thread,
            "start_session",
            serde_json::json!({ "step": 2, "project": "sp-billing", "prompt": "more" }),
        )
        .await;
    assert!(refused.contains("This work is done"), "{refused}");
    let kinds = rig.activity(&bot);
    for kind in [
        ActivityKind::SessionStarted,
        ActivityKind::SessionAnswered,
        ActivityKind::SessionSteered,
        ActivityKind::SessionStopped,
        ActivityKind::WorkFinished,
    ] {
        assert!(kinds.contains(&kind), "{kind:?} in {kinds:?}");
    }
    // The thread's run is interrupted by nothing; the owner can stop it.
    rig.core.sessions.interrupt(&thread).await.unwrap();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_reaches_the_thread_and_every_session() {
    let rig = rig(false);
    let bot = rig.bot(coordinate()).await;
    let work_id = rig.propose(&bot, "Stop me").await;
    let thread = rig.approve(&work_id).await;
    let session = session_id_of(
        &rig.tool(
            &thread,
            "start_session",
            serde_json::json!({ "step": 1, "project": "sp-billing", "prompt": "long" }),
        )
        .await,
    );
    wait_for(
        || {
            rig.session_status(&session) == Some(SessionStatus::Working)
                && rig.session_status(&thread) == Some(SessionStatus::Working)
        },
        "both running",
    )
    .await;

    rig.mutate(serde_json::json!({ "op": "stopBotWork", "workId": work_id }))
        .await;
    wait_for(
        || rig.work(&work_id).state == WorkState::Stopped,
        "the work stopped",
    )
    .await;
    wait_for(
        || {
            rig.session_status(&session) == Some(SessionStatus::Idle)
                && rig.session_status(&thread) == Some(SessionStatus::Idle)
        },
        "the thread and its session interrupted",
    )
    .await;
    let work = rig.work(&work_id);
    assert!(work.steps.iter().all(|s| s.state == StepState::Skipped));
    assert!(rig.activity(&bot).contains(&ActivityKind::WorkStopped));
    // A settled thread after Stop leaves the state alone.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(rig.work(&work_id).state, WorkState::Stopped);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn limits_and_refusals_hold() {
    let rig = rig(false);
    let bot = rig.bot(coordinate()).await;

    // At most three orchestrations run at once; the next waits for a slot.
    let mut threads = Vec::new();
    let mut ids = Vec::new();
    for n in 0..cypher_proto::MAX_RUNNING_WORK {
        let id = rig.propose(&bot, &format!("Plan {n}")).await;
        threads.push(rig.approve(&id).await);
        ids.push(id);
    }
    let waiting = rig.propose(&bot, "Plan waiting").await;
    rig.mutate(serde_json::json!({ "op": "resolveBotPlan", "workId": waiting, "approve": true }))
        .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(rig.work(&waiting).state, WorkState::Proposed, "no slot yet");
    rig.mutate(serde_json::json!({ "op": "stopBotWork", "workId": ids[0] }))
        .await;
    wait_for(
        || rig.work(&waiting).state == WorkState::Running,
        "the waiting plan starts once a slot frees",
    )
    .await;

    // At most eight sessions per orchestration.
    let thread = &threads[1];
    for _ in 0..cypher_proto::MAX_SESSIONS_PER_WORK {
        let started = rig
            .tool(
                thread,
                "start_session",
                serde_json::json!({ "step": 1, "project": "sp-billing", "prompt": "hi" }),
            )
            .await;
        assert!(started.starts_with("Started session"), "{started}");
    }
    let ninth = rig
        .tool(
            thread,
            "start_session",
            serde_json::json!({ "step": 2, "project": "sp-billing", "prompt": "hi" }),
        )
        .await;
    assert!(ninth.contains("the most one orchestration may"), "{ninth}");

    // Scope: an ungranted project reads like an unknown one, and a worktree
    // the work didn't create is no cwd.
    let hidden = rig
        .tool(
            &threads[2],
            "start_session",
            serde_json::json!({ "step": 1, "project": "sp-web", "prompt": "hi" }),
        )
        .await;
    assert!(hidden.starts_with("No project \"sp-web\""), "{hidden}");
    let elsewhere = rig
        .tool(
            &threads[2],
            "start_session",
            serde_json::json!({
                "step": 1, "project": "sp-billing", "prompt": "hi",
                "cwd": rig.root.path().join("sp-web").to_str().unwrap(),
            }),
        )
        .await;
    assert!(
        elsewhere.contains("nor a worktree this work created"),
        "{elsewhere}"
    );
    let plan = rig
        .tool(
            &bot.chat_id,
            "propose_plan",
            serde_json::json!({ "title": "x", "steps": [{ "title": "a", "project": "sp-web" }] }),
        )
        .await;
    assert!(plan.starts_with("No project \"sp-web\""), "{plan}");

    // Each tool works only in its own thread.
    let in_main = rig
        .tool(
            &bot.chat_id,
            "start_session",
            serde_json::json!({ "step": 1, "project": "sp-billing", "prompt": "hi" }),
        )
        .await;
    assert!(
        in_main.contains("only works in an orchestration thread"),
        "{in_main}"
    );
    let in_thread = rig
        .tool(
            thread,
            "propose_plan",
            serde_json::json!({ "title": "x", "steps": [] }),
        )
        .await;
    assert!(
        in_thread.contains("only works in the bot chat"),
        "{in_thread}"
    );
    let mut peer = rig.core.workspace.chat(&bot.chat_id).unwrap().unwrap();
    peer.id = "peer-thread".into();
    peer.bot = Some(BotChat::new(&bot.id, BotOrigin::Peer));
    rig.core.workspace.import_chat_row(&peer).unwrap();
    let from_peer = rig
        .tool("peer-thread", "list_work", serde_json::json!({}))
        .await;
    assert_eq!(from_peer, "list_work isn't available in this thread.");

    // Sessions the work didn't start need Coordinate: steer.
    let other = rig
        .tool(
            thread,
            "stop_session",
            serde_json::json!({ "session_id": "chat-billing" }),
        )
        .await;
    assert!(other.contains("wasn't started by this work"), "{other}");

    // Coordinate switched off applies to the next call of a running thread.
    rig.mutate(serde_json::json!({
        "op": "updateBot", "botId": bot.id, "name": "Bob", "scope": billing_only(),
        "capabilities": { "coordinate": false },
    }))
    .await;
    let off = rig
        .tool(
            thread,
            "start_session",
            serde_json::json!({ "step": 1, "project": "sp-billing", "prompt": "hi" }),
        )
        .await;
    assert!(off.contains("turned off Coordinate"), "{off}");
    for thread in threads
        .iter()
        .chain([&rig.work(&waiting).thread_chat_id.unwrap()])
    {
        let _ = rig.core.sessions.interrupt(thread).await;
    }
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn approvals_gate_tainted_threads_and_others_sessions() {
    let rig = rig(false);
    let bot = rig
        .bot(serde_json::json!({ "workDirectly": false, "coordinate": true, "steer": true }))
        .await;
    let work_id = rig.propose(&bot, "Approvals").await;
    let thread = rig.approve(&work_id).await;
    let pending = |rig: &Rig| {
        rig.approvals(&bot)
            .into_iter()
            .find(|a| a.state == ApprovalState::Pending)
    };

    // Steering a session the work didn't start waits for the owner.
    let client = rig.client();
    let call = {
        let thread = thread.clone();
        tokio::spawn(async move {
            tool(
                &client,
                &thread,
                "steer_session",
                serde_json::json!({ "session_id": "chat-billing", "message": "Rebase please" }),
                None,
            )
            .await
        })
    };
    let approval = wait_for_some("an approval", || pending(&rig)).await;
    assert_eq!(approval.thread_chat_id, thread);
    assert_eq!(approval.work_id.as_deref(), Some(work_id.as_str()));
    assert_eq!(approval.action, "Steer the session \"sp-billing work\"");
    assert!(rig.activity(&bot).contains(&ActivityKind::ApprovalAsked));
    rig.mutate(serde_json::json!({ "op": "resolveBotApproval", "approvalId": approval.id, "approve": true }))
        .await;
    let answer = call.await.unwrap();
    assert!(answer.starts_with("Sent your message"), "{answer}");
    let steers = rig
        .core
        .doc_host
        .open("chat-billing")
        .unwrap()
        .doc()
        .read_commands()
        .unwrap();
    assert!(steers.iter().any(|c| matches!(
        &c.payload,
        SessionCommandPayload::Steer { prompt, .. } if prompt == "Rebase please"
    )));
    wait_for(
        || {
            rig.approvals(&bot)
                .iter()
                .any(|a| a.id == approval.id && a.state == ApprovalState::Approved)
        },
        "the approval recorded",
    )
    .await;

    // Declined: nothing happens, and the model is told.
    let client = rig.client();
    let call = {
        let thread = thread.clone();
        tokio::spawn(async move {
            tool(
                &client,
                &thread,
                "stop_session",
                serde_json::json!({ "session_id": "chat-billing" }),
                None,
            )
            .await
        })
    };
    let approval = wait_for_some("a second approval", || pending(&rig)).await;
    rig.mutate(serde_json::json!({ "op": "resolveBotApproval", "approvalId": approval.id, "approve": false }))
        .await;
    let answer = call.await.unwrap();
    assert!(answer.starts_with("Your owner declined this"), "{answer}");
    let commands = rig
        .core
        .doc_host
        .open("chat-billing")
        .unwrap()
        .doc()
        .read_commands()
        .unwrap();
    assert!(
        !commands
            .iter()
            .any(|c| matches!(c.payload, SessionCommandPayload::Interrupt {})),
        "no interrupt queued"
    );

    // A tainted thread asks before starting a session of its own.
    rig.core
        .bots
        .taint_thread(&thread, "Alice's bot answered in this thread");
    let client = rig.client();
    let call = {
        let thread = thread.clone();
        tokio::spawn(async move {
            tool(
                &client,
                &thread,
                "start_session",
                serde_json::json!({ "step": 1, "project": "sp-billing", "prompt": "hi" }),
                None,
            )
            .await
        })
    };
    let approval = wait_for_some("a taint approval", || pending(&rig)).await;
    assert_eq!(approval.reason, "Alice's bot answered in this thread");
    assert!(approval.action.starts_with("Start a session in sp-billing"));
    assert!(
        rig.work(&work_id).session_ids().next().is_none(),
        "nothing yet"
    );
    rig.mutate(serde_json::json!({ "op": "resolveBotApproval", "approvalId": approval.id, "approve": true }))
        .await;
    let answer = call.await.unwrap();
    assert!(answer.starts_with("Started session"), "{answer}");

    // Unanswered in time: expired.
    rig.core.bots.set_approval_wait(Duration::from_millis(500));
    let answer = rig
        .tool(
            &thread,
            "create_worktree",
            serde_json::json!({ "project": "sp-billing" }),
        )
        .await;
    assert!(answer.contains("didn't approve this in time"), "{answer}");
    assert!(
        rig.approvals(&bot)
            .iter()
            .any(|a| a.state == ApprovalState::Expired)
    );
    let _ = rig.core.sessions.interrupt(&thread).await;
    rig.core.shutdown().await;
}

/// A fake Pi runtime whose permission control is on and whose Landstrip
/// passes `doctor`.
fn fake_runtime(
    dir: &std::path::Path,
    enabled: bool,
) -> cypher_engine::pi::runtime::PiRuntimePaths {
    let paths = cypher_engine::pi::runtime::PiRuntimePaths::for_data_dir(dir);
    let current = &paths.current;
    std::fs::create_dir_all(current.join("extensions")).unwrap();
    std::fs::write(current.join("extensions/cypher-permissions.ts"), "").unwrap();
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    let bin = current
        .join("npm/node_modules/@landstrip")
        .join(format!("landstrip-{os}-{arch}"))
        .join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let landstrip = bin.join("landstrip");
    std::fs::write(
        &landstrip,
        "#!/bin/sh\necho '{\"ok\":true,\"implementation\":\"test\"}'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&landstrip, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(&paths.agent_dir).unwrap();
    std::fs::write(
        paths.agent_dir.join("permissions.json"),
        serde_json::json!({ "enabled": enabled }).to_string(),
    )
    .unwrap();
    paths
}

#[tokio::test(flavor = "multi_thread")]
async fn working_directly_gets_local_roots_a_write_level_and_a_gate() {
    let rig = rig(true);
    let bot = rig
        .bot(serde_json::json!({ "workDirectly": true, "coordinate": false }))
        .await;
    let bot_chat = rig.core.workspace.chat(&bot.chat_id).unwrap().unwrap();
    // The row's level is whatever a client wrote; the engine decides.
    let mut config = bot_chat.config.clone().unwrap();
    config.sandbox = SandboxLevel::DangerFullAccess;
    rig.mutate(
        serde_json::json!({ "op": "setChatConfig", "chatId": bot.chat_id, "config": config }),
    )
    .await;
    // A project on another device is reached only through sessions.
    rig.core
        .workspace
        .create_space("sp-remote", "dev-other", "/srv/billing", None, true)
        .unwrap();
    rig.mutate(serde_json::json!({
        "op": "updateBot", "botId": bot.id, "name": "Bob",
        "scope": BotScope {
            projects: ProjectGrant::Selected {
                space_ids: vec!["sp-billing".into(), "sp-remote".into()],
            },
            ..Default::default()
        },
    }))
    .await;
    rig.core
        .sessions
        .dispatch(
            &bot.chat_id,
            HarnessId::Pi,
            RunRequest {
                cwd: bot_chat.cwd.clone().unwrap(),
                ..run_request("Run the billing tests")
            },
            None,
        )
        .await
        .expect("the bot run starts");
    let (request, env) = wait_for_some("the bot run", || rig.run_of("Run the billing tests")).await;
    let env = env.unwrap();
    for tool in cypher_proto::BOT_DIRECT_TOOLS {
        assert!(env.tools.iter().any(|t| t == tool), "{tool}");
    }
    assert!(!env.tools.iter().any(|t| t == "propose_plan"));
    let billing = std::fs::canonicalize(rig.root.path().join("sp-billing")).unwrap();
    assert_eq!(env.workspace_roots, [billing.to_string_lossy().to_string()]);
    assert_eq!(request.sandbox, SandboxLevel::WorkspaceWrite);
    assert_eq!(
        request.cwd,
        bot_chat.cwd.clone().unwrap(),
        "cwd stays the bot's folder"
    );
    assert!(
        env.system_prompt
            .contains("in these checkouts on this device")
    );

    // Routine and peer threads get no roots and read-only.
    let mut peer = bot_chat.clone();
    peer.id = "peer-thread".into();
    peer.bot = Some(BotChat::new(&bot.id, BotOrigin::Peer));
    rig.core.workspace.import_chat_row(&peer).unwrap();
    assert_eq!(
        cypher_engine::bots::profile::run_sandbox(&rig.core.workspace, &peer),
        Some(SandboxLevel::ReadOnly)
    );
    let peer_env =
        cypher_engine::bots::profile::bot_env(&rig.core.workspace, &peer).expect("a bot env");
    assert!(peer_env.workspace_roots.is_empty());
    assert!(!peer_env.tools.iter().any(|t| t == "bash"));

    // The gate before each Pi tool call: no runtime, no tools.
    let gate = |args: serde_json::Value| rig.tool(&bot.chat_id, "direct_tool", args);
    let bash = serde_json::json!({ "tool": "bash", "command": "cargo test" });
    assert!(gate(bash.clone()).await.starts_with("Pi isn't set up"));
    let data = tempfile::tempdir().unwrap();
    rig.core
        .bots
        .set_pi_runtime(fake_runtime(data.path(), false));
    assert!(
        gate(bash.clone())
            .await
            .starts_with("Permission control is off on this device")
    );
    rig.core
        .bots
        .set_pi_runtime(fake_runtime(data.path(), true));
    assert_eq!(gate(bash.clone()).await, "allow");

    // Tainted: acting calls wait for the owner; reads don't.
    rig.core.bots.taint_thread(&bot.chat_id, "A peer answered");
    assert_eq!(
        gate(serde_json::json!({ "tool": "read", "path": "/x" })).await,
        "allow"
    );
    let client = rig.client();
    let chat_id = bot.chat_id.clone();
    let call =
        tokio::spawn(async move { tool(&client, &chat_id, "direct_tool", bash, None).await });
    let approval = wait_for_some("a gate approval", || {
        rig.approvals(&bot)
            .into_iter()
            .find(|a| a.state == ApprovalState::Pending)
    })
    .await;
    assert_eq!(approval.action, "Run `cargo test`");
    assert_eq!(approval.work_id, None);
    rig.mutate(serde_json::json!({ "op": "resolveBotApproval", "approvalId": approval.id, "approve": false }))
        .await;
    assert!(call.await.unwrap().starts_with("Your owner declined this"));

    // The thread's run settled after the taint, so the taint is gone.
    wait_for(
        || rig.session_status(&bot.chat_id) == Some(SessionStatus::Idle),
        "the bot run settled",
    )
    .await;
    rig.core
        .doc_host
        .queue_command(
            &bot.chat_id,
            SessionCommandPayload::Run {
                request: RunRequest {
                    cwd: bot_chat.cwd.clone().unwrap(),
                    ..run_request("again")
                },
                message_id: "again-1".into(),
                agent_prompt: None,
            },
        )
        .unwrap();
    wait_for_some("a second run", || rig.run_of("again")).await;
    // Until the host clears it, a call still asks; don't wait on that.
    rig.core.bots.set_approval_wait(Duration::from_millis(50));
    let mut cleared = false;
    for _ in 0..50 {
        if gate(serde_json::json!({ "tool": "bash", "command": "ls" })).await == "allow" {
            cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(cleared, "the taint ends when its run settles");

    // Work directly switched off applies to the next call.
    rig.mutate(serde_json::json!({
        "op": "updateBot", "botId": bot.id, "name": "Bob", "scope": billing_only(),
        "capabilities": { "workDirectly": false },
    }))
    .await;
    assert!(
        gate(serde_json::json!({ "tool": "bash", "command": "ls" }))
            .await
            .starts_with("Your owner turned off Work directly")
    );
    let statuses = common::command_statuses(&rig.core, &bot.chat_id);
    assert!(
        statuses
            .iter()
            .all(|(_, s, _)| *s != SessionCommandStatus::Rejected)
    );
    rig.core.shutdown().await;
}
