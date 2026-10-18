//! Agent bots against a real engine (docs/design/agent-bots.md): creation
//! over RPC, the scope every bot tool enforces, edits taking effect on the
//! next call, and a bot chat's run reaching the harness locked down.

mod common;

use std::sync::{Arc, Mutex};

use futures::StreamExt;

use cypher_doc::{MessagePart, MessageRole, SessionCommandPayload};
use cypher_engine::EngineCore;
use cypher_engine::bots::profile::UnattendedStop;
use cypher_proto::{
    Bot, BotChat, BotOrigin, BotScope, HarnessId, PermissionPrompt, PermissionPromptKind,
    ProjectGrant, RunRequest, SessionStatus, UserInputAnswer, UserInputQuestion,
};

use common::{complete_assistant_count, run_request, wait_for};

/// Two projects with one session each; each transcript mentions "invoice".
fn seed(core: &EngineCore, root: &std::path::Path) {
    for (space, chat, text) in [
        (
            "sp-billing",
            "chat-billing",
            "Migrate the invoice tables to v2",
        ),
        ("sp-secret", "chat-secret", "Rotate the invoice signing key"),
    ] {
        let path = root.join(space);
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
        core.doc_host
            .open(chat)
            .unwrap()
            .write_user_message(&format!("{chat}-m1"), text, 1_000)
            .unwrap();
    }
}

async fn create_bot(core: &EngineCore, scope: BotScope) -> Bot {
    let client = cypher_rpc::memory_client(core.rpc_service());
    let value = client
        .call(
            cypher_rpc::methods::CREATE_BOT,
            serde_json::json!({
                "targetDeviceId": core.device_id,
                "name": "Billing bot",
                "icon": "rocket",
                "scope": scope,
            }),
        )
        .await
        .expect("CreateBot");
    serde_json::from_value(value).expect("a Bot")
}

async fn tool(core: &EngineCore, chat_id: &str, name: &str, args: serde_json::Value) -> String {
    let client = cypher_rpc::memory_client(core.rpc_service());
    let reply = client
        .call(
            cypher_rpc::methods::BOT_TOOL,
            serde_json::json!({ "chatId": chat_id, "tool": name, "args": args }),
        )
        .await
        .expect("BotTool answers");
    reply["text"].as_str().expect("text").to_string()
}

