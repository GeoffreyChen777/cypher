//! Session doc schema over `loro` — Rust port of `packages/session-doc/src/schema.ts`.
//!
//! Container layout (MUST stay shape-compatible with the TS edge/tail materializer):
//! - `meta`:     LoroMap  { chatId: string, schemaVersion: number }         (host-only writer)
//! - `messages`: LoroList of LoroMap {
//!   id, role, parts: LoroList<part map>, createdAt, deviceId, status?, continuationOf?,
//!   completedAt?, comments?: json }
//! - `commands`: LoroList of LoroMap {
//!   id, kind, payload(json), issuedBy, issuedAt, basedOn?, expiresAt?, status, resolution? }
//!
//! Part maps: { id, kind: "text"|"tool"|"input"|"error", text?: LoroText, call?: json,
//! isError?, questions?: json, resolved?, message? }. Text bodies are **LoroText** so streaming
//! appends RLE-merge (1.03x oplog overhead vs 125x for whole-value rewrites).

use loro::{ExportMode, LoroDoc, LoroError, LoroList, LoroMap, LoroText, LoroValue, ToJson};
use serde::{Deserialize, Serialize};

use crate::commands::{SessionCommandEntry, SessionCommandStatus};
use crate::constants::{SESSION_SCHEMA_VERSION, TAIL_MESSAGE_COUNT};
use crate::parts::{MessagePart, MessageStatus};

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
    pub fn stage_preview_coverage(
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
    /// whole transcript — one bad entry took down every publish for the chat
    /// (2026-07-31, "missing field `id`" during a multi-update import).
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

    /// Every sealed attachment as `(upload_id, path, file_name)` — rebuild
    /// copy and diagnostics.
    pub fn sealed_attachments(&self) -> Result<Vec<(String, String, String)>, DocError> {
        let mut out = Vec::new();
        self.doc
            .get_map("sealedAttachments")
            .for_each(|key, value| {
                let loro::ValueOrContainer::Container(loro::Container::Map(entry)) = value else {
                    return;
                };
                let path = match entry.get("path") {
                    Some(loro::ValueOrContainer::Value(LoroValue::String(s))) => s.to_string(),
                    _ => return,
                };
                let file_name = match entry.get("fileName") {
                    Some(loro::ValueOrContainer::Value(LoroValue::String(s))) => s.to_string(),
                    _ => String::new(),
                };
                out.push((key.to_string(), path, file_name));
            });
        Ok(out)
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

fn entry_from_json(v: serde_json::Value) -> Result<SessionMessageEntry, DocError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawEntry {
        id: String,
        role: MessageRole,
        #[serde(default)]
        parts: Vec<DocPartJson>,
        created_at: i64,
        device_id: String,
        #[serde(default)]
        status: Option<MessageStatus>,
        #[serde(default)]
        continuation_of: Option<String>,
        #[serde(default)]
        completed_at: Option<i64>,
        #[serde(default)]
        comments: Vec<MessageComment>,
    }
    match serde_json::from_value::<RawEntry>(v.clone()) {
        Ok(raw) => Ok(SessionMessageEntry {
            id: raw.id,
            role: raw.role,
            parts: raw.parts.into_iter().map(from_doc_part).collect(),
            created_at: raw.created_at,
            device_id: raw.device_id,
            status: raw.status,
            continuation_of: raw.continuation_of,
            completed_at: raw.completed_at,
            comments: raw.comments,
        }),
        // 2026-08-10 incident rule: a missing field must cost AT MOST what
        // the field carried — never the entry, never the transcript. Rooms
        // merge writes from every device and app version; one bad writer
        // (or one mangled export) blanking whole sessions for every reader
        // is exactly what tonight looked like.
        Err(strict_err) => salvage_entry(v, strict_err),
    }
}

/// Field-level salvage for entries the strict shape rejects. Missing
/// identity/attribution fields get deterministic stand-ins (content-hashed
/// id, so repeated reads and continuation joins stay stable); parts are
/// salvaged individually — a part missing `kind` is inferred from its
/// content shape, and only truly contentless parts are dropped.
fn salvage_entry(
    v: serde_json::Value,
    strict_err: serde_json::Error,
) -> Result<SessionMessageEntry, DocError> {
    let Some(obj) = v.as_object() else {
        return Err(DocError::Json(strict_err));
    };
    let stable_hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        v.to_string().hash(&mut hasher);
        hasher.finish()
    };
    let str_field = |key: &str| obj.get(key).and_then(|x| x.as_str()).map(str::to_owned);
    let id = str_field("id").unwrap_or_else(|| format!("recovered-{stable_hash:016x}"));
    let role = obj
        .get("role")
        .and_then(|r| serde_json::from_value::<MessageRole>(r.clone()).ok())
        .unwrap_or(MessageRole::Assistant);
    let mut parts = Vec::new();
    let mut dropped_parts = 0usize;
    if let Some(raw_parts) = obj.get("parts").and_then(|p| p.as_array()) {
        for (ix, part) in raw_parts.iter().enumerate() {
            match serde_json::from_value::<DocPartJson>(part.clone()) {
                Ok(p) => parts.push(from_doc_part(p)),
                Err(_) => match salvage_part(part, &id, ix) {
                    Some(p) => parts.push(p),
                    None => dropped_parts += 1,
                },
            }
        }
    }
    tracing::warn!(
        entry = %id,
        error = %strict_err,
        salvaged_parts = parts.len(),
        dropped_parts,
        "transcript entry failed strict parse; salvaged"
    );
    Ok(SessionMessageEntry {
        id,
        role,
        parts,
        created_at: obj.get("createdAt").and_then(|x| x.as_i64()).unwrap_or(0),
        device_id: str_field("deviceId").unwrap_or_default(),
        status: obj
            .get("status")
            .and_then(|s| serde_json::from_value(s.clone()).ok()),
        continuation_of: str_field("continuationOf"),
        completed_at: obj.get("completedAt").and_then(|x| x.as_i64()),
        comments: obj
            .get("comments")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            .unwrap_or_default(),
    })
}

