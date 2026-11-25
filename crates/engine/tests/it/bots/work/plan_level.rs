//! A plan's permission level (docs/design/agent-bots.md §6.6): proposed at
//! the workspace's default unless the owner's own words ask for another,
//! confirmed on approval, and changed by the owner alone — mid-run, reaching
//! every session the plan started, its coordinating session and the ones it
//! starts next, in their live processes.

use super::*;

use cypher_rpc::RpcService;

fn levels_of(levels: &support::LiveLevels, chat: &str) -> Vec<SandboxLevel> {
    levels
        .lock()
        .unwrap()
        .iter()
        .filter(|(id, _)| id == chat)
        .map(|(_, level)| *level)
        .collect()
}

fn session_level(rig: &Rig, session: &str) -> Option<SandboxLevel> {
    rig.core
        .workspace
        .chat_config(session)
        .map(|config| config.sandbox)
}

async fn agent_level(rig: &Rig, chat: &str) -> serde_json::Value {
    let reply = rig
        .core
        .agent_rpc_service()
        .handle(
            cypher_rpc::methods::PERMISSION_LEVEL,
            serde_json::json!({ "chatId": chat }),
        )
        .await
        .expect("a chat on this device is answered");
    let cypher_rpc::RpcReply::Value(reply) = reply else {
        panic!("a unary reply");
    };
    reply["level"].clone()
}

/// The owner types `text` in the workspace's chat; returns once it ran.
async fn owner_says(rig: &Rig, bot: &Bot, text: &str) {
    let chat = rig.core.workspace.chat(&bot.chat_id).unwrap().unwrap();
    rig.core
        .doc_host
        .queue_command(
            &bot.chat_id,
            SessionCommandPayload::Run {
                request: RunRequest {
                    cwd: chat.cwd.clone().unwrap(),
                    ..run_request(text)
                },
                message_id: format!("m-{}", text.len()),
                agent_prompt: None,
            },
        )
        .expect("queued");
    wait_for(
        || {
            rig.words
                .lock()
                .unwrap()
                .iter()
                .any(|(_, prompt, _)| prompt.contains(text))
        },
        "the owner's message to run",
    )
    .await;
    wait_for(
        || rig.session_status(&bot.chat_id) == Some(SessionStatus::Idle),
        "the workspace chat to settle",
    )
    .await;
}

fn plan(title: &str, level: Option<&str>, quote: Option<&str>) -> serde_json::Value {
    let mut plan = serde_json::json!({
        "title": title,
        "steps": [
            { "title": "Fix rounding", "project": "sp-billing" },
            { "title": "Run the tests", "project": "sp-billing" },
        ],
    });
    if let Some(level) = level {
        plan["level"] = level.into();
    }
    if let Some(quote) = quote {
        plan["level_quote"] = quote.into();
    }
    plan
}

