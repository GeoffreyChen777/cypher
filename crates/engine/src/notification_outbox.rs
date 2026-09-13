use cypher_proto::Session;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Durable, idempotent handoff queue for semantic session notification events.
#[derive(Clone)]
pub struct NotificationOutbox {
    path: Arc<PathBuf>,
    draining: Arc<AtomicBool>,
}
impl NotificationOutbox {
    pub fn open(root: &Path) -> Result<Self, rusqlite::Error> {
        let path = root.join("notification-outbox.sqlite");
        let db = rusqlite::Connection::open(&path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS notification_outbox(
              id TEXT PRIMARY KEY, session TEXT NOT NULL, created_at INTEGER NOT NULL);",
        )?;
        Ok(Self {
            path: Arc::new(path),
            draining: Arc::new(AtomicBool::new(false)),
        })
    }
    fn id(session: &Session) -> String {
        format!("{:x}", Sha256::digest(serde_json::to_vec(session).unwrap()))
    }
    pub fn enqueue(&self, session: &Session) -> Result<(), rusqlite::Error> {
        let db = rusqlite::Connection::open(&*self.path)?;
        db.execute(
            "INSERT OR IGNORE INTO notification_outbox(id,session,created_at) VALUES(?,?,?)",
            rusqlite::params![
                Self::id(session),
                serde_json::to_string(session).unwrap(),
                chrono::Utc::now().timestamp_millis()
            ],
        )?;
        Ok(())
    }
    pub fn pending(&self) -> Result<Vec<(String, Session)>, rusqlite::Error> {
        let db = rusqlite::Connection::open(&*self.path)?;
        let mut stmt =
            db.prepare("SELECT id,session FROM notification_outbox ORDER BY created_at,id")?;
        let rows = stmt.query_map([], |row| {
            let text: String = row.get(1)?;
            let session = serde_json::from_str(&text).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    text.len(),
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok((row.get(0)?, session))
        })?;
        rows.collect()
    }
    pub fn remove(&self, id: &str) -> Result<(), rusqlite::Error> {
        let db = rusqlite::Connection::open(&*self.path)?;
        db.execute("DELETE FROM notification_outbox WHERE id=?", [id])?;
        Ok(())
    }
    pub fn begin_drain(&self) -> bool {
        !self.draining.swap(true, Ordering::AcqRel)
    }
    pub fn end_drain(&self) {
        self.draining.store(false, Ordering::Release);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_semantic_events_are_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let q = NotificationOutbox::open(dir.path()).unwrap();
        let s = Session {
            chat_id: "chat".into(),
            device_id: "host".into(),
            status: cypher_proto::SessionStatus::Working,
            started_at: None,
            updated_at: chrono::Utc::now(),
            subagents: vec![],
        };
        q.enqueue(&s).unwrap();
        q.enqueue(&s).unwrap();
        let (id, got) = q.pending().unwrap().pop().unwrap();
        assert_eq!(s, got);
        q.remove(&id).unwrap();
        assert!(q.pending().unwrap().is_empty());
    }
}
