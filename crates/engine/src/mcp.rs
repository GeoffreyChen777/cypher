//! MCP servers from Cypher's isolated Pi agent directory, served by Pi's
//! built-in MCP support. OAuth runs Pi's `/mcp login`, the same path as the
//! TUI, and Pi keeps the credentials in `<agent-dir>/mcp-auth.json`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

use cypher_harness::Harness;

use crate::pi::runtime::PiRuntimePaths;

mod config;
mod legacy;
pub mod login;
pub use config::{AddMcpServers, RemoveMcpServer, add_servers, remove_server};
pub use legacy::adopt_builtin;
static CONFIG_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpAuthKind {
    None,
    Oauth,
    Bearer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpAuthStatus {
    NotRequired,
    SignedIn,
    NeedsAuth,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub name: String,
    pub transport: String,
    pub auth_kind: McpAuthKind,
    pub auth_status: McpAuthStatus,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSnapshot {
    /// Whether Pi's built-in MCP serves `mcp.json` on this runtime. Kept under
    /// its adapter-era wire name, which older viewers require.
    #[serde(rename = "adapterInstalled")]
    pub available: bool,
    /// Why it does not, when it does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    pub servers: Vec<McpServer>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerName {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetMcpServerEnabled {
    pub name: String,
    pub enabled: bool,
}

fn mcp_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("mcp.json")
}

fn credentials_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("mcp-auth.json")
}

/// Why Pi's built-in MCP cannot serve `mcp.json` with this runtime, if it
/// cannot: Pi before 1.0 has none, and an extension that registers `/mcp`
/// (pi-mcp-adapter) replaces it.
pub fn unavailable(paths: &PiRuntimePaths) -> Option<String> {
    if !paths
        .package_dir
        .join("dist/extensions/mcp/index.js")
        .is_file()
    {
        return Some("Install or update the Pi Runtime in Agents to use MCP servers.".into());
    }
    crate::pi::packages::enabled(paths, "npm:pi-mcp-adapter").then(|| {
        "pi-mcp-adapter replaces Pi's built-in MCP. Disable it in Agents to manage MCP servers here."
            .into()
    })
}

fn read_mcp_root(agent_dir: &Path) -> Value {
    let path = mcp_path(agent_dir);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Value::Object(Default::default());
    };
    serde_json::from_str(&text).unwrap_or_else(|_| Value::Object(Default::default()))
}

fn write_json(path: &Path, value: &Value, what: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Invalid {what} path."))?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    text.push('\n');
    use std::io::Write;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| format!("Could not stage {what}."))?;
    temporary
        .write_all(text.as_bytes())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|_| format!("Could not write {what}."))?;
    temporary
        .persist(path)
        .map_err(|_| format!("Could not save {what}."))?;
    Ok(())
}

fn write_mcp_root(agent_dir: &Path, value: &Value) -> Result<(), String> {
    write_json(&mcp_path(agent_dir), value, "MCP configuration")
}

fn transport_label(entry: &Value) -> String {
    if let Some(url) = entry.get("url").and_then(Value::as_str) {
        return match reqwest::Url::parse(url) {
            Ok(mut url) => {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
                url.to_string()
            }
            Err(_) => "HTTP endpoint (invalid URL)".into(),
        };
    }
    if let Some(command) = entry.get("command").and_then(Value::as_str) {
        // Arguments can contain credentials. Listings are safe metadata, not
        // a command preview, especially when viewed from another device.
        return format!("stdio · {command}");
    }
    "Configured".into()
}

fn has_authorization_header(entry: &Value) -> bool {
    entry
        .get("headers")
        .and_then(Value::as_object)
        .is_some_and(|headers| {
            headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("authorization"))
        })
}

