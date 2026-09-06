//! `cypher daemon …` — install/manage `cypher headless` as a background service:
//! a systemd **user** unit on Linux (the VPS deployment target), a launchd
//! LaunchAgent on macOS. The unit runs the current executable with the
//! `CYPHER_*` environment captured at install time, so
//! `CYPHER_EDGE_URL=… cypher daemon install` bakes that override in.
//!
//! Auth is decoupled: without a saved session the service remains up on the
//! local-only profile. `cypher login` and a service restart opt into sync.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

#[cfg(test)]
const LAUNCHD_LABEL: &str = "ai.mvp-lab.cypher";
/// Same unit name the curl|sh installer (`edge/src/install.sh`) writes, so
/// `cypher daemon …` manages that installation rather than a competing copy.
#[cfg(test)]
const SYSTEMD_UNIT: &str = "cypher.service";

fn systemd_unit() -> anyhow::Result<String> {
    Ok(cypher_env::service_names(&cypher_env::data_dir())?.0)
}
fn launchd_label() -> anyhow::Result<String> {
    Ok(cypher_env::service_names(&cypher_env::data_dir())?.1)
}

/// Environment captured into the unit file. `PATH` is always included (the
/// engine spawns harness CLIs like `claude`, which service managers' minimal
/// default PATH won't find); the `CYPHER_*`/logging vars only when set.
const CAPTURED_ENV: &[&str] = &[
    "PATH",
    "CYPHER_DATA_DIR",
    "CYPHER_EDGE_URL",
    "CYPHER_EDGE_TOKEN",
    "CYPHER_ORG_ID",
    "CYPHER_WORKOS_CLIENT_ID",
    "CYPHER_WORKOS_API_BASE",
    "CYPHER_CALLBACK_PORT",
    "CYPHER_HARNESS",
    "CYPHER_DEVICE_NAME",
    "CYPHER_AUTO_UPDATE",
    "CYPHER_PI_RUNTIME_DIR",
    "CYPHER_PI_RUNTIME_BASE_URL",
    "RUST_LOG",
];

pub fn install(data_dir: &Path) -> anyhow::Result<()> {
    install_impl(data_dir, false)
}

pub(crate) fn install_for_setup(data_dir: &Path) -> anyhow::Result<()> {
    install_impl(data_dir, true)
}