/// Salvage one part whose strict `DocPartJson` parse failed: infer the kind
/// from the content shape (`text` → text part, parseable `call` → tool
/// part). `None` only when nothing renderable survives.
fn salvage_part(part: &serde_json::Value, entry_id: &str, ix: usize) -> Option<MessagePart> {
    let obj = part.as_object()?;
    let id = obj
        .get("id")
        .and_then(|x| x.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{entry_id}#recovered-{ix}"));
    if let Some(text) = obj.get("text").and_then(|x| x.as_str()) {
        return Some(MessagePart::Text {
            id,
            text: text.to_owned(),
            agent_text: obj
                .get("agentText")
                .and_then(|x| x.as_str())
                .map(str::to_owned),
        });
    }
    if let Some(call) = obj
        .get("call")
        .and_then(|c| serde_json::from_value(c.clone()).ok())
    {
        return Some(MessagePart::Tool {
            id,
            call,
            is_error: obj
                .get("isError")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            resolved: obj
                .get("resolved")
                .and_then(|x| x.as_bool())
                .unwrap_or(true),
            output: obj
                .get("output")
                .and_then(|x| x.as_str())
                .map(str::to_owned),
            progress: obj
                .get("progress")
                .and_then(|x| x.as_str())
                .map(str::to_owned),
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
        });
    }
    if let Some(message) = obj.get("message").and_then(|x| x.as_str()) {
        return Some(MessagePart::Error {
            id,
            message: message.to_owned(),
        });
    }
    None
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

/// Incremental streaming writer for one assistant entry.
///
/// Port of zeron's `DocSegmentWriter` diff discipline: called with the *folded* parts of the
/// live segment (from `fold_event_into_parts`) at each commit tick, it diffs against what's in
/// the doc and writes only the delta:
/// - trailing text growth → `LoroText` append (RLE-merged),
/// - new parts → pushed,
/// - tool call refresh / resolution / input resolution → in-place map updates.
///
/// Invariant relied upon: the fold only ever APPENDS parts or grows the trailing text; earlier
/// text never mutates. Tool/input parts may update fields in place.
pub struct SegmentWriter<'a> {
    doc: &'a SessionDoc,
    /// Index of this entry in the `messages` list.
    entry_index: usize,
    /// Mirror of what we've written so far (part id → app part).
    written: Vec<MessagePart>,
    entry_id: String,
}

impl<'a> SegmentWriter<'a> {
    /// Begin a streaming assistant entry: pushes the entry with `status: streaming`, no parts.
    pub fn begin(
        doc: &'a SessionDoc,
        entry_id: &str,
        device_id: &str,
        created_at: i64,
    ) -> Result<Self, DocError> {
        let messages = doc.doc.get_list("messages");
        let entry_index = messages.len();
        let map = messages.push_container(LoroMap::new())?;
        write_entry_scalar_fields(
            &map,
            &SessionMessageEntry {
                id: entry_id.into(),
                role: MessageRole::Assistant,
                parts: vec![],
                created_at,
                device_id: device_id.into(),
                status: Some(MessageStatus::Streaming),
                continuation_of: None,
                completed_at: None,
                comments: Vec::new(),
            },
        )?;
        map.insert_container("parts", LoroList::new())?;
        doc.doc.commit();
        Ok(Self {
            doc,
            entry_index,
            written: Vec::new(),
            entry_id: entry_id.to_owned(),
        })
    }

    fn entry_map(&self) -> Result<LoroMap, DocError> {
        let messages = self.doc.doc.get_list("messages");
        match messages.get(self.entry_index) {
            Some(loro::ValueOrContainer::Container(loro::Container::Map(map))) => Ok(map),
            _ => Err(DocError::Schema("streaming entry map missing".into())),
        }
    }

    fn parts_list(&self) -> Result<LoroList, DocError> {
        match self.entry_map()?.get("parts") {
            Some(loro::ValueOrContainer::Container(loro::Container::List(list))) => Ok(list),
            _ => Err(DocError::Schema(
                "streaming entry parts list missing".into(),
            )),
        }
    }

    /// Diff `folded` (the full folded segment so far) into the doc.
    pub fn sync(&mut self, folded: &[MessagePart]) -> Result<(), DocError> {
        let parts = self.parts_list()?;
        let mut dirty = false;

        for (i, part) in folded.iter().enumerate() {
            match self.written.get(i) {
                None => {
                    push_part(&parts, part)?;
                    self.written.push(part.clone());
                    dirty = true;
                }
                Some(prev) if prev == part => {}
                Some(prev) => {
                    match (prev, part) {
                        (
                            MessagePart::Text {
                                text: old,
                                agent_text: old_agent,
                                ..
                            },
                            MessagePart::Text {
                                text: new,
                                agent_text: new_agent,
                                ..
                            },
                        ) if new.starts_with(old.as_str()) => {
                            // An append-mode translation GROWS the text (the
                            // original is its prefix) while stamping the
                            // agent's version in the same step.
                            if old_agent != new_agent {
                                let part_map = part_map_at(&parts, i)?;
                                write_agent_text(&part_map, new_agent.as_deref())?;
                                dirty = true;
                            }
                            // Trailing-text growth: append the suffix into the LoroText.
                            let delta = &new[old.len()..];
                            if !delta.is_empty() {
                                let part_map = part_map_at(&parts, i)?;
                                match part_map.get("text") {
                                    Some(loro::ValueOrContainer::Container(
                                        loro::Container::Text(t),
                                    )) => {
                                        let len = t.len_unicode();
                                        t.insert(len, delta)?;
                                    }
                                    _ => {
                                        return Err(DocError::Schema(
                                            "text part missing LoroText".into(),
                                        ));
                                    }
                                }
                                dirty = true;
                            }
                        }
                        _ => {
                            // Field-level update (tool refresh/resolve, input resolve, or a
                            // non-append text rewrite, which the fold shouldn't produce —
                            // rewrite the part map fields defensively).
                            let part_map = part_map_at(&parts, i)?;
                            update_part_fields(&part_map, part)?;
                            dirty = true;
                        }
                    }
                    self.written[i] = part.clone();
                }
            }
        }

        dirty |= self.doc.preview_commit(&self.entry_id, folded, false)?;
        if dirty {
            self.doc.doc.commit();
        }
        Ok(())
    }

    /// Finish the stream: sync final parts and stamp a terminal status plus
    /// the completion instant (the settled turn's elapsed base — `createdAt`
    /// is the segment's start).
    pub fn finish(mut self, folded: &[MessagePart], status: MessageStatus) -> Result<(), DocError> {
        self.sync(folded)?;
        let map = self.entry_map()?;
        map.insert("status", status_str(status))?;
        map.insert("completedAt", chrono::Utc::now().timestamp_millis())?;
        self.doc.preview_commit(&self.entry_id, folded, true)?;
        self.doc.doc.commit();
        Ok(())
    }
}

fn part_map_at(parts: &LoroList, index: usize) -> Result<LoroMap, DocError> {
    match parts.get(index) {
        Some(loro::ValueOrContainer::Container(loro::Container::Map(map))) => Ok(map),
        _ => Err(DocError::Schema(format!("part map missing at {index}"))),
    }
}

/// In-place field refresh for tool/input parts (and defensive text rewrite).
fn update_part_fields(map: &LoroMap, part: &MessagePart) -> Result<(), DocError> {
    let doc_part = to_doc_part(part)?;
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
    } else {
        // Resolve clears the transient column — the delete must actually land
        // in Loro, or a settled chip would keep rendering a stale live tail.
        map.delete("progress")?;
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
    if doc_part.kind == "text" {
        write_agent_text(map, doc_part.agent_text.as_deref())?;
    }
    if let Some(text) = &doc_part.text {
        // A translation frame replaces the text wholesale; otherwise this is
        // a defensive path only — the fold never rewrites earlier text.
        if let Some(loro::ValueOrContainer::Container(loro::Container::Text(t))) = map.get("text") {
            t.update(text, Default::default())
                .map_err(|e| DocError::Schema(e.to_string()))?;
        }
    }
    Ok(())
}

/// Set or clear a text part's `agentText` — a cleared value must actually be
/// deleted, or a translation that fell back to the original would keep
/// mapping quotes through a version that no longer stands.
fn write_agent_text(map: &LoroMap, agent_text: Option<&str>) -> Result<(), DocError> {
    match agent_text {
        Some(agent_text) => map.insert("agentText", agent_text)?,
        None if map.get("agentText").is_some() => map.delete("agentText")?,
        None => {}
    }
    Ok(())
}

fn loro_value_from_json(v: &serde_json::Value) -> LoroValue {
    LoroValue::from(v.clone())
}

/// Tail sidecar shape (`SessionTail` in TS).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTail {
    pub chat_id: String,
    pub schema_version: u32,
    pub messages: Vec<SessionMessageEntry>,
    pub total_messages: usize,
    pub updated_at: i64,
}

