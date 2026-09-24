//! pi RPC line transport over the child's stdio.
//!
//! pi RPC is strict JSONL — LF (`\n`) is the ONLY record delimiter (the
//! docs are explicit that Node `readline`, which also splits on U+2028/U+2029,
//! is NOT protocol-compliant). This reader splits on the `\n` byte only and
//! strips an optional trailing `\r` (CRLF tolerance) — never on any Unicode
//! separator. It is NOT JSON-RPC 2.0 (the `jsonrpc.rs` client is not used).
//!
//! Inbound lines are three kinds, discriminated by `type`:
//! - `"response"` — command result, resolved against the pending map by id;
//! - `"extension_ui_request"` — extension UI dialog / fire-and-forget;
//! - anything else — an agent event (streamed in stdout order).
//!
//! Writes to a dead child's stdin (EPIPE) are tolerated and logged, matching
//! the ACP client.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};

use crate::HarnessError;

/// A non-response line, in stdout order.
pub(crate) enum Incoming {
    /// An agent event (any `type` other than response / extension_ui_request).
    Event(Value),
    /// An extension UI request (dialog or fire-and-forget). `payload` is the
    /// whole request object — the fields (`title`, `options`, …) live at the
    /// top level, not under a `params` key.
    UiRequest {
        id: String,
        method: String,
        payload: Value,
    },
    /// stdout EOF / read error: the child exited. All pending requests fail.
    Eof,
}

/// Awaiting requests by id. `None` once the reader has stopped: every waiter
/// was failed then, and a request registered afterwards would never resolve,
/// so it is refused instead.
type Pending = Arc<Mutex<Option<HashMap<String, oneshot::Sender<Result<Value, String>>>>>>;

#[derive(Clone)]
pub(crate) struct PiClient {
    next_id: Arc<AtomicI64>,
    pending: Pending,
    writer: mpsc::UnboundedSender<String>,
}

impl PiClient {
    /// Spawn the writer + reader tasks over the child's stdio; returns the
    /// client and the incoming (event / ui-request) channel.
    pub fn new(stdin: ChildStdin, stdout: ChildStdout) -> (Self, mpsc::Receiver<Incoming>) {
        let (writer_tx, writer_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(write_loop(stdin, writer_rx));
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let (incoming_tx, incoming_rx) = mpsc::channel(256);
        tokio::spawn(read_loop(stdout, Arc::clone(&pending), incoming_tx));
        (
            Self {
                next_id: Arc::new(AtomicI64::new(0)),
                pending,
                writer: writer_tx,
            },
            incoming_rx,
        )
    }

    /// Send a command and await its response (`type: "response"` with the
    /// matching id). `success: false` becomes an error; a child exit before
    /// the response does too.
    pub async fn request(
        &self,
        command: &str,
        mut params: Map<String, Value>,
    ) -> Result<Value, HarnessError> {
        let id = format!("z{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let (tx, rx) = oneshot::channel();
        match self.pending.lock().expect("pending lock").as_mut() {
            Some(waiters) => {
                waiters.insert(id.clone(), tx);
            }
            None => {
                return Err(HarnessError::Protocol(format!(
                    "{command}: pi exited before responding"
                )));
            }
        }
        params.insert("id".into(), Value::String(id.clone()));
        params.insert("type".into(), Value::String(command.into()));
        let line = serde_json::to_string(&Value::Object(params)).expect("serializable");
        if self.writer.send(line).is_err() {
            if let Some(waiters) = self.pending.lock().expect("pending lock").as_mut() {
                waiters.remove(&id);
            }
            return Err(HarnessError::Protocol(format!(
                "{command}: pi stdin closed"
            )));
        }
        match rx.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(message)) => Err(HarnessError::Protocol(format!("{command}: {message}"))),
            // Sender dropped: the reader hit EOF and failed all pending.
            Err(_) => Err(HarnessError::Protocol(format!(
                "{command}: pi exited before responding"
            ))),
        }
    }

    /// Fire a command without awaiting its response.
    pub fn send(&self, command: &str, params: Map<String, Value>) {
        let line = self.line(command, params);
        let _ = self.writer.send(line);
    }

    /// Answer an extension UI request (dialog methods only).
    pub fn respond_ui(&self, id: &str, payload: Value) {
        let mut msg = match payload {
            Value::Object(obj) => obj,
            other => {
                let mut obj = Map::new();
                obj.insert("value".into(), other);
                obj
            }
        };
        msg.insert("id".into(), Value::String(id.to_owned()));
        msg.insert("type".into(), Value::String("extension_ui_response".into()));
        let line = serde_json::to_string(&Value::Object(msg)).expect("serializable");
        let _ = self.writer.send(line);
    }

