//! `cypher pair`: Settings → Devices → Pairing for a device without the
//! desktop UI, such as a headless Linux engine. Every call goes to this
//! device's own engine over IPC; the relay refuses them, so no other device
//! can pair this one remotely.

use std::future::Future;
use std::io::{IsTerminal, Write};
use std::time::Duration;

use anyhow::{Context, bail};
use cypher_engine::device_trust::{PairingStatus, TrustMember, TrustState, TrustStatus};
use cypher_engine::{EngineConfig, WorkspaceScope};
use cypher_rpc::{RpcClient, RpcError, methods};

use crate::setup_cli::{Cancel, cancelled, clean_label};

#[derive(clap::Subcommand)]
pub enum PairCommand {
    /// Join your paired devices with the code one of them shows.
    Join {
        /// The code from Add a device, like ABCD-EFGH-JKMN-PQRS-TVWX.
        code: String,
    },
    /// Show a code for another device to enter, then approve it here.
    Add,
}

/// How long to wait for the engine to hear from the server before reporting
/// a status that hasn't.
const FIRST_SYNC: Duration = Duration::from_secs(10);

const JOIN_HINT: &str = "On a paired device, open Settings → Devices → Pairing and click Add a device. Then run `cypher pair join <code>` here.";

pub async fn run(config: EngineConfig, command: Option<PairCommand>) -> anyhow::Result<()> {
    if matches!(command, Some(PairCommand::Add)) && !std::io::stdin().is_terminal() {
        bail!(
            "Approving a device needs a terminal. Run `cypher pair add` in an interactive shell (`ssh -t` for a remote command)."
        );
    }
    let Some(live) = crate::setup_cli::connect(&config).await? else {
        bail!("Cypher isn't running on this device. Run `cypher setup`, then try again.");
    };
    // Pairing rides the edge: a local-only engine has no group to join.
    if live.scope == WorkspaceScope::Local {
        bail!(
            "Pairing needs this device connected to your Cypher account. Run `cypher setup` to connect it."
        );
    }
    let (cancel_tx, cancel) = tokio::sync::watch::channel(false);
    let signals = tokio::spawn(async move {
        let _ = cypher_engine::shutdown_signal().await;
        let _ = cancel_tx.send(true);
    });
    let result = match command {
        None => show(&live.client).await,
        Some(PairCommand::Join { code }) => join(&live.client, &code, &cancel).await,
        Some(PairCommand::Add) => add(&live.client, confirm, &cancel).await,
    };
    signals.abort();
    result
}

/// `Pairing:` for `cypher status`, or `None` when the engine doesn't say.
pub async fn status_line(client: &RpcClient) -> Option<String> {
    let status = tokio::time::timeout(Duration::from_secs(2), async {
        Watch::open(client).await.ok()?.next().await.ok()
    })
    .await
    .ok()
    .flatten()?;
    Some(
        match status.state {
            TrustState::Paired => "paired",
            TrustState::Unpaired => "not paired · `cypher pair`",
            TrustState::Removed => "removed · `cypher pair`",
            TrustState::Unknown => "checking…",
        }
        .into(),
    )
}

async fn show(client: &RpcClient) -> anyhow::Result<()> {
    let status = Watch::open(client).await?.settled().await?;
    print!("{}", status_text(&status));
    Ok(())
}

/// Ask to join with `code`, then wait for the member's approval.
async fn join(client: &RpcClient, code: &str, cancel: &Cancel) -> anyhow::Result<()> {
    let status = call(
        client,
        methods::JOIN_DEVICE_GROUP,
        serde_json::json!({ "code": code }),
    )
    .await?;
    println!("Asked to join. Approve this device on the one that showed the code.");
    println!("It will show this device's key: {}", status.fingerprint);
    println!("Waiting for approval… (Ctrl-C stops waiting)");
    // Opened after the call, so the first status already reflects the join.
    let mut watch = Watch::open(client).await?;
    loop {
        let status = tokio::select! {
            status = watch.next() => status?,
            _ = cancelled(cancel.clone()) => {
                let _ = call(client, methods::CANCEL_DEVICE_PAIRING, serde_json::json!({})).await;
                bail!("Stopped waiting. Run `cypher pair join` with a new code to try again.");
            }
        };
        match &status.pairing {
            _ if status.state == TrustState::Paired => {
                println!("\n✓ This device is paired.");
                print!("{}", members_text(&status.members));
                return Ok(());
            }
            Some(PairingStatus::Joining { error: None }) => {}
            Some(PairingStatus::Joining { error: Some(error) }) => bail!("{}", printable(error)),
            _ => bail!("Joining stopped. Run `cypher pair join` with a new code to try again."),
        }
    }
}

