//! ChatClient behavior against a hand-driven server end (channel pipes — no
//! WebSocket): handshake precision, backfill, push/ack retirement, and the
//! reconnect re-push path. Virtual clock (`start_paused`) so backoff and
//! deadlines cost nothing.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::chat_frames::{decode, encode, frame_type};
use tokio::sync::Notify;

mod recovery;

fn queued(id: &str, persisted: bool) -> PendingPush {
    PendingPush {
        batch_id: id.into(),
        bytes: id.as_bytes().to_vec(),
        persisted,
        sent: false,
    }
}

#[test]
fn failed_or_unrelated_ack_cannot_retire_new_updates_or_advance_cursor() {
    let sink = RecordingSink::default();
    let shared = Mutex::new(Shared {
        cursor: 5,
        ..Shared::default()
    });
    let mut first = queued("first", true);
    first.sent = true;
    sink.enqueue_outbox(&first.batch_id, &first.bytes).unwrap();
    sink.enqueue_outbox("second", b"second").unwrap();
    lock(&shared)
        .pending
        .extend([first, queued("second", true)]);
    acknowledge_durable(&shared, &sink, "unknown", 999).unwrap();
    assert_eq!(lock(&shared).cursor, 5);
    assert!(acknowledge_durable(&shared, &sink, "second", 6).is_err());
    sink.ack_fails.store(true, Ordering::SeqCst);
    assert!(acknowledge_durable(&shared, &sink, "first", 6).is_err());
    assert_eq!(lock(&shared).cursor, 5);
    assert_eq!(lock(&shared).pending.len(), 2);
    sink.ack_fails.store(false, Ordering::SeqCst);
    acknowledge_durable(&shared, &sink, "first", 7).unwrap();
    assert_eq!(lock(&shared).cursor, 5);
    assert!(lock(&shared).gap_repair);
    assert_eq!(lock(&shared).pending.front().unwrap().batch_id, "second");
    assert_eq!(
        sink.load_outbox().unwrap(),
        vec![("second".into(), b"second".to_vec())]
    );
}

/// Run `scenario` on its own runtime in a detached thread and fail unless it
/// finishes within `secs`. A regression of the sink lock contract deadlocks
/// the thread that handles the ACK; under `#[tokio::test]` that hangs the
/// whole test process instead of failing one test. The stuck thread is
/// leaked and dies with the process.
fn must_finish_within(secs: u64, scenario: impl FnOnce() + Send + 'static) {
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        scenario();
        let _ = done_tx.send(());
    });
    match done_rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("scenario did not finish within {secs}s: a lock is held across a sink call")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("scenario panicked (see the failure above)")
        }
    }
}

/// A sink that re-locks the client state from inside `acknowledge_outbox`,
/// standing in for the engine's export-on-ack whose Loro commit hook
/// re-enters `enqueue_update`.
#[derive(Default)]
struct RelockingSink {
    inner: RecordingSink,
    shared: Mutex<Option<Arc<Mutex<Shared>>>>,
    relocked: AtomicUsize,
}

impl ChatDocSink for RelockingSink {
    fn load_outbox(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        self.inner.load_outbox()
    }
    fn enqueue_outbox(&self, id: &str, bytes: &[u8]) -> Result<(), String> {
        self.inner.enqueue_outbox(id, bytes)
    }
    fn acknowledge_outbox(&self, id: &str, cursor: u64) -> Result<(), String> {
        let shared = lock(&self.shared).clone();
        if let Some(shared) = shared {
            let _probe = lock(&shared);
            self.relocked.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.acknowledge_outbox(id, cursor)
    }
    fn apply_row(&self, bytes: &[u8], cursor: u64) {
        self.inner.apply_row(bytes, cursor)
    }
    fn apply_checkpoint(&self, bytes: &[u8], cursor: u64) -> Result<(), String> {
        self.inner.apply_checkpoint(bytes, cursor)
    }
    fn contains_frontier(&self, frontier: &[u8]) -> bool {
        self.inner.contains_frontier(frontier)
    }
    fn advance_cursor(&self, cursor: u64) {
        self.inner.advance_cursor(cursor)
    }
}

#[test]
fn acknowledge_runs_the_sink_with_the_client_state_unlocked() {
    must_finish_within(10, || {
        let sink = RelockingSink::default();
        let shared = Arc::new(Mutex::new(Shared::default()));
        *lock(&sink.shared) = Some(shared.clone());
        let mut batch = queued("b", true);
        batch.sent = true;
        sink.enqueue_outbox("b", b"b").unwrap();
        lock(&shared).pending.push_back(batch);
        acknowledge_durable(&shared, &sink, "b", 1).unwrap();
        assert_eq!(sink.relocked.load(Ordering::SeqCst), 1);
        assert_eq!(lock(&shared).cursor, 1);
        assert!(lock(&shared).pending.is_empty());
        assert!(sink.load_outbox().unwrap().is_empty());
    });
}

/// A sink whose `acknowledge_outbox` enqueues a NEW update on the same
/// client — exactly what the engine's Loro local-update hook does when the
/// ack-time export commits a concurrent writer's pending ops.
#[derive(Default)]
struct ReenteringSink {
    inner: RecordingSink,
    client: Mutex<Option<Arc<ChatClient>>>,
    reentered: AtomicUsize,
}

impl ChatDocSink for ReenteringSink {
    fn load_outbox(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        self.inner.load_outbox()
    }
    fn enqueue_outbox(&self, id: &str, bytes: &[u8]) -> Result<(), String> {
        self.inner.enqueue_outbox(id, bytes)
    }
    fn acknowledge_outbox(&self, id: &str, cursor: u64) -> Result<(), String> {
        let client = lock(&self.client).clone();
        if let Some(client) = client
            && self.reentered.fetch_add(1, Ordering::SeqCst) == 0
        {
            client.enqueue_update(b"reentrant".to_vec());
            client.flush_pending();
        }
        self.inner.acknowledge_outbox(id, cursor)
    }
    fn apply_row(&self, bytes: &[u8], cursor: u64) {
        self.inner.apply_row(bytes, cursor)
    }
    fn apply_checkpoint(&self, bytes: &[u8], cursor: u64) -> Result<(), String> {
        self.inner.apply_checkpoint(bytes, cursor)
    }
    fn contains_frontier(&self, frontier: &[u8]) -> bool {
        self.inner.contains_frontier(frontier)
    }
    fn advance_cursor(&self, cursor: u64) {
        self.inner.advance_cursor(cursor)
    }
}

#[test]
fn websocket_ack_survives_a_sink_that_reenters_the_client() {
    must_finish_within(20, || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let sink = Arc::new(ReenteringSink::default());
            let (pipe, mut end) = pipe_pair();
            let (fetch, _) = fetcher(b"");
            let server = tokio::spawn(async move {
                serve_join(&mut end, serde_json::json!({"headSeq":0,"seqFloor":0,"checkpointSeq":0,"checkpointSize":0,"rowCount":0,"rowBytes":0}), &[], vec![], false).await;
                let first = expect_kind(&mut end, frame_type::PUSH).await;
                assert_eq!(first.payload, b"first");
                send(
                    &end,
                    frame_type::ACK,
                    serde_json::json!({"batchId":first.header["batchId"],"seq":1,"dup":false}),
                    &[],
                )
                .await;
                // The ack's sink call enqueued this; the session loop must
                // still be alive to push it.
                let second = expect_kind(&mut end, frame_type::PUSH).await;
                assert_eq!(second.payload, b"reentrant");
                send(
                    &end,
                    frame_type::ACK,
                    serde_json::json!({"batchId":second.header["batchId"],"seq":2,"dup":false}),
                    &[],
                )
                .await;
            });
            let client = Arc::new(
                ChatClient::connect_with_tuned(
                    connector(vec![pipe]),
                    sink.clone(),
                    fetch,
                    "d",
                    0,
                    ChatTuning::default(),
                )
                .await
                .unwrap(),
            );
            *lock(&sink.client) = Some(client.clone());
            client.enqueue_update(b"first".to_vec());
            client.flush_pending();
            server.await.unwrap();
            // The server returns right after sending the second ACK; wait for
            // the client to process it before asserting on the sink.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while !sink.load_outbox().unwrap().is_empty() {
                assert!(tokio::time::Instant::now() < deadline, "second batch never retired");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(sink.reentered.load(Ordering::SeqCst), 2);
        });
        runtime.shutdown_background();
    });
}

