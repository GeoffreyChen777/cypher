use super::*;
use chrono::TimeDelta;
use cypher_engine::{EngineCore, default_registry};
// `SessionStatus` is only needed to build the fixtures below — the module
// itself derives everything through `cypher_proto::view`.
use cypher_proto::view::{group_chats, project_label};
use cypher_proto::{SessionStatus, UserProfile};

/// An engine that predates `EngineInfo`: it serves no identity method.
struct LegacyIdentityRpc;

#[async_trait]
impl RpcService for LegacyIdentityRpc {
    async fn handle(&self, method: &str, _params: serde_json::Value) -> Result<RpcReply, RpcError> {
        Err(RpcError::UnknownMethod(method.into()))
    }
}

struct DeferredIdentityRpc {
    engine_info: EngineInfo,
    state: tokio::sync::watch::Receiver<DeferredEngineState>,
}

#[async_trait]
impl RpcService for DeferredIdentityRpc {
    async fn handle(&self, method: &str, _params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            methods::ENGINE_INFO => RpcReply::value(&self.engine_info),
            methods::ENGINE_READY => {
                let mut state = self.state.clone();
                wait_for_deferred_engine(&mut state)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&serde_json::json!({ "ready": true }))
            }
            other => Err(RpcError::UnknownMethod(other.into())),
        }
    }
}

#[tokio::test]
async fn legacy_identity_is_rejected() {
    let client = memory_client(Arc::new(LegacyIdentityRpc));
    assert!(query_engine_info(&client).await.is_err());
}

#[tokio::test]
async fn wrong_device_identity_is_rejected_without_creating_a_second_engine() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("device-id"), "expected-device").unwrap();
    let socket = cypher_env::ipc_socket(dir.path()).unwrap();
    let listener = cypher_rpc::LocalListener::bind(&socket).await.unwrap();
    let (_tx, state) = tokio::sync::watch::channel(DeferredEngineState::Ready);
    let server = tokio::spawn(listener.serve(Arc::new(DeferredIdentityRpc {
        engine_info: EngineInfo {
            device_id: "different-device".into(),
            workspace_scope: WorkspaceScope::Local,
        },
        state,
    })));
    let result = EngineHandle::bootstrap(EngineBootConfig {
        data_dir: dir.path().into(),
        ipc_socket: socket.clone(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    })
    .await;
    assert!(result.is_err());
    assert!(cypher_rpc::probe_local(&socket).await.unwrap());
    assert!(!dir.path().join("profiles").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("device-id")).unwrap(),
        "expected-device"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn bootstrap_embeds_engine_when_port_is_free() {
    let dir = tempfile::tempdir().unwrap();
    let handle = EngineHandle::bootstrap(EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_socket: cypher_env::ipc_socket(dir.path()).unwrap(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None, // offline
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    })
    .await
    .unwrap();
    assert_eq!(handle.mode(), EngineMode::InProcess);
    assert!(matches!(
        handle
            .deferred_state()
            .expect("embedded lifecycle")
            .borrow()
            .clone(),
        DeferredEngineState::Ready
    ));
    // Same protocol over the in-memory transport: a real engine answers.
    let harnesses = handle
        .client()
        .call(methods::LIST_HARNESSES, serde_json::json!({}))
        .await
        .unwrap();
    assert!(harnesses.as_array().is_some_and(|h| !h.is_empty()));
    handle.shutdown().await;
}

#[tokio::test]
async fn local_only_runtime_serves_update_status() {
    // Regression (0.1.0): the release checker used to be attached only for
    // edge-enabled runtimes, so a fresh local-only profile had no Updater —
    // `UpdateStatus` errored, the UI's subscription closed instantly, and the
    // generic watch re-subscribed every 2s forever. The updater must exist
    // for every profile (release endpoints are public, updates are
    // device-local); only the token-change wake is edge-gated.
    let dir = tempfile::tempdir().unwrap();
    let handle = EngineHandle::bootstrap(EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_socket: cypher_env::ipc_socket(dir.path()).unwrap(),
        // Unreachable — the 20s initial check is never reached inside this
        // test, so no real network happens.
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None, // offline
        org_id: None,
        workos_client_id: Some("client_test".into()), // signed out → Local
        default_harness: HarnessId::Mock,
    })
    .await
    .unwrap();
    assert_eq!(handle.engine_info().workspace_scope, WorkspaceScope::Local);

    // The RPC is served by the real assembled engine (in-memory transport).
    let mut rx = handle
        .client()
        .subscribe(methods::UPDATE_STATUS, serde_json::json!({}))
        .await
        .expect("a local-only runtime must serve UpdateStatus");

    // Immediate initial frame: current version, no update yet.
    let initial = rx
        .recv()
        .await
        .expect("initial UpdateStatus frame must arrive immediately");
    let status: cypher_update::UpdateStatus = serde_json::from_value(initial).unwrap();
    assert_eq!(status.current_version, cypher_update::current_version());
    assert!(!status.update_available);

    // The stream stays open (no frame, no close) before the 20s check.
    let still_open = tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv()).await;
    assert!(
        still_open.is_err(),
        "UpdateStatus stream must remain open, got: {still_open:?}"
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn bootstrap_reports_local_assembly_failure_before_returning_a_handle() {
    let dir = tempfile::tempdir().unwrap();
    cypher_engine::EngineProfile::local(dir.path()).unwrap();
    std::fs::create_dir(dir.path().join("profiles")).unwrap();
    std::fs::write(dir.path().join("profiles/local"), b"not a directory").unwrap();
    let port = cypher_env::ipc_socket(dir.path()).unwrap();

    let error = match EngineHandle::bootstrap(EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_socket: port.clone(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: Some("client_test".into()),
        default_harness: HarnessId::Mock,
    })
    .await
    {
        Ok(handle) => {
            handle.shutdown().await;
            panic!("a corrupt local store must fail bootstrap")
        }
        Err(error) => error,
    };

    assert!(!format!("{error:#}").is_empty());
    assert!(
        tokio::net::UnixStream::connect(&port).await.is_err(),
        "failed bootstrap must release the IPC listener"
    );
}

#[tokio::test]
async fn deferred_engine_failure_remains_observable_after_early_attach() {
    let (state_tx, mut state_rx) = tokio::sync::watch::channel(DeferredEngineState::Waiting);
    state_tx.send_replace(DeferredEngineState::Failed("store failed".into()));

    assert_eq!(
        wait_for_deferred_engine(&mut state_rx).await,
        Err("store failed".into())
    );
}

#[tokio::test]
async fn remote_viewport_observes_deferred_engine_failure() {
    let dir = tempfile::tempdir().unwrap();
    let port = cypher_env::ipc_socket(dir.path()).unwrap();
    let listener = cypher_rpc::LocalListener::bind(&port).await.unwrap();
    let (state_tx, state_rx) = tokio::sync::watch::channel(DeferredEngineState::Waiting);
    let server = tokio::spawn(listener.serve(Arc::new(DeferredIdentityRpc {
        engine_info: EngineInfo {
            device_id: "owner-device".into(),
            workspace_scope: WorkspaceScope::Local,
        },
        state: state_rx,
    })));

    std::fs::write(dir.path().join("device-id"), "owner-device").unwrap();
    let handle = EngineHandle::bootstrap(EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_socket: port.clone(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    })
    .await
    .expect("second viewport attaches over IPC");
    assert!(matches!(handle.mode(), EngineMode::Remote { .. }));

    let mut deferred = handle
        .deferred_state()
        .expect("remote viewport tracks engine readiness");
    state_tx.send_replace(DeferredEngineState::Failed("store failed".into()));
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_deferred_engine(&mut deferred),
        )
        .await
        .expect("remote readiness probe completes"),
        Err("store failed".into())
    );

    handle.shutdown().await;
    server.abort();
}

#[tokio::test]
async fn an_embedded_engine_serves_the_ipc_socket_for_other_viewports() {
    // The whole point of embedding-and-serving: a second viewport (the
    // terminal app) can attach to this window's engine with no setup, no
    // separate daemon, and no launch ordering.
    let dir = tempfile::tempdir().unwrap();
    let port = cypher_env::ipc_socket(dir.path()).unwrap();
    let handle = EngineHandle::bootstrap(EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_socket: port.clone(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None, // offline
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    })
    .await
    .unwrap();
    assert_eq!(handle.mode(), EngineMode::InProcess);

    // Attach the way an external viewport would, and speak the same protocol.
    let attached = cypher_rpc::connect_local(&port)
        .await
        .expect("a second viewport must be able to attach");
    let harnesses = attached
        .call(methods::LIST_HARNESSES, serde_json::json!({}))
        .await
        .unwrap();
    assert!(harnesses.as_array().is_some_and(|h| !h.is_empty()));

    // Shutting the window down stops accepting, so the next viewport
    // starts its own engine rather than talking to closing stores.
    handle.shutdown().await;
    assert!(
        tokio::net::UnixStream::connect(&port).await.is_err(),
        "the port must be released on shutdown"
    );
}

#[tokio::test]
async fn concurrent_bootstraps_elect_one_embedded_engine() {
    // Two viewports of one app booting at once (the Local-switch restart
    // path): both used to probe a closed port, both embedded, and one lost
    // the data-dir lock. The bootstrap gate must elect exactly one owner
    // and turn the other into a plain remote attach.
    let dir = tempfile::tempdir().unwrap();
    let port = cypher_env::ipc_socket(dir.path()).unwrap();
    let config = EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_socket: port.clone(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None, // offline
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let (a, b) = tokio::join!(
        EngineHandle::bootstrap(config.clone()),
        EngineHandle::bootstrap(config.clone()),
    );
    let a = a.expect("first viewport boots");
    let b = b.expect("second viewport boots");

    let modes = [a.mode(), b.mode()];
    assert_eq!(
        modes
            .iter()
            .filter(|mode| **mode == EngineMode::InProcess)
            .count(),
        1,
        "exactly one viewport embeds: {modes:?}"
    );
    assert_eq!(
        modes
            .iter()
            .filter(|mode| matches!(mode, EngineMode::Remote { .. }))
            .count(),
        1,
        "the other attaches over IPC: {modes:?}"
    );

    for handle in [&a, &b] {
        let mut deferred = handle.deferred_state().expect("lifecycle tracked");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            wait_for_deferred_engine(&mut deferred),
        )
        .await
        .expect("readiness resolves")
        .expect("both viewports reach Ready");
    }

    b.shutdown().await;
    a.shutdown().await;
}