/// Pi signs in with OAuth to HTTP servers without an `Authorization` header
/// or `auth.provider`; the others carry their own credential. Adapter-era
/// fields still count until [`adopt_builtin`] rewrites them.
fn auth_kind(entry: &Value) -> McpAuthKind {
    if entry.get("url").and_then(Value::as_str).is_none() {
        return McpAuthKind::None;
    }
    match entry.get("auth") {
        Some(Value::Bool(false)) => McpAuthKind::None,
        Some(Value::Object(_)) => McpAuthKind::Bearer,
        Some(Value::String(s)) if s == "bearer" => McpAuthKind::Bearer,
        _ if has_authorization_header(entry)
            || entry.get("bearerToken").is_some()
            || entry.get("bearerTokenEnv").is_some() =>
        {
            McpAuthKind::Bearer
        }
        _ => McpAuthKind::Oauth,
    }
}

fn enabled(entry: &Value) -> bool {
    entry.get("enabled") != Some(&Value::Bool(false))
        && entry.get("disabled") != Some(&Value::Bool(true))
}

/// Pi's credential keys for a server: its tool namespace plus URL, then the
/// URL alone, which Pi before 1.0 wrote (`McpOAuthCredentialStore`).
fn credential_keys(name: &str, url: &str) -> Option<[String; 2]> {
    let url = reqwest::Url::parse(url).ok()?.to_string();
    Some([format!("mcp__{}|{url}", name.replace('-', "_")), url])
}