#[tokio::test]
async fn http_persist_failure_blocks_send_and_ack_failure_retries_identical_payload() {
    let sink = RecordingSink::default();
    let shared = Arc::new(Mutex::new(Shared::default()));
    lock(&shared).pending.push_back(queued("b", false));
    let transport = FixedChatTransport {
        body: empty_chat_pull(1),
        pushes: Mutex::new(vec![]),
    };
    let (fetcher, _) = fetcher(b"");
    let (events, _) = broadcast::channel(10);
    sink.write_fails.store(true, Ordering::SeqCst);
    assert!(
        http_sync_once(&transport, &shared, &sink, fetcher.as_ref(), &events)
            .await
            .is_err()
    );
    assert!(lock(&transport.pushes).is_empty());
    sink.write_fails.store(false, Ordering::SeqCst);
    sink.ack_fails.store(true, Ordering::SeqCst);
    assert!(
        http_sync_once(&transport, &shared, &sink, fetcher.as_ref(), &events)
            .await
            .is_err()
    );
    assert_eq!(lock(&shared).cursor, 0);
    assert_eq!(lock(&shared).pending.len(), 1);
    sink.ack_fails.store(false, Ordering::SeqCst);
    http_sync_once(&transport, &shared, &sink, fetcher.as_ref(), &events)
        .await
        .unwrap();
    assert!(lock(&shared).pending.is_empty());
    assert!(sink.load_outbox().unwrap().is_empty());
    assert_eq!(
        *lock(&transport.pushes),
        vec![("b".into(), b"b".to_vec()); 2]
    );
}

#[tokio::test]
async fn outbox_load_failure_does_not_start_an_empty_client() {
    let sink = Arc::new(RecordingSink::default());
    sink.load_fails.store(true, Ordering::SeqCst);
    let (fetcher, _) = fetcher(b"");
    assert!(
        ChatClient::connect_with_tuned(
            connector(vec![]),
            sink,
            fetcher,
            "d",
            0,
            ChatTuning::default()
        )
        .await
        .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn new_client_restores_same_batch_ids_and_appends_new_work_after_them() {
    let sink = Arc::new(RecordingSink::default());
    sink.enqueue_outbox("saved-first", b"old").unwrap();
    let (pipe, mut end) = pipe_pair();
    let (fetch, _) = fetcher(b"");
    let server = tokio::spawn(async move {
        serve_join(&mut end, serde_json::json!({"headSeq":0,"seqFloor":0,"checkpointSeq":0,"checkpointSize":0,"rowCount":0,"rowBytes":0}), &[], vec![], false).await;
        let old = expect_kind(&mut end, frame_type::PUSH).await;
        assert_eq!(old.header["batchId"], "saved-first");
        assert_eq!(old.payload, b"old");
        send(
            &end,
            frame_type::ACK,
            serde_json::json!({"batchId":"saved-first","seq":1,"dup":true}),
            &[],
        )
        .await;
        let new = expect_kind(&mut end, frame_type::PUSH).await;
        assert_eq!(new.payload, b"new");
        assert_ne!(new.header["batchId"], "saved-first");
        send(
            &end,
            frame_type::ACK,
            serde_json::json!({"batchId":new.header["batchId"],"seq":2,"dup":false}),
            &[],
        )
        .await;
        end
    });
    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe]),
        sink.clone(),
        fetch,
        "d",
        0,
        ChatTuning::default(),
    )
    .await
    .unwrap();
    client.enqueue_update(b"new".to_vec());
    let _keep_alive = server.await.unwrap();
    for _ in 0..100 {
        if client.stats().pending_pushes == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(sink.load_outbox().unwrap().is_empty());
    assert_eq!(client.stats().pending_pushes, 0);
    client.shutdown().await;
}