/// Show a code, wait for a device to enter it, and add that device if
/// `confirm` approves it.
async fn add<F>(
    client: &RpcClient,
    confirm: impl FnOnce(String) -> F,
    cancel: &Cancel,
) -> anyhow::Result<()>
where
    F: Future<Output = anyhow::Result<bool>>,
{
    let current = Watch::open(client).await?.settled().await?;
    match current.state {
        TrustState::Paired => {}
        TrustState::Unknown => bail!(
            "Couldn't reach the server: {}. Try again shortly.",
            printable(current.error.as_deref().unwrap_or("not synced yet"))
        ),
        TrustState::Unpaired | TrustState::Removed => {
            bail!("This device isn't paired, so it can't add others yet.\n{JOIN_HINT}")
        }
    }
    let status = call(client, methods::START_DEVICE_PAIRING, serde_json::json!({})).await?;
    let Some(PairingStatus::Inviting {
        code, expires_at, ..
    }) = &status.pairing
    else {
        bail!("The engine didn't show a code. Run `cypher pair add` again.");
    };
    println!("On the new device, open Settings → Devices → Pairing and enter this code:");
    println!("\n    {code}\n");
    println!("On a Linux device, run: cypher pair join {code}");
    println!(
        "The code works for {} minutes. Waiting for the other device… (Ctrl-C cancels)",
        minutes_until(*expires_at)
    );
    let stop = async || {
        let _ = call(
            client,
            methods::CANCEL_DEVICE_PAIRING,
            serde_json::json!({}),
        )
        .await;
    };
    let mut watch = Watch::open(client).await?;
    let device = loop {
        let status = tokio::select! {
            status = watch.next() => status?,
            _ = cancelled(cancel.clone()) => {
                stop().await;
                bail!("Canceled. No device was added.");
            }
        };
        match status.pairing {
            Some(PairingStatus::Inviting {
                request: Some(device),
                ..
            }) => break device,
            Some(PairingStatus::Inviting { request: None, .. }) => {}
            _ => bail!(
                "{} Run `cypher pair add` for a new code.",
                printable(
                    status
                        .error
                        .as_deref()
                        .unwrap_or("The code is no longer valid.")
                )
            ),
        }
    };
    let name = clean_label(&device.name);
    println!(
        "\n\u{201c}{name}\u{201d} wants to join. Its key is {}.",
        device.fingerprint
    );
    println!("The new device shows its own key; approve only if they match.");
    let approved = tokio::select! {
        approved = confirm(format!("Add \u{201c}{name}\u{201d} to your paired devices?")) => approved,
        _ = cancelled(cancel.clone()) => {
            stop().await;
            bail!("Canceled. No device was added.");
        }
    };
    if !matches!(approved, Ok(true)) {
        stop().await;
        approved?;
        println!("Not added.");
        return Ok(());
    }
    call(
        client,
        methods::APPROVE_DEVICE_PAIRING,
        serde_json::json!({}),
    )
    .await?;
    println!("✓ Added \u{201c}{name}\u{201d}.");
    Ok(())
}

/// Ask on the terminal; anything but yes declines.
async fn confirm(question: String) -> anyhow::Result<bool> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let line = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map(|_| line)
    })
    .await?
    .context("Couldn't read the answer.")?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// This device's trust status as the engine streams it.
struct Watch(tokio::sync::mpsc::Receiver<serde_json::Value>);

impl Watch {
    async fn open(client: &RpcClient) -> anyhow::Result<Self> {
        let rx = client
            .subscribe(methods::WATCH_DEVICE_TRUST, serde_json::json!({}))
            .await
            .map_err(engine_error)?;
        Ok(Self(rx))
    }

