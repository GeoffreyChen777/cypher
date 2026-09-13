//! Native workspace authority, control transport and replaceable availability.
use super::*;
use cypher_proto::metadata::{OpKind, RowOp};
use cypher_sync::workspace3::{
    client::{Client, Event},
    journal::Journal,
    wire::Scope,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

fn doc_error(e: impl std::fmt::Display) -> MetadataError {
    MetadataError(e.to_string())
}
struct Presence {
    connection: String,
    expires: i64,
    sessions: Vec<Session>,
}
pub(super) struct State {
    journal: Arc<Mutex<Journal>>,
    client: Option<Arc<Client>>,
    retired: AtomicBool,
    local_sessions: Mutex<HashMap<String, Session>>,
    peers: Mutex<HashMap<String, Presence>>,
    rpc: Mutex<Option<Arc<cypher_rpc::workspace3::Host>>>,
    caller: Option<Arc<cypher_rpc::workspace3::Caller>>,
    demand: Mutex<(Vec<String>, Option<Arc<dyn Fn(Vec<String>) + Send + Sync>>)>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl State {
    pub fn retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }
    pub fn attach_rpc(&self, service: Weak<dyn cypher_rpc::RpcService>) {
        if let Some(client) = &self.client {
            *lock(&self.rpc) = Some(Arc::new(cypher_rpc::workspace3::Host::new(
                client.clone(),
                service,
            )));
        }
    }
    pub fn rpc_caller(&self) -> Option<Arc<cypher_rpc::workspace3::Caller>> {
        self.caller.clone()
    }
    pub fn disconnect(&self) {
        if let Some(caller) = &self.caller {
            caller.disconnected();
        }
        if let Some(rpc) = lock(&self.rpc).as_ref() {
            rpc.disconnected();
        }
        if let Some(client) = &self.client {
            client.disconnect();
        }
    }
    pub fn probe(&self) {
        if let Some(client) = &self.client {
            client.probe();
        }
    }
    pub fn set_demand(&self, hook: Arc<dyn Fn(Vec<String>) + Send + Sync>) {
        let wanted = {
            let mut demand = lock(&self.demand);
            demand.1 = Some(hook.clone());
            demand.0.clone()
        };
        hook(wanted);
    }
    pub async fn shutdown(&self) {
        self.retired.store(true, Ordering::Release);
        if let Some(client) = &self.client {
            client.disconnect();
        }
        let rpc = lock(&self.rpc).take();
        if let Some(rpc) = rpc {
            rpc.shutdown().await;
        }
        if let Some(caller) = &self.caller {
            caller.shutdown().await;
        }
        if let Some(stop) = lock(&self.stop).take() {
            let _ = stop.send(());
        }
        if let Some(client) = &self.client {
            client.shutdown().await;
        }
        let task = lock(&self.task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
    pub fn sessions(&self, inner: &WorkspaceHostInner) -> Vec<Session> {
        let local: Vec<_> = lock(&self.local_sessions).values().cloned().collect();
        let now = now_ms();
        let doc = lock(&inner.reg);
        let mut sessions: Vec<_> = local
            .into_iter()
            .filter(|s| {
                // A bridge projection can precede chat metadata. Suppress known
                // deletions, not a legitimate not-yet-indexed session.
                !lock(&self.journal)
                    .row("chats", &s.chat_id)
                    .ok()
                    .flatten()
                    .is_some_and(|r| r.deleted)
            })
            .collect();
        for (actor, peer) in lock(&self.peers).iter().filter(|(_, p)| p.expires > now) {
            sessions.extend(
                peer.sessions
                    .iter()
                    .filter(|s| {
                        s.device_id == *actor
                            && doc
                                .chat(&s.chat_id)
                                .ok()
                                .flatten()
                                .is_some_and(|c| c.device_id == *actor)
                    })
                    .cloned(),
            );
        }
        sessions.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        sessions
    }
    pub fn record(&self, session: &Session) {
        lock(&self.local_sessions).insert(session.chat_id.clone(), session.clone());
        if let Some(client) = &self.client {
            let sessions: Vec<_> = lock(&self.local_sessions)
                .values()
                .filter(|s| {
                    matches!(
                        s.status,
                        cypher_proto::SessionStatus::Working
                            | cypher_proto::SessionStatus::AwaitingInput
                    ) || !s.subagents.is_empty()
                })
                .cloned()
                .collect();
            if let Err(error) = client.replace_presence(serde_json::json!({"sessions":sessions})) {
                tracing::warn!(%error, "workspace presence update unavailable");
            }
        }
    }
    pub fn sync_status(&self) -> Option<cypher_sync::RoomStatsSnapshot> {
        self.client.as_ref().map(|client| {
            let s = client.watch().borrow().clone();
            cypher_sync::RoomStatsSnapshot {
                connected: s.connected,
                server_known: s.caught_up,
                ..Default::default()
            }
        })
    }
    pub fn overlay_presence(&self, inner: &WorkspaceHostInner, devices: &mut [Device]) {
        let now = now_ms();
        let live: std::collections::HashSet<_> = lock(&self.peers)
            .iter()
            .filter(|(_, p)| p.expires > now)
            .map(|(actor, _)| actor.clone())
            .collect();
        let hook = lock(&inner.peer_alive).clone();
        for device in devices {
            device.last_seen_at = if device.id == inner.config.device_id {
                chrono::DateTime::<Utc>::from_timestamp_millis(now)
            } else if live.contains(&device.id) {
                if let Some(hook) = &hook {
                    hook(&device.id);
                }
                chrono::DateTime::<Utc>::from_timestamp_millis(now)
            } else {
                None
            };
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.retired.store(true, Ordering::Release);
        self.disconnect();
        if let Some(stop) = lock(&self.stop).take() {
            let _ = stop.send(());
        }
    }
}

fn load(journal: &Journal, actor: &str) -> Result<MetadataView, MetadataError> {
    let mut doc = MetadataView::new(actor);
    for kind in ["devices", "spaces", "chats"] {
        let mut after = String::new();
        loop {
            let page = journal.window(kind, &after, 32).map_err(doc_error)?;
            let Some(next) = page.next else {
                break;
            };
            doc.replace_rows(page.rows);
            after = next;
        }
    }
    Ok(doc)
}
pub(super) fn read_profile(
    root: &std::path::Path,
    scope: Scope,
) -> Result<Option<MetadataView>, EngineError> {
    let digest =
        Sha256::digest(serde_json::to_vec(&scope).map_err(|e| EngineError::Other(e.to_string()))?);
    let path = root.join(format!("workspace3-{digest:x}.sqlite"));
    if !path.exists() {
        return Ok(None);
    }
    let actor = scope.actor.clone();
    let journal = Journal::open(&path, scope).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(Some(load(&journal, &actor)?))
}
pub(super) fn mutate<R>(
    inner: &WorkspaceHostInner,
    f: impl FnOnce(&mut MetadataView) -> Result<R, MetadataError>,
) -> Result<R, MetadataError> {
    let state = &inner.v3;
    let mut current = lock(&inner.reg);
    if state.retired() {
        return Err(doc_error("runtime_retired"));
    }
    let mut draft = current.clone();
    let result = f(&mut draft)?;
    let operations: Vec<RowOp> = draft.take_operations();
    if !operations.is_empty() {
        let mut journal = lock(&state.journal);
        journal
            .mutate_many(&operations, now_ms().max(0) as u64)
            .map_err(doc_error)?;
        for op in &operations {
            if let Some(row) = journal.row(&op.kind, &op.id).map_err(doc_error)? {
                current.replace_rows([row]);
            }
            if op.kind == "chats" && op.op == OpKind::Delete {
                lock(&state.local_sessions).remove(&op.id);
            }
        }
        if let Some(client) = &state.client {
            client.nudge();
        }
    }
    drop(current);
    inner.bump_changed();
    Ok(result)
}

pub(super) fn open(
    store: Arc<DocsStore>,
    config: WorkspaceHostConfig,
) -> Result<WorkspaceHost, EngineError> {
    let scope = Scope {
        endpoint: config
            .edge
            .as_ref()
            .map_or_else(|| "local".into(), |e| e.url.clone()),
        org: config.org_id.clone(),
        user: config.user_id.clone(),
        actor: config.device_id.clone(),
    };
    let digest =
        Sha256::digest(serde_json::to_vec(&scope).map_err(|e| EngineError::Other(e.to_string()))?);
    let path = store
        .directory()?
        .join(format!("workspace3-{digest:x}.sqlite"));
    let journal = Journal::open(&path, scope).map_err(|e| EngineError::Other(e.to_string()))?;
    let doc = load(&journal, &config.device_id)?;
    let initial = doc.read_all()?;
    let journal = Arc::new(Mutex::new(journal));
    let (client, events) = if let Some(edge) = &config.edge {
        let (client, events) = Client::spawn_shared_policy(
            journal.clone(),
            edge.room_url(format!("/workspace3/{}/ws", config.org_id)),
            true,
            config.allow_device_rejoin,
        );
        (Some(Arc::new(client)), Some(events))
    } else {
        (None, None)
    };
    let (stop, stopped) = oneshot::channel();
    let state = Arc::new(State {
        journal,
        client: client.clone(),
        retired: AtomicBool::new(false),
        local_sessions: Mutex::new(HashMap::new()),
        rpc: Mutex::new(None),
        caller: client
            .as_ref()
            .map(|client| Arc::new(cypher_rpc::workspace3::Caller::new(client.clone()))),
        peers: Mutex::new(HashMap::new()),
        demand: Mutex::new((vec![], None)),
        stop: Mutex::new(Some(stop)),
        task: Mutex::new(None),
    });
    let (chats_tx, _) = watch::channel(initial.chats);
    let (devices_tx, _) = watch::channel(initial.devices);
    let (spaces_tx, _) = watch::channel(initial.spaces);
    let (sessions_tx, _) = watch::channel(vec![]);
    let (changed_tx, changed_rx) = watch::channel(0);
    let (evicted_tx, _) = watch::channel(false);
    let local = config.edge.is_none();
    let host = WorkspaceHost {
        inner: Arc::new(WorkspaceHostInner {
            v3: state.clone(),
            config,
            reg: Arc::new(Mutex::new(doc)),
            chats_tx,
            devices_tx,
            sessions_tx,
            spaces_tx,
            changed_tx,
            registry_synced: AtomicBool::new(local),
            evicted: AtomicBool::new(false),
            announced: AtomicBool::new(false),
            evicted_tx,
            peer_alive: Mutex::new(None),
            notification_event: Mutex::new(None),
        }),
    };
    if local {
        mutate(&host.inner, |doc| {
            announce_device(doc, &host.inner.config).map_err(doc_error)
        })?;
        host.inner.announced.store(true, Ordering::Release);
    }
    host.inner.publish();
    let weak = Arc::downgrade(&host.inner);
    *lock(&state.task) = Some(tokio::spawn(async move {
        tokio::select! { _ = stopped => {}, _ = run(weak, events, changed_rx) => {} }
    }));
    Ok(host)
}

async fn run(
    weak: Weak<WorkspaceHostInner>,
    mut events: Option<mpsc::Receiver<Event>>,
    mut changed: watch::Receiver<u64>,
) {
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(5));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let event = tokio::select! {
            result = changed.changed() => { if result.is_err() { break; } None },
            _ = timer.tick() => None,
            event = async { match &mut events {
                Some(events) => events.recv().await,
                None => std::future::pending().await,
            }} => { if event.is_none() { events = None; } event },
        };
        let Some(inner) = weak.upgrade() else {
            break;
        };
        let state = &inner.v3;
        if state.retired() {
            break;
        }
        match event {
            Some(Event::Frame { frame, .. }) if frame["type"] == "demand" => {
                if let Ok(wanted) = serde_json::from_value::<Vec<String>>(frame["chats"].clone()) {
                    let hook = {
                        let mut demand = lock(&state.demand);
                        demand.0 = wanted.clone();
                        demand.1.clone()
                    };
                    if let Some(hook) = hook {
                        hook(wanted);
                    }
                }
            }
            Some(Event::Disconnected { .. }) => {
                if let Some(caller) = &state.caller {
                    caller.disconnected();
                }
                if let Some(rpc) = lock(&state.rpc).as_ref() {
                    rpc.disconnected();
                }
                if state.client.as_ref().is_some_and(|c| {
                    c.watch()
                        .borrow()
                        .error
                        .as_ref()
                        .is_some_and(|e| e.contains("device_unpaired"))
                }) {
                    inner.mark_evicted();
                }
            }
            Some(Event::Metadata { keys }) => {
                let mut current = lock(&inner.reg);
                let updated = (|| -> Result<(), cypher_sync::sync3::Error> {
                    let journal = lock(&state.journal);
                    for (kind, id) in keys {
                        if let Some(row) = journal.row(&kind, &id)? {
                            current.replace_rows([row]);
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = updated {
                    tracing::error!(%error, "workspace view unavailable");
                    continue;
                }
                drop(current);
                let confirmed_gone = lock(&state.journal)
                    .canonical_row("devices", &inner.config.device_id)
                    .ok()
                    .flatten()
                    .is_some_and(|d| d.deleted);
                if confirmed_gone && !inner.config.allow_device_rejoin {
                    inner.mark_evicted();
                }
                if state
                    .client
                    .as_ref()
                    .is_some_and(|c| c.watch().borrow().caught_up)
                {
                    inner.registry_synced.store(true, Ordering::Release);
                    if !inner.announced.swap(true, Ordering::AcqRel) {
                        let gone = lock(&inner.reg).device_is_tombstoned(&inner.config.device_id);
                        if gone && !inner.config.allow_device_rejoin {
                            inner.mark_evicted();
                        } else if let Err(error) = mutate(&inner, |doc| {
                            announce_device(doc, &inner.config).map_err(doc_error)
                        }) {
                            tracing::error!(%error, "workspace announce failed");
                        }
                    } else if lock(&inner.reg).device_is_tombstoned(&inner.config.device_id) {
                        inner.mark_evicted();
                    }
                }
            }
            Some(Event::Frame { frame, .. }) if frame["type"] == "presence" => {
                if let (Some(actor), Some(connection), Some(expires)) = (
                    frame["actor"].as_str(),
                    frame["connection"].as_str(),
                    frame["expiresAt"].as_i64(),
                ) {
                    if actor != inner.config.device_id && expires > now_ms() {
                        let sessions = if frame["role"] == "host" {
                            serde_json::from_value::<Vec<Session>>(
                                frame["state"]["sessions"].clone(),
                            )
                            .unwrap_or_default()
                        } else {
                            vec![]
                        };
                        lock(&state.peers).insert(
                            actor.into(),
                            Presence {
                                connection: connection.into(),
                                expires,
                                sessions,
                            },
                        );
                    }
                }
            }
            Some(Event::Frame { frame, .. }) if frame["type"] == "peerClosed" => {
                if let (Some(actor), Some(connection)) =
                    (frame["actor"].as_str(), frame["connection"].as_str())
                {
                    let mut peers = lock(&state.peers);
                    if peers.get(actor).is_some_and(|p| p.connection == connection) {
                        peers.remove(actor);
                    }
                }
            }
            Some(Event::Frame { generation, frame }) => {
                let handled = state
                    .caller
                    .as_ref()
                    .is_some_and(|caller| caller.frame(generation, &frame))
                    || lock(&state.rpc)
                        .as_ref()
                        .is_some_and(|rpc| rpc.frame(generation, &frame));
                if !handled
                    && frame["type"] == "call"
                    && let Some(client) = &state.client
                {
                    let value = cypher_rpc::workspace3::codec::Encoder::new(
                        &serde_json::json!({"id":0,"err":"service_unavailable"}),
                    )
                    .expect("small error")
                    .next()
                    .unwrap();
                    let _ = client
                        .send(
                            generation,
                            serde_json::json!({"type":"reply","token":frame["token"],
                        "sequence":0,"done":true,"value":value}),
                        )
                        .await;
                }
            }
            _ => {}
        }
        inner.publish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn failed_native_commit_never_publishes_the_disposable_draft() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let root = store.directory().unwrap();
        let host = open(
            store,
            WorkspaceHostConfig {
                device_id: "host".into(),
                device_name: "Host".into(),
                platform: "test".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
                allow_device_rejoin: false,
            },
        )
        .unwrap();
        let file = std::fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_str()
                    .is_some_and(|s| s.starts_with("workspace3-") && s.ends_with(".sqlite"))
            })
            .unwrap();
        let db = rusqlite::Connection::open(file).unwrap();
        db.execute_batch("CREATE TRIGGER fail_second_metadata_row BEFORE INSERT ON workspace3_rows
            WHEN NEW.kind='chats' AND NEW.id='b' BEGIN SELECT RAISE(ABORT,'injected storage failure'); END;").unwrap();
        let before: (u64, u64, u64) = db
            .query_row(
                "SELECT cursor,clock_ms,clock_counter FROM workspace3_meta",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        let draft = |view: &mut MetadataView| {
            for id in ["a", "b"] {
                view.claim_chat(id, Some("/work"), None, Utc::now());
            }
            Ok(())
        };
        assert!(mutate(&host.inner, draft).is_err());
        assert!(host.chat("a").unwrap().is_none());
        assert!(host.chat("b").unwrap().is_none());
        let after: (u64, u64, u64) = db
            .query_row(
                "SELECT cursor,clock_ms,clock_counter FROM workspace3_meta",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(before, after);
        assert!(
            lock(&host.inner.v3.journal)
                .canonical_row("chats", "a")
                .unwrap()
                .is_none()
        );
        db.execute_batch("DROP TRIGGER fail_second_metadata_row")
            .unwrap();
        mutate(&host.inner, draft).unwrap();
        assert_eq!(lock(&host.inner.v3.journal).cursor().unwrap(), before.0 + 2);
        assert_eq!(host.read_chats().unwrap().len(), 2);
        host.shutdown_workers().await;
    }
    #[tokio::test]
    async fn activity_is_replaceable_and_does_not_write_metadata_history() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            WorkspaceHostConfig {
                device_id: "host".into(),
                device_name: "Host".into(),
                platform: "test".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
                allow_device_rejoin: false,
            },
        )
        .unwrap();
        let state = &host.inner.v3;
        let before = lock(&state.journal).cursor().unwrap();
        for _ in 0..100 {
            host.record_session(&Session {
                chat_id: "runtime".into(),
                device_id: "host".into(),
                status: cypher_proto::SessionStatus::Working,
                started_at: Some(Utc::now()),
                updated_at: Utc::now(),
                subagents: vec![],
            });
        }
        assert_eq!(lock(&state.journal).cursor().unwrap(), before);
        assert!(lock(&state.journal).pending().unwrap().is_none());
        assert_eq!(host.read_sessions().unwrap().len(), 1);
        host.shutdown_workers().await;
    }
}
