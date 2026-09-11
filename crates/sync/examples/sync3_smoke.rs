//! Local workerd end-to-end: independent SQLite journals, actual WebSockets,
//! typed events, restart, and zero HTTP repair on a healthy stream.
//! Run wrangler dev --local -c wrangler.sync3test.jsonc before this example.
use cypher_proto::sync3::{Operation, Projection, Reply, Request};
use cypher_sync::{
    StaticUrl,
    sync3::{
        Error, Journal, Phase,
        transport::{Client, RepairTransport, Tuning},
    },
};
use futures::future::BoxFuture;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

struct HttpRepair {
    url: String,
    requests: Arc<AtomicUsize>,
}
impl RepairTransport for HttpRepair {
    fn exchange(&self, request: Request) -> BoxFuture<'static, Result<Reply, Error>> {
        let url = self.url.clone();
        self.requests.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let response = reqwest::Client::new()
                .post(url)
                .bearer_auth("sync3-user@sync3-org")
                .json(&request)
                .send()
                .await
                .map_err(|_| Error::Protocol("transport_unavailable".into()))?;
            let body = response
                .bytes()
                .await
                .map_err(|_| Error::Protocol("transport_unavailable".into()))?;
            if body.len() > cypher_proto::sync3::MAX_FRAME_BYTES {
                return Err(Error::Protocol("frame_too_large".into()));
            }
            Ok(serde_json::from_slice(&body)?)
        })
    }
}
async fn wait_cursor(client: &Client, cursor: u64) {
    let mut status = client.watch();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let s = status.borrow_and_update().clone();
            if s.cursor >= cursor && s.phase == Phase::Live {
                break;
            }
            assert!(s.error.is_none(), "unexpected client error: {:?}", s);
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("sync convergence timeout");
}
#[tokio::main]
async fn main() {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:27643".into());
    let parsed = reqwest::Url::parse(&base).unwrap();
    assert_eq!(parsed.scheme(), "http");
    assert!(
        matches!(parsed.host_str(), Some("127.0.0.1") | Some("localhost")),
        "local-only test"
    );
    let room = std::env::args()
        .nth(2)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    assert!(cypher_proto::sync3::valid_id(&room));
    let path = format!("{base}/sync3/sync3-org/chats/{room}/");
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
    let head = fixture["operations"].as_array().unwrap().len() as u64;
    if matches!(
        std::env::args().nth(3).as_deref(),
        Some("--write-journal" | "--verify-journal")
    ) {
        let file = std::path::PathBuf::from(std::env::args().nth(4).expect("test database path"));
        let writing = std::env::args().nth(3).as_deref() == Some("--write-journal");
        assert_eq!(
            file.exists(),
            !writing,
            "never overwrite/reseed a test database"
        );
        let numbers: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/sync3/numbers.json")).unwrap();
        let mut journal = Journal::open(&file, "account", "shared-room", "phone").unwrap();
        if writing {
            journal
                .accept_state(&Reply::State {
                    version: 3,
                    epoch: 1,
                    owner: "host".into(),
                    owner_epoch: 1,
                    head,
                })
                .unwrap();
            let operations: Vec<Operation> =
                serde_json::from_value(fixture["operations"].clone()).unwrap();
            let rows = operations
                .into_iter()
                .enumerate()
                .map(|(i, operation)| cypher_proto::sync3::Row {
                    seq: i as u64 + 1,
                    operation,
                })
                .collect();
            journal
                .apply_page(&Reply::Page {
                    version: 3,
                    epoch: 1,
                    through: head,
                    next: head,
                    done: true,
                    rows,
                })
                .unwrap();
            journal
                .enqueue(&serde_json::from_value(numbers["source"].clone()).unwrap())
                .unwrap();
            println!("PASS: Rust wrote a private normalized SQLite journal for Swift");
        } else {
            assert_eq!(journal.cursor().unwrap(), head + 2);
            let window = journal.message_window(None, 32).unwrap();
            assert_eq!(window.through, head + 2);
            assert_eq!(window.messages.len(), 1);
            assert_eq!(window.messages[0].created_seq, 4);
            assert_eq!(
                journal.projection().unwrap().commands["command"]
                    .command
                    .status,
                cypher_proto::SessionCommandStatus::Applied
            );
            // Re-enqueue the original Rust body after Swift has written the
            // receipt: JSON key ordering must not create a phantom conflict.
            journal
                .enqueue(&serde_json::from_value(numbers["source"].clone()).unwrap())
                .unwrap();
            assert!(journal.pending().unwrap().is_empty());
            assert_eq!(
                serde_json::to_value(
                    &journal.projection().unwrap().commands["numeric-command"].command
                )
                .unwrap(),
                numbers["canonical"]["event"]["command"]
            );
            println!("PASS: Rust reopened Swift's updates to the same SQLite file");
        }
        return;
    }
    if std::env::args().nth(3).as_deref() == Some("--verify-swift") {
        let dir = tempfile::tempdir().unwrap();
        let url = Arc::new(StaticUrl(format!(
            "{}ws?token=sync3-user%40sync3-org",
            path.replacen("http:", "ws:", 1)
        )));
        let reader = Client::spawn(
            Journal::open(
                &dir.path().join("reader.sqlite"),
                "account",
                &path,
                "rust-reader",
            )
            .unwrap(),
            url,
            None,
            Tuning::default(),
        );
        wait_cursor(&reader, head + 1).await;
        let projection = reader.journal().lock().unwrap().projection().unwrap();
        assert_eq!(
            serde_json::to_value(&projection).unwrap()["messages"]["message"]["entry"]["parts"][0]
                ["text"],
            "你好!"
        );
        assert_eq!(projection.commands["swift-command"].actor, "swift-reader");
        reader.shutdown().await;
        println!("PASS: Rust reads the command committed by Swift through real workerd");
        return;
    }
    let response = reqwest::Client::new()
        .post(format!("{path}init"))
        .bearer_auth("sync3-user@sync3-org")
        .json(&serde_json::json!({"owner":"host"}))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "initialization failed: {}",
        response.status()
    );
    let ops: Vec<Operation> = serde_json::from_value(fixture["operations"].clone()).unwrap();
    let expected: Projection = serde_json::from_value(fixture["projection"].clone()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let repair: Arc<dyn RepairTransport> = Arc::new(HttpRepair {
        url: format!("{path}exchange"),
        requests: count.clone(),
    });
    let url = Arc::new(StaticUrl(format!(
        "{}ws?token=sync3-user%40sync3-org",
        path.replacen("http:", "ws:", 1)
    )));
    let host = Client::spawn(
        Journal::open(&dir.path().join("host.sqlite"), "account", &path, "host").unwrap(),
        url.clone(),
        Some(repair.clone()),
        Tuning::default(),
    );
    let phone_path = dir.path().join("phone.sqlite");
    let phone = Client::spawn(
        Journal::open(&phone_path, "account", &path, "phone").unwrap(),
        url.clone(),
        Some(repair.clone()),
        Tuning::default(),
    );
    wait_cursor(&host, 0).await;
    wait_cursor(&phone, 0).await;
    phone.enqueue(&ops[0]).unwrap();
    wait_cursor(&host, 1).await;
    for op in ops.iter().skip(1) {
        host.enqueue(op).unwrap();
    }
    wait_cursor(&phone, head).await;
    wait_cursor(&host, head).await;
    assert_eq!(
        phone.journal().lock().unwrap().projection().unwrap(),
        expected
    );
    assert_eq!(
        host.journal().lock().unwrap().projection().unwrap(),
        expected
    );
    phone.shutdown().await;
    let restarted = Client::spawn(
        Journal::open(&phone_path, "account", &path, "phone").unwrap(),
        url,
        Some(repair),
        Tuning::default(),
    );
    wait_cursor(&restarted, head).await;
    assert!(
        restarted
            .journal()
            .lock()
            .unwrap()
            .pending()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        restarted.journal().lock().unwrap().projection().unwrap(),
        expected
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "healthy sync must never use HTTP fallback"
    );
    restarted.shutdown().await;
    host.shutdown().await;
    println!(
        "PASS: workerd ↔ Rust host/phone; {head} typed events; UTF-8; restart; healthy HTTP repairs=0"
    );
}
