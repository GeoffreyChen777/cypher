//! Download + verify.

use super::*;

/// Stream `{edge}/releases/<file>` to `dest`, requiring the manifest sha256 or
/// a standalone checksum for legacy metadata. Writes through a private temp file so an interrupted download never
/// leaves a plausible-looking artifact behind.
pub async fn download_release_file(
    edge_url: &str,
    manifest: &Manifest,
    file: &str,
    dest: &Path,
) -> anyhow::Result<()> {
    validate_version(&manifest.version)?;
    if file.is_empty()
        || file == "."
        || file == ".."
        || file.len() > 255
        || !file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        bail!("invalid release artifact name");
    }
    let url = format!("{}/releases/{file}", edge_url.trim_end_matches('/'));
    let expected = match manifest.files.get(file).and_then(|m| m.sha256.as_deref()) {
        Some(hash) => hash.to_owned(),
        None if manifest.files.is_empty() => {
            let response = http_client()?
                .get(format!("{url}.sha256"))
                .send()
                .await?
                .error_for_status()
                .context("fetching required artifact checksum")?;
            String::from_utf8(limited_body(response, 256).await?)?
                .trim()
                .to_owned()
        }
        None => bail!("release manifest is missing the checksum for {file}"),
    };
    if !valid_sha256(&expected) {
        bail!("invalid SHA-256 for {file}");
    }
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let partial = tempfile::Builder::new()
        .prefix(".download-")
        .tempfile_in(parent)?;
    let resp = http_client()?
        .get(&url)
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?
        .error_for_status()
        .with_context(|| format!("downloading {url}"))?;
    let mut out = tokio::fs::File::from_std(partial.reopen()?);
    let mut hasher = Sha256::new();
    let mut stream = resp.bytes_stream();
    let mut size = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading download stream")?;
        size += chunk.len() as u64;
        if size > 512 * 1024 * 1024 {
            bail!("release artifact exceeds 512 MiB limit");
        }
        hasher.update(&chunk);
        out.write_all(&chunk).await.context("writing download")?;
    }
    out.flush().await.context("flushing download")?;
    out.sync_all().await.context("syncing download")?;
    drop(out);
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(&expected) {
        bail!("checksum mismatch for {file}: expected {expected}, got {actual}");
    }
    partial
        .persist(dest)
        .with_context(|| format!("moving {} into place", dest.display()))?;
    Ok(())
}

/// Run blocking work (`tar`, `ditto`, bundle moves) on tokio's blocking pool.
/// The headed app embeds the engine in a small runtime; a synchronous call on
/// one of its workers stalls every other task there.
pub(super) async fn off_runtime<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .context("update task failed")?
}

pub(super) fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = std::process::Command::new(program)
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
