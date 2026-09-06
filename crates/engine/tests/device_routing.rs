//! M4b integration: `targetDeviceId` routing — engine A forwards device-addressed RPCs
//! to engine B through B's device-room relay (host relay on B, link cache on A), with a
//! minimal in-memory device-room standing in for the edge DO (route client→host with
//! `from` stamped, host→client by `to`).

// tungstenite's `accept_hdr_async` callback signature fixes the Err type as a full
// `Response` — its size is not ours to shrink.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::server::{
    Request as WsRequest, Response as WsResponse,
};

use cypher_doc::SessionCommandPayload;
use cypher_engine::{EngineCore, HarnessRegistry};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode,
};
use cypher_rpc::{
    DeviceFrameHeader, LinkCache, LinkCacheConfig, StaticToken, decode_device_frame,
    encode_device_frame, methods,
};

// ---------------------------------------------------------------------------
// Minimal in-memory device room (route-only subset of the DO semantics)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RelayState {
    host: Option<mpsc::UnboundedSender<Vec<u8>>>,
    clients: HashMap<String, mpsc::UnboundedSender<Vec<u8>>>,
}

async fn fake_device_room() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind relay");
    let url = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().expect("addr").port()
    );
    let state = Arc::new(Mutex::new(RelayState::default()));
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let mut uri = String::new();
                let Ok(ws) = tokio_tungstenite::accept_hdr_async(
                    stream,
                    |req: &WsRequest, res: WsResponse| {
                        uri = req.uri().to_string();
                        Ok(res)
                    },
                )
                .await
                else {
                    return;
                };
                let query = uri.split_once('?').map(|(_, q)| q).unwrap_or("");
                let is_host = query.contains("role=host");
                let conn_id = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("connId="))
                    .unwrap_or("anon")
                    .to_string();
                let (mut sink, mut ws_stream) = ws.split();
                let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
                {
                    let mut st = state.lock().expect("lock");
                    if is_host {
                        st.host = Some(tx);
                    } else {
                        st.clients.insert(conn_id.clone(), tx);
                    }
                }
                let writer = tokio::spawn(async move {
                    while let Some(bytes) = rx.recv().await {
                        if sink.send(WsMessage::Binary(bytes)).await.is_err() {
                            break;
                        }
                    }
                });
                while let Some(Ok(message)) = ws_stream.next().await {
                    let WsMessage::Binary(bytes) = message else {
                        continue;
                    };
                    let Ok((header, payload)) = decode_device_frame(&bytes) else {
                        break;
                    };
                    let st = state.lock().expect("lock");
                    if is_host {
                        let Some(to) = header.to else { continue };
                        if let Some(client) = st.clients.get(&to) {
                            let stripped = DeviceFrameHeader::new(header.s, header.k);
                            let _ = client
                                .send(encode_device_frame(&stripped, &payload).expect("encode"));
                        }
                    } else if let Some(host) = &st.host {
                        let mut routed = DeviceFrameHeader::new(header.s, header.k);
                        routed.from = Some(conn_id.clone());
                        let _ = host.send(encode_device_frame(&routed, &payload).expect("encode"));
                    }
                }
                writer.abort();
            });
        }
    });
    (url, task)
}

// ---------------------------------------------------------------------------
// Engine fixtures
// ---------------------------------------------------------------------------

/// Instant mock harness so a forwarded QueueCommand fully executes on the target.
struct InstantHarness;

#[async_trait]
impl Harness for InstantHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Instant"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        _request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        Ok(futures::stream::iter([
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "instant-1".into(),
                tools: vec![],
                cwd: "/tmp".into(),
                session_id: "hs-1".into(),
                assistant_message_id: "a-1".into(),
            }),
            Ok(AgentEvent::TextDelta {
                text: "remote reply".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("hs-1".into()),
            }),
        ])
        .boxed())
    }
}

fn registry() -> Arc<HarnessRegistry> {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(InstantHarness));
    Arc::new(registry)
}

