//! Managed (symlink) installs — the daemon/VPS path.

use super::*;

/// Download + unpack the headless tarball into `app_root/<ver>` (idempotent —
/// an already-staged version is reused). Returns the versioned dir.
pub async fn stage_headless(
    edge_url: &str,
    manifest: &Manifest,
    app_root: &Path,
) -> anyhow::Result<PathBuf> {
    let version = &manifest.version;
    validate_version(version)?;
    let dest = app_root.join(version);
    if headless_binary_ready(&dest) {
        return Ok(dest);
    }
    if dest.exists() || dest.is_symlink() {
        bail!(
            "{} is an incomplete install; move it aside before retrying",
            dest.display()
        );
    }
    let file = manifest.headless_file();
    std::fs::create_dir_all(app_root)?;
    let staging = tempfile::Builder::new()
        .prefix(".stage-")
        .tempdir_in(app_root)?;
    let stage = staging.path().to_path_buf();
    let tarball = stage.join(&file);
    download_release_file(edge_url, manifest, &file, &tarball).await?;
    // Archive validation and extraction shell out to `tar`: blocking pool.
    // `staging` outlives the await, so the temp dir is still there.
    let dest = off_runtime(move || {
        validate_headless_archive(&tarball, file.trim_end_matches(".tar.gz"))?;
        let unpacked = stage.join("unpacked");
        std::fs::create_dir_all(&unpacked)?;
        // Tarball root is the versioned stage dir (see scripts/package-linux.sh);
        // strip it exactly as install.sh does.
        run(
            "tar",
            &[
                "-xzf",
                &tarball.to_string_lossy(),
                "-C",
                &unpacked.to_string_lossy(),
                "--strip-components=1",
                "--no-same-owner",
            ],
        )?;
        if !headless_binary_ready(&unpacked) {
            bail!("tarball {file} did not contain a runnable cypher binary");
        }
        match std::fs::rename(&unpacked, &dest) {
            Ok(()) => {}
            // Lost a race with another stager — the staged copy is equivalent.
            Err(err) => {
                if headless_binary_ready(&dest) {
                    return Ok(dest);
                }
                return Err(err).with_context(|| format!("moving {} into place", dest.display()));
            }
        }
        Ok(dest)
    })
    .await?;
    // A newer release supersedes any older one still waiting to be applied.
    let (app_root, version) = (app_root.to_path_buf(), version.clone());
    off_runtime(move || {
        prune_managed_versions(&app_root, |staged| version_newer(&version, staged));
        Ok(())
    })
    .await?;
    Ok(dest)
}

