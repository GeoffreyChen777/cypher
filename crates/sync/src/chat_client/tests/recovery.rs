use super::*;

#[derive(Default)]
struct CountingTransport {
    pulls: AtomicUsize,
    posts: AtomicUsize,
    rows: Mutex<Vec<(String, Vec<u8>)>>,
}

fn state(head: u64) -> serde_json::Value {
    serde_json::json!({"headSeq": head, "seqFloor": 0, "checkpointSeq": 0,
        "checkpointSize": 0, "rowCount": head, "rowBytes": head})
}

impl ChatTransport for CountingTransport {
    fn fetch_rows(&self, after: u64) -> BoxFuture<'static, Result<Vec<u8>, SyncError>> {
        self.pulls.fetch_add(1, Ordering::SeqCst);
        let rows = lock(&self.rows);
        let mut frames = vec![encode(frame_type::STATE, &state(rows.len() as u64), &[])];
        for (index, (id, bytes)) in rows.iter().enumerate() {
            let seq = index as u64 + 1;
            if seq > after {
                frames.push(encode(
                    frame_type::ROW,
                    &serde_json::json!({"seq": seq, "batchId": id, "device": "dev-a"}),
                    bytes,
                ));
            }
        }
        frames.push(encode(
            frame_type::ROWS_DONE,
            &serde_json::json!({"headSeq": rows.len()}),
            &[],
        ));
        let bytes = framed_pull(frames);
        Box::pin(async move { Ok(bytes) })
    }

    fn push(
        &self,
        batch_id: String,
        bytes: Vec<u8>,
    ) -> BoxFuture<'static, Result<String, SyncError>> {
        self.posts.fetch_add(1, Ordering::SeqCst);
        let mut rows = lock(&self.rows);
        let index = rows
            .iter()
            .position(|(id, _)| id == &batch_id)
            .unwrap_or_else(|| {
                rows.push((batch_id.clone(), bytes));
                rows.len() - 1
            });
        Box::pin(async move {
            Ok(serde_json::json!({"batchId": batch_id, "seq": index + 1}).to_string())
        })
    }
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

struct Room {
    client: ChatClient,
    end: ServerEnd,
    http: Arc<CountingTransport>,
    sink: Arc<RecordingSink>,
}

async fn room() -> Room {
    let (pipe, mut end) = pipe_pair();
    let server = tokio::spawn(async move {
        serve_join(&mut end, state(0), &[], vec![], false).await;
        end
    });
    let sink = Arc::new(RecordingSink::default());
    let http = Arc::new(CountingTransport::default());
    let (fetch, _) = fetcher(b"");
    let client = ChatClient::connect_with_transport(
        connector(vec![pipe]),
        sink.clone(),
        fetch,
        "dev-a",
        0,
        ChatTuning::default(),
        Some(http.clone()),
    )
    .await
    .unwrap();
    let end = server.await.unwrap();
    settle().await;
    assert!(client.stats().connected);
    assert_eq!(
        http.pulls.load(Ordering::SeqCst),
        1,
        "a cold chat pulls beside its first dial"
    );
    Room {
        client,
        end,
        http,
        sink,
    }
}