fn read_credentials(agent_dir: &Path) -> Map<String, Value> {
    std::fs::read(credentials_path(agent_dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn token_status(state: &Value) -> McpAuthStatus {
    let tokens = &state["tokens"];
    if tokens["access_token"].as_str().is_none_or(str::is_empty) {
        return McpAuthStatus::NeedsAuth;
    }
    // Pi refreshes an expired token itself while it has a refresh token.
    if tokens["refresh_token"].as_str().is_none_or(str::is_empty)
        && let Some(expires) = state["tokensExpireAt"].as_i64()
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if expires < now {
            return McpAuthStatus::Expired;
        }
    }
    McpAuthStatus::SignedIn
}

fn auth_status(
    credentials: &Map<String, Value>,
    name: &str,
    entry: &Value,
    kind: McpAuthKind,
) -> McpAuthStatus {
    match kind {
        McpAuthKind::None => McpAuthStatus::NotRequired,
        // The credential is in mcp.json (or a provider login); we only know
        // one is configured.
        McpAuthKind::Bearer => McpAuthStatus::SignedIn,
        McpAuthKind::Oauth => entry["url"]
            .as_str()
            .and_then(|url| credential_keys(name, url))
            .and_then(|keys| keys.iter().find_map(|key| credentials.get(key)))
            .map_or(McpAuthStatus::NeedsAuth, token_status),
    }
}

fn status_of(agent_dir: &Path, name: &str) -> McpAuthStatus {
    let entry = read_mcp_root(agent_dir)["mcpServers"][name].clone();
    auth_status(
        &read_credentials(agent_dir),
        name,
        &entry,
        auth_kind(&entry),
    )
}

pub fn list(paths: &PiRuntimePaths) -> McpSnapshot {
    let agent_dir = &paths.agent_dir;
    let root = read_mcp_root(agent_dir);
    let credentials = read_credentials(agent_dir);
    let servers = root
        .get("mcpServers")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(name, entry)| {
                    let kind = auth_kind(entry);
                    McpServer {
                        enabled: enabled(entry),
                        auth_status: auth_status(&credentials, name, entry, kind),
                        auth_kind: kind,
                        transport: transport_label(entry),
                        name: name.clone(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let unavailable = unavailable(paths);
    McpSnapshot {
        available: unavailable.is_none(),
        unavailable,
        servers,
    }
}

pub fn set_enabled(
    paths: &PiRuntimePaths,
    params: SetMcpServerEnabled,
) -> Result<McpSnapshot, String> {
    let agent_dir = &paths.agent_dir;
    let _guard = CONFIG_WRITE
        .lock()
        .map_err(|_| "MCP configuration is busy.".to_string())?;
    let mut root = config::read_for_update(agent_dir)?;
    let servers = root
        .as_object_mut()
        .ok_or_else(|| "MCP settings are not a JSON object.".to_string())?
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Default::default()));
    let map = servers
        .as_object_mut()
        .ok_or_else(|| "mcpServers is not an object.".to_string())?;
    let entry = map
        .get_mut(&params.name)
        .ok_or_else(|| format!("MCP server \"{}\" is not configured.", params.name))?;
    let object = entry
        .as_object_mut()
        .ok_or_else(|| "MCP server entry is not an object.".to_string())?;
    // Pi's own `/mcp` writes the same shape: no key while enabled.
    object.remove("disabled");
    if params.enabled {
        object.remove("enabled");
    } else {
        object.insert("enabled".into(), Value::Bool(false));
    }
    write_mcp_root(agent_dir, &root)?;
    Ok(list(paths))
}

/// Pi locks `mcp-auth.json` with proper-lockfile: a `<file>.lock` directory,
/// taken over once its mtime is older than this.
const CREDENTIALS_LOCK_STALE: std::time::Duration = std::time::Duration::from_secs(10);

/// Remove a server's stored OAuth credentials from Pi's `mcp-auth.json`,
/// under the lock Pi takes for every read-modify-write of that file.
fn remove_credentials(agent_dir: &Path, name: &str, url: &str) -> Result<(), String> {
    let path = credentials_path(agent_dir);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(meta) if meta.is_file() => {}
        _ => return Err("Pi's MCP credential store is unavailable or a symlink.".into()),
    }
    let Some(keys) = credential_keys(name, url) else {
        return Ok(());
    };
    let lock = PathBuf::from(format!("{}.lock", path.display()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match std::fs::create_dir(&lock) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(&lock)
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > CREDENTIALS_LOCK_STALE);
                if stale {
                    let _ = std::fs::remove_dir(&lock);
                } else if std::time::Instant::now() >= deadline {
                    return Err("Pi's MCP credentials are busy. Retry in a moment.".into());
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
            Err(_) => return Err("Could not lock Pi's MCP credential store.".into()),
        }
    }
    let result = (|| {
        let bytes = std::fs::read(&path).map_err(|_| "Could not read Pi's MCP credentials.")?;
        let mut states: Map<String, Value> = if bytes.iter().all(u8::is_ascii_whitespace) {
            Map::new()
        } else {
            serde_json::from_slice(&bytes).map_err(|_| "Pi's MCP credential file is invalid.")?
        };
        if keys.iter().any(|key| states.remove(key).is_some()) {
            write_json(&path, &Value::Object(states), "MCP credentials")?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir(&lock);
    result
}

/// Remove a server's stored OAuth credentials. Servers without OAuth have
/// none.
fn sign_out(agent_dir: &Path, name: &str) -> Result<(), String> {
    let entry = read_mcp_root(agent_dir)["mcpServers"][name].clone();
    match entry["url"].as_str() {
        Some(url) if auth_kind(&entry) == McpAuthKind::Oauth => {
            remove_credentials(agent_dir, name, url)
        }
        _ => Ok(()),
    }
}

pub fn logout(paths: &PiRuntimePaths, name: &str) -> Result<McpSnapshot, String> {
    sign_out(&paths.agent_dir, name)?;
    Ok(list(paths))
}

/// Non-interactive sign-in for older viewers: only a browser on this host
/// can complete it, through Pi's loopback callback.
pub async fn authenticate(
    paths: &PiRuntimePaths,
    name: &str,
    harness: &dyn Harness,
) -> Result<McpSnapshot, String> {
    let name = name.trim().to_string();
    if name.is_empty() || name.contains(char::is_whitespace) {
        return Err("Server name is required.".into());
    }
    if let Some(reason) = unavailable(paths) {
        return Err(reason);
    }
    harness
        .run_slash(&format!("/mcp login {name}"))
        .await
        .map_err(|err| err.to_string())?;
    let paths = paths.clone();
    crate::util::off_runtime(move || list(&paths)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listings_never_expose_url_credentials_or_command_arguments() {
        let http = serde_json::json!({
            "url": "https://user:fixture-secret@example.com/mcp?api_key=fixture-secret#fixture-secret"
        });
        assert_eq!(transport_label(&http), "https://example.com/mcp");
        let stdio = serde_json::json!({"command": "node", "args": ["--token", "fixture-secret"]});
        assert_eq!(transport_label(&stdio), "stdio · node");
    }

    #[test]
    fn credential_keys_match_pi() {
        // Pi: `${mcpNamespace(name)}|${String(new URL(url))}`, then the URL.
        assert_eq!(
            credential_keys("mvp-lab-discord", "https://mcp.mvp-lab.ai/wiki-mcp/mcp").unwrap(),
            [
                "mcp__mvp_lab_discord|https://mcp.mvp-lab.ai/wiki-mcp/mcp".to_string(),
                "https://mcp.mvp-lab.ai/wiki-mcp/mcp".to_string()
            ]
        );
        assert_eq!(
            credential_keys("docs", "HTTPS://Example.com:443").unwrap()[0],
            "mcp__docs|https://example.com/"
        );
    }

    #[test]
    fn auth_kind_follows_pi_and_reads_adapter_fields() {
        let oauth = serde_json::json!({"url": "https://x/mcp"});
        assert_eq!(auth_kind(&oauth), McpAuthKind::Oauth);
        let header =
            serde_json::json!({"url": "https://x/mcp", "headers": {"authorization": "Bearer t"}});
        assert_eq!(auth_kind(&header), McpAuthKind::Bearer);
        let other_header = serde_json::json!({"url": "https://x/mcp", "headers": {"X-Key": "k"}});
        assert_eq!(auth_kind(&other_header), McpAuthKind::Oauth);
        let provider = serde_json::json!({"url": "https://x/mcp", "auth": {"provider": "github"}});
        assert_eq!(auth_kind(&provider), McpAuthKind::Bearer);
        let off = serde_json::json!({"url": "https://x/mcp", "auth": false});
        assert_eq!(auth_kind(&off), McpAuthKind::None);
        let legacy =
            serde_json::json!({"url": "https://x/mcp", "auth": "bearer", "bearerToken": "t"});
        assert_eq!(auth_kind(&legacy), McpAuthKind::Bearer);
        let stdio = serde_json::json!({"command": "npx", "args": ["-y", "foo"]});
        assert_eq!(auth_kind(&stdio), McpAuthKind::None);
    }

    #[test]
    fn token_status_reads_pi_state() {
        assert_eq!(
            token_status(&serde_json::json!({"clientInformation": {"client_id": "c"}})),
            McpAuthStatus::NeedsAuth
        );
        let expired = serde_json::json!({"tokens": {"access_token": "x"}, "tokensExpireAt": 1});
        assert_eq!(token_status(&expired), McpAuthStatus::Expired);
        let refreshable = serde_json::json!({
            "tokens": {"access_token": "x", "refresh_token": "r"}, "tokensExpireAt": 1
        });
        assert_eq!(token_status(&refreshable), McpAuthStatus::SignedIn);
        let live = serde_json::json!({
            "tokens": {"access_token": "x"}, "tokensExpireAt": 4102444800000i64
        });
        assert_eq!(token_status(&live), McpAuthStatus::SignedIn);
    }

    fn paths_with_builtin(dir: &Path) -> PiRuntimePaths {
        let paths = PiRuntimePaths::for_data_dir(dir);
        let mcp = paths.package_dir.join("dist/extensions/mcp");
        std::fs::create_dir_all(&mcp).unwrap();
        std::fs::write(mcp.join("index.js"), "").unwrap();
        std::fs::create_dir_all(&paths.agent_dir).unwrap();
        paths
    }

    #[test]
    fn availability_needs_builtin_mcp_without_the_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let bare = PiRuntimePaths::for_data_dir(dir.path());
        assert!(!list(&bare).available);
        let paths = paths_with_builtin(dir.path());
        assert!(list(&paths).available);
        std::fs::write(
            paths.agent_dir.join("settings.json"),
            r#"{"packages":["npm:pi-mcp-adapter"]}"#,
        )
        .unwrap();
        let snapshot = list(&paths);
        assert!(!snapshot.available);
        assert!(snapshot.unavailable.unwrap().contains("pi-mcp-adapter"));
        assert_eq!(
            serde_json::to_value(list(&bare)).unwrap()["adapterInstalled"],
            false
        );
    }

    #[test]
    fn sign_in_status_and_sign_out_use_pi_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_with_builtin(dir.path());
        std::fs::write(
            mcp_path(&paths.agent_dir),
            r#"{"mcpServers":{"docs-a":{"url":"https://example.com/mcp"},"docs-b":{"url":"https://example.com/mcp","enabled":false}}}"#,
        )
        .unwrap();
        let signed_in =
            serde_json::json!({"tokens": {"access_token": "fixture-secret", "refresh_token": "r"}});
        std::fs::write(
            credentials_path(&paths.agent_dir),
            serde_json::to_vec(&serde_json::json!({
                "mcp__docs_a|https://example.com/mcp": signed_in,
                "mcp__docs_b|https://example.com/mcp": signed_in,
                "mcp__other|https://other.example/": signed_in,
            }))
            .unwrap(),
        )
        .unwrap();
        let snapshot = list(&paths);
        assert!(
            snapshot
                .servers
                .iter()
                .all(|s| s.auth_status == McpAuthStatus::SignedIn)
        );
        assert!(!snapshot.servers[1].enabled);
        assert!(
            !serde_json::to_string(&snapshot)
                .unwrap()
                .contains("fixture-secret")
        );
        let snapshot = logout(&paths, "docs-a").unwrap();
        assert_eq!(snapshot.servers[0].auth_status, McpAuthStatus::NeedsAuth);
        assert_eq!(snapshot.servers[1].auth_status, McpAuthStatus::SignedIn);
        let stored = read_credentials(&paths.agent_dir);
        assert_eq!(stored.len(), 2);
        assert!(stored.contains_key("mcp__other|https://other.example/"));
        assert!(
            !credentials_path(&paths.agent_dir)
                .with_extension("json.lock")
                .exists()
        );
    }

    #[test]
    fn sign_out_waits_for_and_takes_over_pi_locks() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_with_builtin(dir.path());
        std::fs::write(
            mcp_path(&paths.agent_dir),
            r#"{"mcpServers":{"docs":{"url":"https://example.com/mcp"}}}"#,
        )
        .unwrap();
        let path = credentials_path(&paths.agent_dir);
        std::fs::write(&path, r#"{"mcp__docs|https://example.com/mcp":{}}"#).unwrap();
        let lock = PathBuf::from(format!("{}.lock", path.display()));
        std::fs::create_dir(&lock).unwrap();
        assert!(logout(&paths, "docs").unwrap_err().contains("busy"));
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::open(&lock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        logout(&paths, "docs").unwrap();
        assert!(read_credentials(&paths.agent_dir).is_empty());
        assert!(!lock.exists());
    }

    #[test]
    fn enabling_writes_pi_shape() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_with_builtin(dir.path());
        std::fs::write(
            mcp_path(&paths.agent_dir),
            r#"{"mcpServers":{"docs":{"url":"https://example.com/mcp","disabled":true}}}"#,
        )
        .unwrap();
        assert!(!list(&paths).servers[0].enabled);
        let enable = |enabled| SetMcpServerEnabled {
            name: "docs".into(),
            enabled,
        };
        assert!(set_enabled(&paths, enable(true)).unwrap().servers[0].enabled);
        assert_eq!(
            read_mcp_root(&paths.agent_dir)["mcpServers"]["docs"],
            serde_json::json!({"url": "https://example.com/mcp"})
        );
        assert!(!set_enabled(&paths, enable(false)).unwrap().servers[0].enabled);
        assert_eq!(
            read_mcp_root(&paths.agent_dir)["mcpServers"]["docs"]["enabled"],
            false
        );
    }
}