#[tokio::test]
async fn a_stranger_on_the_ipc_socket_does_not_wedge_the_window() {
    let dir = tempfile::tempdir().unwrap();
    // The port probe only proves *something* is listening. A process that
    // accepts TCP and never speaks WebSocket used to hang the dial forever;
    // now it times out and we embed instead, losing only the ability to
    // serve other viewports.
    let port = cypher_env::ipc_socket(dir.path()).unwrap();
    let squatter = cypher_rpc::LocalListener::bind(&port).await.unwrap();
    let handle = EngineHandle::bootstrap(EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_socket: port.clone(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    })
    .await;
    assert!(handle.is_err(), "a foreign IPC endpoint must fail closed");
    drop(squatter);
}

#[tokio::test]
async fn production_bootstrap_opens_local_data_without_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    let handle = EngineHandle::bootstrap(EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_socket: cypher_env::ipc_socket(dir.path()).unwrap(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: Some("client_test".into()),
        default_harness: HarnessId::Mock,
    })
    .await
    .unwrap();

    assert_eq!(handle.engine_info().workspace_scope, WorkspaceScope::Local);
    let info: EngineInfo = handle
        .client()
        .call_as(methods::ENGINE_INFO, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(info, *handle.engine_info());

    let mut auth = handle
        .client()
        .subscribe(methods::AUTH_STATUS, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(
        parse_auth_state(&auth.recv().await.unwrap()),
        Some(AuthState::SignedOut)
    );
    let harnesses = handle
        .client()
        .call(methods::LIST_HARNESSES, serde_json::json!({}))
        .await
        .expect("local data RPC is immediately available");
    assert!(harnesses.as_array().is_some_and(|items| !items.is_empty()));
    assert!(
        !dir.path().join("orgs/dev-org/dev-user").exists(),
        "production boot must not create dev-user data"
    );
    assert!(dir.path().join("profiles/local").is_dir());
    handle.shutdown().await;
}

#[tokio::test]
async fn engine_info_is_available_while_cloud_onboarding_is_deferred() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("session.json"),
        r#"{"refreshToken":"saved","user":{"id":"user_1","email":"u@example.com"}}"#,
    )
    .unwrap();
    let handle = EngineHandle::bootstrap(EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_socket: cypher_env::ipc_socket(dir.path()).unwrap(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: Some("client_test".into()),
        default_harness: HarnessId::Mock,
    })
    .await
    .unwrap();

    assert!(matches!(
        handle
            .deferred_state()
            .expect("embedded lifecycle")
            .borrow()
            .clone(),
        DeferredEngineState::Waiting
    ));

    let info: EngineInfo = handle
        .client()
        .call_as(methods::ENGINE_INFO, serde_json::json!({}))
        .await
        .expect("EngineInfo bypasses deferred cloud stores");
    assert_eq!(info.workspace_scope, WorkspaceScope::Synced);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            handle
                .client()
                .call(methods::LIST_HARNESSES, serde_json::json!({})),
        )
        .await
        .is_err(),
        "cloud data waits for organization onboarding"
    );
    assert!(!dir.path().join("orgs").exists());
    handle.shutdown().await;
}

#[tokio::test]
async fn bootstrap_connects_when_daemon_is_listening() {
    // Stand in for `cypher headless`: an engine served over the WS IPC socket.
    let daemon_dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(
        daemon_dir.path(),
        Arc::new(default_registry(daemon_dir.path().join("agent-sessions"))),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let port = cypher_env::ipc_socket(daemon_dir.path()).unwrap();
    let listener = cypher_rpc::LocalListener::bind(&port).await.unwrap();
    tokio::spawn(listener.serve(core.rpc_service()));

    let handle = EngineHandle::bootstrap(EngineBootConfig {
        data_dir: daemon_dir.path().to_path_buf(),
        ipc_socket: port.clone(),
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    })
    .await
    .unwrap();
    assert_eq!(
        handle.mode(),
        EngineMode::Remote {
            url: format!("unix:{}", port.display())
        }
    );
    assert_eq!(
        handle.engine_info().workspace_scope,
        WorkspaceScope::Development
    );
    let harnesses = handle
        .client()
        .call(methods::LIST_HARNESSES, serde_json::json!({}))
        .await
        .unwrap();
    assert!(harnesses.as_array().is_some_and(|h| !h.is_empty()));
    assert!(matches!(
        handle
            .client()
            .call(methods::STOP_ENGINE, serde_json::json!({}))
            .await,
        Err(RpcError::Failed(message))
            if message == format!("unknown method: {}", methods::STOP_ENGINE)
    ));
}

fn chat(id: &str, created_min: i64, last_msg_min: Option<i64>) -> Chat {
    let base = DateTime::parse_from_rfc3339("2026-07-19T12:00:00Z")
        .unwrap()
        .to_utc();
    Chat {
        id: id.into(),
        last_message_at: last_msg_min.map(|m| base + TimeDelta::minutes(m)),
        created_at: base + TimeDelta::minutes(created_min),
        ..crate::test_fixtures::chat()
    }
}

fn space(id: &str, device_id: &str, path: &str, created_min: i64) -> Space {
    let base = DateTime::parse_from_rfc3339("2026-07-19T12:00:00Z")
        .unwrap()
        .to_utc();
    Space {
        id: id.into(),
        device_id: device_id.into(),
        path: path.into(),
        created_at: base + TimeDelta::minutes(created_min),
        ..crate::test_fixtures::space()
    }
}

fn session(
    chat_id: &str,
    status: SessionStatus,
    updated_secs_ago: i64,
    now: DateTime<Utc>,
) -> Session {
    Session {
        chat_id: chat_id.into(),
        status,
        updated_at: now - TimeDelta::seconds(updated_secs_ago),
        ..crate::test_fixtures::session()
    }
}

fn user_entry(id: &str) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role: cypher_doc::MessageRole::User,
        ..crate::test_fixtures::entry()
    }
}

fn device(id: &str, name: &str) -> Device {
    Device {
        id: id.into(),
        name: name.into(),
        platform: "macos".into(),
        last_seen_at: None,
        created_at: None,
        version: None,
    }
}

#[test]
fn local_workspace_hides_the_unknown_device_sentinel() {
    let mut state = AppState::new();
    state.workspace_scope = Some(WorkspaceScope::Local);
    state.local_device_id = Some("local".into());

    state.apply_devices(vec![
        device("local", "unknown-device"),
        device("remote", "unknown-device"),
    ]);

    assert_eq!(state.device_name("local"), Some("Local"));
    assert_eq!(state.device_name("remote"), Some("unknown-device"));

    state.apply_devices(vec![device("local", "José's MacBook Pro")]);
    assert_eq!(state.device_name("local"), Some("José's MacBook Pro"));
}

#[test]
fn update_backoff_is_capped_and_restarts_from_zero() {
    // 2 → 4 → 8 → 16 → 30s cap.
    assert_eq!(update_backoff_delay(0), std::time::Duration::from_secs(2));
    assert_eq!(update_backoff_delay(1), std::time::Duration::from_secs(4));
    assert_eq!(update_backoff_delay(2), std::time::Duration::from_secs(8));
    assert_eq!(update_backoff_delay(3), std::time::Duration::from_secs(16));
    assert_eq!(update_backoff_delay(4), std::time::Duration::from_secs(30));
    // Any further step stays capped — a broken stream cannot spin faster.
    assert_eq!(
        update_backoff_delay(100),
        std::time::Duration::from_secs(30)
    );
    // A healthy stream resets the step, so the next retry is fast again.
    assert_eq!(update_backoff_delay(0), std::time::Duration::from_secs(2));
}

#[test]
fn send_pending_overlays_working_until_ttl() {
    let now = Utc::now();
    let s_chat = chat("c", 0, Some(10)); // unseen, no session row
    let mut s = AppState::new();
    assert_eq!(s.display_status_for(&s_chat, now), ChatIndicator::Completed);
    assert_eq!(s.indicator_for("c", now), Indicator::None);
    s.begin_pending_send("c", "m1", now);
    assert_eq!(s.display_status_for(&s_chat, now), ChatIndicator::Working);
    assert_eq!(s.indicator_for("c", now), Indicator::Working);
    // Time-bounded: an offline host must not leave an eternal spinner.
    let later = now + TimeDelta::milliseconds(PENDING_SEND_TTL_MS + 1);
    assert_eq!(
        s.display_status_for(&s_chat, later),
        ChatIndicator::Completed
    );
    assert_eq!(s.indicator_for("c", later), Indicator::None);
}

#[test]
fn send_pending_acked_when_the_host_writes_the_message_back() {
    let now = Utc::now();
    let mut s = AppState::new();
    s.selected_chat = Some("c".into());
    s.begin_pending_send("c", "m1", now);
    // A frame without the message keeps the overlay.
    s.apply_transcript(vec![user_entry("other")]);
    assert!(s.send_pending("c", now));
    // The host executed the command: our id comes back in the doc.
    s.apply_transcript(vec![user_entry("other"), user_entry("m1")]);
    assert!(!s.send_pending("c", now));
}

#[test]
fn a_chat_minted_by_a_send_survives_frames_before_its_row() {
    // A canvas's first send selects the client-minted id before the
    // chats frame carrying its row lands (bug: the tab closed mid-send).
    let mut s = AppState::new();
    s.apply_chats(vec![chat("a", 0, None)]);
    s.selected_chat = Some("new".into());
    s.begin_pending_send("new", "m1", Utc::now());
    s.apply_chats(vec![chat("a", 0, None)]);
    assert_eq!(s.selected_chat.as_deref(), Some("new"));
    // The row arrives: nothing to heal.
    s.apply_chats(vec![chat("a", 0, None), chat("new", 1, None)]);
    assert_eq!(s.selected_chat.as_deref(), Some("new"));
    // Without a send in flight a vanished chat still drops.
    s.end_pending_send("new", "m1");
    s.apply_chats(vec![chat("a", 0, None)]);
    assert_eq!(s.selected_chat, None);
}

