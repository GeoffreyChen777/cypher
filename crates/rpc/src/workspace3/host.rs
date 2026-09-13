use super::codec::{Decoder, Encoder};
use crate::{RpcError, RpcReply, RpcService, ServerFrame};
use cypher_sync::workspace3::client::Client;
use futures::StreamExt;
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
}
type Slots = Mutex<HashMap<String, Slot>>;
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn failed(code: &str) -> RpcError {
    RpcError::Transport(code.into())
}

/// Host-side bounded RPC adapter. The service is weak to avoid a
/// WorkspaceHost → RPC service → WorkspaceHost ownership cycle.
pub struct Host {
    client: Arc<Client>,
    service: Weak<dyn RpcService>,
    slots: Arc<Slots>,
    tasks: TaskTracker,
    stop: CancellationToken,
}
impl Host {
    pub fn new(client: Arc<Client>, service: Weak<dyn RpcService>) -> Self {
        Self {
            client,
            service,
            slots: Arc::new(Mutex::new(HashMap::new())),
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
        }
    }
    pub fn frame(&self, generation: u64, frame: &Value) -> bool {
        let Some(token) = frame["token"].as_str() else {
            return false;
        };
        if self.stop.is_cancelled() {
            return false;
        }
        let status = self.client.watch().borrow().clone();
        if !status.connected || status.generation != generation {
            return false;
        }
        if frame["type"] != "call" {
            let slots = lock(&self.slots);
            let Some(slot) = slots.get(token) else {
                return false;
            };
            if frame["type"] == "cancel" || frame["type"] == "error" {
                slot.stop.cancel();
            } else if slot.input.try_send(frame.clone()).is_err() {
                // No unbounded per-call queues, even for a peer flooding ACKs.
                slot.stop.cancel();
            }
            return true;
        }
        let mut slots = lock(&self.slots);
        if slots.len() >= 8 || slots.contains_key(token) {
            // The Hub promises a global eight-call host budget. A violated
            // protocol cannot silently dispatch untracked application work.
            self.client.reconnect();
            return true;
        }
        let (input, rx) = mpsc::channel(8);
        let stop = self.stop.child_token();
        slots.insert(
            token.into(),
            Slot {
                input,
                stop: stop.clone(),
            },
        );
        let cleanup = Cleanup {
            slots: Arc::downgrade(&self.slots),
            token: token.into(),
        };
        let client = self.client.clone();
        let service = self.service.upgrade();
        let frame = frame.clone();
        self.tasks.spawn(async move {
            let _cleanup = cleanup;
            let mut channel = Channel {
                client,
                generation,
                token: frame["token"].as_str().unwrap().into(),
                rx,
                next: 0,
                through: 0,
            };
            tokio::select! {
                _ = stop.cancelled() => {},
                result = serve(service, frame, &mut channel) => {
                    if let Err(error) = result {
                        // A transport failure is not evidence the service did
                        // not run. Retire reachability; callers cannot replay.
                        tracing::warn!(%error, "workspace RPC delivery uncertain");
                        channel.client.reconnect();
                    }
                }
            }
        });
        true
    }
    pub fn disconnected(&self) {
        for slot in lock(&self.slots).values() {
            slot.stop.cancel();
        }
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
struct Cleanup {
    slots: Weak<Slots>,
    token: String,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(slots) = self.slots.upgrade() {
            lock(&slots).remove(&self.token);
        }
    }
}

struct Channel {
    client: Arc<Client>,
    generation: u64,
    token: String,
    rx: mpsc::Receiver<Value>,
    next: u64,
    through: u64,
}
impl Channel {
    async fn send(&self, mut frame: Value) -> Result<(), RpcError> {
        frame["token"] = json!(self.token);
        self.client
            .send(self.generation, frame)
            .await
            .map_err(|e| failed(&e.to_string()))
    }
    fn credit(&mut self, frame: Value) -> Result<(), RpcError> {
        let through = frame["through"]
            .as_u64()
            .ok_or_else(|| failed("invalid_credit"))?;
        if frame["type"] != "credit" || through < self.through || through > self.next {
            return Err(failed("invalid_credit"));
        }
        self.through = through;
        Ok(())
    }
    async fn receive(&mut self) -> Result<Value, RpcError> {
        tokio::time::timeout(Duration::from_secs(30), self.rx.recv())
            .await
            .map_err(|_| failed("rpc_deadline"))?
            .ok_or(RpcError::Closed)
    }
    async fn output(&mut self, value: &ServerFrame, terminal: bool) -> Result<(), RpcError> {
        let mut encoder = Encoder::new(value)?.peekable();
        while let Some(part) = encoder.next() {
            while self.next - self.through >= 2 {
                let frame = self.receive().await?;
                self.credit(frame)?;
            }
            self.send(json!({"type":"reply","sequence":self.next,"done":terminal && encoder.peek().is_none(),"value":part})).await?;
            self.next += 1;
        }
        Ok(())
    }
    async fn reject(&mut self, error: impl std::fmt::Display) -> Result<(), RpcError> {
        self.output(
            &ServerFrame {
                err: Some(error.to_string()),
                ..Default::default()
            },
            true,
        )
        .await
    }
}
async fn serve(
    service: Option<Arc<dyn RpcService>>,
    frame: Value,
    channel: &mut Channel,
) -> Result<(), RpcError> {
    let Some(service) = service else {
        return channel.reject("service_unavailable").await;
    };
    let params = if frame["input"] == true {
        let mut decoder = Decoder::default();
        let mut sequence = 0;
        loop {
            let part = channel.receive().await?;
            if part["type"] != "input" || part["sequence"].as_u64() != Some(sequence) {
                return channel.reject("invalid_input_sequence").await;
            }
            let value = match decoder.push(part["value"].clone()) {
                Ok(value) => value,
                Err(error) => return channel.reject(error).await,
            };
            if part["done"].as_bool() != Some(value.is_some()) {
                return channel.reject("invalid_input_end").await;
            }
            sequence += 1;
            channel
                .send(json!({"type":"inputAck","through":sequence}))
                .await?;
            if let Some(value) = value {
                break value;
            }
        }
    } else {
        frame["params"].clone()
    };
    // Exactly one invocation, only after complete validated input. Dropping
    // the service future on disconnect never authorizes another invocation.
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        service.handle(frame["method"].as_str().unwrap_or(""), params),
    )
    .await;
    let reply = match result {
        Ok(Ok(reply)) => reply,
        Ok(Err(error)) => return channel.reject(error).await,
        Err(_) => return channel.reject("delivery_unknown").await,
    };
    match reply {
        RpcReply::Value(value) => {
            let frame = ServerFrame {
                ok: Some(value),
                ..Default::default()
            };
            // Check size before any fragment is sent; report, never truncate.
            if let Err(error) = Encoder::new(&frame) {
                return channel.reject(error).await;
            }
            channel.output(&frame, true).await
        }
        RpcReply::Stream(mut stream) => loop {
            tokio::select! {
                item = stream.next() => {
                    let terminal = item.is_none();
                    let frame = ServerFrame { item, done: terminal, ..Default::default() };
                    if let Err(error) = Encoder::new(&frame) { return channel.reject(error).await; }
                    channel.output(&frame, terminal).await?;
                    if terminal { return Ok(()); }
                }
                frame = channel.rx.recv() => channel.credit(frame.ok_or(RpcError::Closed)?)?,
            }
        },
    }
}
