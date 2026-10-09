//! [`SegmentWriter`]: the incremental streaming writer for one assistant entry.

use super::*;

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
    /// The `models` written so far, and whether a change awaits a commit.
    models: Vec<AnsweredModel>,
    models_dirty: bool,
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
                models: Vec::new(),
            },
        )?;
        map.insert_container("parts", LoroList::new())?;
        doc.doc.commit();
        Ok(Self {
            doc,
            entry_index,
            written: Vec::new(),
            entry_id: entry_id.to_owned(),
            models: Vec::new(),
            models_dirty: false,
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

    /// Record the models that answered the segment so far. Written into the
    /// entry now; committed with the next [`Self::sync`] or [`Self::finish`].
    pub fn set_models(&mut self, models: &[AnsweredModel]) -> Result<(), DocError> {
        if self.models == models {
            return Ok(());
        }
        self.entry_map()?.insert(
            "models",
            loro_value_from_json(&serde_json::to_value(models)?),
        )?;
        self.models = models.to_vec();
        self.models_dirty = true;
        Ok(())
    }

    /// Diff `folded` (the full folded segment so far) into the doc.
    pub fn sync(&mut self, folded: &[MessagePart]) -> Result<(), DocError> {
        let parts = self.parts_list()?;
        // A models change alone may wait for the next eager commit, like
        // thinking growth: it labels the turn, which settles with `finish`.
        let mut dirty = std::mem::take(&mut self.models_dirty);
        // Any change other than thinking growth: the commit must reach other
        // devices on the normal cadence (see `local_commit_is_deferrable`).
        let mut eager = false;

        for (i, part) in folded.iter().enumerate() {
            match self.written.get(i) {
                None => {
                    push_part(&parts, part)?;
                    self.written.push(part.clone());
                    dirty = true;
                    eager |= !matches!(part, MessagePart::Reasoning { .. });
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
                                eager = true;
                            }
                            // Trailing-text growth: append the suffix into the LoroText.
                            let delta = &new[old.len()..];
                            if !delta.is_empty() {
                                append_text(&part_map_at(&parts, i)?, "text", delta)?;
                                dirty = true;
                                eager = true;
                            }
                        }
                        (
                            MessagePart::Reasoning { text: old, .. },
                            MessagePart::Reasoning { text: new, .. },
                        ) if new.starts_with(old.as_str()) => {
                            let delta = &new[old.len()..];
                            if !delta.is_empty() {
                                append_text(&part_map_at(&parts, i)?, "reasoning", delta)?;
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
                            eager = true;
                        }
                    }
                    self.written[i] = part.clone();
                }
            }
        }

        if self.doc.preview_commit(&self.entry_id, folded, false)? {
            dirty = true;
            eager = true;
        }
        if dirty && eager {
            self.doc.doc.commit();
        } else if dirty {
            commit_deferrable(&self.doc.doc);
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

thread_local! {
    static DEFERRABLE_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the local commit running on this thread only grew reasoning. A
/// sync client may hold such an update for its next push instead of
/// scheduling one: thinking then reaches other devices with the next text,
/// tool call or segment boundary, for no writes of its own. Meaningful only
/// inside a `subscribe_local_update` hook, which Loro runs synchronously in
/// the commit, on the committing thread — another writer's commit can never
/// read this one's mark.
pub fn local_commit_is_deferrable() -> bool {
    DEFERRABLE_COMMIT.with(std::cell::Cell::get)
}

fn commit_deferrable(doc: &LoroDoc) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            DEFERRABLE_COMMIT.with(|mark| mark.set(false));
        }
    }
    DEFERRABLE_COMMIT.with(|mark| mark.set(true));
    let _reset = Reset;
    doc.commit();
}

/// Append `delta` to the part's streamed LoroText under `key`.
fn append_text(part_map: &LoroMap, key: &str, delta: &str) -> Result<(), DocError> {
    match part_map.get(key) {
        Some(loro::ValueOrContainer::Container(loro::Container::Text(t))) => {
            t.insert(t.len_unicode(), delta)?;
            Ok(())
        }
        _ => Err(DocError::Schema(format!("{key} part missing LoroText"))),
    }
}

fn part_map_at(parts: &LoroList, index: usize) -> Result<LoroMap, DocError> {
    match parts.get(index) {
        Some(loro::ValueOrContainer::Container(loro::Container::Map(map))) => Ok(map),
        _ => Err(DocError::Schema(format!("part map missing at {index}"))),
    }
}

/// In-place field refresh for tool/input parts (and defensive text rewrite).
pub(super) fn update_part_fields(map: &LoroMap, part: &MessagePart) -> Result<(), DocError> {
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
    if let Some(reasoning) = &doc_part.reasoning
        && let Some(loro::ValueOrContainer::Container(loro::Container::Text(t))) =
            map.get("reasoning")
    {
        // Defensive only: the fold never rewrites earlier thinking.
        t.update(reasoning, Default::default())
            .map_err(|e| DocError::Schema(e.to_string()))?;
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
