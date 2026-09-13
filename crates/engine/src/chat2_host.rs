//! chat2 host wiring (docs/chat2-sync.md C3): the engine-side implementations
//! of [`cypher_sync::chat_client::ChatDocSink`] and
//! [`cypher_sync::chat_client::CheckpointFetcher`], binding a
//! [`crate::doc_host::ChatDocHandle`]'s live doc to a chat2 room.
//!
//! The C2 rule is enforced HERE: every sink method persists doc content AND
//! the room cursor in one `save_snapshot_with_cursor` transaction, so a
//! restored backup can never disagree with its own cursor — the root cause
//! of the redownload-forever class the old s2 clients suffered.

use std::sync::Arc;

use cypher_doc::SessionDoc;
use cypher_sync::chat_client::{ChatDocSink, ChatTransport, CheckpointFetcher};
use cypher_sync::{DocsStore, SyncError};
use futures::future::BoxFuture;

use crate::doc_host::EdgeConfig;

/// Doc epoch stamped on every chat2-synced snapshot (docs/chat2-sync.md M1:
/// thin docs are lineage epoch 2; M3 readers discard-and-adopt below it).
pub const CHAT2_DOC_EPOCH: u32 = 2;

/// [`ChatDocSink`] over a live [`SessionDoc`] + the cursor-bearing store.
///
/// Loro import of a remote row/checkpoint fires the doc's root subscription,
/// so the transcript watch, command drain, and debounced UI publish all ride
/// the existing change plumbing — this type only owns import + same-tx
/// persistence.
pub struct EngineChatSink {
    /// WEAK: the sink lives inside the handle's `ChatClient` for the
    /// client's whole life — a strong ref here kept
    /// `Arc::strong_count(&handle.doc) > 1` permanently, which reads as
    /// "live writer" to `pinned()` and made every chat2 handle immune to
    /// LRU eviction (unbounded warm-doc growth). Callbacks upgrade per
    /// call; a dead doc (evicted handle) is a no-op.
    doc: std::sync::Weak<SessionDoc>,
    store: Arc<DocsStore>,
    chat_id: String,
}

impl EngineChatSink {
    pub fn new(doc: &Arc<SessionDoc>, store: Arc<DocsStore>, chat_id: impl Into<String>) -> Self {
        Self {
            doc: Arc::downgrade(doc),
            store,
            chat_id: chat_id.into(),
        }
    }

    /// Export the CURRENT doc and persist it with `cursor` in one tx.
    fn persist_with_cursor(&self, cursor: u64) {
        let Some(doc) = self.doc.upgrade() else {
            return;
        };
        match doc.export_snapshot() {
            Ok(bytes) => {
                if let Err(err) = self.store.save_snapshot_with_cursor(
                    &self.chat_id,
                    &bytes,
                    cursor,
                    CHAT2_DOC_EPOCH,
                ) {
                    tracing::warn!(chat = %self.chat_id, error = %err,
                        "chat2 sink: snapshot persist failed (will retry on next change)");
                }
            }
            Err(err) => {
                tracing::warn!(chat = %self.chat_id, error = %err,
                    "chat2 sink: snapshot export failed");
            }
        }
    }
}

impl ChatDocSink for EngineChatSink {
    fn apply_row(&self, bytes: &[u8], cursor: u64) {
        let Some(doc) = self.doc.upgrade() else {
            return;
        };
        if let Err(err) = doc.doc().import(bytes) {
            // Malformed remote bytes cost the row, never the doc (the same
            // skip-not-fail rule as transcript reads). The cursor still
            // advances: replaying a poison row forever is the wedge class.
            tracing::warn!(chat = %self.chat_id, error = %err,
                "chat2 sink: row import failed; skipping row");
        }
        self.persist_with_cursor(cursor);
    }

    fn apply_checkpoint(&self, bytes: &[u8], cursor: u64) -> Result<(), String> {
        let doc = self.doc.upgrade().ok_or("doc evicted")?;
        doc.doc()
            .import(bytes)
            .map_err(|e| format!("checkpoint import: {e}"))?;
        self.persist_with_cursor(cursor);
        Ok(())
    }