fn work_titled(rig: &Rig, title: &str) -> BotWork {
    rig.core
        .workspace
        .read_bot_records()
        .work
        .into_iter()
        .find(|w| w.title == title)
        .expect("a work row")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_is_proposed_at_the_workspaces_level_unless_the_owners_words_ask() {
    let rig = rig(false);
    let bot = rig
        .bot(serde_json::json!({ "workDirectly": true, "coordinate": true }))
        .await;
    // The workspace's default for new plans.
    rig.mutate(serde_json::json!({
        "op": "updateBot", "botId": bot.id, "name": "Bob", "scope": billing_only(),
        "plans": { "level": "read-only" },
    }))
    .await;
    let strict = rig
        .tool(&bot.chat_id, "propose_plan", plan("Strict", None, None))
        .await;
    assert!(
        strict.contains("at the permission level Ask before edits"),
        "{strict}"
    );
    let work = work_titled(&rig, "Strict");
    assert_eq!(work.proposed_level, Some(SandboxLevel::ReadOnly));
    assert_eq!(work.level, None, "the owner hasn't chosen yet");
    assert_eq!(work.effective_level(), SandboxLevel::ReadOnly);

    // Back to the default: absent settings run at workspace-write.
    rig.mutate(serde_json::json!({
        "op": "updateBot", "botId": bot.id, "name": "Bob", "scope": billing_only(),
        "plans": {},
    }))
    .await;
    rig.tool(&bot.chat_id, "propose_plan", plan("Plain", None, None))
        .await;
    assert_eq!(
        work_titled(&rig, "Plain").effective_level(),
        SandboxLevel::WorkspaceWrite
    );

    // Higher than the workspace's: only on the owner's own words.
    let refused = rig
        .tool(
            &bot.chat_id,
            "propose_plan",
            plan("Wide", Some("danger-full-access"), None),
        )
        .await;
    assert!(refused.starts_with("Only your owner chooses"), "{refused}");
    owner_says(
        &rig,
        &bot,
        "Fix the build, and use full access for this one",
    )
    .await;
    let invented = rig
        .tool(
            &bot.chat_id,
            "propose_plan",
            plan(
                "Wide",
                Some("danger-full-access"),
                Some("run everything with full access"),
            ),
        )
        .await;
    assert!(
        invented.contains("isn't in what your owner wrote"),
        "{invented}"
    );
    let quoted = rig
        .tool(
            &bot.chat_id,
            "propose_plan",
            plan(
                "Wide",
                Some("danger-full-access"),
                Some("Use full access for this one"),
            ),
        )
        .await;
    assert!(
        quoted.contains("at the permission level Full access"),
        "{quoted}"
    );
    let wide = work_titled(&rig, "Wide");
    assert_eq!(wide.proposed_level, Some(SandboxLevel::DangerFullAccess));
    assert_eq!(
        wide.level_quote.as_deref(),
        Some("Use full access for this one")
    );
    // A stricter level needs no words: it only takes away.
    rig.tool(
        &bot.chat_id,
        "propose_plan",
        plan("Careful", Some("read-only"), None),
    )
    .await;
    assert_eq!(
        work_titled(&rig, "Careful").effective_level(),
        SandboxLevel::ReadOnly
    );

    // Approving confirms a level: the one picked on the card, else the
    // proposal's.
    rig.mutate(serde_json::json!({
        "op": "resolveBotPlan", "workId": wide.id, "approve": true, "level": "workspace-write",
    }))
    .await;
    assert_eq!(rig.work(&wide.id).level, Some(SandboxLevel::WorkspaceWrite));
    let careful = work_titled(&rig, "Careful");
    rig.mutate(
        serde_json::json!({ "op": "resolveBotPlan", "workId": careful.id, "approve": true }),
    )
    .await;
    assert_eq!(rig.work(&careful.id).level, Some(SandboxLevel::ReadOnly));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_changes_a_running_plans_level_in_every_session_at_once() {
    let levels = support::LiveLevels::default();
    let rig = rig_with(false, |harness| harness.live_levels(levels.clone()));
    let bot = rig
        .bot(serde_json::json!({ "workDirectly": true, "coordinate": true }))
        .await;
    let work_id = rig.propose(&bot, "Fix rounding").await;
    assert_eq!(
        rig.work(&work_id).proposed_level,
        Some(SandboxLevel::WorkspaceWrite)
    );
    let thread = rig.approve(&work_id).await;
    // The coordinating session runs at the plan's level (it works directly
    // in a local checkout).
    let (brief, _) = wait_for_some("the brief run", || {
        rig.run_of("Carry out this approved plan")
    })
    .await;
    assert_eq!(brief.sandbox, SandboxLevel::WorkspaceWrite);
    assert_eq!(
        rig.work(&work_id).applied_level,
        Some(SandboxLevel::WorkspaceWrite)
    );
    // A session it starts: the plan's level, whatever the agent asks for.
    let first = session_id_of(
        &rig.tool(
            &thread,
            "start_session",
            serde_json::json!({
                "step": 1, "project": "sp-billing", "prompt": "long",
                "level": "danger-full-access", "sandbox": "danger-full-access",
            }),
        )
        .await,
    );
    assert_eq!(
        session_level(&rig, &first),
        Some(SandboxLevel::WorkspaceWrite)
    );
    wait_for(
        || rig.session_status(&first) == Some(SessionStatus::Working),
        "the first session to run",
    )
    .await;

    // Nothing an agent can reach sets it: not the agent socket's Mutate,
    // not a workspace tool, not the bot tool socket.
    let agent = rig.core.agent_rpc_service();
    let mutate = serde_json::json!({
        "op": "setBotWorkLevel", "workId": work_id, "level": "danger-full-access",
    });
    assert!(
        agent
            .handle(cypher_rpc::methods::MUTATE, mutate.clone())
            .await
            .is_err(),
        "the agent socket has no Mutate"
    );
    for (chat, name) in [
        (&thread, "setBotWorkLevel"),
        (&thread, "set_work_level"),
        (&bot.chat_id, "set_plan_level"),
    ] {
        let reply = agent
            .handle(
                cypher_rpc::methods::BOT_TOOL,
                serde_json::json!({
                    "chatId": chat, "tool": name,
                    "args": { "work_id": work_id, "level": "danger-full-access" },
                }),
            )
            .await
            .expect("a bot tool answers");
        let cypher_rpc::RpcReply::Value(reply) = reply else {
            panic!("a unary reply");
        };
        assert_eq!(reply["isError"], true, "{name}: {reply}");
    }
    // The coordinator's own tools leave it alone too.
    rig.tool(
        &thread,
        "update_step",
        serde_json::json!({ "step": 1, "state": "running", "note": "level: danger-full-access" }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let work = rig.work(&work_id);
    assert_eq!(
        work.level,
        Some(SandboxLevel::WorkspaceWrite),
        "approval set it"
    );
    assert_eq!(work.effective_level(), SandboxLevel::WorkspaceWrite);
    assert_eq!(
        session_level(&rig, &first),
        Some(SandboxLevel::WorkspaceWrite)
    );
    assert!(levels.lock().unwrap().is_empty(), "no live change");
    assert_eq!(agent_level(&rig, &first).await, "workspace-write");

    // The owner switches it to Full access mid-run.
    rig.mutate(mutate).await;
    wait_for(
        || rig.work(&work_id).applied_level == Some(SandboxLevel::DangerFullAccess),
        "the host to apply the level",
    )
    .await;
    assert_eq!(
        session_level(&rig, &first),
        Some(SandboxLevel::DangerFullAccess)
    );
    wait_for(
        || {
            levels_of(&levels, &first) == [SandboxLevel::DangerFullAccess]
                && levels_of(&levels, &thread) == [SandboxLevel::DangerFullAccess]
        },
        "both live runs to be given Full access",
    )
    .await;
    assert_eq!(agent_level(&rig, &first).await, "danger-full-access");
    assert_eq!(agent_level(&rig, &thread).await, "danger-full-access");
    // Still the same processes: nothing was restarted for it.
    assert_eq!(rig.session_status(&first), Some(SessionStatus::Working));
    assert!(rig.activity(&bot).contains(&ActivityKind::PlanLevelChanged));
    let line = rig
        .core
        .workspace
        .read_bot_records()
        .activity
        .into_iter()
        .find(|a| a.kind == ActivityKind::PlanLevelChanged)
        .unwrap();
    assert_eq!(line.work_id.as_deref(), Some(work_id.as_str()));
    assert!(
        line.text.contains("from Edit project to Full access"),
        "{}",
        line.text
    );

    // A session it starts from now on starts there.
    let second = session_id_of(
        &rig.tool(
            &thread,
            "start_session",
            serde_json::json!({ "step": 2, "project": "sp-billing", "prompt": "long" }),
        )
        .await,
    );
    assert_eq!(
        session_level(&rig, &second),
        Some(SandboxLevel::DangerFullAccess)
    );
    let started = wait_for_some("the second session's run", || {
        rig.runs
            .lock()
            .unwrap()
            .iter()
            .filter(|(r, _)| r.prompt == "long")
            .nth(1)
            .map(|(r, _)| r.sandbox)
    })
    .await;
    assert_eq!(started, SandboxLevel::DangerFullAccess);

    // And back: every live run takes the stricter level as well.
    rig.mutate(serde_json::json!({
        "op": "setBotWorkLevel", "workId": work_id, "level": "workspace-write",
    }))
    .await;
    wait_for(
        || {
            levels_of(&levels, &first).last() == Some(&SandboxLevel::WorkspaceWrite)
                && levels_of(&levels, &thread).last() == Some(&SandboxLevel::WorkspaceWrite)
                && levels_of(&levels, &second).last() == Some(&SandboxLevel::WorkspaceWrite)
        },
        "every live run to be given the stricter level",
    )
    .await;
    assert_eq!(
        session_level(&rig, &second),
        Some(SandboxLevel::WorkspaceWrite)
    );

    // An ended plan's level is history.
    rig.mutate(serde_json::json!({ "op": "stopBotWork", "workId": work_id }))
        .await;
    wait_for(|| !rig.work(&work_id).is_live(), "the plan to stop").await;
    let late = rig
        .client()
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "setBotWorkLevel", "workId": work_id, "level": "danger-full-access",
            }),
        )
        .await;
    assert!(late.is_err(), "{late:?}");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sessions_own_level_change_reaches_its_live_run() {
    let levels = support::LiveLevels::default();
    let rig = rig_with(false, |harness| harness.live_levels(levels.clone()));
    let config = |level: &str| serde_json::json!({ "harness": "pi", "model": null, "reasoning": null, "sandbox": level });
    rig.mutate(serde_json::json!({
        "op": "setChatConfig", "chatId": "chat-billing", "config": config("workspace-write"),
    }))
    .await;
    let cwd = rig.root.path().join("sp-billing");
    rig.core
        .sessions
        .dispatch(
            "chat-billing",
            HarnessId::Pi,
            RunRequest {
                cwd: cwd.to_string_lossy().into_owned(),
                ..run_request("long")
            },
            None,
        )
        .await
        .expect("the run starts");
    wait_for(
        || rig.session_status("chat-billing") == Some(SessionStatus::Working),
        "the run to start",
    )
    .await;
    // `/permissions` mid-run: the live process gets it, no restart.
    rig.mutate(serde_json::json!({
        "op": "setChatConfig", "chatId": "chat-billing", "config": config("danger-full-access"),
    }))
    .await;
    wait_for(
        || levels_of(&levels, "chat-billing") == [SandboxLevel::DangerFullAccess],
        "the live run to be given Full access",
    )
    .await;
    assert_eq!(
        rig.core.sessions.live_level("chat-billing"),
        Some(SandboxLevel::DangerFullAccess)
    );
    assert_eq!(
        rig.session_status("chat-billing"),
        Some(SessionStatus::Working)
    );
    rig.mutate(serde_json::json!({
        "op": "setChatConfig", "chatId": "chat-billing", "config": config("read-only"),
    }))
    .await;
    wait_for(
        || levels_of(&levels, "chat-billing").last() == Some(&SandboxLevel::ReadOnly),
        "the live run to be given read-only",
    )
    .await;
    assert_eq!(agent_level(&rig, "chat-billing").await, "read-only");
    rig.core.shutdown().await;
}
