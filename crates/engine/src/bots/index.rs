//! The bot search index: SQLite FTS5 over message text, one file per
//! profile (`<store>/bots-index.sqlite`).
//!
//! It holds the chats some bot hosted on this device may read message text
//! from (the union of their scopes; bot chats never). A background task keeps
//! it current: at startup, after chat or bot row changes (debounced) and on a
//! slow tick, it re-reads only the chats whose activity or stored snapshot
//! moved since they were indexed, through [`DocHost::read_transcript`] (no
//! room joins, no warm docs), a bounded number per pass, and drops chats that
//! left every scope or were deleted. Scope is still checked at query time
//! against the caller's view, so a narrowed scope applies at once.
//!
//! The index is a cache: a corrupt or outdated file is deleted and rebuilt,
//! and with no index at all `search_sessions` falls back to scanning.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cypher_doc::SessionMessageEntry;
use cypher_proto::{Bot, Chat};
use rusqlite::{Connection, ErrorCode, params};
use tokio_util::sync::CancellationToken;

use super::tools::{entry_text, role};
use crate::EngineError;
use crate::host::doc_host::DocHost;
use crate::host::workspace_host::WorkspaceHost;
use crate::util::{lock, off_runtime};

/// Bumped when the schema changes; an older file is rebuilt.
const SCHEMA_VERSION: i64 = 2;
/// Chats re-read per pass; the task runs passes back to back until caught up.
const CHATS_PER_PASS: usize = 40;
/// Message text kept per message.
const MESSAGE_MAX_CHARS: usize = 16_000;
/// Matching messages one query reads before grouping them by chat.
const QUERY_ROWS: usize = 2_000;
/// Wait after a change notification so a burst becomes one pass.
const DEBOUNCE: Duration = Duration::from_millis(1_500);
/// Catches snapshots another device's chats gained without a row change.
const RESCAN: Duration = Duration::from_secs(120);
/// Pause between back-to-back passes.
const PASS_GAP: Duration = Duration::from_millis(200);

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS chats (
        chat_id TEXT PRIMARY KEY,
        activity_ms INTEGER NOT NULL,
        saved_ms INTEGER NOT NULL,
        stored INTEGER NOT NULL
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS entries (
        id INTEGER PRIMARY KEY,
        chat_id TEXT NOT NULL,
        idx INTEGER NOT NULL,
        role TEXT NOT NULL,
        UNIQUE (chat_id, idx)
    );
    CREATE VIRTUAL TABLE IF NOT EXISTS messages
        USING fts5(text, tokenize = 'trigram remove_diacritics 1');
";

/// One matching message.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Hit {
    pub chat_id: String,
    pub index: usize,
    pub role: String,
    pub text: String,
    /// bm25: lower is more relevant.
    pub score: f64,
}

/// A query's answer: matches in relevance order, and which of the asked-for
/// chats the index holds (`false`: indexed, but no copy on this device).
#[derive(Debug, Default)]
pub(super) struct Lookup {
    pub hits: Vec<Hit>,
    pub indexed: HashMap<String, bool>,
}

/// What one pass did.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Pass {
    pub indexed: usize,
    pub removed: usize,
    /// Moved chats left for the next pass.
    pub remaining: usize,
}

pub(super) struct SearchIndex {
    path: PathBuf,
    /// Opened on first use; dropped after a corruption so the next use
    /// rebuilds.
    conn: Mutex<Option<Connection>>,
}

