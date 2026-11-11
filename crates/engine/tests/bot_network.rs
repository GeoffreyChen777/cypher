//! The bot network (docs/design/agent-bots.md §11) between two engines,
//! each signed in as its own user, through an in-process fake Edge
//! (`common/bot_edge.rs`): invites and connections on both sides, peer
//! questions in Auto, Review and Off, revocation, late answers, the taint
//! rule, and the local profile with no network.

#[path = "common/bot_edge.rs"]
mod bot_edge;
mod common;

use std::sync::{Arc, Mutex};

use cypher_doc::MessageRole;
use cypher_engine::EngineCore;
use cypher_engine::HarnessRegistry;
use cypher_proto::{
    ActivityKind, Bot, BotConnection, BotExchange, BotOrigin, BotScope, BotThread,
    ExchangeDirection, ExchangeState, HarnessId, ProjectGrant, RunRequest,
};

use bot_edge::FakeHub;
use common::wait_for;

type Runs = Arc<Mutex<Vec<(RunRequest, Option<cypher_harness::BotRunEnv>)>>>;

/// One user's engine with one bot.
struct Side {
    core: EngineCore,
    bot: Bot,
    runs: Runs,
    /// The signed-in owner peers see; tests change their photo.
    owner: Arc<Mutex<cypher_engine::bots::Owner>>,
    _dir: tempfile::TempDir,
}

/// What every peer question is answered with (secrets included, to see the
/// scrub at work).
const PEER_REPLY: &str = "Billing is green: invoices v2 shipped. Deploy key AKIAIOSFODNN7EXAMPLE.";

/// A stand-in Pi: peer questions get [`PEER_REPLY`], anything else "Noted.".
fn harness(runs: Runs) -> common::TestHarness {
    common::TestHarness::new(HarnessId::Pi, "Bot", move |request, controls| {
        runs.lock()
            .unwrap()
            .push((request.clone(), controls.host.profile.bot().cloned()));
        let text = if request.prompt.starts_with("Question from") {
            PEER_REPLY
        } else {
            "Noted."
        };
        common::reply(HarnessId::Pi, &request, "sess", text)
    })
}

async fn side(hub: &Arc<FakeHub>, user: &str, owner: &str, bot_name: &str) -> Side {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let device = format!("dev-{user}");
    std::fs::write(data.join("device-id"), &device).unwrap();
    let runs = Runs::default();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(harness(runs.clone())));
    let core = EngineCore::assemble(&data, Arc::new(registry), HarnessId::Pi, None).unwrap();
    // A project the bot reads, and one the connection will hide.
    for space in ["sp-billing", "sp-secret"] {
        let path = dir.path().join(space);
        std::fs::create_dir_all(&path).unwrap();
        core.workspace
            .create_space(space, &core.device_id, path.to_str().unwrap(), None, false)
            .unwrap();
    }
    let owner = Arc::new(Mutex::new(cypher_engine::bots::Owner {
        name: owner.to_string(),
        avatar_url: None,
    }));
    let profile = owner.clone();
    core.bots
        .set_owner(Arc::new(move || Some(profile.lock().unwrap().clone())));
    core.bots.attach_network(hub.client(user));
    hub.register_device(&device, user, core.bots.clone());
    let client = cypher_rpc::memory_client(core.rpc_service());
    let bot: Bot = serde_json::from_value(
        client
            .call(
                cypher_rpc::methods::CREATE_BOT,
                serde_json::json!({
                    "targetDeviceId": core.device_id,
                    "name": bot_name,
                    "scope": BotScope { projects: ProjectGrant::All, ..Default::default() },
                }),
            )
            .await
            .expect("CreateBot"),
    )
    .unwrap();
    Side {
        core,
        bot,
        runs,
        owner,
        _dir: dir,
    }
}

async fn call(core: &EngineCore, method: &str, params: serde_json::Value) -> serde_json::Value {
    cypher_rpc::memory_client(core.rpc_service())
        .call(method, params)
        .await
        .unwrap_or_else(|err| panic!("{method}: {err}"))
}

