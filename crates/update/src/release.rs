//! Release metadata.

use super::*;

/// `{edge}/releases/manifest.json` — written by the release workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// Per-platform build counter. Platforms share a version but publish
    /// independently, so the same version may be re-cut for one platform
    /// alone. Absent (1) on legacy and pre-manifest channels.
    #[serde(default = "one")]
    pub build: u32,
    /// Artifact file name → metadata. Empty for pre-manifest releases resolved
    /// via `latest.txt` — downloads then require the artifact's .sha256 sidecar.
    #[serde(default)]
    pub files: BTreeMap<String, FileMeta>,
    /// Role → artifact file name, written by the per-platform publisher. A
    /// re-cut build does not use the historic name, so the name is read from
    /// here rather than rebuilt from the version.
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
}

pub(super) fn one() -> u32 {
    1
}

impl Default for Manifest {
    /// Build 1, matching the serde default: a channel that does not state a
    /// build is the first cut of its version, never a zeroth one.
    fn default() -> Self {
        Self {
            version: String::new(),
            build: 1,
            files: BTreeMap::new(),
            roles: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileMeta {
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Release channel for this platform: `linux` or `macos`. Each channel moves
/// on its own, so a client only ever consults its own.
pub fn channel() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Artifact-name platform pair — `uname`-style strings matching the packaging
/// scripts: `linux-x86_64`, `linux-aarch64`, `macos-arm64`.
pub fn platform_key() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = match (os, std::env::consts::ARCH) {
        ("macos", "aarch64") => "arm64",
        (_, arch) => arch,
    };
    (os, arch)
}

/// `cypher-<ver>-<os>-<arch>.tar.gz` — the headless/CLI tarball (Linux CI builds).
pub fn headless_artifact(version: &str) -> String {
    let (os, arch) = platform_key();
    format!("cypher-{version}-{os}-{arch}.tar.gz")
}

/// `cypher-<ver>-macos-<arch>-app.tar.gz` — the macOS app update payload.
pub fn mac_app_artifact(version: &str) -> String {
    let (_, arch) = platform_key();
    format!("cypher-{version}-macos-{arch}-app.tar.gz")
}

impl Manifest {
    /// Resolve an artifact by role, falling back to the historic name.
    ///
    /// A per-platform manifest names its artifacts explicitly, which is what
    /// lets a re-cut build ship under a name the version alone cannot produce.
    /// The fallback keeps legacy and `latest.txt` channels working, where the
    /// name has always been derived from the version.
    fn artifact(&self, role: &str, fallback: impl FnOnce(&str) -> String) -> String {
        self.roles
            .get(role)
            .cloned()
            .unwrap_or_else(|| fallback(&self.version))
    }

    /// The headless/CLI tarball for this machine.
    pub(crate) fn headless_file(&self) -> String {
        let (_, arch) = platform_key();
        self.artifact(&format!("headless-{arch}"), headless_artifact)
    }

    /// The macOS app update payload.
    pub(crate) fn mac_app_file(&self) -> String {
        let (_, arch) = platform_key();
        self.artifact(&format!("app-{arch}"), mac_app_artifact)
    }
}

/// Strictly-newer dotted-numeric compare (`0.1.10` > `0.1.9` > `0.1`).
/// Unparseable versions never count as newer — a garbage `latest.txt` must not
/// trigger an update loop.
pub fn version_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> Option<Vec<u64>> {
        let nums: Vec<u64> = v
            .trim()
            .trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().ok())
            .collect::<Option<_>>()?;
        (!nums.is_empty()).then_some(nums)
    }
    match (parts(latest), parts(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Fetch the newest release metadata for THIS platform.
///
/// Platforms publish independently, so the per-platform channel is the
/// authoritative one: `{channel}/manifest.json`. The shared `manifest.json`
/// and then `latest.txt` remain as fallbacks for channels published before
/// decoupling, and for a client pointed at an older deployment.
pub async fn fetch_latest(edge_url: &str) -> anyhow::Result<Manifest> {
    let base = edge_url.trim_end_matches('/');
    let client = http_client()?;
    for path in [
        format!("{}/manifest.json", channel()),
        "manifest.json".to_string(),
    ] {
        let url = format!("{base}/releases/{path}");
        match client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("fetching {path}"))?
        {
            resp if resp.status().is_success() => {
                let manifest: Manifest =
                    serde_json::from_slice(&limited_body(resp, 1024 * 1024).await?)
                        .with_context(|| format!("parsing {path}"))?;
                validate_version(&manifest.version)?;
                return Ok(manifest);
            }
            resp if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                tracing::debug!(status = %resp.status(), %path, "release manifest unavailable")
            }
            resp => bail!("fetching {path} failed (HTTP {})", resp.status()),
        }
    }
    let latest_url = format!("{base}/releases/latest.txt");
    let response = client
        .get(&latest_url)
        .send()
        .await
        .context("fetching latest.txt")?
        .error_for_status()
        .context("fetching latest.txt")?;
    let version = String::from_utf8(limited_body(response, 256).await?)?
        .trim()
        .to_string();
    validate_version(&version)?;
    Ok(Manifest {
        version,
        ..Manifest::default()
    })
}

pub(super) fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("cypher/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("building http client")
}

pub(super) fn validate_version(version: &str) -> anyhow::Result<()> {
    if version.is_empty()
        || version.len() > 64
        || version
            .split('.')
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        bail!("invalid release version");
    }
    Ok(())
}

pub(super) async fn limited_body(
    response: reqwest::Response,
    limit: usize,
) -> anyhow::Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > limit {
            bail!("release metadata exceeds size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(super) fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