#[test]
fn a_stale_send_no_longer_holds_a_vanished_selection() {
    let mut s = AppState::new();
    s.selected_chat = Some("new".into());
    let long_ago = Utc::now() - TimeDelta::milliseconds(PENDING_SEND_TTL_MS + 1);
    s.begin_pending_send("new", "m1", long_ago);
    s.apply_chats(vec![chat("a", 0, None)]);
    assert_eq!(s.selected_chat, None);
}

#[test]
fn mirroring_unchanged_lists_reports_no_change() {
    let mut main = AppState::new();
    main.apply_chats(vec![chat("a", 0, None)]);
    let mut context = AppState::new();
    assert!(context.mirror_lists(&main), "first mirror copies");
    assert!(!context.mirror_lists(&main), "same lists: no notify");
    main.apply_sessions(vec![]);
    assert!(!context.mirror_lists(&main));
    main.apply_chats(vec![chat("a", 0, None), chat("b", 1, None)]);
    assert!(context.mirror_lists(&main), "a chats frame mirrors");
    assert_eq!(context.chats.len(), 2);
}

#[test]
fn transcript_revision_tracks_what_a_transcript_renders() {
    let mut s = AppState::new();
    s.selected_chat = Some("c".into());
    let mut last = s.transcript_revision();
    let mut bumped = |s: &AppState| {
        let now = s.transcript_revision();
        let changed = now != last;
        last = now;
        changed
    };
    s.apply_transcript(vec![user_entry("m1")]);
    assert!(bumped(&s));
    s.push_echo("c", user_entry("m2"));
    assert!(bumped(&s));
    s.mark_steer("m2");
    assert!(bumped(&s));
    s.apply_commands(Vec::new());
    assert!(bumped(&s));
    s.remove_echo("c", "m2");
    assert!(bumped(&s));
    // List-only changes leave it alone.
    s.apply_sessions(vec![]);
    s.apply_chats(vec![chat("c", 0, None)]);
    assert!(!bumped(&s));
}

#[test]
fn send_failure_cleanup_only_ends_its_own_overlay() {
    let now = Utc::now();
    let mut s = AppState::new();
    s.begin_pending_send("c", "m1", now);
    s.begin_pending_send("c", "m2", now); // quick resend superseded m1
    s.end_pending_send("c", "m1"); // m1's failure cleanup arrives late
    assert!(s.send_pending("c", now), "m2's overlay must survive");
    s.end_pending_send("c", "m2");
    assert!(!s.send_pending("c", now));
}