#[test]
fn cumulative_export_uses_source_frontier_not_an_empty_document() {
    let first = loro::LoroDoc::new();
    let text = first.get_text("text");
    text.insert(0, "one").unwrap();
    first.commit();
    let baseline = first
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .unwrap();
    let base = first.oplog_vv();
    text.insert(3, " two").unwrap();
    first.commit();
    let partial = first.export(loro::ExportMode::updates(&base)).unwrap();
    let empty = loro::LoroDoc::new();
    empty.import(&partial).unwrap();
    assert_ne!(
        empty.get_text("text").to_string(),
        "one two",
        "missing causal prefix must not be treated as a complete merge source"
    );
    let merged = first.export(loro::ExportMode::updates(&base)).unwrap();
    let restored = loro::LoroDoc::new();
    restored.import(&baseline).unwrap();
    restored.import(&merged).unwrap();
    assert_eq!(restored.get_text("text").to_string(), "one two");
}

#[test]
fn coalescing_never_exports_an_update_with_unresolved_causal_history() {
    let source = loro::LoroDoc::new();
    let text = source.get_text("text");
    text.insert(0, "one").unwrap();
    source.commit();
    let first = source
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .unwrap();
    let base = source.oplog_vv();
    text.insert(3, " two").unwrap();
    source.commit();
    let second = source.export(loro::ExportMode::updates(&base)).unwrap();

    let merged = merge_loro_updates(&[first.clone(), second.clone()]).unwrap();
    let restored = loro::LoroDoc::new();
    restored.import(&merged).unwrap();
    assert_eq!(restored.get_text("text").to_string(), "one two");

    // A fresh temporary doc cannot safely turn a dependent delta into a
    // cumulative export. Falling back to separate durable batches is safer
    // than sending a payload that appears valid but drops the prefix.
    assert!(merge_loro_updates(&[second]).is_none());
}

// ── plumbing: linked pipes + scripted connector ─────────────────────────────

struct ServerEnd {
    tx: mpsc::Sender<Vec<u8>>,
    rx: mpsc::Receiver<Vec<u8>>,
}

fn pipe_pair() -> (BinPipe, ServerEnd) {
    let (c2s_tx, c2s_rx) = mpsc::channel(64);
    let (s2c_tx, s2c_rx) = mpsc::channel(64);
    (
        BinPipe {
            tx: c2s_tx,
            rx: s2c_rx,
        },
        ServerEnd {
            tx: s2c_tx,
            rx: c2s_rx,
        },
    )
}

struct ChanConnector {
    pipes: Mutex<VecDeque<BinPipe>>,
}

impl BinConnector for ChanConnector {
    fn connect(&self) -> BoxFuture<'static, Result<BinPipe, SyncError>> {
        let pipe = lock(&self.pipes).pop_front();
        Box::pin(async move { pipe.ok_or(SyncError::Closed) })
    }
}

// ── sink + fetcher doubles ──────────────────────────────────────────────────

#[derive(Default)]
struct RecordingSink {
    outbox: Mutex<Vec<(String, Vec<u8>)>>,
    write_fails: std::sync::atomic::AtomicBool,
    ack_fails: std::sync::atomic::AtomicBool,
    load_fails: std::sync::atomic::AtomicBool,
    rows: Mutex<Vec<(Vec<u8>, u64)>>,
    checkpoints: Mutex<Vec<(Vec<u8>, u64)>>,
    cursor_advances: Mutex<Vec<u64>>,
    frontier_contained: std::sync::atomic::AtomicBool,
}

impl ChatDocSink for RecordingSink {
    fn load_outbox(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        if self.load_fails.load(Ordering::SeqCst) {
            return Err("injected load failure".into());
        }
        Ok(lock(&self.outbox).clone())
    }
    fn enqueue_outbox(&self, id: &str, bytes: &[u8]) -> Result<(), String> {
        if self.write_fails.load(Ordering::SeqCst) {
            return Err("injected write failure".into());
        }
        let mut rows = lock(&self.outbox);
        if let Some((_, old)) = rows.iter().find(|(key, _)| key == id) {
            assert_eq!(old, bytes);
        } else {
            rows.push((id.into(), bytes.into()));
        }
        Ok(())
    }
    fn acknowledge_outbox(&self, id: &str, cursor: u64) -> Result<(), String> {
        if self.ack_fails.load(Ordering::SeqCst) {
            return Err("injected ACK persistence failure".into());
        }
        self.advance_cursor(cursor);
        lock(&self.outbox).retain(|(key, _)| key != id);
        Ok(())
    }
    fn apply_row(&self, bytes: &[u8], cursor: u64) {
        lock(&self.rows).push((bytes.to_vec(), cursor));
    }
    fn apply_checkpoint(&self, bytes: &[u8], cursor: u64) -> Result<(), String> {
        lock(&self.checkpoints).push((bytes.to_vec(), cursor));
        Ok(())
    }
    fn contains_frontier(&self, _frontier: &[u8]) -> bool {
        self.frontier_contained
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    fn advance_cursor(&self, cursor: u64) {
        lock(&self.cursor_advances).push(cursor);
    }
}

struct FixedFetcher {
    bytes: Vec<u8>,
    calls: Arc<std::sync::atomic::AtomicU64>,
}

struct FixedChatTransport {
    body: Vec<u8>,
    pushes: Mutex<Vec<(String, Vec<u8>)>>,
}

impl ChatTransport for FixedChatTransport {
    fn fetch_rows(&self, _after: u64) -> BoxFuture<'static, Result<Vec<u8>, SyncError>> {
        let body = self.body.clone();
        Box::pin(async move { Ok(body) })
    }

    fn push(
        &self,
        batch_id: String,
        bytes: Vec<u8>,
    ) -> BoxFuture<'static, Result<String, SyncError>> {
        lock(&self.pushes).push((batch_id.clone(), bytes));
        Box::pin(async move {
            Ok(serde_json::json!({
                "batchId": batch_id,
                "seq": 1,
                "dup": false
            })
            .to_string())
        })
    }
}

impl CheckpointFetcher for FixedFetcher {
    fn fetch(&self) -> BoxFuture<'static, Result<Vec<u8>, SyncError>> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bytes = self.bytes.clone();
        Box::pin(async move { Ok(bytes) })
    }
}

struct GatedChatTransport {
    body: Vec<u8>,
    pushes: AtomicUsize,
    first_push_started: Arc<Notify>,
    release_first_push: Arc<Notify>,
    hang: bool,
}

