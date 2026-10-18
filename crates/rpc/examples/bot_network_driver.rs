//! Two-user bot network smoke driver (`scripts/e2e-bots.sh` runs it).
//!
//! Connects to two running headless engines, each signed in as a different
//! user against a real (local, dev-auth) Edge, and drives the bot network
//! (docs/design/agent-bots.md §11) end to end:
//!
//! 1. a bot on each engine, both answering with the mock harness;
//! 2. `CreateBotInvite` on A, `RedeemBotInvite` on B, and A's connection row
//!    arriving from its room's `link` entry (nudge → pull);
//! 3. B's bot asks A's bot with `ask_bot`; A's host runs the question in its
//!    peer thread and the answer comes back through both rooms;
//! 4. B revokes, and A's row turns revoked.
//!
//! Prints `PASS`/`FAIL` lines; exits nonzero on failure.

#![allow(clippy::print_stdout, clippy::print_stderr)] // a command-line tool

use std::time::{Duration, Instant};

use cypher_rpc::{RpcClient, connect_local, methods};

const STEP_TIMEOUT: Duration = Duration::from_secs(90);

fn fail(message: &str) -> ! {
    eprintln!("FAIL: {message}");
    std::process::exit(1);
}

fn pass(message: &str) {
    println!("PASS: {message}");
}

async fn call(client: &RpcClient, method: &str, params: serde_json::Value) -> serde_json::Value {
    client
        .call(method, params)
        .await
        .unwrap_or_else(|err| fail(&format!("{method}: {err}")))
}

/// A bot on `client`'s device whose chat runs the mock harness.
async fn bot(client: &RpcClient, name: &str) -> serde_json::Value {
    let device = call(client, methods::ENGINE_INFO, serde_json::json!({})).await["deviceId"]
        .as_str()
        .unwrap_or_else(|| fail("EngineInfo without deviceId"))
        .to_string();
    let bot = call(
        client,
        methods::CREATE_BOT,
        serde_json::json!({ "targetDeviceId": device, "name": name }),
    )
    .await;
    call(
        client,
        methods::MUTATE,
        serde_json::json!({
            "op": "setChatConfig",
            "chatId": bot["chatId"],
            "config": { "harness": "mock", "model": null, "reasoning": null, "sandbox": "read-only" },
        }),
    )
    .await;
    bot
}

/// Poll the bot records until `found` picks something out.
async fn wait_records<T>(
    client: &RpcClient,
    what: &str,
    mut found: impl FnMut(&serde_json::Value) -> Option<T>,
) -> T {
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        let mut rx = client
            .subscribe(methods::WATCH_BOT_RECORDS, serde_json::json!({}))
            .await
            .unwrap_or_else(|err| fail(&format!("{what}: subscribe: {err}")));
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                fail(&format!("{what}: timed out"));
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(records)) => {
                    if let Some(value) = found(&records) {
                        return value;
                    }
                }
                Ok(None) => break,
                Err(_) => fail(&format!("{what}: timed out")),
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let a_dir = args.next().expect("user A's data directory");
    let b_dir = args.next().expect("user B's data directory");
    let socket = |dir: &str| cypher_env::ipc_socket(std::path::Path::new(dir)).unwrap();
    let a = connect_local(&socket(&a_dir))
        .await
        .unwrap_or_else(|err| fail(&format!("connect A: {err}")));
    let b = connect_local(&socket(&b_dir))
        .await
        .unwrap_or_else(|err| fail(&format!("connect B: {err}")));

    let a_bot = bot(&a, "Billing bot").await;
    let b_bot = bot(&b, "Release bot").await;
    pass("a bot on each engine");

    let invite = call(
        &a,
        methods::CREATE_BOT_INVITE,
        serde_json::json!({ "botId": a_bot["id"] }),
    )
    .await;
    let code = invite["code"]
        .as_str()
        .unwrap_or_else(|| fail("invite without a code"));
    pass("A created an invite");
    let connection = call(
        &b,
        methods::REDEEM_BOT_INVITE,
        serde_json::json!({ "botId": b_bot["id"], "code": code }),
    )
    .await;
    let connection_id = connection["id"]
        .as_str()
        .unwrap_or_else(|| fail("no id"))
        .to_string();
    if connection["peer"]["name"] != "Billing bot" {
        fail(&format!(
            "B's connection names the wrong peer: {connection}"
        ));
    }
    pass("B redeemed it");
    let peer = wait_records(&a, "A's connection row", |records| {
        records["connections"]
            .as_array()?
            .iter()
            .find(|c| c["id"] == connection_id.as_str())
            .map(|c| c["peer"]["name"].clone())
    })
    .await;
    if peer != "Release bot" {
        fail(&format!("A's connection names the wrong peer: {peer}"));
    }
    pass("A learned of the connection from its room");

    let answer = call(
        &b,
        methods::BOT_TOOL,
        serde_json::json!({
            "chatId": b_bot["chatId"],
            "tool": "ask_bot",
            "args": { "bot": "Billing bot", "question": "How is billing doing?", "wait_s": 60 },
        }),
    )
    .await;
    let text = answer["text"].as_str().unwrap_or_default();
    if !text.contains("answered:") {
        fail(&format!("ask_bot did not get an answer: {text}"));
    }
    pass(&format!(
        "B's bot asked A's bot and got: {}",
        text.lines().nth(3).unwrap_or_default()
    ));
    wait_records(&a, "A's exchange row", |records| {
        records["exchanges"]
            .as_array()?
            .iter()
            .find(|e| e["direction"] == "incoming" && e["state"] == "answered")
            .map(|_| ())
    })
    .await;
    pass("A recorded the answered exchange");

    call(
        &b,
        methods::MUTATE,
        serde_json::json!({ "op": "revokeBotConnection", "connectionId": connection_id }),
    )
    .await;
    wait_records(&a, "A's revoked row", |records| {
        records["connections"]
            .as_array()?
            .iter()
            .find(|c| c["id"] == connection_id.as_str() && !c["revokedAt"].is_null())
            .map(|_| ())
    })
    .await;
    pass("B revoked; A's connection is revoked too");
    println!("PASS: bot network end to end");
}
