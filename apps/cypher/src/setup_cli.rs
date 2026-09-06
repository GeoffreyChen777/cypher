//! Linux device onboarding. Authentication and Runtime installation finish
//! before the service starts; a paused managed service is restored on failure.
use anyhow::{Context, bail};
use cypher_engine::{Auth, AuthState, Engine, EngineConfig, InstanceLock, WorkspaceScope};
use cypher_rpc::{RpcClient, methods};
use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(clap::Args, Default)]
pub struct SetupOptions {
    /// Skip account connection (new local-only devices only).
    #[arg(long)]
    pub local: bool,
    /// Run in this terminal instead of installing a background service.
    #[arg(long)]
    pub foreground: bool,
    /// Never prompt. Requires --local or an existing complete account.
    #[arg(long)]
    pub non_interactive: bool,
}

struct LiveEngine {
    client: RpcClient,
    scope: WorkspaceScope,
    device: String,
}

fn complete_account(config: &EngineConfig) -> bool {
    matches!(
        Auth::saved_state(&config.data_dir),
        Some(AuthState::SignedIn {
            org_id: Some(_),
            ..
        })
    )
}

fn marker(data: &Path) -> PathBuf {
    data.join("setup-completed.json")
}

fn runtime_label(data: &Path) -> String {
    let paths = cypher_engine::pi_runtime::PiRuntimePaths::for_data_dir(data);
    if !paths.installed() {
        return "not installed".into();
    }
    std::fs::read(paths.current.join("runtime.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| {
            v["piVersion"]
                .as_str()
                .map(|version| format!("Pi {}", clean_label(version)))
        })
        .unwrap_or_else(|| "installed (version unavailable)".into())
}

fn clean_label(label: &str) -> String {
    label
        .chars()
        .filter(|ch| !ch.is_control())
        .take(100)
        .collect()
}

type Cancel = tokio::sync::watch::Receiver<bool>;
fn check_cancel(cancel: &Cancel) -> anyhow::Result<()> {
    if *cancel.borrow() {
        bail!("Setup canceled. Run `cypher setup` to continue.");
    }
    Ok(())
}
async fn cancelled(mut cancel: Cancel) {
    let _ = cancel.wait_for(|stop| *stop).await;
}

async fn prompt(prompt: &str, default: bool, cancel: &Cancel) -> anyhow::Result<bool> {
    check_cancel(cancel)?;
    print!("{prompt} {}", if default { "[Y/n] " } else { "[y/N] " });
    std::io::stdout().flush()?;
    let reader = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    });
    let line = tokio::select! {
        line=reader => line?,
        _=cancelled(cancel.clone())=>bail!("Setup canceled. Run `cypher setup` to continue."),
    }
    .context("Setup canceled: terminal input closed. Run `cypher setup` to continue.")?;
    match line.trim().to_ascii_lowercase().as_str() {
        "" => Ok(default),
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => bail!("Enter y or n. Run `cypher setup` to continue."),
    }
}

async fn connect(config: &EngineConfig) -> anyhow::Result<Option<LiveEngine>> {
    let client = match tokio::time::timeout(
        Duration::from_secs(2),
        cypher_rpc::connect_local(&config.ipc_socket),
    )
    .await
    {
        Ok(Ok(client)) => client,
        _ => {
            // A foreign/unresponsive listener must never be treated as an
            // empty slot which setup can overwrite or stop.
            if cypher_rpc::probe_local(&config.ipc_socket).await? {
                bail!(
                    "The IPC socket belongs to an unresponsive or different process. Check `cypher status --verbose`."
                );
            }
            return Ok(None);
        }
    };
    let value = tokio::time::timeout(
        Duration::from_secs(3),
        client.call(methods::ENGINE_INFO, serde_json::json!({})),
    )
    .await
    .context("Engine identity check timed out.")??;
    let info: cypher_engine::EngineInfo = serde_json::from_value(value)?;
    let expected = std::fs::read_to_string(config.data_dir.join("device-id")).unwrap_or_default();
    if expected.trim().is_empty() || info.device_id != expected.trim() {
        bail!(
            "The running engine belongs to a different data directory. Check CYPHER_DATA_DIR; nothing was changed."
        );
    }
    Ok(Some(LiveEngine {
        client,
        scope: info.workspace_scope,
        device: info.device_id,
    }))
}