    fn line(&self, command: &str, mut params: Map<String, Value>) -> String {
        params.insert("type".into(), Value::String(command.into()));
        serde_json::to_string(&Value::Object(params)).expect("serializable")
    }
}

/// Owns the child's stdin; a write failure (EPIPE after the child died) is
/// tolerated and logged.
async fn write_loop(mut stdin: ChildStdin, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(line) = rx.recv().await {
        let write = async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await
        };
        if let Err(e) = write.await {
            tracing::debug!(target: "cypher_harness::pi", "stdin write failed (tolerated): {e}");
            return;
        }
    }
}

/// Parse stdout lines until the child or the session goes away, then fail
/// every awaiting request and refuse new ones. Only a real EOF / read error
/// is signalled as [`Incoming::Eof`]; a dropped receiver has nobody to tell.
async fn read_loop(stdout: ChildStdout, pending: Pending, tx: mpsc::Sender<Incoming>) {
    let eof = read_lines(stdout, &pending, &tx).await;
    pending.lock().expect("pending lock").take();
    if eof {
        let _ = tx.send(Incoming::Eof).await;
    }
}

/// Parse stdout lines: responses resolve the pending map, extension UI
/// requests and events forward in order. Non-JSON noise is skipped.
/// Returns `true` on EOF / read error, `false` once the session dropped its
/// receiver.
async fn read_lines(stdout: ChildStdout, pending: &Pending, tx: &mpsc::Sender<Incoming>) -> bool {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::with_capacity(1024);
    loop {
        buf.clear();
        // read_until stops at the `\n` byte (0x0A) only — never at U+2028 /
        // U+2029, which are valid multi-byte characters inside JSON strings.
        let n = match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(_) => break, // a read error ends the loop like EOF
        };
        let mut end = if buf[n - 1] == b'\n' { n - 1 } else { n };
        if end > 0 && buf[end - 1] == b'\r' {
            end -= 1; // CRLF tolerance
        }
        let line = std::str::from_utf8(&buf[..end]).unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            tracing::debug!(target: "cypher_harness::pi", "non-JSON stdout line (skipped)");
            continue;
        };
        match msg.get("type").and_then(Value::as_str) {
            Some("response") => {
                let Some(id) = msg.get("id").and_then(Value::as_str).map(str::to_owned) else {
                    continue;
                };
                let sender = pending
                    .lock()
                    .expect("pending lock")
                    .as_mut()
                    .and_then(|waiters| waiters.remove(&id));
                let Some(sender) = sender else {
                    // A fire-and-forget command's response: nobody awaits it.
                    continue;
                };
                let outcome = if msg.get("success").and_then(Value::as_bool).unwrap_or(false) {
                    Ok(msg.get("data").cloned().unwrap_or(Value::Null))
                } else {
                    Err(msg
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("pi command failed: {msg}")))
                };
                let _ = sender.send(outcome);
            }
            Some("extension_ui_request") => {
                let id = msg
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let method = msg
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let incoming = Incoming::UiRequest {
                    id,
                    method,
                    payload: msg,
                };
                if tx.send(incoming).await.is_err() {
                    return false;
                }
            }
            _ => {
                if tx.send(Incoming::Event(msg)).await.is_err() {
                    return false;
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::process::{Child, Command};

    fn spawn(script: &str) -> (Child, PiClient, mpsc::Receiver<Incoming>) {
        let mut child = Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sh");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (client, incoming) = PiClient::new(stdin, stdout);
        (child, client, incoming)
    }

    async fn request_settles(client: &PiClient) -> Result<Value, HarnessError> {
        tokio::time::timeout(
            Duration::from_secs(5),
            client.request("get_session_stats", Map::new()),
        )
        .await
        .expect("request settles instead of hanging")
    }

    #[tokio::test]
    async fn request_after_eof_fails_instead_of_hanging() {
        // The child keeps stdin open (so the write succeeds) but closes
        // stdout: a request registered after the reader drained the pending
        // map must fail, not wait for a response that can never arrive.
        let (_child, client, mut incoming) = spawn("exec 1>&-; sleep 5");
        assert!(matches!(incoming.recv().await, Some(Incoming::Eof)));
        assert!(request_settles(&client).await.is_err());
    }

    #[tokio::test]
    async fn dropped_session_fails_awaiting_requests() {
        // The session stops listening while a request is in flight: the
        // reader's next forward fails, and the waiter must be released with
        // it even though the child is still alive.
        let (_child, client, incoming) =
            spawn(r#"read -r _; printf '{"type":"agent_start"}\n'; sleep 5"#);
        drop(incoming);
        assert!(request_settles(&client).await.is_err());
    }
}