pub(super) fn headless_binary_ready(dir: &Path) -> bool {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let binary = dir.join("cypher");
    if dir.is_symlink() || binary.is_symlink() || !binary.is_file() {
        return false;
    }
    let Ok(mut child) = Command::new(binary)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

pub(super) fn validate_headless_archive(tarball: &Path, root: &str) -> anyhow::Result<()> {
    fn listing(tarball: &Path, flag: &str) -> anyhow::Result<String> {
        use std::io::Read;
        use std::process::{Command, Stdio};
        let mut child = Command::new("tar")
            .arg(flag)
            .arg(tarball)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut bytes = Vec::new();
        let read = child
            .stdout
            .take()
            .unwrap()
            .take(65537)
            .read_to_end(&mut bytes);
        if read.is_err() || bytes.len() > 65536 {
            let _ = child.kill();
            let _ = child.wait();
            bail!("invalid or oversized archive listing");
        }
        if !child.wait()?.success() {
            bail!("invalid release archive");
        }
        Ok(String::from_utf8(bytes)?)
    }
    let allowed = ["", "cypher", "install.sh", "cypher.desktop", "cypher.png"]
        .map(|name| format!("{root}/{name}"));
    for member in listing(tarball, "-tzf")?.lines() {
        if !allowed.iter().any(|name| name == member) {
            bail!("unexpected release archive member");
        }
    }
    for member in listing(tarball, "-tvzf")?.lines() {
        if !member.starts_with('-') && !member.starts_with('d') {
            bail!("release archive links and special files are not allowed");
        }
    }
    Ok(())
}

/// Atomically repoint `app_root/current` at `app_root/<ver>` (symlink to a temp
/// name, then rename over — never a window with no `current`).
pub fn apply_headless(app_root: &Path, version: &str) -> anyhow::Result<()> {
    validate_version(version)?;
    #[cfg(unix)]
    {
        let app_root = std::path::absolute(app_root)?;
        let target = app_root.join(version);
        if !headless_binary_ready(&target) {
            bail!("{} is not a staged install", target.display());
        }
        let staging = tempfile::Builder::new()
            .prefix(".current-")
            .tempdir_in(&app_root)?;
        let tmp = staging.path().join("link");
        std::os::unix::fs::symlink(&target, &tmp).context("creating current symlink")?;
        std::fs::rename(&tmp, app_root.join("current")).context("swapping current symlink")?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (app_root, version);
        bail!("managed installs are unix-only");
    }
}

/// How long an installer's temporary directory (`.stage-*` from
/// [`stage_headless`], `.install-*` from install.sh, `.current-*` from
/// [`apply_headless`]) may sit before it counts as abandoned by a crash.
const ABANDONED_TEMP_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Delete the version directories of a managed install that `stale` selects,
/// plus abandoned installer temp dirs. Never touched, whatever `stale` says:
/// `current`'s target, the running binary's version, versions any live
/// process runs from, and versions a service unit still names directly (units
/// written before the switch to `app/current` pin one).
pub(super) fn prune_managed_versions(app_root: &Path, stale: impl Fn(&str) -> bool) {
    let mut keep = service_pinned_versions(app_root, &service_unit_dirs());
    keep.extend(running_versions(app_root));
    if let Some(version) = std::fs::read_link(app_root.join("current"))
        .ok()
        .and_then(|target| Some(target.file_name()?.to_str()?.to_owned()))
    {
        keep.insert(version);
    }
    prune_versions_except(app_root, &keep, stale);
}

pub(super) fn prune_versions_except(
    app_root: &Path,
    keep: &std::collections::BTreeSet<String>,
    stale: impl Fn(&str) -> bool,
) {
    let Ok(entries) = std::fs::read_dir(app_root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // `file_type` does not follow links: `current` and stray links stay.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if [".stage-", ".install-", ".current-"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            let abandoned = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > ABANDONED_TEMP_AGE);
            if abandoned {
                remove_install_dir(&path, "abandoned install temp dir");
            }
            continue;
        }
        if validate_version(&name).is_ok() && !keep.contains(&name) && stale(&name) {
            remove_install_dir(&path, "old app version");
        }
    }
}

fn remove_install_dir(path: &Path, what: &str) {
    match std::fs::remove_dir_all(path) {
        Ok(()) => tracing::info!(path = %path.display(), "removed {what}"),
        Err(err) => tracing::warn!(path = %path.display(), error = %err, "could not remove {what}"),
    }
}

/// Where `cypher daemon install` writes service definitions.
fn service_unit_dirs() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    vec![
        home.join(".config/systemd/user"),
        home.join("Library/LaunchAgents"),
    ]
}

/// Versions a service unit runs by path (`…/app/<ver>/cypher`) instead of
/// through `current`. Deleting one would leave that service unable to start.
pub(super) fn service_pinned_versions(
    app_root: &Path,
    unit_dirs: &[PathBuf],
) -> std::collections::BTreeSet<String> {
    let mut pinned = std::collections::BTreeSet::new();
    let Some(root_name) = app_root.file_name().and_then(|name| name.to_str()) else {
        return pinned;
    };
    let Ok(entries) = std::fs::read_dir(app_root) else {
        return pinned;
    };
    let versions: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| validate_version(name).is_ok())
        .collect();
    let units: Vec<String> = unit_dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .collect();
    for version in versions {
        let needle = format!("/{root_name}/{version}/cypher");
        if units.iter().any(|unit| unit.contains(&needle)) {
            pinned.insert(version);
        }
    }
    pinned
}