fn install_impl(data_dir: &Path, quiet: bool) -> anyhow::Result<()> {
    let label = launchd_label()?;
    let exe = std::env::current_exe().context("resolving the cypher executable path")?;
    let mut env = captured_env();
    // Relative paths otherwise resolve under the service manager's working
    // directory rather than the shell that ran `daemon install`.
    let data_dir = std::path::absolute(data_dir)?;
    env.retain(|(key, _)| key != "CYPHER_DATA_DIR");
    env.push((
        "CYPHER_DATA_DIR".into(),
        data_dir.to_string_lossy().into_owned(),
    ));
    if cfg!(target_os = "macos") {
        let plist = launchd_plist_path()?;
        std::fs::create_dir_all(plist.parent().expect("LaunchAgents parent"))?;
        std::fs::create_dir_all(&data_dir)?;
        // Reinstall-friendly: unload any previous incarnation before rewriting.
        let _ = run_quiet("launchctl", &["bootout", &launchd_service_target()?]);
        write_private_file(
            &plist,
            &render_launchd_plist(&exe, &env, &data_dir.join("daemon.log"), &label),
        )?;
        run(
            "launchctl",
            &["bootstrap", &launchd_domain()?, &plist.to_string_lossy()],
        )?;
        if !quiet {
            println!("Installed and started {label} ({}).", plist.display());
        }
    } else if cfg!(target_os = "linux") {
        // systemd 245 rejects these characters in the executable path even
        // after correct C-style quoting. Fail before rewriting a working unit.
        if exe.to_str().is_none_or(|path| {
            path.chars()
                .any(|ch| ch.is_ascii_control() || matches!(ch, '"' | '\'' | '\\'))
        }) {
            bail!(
                "systemd requires an executable path without quotes, backslashes or control characters (UTF-8)"
            );
        }
        let unit = systemd_unit_path()?;
        std::fs::create_dir_all(unit.parent().expect("systemd user dir"))?;
        write_private_file(&unit, &render_systemd_unit(&exe, &env))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", &systemd_unit()?])?;
        // enable --now only starts inactive services: a reinstall must pick up
        // a new executable/environment even if the old service is still up.
        run("systemctl", &["--user", "restart", &systemd_unit()?])?;
        if !quiet {
            println!("Background service installed and started.");
        }
    } else {
        bail!("cypher daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
    Ok(())
}

/// Setup may manage only its own unit/data/port. Existing custom environment
/// remains untouched: a matching unit is started, not regenerated.
pub(crate) fn setup_unit_matches(config: &cypher_engine::EngineConfig) -> anyhow::Result<bool> {
    let data_dir = &config.data_dir;
    let path = systemd_unit_path()?;
    if let Ok(output) = bounded_command(
        "systemctl",
        &[
            "--user",
            "show",
            &systemd_unit()?,
            "--property=FragmentPath",
            "--value",
        ],
        true,
    ) && output.status.success()
    {
        let fragment = String::from_utf8_lossy(&output.stdout);
        if !fragment.trim().is_empty() && Path::new(fragment.trim()) != path {
            bail!(
                "A different Cypher service is already installed. It was left unchanged; use its existing installation to configure this device."
            );
        }
    }
    let (text, exists) = match std::fs::read_to_string(&path) {
        Ok(text) => (text, true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut env = captured_env();
            env.retain(|(key, _)| key != "CYPHER_DATA_DIR");
            env.push((
                "CYPHER_DATA_DIR".into(),
                std::path::absolute(data_dir)?
                    .to_string_lossy()
                    .into_owned(),
            ));
            (render_systemd_unit(&std::env::current_exe()?, &env), false)
        }
        Err(_) => bail!("Cannot inspect the existing Cypher service. Check its file permissions."),
    };
    if text
        .lines()
        .any(|line| line.starts_with("Environment=") && line.contains("CYPHER_IPC_PORT="))
    {
        bail!(
            "This service still sets CYPHER_IPC_PORT. Remove the obsolete TCP setting and explicitly reinstall the service; nothing was stopped."
        );
    }
    let data_dir = std::path::absolute(data_dir)?;
    let data_line = format!(
        "Environment={}",
        systemd_quote(&format!("CYPHER_DATA_DIR={}", data_dir.display()))
    );
    let exe_line = format!(
        "ExecStart=:{} headless",
        systemd_exec_path(&std::env::current_exe()?)
    );
    let command_path = home_dir()?.join(".local/bin/cypher");
    let alias_line = format!(
        "ExecStart=:{} headless",
        systemd_quote(&command_path.to_string_lossy())
    );
    let alias_matches = matches!((std::fs::canonicalize(&command_path), std::env::current_exe()),
        (Ok(alias),Ok(current)) if alias==current)
        && text.lines().any(|line| line == alias_line);
    let mut expected = vec![
        (
            "CYPHER_DATA_DIR",
            Some(data_dir.to_string_lossy().into_owned()),
        ),
        ("CYPHER_EDGE_URL", Some(config.edge_url.clone())),
        ("CYPHER_EDGE_TOKEN", config.edge_token.clone()),
        ("CYPHER_ORG_ID", config.org_id.clone()),
        (
            "CYPHER_WORKOS_CLIENT_ID",
            Some(config.workos_client_id.clone().unwrap_or_default()),
        ),
    ];
    for key in [
        "CYPHER_WORKOS_API_BASE",
        "CYPHER_CALLBACK_PORT",
        "CYPHER_PI_RUNTIME_DIR",
        "CYPHER_PI_RUNTIME_BASE_URL",
    ] {
        expected.push((key, std::env::var(key).ok()));
    }
    for (key, value) in &expected {
        let prefix = format!("Environment=\"{key}=");
        if let Some(line) = text.lines().find(|line| line.starts_with(&prefix)) {
            let correct = value
                .as_ref()
                .map(|value| format!("Environment={}", systemd_quote(&format!("{key}={value}"))));
            if correct.as_deref() != Some(line) {
                bail!(
                    "Setup environment differs from the saved Cypher service. Use the original CYPHER_* settings; the existing service was not changed."
                );
            }
        } else if *key != "CYPHER_DATA_DIR" && std::env::var_os(key).is_some() {
            bail!(
                "Setup would change the saved service environment. Use its original CYPHER_* settings, or explicitly reconfigure with `cypher daemon install`."
            );
        }
    }
    if text
        .lines()
        .filter(|line| line.starts_with("EnvironmentFile="))
        .any(|line| line != "EnvironmentFile=-%h/.cypher/env")
    {
        bail!(
            "This service has custom EnvironmentFile settings. Keep it unchanged and use its existing configuration."
        );
    }
    let env_file = home_dir()?.join(".cypher/env");
    if env_file.exists() {
        let content = std::fs::read_to_string(env_file)
            .context("Could not inspect the service environment file.")?;
        for line in content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with(['#', ';']))
        {
            let Some((key, raw)) = line.split_once('=') else {
                bail!("Custom service environment syntax cannot be safely interpreted by setup.");
            };
            if key.trim() == "CYPHER_IPC_PORT" {
                bail!(
                    "Remove obsolete CYPHER_IPC_PORT from the service EnvironmentFile before setup; nothing was stopped."
                );
            }
            if let Some((_, value)) = expected
                .iter()
                .find(|(expected, _)| *expected == key.trim())
            {
                let raw = raw.trim();
                let decoded = if raw.starts_with('"') {
                    serde_json::from_str::<String>(raw).ok()
                } else if raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2 {
                    Some(raw[1..raw.len() - 1].to_string())
                } else {
                    Some(raw.to_string())
                };
                if decoded.as_ref() != value.as_ref() {
                    bail!(
                        "The service EnvironmentFile overrides setup's CYPHER_* settings. Set matching shell variables before running setup; nothing was changed."
                    );
                }
            }
        }
    }
    if !text.lines().any(|line| line == data_line)
        || !(text.lines().any(|line| line == exe_line) || alias_matches)
    {
        bail!(
            "The existing Cypher service uses a different installation or configuration. Keep it unchanged and run setup with its original executable and CYPHER_DATA_DIR."
        );
    }
    Ok(exists)
}

pub(crate) fn user_bus_available() -> bool {
    run_quiet("systemctl", &["--user", "show-environment"]).is_ok()
}

pub(crate) fn setup_service_pid() -> Option<u32> {
    let output = bounded_command(
        "systemctl",
        &[
            "--user",
            "show",
            &systemd_unit().ok()?,
            "--property=MainPID",
            "--value",
        ],
        true,
    )
    .ok()?;
    output.status.success().then_some(())?;
    String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0)
}