fn billing_only() -> BotScope {
    BotScope {
        projects: ProjectGrant::Selected {
            space_ids: vec!["sp-billing".into()],
        },
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bot_tools_see_only_the_granted_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let core = common::assemble_mock(&tmp.path().join("data"));
    seed(&core, tmp.path());
    let bot = create_bot(&core, billing_only()).await;

    // The bot's chat: hosted here, marked, named, with a private folder.
    let chat = core
        .workspace
        .chat(&bot.chat_id)
        .unwrap()
        .expect("bot chat");
    assert_eq!(
        chat.bot.as_ref().map(|b| b.bot_id.as_str()),
        Some(bot.id.as_str())
    );
    assert_eq!(chat.title.as_deref(), Some("Billing bot"));
    assert_eq!(chat.space_id, None);
    let folder = std::path::PathBuf::from(chat.cwd.clone().unwrap());
    assert!(folder.is_dir() && folder.ends_with(format!("bots/{}", bot.id)));

    let projects = tool(&core, &bot.chat_id, "list_projects", serde_json::json!({})).await;
    assert!(projects.contains("sp-billing"), "{projects}");
    assert!(!projects.contains("sp-secret"), "{projects}");

    let sessions = tool(&core, &bot.chat_id, "list_sessions", serde_json::json!({})).await;
    assert!(sessions.contains("chat-billing"), "{sessions}");
    assert!(!sessions.contains("chat-secret"), "{sessions}");
    assert!(
        !sessions.contains(&bot.chat_id),
        "a bot never sees its own chat"
    );

    let found = tool(
        &core,
        &bot.chat_id,
        "search_sessions",
        serde_json::json!({ "query": "invoice" }),
    )
    .await;
    assert!(found.contains("Migrate the invoice tables"), "{found}");
    assert!(!found.contains("signing key"), "{found}");

    let read = tool(
        &core,
        &bot.chat_id,
        "read_session",
        serde_json::json!({ "session_id": "chat-billing" }),
    )
    .await;
    assert!(read.contains("Migrate the invoice tables to v2"), "{read}");

    // Out of scope reads exactly like missing: the model cannot probe.
    let hidden = tool(
        &core,
        &bot.chat_id,
        "read_session",
        serde_json::json!({ "session_id": "chat-secret" }),
    )
    .await;
    let missing = tool(
        &core,
        &bot.chat_id,
        "read_session",
        serde_json::json!({ "session_id": "no-such-chat" }),
    )
    .await;
    assert_eq!(
        hidden.replace("chat-secret", "ID"),
        missing.replace("no-such-chat", "ID")
    );
    let hidden_project = tool(
        &core,
        &bot.chat_id,
        "list_sessions",
        serde_json::json!({ "project": "sp-secret" }),
    )
    .await;
    assert!(hidden_project.starts_with("No project"), "{hidden_project}");

    // Only a bot chat may call the tools.
    let refused = tool(
        &core,
        "chat-billing",
        "list_sessions",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(refused, "This chat is not an agent bot.");
}

#[tokio::test(flavor = "multi_thread")]
async fn scope_edits_apply_to_the_next_call() {
    let tmp = tempfile::tempdir().unwrap();
    let core = common::assemble_mock(&tmp.path().join("data"));
    seed(&core, tmp.path());
    let bot = create_bot(&core, billing_only()).await;
    let client = cypher_rpc::memory_client(core.rpc_service());
    let update = |scope: BotScope| {
        serde_json::json!({
            "op": "updateBot",
            "botId": bot.id,
            "name": "Release bot",
            "scope": scope,
        })
    };

    client
        .call(
            cypher_rpc::methods::MUTATE,
            update(BotScope {
                projects: ProjectGrant::All,
                ..Default::default()
            }),
        )
        .await
        .expect("updateBot");
    let sessions = tool(&core, &bot.chat_id, "list_sessions", serde_json::json!({})).await;
    assert!(sessions.contains("chat-secret"), "{sessions}");
    assert_eq!(
        core.workspace
            .chat(&bot.chat_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Release bot"),
        "a rename retitles the bot's chat"
    );

    // Without message text the bot keeps titles and previews only.
    client
        .call(
            cypher_rpc::methods::MUTATE,
            update(BotScope {
                projects: ProjectGrant::All,
                transcripts: false,
                ..Default::default()
            }),
        )
        .await
        .expect("updateBot");
    let read = tool(
        &core,
        &bot.chat_id,
        "read_session",
        serde_json::json!({ "session_id": "chat-billing" }),
    )
    .await;
    assert!(read.contains("may not read message text"), "{read}");
    let found = tool(
        &core,
        &bot.chat_id,
        "search_sessions",
        serde_json::json!({ "query": "tables" }),
    )
    .await;
    assert!(!found.contains("Migrate the invoice tables"), "{found}");

    // An invalid edit is refused before anything is written.
    let refused = client
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "updateBot", "botId": bot.id, "name": "  ", "scope": {},
            }),
        )
        .await;
    assert!(refused.is_err());

    // Deleting the bot removes its chat and its folder; its tools stop.
    let folder = core
        .workspace
        .chat(&bot.chat_id)
        .unwrap()
        .unwrap()
        .cwd
        .unwrap();
    client
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({ "op": "deleteBot", "botId": bot.id }),
        )
        .await
        .expect("deleteBot");
    assert!(core.workspace.bot(&bot.id).unwrap().is_none());
    assert!(core.workspace.chat(&bot.chat_id).unwrap().is_none());
    wait_for(
        || !std::path::Path::new(&folder).exists(),
        "bot folder removed",
    )
    .await;
}

