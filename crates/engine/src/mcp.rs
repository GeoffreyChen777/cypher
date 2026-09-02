//! MCP servers from Pi's `~/.pi/agent/mcp.json`, plus OAuth via Pi's
//! `pi-mcp-adapter` slash command (`/mcp-auth`), the same path as the TUI.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

use cypher_harness::Harness;

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
    pub adapter_installed: bool,
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

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn agent_dir() -> Option<PathBuf> {
    home().map(|h| h.join(".pi/agent"))
}

fn mcp_path() -> Option<PathBuf> {
    agent_dir().map(|d| d.join("mcp.json"))
}

fn adapter_dir() -> Option<PathBuf> {
    agent_dir().map(|d| d.join("npm/node_modules/pi-mcp-adapter"))
}

pub fn oauth_account(server: &str) -> String {
    format!("sha256-{:x}", Sha256::digest(server.as_bytes()))
}

fn read_mcp_root() -> Value {
    let Some(path) = mcp_path() else {
        return Value::Object(Default::default());
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Value::Object(Default::default());
    };
    serde_json::from_str(&text).unwrap_or_else(|_| Value::Object(Default::default()))
}

fn write_mcp_root(value: &Value) -> Result<(), String> {
    let path = mcp_path().ok_or_else(|| "HOME is not set.".to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "Invalid MCP settings path.".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn transport_label(entry: &Value) -> String {
    if let Some(url) = entry.get("url").and_then(Value::as_str) {
        return url.to_string();
    }
    if let Some(command) = entry.get("command").and_then(Value::as_str) {
        let args = entry
            .get("args")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        return if args.is_empty() {
            command.to_string()
        } else {
            format!("{command} {args}")
        };
    }
    if let Some(socket) = entry.get("socket").and_then(Value::as_str) {
        return socket.to_string();
    }
    "Configured".into()
}

fn auth_kind(entry: &Value) -> McpAuthKind {
    match entry.get("auth") {
        Some(Value::String(s)) if s == "oauth" => McpAuthKind::Oauth,
        Some(Value::String(s)) if s == "bearer" => McpAuthKind::Bearer,
        Some(Value::Bool(false)) => McpAuthKind::None,
        _ if entry.get("url").and_then(Value::as_str).is_some()
            && entry.get("headers").is_none() =>
        {
            McpAuthKind::Oauth
        }
        _ => McpAuthKind::None,
    }
}

fn keychain_payload(account: &str) -> Option<String> {
    let _ = ensure_app_keychain();
    let mut args = vec![
        "find-generic-password".into(),
        "-s".into(),
        "pi-mcp-adapter.oauth".into(),
        "-a".into(),
        account.to_string(),
        "-w".into(),
    ];
    if let Ok(kc) = app_keychain_path() {
        args.push(kc.display().to_string());
    }
    let output = std::process::Command::new("security")
        .args(&args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn legacy_payload(account: &str) -> Option<String> {
    let path = agent_dir()?.join("mcp-oauth").join(account).join("tokens.json");
    std::fs::read_to_string(path).ok()
}

fn token_status(payload: &str) -> McpAuthStatus {
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        return McpAuthStatus::SignedIn;
    };
    let tokens = value.get("tokens").unwrap_or(&value);
    if tokens.get("accessToken").and_then(Value::as_str).is_none() {
        return McpAuthStatus::NeedsAuth;
    }
    if let Some(expires) = tokens.get("expiresAt").and_then(Value::as_i64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if expires > 0 && expires + 30 < now {
            return McpAuthStatus::Expired;
        }
    }
    McpAuthStatus::SignedIn
}

fn auth_status(name: &str, kind: McpAuthKind) -> McpAuthStatus {
    match kind {
        McpAuthKind::None => McpAuthStatus::NotRequired,
        McpAuthKind::Bearer => {
            // Static bearer is configured in mcp.json; we only know it's required.
            McpAuthStatus::SignedIn
        }
        McpAuthKind::Oauth => {
            let account = oauth_account(name);
            match keychain_payload(&account).or_else(|| legacy_payload(&account)) {
                Some(payload) => token_status(&payload),
                None => McpAuthStatus::NeedsAuth,
            }
        }
    }
}

pub fn list() -> McpSnapshot {
    let root = read_mcp_root();
    let servers = root
        .get("mcpServers")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(name, entry)| {
                    let kind = auth_kind(entry);
                    McpServer {
                        enabled: !entry
                            .get("disabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        auth_status: auth_status(name, kind),
                        auth_kind: kind,
                        transport: transport_label(entry),
                        name: name.clone(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    McpSnapshot {
        adapter_installed: adapter_dir().is_some_and(|p| p.join("mcp-auth-flow.ts").is_file()),
        servers,
    }
}

pub fn set_enabled(params: SetMcpServerEnabled) -> Result<McpSnapshot, String> {
    let mut root = read_mcp_root();
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
    if params.enabled {
        object.remove("disabled");
    } else {
        object.insert("disabled".into(), Value::Bool(true));
    }
    write_mcp_root(&root)?;
    Ok(list())
}

pub fn auth_dump_path() -> Option<PathBuf> {
    agent_dir().map(|d| d.join(".cypher-mcp-auth-dump.jsonl"))
}

fn persist_auth_dump(path: &PathBuf) -> Result<usize, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(0);
    };
    let mut wrote = 0usize;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value =
            serde_json::from_str(line).map_err(|e| format!("Invalid auth dump: {e}"))?;
        let service = value
            .get("service")
            .and_then(Value::as_str)
            .unwrap_or("pi-mcp-adapter.oauth");
        let account = value
            .get("account")
            .and_then(Value::as_str)
            .ok_or_else(|| "Auth dump missing account.".to_string())?;
        let password = value
            .get("password")
            .and_then(Value::as_str)
            .ok_or_else(|| "Auth dump missing password.".to_string())?;
        persist_secret(service, account, password)?;
        wrote += 1;
    }
    Ok(wrote)
}

fn persist_secret(service: &str, account: &str, password: &str) -> Result<(), String> {
    if let Some(dir) = agent_dir().map(|d| d.join("mcp-oauth").join(account)) {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("tokens.json"), password).map_err(|e| e.to_string())?;
    }
    let _ = persist_secret_keychain(service, account, password);
    Ok(())
}

fn app_keychain_path() -> Result<PathBuf, String> {
    agent_dir()
        .map(|d| d.join("cypher-mcp.keychain-db"))
        .ok_or_else(|| "HOME is not set.".to_string())
}

fn app_keychain_pass_path() -> Result<PathBuf, String> {
    agent_dir()
        .map(|d| d.join("cypher-mcp.keychain-pass"))
        .ok_or_else(|| "HOME is not set.".to_string())
}

fn security_ok(args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new("security")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if err.is_empty() {
        format!("security {} failed", args.first().copied().unwrap_or_default())
    } else {
        err
    })
}

fn ensure_app_keychain() -> Result<(PathBuf, String), String> {
    let path = app_keychain_path()?;
    let pass_path = app_keychain_pass_path()?;
    let password = if pass_path.is_file() {
        std::fs::read_to_string(&pass_path)
            .map_err(|e| e.to_string())?
            .trim()
            .to_string()
    } else {
        let generated = uuid::Uuid::new_v4().to_string();
        if let Some(parent) = pass_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&pass_path, &generated).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&pass_path, std::fs::Permissions::from_mode(0o600));
        }
        generated
    };
    if !path.is_file() {
        security_ok(&[
            "create-keychain",
            "-p",
            &password,
            path.to_str().unwrap_or_default(),
        ])?;
        let _ = security_ok(&[
            "set-keychain-settings",
            "-lut",
            "2147483647",
            path.to_str().unwrap_or_default(),
        ]);
    }
    security_ok(&[
        "unlock-keychain",
        "-p",
        &password,
        path.to_str().unwrap_or_default(),
    ])?;
    prepend_keychain_search(&path)?;
    Ok((path, password))
}