/// One bot tool call as the bot tools extension makes it: with the Pi tool
/// call's id, which the RPC hands the tool beside the model's arguments.
async fn tool(core: &EngineCore, chat_id: &str, name: &str, args: serde_json::Value) -> String {
    let reply = call(
        core,
        cypher_rpc::methods::BOT_TOOL,
        serde_json::json!({ "chatId": chat_id, "tool": name, "args": args, "toolCallId": "call-1" }),
    )
    .await;
    reply["text"].as_str().expect("text").to_string()
}

async fn mutate(core: &EngineCore, op: serde_json::Value) {
    call(core, cypher_rpc::methods::MUTATE, op).await;
}

fn connections(side: &Side) -> Vec<BotConnection> {
    side.core
        .workspace
        .read_bot_records()
        .connections
        .into_iter()
        .filter(|c| c.bot_id == side.bot.id)
        .collect()
}

fn exchanges(side: &Side, direction: ExchangeDirection) -> Vec<BotExchange> {
    side.core
        .workspace
        .read_bot_records()
        .exchanges
        .into_iter()
        .filter(|e| e.bot_id == side.bot.id && e.direction == direction)
        .collect()
}

/// Whether `side`'s one incoming question has settled as `state`.
fn settled(side: &Side, state: ExchangeState) -> bool {
    exchanges(side, ExchangeDirection::Incoming)
        .first()
        .is_some_and(|e| e.state == state)
}

fn activity(side: &Side, kind: ActivityKind) -> Vec<String> {
    side.core
        .workspace
        .read_bot_records()
        .activity
        .into_iter()
        .filter(|a| a.bot_id == side.bot.id && a.kind == kind)
        .map(|a| a.text)
        .collect()
}

fn peer_questions(side: &Side) -> Vec<(RunRequest, Option<cypher_harness::BotRunEnv>)> {
    side.runs
        .lock()
        .unwrap()
        .iter()
        .filter(|(r, _)| r.prompt.starts_with("Question from"))
        .cloned()
        .collect()
}

/// Alice's "Billing bot" invites; Bob's "Release bot" redeems. Returns the
/// connection id, once both sides hold the row.
async fn connect(alice: &Side, bob: &Side) -> String {
    let invite = call(
        &alice.core,
        cypher_rpc::methods::CREATE_BOT_INVITE,
        serde_json::json!({ "targetDeviceId": alice.core.device_id, "botId": alice.bot.id }),
    )
    .await;
    let code = invite["code"].as_str().expect("code").to_string();
    let connection: BotConnection = serde_json::from_value(
        call(
            &bob.core,
            cypher_rpc::methods::REDEEM_BOT_INVITE,
            serde_json::json!({ "targetDeviceId": bob.core.device_id, "botId": bob.bot.id, "code": code }),
        )
        .await,
    )
    .unwrap();
    wait_for(
        || connections(alice).iter().any(|c| c.id == connection.id),
        "the inviting side learns of the connection",
    )
    .await;
    connection.id
}