#[test]
fn upload_progress_is_scoped_to_its_chat_and_ends_with_the_send() {
    let mut s = AppState::new();
    let done = Arc::new(std::sync::atomic::AtomicU64::new(0));
    s.begin_upload_progress("c", 200, done.clone());
    assert_eq!(s.upload_progress_percent("c"), Some(0));
    // The OTHER conversations keep their own spinner word — one chat's
    // upload must never narrate itself under theirs.
    assert_eq!(s.upload_progress_percent("other"), None);
    done.store(100, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(s.upload_progress_percent("c"), Some(50));
    // Sealed at 100%: the send retires the trailer instead of leaving
    // "Uploading 100%" frozen under every chat forever.
    done.store(200, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(s.upload_progress_percent("c"), Some(100));
    s.end_upload_progress("c");
    assert_eq!(s.upload_progress_percent("c"), None);
}

#[test]
fn upload_cleanup_only_ends_its_own_progress() {
    let mut s = AppState::new();
    let first = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let second = Arc::new(std::sync::atomic::AtomicU64::new(0));
    s.begin_upload_progress("a", 100, first);
    s.begin_upload_progress("b", 100, second); // b's send claimed the slot
    s.end_upload_progress("a"); // a's late cleanup must not steal it
    assert_eq!(s.upload_progress_percent("b"), Some(0));
    s.end_upload_progress("b");
    assert_eq!(s.upload_progress_percent("b"), None);
}

#[test]
fn chats_sort_by_last_message_desc_with_created_fallback() {
    let mut chats = vec![
        chat("a", 0, Some(10)),
        chat("b", 5, None), // no messages → keys on created_at (+5min)
        chat("c", 1, Some(30)),
        chat("d", 40, None), // created after every message
    ];
    sort_chats(&mut chats);
    let order: Vec<&str> = chats.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(order, ["d", "c", "a", "b"]);
}

#[test]
fn chat_sort_ties_are_deterministic() {
    let mut chats = vec![chat("z", 0, Some(10)), chat("a", 0, Some(10))];
    sort_chats(&mut chats);
    assert_eq!(chats[0].id, "a");
}

#[test]
fn working_indicator_staleness() {
    let now = Utc::now();
    // Fresh working session shows.
    let fresh = session("c", SessionStatus::Working, 10, now);
    assert_eq!(effective_indicator(Some(&fresh), now), Indicator::Working);
    // Stale working session is suppressed — crashed backend, not eternal spinner.
    let stale = session("c", SessionStatus::Working, 46, now);
    assert_eq!(effective_indicator(Some(&stale), now), Indicator::None);
    // Exactly at the boundary still shows (strictly-older-than semantics).
    let edge = session("c", SessionStatus::Working, 45, now);
    assert_eq!(effective_indicator(Some(&edge), now), Indicator::Working);
    // Future timestamps (clock skew) count as fresh.
    let skewed = session("c", SessionStatus::Working, -30, now);
    assert_eq!(effective_indicator(Some(&skewed), now), Indicator::Working);
}

#[test]
fn indicator_kinds() {
    let now = Utc::now();
    assert_eq!(effective_indicator(None, now), Indicator::None);
    let idle = session("c", SessionStatus::Idle, 0, now);
    assert_eq!(effective_indicator(Some(&idle), now), Indicator::None);
    // Errored is not staleness-gated: the error stays visible.
    let errored = session("c", SessionStatus::Errored, 600, now);
    assert_eq!(effective_indicator(Some(&errored), now), Indicator::Errored);
    let awaiting = session("c", SessionStatus::AwaitingInput, 5, now);
    assert_eq!(
        effective_indicator(Some(&awaiting), now),
        Indicator::AwaitingInput
    );
    let awaiting_stale = session("c", SessionStatus::AwaitingInput, 300, now);
    assert_eq!(
        effective_indicator(Some(&awaiting_stale), now),
        Indicator::None
    );
}

#[test]
fn display_status_derivation() {
    let now = Utc::now();
    let mut c = chat("c", 0, Some(10));
    // Live states win regardless of seen.
    let working = session("c", SessionStatus::Working, 5, now);
    assert_eq!(
        display_status(&c, Some(&working), now),
        ChatIndicator::Working
    );
    let awaiting = session("c", SessionStatus::AwaitingInput, 5, now);
    assert_eq!(
        display_status(&c, Some(&awaiting), now),
        ChatIndicator::AwaitingInput
    );
    // Finished + unseen = Completed (no session row at all).
    assert_eq!(display_status(&c, None, now), ChatIndicator::Completed);
    // Idle session + unseen = Completed.
    let idle = session("c", SessionStatus::Idle, 5, now);
    assert_eq!(
        display_status(&c, Some(&idle), now),
        ChatIndicator::Completed
    );
    // Stale working session falls back to the seen check.
    let stale = session("c", SessionStatus::Working, 300, now);
    assert_eq!(
        display_status(&c, Some(&stale), now),
        ChatIndicator::Completed
    );
    // Seen after the last message = Idle.
    c.last_seen_at = c.last_message_at.map(|t| t + TimeDelta::minutes(1));
    assert_eq!(display_status(&c, Some(&idle), now), ChatIndicator::Idle);
    // Errored + unseen = Errored; seen clears it to Idle.
    let errored = session("c", SessionStatus::Errored, 600, now);
    assert_eq!(display_status(&c, Some(&errored), now), ChatIndicator::Idle);
    c.last_seen_at = None;
    assert_eq!(
        display_status(&c, Some(&errored), now),
        ChatIndicator::Errored
    );
    // No messages at all: nothing to see — Idle.
    let fresh = chat("f", 0, None);
    assert_eq!(display_status(&fresh, None, now), ChatIndicator::Idle);
}

#[test]
fn attention_count_tracks_the_sidebar_corners() {
    let now = Utc::now();
    let mut s = AppState::new();
    let mut seen = chat("seen", 0, Some(1));
    seen.last_seen_at = seen.last_message_at;
    let mut archived = chat("archived", 0, Some(2));
    archived.archived = true;
    let mut orphan = chat("orphan", 0, Some(3));
    let mut asked_seen = chat("asked-seen", 0, Some(8));
    asked_seen.last_seen_at = asked_seen.last_message_at;
    orphan.space_id = Some("gone".into());
    s.apply_chats(vec![
        chat("done", 0, Some(4)),
        chat("asking", 0, Some(5)),
        chat("failed", 0, Some(6)),
        chat("running", 0, Some(7)),
        chat("empty", 0, None),
        seen,
        archived,
        orphan,
        asked_seen,
    ]);
    s.apply_sessions(vec![
        session("asking", SessionStatus::AwaitingInput, 5, now),
        session("failed", SessionStatus::Errored, 5, now),
        session("running", SessionStatus::Working, 5, now),
        session("asked-seen", SessionStatus::AwaitingInput, 5, now),
    ]);
    // done (unseen), asking, failed — not working, empty, seen, archived,
    // a question already looked at (on any device), or a chat whose
    // project is gone (the overview hides it too).
    assert_eq!(s.attention_count(now), 3);
    s.begin_pending_send("done", "m1", now);
    assert_eq!(
        s.attention_count(now),
        2,
        "a send in flight reads as working"
    );
}

#[test]
fn active_list_sorts_by_recency_only_status_never_moves_rows() {
    let a = chat("a", 0, Some(10)); // Completed (older)
    let b = chat("b", 0, Some(20)); // Completed (newer)
    let c = chat("c", 0, Some(5)); // AwaitingInput
    let d = chat("d", 0, Some(1)); // Working
    let mut rows = vec![
        (ChatIndicator::Completed, &a),
        (ChatIndicator::Completed, &b),
        (ChatIndicator::AwaitingInput, &c),
        (ChatIndicator::Working, &d),
    ];
    sort_active(&mut rows);
    let order: Vec<&str> = rows.iter().map(|(_, c)| c.id.as_str()).collect();
    assert_eq!(order, ["b", "a", "c", "d"], "recency desc, status ignored");

    // Opening a completed session (completed → seen → idle) must NOT
    // change its position (user report: rows jumped under the pointer).
    let mut seen = vec![
        (ChatIndicator::Idle, &a),
        (ChatIndicator::Completed, &b),
        (ChatIndicator::AwaitingInput, &c),
        (ChatIndicator::Working, &d),
    ];
    sort_active(&mut seen);
    let order_after: Vec<&str> = seen.iter().map(|(_, c)| c.id.as_str()).collect();
    assert_eq!(order, order_after);
}

#[test]
fn tabs_order_by_creation_not_activity() {
    let a = chat("a", 5, Some(100)); // created later, very active
    let b = chat("b", 1, Some(2));
    let mut tabs = vec![&a, &b];
    sort_tabs(&mut tabs);
    let order: Vec<&str> = tabs.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(order, ["b", "a"]);
}

#[test]
fn apply_spaces_sorts_and_heals_selection() {
    let mut state = AppState::new();
    state.apply_spaces(vec![
        space("s2", "dev", "/b", 2),
        space("s1", "dev", "/a", 1),
    ]);
    let ids: Vec<&str> = state.spaces.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["s1", "s2"]);
    // First frame auto-selects the first space.
    assert_eq!(state.selected_space.as_deref(), Some("s1"));
    state.selected_space = Some("s2".into());
    // Vanished selection heals to the first space.
    state.apply_spaces(vec![space("s1", "dev", "/a", 1)]);
    assert_eq!(state.selected_space.as_deref(), Some("s1"));
    // No spaces at all: selection clears.
    state.apply_spaces(vec![]);
    assert_eq!(state.selected_space, None);
}

#[test]
fn first_space_on_picked_device_is_deterministic() {
    let mut state = AppState::new();
    state.apply_spaces(vec![
        space("z", "laptop", "/z", 3),
        space("a", "phone", "/a", 1),
        space("m", "laptop", "/m", 2),
    ]);
    // Picked device wins, in display order ("laptop" spaces: m, z).
    state.selected_device = Some("laptop".into());
    assert_eq!(state.first_space_on_picked_device().as_deref(), Some("m"));
    // Unpicked device falls back to the local device.
    state.selected_device = None;
    state.local_device_id = Some("laptop".into());
    assert_eq!(state.first_space_on_picked_device().as_deref(), Some("m"));
    // Neither device matches: any space at all (display-first).
    state.local_device_id = Some("server".into());
    assert_eq!(state.first_space_on_picked_device().as_deref(), Some("a"));
    // No spaces: nothing to fall back to.
    state.apply_spaces(vec![]);
    assert_eq!(state.first_space_on_picked_device(), None);
}

#[test]
fn selected_space_if_live_rejects_dangling_ids() {
    let mut state = AppState::new();
    state.apply_spaces(vec![space("s1", "dev", "/a", 1)]);
    assert_eq!(state.selected_space.as_deref(), Some("s1"));
    assert_eq!(state.selected_space_if_live().as_deref(), Some("s1"));
    // A dangling selection (project deleted elsewhere) is not "live".
    state.selected_space = Some("ghost".into());
    assert_eq!(state.selected_space_if_live(), None);
    // No selection at all is not "live" either.
    state.selected_space = None;
    assert_eq!(state.selected_space_if_live(), None);
}

/// Two projects plus a project-less chat, each with an unseen finished
/// session — the fixture for the project-window scoping tests.
fn scoped_fixture() -> AppState {
    let mut state = AppState::new();
    state.apply_spaces(vec![space("a", "dev", "/a", 1), space("b", "dev", "/b", 2)]);
    let mut in_a = chat("in-a", 0, Some(3));
    in_a.space_id = Some("a".into());
    let mut in_b = chat("in-b", 0, Some(2));
    in_b.space_id = Some("b".into());
    state.apply_chats(vec![in_a, in_b, chat("loose", 0, Some(1))]);
    state
}

fn listed(state: &AppState) -> (Vec<&str>, Vec<&str>, Vec<String>) {
    let chats = state.visible_chats().map(|c| c.id.as_str()).collect();
    let spaces = state
        .spaces_sorted()
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    let groups = state
        .sidebar_groups(Utc::now())
        .into_iter()
        .map(|g| g.key)
        .collect();
    (chats, spaces, groups)
}

#[test]
fn main_window_scope_hides_projects_open_elsewhere() {
    let mut state = scoped_fixture();
    state.scope.hidden = HashSet::from(["b".to_string()]);
    let (chats, spaces, groups) = listed(&state);
    assert_eq!(chats, ["in-a", "loose"]);
    assert_eq!(spaces, ["a"]);
    assert_eq!(groups, ["s:a", "np:dev"]);
    // Even when quiet: a hidden project's empty card stays hidden.
    state.apply_chats(Vec::new());
    assert_eq!(listed(&state).2, ["s:a"]);
}

#[test]
fn project_window_scope_lists_only_its_project() {
    let mut state = scoped_fixture();
    state.scope.only = Some("b".into());
    let (chats, spaces, groups) = listed(&state);
    assert_eq!(chats, ["in-b"], "no other project, no project-less chats");
    assert_eq!(spaces, ["b"]);
    assert_eq!(groups, ["s:b"]);
    assert_eq!(state.first_space_on_picked_device().as_deref(), Some("b"));
}

#[test]
fn dock_badge_counts_every_window() {
    let now = Utc::now();
    let mut state = scoped_fixture();
    assert_eq!(state.attention_count(now), 3);
    state.scope.hidden = HashSet::from(["a".to_string(), "b".to_string()]);
    assert_eq!(state.attention_count(now), 3, "the Dock icon is app-wide");
}

#[test]
fn chats_in_space_filters_and_orders() {
    let mut state = AppState::new();
    state.apply_spaces(vec![space("s1", "dev", "/a", 1)]);
    let mut in_space_new = chat("new", 5, None);
    in_space_new.space_id = Some("s1".into());
    let mut in_space_old = chat("old", 1, Some(50)); // active but created first
    in_space_old.space_id = Some("s1".into());
    let mut other = chat("other", 2, None);
    other.space_id = Some("s2".into());
    let mut archived = chat("gone", 0, None);
    archived.space_id = Some("s1".into());
    archived.archived = true;
    let dangling = chat("dangling", 3, None); // no space id
    state.apply_chats(vec![in_space_new, in_space_old, other, archived, dangling]);
    let ids: Vec<&str> = state
        .chats_in_space("s1")
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(ids, ["old", "new"]);
    // The overview shows every live-space chat (idle included) PLUS
    // project-less chats (first-class since the project selectors);
    // chats of unknown spaces stay hidden. Completed ("old") outranks
    // idle ("new"/"dangling").
    let now = Utc::now();
    let overview: Vec<&str> = state
        .overview_chats(now)
        .iter()
        .map(|(_, c)| c.id.as_str())
        .collect();
    assert_eq!(overview, ["old", "new", "dangling"]);
}

#[test]
fn sidebar_groups_order_by_newest_chat_and_keep_overview_order() {
    let mut state = AppState::new();
    state.apply_spaces(vec![
        space("s1", "dev-a", "/a", 1),
        space("s2", "dev-b", "/b", 2),
        space("s3", "dev-a", "/c", 3),
    ]);
    let mut a = chat("a", 0, Some(10)); // in s1
    a.space_id = Some("s1".into());
    let mut b = chat("b", 0, Some(20)); // in s2 (newest)
    b.space_id = Some("s2".into());
    let mut c = chat("c", 0, Some(5)); // in s3 (oldest)
    c.space_id = Some("s3".into());
    let mut d = chat("d", 1, Some(15)); // in s1, newer than a
    d.space_id = Some("s1".into());
    state.apply_chats(vec![a, b, c, d]);
    let now = Utc::now();
    let groups = state.sidebar_groups(now);
    // Groups ordered by their newest chat: s2 (b=20), s1 (d=15), s3 (c=5).
    let keys: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(keys, ["s:s2", "s:s1", "s:s3"]);
    // Chats inside a group retain the overview (recency) order.
    let s1: Vec<&str> = groups[1].chats.iter().map(|(_, c)| c.id.as_str()).collect();
    assert_eq!(s1, ["d", "a"]);
    // Stability: the same call yields the same keys.
    let again_groups = state.sidebar_groups(now);
    let again: Vec<&str> = again_groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(again, keys);
}

#[test]
fn sidebar_groups_status_changes_never_reorder_cards() {
    let mut state = AppState::new();
    state.apply_spaces(vec![
        space("s1", "dev", "/a", 1),
        space("s2", "dev", "/b", 2),
    ]);
    let mut a = chat("a", 0, Some(10));
    a.space_id = Some("s1".into());
    let mut b = chat("b", 0, Some(20));
    b.space_id = Some("s2".into());
    state.apply_chats(vec![a, b]);
    let now = Utc::now();
    let before: Vec<String> = state
        .sidebar_groups(now)
        .iter()
        .map(|g| g.key.clone())
        .collect();
    assert_eq!(before, ["s:s2", "s:s1"]);
    // s1's chat turns Working — status must never move the card.
    state.apply_sessions(vec![session("a", SessionStatus::Working, 5, now)]);
    let after: Vec<String> = state
        .sidebar_groups(now)
        .iter()
        .map(|g| g.key.clone())
        .collect();
    assert_eq!(after, before);
}

#[test]
fn sidebar_groups_append_empty_spaces_deterministically() {
    let mut state = AppState::new();
    state.apply_spaces(vec![
        space("active", "dev", "/z", 1),
        space("empty-b", "dev-b", "/b", 2),
        space("empty-a", "dev-a", "/a", 3),
    ]);
    let mut c = chat("c", 0, Some(5));
    c.space_id = Some("active".into());
    state.apply_chats(vec![c]);
    let now = Utc::now();
    let groups = state.sidebar_groups(now);
    let summary: Vec<(&str, usize)> = groups
        .iter()
        .map(|g| (g.title.as_str(), g.chats.len()))
        .collect();
    // The active space leads; quiet spaces are appended deterministically
    // (display name "a" < "b"), never by recency noise.
    assert_eq!(summary, [("z", 1), ("a", 0), ("b", 0)]);
    // Stable across renders.
    let again_groups = state.sidebar_groups(now);
    let again: Vec<&str> = again_groups.iter().map(|g| g.title.as_str()).collect();
    let first: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
    assert_eq!(again, first);
}

#[test]
fn sidebar_groups_show_every_host_together_with_device_labels() {
    let mut state = AppState::new();
    state.devices = vec![device("dev-a", "MacBook"), device("dev-b", "Desktop")];
    state.apply_spaces(vec![
        space("s-a", "dev-a", "/a", 1),
        space("s-b", "dev-b", "/b", 2),
    ]);
    let now = Utc::now();
    let groups = state.sidebar_groups(now);
    let keys: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(keys, ["s:s-a", "s:s-b"], "both hosts in the one sidebar");
    assert_eq!(groups[0].device, "MacBook");
    assert_eq!(groups[1].device, "Desktop");
    // Quiet spaces are still cards (project management stays reachable).
    assert!(groups.iter().all(|g| g.chats.is_empty()));
}

#[test]
fn sidebar_groups_synthetic_no_project_and_unavailable() {
    let mut state = AppState::new();
    state.devices = vec![device("dev-b", "Laptop"), device("dev-c", "Tablet")];
    state.apply_spaces(vec![space("s1", "dev-a", "/a", 1)]);
    let mut in_space = chat("in", 0, Some(30));
    in_space.space_id = Some("s1".into());
    let mut no_project = chat("np", 0, Some(20));
    no_project.space_id = None;
    no_project.device_id = "dev-b".into();
    let mut dangling = chat("dang", 0, Some(10));
    dangling.space_id = Some("gone".into());
    dangling.device_id = "dev-c".into();
    state.apply_chats(vec![in_space, no_project, dangling]);
    let now = Utc::now();
    let groups = state.sidebar_groups(now);
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].key, "s:s1");
    assert_eq!(groups[0].kind, SidebarGroupKind::Space);
    assert_eq!(groups[0].space_id, Some("s1"));
    assert_eq!(groups[1].key, "np:dev-b");
    assert_eq!(groups[1].kind, SidebarGroupKind::NoProject);
    assert_eq!(groups[1].title, "No project");
    assert_eq!(groups[1].device, "Laptop");
    assert_eq!(groups[1].space_id, None, "synthetic cards have no menu");
    assert_eq!(groups[2].key, "u:gone");
    assert_eq!(groups[2].kind, SidebarGroupKind::Unavailable);
    assert_eq!(groups[2].title, "Unavailable project");
    assert_eq!(groups[2].device, "Tablet");
    assert_eq!(groups[2].space_id, None);
    // Dangling-id chats are included (not dropped like the old overview).
    let dangling_chats: Vec<&str> = groups[2].chats.iter().map(|(_, c)| c.id.as_str()).collect();
    assert_eq!(dangling_chats, ["dang"]);
}

