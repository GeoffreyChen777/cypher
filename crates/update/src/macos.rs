//! macOS app-bundle installs — the desktop path.

use super::*;

/// Download + unpack the app tarball into `{data_dir}/updates/<ver>/Cypher.app`
/// (idempotent). Returns the staged bundle path.
pub async fn stage_mac_app(
    edge_url: &str,
    manifest: &Manifest,
    data_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let version = &manifest.version;
    validate_version(version)?;
    let dir = data_dir.join("updates").join(version);
    let staged = dir.join("Cypher.app");
    if staged.join("Contents/MacOS/cypher").exists() {
        return Ok(staged);
    }
    {
        let dir = dir.clone();
        off_runtime(move || {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))
        })
        .await?;
    }
    let file = manifest.mac_app_file();
    let tarball = dir.join(&file);
    download_release_file(edge_url, manifest, &file, &tarball).await?;
    // Extracting the whole app bundle takes seconds: blocking pool, not a worker.
    let staged = off_runtime(move || {
        run(
            "tar",
            &[
                "-xzf",
                &tarball.to_string_lossy(),
                "-C",
                &dir.to_string_lossy(),
            ],
        )?;
        std::fs::remove_file(&tarball).ok();
        let binary = staged.join("Contents/MacOS/cypher");
        if !binary.exists() {
            bail!("app tarball {file} did not contain Cypher.app");
        }
        Ok(staged)
    })
    .await?;
    // A newer bundle supersedes any older one still waiting to be applied.
    let (data_dir, version) = (data_dir.to_path_buf(), version.clone());
    off_runtime(move || {
        prune_staged_updates(&data_dir, |staged| version_newer(&version, staged));
        Ok(())
    })
    .await?;
    Ok(staged)
}

/// Delete the staged app bundles under `{data_dir}/updates` that `stale`
/// selects by version. Entries not named like a release are left alone.
pub(super) fn prune_staged_updates(data_dir: &Path, stale: impl Fn(&str) -> bool) {
    let Ok(entries) = std::fs::read_dir(data_dir.join("updates")) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(version) = name.to_str() else {
            continue;
        };
        if validate_version(version).is_err() || !stale(version) {
            continue;
        }
        let path = entry.path();
        let removed = if path.is_dir() && !path.is_symlink() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match removed {
            Ok(()) => tracing::info!(%version, "removed staged app update"),
            Err(err) => {
                tracing::warn!(%version, error = %err, "could not remove staged app update")
            }
        }
    }
}

/// Swap the installed bundle for the staged one: `ditto` the staged copy next to
/// the target (metadata-preserving, cross-volume safe), then two renames — the
/// old bundle is restored if the second rename fails.
pub fn apply_mac_app(staged: &Path, bundle: &Path) -> anyhow::Result<()> {
    let parent = bundle
        .parent()
        .context("app bundle has no parent directory")?;
    let name = bundle
        .file_name()
        .context("app bundle has no name")?
        .to_string_lossy();
    let pid = std::process::id();
    let fresh = parent.join(format!(".{name}.new-{pid}"));
    let old = parent.join(format!(".{name}.old-{pid}"));
    let _ = std::fs::remove_dir_all(&fresh);
    run(
        "ditto",
        &[&staged.to_string_lossy(), &fresh.to_string_lossy()],
    )?;
    std::fs::rename(bundle, &old).context("moving the current app aside")?;
    if let Err(err) = std::fs::rename(&fresh, bundle) {
        let _ = std::fs::rename(&old, bundle);
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(err).context("installing the new app bundle");
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Detached relauncher: waits for THIS process to exit, then `open`s the bundle.
/// (Opening before exit would race the single-instance engine lock and the IPC
/// port.) The caller quits the app after this returns.
pub fn relaunch_app_after_exit(bundle: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let pid = std::process::id();
        let script = format!(
            "while /bin/kill -0 {pid} 2>/dev/null; do sleep 0.2; done; /usr/bin/open \"{}\"",
            bundle.display()
        );
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        if let Err(err) = command.spawn() {
            tracing::error!(error = %err, "failed to spawn the relauncher");
        }
    }
    #[cfg(not(unix))]
    let _ = bundle;
}
