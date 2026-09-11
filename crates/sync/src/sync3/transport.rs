//! One v3 connection owner. HTTP is entered only after a failed WS session,
//! never as a side effect of enqueueing an operation on a healthy connection.
use super::{Error, Journal, Phase, invalid};
use crate::UrlProvider;
use cypher_proto::sync3::{self as wire, Operation, Reply, Request};
use futures::{SinkExt, StreamExt, future::BoxFuture};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

pub trait RepairTransport: Send + Sync + 'static {
    /// A fresh credential must be resolved for every exchange. The actor
    /// serializes these calls; implementations must not independently retry.
    fn exchange(&self, request: Request) -> BoxFuture<'static, Result<Reply, Error>>;
}
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub phase: Phase,
    pub generation: u64,
    pub cursor: u64,
    pub repairs: u64,
    pub error: Option<String>,
}
#[derive(Clone, Copy)]
pub struct Tuning {
    pub deadline: Duration,
    pub probe_interval: Duration,
    pub retry_base: Duration,
    pub retry_cap: Duration,
}
impl Default for Tuning {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(15),
            probe_interval: Duration::from_secs(60),
            retry_base: Duration::from_millis(500),
            retry_cap: Duration::from_secs(60),
        }
    }
}
fn lock(journal: &Mutex<Journal>) -> std::sync::MutexGuard<'_, Journal> {
    journal
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn recoverable(error: &Error) -> bool {
    matches!(error, Error::Protocol(code) if
        matches!(code.as_str(), "transport_unavailable" | "business_timeout" | "reauth_required"))
}
fn check_reply(reply: &Reply) -> Result<(), Error> {
    if let Reply::Error { version, code } = reply {
        return Err(invalid(if *version == wire::VERSION {
            code
        } else {
            "upgrade_required"
        }));
    }
    Ok(())
}