#[test]
fn sidebar_groups_pinned_projects_and_sessions_lead() {
    let mut state = AppState::new();
    state.devices = vec![device("dev-a", "Mac")];
    let mut pinned_space = space("s-pinned", "dev-a", "/p", 1);
    pinned_space.pinned = true;
    state.apply_spaces(vec![
        space("s-busy", "dev-a", "/b", 1),
        pinned_space,
        space("s-quiet", "dev-a", "/q", 1),
    ]);
    // s-busy has the newest activity; s-pinned is older but pinned.
    let mut busy = chat("busy", 0, Some(30));
    busy.space_id = Some("s-busy".into());
    let mut old_pinned_session = chat("old-pin", 0, Some(1));
    old_pinned_session.space_id = Some("s-busy".into());
    old_pinned_session.pinned = true;
    let mut in_pinned = chat("in-pinned", 0, Some(5));
    in_pinned.space_id = Some("s-pinned".into());
    state.apply_chats(vec![busy, old_pinned_session, in_pinned]);
    let groups = state.sidebar_groups(Utc::now());
    let keys: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(keys, ["s:s-pinned", "s:s-busy", "s:s-quiet"]);
    assert!(groups[0].pinned && !groups[1].pinned);
    // Within the busy project the pinned (older) session leads.
    let busy_ids: Vec<&str> = groups[1].chats.iter().map(|(_, c)| c.id.as_str()).collect();
    assert_eq!(busy_ids, ["old-pin", "busy"]);
}

#[test]
fn sidebar_view_filters_by_device_and_sorts_with_pins_first() {
    let mut state = AppState::new();
    state.devices = vec![device("dev-a", "Mac"), device("dev-b", "Linux box")];
    state.apply_spaces(vec![
        space("s-zeta", "dev-a", "/zeta", 1),
        space("s-alpha", "dev-b", "/alpha", 3),
        space("s-mid", "dev-a", "/mid", 2),
    ]);
    let mut newest = chat("n", 0, Some(30));
    newest.space_id = Some("s-zeta".into());
    let mut older = chat("o", 0, Some(10));
    older.space_id = Some("s-alpha".into());
    let mut oldest = chat("p", 0, Some(1));
    oldest.space_id = Some("s-mid".into());
    state.apply_chats(vec![newest, older, oldest]);
    let now = Utc::now();
    let keys = |state: &AppState, view: &SidebarView| -> Vec<String> {
        state
            .sidebar_groups_with(now, view)
            .iter()
            .map(|g| g.key.clone())
            .collect()
    };
    let view = |sort: SidebarSort, device: Option<&str>| SidebarView {
        sort,
        device: device.map(str::to_string),
        reversed: false,
    };
    assert_eq!(
        keys(&state, &view(SidebarSort::Activity, None)),
        ["s:s-zeta", "s:s-alpha", "s:s-mid"]
    );
    assert_eq!(
        keys(&state, &view(SidebarSort::Name, None)),
        ["s:s-alpha", "s:s-mid", "s:s-zeta"]
    );
    // Device: "Linux box" < "Mac"; within Mac the activity order holds.
    assert_eq!(
        keys(&state, &view(SidebarSort::Device, None)),
        ["s:s-alpha", "s:s-zeta", "s:s-mid"]
    );
    // Date: newest created first (alpha=3, mid=2, zeta=1).
    assert_eq!(
        keys(&state, &view(SidebarSort::Date, None)),
        ["s:s-alpha", "s:s-mid", "s:s-zeta"]
    );
    assert_eq!(
        keys(&state, &view(SidebarSort::Name, Some("dev-a"))),
        ["s:s-mid", "s:s-zeta"]
    );
    // Reversed: Z→A, and the pin still leads afterwards.
    let reversed = SidebarView {
        sort: SidebarSort::Name,
        device: None,
        reversed: true,
    };
    assert!(reversed.descending());
    assert!(!view(SidebarSort::Name, None).descending());
    assert!(view(SidebarSort::Activity, None).descending());
    assert_eq!(
        keys(&state, &reversed),
        ["s:s-zeta", "s:s-mid", "s:s-alpha"]
    );
    // A pinned project leads regardless of the sort.
    let mut spaces = state.spaces.clone();
    spaces.iter_mut().find(|s| s.id == "s-zeta").unwrap().pinned = true;
    state.apply_spaces(spaces);
    assert_eq!(
        keys(&state, &view(SidebarSort::Name, None)),
        ["s:s-zeta", "s:s-alpha", "s:s-mid"]
    );
}

#[test]
fn sidebar_groups_quick_chats_by_scratch_folder_shape() {
    let mut state = AppState::new();
    state.devices = vec![device("dev-b", "Laptop")];
    let mut quick = chat("q1", 0, Some(30));
    quick.space_id = None;
    quick.device_id = "dev-b".into();
    quick.cwd = Some("/tmp/cypher-scratch/q1".into());
    let mut plain = chat("np", 0, Some(20));
    plain.space_id = None;
    plain.device_id = "dev-b".into();
    plain.cwd = Some("~".into());
    // A scratch-shaped cwd minted for ANOTHER chat is not this chat's.
    let mut foreign = chat("other", 0, Some(10));
    foreign.space_id = None;
    foreign.device_id = "dev-b".into();
    foreign.cwd = Some("/tmp/cypher-scratch/q1".into());
    assert!(quick.is_scratch());
    assert!(!plain.is_scratch());
    assert!(!foreign.is_scratch());
    state.apply_chats(vec![quick, plain, foreign]);
    let groups = state.sidebar_groups(Utc::now());
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].key, "sc");
    assert_eq!(groups[0].kind, SidebarGroupKind::Scratch);
    assert_eq!(groups[0].title, "Quick chats");
    assert_eq!(groups[0].device, "Laptop");
    assert_eq!(groups[1].key, "np:dev-b");
    let plain_ids: Vec<&str> = groups[1].chats.iter().map(|(_, c)| c.id.as_str()).collect();
    assert_eq!(plain_ids, ["np", "other"]);
}

#[test]
fn sidebar_groups_merge_quick_chats_across_devices() {
    let mut state = AppState::new();
    state.devices = vec![device("dev-a", "Desk"), device("dev-b", "Laptop")];
    let quick = |id: &str, device: &str, at: i64| {
        let mut row = chat(id, 0, Some(at));
        row.space_id = None;
        row.device_id = device.into();
        row.cwd = Some(format!("/tmp/cypher-scratch/{id}"));
        row
    };
    state.apply_chats(vec![
        quick("b1", "dev-b", 30),
        chat("s1", 0, Some(20)),
        quick("a1", "dev-a", 25),
        quick("b2", "dev-b", 10),
    ]);
    let scratch = |view: &SidebarView| {
        let groups = state.sidebar_groups_with(Utc::now(), view);
        let cards: Vec<_> = groups
            .iter()
            .filter(|g| g.kind == SidebarGroupKind::Scratch)
            .map(|g| {
                let ids: Vec<String> = g.chats.iter().map(|(_, c)| c.id.clone()).collect();
                (g.key.clone(), g.device.clone(), ids)
            })
            .collect();
        cards
    };
    // One card, newest first across hosts, no single device name.
    assert_eq!(
        scratch(&SidebarView::default()),
        [(
            "sc".to_string(),
            String::new(),
            vec!["b1".into(), "a1".into(), "b2".into()]
        )]
    );
    // The device filter still narrows the merged card to one host.
    let only_a = SidebarView {
        device: Some("dev-a".into()),
        ..SidebarView::default()
    };
    assert_eq!(
        scratch(&only_a),
        [("sc".to_string(), "Desk".to_string(), vec!["a1".into()])]
    );
}