    fn contains_frontier(&self, frontier: &[u8]) -> bool {
        let Some(doc) = self.doc.upgrade() else {
            // Eviction ends the doc's live handle; the doc host will reopen it
            // and establish a fresh sink/client. Do not spin a checkpoint fetch
            // that can never be applied through this dead weak reference.
            return true;
        };
        if frontier.is_empty() {
            // Empty is not a proof of containment. In particular, a fresh
            // room may advertise a real checkpoint with an empty frontier.
            return false;
        }
        let Ok(vv) = loro::VersionVector::decode(frontier) else {
            // Unreadable frontier → claim NOT contained: the client then
            // fetches the checkpoint, which is always safe (full-state
            // merge), never silently skips history.
            return false;
        };
        // A decodable but empty VV is the encoded-empty Frontier case. It is
        // a vacuous claim just like a zero-length payload.
        if vv.is_empty() {
            return false;
        }
        doc.doc().oplog_vv().includes_vv(&vv)
    }

    fn advance_cursor(&self, cursor: u64) {
        self.persist_with_cursor(cursor);
    }
}

/// `GET /chat2/{chatId}/checkpoint` with Range resume — the fetcher half of
/// the C1 client contract. Partial downloads resume at the byte offset where
/// the previous attempt died (the DO serves 206), which is the entire point
/// of checkpoint-over-HTTP on the 1.2 Mbps links this design targets.
pub struct EdgeCheckpointFetcher {
    http: reqwest::Client,
    edge: EdgeConfig,
    chat_id: String,
}

impl EdgeCheckpointFetcher {
    pub fn new(http: reqwest::Client, edge: EdgeConfig, chat_id: impl Into<String>) -> Self {
        Self {
            http,
            edge,
            chat_id: chat_id.into(),
        }
    }
}

impl CheckpointFetcher for EdgeCheckpointFetcher {
    fn fetch(&self) -> BoxFuture<'static, Result<Vec<u8>, SyncError>> {
        let http = self.http.clone();
        let edge = self.edge.clone();
        let url = format!(
            "{}/chat2/{}/checkpoint",
            edge.url.trim_end_matches('/'),
            self.chat_id
        );
        Box::pin(async move {
            let mut got: Vec<u8> = Vec::new();
            let mut seen_seq: Option<String> = None;
            // Range-resume loop: each attempt continues at the byte where
            // the last one stopped. Attempt count bounds a flapping link;
            // the ChatClient's own deadline bounds wall clock.
            for _attempt in 0..4 {
                let bearer = edge
                    .bearer()
                    .await
                    .ok_or_else(|| SyncError::Auth("signed out".into()))?;
                let mut req = http.get(&url).bearer_auth(&bearer);
                if !got.is_empty() {
                    req = req.header("range", format!("bytes={}-", got.len()));
                }
                let res = match req.send().await {
                    Ok(res) => res,
                    Err(err) => {
                        tracing::warn!(error = %err, "chat2 checkpoint fetch attempt failed");
                        continue;
                    }
                };
                // Resume validator: a NEW checkpoint can commit between
                // attempts, and a Range against it would splice two different
                // blobs (the import fails and burns a whole redial cycle).
                // The DO stamps every response with the checkpoint's seq —
                // on change, restart the download from byte 0.
                let seq = res
                    .headers()
                    .get("x-chat2-checkpoint-seq")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                if seq.is_some() && seen_seq.is_some() && seq != seen_seq {
                    tracing::info!(
                        resumed_at = got.len(),
                        "chat2 checkpoint replaced mid-download; restarting from 0"
                    );
                    got.clear();
                    seen_seq = seq;
                    continue;
                }
                if seq.is_some() {
                    seen_seq = seq;
                }
                match res.status().as_u16() {
                    200 => got.clear(),
                    206 => {}
                    416 => return Err(SyncError::Protocol("checkpoint range beyond end".into())),
                    404 => return Err(SyncError::Protocol("no checkpoint".into())),
                    code => return Err(SyncError::Protocol(format!("checkpoint HTTP {code}"))),
                }
                let mut stream = res;
                loop {
                    match stream.chunk().await {
                        Ok(Some(chunk)) => got.extend_from_slice(&chunk),
                        Ok(None) => return Ok(got),
                        Err(err) => {
                            // Mid-body drop: keep the bytes, resume via Range.
                            tracing::warn!(error = %err, resumed_at = got.len(),
                                "chat2 checkpoint stream dropped; resuming");
                            break;
                        }
                    }
                }
            }
            Err(SyncError::Protocol(
                "checkpoint fetch exhausted resume attempts".into(),
            ))
        })
    }
}