pub struct Client {
    journal: Arc<Mutex<Journal>>,
    nudge: mpsc::Sender<()>,
    shutdown: watch::Sender<bool>,
    status: watch::Receiver<Status>,
    task: tokio::task::JoinHandle<()>,
}
impl Client {
    pub fn spawn(
        journal: Journal,
        url: Arc<dyn UrlProvider>,
        repair: Option<Arc<dyn RepairTransport>>,
        tuning: Tuning,
    ) -> Self {
        let journal = Arc::new(Mutex::new(journal));
        let (nudge, rx) = mpsc::channel(1);
        let (shutdown, mut stop) = watch::channel(false);
        let initial = Status {
            phase: Phase::Connecting,
            generation: 0,
            cursor: lock(&journal).cursor().unwrap_or(0),
            repairs: 0,
            error: None,
        };
        let (status_tx, status) = watch::channel(initial);
        let actor_journal = journal.clone();
        let task = tokio::spawn(async move {
            let mut actor = Actor {
                journal: actor_journal,
                rx,
                status: status_tx,
                tuning,
            };
            tokio::select! {
                _ = stop.wait_for(|stop| *stop) => {}
                _ = actor.run(url, repair) => {}
            }
            actor
                .status
                .send_modify(|status| status.phase = Phase::Offline);
        });
        Self {
            journal,
            nudge,
            shutdown,
            status,
            task,
        }
    }
    /// Successful return means local durable enqueue only, not cloud ACK.
    pub fn enqueue(&self, operation: &Operation) -> Result<(), Error> {
        lock(&self.journal).enqueue(operation)?;
        let _ = self.nudge.try_send(());
        Ok(())
    }
    pub fn watch(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }
    pub fn journal(&self) -> Arc<Mutex<Journal>> {
        self.journal.clone()
    }
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        let _ = self.task.await;
    }
}
struct Actor {
    journal: Arc<Mutex<Journal>>,
    rx: mpsc::Receiver<()>,
    status: watch::Sender<Status>,
    tuning: Tuning,
}
impl Actor {
    fn publish(&self, phase: Phase, error: Option<String>) {
        let cursor = lock(&self.journal).cursor().unwrap_or(0);
        self.status.send_modify(|s| {
            s.phase = phase;
            s.cursor = cursor;
            s.error = error;
        });
    }
    async fn run(&mut self, url: Arc<dyn UrlProvider>, repair: Option<Arc<dyn RepairTransport>>) {
        let mut backoff = self.tuning.retry_base;
        loop {
            self.status.send_modify(|s| s.generation += 1);
            self.publish(Phase::Connecting, None);
            let started = tokio::time::Instant::now();
            let result = self.session(url.clone()).await;
            match result {
                Ok(()) => return,
                Err(error) => {
                    // Storage/semantic conflicts cannot be repaired by blindly
                    // resending. Park until shutdown; UI must surface recovery.
                    self.publish(Phase::Suspect, Some(error.to_string()));
                    if !recoverable(&error) {
                        std::future::pending::<()>().await;
                    }
                    if let Some(repair) = &repair {
                        self.publish(Phase::Repairing, None);
                        self.status.send_modify(|s| s.repairs += 1);
                        let result = tokio::time::timeout(
                            self.tuning.deadline * 2,
                            repair_once(&self.journal, repair.as_ref()),
                        )
                        .await;
                        if let Ok(Err(error)) = result {
                            self.publish(Phase::Suspect, Some(error.to_string()));
                            if !recoverable(&error) {
                                std::future::pending::<()>().await;
                            }
                        }
                    }
                    if started.elapsed() > self.tuning.probe_interval {
                        backoff = self.tuning.retry_base;
                    }
                    self.publish(Phase::Connecting, Some(error.to_string()));
                    // Jitter does not require mutable RNG state across await.
                    let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]);
                    tokio::time::sleep(backoff + Duration::from_millis(jitter)).await;
                    backoff = (backoff * 2).min(self.tuning.retry_cap);
                }
            }
        }
    }
    async fn session(&mut self, url: Arc<dyn UrlProvider>) -> Result<(), Error> {
        let url = tokio::time::timeout(self.tuning.deadline, url.url())
            .await
            .map_err(|_| invalid("business_timeout"))?
            .map_err(|_| invalid("transport_unavailable"))?;
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(wire::MAX_FRAME_BYTES);
        config.max_frame_size = Some(wire::MAX_FRAME_BYTES);
        let (mut socket, _) = tokio::time::timeout(
            self.tuning.deadline,
            tokio_tungstenite::connect_async_with_config(url, Some(config), false),
        )
        .await
        .map_err(|_| invalid("business_timeout"))?
        .map_err(|_| invalid("transport_unavailable"))?;
        let hello = lock(&self.journal).hello()?;
        send(&mut socket, &hello, self.tuning.deadline).await?;
        let mut ready = false;
        let mut head = 0;
        let mut handshake = Some(tokio::time::Instant::now() + self.tuning.deadline);
        let mut pull: Option<(u64, tokio::time::Instant)> = None;
        let mut push: Option<(Vec<String>, tokio::time::Instant)> = None;
        let mut probe_deadline = None;
        let mut probe = tokio::time::interval(self.tuning.probe_interval);
        probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        probe.tick().await;
        let mut keepalive = tokio::time::interval(Duration::from_secs(20));
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        keepalive.tick().await;
        loop {
            if ready {
                let cursor = lock(&self.journal).cursor()?;
                if cursor < head && pull.is_none() {
                    let epoch = lock(&self.journal).epoch()?;
                    send(
                        &mut socket,
                        &Request::Pull {
                            version: 3,
                            epoch,
                            after: cursor,
                            through: head,
                        },
                        self.tuning.deadline,
                    )
                    .await?;
                    pull = Some((head, tokio::time::Instant::now() + self.tuning.deadline));
                    self.publish(Phase::CatchingUp, None);
                }
                if cursor == head && pull.is_none() {
                    self.publish(Phase::Live, None);
                    if push.is_none() {
                        let operations = lock(&self.journal).pending()?;
                        if !operations.is_empty() {
                            let ids = operations.iter().map(|op| op.id.clone()).collect();
                            send(
                                &mut socket,
                                &Request::Push {
                                    version: 3,
                                    operations,
                                },
                                self.tuning.deadline,
                            )
                            .await?;
                            push = Some((ids, tokio::time::Instant::now() + self.tuning.deadline));
                        }
                    }
                }
            }
            let deadline = [
                handshake,
                pull.as_ref().map(|p| p.1),
                push.as_ref().map(|p| p.1),
                probe_deadline,
            ]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86_400));
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return Err(invalid("business_timeout")),
                _ = self.rx.recv() => {},
                _ = probe.tick(), if ready => {
                    send(&mut socket, &Request::Probe { version:3 }, self.tuning.deadline).await?;
                    probe_deadline.get_or_insert(tokio::time::Instant::now() + self.tuning.deadline);
                }
                _ = keepalive.tick() => {
                    tokio::time::timeout(self.tuning.deadline, socket.send(Message::text("ping"))).await
                        .map_err(|_|invalid("business_timeout"))?.map_err(|_|invalid("transport_unavailable"))?;
                }
                message = socket.next() => {
                    let message = message.ok_or_else(||invalid("transport_unavailable"))?
                        .map_err(|_|invalid("transport_unavailable"))?;
                    let text = match message {
                        Message::Text(text) if text == "pong" => continue,
                        Message::Text(text) => text,
                        Message::Ping(_) | Message::Pong(_) => continue,
                        Message::Close(_) => return Err(invalid("transport_unavailable")),
                        _ => return Err(invalid("expected_json")),
                    };
                    let reply:Reply = serde_json::from_str(&text)?;
                    check_reply(&reply)?;
                    match &reply {
                        Reply::State { head: new_head, .. } => {
                            lock(&self.journal).accept_state(&reply)?;
                            head = head.max(*new_head); ready = true; handshake = None; probe_deadline = None;
                        }
                        Reply::Ack { receipts, .. } => {
                            let Some((expected, _)) = &push else {return Err(invalid("unexpected_ack"));};
                            if expected.len()!=receipts.len() || expected.iter().zip(receipts).any(|(id,r)|id!=&r.id) {
                                return Err(invalid("receipt_conflict"));
                            }
                            lock(&self.journal).acknowledge(&reply)?;
                            head=head.max(receipts.iter().map(|r|r.seq).max().unwrap_or(0));
                            push=None;
                        }
                        Reply::Page { through, .. } => {
                            if pull.as_ref().map(|p|p.0)!=Some(*through) {return Err(invalid("unexpected_page"));}
                            lock(&self.journal).apply_page(&reply)?;pull=None;
                        }
                        Reply::Error { code, .. } => return Err(invalid(code)),
                    }
                }
            }
        }
    }
}