fn assemble(dir: &std::path::Path, device_id: &str) -> EngineCore {
    std::fs::create_dir_all(dir).expect("create data dir");
    std::fs::write(dir.join("device-id"), device_id).expect("write device id");
    EngineCore::assemble(dir, registry(), HarnessId::Mock, None).expect("engine assembles")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn target_device_id_routes_over_the_relay() {
    let (relay_url, _relay) = fake_device_room().await;
    let dirs = tempfile::tempdir().expect("tempdir");

    // Engine B hosts its device room on the fake relay.
    let core_b = assemble(&dirs.path().join("b"), "device-b");
    let _host = core_b.start_host_relay(&relay_url);

    // Engine A dials peers through the same relay.
    let core_a = assemble(&dirs.path().join("a"), "device-a");
    let mut link_config =
        LinkCacheConfig::new(relay_url.clone(), Arc::new(StaticToken("test-user".into())));
    link_config.probe_timeout = Duration::from_secs(5);
    core_a.set_links(LinkCache::new(link_config));

    // Seed a transcript on B only — proves reads come from B, not A's (empty) doc.
    let handle_b = core_b.doc_host.open("chat-remote").expect("open chat on B");
    handle_b
        .write_user_message("m-b-1", "hello from B", 1_000)
        .expect("write user message");

    let client = cypher_rpc::memory_client(core_a.rpc_service());

    // Our own id in targetDeviceId: handled locally, no forward.
    let local = client
        .call(
            methods::LIST_HARNESSES,
            serde_json::json!({ "targetDeviceId": "device-a" }),
        )
        .await
        .expect("local list");
    assert!(local.is_array());

    // Unary forward: ListHarnesses answered by B through the relay. (The host relay
    // dials with backoff; retry until its session is up.)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let remote = loop {
        match client
            .call(
                methods::LIST_HARNESSES,
                serde_json::json!({ "targetDeviceId": "device-b" }),
            )
            .await
        {
            Ok(value) => break value,
            Err(err) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "relay never came up: {err}"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    assert!(remote.is_array());

    // The add-space picker's exact call: browse a folder ON B from A's IPC
    // surface (ListFolders + targetDeviceId, relay-forwarded).
    let browse_dir = dirs.path().join("b-folders");
    std::fs::create_dir_all(browse_dir.join("project-x")).expect("browse fixture");
    let listing = client
        .call(
            methods::LIST_FOLDERS,
            serde_json::json!({
                "path": browse_dir.to_string_lossy(),
                "targetDeviceId": "device-b",
            }),
        )
        .await
        .expect("remote ListFolders");
    let names: Vec<&str> = listing["entries"]
        .as_array()
        .expect("entries array")
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(
        names.contains(&"project-x"),
        "remote folder listing must come from B's filesystem: {names:?}"
    );

    // Streaming proxy: WatchDocMessages against B's doc from A's IPC surface.
    let mut stream = client
        .subscribe(
            methods::WATCH_DOC_MESSAGES,
            serde_json::json!({ "chatId": "chat-remote", "targetDeviceId": "device-b" }),
        )
        .await
        .expect("remote subscribe");
    // The watch emits its current value first ([] if B's publish pass hasn't run yet),
    // then re-emits on every doc change — read until B's entry arrives.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let item = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("remote transcript before timeout")
            .expect("stream alive");
        if item.to_string().contains("hello from B") {
            break;
        }
    }

    // Unary forward with side effects: QueueCommand lands (and executes) on B.
    let command = serde_json::to_value(SessionCommandPayload::Run {
        request: RunRequest {
            prompt: "run remotely".into(),
            harness: None,
            model: None,
            reasoning: None,
            model_options: serde_json::Map::new(),
            cwd: "/tmp".into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            attachments: Vec::new(),
            pending_attachments: Vec::new(),
            resume: None,
            worktree: None,
        },
        message_id: "m-a-1".into(),

        agent_prompt: None,
    })
    .expect("serialize command");
    let queued = client
        .call(
            methods::QUEUE_COMMAND,
            serde_json::json!({
                "chatId": "chat-remote",
                "targetDeviceId": "device-b",
                "command": command,
            }),
        )
        .await
        .expect("queue on B");
    let command_id = queued["commandId"]
        .as_str()
        .expect("command id")
        .to_string();
    let commands = handle_b.doc().read_commands().expect("read B commands");
    assert!(
        commands.iter().any(|c| c.id == command_id),
        "command must live in B's doc"
    );

    core_a.shutdown().await;
    core_b.shutdown().await;
}

/// M5: terminals are device-addressable — OpenTerminal/WriteTerminal forward as
/// unary calls and SubscribeTerminal proxies its stream through the relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_stream_proxies_over_the_relay() {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;

    let (relay_url, _relay) = fake_device_room().await;
    let dirs = tempfile::tempdir().expect("tempdir");
    let cwd = dirs.path().join("work");
    std::fs::create_dir_all(&cwd).expect("cwd");

    // Engine B hosts its device room; its chat row (via its space) pins the
    // terminal cwd.
    let core_b = assemble(&dirs.path().join("b"), "device-b");
    core_b
        .workspace
        .create_space(
            "space-term",
            "device-b",
            &cwd.to_string_lossy(),
            None,
            false,
        )
        .expect("space row on B");
    core_b
        .workspace
        .create_chat("chat-term", Some("space-term"), None, None, None)
        .expect("chat row on B");
    let _host = core_b.start_host_relay(&relay_url);

    let core_a = assemble(&dirs.path().join("a"), "device-a");
    let mut link_config =
        LinkCacheConfig::new(relay_url.clone(), Arc::new(StaticToken("test-user".into())));
    link_config.probe_timeout = Duration::from_secs(5);
    core_a.set_links(LinkCache::new(link_config));
    let client = cypher_rpc::memory_client(core_a.rpc_service());

    // OpenTerminal forwards to B once the relay session is up.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let session = loop {
        match client
            .call(
                methods::OPEN_TERMINAL,
                serde_json::json!({
                    "chatId": "chat-term",
                    "cols": 80,
                    "rows": 24,
                    "targetDeviceId": "device-b",
                }),
            )
            .await
        {
            Ok(session) => break session,
            Err(err) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "relay never came up: {err}"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    let terminal_id = session["id"].as_str().expect("terminal id").to_string();
    assert_eq!(
        session["cwd"].as_str(),
        Some(&*cwd.to_string_lossy()),
        "cwd from B's chat row"
    );

    // SubscribeTerminal: the stream is proxied item-by-item through the relay.
    let mut stream = client
        .subscribe(
            methods::SUBSCRIBE_TERMINAL,
            serde_json::json!({ "terminalId": terminal_id, "targetDeviceId": "device-b" }),
        )
        .await
        .expect("remote subscribe");
    client
        .call(
            methods::WRITE_TERMINAL,
            serde_json::json!({
                "terminalId": terminal_id,
                "data": BASE64.encode("echo r3lay-$((20+2))\n"),
                "targetDeviceId": "device-b",
            }),
        )
        .await
        .expect("remote write");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut transcript = Vec::new();
    loop {
        let item = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("proxied terminal output before timeout")
            .expect("stream alive");
        if item["type"] == "data" {
            let bytes = BASE64
                .decode(item["data"].as_str().expect("data"))
                .expect("valid base64");
            transcript.extend(bytes);
        }
        if String::from_utf8_lossy(&transcript).contains("r3lay-22") {
            break;
        }
    }

    client
        .call(
            methods::CLOSE_TERMINAL,
            serde_json::json!({ "terminalId": terminal_id, "targetDeviceId": "device-b" }),
        )
        .await
        .expect("remote close");

    core_a.shutdown().await;
    core_b.shutdown().await;
}

#[tokio::test]
async fn remote_target_without_links_fails_clearly() {
    let dirs = tempfile::tempdir().expect("tempdir");
    let core = assemble(&dirs.path().join("solo"), "device-solo");
    let client = cypher_rpc::memory_client(core.rpc_service());
    let err = client
        .call(
            methods::LIST_HARNESSES,
            serde_json::json!({ "targetDeviceId": "device-elsewhere" }),
        )
        .await
        .expect_err("offline forward must fail");
    assert!(
        err.to_string().contains("remote routing unavailable"),
        "got: {err}"
    );
    core.shutdown().await;
}

// Distinct catalogs prove each settings page resolves the selected host,
// including when both hosts offer the same harness.
struct DeviceCatalog(&'static str);
#[async_trait]
impl Harness for DeviceCatalog {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }
    fn display_name(&self) -> &str {
        self.0
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![Model {
            id: format!("{}/model", self.0),
            label: self.0.into(),
            description: None,
            reasoning_levels: vec![],
            options: vec![],
        }])
    }
    async fn commands(&self) -> Result<Vec<cypher_proto::SlashCommand>, HarnessError> {
        Ok(vec![cypher_proto::SlashCommand {
            name: format!("{}-command", self.0),
            description: String::new(),
            input_hint: None,
        }])
    }
    async fn run(
        &self,
        _: RunRequest,
        _: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        Ok(futures::stream::empty().boxed())
    }
}

