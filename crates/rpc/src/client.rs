//! Client side: request/stream multiplexing over string frames, plus the adapter that
//! runs a client over an already-dialed WebSocket (device-room links).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
    Stream(mpsc::Sender<serde_json::Value>),
}

struct Shared {
    pending: Mutex<HashMap<u64, Pending>>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Pending>> {
        crate::lock(&self.pending)
    }
}

/// Dials a replacement for a connection that closed
/// ([`RpcClient::with_redial`]).
pub type Redial =
    Arc<dyn Fn() -> futures::future::BoxFuture<'static, Result<RpcClient, RpcError>> + Send + Sync>;

/// A failed redial is not repeated for this long: every watch of a viewport
/// retries on its own, and one dial answers them all.
const REDIAL_PAUSE: std::time::Duration = std::time::Duration::from_millis(500);

struct Redialer {
    dial: Redial,
    /// One dial at a time; holds when the last one failed.
    last_failure: tokio::sync::Mutex<Option<std::time::Instant>>,
}

/// Why a request got no answer: it never left (the connection was already
/// closed, so sending it again elsewhere is safe), or it failed.
enum Unanswered {
    Unsent,
    Failed(RpcError),
}

impl From<Unanswered> for RpcError {
    fn from(unanswered: Unanswered) -> Self {
        match unanswered {
            Unanswered::Unsent => RpcError::Closed,
            Unanswered::Failed(err) => err,
        }
    }
}

/// One connection: its outbound queue, the requests waiting on it and the
/// task reading its frames.
struct Conn {
    out: mpsc::Sender<String>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    reader: tokio::task::JoinHandle<()>,
}

/// A multiplexing RPC client over any string-frame duplex ([`crate::memory_client`] or
/// [`crate::connect_local`]). Cheap to clone-by-Arc internally; use one per connection.
///
/// A client is one connection for life unless it was given a way to dial
/// another ([`Self::with_redial`]): then a request that finds the connection
/// closed dials again and goes out on the new one.
pub struct RpcClient {
    conn: Mutex<Arc<Conn>>,
    redial: Option<Redialer>,
}

impl Conn {
    fn new(out: mpsc::Sender<String>, mut inbound: mpsc::Receiver<String>) -> Self {
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
        }
    }

    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, Unanswered> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.lock().insert(id, Pending::Call(tx));
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
        rx.await
            .map_err(|_| Unanswered::Failed(RpcError::Closed))?
            .map_err(Unanswered::Failed)
    }

    async fn subscribe(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<mpsc::Receiver<serde_json::Value>, Unanswered> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(STREAM_QUEUE_CAP);
        self.shared.lock().insert(id, Pending::Stream(tx));
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

    async fn send(&self, frame: ClientFrame) -> Result<(), Unanswered> {
        let json = serde_json::to_string(&frame).map_err(|e| {
            Unanswered::Failed(RpcError::Transport(format!("serialize frame: {e}")))
        })?;
        self.out.send(json).await.map_err(|_| Unanswered::Unsent)
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl RpcClient {
    /// Wrap an existing duplex: `out` carries client frames, `inbound` server frames.
    pub fn new(out: mpsc::Sender<String>, inbound: mpsc::Receiver<String>) -> Self {
        Self {
            conn: Mutex::new(Arc::new(Conn::new(out, inbound))),
            redial: None,
        }
    }

    /// Dial again with `dial` when a request finds the connection closed
    /// (the engine it reached restarted). Requests in flight when it closed
    /// still fail, and streams still end: only a request that never left is
    /// sent on the new connection, so nothing is delivered twice.
    ///
    /// `dial` is called on whatever executor made the request, which need
    /// not be a Tokio runtime: a dial that needs one hops onto it itself.
    pub fn with_redial(mut self, dial: Redial) -> Self {
        self.redial = Some(Redialer {
            dial,
            last_failure: tokio::sync::Mutex::new(None),
        });
        self
    }

    fn conn(&self) -> Arc<Conn> {
        crate::lock(&self.conn).clone()
    }

    /// The connection to use after `closed` turned out closed: one another
    /// request already dialed, else a new one.
    async fn reconnect(&self, closed: &Arc<Conn>) -> Result<Arc<Conn>, RpcError> {
        let Some(redialer) = &self.redial else {
            return Err(RpcError::Closed);
        };
        let mut last_failure = redialer.last_failure.lock().await;
        let current = self.conn();
        if !Arc::ptr_eq(&current, closed) {
            return Ok(current);
        }
        if last_failure.is_some_and(|at| at.elapsed() < REDIAL_PAUSE) {
            return Err(RpcError::Closed);
        }
        match (redialer.dial)().await {
            Ok(client) => {
                let conn = client.conn();
                *crate::lock(&self.conn) = conn.clone();
                *last_failure = None;
                tracing::info!("rpc: reconnected");
                Ok(conn)
            }
            Err(err) => {
                tracing::debug!(error = %err, "rpc: redial failed");
                *last_failure = Some(std::time::Instant::now());
                Err(RpcError::Closed)
            }
        }
    }

    /// Unary request.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let conn = self.conn();
        if self.redial.is_none() {
            return Ok(conn.call(method, params).await?);
        }
        match conn.call(method, params.clone()).await {
            Err(Unanswered::Unsent) => {
                let conn = self.reconnect(&conn).await?;
                Ok(conn.call(method, params).await?)
            }
            other => Ok(other?),
        }
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
        let conn = self.conn();
        if self.redial.is_none() {
            return Ok(conn.subscribe(method, params).await?);
        }
        match conn.subscribe(method, params.clone()).await {
            Err(Unanswered::Unsent) => {
                let conn = self.reconnect(&conn).await?;
                Ok(conn.subscribe(method, params).await?)
            }
            other => Ok(other?),
        }
    }
}

async fn route_frame(shared: &Arc<Shared>, out: &mpsc::Sender<String>, frame: ServerFrame) {
    let id = frame.id;
    if let Some(err) = frame.err {
        match shared.lock().remove(&id) {
            Some(Pending::Call(tx)) => {
                let _ = tx.send(Err(RpcError::Failed(err)));
            }
            Some(Pending::Stream(_)) | None => {
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
            Some(Pending::Stream(tx)) => Some(tx.clone()),
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
