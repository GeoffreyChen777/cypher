//! A bot's coordination against a real engine (docs/design/agent-bots.md
//! §6, P2): plans proposed and approved over `Mutate`, the orchestration
//! thread started from a brief, worker sessions driven through the command
//! plane, Stop, limits, capability refusals, approvals, and *Work directly*'s
//! roots and permission level.

use crate::support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;

use cypher_doc::{MessagePart, SessionCommandPayload, SessionCommandStatus};
use cypher_engine::EngineCore;
use cypher_proto::{
    self as p, ActivityKind, ApprovalState, Bot, BotApproval, BotChat, BotOrigin, BotScope,
    BotThread, BotWork, DoneStatus, HarnessId, ProjectGrant, RunRequest, SandboxLevel,
    SessionStatus, StepState, UserInputQuestion, WorkState,
};

use support::{run_request, wait_for, wait_for_some};

mod plan_level;

type Runs = Arc<Mutex<Vec<(RunRequest, Option<cypher_harness::BotRunEnv>)>>>;
/// Each run's chat, prompt and the owner's words it was given
/// (`CYPHER_OWNER_WORDS`).
type Words = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

/// A stand-in Pi. By prompt: the orchestration brief and `long` run until
/// interrupted (`finish` makes the brief end at once with "All done."),
/// `ask` asks which table to migrate and reports the answer, anything else
/// answers "ok".
fn harness(runs: Runs, words: Words, finish: bool) -> support::TestHarness {
    support::TestHarness::new(HarnessId::Pi, "Stand-in", move |request, controls| {
        runs.lock()
            .unwrap()
            .push((request.clone(), controls.host.profile.bot().cloned()));
        words.lock().unwrap().push((
            controls.host.chat_id.clone().unwrap_or_default(),
            request.prompt.clone(),
            controls.host.owner_words.clone(),
        ));
        let started =
            support::session_started(HarnessId::Pi, "mock-1", &request.cwd, "sess", "a-1");
        let brief = request.prompt.starts_with("Carry out this approved plan");
        if brief && finish {
            return support::reply(HarnessId::Pi, &request, "sess", "All done.");
        }
        // The time warning reaches this stand-in as a turn of its own (a real
        // Pi takes it mid-turn): the coordinator carries on.
        let warned = request
            .prompt
            .contains("left before this plan's time limit");
        if brief || warned || request.prompt == "long" {
            let interrupt = controls.interrupt.clone();
            let rest = futures::stream::once(async move {
                interrupt.cancelled().await;
                futures::stream::iter([Ok(support::done_with(
                    DoneStatus::Interrupted,
                    Some("sess"),
                ))])
            })
            .flatten();
            // The brief's run says something before it holds, so a cut-off
            // thread leaves half an answer behind.
            let opening = if brief {
                vec![Ok(started), Ok(support::text("On it."))]
            } else {
                vec![Ok(started)]
            };
            return Ok(futures::stream::iter(opening).chain(rest).boxed());
        }
        if request.prompt == "ask" {
            let answer = (controls.request_input)(vec![UserInputQuestion {
                id: "q1".into(),
                header: "Table".into(),
                question: "Migrate invoices or credits?".into(),
                options: vec!["invoices".into(), "credits".into()],
                multi_select: false,
                permission: None,
                asked_by: None,
            }]);
            let rest = futures::stream::once(async move {
                let picked = answer
                    .await
                    .ok()
                    .and_then(|answers| answers.into_iter().next())
                    .map(|a| a.labels.join(","))
                    .unwrap_or_default();
                futures::stream::iter([
                    Ok(support::text(&format!("Migrated {picked}."))),
                    Ok(support::done("sess")),
                ])
            })
            .flatten();
            return Ok(futures::stream::iter([Ok(started)]).chain(rest).boxed());
        }
        support::reply(HarnessId::Pi, &request, "sess", "ok")
    })
}

struct Rig {
    core: EngineCore,
    runs: Runs,
    words: Words,
    root: tempfile::TempDir,
}

fn rig(finish: bool) -> Rig {
    rig_with(finish, |harness| harness)
}

/// [`rig`], with its stand-in Pi tuned by `tune`.
fn rig_with(finish: bool, tune: impl FnOnce(support::TestHarness) -> support::TestHarness) -> Rig {
    let root = tempfile::tempdir().unwrap();
    let runs = Runs::default();
    let words = Words::default();
    let core = support::engine_at(
        &root.path().join("data"),
        tune(harness(runs.clone(), words.clone(), finish)),
    );
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
    Rig {
        core,
        runs,
        words,
        root,
    }
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
    assert_eq!(
        chat.started_by,
        Some(p::ChatStartedBy {
            bot_id: bot.id.clone(),
            work_id: Some(work_id.clone()),
        }),
        "marked as its workspace's, so the Sessions view leaves it out"
    );
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

    // The workspace's chat is told once: a post naming the plan and how it
    // ended, with the result as its text, then a turn of the workspace's
    // agent that carries the result and asks for a short answer.
    let post_id = p::work_post_id(&work_id);
    let event_id = format!("{}{post_id}", p::BOT_EVENT_PREFIX);
    wait_for(
        || {
            support::entries(&rig.core, &bot.chat_id)
                .iter()
                .any(|e| e.id == event_id)
        },
        "the plan's notice",
    )
    .await;
    let entries = support::entries(&rig.core, &bot.chat_id);
    let post = entries.iter().find(|e| e.id == post_id).expect("the post");
    assert!(matches!(
        &post.parts[..],
        [MessagePart::Post { text, from: p::BotPost::Work { work_id: id, title, outcome, .. }, .. }]
            if text == "Migrated invoices."
                && *id == work_id
                && title == "Migrate"
                && *outcome == WorkState::Done
    ));
    let notice_runs = || -> Vec<String> {
        rig.runs
            .lock()
            .unwrap()
            .iter()
            .map(|(r, _)| r.prompt.clone())
            // (After the status of its plans, which every turn there carries.)
            .filter(|prompt| prompt.contains("[Plan \"Migrate\" finished]\n\nMigrated invoices."))
            .collect()
    };
    wait_for(|| !notice_runs().is_empty(), "the agent's turn").await;
    let told = notice_runs();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("one or two sentences"), "{}", told[0]);
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
    // Running work can't be deleted; it has to be stopped first.
    let refused = rig
        .client()
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({ "op": "deleteBotWork", "workId": work_id }),
        )
        .await;
    assert!(
        refused.is_err_and(|err| err.to_string().contains("Stop or cancel")),
        "a running item stays"
    );

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

    // Deleted, it takes its background session; the project's stays.
    rig.mutate(serde_json::json!({ "op": "deleteBotWork", "workId": work_id }))
        .await;
    assert!(
        rig.core
            .workspace
            .record::<BotWork>(cypher_proto::KIND_BOT_WORK, &work_id)
            .is_none()
    );
    assert!(rig.core.workspace.chat(&thread).unwrap().is_none());
    assert!(rig.core.workspace.chat(&session).unwrap().is_some());
    rig.core.shutdown().await;
}