impl SearchIndex {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            conn: Mutex::new(None),
        }
    }

    /// Run `f` on the open index (opening or rebuilding it first). A
    /// corrupt file is deleted, so the next call starts empty.
    fn with_conn<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, EngineError> {
        let mut slot = lock(&self.conn);
        if slot.is_none() {
            *slot = Some(self.open()?);
        }
        let conn = slot.as_mut().expect("opened above");
        f(conn).map_err(|err| {
            if is_corrupt(&err) {
                tracing::warn!(error = %err, "bot search index is corrupt; rebuilding");
                *slot = None;
                remove_files(&self.path);
            }
            EngineError::Other(format!("bot search index: {err}"))
        })
    }

    fn open(&self) -> Result<Connection, EngineError> {
        match open_at(&self.path) {
            Ok(conn) => Ok(conn),
            Err(err) => {
                tracing::warn!(error = %err, "bot search index unreadable; rebuilding");
                remove_files(&self.path);
                open_at(&self.path)
                    .map_err(|err| EngineError::Other(format!("bot search index: {err}")))
            }
        }
    }

    /// One bounded pass over `targets` (newest activity first): drop chats
    /// no longer targeted, then re-read up to `budget` chats whose
    /// watermark (row activity, snapshot save time) moved.
    pub(super) fn sync(
        &self,
        targets: &[Chat],
        saved: &HashMap<String, i64>,
        mut read: impl FnMut(&str) -> Result<Option<Vec<SessionMessageEntry>>, EngineError>,
        budget: usize,
    ) -> Result<Pass, EngineError> {
        if targets.is_empty() && lock(&self.conn).is_none() && !self.path.exists() {
            return Ok(Pass::default());
        }
        let known: HashMap<String, (i64, i64)> = self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT chat_id, activity_ms, saved_ms FROM chats")?;
            stmt.query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?
                .collect()
        })?;
        let wanted: HashSet<&str> = targets.iter().map(|c| c.id.as_str()).collect();
        let gone: Vec<&String> = known
            .keys()
            .filter(|id| !wanted.contains(id.as_str()))
            .collect();
        if !gone.is_empty() {
            self.with_conn(|conn| {
                let tx = conn.transaction()?;
                for id in &gone {
                    forget(&tx, id)?;
                }
                tx.commit()
            })?;
        }
        let watermark =
            |chat: &Chat| (activity_ms(chat), saved.get(&chat.id).copied().unwrap_or(0));
        let moved: Vec<&Chat> = targets
            .iter()
            .filter(|chat| known.get(&chat.id) != Some(&watermark(chat)))
            .collect();
        let mut indexed = 0;
        for chat in moved.iter().take(budget) {
            let entries = match read(&chat.id) {
                Ok(entries) => entries,
                Err(err) => {
                    // Recorded as uncopied; a later save moves the watermark.
                    tracing::debug!(chat = %chat.id, error = %err, "bot index skipped a chat");
                    None
                }
            };
            let (activity, saved) = watermark(chat);
            self.with_conn(|conn| {
                let tx = conn.transaction()?;
                forget(&tx, &chat.id)?;
                for (index, entry) in entries.iter().flatten().enumerate() {
                    let text = entry_text(entry);
                    if text.trim().is_empty() {
                        continue;
                    }
                    let text: String = text.chars().take(MESSAGE_MAX_CHARS).collect();
                    tx.execute(
                        "INSERT INTO entries (chat_id, idx, role) VALUES (?1, ?2, ?3)",
                        params![chat.id, index as i64, role(entry)],
                    )?;
                    tx.execute(
                        "INSERT INTO messages (rowid, text) VALUES (?1, ?2)",
                        params![tx.last_insert_rowid(), text],
                    )?;
                }
                tx.execute(
                    "INSERT INTO chats (chat_id, activity_ms, saved_ms, stored)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![chat.id, activity, saved, entries.is_some()],
                )?;
                tx.commit()
            })?;
            indexed += 1;
        }
        Ok(Pass {
            indexed,
            removed: gone.len(),
            remaining: moved.len() - indexed,
        })
    }

    /// Messages containing every word (anywhere, any case, like the scan),
    /// most relevant first, among `chat_ids` only.
    pub(super) fn search(
        &self,
        words: &[String],
        chat_ids: &[String],
    ) -> Result<Lookup, EngineError> {
        if chat_ids.is_empty() || (lock(&self.conn).is_none() && !self.path.exists()) {
            return Ok(Lookup::default());
        }
        let query = match_query(words);
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            tx.execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS scope (chat_id TEXT PRIMARY KEY) WITHOUT ROWID;
                 DELETE FROM temp.scope;",
            )?;
            {
                let mut insert = tx.prepare("INSERT OR IGNORE INTO temp.scope VALUES (?1)")?;
                for id in chat_ids {
                    insert.execute([id])?;
                }
            }
            let indexed = {
                let mut stmt = tx.prepare(
                    "SELECT c.chat_id, c.stored FROM chats c JOIN temp.scope s USING (chat_id)",
                )?;
                stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<HashMap<String, bool>>>()?
            };
            let hits = match &query {
                None => Vec::new(),
                Some(query) => {
                    let (sql, values) = query.sql();
                    let mut stmt = tx.prepare(&sql)?;
                    stmt.query_map(rusqlite::params_from_iter(values), |row| {
                        Ok(Hit {
                            chat_id: row.get(0)?,
                            index: row.get::<_, i64>(1)? as usize,
                            role: row.get(2)?,
                            text: row.get(3)?,
                            score: row.get(4)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                }
            };
            tx.execute("DELETE FROM temp.scope", [])?;
            tx.commit()?;
            Ok(Lookup { hits, indexed })
        })
    }
}

fn open_at(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let tables: i64 = conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))?;
    if tables > 0 && version != SCHEMA_VERSION {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some(format!("schema version {version}")),
        ));
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(SCHEMA)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(conn)
}

fn forget(tx: &rusqlite::Transaction<'_>, chat_id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM messages WHERE rowid IN (SELECT id FROM entries WHERE chat_id = ?1)",
        [chat_id],
    )?;
    tx.execute("DELETE FROM entries WHERE chat_id = ?1", [chat_id])?;
    tx.execute("DELETE FROM chats WHERE chat_id = ?1", [chat_id])?;
    Ok(())
}

fn is_corrupt(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if matches!(e.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
    )
}

fn remove_files(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(file));
    }
}

fn activity_ms(chat: &Chat) -> i64 {
    chat.last_message_at
        .unwrap_or(chat.created_at)
        .timestamp_millis()
}