#[cfg(unix)]
fn settings_engine(dir: &std::path::Path, device: &'static str) -> EngineCore {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("device-id"), device).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(DeviceCatalog(device)));
    let core = EngineCore::assemble(dir, Arc::new(registry), HarnessId::Pi, None).unwrap();
    let current = dir.join("pi-runtime/current");
    for folder in ["bin", "pi", "npm"] {
        std::fs::create_dir_all(current.join(folder)).unwrap();
    }
    std::fs::write(
        current.join("runtime.json"),
        r#"{"version":"1","piVersion":"0.85.0","plugins":{}}"#,
    )
    .unwrap();
    std::fs::write(
        current.join("pi/package.json"),
        r#"{"name":"fixture-pi","version":"1"}"#,
    )
    .unwrap();
    std::fs::write(current.join("npm/package.json"), r#"{"private":true}"#).unwrap();
    std::fs::write(current.join("bin/pi"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(current.join("bin/node"), "#!/bin/sh\nexec python3 \"$@\"\n").unwrap();
    for exe in ["bin/pi", "bin/node"] {
        std::fs::set_permissions(current.join(exe), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    // Test-only transport fixture. Observe no secret values; the real Runtime
    // credential lifecycle is covered by provider-service.test.mjs.
    let helper = format!(
        r#"import json,os,sys
p=json.load(sys.stdin)
agent=os.environ["PI_CODING_AGENT_DIR"]
with open(os.path.join(agent,"observed.json"),"w") as f:
    json.dump({{"action":p["action"],"id":p.get("id"),
      "credentialReceived":p.get("apiKey")=="fixture-key",
      "routingStripped":"targetDeviceId" not in p}},f)
print(json.dumps({{"ok":True,"data":{{"providers":[{{
  "id":"{device}","baseUrl":"https://example.com","providerType":"newapi",
  "credentialSaved":p["action"]=="save","state":"unverified","modelCount":1
}}]}}}}))
"#
    );
    std::fs::write(current.join("provider-service.mjs"), helper).unwrap();
    let runtime =
        cypher_engine::pi_runtime::PiRuntimeManager::spawn("http://127.0.0.1:1".into(), dir);
    std::fs::write(runtime.paths().agent_dir.join("mcp.json"), serde_json::to_vec(&serde_json::json!({
        "mcpServers": { format!("{device}-mcp"): {
            "url": "https://user:fixture-secret@example.com/mcp?token=fixture-secret", "auth": false
        }}
    })).unwrap()).unwrap();
    core.set_pi_runtime(runtime);
    core
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_settings_keep_provider_credentials_and_mcp_changes_on_the_target() {
    let (url, relay) = fake_device_room().await;
    let dirs = tempfile::tempdir().unwrap();
    let a_dir = dirs.path().join("a");
    let b_dir = dirs.path().join("b");
    let b = settings_engine(&b_dir, "device-b");
    let _host = b.start_host_relay(&url);
    let a = settings_engine(&a_dir, "device-a");
    let config = LinkCacheConfig::new(url, Arc::new(StaticToken("test-user".into())));
    a.set_links(LinkCache::new(config));
    let client = cypher_rpc::memory_client(a.rpc_service());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if client
            .call(
                methods::LIST_HARNESSES,
                serde_json::json!({"targetDeviceId":"device-b"}),
            )
            .await
            .is_ok()
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "relay did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    for device in ["device-a", "device-b"] {
        let commands = client
            .call(
                methods::LIST_COMMANDS,
                serde_json::json!({"harness":"pi","targetDeviceId":device}),
            )
            .await
            .unwrap();
        assert_eq!(commands[0]["name"], format!("{device}-command"));
        let models = client
            .call(
                methods::LIST_MODELS,
                serde_json::json!({"harness":"pi","targetDeviceId":device}),
            )
            .await
            .unwrap();
        assert_eq!(models[0]["id"], format!("{device}/model"));
        let packages = client
            .call(
                methods::LIST_PI_PACKAGES,
                serde_json::json!({"targetDeviceId":device}),
            )
            .await
            .unwrap();
        assert!(packages.is_object());
    }
    for (method, action) in [
        (methods::LIST_PI_PROVIDERS, "list"),
        (methods::SAVE_PI_PROVIDER, "save"),
        (methods::REFRESH_PI_PROVIDER, "refresh"),
        (methods::LOGOUT_PI_PROVIDER, "logout"),
        (methods::REMOVE_PI_PROVIDER, "remove"),
    ] {
        let mut params = serde_json::json!({"id":"gateway","targetDeviceId":"device-b"});
        if method == methods::SAVE_PI_PROVIDER {
            params["baseUrl"] = "https://example.com".into();
            params["apiKey"] = "fixture-key".into();
        }
        let reply = client.call(method, params).await.unwrap();
        assert_eq!(reply["providers"][0]["id"], "device-b");
        assert!(!reply.to_string().contains("fixture-key"));
        let observed: serde_json::Value = serde_json::from_slice(
            &std::fs::read(b_dir.join("pi-runtime/agent/observed.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(observed["action"], action);
        assert_eq!(observed["routingStripped"], true);
        if action == "save" {
            assert_eq!(observed["credentialReceived"], true);
        }
        assert!(
            !a_dir.join("pi-runtime/agent/observed.json").exists(),
            "remote call touched local provider state"
        );
    }
    let local = client.call(methods::SAVE_PI_PROVIDER, serde_json::json!({
        "id":"local-gateway","baseUrl":"https://example.com","apiKey":"fixture-key","targetDeviceId":"device-a"
    })).await.unwrap();
    assert_eq!(
        local["providers"][0]["id"], "device-a",
        "explicit local routing is accepted"
    );
    let mcp = client
        .call(
            methods::LIST_MCP_SERVERS,
            serde_json::json!({"targetDeviceId":"device-b"}),
        )
        .await
        .unwrap();
    assert_eq!(mcp["servers"][0]["name"], "device-b-mcp");
    assert!(
        !mcp.to_string().contains("fixture-secret"),
        "MCP list exposed URL credentials"
    );
    let changed = client
        .call(
            methods::SET_MCP_SERVER_ENABLED,
            serde_json::json!({"targetDeviceId":"device-b","name":"device-b-mcp","enabled":false}),
        )
        .await
        .unwrap();
    assert_eq!(changed["servers"][0]["enabled"], false);
    let local_mcp = client
        .call(methods::LIST_MCP_SERVERS, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(local_mcp["servers"][0]["enabled"], true);
    let added = client.call(methods::ADD_MCP_SERVERS, serde_json::json!({
        "targetDeviceId":"device-b", "servers":{
            "new-web":{"url":"https://example.com/mcp","auth":"bearer","bearerToken":"fixture-mcp-key"}
        }
    })).await.unwrap();
    assert_eq!(added["servers"].as_array().unwrap().len(), 2);
    assert!(!added.to_string().contains("fixture-mcp-key"));
    let local_after = client
        .call(methods::LIST_MCP_SERVERS, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(local_after["servers"].as_array().unwrap().len(), 1);
    let remote_bytes = std::fs::read(b_dir.join("pi-runtime/agent/mcp.json")).unwrap();
    assert!(
        String::from_utf8(remote_bytes)
            .unwrap()
            .contains("fixture-mcp-key")
    );
    assert!(
        client
            .call(
                methods::ADD_MCP_SERVERS,
                serde_json::json!({
                    "servers":{"missing-target":{"command":"node"}}
                })
            )
            .await
            .is_err()
    );
    let malformed = client
        .call(
            methods::ADD_MCP_SERVERS,
            serde_json::json!({
                "targetDeviceId":"device-b", "servers":{"bad":{"command":{"key":"fixture-mcp-key"}}}
            }),
        )
        .await
        .unwrap_err();
    assert!(!malformed.to_string().contains("fixture-mcp-key"));
    let account = cypher_engine::mcp::oauth_account("new-web");
    for folder in [&a_dir, &b_dir] {
        let oauth = folder.join("pi-runtime/agent/mcp-oauth").join(&account);
        std::fs::create_dir_all(&oauth).unwrap();
        std::fs::write(oauth.join("tokens.json"), "fixture-oauth").unwrap();
    }
    let removed = client
        .call(
            methods::REMOVE_MCP_SERVER,
            serde_json::json!({
                "targetDeviceId":"device-b","name":"new-web"
            }),
        )
        .await
        .unwrap();
    assert_eq!(removed["servers"].as_array().unwrap().len(), 1);
    assert!(
        !b_dir
            .join("pi-runtime/agent/mcp-oauth")
            .join(&account)
            .exists()
    );
    assert!(
        a_dir
            .join("pi-runtime/agent/mcp-oauth")
            .join(&account)
            .join("tokens.json")
            .exists()
    );
    assert!(
        client
            .call(
                methods::REMOVE_MCP_SERVER,
                serde_json::json!({"name":"device-a-mcp"})
            )
            .await
            .is_err()
    );
    let bad = client
        .call(
            methods::SAVE_PI_PROVIDER,
            serde_json::json!({
                "id":"gateway","baseUrl":"https://example.com","apiKey":{"secret":"must-not-echo"},
                "targetDeviceId":"device-b"
            }),
        )
        .await
        .unwrap_err();
    assert!(!bad.to_string().contains("must-not-echo"));
    for target in [
        serde_json::Value::Null,
        serde_json::json!(123),
        serde_json::json!(""),
    ] {
        for method in [
            methods::LIST_PI_PROVIDERS,
            methods::LIST_PI_PACKAGES,
            methods::LIST_MCP_SERVERS,
            methods::LIST_COMMANDS,
        ] {
            assert!(
                client
                    .call(
                        method,
                        serde_json::json!({"targetDeviceId":target,"harness":"pi"})
                    )
                    .await
                    .is_err(),
                "{method} must reject invalid routing instead of falling back locally"
            );
        }
    }
    relay.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn remote_provider_errors_never_fall_back_to_local_and_insecure_relays_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let core = settings_engine(dir.path(), "device-a");
    let client = cypher_rpc::memory_client(core.rpc_service());
    let params = serde_json::json!({
        "id":"gateway","baseUrl":"https://example.com","apiKey":"fixture-key","targetDeviceId":"device-b"
    });
    let error = client
        .call(methods::SAVE_PI_PROVIDER, params.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("remote routing unavailable"));
    core.set_links(LinkCache::new(LinkCacheConfig::new(
        "http://insecure.example.com",
        Arc::new(StaticToken("test-user".into())),
    )));
    let client = cypher_rpc::memory_client(core.rpc_service());
    let error = client
        .call(methods::SAVE_PI_PROVIDER, params)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("HTTPS/WSS"));
    assert!(!dir.path().join("pi-runtime/agent/observed.json").exists());
    let before = std::fs::read(dir.path().join("pi-runtime/agent/mcp.json")).unwrap();
    let error = client.call(methods::ADD_MCP_SERVERS, serde_json::json!({
        "targetDeviceId":"device-b","servers":{"test":{"command":"node","env":{"KEY":"fixture-mcp-key"}}}
    })).await.unwrap_err();
    assert!(error.to_string().contains("HTTPS/WSS"));
    assert!(!error.to_string().contains("fixture-mcp-key"));
    let error = client
        .call(
            methods::REMOVE_MCP_SERVER,
            serde_json::json!({
                "targetDeviceId":"device-b","name":"device-a-mcp"
            }),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("HTTPS/WSS"));
    assert_eq!(
        std::fs::read(dir.path().join("pi-runtime/agent/mcp.json")).unwrap(),
        before
    );
}
