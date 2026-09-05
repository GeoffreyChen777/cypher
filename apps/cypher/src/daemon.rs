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

const LAUNCHD_LABEL: &str = "ai.mvp-lab.cypher";
/// Same unit name the curl|sh installer (`edge/src/install.sh`) writes, so
/// `cypher daemon …` manages that installation rather than a competing copy.
const SYSTEMD_UNIT: &str = "cypher.service";

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
    "CYPHER_IPC_PORT",
    "CYPHER_CALLBACK_PORT",
    "CYPHER_HARNESS",
    "CYPHER_DEVICE_NAME",
    "CYPHER_AUTO_UPDATE",
    "CYPHER_PI_RUNTIME_DIR",
    "CYPHER_PI_RUNTIME_BASE_URL",
    "RUST_LOG",
];

pub fn install(data_dir: &Path) -> anyhow::Result<()> {
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
            &render_launchd_plist(&exe, &env, &data_dir.join("daemon.log")),
        )?;
        run(
            "launchctl",
            &["bootstrap", &launchd_domain()?, &plist.to_string_lossy()],
        )?;
        println!(
            "Installed and started {LAUNCHD_LABEL} ({}).",
            plist.display()
        );
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
        run("systemctl", &["--user", "enable", SYSTEMD_UNIT])?;
        // enable --now only starts inactive services: a reinstall must pick up
        // a new executable/environment even if the old service is still up.
        run("systemctl", &["--user", "restart", SYSTEMD_UNIT])?;
        println!("Installed and started {SYSTEMD_UNIT} ({}).", unit.display());
        println!(
            "For start-at-boot without an active login session (VPS): loginctl enable-linger $USER"
        );
    } else {
        bail!("cypher daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
    println!(
        "Without a saved account the engine stays local-only; sign-in and restart are optional for sync."
    );
    println!(
        "Logs: {}",
        if cfg!(target_os = "macos") {
            format!("{}", data_dir.join("daemon.log").display())
        } else {
            format!("journalctl --user -u {SYSTEMD_UNIT}")
        }
    );
    Ok(())
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
        run("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT])?;
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
        run("systemctl", &["--user", "start", SYSTEMD_UNIT])?;
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
        run("systemctl", &["--user", "stop", SYSTEMD_UNIT])?;
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
        run("systemctl", &["--user", "restart", SYSTEMD_UNIT])?;
        println!("Restarted.");
        Ok(())
    } else {
        bail!("cypher daemon is only supported on macOS (launchd) and Linux (systemd)");
    }
}

pub fn status() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let output = Command::new("launchctl")
            .args(["print", &launchd_service_target()?])
            .output()
            .context("running launchctl")?;
        if !output.status.success() {
            println!(
                "{LAUNCHD_LABEL}: not loaded{}",
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
        println!("{LAUNCHD_LABEL}: loaded");
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
            .args(["--user", "--no-pager", "status", SYSTEMD_UNIT])
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

fn write_private_file(path: &Path, content: &str) -> anyhow::Result<()> {
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

fn render_launchd_plist(exe: &Path, env: &[(String, String)], log: &Path) -> String {
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
        label = LAUNCHD_LABEL,
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
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

fn systemd_unit_path() -> anyhow::Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or(home_dir()?.join(".config"));
    Ok(config.join("systemd/user").join(SYSTEMD_UNIT))
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
    Ok(format!("{}/{LAUNCHD_LABEL}", launchd_domain()?))
}

/// Run a command echoing it first; error (with stderr) on nonzero exit.
fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    println!("$ {program} {}", args.join(" "));
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Run without echoing; used where failure is an expected branch.
fn run_quiet(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!("{program} failed ({})", output.status);
    }
    Ok(())
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
    fn curl_installer_always_starts_the_local_capable_service() {
        let installer = include_str!("../../../edge/src/install.sh");
        assert!(!installer.contains("session.json"));
        assert!(installer.contains("\"$app_root/current/cypher\" daemon install"));
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