impl GatedChatTransport {
    fn new(body: Vec<u8>, hang: bool) -> Arc<Self> {
        Arc::new(Self {
            body,
            pushes: AtomicUsize::new(0),
            first_push_started: Arc::new(Notify::new()),
            release_first_push: Arc::new(Notify::new()),
            hang,
        })
    }
}

impl ChatTransport for GatedChatTransport {
    fn fetch_rows(&self, _after: u64) -> BoxFuture<'static, Result<Vec<u8>, SyncError>> {
        let body = self.body.clone();
        Box::pin(async move { Ok(body) })
    }

    fn push(
        &self,
        batch_id: String,
        _bytes: Vec<u8>,
    ) -> BoxFuture<'static, Result<String, SyncError>> {
        let index = self.pushes.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            self.first_push_started.notify_one();
        }
        if self.hang {
            return Box::pin(std::future::pending());
        }
        if index == 0 {
            let release = self.release_first_push.clone();
            return Box::pin(async move {
                release.notified().await;
                Ok(serde_json::json!({
                    "batchId": batch_id,
                    "seq": 1
                })
                .to_string())
            });
        }
        Box::pin(async move {
            Ok(serde_json::json!({
                "batchId": batch_id,
                "seq": index as u64 + 1
            })
            .to_string())
        })
    }
}

fn empty_chat_pull(head_seq: u64) -> Vec<u8> {
    framed_pull(vec![
        encode(
            frame_type::STATE,
            &serde_json::json!({
                "headSeq": head_seq,
                "seqFloor": 0,
                "checkpointSeq": 0,
                "checkpointSize": 0,
                "rowCount": 0,
                "rowBytes": 0
            }),
            &[],
        ),
        encode(
            frame_type::ROWS_DONE,
            &serde_json::json!({ "headSeq": head_seq }),
            &[],
        ),
    ])
}

// ── server-side script helpers ──────────────────────────────────────────────

async fn expect_kind(end: &mut ServerEnd, kind: u8) -> wire::WireFrame {
    // Reads exactly ONE frame and requires it to be `kind` — it never skipped
    // ahead, because the panic below was unconditional. The `loop` that used to
    // wrap this could not reach a second iteration (clippy::never_loop, which
    // is deny-by-default and failed the whole workspace lint).
    let bytes = end.rx.recv().await.expect("client hung up");
    let frame = decode(&bytes).expect("client sent undecodable frame");
    if frame.kind != kind {
        panic!("expected frame {kind:#x}, got {:#x}", frame.kind);
    }
    frame
}

async fn send(end: &ServerEnd, kind: u8, header: serde_json::Value, payload: &[u8]) {
    end.tx.send(encode(kind, &header, payload)).await.unwrap();
}

fn framed_pull(frames: Vec<Vec<u8>>) -> Vec<u8> {
    let mut body = Vec::new();
    for frame in frames {
        body.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        body.extend_from_slice(&frame);
    }
    body
}

#[tokio::test]
async fn https_pull_applies_only_contiguous_rows() {
    let sink = Arc::new(RecordingSink::default());
    let transport = Arc::new(FixedChatTransport {
        body: framed_pull(vec![
            encode(
                frame_type::STATE,
                &serde_json::json!({
                    "headSeq": 3,
                    "seqFloor": 0,
                    "checkpointSeq": 0,
                    "checkpointSize": 0,
                    "rowCount": 3,
                    "rowBytes": 3
                }),
                &[],
            ),
            encode(
                frame_type::ROW,
                &serde_json::json!({"seq": 1, "device": "a", "batchId": "a"}),
                b"1",
            ),
            encode(
                frame_type::ROW,
                &serde_json::json!({"seq": 3, "device": "b", "batchId": "b"}),
                b"3",
            ),
        ]),
        pushes: Mutex::new(Vec::new()),
    });
    let shared = Arc::new(Mutex::new(Shared::default()));
    let (events, _) = broadcast::channel(16);
    let fetcher = Arc::new(FixedFetcher {
        bytes: Vec::new(),
        calls: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    });
    http_sync_once(
        transport.as_ref(),
        &shared,
        sink.as_ref(),
        fetcher.as_ref(),
        &events,
    )
    .await
    .unwrap();
    assert_eq!(lock(&shared).cursor, 1);
    assert_eq!(lock(&sink.rows).as_slice(), &[(b"1".to_vec(), 1)]);
}

/// Answer hello with `state`, then serve the rows request with `rows`.
/// Returns the observed `after` from the rows request. `expect_exclude`
/// pins the F1 rule: the process's FIRST backfill must redownload own rows
/// (false), same-process reconnects skip them (true).
async fn serve_join(
    end: &mut ServerEnd,
    state: serde_json::Value,
    frontier: &[u8],
    rows: Vec<(u64, &str, Vec<u8>)>,
    expect_exclude: bool,
) -> u64 {
    let hello = expect_kind(end, frame_type::HELLO).await;
    assert!(hello.header["device"].is_string());
    let head_seq = state["headSeq"].as_u64().unwrap();
    send(end, frame_type::STATE, state, frontier).await;
    let req = expect_kind(end, frame_type::ROWS_REQ).await;
    assert_eq!(req.header["excludeOwn"], expect_exclude);
    let after = req.header["after"].as_u64().unwrap();
    for (seq, device, bytes) in rows {
        send(
            end,
            frame_type::ROW,
            serde_json::json!({"seq": seq, "device": device, "batchId": format!("b{seq}")}),
            &bytes,
        )
        .await;
    }
    send(
        end,
        frame_type::ROWS_DONE,
        serde_json::json!({"headSeq": head_seq}),
        &[],
    )
    .await;
    after
}

fn connector(pipes: Vec<BinPipe>) -> Arc<ChanConnector> {
    Arc::new(ChanConnector {
        pipes: Mutex::new(pipes.into_iter().collect()),
    })
}

fn fetcher(bytes: &[u8]) -> (Arc<FixedFetcher>, Arc<std::sync::atomic::AtomicU64>) {
    let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
    (
        Arc::new(FixedFetcher {
            bytes: bytes.to_vec(),
            calls: calls.clone(),
        }),
        calls,
    )
}

// ── plan_catch_up (pure) ────────────────────────────────────────────────────