type HostLog = Arc<Mutex<Vec<(RunRequest, Option<cypher_harness::BotRunEnv>)>>>;

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_chat_runs_locked_down() {
    let tmp = tempfile::tempdir().unwrap();
    let log = HostLog::default();
    let seen = log.clone();
    // Bot chats run on Pi; this stand-in answers under its id.
    let harness = common::TestHarness::new(HarnessId::Pi, "Recording", move |request, controls| {
        seen.lock()
            .unwrap()
            .push((request.clone(), controls.host.profile.bot().cloned()));
        common::reply(HarnessId::Pi, &request, "sess-bot", "On it.")
    });
    let core = common::engine_at(&tmp.path().join("data"), harness);
    seed(&core, tmp.path());
    let bot = create_bot(&core, billing_only()).await;
    let chat = core.workspace.chat(&bot.chat_id).unwrap().unwrap();
    // A data-directory restore can lose the folder; the run recreates it.
    std::fs::remove_dir(chat.cwd.as_deref().unwrap()).unwrap();

    core.doc_host
        .queue_command(
            &bot.chat_id,
            SessionCommandPayload::Run {
                request: RunRequest {
                    cwd: chat.cwd.clone().unwrap(),
                    ..run_request("What changed in billing?")
                },
                message_id: "msg-bot-1".into(),
                agent_prompt: None,
            },
        )
        .expect("queue the bot run");
    wait_for(
        || complete_assistant_count(&core, &bot.chat_id) == 1,
        "the bot's turn",
    )
    .await;

    let runs = log.lock().unwrap().clone();
    let (request, env) = runs
        .iter()
        .find(|(request, _)| request.prompt == "What changed in billing?")
        .expect("the bot run reached the harness");
    let env = env.as_ref().expect("bot runs carry the bot env");
    assert_eq!(env.bot_id, bot.id);
    assert_eq!(env.origin, cypher_proto::BotOrigin::Owner);
    // The read tools, plus the network tools every owner thread carries.
    assert_eq!(
        env.tools[..cypher_proto::BOT_TOOLS.len()],
        cypher_proto::BOT_TOOLS
    );
    for tool in cypher_proto::BOT_NETWORK_TOOLS {
        assert!(env.tools.iter().any(|t| t == tool), "{tool}");
    }
    assert!(env.workspace_roots.is_empty());
    assert!(env.system_prompt.contains("\"Billing bot\""));
    assert!(
        env.system_prompt.contains("sp-billing"),
        "{}",
        env.system_prompt
    );
    assert!(!env.system_prompt.contains("sp-secret"));
    assert!(
        std::path::Path::new(&request.cwd).is_dir(),
        "folder recreated"
    );

    // An ordinary chat's run carries no bot env.
    core.doc_host
        .queue_command(
            "chat-billing",
            SessionCommandPayload::Run {
                request: run_request("plain run"),
                message_id: "msg-plain-1".into(),
                agent_prompt: None,
            },
        )
        .expect("queue a plain run");
    wait_for(
        || complete_assistant_count(&core, "chat-billing") == 1,
        "the plain turn",
    )
    .await;
    let runs = log.lock().unwrap().clone();
    let (_, env) = runs
        .iter()
        .find(|(request, _)| request.prompt == "plain run")
        .expect("the plain run reached the harness");
    assert!(env.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_chat_refuses_commands_that_widen_its_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let harness = common::TestHarness::new(HarnessId::Pi, "Echo", |request, _| {
        common::reply(HarnessId::Pi, &request, "sess", "ok")
    });
    let core = common::engine_at(&tmp.path().join("data"), harness);
    seed(&core, tmp.path());
    let bot = create_bot(&core, billing_only()).await;
    let cwd = core
        .workspace
        .chat(&bot.chat_id)
        .unwrap()
        .unwrap()
        .cwd
        .unwrap();
    let run = |prompt: &str| RunRequest {
        cwd: cwd.clone(),
        ..run_request(prompt)
    };
    for prompt in ["/scripts on", "  /goal ship it", "/orchestrate"] {
        let refused = core
            .sessions
            .dispatch(&bot.chat_id, HarnessId::Pi, run(prompt), None)
            .await;
        assert!(
            refused.is_err_and(|e| e.to_string().contains("isn't available in a bot chat")),
            "{prompt} must be refused"
        );
    }
    // Other prompts, and the same command in an ordinary chat, still run.
    core.sessions
        .dispatch(&bot.chat_id, HarnessId::Pi, run("/compact"), None)
        .await
        .expect("an ordinary command runs");
    let plain = core.workspace.chat("chat-billing").unwrap().unwrap();
    cypher_engine::bots::refuse_widening_command(&plain, "/scripts on")
        .expect("ordinary chats are unaffected");
}

/// A thread of `bot` with `origin`, written the way the engine will when it
/// starts one (routines and peer threads have no creation path yet).
fn bot_thread(core: &EngineCore, bot: &Bot, id: &str, origin: BotOrigin) -> String {
    let mut chat = core.workspace.chat(&bot.chat_id).unwrap().unwrap();
    chat.id = id.into();
    chat.bot = Some(BotChat::new(&bot.id, origin));
    core.workspace.import_chat_row(&chat).unwrap();
    chat.cwd.unwrap()
}

/// A stand-in Pi that asks to run a command outside the sandbox, records
/// the answer when one is there at once, then finishes its turn once
/// answered.
fn asking_harness(answers: Arc<Mutex<Vec<Vec<UserInputAnswer>>>>) -> common::TestHarness {
    common::TestHarness::new(HarnessId::Pi, "Asking", move |request, controls| {
        let mut answer = (controls.request_input)(vec![UserInputQuestion {
            id: "perm-1".into(),
            header: "Approval".into(),
            question: "Run this command outside the sandbox?".into(),
            options: vec!["Run once".into(), "Deny".into()],
            multi_select: false,
            permission: Some(PermissionPrompt {
                kind: PermissionPromptKind::Escalate,
                tool_call_id: None,
            }),
        }]);
        if let Ok(immediate) = answer.try_recv() {
            answers.lock().unwrap().push(immediate);
        }
        let started = common::session_started(HarnessId::Pi, "mock-1", &request.cwd, "s", "a-1");
        let rest = futures::stream::once(async move {
            let _ = answer.await;
            futures::stream::iter(
                [common::text("Carried on without it."), common::done("s")].map(Ok),
            )
        })
        .flatten();
        Ok(futures::stream::iter([Ok(started)]).chain(rest).boxed())
    })
}

fn status(core: &EngineCore, chat_id: &str) -> Option<SessionStatus> {
    core.sessions.session_status(chat_id).map(|s| s.status)
}

/// A routine thread runs unattended: the approval it asks for is denied on
/// the spot, never parked as a card, and the run settles failed with the
/// reason in its transcript and on the unattended-stop hook.
#[tokio::test(flavor = "multi_thread")]
async fn an_unattended_run_that_asks_stops_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let answers = Arc::new(Mutex::new(Vec::new()));
    let core = common::engine_at(&tmp.path().join("data"), asking_harness(answers.clone()));
    let stops = Arc::new(Mutex::new(Vec::<UnattendedStop>::new()));
    let heard = stops.clone();
    core.sessions.set_unattended_listener(Arc::new(move |stop| {
        heard.lock().unwrap().push(stop.clone())
    }));
    seed(&core, tmp.path());
    let bot = create_bot(&core, billing_only()).await;
    let cwd = bot_thread(&core, &bot, "routine-1", BotOrigin::Routine);

    core.sessions
        .dispatch(
            "routine-1",
            HarnessId::Pi,
            RunRequest {
                cwd,
                ..run_request("Morning summary")
            },
            None,
        )
        .await
        .expect("the routine run starts");
    wait_for(
        || status(&core, "routine-1") == Some(SessionStatus::Errored),
        "the routine run to settle failed",
    )
    .await;

    let expected = "This run is unattended, so it couldn't ask for approval to run a command \
                    outside the sandbox. It stopped.";
    assert_eq!(
        answers.lock().unwrap().clone(),
        vec![Vec::new()],
        "denied at once"
    );
    assert_eq!(
        stops.lock().unwrap().clone(),
        vec![UnattendedStop {
            chat_id: "routine-1".into(),
            reason: expected.into(),
        }]
    );
    let parts: Vec<MessagePart> = common::entries(&core, "routine-1")
        .into_iter()
        .filter(|e| e.role == MessageRole::Assistant)
        .flat_map(|e| e.parts)
        .collect();
    assert!(
        parts
            .iter()
            .any(|p| matches!(p, MessagePart::Error { message, .. } if message == expected)),
        "{parts:?}"
    );
    assert!(
        !parts.iter().any(|p| matches!(p, MessagePart::Input { .. })),
        "no approval card: {parts:?}"
    );
    assert!(
        !parts
            .iter()
            .any(|p| matches!(p, MessagePart::Text { text, .. } if text.contains("Carried on"))),
        "the run stopped: {parts:?}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(status(&core, "routine-1"), Some(SessionStatus::Errored));
}

/// Attended threads are unchanged: the owner's thread parks the approval
/// for someone to answer.
#[tokio::test(flavor = "multi_thread")]
async fn an_attended_bot_run_waits_for_its_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let answers = Arc::new(Mutex::new(Vec::new()));
    let core = common::engine_at(&tmp.path().join("data"), asking_harness(answers.clone()));
    seed(&core, tmp.path());
    let bot = create_bot(&core, billing_only()).await;
    let cwd = core
        .workspace
        .chat(&bot.chat_id)
        .unwrap()
        .unwrap()
        .cwd
        .unwrap();

    core.sessions
        .dispatch(
            &bot.chat_id,
            HarnessId::Pi,
            RunRequest {
                cwd,
                ..run_request("Clean up")
            },
            None,
        )
        .await
        .expect("the owner run starts");
    wait_for(
        || status(&core, &bot.chat_id) == Some(SessionStatus::AwaitingInput),
        "the owner run to wait on its approval",
    )
    .await;
    assert!(answers.lock().unwrap().is_empty(), "nobody answered yet");
    core.sessions.interrupt(&bot.chat_id).await.unwrap();
}