/// Versions this process and any other live process execute from. Linux
/// exposes every process's binary as `/proc/<pid>/exe`, which covers a desktop
/// app still running an old version after the service restarted onto a new
/// one. Elsewhere only this process is known.
pub(super) fn running_versions(app_root: &Path) -> std::collections::BTreeSet<String> {
    let roots: Vec<PathBuf> = [
        Some(app_root.to_path_buf()),
        std::fs::canonicalize(app_root).ok(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let version_of = |exe: &Path| {
        roots.iter().find_map(|root| {
            let relative = exe.strip_prefix(root).ok()?;
            let first = relative.components().next()?;
            Some(first.as_os_str().to_str()?.to_owned())
        })
    };
    let mut running = std::collections::BTreeSet::new();
    if let Some(version) = std::env::current_exe()
        .ok()
        .and_then(|exe| version_of(&exe))
    {
        running.insert(version);
    }
    #[cfg(target_os = "linux")]
    if let Ok(processes) = std::fs::read_dir("/proc") {
        for process in processes.flatten() {
            let Ok(exe) = std::fs::read_link(process.path().join("exe")) else {
                continue;
            };
            // A replaced binary reads `<path> (deleted)`; it still runs.
            let exe = exe.to_string_lossy();
            let exe = Path::new(exe.strip_suffix(" (deleted)").unwrap_or(&exe));
            if let Some(version) = version_of(exe) {
                running.insert(version);
            }
        }
    }
    running
}

/// The managed layout every Linux install converges on: `~/.cypher/app`.
pub fn managed_app_root(home: &Path) -> PathBuf {
    home.join(".cypher").join("app")
}

/// Point `~/.local/bin/cypher` at `<app_root>/current/cypher`, replacing an
/// existing file or link atomically (the installer's own layout). A directory
/// at that path is left alone and reported.
pub fn link_command(home: &Path, app_root: &Path) -> anyhow::Result<PathBuf> {
    #[cfg(unix)]
    {
        let bin = home.join(".local").join("bin");
        std::fs::create_dir_all(&bin)?;
        let command = bin.join("cypher");
        if command.is_dir() && !command.is_symlink() {
            bail!("{} is a directory; move it aside", command.display());
        }
        let target = app_root.join("current").join("cypher");
        if std::fs::read_link(&command).is_ok_and(|existing| existing == target) {
            return Ok(command);
        }
        let staging = tempfile::Builder::new()
            .prefix(".cypher-")
            .tempdir_in(&bin)?;
        let tmp = staging.path().join("cypher");
        std::os::unix::fs::symlink(&target, &tmp).context("creating the command link")?;
        std::fs::rename(&tmp, &command).context("replacing the command link")?;
        Ok(command)
    }
    #[cfg(not(unix))]
    {
        let _ = (home, app_root);
        bail!("managed installs are unix-only");
    }
}

/// Bring an unmanaged Linux binary (hand-copied, or installed by an older
/// layout) into the managed layout: stage the release under `~/.cypher/app`,
/// switch `current`, link the command, and repoint a service unit that ran
/// the old executable. Returns the managed app root.
pub async fn adopt_managed_install(
    edge_url: &str,
    manifest: &Manifest,
    home: &Path,
    data_dir: &Path,
    previous_exe: &Path,
) -> anyhow::Result<PathBuf> {
    let app_root = managed_app_root(home);
    stage_headless(edge_url, manifest, &app_root).await?;
    apply_headless(&app_root, &manifest.version)?;
    link_command(home, &app_root)?;
    let previous = previous_exe.to_string_lossy().into_owned();
    let alias = home
        .join(".local/bin/cypher")
        .to_string_lossy()
        .into_owned();
    rewrite_linux_service_exec(data_dir, |line| {
        exec_line_binary(line).is_some_and(|binary| binary == previous || binary == alias)
    })?;
    Ok(app_root)
}

/// The executable an `ExecStart=:"<path>" headless` line runs, unquoted.
pub(super) fn exec_line_binary(line: &str) -> Option<String> {
    let rest = line.strip_prefix("ExecStart=:\"")?;
    let rest = rest.strip_suffix("\" headless")?;
    Some(
        rest.replace("\\\"", "\"")
            .replace("\\\\", "\\")
            .replace("%%", "%"),
    )
}

/// Restart the installed engine service (the same units `cypher daemon` and the
/// curl|sh installer manage). Called after a symlink swap so the running daemon
/// picks up the new binary.
pub fn restart_service(data_dir: &Path) -> anyhow::Result<()> {
    let (unit, label) = cypher_env::service_names(data_dir)?;
    if cfg!(target_os = "macos") {
        let output = std::process::Command::new("id").arg("-u").output()?;
        let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
        run(
            "launchctl",
            &["kickstart", "-k", &format!("gui/{uid}/{label}")],
        )
        .map_err(|_| anyhow::anyhow!("no cypher service is loaded to restart"))
    } else {
        run("systemctl", &["--user", "restart", &unit])
    }
}

pub fn migrate_linux_service_to_current(data_dir: &Path) -> anyhow::Result<bool> {
    rewrite_linux_service_exec(data_dir, |line| {
        line.starts_with("ExecStart=:\"%h/.cypher/app/")
            && line.ends_with("/cypher\" headless")
            && line != CURRENT_EXEC_LINE
    })
}

pub(super) const CURRENT_EXEC_LINE: &str = "ExecStart=:\"%h/.cypher/app/current/cypher\" headless";

/// Rewrite the default unit's `ExecStart` to the managed `current` link when
/// `matches` accepts the existing line. Only the default data directory's
/// unit at its expected path is touched; anything else is left as-is.
fn rewrite_linux_service_exec(
    data_dir: &Path,
    matches: impl Fn(&str) -> bool,
) -> anyhow::Result<bool> {
    if !cfg!(target_os = "linux") {
        return Ok(false);
    }
    let unit = cypher_env::service_names(data_dir)?.0;
    let output = std::process::Command::new("systemctl")
        .args([
            "--user",
            "show",
            &unit,
            "--property=FragmentPath",
            "--value",
        ])
        .output()?;
    if !output.status.success() {
        return Ok(false);
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is unset")?;
    let expected = home.join(".config/systemd/user/cypher.service");
    if Path::new(&path) != expected || !expected.is_file() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(&expected)?;
    let data = std::path::absolute(data_dir)?;
    let data_line = format!(
        "Environment={}",
        systemd_quote(&format!("CYPHER_DATA_DIR={}", data.display()))
    );
    if !text.lines().any(|line| line == data_line) {
        return Ok(false);
    }
    let Some(rewritten) = rewrite_exec_start(&text, matches) else {
        return Ok(false);
    };
    let tmp = expected.with_extension("service.cypher-update");
    std::fs::write(&tmp, rewritten)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, &expected)?;
    run("systemctl", &["--user", "daemon-reload"])?;
    Ok(true)
}

/// systemd C-style quoting as `cypher daemon install` writes it: the unit's
/// own `Environment=` line for a data directory containing `%`, `"` or `\\`
/// must still be recognised.
pub(super) fn systemd_quote(value: &str) -> String {
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

/// Pure half of [`rewrite_linux_service_exec`]: the unit text with the first
/// matching `ExecStart` line replaced, or `None` when nothing matched.
pub(super) fn rewrite_exec_start(text: &str, matches: impl Fn(&str) -> bool) -> Option<String> {
    let old = text
        .lines()
        .find(|line| line.starts_with("ExecStart=") && matches(line))?;
    Some(text.replacen(old, CURRENT_EXEC_LINE, 1))
}

pub(super) fn restart_service_from_engine(data_dir: &Path) -> anyhow::Result<()> {
    if cfg!(target_os = "linux") {
        // A synchronous restart waits for THIS service to exit, while runtime
        // teardown waits for this worker to return: systemd eventually SIGKILLs
        // it at TimeoutStopSec. Queue the restart instead of waiting on ourselves.
        run(
            "systemctl",
            &[
                "--user",
                "--no-block",
                "restart",
                &cypher_env::service_names(data_dir)?.0,
            ],
        )
    } else {
        restart_service(data_dir)
    }
}