#[test]
fn catch_up_plan_covers_the_decision_table() {
    let state = |head: u64, ckpt: u64| wire::StateHeader {
        head_seq: head,
        seq_floor: ckpt,
        checkpoint_seq: ckpt,
        checkpoint_size: if ckpt > 0 { 1000 } else { 0 },
        row_count: 0,
        row_bytes: 0,
    };
    // Seeded-at-zero room (M1): checkpointSeq 0 but a real blob — the
    // presence test is SIZE; a fresh reader must fetch the seed.
    let seeded = wire::StateHeader {
        head_seq: 0,
        seq_floor: 0,
        checkpoint_seq: 0,
        checkpoint_size: 276_000,
        row_count: 0,
        row_bytes: 0,
    };
    assert_eq!(
        plan_catch_up(0, &seeded, false),
        CatchUpPlan::CheckpointThenRows { after: 0 }
    );
    assert_eq!(
        plan_catch_up(0, &seeded, true),
        CatchUpPlan::RowsOnly { after: 0 }
    );
    // Empty room / no checkpoint: rows from the cursor.
    assert_eq!(
        plan_catch_up(0, &state(0, 0), true),
        CatchUpPlan::RowsOnly { after: 0 }
    );
    assert_eq!(
        plan_catch_up(4, &state(9, 0), true),
        CatchUpPlan::RowsOnly { after: 4 }
    );
    // Frontier contained: skip the checkpoint even from an older cursor.
    assert_eq!(
        plan_catch_up(2, &state(9, 5), true),
        CatchUpPlan::RowsOnly { after: 5 }
    );
    assert_eq!(
        plan_catch_up(7, &state(9, 5), true),
        CatchUpPlan::RowsOnly { after: 7 }
    );
    // Frontier missing: checkpoint first, rows after it.
    assert_eq!(
        plan_catch_up(2, &state(9, 5), false),
        CatchUpPlan::CheckpointThenRows { after: 5 }
    );
    // Server lost state (cursor ahead of head): cursor is meaningless.
    assert_eq!(
        plan_catch_up(50, &state(3, 0), true),
        CatchUpPlan::RowsOnly { after: 0 }
    );
}

// ── end-to-end actor behavior ───────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn fresh_join_backfills_rows_and_advances_cursor() {
    let (pipe, mut end) = pipe_pair();
    let sink = Arc::new(RecordingSink::default());
    let (fetch, fetch_calls) = fetcher(b"");

    let server = tokio::spawn(async move {
        let after = serve_join(
            &mut end,
            serde_json::json!({"headSeq": 2, "seqFloor": 0, "checkpointSeq": 0,
                "checkpointSize": 0, "rowCount": 2, "rowBytes": 64}),
            &[],
            vec![(1, "dev-b", vec![0xaa]), (2, "dev-b", vec![0xbb])],
            false,
        )
        .await;
        assert_eq!(after, 0);
        end
    });

    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe]),
        sink.clone(),
        fetch,
        "dev-a",
        0,
        ChatTuning::default(),
    )
    .await
    .expect("join succeeds");
    server.await.unwrap();

    assert_eq!(
        *lock(&sink.rows),
        vec![(vec![0xaa], 1), (vec![0xbb], 2)],
        "both remote rows imported in seq order"
    );
    assert_eq!(fetch_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    let stats = client.stats();
    assert!(stats.connected);
    assert_eq!(stats.cursor, 2);
    client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn contained_frontier_skips_the_checkpoint_download() {
    let (pipe, mut end) = pipe_pair();
    let sink = Arc::new(RecordingSink::default());
    sink.frontier_contained
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (fetch, fetch_calls) = fetcher(b"never");

    let server = tokio::spawn(async move {
        let after = serve_join(
            &mut end,
            serde_json::json!({"headSeq": 8, "seqFloor": 5, "checkpointSeq": 5,
                "checkpointSize": 160_000, "rowCount": 3, "rowBytes": 900}),
            &[1, 2, 3],
            vec![
                (6, "dev-b", vec![6]),
                (7, "dev-b", vec![7]),
                (8, "dev-b", vec![8]),
            ],
            false,
        )
        .await;
        // Client-side precision: cursor was 0 but the frontier is local —
        // skip straight past the checkpointed span.
        assert_eq!(after, 5);
        end
    });

    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe]),
        sink.clone(),
        fetch,
        "dev-a",
        0,
        ChatTuning::default(),
    )
    .await
    .expect("join succeeds");
    server.await.unwrap();

    assert_eq!(fetch_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert!(lock(&sink.checkpoints).is_empty());
    assert_eq!(lock(&sink.rows).len(), 3);
    assert_eq!(client.stats().cursor, 8);
    client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn missing_frontier_fetches_and_imports_the_checkpoint_first() {
    let (pipe, mut end) = pipe_pair();
    let sink = Arc::new(RecordingSink::default());
    let (fetch, fetch_calls) = fetcher(b"checkpoint-bytes");

    let server = tokio::spawn(async move {
        let after = serve_join(
            &mut end,
            serde_json::json!({"headSeq": 6, "seqFloor": 5, "checkpointSeq": 5,
                "checkpointSize": 16, "rowCount": 1, "rowBytes": 10}),
            &[9, 9, 9],
            vec![(6, "dev-b", vec![6])],
            false,
        )
        .await;
        assert_eq!(after, 5, "rows resume after the checkpoint");
        end
    });

    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe]),
        sink.clone(),
        fetch,
        "dev-a",
        2,
        ChatTuning::default(),
    )
    .await
    .expect("join succeeds");
    server.await.unwrap();

    assert_eq!(fetch_calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(
        *lock(&sink.checkpoints),
        vec![(b"checkpoint-bytes".to_vec(), 5)]
    );
    assert_eq!(*lock(&sink.rows), vec![(vec![6u8], 6)]);
    client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn unacked_pushes_survive_reconnect_and_acks_retire_them() {
    let (pipe1, mut end1) = pipe_pair();
    let (pipe2, mut end2) = pipe_pair();
    let sink = Arc::new(RecordingSink::default());
    let (fetch, _) = fetcher(b"");

    let empty_state = serde_json::json!({"headSeq": 0, "seqFloor": 0,
        "checkpointSeq": 0, "checkpointSize": 0, "rowCount": 0, "rowBytes": 0});

    let s1 = tokio::spawn({
        let state = empty_state.clone();
        async move {
            serve_join(&mut end1, state, &[], vec![], false).await;
            // Receive the push but die before acking — the client must
            // re-push the SAME batch id on the next session.
            let push = expect_kind(&mut end1, frame_type::PUSH).await;
            let batch_id = push.header["batchId"].as_str().unwrap().to_string();
            assert_eq!(push.payload, vec![0xd1u8]);
            drop(end1); // socket dies
            batch_id
        }
    });

    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe1, pipe2]),
        sink.clone(),
        fetch,
        "dev-a",
        0,
        ChatTuning::default(),
    )
    .await
    .expect("join succeeds");

    client.enqueue_update(vec![0xd1]);
    let first_batch = s1.await.unwrap();
    assert_eq!(
        client.stats().pending_pushes,
        1,
        "unacked batch stays queued"
    );

    // Second session: same handshake, then the replayed push gets acked.
    let s2 = tokio::spawn({
        let state = empty_state.clone();
        async move {
            serve_join(&mut end2, state, &[], vec![], true).await;
            let push = expect_kind(&mut end2, frame_type::PUSH).await;
            let batch_id = push.header["batchId"].as_str().unwrap().to_string();
            send(
                &end2,
                frame_type::ACK,
                serde_json::json!({"batchId": batch_id, "seq": 1, "dup": false}),
                &[],
            )
            .await;
            (batch_id, end2)
        }
    });
    let (replayed_batch, _keep_alive) = s2.await.unwrap();
    assert_eq!(
        replayed_batch, first_batch,
        "reconnect replays the same batch id"
    );

    // Ack lands asynchronously — wait for the pending queue to drain.
    let mut events = client.events();
    while client.stats().pending_pushes > 0 {
        let _ = events.recv().await;
    }
    assert_eq!(client.stats().cursor, 1, "ack advanced the cursor");
    assert_eq!(*lock(&sink.cursor_advances), vec![1]);
    client.shutdown().await;
}

