//! Scoped MCP configuration writes. Never return submitted secrets or
//! overwrite an existing server, malformed file, or unrelated root settings.
use super::*;
use std::collections::BTreeMap;

const MAX_REQUEST: usize = 65_536;
const MAX_FILE: u64 = 1_048_576;

// Intentionally not Debug: entries can contain credentials.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddMcpServers {
    pub servers: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveMcpServer {
    pub name: String,
}

impl RemoveMcpServer {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.len() > 512 || self.name.contains('\0') {
            return Err("Select a valid MCP server to delete.".into());
        }
        Ok(())
    }
}

fn checked_path(path: &Path, directory: bool) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Ok(meta)
            if !meta.file_type().is_symlink()
                && if directory {
                    meta.is_dir()
                } else {
                    meta.is_file()
                } =>
        {
            Ok(true)
        }
        _ => Err("Cypher OAuth storage is unavailable or contains a symlink.".into()),
    }
}

#[cfg(target_os = "macos")]
fn delete_private_keychain_entry(path: &Path, account: &str) -> Result<(), String> {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    // Never search/delete from the login keychain or create a keychain here.
    // No token or keychain password is placed in argv or captured output.
    let mut child = Command::new("/usr/bin/security")
        .args([
            "delete-generic-password",
            "-s",
            "pi-mcp-adapter.oauth",
            "-a",
            account,
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Could not open Cypher's OAuth credential store.")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() || status.code() == Some(44) => return Ok(()),
            Ok(Some(_)) => {
                return Err(
                    "Could not remove Cypher's OAuth credential. Unlock its keychain and retry."
                        .into(),
                );
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Removing Cypher's OAuth credential timed out or failed. Retry after unlocking its keychain.".into());
            }
        }
    }
}

fn remove_private_oauth(agent_dir: &Path, name: &str) -> Result<(), String> {
    let account = oauth_account(name);
    let oauth_root = agent_dir.join("mcp-oauth");
    let has_root = checked_path(&oauth_root, true)?;
    let account_path = oauth_root.join(&account);
    let has_account = has_root && checked_path(&account_path, true)?;
    let keychain = app_keychain_path(agent_dir);
    let has_keychain = checked_path(&keychain, false)?;
    #[cfg(target_os = "macos")]
    if has_keychain {
        delete_private_keychain_entry(&keychain, &account)?;
    }
    #[cfg(not(target_os = "macos"))]
    if has_keychain {
        return Err(
            "A macOS Cypher OAuth keychain is present and cannot be cleared on this platform."
                .into(),
        );
    }
    if has_account {
        std::fs::remove_dir_all(account_path)
            .map_err(|_| "Could not remove Cypher's OAuth token files.")?;
    }
    Ok(())
}

pub fn remove_server(agent_dir: &Path, params: RemoveMcpServer) -> Result<McpSnapshot, String> {
    params.validate()?;
    let _guard = CONFIG_WRITE
        .lock()
        .map_err(|_| "MCP configuration is busy.")?;
    let mut root = read_for_update(agent_dir)?;
    let servers = root
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .ok_or("MCP server is no longer configured. Refresh the list.")?;
    if !servers.contains_key(&params.name) {
        return Err("MCP server is no longer configured. Refresh the list.".into());
    }
    // Keep the configuration on cleanup failure, so the user can retry. Never
    // pretend the cross-file/keychain operation is an atomic transaction.
    remove_private_oauth(agent_dir, &params.name).map_err(|error| format!(
        "{error} Server configuration was kept; some login data may already have been removed. Retry deletion."
    ))?;
    servers.remove(&params.name);
    write_mcp_root(agent_dir, &root).map_err(|_| {
        "OAuth login data was removed, but the MCP configuration could not be saved. Retry deletion.".to_string()
    })?;
    Ok(list(agent_dir))
}

