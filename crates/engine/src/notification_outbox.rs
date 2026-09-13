//! Scope-bound notification transport queue. This queue protects handoff after
//! enqueue; it is not yet atomic with the conversation source transaction.
use cypher_proto::Session;
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{future::Future, path::Path, sync::Mutex, time::Duration};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const MAX_EVENT_BYTES: usize = 256 * 1024;

pub struct NotificationOutbox {
    db: Mutex<Connection>,
    wake: Notify,
}

impl NotificationOutbox {
    pub fn open(root: &Path, user: &str, org: &str) -> anyhow::Result<Self> {
        let path = root.join("notification-outbox.sqlite");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            if path.exists() && std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                anyhow::bail!("notification outbox cannot be a symlink");
            }
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .mode(0o600)
                .open(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        let mut db = Connection::open(&path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS notification_outbox(
              id TEXT PRIMARY KEY, session TEXT NOT NULL, created_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS notification_scope(
              singleton INTEGER PRIMARY KEY CHECK(singleton=1), user TEXT NOT NULL, org TEXT NOT NULL);")?;
        let tx = db.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO notification_scope VALUES(1,?,?)",
            [user, org],
        )?;
        let saved: (String, String) = tx.query_row(
            "SELECT user,org FROM notification_scope WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        anyhow::ensure!(
            saved == (user.into(), org.into()),
            "notification scope mismatch"
        );
        tx.commit()?;
        Ok(Self {
            db: Mutex::new(db),
            wake: Notify::new(),
        })
    }
    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub fn enqueue(&self, session: &Session) -> anyhow::Result<()> {
        let body = serde_json::to_string(session)?;
        anyhow::ensure!(
            body.len() <= MAX_EVENT_BYTES,
            "notification event too large"
        );
        let id = format!("{:x}", Sha256::digest(body.as_bytes()));
        self.db().execute(
            "INSERT OR IGNORE INTO notification_outbox(id,session,created_at) VALUES(?,?,?)",
            rusqlite::params![id, body, chrono::Utc::now().timestamp_millis()],
        )?;
        // Commit precedes wake. Notify retains a permit if the consumer has not
        // started waiting yet, closing the empty-queue/enqueue race.
        self.wake.notify_one();
        Ok(())
    }
    fn next(&self) -> anyhow::Result<Option<(String, Session)>> {
        // rowid preserves insertion order even for simultaneous timestamps.
        // Only one bounded body enters memory at a time.
        let row: Option<(String, String)> = self
            .db()
            .query_row(
                "SELECT id,session FROM notification_outbox ORDER BY rowid LIMIT 1",
                [],
                |r| {
                    let body = r.get_ref(1)?.as_str()?;
                    if body.len() > MAX_EVENT_BYTES {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    Ok((r.get(0)?, body.into()))
                },
            )
            .optional()?;
        row.map(|(id, body)| {
            anyhow::ensure!(
                id == format!("{:x}", Sha256::digest(body.as_bytes())),
                "notification event corrupt"
            );
            Ok((id, serde_json::from_str(&body)?))
        })
        .transpose()
    }
    fn remove(&self, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.db()
                .execute("DELETE FROM notification_outbox WHERE id=?", [id])?
                == 1,
            "notification receipt missing"
        );
        Ok(())
    }
}

/// One joined worker owned by an engine runtime, cancelled on auth/runtime
/// replacement. Empty queues sleep on notifications, not HTTP polling.
pub struct Worker {
    stop: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Worker {
    pub fn spawn<F, Fut>(
        queue: std::sync::Arc<NotificationOutbox>,
        retry: Duration,
        send: F,
    ) -> Self
    where
        F: Fn(Session) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send,
    {
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        let task = tokio::spawn(async move {
            loop {
                let step = async {
                    match queue.next() {
                        Ok(Some((id, session))) => {
                            send(session).await?;
                            queue.remove(&id).map_err(|e| e.to_string())
                        }
                        Ok(None) => {
                            queue.wake.notified().await;
                            Ok(())
                        }
                        Err(error) => Err(error.to_string()),
                    }
                };
                let result = tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => break,
                    result = step => result,
                };
                if let Err(error) = result {
                    tracing::warn!(%error, "notification handoff retained for retry");
                    tokio::select! {
                        _ = cancelled.cancelled() => break,
                        _ = tokio::time::sleep(retry) => {}
                    }
                }
            }
        });
        Self {
            stop,
            task: Some(task),
        }
    }
    pub fn cancel(&self) {
        self.stop.cancel();
    }
    pub async fn shutdown(mut self) {
        self.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    fn session(chat: &str) -> Session {
        Session {
            chat_id: chat.into(),
            device_id: "host".into(),
            status: cypher_proto::SessionStatus::Working,
            started_at: None,
            updated_at: chrono::DateTime::UNIX_EPOCH,
            subagents: vec![],
        }
    }
    #[test]
    fn durable_order_dedupe_scope_and_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let q = NotificationOutbox::open(dir.path(), "user", "org").unwrap();
        q.enqueue(&session("first")).unwrap();
        q.enqueue(&session("first")).unwrap();
        q.enqueue(&session("second")).unwrap();
        drop(q);
        assert!(NotificationOutbox::open(dir.path(), "other", "org").is_err());
        let q = NotificationOutbox::open(dir.path(), "user", "org").unwrap();
        let (id, got) = q.next().unwrap().unwrap();
        assert_eq!(got, session("first"));
        q.remove(&id).unwrap();
        let (id, got) = q.next().unwrap().unwrap();
        assert_eq!(got, session("second"));
        q.remove(&id).unwrap();
        assert!(q.next().unwrap().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join("notification-outbox.sqlite"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[tokio::test]
    async fn empty_start_then_enqueue_and_failed_delivery_retry() {
        let dir = tempfile::tempdir().unwrap();
        let q = Arc::new(NotificationOutbox::open(dir.path(), "user", "org").unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let attempts = count.clone();
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let worker = Worker::spawn(q.clone(), Duration::from_millis(10), move |s| {
            let failed = attempts.fetch_add(1, Ordering::SeqCst) == 0;
            let sent = sent.clone();
            async move {
                if failed {
                    return Err("network unavailable".into());
                }
                sent.send(s).unwrap();
                Ok(())
            }
        });
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 0);
        q.enqueue(&session("first")).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), received.recv())
                .await
                .unwrap()
                .unwrap(),
            session("first")
        );
        q.enqueue(&session("second")).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), received.recv())
                .await
                .unwrap()
                .unwrap(),
            session("second")
        );
        worker.shutdown().await;
        assert!(q.next().unwrap().is_none());
    }
    #[tokio::test]
    async fn cancellation_retains_inflight_and_restart_drains_without_another_event() {
        let dir = tempfile::tempdir().unwrap();
        let q = Arc::new(NotificationOutbox::open(dir.path(), "user", "org").unwrap());
        q.enqueue(&session("saved")).unwrap();
        let (started, mut start) = tokio::sync::mpsc::unbounded_channel();
        let worker = Worker::spawn(q.clone(), Duration::from_millis(10), move |_| {
            started.send(()).unwrap();
            std::future::pending::<Result<(), String>>()
        });
        tokio::time::timeout(Duration::from_secs(2), start.recv())
            .await
            .unwrap();
        worker.shutdown().await;
        assert!(q.next().unwrap().is_some());
        drop(q);
        let q = Arc::new(NotificationOutbox::open(dir.path(), "user", "org").unwrap());
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let worker = Worker::spawn(q.clone(), Duration::from_millis(10), move |s| {
            let sent = sent.clone();
            async move {
                sent.send(s).unwrap();
                Ok(())
            }
        });
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), received.recv())
                .await
                .unwrap()
                .unwrap(),
            session("saved")
        );
        worker.shutdown().await;
        assert!(q.next().unwrap().is_none());
    }
    #[tokio::test]
    async fn corrupt_rows_are_not_empty_queues_and_ack_failure_retains_event() {
        let dir = tempfile::tempdir().unwrap();
        let q = Arc::new(NotificationOutbox::open(dir.path(), "user", "org").unwrap());
        q.enqueue(&session("saved")).unwrap();
        q.db().execute_batch("CREATE TRIGGER refuse_delete BEFORE DELETE ON notification_outbox BEGIN SELECT RAISE(ABORT,'fault'); END;").unwrap();
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let worker = Worker::spawn(q.clone(), Duration::from_millis(10), move |_| {
            let sent = sent.clone();
            async move {
                sent.send(()).unwrap();
                Ok(())
            }
        });
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(2), received.recv())
                .await
                .unwrap();
        }
        worker.shutdown().await;
        assert!(q.next().unwrap().is_some());
        q.db()
            .execute("UPDATE notification_outbox SET session='{}'", [])
            .unwrap();
        assert!(q.next().is_err());
    }
}
