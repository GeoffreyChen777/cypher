use super::codec::{Decoder, Encoder};
use crate::{RpcError, ServerFrame};
use cypher_sync::workspace3::client::Client;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

struct Slot {
    input: mpsc::Sender<Value>,
    stop: CancellationToken,
    token: Option<String>,
    generation: u64,
}
type Slots = Mutex<HashMap<String, Slot>>;
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn error(code: &str) -> RpcError {
    RpcError::Transport(code.into())
}

pub struct Responses {
    rx: mpsc::Receiver<Result<ServerFrame, RpcError>>,
    stop: CancellationToken,
}
impl Responses {
    pub async fn recv(&mut self) -> Option<Result<ServerFrame, RpcError>> {
        self.rx.recv().await
    }
}
impl Drop for Responses {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Calls are transient and generation-bound. Reconnect never re-enqueues a
/// call; application code receives uncertainty and decides what to do.
pub struct Caller {
    client: Arc<Client>,
    slots: Arc<Slots>,
    tasks: TaskTracker,
    stop: CancellationToken,
}
impl Caller {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            client,
            slots: Arc::new(Mutex::new(HashMap::new())),
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
        }
    }
    pub fn frame(&self, generation: u64, frame: &Value) -> bool {
        let mut slots = lock(&self.slots);
        let slot = if let Some(id) = frame["id"].as_str() {
            slots.get_mut(id)
        } else if let Some(token) = frame["token"].as_str() {
            slots
                .values_mut()
                .find(|s| s.token.as_deref() == Some(token))
        } else {
            None
        };
        let Some(slot) = slot else {
            return false;
        };
        if generation != slot.generation {
            return true;
        }
        if frame["type"] == "routed" {
            slot.token = frame["token"].as_str().map(str::to_owned);
        }
        if slot.input.try_send(frame.clone()).is_err() {
            slot.stop.cancel();
        }
        true
    }
    pub fn request(
        &self,
        target: &str,
        method: &str,
        params: Value,
    ) -> Result<Responses, RpcError> {
        let status = self.client.watch().borrow().clone();
        if self.stop.is_cancelled() || !status.connected {
            return Err(error("workspace_unavailable"));
        }
        if !cypher_sync::workspace3::wire::id(target)
            || method.is_empty()
            || method.len() > 96
            || !method.as_bytes()[0].is_ascii_alphabetic()
            || !method.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(error("invalid_rpc_target_or_method"));
        }
        let mut slots = lock(&self.slots);
        if slots.len() >= 8 {
            return Err(error("rpc_capacity"));
        }
        let encoder = Encoder::new(&params)?.peekable();
        let id = uuid::Uuid::new_v4().to_string();
        let stop = self.stop.child_token();
        let (input, rx) = mpsc::channel(8);
        let (output, response) = mpsc::channel(2);
        slots.insert(
            id.clone(),
            Slot {
                input,
                stop: stop.clone(),
                token: None,
                generation: status.generation,
            },
        );
        let cleanup = Cleanup {
            slots: Arc::downgrade(&self.slots),
            id: id.clone(),
        };
        let mut task = Request {
            client: self.client.clone(),
            generation: status.generation,
            id,
            token: None,
            stop: stop.clone(),
            rx,
            output,
            input: encoder,
            sent: 0,
            through: 0,
            next: 0,
            decoder: Decoder::default(),
            first: true,
        };
        let target = target.to_owned();
        let method = method.to_owned();
        self.tasks.spawn(async move {
            let _cleanup = cleanup;
            let result = task.run(target, method).await;
            if let Err(error) = result {
                // Stop routing, not execution authority. There is no replay.
                if let Some(token) = &task.token {
                    let _ = task
                        .client
                        .send(task.generation, json!({"type":"cancel","token":token}))
                        .await;
                }
                let _ = task.output.try_send(Err(error));
            }
        });
        Ok(Responses { rx: response, stop })
    }
    pub async fn call(&self, target: &str, method: &str, params: Value) -> Result<Value, RpcError> {
        let mut response = self.request(target, method, params)?;
        let frame = response
            .recv()
            .await
            .ok_or_else(|| error("delivery_unknown"))??;
        if let Some(error) = frame.err {
            return Err(RpcError::Failed(error));
        }
        frame.ok.ok_or_else(|| error("expected_unary_reply"))
    }
    pub fn disconnected(&self) {
        for slot in lock(&self.slots).values() {
            slot.stop.cancel();
        }
    }
    pub fn active_requests(&self) -> usize {
        lock(&self.slots).len()
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}
impl Drop for Caller {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
struct Cleanup {
    slots: Weak<Slots>,
    id: String,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(slots) = self.slots.upgrade() {
            lock(&slots).remove(&self.id);
        }
    }
}
struct Request {
    client: Arc<Client>,
    generation: u64,
    id: String,
    token: Option<String>,
    stop: CancellationToken,
    rx: mpsc::Receiver<Value>,
    output: mpsc::Sender<Result<ServerFrame, RpcError>>,
    input: std::iter::Peekable<Encoder>,
    sent: u64,
    through: u64,
    next: u64,
    decoder: Decoder,
    first: bool,
}
impl Request {
    async fn send(&self, frame: Value) -> Result<(), RpcError> {
        self.client
            .send(self.generation, frame)
            .await
            .map_err(|e| error(&e.to_string()))
    }
    async fn run(&mut self, target: String, method: String) -> Result<(), RpcError> {
        if self.stop.is_cancelled() {
            return Err(error("consumer_closed"));
        }
        let first_deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        let mut progress = tokio::time::Instant::now();
        let mut status = self.client.watch();
        // One uniform codec: no ambiguous boundary between an inline params
        // object and a fragmented object that merely resembles a descriptor.
        self.send(json!({"type":"call","id":self.id,"target":target,"method":method,"params":{},"input":true})).await?;
        loop {
            // Before routed, retain this request long enough to receive its
            // cancellation capability. Never abandon an unknown live route.
            if self.stop.is_cancelled() && self.token.is_some() {
                return Err(error("consumer_closed"));
            }
            if let Some(token) = &self.token {
                if self.sent - self.through < 2
                    && let Some(part) = self.input.next()
                {
                    let done = self.input.peek().is_none();
                    self.send(json!({"type":"input","token":token,"sequence":self.sent,"done":done,"value":part})).await?;
                    self.sent += 1;
                    continue;
                }
            }
            let deadline = if self.first {
                first_deadline
            } else if self.decoder.incomplete() {
                progress + Duration::from_secs(30)
            } else {
                tokio::time::Instant::now() + Duration::from_secs(3600)
            };
            let frame = tokio::select! {
                _ = self.stop.cancelled(), if self.token.is_some() => return Err(error("consumer_closed")),
                changed = status.changed() => {
                    let current = status.borrow_and_update().clone();
                    if changed.is_err() || !current.connected || current.generation != self.generation {
                        return Err(error("delivery_unknown"));
                    }
                    continue;
                }
                result = tokio::time::timeout_at(deadline, self.rx.recv()) =>
                    result.map_err(|_| error("delivery_unknown"))?.ok_or_else(|| error("delivery_unknown"))?,
            };
            match frame["type"].as_str() {
                Some("routed") => {
                    if self.token.is_some() {
                        return Err(error("duplicate_route"));
                    }
                    self.token = Some(
                        frame["token"]
                            .as_str()
                            .ok_or_else(|| error("invalid_route"))?
                            .into(),
                    );
                }
                Some("inputCredit") => {
                    let through = frame["through"]
                        .as_u64()
                        .ok_or_else(|| error("invalid_credit"))?;
                    if frame["token"].as_str() != self.token.as_deref()
                        || through < self.through
                        || through > self.sent
                    {
                        return Err(error("invalid_credit"));
                    }
                    self.through = through;
                }
                Some("reply") => {
                    let token = self
                        .token
                        .as_ref()
                        .ok_or_else(|| error("reply_before_route"))?;
                    if frame["sequence"].as_u64() != Some(self.next) {
                        return Err(error("rpc_sequence"));
                    }
                    let done = frame["done"]
                        .as_bool()
                        .ok_or_else(|| error("invalid_reply"))?;
                    let decoded = self.decoder.push(frame["value"].clone())?;
                    progress = tokio::time::Instant::now();
                    if done && decoded.is_none() {
                        return Err(error("truncated_rpc_reply"));
                    }
                    self.next += 1;
                    if let Some(value) = decoded {
                        if value.as_object().is_none_or(|o| {
                            o.keys()
                                .any(|k| !["id", "ok", "err", "item", "done"].contains(&k.as_str()))
                        }) || value["id"].as_u64() != Some(0)
                        {
                            return Err(error("invalid_application_reply"));
                        }
                        let reply: ServerFrame = serde_json::from_value(value)
                            .map_err(|_| error("invalid_application_reply"))?;
                        let terminal = reply.ok.is_some() || reply.err.is_some() || reply.done;
                        if terminal != done
                            || usize::from(reply.ok.is_some())
                                + usize::from(reply.err.is_some())
                                + usize::from(reply.item.is_some())
                                + usize::from(reply.done)
                                != 1
                        {
                            return Err(error("invalid_application_reply"));
                        }
                        tokio::time::timeout(Duration::from_secs(30), self.output.send(Ok(reply)))
                            .await
                            .map_err(|_| error("consumer_backpressure"))?
                            .map_err(|_| error("consumer_closed"))?;
                        self.first = false;
                    }
                    // ACK only after bounded assembly and delivery. Finish this
                    // final ACK even when a unary consumer immediately drops.
                    self.send(json!({"type":"ack","token":token,"through":self.next}))
                        .await?;
                    if done {
                        return Ok(());
                    }
                }
                Some("error") => {
                    return Err(error(frame["code"].as_str().unwrap_or("delivery_unknown")));
                }
                _ => return Err(error("unexpected_rpc_frame")),
            }
        }
    }
}