fn string_map(value: &Value) -> bool {
    value.as_object().is_some_and(|map| {
        map.iter().all(|(key, value)| {
            !key.is_empty()
                && !key.contains(['\0', '\r', '\n'])
                && value.as_str().is_some_and(|s| !s.contains('\0'))
        })
    })
}

impl AddMcpServers {
    pub fn validate(&self) -> Result<(), String> {
        if self.servers.is_empty()
            || self.servers.len() > 32
            || serde_json::to_vec(&self.servers).map_or(true, |bytes| bytes.len() > MAX_REQUEST)
        {
            return Err("Add 1–32 MCP servers, with at most 64 KiB of configuration.".into());
        }
        for (name, value) in &self.servers {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                || matches!(name.as_str(), "__proto__" | "constructor" | "prototype")
            {
                return Err(
                    "Server names must use 1–64 letters, digits, dots, underscores or hyphens."
                        .into(),
                );
            }
            let entry = value
                .as_object()
                .ok_or("Each MCP server must be a JSON object.")?;
            let known = [
                "command",
                "args",
                "env",
                "cwd",
                "url",
                "headers",
                "auth",
                "bearerToken",
                "bearerTokenEnv",
                "oauth",
                "disabled",
                "lifecycle",
                "protocolVersion",
                "requestTimeoutMs",
                "type",
            ];
            if entry.keys().any(|key| !known.contains(&key.as_str())) {
                return Err("Unsupported MCP option. Supported: command, args, env, cwd, url, headers, auth, bearerToken, bearerTokenEnv, oauth, disabled, lifecycle, protocolVersion, requestTimeoutMs, type.".into());
            }
            let http = entry.contains_key("url");
            if http == entry.contains_key("command") {
                return Err(
                    "Each server needs either an HTTP URL or a stdio command, not both.".into(),
                );
            }
            for key in ["command", "cwd", "url", "bearerToken", "bearerTokenEnv"] {
                if let Some(value) = entry.get(key)
                    && !value
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty() && !s.contains(['\0', '\r', '\n']))
                {
                    return Err("MCP command, URL, path and token fields must be non-empty single-line strings.".into());
                }
            }
            if let Some(args) = entry.get("args")
                && !args.as_array().is_some_and(|args| {
                    args.iter()
                        .all(|arg| arg.as_str().is_some_and(|s| !s.contains('\0')))
                })
            {
                return Err("Arguments must be a JSON array of strings.".into());
            }
            for key in ["env", "headers"] {
                if let Some(value) = entry.get(key)
                    && !string_map(value)
                {
                    return Err(
                        "Environment variables and headers must be JSON objects of strings.".into(),
                    );
                }
            }
            if let Some(headers) = entry.get("headers").and_then(Value::as_object)
                && headers.iter().any(|(key, value)| {
                    reqwest::header::HeaderName::from_bytes(key.as_bytes()).is_err()
                        || reqwest::header::HeaderValue::from_str(value.as_str().unwrap_or(""))
                            .is_err()
                })
            {
                return Err("Invalid HTTP header name or value.".into());
            }
            if http {
                let url = reqwest::Url::parse(entry["url"].as_str().unwrap_or(""))
                    .map_err(|_| "Enter a valid HTTPS MCP URL.")?;
                let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
                if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
                    return Err(
                        "MCP URLs require HTTPS; HTTP is allowed only for loopback hosts.".into(),
                    );
                }
                if url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err("Use a URL without credentials, query or fragment. Put authentication in the token or headers fields.".into());
                }
                if ["args", "env", "cwd"]
                    .iter()
                    .any(|key| entry.contains_key(*key))
                {
                    return Err(
                        "Arguments, environment and working directory apply only to stdio servers."
                            .into(),
                    );
                }
            } else if ["headers", "auth", "bearerToken", "bearerTokenEnv", "oauth"]
                .iter()
                .any(|key| entry.contains_key(*key))
            {
                return Err(
                    "HTTP authentication fields cannot be used with a stdio command.".into(),
                );
            }
            for (key, allowed) in [
                ("auth", &["bearer", "oauth"][..]),
                (
                    "lifecycle",
                    &["lazy", "eager", "keep-alive", "lazy-keep-alive"][..],
                ),
                ("protocolVersion", &["legacy", "auto", "2026-07-28"][..]),
                (
                    "type",
                    if http {
                        &["http", "sse", "streamable-http"][..]
                    } else {
                        &["stdio"][..]
                    },
                ),
            ] {
                if let Some(value) = entry.get(key)
                    && !(key == "auth" && value == &Value::Bool(false))
                    && !value.as_str().is_some_and(|s| allowed.contains(&s))
                {
                    return Err(
                        "Invalid MCP authentication, lifecycle, protocol or transport option."
                            .into(),
                    );
                }
            }
            if entry.get("disabled").is_some_and(|v| !v.is_boolean())
                || entry.get("requestTimeoutMs").is_some_and(|v| !v.is_u64())
            {
                return Err(
                    "disabled must be boolean and requestTimeoutMs a non-negative integer.".into(),
                );
            }
            if let Some(oauth) = entry.get("oauth") {
                let oauth = oauth
                    .as_object()
                    .ok_or("OAuth options must be an object.")?;
                if entry.get("auth").and_then(Value::as_str) != Some("oauth")
                    || oauth.iter().any(|(key, value)| {
                        ![
                            "clientId",
                            "clientSecret",
                            "scope",
                            "redirectUri",
                            "grantType",
                        ]
                        .contains(&key.as_str())
                            || !value
                                .as_str()
                                .is_some_and(|s| !s.contains(['\0', '\r', '\n']))
                    })
                {
                    return Err("OAuth supports clientId, clientSecret, scope, redirectUri and grantType, with auth set to oauth.".into());
                }
                if oauth.get("grantType").is_some_and(|v| {
                    !matches!(
                        v.as_str(),
                        Some("authorization_code" | "client_credentials")
                    )
                }) {
                    return Err(
                        "OAuth grantType must be authorization_code or client_credentials.".into(),
                    );
                }
            }
            let bearer = entry.get("auth").and_then(Value::as_str) == Some("bearer");
            let token = entry.contains_key("bearerToken");
            let token_env = entry.contains_key("bearerTokenEnv");
            if (bearer && token == token_env) || (!bearer && (token || token_env)) {
                return Err("Bearer authentication needs exactly one of bearerToken or bearerTokenEnv, with auth set to bearer.".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(value: Value) -> AddMcpServers {
        serde_json::from_value(serde_json::json!({"servers": value})).unwrap()
    }

    #[test]
    fn deletion_removes_only_the_named_server_and_its_private_oauth_files() {
        let dir = tempfile::tempdir().unwrap();
        add_servers(
            dir.path(),
            request(serde_json::json!({
                "remove-me":{"command":"node","env":{"KEY":"fixture-secret"}},
                "keep-me":{"command":"other"}
            })),
        )
        .unwrap();
        let oauth = dir.path().join("mcp-oauth");
        for name in ["remove-me", "keep-me"] {
            let folder = oauth.join(oauth_account(name));
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("tokens.json"), "fixture-oauth-secret").unwrap();
        }
        std::fs::write(dir.path().join("unrelated.json"), "keep").unwrap();
        let result = remove_server(
            dir.path(),
            RemoveMcpServer {
                name: "remove-me".into(),
            },
        )
        .unwrap();
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.servers[0].name, "keep-me");
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("fixture-secret")
        );
        assert!(!oauth.join(oauth_account("remove-me")).exists());
        assert!(
            oauth
                .join(oauth_account("keep-me"))
                .join("tokens.json")
                .exists()
        );
        assert_eq!(
            std::fs::read(dir.path().join("unrelated.json")).unwrap(),
            b"keep"
        );
        assert!(
            !app_keychain_path(dir.path()).exists(),
            "deletion must not create a keychain"
        );
        assert!(
            remove_server(
                dir.path(),
                RemoveMcpServer {
                    name: "missing".into()
                }
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn deletion_refuses_credential_symlinks_and_keeps_configuration_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        add_servers(
            dir.path(),
            request(serde_json::json!({"test":{"command":"node"}})),
        )
        .unwrap();
        let before = std::fs::read(mcp_path(dir.path())).unwrap();
        let external = outside.path().join(oauth_account("test"));
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(external.join("tokens.json"), "do-not-delete").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("mcp-oauth")).unwrap();
        let error = remove_server(
            dir.path(),
            RemoveMcpServer {
                name: "test".into(),
            },
        )
        .err()
        .unwrap();
        assert!(error.contains("configuration was kept"));
        assert_eq!(std::fs::read(mcp_path(dir.path())).unwrap(), before);
        assert_eq!(
            std::fs::read(external.join("tokens.json")).unwrap(),
            b"do-not-delete"
        );
    }

    #[test]
    fn adds_atomically_preserves_existing_configuration_and_redacts_reply() {
        let dir = tempfile::tempdir().unwrap();
        let original = serde_json::json!({
            "settings": {"toolPrefix": "short"},
            "mcpServers": {"existing": {"command": "existing", "disabled": true}},
        });
        std::fs::write(mcp_path(dir.path()), serde_json::to_vec(&original).unwrap()).unwrap();
        let snapshot = add_servers(dir.path(), request(serde_json::json!({
            "web": {"url":"https://example.com/mcp", "auth":"bearer", "bearerToken":"fixture-secret"},
            "local": {"command":"node", "args":["--token","fixture-secret"],
                "env":{"SECRET":"fixture-secret"}, "type":"stdio"},
        }))).unwrap();
        assert_eq!(snapshot.servers.len(), 3);
        assert!(
            !serde_json::to_string(&snapshot)
                .unwrap()
                .contains("fixture-secret")
        );
        let saved = read_for_update(dir.path()).unwrap();
        assert_eq!(saved["settings"], original["settings"]);
        assert_eq!(
            saved["mcpServers"]["existing"],
            original["mcpServers"]["existing"]
        );
        assert_eq!(
            saved["mcpServers"]["local"]["env"]["SECRET"],
            "fixture-secret"
        );
        let before = std::fs::read(mcp_path(dir.path())).unwrap();
        assert!(
            add_servers(
                dir.path(),
                request(serde_json::json!({
                    "existing":{"command":"replacement"}, "another":{"command":"new"}
                }))
            )
            .is_err()
        );
        assert_eq!(std::fs::read(mcp_path(dir.path())).unwrap(), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(mcp_path(dir.path()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_existing_json_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        for bytes in [
            b"invalid fixture-secret".as_slice(),
            b"[]",
            b"{\"mcpServers\":[]}",
        ] {
            std::fs::write(mcp_path(dir.path()), bytes).unwrap();
            let error = add_servers(
                dir.path(),
                request(serde_json::json!({"test":{"command":"node"}})),
            )
            .err()
            .unwrap();
            assert!(!error.contains("fixture-secret"));
            assert_eq!(std::fs::read(mcp_path(dir.path())).unwrap(), bytes);
        }
    }

    #[test]
    fn invalid_entries_are_rejected_without_echoing_values() {
        for entry in [
            serde_json::json!({"url":"http://example.com/fixture-secret"}),
            serde_json::json!({"url":"https://example.com/?token=fixture-secret"}),
            serde_json::json!({"url":"https://fixture-secret@example.com/"}),
            serde_json::json!({"url":"https://example.com", "command":"fixture-secret"}),
            serde_json::json!({"command":"node", "args":"fixture-secret"}),
            serde_json::json!({"command":"node", "env":{"X":123}}),
            serde_json::json!({"url":"https://example.com", "headers":{"X":"fixture-secret\nbad"}}),
            serde_json::json!({"url":"https://example.com", "auth":"bearer"}),
            serde_json::json!({"url":"https://example.com", "oauth":{"clientSecret":"fixture-secret"}}),
            serde_json::json!({"url":"https://example.com", "unsupported":"fixture-secret"}),
        ] {
            let error = request(serde_json::json!({"test":entry}))
                .validate()
                .unwrap_err();
            assert!(!error.contains("fixture-secret"));
        }
        for name in ["", "../path", "__proto__", "constructor"] {
            assert!(
                request(serde_json::json!({name: {"command":"node"}}))
                    .validate()
                    .is_err()
            );
        }
        assert!(request(serde_json::json!({})).validate().is_err());
        assert!(
            request(serde_json::json!({"test":{"command":"node", "args":["x".repeat(65_537)]}}))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn validates_http_stdio_oauth_and_loopback() {
        for entry in [
            serde_json::json!({"url":"http://127.0.0.1:3456/mcp", "auth":false}),
            serde_json::json!({"url":"https://example.com/mcp", "auth":"oauth", "oauth":{"clientId":"public-client"}}),
            serde_json::json!({"url":"https://example.com/mcp", "headers":{"Authorization":"Bearer fixture-secret"}}),
            serde_json::json!({"command":"/some path/server","args":["--option","value"],"env":{"KEY":"value"},"cwd":"/work"}),
        ] {
            request(serde_json::json!({"test":entry}))
                .validate()
                .unwrap();
        }
    }

    #[test]
    fn concurrent_additions_do_not_lose_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for name in ["first", "second"] {
                let path = dir.path();
                scope.spawn(move || {
                    add_servers(path, request(serde_json::json!({name:{"command":"node"}})))
                        .unwrap()
                });
            }
        });
        assert_eq!(list(dir.path()).servers.len(), 2);
    }
}

pub(super) fn read_for_update(agent_dir: &Path) -> Result<Value, String> {
    let path = mcp_path(agent_dir);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(serde_json::json!({})),
        Err(_) => return Err("Could not read the existing MCP configuration.".into()),
    };
    if !metadata.is_file() || metadata.len() > MAX_FILE {
        return Err("MCP configuration must be a regular file no larger than 1 MiB.".into());
    }
    let bytes =
        std::fs::read(path).map_err(|_| "Could not read the existing MCP configuration.")?;
    let root: Value = serde_json::from_slice(&bytes).map_err(
        |_| "Existing mcp.json is invalid. Repair it before adding servers; it was not modified.",
    )?;
    if !root.is_object() || root.get("mcpServers").is_some_and(|v| !v.is_object()) {
        return Err(
            "Existing mcp.json must contain an object with an optional mcpServers object.".into(),
        );
    }
    Ok(root)
}

pub fn add_servers(agent_dir: &Path, params: AddMcpServers) -> Result<McpSnapshot, String> {
    params.validate()?;
    let _guard = CONFIG_WRITE
        .lock()
        .map_err(|_| "MCP configuration is busy.")?;
    let mut root = read_for_update(agent_dir)?;
    let servers = root
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .unwrap();
    if params.servers.keys().any(|name| servers.contains_key(name)) {
        return Err("An MCP server with this name already exists. Choose a different name; no servers were added.".into());
    }
    for (name, mut entry) in params.servers {
        // The adapter chooses its transport from command/url, not type.
        entry.as_object_mut().unwrap().remove("type");
        servers.insert(name, entry);
    }
    if serde_json::to_vec_pretty(&root).map_or(true, |v| v.len() as u64 > MAX_FILE) {
        return Err("The resulting MCP configuration exceeds 1 MiB.".into());
    }
    write_mcp_root(agent_dir, &root)?;
    Ok(list(agent_dir))
}