/// Plain-HTTPS Chat2 pull/push transport. The URL is derived from the same
/// EdgeConfig as the WebSocket and checkpoint fetcher; each request obtains a
/// fresh bearer so token refresh and reconnect share one auth source.
pub struct EdgeChatTransport {
    http: reqwest::Client,
    edge: EdgeConfig,
    chat_id: String,
    device_id: String,
}

impl EdgeChatTransport {
    pub fn new(
        http: reqwest::Client,
        edge: EdgeConfig,
        chat_id: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Self {
        Self {
            http,
            edge,
            chat_id: chat_id.into(),
            device_id: device_id.into(),
        }
    }

    fn url(&self) -> String {
        format!(
            "{}/chat2/{}/rows",
            self.edge.url.trim_end_matches('/'),
            self.chat_id
        )
    }
}

impl ChatTransport for EdgeChatTransport {
    fn fetch_rows(&self, after: u64) -> BoxFuture<'static, Result<Vec<u8>, SyncError>> {
        let http = self.http.clone();
        let edge = self.edge.clone();
        let url = self.url();
        let device = self.device_id.clone();
        Box::pin(async move {
            let bearer = edge
                .bearer()
                .await
                .ok_or_else(|| SyncError::Auth("signed out".into()))?;
            let response = http
                .get(url)
                .query(&[
                    ("after", after.to_string()),
                    ("device", device),
                    ("excludeOwn", "0".to_string()),
                ])
                .bearer_auth(bearer)
                .send()
                .await
                .map_err(|err| SyncError::WebSocket(err.to_string()))?;
            if !response.status().is_success() {
                return Err(SyncError::Protocol(format!(
                    "chat pull HTTP {}",
                    response.status()
                )));
            }
            response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|err| SyncError::WebSocket(err.to_string()))
        })
    }

    fn push(
        &self,
        batch_id: String,
        bytes: Vec<u8>,
    ) -> BoxFuture<'static, Result<String, SyncError>> {
        let http = self.http.clone();
        let edge = self.edge.clone();
        let url = self.url();
        let device = self.device_id.clone();
        Box::pin(async move {
            let bearer = edge
                .bearer()
                .await
                .ok_or_else(|| SyncError::Auth("signed out".into()))?;
            let response = http
                .post(url)
                .query(&[("batchId", batch_id), ("device", device)])
                .bearer_auth(bearer)
                .body(bytes)
                .send()
                .await
                .map_err(|err| SyncError::WebSocket(err.to_string()))?;
            if !response.status().is_success() {
                return Err(SyncError::Protocol(format!(
                    "chat push HTTP {}",
                    response.status()
                )));
            }
            response
                .text()
                .await
                .map_err(|err| SyncError::WebSocket(err.to_string()))
        })
    }
}

#[cfg(test)]
mod frontier_tests {
    use super::*;

    fn test_sink(name: &str) -> (EngineChatSink, Arc<SessionDoc>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "cypher-frontier-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Arc::new(DocsStore::open(&dir).expect("store opens"));
        let doc = Arc::new(SessionDoc::from_doc(loro::LoroDoc::new()));
        (EngineChatSink::new(&doc, store, name), doc, dir)
    }

    #[test]
    fn empty_frontier_is_not_contained() {
        let (sink, _doc, dir) = test_sink("empty");
        assert!(
            !sink.contains_frontier(&[]),
            "an empty frontier cannot prove checkpoint containment"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn encoded_empty_frontier_is_not_contained() {
        let (sink, _doc, dir) = test_sink("encoded-empty");
        let encoded_empty = loro::VersionVector::default().encode();
        assert!(
            !sink.contains_frontier(&encoded_empty),
            "an encoded-empty frontier is a vacuous claim"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn non_empty_contained_frontier_is_contained() {
        let (sink, doc, dir) = test_sink("contained");
        doc.doc().get_map("meta").insert("k", "v").expect("insert");
        doc.doc().commit();
        let vv = doc.doc().oplog_vv().encode();
        assert!(sink.contains_frontier(&vv));
        let _ = std::fs::remove_dir_all(dir);
    }
}