    async fn next(&mut self) -> anyhow::Result<TrustStatus> {
        loop {
            // The engine ends the stream when it has no pairing to offer
            // (device keys unavailable) or stops.
            let value = self.0.recv().await.context(
                "Pairing isn't available from this device's engine. Check `cypher logs`.",
            )?;
            if let Ok(status) = serde_json::from_value(value) {
                return Ok(status);
            }
        }
    }

    /// The first status that has heard from the server, or the last one
    /// before [`FIRST_SYNC`].
    async fn settled(&mut self) -> anyhow::Result<TrustStatus> {
        let deadline = tokio::time::Instant::now() + FIRST_SYNC;
        let mut status = self.next().await?;
        while status.state == TrustState::Unknown && status.error.is_none() {
            match tokio::time::timeout_at(deadline, self.next()).await {
                Ok(next) => status = next?,
                Err(_) => break,
            }
        }
        Ok(status)
    }
}

async fn call(
    client: &RpcClient,
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<TrustStatus> {
    let value = client.call(method, params).await.map_err(engine_error)?;
    serde_json::from_value(value).context("The engine's pairing reply was unreadable.")
}

fn engine_error(err: RpcError) -> anyhow::Error {
    match err {
        // The engine's own message is written for people.
        RpcError::Failed(message) => anyhow::anyhow!(printable(&message)),
        other => anyhow::anyhow!(
            "The engine didn't answer: {}",
            printable(&other.to_string())
        ),
    }
}

/// `text` without control characters. Errors can carry the server's words,
/// and the server is exactly what pairing doesn't trust.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

fn minutes_until(epoch_ms: u64) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    epoch_ms.saturating_sub(now).div_ceil(60_000)
}

/// `cypher pair` without a subcommand. Pure.
fn status_text(status: &TrustStatus) -> String {
    let mut out = String::new();
    let state = match status.state {
        TrustState::Paired => "paired".to_string(),
        TrustState::Unpaired => "not paired".to_string(),
        TrustState::Removed => "removed from your paired devices".to_string(),
        TrustState::Unknown => match &status.error {
            Some(error) => format!("unknown · couldn't reach the server: {}", printable(error)),
            None => "checking… (not synced with the server yet)".to_string(),
        },
    };
    out.push_str(&format!("Pairing:  {state}\n"));
    out.push_str(&format!("Key:      {}\n", status.fingerprint));
    if status.conflict {
        out.push_str(
            "Warning:  the server's list of paired devices disagrees with this device's copy.\n          This device keeps its own copy. If you didn't change your devices,\n          someone may be tampering with the server.\n",
        );
    }
    match &status.pairing {
        Some(PairingStatus::Inviting { code, .. }) => {
            out.push_str(&format!(
                "Code:     {code} (waiting for the other device)\n"
            ));
        }
        Some(PairingStatus::Joining { error: None }) => {
            out.push_str("Joining:  waiting for approval on your other device\n");
        }
        Some(PairingStatus::Joining { error: Some(error) }) => {
            out.push_str(&format!("Joining:  failed · {}\n", printable(error)));
        }
        None => {}
    }
    match status.state {
        TrustState::Paired => {
            out.push_str(&members_text(&status.members));
            out.push_str(&format!(
                "Policy:   {}\n",
                if status.require_paired {
                    "only paired devices can connect"
                } else {
                    "any device signed in to your account can connect"
                }
            ));
            out.push_str("Next:     `cypher pair add` adds another device\n");
        }
        TrustState::Unpaired | TrustState::Removed => {
            out.push_str(&format!("Next:     {JOIN_HINT}\n"));
        }
        TrustState::Unknown => {}
    }
    out
}

