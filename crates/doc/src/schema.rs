//! Session doc schema over `loro`.
//!
//! Container layout (MUST stay shape-compatible with the TS edge/tail materializer):
//! - `meta`:     LoroMap  { chatId: string, schemaVersion: number }         (host-only writer)
//! - `messages`: LoroList of LoroMap {
//!   id, role, parts: LoroList<part map>, createdAt, deviceId, status?, continuationOf?,
//!   completedAt?, comments?: json, models?: json }
//! - `commands`: LoroList of LoroMap {
//!   id, kind, payload(json), issuedBy, issuedAt, basedOn?, expiresAt?, status, resolution? }
//!
//! Part maps: { id, kind: "text"|"tool"|"input"|"error", text?: LoroText, call?: json,
//! isError?, questions?: json, resolved?, message? }. Text bodies are **LoroText** so streaming
//! appends RLE-merge (1.03x oplog overhead vs 125x for whole-value rewrites).

use loro::{ExportMode, LoroDoc, LoroError, LoroList, LoroMap, LoroText, LoroValue, ToJson};
use serde::{Deserialize, Serialize};

use cypher_proto::AnsweredModel;

use crate::commands::{SessionCommandEntry, SessionCommandStatus};
use crate::constants::SESSION_SCHEMA_VERSION;
use crate::parts::{MessagePart, MessageStatus};

mod salvage;
mod segment_writer;

use salvage::*;
pub use segment_writer::*;

#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("loro: {0}")]
    Loro(#[from] LoroError),
    #[error("schema: {0}")]
    Schema(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// One entry in the doc's `messages` list (`SessionMessageEntry` in TS).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageEntry {
    pub id: String,
    pub role: MessageRole,
    pub parts: Vec<MessagePart>,
    /// Epoch millis.
    pub created_at: i64,
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<MessageStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_of: Option<String>,
    /// Epoch millis the segment reached a terminal status — stamped by
    /// [`SegmentWriter::finish`] (additive: absent on old rows, old writers,
    /// and every entry that is still streaming). With `created_at` this is
    /// the only durable record of how long a settled turn took, so the
    /// transcript can label it after a reload or on another device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    /// The comments that rode a user prompt (the Comment feature) — the
    /// agent received them inside its effective prompt; the transcript shows
    /// them beside the visible one. Additive: absent on old rows, old
    /// writers, and every prompt sent without comments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<MessageComment>,
    /// The models that answered an assistant segment, distinct and in the
    /// order they first answered (a turn is one or more model calls). Only
    /// harnesses that report it (pi) write it, as each call completes.
    /// Additive: absent on old rows, old writers, and other harnesses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<AnsweredModel>,
}

/// One comment sent with a user prompt: the quote as the user selected it
/// and what they wrote about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageComment {
    pub quote: String,
    pub comment: String,
}

impl MessageComment {
    /// The comments a wrapped effective prompt carries
    /// ([`cypher_proto::agent_prompt::parse_comments`]).
    pub fn from_agent_prompt(prompt: &str) -> Vec<Self> {
        cypher_proto::agent_prompt::parse_comments(prompt)
            .into_iter()
            .map(|comment| Self {
                quote: comment.displayed_quote().to_owned(),
                comment: comment.comment,
            })
            .collect()
    }
}

/// The doc-resident flat part map (`DocMessagePart` in TS). Distinct from the app-layer
/// [`MessagePart`]: input parts key on their request id, error parts store `message`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocPartJson {
    id: String,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// The agent's version of a translated text part (additive — absent on
    /// old rows, old writers and untranslated text). A plain string, not a
    /// LoroText: it is written whole, never streamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_text: Option<String>,
    /// A reasoning part's body (additive). Its own key, not `text`: readers
    /// older than the kind fall back to a text part built from `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    call: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    questions: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    /// Tool output summary (additive — absent on old rows and old writers;
    /// pre-strip writers stored up to 4KB of capped output here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    /// Transient live-progress tail for an UNRESOLVED tool (additive; the
    /// fold clears it on resolve, so it never survives a settled chip).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress: Option<String>,
    /// Capped inline tool diff (additive; pre-strip writers only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diff: Option<serde_json::Value>,
    /// Sidecar key of the full output (additive, docs/chat2-sync.md A1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_ref: Option<String>,
    /// Full-output byte length (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_bytes: Option<u64>,
    /// Sidecar key of the full diff JSON (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diff_ref: Option<String>,
    /// Per-file diff stats (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diff_stats: Option<serde_json::Value>,
}