/// No client operation creates a bot thread or changes one's marker: the
/// Mutate surface edits fields of a row and never its `bot` marker, an
/// idempotent CreateChat leaves an existing thread alone, and a fork of a
/// thread is an ordinary chat.
#[tokio::test(flavor = "multi_thread")]
async fn clients_cannot_forge_or_change_a_bot_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let core = common::assemble_mock(&tmp.path().join("data"));
    seed(&core, tmp.path());
    let bot = create_bot(&core, billing_only()).await;
    bot_thread(&core, &bot, "peer-1", BotOrigin::Peer);
    let client = cypher_rpc::memory_client(core.rpc_service());
    let mutate = |op: serde_json::Value| client.call(cypher_rpc::methods::MUTATE, op);
    let forged = serde_json::json!({ "botId": bot.id, "origin": "owner" });

    // A new chat with a smuggled marker is an ordinary chat.
    mutate(serde_json::json!({
        "op": "createChat",
        "chatId": "forged",
        "spaceId": "sp-billing",
        "bot": forged,
    }))
    .await
    .expect("createChat");
    let chat = core.workspace.chat("forged").unwrap().expect("created");
    assert_eq!(chat.bot, None);

    // Every row edit keeps the thread's marker exactly as stamped.
    for chat_id in [bot.chat_id.as_str(), "peer-1"] {
        let before = core.workspace.chat(chat_id).unwrap().unwrap().bot;
        for op in [
            serde_json::json!({ "op": "createChat", "chatId": chat_id, "spaceId": "sp-billing", "bot": forged }),
            serde_json::json!({ "op": "renameChat", "chatId": chat_id, "title": "Renamed" }),
            serde_json::json!({ "op": "setChatActivity", "chatId": chat_id, "lastMessageAt": 5_000 }),
            serde_json::json!({ "op": "setChatArchived", "chatId": chat_id, "archived": false }),
            serde_json::json!({ "op": "setChatPinned", "chatId": chat_id, "pinned": true }),
            serde_json::json!({ "op": "setChatCwd", "chatId": chat_id, "cwd": "/tmp", "bot": forged }),
        ] {
            mutate(op.clone()).await.expect("mutate");
            let after = core.workspace.chat(chat_id).unwrap().unwrap();
            assert_eq!(after.bot, before, "{op}");
            assert_eq!(after.space_id, None, "{op}");
        }
    }
    assert_eq!(
        core.workspace.chat("peer-1").unwrap().unwrap().bot,
        Some(BotChat::new(&bot.id, BotOrigin::Peer))
    );

    // A fork of a thread carries none of its marker.
    let source = core.workspace.chat("peer-1").unwrap().unwrap();
    core.workspace
        .create_fork_chat("fork-1", &source, "Fork", None, None, None)
        .unwrap();
    assert_eq!(core.workspace.chat("fork-1").unwrap().unwrap().bot, None);
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn activity_and_git_state_stay_in_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let core = common::assemble_mock(&tmp.path().join("data"));
    seed(&core, tmp.path());
    let bot = create_bot(&core, billing_only()).await;

    let activity = tool(
        &core,
        &bot.chat_id,
        "session_activity",
        serde_json::json!({}),
    )
    .await;
    assert!(
        activity.contains("Activity in the last 24 hours"),
        "{activity}"
    );
    assert!(activity.contains("chat-billing"), "{activity}");
    assert!(activity.contains("new, started"), "{activity}");
    assert!(!activity.contains("chat-secret"), "{activity}");

    // A local space reads git directly.
    let repo = tmp.path().join("sp-billing");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("invoice.rs"), "fn total() {}\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "Add invoice totals"]);
    std::fs::write(repo.join("invoice.rs"), "fn total() -> u32 { 0 }\n").unwrap();
    std::fs::write(repo.join("notes.md"), "todo\n").unwrap();
    let state = tool(
        &core,
        &bot.chat_id,
        "project_git",
        serde_json::json!({ "project": "sp-billing" }),
    )
    .await;
    assert!(
        state.contains("Branch: main · no upstream branch"),
        "{state}"
    );
    assert!(state.contains("Working tree: 2 file(s) changed"), "{state}");
    assert!(state.contains("added notes.md"), "{state}");
    assert!(state.contains("Add invoice totals — Ada"), "{state}");

    let not_git = tool(
        &core,
        &bot.chat_id,
        "project_git",
        serde_json::json!({ "project": "sp-secret" }),
    )
    .await;
    let unknown = tool(
        &core,
        &bot.chat_id,
        "project_git",
        serde_json::json!({ "project": "sp-nope" }),
    )
    .await;
    assert_eq!(
        not_git.replace("sp-secret", "ID"),
        unknown.replace("sp-nope", "ID"),
        "out of scope reads like unknown"
    );

    // Another device's space needs that device; with no relay it is offline.
    core.workspace
        .create_space("sp-remote", "dev-other", "/srv/billing", None, true)
        .unwrap();
    let client = cypher_rpc::memory_client(core.rpc_service());
    client
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "updateBot", "botId": bot.id, "name": "Billing bot",
                "scope": BotScope { projects: ProjectGrant::All, ..Default::default() },
            }),
        )
        .await
        .expect("updateBot");
    let remote = tool(
        &core,
        &bot.chat_id,
        "project_git",
        serde_json::json!({ "project": "sp-remote" }),
    )
    .await;
    assert!(
        remote.ends_with("is offline, so its git state can't be read right now."),
        "{remote}"
    );
    let secret = tool(
        &core,
        &bot.chat_id,
        "project_git",
        serde_json::json!({ "project": "sp-secret" }),
    )
    .await;
    assert!(secret.contains("is not a git repository"), "{secret}");
}