// ── 2026-08-10 review fixes (F1–F4) ─────────────────────────────────────────

struct PendingFetcher;
impl CheckpointFetcher for PendingFetcher {
    fn fetch(&self) -> BoxFuture<'static, Result<Vec<u8>, SyncError>> {
        Box::pin(std::future::pending())
    }
}

/// F2: a permanent server verdict (`too_large`) retires the batch from the
/// replay queue; a transient one (`quota`) keeps it and re-pushes on the
/// retry clock without waiting for a new enqueue.
#[tokio::test(start_paused = true)]
async fn permanent_rejection_retires_transient_keeps_and_retries() {
    let (pipe, mut end) = pipe_pair();
    let sink = Arc::new(RecordingSink::default());
    let (fetch, _) = fetcher(b"");
    let empty_state = serde_json::json!({"headSeq": 0, "seqFloor": 0,
        "checkpointSeq": 0, "checkpointSize": 0, "rowCount": 0, "rowBytes": 0});

    let server = tokio::spawn(async move {
        serve_join(&mut end, empty_state, &[], vec![], false).await;
        // First batch: permanently rejected.
        let doomed = expect_kind(&mut end, frame_type::PUSH).await;
        let doomed_id = doomed.header["batchId"].as_str().unwrap().to_string();
        send(
            &end,
            frame_type::ERROR,
            serde_json::json!({"code": "too_large", "message": "push rejected", "batchId": doomed_id}),
            &[],
        )
        .await;
        // Second batch: quota-limited once, then replayed by the retry clock
        // (no further enqueue nudges) and acked.
        let quotad = expect_kind(&mut end, frame_type::PUSH).await;
        let quotad_id = quotad.header["batchId"].as_str().unwrap().to_string();
        send(
            &end,
            frame_type::ERROR,
            serde_json::json!({"code": "quota", "message": "later", "batchId": quotad_id}),
            &[],
        )
        .await;
        let replay = expect_kind(&mut end, frame_type::PUSH).await;
        assert_eq!(
            replay.header["batchId"].as_str().unwrap(),
            quotad_id,
            "retry clock replays the SAME quota-limited batch"
        );
        send(
            &end,
            frame_type::ACK,
            serde_json::json!({"batchId": quotad_id, "seq": 1, "dup": false}),
            &[],
        )
        .await;
        end
    });

    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe]),
        sink.clone(),
        fetch,
        "dev-a",
        0,
        ChatTuning::default(),
    )
    .await
    .expect("join succeeds");

    let mut events = client.events();
    client.enqueue_update(vec![0xd0]); // doomed
    // Retirement lands asynchronously; PushRejected marks it.
    loop {
        if let Ok(ChatEvent::PushRejected) = events.recv().await {
            break;
        }
    }
    assert_eq!(client.stats().pending_pushes, 0, "doomed batch retired");

    client.enqueue_update(vec![0xb0]);
    let _keep = server.await.unwrap();
    while client.stats().pending_pushes > 0 {
        let _ = events.recv().await;
    }
    assert_eq!(client.stats().cursor, 1, "quota batch eventually landed");
    assert!(client.stats().rejected >= 2);
    client.shutdown().await;
}

/// F2: batches over the row cap never enter the replay queue.
#[tokio::test(start_paused = true)]
async fn oversized_enqueue_is_refused_at_the_door() {
    let (pipe, mut end) = pipe_pair();
    let sink = Arc::new(RecordingSink::default());
    let (fetch, _) = fetcher(b"");
    let empty_state = serde_json::json!({"headSeq": 0, "seqFloor": 0,
        "checkpointSeq": 0, "checkpointSize": 0, "rowCount": 0, "rowBytes": 0});
    let server = tokio::spawn(async move {
        serve_join(&mut end, empty_state, &[], vec![], false).await;
        end
    });
    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe]),
        sink,
        fetch,
        "dev-a",
        0,
        ChatTuning::default(),
    )
    .await
    .expect("join succeeds");
    let _keep = server.await.unwrap();

    // Exactly the DO row cap (1 MiB): the frame header would push the WS
    // message over the runtime's 1 MiB cap and the socket would close with
    // NO error frame to retire the batch — the gate must refuse it (this is
    // why MAX_PUSH_BYTES carries headroom below the row cap).
    client.enqueue_update(vec![0u8; 1024 * 1024]);
    let stats = client.stats();
    assert_eq!(stats.pending_pushes, 0, "boundary batch not queued");
    assert_eq!(stats.rejected, 1);
    client.shutdown().await;
}

