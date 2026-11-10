//! What the memory service (`crate::memory`, docs/design/memory.md) uses of
//! the bot track: the search index and transcript rendering behind
//! `search_history` and `read_history`, the taint of peer answers, the
//! activity log, and how a hidden run ended.
//!
//! The caller decides which chats a reader may see; everything here works
//! on the chats it is given.

use std::collections::HashMap;

use chrono::Utc;

use cypher_doc::SessionCommandPayload;
use cypher_proto::{ActivityKind, Bot, BotScope, Chat, ProjectGrant};

use super::Bots;
use super::memory::record_activity;
use super::tools::{View, find, render_session};
use crate::EngineError;
use crate::util::off_runtime;

impl Bots {
    /// A view over exactly `chats`, message text included.
    fn history_view(&self, chats: Vec<Chat>) -> Result<View, EngineError> {
        let workspace = &self.inner.workspace;
        let bot = Bot {
            id: String::new(),
            name: String::new(),
            icon: None,
            color: None,
            host_device_id: workspace.device_id().to_string(),
            chat_id: String::new(),
            instructions: None,
            scope: BotScope {
                projects: ProjectGrant::All,
                include_archived: true,
                transcripts: true,
                ..Default::default()
            },
            capabilities: Default::default(),
            paused: false,
            created_at: Utc::now(),
        };
        let sessions = workspace.read_sessions()?;
        Ok(View {
            bot,
            spaces: workspace.read_spaces()?,
            chats,
            sessions: sessions
                .into_iter()
                .map(|s| (s.chat_id.clone(), s))
                .collect::<HashMap<_, _>>(),
            devices: workspace
                .read_devices()?
                .into_iter()
                .map(|d| (d.id, d.name))
                .collect(),
            now: Utc::now(),
        })
    }

    /// `search_history`: `query` over `chats` (newest first), through the
    /// index, best matches first.
    pub(crate) async fn search_history(
        &self,
        chats: Vec<Chat>,
        query: &str,
        limit: usize,
    ) -> Result<String, EngineError> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Err(EngineError::Other(
                "Give search_history some words to find.".into(),
            ));
        }
        let view = self.history_view(chats)?;
        let candidates = view.chats.clone();
        let index = self.inner.index.clone();
        let docs = self.inner.doc_host.clone();
        let find_words = words.clone();
        let found = off_runtime(move || {
            find(
                &candidates,
                &find_words,
                true,
                limit,
                |words, ids| index.search(words, ids),
                |id| docs.read_transcript(id),
            )
        })
        .await
        .map_err(EngineError::Other)?;
        let mut out = vec![if found.hits.is_empty() {
            format!("No past conversations match \"{}\".", words.join(" "))
        } else {
            format!(
                "Past conversations matching \"{}\" (best first; read one with read_history):",
                words.join(" ")
            )
        }];
        for (chat, excerpts) in &found.hits {
            out.push(format!("- {}", view.header(chat)));
            for line in excerpts {
                out.push(format!("  {line}"));
            }
        }
        if found.unstored > 0 {
            out.push(format!(
                "{} conversation(s) have no copy on this device yet and were matched by title only.",
                found.unstored
            ));
        }
        if found.not_searched > 0 {
            out.push(format!(
                "{} older conversation(s) are not indexed yet; only their titles were searched.",
                found.not_searched
            ));
        }
        Ok(out.join("\n"))
    }

    /// `read_history`: one conversation's messages.
    pub(crate) async fn read_history(
        &self,
        chat: Chat,
        from: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, EngineError> {
        let view = self.history_view(vec![chat.clone()])?;
        let entries = self.transcript(&chat).await?;
        Ok(render_session(
            &view,
            &chat,
            &entries,
            from,
            limit,
            "read_history",
        ))
    }

    /// Why a workspace thread is tainted by another workspace's answer.
    pub(crate) fn peer_taint(&self, chat_id: &str) -> Option<String> {
        self.inner.taint.reason(chat_id)
    }

    /// One line in a workspace's Activity about its memory.
    pub(crate) fn note_memory_activity(&self, bot_id: &str, text: &str, chat_id: Option<&str>) {
        record_activity(
            &self.inner.workspace,
            bot_id,
            ActivityKind::NoteWritten,
            text,
            chat_id.filter(|id| !id.is_empty()),
            None,
        );
    }

    /// How the run started in `chat_id` at `run_at` ended: `(failed, its
    /// last answer or why it failed)`; `None` while it runs.
    pub(crate) fn run_outcome(&self, chat_id: &str, run_at: i64) -> Option<(bool, Option<String>)> {
        let commands = self
            .inner
            .doc_host
            .open(chat_id)
            .ok()?
            .doc()
            .read_commands()
            .unwrap_or_default();
        let command = commands.iter().rev().find(|c| {
            c.issued_at >= run_at && matches!(c.payload, SessionCommandPayload::Run { .. })
        });
        let status = self
            .inner
            .doc_host
            .sessions()
            .and_then(|sessions| sessions.session_status(chat_id))
            .map(|session| session.status);
        match super::memory::run_progress(run_at, cypher_proto::now_ms(), command, status) {
            None => None,
            Some((failed, reason)) => {
                let result = self.run_text(chat_id, run_at);
                let failed = failed || result.0.is_none();
                Some(if failed {
                    (
                        true,
                        reason
                            .or(result.1)
                            .or_else(|| Some("The run stopped before it finished.".into())),
                    )
                } else {
                    (false, result.0)
                })
            }
        }
    }
}