fn prepend_keychain_search(path: &PathBuf) -> Result<(), String> {
    let listed = std::process::Command::new("security")
        .args(["list-keychains", "-d", "user"])
        .output()
        .map_err(|e| e.to_string())?;
    let existing: Vec<String> = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .map(|line| line.trim().trim_matches('"').to_string())
        .filter(|line| !line.is_empty())
        .collect();
    let ours = path.display().to_string();
    if existing.iter().any(|item| item == &ours) {
        return Ok(());
    }
    let mut args = vec!["list-keychains".into(), "-d".into(), "user".into(), "-s".into(), ours];
    args.extend(existing);
    let status = std::process::Command::new("security")
        .args(&args)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("Could not add Cypher's MCP keychain to the search list.".into())
    }
}

#[cfg(target_os = "macos")]
fn persist_secret_keychain(service: &str, account: &str, password: &str) -> Result<(), String> {
    // Login keychain writes need a UI session (TUI/Terminal has one; a GUI
    // child and even this process when launched detached do not). Store in an
    // app-owned keychain we can unlock without a prompt, then put it on the
    // search list so Pi's adapter can read the same item.
    let (keychain, _) = ensure_app_keychain()?;
    let kc = keychain.to_str().unwrap_or_default();
    let _ = security_ok(&["delete-generic-password", "-s", service, "-a", account, kc]);
    security_ok(&[
        "add-generic-password",
        "-a",
        account,
        "-s",
        service,
        "-A",
        "-w",
        password,
        kc,
    ])
}

