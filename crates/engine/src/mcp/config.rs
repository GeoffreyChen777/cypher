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

pub fn remove_server(
    paths: &PiRuntimePaths,
    params: RemoveMcpServer,
) -> Result<McpSnapshot, String> {
    params.validate()?;
    let agent_dir = &paths.agent_dir;
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
    // pretend the cross-file operation is an atomic transaction.
    sign_out(agent_dir, &params.name).map_err(|error| format!(
        "{error} Server configuration was kept; some login data may already have been removed. Retry deletion."
    ))?;
    servers.remove(&params.name);
    write_mcp_root(agent_dir, &root).map_err(|_| {
        "OAuth login data was removed, but the MCP configuration could not be saved. Retry deletion.".to_string()
    })?;
    Ok(list(paths))
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

/// Pi's tool namespace: server names that differ only in `-` and `_` clash.
fn namespace(name: &str) -> String {
    name.replace('-', "_")
}

fn exposure(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some("codemode" | "codemode-deferred" | "deferred" | "direct" | "hidden")
    )
}

fn single_line(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.trim().is_empty() && !s.contains(['\0', '\r', '\n']))
}

impl AddMcpServers {
    /// Pi's `mcpServers` shape. The adapter-era fields older viewers still
    /// send (`auth: "oauth" | "bearer" | false`, `bearerToken`,
    /// `bearerTokenEnv`, `disabled`, `requestTimeoutMs`, `oauth.redirectUri`)
    /// are accepted and rewritten on save.
    pub fn validate(&self) -> Result<(), String> {
        if self.servers.is_empty()
            || self.servers.len() > 32
            || serde_json::to_vec(&self.servers).map_or(true, |bytes| bytes.len() > MAX_REQUEST)
        {
            return Err("Add 1–32 MCP servers, with at most 64 KiB of configuration.".into());
        }
        let mut namespaces = std::collections::BTreeSet::new();
        for (name, value) in &self.servers {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                || matches!(name.as_str(), "__proto__" | "constructor" | "prototype")
            {
                return Err(
                    "Server names must use 1–64 letters, digits, underscores or hyphens.".into(),
                );
            }
            if !namespaces.insert(namespace(name)) {
                return Err("Server names that differ only in - and _ count as the same server. Choose distinct names.".into());
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
                "oauth",
                "auth",
                "enabled",
                "timeout",
                "exposure",
                "toolExposure",
                "description",
                "type",
                "bearerToken",
                "bearerTokenEnv",
                "disabled",
                "requestTimeoutMs",
            ];
            if entry.keys().any(|key| !known.contains(&key.as_str())) {
                return Err("Unsupported MCP option. Supported: command, args, env, cwd, url, headers, oauth, auth, enabled, timeout, exposure, toolExposure, description, type.".into());
            }
            let http = entry.contains_key("url");
            if http == entry.contains_key("command") {
                return Err(
                    "Each server needs either an HTTP URL or a stdio command, not both.".into(),
                );
            }
            for key in ["command", "cwd", "url", "bearerToken", "bearerTokenEnv"] {
                if let Some(value) = entry.get(key)
                    && !single_line(value)
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
            let transports: &[&str] = if http {
                &["http", "streamable-http"]
            } else {
                &["stdio"]
            };
            if entry
                .get("type")
                .is_some_and(|t| !t.as_str().is_some_and(|t| transports.contains(&t)))
            {
                return Err("MCP type must be stdio, http or streamable-http. Pi does not support the legacy SSE transport; most servers also serve streamable HTTP, often at /mcp.".into());
            }
            if entry.get("auth").is_some_and(|auth| match auth {
                Value::Object(auth) => {
                    auth.len() != 1 || !auth.get("provider").is_some_and(single_line)
                }
                Value::Bool(false) => false,
                other => !matches!(other.as_str(), Some("oauth" | "bearer")),
            }) {
                return Err("auth must be {\"provider\": \"<Pi provider>\"}; HTTP servers without an Authorization header sign in with OAuth when they require it.".into());
            }
            if ["enabled", "disabled"]
                .iter()
                .any(|key| entry.get(*key).is_some_and(|v| !v.is_boolean()))
                || entry
                    .get("timeout")
                    .is_some_and(|v| !v.as_f64().is_some_and(|t| t > 0.0))
                || entry.get("requestTimeoutMs").is_some_and(|v| !v.is_u64())
            {
                return Err(
                    "enabled must be boolean and timeout a positive number of seconds.".into(),
                );
            }
            if entry.get("exposure").is_some_and(|v| !exposure(v))
                || entry.get("toolExposure").is_some_and(|v| {
                    !v.as_object()
                        .is_some_and(|tools| tools.values().all(exposure))
                })
            {
                return Err("exposure must be codemode, deferred, direct or hidden, and toolExposure map tool names to one of those.".into());
            }
            if entry.get("description").is_some_and(|v| !v.is_string()) {
                return Err("description must be a string.".into());
            }
            if let Some(oauth) = entry.get("oauth") {
                let oauth = oauth
                    .as_object()
                    .ok_or("OAuth options must be an object.")?;
                if oauth.iter().any(|(key, value)| match key.as_str() {
                    "clientId" | "clientSecret" | "scope" | "clientName" => !single_line(value),
                    "callbackPort" => !value.as_u64().is_some_and(|p| (1..=65_535).contains(&p)),
                    "callbackUrl" | "redirectUri" => {
                        !value.as_str().is_some_and(super::legacy::loopback_redirect)
                    }
                    "authServerMetadataUrl" => !value.as_str().is_some_and(|url| {
                        reqwest::Url::parse(url).is_ok_and(|url| {
                            url.scheme() == "https"
                                || (url.scheme() == "http"
                                    && matches!(
                                        url.host_str(),
                                        Some("localhost" | "127.0.0.1" | "[::1]")
                                    ))
                        })
                    }),
                    _ => true,
                }) {
                    return Err("OAuth supports clientId, clientSecret, scope, clientName, callbackPort, a loopback http callbackUrl and an HTTPS authServerMetadataUrl.".into());
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

pub fn add_servers(paths: &PiRuntimePaths, params: AddMcpServers) -> Result<McpSnapshot, String> {
    params.validate()?;
    let agent_dir = &paths.agent_dir;
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
    if params.servers.keys().any(|name| {
        servers
            .keys()
            .any(|existing| namespace(existing) == namespace(name))
    }) {
        return Err("An MCP server with this name already exists. Choose a different name; no servers were added.".into());
    }
    for (name, mut entry) in params.servers {
        super::legacy::to_builtin(entry.as_object_mut().unwrap());
        servers.insert(name, entry);
    }
    if serde_json::to_vec_pretty(&root).map_or(true, |v| v.len() as u64 > MAX_FILE) {
        return Err("The resulting MCP configuration exceeds 1 MiB.".into());
    }
    write_mcp_root(agent_dir, &root)?;
    Ok(list(paths))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(value: Value) -> AddMcpServers {
        serde_json::from_value(serde_json::json!({"servers": value})).unwrap()
    }

    fn runtime() -> (tempfile::TempDir, PiRuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = PiRuntimePaths::for_data_dir(dir.path());
        std::fs::create_dir_all(&paths.agent_dir).unwrap();
        (dir, paths)
    }

    #[test]
    fn deletion_removes_only_the_named_server_and_its_credentials() {
        let (_dir, paths) = runtime();
        let agent = &paths.agent_dir;
        add_servers(
            &paths,
            request(serde_json::json!({
                "remove-me":{"url":"https://example.com/mcp"},
                "keep-me":{"url":"https://example.com/mcp"},
                "local":{"command":"node","env":{"KEY":"fixture-secret"}}
            })),
        )
        .unwrap();
        let signed_in = serde_json::json!({"tokens":{"access_token":"fixture-oauth-secret"}});
        std::fs::write(
            credentials_path(agent),
            serde_json::to_vec(&serde_json::json!({
                "mcp__remove_me|https://example.com/mcp": signed_in,
                "mcp__keep_me|https://example.com/mcp": signed_in,
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(agent.join("unrelated.json"), "keep").unwrap();
        let result = remove_server(
            &paths,
            RemoveMcpServer {
                name: "remove-me".into(),
            },
        )
        .unwrap();
        assert_eq!(
            result
                .servers
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["keep-me", "local"]
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("fixture-secret")
        );
        let credentials = read_credentials(agent);
        assert_eq!(credentials.len(), 1);
        assert!(credentials.contains_key("mcp__keep_me|https://example.com/mcp"));
        assert_eq!(
            std::fs::read(agent.join("unrelated.json")).unwrap(),
            b"keep"
        );
        remove_server(
            &paths,
            RemoveMcpServer {
                name: "local".into(),
            },
        )
        .unwrap();
        assert!(
            remove_server(
                &paths,
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
        let (_dir, paths) = runtime();
        let outside = tempfile::tempdir().unwrap();
        add_servers(
            &paths,
            request(serde_json::json!({"test":{"url":"https://example.com/mcp"}})),
        )
        .unwrap();
        let before = std::fs::read(mcp_path(&paths.agent_dir)).unwrap();
        let external = outside.path().join("mcp-auth.json");
        std::fs::write(&external, r#"{"mcp__test|https://example.com/mcp":{}}"#).unwrap();
        std::os::unix::fs::symlink(&external, credentials_path(&paths.agent_dir)).unwrap();
        let error = remove_server(
            &paths,
            RemoveMcpServer {
                name: "test".into(),
            },
        )
        .err()
        .unwrap();
        assert!(error.contains("configuration was kept"));
        assert_eq!(std::fs::read(mcp_path(&paths.agent_dir)).unwrap(), before);
        assert!(
            std::fs::read_to_string(&external)
                .unwrap()
                .contains("mcp__test")
        );
    }

    #[test]
    fn adds_atomically_preserves_existing_configuration_and_redacts_reply() {
        let (_dir, paths) = runtime();
        let original = serde_json::json!({
            "autoEnableCodemode": true,
            "mcpServers": {"existing": {"command": "existing", "enabled": false}},
        });
        std::fs::write(
            mcp_path(&paths.agent_dir),
            serde_json::to_vec(&original).unwrap(),
        )
        .unwrap();
        let snapshot = add_servers(&paths, request(serde_json::json!({
            "web": {"url":"https://example.com/mcp", "headers": {"Authorization": "Bearer fixture-secret"}},
            "local-tools": {"command":"node", "args":["--token","fixture-secret"],
                "env":{"SECRET":"fixture-secret"}, "type":"stdio"},
        }))).unwrap();
        assert_eq!(snapshot.servers.len(), 3);
        assert!(
            !serde_json::to_string(&snapshot)
                .unwrap()
                .contains("fixture-secret")
        );
        let saved = read_for_update(&paths.agent_dir).unwrap();
        assert_eq!(saved["autoEnableCodemode"], true);
        assert_eq!(
            saved["mcpServers"]["existing"],
            original["mcpServers"]["existing"]
        );
        assert_eq!(
            saved["mcpServers"]["local-tools"]["env"]["SECRET"],
            "fixture-secret"
        );
        let before = std::fs::read(mcp_path(&paths.agent_dir)).unwrap();
        for duplicate in [
            serde_json::json!({"existing":{"command":"replacement"}, "another":{"command":"new"}}),
            // Pi treats `-` and `_` alike in server names.
            serde_json::json!({"local_tools":{"command":"node"}}),
        ] {
            assert!(add_servers(&paths, request(duplicate)).is_err());
        }
        assert!(
            add_servers(
                &paths,
                request(serde_json::json!({"a-b":{"command":"x"}, "a_b":{"command":"y"}}))
            )
            .is_err()
        );
        assert_eq!(std::fs::read(mcp_path(&paths.agent_dir)).unwrap(), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(mcp_path(&paths.agent_dir))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn adapter_fields_from_older_viewers_are_saved_in_pi_shape() {
        let (_dir, paths) = runtime();
        add_servers(
            &paths,
            request(serde_json::json!({
                "web": {"url":"https://example.com/mcp", "auth":"bearer", "bearerToken":"fixture-secret"},
                "docs": {"url":"https://example.com/docs", "auth":"oauth"},
                "open": {"url":"https://example.com/open", "auth":false},
            })),
        )
        .unwrap();
        let saved = read_for_update(&paths.agent_dir).unwrap();
        assert_eq!(
            saved["mcpServers"],
            serde_json::json!({
                "web": {"url":"https://example.com/mcp", "headers": {"Authorization": "Bearer fixture-secret"}},
                "docs": {"url":"https://example.com/docs"},
                "open": {"url":"https://example.com/open"},
            })
        );
    }

    #[test]
    fn invalid_existing_json_is_not_replaced() {
        let (_dir, paths) = runtime();
        for bytes in [
            b"invalid fixture-secret".as_slice(),
            b"[]",
            b"{\"mcpServers\":[]}",
        ] {
            std::fs::write(mcp_path(&paths.agent_dir), bytes).unwrap();
            let error = add_servers(
                &paths,
                request(serde_json::json!({"test":{"command":"node"}})),
            )
            .err()
            .unwrap();
            assert!(!error.contains("fixture-secret"));
            assert_eq!(std::fs::read(mcp_path(&paths.agent_dir)).unwrap(), bytes);
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
            serde_json::json!({"url":"https://example.com", "auth":"fixture-secret"}),
            serde_json::json!({"url":"https://example.com", "auth":{"provider":"x", "token":"fixture-secret"}}),
            serde_json::json!({"url":"https://example.com", "oauth":{"clientSecret":"fixture-secret\n"}}),
            serde_json::json!({"url":"https://example.com", "oauth":{"callbackUrl":"https://fixture-secret.example/cb"}}),
            serde_json::json!({"url":"https://example.com", "oauth":{"grantType":"client_credentials"}}),
            serde_json::json!({"url":"https://example.com", "type":"sse"}),
            serde_json::json!({"url":"https://example.com", "exposure":"fixture-secret"}),
            serde_json::json!({"url":"https://example.com", "timeout":0}),
            serde_json::json!({"url":"https://example.com", "unsupported":"fixture-secret"}),
        ] {
            let error = request(serde_json::json!({"test":entry}))
                .validate()
                .unwrap_err();
            assert!(!error.contains("fixture-secret"));
        }
        for name in ["", "../path", "dotted.name", "__proto__", "constructor"] {
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
    fn validates_pi_options_and_loopback() {
        for entry in [
            serde_json::json!({"url":"http://127.0.0.1:3456/mcp"}),
            serde_json::json!({"url":"https://example.com/mcp", "oauth":{"clientId":"public-client",
                "callbackUrl":"http://localhost:8976/callback", "scope":"read offline_access"}}),
            serde_json::json!({"url":"https://example.com/mcp", "oauth":{"callbackPort":8765,
                "clientName":"Claude Code", "authServerMetadataUrl":"https://auth.example/.well-known/openid-configuration"}}),
            serde_json::json!({"url":"https://example.com/mcp", "headers":{"Authorization":"Bearer ${TOKEN}"},
                "exposure":"deferred", "toolExposure":{"search":"direct","delete_*":"hidden"},
                "description":"Docs", "timeout":30, "enabled":false, "type":"http"}),
            serde_json::json!({"url":"https://example.com/mcp", "auth":{"provider":"github"}}),
            serde_json::json!({"command":"/some path/server","args":["--option","value"],"env":{"KEY":"value"},"cwd":"/work"}),
        ] {
            request(serde_json::json!({"test":entry}))
                .validate()
                .unwrap();
        }
    }

    #[test]
    fn concurrent_additions_do_not_lose_other_servers() {
        let (_dir, paths) = runtime();
        std::thread::scope(|scope| {
            for name in ["first", "second"] {
                let paths = &paths;
                scope.spawn(move || {
                    add_servers(paths, request(serde_json::json!({name:{"command":"node"}})))
                        .unwrap()
                });
            }
        });
        assert_eq!(list(&paths).servers.len(), 2);
    }
}