#[tokio::test(start_paused = true)]
async fn healthy_ws_writes_do_not_post_or_pull_http() {
    let mut room = room().await;
    for seq in 1..=10 {
        room.client.enqueue_update(vec![seq]);
        let push = expect_kind(&mut room.end, frame_type::PUSH).await;
        send(
            &room.end,
            frame_type::ACK,
            serde_json::json!({"batchId": push.header["batchId"], "seq": seq}),
            &[],
        )
        .await;
        settle().await;
    }
    tokio::time::advance(PUSH_ACK_DEADLINE + Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(room.client.stats().pending_pushes, 0);
    assert_eq!(room.client.stats().cursor, 10);
    assert_eq!(room.http.posts.load(Ordering::SeqCst), 0);
    assert_eq!(room.http.pulls.load(Ordering::SeqCst), 1);
    assert_eq!(
        room.client.stats().disconnects,
        0,
        "retired ACK deadlines must not fire"
    );
    room.client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn lost_ack_recovers_same_batch_despite_unrelated_protocol_traffic() {
    let mut room = room().await;
    room.client.enqueue_update(vec![1]);
    let push = expect_kind(&mut room.end, frame_type::PUSH).await;
    let id = push.header["batchId"].as_str().unwrap().to_owned();
    // The room committed the write but its ACK vanished.
    lock(&room.http.rows).push((id.clone(), push.payload));
    for _ in 0..PUSH_ACK_DEADLINE.as_secs() {
        send(&room.end, frame_type::PRESENCE, serde_json::json!({}), &[]).await;
        send(
            &room.end,
            frame_type::PROBE_OK,
            serde_json::json!({"headSeq": 0}),
            &[],
        )
        .await;
        settle().await;
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    settle().await;
    assert!(room.client.stats().disconnects > 0);
    assert_eq!(room.client.stats().pending_pushes, 0);
    assert_eq!(room.http.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        lock(&room.http.rows).len(),
        1,
        "batch identity deduplicates recovery"
    );
    assert_eq!(lock(&room.http.rows)[0].0, id);
    room.client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn foreground_probe_repairs_a_missed_phone_command_without_http() {
    let mut room = room().await;
    // Foreground/resume uses the existing ProbeSync → chat.probe path.
    room.client.probe();
    expect_kind(&mut room.end, frame_type::PROBE).await;
    send(
        &room.end,
        frame_type::PROBE_OK,
        serde_json::json!({"headSeq": 1}),
        &[],
    )
    .await;
    let req = expect_kind(&mut room.end, frame_type::ROWS_REQ).await;
    assert_eq!(req.header["after"], 0);
    assert_eq!(req.header["excludeOwn"], false);
    send(
        &room.end,
        frame_type::ROW,
        serde_json::json!({"seq": 1, "device": "phone", "batchId": "command"}),
        b"phone-command",
    )
    .await;
    send(
        &room.end,
        frame_type::ROWS_DONE,
        serde_json::json!({"headSeq": 1}),
        &[],
    )
    .await;
    settle().await;
    tokio::time::advance(BACKFILL_DEADLINE + Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(room.client.stats().cursor, 1);
    assert_eq!(lock(&room.sink.rows)[0].0, b"phone-command");
    assert_eq!(room.http.pulls.load(Ordering::SeqCst), 1);
    assert_eq!(room.client.stats().disconnects, 0);
    room.client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn presence_does_not_cancel_a_probe_or_a_stalled_row_repair_deadline() {
    for answer_probe in [false, true] {
        let mut room = room().await;
        room.client.probe();
        expect_kind(&mut room.end, frame_type::PROBE).await;
        let deadline = if answer_probe {
            send(
                &room.end,
                frame_type::PROBE_OK,
                serde_json::json!({"headSeq": 1}),
                &[],
            )
            .await;
            expect_kind(&mut room.end, frame_type::ROWS_REQ).await;
            BACKFILL_DEADLINE
        } else {
            PROBE_DEADLINE
        };
        for _ in 0..deadline.as_secs() {
            send(&room.end, frame_type::PRESENCE, serde_json::json!({}), &[]).await;
            settle().await;
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        settle().await;
        assert!(room.client.stats().disconnects > 0);
        assert!(
            room.http.pulls.load(Ordering::SeqCst) > 1,
            "stalled WS must use HTTP recovery"
        );
        room.client.shutdown().await;
    }
}

#[tokio::test(start_paused = true)]
async fn permanent_rejection_does_not_strand_the_next_queued_write() {
    let mut room = room().await;
    room.client.enqueue_update(vec![1]);
    room.client.enqueue_update(vec![2]);
    let bad = expect_kind(&mut room.end, frame_type::PUSH).await;
    send(
        &room.end,
        frame_type::ERROR,
        serde_json::json!({"code":"too_large", "batchId":bad.header["batchId"]}),
        &[],
    )
    .await;
    let good = expect_kind(&mut room.end, frame_type::PUSH).await;
    assert_eq!(good.payload, vec![2]);
    send(
        &room.end,
        frame_type::ACK,
        serde_json::json!({"batchId": good.header["batchId"], "seq": 1}),
        &[],
    )
    .await;
    settle().await;
    assert_eq!(room.client.stats().pending_pushes, 0);
    assert_eq!(room.http.posts.load(Ordering::SeqCst), 0);
    room.client.shutdown().await;
}

#[tokio::test]
async fn real_socket_text_pongs_cannot_hide_a_missing_push_ack() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/chat2/test/ws", listener.local_addr().unwrap());
    let pongs = Arc::new(AtomicUsize::new(0));
    let answered = pongs.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            let response = match message {
                WsMessage::Text(_) => {
                    answered.fetch_add(1, Ordering::SeqCst);
                    Some(WsMessage::Text("pong".into()))
                }
                WsMessage::Binary(bytes) => {
                    let frame = decode(&bytes).unwrap();
                    match frame.kind {
                        frame_type::HELLO => {
                            Some(WsMessage::Binary(encode(frame_type::STATE, &state(0), &[])))
                        }
                        frame_type::ROWS_REQ => Some(WsMessage::Binary(encode(
                            frame_type::ROWS_DONE,
                            &serde_json::json!({"headSeq": 0}),
                            &[],
                        ))),
                        _ => None, // PUSH deliberately gets no ACK
                    }
                }
                _ => None,
            };
            if let Some(response) = response
                && ws.send(response).await.is_err()
            {
                break;
            }
        }
    });
    let http = Arc::new(CountingTransport::default());
    let (fetch, _) = fetcher(b"");
    // Shortened clocks: pings well inside the ACK deadline, so the socket
    // stays visibly alive while the PUSH goes unanswered.
    let push_ack_deadline = Duration::from_secs(1);
    let client = ChatClient::connect_with_transport(
        Arc::new(WsBinConnector {
            url: Arc::new(StaticUrl(url)),
            preview: None,
            ping_interval: Duration::from_millis(200),
        }),
        Arc::new(RecordingSink::default()),
        fetch,
        "dev-a",
        0,
        ChatTuning {
            push_ack_deadline,
            ..ChatTuning::default()
        },
        Some(http.clone()),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !client.stats().connected {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Let bootstrap finish before enqueuing: this exercises WS ACK recovery,
    // not the deliberately retained bootstrap HTTP path.
    tokio::time::sleep(Duration::from_millis(30)).await;
    client.enqueue_update(vec![1]);
    tokio::time::timeout(push_ack_deadline + Duration::from_secs(5), async {
        while client.stats().pending_pushes != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pong-only socket must recover the queued write");
    assert!(pongs.load(Ordering::SeqCst) > 0);
    assert!(client.stats().disconnects > 0);
    assert_eq!(http.posts.load(Ordering::SeqCst), 1);
    client.shutdown().await;
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn presence_only_traffic_cannot_postpone_the_quiet_business_probe() {
    let mut room = room().await;
    for _ in 0..PROBE_QUIET_DEFAULT.as_secs() / 10 {
        send(&room.end, frame_type::PRESENCE, serde_json::json!({}), &[]).await;
        settle().await;
        tokio::time::advance(Duration::from_secs(10)).await;
    }
    settle().await;
    let frame = room
        .end
        .rx
        .try_recv()
        .expect("presence must not reset the business-probe clock");
    assert_eq!(decode(&frame).unwrap().kind, frame_type::PROBE);
    assert_eq!(room.http.pulls.load(Ordering::SeqCst), 1);
    room.client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn incomplete_row_repairs_are_bounded_then_recover_over_http() {
    let mut room = room().await;
    lock(&room.http.rows).push(("phone-command".into(), b"command".to_vec()));
    room.client.probe();
    expect_kind(&mut room.end, frame_type::PROBE).await;
    send(
        &room.end,
        frame_type::PROBE_OK,
        serde_json::json!({"headSeq": 1}),
        &[],
    )
    .await;
    for _ in 0..3 {
        expect_kind(&mut room.end, frame_type::ROWS_REQ).await;
        // Answers keep coming, but none supplies the missing row.
        send(
            &room.end,
            frame_type::ROWS_DONE,
            serde_json::json!({"headSeq": 1}),
            &[],
        )
        .await;
    }
    settle().await;
    assert!(room.client.stats().disconnects > 0);
    assert_eq!(room.client.stats().cursor, 1);
    assert_eq!(lock(&room.sink.rows)[0].0, b"command");
    assert!(room.http.pulls.load(Ordering::SeqCst) > 1);
    room.client.shutdown().await;
}

// ── C1: the HTTPS pull hedges the dial ──────────────────────────────────────

/// A dial that never completes: a WebSocket upgrade that stalls.
struct StalledConnector;

impl BinConnector for StalledConnector {
    fn connect(&self) -> BoxFuture<'static, Result<BinPipe, SyncError>> {
        Box::pin(std::future::pending())
    }
}

/// A chat this device synced before (cursor 3): the hedge applies.
const WARM: u64 = 3;

async fn hedged_client(
    connector: Arc<dyn BinConnector>,
    sink: Arc<RecordingSink>,
) -> (ChatClient, Arc<CountingTransport>) {
    client_at(connector, sink, WARM).await
}

async fn client_at(
    connector: Arc<dyn BinConnector>,
    sink: Arc<RecordingSink>,
    cursor: u64,
) -> (ChatClient, Arc<CountingTransport>) {
    let http = Arc::new(CountingTransport::default());
    let (fetch, _) = fetcher(b"");
    let client = ChatClient::connect_with_transport(
        connector,
        sink,
        fetch,
        "dev-a",
        cursor,
        ChatTuning::default(),
        Some(http.clone()),
    )
    .await
    .unwrap();
    settle().await;
    (client, http)
}

#[tokio::test(start_paused = true)]
async fn a_rejoin_inside_the_hedge_needs_no_https_pull() {
    let (first, mut first_end) = pipe_pair();
    let (second, mut second_end) = pipe_pair();
    let server = tokio::spawn(async move {
        let rows = (1..=WARM)
            .map(|seq| (seq, "dev-b", vec![seq as u8]))
            .collect();
        serve_join(&mut first_end, state(WARM), &[], rows, false).await;
        // The session drops; the client redials after its 250 ms backoff.
        drop(first_end);
        serve_join(&mut second_end, state(WARM), &[], vec![], true).await;
        second_end
    });
    let (client, http) = hedged_client(
        connector(vec![first, second]),
        Arc::new(RecordingSink::default()),
    )
    .await;
    let _end = server.await.unwrap();
    tokio::time::advance(Duration::from_secs(10)).await;
    settle().await;
    assert!(client.stats().connected);
    assert_eq!(client.stats().disconnects, 1);
    assert_eq!(
        http.pulls.load(Ordering::SeqCst),
        0,
        "start and rejoin both joined inside the hedge"
    );
    client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_stalled_dial_pulls_once_at_the_hedge() {
    let (client, http) = hedged_client(
        Arc::new(StalledConnector),
        Arc::new(RecordingSink::default()),
    )
    .await;
    tokio::time::advance(crate::hedge::HEDGE_DELAY - Duration::from_millis(100)).await;
    settle().await;
    assert_eq!(http.pulls.load(Ordering::SeqCst), 0, "not before the hedge");
    tokio::time::advance(Duration::from_millis(200)).await;
    settle().await;
    assert_eq!(http.pulls.load(Ordering::SeqCst), 1, "one pull at 1.5 s");
    // Until the dial's own timeout (unchanged), nothing more.
    tokio::time::advance(CONNECT_TIMEOUT - crate::hedge::HEDGE_DELAY - Duration::from_secs(1))
        .await;
    settle().await;
    assert_eq!(http.pulls.load(Ordering::SeqCst), 1);
    client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_stalled_handshake_pulls_at_the_hedge() {
    // The upgrade succeeds but `state` never comes.
    let (pipe, end) = pipe_pair();
    let (client, http) =
        hedged_client(connector(vec![pipe]), Arc::new(RecordingSink::default())).await;
    tokio::time::advance(crate::hedge::HEDGE_DELAY + Duration::from_millis(100)).await;
    settle().await;
    assert_eq!(http.pulls.load(Ordering::SeqCst), 1);
    drop(end);
    client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_dial_pulls_at_once() {
    // No pipes: the first dial fails straight away.
    let (client, http) = hedged_client(connector(vec![]), Arc::new(RecordingSink::default())).await;
    assert_eq!(http.pulls.load(Ordering::SeqCst), 1);
    client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_waiting_local_write_pulls_at_once() {
    let sink = Arc::new(RecordingSink::default());
    sink.enqueue_outbox("offline-write", b"w").unwrap();
    let (client, http) = hedged_client(Arc::new(StalledConnector), sink).await;
    assert_eq!(http.posts.load(Ordering::SeqCst), 1, "the write goes out");
    assert_eq!(
        http.pulls.load(Ordering::SeqCst),
        1,
        "with its pull, at once"
    );
    tokio::time::advance(crate::hedge::HEDGE_DELAY * 2).await;
    settle().await;
    assert_eq!(
        http.pulls.load(Ordering::SeqCst),
        1,
        "that pull was the connect's hedge"
    );
    client.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_cold_chat_pulls_beside_its_first_dial() {
    // Never synced: nothing local to show, so the first row must not wait
    // for the hedge.
    let (client, http) = client_at(
        Arc::new(StalledConnector),
        Arc::new(RecordingSink::default()),
        0,
    )
    .await;
    assert_eq!(http.pulls.load(Ordering::SeqCst), 1);
    tokio::time::advance(crate::hedge::HEDGE_DELAY * 2).await;
    settle().await;
    assert_eq!(http.pulls.load(Ordering::SeqCst), 1);
    client.shutdown().await;
}