async fn pair() -> (Arc<FakeHub>, Side, Side) {
    let hub = FakeHub::new();
    let alice = side(&hub, "alice", "Alice", "Billing bot").await;
    let bob = side(&hub, "bob", "Bob", "Release bot").await;
    (hub, alice, bob)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invite_connects_both_bots_and_auto_answers_flow_back() {
    let (hub, alice, bob) = pair().await;
    let invite = call(
        &alice.core,
        cypher_rpc::methods::CREATE_BOT_INVITE,
        serde_json::json!({ "targetDeviceId": alice.core.device_id, "botId": alice.bot.id }),
    )
    .await;
    let code = invite["code"].as_str().unwrap().to_string();
    let invites = alice.core.workspace.read_bot_records().invites;
    assert_eq!(invites.len(), 1);
    assert_eq!(invites[0].id, invite["inviteId"].as_str().unwrap());
    assert!(
        !serde_json::to_string(&invites[0]).unwrap().contains(&code),
        "the code is never stored"
    );
    let room = code.split('.').next().unwrap().to_string();
    assert_eq!(
        hub.profile(&room).map(|p| (p.name, p.owner_name)),
        Some(("Billing bot".into(), "Alice".into()))
    );

    let connection: BotConnection = serde_json::from_value(
        call(
            &bob.core,
            cypher_rpc::methods::REDEEM_BOT_INVITE,
            serde_json::json!({ "botId": bob.bot.id, "code": code }),
        )
        .await,
    )
    .unwrap();
    assert_eq!(connection.peer.name, "Billing bot");
    assert_eq!(connection.peer.owner_name, "Alice");
    assert_eq!(connections(&bob).len(), 1);
    wait_for(
        || connections(&alice).len() == 1,
        "Alice's host writes its row from the link entry",
    )
    .await;
    let theirs = &connections(&alice)[0];
    assert_eq!(theirs.id, connection.id);
    assert_eq!(theirs.peer.name, "Release bot");
    assert_eq!(theirs.peer.owner_name, "Bob");
    wait_for(
        || alice.core.workspace.read_bot_records().invites[0].uses == 1,
        "the invite counts its use",
    )
    .await;
    assert_eq!(activity(&alice, ActivityKind::ConnectionAdded).len(), 1);
    assert_eq!(activity(&bob, ActivityKind::ConnectionAdded).len(), 1);

    let listed = tool(
        &bob.core,
        &bob.bot.chat_id,
        "list_connections",
        serde_json::json!({}),
    )
    .await;
    assert!(
        listed.contains("\"Billing bot\" (Alice's workspace)"),
        "{listed}"
    );

    let answer = tool(
        &bob.core,
        &bob.bot.chat_id,
        "ask_bot",
        serde_json::json!({ "bot": "billing BOT", "question": "How is billing doing?", "wait_s": 30 }),
    )
    .await;
    assert!(
        answer.contains("Billing is green: invoices v2 shipped."),
        "{answer}"
    );
    assert!(answer.contains("<peer-answer>"), "{answer}");
    assert!(
        !answer.contains("AKIAIOSFODNN7EXAMPLE"),
        "scrubbed: {answer}"
    );
    assert!(answer.contains("[redacted]"), "{answer}");
    assert!(
        bob.core.bots.tainted(&bob.bot.chat_id).is_some(),
        "the asking thread is tainted"
    );

    // The answering side: the question ran in the connection's peer thread,
    // read-only, and the exchange is on record.
    let questions = peer_questions(&alice);
    assert_eq!(questions.len(), 1);
    let (request, env) = &questions[0];
    assert!(request.prompt.contains("How is billing doing?"));
    assert!(
        request
            .prompt
            .starts_with("Question from \"Release bot\" (Bob's workspace)")
    );
    let env = env.as_ref().expect("a bot run");
    assert_eq!(env.origin, BotOrigin::Peer);
    assert_eq!(env.tools, cypher_proto::BOT_TOOLS, "read tools only");
    assert!(
        env.system_prompt
            .contains("\"Release bot\", the workspace of Bob"),
        "{}",
        env.system_prompt
    );
    let thread_id = connections(&alice)[0]
        .thread_chat_id
        .clone()
        .expect("peer thread recorded");
    let thread = alice.core.workspace.chat(&thread_id).unwrap().unwrap();
    let marker = thread.bot.expect("a bot thread");
    assert_eq!(marker.origin, BotOrigin::Peer);
    assert_eq!(
        marker.thread,
        BotThread::Peer {
            connection_id: connection.id.clone()
        }
    );
    assert_eq!(
        thread.cwd,
        alice
            .core
            .workspace
            .chat(&alice.bot.chat_id)
            .unwrap()
            .unwrap()
            .cwd
    );
    // Bob can hold the answer a moment before Alice's host writes its row.
    wait_for(
        || {
            settled(&alice, ExchangeState::Answered)
                && !activity(&alice, ActivityKind::PeerAnswered).is_empty()
        },
        "Alice's row records the answer",
    )
    .await;
    let incoming = exchanges(&alice, ExchangeDirection::Incoming);
    assert_eq!(incoming.len(), 1);
    assert!(
        incoming[0]
            .answer
            .as_deref()
            .unwrap()
            .contains("[redacted]")
    );
    assert_eq!(activity(&alice, ActivityKind::PeerAnswered).len(), 1);
    let outgoing = exchanges(&bob, ExchangeDirection::Outgoing);
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].state, ExchangeState::Answered);
    assert_eq!(activity(&bob, ActivityKind::PeerAsked).len(), 1);
    assert_eq!(hub.pending_asks(&room), 0, "the ask was acknowledged");
    // Each engine only ever called its own user's rooms.
    assert!(
        hub.calls()
            .iter()
            .filter(|c| c.starts_with("bob "))
            .all(|c| !c.ends_with(&room)),
        "{:?}",
        hub.calls()
    );
}