/// App parts → doc part json (mirror of `toDocParts`).
fn to_doc_part(part: &MessagePart) -> Result<DocPartJson, DocError> {
    Ok(match part {
        MessagePart::Text {
            id,
            text,
            agent_text,
        } => DocPartJson {
            id: id.clone(),
            kind: "text".into(),
            text: Some(text.clone()),
            agent_text: agent_text.clone(),
            ..Default::default()
        },
        MessagePart::Tool {
            id,
            call,
            is_error,
            resolved,
            output,
            progress,
            diff,
            output_ref,
            output_bytes,
            diff_ref,
            diff_stats,
        } => DocPartJson {
            id: id.clone(),
            kind: "tool".into(),
            call: Some(serde_json::to_value(call)?),
            // TS shape parity: `isError` is written only once the tool result arrived;
            // its presence IS the resolution marker.
            is_error: if *resolved { Some(*is_error) } else { None },
            output: output.clone(),
            progress: progress.clone(),
            diff: diff.as_ref().map(serde_json::to_value).transpose()?,
            output_ref: output_ref.clone(),
            output_bytes: *output_bytes,
            diff_ref: diff_ref.clone(),
            diff_stats: diff_stats.as_ref().map(serde_json::to_value).transpose()?,
            ..Default::default()
        },
        MessagePart::Input {
            id: _,
            request_id,
            questions,
            resolved,
        } => DocPartJson {
            id: request_id.clone(),
            kind: "input".into(),
            questions: Some(serde_json::to_value(questions)?),
            resolved: Some(*resolved),
            ..Default::default()
        },
        MessagePart::Error { id, message } => DocPartJson {
            id: id.clone(),
            kind: "error".into(),
            message: Some(message.clone()),
            ..Default::default()
        },
        MessagePart::Reasoning { id, text } => DocPartJson {
            id: id.clone(),
            kind: "reasoning".into(),
            reasoning: Some(text.clone()),
            ..Default::default()
        },
    })
}

/// Doc part json → app part (mirror of `fromDocParts`; malformed degrades to empty text).
fn from_doc_part(p: DocPartJson) -> MessagePart {
    match p.kind.as_str() {
        "tool" => match p.call.and_then(|c| serde_json::from_value(c).ok()) {
            Some(call) => MessagePart::Tool {
                id: p.id,
                call,
                is_error: p.is_error.unwrap_or(false),
                resolved: p.is_error.is_some(),
                output: p.output,
                progress: p.progress,
                diff: p.diff.and_then(|d| serde_json::from_value(d).ok()),
                output_ref: p.output_ref,
                output_bytes: p.output_bytes,
                diff_ref: p.diff_ref,
                diff_stats: p.diff_stats.and_then(|s| serde_json::from_value(s).ok()),
            },
            None => MessagePart::Text {
                id: p.id,
                text: String::new(),
                agent_text: None,
            },
        },
        "input" => MessagePart::Input {
            id: p.id.clone(),
            request_id: p.id,
            questions: p
                .questions
                .and_then(|q| serde_json::from_value(q).ok())
                .unwrap_or_default(),
            resolved: p.resolved.unwrap_or(false),
        },
        "error" => MessagePart::Error {
            id: p.id,
            message: p.message.unwrap_or_default(),
        },
        "reasoning" => MessagePart::Reasoning {
            id: p.id,
            text: p.reasoning.unwrap_or_default(),
        },
        _ => MessagePart::Text {
            id: p.id,
            text: p.text.unwrap_or_default(),
            agent_text: p.agent_text,
        },
    }
}

/// A session doc handle: typed access over a LoroDoc with the schema above.
pub struct SessionDoc {
    doc: LoroDoc,
    preview_hook: std::sync::RwLock<Option<crate::PreviewCommitHook>>,
}

impl SessionDoc {
    /// Wrap an existing doc (e.g. imported from a snapshot).
    pub fn from_doc(doc: LoroDoc) -> Self {
        Self {
            doc,
            preview_hook: Default::default(),
        }
    }