/// The request id a `wait_for_session` answer names.
fn request_id_of(text: &str) -> String {
    text.split("request ")
        .nth(1)
        .and_then(|rest| rest.split([';', ')']).next())
        .unwrap_or_else(|| panic!("no request id in {text}"))
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wait_hands_a_question_over_once_then_waits_for_the_owner() {
    let rig = rig(false);
    let bot = rig.bot(coordinate()).await;
    let work_id = rig.propose(&bot, "Ask the owner").await;
    let thread = rig.approve(&work_id).await;
    let session = session_id_of(
        &rig.tool(
            &thread,
            "start_session",
            serde_json::json!({ "step": 1, "project": "sp-billing", "prompt": "ask" }),
        )
        .await,
    );
    wait_for(
        || rig.session_status(&session) == Some(SessionStatus::AwaitingInput),
        "the session to ask",
    )
    .await;

    // The first wait reads the question out at once.
    let wait = |timeout_s: u64| {
        rig.tool(
            &thread,
            "wait_for_session",
            serde_json::json!({ "session_id": session, "timeout_s": timeout_s }),
        )
    };
    let first = wait(30).await;
    assert!(first.contains("is asking a question"), "{first}");
    assert!(first.contains("options: invoices | credits"), "{first}");
    let request_id = request_id_of(&first);

    // The plan doesn't say, so the thread waits again: this time the wait
    // holds until it runs out, the question still the owner's.
    let began = tokio::time::Instant::now();
    let second = wait(1).await;
    assert!(
        began.elapsed() >= Duration::from_millis(900),
        "the second wait returned after {:?}",
        began.elapsed()
    );
    assert!(
        second.contains("still waiting on the same question; your owner hasn't answered"),
        "{second}"
    );
    assert_eq!(rig.work(&work_id).state, WorkState::Running);
    assert_eq!(rig.work(&work_id).steps[0].state, StepState::Asking);

    // The owner answers in the session while the thread waits: the wait
    // ends with the session's result, and the plan goes on.
    let (third, ()) = tokio::join!(wait(30), async {
        tokio::time::sleep(Duration::from_millis(400)).await;
        rig.core
            .doc_host
            .queue_command(
                &session,
                SessionCommandPayload::RespondInput {
                    request_id: request_id.clone(),
                    answers: vec![cypher_proto::UserInputAnswer {
                        question_id: "q1".into(),
                        labels: vec!["credits".into()],
                    }],
                },
            )
            .unwrap();
    });
    assert!(third.contains("finished its run"), "{third}");
    assert!(third.contains("Migrated credits."), "{third}");
    assert_eq!(rig.work(&work_id).state, WorkState::Running);

    // Stop ends a wait on a question nobody answers.
    let asking = session_id_of(
        &rig.tool(
            &thread,
            "start_session",
            serde_json::json!({ "step": 2, "project": "sp-billing", "prompt": "ask" }),
        )
        .await,
    );
    wait_for(
        || rig.session_status(&asking) == Some(SessionStatus::AwaitingInput),
        "the second session to ask",
    )
    .await;
    let wait_on = |timeout_s: u64| {
        rig.tool(
            &thread,
            "wait_for_session",
            serde_json::json!({ "session_id": asking, "timeout_s": timeout_s }),
        )
    };
    assert!(wait_on(30).await.contains("is asking a question"));
    let began = tokio::time::Instant::now();
    let (stopped, ()) = tokio::join!(wait_on(60), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        rig.mutate(serde_json::json!({ "op": "stopBotWork", "workId": work_id }))
            .await;
    });
    assert!(began.elapsed() < Duration::from_secs(20), "{stopped}");
    wait_for(
        || rig.work(&work_id).state == WorkState::Stopped,
        "the work stopped",
    )
    .await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn however_a_plan_ends_its_open_steps_end_with_it() {
    let rig = rig(false);
    let bot = rig.bot(coordinate()).await;

    // finish_work with a step under way and one never started.
    let failed = rig.propose(&bot, "Fails").await;
    let thread = rig.approve(&failed).await;
    let step = rig
        .tool(
            &thread,
            "update_step",
            serde_json::json!({ "step": 1, "state": "running", "note": "n".repeat(500) }),
        )
        .await;
    assert!(step.starts_with("Step 1 is running."), "{step}");
    assert!(step.contains("note was cut to 200 characters"), "{step}");
    let long = rig
        .tool(
            &thread,
            "finish_work",
            serde_json::json!({ "outcome": "failed", "result": "r".repeat(30_001) }),
        )
        .await;
    assert!(
        long.contains("has 30001 characters, 1 over the 30000 it holds"),
        "{long}"
    );
    assert!(long.contains("Nothing is cut"), "{long}");
    assert_eq!(rig.work(&failed).state, WorkState::Running, "refused");
    let finished = rig
        .tool(
            &thread,
            "finish_work",
            serde_json::json!({ "outcome": "failed", "result": "Blocked." }),
        )
        .await;
    assert!(finished.starts_with("Recorded."), "{finished}");
    let work = rig.work(&failed);
    assert_eq!(work.state, WorkState::Failed);
    assert_eq!(
        work.steps.iter().map(|s| s.state).collect::<Vec<_>>(),
        [StepState::Failed, StepState::Skipped]
    );
    assert!(work.steps.iter().all(|s| s.ended_at == work.ended_at));
    rig.core.sessions.interrupt(&thread).await.unwrap();

    // A thread cut off mid-turn, with no finish_work: failed, not done.
    let cut = rig.propose(&bot, "Cut off").await;
    let thread = rig.approve(&cut).await;
    rig.tool(
        &thread,
        "update_step",
        serde_json::json!({ "step": 1, "state": "running" }),
    )
    .await;
    wait_for(
        || rig.session_status(&thread) == Some(SessionStatus::Working),
        "the thread to run",
    )
    .await;
    rig.core.sessions.interrupt(&thread).await.unwrap();
    wait_for(
        || rig.work(&cut).state != WorkState::Running,
        "the cut-off work to settle",
    )
    .await;
    let work = rig.work(&cut);
    assert_eq!(work.state, WorkState::Failed);
    assert!(
        work.result.as_deref().is_some_and(|r| r
            .contains("was stopped before it finished the plan")
            && r.ends_with("On it.")),
        "{:?}",
        work.result
    );
    assert_eq!(
        work.steps.iter().map(|s| s.state).collect::<Vec<_>>(),
        [StepState::Failed, StepState::Skipped]
    );
    assert!(rig.activity(&bot).contains(&ActivityKind::WorkFailed));

    // A row an older engine left: ended, a step still running. The host
    // closes it on its next pass.
    let mut stale = rig.work(&cut);
    stale.id = "stale-work".into();
    stale.thread_chat_id = None;
    stale.state = WorkState::Done;
    stale.steps[0].state = StepState::Running;
    stale.steps[0].ended_at = None;
    stale.steps[1].state = StepState::Pending;
    rig.core
        .workspace
        .put_record(
            cypher_proto::KIND_BOT_WORK,
            &stale.id,
            &stale,
            cypher_engine::RecordFields::All,
        )
        .unwrap();
    wait_for(
        || {
            rig.work("stale-work")
                .steps
                .iter()
                .map(|s| s.state)
                .collect::<Vec<_>>()
                == [StepState::Done, StepState::Skipped]
        },
        "the host to close a stale row's steps",
    )
    .await;
    assert!(rig.work("stale-work").steps[0].ended_at.is_some());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_is_refused_rather_than_cut() {
    let rig = rig(false);
    let bot = rig.bot(coordinate()).await;
    let propose = |steps: serde_json::Value| {
        rig.tool(
            &bot.chat_id,
            "propose_plan",
            serde_json::json!({ "title": "Long plan", "steps": steps }),
        )
    };
    // The details share one budget, whether one step uses it or several,
    // and the refusal says by how much it is over.
    let long_step = propose(serde_json::json!([
        { "title": "a", "project": "sp-billing", "detail": "d".repeat(30_001) },
    ]))
    .await;
    assert!(
        long_step.contains("add up to 30001 characters, 1 over the 30000 a plan holds in all"),
        "{long_step}"
    );
    let long_plan = propose(serde_json::json!([
        { "title": "a", "detail": "d".repeat(10_000) },
        { "title": "b", "detail": "d".repeat(10_000) },
        { "title": "c", "detail": "d".repeat(10_250) },
    ]))
    .await;
    assert!(
        long_plan.contains("add up to 30250 characters, 250 over the 30000"),
        "{long_plan}"
    );
    assert!(long_plan.contains("Nothing is cut for you"), "{long_plan}");
    let long_title = propose(serde_json::json!([{ "title": "t".repeat(161) }])).await;
    assert!(
        long_title.contains("Step 1's title has 161"),
        "{long_title}"
    );
    assert!(
        rig.core.workspace.read_bot_records().work.is_empty(),
        "a refused plan writes nothing"
    );
    assert!(
        plan_bodies(&rig, &bot.chat_id).is_empty(),
        "nor any full text"
    );
    // A refusal is flagged, so the extension answers it as an error result
    // (and the chat draws no plan card for it); an answer is not.
    let raw = |steps: serde_json::Value| {
        let client = rig.client();
        let chat_id = bot.chat_id.clone();
        async move {
            client
                .call(
                    cypher_rpc::methods::BOT_TOOL,
                    serde_json::json!({
                        "chatId": chat_id, "tool": "propose_plan",
                        "args": { "title": "Long plan", "steps": steps },
                    }),
                )
                .await
                .expect("BotTool answers")
        }
    };
    let refused = raw(serde_json::json!([{ "title": "t".repeat(161) }])).await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert_eq!(
        refused["text"].as_str(),
        Some(long_title.as_str()),
        "the model reads the same words"
    );

    // At the caps it is stored whole, and the brief carries every character:
    // one step may hold every detail character a plan has.
    rig.core
        .doc_host
        .open(&bot.chat_id)
        .unwrap()
        .write_user_message("m-long", &"词".repeat(30_050), 1_000)
        .unwrap();
    let detail = format!("{}END", "设".repeat(29_997));
    let kept = raw(serde_json::json!([
        { "title": "a", "project": "sp-billing", "detail": detail },
    ]))
    .await;
    assert!(kept.get("isError").is_none(), "{kept}");
    let kept = kept["text"].as_str().expect("text").to_owned();
    assert!(kept.contains("waiting for your owner's approval"), "{kept}");
    // Chats tell an accepted call from an older build's unflagged refusal by
    // these words (bot_plan.rs `PLAN_MADE_PREFIX`, iOS `planMadePrefix`).
    assert!(kept.starts_with("The plan \"Long plan\" (work "), "{kept}");
    // The owner's own message can't be refused; the model is told it was cut.
    assert!(
        kept.contains(
            "has 30050 characters and the background session is given only its first 30000"
        ),
        "{kept}"
    );
    let work = rig
        .core
        .workspace
        .read_bot_records()
        .work
        .into_iter()
        .next()
        .expect("the plan");
    // The row holds a preview; the chat doc the whole detail.
    assert!(work.body_rev.is_some());
    assert_ne!(work.steps[0].detail.as_deref(), Some(detail.as_str()));
    let body = plan_bodies(&rig, &bot.chat_id)
        .remove(&work.id)
        .and_then(|body| body.plan)
        .expect("the plan's body");
    assert_eq!(body.details["1"], detail);
    assert_eq!(body.instruction.chars().count(), p::INSTRUCTION_BODY_MAX);
    let thread = rig.approve(&work.id).await;
    let (request, _) = wait_for_some("the brief run", || {
        rig.run_of("Carry out this approved plan")
    })
    .await;
    assert!(request.prompt.contains(&detail), "the whole detail");
    assert!(
        request
            .prompt
            .contains("only its first 30000 characters, the rest was cut"),
        "{}",
        request.prompt
    );
    rig.mutate(serde_json::json!({ "op": "stopBotWork", "workId": work.id }))
        .await;
    wait_for(
        || rig.session_status(&thread) == Some(SessionStatus::Idle),
        "the thread stopped",
    )
    .await;
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
        in_main.contains("only works in a plan's background session"),
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
        in_thread.contains("only works in the workspace chat"),
        "{in_thread}"
    );
    let mut peer = rig.core.workspace.chat(&bot.chat_id).unwrap().unwrap();
    peer.id = "peer-thread".into();
    peer.bot = Some(BotChat::new(&bot.id, BotOrigin::Peer));
    rig.core.workspace.import_chat_row(&peer).unwrap();
    let from_peer = rig
        .tool("peer-thread", "list_work", serde_json::json!({}))
        .await;
    assert_eq!(from_peer, "list_work isn't available in this session.");

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
async fn a_plans_time_limit_models_and_limits_follow_the_owners_settings() {
    let rig = rig(false);
    let bot = rig.bot(coordinate()).await;
    rig.mutate(serde_json::json!({
        "op": "updateBot", "botId": bot.id, "name": "Bob", "scope": billing_only(),
        "plans": { "timeLimitMinutes": 120, "runningMax": 1, "sessionsMax": 3 },
        "sessionChoice": { "model": "ws-model", "thinking": "low" },
    }))
    .await;
    let bot = rig.core.workspace.bot(&bot.id).unwrap().expect("the bot");
    assert_eq!(bot.plans.time_limit_minutes(), 120);
    assert_eq!(bot.session_choice.model.as_deref(), Some("ws-model"));
    let refused = rig
        .client()
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "updateBot", "botId": bot.id, "name": "Bob", "scope": billing_only(),
                "plans": { "timeLimitMinutes": 5 },
            }),
        )
        .await;
    assert!(refused.is_err(), "a limit out of range is refused");

    rig.core
        .doc_host
        .open(&bot.chat_id)
        .unwrap()
        .write_user_message("m-1", "Fix it with Opus, give it 90 minutes", 1_000)
        .unwrap();
    let propose = |extra: serde_json::Value| {
        let mut args = serde_json::json!({
            "title": "Fix rounding",
            "steps": [
                {
                    "title": "Fix", "project": "sp-billing",
                    "session": { "model": "step-model", "thinking": "xhigh", "fast": true },
                },
                { "title": "Test", "project": "sp-billing" },
            ],
        });
        for (key, value) in extra.as_object().unwrap() {
            args[key] = value.clone();
        }
        args
    };
    let bad = rig
        .tool(
            &bot.chat_id,
            "propose_plan",
            propose(serde_json::json!({ "time_limit_minutes": 5 })),
        )
        .await;
    assert!(bad.contains("out of range"), "{bad}");
    let bad = rig
        .tool(
            &bot.chat_id,
            "propose_plan",
            propose(serde_json::json!({ "coordinator": { "thinking": "huge" } })),
        )
        .await;
    assert!(bad.contains("is no thinking level"), "{bad}");
    let text = tool(
        &rig.client(),
        &bot.chat_id,
        "propose_plan",
        propose(serde_json::json!({
            "time_limit_minutes": 90,
            "coordinator": { "model": "coord-model", "thinking": "high" },
        })),
        Some("call-1"),
    )
    .await;
    assert!(text.contains("it may take 1 hour 30 minutes"), "{text}");
    let work_id = rig
        .core
        .workspace
        .read_bot_records()
        .work
        .into_iter()
        .find(|w| w.title == "Fix rounding")
        .expect("the plan")
        .id;
    let work = rig.work(&work_id);
    assert_eq!(work.time_limit_minutes, Some(90));
    assert_eq!(
        work.coordinator_choice.model.as_deref(),
        Some("coord-model")
    );
    assert_eq!(
        work.steps[0].session_choice.thinking,
        Some(p::ReasoningLevel::XHigh)
    );

    // The coordinating session runs on the plan's choice, and its brief
    // says how long it has and how many sessions it may start.
    let thread = rig.approve(&work_id).await;
    let (brief, _) =
        wait_for_some("the brief", || rig.run_of("Carry out this approved plan")).await;
    assert_eq!(brief.model.as_deref(), Some("coord-model"));
    assert_eq!(brief.reasoning, Some(p::ReasoningLevel::High));
    assert!(
        brief
            .prompt
            .contains("stopped 1 hour 30 minutes after it started"),
        "{}",
        brief.prompt
    );
    assert!(
        brief.prompt.contains("at most 3 in all"),
        "{}",
        brief.prompt
    );
    assert!(
        brief
            .prompt
            .contains("Its sessions use step-model, xhigh thinking, Fast mode"),
        "{}",
        brief.prompt
    );

    // A session takes the call's choice, else its step's, else the
    // workspace's for sessions.
    for (step, prompt, extra) in [
        (1, "one", serde_json::json!({})),
        (2, "two", serde_json::json!({})),
        (
            2,
            "three",
            serde_json::json!({ "model": "call-model", "fast": false }),
        ),
    ] {
        let mut args =
            serde_json::json!({ "step": step, "project": "sp-billing", "prompt": prompt });
        for (key, value) in extra.as_object().unwrap() {
            args[key] = value.clone();
        }
        let started = rig.tool(&thread, "start_session", args).await;
        assert!(started.starts_with("Started session"), "{started}");
    }
    let fast = |r: &RunRequest| r.model_options.get("fast").and_then(|v| v.as_bool());
    let (one, _) = wait_for_some("run one", || rig.run_of("one")).await;
    assert_eq!(
        (one.model.as_deref(), one.reasoning, fast(&one)),
        (
            Some("step-model"),
            Some(p::ReasoningLevel::XHigh),
            Some(true)
        )
    );
    let (two, _) = wait_for_some("run two", || rig.run_of("two")).await;
    assert_eq!(
        (two.model.as_deref(), two.reasoning, fast(&two)),
        (Some("ws-model"), Some(p::ReasoningLevel::Low), None)
    );
    let (three, _) = wait_for_some("run three", || rig.run_of("three")).await;
    assert_eq!(
        (three.model.as_deref(), three.reasoning, fast(&three)),
        (
            Some("call-model"),
            Some(p::ReasoningLevel::Low),
            Some(false)
        )
    );
    let fourth = rig
        .tool(
            &thread,
            "start_session",
            serde_json::json!({ "step": 2, "project": "sp-billing", "prompt": "four" }),
        )
        .await;
    assert!(fourth.contains("has started 3 sessions"), "{fourth}");

    // One plan at a time: the next approved one waits.
    let next = rig.propose(&bot, "Next").await;
    rig.mutate(serde_json::json!({ "op": "resolveBotPlan", "workId": next, "approve": true }))
        .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(rig.work(&next).state, WorkState::Proposed, "no slot");

    // Near its limit the coordinating session is told to wrap up, once.
    let started = p::now_ms() - 80 * 60_000;
    rig.core
        .workspace
        .update_record(
            p::KIND_BOT_WORK,
            &work_id,
            [("startedAt".to_string(), serde_json::json!(started))]
                .into_iter()
                .collect(),
        )
        .unwrap();
    let warnings = || {
        rig.core
            .doc_host
            .open(&thread)
            .unwrap()
            .doc()
            .read_commands()
            .unwrap()
            .into_iter()
            .filter(|c| {
                matches!(
                    &c.payload,
                    SessionCommandPayload::Steer { agent_prompt: Some(text), .. }
                        if text.contains("Don't start anything new")
                )
            })
            .count()
    };
    wait_for(|| warnings() == 1, "the warning").await;
    assert!(rig.work(&work_id).warned_at.is_some());
    let details = rig
        .tool(
            &bot.chat_id,
            "get_work",
            serde_json::json!({ "work_id": work_id }),
        )
        .await;
    assert!(
        details.contains("Time limit 1 hour 30 minutes: about 9 minutes left")
            || details.contains("Time limit 1 hour 30 minutes: about 10 minutes left"),
        "{details}"
    );

    // The owner adds an hour: no stop at the old limit, and a new warning
    // only near the new one.
    rig.mutate(serde_json::json!({ "op": "addBotWorkTime", "workId": work_id, "minutes": 60 }))
        .await;
    assert_eq!(rig.work(&work_id).time_added_minutes, Some(60));
    let refused = rig
        .client()
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({ "op": "addBotWorkTime", "workId": next, "minutes": 60 }),
        )
        .await;
    assert!(refused.is_err(), "only a running plan gets more time");
    rig.core
        .workspace
        .update_record(
            p::KIND_BOT_WORK,
            &work_id,
            [(
                "startedAt".to_string(),
                serde_json::json!(p::now_ms() - 100 * 60_000),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        rig.work(&work_id).state,
        WorkState::Running,
        "100 minutes in, 150 allowed"
    );
    assert_eq!(warnings(), 1, "the new deadline is still far");

    // Past it, the plan stops and says why.
    rig.core
        .workspace
        .update_record(
            p::KIND_BOT_WORK,
            &work_id,
            [(
                "startedAt".to_string(),
                serde_json::json!(p::now_ms() - 151 * 60_000),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap();
    wait_for(
        || rig.work(&work_id).state == WorkState::Stopped,
        "stopped for time",
    )
    .await;
    assert_eq!(
        rig.work(&work_id).result.as_deref(),
        Some("It ran past its time limit of 2 hours 30 minutes, so it was stopped.")
    );
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
        .taint_thread(&thread, "Alice's workspace answered in this session");
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
    assert_eq!(
        approval.reason,
        "Alice's workspace answered in this session"
    );
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
        env.system_prompt.contains(&format!(
            "you run commands and edit files yourself in {}",
            billing.to_string_lossy()
        )),
        "{}",
        env.system_prompt
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
    let statuses = support::command_statuses(&rig.core, &bot.chat_id);
    assert!(
        statuses
            .iter()
            .all(|(_, s, _)| *s != SessionCommandStatus::Rejected)
    );
    rig.core.shutdown().await;
}

/// The owner's words a run of `chat` with `prompt` was given, parsed.
fn words_of(rig: &Rig, chat: &str, prompt: &str) -> Option<serde_json::Value> {
    rig.words
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(c, p, _)| c == chat && p == prompt)
        .map(|(_, _, words)| {
            serde_json::from_str(words.as_deref().expect("the run got the owner's words"))
                .expect("JSON")
        })
}

fn texts(words: &serde_json::Value) -> Vec<(String, String)> {
    words["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["source"].as_str().unwrap().to_string(),
                m["text"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Only the owner's words reach the permission check
/// (docs/design/permission-control.md §7.3): a plan's background thread and
/// its sessions get the instruction from the plan's record and what the
/// owner types into them, never the brief, the prompt a session starts
/// from, or the orchestrator's steering, which the engine marks as its
/// agents' in the command ledger.
#[tokio::test(flavor = "multi_thread")]
async fn a_plans_runs_carry_the_owners_words_never_the_agents() {
    let rig = rig(false);
    let bot = rig.bot(coordinate()).await;
    let work_id = rig.propose(&bot, "Ship").await;
    let thread = rig.approve(&work_id).await;
    let instruction = ("plan".to_string(), "Fix the invoice rounding".to_string());
    // The card the owner approved, from the plan's record (§7.3.4).
    let card = (
        "card".to_string(),
        "Ship\nFix it, then test it\n1. Fix rounding\nin a worktree\n2. Run the tests".to_string(),
    );
    let planned = [instruction.clone(), card.clone()];

    // The orchestration thread: the brief is the agent's.
    let brief = wait_for_some("the brief's run", || {
        rig.run_of("Carry out this approved plan")
    })
    .await
    .0
    .prompt;
    let words = wait_for_some("the thread's run", || words_of_opt(&rig, &thread, &brief)).await;
    assert_eq!(texts(&words), &planned);
    assert_eq!(words["agentWritten"], true);

    // A session it starts: its prompt is the agent's too.
    let started = rig
        .tool(
            &thread,
            "start_session",
            serde_json::json!({ "step": 1, "project": "sp-billing", "prompt": "open a PR to main and merge it" }),
        )
        .await;
    let session = session_id_of(&started);
    let words = wait_for_some("the session's run", || {
        words_of_opt(&rig, &session, "open a PR to main and merge it")
    })
    .await;
    assert_eq!(texts(&words), &planned);
    wait_for(
        || rig.session_status(&session) == Some(SessionStatus::Idle),
        "the session to settle",
    )
    .await;

    // The orchestrator steers it: a new turn, still only the owner's words.
    let steered = rig
        .tool(
            &thread,
            "steer_session",
            serde_json::json!({ "session_id": session, "message": "Merge PR 52 now." }),
        )
        .await;
    assert!(steered.starts_with("Sent your message"), "{steered}");
    let words = wait_for_some("the steered run", || {
        words_of_opt(&rig, &session, "Merge PR 52 now.")
    })
    .await;
    assert_eq!(texts(&words), &planned);
    wait_for(
        || rig.session_status(&session) == Some(SessionStatus::Idle),
        "the session to settle again",
    )
    .await;

    // The owner types into the session (the composer's command): theirs.
    let command = serde_json::to_value(SessionCommandPayload::Run {
        request: run_request("pr 给main 没问题合并"),
        message_id: "m-owner".into(),
        agent_prompt: None,
    })
    .unwrap();
    rig.client()
        .call(
            cypher_rpc::methods::QUEUE_COMMAND,
            serde_json::json!({ "chatId": session, "command": command }),
        )
        .await
        .expect("queued");
    let words = wait_for_some("the owner's run", || {
        words_of_opt(&rig, &session, "pr 给main 没问题合并")
    })
    .await;
    // Short, and right after the session's own answer: a reply to it, carrying it.
    assert!(
        words["messages"][2]["proposal"]
            .as_str()
            .is_some_and(|p| !p.is_empty()),
        "{words}"
    );
    // A run's process outlives the turn it was started for, so its
    // permission extension asks for the words as of now (`OwnerWords`, on
    // the agent socket): the same ones, read from the ledger.
    {
        use cypher_rpc::RpcService;
        let agent = rig.core.agent_rpc_service();
        let reply = agent
            .handle(
                cypher_rpc::methods::OWNER_WORDS,
                serde_json::json!({ "chatId": session }),
            )
            .await
            .expect("a chat on this device is answered");
        let cypher_rpc::RpcReply::Value(reply) = reply else {
            panic!("a unary reply");
        };
        assert_eq!(
            texts(&reply["words"]),
            [
                instruction.clone(),
                card.clone(),
                ("reply".to_string(), "pr 给main 没问题合并".to_string())
            ]
        );
        assert_eq!(reply["words"]["agentWritten"], true);
        assert!(
            agent
                .handle(
                    cypher_rpc::methods::OWNER_WORDS,
                    serde_json::json!({ "chatId": "no-such-chat" }),
                )
                .await
                .is_err(),
            "only a chat on this device"
        );
    }
    assert_eq!(
        texts(&words),
        [
            instruction,
            card,
            ("reply".to_string(), "pr 给main 没问题合并".to_string())
        ]
    );

    // The ledger says who wrote what.
    let authors: Vec<(String, Option<String>)> = rig
        .core
        .doc_host
        .open(&session)
        .unwrap()
        .doc()
        .read_commands()
        .unwrap()
        .into_iter()
        .filter_map(|c| match c.payload {
            SessionCommandPayload::Run { request, .. } => Some((request.prompt, c.author)),
            SessionCommandPayload::Steer { prompt, .. } => Some((prompt, c.author)),
            _ => None,
        })
        .collect();
    assert_eq!(
        authors,
        [
            (
                "open a PR to main and merge it".to_string(),
                Some("plan".to_string())
            ),
            ("Merge PR 52 now.".to_string(), Some("plan".to_string())),
            ("pr 给main 没问题合并".to_string(), None),
        ]
    );
}

fn words_of_opt(rig: &Rig, chat: &str, prompt: &str) -> Option<serde_json::Value> {
    let found = rig
        .words
        .lock()
        .unwrap()
        .iter()
        .any(|(c, p, _)| c == chat && p == prompt);
    found.then(|| words_of(rig, chat, prompt)).flatten()
}

// ── plan bodies (docs/design/plan-body.md) ─────────────────────────────────

/// Every `planBodies` entry in `chat`'s doc, by work id.
fn plan_bodies(rig: &Rig, chat: &str) -> std::collections::BTreeMap<String, p::WorkBodies> {
    rig.core.doc_host.open(chat).unwrap().doc().plan_bodies()
}

/// The two details of [`propose_long`]: about 20,000 characters, far over
/// what a row holds.
fn long_details() -> [String; 2] {
    [
        format!("{}END-1", "设".repeat(12_000)),
        format!("{}END-2", "detail ".repeat(1_150)),
    ]
}

/// The owner's message, then a plan with [`long_details`]; its work id.
async fn propose_long(rig: &Rig, bot: &Bot, title: &str) -> String {
    rig.core
        .doc_host
        .open(&bot.chat_id)
        .unwrap()
        .write_user_message(&format!("m-{title}"), "Rewrite the billing exports", 1_000)
        .unwrap();
    let [one, two] = long_details();
    let text = rig
        .tool(
            &bot.chat_id,
            "propose_plan",
            serde_json::json!({
                "title": title,
                "steps": [
                    { "title": "Rewrite", "project": "sp-billing", "detail": one },
                    { "title": "Check", "project": "sp-billing", "detail": two },
                ],
            }),
        )
        .await;
    assert!(text.contains("waiting for your owner's approval"), "{text}");
    rig.core
        .workspace
        .read_bot_records()
        .work
        .into_iter()
        .find(|w| w.title == title)
        .expect("a work row")
        .id
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_plan_keeps_its_full_text_in_the_workspace_chat() {
    let rig = rig(false);
    let bot = rig.bot(coordinate()).await;
    let work_id = propose_long(&rig, &bot, "Long").await;
    let [one, two] = long_details();

    // The row is the index: previews within today's caps, and the rev of
    // the full text, which is in the workspace chat's doc.
    let work = rig.work(&work_id);
    let rev = work.body_rev.clone().expect("a bodyRev");
    assert!(work.details_len() <= p::PLAN_DETAILS_MAX);
    assert!(work.steps[0].detail.as_deref().unwrap().ends_with('…'));
    assert_eq!(work.instruction, "Rewrite the billing exports");
    let bodies = plan_bodies(&rig, &bot.chat_id);
    let body = bodies[&work_id].plan.clone().expect("the plan's body");
    assert_eq!(body.rev, rev);
    assert!(body.is_intact());
    assert_eq!(body.details["1"], one);
    assert_eq!(body.details["2"], two);
    assert_eq!(body.instruction, "Rewrite the billing exports");
    assert_eq!(bodies[&work_id].result, None);
    let read = p::PlanText::of(&work, bodies.get(&work_id));
    assert_eq!(read.plan, p::TextSource::Body);

    // get_work and the brief read the body; list_work the row.
    let got = rig
        .tool(
            &bot.chat_id,
            "get_work",
            serde_json::json!({ "work_id": work_id }),
        )
        .await;
    assert!(
        got.contains(&format!("   detail: {one}")),
        "the whole detail"
    );
    assert!(got.contains(&format!("   detail: {two}")));
    assert!(!got.contains("missing on this device"), "{got}");
    let listed = rig
        .tool(&bot.chat_id, "list_work", serde_json::json!({}))
        .await;
    assert!(
        listed.contains(&work_id) && listed.len() < 1_000,
        "{listed}"
    );

    let thread = rig.approve(&work_id).await;
    let (request, _) = wait_for_some("the brief run", || {
        rig.run_of("Carry out this approved plan")
    })
    .await;
    assert!(request.prompt.contains("1. Rewrite — in sp-billing"));
    assert!(
        request.prompt.contains(&format!("   {one}\n")),
        "detail 1 whole"
    );
    assert!(
        request.prompt.contains(&format!("   {two}\n")),
        "detail 2 whole"
    );
    assert!(
        request
            .prompt
            .contains("\"\"\"\nRewrite the billing exports\n\"\"\"")
    );
    assert!(!request.prompt.contains('…'), "no preview in the brief");

    // A long result: refused over its cap, kept whole under it.
    let result = format!("Rewrote the exports. {}END", "结".repeat(8_000));
    let finished = rig
        .tool(
            &thread,
            "finish_work",
            serde_json::json!({ "outcome": "done", "result": result }),
        )
        .await;
    assert!(finished.starts_with("Recorded."), "{finished}");
    let work = rig.work(&work_id);
    assert_eq!(work.state, WorkState::Done);
    let result_rev = work.result_rev.clone().expect("a resultRev");
    let preview = work.result.clone().expect("a preview");
    assert!(preview.chars().count() <= p::RESULT_MAX && preview.ends_with('…'));
    let stored = plan_bodies(&rig, &bot.chat_id)[&work_id].clone();
    let stored_result = stored.result.expect("the result's body");
    assert_eq!(stored_result.rev, result_rev);
    assert_eq!(stored_result.text, result);
    assert_eq!(stored.plan, Some(body), "the plan's body is untouched");
    let got = rig
        .tool(
            &bot.chat_id,
            "get_work",
            serde_json::json!({ "work_id": work_id }),
        )
        .await;
    assert!(
        got.contains(&format!("Result:\n{result}")),
        "the whole result"
    );

    // The notice's post holds the whole result; the agent's turn a clip that
    // says where the rest is.
    let post_id = p::work_post_id(&work_id);
    let event_id = format!("{}{post_id}", p::BOT_EVENT_PREFIX);
    wait_for(
        || {
            support::entries(&rig.core, &bot.chat_id)
                .iter()
                .any(|e| e.id == event_id)
        },
        "the plan's notice",
    )
    .await;
    let entries = support::entries(&rig.core, &bot.chat_id);
    let post = entries.iter().find(|e| e.id == post_id).expect("the post");
    assert!(
        matches!(&post.parts[..], [MessagePart::Post { text, .. }] if *text == result),
        "the post is the whole result"
    );
    let told = || {
        rig.runs
            .lock()
            .unwrap()
            .iter()
            .map(|(r, _)| r.prompt.clone())
            .find(|prompt| prompt.contains("[Plan \"Long\" finished]"))
    };
    let told = wait_for_some("the agent's turn", told).await;
    assert!(
        told.contains(&format!(
            "characters are shown here; get_work with work_id {work_id} shows all of it."
        )),
        "{told}"
    );
    assert!(!told.contains("结END"), "the turn's copy is cut");
    rig.core.sessions.interrupt(&thread).await.unwrap();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_that_fits_its_row_writes_no_body() {
    let rig = rig(false);
    let bot = rig.bot(coordinate()).await;
    let work_id = rig.propose(&bot, "Short").await;
    let work = rig.work(&work_id);
    assert_eq!(work.body_rev, None);
    assert_eq!(work.steps[0].detail.as_deref(), Some("in a worktree"));
    let thread = rig.approve(&work_id).await;
    let finished = rig
        .tool(
            &thread,
            "finish_work",
            serde_json::json!({ "outcome": "done", "result": "r".repeat(p::RESULT_MAX) }),
        )
        .await;
    assert!(finished.starts_with("Recorded."), "{finished}");
    let work = rig.work(&work_id);
    assert_eq!(work.result_rev, None, "a result that fits is the row's");
    assert_eq!(
        work.result.as_deref(),
        Some("r".repeat(p::RESULT_MAX).as_str())
    );
    assert!(
        plan_bodies(&rig, &bot.chat_id).is_empty(),
        "nothing in the chat doc"
    );
    rig.core.sessions.interrupt(&thread).await.unwrap();
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_without_its_full_text_never_runs() {
    let rig = rig(false);
    let bot = rig
        .bot(serde_json::json!({
            "workDirectly": false, "coordinate": true,
        }))
        .await;
    // Three ways a body can fail the host's check: gone, another plan's
    // (another rev) and edited under its rev.
    let missing = propose_long(&rig, &bot, "Missing").await;
    let other = propose_long(&rig, &bot, "Other").await;
    let edited = propose_long(&rig, &bot, "Edited").await;
    let handle = rig.core.doc_host.open(&bot.chat_id).unwrap();
    assert!(handle.doc().delete_plan_body(&missing).unwrap());
    let replaced = p::PlanBody::new("Something else".into(), Default::default(), 1);
    handle.doc().put_plan_body(&other, &replaced).unwrap();
    let mut forged = plan_bodies(&rig, &bot.chat_id)[&edited]
        .plan
        .clone()
        .unwrap();
    forged.instruction = "Delete the billing exports".into();
    assert_eq!(Some(&forged.rev), rig.work(&edited).body_rev.as_ref());
    handle.doc().put_plan_body(&edited, &forged).unwrap();

    for id in [&missing, &other, &edited] {
        rig.mutate(serde_json::json!({ "op": "resolveBotPlan", "workId": id, "approve": true }))
            .await;
    }
    for id in [&missing, &other, &edited] {
        wait_for(
            || rig.work(id).state == WorkState::Failed,
            "the plan to fail",
        )
        .await;
        let work = rig.work(id);
        assert!(
            work.result
                .as_deref()
                .is_some_and(|r| r.starts_with("Its full text is missing on its host")),
            "{:?}",
            work.result
        );
        assert_eq!(work.thread_chat_id, None, "no thread was made");
        assert!(work.ended_at.is_some());
    }
    assert!(
        rig.run_of("Carry out this approved plan").is_none(),
        "nothing ran on the previews"
    );
    assert!(rig.activity(&bot).contains(&ActivityKind::WorkFailed));
    // The workspace's chat hears that it failed.
    let post_id = p::work_post_id(&missing);
    wait_for(
        || {
            support::entries(&rig.core, &bot.chat_id)
                .iter()
                .any(|e| e.id == post_id)
        },
        "the failure's notice",
    )
    .await;
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bodies_without_rows_are_cleaned_after_the_hour() {
    let rig = rig(false);
    let bot = rig.bot(coordinate()).await;
    let hour = 3_600_000;
    let now = p::now_ms();

    // A crash between the two writes: the body landed, its row never did.
    let orphan = p::PlanBody::new("Never proposed".into(), Default::default(), now);
    rig.core
        .doc_host
        .open(&bot.chat_id)
        .unwrap()
        .doc()
        .put_plan_body("crashed", &orphan)
        .unwrap();
    // A plan with its row, and one deleted elsewhere (any owner device may
    // delete a row; only the host cleans its body).
    let kept = propose_long(&rig, &bot, "Kept").await;
    let deleted = propose_long(&rig, &bot, "Deleted").await;
    rig.mutate(serde_json::json!({ "op": "resolveBotPlan", "workId": deleted, "approve": false }))
        .await;
    wait_for(
        || rig.work(&deleted).state == WorkState::Cancelled,
        "the plan cancelled",
    )
    .await;
    rig.mutate(serde_json::json!({ "op": "deleteBotWork", "workId": deleted }))
        .await;
    assert!(
        rig.core
            .workspace
            .record::<BotWork>(p::KIND_BOT_WORK, &deleted)
            .is_none()
    );
    let keys = |rig: &Rig| {
        plan_bodies(rig, &bot.chat_id)
            .into_keys()
            .collect::<Vec<_>>()
    };
    let mut all = vec!["crashed".to_string(), kept.clone(), deleted.clone()];
    all.sort();
    assert_eq!(keys(&rig), all);

    // Within the hour the window between the writes is respected.
    assert_eq!(rig.core.bots.clean_plan_bodies(now + hour / 2), 0);
    assert_eq!(keys(&rig), all);
    // Past it, both orphans go; the body with a row stays, however old.
    assert_eq!(rig.core.bots.clean_plan_bodies(now + 2 * hour), 2);
    assert_eq!(keys(&rig), std::slice::from_ref(&kept));
    assert_eq!(rig.core.bots.clean_plan_bodies(now + 100 * hour), 0);
    assert!(rig.work(&kept).body_rev.is_some());

    // Pruning takes a body with its row: one finished plan more than the
    // host keeps, the oldest going first.
    let handle = rig.core.doc_host.open(&bot.chat_id).unwrap();
    let mut row = rig.work(&kept);
    row.state = WorkState::Done;
    row.thread_chat_id = None;
    for i in 0..=p::KEEP_FINISHED_WORK {
        let id = format!("old-{i}");
        row.id = id.clone();
        row.ended_at = Some(1_000 + i as i64);
        let body = p::PlanBody::new(id.clone(), Default::default(), now);
        row.body_rev = Some(body.rev.clone());
        handle.doc().put_plan_body(&id, &body).unwrap();
        rig.core
            .workspace
            .put_record(
                p::KIND_BOT_WORK,
                &id,
                &row,
                cypher_engine::RecordFields::All,
            )
            .unwrap();
    }
    wait_for(
        || {
            rig.core
                .workspace
                .record::<BotWork>(p::KIND_BOT_WORK, "old-0")
                .is_none()
        },
        "the oldest plan pruned",
    )
    .await;
    let left = plan_bodies(&rig, &bot.chat_id);
    assert!(!left.contains_key("old-0"), "its body went with it");
    assert!(left.contains_key("old-1") && left.contains_key(&kept));
    rig.core.shutdown().await;
}