pub(crate) fn setup_stop() -> anyhow::Result<()> {
    run_quiet("systemctl", &["--user", "stop", &systemd_unit()?])
}

pub(crate) fn setup_start() -> anyhow::Result<()> {
    run_quiet("systemctl", &["--user", "start", &systemd_unit()?])
}

pub(crate) fn enable_setup_service() -> anyhow::Result<()> {
    run_quiet("systemctl", &["--user", "enable", &systemd_unit()?])
}

pub(crate) fn linger_enabled() -> bool {
    let uid = unsafe { libc::getuid() }.to_string();
    bounded_command(
        "loginctl",
        &["show-user", &uid, "--property=Linger", "--value"],
        true,
    )
    .is_ok_and(|out| out.status.success() && out.stdout.trim_ascii() == b"yes")
}

pub(crate) fn enable_linger(sudo: bool) -> bool {
    let uid = unsafe { libc::getuid() }.to_string();
    if !sudo {
        return run_quiet("loginctl", &["--no-ask-password", "enable-linger", &uid]).is_ok()
            && linger_enabled();
    }
    let mut command = if sudo {
        let mut command = Command::new("sudo");
        command.args(["loginctl", "enable-linger", &uid]);
        command
    } else {
        let mut command = Command::new("loginctl");
        command.args(["--no-ask-password", "enable-linger", &uid]);
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
    };
    command.status().is_ok_and(|status| status.success()) && linger_enabled()
}

