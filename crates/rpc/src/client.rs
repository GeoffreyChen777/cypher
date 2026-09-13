//! Client side: request/stream multiplexing over string frames + the WebSocket dialer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::{ClientFrame, RpcError, ServerFrame};

/// Per-stream queue depth. Bounded: route_frame awaits a full queue, pausing
/// the connection reader — transport backpressure instead of unbounded growth
/// when a consumer stalls behind a fast producer (watch frames every 120ms
/// during streaming used to pile up whole-transcript payloads here).
const STREAM_QUEUE_CAP: usize = 256;

enum Pending {
    Call(oneshot::Sender<Result<serde_json::Value, RpcError>>),
    Stream {
        tx: mpsc::Sender<serde_json::Value>,
        _stop: oneshot::Sender<()>,
    },
}

struct Shared {
    pending: Mutex<HashMap<u64, Pending>>,
}
struct CancelCall {
    shared: Arc<Shared>,
    out: mpsc::Sender<String>,
    id: u64,
}
impl Drop for CancelCall {
    fn drop(&mut self) {
        if self.shared.lock().remove(&self.id).is_some() {
            // Best effort without spawning an unbounded number of teardown
            // tasks. The remote adapter also owns a bounded business deadline.
            let _ = self
                .out
                .try_send(serde_json::json!({"id":self.id,"cancel":true}).to_string());
        }
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Pending>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A multiplexing RPC client over any string-frame duplex ([`crate::memory_client`] or
/// [`crate::connect_local`]). Cheap to clone-by-Arc internally; use one per connection.
pub struct RpcClient {
    out: mpsc::Sender<String>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    reader: tokio::task::JoinHandle<()>,
    stream_capacity: usize,
    runtime: tokio::runtime::Handle,
}

impl RpcClient {
    /// Wrap an existing duplex: `out` carries client frames, `inbound` server frames.
    pub fn new(out: mpsc::Sender<String>, inbound: mpsc::Receiver<String>) -> Self {
        Self::with_stream_capacity(out, inbound, STREAM_QUEUE_CAP)
    }
    pub fn with_stream_capacity(
        out: mpsc::Sender<String>,
        mut inbound: mpsc::Receiver<String>,
        stream_capacity: usize,
    ) -> Self {
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
        });
        let reader_shared = shared.clone();
        let reader_out = out.clone();
        let reader = tokio::spawn(async move {
            while let Some(payload) = inbound.recv().await {
                for line in payload.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let frame: ServerFrame = match serde_json::from_str(line) {
                        Ok(frame) => frame,
                        Err(err) => {
                            tracing::warn!(error = %err, "rpc: dropping malformed server frame");
                            continue;
                        }
                    };
                    route_frame(&reader_shared, &reader_out, frame).await;
                }
            }
            // Connection closed: fail everything still pending.
            let drained: Vec<Pending> = {
                let mut pending = reader_shared.lock();
                pending.drain().map(|(_, p)| p).collect()
            };
            for entry in drained {
                if let Pending::Call(tx) = entry {
                    let _ = tx.send(Err(RpcError::Closed));
                }
                // Streams end by sender drop.
            }
        });
        Self {
            out,
            shared,
            next_id: AtomicU64::new(1),
            reader,
            stream_capacity: stream_capacity.max(1),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// Unary request.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.lock().insert(id, Pending::Call(tx));
        let _cancel = CancelCall {
            shared: self.shared.clone(),
            out: self.out.clone(),
            id,
        };
        self.send(ClientFrame {
            id,
            method: Some(method.into()),
            params,
            cancel: false,
        })
        .await
        .inspect_err(|_| {
            self.shared.lock().remove(&id);
        })?;
        rx.await.map_err(|_| RpcError::Closed)?
    }

    /// Typed unary request.
    pub async fn call_as<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, RpcError> {
        let value = self.call(method, params).await?;
        serde_json::from_value(value).map_err(|e| RpcError::BadParams(e.to_string()))
    }

    /// Streaming request: items arrive on the receiver; it closes when the server sends
    /// `{done}` or `{err}`, or the connection drops. Dropping the receiver cancels the
    /// stream server-side (the reader notices the dead channel and sends `{id, cancel}`).
    pub async fn subscribe(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<mpsc::Receiver<serde_json::Value>, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(self.stream_capacity);
        let (stop, stopped) = oneshot::channel();
        self.shared.lock().insert(
            id,
            Pending::Stream {
                tx: tx.clone(),
                _stop: stop,
            },
        );
        let shared = Arc::downgrade(&self.shared);
        let out = self.out.clone();
        self.runtime.spawn(async move {
            tokio::select! {
                _ = stopped => {},
                _ = tx.closed() => {
                    if let Some(shared) = shared.upgrade() { shared.lock().remove(&id); }
                    let _ = out.send(serde_json::json!({"id":id,"cancel":true}).to_string()).await;
                }
            }
        });
        self.send(ClientFrame {
            id,
            method: Some(method.into()),
            params,
            cancel: false,
        })
        .await
        .inspect_err(|_| {
            self.shared.lock().remove(&id);
        })?;
        Ok(rx)
    }

    async fn send(&self, frame: ClientFrame) -> Result<(), RpcError> {
        let json = serde_json::to_string(&frame)
            .map_err(|e| RpcError::Transport(format!("serialize frame: {e}")))?;
        self.out.send(json).await.map_err(|_| RpcError::Closed)
    }
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

async fn route_frame(shared: &Arc<Shared>, out: &mpsc::Sender<String>, frame: ServerFrame) {
    let id = frame.id;
    if let Some(err) = frame.err {
        match shared.lock().remove(&id) {
            Some(Pending::Call(tx)) => {
                let _ = tx.send(Err(RpcError::Failed(err)));
            }
            Some(Pending::Stream { .. }) | None => {
                // Stream errored: the sender drop closes the receiver.
                tracing::debug!(id, %err, "rpc: stream ended with error");
            }
        }
        return;
    }
    if let Some(value) = frame.ok {
        if let Some(Pending::Call(tx)) = shared.lock().remove(&id) {
            let _ = tx.send(Ok(value));
        }
        return;
    }
    if let Some(item) = frame.item {
        // Clone the sender out of the lock: the bounded send must await
        // (backpressure) without holding `shared`.
        let tx = match shared.lock().get(&id) {
            Some(Pending::Stream { tx, .. }) => Some(tx.clone()),
            _ => None,
        };
        let dead = match tx {
            Some(tx) => tx.send(item).await.is_err(),
            None => false,
        };
        if dead {
            // Receiver was dropped — cancel server-side and forget the stream.
            shared.lock().remove(&id);
            if let Ok(json) = serde_json::to_string(&ClientFrame {
                id,
                method: None,
                params: serde_json::Value::Null,
                cancel: true,
            }) {
                let _ = out.send(json).await;
            }
        }
        return;
    }
    if frame.done {
        shared.lock().remove(&id);
    }
}

pub(crate) fn client_from_websocket<S>(ws: tokio_tungstenite::WebSocketStream<S>) -> RpcClient
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let (in_tx, in_rx) = mpsc::channel::<String>(256);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                frame = out_rx.recv() => match frame {
                    Some(text) => {
                        if sink.send(WsMessage::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        let _ = sink.send(WsMessage::Close(None)).await;
                        break;
                    }
                },
                message = stream.next() => match message {
                    Some(Ok(WsMessage::Text(text))) => {
                        if in_tx.send(text).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
            }
        }
    });
    RpcClient::new(out_tx, in_rx)
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    #[test]
    fn subscription_can_be_polled_by_the_foreground_ui_executor() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (out, mut frames) = mpsc::channel(2);
        let (_server, incoming) = mpsc::channel(2);
        let client = {
            let _entered = runtime.enter();
            RpcClient::with_stream_capacity(out, incoming, 2)
        };
        // GPUI polls here without an entered Tokio runtime.
        let receiver =
            futures::executor::block_on(client.subscribe("Never", serde_json::json!({}))).unwrap();
        drop(receiver);
        runtime.block_on(async {
            frames.recv().await.unwrap();
            let cancel = tokio::time::timeout(std::time::Duration::from_secs(1), frames.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(serde_json::from_str::<ClientFrame>(&cancel).unwrap().cancel);
        });
    }
    #[tokio::test]
    async fn dropping_an_idle_stream_sends_cancel_without_a_server_item() {
        let (out, mut frames) = mpsc::channel(2);
        let (_server, incoming) = mpsc::channel(2);
        let client = RpcClient::with_stream_capacity(out, incoming, 2);
        let response = client
            .subscribe("Never", serde_json::json!({}))
            .await
            .unwrap();
        let first: ClientFrame = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
        drop(response);
        let cancel = tokio::time::timeout(std::time::Duration::from_secs(1), frames.recv())
            .await
            .unwrap()
            .unwrap();
        let cancel: ClientFrame = serde_json::from_str(&cancel).unwrap();
        assert_eq!(first.id, cancel.id);
        assert!(cancel.cancel);
        assert!(client.shared.lock().is_empty());
    }
    #[tokio::test]
    async fn dropping_a_pending_unary_cancels_and_releases_the_slot() {
        let (out, mut frames) = mpsc::channel(2);
        let (_server, incoming) = mpsc::channel(2);
        let client = RpcClient::with_stream_capacity(out, incoming, 2);
        let mut call = Box::pin(client.call("Slow", serde_json::json!({})));
        let first = tokio::select! { _ = &mut call => panic!("must be pending"), frame = frames.recv() => frame.unwrap() };
        drop(call);
        let cancel: ClientFrame = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientFrame>(&first).unwrap().id,
            cancel.id
        );
        assert!(cancel.cancel);
        assert!(client.shared.lock().is_empty());
    }
}