/// The `Devices:` block. Pure.
fn members_text(members: &[TrustMember]) -> String {
    let mut out = String::new();
    for (ix, member) in members.iter().enumerate() {
        let label = if ix == 0 { "Devices:" } else { "" };
        let name = clean_label(&member.name);
        let detail = if member.is_self {
            "this device".to_string()
        } else {
            format!("key {}", member.fingerprint)
        };
        out.push_str(&format!("{label:<10}{name} · {detail}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, is_self: bool) -> TrustMember {
        TrustMember {
            id: format!("{name}-id"),
            name: name.into(),
            fingerprint: format!("{name}-fp"),
            is_self,
        }
    }

    fn status(state: TrustState, pairing: Option<PairingStatus>) -> TrustStatus {
        TrustStatus {
            device_id: "linux-id".into(),
            fingerprint: "linux-fp".into(),
            state,
            members: if state == TrustState::Paired {
                vec![member("gpu-server", true), member("MacBook", false)]
            } else {
                Vec::new()
            },
            require_paired: false,
            pairing,
            conflict: false,
            error: None,
        }
    }

    /// An engine fake: answers each call with the next queued status, and
    /// each `WatchDeviceTrust` with the queued stream. Records the methods.
    struct Fake {
        client: RpcClient,
        methods: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        _task: tokio::task::JoinHandle<()>,
    }

    fn fake(calls: Vec<Result<TrustStatus, &'static str>>, stream: Vec<TrustStatus>) -> Fake {
        let (out, mut requests) = tokio::sync::mpsc::channel::<String>(16);
        let (responses, input) = tokio::sync::mpsc::channel::<String>(64);
        let client = RpcClient::new(out, input);
        let methods_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = methods_seen.clone();
        let task = tokio::spawn(async move {
            let mut calls = calls.into_iter();
            while let Some(request) = requests.recv().await {
                let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                let Some(method) = request["method"].as_str() else {
                    continue;
                };
                seen.lock().unwrap().push(method.to_string());
                let id = &request["id"];
                if method == methods::WATCH_DEVICE_TRUST {
                    for item in &stream {
                        let frame = serde_json::json!({ "id": id, "item": item });
                        responses.send(frame.to_string()).await.unwrap();
                    }
                    continue;
                }
                let reply = calls
                    .next()
                    .unwrap_or_else(|| Ok(status(TrustState::Paired, None)));
                let frame = match reply {
                    Ok(reply) => serde_json::json!({ "id": id, "ok": reply }),
                    Err(err) => serde_json::json!({ "id": id, "err": err }),
                };
                responses.send(frame.to_string()).await.unwrap();
            }
        });
        Fake {
            client,
            methods: methods_seen,
            _task: task,
        }
    }

    fn never_cancel() -> (tokio::sync::watch::Sender<bool>, Cancel) {
        tokio::sync::watch::channel(false)
    }

    fn inviting(request: Option<TrustMember>) -> Option<PairingStatus> {
        Some(PairingStatus::Inviting {
            code: "ABCD-EFGH-JKMN-PQRS-TVWX".into(),
            expires_at: 0,
            request,
        })
    }

    #[tokio::test]
    async fn join_waits_for_approval_and_reports_the_group() {
        let joining = Some(PairingStatus::Joining { error: None });
        let engine = fake(
            vec![Ok(status(TrustState::Unpaired, joining.clone()))],
            vec![
                status(TrustState::Unpaired, joining),
                status(TrustState::Paired, None),
            ],
        );
        let (_tx, cancel) = never_cancel();
        join(&engine.client, "abcd-efgh", &cancel).await.unwrap();
        assert_eq!(
            engine.methods.lock().unwrap()[..2],
            [methods::JOIN_DEVICE_GROUP, methods::WATCH_DEVICE_TRUST]
        );
    }

    #[tokio::test]
    async fn join_surfaces_the_engines_failure() {
        let engine = fake(
            vec![Ok(status(
                TrustState::Unpaired,
                Some(PairingStatus::Joining { error: None }),
            ))],
            vec![status(
                TrustState::Unpaired,
                Some(PairingStatus::Joining {
                    error: Some(
                        "No approval arrived. Show a new code on your other device.".into(),
                    ),
                }),
            )],
        );
        let (_tx, cancel) = never_cancel();
        let err = join(&engine.client, "code", &cancel).await.unwrap_err();
        assert!(err.to_string().contains("No approval arrived"));

        let engine = fake(vec![Err("That code isn't valid.")], vec![]);
        let err = join(&engine.client, "nope", &cancel).await.unwrap_err();
        assert_eq!(err.to_string(), "That code isn't valid.");
    }

    #[tokio::test]
    async fn join_cancels_when_interrupted() {
        let joining = Some(PairingStatus::Joining { error: None });
        let engine = fake(
            vec![Ok(status(TrustState::Unpaired, joining.clone()))],
            vec![status(TrustState::Unpaired, joining)],
        );
        let (tx, cancel) = never_cancel();
        let interrupt = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(true).unwrap();
        };
        let (result, ()) = tokio::join!(join(&engine.client, "code", &cancel), interrupt);
        assert!(result.unwrap_err().to_string().contains("Stopped waiting"));
        assert_eq!(
            engine.methods.lock().unwrap().last().map(String::as_str),
            Some(methods::CANCEL_DEVICE_PAIRING)
        );
    }

    #[tokio::test]
    async fn add_approves_only_when_confirmed() {
        let request = Some(member("MacBook", false));
        for (answer, last) in [
            (true, methods::APPROVE_DEVICE_PAIRING),
            (false, methods::CANCEL_DEVICE_PAIRING),
        ] {
            let engine = fake(
                vec![Ok(status(TrustState::Paired, inviting(None)))],
                vec![
                    status(TrustState::Paired, inviting(None)),
                    status(TrustState::Paired, inviting(request.clone())),
                ],
            );
            let (_tx, cancel) = never_cancel();
            let asked = std::sync::Mutex::new(None);
            add(
                &engine.client,
                |question| {
                    *asked.lock().unwrap() = Some(question);
                    async move { Ok(answer) }
                },
                &cancel,
            )
            .await
            .unwrap();
            assert!(
                asked
                    .lock()
                    .unwrap()
                    .as_deref()
                    .unwrap()
                    .contains("MacBook")
            );
            let seen = engine.methods.lock().unwrap();
            assert!(seen.iter().any(|m| m == methods::START_DEVICE_PAIRING));
            assert_eq!(seen.last().map(String::as_str), Some(last));
            assert!(
                answer || !seen.iter().any(|m| m == methods::APPROVE_DEVICE_PAIRING),
                "a declined device is never approved"
            );
        }
    }

    #[tokio::test]
    async fn add_reports_an_expired_code_and_an_unpaired_device() {
        // Called only if there were a device to approve, and then it fails.
        let never = |_: String| async { Err::<bool, _>(anyhow::anyhow!("asked to approve")) };
        let mut expired = status(TrustState::Paired, None);
        expired.error = Some("The code expired. Show a new one.".into());
        let engine = fake(
            vec![Ok(status(TrustState::Paired, inviting(None)))],
            vec![status(TrustState::Paired, inviting(None)), expired],
        );
        let (_tx, cancel) = never_cancel();
        let err = add(&engine.client, never, &cancel).await.unwrap_err();
        assert!(err.to_string().contains("The code expired"), "{err}");

        let engine = fake(vec![], vec![status(TrustState::Unpaired, None)]);
        let err = add(&engine.client, never, &cancel).await.unwrap_err();
        assert!(err.to_string().contains("cypher pair join"), "{err}");
        assert!(
            !engine
                .methods
                .lock()
                .unwrap()
                .iter()
                .any(|m| m == methods::START_DEVICE_PAIRING),
            "an unpaired device shows no code"
        );
    }

    #[test]
    fn status_names_the_next_step_for_each_state() {
        let paired = status_text(&status(TrustState::Paired, None));
        assert!(paired.starts_with("Pairing:  paired\n"));
        assert!(paired.contains("Devices:  gpu-server · this device\n"));
        assert!(paired.contains("          MacBook · key MacBook-fp\n"));
        assert!(paired.contains("cypher pair add"));
        for state in [TrustState::Unpaired, TrustState::Removed] {
            assert!(status_text(&status(state, None)).contains("cypher pair join <code>"));
        }
        let mut offline = status(TrustState::Unknown, None);
        offline.error = Some("timed out\x1b[2J".into());
        let text = status_text(&offline);
        assert!(text.contains("couldn't reach the server: timed out[2J"));
        assert!(
            !text.contains('\x1b'),
            "server text can't drive the terminal"
        );
    }

    #[test]
    fn device_names_cannot_inject_terminal_controls() {
        let mut evil = member("evil\x1b]0;owned\x07\nname", false);
        evil.fingerprint = "0123456789abcdef".into();
        let text = members_text(&[evil]);
        assert!(!text.contains('\x1b') && !text.contains('\x07'));
        assert_eq!(text.lines().count(), 1);
    }
}
