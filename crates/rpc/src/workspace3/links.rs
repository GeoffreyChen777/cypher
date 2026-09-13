use super::Caller;
use crate::{ClientFrame, RpcClient, RpcError, ServerFrame};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Device-addressed RPC handles all share the existing account-scoped Hub.
/// No device-room sockets, query tokens, dial retries or side-effect replay.
pub struct Links {
    caller: Arc<Caller>,
    clients: Mutex<HashMap<String, Weak<RpcClient>>>,
    stop: CancellationToken,
}
impl Links {
    pub fn new(caller: Arc<Caller>) -> Arc<Self> {
        Arc::new(Self {
            caller,
            clients: Mutex::new(HashMap::new()),
            stop: CancellationToken::new(),
        })
    }
    pub fn credential_transport_allowed(&self) -> bool {
        !self.stop.is_cancelled()
    }
    pub async fn client(self: &Arc<Self>, target: &str) -> Result<Arc<RpcClient>, RpcError> {
        if self.stop.is_cancelled() {
            return Err(RpcError::Closed);
        }
        let mut clients = lock(&self.clients);
        clients.retain(|_, client| client.strong_count() > 0);
        if let Some(client) = clients.get(target).and_then(Weak::upgrade) {
            return Ok(client);
        }
        if clients.len() >= 64 {
            return Err(RpcError::Transport("rpc_client_capacity".into()));
        }
        let (out, inbound) = mpsc::channel(2);
        let (replies, input) = mpsc::channel(2);
        let client = Arc::new(RpcClient::with_stream_capacity(out, input, 2));
        let caller = self.caller.clone();
        let stop = self.stop.child_token();
        let target = target.to_owned();
        clients.insert(target.clone(), Arc::downgrade(&client));
        tokio::spawn(bridge(caller, target, inbound, replies, stop));
        Ok(client)
    }
    pub fn invalidate(&self, target: &str) {
        lock(&self.clients).remove(target);
    }
    pub fn reset_cooldown(&self, _: &str) {} // No device dial/cooldown in v3.
    pub fn disconnect_all(&self) {
        self.stop.cancel();
        self.caller.disconnected();
        lock(&self.clients).clear();
    }
}
impl Drop for Links {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
async fn bridge(
    caller: Arc<Caller>,
    target: String,
    mut inbound: mpsc::Receiver<String>,
    replies: mpsc::Sender<String>,
    stop: CancellationToken,
) {
    let mut calls = HashMap::<u64, CancellationToken>::new();
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = replies.closed() => break,
            joined = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Ok(id)) = joined { calls.remove(&id); }
            }
            frame = inbound.recv() => {
                let Some(frame) = frame else { break; };
                let Ok(frame) = serde_json::from_str::<ClientFrame>(&frame) else { break; };
                if frame.cancel {
                    if let Some(stop) = calls.remove(&frame.id) { stop.cancel(); }
                    continue;
                }
                let Some(method) = frame.method else { break; };
                let cancel = stop.child_token();
                let response = if calls.len() >= 8 || calls.contains_key(&frame.id) {
                    Err(RpcError::Transport("rpc_capacity".into()))
                } else { caller.request(&target, &method, frame.params) };
                let mut response = match response {
                    Ok(response) => response,
                    Err(error) => {
                        let body = serde_json::to_string(&ServerFrame { id: frame.id, err: Some(error.to_string()), ..Default::default() }).unwrap();
                        if tokio::time::timeout(std::time::Duration::from_secs(30), replies.send(body)).await.is_err() { break; }
                        continue;
                    }
                };
                calls.insert(frame.id, cancel.clone());
                let replies = replies.clone();
                tasks.spawn(async move {
                    loop {
                        let next = tokio::select! { _ = cancel.cancelled() => break, value = response.recv() => value };
                        let mut next = match next {
                            Some(Ok(value)) => value,
                            Some(Err(error)) => ServerFrame { err: Some(error.to_string()), ..Default::default() },
                            None => ServerFrame { err: Some("delivery_unknown".into()), ..Default::default() },
                        };
                        next.id = frame.id;
                        let terminal = next.done || next.err.is_some() || next.ok.is_some();
                        let body = serde_json::to_string(&next).unwrap();
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            result = replies.send(body) => if result.is_err() { break; },
                        }
                        if terminal { break; }
                    }
                    frame.id
                });
            }
        }
    }
    for cancel in calls.values() {
        cancel.cancel();
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}