/// The chats the bot search index holds a transcript of, from its file.
fn indexed_chats(bot_folder: &str) -> Vec<String> {
    let store = std::path::Path::new(bot_folder)
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap();
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        store.join("bots-index.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare("SELECT chat_id FROM chats WHERE stored = 1 ORDER BY chat_id")
    else {
        return Vec::new();
    };
    stmt.query_map([], |row| row.get(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn search_reads_the_index_and_filters_scope_at_query_time() {
    let tmp = tempfile::tempdir().unwrap();
    let core = common::assemble_mock(&tmp.path().join("data"));
    seed(&core, tmp.path());
    let bot = create_bot(&core, billing_only()).await;
    let folder = core
        .workspace
        .chat(&bot.chat_id)
        .unwrap()
        .unwrap()
        .cwd
        .unwrap();

    // The background indexer picks up the new bot's scope, and only it.
    wait_for(
        || indexed_chats(&folder) == ["chat-billing"],
        "the billing chat indexed",
    )
    .await;
    let found = tool(
        &core,
        &bot.chat_id,
        "search_sessions",
        serde_json::json!({ "query": "invo tables" }),
    )
    .await;
    assert!(
        found.contains("#0 user: Migrate the invoice tables"),
        "{found}"
    );
    assert!(!found.contains("not indexed"), "{found}");

    // Scope applies at query time, before the index catches up.
    let client = cypher_rpc::memory_client(core.rpc_service());
    client
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "updateBot", "botId": bot.id, "name": "Billing bot",
                "scope": BotScope {
                    projects: ProjectGrant::All,
                    exclude_sessions: vec!["chat-billing".into()],
                    ..Default::default()
                },
            }),
        )
        .await
        .expect("updateBot");
    let found = tool(
        &core,
        &bot.chat_id,
        "search_sessions",
        serde_json::json!({ "query": "invoice" }),
    )
    .await;
    assert!(!found.contains("Migrate"), "{found}");
    assert!(found.contains("Rotate the invoice signing key"), "{found}");

    // The index follows: the excluded chat leaves, the newly granted one joins.
    wait_for(
        || indexed_chats(&folder) == ["chat-secret"],
        "the index follows the scope",
    )
    .await;

    // A deleted bot leaves nothing indexed.
    client
        .call(
            cypher_rpc::methods::MUTATE,
            serde_json::json!({ "op": "deleteBot", "botId": bot.id }),
        )
        .await
        .expect("deleteBot");
    wait_for(|| indexed_chats(&folder).is_empty(), "the index emptied").await;
    core.shutdown().await;
}