#[test]
fn apply_chats_drops_vanished_selection() {
    let mut state = AppState::new();
    state.apply_chats(vec![chat("a", 0, None), chat("b", 1, None)]);
    state.selected_chat = Some("a".into());
    state.transcript = vec![];
    state.apply_chats(vec![chat("b", 1, None)]);
    assert_eq!(state.selected_chat, None);
    // Still-present selection survives.
    state.selected_chat = Some("b".into());
    state.apply_chats(vec![chat("b", 1, None), chat("c", 2, None)]);
    assert_eq!(state.selected_chat.as_deref(), Some("b"));
}

/// A backend whose RPC server never runs (an undriven current-thread
/// runtime): watches can be spawned but never deliver.
struct IdleEngine(RpcClient);

#[async_trait]
impl EngineBackend for IdleEngine {
    fn client(&self) -> &RpcClient {
        &self.0
    }
    fn mode(&self) -> EngineMode {
        EngineMode::InProcess
    }
    async fn shutdown(&self) {}
}

fn idle_engine(rt: &tokio::runtime::Runtime) -> EngineHandle {
    let _guard = rt.enter();
    EngineHandle {
        inner: Arc::new(IdleEngine(memory_client(Arc::new(LegacyIdentityRpc)))),
        engine_info: EngineInfo {
            device_id: "dev".into(),
            workspace_scope: WorkspaceScope::Local,
        },
        deferred_state: None,
    }
}

fn idle_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

fn chat_ids(state: &AppState) -> Vec<&str> {
    state.chats.iter().map(|c| c.id.as_str()).collect()
}

#[gpui::test]
fn session_context_mirrors_main_lists(cx: &mut gpui::TestAppContext) {
    let main = cx.new(|_| AppState::new());
    main.update(cx, |m, _| {
        m.apply_chats(vec![chat("a", 0, None), chat("b", 1, None)])
    });
    let ctx = cx.update(|cx| AppState::new_session_context(&main, Some("a".into()), cx));
    ctx.read_with(cx, |c, _| {
        assert_eq!(c.selected_chat.as_deref(), Some("a"));
        assert_eq!(chat_ids(c), ["b", "a"]);
        assert!(c.parent().is_some());
    });

    main.update(cx, |m, cx| {
        m.apply_chats(vec![
            chat("a", 0, None),
            chat("b", 1, None),
            chat("c", 2, None),
        ]);
        m.sessions = vec![session("c", SessionStatus::Working, 0, Utc::now())];
        cx.notify();
    });
    cx.run_until_parked();
    ctx.read_with(cx, |c, _| {
        assert_eq!(chat_ids(c), ["c", "b", "a"]);
        assert_eq!(c.sessions.len(), 1);
    });

    // Deleted elsewhere: the tile's selection goes the same way
    // `apply_chats` drops it.
    main.update(cx, |m, cx| {
        m.apply_chats(vec![chat("b", 1, None)]);
        cx.notify();
    });
    cx.run_until_parked();
    ctx.read_with(cx, |c, _| assert_eq!(c.selected_chat, None));
}

#[gpui::test]
fn session_context_keeps_its_own_selection(cx: &mut gpui::TestAppContext) {
    let main = cx.new(|_| AppState::new());
    main.update(cx, |m, _| {
        m.apply_chats(vec![chat("a", 0, None), chat("b", 1, None)])
    });
    let ctx = cx.update(|cx| AppState::new_session_context(&main, Some("a".into()), cx));
    main.update(cx, |m, cx| m.select_chat(Some("b".into()), cx));
    cx.run_until_parked();
    assert_eq!(
        main.read_with(cx, |m, _| m.selected_chat.clone())
            .as_deref(),
        Some("b")
    );
    assert_eq!(
        ctx.read_with(cx, |c, _| c.selected_chat.clone()).as_deref(),
        Some("a")
    );
}

#[gpui::test]
fn session_context_shares_the_pending_send_overlay(cx: &mut gpui::TestAppContext) {
    let now = Utc::now();
    let main = cx.new(|_| AppState::new());
    main.update(cx, |m, _| m.apply_chats(vec![chat("a", 0, None)]));
    let ctx = cx.update(|cx| AppState::new_session_context(&main, Some("a".into()), cx));
    ctx.update(cx, |c, _| c.begin_pending_send("a", "m1", now));
    main.read_with(cx, |m, _| {
        assert!(m.send_pending("a", now));
        assert_eq!(m.indicator_for("a", now), Indicator::Working);
    });
    // The tile's transcript acks it for everyone.
    ctx.update(cx, |c, _| c.apply_transcript(vec![user_entry("m1")]));
    main.read_with(cx, |m, _| {
        assert!(!m.send_pending("a", now));
        assert_eq!(m.indicator_for("a", now), Indicator::None);
    });
}

#[gpui::test]
fn session_context_forwards_config_and_seen_to_main(cx: &mut gpui::TestAppContext) {
    let main = cx.new(|_| AppState::new());
    // Unseen activity on "a".
    main.update(cx, |m, _| m.apply_chats(vec![chat("a", 0, Some(5))]));
    assert!(main.read_with(cx, |m, _| m.chats[0].unseen()));
    // Opening the tile marks it seen on main too (main's sidebar badge).
    let ctx = cx.update(|cx| AppState::new_session_context(&main, Some("a".into()), cx));
    cx.run_until_parked();
    assert!(!main.read_with(cx, |m, _| m.chats[0].unseen()));
    assert!(!ctx.read_with(cx, |c, _| c.chats[0].unseen()));

    let config = cypher_proto::ChatConfig {
        harness: HarnessId::Pi,
        model: Some("claude-fable-5".into()),
        reasoning: None,
        model_options: serde_json::Map::new(),
        sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
    };
    ctx.update(cx, |c, cx| {
        c.set_chat_config_optimistic("a", config.clone(), cx)
    });
    cx.run_until_parked();
    // Landed on main, so the mirror copy keeps it on the tile.
    assert_eq!(
        main.read_with(cx, |m, _| m.chats[0].config.clone()),
        Some(config.clone())
    );
    assert_eq!(
        ctx.read_with(cx, |c, _| c.chats[0].config.clone()),
        Some(config)
    );
}

#[gpui::test]
fn lists_only_select_chat_spawns_no_watch(cx: &mut gpui::TestAppContext) {
    let rt = idle_runtime();
    let engine = idle_engine(&rt);
    let main = cx.new(|_| AppState::new());
    main.update(cx, |m, cx| {
        m.engine = Some(engine);
        m.apply_chats(vec![chat("a", 0, None), chat("b", 1, None)]);
        // Control: a transcript-owning state subscribes on select.
        m.select_chat(Some("a".into()), cx);
        assert!(m.transcript_task.is_some() && m.commands_task.is_some());

        m.set_transcript_watches(false, cx);
        assert!(m.transcript_task.is_none() && m.commands_task.is_none());
        m.select_chat(Some("b".into()), cx);
        assert_eq!(m.selected_chat.as_deref(), Some("b"));
        assert!(m.transcript_task.is_none() && m.commands_task.is_none());

        m.set_transcript_watches(true, cx);
        assert!(m.transcript_task.is_some() && m.commands_task.is_some());
    });
}

#[gpui::test]
fn canvas_context_selects_its_first_chat_without_an_engine(cx: &mut gpui::TestAppContext) {
    let main = cx.new(|_| AppState::new());
    main.update(cx, |m, _| {
        m.apply_spaces(vec![space("s1", "dev", "/p", 0)]);
        m.selected_device = Some("dev".into());
    });
    let ctx = cx.update(|cx| AppState::new_session_context(&main, None, cx));
    ctx.read_with(cx, |c, _| {
        assert_eq!(c.selected_chat, None);
        assert_eq!(c.selected_space.as_deref(), Some("s1"));
        assert_eq!(c.selected_device.as_deref(), Some("dev"));
    });
    // The first send mints a chat and selects it on the tile.
    let mut minted = chat("new", 3, None);
    minted.space_id = Some("s1".into());
    main.update(cx, |m, cx| {
        m.apply_chats(vec![minted]);
        cx.notify();
    });
    cx.run_until_parked();
    ctx.update(cx, |c, cx| {
        c.select_chat(Some("new".into()), cx);
        assert_eq!(c.selected_chat.as_deref(), Some("new"));
        assert!(c.transcript_task.is_none());
    });
    assert_eq!(main.read_with(cx, |m, _| m.selected_chat.clone()), None);
}