/// F4 (second half): `shutdown()` must complete promptly even while the
/// actor is parked inside a hung checkpoint fetch.
#[tokio::test(start_paused = true)]
async fn shutdown_interrupts_a_hung_checkpoint_fetch() {
    let (pipe1, mut end1) = pipe_pair();
    let (pipe2, mut end2) = pipe_pair();
    let sink = Arc::new(RecordingSink::default()); // frontier NOT contained
    let empty_state = serde_json::json!({"headSeq": 0, "seqFloor": 0,
        "checkpointSeq": 0, "checkpointSize": 0, "rowCount": 0, "rowBytes": 0});

    // Session 1: clean join (no checkpoint), then the socket dies.
    let s1 = tokio::spawn(async move {
        serve_join(&mut end1, empty_state, &[], vec![], false).await;
        drop(end1);
    });
    // Session 2: a checkpoint appeared — the client must fetch, and hangs.
    let s2 = tokio::spawn(async move {
        let _hello = expect_kind(&mut end2, frame_type::HELLO).await;
        send(
            &end2,
            frame_type::STATE,
            serde_json::json!({"headSeq": 9, "seqFloor": 5, "checkpointSeq": 5,
                "checkpointSize": 1000, "rowCount": 4, "rowBytes": 40}),
            &[7, 7, 7],
        )
        .await;
        end2 // keep the pipe alive; only the fetch is stuck
    });

    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe1, pipe2]),
        sink,
        Arc::new(PendingFetcher),
        "dev-a",
        0,
        ChatTuning::default(),
    )
    .await
    .expect("first join succeeds");
    s1.await.unwrap();
    let _keep = s2.await.unwrap();
    // Let the actor redial and park inside the hung fetch.
    tokio::time::sleep(Duration::from_secs(2)).await;
    tokio::time::timeout(Duration::from_secs(30), client.shutdown())
        .await
        .expect("shutdown must not hang on a stuck fetch");
}

/// F3: a server whose headSeq fell behind our cursor (reset/wiped room) is
/// SURFACED — counted in stats, honest head_seq — not silently absorbed.
#[tokio::test(start_paused = true)]
async fn server_reset_is_counted_and_head_seq_stays_honest() {
    let (pipe, mut end) = pipe_pair();
    let sink = Arc::new(RecordingSink::default());
    let (fetch, _) = fetcher(b"");
    let server = tokio::spawn(async move {
        let after = serve_join(
            &mut end,
            serde_json::json!({"headSeq": 3, "seqFloor": 0, "checkpointSeq": 0,
                "checkpointSize": 0, "rowCount": 3, "rowBytes": 30}),
            &[],
            vec![
                (1, "dev-b", vec![1]),
                (2, "dev-b", vec![2]),
                (3, "dev-b", vec![3]),
            ],
            false,
        )
        .await;
        assert_eq!(after, 0, "meaningless cursor treated as fresh");
        end
    });
    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe]),
        sink,
        fetch,
        "dev-a",
        50, // persisted cursor from before the room was wiped
        ChatTuning::default(),
    )
    .await
    .expect("join succeeds");
    let _keep = server.await.unwrap();

    let stats = client.stats();
    assert_eq!(stats.server_resets, 1, "reset visible to the host");
    assert_eq!(stats.head_seq, 3, "server view not masked by the cursor");
    assert_eq!(stats.cursor, 3, "cursor re-anchored by the backfill");
    client.shutdown().await;
}

/// F4: a checkpoint fetch that never resolves fails the first join within
/// the deadline instead of hanging the actor (and shutdown) forever.
#[tokio::test(start_paused = true)]
async fn hung_checkpoint_fetch_fails_the_join_within_deadline() {
    let (pipe, mut end) = pipe_pair();
    let sink = Arc::new(RecordingSink::default()); // frontier NOT contained
    let server = tokio::spawn(async move {
        let hello = expect_kind(&mut end, frame_type::HELLO).await;
        assert!(hello.header["device"].is_string());
        send(
            &end,
            frame_type::STATE,
            serde_json::json!({"headSeq": 9, "seqFloor": 5, "checkpointSeq": 5,
                "checkpointSize": 1000, "rowCount": 4, "rowBytes": 40}),
            &[7, 7, 7],
        )
        .await;
        end // keep the pipe alive; the fetch is what must time out
    });
    let joined = ChatClient::connect_with_tuned(
        connector(vec![pipe]),
        sink,
        Arc::new(PendingFetcher),
        "dev-a",
        0,
        ChatTuning::default(),
    )
    .await;
    assert!(joined.is_err(), "hung fetch must not hang the join");
    let _keep = server.await.unwrap();
}

/// M1 seed shape: checkpointSeq 0 with a real blob. BOTH presence tests
/// (plan_catch_up AND run_session's frontier short-circuit) must key on
/// SIZE — the 2026-08-10 gauntlet caught seq==0 short-circuits in each,
/// which would have made every adopted reader skip the seed and render an
/// EMPTY transcript.
#[tokio::test(start_paused = true)]
async fn seeded_at_zero_room_fetches_the_checkpoint() {
    let (pipe, mut end) = pipe_pair();
    let sink = Arc::new(RecordingSink::default()); // frontier NOT contained
    let (fetch, fetch_calls) = fetcher(b"seed-checkpoint-bytes");

    let server = tokio::spawn(async move {
        let after = serve_join(
            &mut end,
            serde_json::json!({"headSeq": 0, "seqFloor": 0, "checkpointSeq": 0,
                "checkpointSize": 276_342, "rowCount": 0, "rowBytes": 0}),
            &[7, 7, 7], // non-empty frontier the fresh doc can't contain
            vec![],
            false,
        )
        .await;
        assert_eq!(after, 0);
        end
    });

    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe]),
        sink.clone(),
        fetch,
        "dev-a",
        0,
        ChatTuning::default(),
    )
    .await
    .expect("join succeeds");
    let _keep = server.await.unwrap();

    assert_eq!(
        fetch_calls.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "seed checkpoint fetched despite checkpointSeq == 0"
    );
    assert_eq!(
        *lock(&sink.checkpoints),
        vec![(b"seed-checkpoint-bytes".to_vec(), 0)]
    );
    client.shutdown().await;
}