async fn send(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    frame: &Request,
    deadline: Duration,
) -> Result<(), Error> {
    let text = serde_json::to_string(frame)?;
    if text.len() > wire::MAX_FRAME_BYTES {
        return Err(invalid("frame_too_large"));
    }
    tokio::time::timeout(deadline, socket.send(Message::text(text)))
        .await
        .map_err(|_| invalid("business_timeout"))?
        .map_err(|_| invalid("transport_unavailable"))
}

/// One finite HTTP repair, at most eight exchanges. Progress is durable if the
/// budget ends midway. Subsequent attempts resume rather than start over.
async fn repair_once(
    journal: &Mutex<Journal>,
    transport: &dyn RepairTransport,
) -> Result<(), Error> {
    let hello = lock(journal).hello()?;
    let state = transport.exchange(hello).await?;
    check_reply(&state)?;
    let mut head = lock(journal).accept_state(&state)?;
    for _ in 0..7 {
        let (cursor, epoch) = {
            let j = lock(journal);
            (j.cursor()?, j.epoch()?)
        };
        if cursor < head {
            let page = transport
                .exchange(Request::Pull {
                    version: 3,
                    epoch,
                    after: cursor,
                    through: head,
                })
                .await?;
            check_reply(&page)?;
            lock(journal).apply_page(&page)?;
        } else {
            let operations = lock(journal).pending()?;
            if operations.is_empty() {
                return Ok(());
            }
            let ids: Vec<_> = operations.iter().map(|op| op.id.clone()).collect();
            let ack = transport
                .exchange(Request::Push {
                    version: 3,
                    operations,
                })
                .await?;
            check_reply(&ack)?;
            if !matches!(&ack, Reply::Ack { receipts, .. } if receipts.len()==ids.len() &&
                receipts.iter().zip(&ids).all(|(receipt,id)| &receipt.id==id))
            {
                return Err(invalid("receipt_conflict"));
            }
            lock(journal).acknowledge(&ack)?;
            if let Reply::Ack { receipts, .. } = ack {
                head = head.max(receipts.iter().map(|r| r.seq).max().unwrap_or(0));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wire::{Command, Event, Receipt, Row};

    struct Repair {
        calls: AtomicUsize,
        committed: Arc<Mutex<Vec<Operation>>>,
    }
    impl RepairTransport for Repair {
        fn exchange(&self, request: Request) -> BoxFuture<'static, Result<Reply, Error>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut rows = self.committed.lock().unwrap();
            let reply = match request {
                Request::Hello { .. } | Request::Probe { .. } => state(rows.len() as u64, 1),
                Request::Pull { after, through, .. } => Reply::Page {
                    version: 3,
                    epoch: 1,
                    through,
                    next: through,
                    done: true,
                    rows: rows
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i as u64 >= after && (*i as u64) < through)
                        .map(|(i, op)| Row {
                            seq: i as u64 + 1,
                            operation: op.clone(),
                        })
                        .collect(),
                },
                Request::Push { operations, .. } => {
                    let mut receipts = Vec::new();
                    for op in operations {
                        let seq = if let Some(i) = rows.iter().position(|old| old.id == op.id) {
                            i + 1
                        } else {
                            rows.push(op.clone());
                            rows.len()
                        };
                        receipts.push(Receipt {
                            id: op.id,
                            seq: seq as u64,
                        });
                    }
                    Reply::Ack {
                        version: 3,
                        epoch: 1,
                        receipts,
                    }
                }
            };
            Box::pin(async move { Ok(reply) })
        }
    }
    fn op() -> Operation {
        Operation {
            id: "one".into(),
            actor: "phone".into(),
            owner_epoch: 1,
            event: Event::CommandQueued {
                command_id: "cmd".into(),
                command: Command::Send {
                    text: "hello".into(),
                },
            },
        }
    }
    fn state(head: u64, epoch: u64) -> Reply {
        Reply::State {
            version: 3,
            epoch,
            head,
            owner: "host".into(),
            owner_epoch: 1,
        }
    }
    fn tuning() -> Tuning {
        Tuning {
            deadline: Duration::from_millis(250),
            probe_interval: Duration::from_secs(2),
            retry_base: Duration::from_millis(10),
            retry_cap: Duration::from_millis(20),
        }
    }
    async fn until(client: &Client, predicate: impl Fn(&Status) -> bool) {
        let mut status = client.watch();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if predicate(&status.borrow_and_update()) {
                    return;
                }
                status.changed().await.unwrap();
            }
        })
        .await
        .expect("client progress deadline");
    }
    #[tokio::test]
    async fn lost_ack_repair_retires_committed_outbox_without_duplicate_commit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let committed = Arc::new(Mutex::new(Vec::new()));
        let server_rows = committed.clone();
        let server = tokio::spawn(async move {
            for connection in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let _hello = ws.next().await.unwrap().unwrap();
                let head = server_rows.lock().unwrap().len() as u64;
                ws.send(Message::text(
                    serde_json::to_string(&state(head, 1)).unwrap(),
                ))
                .await
                .unwrap();
                if connection == 0 {
                    let push = ws.next().await.unwrap().unwrap();
                    let Request::Push { operations, .. } =
                        serde_json::from_str(&push.into_text().unwrap()).unwrap()
                    else {
                        panic!("push expected");
                    };
                    server_rows.lock().unwrap().extend(operations);
                    // Commit happened, then the connection died before ACK.
                    ws.close(None).await.unwrap();
                } else {
                    while let Some(Ok(message)) = ws.next().await {
                        if message.is_close() {
                            break;
                        }
                    }
                }
            }
        });
        let journal =
            Journal::open(std::path::Path::new(":memory:"), "account", "room", "phone").unwrap();
        let repair = Arc::new(Repair {
            calls: AtomicUsize::new(0),
            committed: committed.clone(),
        });
        let client = Client::spawn(
            journal,
            Arc::new(crate::StaticUrl(format!("ws://{address}"))),
            Some(repair.clone()),
            tuning(),
        );
        until(&client, |s| s.phase == Phase::Live).await;
        assert_eq!(repair.calls.load(Ordering::SeqCst), 0);
        client.enqueue(&op()).unwrap();
        until(&client, |s| {
            s.phase == Phase::Live && s.cursor == 1 && s.generation >= 2
        })
        .await;
        assert_eq!(committed.lock().unwrap().len(), 1);
        assert_eq!(lock(&client.journal).pending().unwrap().len(), 0);
        assert_eq!(
            lock(&client.journal).projection().unwrap().commands.len(),
            1
        );
        assert_eq!(repair.calls.load(Ordering::SeqCst), 2); // hello + missing committed row
        client.shutdown().await;
        server.abort();
    }
    #[tokio::test]
    async fn pongs_do_not_hide_a_missing_business_ack() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let committed = Arc::new(Mutex::new(Vec::new()));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.next().await.unwrap().unwrap();
            ws.send(Message::text(serde_json::to_string(&state(0, 1)).unwrap()))
                .await
                .unwrap();
            ws.next().await.unwrap().unwrap(); // push, deliberately never ACK
            loop {
                if ws.send(Message::text("pong")).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        let journal =
            Journal::open(std::path::Path::new(":memory:"), "account", "room", "phone").unwrap();
        let repair = Arc::new(Repair {
            calls: AtomicUsize::new(0),
            committed,
        });
        let client = Client::spawn(
            journal,
            Arc::new(crate::StaticUrl(format!("ws://{address}"))),
            Some(repair.clone()),
            tuning(),
        );
        until(&client, |s| s.phase == Phase::Live).await;
        client.enqueue(&op()).unwrap();
        until(&client, |s| s.repairs > 0).await;
        client.shutdown().await;
        server.abort();
    }
    #[tokio::test]
    async fn epoch_conflict_parks_without_http_or_retry_storm() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.next().await.unwrap().unwrap();
            ws.send(Message::text(serde_json::to_string(&state(0, 2)).unwrap()))
                .await
                .unwrap();
        });
        let mut journal =
            Journal::open(std::path::Path::new(":memory:"), "account", "room", "phone").unwrap();
        journal.accept_state(&state(0, 1)).unwrap();
        journal.enqueue(&op()).unwrap();
        let repair = Arc::new(Repair {
            calls: AtomicUsize::new(0),
            committed: Arc::new(Mutex::new(Vec::new())),
        });
        let client = Client::spawn(
            journal,
            Arc::new(crate::StaticUrl(format!("ws://{address}"))),
            Some(repair.clone()),
            tuning(),
        );
        until(&client, |s| {
            s.phase == Phase::Suspect
                && s.error
                    .as_deref()
                    .is_some_and(|e| e.contains("epoch_mismatch"))
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(repair.calls.load(Ordering::SeqCst), 0);
        assert_eq!(client.watch().borrow().generation, 1);
        assert_eq!(lock(&client.journal).pending().unwrap(), vec![op()]);
        client.shutdown().await;
        server.await.unwrap();
    }
}
