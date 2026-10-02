//! The move from pi-mcp-adapter to Pi's built-in MCP. The adapter's fields
//! that Pi rejects or reads differently are rewritten, and its credential
//! stores, which nothing reads any more, are deleted: each OAuth server signs
//! in once more, into Pi's `mcp-auth.json`.
use super::*;

/// Once the runtime serves MCP itself: rewrite `mcp.json` and delete the
/// adapter's state. Runs at engine boot and after each Runtime activation;
/// it is a no-op once done.
pub fn adopt_builtin(paths: &PiRuntimePaths) -> Result<(), String> {
    if unavailable(paths).is_some() {
        return Ok(());
    }
    migrate_config(&paths.agent_dir)?;
    remove_adapter_state(&paths.agent_dir);
    Ok(())
}

pub(super) fn loopback_redirect(uri: &str) -> bool {
    reqwest::Url::parse(uri).is_ok_and(|url| {
        url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

/// Rewrite one server's adapter fields into Pi's shape. Fields Pi ignores
/// (`lifecycle`, `directTools`, `oauth.grantType`, …) stay. Returns whether
/// anything changed.
pub(super) fn to_builtin(entry: &mut Map<String, Value>) -> bool {
    let before = entry.clone();
    if entry.remove("disabled") == Some(Value::Bool(true)) && !entry.contains_key("enabled") {
        entry.insert("enabled".into(), Value::Bool(false));
    }
    // A static bearer token becomes the Authorization header Pi sends.
    let token = entry
        .remove("bearerToken")
        .and_then(|token| token.as_str().map(|token| format!("Bearer {token}")));
    let token_env = entry
        .remove("bearerTokenEnv")
        .and_then(|name| name.as_str().map(|name| format!("Bearer ${{{name}}}")));
    if let Some(value) = token.or(token_env)
        && !has_authorization_header(&Value::Object(entry.clone()))
        && let Some(headers) = entry
            .entry("headers")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
    {
        headers.insert("Authorization".into(), Value::String(value));
    }
    // Pi takes `auth` only as `{ "provider": … }` and skips a server with the
    // adapter's "oauth", "bearer" or false. OAuth is its default anyway.
    if entry.get("auth").is_some_and(|auth| !auth.is_object()) {
        entry.remove("auth");
    }
    match entry.get_mut("oauth") {
        Some(Value::Object(oauth)) => {
            if !oauth.contains_key("callbackUrl")
                && let Some(uri) = oauth
                    .get("redirectUri")
                    .and_then(Value::as_str)
                    .filter(|uri| loopback_redirect(uri))
                    .map(str::to_owned)
            {
                oauth.remove("redirectUri");
                oauth.insert("callbackUrl".into(), Value::String(uri));
            }
        }
        Some(_) => {
            entry.remove("oauth");
        }
        None => {}
    }
    if let Some(ms) = entry.remove("requestTimeoutMs").and_then(|ms| ms.as_u64())
        && ms > 0
        && !entry.contains_key("timeout")
    {
        let seconds = if ms % 1000 == 0 {
            serde_json::json!(ms / 1000)
        } else {
            serde_json::json!(ms as f64 / 1000.0)
        };
        entry.insert("timeout".into(), seconds);
    }
    *entry != before
}

fn migrate_config(agent_dir: &Path) -> Result<(), String> {
    let _guard = CONFIG_WRITE
        .lock()
        .map_err(|_| "MCP configuration is busy.")?;
    if !mcp_path(agent_dir).exists() {
        return Ok(());
    }
    let mut root = config::read_for_update(agent_dir)?;
    let mut changed = false;
    if let Some(servers) = root.get_mut("mcpServers").and_then(Value::as_object_mut) {
        for entry in servers.values_mut().filter_map(Value::as_object_mut) {
            changed |= to_builtin(entry);
        }
    }
    if changed {
        write_mcp_root(agent_dir, &root)?;
        tracing::info!("moved mcp.json from pi-mcp-adapter to Pi's built-in MCP");
    }
    Ok(())
}

/// Delete the adapter's private keychain (which also takes it off the user's
/// keychain search list), its password, token files, sign-in dump and caches.
/// Never touches the login keychain: a system Pi may use the same entries.
fn remove_adapter_state(agent_dir: &Path) {
    let keychain = agent_dir.join("cypher-mcp.keychain-db");
    let keychain_gone = match std::fs::symlink_metadata(&keychain) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Ok(meta) if meta.is_file() => delete_keychain(&keychain),
        _ => false,
    };
    if keychain_gone {
        let _ = std::fs::remove_file(agent_dir.join("cypher-mcp.keychain-pass"));
    } else {
        tracing::warn!(
            "could not delete pi-mcp-adapter's private MCP keychain; retrying next start"
        );
    }
    // `remove_dir_all` removes a symlink itself, never its target.
    let _ = std::fs::remove_dir_all(agent_dir.join("mcp-oauth"));
    for file in [
        ".cypher-mcp-auth-dump.jsonl",
        "mcp-cache.json",
        "mcp-onboarding.json",
    ] {
        let _ = std::fs::remove_file(agent_dir.join(file));
    }
}

#[cfg(target_os = "macos")]
fn delete_keychain(path: &Path) -> bool {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    // Deleting a keychain needs neither its password nor an unlock prompt.
    let Ok(mut child) = Command::new("/usr/bin/security")
        .arg("delete-keychain")
        .arg(path)
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
            Ok(Some(status)) => return status.success() || !path.exists(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn delete_keychain(path: &Path) -> bool {
    std::fs::remove_file(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated(entry: Value) -> Value {
        let mut entry = entry.as_object().unwrap().clone();
        to_builtin(&mut entry);
        Value::Object(entry)
    }

    #[test]
    fn adapter_fields_become_pi_fields() {
        assert_eq!(
            migrated(serde_json::json!({
                "url": "https://mcp.example/mcp", "auth": "oauth",
                "oauth": {"clientId": "c", "scope": "read offline_access",
                    "redirectUri": "http://localhost:8976/callback"},
                "disabled": true, "requestTimeoutMs": 30000, "lifecycle": "eager"
            })),
            serde_json::json!({
                "url": "https://mcp.example/mcp",
                "oauth": {"clientId": "c", "scope": "read offline_access",
                    "callbackUrl": "http://localhost:8976/callback"},
                "enabled": false, "timeout": 30, "lifecycle": "eager"
            })
        );
        assert_eq!(
            migrated(serde_json::json!({
                "url": "https://x/mcp", "auth": "bearer", "bearerToken": "fixture-secret",
                "headers": {"X-Team": "a"}
            })),
            serde_json::json!({
                "url": "https://x/mcp",
                "headers": {"X-Team": "a", "Authorization": "Bearer fixture-secret"}
            })
        );
        assert_eq!(
            migrated(serde_json::json!({"url": "https://x/mcp", "bearerTokenEnv": "DOCS_TOKEN"})),
            serde_json::json!({"url": "https://x/mcp", "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}})
        );
        // An explicit header wins over the adapter's token field.
        assert_eq!(
            migrated(serde_json::json!({
                "url": "https://x/mcp", "bearerToken": "t", "headers": {"authorization": "Basic b"}
            })),
            serde_json::json!({"url": "https://x/mcp", "headers": {"authorization": "Basic b"}})
        );
        assert_eq!(
            migrated(serde_json::json!({
                "url": "https://x/mcp", "auth": false, "oauth": false, "requestTimeoutMs": 1500
            })),
            serde_json::json!({"url": "https://x/mcp", "timeout": 1.5})
        );
        // Pi only serves loopback callbacks; another redirect stays as it was.
        assert_eq!(
            migrated(serde_json::json!({
                "url": "https://x/mcp", "oauth": {"redirectUri": "https://app.example/callback"}
            }))["oauth"],
            serde_json::json!({"redirectUri": "https://app.example/callback"})
        );
    }

    #[test]
    fn pi_shaped_entries_are_left_alone() {
        for entry in [
            serde_json::json!({"url": "https://x/mcp", "enabled": false, "timeout": 30,
                "auth": {"provider": "github"}, "exposure": "direct"}),
            serde_json::json!({"command": "npx", "args": ["-y", "server"], "env": {"K": "${V}"}}),
            serde_json::json!({"url": "https://x/mcp", "headers": {"Authorization": "Bearer t"},
                "oauth": {"clientId": "c", "callbackPort": 8765}}),
        ] {
            let mut object = entry.as_object().unwrap().clone();
            assert!(!to_builtin(&mut object));
            assert_eq!(Value::Object(object), entry);
        }
    }

    #[test]
    fn adoption_waits_for_builtin_mcp_then_migrates_and_clears_adapter_state() {
        let dir = tempfile::tempdir().unwrap();
        let paths = PiRuntimePaths::for_data_dir(dir.path());
        let agent = &paths.agent_dir;
        std::fs::create_dir_all(agent.join("mcp-oauth/sha256-x")).unwrap();
        std::fs::write(
            agent.join("mcp-oauth/sha256-x/tokens.json"),
            "fixture-secret",
        )
        .unwrap();
        std::fs::write(agent.join("cypher-mcp.keychain-pass"), "fixture-secret").unwrap();
        std::fs::write(agent.join("mcp-cache.json"), "{}").unwrap();
        std::fs::write(agent.join("mcp-auth.json"), "{}").unwrap();
        let original = serde_json::json!({
            "settings": {"toolPrefix": "short"},
            "mcpServers": {"docs": {"url": "https://x/mcp", "auth": "oauth", "disabled": true}}
        });
        std::fs::write(mcp_path(agent), serde_json::to_vec(&original).unwrap()).unwrap();

        // Still on a runtime without built-in MCP: nothing moves.
        adopt_builtin(&paths).unwrap();
        assert_eq!(read_mcp_root(agent), original);
        assert!(agent.join("mcp-oauth").exists());

        let mcp = paths.package_dir.join("dist/extensions/mcp");
        std::fs::create_dir_all(&mcp).unwrap();
        std::fs::write(mcp.join("index.js"), "").unwrap();
        adopt_builtin(&paths).unwrap();
        assert_eq!(
            read_mcp_root(agent),
            serde_json::json!({
                "settings": {"toolPrefix": "short"},
                "mcpServers": {"docs": {"url": "https://x/mcp", "enabled": false}}
            })
        );
        assert!(!agent.join("mcp-oauth").exists());
        assert!(!agent.join("cypher-mcp.keychain-pass").exists());
        assert!(!agent.join("mcp-cache.json").exists());
        assert!(agent.join("mcp-auth.json").exists(), "Pi's own store stays");
        let before = std::fs::read(mcp_path(agent)).unwrap();
        adopt_builtin(&paths).unwrap();
        assert_eq!(std::fs::read(mcp_path(agent)).unwrap(), before);
    }
}
