//! One reconnect owner for metadata, demand, presence and bounded RPC frames.
//! Only metadata is replayable. All transient frames carry a connection
//! generation and are rejected rather than replayed after a reconnect.
use super::{
    journal::Journal,
    wire::{self, FRAME_BYTES, Page, error},
};
use crate::{UrlProvider, sync3::Error};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub generation: u64,
    pub connected: bool,
    pub caught_up: bool,
    pub cursor: u64,
    pub error: Option<String>,
}
#[derive(Debug)]
pub enum Event {
    Connected { generation: u64, connection: String },
    Disconnected { generation: u64 },
    Metadata { keys: Vec<(String, String)> },
    Frame { generation: u64, frame: Value },
}
#[derive(Clone)]
struct Desired {
    version: u64,
    probe: bool,
    reconnect: bool,
    topics: Vec<String>,
    presence: Value,
}
struct Send {
    generation: u64,
    body: String,
    receipt: oneshot::Sender<Result<(), String>>,
}
pub struct Client {
    journal: Arc<Mutex<Journal>>,
    desired: Arc<Mutex<Desired>>,
    wake: Arc<Notify>,
    commands: mpsc::Sender<Send>,
    status: watch::Receiver<Status>,
    retired: Arc<AtomicBool>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    finished: watch::Receiver<bool>,
}
impl Client {
    /// Install the returned receiver before making this client available to
    /// RPC callers. A slow consumer is bounded and cannot silently lose calls.
    pub fn spawn(
        journal: Journal,
        url: Arc<dyn UrlProvider>,
        host: bool,
    ) -> (Self, mpsc::Receiver<Event>) {
        let journal = Arc::new(Mutex::new(journal));
        Self::spawn_shared(journal, url, host)
    }
    /// The runtime owns storage separately so it can finish private local
    /// bookkeeping after revoking network reachability during sign-out.
    pub fn spawn_shared(
        journal: Arc<Mutex<Journal>>,
        url: Arc<dyn UrlProvider>,
        host: bool,
    ) -> (Self, mpsc::Receiver<Event>) {
        Self::spawn_shared_policy(journal, url, host, false)
    }
    pub fn spawn_shared_policy(
        journal: Arc<Mutex<Journal>>,
        url: Arc<dyn UrlProvider>,
        host: bool,
        allow_rejoin: bool,
    ) -> (Self, mpsc::Receiver<Event>) {
        let desired = Arc::new(Mutex::new(Desired {
            version: 0,
            probe: false,
            reconnect: false,
            topics: vec![],
            presence: json!({}),
        }));
        let wake = Arc::new(Notify::new());
        let (commands, incoming) = mpsc::channel(32);
        let (events, receiver) = mpsc::channel(32);
        let (status_tx, status) = watch::channel(Status::default());
        let (stop, stopped) = oneshot::channel();
        let (finished_tx, finished) = watch::channel(false);
        let retired = Arc::new(AtomicBool::new(false));
        let stopped_flag = retired.clone();
        let mut actor = Actor {
            journal: journal.clone(),
            desired: desired.clone(),
            wake: wake.clone(),
            commands: incoming,
            events,
            status: status_tx,
            generation: 0,
            host,
            allow_rejoin,
        };
        let task = tokio::spawn(async move {
            tokio::select! { _ = stopped => {}, _ = actor.run(url) => {} }
            actor.status.send_modify(|s| {
                s.connected = false;
                s.caught_up = false;
            });
            stopped_flag.store(true, Ordering::Release);
            finished_tx.send_replace(true);
        });
        (
            Self {
                journal,
                desired,
                wake,
                commands,
                status,
                retired,
                stop: Mutex::new(Some(stop)),
                task: Mutex::new(Some(task)),
                finished,
            },
            receiver,
        )
    }
    pub fn watch(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }
    pub fn nudge(&self) {
        if !self.retired.load(Ordering::Acquire) {
            self.wake.notify_one();
        }
    }
    pub fn probe(&self) {
        if !self.retired.load(Ordering::Acquire) {
            lock(&self.desired).probe = true;
            self.wake.notify_one();
        }
    }
    /// Retire transient RPC routes, but retain safe metadata outbox replay.
    pub fn reconnect(&self) {
        if !self.retired.load(Ordering::Acquire) {
            lock(&self.desired).reconnect = true;
            self.wake.notify_one();
        }
    }
    pub fn read<T>(&self, read: impl FnOnce(&Journal) -> Result<T, Error>) -> Result<T, Error> {
        let journal = lock(&self.journal);
        if self.retired.load(Ordering::Acquire) {
            return Err(error("runtime_retired"));
        }
        read(&journal)
    }
    pub fn write<T>(
        &self,
        write: impl FnOnce(&mut Journal) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut journal = lock(&self.journal);
        if self.retired.load(Ordering::Acquire) {
            return Err(error("runtime_retired"));
        }
        let value = write(&mut journal)?;
        self.wake.notify_one();
        Ok(value)
    }
    pub fn replace_presence(&self, state: Value) -> Result<(), Error> {
        if self.retired.load(Ordering::Acquire) {
            return Err(error("runtime_retired"));
        }
        wire::value(&state, 0)?;
        if !state.is_object() || serde_json::to_vec(&state)?.len() > 64 * 1024 {
            return Err(error("invalid_presence"));
        }
        let mut desired = lock(&self.desired);
        if self.retired.load(Ordering::Acquire) {
            return Err(error("runtime_retired"));
        }
        desired.presence = state;
        desired.version = desired
            .version
            .checked_add(1)
            .ok_or_else(|| error("sequence_exhausted"))?;
        self.wake.notify_one();
        Ok(())
    }
    pub fn watch_chats(&self, chats: Vec<String>) -> Result<(), Error> {
        if self.retired.load(Ordering::Acquire) {
            return Err(error("runtime_retired"));
        }
        if chats.len() > 8 || !chats.iter().all(|c| wire::id(c)) {
            return Err(error("invalid_topics"));
        }
        let mut desired = lock(&self.desired);
        if self.retired.load(Ordering::Acquire) {
            return Err(error("runtime_retired"));
        }
        desired.topics = chats;
        desired.version = desired
            .version
            .checked_add(1)
            .ok_or_else(|| error("sequence_exhausted"))?;
        self.wake.notify_one();
        Ok(())
    }
    /// Success means locally sent, not remotely delivered/executed. The caller
    /// must await routed/reply, honor credits, and report uncertain disconnects.
    pub async fn send(&self, generation: u64, mut frame: Value) -> Result<(), Error> {
        if self.retired.load(Ordering::Acquire) {
            return Err(error("runtime_retired"));
        }
        let status = self.status.borrow().clone();
        if !status.connected || status.generation != generation {
            return Err(error("connection_retired"));
        }
        wire::value(&frame, 0)?;
        let object = frame
            .as_object_mut()
            .ok_or_else(|| error("invalid_frame"))?;
        if !["call", "reply", "ack", "cancel", "input", "inputAck"]
            .contains(&object.get("type").and_then(Value::as_str).unwrap_or(""))
        {
            return Err(error("invalid_control_frame"));
        }
        object.insert("version".into(), json!(3));
        let body = serde_json::to_string(&frame)?;
        if body.len() > FRAME_BYTES {
            return Err(error("frame_too_large"));
        }
        let (receipt, result) = oneshot::channel();
        let send = Send {
            generation,
            body,
            receipt,
        };
        tokio::time::timeout(Duration::from_secs(30), self.commands.send(send))
            .await
            .map_err(|_| error("control_backpressure"))?
            .map_err(|_| error("runtime_retired"))?;
        tokio::time::timeout(Duration::from_secs(30), result)
            .await
            .map_err(|_| error("delivery_unknown"))?
            .map_err(|_| error("delivery_unknown"))?
            .map_err(|code| error(&code))
    }
    pub async fn shutdown(&self) {
        self.disconnect();
        // Fence any write that was already in the synchronous critical section.
        drop(lock(&self.journal));
        if let Some(stop) = lock(&self.stop).take() {
            let _ = stop.send(());
        }
        let task = lock(&self.task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
        let mut finished = self.finished.clone();
        while !*finished.borrow_and_update() {
            if finished.changed().await.is_err() {
                break;
            }
        }
    }
    pub fn disconnect(&self) {
        self.retired.store(true, Ordering::Release);
        if let Some(stop) = lock(&self.stop).take() {
            let _ = stop.send(());
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.retired.store(true, Ordering::Release);
        if let Some(stop) = lock(&self.stop).take() {
            let _ = stop.send(());
        }
    }
}
struct Actor {
    journal: Arc<Mutex<Journal>>,
    desired: Arc<Mutex<Desired>>,
    wake: Arc<Notify>,
    commands: mpsc::Receiver<Send>,
    events: mpsc::Sender<Event>,
    status: watch::Sender<Status>,
    generation: u64,
    host: bool,
    allow_rejoin: bool,
}
impl Actor {
    async fn event(&self, event: Event) -> Result<(), Error> {
        tokio::time::timeout(Duration::from_secs(30), self.events.send(event))
            .await
            .map_err(|_| error("consumer_backpressure"))?
            .map_err(|_| error("consumer_closed"))
    }
    async fn run(&mut self, url: Arc<dyn UrlProvider>) {
        let mut backoff = Duration::from_millis(250);
        loop {
            self.generation += 1;
            self.status.send_modify(|s| {
                s.generation = self.generation;
                s.connected = false;
                s.caught_up = false;
            });
            let started = tokio::time::Instant::now();
            let failure = self
                .session(url.clone())
                .await
                .err()
                .unwrap_or_else(|| error("transport_unavailable"));
            self.status.send_modify(|s| {
                s.connected = false;
                s.caught_up = false;
                s.error = Some(failure.to_string());
            });
            if self
                .event(Event::Disconnected {
                    generation: self.generation,
                })
                .await
                .is_err()
            {
                return;
            }
            if !matches!(&failure, Error::Protocol(code) if ["transport_unavailable","business_timeout","reauth_required"].contains(&code.as_str()))
            {
                std::future::pending::<()>().await;
            }
            if started.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_millis(250);
            }
            tokio::time::sleep(
                backoff + Duration::from_millis(u64::from(uuid::Uuid::new_v4().as_bytes()[0])),
            )
            .await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }
    async fn session(&mut self, url: Arc<dyn UrlProvider>) -> Result<(), Error> {
        let request = tokio::time::timeout(Duration::from_secs(30), url.request())
            .await
            .map_err(|_| error("business_timeout"))?
            .map_err(|e| match e {
                crate::SyncError::Protocol(code) => Error::Protocol(code),
                crate::SyncError::Auth(_) => error("reauth_required"),
                _ => error("transport_unavailable"),
            })?;
        let scope = lock(&self.journal).scope().clone();
        let expected = format!(
            "{}/workspace3/{}/ws",
            scope
                .endpoint
                .trim_end_matches('/')
                .replacen("http", "ws", 1),
            scope.org
        );
        if request.uri().to_string() != expected {
            return Err(error("workspace_endpoint_mismatch"));
        }
        let mut config = WebSocketConfig::default();
        config.max_frame_size = Some(FRAME_BYTES);
        config.max_message_size = Some(FRAME_BYTES);
        let (mut ws, _) = tokio::time::timeout(
            Duration::from_secs(30),
            tokio_tungstenite::connect_async_with_config(request, Some(config), false),
        )
        .await
        .map_err(|_| error("business_timeout"))?
        .map_err(|_| error("transport_unavailable"))?;
        let after = lock(&self.journal).cursor()?;
        send(&mut ws, json!({ "version":3,"type":"hello","user":scope.user,"org":scope.org,"actor":scope.actor,
            "role":if self.host {"host"} else {"viewer"},"after":after })).await?;
        let mut welcome = false;
        let mut page_after = Some((after, tokio::time::Instant::now() + Duration::from_secs(30)));
        let mut head = after;
        let mut push: Option<(String, String, tokio::time::Instant)> = None;
        let mut applied_desired = None;
        let mut presence = tokio::time::interval(Duration::from_secs(15));
        presence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut clock = tokio::time::interval(Duration::from_secs(1));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut probe_deadline = None;
        let mut last_business = tokio::time::Instant::now();
        loop {
            if welcome {
                let desired = lock(&self.desired).clone();
                if desired.reconnect {
                    lock(&self.desired).reconnect = false;
                    return Err(error("transport_unavailable"));
                }
                if desired.probe && probe_deadline.is_none() {
                    lock(&self.desired).probe = false;
                    send(&mut ws, json!({"version":3,"type":"probe","id":"liveness"})).await?;
                    probe_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(30));
                }
                if applied_desired != Some(desired.version) {
                    send(
                        &mut ws,
                        json!({"version":3,"type":"watch","chats":desired.topics}),
                    )
                    .await?;
                    send(
                        &mut ws,
                        json!({"version":3,"type":"presence","state":desired.presence}),
                    )
                    .await?;
                    applied_desired = Some(desired.version);
                }
                let cursor = lock(&self.journal).cursor()?;
                if page_after.is_none() && cursor < head {
                    self.status.send_if_modified(|s| {
                        if s.caught_up {
                            s.caught_up = false;
                            true
                        } else {
                            false
                        }
                    });
                    send(&mut ws, json!({"version":3,"type":"page","after":cursor})).await?;
                    page_after = Some((
                        cursor,
                        tokio::time::Instant::now() + Duration::from_secs(30),
                    ));
                }
                if self.status.borrow().caught_up {
                    let own = lock(&self.journal).canonical_row("devices", &scope.actor)?;
                    if let Some(own) = own {
                        if !own.deleted {
                            self.allow_rejoin = false;
                        } else if !self.allow_rejoin {
                            return Err(error("device_unpaired"));
                        }
                    }
                }
                if push.is_none() && self.status.borrow().caught_up {
                    let pending = lock(&self.journal).pending()?;
                    if let Some(pending) = pending {
                        send_text(&mut ws, pending.request).await?;
                        push = Some((
                            pending.id,
                            pending.hash,
                            tokio::time::Instant::now() + Duration::from_secs(30),
                        ));
                    }
                }
            }
            tokio::select! {
                _ = self.wake.notified() => {},
                _ = clock.tick() => {
                    let now = tokio::time::Instant::now();
                    if page_after.as_ref().is_some_and(|(_, d)| now >= *d) ||
                        push.as_ref().is_some_and(|(_,_,d)| now >= *d) ||
                        probe_deadline.is_some_and(|d| now >= d) { return Err(error("business_timeout")); }
                    if welcome && last_business.elapsed() >= Duration::from_secs(900) && probe_deadline.is_none() {
                        send(&mut ws, json!({"version":3,"type":"probe","id":"liveness"})).await?;
                        probe_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(30));
                    }
                },
                _ = presence.tick(), if welcome => {
                    let state = lock(&self.desired).presence.clone();
                    send(&mut ws, json!({"version":3,"type":"presence","state":state})).await?;
                },
                Some(command) = self.commands.recv() => {
                    if command.receipt.is_closed() { continue; }
                    if !welcome || command.generation != self.generation {
                        let _ = command.receipt.send(Err("connection_retired".into()));
                        continue;
                    }
                    match send_text(&mut ws, command.body).await {
                        Ok(()) => { let _ = command.receipt.send(Ok(())); },
                        Err(e) => { let _ = command.receipt.send(Err("delivery_unknown".into())); return Err(e); }
                    }
                },
                incoming = ws.next() => {
                    let incoming = incoming.ok_or_else(|| error("transport_unavailable"))?.map_err(|_| error("transport_unavailable"))?;
                    let text = match incoming {
                        Message::Text(s) if s == "pong" => continue,
                        Message::Text(s) => s,
                        Message::Ping(_) | Message::Pong(_) => continue,
                        Message::Close(_) => return Err(error("transport_unavailable")),
                        _ => return Err(error("invalid_frame")),
                    };
                    if text.len() > FRAME_BYTES { return Err(error("frame_too_large")); }
                    let frame: Value = serde_json::from_str(&text)?;
                    wire::value(&frame, 0)?;
                    wire::frame(&frame)?;
                    if frame["version"].as_u64() != Some(3) { return Err(error("upgrade_required")); }
                    let kind = frame["type"].as_str().ok_or_else(|| error("invalid_frame"))?;
                    last_business = tokio::time::Instant::now();
                    match kind {
                        "welcome" | "page" => {
                            let mut connected = None;
                            if kind == "welcome" {
                                if welcome || frame["user"].as_str() != Some(scope.user.as_str()) ||
                                    frame["org"].as_str() != Some(scope.org.as_str()) { return Err(error("account_mismatch")); }
                                let connection = frame["connection"].as_str().filter(|s| wire::id(s)).ok_or_else(|| error("invalid_connection"))?.to_string();
                                connected = Some(connection);
                            } else if !welcome { return Err(error("hello_required")); }
                            let (after, _) = page_after.take().ok_or_else(|| error("unexpected_page"))?;
                            let page = Page { through: number(&frame,"through")?, next: number(&frame,"next")?,
                                done: frame["done"].as_bool().ok_or_else(|| error("invalid_page"))?, rows: wire::rows(&frame["rows"])? };
                            lock(&self.journal).apply_page(after, &page)?;
                            if let Some(connection) = connected {
                                welcome = true;
                                self.status.send_modify(|s| { s.connected = true; s.error = None; });
                                self.event(Event::Connected { generation: self.generation, connection }).await?;
                            }
                            head = head.max(page.through);
                            self.status.send_modify(|s| { s.cursor = page.next; s.caught_up = page.done; });
                            self.event(Event::Metadata { keys: page.rows.iter().map(|r| (r.kind.clone(),r.id.clone())).collect() }).await?;
                        },
                        "pushed" => {
                            let (id, hash, _) = push.as_ref().ok_or_else(|| error("unexpected_ack"))?;
                            if frame["id"].as_str() != Some(id) || frame["requestHash"].as_str() != Some(hash) { return Err(error("workspace_ack_mismatch")); }
                            let through = number(&frame,"through")?;
                            let rows = wire::rows(&frame["rows"])?;
                            lock(&self.journal).acknowledge(id, hash, through, &rows)?;
                            head = head.max(through);
                            push = None;
                            self.event(Event::Metadata { keys: rows.iter().map(|r| (r.kind.clone(),r.id.clone())).collect() }).await?;
                        },
                        "changed" => head = head.max(number(&frame,"through")?),
                        "probeOk" => {
                            if probe_deadline.take().is_none() || frame["id"] != "liveness" { return Err(error("unexpected_probe")); }
                            head = head.max(number(&frame,"through")?);
                        },
                        "error" => {
                            let code = frame["code"].as_str().unwrap_or("invalid_error");
                            if code == "reauth_required" || !welcome || push.as_ref().is_some_and(|(id,_,_)| frame["id"].as_str() == Some(id.as_str())) {
                                return Err(error(code));
                            }
                            self.event(Event::Frame { generation: self.generation, frame }).await?;
                        },
                        "demand" | "presence" | "peerClosed" | "routed" | "reply" | "credit" | "cancel" | "call" | "watching" | "input" | "inputCredit" => {
                            if !welcome { return Err(error("hello_required")); }
                            if kind == "call" && !self.host { return Err(error("not_rpc_host")); }
                            self.event(Event::Frame { generation: self.generation, frame }).await?;
                        },
                        _ => return Err(error("unknown_message")),
                    }
                },
            }
        }
    }
}
fn number(frame: &Value, field: &str) -> Result<u64, Error> {
    frame[field]
        .as_u64()
        .filter(|v| *v <= wire::MAX_SAFE)
        .ok_or_else(|| error("invalid_number"))
}
async fn send(ws: &mut crate::dial::WsStream, value: Value) -> Result<(), Error> {
    send_text(ws, serde_json::to_string(&value)?).await
}
async fn send_text(ws: &mut crate::dial::WsStream, body: String) -> Result<(), Error> {
    if body.len() > FRAME_BYTES {
        return Err(error("frame_too_large"));
    }
    tokio::time::timeout(Duration::from_secs(30), ws.send(Message::Text(body.into())))
        .await
        .map_err(|_| error("business_timeout"))?
        .map_err(|_| error("transport_unavailable"))
}