pub fn uninstall() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let _ = run_quiet("launchctl", &["bootout", &launchd_service_target()?]);
        let plist = launchd_plist_path()?;
        match std::fs::remove_file(&plist) {
            Ok(()) => println!("Removed {}.", plist.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                println!("Not installed.")
            }
            Err(err) => return Err(err.into()),
        }
    } else if cfg!(target_os = "linux") {
        let unit = systemd_unit_path()?;
        if !unit.try_exists()? {
            println!("Not installed.");
            return Ok(());
        }
        // Do not claim removal while a service we failed to stop keeps running.
        run(
            "systemctl",
            &["--user", "disable", "--now", &systemd_unit()?],
        )?;
        match std::fs::remove_file(&unit) {
            Ok(()) => {
                run("systemctl", &["--user", "daemon-reload"])?;
                println!("Removed {}.", unit.display());
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                println!("Not installed.")
            }
            Err(err) => return Err(err.into()),
        }
    } else {
        bail!("cypher daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
    Ok(())
}

pub fn start() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let plist = launchd_plist_path()?;
        if !plist.exists() {
            bail!("not installed — run `cypher daemon install` first");
        }
        // `stop` boots the job out of the domain, so start = bootstrap; already
        // loaded is fine, then kickstart guarantees a running process either way.
        let _ = run_quiet(
            "launchctl",
            &["bootstrap", &launchd_domain()?, &plist.to_string_lossy()],
        );
        run("launchctl", &["kickstart", &launchd_service_target()?])?;
    } else if cfg!(target_os = "linux") {
        run("systemctl", &["--user", "start", &systemd_unit()?])?;
    } else {
        bail!("cypher daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
    println!("Started.");
    Ok(())
}

pub fn stop() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        // bootout (not `kill`): with KeepAlive the job would otherwise respawn.
        run("launchctl", &["bootout", &launchd_service_target()?])?;
    } else if cfg!(target_os = "linux") {
        run("systemctl", &["--user", "stop", &systemd_unit()?])?;
    } else {
        bail!("cypher daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
    println!("Stopped.");
    Ok(())
}

pub fn restart() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        if run_quiet(
            "launchctl",
            &["kickstart", "-k", &launchd_service_target()?],
        )
        .is_err()
        {
            // Not loaded (e.g. after `stop`) — fall through to a plain start.
            return start();
        }
        println!("Restarted.");
        Ok(())
    } else if cfg!(target_os = "linux") {
        run("systemctl", &["--user", "restart", &systemd_unit()?])?;
        println!("Restarted.");
        Ok(())
    } else {
        bail!("cypher daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
}

pub fn status() -> anyhow::Result<()> {
    let label = launchd_label()?;
    if cfg!(target_os = "macos") {
        let output = Command::new("launchctl")
            .args(["print", &launchd_service_target()?])
            .output()
            .context("running launchctl")?;
        if !output.status.success() {
            println!(
                "{label}: not loaded{}",
                if launchd_plist_path()?.exists() {
                    " (installed — `cypher daemon start`)"
                } else {
                    " (not installed — `cypher daemon install`)"
                }
            );
            return Ok(());
        }
        // `launchctl print` is pages long; surface just the liveness lines.
        let text = String::from_utf8_lossy(&output.stdout);
        println!("{label}: loaded");
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("state = ")
                || trimmed.starts_with("pid = ")
                || trimmed.starts_with("last exit code = ")
            {
                println!("  {trimmed}");
            }
        }
        Ok(())
    } else if cfg!(target_os = "linux") {
        // Passthrough; `status` exits nonzero for inactive units, which is not an
        // error for us to report — the output already says it.
        let status = Command::new("systemctl")
            .args(["--user", "--no-pager", "status", &systemd_unit()?])
            .status()
            .context("running systemctl")?;
        if !status.success() && !matches!(status.code(), Some(3 | 4)) {
            bail!("could not query the systemd user service ({status})");
        }
        Ok(())
    } else {
        bail!("cypher daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
}

// ---------------------------------------------------------------------------
// Unit rendering (pure — unit-tested below)
// ---------------------------------------------------------------------------

fn captured_env() -> Vec<(String, String)> {
    CAPTURED_ENV
        .iter()
        .filter_map(|key| std::env::var(key).ok().map(|v| (key.to_string(), v)))
        .collect()
}

fn render_systemd_unit(exe: &Path, env: &[(String, String)]) -> String {
    let mut unit = String::from(
        "[Unit]\nDescription=Cypher headless engine\nAfter=network-online.target\nStartLimitIntervalSec=60\nStartLimitBurst=5\n\n[Service]\n",
    );
    for (key, value) in env {
        // systemd unquotes the value; escape the characters it treats specially.
        unit.push_str(&format!(
            "Environment={}\n",
            systemd_quote(&format!("{key}={value}"))
        ));
    }
    unit.push_str(&format!(
        "ExecStart=:{} headless\nRestart=on-failure\nRestartSec=5\nUMask=0077\nEnvironmentFile=-%h/.cypher/env\n\n[Install]\nWantedBy=default.target\n",
        systemd_exec_path(exe)
    ));
    unit
}

/// The ExecStart binary path. An exe under `~/.cypher/app/` came from the
/// curl|sh installer, whose upgrades relink `app/current` — point the unit at
/// the symlink (as the installer's own unit does) so it never pins one version.
/// (`current_exe` resolves symlinks, so the versioned dir is what we see here.)
fn systemd_exec_path(exe: &Path) -> String {
    exec_path_for(exe, std::env::var_os("HOME").map(PathBuf::from).as_deref())
}

fn exec_path_for(exe: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return systemd_quote(&exe.to_string_lossy());
    };
    let cypher_root = home.join(".cypher/app");
    if exe.starts_with(&cypher_root) {
        // Point the unit at the installer's `current` symlink so upgrades
        // relink without touching the unit.
        "\"%h/.cypher/app/current/cypher\"".to_string()
    } else {
        systemd_quote(&exe.to_string_lossy())
    }
}

/// systemd.syntax C-style quoting, plus specifier expansion. ExecStart uses
/// the ':' prefix to disable argv environment expansion. Doubling '$' is NOT
/// correct for its executable path: systemd 245 expands argv, not command->path.
/// Environment= does not expand '$'; literal '%' must be doubled in both.
fn systemd_quote(value: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '%' => quoted.push_str("%%"),
            ch if ch.is_ascii_control() => quoted.push_str(&format!("\\x{:02x}", ch as u32)),
            ch => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

pub(crate) fn write_private_file(path: &Path, content: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let result = (|| {
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    let _ = std::fs::remove_file(temp);
    result.context("writing private service configuration")
}

fn render_launchd_plist(exe: &Path, env: &[(String, String)], log: &Path, label: &str) -> String {
    let mut env_dict = String::new();
    for (key, value) in env {
        env_dict.push_str(&format!(
            "      <key>{}</key><string>{}</string>\n",
            xml_escape(key),
            xml_escape(value)
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key><string>{label}</string>
    <key>ProgramArguments</key>
    <array>
      <string>{exe}</string>
      <string>headless</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
{env_dict}    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key>
    <dict>
      <key>SuccessfulExit</key><false/>
    </dict>
    <key>ThrottleInterval</key><integer>30</integer>
    <key>StandardOutPath</key><string>{log}</string>
    <key>StandardErrorPath</key><string>{log}</string>
  </dict>
</plist>
"#,
        label = xml_escape(label),
        exe = xml_escape(&exe.to_string_lossy()),
        env_dict = env_dict,
        log = xml_escape(&log.to_string_lossy()),
    )
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// Paths + process helpers
// ---------------------------------------------------------------------------

fn home_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME not set")
}

fn launchd_plist_path() -> anyhow::Result<PathBuf> {
    Ok(home_dir()?
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", launchd_label()?)))
}

fn systemd_unit_path() -> anyhow::Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or(home_dir()?.join(".config"));
    Ok(config.join("systemd/user").join(&systemd_unit()?))
}

fn launchd_domain() -> anyhow::Result<String> {
    let output = Command::new("id").arg("-u").output().context("id -u")?;
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if uid.is_empty() {
        bail!("could not determine the current uid");
    }
    Ok(format!("gui/{uid}"))
}

fn launchd_service_target() -> anyhow::Result<String> {
    Ok(format!("{}/{}", launchd_domain()?, launchd_label()?))
}

/// Run a service command without noisy command echoes or captured environment
/// text. Explicit status/log commands remain the diagnostics surface.
fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    run_quiet(program, args)
}

/// Run without echoing; used where failure is an expected branch.
fn run_quiet(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = bounded_command(program, args, false)?;
    if !output.status.success() {
        bail!("{program} failed ({})", output.status);
    }
    Ok(())
}

fn bounded_command(
    program: &str,
    args: &[&str],
    capture: bool,
) -> anyhow::Result<std::process::Output> {
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("Could not run {program}."))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .context("Could not read service-manager result.");
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{program} timed out or failed. Run `cypher logs` to investigate.");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_unit_shape() {
        let unit = render_systemd_unit(
            Path::new("/usr/local/bin/cypher"),
            &[
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("CYPHER_EDGE_URL".into(), "https://edge.example".into()),
                ("RUST_LOG".into(), "info,cypher=\"debug\"".into()),
            ],
        );
        assert!(unit.contains("ExecStart=:\"/usr/local/bin/cypher\" headless\n"));
        assert!(unit.contains("Environment=\"PATH=/usr/bin:/bin\"\n"));
        assert!(unit.contains("Environment=\"CYPHER_EDGE_URL=https://edge.example\"\n"));
        // Inner quotes escaped so systemd re-parses the value verbatim.
        assert!(unit.contains("Environment=\"RUST_LOG=info,cypher=\\\"debug\\\"\"\n"));
        assert!(unit.contains("StartLimitIntervalSec=60\n"));
        assert!(unit.contains("StartLimitBurst=5\n"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(!unit.contains("session.json"));
        assert!(!unit.contains("ConditionPathExists"));
        assert!(unit.contains("EnvironmentFile=-%h/.cypher/env"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn curl_installer_delegates_setup_instead_of_starting_before_login() {
        let installer = include_str!("../../../edge/src/install.sh");
        assert!(!installer.contains("session.json"));
        assert!(installer.contains("exec \"$app_root/current/cypher\" setup </dev/tty"));
        assert!(!installer.contains("\"$app_root/current/cypher\" daemon install"));
        assert!(!installer.contains("claude.ai"));
    }

    #[test]
    fn installed_exe_uses_the_current_symlink() {
        // Installer-managed binary (current_exe resolves the `current` symlink to
        // the versioned dir): the unit must point back at the symlink.
        assert_eq!(
            exec_path_for(
                Path::new("/home/u/.cypher/app/0.3.0/cypher"),
                Some(Path::new("/home/u")),
            ),
            "\"%h/.cypher/app/current/cypher\""
        );
        // Source build: literal path.
        assert_eq!(
            exec_path_for(
                Path::new("/src/target/debug/cypher"),
                Some(Path::new("/home/u"))
            ),
            "\"/src/target/debug/cypher\""
        );
    }

    #[test]
    fn launchd_plist_shape() {
        let plist = render_launchd_plist(
            Path::new("/Users/x/cypher & co/cypher"),
            &[("CYPHER_EDGE_URL".into(), "https://e?a=1&b=2".into())],
            Path::new("/Users/x/.cypher/daemon.log"),
            LAUNCHD_LABEL,
        );
        assert!(plist.contains("<key>Label</key><string>ai.mvp-lab.cypher</string>"));
        // XML-escaped exe path and env value.
        assert!(plist.contains("<string>/Users/x/cypher &amp; co/cypher</string>"));
        assert!(plist.contains("<string>https://e?a=1&amp;b=2</string>"));
        assert!(plist.contains("<string>headless</string>"));
        assert!(plist.contains("<key>SuccessfulExit</key><false/>"));
        assert!(
            plist
                .contains("<key>StandardOutPath</key><string>/Users/x/.cypher/daemon.log</string>")
        );
    }

    #[test]
    fn service_identifiers_are_stable() {
        assert_eq!(LAUNCHD_LABEL, "ai.mvp-lab.cypher");
        assert_eq!(SYSTEMD_UNIT, "cypher.service");
        assert_eq!(
            launchd_plist_path().unwrap().file_name().unwrap(),
            "ai.mvp-lab.cypher.plist"
        );
    }

    #[test]
    fn systemd_escapes_paths_specifiers_and_newlines() {
        assert_eq!(
            systemd_quote("/home/a b/100%/$HOME/cypher"),
            "\"/home/a b/100%%/$HOME/cypher\""
        );
        assert_eq!(
            systemd_quote("X=a\nExecStart=evil\t%h$HOME"),
            "\"X=a\\nExecStart=evil\\t%%h$HOME\""
        );
    }

    #[cfg(unix)]
    #[test]
    fn captured_credentials_are_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cypher.service");
        std::fs::write(&path, "old").unwrap();
        write_private_file(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