/// The HTTP client speaks the Edge's routes: paths, methods, bodies, and
/// the Edge's `{error, message}` refusals.
#[tokio::test(flavor = "multi_thread")]
async fn the_http_client_speaks_the_edge_routes() {
    use cypher_engine::bots::{BotEdge, HttpBotEdge};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::<(String, String, String)>::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let Some((method, target, body)) = common::edge::read_request(&mut stream).await else {
                continue;
            };
            log.lock()
                .unwrap()
                .push((method.clone(), target.clone(), body));
            let (status, reply) = match target.as_str() {
                "/bots/room-1/inbox" => (
                    "200 OK",
                    r#"{"entries":[{"t":"state","exchangeId":"ex-1","state":"working"},{"t":"future"}]}"#,
                ),
                "/bots/room-1/ask" => (
                    "429 Too Many Requests",
                    r#"{"error":"rate_limited","message":"At most 30 questions an hour on one connection."}"#,
                ),
                _ => ("200 OK", r#"{"ok":true}"#),
            };
            common::edge::respond(&mut stream, status, reply).await;
        }
    });
    let edge = HttpBotEdge::new(cypher_engine::EdgeConfig::with_static_token(&url, "alice"));
    let page = edge.inbox("room-1").await.expect("inbox");
    assert_eq!(
        page.entries,
        vec![
            cypher_proto::BotFrame::State {
                exchange_id: "ex-1".into(),
                state: cypher_proto::WireState::Working
            },
            cypher_proto::BotFrame::Unknown
        ]
    );
    let refused = edge
        .ask(
            "room-1",
            &cypher_proto::AskRequest {
                exchange_id: "ex-2".into(),
                connection_id: "cn-1".into(),
                text: "q".into(),
            },
        )
        .await
        .expect_err("rate limited");
    assert_eq!(refused.status, Some(429));
    assert_eq!(refused.code, "rate_limited");
    assert_eq!(
        refused.to_string(),
        "At most 30 questions an hour on one connection."
    );
    edge.revoke_connection("room-1", "cn-1")
        .await
        .expect("revoke");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0].0, "GET");
    assert_eq!(
        seen[1],
        (
            "POST".to_string(),
            "/bots/room-1/ask".to_string(),
            r#"{"exchangeId":"ex-2","connectionId":"cn-1","text":"q"}"#.to_string()
        )
    );
    assert_eq!(
        (seen[2].0.as_str(), seen[2].1.as_str()),
        ("DELETE", "/bots/room-1/connections/cn-1")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_thread_reads_through_the_connection_scope() {
    let (_hub, alice, bob) = pair().await;
    let connection_id = connect(&alice, &bob).await;
    mutate(
        &alice.core,
        serde_json::json!({
            "op": "updateBotConnection",
            "connectionId": connection_id,
            "replyMode": "auto",
            "scope": BotScope {
                projects: ProjectGrant::Selected { space_ids: vec!["sp-billing".into()] },
                ..Default::default()
            },
        }),
    )
    .await;
    tool(
        &bob.core,
        &bob.bot.chat_id,
        "ask_bot",
        serde_json::json!({ "bot": connection_id, "question": "Which projects?", "wait_s": 30 }),
    )
    .await;
    let thread_id = connections(&alice)[0].thread_chat_id.clone().unwrap();
    let projects = tool(
        &alice.core,
        &thread_id,
        "list_projects",
        serde_json::json!({}),
    )
    .await;
    assert!(projects.contains("sp-billing"), "{projects}");
    assert!(!projects.contains("sp-secret"), "{projects}");
    // The owner's own chat still sees both.
    let own = tool(
        &alice.core,
        &alice.bot.chat_id,
        "list_projects",
        serde_json::json!({}),
    )
    .await;
    assert!(own.contains("sp-secret"), "{own}");
    // Peer threads never get the network tools.
    let relayed = tool(
        &alice.core,
        &thread_id,
        "ask_bot",
        serde_json::json!({ "bot": "Release bot", "question": "relay this" }),
    )
    .await;
    assert_eq!(relayed, "ask_bot isn't available in this session.");
}

#[tokio::test(flavor = "multi_thread")]
async fn review_mode_waits_for_the_owner_and_the_late_answer_reaches_the_main_chat() {
    let (_hub, alice, bob) = pair().await;
    let connection_id = connect(&alice, &bob).await;
    mutate(
        &alice.core,
        serde_json::json!({ "op": "updateBotConnection", "connectionId": connection_id, "replyMode": "review" }),
    )
    .await;
    let pending = tool(
        &bob.core,
        &bob.bot.chat_id,
        "ask_bot",
        serde_json::json!({ "bot": "Billing bot", "question": "Can we ship Friday?", "wait_s": 1 }),
    )
    .await;
    assert!(pending.starts_with("pending:"), "{pending}");
    assert!(bob.core.bots.tainted(&bob.bot.chat_id).is_none());

    wait_for(
        || {
            exchanges(&alice, ExchangeDirection::Incoming)
                .first()
                .is_some_and(|e| e.state == ExchangeState::AwaitingReview)
        },
        "the draft waits for Alice",
    )
    .await;
    let draft = exchanges(&alice, ExchangeDirection::Incoming).remove(0);
    assert!(draft.awaiting_review());
    assert!(
        draft.answer.as_deref().unwrap().contains("[redacted]"),
        "drafts are scrubbed"
    );
    // Bob may see its progress (delivered, working, awaiting_review), never
    // an answer.
    let waiting = exchanges(&bob, ExchangeDirection::Outgoing).remove(0);
    assert!(
        !matches!(
            waiting.state,
            ExchangeState::Answered | ExchangeState::Declined
        ) && waiting.answer.is_none(),
        "nothing was sent: {waiting:?}"
    );
    let main_runs = |side: &Side| {
        side.runs
            .lock()
            .unwrap()
            .iter()
            .filter(|(r, _)| r.prompt.starts_with("[Answer from"))
            .count()
    };
    assert_eq!(main_runs(&bob), 0);

    mutate(
        &alice.core,
        serde_json::json!({
            "op": "reviewBotExchange",
            "exchangeId": draft.id,
            "send": true,
            "text": "Yes, Friday works.",
        }),
    )
    .await;
    wait_for(
        || exchanges(&bob, ExchangeDirection::Outgoing)[0].state == ExchangeState::Answered,
        "the reviewed answer reaches Bob",
    )
    .await;
    assert_eq!(
        exchanges(&bob, ExchangeDirection::Outgoing)[0]
            .answer
            .as_deref(),
        Some("Yes, Friday works.")
    );
    wait_for(
        || settled(&alice, ExchangeState::Answered),
        "Alice's row records the sent answer",
    )
    .await;
    // Late: a durable run in Bob's main chat, attributed, and tainted.
    wait_for(
        || main_runs(&bob) == 1,
        "the late answer runs in the workspace chat",
    )
    .await;
    let entries = common::entries(&bob.core, &bob.bot.chat_id);
    let user = entries
        .iter()
        .find(|e| e.role == MessageRole::User)
        .expect("the late answer is in the transcript");
    let shown = match &user.parts[0] {
        cypher_doc::MessagePart::Text { text, .. } => text.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        shown,
        "[Answer from \"Billing bot\" (Alice's workspace) to your question \"Can we ship Friday?\"]\n\n\
         Yes, Friday works."
    );
    assert!(bob.core.bots.tainted(&bob.bot.chat_id).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn replies_off_decline_without_running() {
    let (_hub, alice, bob) = pair().await;
    let connection_id = connect(&alice, &bob).await;
    mutate(
        &alice.core,
        serde_json::json!({ "op": "updateBotConnection", "connectionId": connection_id, "replyMode": "off" }),
    )
    .await;
    let answer = tool(
        &bob.core,
        &bob.bot.chat_id,
        "ask_bot",
        serde_json::json!({ "bot": "Billing bot", "question": "Anything?", "wait_s": 30 }),
    )
    .await;
    assert_eq!(
        answer,
        "\"Billing bot\" (Alice's workspace) declined to answer."
    );
    assert!(
        peer_questions(&alice).is_empty(),
        "no run for a declined question"
    );
    wait_for(
        || settled(&alice, ExchangeState::Declined),
        "Alice's row records the decline",
    )
    .await;
    assert!(bob.core.bots.tainted(&bob.bot.chat_id).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_disconnects_both_sides() {
    let (_hub, alice, bob) = pair().await;
    let connection_id = connect(&alice, &bob).await;
    mutate(
        &bob.core,
        serde_json::json!({ "op": "revokeBotConnection", "connectionId": connection_id }),
    )
    .await;
    wait_for(
        || connections(&bob)[0].revoked_at.is_some(),
        "Bob's host revokes on the Edge",
    )
    .await;
    wait_for(
        || connections(&alice)[0].revoked_at.is_some(),
        "Alice's host learns of it",
    )
    .await;
    assert_eq!(activity(&bob, ActivityKind::ConnectionRevoked).len(), 1);
    assert_eq!(activity(&alice, ActivityKind::ConnectionRevoked).len(), 1);
    let refused = tool(
        &bob.core,
        &bob.bot.chat_id,
        "ask_bot",
        serde_json::json!({ "bot": "Billing bot", "question": "Still there?" }),
    )
    .await;
    assert!(refused.starts_with("No connected workspace"), "{refused}");
    let listed = tool(
        &alice.core,
        &alice.bot.chat_id,
        "list_connections",
        serde_json::json!({}),
    )
    .await;
    assert!(
        listed.starts_with("You have no connected workspaces"),
        "{listed}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invites_can_be_revoked_and_used_only_once() {
    let hub = FakeHub::new();
    let alice = side(&hub, "alice", "Alice", "Billing bot").await;
    let bob = side(&hub, "bob", "Bob", "Release bot").await;
    let carol = side(&hub, "carol", "Carol", "Ops bot").await;
    let invite = call(
        &alice.core,
        cypher_rpc::methods::CREATE_BOT_INVITE,
        serde_json::json!({ "botId": alice.bot.id }),
    )
    .await;
    let code = invite["code"].as_str().unwrap().to_string();
    call(
        &bob.core,
        cypher_rpc::methods::REDEEM_BOT_INVITE,
        serde_json::json!({ "botId": bob.bot.id, "code": code }),
    )
    .await;
    let used = cypher_rpc::memory_client(carol.core.rpc_service())
        .call(
            cypher_rpc::methods::REDEEM_BOT_INVITE,
            serde_json::json!({ "botId": carol.bot.id, "code": code }),
        )
        .await;
    assert!(used.is_err(), "one use");

    let second = call(
        &alice.core,
        cypher_rpc::methods::CREATE_BOT_INVITE,
        serde_json::json!({ "botId": alice.bot.id }),
    )
    .await;
    let invite_id = second["inviteId"].as_str().unwrap().to_string();
    mutate(
        &alice.core,
        serde_json::json!({ "op": "revokeBotInvite", "inviteId": invite_id }),
    )
    .await;
    wait_for(
        || {
            alice
                .core
                .workspace
                .read_bot_records()
                .invites
                .iter()
                .any(|i| i.id == invite_id && i.revoked_at.is_some())
        },
        "the host revokes the invite",
    )
    .await;
    let revoked = cypher_rpc::memory_client(carol.core.rpc_service())
        .call(
            cypher_rpc::methods::REDEEM_BOT_INVITE,
            serde_json::json!({ "botId": carol.bot.id, "code": second["code"] }),
        )
        .await;
    assert!(revoked.is_err(), "a revoked invite can't be redeemed");
    assert!(connections(&carol).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_edge_the_network_asks_you_to_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    let core = common::engine_at(&dir.path().join("data"), harness(Runs::default()));
    let client = cypher_rpc::memory_client(core.rpc_service());
    let bot: Bot = serde_json::from_value(
        client
            .call(
                cypher_rpc::methods::CREATE_BOT,
                serde_json::json!({ "targetDeviceId": core.device_id, "name": "Local bot" }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let invite = client
        .call(
            cypher_rpc::methods::CREATE_BOT_INVITE,
            serde_json::json!({ "botId": bot.id }),
        )
        .await
        .expect_err("no Edge");
    assert!(
        invite.to_string().contains(cypher_engine::bots::SIGNED_OUT),
        "{invite}"
    );
    let redeem = client
        .call(
            cypher_rpc::methods::REDEEM_BOT_INVITE,
            serde_json::json!({ "botId": bot.id, "code": "room.secret" }),
        )
        .await
        .expect_err("no Edge");
    assert!(redeem.to_string().contains(cypher_engine::bots::SIGNED_OUT));
    for (name, args) in [
        ("list_connections", serde_json::json!({})),
        (
            "ask_bot",
            serde_json::json!({ "bot": "x", "question": "q" }),
        ),
    ] {
        assert_eq!(
            tool(&core, &bot.chat_id, name, args).await,
            cypher_engine::bots::SIGNED_OUT
        );
    }
}

/// The owner's photo travels with their profile: the connecting side gets
/// it, a new one reaches existing connections when the profile is
/// republished, and a URL off the identity providers' avatar hosts never
/// lands.
#[tokio::test(flavor = "multi_thread")]
async fn the_owners_photo_reaches_connections_and_follows_changes() {
    let (_hub, alice, bob) = pair().await;
    let photo = "https://avatars.githubusercontent.com/u/1?v=4";
    alice.owner.lock().unwrap().avatar_url = Some(photo.into());
    bob.owner.lock().unwrap().avatar_url = Some("https://evil.example/pixel.png".into());
    let connection = connect(&alice, &bob).await;
    let peer_of = |side: &Side| {
        connections(side)
            .into_iter()
            .find(|c| c.id == connection)
            .map(|c| c.peer)
            .expect("the connection")
    };
    assert_eq!(peer_of(&bob).owner_avatar_url.as_deref(), Some(photo));
    assert_eq!(peer_of(&alice).owner_avatar_url, None, "not an avatar host");

    let newer = "https://lh3.googleusercontent.com/a/new-photo";
    alice.owner.lock().unwrap().avatar_url = Some(newer.into());
    // Any publish carries the change; a new invite publishes.
    call(
        &alice.core,
        cypher_rpc::methods::CREATE_BOT_INVITE,
        serde_json::json!({ "targetDeviceId": alice.core.device_id, "botId": alice.bot.id }),
    )
    .await;
    wait_for(
        || peer_of(&bob).owner_avatar_url.as_deref() == Some(newer),
        "the new photo reaches the connection",
    )
    .await;
    assert_eq!(peer_of(&bob).owner_name, "Alice");
    let connected = activity(&bob, ActivityKind::ConnectionAdded);
    assert_eq!(
        connected.len(),
        1,
        "an update, not a second connection: {connected:?}"
    );
}