#[cfg(not(target_os = "macos"))]
fn persist_secret_keychain(_service: &str, _account: &str, _password: &str) -> Result<(), String> {
    Ok(())
}

pub fn logout(name: &str) -> Result<McpSnapshot, String> {
    let account = oauth_account(name);
    let _ = std::process::Command::new("security")
        .args([
            "delete-generic-password",
            "-s",
            "pi-mcp-adapter.oauth",
            "-a",
            &account,
        ])
        .output();
    if let Ok(kc) = app_keychain_path() {
        let _ = std::process::Command::new("security")
            .args([
                "delete-generic-password",
                "-s",
                "pi-mcp-adapter.oauth",
                "-a",
                &account,
                kc.to_str().unwrap_or_default(),
            ])
            .output();
    }
    if let Some(dir) = agent_dir().map(|d| d.join("mcp-oauth").join(&account)) {
        let _ = std::fs::remove_dir_all(dir);
    }
    Ok(list())
}

pub async fn authenticate(name: &str, harness: &dyn Harness) -> Result<McpSnapshot, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Server name is required.".into());
    }
    let dump = auth_dump_path().ok_or_else(|| "HOME is not set.".to_string())?;
    let _ = std::fs::remove_file(&dump);
    // Drop a leftover keychain item first. `@napi-rs/keyring` cannot always
    // overwrite an entry created by a different parent process (TUI vs GUI).
    let _ = logout(name);
    let slash = harness.run_slash(&format!("/mcp-auth {name}")).await;
    let dumped = persist_auth_dump(&dump);
    let _ = std::fs::remove_file(&dump);
    match (slash, dumped) {
        (_, Ok(n)) if n > 0 => Ok(list()),
        (Err(err), _) => Err(err.to_string()),
        (Ok(_), Err(err)) => Err(err),
        (Ok(_), Ok(_)) => Ok(list()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_account_hashes_the_server_name() {
        assert_eq!(
            oauth_account("mvp-lab-discord"),
            format!("sha256-{:x}", Sha256::digest(b"mvp-lab-discord"))
        );
    }

    #[test]
    fn auth_kind_reads_oauth_and_url_defaults() {
        let oauth = serde_json::json!({"url": "https://x/mcp", "auth": "oauth"});
        assert_eq!(auth_kind(&oauth), McpAuthKind::Oauth);
        let auto = serde_json::json!({"url": "https://x/mcp"});
        assert_eq!(auth_kind(&auto), McpAuthKind::Oauth);
        let off = serde_json::json!({"url": "https://x/mcp", "auth": false});
        assert_eq!(auth_kind(&off), McpAuthKind::None);
        let stdio = serde_json::json!({"command": "npx", "args": ["-y", "foo"]});
        assert_eq!(auth_kind(&stdio), McpAuthKind::None);
        assert_eq!(transport_label(&stdio), "npx -y foo");
    }

    #[test]
    fn expired_tokens_are_detected() {
        let payload = serde_json::json!({
            "tokens": { "accessToken": "x", "expiresAt": 1 }
        })
        .to_string();
        assert_eq!(token_status(&payload), McpAuthStatus::Expired);
        let live = serde_json::json!({
            "tokens": { "accessToken": "x", "expiresAt": 4102444800i64 }
        })
        .to_string();
        assert_eq!(token_status(&live), McpAuthStatus::SignedIn);
    }
}