async fn ensure_idle(live: &LiveEngine) -> anyhow::Result<()> {
    let active = tokio::time::timeout(Duration::from_secs(3), async {
        let mut sessions = live
            .client
            .subscribe(methods::WATCH_SESSIONS, serde_json::json!({}))
            .await?;
        let value = sessions.recv().await.ok_or(cypher_rpc::RpcError::Closed)?;
        let sessions: Vec<cypher_proto::Session> = serde_json::from_value(value).map_err(|_| {
            cypher_rpc::RpcError::Failed("Cannot determine active sessions.".into())
        })?;
        Ok::<_, cypher_rpc::RpcError>(sessions.iter().any(|s| {
            matches!(
                s.status,
                cypher_proto::SessionStatus::Working | cypher_proto::SessionStatus::AwaitingInput
            )
        }))
    })
    .await
    .context("Could not check active runs; the engine was not stopped.")??;
    if active {
        bail!(
            "Cypher is busy. Finish active runs, then run `cypher setup` again. Nothing was stopped."
        );
    }
    Ok(())
}

async fn wait_stopped(config: &EngineConfig) -> anyhow::Result<()> {
    for _ in 0..100 {
        if InstanceLock::holder(&config.data_dir).is_none()
            && !cypher_rpc::probe_local(&config.ipc_socket).await?
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("The previous engine has not stopped yet. Run `cypher setup` again shortly.");
}

async fn install_runtime(config: &EngineConfig, cancel: &Cancel) -> anyhow::Result<()> {
    check_cancel(cancel)?;
    let manager = cypher_engine::pi_runtime::PiRuntimeManager::spawn(
        config.edge_url.clone(),
        &config.data_dir,
    );
    if manager.status().installed && manager.status().version.is_some() {
        println!("✓ Pi Runtime installed");
        manager.shutdown().await;
        return Ok(());
    }
    println!("Installing Pi Runtime…");
    let mut updates = manager.watch_runtime();
    let install = manager.install_latest();
    tokio::pin!(install);
    let mut previous = 0;
    let result = loop {
        tokio::select! {
            _ = cancelled(cancel.clone()) => break Err("Setup canceled.".into()),
            result = &mut install => break result,
            changed = updates.changed() => {
                if changed.is_err() { continue; }
                let status = updates.borrow().clone();
                if let Some(total) = status.total_bytes.filter(|total| *total > 0) {
                    let percent = status.downloaded_bytes.saturating_mul(100).checked_div(total).unwrap_or(0).min(100);
                    if percent / 25 > previous / 25 {
                        println!("  {}%", percent / 25 * 25);
                        previous = percent;
                    }
                }
            }
        }
    };
    manager.shutdown().await;
    result.map_err(|error| {
        anyhow::anyhow!("Runtime installation failed: {error}\nRun `cypher setup` to retry.")
    })?;
    println!("✓ Pi Runtime installed");
    Ok(())
}

async fn ready(config: &EngineConfig, remote: bool, cancel: &Cancel) -> anyhow::Result<LiveEngine> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    loop {
        check_cancel(cancel)?;
        if let Some(live) = connect(config).await? {
            let ready = tokio::time::timeout(
                Duration::from_secs(3),
                live.client
                    .call(methods::ENGINE_READY, serde_json::json!({})),
            )
            .await;
            if matches!(ready, Ok(Ok(_))) {
                if !remote {
                    return Ok(live);
                }
                let status = tokio::time::timeout(
                    Duration::from_secs(3),
                    live.client
                        .call(methods::SYNC_STATUS, serde_json::json!({})),
                )
                .await;
                if let Ok(Ok(status)) = status
                    && live.scope == WorkspaceScope::Synced
                    && status["workspace"]["connected"] == true
                {
                    return Ok(live);
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "The service was started, but device readiness could not be confirmed. Check `cypher logs`, then run `cypher setup` to retry."
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn device_label(live: &LiveEngine) -> String {
    let name = tokio::time::timeout(Duration::from_secs(2), async {
        let mut devices = live
            .client
            .subscribe(methods::WATCH_DEVICES, serde_json::json!({}))
            .await
            .ok()?;
        let devices = devices.recv().await?;
        devices
            .as_array()?
            .iter()
            .find(|d| d["id"] == live.device)?["name"]
            .as_str()
            .map(str::to_string)
    })
    .await
    .ok()
    .flatten();
    clean_label(name.as_deref().unwrap_or(&live.device))
}

fn same_running_binary() -> bool {
    let Some(pid) = crate::daemon::setup_service_pid() else {
        return false;
    };
    matches!((std::fs::read_link(format!("/proc/{pid}/exe")), std::env::current_exe()),
        (Ok(running),Ok(current)) if running==current)
}

struct Recovery {
    paused: bool,
}

pub async fn run(mut config: EngineConfig, options: SetupOptions) -> anyhow::Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("This setup wizard is for Linux devices. Use Settings on the desktop app.");
    }
    if !config
        .workos_client_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
    {
        bail!(
            "Development auth overrides are not supported by guided setup. Use normal Cypher account settings, or run `cypher headless` explicitly for development mode."
        );
    }
    let interactive = std::io::stdin().is_terminal() && !options.non_interactive;
    if !interactive && !options.local && !complete_account(&config) {
        bail!(
            "Account connection needs a terminal. Run `cypher setup` in an SSH terminal, or use `cypher setup --local --non-interactive` for local-only setup."
        );
    }
    config.data_dir = std::path::absolute(&config.data_dir)?;
    std::fs::create_dir_all(&config.data_dir)?;
    // Separate coordinator lock: an idempotent readiness check may run while
    // the engine owns engine.lock, but two setup wizards may not interleave.
    let coordinator = config.data_dir.join("setup-coordinator");
    std::fs::create_dir_all(&coordinator)?;
    let _setup_lock = InstanceLock::acquire(&coordinator).map_err(|_| {
        anyhow::anyhow!("Another setup is running. Finish it before starting setup again.")
    })?;
    let mut recovery = Recovery { paused: false };
    let (cancel_tx, cancel) = tokio::sync::watch::channel(false);
    let signals = tokio::spawn(async move {
        let _ = cypher_engine::shutdown_signal().await;
        let _ = cancel_tx.send(true);
    });
    let result = run_inner(&config, &options, interactive, &mut recovery, &cancel).await;
    if recovery.paused {
        match crate::daemon::setup_start() {
            Ok(()) => eprintln!("Previous background service restarted."),
            Err(_) => eprintln!(
                "The previous service could not be restarted. Run `cypher setup` to recover."
            ),
        }
    }
    drop(_setup_lock);
    let result = match result {
        Ok(true) => run_foreground(&config, &cancel).await,
        Ok(false) => Ok(()),
        Err(error) => Err(error),
    };
    signals.abort();
    let _ = signals.await;
    result
}

async fn run_foreground(config: &EngineConfig, cancel: &Cancel) -> anyhow::Result<()> {
    check_cancel(cancel)?;
    let mut child = tokio::process::Command::new(std::env::current_exe()?)
        .arg("headless")
        .env("CYPHER_DATA_DIR", &config.data_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    tokio::select! {
        status=child.wait()=>{
            if !status?.success() {bail!("Foreground engine stopped unexpectedly. Check `cypher logs`.");}
        }
        _=cancelled(cancel.clone())=>{
            if let Some(pid)=child.id() {
                #[cfg(unix)]
                unsafe { libc::kill(pid as libc::pid_t,libc::SIGTERM); }
            }
            if tokio::time::timeout(Duration::from_secs(10),child.wait()).await.is_err() {
                child.kill().await?;
            }
        }
    }
    Ok(())
}

async fn run_inner(
    config: &EngineConfig,
    options: &SetupOptions,
    interactive: bool,
    recovery: &mut Recovery,
    cancel: &Cancel,
) -> anyhow::Result<bool> {
    println!("Cypher setup\n");
    let signed_in = complete_account(config);
    let remote = if options.local {
        if Auth::saved_state(&config.data_dir).is_some() {
            bail!(
                "This device already has an account session. It was preserved; use `cypher logout` before choosing local-only setup."
            );
        }
        false
    } else if signed_in {
        true
    } else if interactive {
        println!("Connect this device to Cypher desktop. Existing local chats will stay local.");
        prompt("Connect to your Cypher account?", true, cancel).await?
    } else {
        false
    };
    if !remote && Auth::saved_state(&config.data_dir).is_some() {
        bail!(
            "An existing account session was preserved. Complete account connection, or use `cypher logout` before choosing local-only setup."
        );
    }
    let bus = !options.foreground && crate::daemon::user_bus_available();
    let foreground = if options.foreground {
        true
    } else if !bus {
        if !interactive {
            bail!(
                "No systemd user service is available. Run `cypher setup --foreground` in a terminal instead."
            );
        }
        if !prompt(
            "Background services are unavailable. Run in this terminal (stops when it closes)?",
            false,
            cancel,
        )
        .await?
        {
            bail!("Setup paused. Run `cypher setup` when a user service is available.");
        }
        true
    } else {
        false
    };
    let unit_exists = if foreground {
        false
    } else {
        crate::daemon::setup_unit_matches(config)?
    };
    let live = connect(config).await?;
    let runtime_paths = cypher_engine::pi_runtime::PiRuntimePaths::for_data_dir(&config.data_dir);
    let already_ready = live.as_ref().is_some_and(|live| {
        unit_exists
            && runtime_paths.installed()
            && same_running_binary()
            && ((remote && signed_in && live.scope == WorkspaceScope::Synced)
                || (!remote && live.scope != WorkspaceScope::Synced))
    });
    if !already_ready {
        check_cancel(cancel)?;
        if let Some(live) = &live {
            ensure_idle(live).await?;
            let pid =
                InstanceLock::holder(&config.data_dir).and_then(|p| p.trim().parse::<u32>().ok());
            if !unit_exists || pid.is_none() || crate::daemon::setup_service_pid() != pid {
                bail!(
                    "An unmanaged Cypher engine is already running. Close that instance, then run `cypher setup`; it was not stopped."
                );
            }
            recovery.paused = true;
            crate::daemon::setup_stop()?;
            wait_stopped(config).await?;
        }
        drop(live);
        check_cancel(cancel)?;
        // The same lock as the engine guards auth and offline Runtime writes.
        let lock = InstanceLock::acquire(&config.data_dir)?;
        if remote {
            let auth = Engine::build_auth(config).await;
            if !auth.workos_enabled() {
                bail!(
                    "Remote account setup requires a WorkOS-enabled Edge. Check CYPHER_EDGE_URL and CYPHER_WORKOS_CLIENT_ID."
                );
            }
            if matches!(auth.state(), AuthState::SignedIn { .. })
                && tokio::time::timeout(Duration::from_secs(20), auth.access_token())
                    .await
                    .context("Account verification timed out. Run `cypher setup` to retry.")?
                    .is_none()
                && matches!(auth.state(), AuthState::SignedIn { .. })
            {
                bail!(
                    "The account could not be verified. Check network access, then run `cypher setup`."
                );
            }
            if !matches!(
                auth.state(),
                AuthState::SignedIn {
                    org_id: Some(_),
                    ..
                }
            ) {
                if !interactive {
                    bail!("Account setup is incomplete. Run `cypher setup` in a terminal.");
                }
                cypher_engine::terminal_sign_in_until(&auth, cancelled(cancel.clone())).await?;
            }
            println!("✓ Account connected");
        } else {
            println!("✓ Local-only mode (not connected to desktop)");
        }
        install_runtime(config, cancel).await?;
        drop(lock);
        check_cancel(cancel)?;
        if foreground {
            println!("\nRunning in this terminal. Keep it open; Ctrl-C stops Cypher.");
            println!("This is not a persistent background service.");
            println!("Logs: cypher logs");
            return Ok(true);
        }
        if unit_exists {
            crate::daemon::enable_setup_service().context(
                "Could not enable the background service. Run `cypher logs` for details.",
            )?;
            crate::daemon::setup_start().context(
                "Could not start the background service. Run `cypher logs` for details.",
            )?;
        } else {
            crate::daemon::install_for_setup(&config.data_dir).context(
                "Could not install/start the background service. Run `cypher logs` for details.",
            )?;
        }
        recovery.paused = false;
    }
    let persistent = if crate::daemon::linger_enabled() || crate::daemon::enable_linger(false) {
        true
    } else if interactive
        && prompt(
            "Allow sudo to keep Cypher running after logout and reboot?",
            false,
            cancel,
        )
        .await?
    {
        crate::daemon::enable_linger(true)
    } else {
        false
    };
    let live = ready(config, remote, cancel).await?;
    let name = device_label(&live).await;
    check_cancel(cancel)?;
    let metadata = serde_json::json!({"version":1,"mode":if remote {"remote"} else {"local"}});
    crate::daemon::write_private_file(
        &marker(&config.data_dir),
        &serde_json::to_string(&metadata)?,
    )?;
    println!(
        "\n✓ {}",
        if remote {
            "Device connected"
        } else {
            "Local device ready"
        }
    );
    println!("Device:  {name}");
    println!("Runtime: {}", runtime_label(&config.data_dir));
    println!(
        "Service: running{}",
        if persistent {
            " · starts at boot"
        } else {
            " · may stop after logout"
        }
    );
    if remote {
        println!("\nIn Cypher desktop, select this device and configure Providers / MCP.");
    } else {
        println!("\nRun `cypher setup` when you want to connect this device to desktop.");
    }
    if !persistent {
        println!("Run `cypher setup` to finish persistent startup.");
    }
    Ok(false)
}

#[cfg(not(feature = "ui"))]
pub async fn default_entry(config: EngineConfig) -> anyhow::Result<()> {
    if marker(&config.data_dir).is_file() {
        return status(config).await;
    }
    if !std::io::stdin().is_terminal() {
        bail!("Cypher is not configured. Run `cypher setup` in a terminal.");
    }
    run(config, SetupOptions::default()).await
}

pub async fn status(config: EngineConfig) -> anyhow::Result<()> {
    let live = connect(&config).await?;
    if let Some(live) = live {
        println!("Device:   {}", device_label(&live).await);
        println!(
            "Mode:     {}",
            match live.scope {
                WorkspaceScope::Synced => "synced workspace",
                WorkspaceScope::Local => "local only",
                WorkspaceScope::Development => "development",
            }
        );
        println!("Engine:   running");
        let online = tokio::time::timeout(
            Duration::from_secs(2),
            live.client
                .call(methods::SYNC_STATUS, serde_json::json!({})),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|s| s["workspace"]["connected"] == true);
        if live.scope == WorkspaceScope::Synced {
            println!(
                "Desktop:  {}",
                if online { "connected" } else { "reconnecting" }
            );
        }
    } else {
        println!("Engine:   stopped");
        println!("Next:     cypher setup");
    }
    println!("Runtime:  {}", runtime_label(&config.data_dir));
    Ok(())
}

pub fn logs(config: EngineConfig, follow: bool) -> anyhow::Result<()> {
    let path = config.data_dir.join("logs/cypher-headless.log");
    if !path.is_file() {
        if cfg!(target_os = "linux") && crate::daemon::setup_unit_matches(&config)? {
            let mut command = std::process::Command::new("journalctl");
            command.args(["--user", "--no-pager", "-u", "cypher.service", "-n", "80"]);
            if follow {
                command.arg("-f");
            }
            if command.status()?.success() {
                return Ok(());
            }
            bail!("Could not read the service journal.");
        }
        bail!("No engine log yet. Run `cypher setup` first.");
    }
    let mut command = std::process::Command::new("tail");
    command.args(["-n", "80"]);
    if follow {
        command.arg("-F");
    }
    let status = command
        .arg("--")
        .arg(path)
        .status()
        .context("Could not open logs with tail.")?;
    if !status.success() {
        bail!("Could not read engine logs.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_labels_cannot_inject_terminal_controls() {
        assert_eq!(clean_label("gpu\nserver\x1b"), "gpuserver");
    }
    #[test]
    fn setup_options_require_explicit_local_or_saved_auth_for_unattended_use() {
        let data = tempfile::tempdir().unwrap();
        let config = EngineConfig {
            data_dir: data.path().into(),
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            ipc_socket: cypher_env::ipc_socket(data.path()).unwrap(),
            default_harness: cypher_engine::HarnessId::Mock,
            workos_client_id: Some("fixture".into()),
        };
        assert!(!complete_account(&config));
        assert!(!marker(data.path()).exists());
    }

    #[tokio::test]
    async fn running_or_awaiting_input_sessions_cannot_be_stopped_by_setup() {
        for (status, blocked) in [
            ("idle", false),
            ("errored", false),
            ("working", true),
            ("awaitingInput", true),
        ] {
            let (out, mut requests) = tokio::sync::mpsc::channel::<String>(4);
            let (responses, input) = tokio::sync::mpsc::channel::<String>(4);
            let client = RpcClient::new(out, input);
            let service = tokio::spawn(async move {
                while let Some(request) = requests.recv().await {
                    let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                    if request["method"] == methods::WATCH_SESSIONS {
                        responses
                            .send(
                                serde_json::json!({
                                    "id":request["id"],"item":[{
                                        "chatId":"fixture","deviceId":"fixture","status":status,
                                        "startedAt":null,"updatedAt":"2026-09-01T00:00:00Z"
                                    }]
                                })
                                .to_string(),
                            )
                            .await
                            .unwrap();
                    }
                }
            });
            let live = LiveEngine {
                client,
                scope: WorkspaceScope::Local,
                device: "fixture".into(),
            };
            assert_eq!(ensure_idle(&live).await.is_err(), blocked);
            service.abort();
        }
    }
}