// ── row-gap contiguity + repair ────────────────────────────────────────────

/// A live broadcast can outrun the backfill during a join, delivering seq N
/// while seq N-1 was never received. The cursor must hold at the last
/// contiguous sequence and request a bounded repair instead of skipping the
/// hole.
#[tokio::test(start_paused = true)]
async fn live_row_gap_holds_cursor_and_repairs() {
    let (pipe, mut end) = pipe_pair();
    let sink = Arc::new(RecordingSink::default());
    let (fetch, _) = fetcher(b"");

    let server = tokio::spawn(async move {
        let after = serve_join(
            &mut end,
            serde_json::json!({"headSeq": 1, "seqFloor": 0, "checkpointSeq": 0,
                "checkpointSize": 0, "rowCount": 1, "rowBytes": 32}),
            &[],
            vec![(1, "dev-b", vec![0x01])],
            false,
        )
        .await;
        assert_eq!(after, 0);

        // Live frame with a hole: seq 3 arrives, seq 2 did not.
        send(
            &end,
            frame_type::ROW,
            serde_json::json!({"seq": 3, "device": "dev-b", "batchId": "b3"}),
            &[0x03],
        )
        .await;
        let req = expect_kind(&mut end, frame_type::ROWS_REQ).await;
        assert_eq!(
            req.header["after"].as_u64().unwrap(),
            1,
            "repair starts at the honest cursor"
        );
        for (seq, bytes) in [(2u64, vec![0x02u8]), (3, vec![0x03])] {
            send(
                &end,
                frame_type::ROW,
                serde_json::json!({"seq": seq, "device": "dev-b", "batchId": format!("b{seq}")}),
                &bytes,
            )
            .await;
        }
        send(
            &end,
            frame_type::ROWS_DONE,
            serde_json::json!({"headSeq": 3}),
            &[],
        )
        .await;
        end
    });

    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe]),
        sink.clone(),
        fetch,
        "dev-a",
        0,
        ChatTuning::default(),
    )
    .await
    .expect("join succeeds");
    let _keep = server.await.unwrap();

    assert_eq!(client.stats().cursor, 3);
    assert_eq!(
        *lock(&sink.rows),
        vec![
            (vec![0x01], 1),
            (vec![0x03], 1),
            (vec![0x02], 2),
            (vec![0x03], 3),
        ],
        "cursor held through the gap and walked by the repair"
    );
    client.shutdown().await;
}

/// A persisted cursor over a checkpoint-less room gets amnesty to zero on
/// the first join. This heals replicas whose prior cursor advanced while
/// Loro silently parked causal operations.
#[tokio::test(start_paused = true)]
async fn checkpointless_amnesty_refetches_from_zero() {
    let (pipe, mut end) = pipe_pair();
    let sink = Arc::new(RecordingSink::default());
    let (fetch, _) = fetcher(b"");

    let server = tokio::spawn(async move {
        let after = serve_join(
            &mut end,
            serde_json::json!({"headSeq": 3, "seqFloor": 0, "checkpointSeq": 0,
                "checkpointSize": 0, "rowCount": 3, "rowBytes": 96}),
            &[],
            vec![
                (1, "dev-b", vec![0x01]),
                (2, "dev-b", vec![0x02]),
                (3, "dev-b", vec![0x03]),
            ],
            false,
        )
        .await;
        assert_eq!(after, 0, "amnesty must refetch the whole log");
        end
    });

    let client = ChatClient::connect_with_tuned(
        connector(vec![pipe]),
        sink.clone(),
        fetch,
        "dev-a",
        3,
        ChatTuning::default(),
    )
    .await
    .expect("join succeeds");
    let _keep = server.await.unwrap();

    assert_eq!(
        *lock(&sink.rows),
        vec![(vec![0x01], 1), (vec![0x02], 2), (vec![0x03], 3)],
        "all rows re-imported from zero"
    );
    assert_eq!(client.stats().cursor, 3);
    client.shutdown().await;
}

#[tokio::test]
async fn https_nudge_during_ws_failure_retries_after_inflight_sync() {
    let sink = Arc::new(RecordingSink::default());
    let fetcher = Arc::new(FixedFetcher {
        bytes: Vec::new(),
        calls: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    });
    let transport = GatedChatTransport::new(empty_chat_pull(2), false);
    let client = ChatClient::connect_via_transport(
        Arc::new(StaticUrl("ws://127.0.0.1:9/chat2/test/ws".into())),
        sink,
        fetcher,
        "dev-a",
        0,
        transport.clone(),
    )
    .await
    .expect("local-first client starts");

    client.enqueue_update(vec![0x01]);
    transport.first_push_started.notified().await;
    client.enqueue_update(vec![0x02]);
    transport.release_first_push.notify_one();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if transport.pushes.load(Ordering::SeqCst) >= 2 && client.stats().pending_pushes == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second HTTPS push was not scheduled");
    client.shutdown().await;
}

#[tokio::test]
async fn https_timeout_releases_chat_single_flight_for_retry() {
    let sink = Arc::new(RecordingSink::default());
    let fetcher = Arc::new(FixedFetcher {
        bytes: Vec::new(),
        calls: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    });
    let transport = GatedChatTransport::new(empty_chat_pull(0), true);
    let tuning = ChatTuning {
        probe_quiet: Duration::from_secs(60),
        http_timeout: Duration::from_millis(20),
    };
    let client = ChatClient::connect_with_transport(
        Arc::new(WsBinConnector {
            preview: None,
            url: Arc::new(StaticUrl("ws://127.0.0.1:9/chat2/test/ws".into())),
        }),
        sink,
        fetcher,
        "dev-a",
        0,
        tuning,
        Some(transport.clone()),
    )
    .await
    .expect("local-first client starts");

    client.enqueue_update(vec![0x01]);
    transport.first_push_started.notified().await;
    client.enqueue_update(vec![0x02]);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if transport.pushes.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed-out HTTPS sync did not allow a retry");
    client.shutdown().await;
}