/// A search over the trigram index. Words of three or more characters
/// become one FTS5 MATCH of quoted substrings; shorter ones cannot be
/// trigram-matched and filter the rows with `LIKE` instead. Substrings, not
/// words: code is full of identifiers (`BotScope`, `invoice_total`), and the
/// scan this replaced matched inside them too.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct MatchQuery {
    pub fts: Option<String>,
    /// `LIKE` patterns, `%` and `_` escaped with `\`.
    pub short: Vec<String>,
}

impl MatchQuery {
    /// The statement and its parameters, scoped to `temp.scope`.
    fn sql(&self) -> (String, Vec<String>) {
        let mut sql = String::from("SELECT e.chat_id, e.idx, e.role, messages.text, ");
        let mut values = Vec::new();
        sql.push_str(match self.fts {
            Some(_) => "messages.rank",
            None => "0.0",
        });
        sql.push_str(
            " FROM messages JOIN entries e ON e.id = messages.rowid
              WHERE e.chat_id IN (SELECT chat_id FROM temp.scope)",
        );
        if let Some(fts) = &self.fts {
            values.push(fts.clone());
            sql.push_str(&format!(" AND messages MATCH ?{}", values.len()));
        }
        for pattern in &self.short {
            values.push(pattern.clone());
            sql.push_str(&format!(
                " AND messages.text LIKE ?{} ESCAPE '\\'",
                values.len()
            ));
        }
        sql.push_str(match self.fts {
            Some(_) => " ORDER BY messages.rank",
            None => " ORDER BY e.id DESC",
        });
        sql.push_str(&format!(" LIMIT {QUERY_ROWS}"));
        (sql, values)
    }
}

/// The query for `words`. Words with nothing to match on (bare punctuation)
/// are dropped; `None` when none remain.
pub(super) fn match_query(words: &[String]) -> Option<MatchQuery> {
    let words: Vec<&String> = words
        .iter()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .collect();
    let long: Vec<String> = words
        .iter()
        .filter(|w| w.chars().count() >= 3)
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect();
    let short: Vec<String> = words
        .iter()
        .filter(|w| w.chars().count() < 3)
        .map(|w| {
            let escaped = w
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("%{escaped}%")
        })
        .collect();
    (!long.is_empty() || !short.is_empty()).then(|| MatchQuery {
        fts: (!long.is_empty()).then(|| long.join(" ")),
        short,
    })
}

/// The chats worth indexing: what the bots hosted on `device_id` that may
/// read message text can see, newest activity first.
pub(super) fn targets(device_id: &str, bots: &[Bot], chats: &[Chat]) -> Vec<Chat> {
    let mut seen = HashSet::new();
    let mut out: Vec<Chat> = Vec::new();
    for bot in bots
        .iter()
        .filter(|b| b.host_device_id == device_id && b.scope.transcripts)
    {
        for chat in bot.scope.visible_chats(chats) {
            if seen.insert(chat.id.as_str()) {
                out.push(chat.clone());
            }
        }
    }
    out.sort_by_key(|c| std::cmp::Reverse(activity_ms(c)));
    out
}

// ── the background task ─────────────────────────────────────────────────────

/// Keep `index` current until `cancel` fires.
pub(super) async fn run(
    index: Arc<SearchIndex>,
    workspace: WorkspaceHost,
    docs: DocHost,
    cancel: CancellationToken,
) {
    let mut chats = workspace.watch_chats();
    let mut bots = workspace.watch_bots();
    let mut tick = tokio::time::interval(RESCAN);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tick.tick() => {}
            changed = chats.changed() => if changed.is_err() { return },
            changed = bots.changed() => if changed.is_err() { return },
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(DEBOUNCE) => {}
        }
        chats.mark_unchanged();
        bots.mark_unchanged();
        loop {
            match pass(&index, &workspace, &docs).await {
                Ok(pass) => {
                    if pass.indexed > 0 || pass.removed > 0 {
                        tracing::debug!(?pass, "bot search index pass");
                    }
                    if pass.remaining == 0 {
                        break;
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "bot search index pass failed");
                    break;
                }
            }
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(PASS_GAP) => {}
            }
        }
    }
}

/// One pass, off the runtime.
pub(super) async fn pass(
    index: &Arc<SearchIndex>,
    workspace: &WorkspaceHost,
    docs: &DocHost,
) -> Result<Pass, EngineError> {
    let index = index.clone();
    let workspace = workspace.clone();
    let docs = docs.clone();
    off_runtime(move || {
        let targets = targets(
            workspace.device_id(),
            &workspace.read_bots()?,
            &workspace.read_chats()?,
        );
        // Nothing to index (no reading bots here): only removals remain.
        let saved = if targets.is_empty() {
            HashMap::new()
        } else {
            docs.snapshot_saved_times()?
        };
        index.sync(
            &targets,
            &saved,
            |id| docs.read_transcript(id),
            CHATS_PER_PASS,
        )
    })
    .await
    .map_err(EngineError::Other)?
}