#[gpui::test]
fn canvas_context_keeps_a_minted_chat_until_a_chats_frame_drops_it(cx: &mut gpui::TestAppContext) {
    let main = cx.new(|_| AppState::new());
    main.update(cx, |m, _| m.apply_chats(vec![chat("a", 0, None)]));
    let ctx = cx.update(|cx| AppState::new_session_context(&main, None, cx));
    // The composer selects the client-minted id before its row syncs.
    ctx.update(cx, |c, cx| c.select_chat(Some("new".into()), cx));
    // Unrelated main notifies (a sessions frame) must not drop it.
    main.update(cx, |m, cx| {
        m.apply_sessions(Vec::new());
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        ctx.read_with(cx, |c, _| c.selected_chat.clone()).as_deref(),
        Some("new")
    );
    // The row lands, then is deleted elsewhere: the frame drops it.
    main.update(cx, |m, cx| {
        m.apply_chats(vec![chat("a", 0, None), chat("new", 1, None)]);
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        ctx.read_with(cx, |c, _| c.selected_chat.clone()).as_deref(),
        Some("new")
    );
    main.update(cx, |m, cx| {
        m.apply_chats(vec![chat("a", 0, None)]);
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(ctx.read_with(cx, |c, _| c.selected_chat.clone()), None);
}

#[test]
fn insert_chat_optimistic_inserts_sorts_and_is_idempotent() {
    // A promoted Side Chat lands optimistically before the next
    // chats frame — inserted (sorted), never duplicated, and never
    // clobbering an authoritative row that already arrived.
    let mut state = AppState::new();
    state.apply_chats(vec![chat("old", 0, None), chat("new", 5, None)]);
    let mut promoted = chat("promoted", 9, None);
    promoted.space_id = Some("s1".into());
    state.insert_chat_optimistic(promoted.clone());
    assert_eq!(
        state
            .chats
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        ["promoted", "new", "old"],
        "inserted row participates in the sort (newest created first)"
    );
    // Idempotent: the same id is never inserted twice.
    state.insert_chat_optimistic(promoted);
    assert_eq!(state.chats.len(), 3);
    // An already-present id is untouched (authoritative frame won).
    state.insert_chat_optimistic(chat("old", 0, None));
    assert_eq!(state.chats.len(), 3);
}

#[test]
fn apply_chat_config_stamps_the_row() {
    let mut state = AppState::new();
    state.apply_chats(vec![chat("a", 0, None), chat("b", 1, None)]);
    let config = cypher_proto::ChatConfig {
        harness: HarnessId::Pi,
        model: Some("claude-fable-5".into()),
        reasoning: Some(cypher_proto::ReasoningLevel::XHigh),
        model_options: serde_json::Map::new(),
        sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
    };
    state.apply_chat_config("a", config.clone());
    assert_eq!(
        state.chats.iter().find(|c| c.id == "a").unwrap().config,
        Some(config)
    );
    assert!(
        state
            .chats
            .iter()
            .find(|c| c.id == "b")
            .unwrap()
            .config
            .is_none()
    );
    // Unknown chat: no-op, no panic.
    state.apply_chat_config(
        "missing",
        cypher_proto::ChatConfig {
            harness: HarnessId::Pi,
            model: None,
            reasoning: None,
            model_options: serde_json::Map::new(),
            sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
        },
    );
}

// ---- temporary Side Chat fork ----

#[test]
fn side_chat_synthetic_row_inherits_parent_context() {
    // The fork's synthetic row carries the parent's device/space/cwd/
    // branch/checkout/config so the reused Transcript/Composer read the
    // inherited working context.
    let mut parent = chat("parent", 0, Some(10));
    parent.device_id = "remote-dev".into();
    parent.cwd = Some("/home/w/dev/cypher".into());
    parent.branch = Some("cypher/side".into());
    parent.checkout_id = Some("co-1".into());
    parent.space_id = Some("s1".into());
    parent.config = Some(cypher_proto::ChatConfig {
        harness: HarnessId::Pi,
        model: Some("claude-fable-5".into()),
        reasoning: Some(cypher_proto::ReasoningLevel::High),
        model_options: serde_json::Map::new(),
        sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
    });
    let row = side_chat_synthetic_row(&parent, "side-1", "remote-dev");
    assert_eq!(row.id, "side-1");
    assert_eq!(row.device_id, "remote-dev");
    assert_eq!(row.cwd.as_deref(), Some("/home/w/dev/cypher"));
    assert_eq!(row.branch.as_deref(), Some("cypher/side"));
    assert_eq!(row.checkout_id.as_deref(), Some("co-1"));
    assert_eq!(row.space_id.as_deref(), Some("s1"));
    assert_eq!(row.config, parent.config);
    // It is its OWN row — no public row exists until promotion.
    assert_ne!(row.id, parent.id);
    assert!(!row.archived && row.last_message_at.is_none());
}

#[test]
fn side_chat_status_projects_into_sessions_by_id() {
    // The private WatchSideChatStatus frames upsert into `sessions` so
    // the reused status logic (indicator_for / run_live) works.
    let mut state = AppState::new();
    let status =
        |s: cypher_proto::SessionStatus, at: chrono::DateTime<chrono::Utc>| -> SideChatStatus {
            SideChatStatus {
                side_chat_id: "side-1".into(),
                status: s,
                started_at: Some(at),
                updated_at: at,
            }
        };
    let t0 = chrono::Utc::now();
    state.apply_side_chat_status(
        status(cypher_proto::SessionStatus::Working, t0),
        "remote-dev",
    );
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.sessions[0].chat_id, "side-1");
    assert_eq!(state.sessions[0].device_id, "remote-dev");
    assert_eq!(
        state.sessions[0].status,
        cypher_proto::SessionStatus::Working
    );
    // A later frame upserts, never duplicates.
    state.apply_side_chat_status(
        status(
            cypher_proto::SessionStatus::Idle,
            t0 + chrono::TimeDelta::seconds(1),
        ),
        "remote-dev",
    );
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.sessions[0].status, cypher_proto::SessionStatus::Idle);
    // The fork's indicator reads the projected row (no public session).
    assert_eq!(
        state.indicator_for("side-1", t0 + chrono::TimeDelta::seconds(2)),
        Indicator::None
    );
}

#[test]
fn visible_chats_filters_archived() {
    let mut state = AppState::new();
    let mut archived = chat("a", 0, Some(99));
    archived.archived = true;
    state.apply_chats(vec![archived, chat("b", 1, None)]);
    let visible: Vec<&str> = state.visible_chats().map(|c| c.id.as_str()).collect();
    assert_eq!(visible, ["b"]);
}

/// Cypher child subagent chats are hidden from the root sidebar/overview
/// (`visible_chats` / `overview_chats`), yet remain selectable: a selected
/// child still resolves through `selected_chat_row` (the Inspector
/// navigation path) and survives `apply_chats` (it stays in `self.chats`).
#[test]
fn child_chats_hidden_from_root_but_selected_row_works() {
    let mut state = AppState::new();
    let parent = chat("parent", 0, Some(10));
    let mut child = chat("child-1", 1, Some(11));
    child.child = Some(cypher_proto::ChildChat {
        parent_chat_id: "parent".into(),
        parent_run_id: "run-1".into(),
        agent: "planner".into(),
        task: "Plan the panel".into(),
        mode: cypher_proto::SubagentRunMode::Async,
        tool_call_id: None,
        profile: cypher_proto::ChildAgentProfile {
            system_prompt: "You are the planner.".into(),
            tools: vec![],
            model: None,
            thinking: None,
        },
    });
    let mut archived = chat("archived", 2, None);
    archived.archived = true;
    state.apply_chats(vec![parent, child, archived]);

    // Root lists exclude both children and archived rows.
    let visible: Vec<&str> = state.visible_chats().map(|c| c.id.as_str()).collect();
    assert_eq!(visible, ["parent"], "child chat hidden from root");
    let overview: Vec<&str> = state
        .overview_chats(
            DateTime::parse_from_rfc3339("2026-07-19T12:20:00Z")
                .unwrap()
                .to_utc(),
        )
        .into_iter()
        .map(|(_, c)| c.id.as_str())
        .collect();
    assert_eq!(overview, ["parent"], "child chat hidden from overview");

    // A selected child still resolves (Inspector navigation target).
    state.selected_chat = Some("child-1".into());
    assert_eq!(
        state.selected_chat_row().map(|c| c.id.as_str()),
        Some("child-1")
    );
    assert!(state.selected_chat_row().is_some_and(|c| c.is_child()));
    // And a later chats frame keeps it (apply_chats only clears a
    // selection whose row vanished from self.chats entirely).
    state.apply_chats(state.chats.clone());
    assert_eq!(state.selected_chat.as_deref(), Some("child-1"));
}

#[test]
fn echoes_show_until_doc_frame_confirms() {
    let mut state = AppState::new();
    state.selected_chat = Some("c1".into());
    let echo = SessionMessageEntry {
        id: "m1".into(),
        role: cypher_doc::MessageRole::User,
        device_id: "local".into(),
        ..crate::test_fixtures::entry()
    };
    state.push_echo("c1", echo.clone());
    // Duplicate pushes dedupe.
    state.push_echo("c1", echo.clone());
    assert_eq!(state.pending_echoes().len(), 1);
    // Frames without the id keep the echo.
    state.apply_transcript(vec![]);
    assert_eq!(state.pending_echoes().len(), 1);
    // The confirming frame prunes it.
    state.apply_transcript(vec![SessionMessageEntry {
        id: "m1".into(),
        ..echo.clone()
    }]);
    assert!(state.pending_echoes().is_empty());
    // Failure path: explicit removal.
    state.push_echo(
        "c1",
        SessionMessageEntry {
            id: "m2".into(),
            ..echo.clone()
        },
    );
    state.remove_echo("c1", "m2");
    assert!(state.pending_echoes().is_empty());
    // Echoes are per chat.
    state.push_echo(
        "other",
        SessionMessageEntry {
            id: "m3".into(),
            ..echo
        },
    );
    assert!(state.pending_echoes().is_empty());
}

#[test]
fn gate_phases() {
    let user = UserProfile {
        id: "u".into(),
        email: "w@example.com".into(),
        name: None,
        avatar_url: None,
    };
    assert_eq!(
        gate_phase(&ConnectionStatus::Connecting, None, None),
        GatePhase::Loading
    );
    assert_eq!(
        gate_phase(&ConnectionStatus::Failed("boom".into()), None, None),
        GatePhase::Failed("boom".into())
    );
    assert_eq!(
        gate_phase(
            &ConnectionStatus::Ready,
            Some(WorkspaceScope::Local),
            Some(&AuthState::SignedOut),
        ),
        GatePhase::Ready
    );
    assert_eq!(
        gate_phase(
            &ConnectionStatus::Ready,
            Some(WorkspaceScope::Synced),
            Some(&AuthState::SignedOut),
        ),
        GatePhase::SignIn
    );
    assert_eq!(
        gate_phase(
            &ConnectionStatus::Ready,
            Some(WorkspaceScope::Synced),
            Some(&AuthState::SignedIn {
                user: user.clone(),
                org_id: None
            })
        ),
        GatePhase::Ready
    );
    // No org yet → org gate.
    assert_eq!(
        gate_phase(
            &ConnectionStatus::Ready,
            Some(WorkspaceScope::Synced),
            Some(&AuthState::NeedsOrganization { user })
        ),
        GatePhase::OrgGate
    );
}

#[test]
fn auth_changes_do_not_change_a_local_runtime_scope_or_watches() {
    let mut state = AppState::new();
    state.workspace_scope = Some(WorkspaceScope::Local);
    state.watch_tasks.push(Task::ready(()));

    state.apply_auth(AuthState::NeedsOrganization {
        user: UserProfile {
            id: "u".into(),
            email: "w@example.com".into(),
            name: None,
            avatar_url: None,
        },
    });
    assert_eq!(state.workspace_scope, Some(WorkspaceScope::Local));
    assert_eq!(state.watch_tasks.len(), 1);

    state.apply_auth(AuthState::SignedIn {
        user: UserProfile {
            id: "u".into(),
            email: "w@example.com".into(),
            name: None,
            avatar_url: None,
        },
        org_id: Some("org-1".into()),
    });
    assert_eq!(state.workspace_scope, Some(WorkspaceScope::Local));
    assert_eq!(state.watch_tasks.len(), 1);
}

fn chat_with_cwd(id: &str, created_min: i64, cwd: Option<&str>) -> Chat {
    let mut c = chat(id, created_min, None);
    c.cwd = cwd.map(str::to_string);
    c
}

#[test]
fn project_labels_from_cwd() {
    assert_eq!(project_label(Some("/home/w/dev/cypher")), "cypher");
    assert_eq!(project_label(Some("/home/w/dev/cypher/")), "cypher");
    assert_eq!(project_label(None), "No project");
    assert_eq!(project_label(Some("   ")), "No project");
    assert_eq!(project_label(Some("/")), "/");
}

#[test]
fn grouped_sidebar_preserves_recency_order() {
    // Input is sidebar-sorted (most recent first).
    let chats = [
        chat_with_cwd("a", 9, Some("/dev/cypher")),
        chat_with_cwd("b", 8, Some("/dev/zed")),
        chat_with_cwd("c", 7, Some("/dev/cypher")),
        chat_with_cwd("d", 6, None),
    ];
    let groups = group_chats(chats.iter());
    let labels: Vec<&str> = groups.iter().map(|g| g.label.as_str()).collect();
    // Groups ordered by their most recent chat; rows keep order.
    assert_eq!(labels, ["cypher", "zed", "No project"]);
    let cypher_ids: Vec<&str> = groups[0].chats.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(cypher_ids, ["a", "c"]);
    assert!(group_chats(std::iter::empty()).is_empty());
}

#[test]
fn relative_times_match_cypher_format() {
    let now = Utc::now();
    let ago = |secs: i64| now - chrono::Duration::seconds(secs);
    assert_eq!(format_time_ago(ago(0), now), "now");
    assert_eq!(format_time_ago(ago(59), now), "now");
    assert_eq!(format_time_ago(ago(60), now), "1m");
    assert_eq!(format_time_ago(ago(59 * 60), now), "59m");
    assert_eq!(format_time_ago(ago(60 * 60), now), "1h");
    assert_eq!(format_time_ago(ago(23 * 3600 + 3599), now), "23h");
    assert_eq!(format_time_ago(ago(24 * 3600), now), "1d");
    assert_eq!(format_time_ago(ago(6 * 86400), now), "6d");
    assert_eq!(format_time_ago(ago(7 * 86400), now), "1w");
    assert_eq!(format_time_ago(ago(30 * 86400), now), "4w");
    assert_eq!(format_time_ago(ago(35 * 86400), now), "1mo");
    assert_eq!(format_time_ago(ago(400 * 86400), now), "1y");
    // Clock skew (future timestamps) clamps to "now".
    assert_eq!(
        format_time_ago(now + chrono::Duration::hours(2), now),
        "now"
    );
}

#[test]
fn chat_location_joins_project_and_branch() {
    let mut c = chat_with_cwd("x", 1, Some("/home/w/dev/soccertcg"));
    c.branch = Some("cypher/rebalance".into());
    assert_eq!(
        chat_location(&c).as_deref(),
        Some("soccertcg · cypher/rebalance")
    );
    c.branch = None;
    assert_eq!(chat_location(&c).as_deref(), Some("soccertcg"));
    c.cwd = None;
    c.branch = Some("main".into());
    assert_eq!(chat_location(&c).as_deref(), Some("main"));
    c.branch = Some("   ".into());
    assert_eq!(chat_location(&c), None);
    c.branch = None;
    assert_eq!(chat_location(&c), None);
}

#[test]
fn org_gate_reducers() {
    assert_eq!(org_setup(vec![]), OrgSetup::AutoCreate);
    assert_eq!(
        org_setup(vec![OrgRow {
            organization_id: "only".into(),
            name: "Personal".into(),
        }]),
        OrgSetup::AutoSelect("only".into())
    );

    let rows = parse_orgs(&serde_json::json!({ "orgs": [
        { "id": "m2", "organizationId": "o2", "name": "beta" },
        { "id": "m1", "organizationId": "o1", "name": "Alpha" },
        { "id": "m3", "organizationId": "o1", "name": "Alpha" },
    ]}));
    assert_eq!(rows.len(), 3);
    let sorted = sort_memberships(rows);
    let names: Vec<&str> = sorted.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(
        names,
        ["Alpha", "beta"],
        "case-insensitive sort + dedupe by org id"
    );
    let rows = parse_orgs(&serde_json::json!({ "orgs": [
        { "organizationId": "o2", "name": "beta" },
        { "organizationId": "o1", "name": "Alpha" },
    ]}));
    assert!(matches!(org_setup(rows), OrgSetup::Pick(rows) if
        rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>() == ["Alpha", "beta"]
    ));
    // Bare-array replies parse too; garbage yields empty.
    assert_eq!(
        parse_orgs(&serde_json::json!([{ "id": "m", "organizationId": "o", "name": "n" }])).len(),
        1
    );
    assert!(parse_orgs(&serde_json::json!("nope")).is_empty());
}

fn run_command(
    id: &str,
    message_id: &str,
    issued_at: i64,
    status: SessionCommandStatus,
) -> SessionCommandEntry {
    SessionCommandEntry {
        id: id.into(),
        payload: SessionCommandPayload::Run {
            request: cypher_proto::RunRequest {
                prompt: format!("prompt-{id}"),
                harness: None,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                cwd: "/tmp".into(),
                sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
                auto_approve: false,
                attachments: Vec::new(),
                pending_attachments: Vec::new(),
                resume: None,
                worktree: None,
            },
            message_id: message_id.into(),
            agent_prompt: None,
        },
        issued_by: "dev".into(),
        issued_at,
        based_on: None,
        expires_at: None,
        status,
        resolution: Some("nope".into()),
        sent_at: Some(issued_at),
    }
}

#[test]
fn command_send_status_projects_durable_truth() {
    // No command for the message: no status.
    assert_eq!(command_send_status(&[], "m1"), None);
    // Pending first attempt = Queued.
    let queued = run_command("c1", "m1", 1, SessionCommandStatus::Pending);
    assert_eq!(
        command_send_status(std::slice::from_ref(&queued), "m1"),
        Some(CommandSendStatus::Queued)
    );
    // Applied is resolved — nothing for the UI to show.
    let applied = run_command("c1", "m1", 1, SessionCommandStatus::Applied);
    assert_eq!(command_send_status(&[applied], "m1"), None);
    // Rejected / Expired = Failed.
    let rejected = run_command("c1", "m1", 1, SessionCommandStatus::Rejected);
    assert_eq!(
        command_send_status(std::slice::from_ref(&rejected), "m1"),
        Some(CommandSendStatus::Failed)
    );
    let expired = run_command("c1", "m1", 1, SessionCommandStatus::Expired);
    assert_eq!(
        command_send_status(&[expired], "m1"),
        Some(CommandSendStatus::Failed)
    );
    // A live pending attempt AFTER a failure = Retrying.
    let retry = run_command("c2", "m1", 2, SessionCommandStatus::Pending);
    assert_eq!(
        command_send_status(&[rejected, retry], "m1"),
        Some(CommandSendStatus::Retrying)
    );
}

#[test]
fn failed_commands_lists_only_retryable_latest_failures() {
    let rejected = run_command("c1", "m1", 1, SessionCommandStatus::Rejected);
    let retry_pending = run_command("c2", "m1", 2, SessionCommandStatus::Pending);
    let rejected_again = run_command("c3", "m1", 3, SessionCommandStatus::Rejected);
    let expired_other = run_command("c4", "m2", 4, SessionCommandStatus::Expired);
    let applied_other = run_command("c5", "m3", 5, SessionCommandStatus::Applied);
    let commands = vec![
        rejected,
        retry_pending,
        rejected_again,
        expired_other,
        applied_other,
    ];
    let failed = failed_commands(&commands);
    // m1: the retry is in flight (skip); m3 applied (skip); m2: expired.
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].command_id, "c4");
    assert_eq!(failed[0].resolution.as_deref(), Some("nope"));

    // After the retry fails again, the LATEST failed attempt is the target.
    let commands = vec![
        run_command("c1", "m1", 1, SessionCommandStatus::Rejected),
        run_command("c2", "m1", 2, SessionCommandStatus::Rejected),
    ];
    let failed = failed_commands(&commands);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].command_id, "c2");
    assert_eq!(failed[0].prompt, "prompt-c2");
}