    /// Create + initialize a fresh doc for `chat_id` (host-only).
    pub fn init(chat_id: &str) -> Result<Self, DocError> {
        let doc = LoroDoc::new();
        let meta = doc.get_map("meta");
        meta.insert("chatId", chat_id)?;
        meta.insert("schemaVersion", SESSION_SCHEMA_VERSION as i64)?;
        doc.commit();
        Ok(Self::from_doc(doc))
    }

    pub fn doc(&self) -> &LoroDoc {
        &self.doc
    }

    pub fn set_preview_hook(&self, hook: crate::PreviewCommitHook) {
        *self.preview_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    pub fn preview_coverage(&self) -> Option<crate::PreviewCoverage> {
        let value = self.doc.get_map("meta").get("previewCoverage")?;
        let loro::ValueOrContainer::Value(LoroValue::String(json)) = value else {
            return None;
        };
        serde_json::from_str(json.as_str()).ok()
    }

    /// Stage metadata in the same transaction as the writer's text/status.
    /// No commit here: callers must not acknowledge a marker without its text.
    pub(crate) fn stage_preview_coverage(
        &self,
        coverage: &crate::PreviewCoverage,
    ) -> Result<bool, DocError> {
        if self.preview_coverage().as_ref() == Some(coverage) {
            return Ok(false);
        }
        self.doc
            .get_map("meta")
            .insert("previewCoverage", serde_json::to_string(coverage)?)?;
        Ok(true)
    }

    fn preview_commit(
        &self,
        entry: &str,
        parts: &[MessagePart],
        complete: bool,
    ) -> Result<bool, DocError> {
        let hook = self
            .preview_hook
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(coverage) = hook.and_then(|h| h(entry, parts, complete)) else {
            return Ok(false);
        };
        self.stage_preview_coverage(&coverage)
    }

    pub fn chat_id(&self) -> Option<String> {
        match self.doc.get_map("meta").get("chatId") {
            Some(loro::ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
            _ => None,
        }
    }

    /// Insert a complete message entry (user/system messages, command-side inserts).
    /// Streaming assistant entries go through [`SegmentWriter`].
    pub fn push_message(&self, entry: &SessionMessageEntry) -> Result<(), DocError> {
        let messages = self.doc.get_list("messages");
        let map = messages.push_container(LoroMap::new())?;
        write_entry_scalar_fields(&map, entry)?;
        let parts = map.insert_container("parts", LoroList::new())?;
        for part in &entry.parts {
            push_part(&parts, part)?;
        }
        self.doc.commit();
        Ok(())
    }

    /// Read all entries (continuations NOT joined — see `join_continuation_entries`).
    ///
    /// Malformed entries are SKIPPED, not fatal: a torn intermediate state
    /// (an entry map imported before the update that fills its fields) or a
    /// peer on a newer schema must degrade to a missing row, never blank the
    /// whole transcript.
    pub fn read_entries(&self) -> Result<Vec<SessionMessageEntry>, DocError> {
        // Materialize only the messages container — a whole-doc deep value
        // here also serialized the commands ledger on every 120ms commit tick.
        let messages = self
            .doc
            .get_list("messages")
            .get_deep_value()
            .to_json_value();
        let raw: Vec<serde_json::Value> = serde_json::from_value(messages)?;
        Ok(raw
            .into_iter()
            .filter_map(|v| match entry_from_json(v) {
                Ok(entry) => Some(entry),
                Err(err) => {
                    tracing::warn!(
                        chat = %self.chat_id().unwrap_or_default(),
                        error = %err,
                        "skipping unsalvageable transcript entry"
                    );
                    None
                }
            })
            .collect())
    }

    /// Read the commands ledger.
    ///
    /// Same skip-not-fail policy as `read_entries`: any device can append
    /// here, and one malformed entry must not wedge command draining for the
    /// chat forever (an unparseable command can't be executed anyway).
    pub fn read_commands(&self) -> Result<Vec<SessionCommandEntry>, DocError> {
        // Container-scoped for the same reason as `read_entries`: the drain
        // loop runs this per tick and must not pay for the transcript.
        let commands = self
            .doc
            .get_list("commands")
            .get_deep_value()
            .to_json_value();
        let raw: Vec<serde_json::Value> = serde_json::from_value(commands)?;
        Ok(raw
            .into_iter()
            .filter_map(|v| match serde_json::from_value(v) {
                Ok(entry) => Some(entry),
                Err(err) => {
                    tracing::warn!(error = %err, "skipping malformed command entry");
                    None
                }
            })
            .collect())
    }

    /// Append a command entry (rule 1: own entries only, append-only).
    pub fn queue_command(&self, entry: &SessionCommandEntry) -> Result<(), DocError> {
        let commands = self.doc.get_list("commands");
        let map = commands.push_container(LoroMap::new())?;
        map.insert("id", entry.id.as_str())?;
        map.insert(
            "kind",
            serde_json::to_value(entry.kind())?
                .as_str()
                .ok_or_else(|| DocError::Schema("kind not a string".into()))?,
        )?;
        map.insert(
            "payload",
            loro_value_from_json(&serde_json::to_value(&entry.payload)?),
        )?;
        map.insert("issuedBy", entry.issued_by.as_str())?;
        map.insert("issuedAt", entry.issued_at)?;
        if let Some(based_on) = &entry.based_on {
            map.insert(
                "basedOn",
                loro_value_from_json(&serde_json::to_value(based_on)?),
            )?;
        }
        if let Some(expires_at) = entry.expires_at {
            map.insert("expiresAt", expires_at)?;
        }
        if let Some(sent_at) = entry.sent_at {
            map.insert("sentAt", sent_at)?;
        }
        map.insert(
            "status",
            serde_json::to_value(entry.status)?
                .as_str()
                .ok_or_else(|| DocError::Schema("status not a string".into()))?,
        )?;
        self.doc.commit();
        Ok(())
    }

    /// Rule 2: host (or the issuing composer, for `cancelled`) writes an outcome.
    pub fn set_command_status(
        &self,
        command_id: &str,
        status: SessionCommandStatus,
        resolution: Option<&str>,
    ) -> Result<(), DocError> {
        let commands = self.doc.get_list("commands");
        for i in 0..commands.len() {
            if let Some(loro::ValueOrContainer::Container(loro::Container::Map(map))) =
                commands.get(i)
            {
                let id_matches = matches!(
                    map.get("id"),
                    Some(loro::ValueOrContainer::Value(LoroValue::String(s))) if s.as_str() == command_id
                );
                if id_matches {
                    map.insert(
                        "status",
                        serde_json::to_value(status)?
                            .as_str()
                            .ok_or_else(|| DocError::Schema("status not a string".into()))?,
                    )?;
                    if let Some(r) = resolution {
                        map.insert("resolution", r)?;
                    }
                    self.doc.commit();
                    return Ok(());
                }
            }
        }
        Err(DocError::Schema(format!("command {command_id} not found")))
    }

    /// Remove message entries by id (Session Rewind: the transcript is
    /// truncated at a settled anchor). Deletion walks the list BACKWARD so
    /// earlier indices stay valid, and lands in ONE commit so watchers see a
    /// single truncated transcript rather than a shrinking sequence of them.
    /// Entries whose map carries no readable `id` are left alone (the same
    /// skip-not-fail policy as [`Self::read_entries`] — a torn or
    /// newer-schema entry is never collateral). Returns how many were
    /// removed; an empty id set is a no-op with no commit.
    pub fn remove_messages(
        &self,
        ids: &std::collections::HashSet<String>,
    ) -> Result<usize, DocError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let messages = self.doc.get_list("messages");
        let mut removed = 0usize;
        for i in (0..messages.len()).rev() {
            let Some(loro::ValueOrContainer::Container(loro::Container::Map(map))) =
                messages.get(i)
            else {
                continue;
            };
            let Some(loro::ValueOrContainer::Value(LoroValue::String(id))) = map.get("id") else {
                continue;
            };
            if ids.contains(id.as_str()) {
                messages.delete(i, 1)?;
                removed += 1;
            }
        }
        if removed > 0 {
            self.doc.commit();
        }
        Ok(removed)
    }

    /// Stamp a terminal status on an existing message entry by id (recovery:
    /// abandoned `streaming` entries from a dead run are stamped `aborted`).
    /// Returns `false` when no entry with that id exists.
    pub fn set_message_status(
        &self,
        message_id: &str,
        status: MessageStatus,
    ) -> Result<bool, DocError> {
        let messages = self.doc.get_list("messages");
        for i in 0..messages.len() {
            if let Some(loro::ValueOrContainer::Container(loro::Container::Map(map))) =
                messages.get(i)
            {
                let id_matches = matches!(
                    map.get("id"),
                    Some(loro::ValueOrContainer::Value(LoroValue::String(s))) if s.as_str() == message_id
                );
                if id_matches {
                    map.insert("status", status_str(status))?;
                    // Crash recovery closes a streaming entry here rather
                    // than through the writer: stamp the same completion
                    // instant so the settled turn still carries a span.
                    if status != MessageStatus::Streaming && map.get("completedAt").is_none() {
                        map.insert("completedAt", chrono::Utc::now().timestamp_millis())?;
                    }
                    self.doc.commit();
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Append an error part to an existing entry (crash recovery: the aborted
    /// entry must SAY why it ended — "Run interrupted by engine restart…" —
    /// not just truncate silently). Returns `false` when no entry matches.
    pub fn append_error_part(
        &self,
        message_id: &str,
        part_id: &str,
        message: &str,
    ) -> Result<bool, DocError> {
        let messages = self.doc.get_list("messages");
        for i in 0..messages.len() {
            let Some(loro::ValueOrContainer::Container(loro::Container::Map(entry))) =
                messages.get(i)
            else {
                continue;
            };
            let id_matches = matches!(
                entry.get("id"),
                Some(loro::ValueOrContainer::Value(LoroValue::String(s))) if s.as_str() == message_id
            );
            if !id_matches {
                continue;
            }
            let Some(loro::ValueOrContainer::Container(loro::Container::List(parts))) =
                entry.get("parts")
            else {
                continue;
            };
            // Idempotent per part id (recovery may re-run on a crash loop).
            for j in 0..parts.len() {
                if let Some(loro::ValueOrContainer::Container(loro::Container::Map(part))) =
                    parts.get(j)
                    && matches!(
                        part.get("id"),
                        Some(loro::ValueOrContainer::Value(LoroValue::String(s))) if s.as_str() == part_id
                    )
                {
                    return Ok(true);
                }
            }
            push_part(
                &parts,
                &MessagePart::Error {
                    id: part_id.to_string(),
                    message: message.to_string(),
                },
            )?;
            self.doc.commit();
            return Ok(true);
        }
        Ok(false)
    }

    /// Mark the input part carrying `request_id` resolved, wherever it lives
    /// (input parts store the request id as their part id). The live-run path
    /// resolves through the entry fold; this direct write is for answers to a
    /// question whose run already died — no fold owns the entry anymore.
    /// Returns `false` when no such part exists.
    pub fn resolve_input(&self, request_id: &str) -> Result<bool, DocError> {
        let messages = self.doc.get_list("messages");
        for i in 0..messages.len() {
            let Some(loro::ValueOrContainer::Container(loro::Container::Map(entry))) =
                messages.get(i)
            else {
                continue;
            };
            let Some(loro::ValueOrContainer::Container(loro::Container::List(parts))) =
                entry.get("parts")
            else {
                continue;
            };
            for j in 0..parts.len() {
                let Some(loro::ValueOrContainer::Container(loro::Container::Map(part))) =
                    parts.get(j)
                else {
                    continue;
                };
                let is_input = matches!(
                    part.get("kind"),
                    Some(loro::ValueOrContainer::Value(LoroValue::String(s))) if s.as_str() == "input"
                );
                let id_matches = matches!(
                    part.get("id"),
                    Some(loro::ValueOrContainer::Value(LoroValue::String(s))) if s.as_str() == request_id
                );
                if is_input && id_matches {
                    part.insert("resolved", true)?;
                    self.doc.commit();
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Stamp the agent's version of a user message: the translation the agent
    /// received for a prompt the transcript shows as typed. The newest user
    /// entry among the last [`USER_AGENT_TEXT_SCAN`] whose text is `source`
    /// (outer whitespace ignored) and that carries no agent version yet wins
    /// — a prompt repeated word for word is stamped once per delivery,
    /// newest first, and a translation reported for text no recent entry
    /// holds is dropped rather than attached to the wrong message. Returns
    /// whether an entry was stamped.
    pub fn stamp_user_agent_text(&self, source: &str, agent_text: &str) -> Result<bool, DocError> {
        let source = source.trim();
        if source.is_empty() || agent_text.trim().is_empty() || agent_text.trim() == source {
            return Ok(false);
        }
        let messages = self.doc.get_list("messages");
        let len = messages.len();
        for i in (len.saturating_sub(USER_AGENT_TEXT_SCAN)..len).rev() {
            let Some(loro::ValueOrContainer::Container(loro::Container::Map(entry))) =
                messages.get(i)
            else {
                continue;
            };
            let is_user = matches!(
                entry.get("role"),
                Some(loro::ValueOrContainer::Value(LoroValue::String(s))) if s.as_str() == "user"
            );
            if !is_user {
                continue;
            }
            let Some(loro::ValueOrContainer::Container(loro::Container::List(parts))) =
                entry.get("parts")
            else {
                continue;
            };
            // User entries are written as one text part (`write_user_message`).
            let Some(loro::ValueOrContainer::Container(loro::Container::Map(part))) = parts.get(0)
            else {
                continue;
            };
            if part.get("agentText").is_some() {
                continue;
            }
            let matches = match part.get("text") {
                Some(loro::ValueOrContainer::Container(loro::Container::Text(t))) => {
                    t.to_string().trim() == source
                }
                _ => false,
            };
            if matches {
                part.insert("agentText", agent_text.trim())?;
                self.doc.commit();
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Seal one attachment against this chat: record the durable final path
    /// (and display file name) for `upload_id` in the `sealedAttachments`
    /// map. The seal is what unblocks a Run command whose `pending_attachments`
    /// names this upload — the host writes it when the UploadCommit lands.
    /// Additive container: docs that never seal anything simply don't carry it.
    /// Idempotent: re-sealing the same upload id overwrites the entry.
    pub fn seal_attachment(
        &self,
        upload_id: &str,
        path: &str,
        file_name: &str,
    ) -> Result<(), DocError> {
        let map = self.doc.get_map("sealedAttachments");
        let entry = map.insert_container(upload_id, LoroMap::new())?;
        entry.insert("path", path)?;
        entry.insert("fileName", file_name)?;
        self.doc.commit();
        Ok(())
    }

    /// The sealed final path + display name for `upload_id`, if any.
    pub fn sealed_attachment(&self, upload_id: &str) -> Result<Option<(String, String)>, DocError> {
        let Some(loro::ValueOrContainer::Container(loro::Container::Map(entry))) =
            self.doc.get_map("sealedAttachments").get(upload_id)
        else {
            return Ok(None);
        };
        let path = match entry.get("path") {
            Some(loro::ValueOrContainer::Value(LoroValue::String(s))) => s.to_string(),
            _ => return Ok(None),
        };
        let file_name = match entry.get("fileName") {
            Some(loro::ValueOrContainer::Value(LoroValue::String(s))) => s.to_string(),
            _ => String::new(),
        };
        Ok(Some((path, file_name)))
    }

    /// Export a snapshot (persistence) — `ExportMode::Snapshot`.
    pub fn export_snapshot(&self) -> Result<Vec<u8>, DocError> {
        self.doc
            .export(ExportMode::Snapshot)
            .map_err(|e| DocError::Schema(e.to_string()))
    }
}

/// How far back [`SessionDoc::stamp_user_agent_text`] looks for the prompt a
/// translation belongs to. The translation lands while its own prompt is at
/// or near the tail; a few steers queued behind it are the only entries that
/// can come after.
const USER_AGENT_TEXT_SCAN: usize = 32;

fn write_entry_scalar_fields(map: &LoroMap, entry: &SessionMessageEntry) -> Result<(), DocError> {
    map.insert("id", entry.id.as_str())?;
    map.insert(
        "role",
        match entry.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
        },
    )?;
    map.insert("createdAt", entry.created_at)?;
    map.insert("deviceId", entry.device_id.as_str())?;
    if let Some(status) = entry.status {
        map.insert("status", status_str(status))?;
    }
    if let Some(continuation_of) = &entry.continuation_of {
        map.insert("continuationOf", continuation_of.as_str())?;
    }
    if let Some(completed_at) = entry.completed_at {
        map.insert("completedAt", completed_at)?;
    }
    if !entry.comments.is_empty() {
        map.insert(
            "comments",
            loro_value_from_json(&serde_json::to_value(&entry.comments)?),
        )?;
    }
    if !entry.models.is_empty() {
        map.insert(
            "models",
            loro_value_from_json(&serde_json::to_value(&entry.models)?),
        )?;
    }
    Ok(())
}

fn status_str(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::Streaming => "streaming",
        MessageStatus::Complete => "complete",
        MessageStatus::Aborted => "aborted",
    }
}

/// Append one part map to a parts list; text bodies become LoroText containers.
fn push_part(parts: &LoroList, part: &MessagePart) -> Result<(), DocError> {
    let map = parts.push_container(LoroMap::new())?;
    let doc_part = to_doc_part(part)?;
    map.insert("id", doc_part.id.as_str())?;
    map.insert("kind", doc_part.kind.as_str())?;
    if let Some(text) = &doc_part.text {
        let t = map.insert_container("text", LoroText::new())?;
        t.insert(0, text)?;
    }
    if let Some(agent_text) = &doc_part.agent_text {
        map.insert("agentText", agent_text.as_str())?;
    }
    if let Some(reasoning) = &doc_part.reasoning {
        // Streamed like text, so a growing thought appends instead of
        // rewriting the whole string every commit.
        let t = map.insert_container("reasoning", LoroText::new())?;
        t.insert(0, reasoning)?;
    }
    if let Some(call) = &doc_part.call {
        map.insert("call", loro_value_from_json(call))?;
    }
    if let Some(is_error) = doc_part.is_error {
        map.insert("isError", is_error)?;
    }
    if let Some(questions) = &doc_part.questions {
        map.insert("questions", loro_value_from_json(questions))?;
    }
    if let Some(resolved) = doc_part.resolved {
        map.insert("resolved", resolved)?;
    }
    if let Some(message) = &doc_part.message {
        map.insert("message", message.as_str())?;
    }
    if let Some(output) = &doc_part.output {
        map.insert("output", output.as_str())?;
    }
    if let Some(progress) = &doc_part.progress {
        map.insert("progress", progress.as_str())?;
    }
    if let Some(diff) = &doc_part.diff {
        map.insert("diff", loro_value_from_json(diff))?;
    }
    if let Some(output_ref) = &doc_part.output_ref {
        map.insert("outputRef", output_ref.as_str())?;
    }
    if let Some(output_bytes) = doc_part.output_bytes {
        map.insert("outputBytes", output_bytes as i64)?;
    }
    if let Some(diff_ref) = &doc_part.diff_ref {
        map.insert("diffRef", diff_ref.as_str())?;
    }
    if let Some(diff_stats) = &doc_part.diff_stats {
        map.insert("diffStats", loro_value_from_json(diff_stats))?;
    }
    Ok(())
}

/// Render-time continuation join at the entry level (`joinContinuations` in TS):
/// concatenate continuation entries' parts onto their root, in list order.
pub fn join_continuation_entries(entries: Vec<SessionMessageEntry>) -> Vec<SessionMessageEntry> {
    if !entries.iter().any(|e| e.continuation_of.is_some()) {
        return entries;
    }
    let mut out: Vec<SessionMessageEntry> = Vec::with_capacity(entries.len());
    let mut root_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for entry in entries {
        match &entry.continuation_of {
            Some(root_id) => {
                if let Some(&at) = root_index.get(root_id) {
                    // The join's span is the root's start to the LAST
                    // segment's finish — a continuation that is still
                    // streaming clears the root's stamp, so a joined entry is
                    // never labelled complete while it is still being written.
                    out[at].completed_at = entry.completed_at;
                    out[at].parts.extend(entry.parts);
                    for model in entry.models {
                        if !out[at].models.contains(&model) {
                            out[at].models.push(model);
                        }
                    }
                } else {
                    // Orphan continuation — surface as its own entry rather than dropping.
                    out.push(entry);
                }
            }
            None => {
                root_index.insert(entry.id.clone(), out.len());
                out.push(entry);
            }
        }
    }
    out
}

fn loro_value_from_json(v: &serde_json::Value) -> LoroValue {
    LoroValue::from(v.clone())
}

#[cfg(test)]
mod tests;