/// Materialize the last-N joined messages (`materializeTail` in TS).
pub fn materialize_tail(
    doc: &SessionDoc,
    now: i64,
    tail_count: usize,
) -> Result<SessionTail, DocError> {
    let all = join_continuation_entries(doc.read_entries()?);
    let total = all.len();
    let start = total.saturating_sub(if tail_count == 0 {
        TAIL_MESSAGE_COUNT
    } else {
        tail_count
    });
    Ok(SessionTail {
        chat_id: doc.chat_id().unwrap_or_default(),
        schema_version: SESSION_SCHEMA_VERSION,
        messages: all[start..].to_vec(),
        total_messages: total,
        updated_at: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::fold_event_into_parts;
    use cypher_proto::{AgentEvent, ToolCall};

    fn user_entry(id: &str, text: &str) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "t0".into(),
                text: text.into(),
                agent_text: None,
            }],
            created_at: 1,
            device_id: "dev-a".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
            completed_at: None,
            comments: Vec::new(),
        }
    }

    #[test]
    fn round_trips_message_entries() {
        let doc = SessionDoc::init("chat-1").unwrap();
        doc.push_message(&user_entry("m1", "hello")).unwrap();
        let entries = doc.read_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "m1");
        assert_eq!(
            entries[0].parts,
            vec![MessagePart::Text {
                id: "t0".into(),
                text: "hello".into(),
                agent_text: None,
            }]
        );
        assert_eq!(doc.chat_id().as_deref(), Some("chat-1"));
    }

    #[test]
    fn round_trips_message_comments() {
        let doc = SessionDoc::init("chat-1").unwrap();
        let mut commented = user_entry("m1", "");
        commented.comments = vec![MessageComment {
            quote: "a \"quoted\"\nline".into(),
            comment: "why?".into(),
        }];
        doc.push_message(&commented).unwrap();
        doc.push_message(&user_entry("m2", "plain")).unwrap();
        let other = LoroDoc::new();
        other.import(&doc.export_snapshot().unwrap()).unwrap();
        let entries = SessionDoc::from_doc(other).read_entries().unwrap();
        assert_eq!(entries[0].comments, commented.comments);
        assert!(entries[1].comments.is_empty());
    }

    #[test]
    fn remove_messages_truncates_and_leaves_the_prefix_intact() {
        let doc = SessionDoc::init("chat-1").unwrap();
        for id in ["m1", "m2", "m3", "m4"] {
            doc.push_message(&user_entry(id, id)).unwrap();
        }
        let cut: std::collections::HashSet<String> =
            ["m2".to_string(), "m4".to_string()].into_iter().collect();
        assert_eq!(doc.remove_messages(&cut).unwrap(), 2);
        let left: Vec<String> = doc
            .read_entries()
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(left, vec!["m1".to_string(), "m3".to_string()]);
        // Idempotent: the ids are already gone.
        assert_eq!(doc.remove_messages(&cut).unwrap(), 0);
        // An empty set never touches the doc.
        assert_eq!(
            doc.remove_messages(&std::collections::HashSet::new())
                .unwrap(),
            0
        );
        assert_eq!(doc.read_entries().unwrap().len(), 2);
    }

    #[test]
    fn resolve_input_stamps_the_part_in_place() {
        let doc = SessionDoc::init("chat-1").unwrap();
        doc.push_message(&SessionMessageEntry {
            id: "m1".into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Input {
                id: "r1".into(),
                request_id: "r1".into(),
                questions: vec![],
                resolved: false,
            }],
            created_at: 1,
            device_id: "dev-a".into(),
            // The orphan case: the run died and recovery stamped the entry.
            status: Some(MessageStatus::Aborted),
            continuation_of: None,
            completed_at: None,
            comments: Vec::new(),
        })
        .unwrap();
        assert!(!doc.resolve_input("nope").unwrap());
        assert!(doc.resolve_input("r1").unwrap());
        let entries = doc.read_entries().unwrap();
        assert!(matches!(
            &entries[0].parts[0],
            MessagePart::Input { resolved: true, .. }
        ));
    }

    #[test]
    fn snapshot_round_trips_between_docs() {
        let doc = SessionDoc::init("chat-1").unwrap();
        doc.push_message(&user_entry("m1", "hello")).unwrap();
        let bytes = doc.export_snapshot().unwrap();

        let other = LoroDoc::new();
        other.import(&bytes).unwrap();
        let restored = SessionDoc::from_doc(other);
        assert_eq!(
            restored.read_entries().unwrap(),
            doc.read_entries().unwrap()
        );
    }

    #[test]
    fn two_peers_converge_on_concurrent_inserts() {
        let a = SessionDoc::init("chat-1").unwrap();
        let b = SessionDoc::from_doc({
            let d = LoroDoc::new();
            d.import(&a.export_snapshot().unwrap()).unwrap();
            d
        });
        a.push_message(&user_entry("m-a", "from a")).unwrap();
        b.push_message(&user_entry("m-b", "from b")).unwrap();

        // Cross-import updates.
        let a_update = a
            .doc()
            .export(ExportMode::updates(&b.doc().oplog_vv()))
            .unwrap();
        let b_update = b
            .doc()
            .export(ExportMode::updates(&a.doc().oplog_vv()))
            .unwrap();
        b.doc().import(&a_update).unwrap();
        a.doc().import(&b_update).unwrap();

        let ea = a.read_entries().unwrap();
        let eb = b.read_entries().unwrap();
        assert_eq!(ea, eb);
        assert_eq!(ea.len(), 2); // one insert from each peer, converged in the same order
    }

    #[test]
    fn segment_writer_streams_text_incrementally() {
        let doc = SessionDoc::init("chat-1").unwrap();
        let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();

        let mut folded = Vec::new();
        fold_event_into_parts(&mut folded, &AgentEvent::TextDelta { text: "Hel".into() });
        writer.sync(&folded).unwrap();
        fold_event_into_parts(&mut folded, &AgentEvent::TextDelta { text: "lo".into() });
        writer.sync(&folded).unwrap();
        fold_event_into_parts(
            &mut folded,
            &AgentEvent::ToolCall {
                id: "tool-1".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
            },
        );
        writer.sync(&folded).unwrap();
        fold_event_into_parts(
            &mut folded,
            &AgentEvent::ToolResult {
                id: "tool-1".into(),
                is_error: false,
                output: None,
                diff: None,
            },
        );
        writer.sync(&folded).unwrap();
        writer.finish(&folded, MessageStatus::Complete).unwrap();

        let entries = doc.read_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, Some(MessageStatus::Complete));
        assert_eq!(entries[0].parts.len(), 2);
        match &entries[0].parts[0] {
            MessagePart::Text { text, .. } => assert_eq!(text, "Hello"),
            other => panic!("unexpected {other:?}"),
        }
        match &entries[0].parts[1] {
            MessagePart::Tool {
                resolved, is_error, ..
            } => {
                assert!(*resolved);
                assert!(!*is_error);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    fn agent_text_of(doc: &SessionDoc) -> (String, Option<String>) {
        match &doc.read_entries().unwrap()[0].parts[0] {
            MessagePart::Text {
                text, agent_text, ..
            } => (text.clone(), agent_text.clone()),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// A replace-mode translation rewrites the text wholesale; the answer the
    /// model wrote survives in Loro as the part's agent version.
    #[test]
    fn segment_writer_keeps_the_original_under_a_translation() {
        let doc = SessionDoc::init("chat-1").unwrap();
        let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();
        let mut folded = Vec::new();
        fold_event_into_parts(
            &mut folded,
            &AgentEvent::TextDelta {
                text: "The answer.".into(),
            },
        );
        writer.sync(&folded).unwrap();
        for frame in ["The answer.", "答", "答案。"] {
            fold_event_into_parts(&mut folded, &AgentEvent::Translation { text: frame.into() });
            writer.sync(&folded).unwrap();
        }
        writer.finish(&folded, MessageStatus::Complete).unwrap();
        assert_eq!(
            agent_text_of(&doc),
            ("答案。".to_string(), Some("The answer.".to_string()))
        );
    }

    /// Append mode grows the text with the original as its prefix — the
    /// writer's append path must still stamp the agent version.
    #[test]
    fn segment_writer_stamps_the_original_on_an_append_mode_growth() {
        let doc = SessionDoc::init("chat-1").unwrap();
        let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();
        let mut folded = Vec::new();
        fold_event_into_parts(&mut folded, &AgentEvent::TextDelta { text: "Hi.".into() });
        writer.sync(&folded).unwrap();
        fold_event_into_parts(
            &mut folded,
            &AgentEvent::Translation {
                text: "Hi.\n\n---\n\n你好。".into(),
            },
        );
        writer.sync(&folded).unwrap();
        assert_eq!(
            agent_text_of(&doc),
            ("Hi.\n\n---\n\n你好。".to_string(), Some("Hi.".to_string()))
        );
    }

    /// A translation that fails puts the original back — nothing is
    /// translated any more, so the agent version must be DELETED in Loro, not
    /// just dropped from the fold's mirror.
    #[test]
    fn a_restored_original_clears_the_agent_version() {
        let doc = SessionDoc::init("chat-1").unwrap();
        let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();
        let mut folded = Vec::new();
        fold_event_into_parts(
            &mut folded,
            &AgentEvent::TextDelta {
                text: "Answer".into(),
            },
        );
        writer.sync(&folded).unwrap();
        fold_event_into_parts(&mut folded, &AgentEvent::Translation { text: "答".into() });
        writer.sync(&folded).unwrap();
        assert_eq!(agent_text_of(&doc).1.as_deref(), Some("Answer"));
        fold_event_into_parts(
            &mut folded,
            &AgentEvent::Translation {
                text: "Answer".into(),
            },
        );
        writer.finish(&folded, MessageStatus::Complete).unwrap();
        assert_eq!(agent_text_of(&doc), ("Answer".to_string(), None));
    }

    #[test]
    fn user_agent_text_stamps_the_newest_matching_unstamped_prompt() {
        let doc = SessionDoc::init("chat-1").unwrap();
        doc.push_message(&user_entry("m1", "你好")).unwrap();
        doc.push_message(&user_entry("m2", "别的")).unwrap();
        doc.push_message(&user_entry("m3", "你好")).unwrap();
        // Outer whitespace on either side is not a difference.
        assert!(doc.stamp_user_agent_text(" 你好\n", "Hello").unwrap());
        assert!(doc.stamp_user_agent_text("你好", "Hi").unwrap());
        // Every copy is stamped; nothing else matches.
        assert!(!doc.stamp_user_agent_text("你好", "Hey").unwrap());
        assert!(!doc.stamp_user_agent_text("missing", "x").unwrap());
        // An "unchanged" translation is no translation.
        assert!(!doc.stamp_user_agent_text("别的", "别的").unwrap());
        let stamped: Vec<Option<String>> = doc
            .read_entries()
            .unwrap()
            .into_iter()
            .map(|e| match &e.parts[0] {
                MessagePart::Text { agent_text, .. } => agent_text.clone(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(stamped, vec![Some("Hi".into()), None, Some("Hello".into())]);
    }

    /// Live progress is transient column state: a ToolCall creates the part
    /// without it, ToolProgress writes the tail into Loro, and ToolResult
    /// CLEARS it (the delete must actually land in Loro — not just in the
    /// fold's in-memory mirror — or a settled chip keeps a stale live tail).
    #[test]
    fn segment_writer_persists_progress_and_clears_on_resolve() {
        let doc = SessionDoc::init("chat-4").unwrap();
        let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();

        let mut folded = Vec::new();
        fold_event_into_parts(
            &mut folded,
            &AgentEvent::ToolCall {
                id: "t1".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
            },
        );
        writer.sync(&folded).unwrap();
        let entries = doc.read_entries().unwrap();
        assert!(matches!(
            &entries[0].parts[0],
            MessagePart::Tool {
                progress: None,
                resolved: false,
                ..
            }
        ));

        // A progress tick writes the transient tail into Loro.
        fold_event_into_parts(
            &mut folded,
            &AgentEvent::ToolProgress {
                id: "t1".into(),
                output: "compiling\nlinking".into(),
            },
        );
        writer.sync(&folded).unwrap();
        let entries = doc.read_entries().unwrap();
        match &entries[0].parts[0] {
            MessagePart::Tool {
                progress,
                resolved: false,
                ..
            } => assert_eq!(progress.as_deref(), Some("compiling\nlinking")),
            other => panic!("unexpected {other:?}"),
        }

        // Resolve clears the column in Loro.
        fold_event_into_parts(
            &mut folded,
            &AgentEvent::ToolResult {
                id: "t1".into(),
                is_error: false,
                output: None,
                diff: None,
            },
        );
        writer.sync(&folded).unwrap();
        writer.finish(&folded, MessageStatus::Complete).unwrap();
        let entries = doc.read_entries().unwrap();
        match &entries[0].parts[0] {
            MessagePart::Tool {
                resolved: true,
                progress: None,
                ..
            } => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    /// The ToolResult resolution path goes through `update_part_fields` —
    /// the stripped output summary, sidecar refs, and diff stats must survive
    /// the doc round trip (regression: output/diff were silently dropped
    /// there while `to_doc_part` carried them).
    #[test]
    fn segment_writer_round_trips_stripped_tool_fields() {
        let doc = SessionDoc::init("chat-2").unwrap();
        let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();

        let mut folded = Vec::new();
        fold_event_into_parts(
            &mut folded,
            &AgentEvent::ToolCall {
                id: "t1".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
            },
        );
        writer.sync(&folded).unwrap();
        fold_event_into_parts(
            &mut folded,
            &AgentEvent::ToolResult {
                id: "t1".into(),
                is_error: false,
                output: Some("total 0\nmore lines".into()),
                diff: Some(cypher_proto::ToolDiff {
                    path: "/w/a.rs".into(),
                    old_text: Some("old\n".into()),
                    new_text: "new\n".into(),
                }),
            },
        );
        crate::parts::apply_sidecar_refs("chat-2", &mut folded);
        writer.sync(&folded).unwrap();
        writer.finish(&folded, MessageStatus::Complete).unwrap();

        let entries = doc.read_entries().unwrap();
        match &entries[0].parts[0] {
            MessagePart::Tool {
                output,
                output_ref,
                output_bytes,
                diff,
                diff_ref,
                diff_stats,
                ..
            } => {
                // The bounded output summary is retained for the expandable
                // chip body. Full-output sidecar refs remain absent while
                // sidecar storage is disabled; diff stats still get their ref.
                assert_eq!(output.as_deref(), Some("total 0\nmore lines"));
                assert_eq!(output_ref.as_deref(), None);
                assert_eq!(*output_bytes, None);
                assert!(diff.is_none(), "no inline diff text in the doc");
                assert_eq!(diff_ref.as_deref(), Some("chat-2/t1.diff"));
                let stats = diff_stats.as_ref().expect("stats survive");
                assert_eq!(stats[0].path, "/w/a.rs");
                assert_eq!((stats[0].additions, stats[0].deletions), (1, 1));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Old pre-strip docs carry inline `output`/`diff` — they must still read
    /// back (schema changes are serde-additive ONLY; old readers, old docs).
    #[test]
    fn pre_strip_doc_parts_still_round_trip() {
        let doc = SessionDoc::init("chat-3").unwrap();
        doc.push_message(&SessionMessageEntry {
            id: "m1".into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Tool {
                id: "t1".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
                is_error: false,
                resolved: true,
                output: Some("full inline output\nline 2".into()),
                progress: None,
                diff: Some(cypher_proto::ToolDiff {
                    path: "/w/a.rs".into(),
                    old_text: Some("old".into()),
                    new_text: "new".into(),
                }),
                output_ref: None,
                output_bytes: None,
                diff_ref: None,
                diff_stats: None,
            }],
            created_at: 1,
            device_id: "dev-a".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
            completed_at: None,
            comments: Vec::new(),
        })
        .unwrap();
        let entries = doc.read_entries().unwrap();
        match &entries[0].parts[0] {
            MessagePart::Tool { output, diff, .. } => {
                assert_eq!(output.as_deref(), Some("full inline output\nline 2"));
                assert_eq!(diff.as_ref().unwrap().new_text, "new");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn set_message_status_stamps_existing_entry() {
        let doc = SessionDoc::init("chat-1").unwrap();
        let mut entry = user_entry("m1", "hello");
        entry.role = MessageRole::Assistant;
        entry.status = Some(MessageStatus::Streaming);
        doc.push_message(&entry).unwrap();

        assert!(
            doc.set_message_status("m1", MessageStatus::Aborted)
                .unwrap()
        );
        assert!(
            !doc.set_message_status("nope", MessageStatus::Aborted)
                .unwrap()
        );
        let entries = doc.read_entries().unwrap();
        assert_eq!(entries[0].status, Some(MessageStatus::Aborted));
        // Crash recovery closes the entry here: it still gets a span.
        assert!(
            entries[0]
                .completed_at
                .is_some_and(|at| at >= entry.created_at)
        );
    }

    /// A finished segment carries its own span: `createdAt` → `completedAt`.
    /// That pair is the only durable record of how long a settled turn took
    /// (the transcript's "Worked for …" rule), so it must survive a reload of
    /// the doc, not just the live session.
    #[test]
    fn finish_stamps_the_completion_instant() {
        let doc = SessionDoc::init("chat-1").unwrap();
        let started = chrono::Utc::now().timestamp_millis();
        let writer = SegmentWriter::begin(&doc, "m1", "dev", started).unwrap();
        let folded = vec![MessagePart::Text {
            id: "t0".into(),
            text: "hello".into(),
            agent_text: None,
        }];
        // Streaming entries carry no completion — the turn is still open.
        assert_eq!(doc.read_entries().unwrap()[0].completed_at, None);
        writer.finish(&folded, MessageStatus::Complete).unwrap();

        let reopened = LoroDoc::new();
        reopened.import(&doc.export_snapshot().unwrap()).unwrap();
        let entry = SessionDoc::from_doc(reopened)
            .read_entries()
            .unwrap()
            .remove(0);
        assert_eq!(entry.status, Some(MessageStatus::Complete));
        assert!(entry.completed_at.is_some_and(|at| at >= started));
    }

    /// The join spans the root's start to the LAST segment's finish — and a
    /// continuation still streaming leaves the joined entry unstamped.
    #[test]
    fn continuation_join_takes_the_last_segments_completion() {
        let mut root = user_entry("m1", "a");
        root.role = MessageRole::Assistant;
        root.created_at = 1_000;
        root.completed_at = Some(2_000);
        let mut tail = user_entry("m1#c1", "b");
        tail.role = MessageRole::Assistant;
        tail.continuation_of = Some("m1".into());
        tail.completed_at = Some(9_000);

        let joined = join_continuation_entries(vec![root.clone(), tail.clone()]);
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].created_at, 1_000);
        assert_eq!(joined[0].completed_at, Some(9_000));

        let mut live_tail = tail;
        live_tail.completed_at = None;
        let joined = join_continuation_entries(vec![root, live_tail]);
        assert_eq!(joined[0].completed_at, None, "still being written");
    }

    #[test]
    fn command_queue_and_outcome_round_trip() {
        use crate::commands::{SessionCommandPayload, SessionCommandStatus};
        let doc = SessionDoc::init("chat-1").unwrap();
        let entry = SessionCommandEntry {
            id: "c1".into(),
            payload: SessionCommandPayload::Steer {
                prompt: "focus".into(),
                message_id: None,
                agent_prompt: None,
            },
            issued_by: "dev-b".into(),
            issued_at: 10,
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Pending,
            resolution: None,
            sent_at: None,
        };
        doc.queue_command(&entry).unwrap();
        doc.set_command_status("c1", SessionCommandStatus::Applied, None)
            .unwrap();
        let commands = doc.read_commands().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].status, SessionCommandStatus::Applied);
        assert_eq!(commands[0].payload, entry.payload);
    }

    #[test]
    fn sealed_attachments_round_trip_and_survive_snapshot() {
        // A fresh doc (no container) reads empty — the container is additive.
        let doc = SessionDoc::init("chat-1").unwrap();
        assert!(doc.sealed_attachment("up-1").unwrap().is_none());
        assert!(doc.sealed_attachments().unwrap().is_empty());

        doc.seal_attachment("up-1", "/up/1-a.png", "a.png").unwrap();
        doc.seal_attachment("up-2", "/up/2-b.png", "b.png").unwrap();
        assert_eq!(
            doc.sealed_attachment("up-1").unwrap(),
            Some(("/up/1-a.png".into(), "a.png".into()))
        );
        let mut sealed = doc.sealed_attachments().unwrap();
        sealed.sort();
        assert_eq!(
            sealed,
            vec![
                ("up-1".into(), "/up/1-a.png".into(), "a.png".into()),
                ("up-2".into(), "/up/2-b.png".into(), "b.png".into()),
            ]
        );
        // Re-sealing the same id is idempotent (overwrites in place).
        doc.seal_attachment("up-1", "/up/1-c.png", "c.png").unwrap();
        assert_eq!(
            doc.sealed_attachment("up-1").unwrap(),
            Some(("/up/1-c.png".into(), "c.png".into()))
        );
        assert_eq!(doc.sealed_attachments().unwrap().len(), 2);

        // The container crosses an export/import snapshot intact.
        let bytes = doc.export_snapshot().unwrap();
        let restored = SessionDoc::from_doc({
            let d = loro::LoroDoc::new();
            d.import(&bytes).unwrap();
            d
        });
        assert_eq!(
            restored.sealed_attachment("up-1").unwrap(),
            Some(("/up/1-c.png".into(), "c.png".into()))
        );
        assert_eq!(restored.sealed_attachments().unwrap().len(), 2);
    }

    #[test]
    fn tail_materializes_last_n_joined() {
        let doc = SessionDoc::init("chat-1").unwrap();
        for i in 0..5 {
            doc.push_message(&user_entry(&format!("m{i}"), &format!("msg {i}")))
                .unwrap();
        }
        let tail = materialize_tail(&doc, 99, 2).unwrap();
        assert_eq!(tail.total_messages, 5);
        assert_eq!(tail.messages.len(), 2);
        assert_eq!(tail.messages[1].id, "m4");
        assert_eq!(tail.chat_id, "chat-1");
    }

    /// 2026-08-10 incident: entries/parts missing strict fields must salvage
    /// field-by-field — a fresh reader importing a room's merged doc must
    /// never render a BLANK transcript because some writer (old app version,
    /// other-platform client, mangled export) omitted metadata.
    #[test]
    fn malformed_entries_salvage_instead_of_vanishing() {
        // Entry missing `id` + `deviceId`; one part missing `kind` but
        // carrying text; one part contentless (dropped).
        let v = serde_json::json!({
            "role": "assistant",
            "createdAt": 123,
            "parts": [
                { "id": "p1", "text": "still readable" },
                { "opaque": true },
                { "id": "p3", "kind": "text", "text": "well-formed" }
            ]
        });
        let entry = entry_from_json(v.clone()).expect("salvaged");
        assert!(
            entry.id.starts_with("recovered-"),
            "deterministic stand-in id"
        );
        let again = entry_from_json(v).expect("salvaged again");
        assert_eq!(entry.id, again.id, "recovered id is stable across reads");
        assert_eq!(entry.role, MessageRole::Assistant);
        assert_eq!(entry.created_at, 123);
        assert_eq!(
            entry.parts.len(),
            2,
            "text parts survive, contentless part dropped"
        );
        match &entry.parts[0] {
            MessagePart::Text { text, .. } => assert_eq!(text, "still readable"),
            other => panic!("unexpected {other:?}"),
        }

        // Tool part missing `kind` but with a parseable call salvages as Tool.
        let v = serde_json::json!({
            "role": "assistant",
            "createdAt": 1,
            "parts": [ { "id": "t1", "call": { "kind": "exec", "command": "ls" }, "output": "x" } ]
        });
        let entry = entry_from_json(v).expect("salvaged");
        assert!(matches!(
            &entry.parts[0],
            MessagePart::Tool { resolved: true, .. }
        ));

        // Only non-objects are truly unsalvageable.
        assert!(entry_from_json(serde_json::json!("garbage")).is_err());
        assert!(entry_from_json(serde_json::json!(42)).is_err());

        // Well-formed entries take the strict path unchanged.
        let v = serde_json::json!({
            "id": "m1", "role": "user", "createdAt": 5, "deviceId": "d",
            "parts": [ { "id": "p", "kind": "text", "text": "hi" } ]
        });
        let entry = entry_from_json(v).expect("strict");
        assert_eq!(entry.id, "m1");
    }
}