#[test]
fn failed_command_lifts_echo_veil_and_ends_send_overlay() {
    let now = Utc::now();
    let mut s = AppState::new();
    s.selected_chat = Some("c".into());
    s.begin_pending_send("c", "m1", now);
    // A fresh pending command keeps the echo pending + the overlay.
    s.apply_commands(vec![run_command(
        "c1",
        "m1",
        1,
        SessionCommandStatus::Pending,
    )]);
    assert!(s.echo_pending("m1"));
    assert!(s.send_pending("c", now));
    // The durable Rejected ends both: full-opacity echo + truthful dot.
    s.apply_commands(vec![run_command(
        "c1",
        "m1",
        1,
        SessionCommandStatus::Rejected,
    )]);
    assert!(!s.echo_pending("m1"));
    assert!(!s.send_pending("c", now));
    assert_eq!(s.failed_commands().len(), 1);
    // A retry in flight re-arms the echo veil (the message is sending again).
    s.apply_commands(vec![
        run_command("c1", "m1", 1, SessionCommandStatus::Rejected),
        run_command("c2", "m1", 2, SessionCommandStatus::Pending),
    ]);
    assert!(s.echo_pending("m1"));
    assert!(s.failed_commands().is_empty());
}

#[test]
fn steer_ids_join_ledger_steers_and_local_echoes() {
    let mut s = AppState::new();
    s.selected_chat = Some("c".into());
    let mut steer = run_command("c2", "ignored", 2, SessionCommandStatus::Applied);
    steer.payload = SessionCommandPayload::Steer {
        prompt: "nudge".into(),
        message_id: Some("m2".into()),
        agent_prompt: None,
    };
    s.apply_commands(vec![
        run_command("c1", "m1", 1, SessionCommandStatus::Applied),
        steer,
    ]);
    s.mark_steer("m3");
    let ids = s.steer_message_ids();
    assert!(!ids.contains("m1"), "a Run is a plain prompt");
    assert!(ids.contains("m2"), "the ledger's Steer message id");
    assert!(ids.contains("m3"), "this device's unsynced steer echo");
}
