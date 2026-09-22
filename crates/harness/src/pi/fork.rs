//! Pi-native Session Fork controller (Session Fork v1).
//!
//! A fork is materialized by a SEPARATE short-lived helper process
//! (`pi --mode rpc --session-dir <cypher-owned> --no-extensions`) — never the
//! source chat's live client. `--no-extensions` guarantees no
//! `session_before_fork` extension hook can cancel or mutate the fork.
//!
//! Protocol (pi's own RPC, `docs/rpc.md`):
//! - `switch_session {sessionPath}` — load the source session;
//! - `get_entries` → `{entries, leafId}` in APPEND order (abandoned branches
//!   included) — the ACTIVE branch is rebuilt by walking `leafId -> parentId`;
//! - `fork {entryId}` — new persisted session BEFORE that user entry,
//!   restoring its prompt into the editor; the source file is never mutated;
//! - `clone {}` — new persisted session duplicating the active branch at its
//!   current leaf;
//! - `get_state` → `sessionFile` (the new session's path).
//!
//! The helper verifies the Cypher visible user prompts against the pi active
//! branch's user entries (exact text, the known `…\n\nUser request:\n<visible>`
//! augmented wrappers, or the pre-translation original recorded by the
//! translation extension) and REFUSES ambiguity/mismatch. Slash-command
//! prompts are extension input (`/goal`, `/subagents`, …) that pi rewrites or
//! consumes, so they never anchor the alignment. The one positional fallback
//! is a transcript with no slash commands whose prompt and user-entry counts
//! agree — a translated session from before the extension recorded originals.
//! The source session file is never written; the returned new session file is
//! verified non-empty and different from the source.
//!
//! Materialization contract: a fork BEFORE THE FIRST USER is empty-context —
//! real pi (0.84.1) returns a `sessionFile` that is NOT persisted until the
//! first user message lands, so the controller returns `session_path: None`
//! for that boundary instead of a bogus missing path. Every other boundary
//! (before a later user, or a leaf clone) copies a real prefix and pi MUST
//! have persisted a regular managed session file — a missing file there is a
//! loud error, never silently forwarded.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

use crate::HarnessError;
use crate::pi::PiHarness;
use crate::pi::client::{Incoming, PiClient};
use crate::{compose_child_path, shutdown_child};

/// The custom entry the Cypher translation extension appends when it rewrites
/// a prompt: `data: {original, translated}`. Pi stores only the translated
/// text as the user message, so this is the one place the prompt the user
/// actually typed (and Cypher shows) survives in the session.
const TRANSLATION_INPUT_ENTRY: &str = "cypher-translation-input";

/// One parsed `get_entries` message entry (only the fields the fork needs).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PiEntry {
    pub id: String,
    pub parent_id: Option<String>,
    /// `None` for non-message entries (compaction, model changes, …).
    pub role: Option<String>,
    /// The visible text of a user message (string or `text` content blocks
    /// joined), `None` for non-user messages.
    pub user_text: Option<String>,
    /// For a user message the translation extension rewrote: the text the
    /// user typed. Filled by [`active_branch_user_entries`].
    pub original_text: Option<String>,
    /// For a translation record entry: `(original, translated)`.
    pub translation: Option<(String, String)>,
}

/// Rebuild the ACTIVE branch (leaf → root) from `get_entries` append-order
/// data, filtering to the user messages on it. Abandoned branches are
/// deliberately ignored. Returns the user entries oldest → newest.
///
/// A translation record is paired with the first LATER user message whose
/// text is its translation. They are not necessarily adjacent: a prompt
/// queued while the agent streams is recorded when it is submitted but only
/// lands as a user message after the current turn.
pub(crate) fn active_branch_user_entries(
    entries: &[PiEntry],
    leaf_id: Option<&str>,
) -> Vec<PiEntry> {
    let by_id: HashMap<&str, &PiEntry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut branch: Vec<&PiEntry> = Vec::new();
    let mut current = leaf_id.and_then(|id| by_id.get(id).copied());
    while let Some(entry) = current {
        branch.push(entry);
        current = entry
            .parent_id
            .as_deref()
            .and_then(|pid| by_id.get(pid).copied());
    }
    branch.reverse();
    let mut pending: Vec<&(String, String)> = Vec::new();
    let mut users = Vec::new();
    for entry in branch {
        if let Some(record) = &entry.translation {
            pending.push(record);
            continue;
        }
        if entry.role.as_deref() != Some("user") {
            continue;
        }
        let Some(text) = entry.user_text.clone() else {
            continue;
        };
        let stored = strip_attachment_trailer(&text).trim();
        let original_text = pending
            .iter()
            .position(|(_, translated)| strip_attachment_trailer(translated).trim() == stored)
            .map(|i| pending.remove(i).0.clone());
        users.push(PiEntry {
            id: entry.id.clone(),
            role: Some("user".into()),
            user_text: Some(text),
            original_text,
            ..PiEntry::default()
        });
    }
    users
}

/// Strip the image-attachment trailer (`…\n\nAttached images (local files` …
/// `):` …) from a user message's text, returning the visible prompt. The
/// trailer rides cypher prompt text AND the pi session user entries; prompt
/// mapping compares the VISIBLE text, so both sides are normalized through
/// this. Mirrors the UI's `parseUserMessageImages` marker (case-insensitive
/// line start, `):` end).
pub fn strip_attachment_trailer(content: &str) -> &str {
    let lower = content.to_ascii_lowercase();
    let needle = "\n\nattached images (local files";
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find(needle) {
        let gap = from + rel;
        let line_start = gap + 2;
        let line_end = content[line_start..]
            .find('\n')
            .map(|p| line_start + p)
            .unwrap_or(content.len());
        let line = content[line_start..line_end].trim_end_matches('\r');
        if line.ends_with("):") {
            return content[..gap].trim_end();
        }
        from = line_start;
    }
    content
}

/// Does a pi user entry's text correspond to a Cypher visible prompt? Either
/// an exact match (both sides attachment-trailer-stripped) or a known
/// augmented wrapper (Comments / Side Chat context) ending in
/// `\n\nUser request:\n<visible>`.
pub(crate) fn matches_prompt(entry_text: &str, visible: &str) -> bool {
    let entry = strip_attachment_trailer(entry_text);
    let visible = strip_attachment_trailer(visible);
    if entry == visible {
        return true;
    }
    let wrapped = format!("\n\nUser request:\n{visible}");
    entry.ends_with(&wrapped)
}

/// A slash-command prompt is extension input: pi either consumes it
/// (`/subagents` leaves no user message) or rewrites it (`/goal` becomes a
/// goal-mode prompt), so its text says nothing about which entry it produced.
pub(crate) fn is_command_prompt(visible: &str) -> bool {
    visible.trim_start().starts_with('/')
}

/// Where one Cypher prompt lands in the pi active branch's user entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptEntry {
    /// Every complete alignment maps the prompt to this entry.
    Entry(usize),
    /// A slash command: it anchors nothing (see [`is_command_prompt`]).
    Command,
    /// Complete alignments disagree on this prompt's entry (repeated text,
    /// e.g. a prompt pi holds twice after a resend).
    Ambiguous,
}

/// Map the ordered Cypher visible USER prompts onto the pi active branch's
/// user entries by monotonic sequence alignment (prompt j → entry i_j with
/// i_0 < i_1 < …). Extra pi user entries are allowed (the alignment picks a
/// subset), and repeated prompt text is fine as long as the SEQUENCE pins it
/// (e.g. `["hi","hi"]` over `["hi","hi"]` → `[0,1]`).
///
/// A prompt maps to [`PromptEntry::Entry`] when EVERY complete alignment puts
/// it on the same entry — then a fork before it is right whichever alignment
/// is the true one — and to [`PromptEntry::Ambiguous`] otherwise. Ambiguity
/// is therefore only fatal for a fork AT that prompt, never for the whole
/// transcript. No complete alignment at all is a mismatch.
///
/// Slash-command prompts ([`is_command_prompt`]) take no part in the
/// alignment. When nothing aligns, a transcript without slash commands whose
/// prompt count equals the user-entry count maps position by position — each
/// prompt produced exactly one user message and only the text differs (a
/// translation from before the extension recorded originals). A prompt whose
/// text matches some OTHER entry but not its own position vetoes that
/// fallback. Returns one [`PromptEntry`] per prompt.
pub(crate) fn map_prompts_to_entries(
    active_users: &[PiEntry],
    visible_prompts: &[String],
) -> Result<Vec<PromptEntry>, HarnessError> {
    let mut mapped = vec![PromptEntry::Command; visible_prompts.len()];
    // The prompts that anchor the alignment, as indices into `visible_prompts`.
    let anchors: Vec<usize> = (0..visible_prompts.len())
        .filter(|&j| !is_command_prompt(&visible_prompts[j]))
        .collect();
    let n = anchors.len();
    let m = active_users.len();
    if n == 0 {
        return Ok(mapped);
    }
    let matches: Vec<Vec<bool>> = anchors
        .iter()
        .map(|&j| {
            (0..m)
                .map(|i| matches_entry(active_users, i, &visible_prompts[j]))
                .collect()
        })
        .collect();
    // reach_fwd[j][i]: anchors 0..=j align with anchor j on entry i.
    let mut reach_fwd = vec![vec![false; m]; n];
    for j in 0..n {
        let mut earlier = j == 0;
        for i in 0..m {
            reach_fwd[j][i] = matches[j][i] && earlier;
            if j > 0 && reach_fwd[j - 1][i] {
                earlier = true;
            }
        }
    }
    // reach_back[j][i]: anchors j..n align with anchor j on entry i.
    let mut reach_back = vec![vec![false; m]; n];
    for j in (0..n).rev() {
        let mut later = j == n - 1;
        for i in (0..m).rev() {
            reach_back[j][i] = matches[j][i] && later;
            if j + 1 < n && reach_back[j + 1][i] {
                later = true;
            }
        }
    }
    if !reach_fwd[n - 1].iter().any(|&r| r) {
        if let Some(positional) = positional_mapping(active_users, visible_prompts) {
            tracing::info!(
                prompts = visible_prompts.len(),
                "session fork mapping: prompt text differs from the pi user entries; \
                 mapping by position (equal counts, no slash commands)"
            );
            return Ok(positional);
        }
        return Err(HarnessError::Protocol(first_unmatched_detail(
            active_users,
            visible_prompts,
            &anchors,
        )));
    }
    for (j, &prompt) in anchors.iter().enumerate() {
        let mut feasible = (0..m).filter(|&i| reach_fwd[j][i] && reach_back[j][i]);
        mapped[prompt] = match (feasible.next(), feasible.next()) {
            (Some(i), None) => PromptEntry::Entry(i),
            _ => PromptEntry::Ambiguous,
        };
    }
    Ok(mapped)
}

/// The equal-count positional fallback described on
/// [`map_prompts_to_entries`]; `None` when it does not apply.
fn positional_mapping(
    active_users: &[PiEntry],
    visible_prompts: &[String],
) -> Option<Vec<PromptEntry>> {
    if visible_prompts.len() != active_users.len()
        || visible_prompts.iter().any(|p| is_command_prompt(p))
    {
        return None;
    }
    let consistent = visible_prompts.iter().enumerate().all(|(j, p)| {
        matches_entry(active_users, j, p)
            || !(0..active_users.len()).any(|i| matches_entry(active_users, i, p))
    });
    consistent.then(|| (0..visible_prompts.len()).map(PromptEntry::Entry).collect())
}

/// Name the first anchoring prompt the greedy (earliest-match) scan cannot
/// place — for a subsequence match that is exactly where every alignment
/// breaks, and the one detail that makes a refusal diagnosable.
fn first_unmatched_detail(
    active_users: &[PiEntry],
    visible_prompts: &[String],
    anchors: &[usize],
) -> String {
    let mut next = 0;
    for &j in anchors {
        match (next..active_users.len())
            .find(|&i| matches_entry(active_users, i, &visible_prompts[j]))
        {
            Some(i) => next = i + 1,
            None => {
                let preview: String = visible_prompts[j].chars().take(60).collect();
                return format!(
                    "session fork mapping: user message #{} ({preview:?}) has no \
                     matching user entry in the Pi session ({} Cypher prompts, {} \
                     Pi user entries)",
                    j + 1,
                    visible_prompts.len(),
                    active_users.len()
                );
            }
        }
    }
    "session fork mapping: no alignment maps the visible prompts onto the Pi session".into()
}

fn matches_entry(active_users: &[PiEntry], i: usize, prompt: &str) -> bool {
    let entry = &active_users[i];
    [&entry.user_text, &entry.original_text]
        .into_iter()
        .flatten()
        .any(|t| matches_prompt(t, prompt))
}

/// Parse the `get_entries` response `data` into raw entries + leaf id.
fn parse_entries_response(data: &Value) -> (Vec<PiEntry>, Option<String>) {
    let entries = data
        .get("entries")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(parse_entry).collect::<Vec<PiEntry>>())
        .unwrap_or_default();
    let leaf = data
        .get("leafId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    (entries, leaf)
}

/// Parse one entry object; non-message entries yield `role: None`.
fn parse_entry(v: &Value) -> Option<PiEntry> {
    let id = v.get("id").and_then(Value::as_str)?.to_string();
    let parent_id = v.get("parentId").and_then(Value::as_str).map(str::to_owned);
    match v.get("type").and_then(Value::as_str) {
        Some("message") => {}
        Some("custom")
            if v.get("customType").and_then(Value::as_str) == Some(TRANSLATION_INPUT_ENTRY) =>
        {
            let field = |name: &str| {
                v.get("data")
                    .and_then(|d| d.get(name))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            return Some(PiEntry {
                id,
                parent_id,
                translation: field("original").zip(field("translated")),
                ..PiEntry::default()
            });
        }
        _ => {
            return Some(PiEntry {
                id,
                parent_id,
                ..PiEntry::default()
            });
        }
    }
    let message = v.get("message")?;
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let user_text = if role.as_deref() == Some("user") {
        message_text(message.get("content"))
    } else {
        None
    };
    Some(PiEntry {
        id,
        parent_id,
        role,
        user_text,
        ..PiEntry::default()
    })
}

/// Extract the visible text from pi user content: a bare string or `text`
/// content blocks joined with a double newline (mirrors pi's `contentText`
/// semantics closely enough for prompt mapping).
fn message_text(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(blocks)) => {
            let texts: Vec<String> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_owned))
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n\n"))
            }
        }
        _ => None,
    }
}

impl PiHarness {
    /// Session Fork v1 (Pi-only): materialize a NEW persisted pi session for
    /// the requested boundary on the source session, without touching the
    /// source session file or any live client. Returns the new session path
    /// — `None` for an EMPTY-CONTEXT fork before the first user (pi does not
    /// persist that file until the first user message lands).
    ///
    /// The SOURCE session is never handed to pi: it is validated as a regular
    /// file under the managed `session_dir`, byte-snapshotted into a unique
    /// scratch JSONL (0600), and `switch_session` loads the SCRATCH — so even
    /// if pi migrates/rebinds sessions the source bytes are untouched. The
    /// scratch is removed on every path once the helper is torn down.
    ///
    /// The helper process is the same `pi --mode rpc --session-dir` shape as
    /// a run child, plus `--no-extensions` so `session_before_fork` hooks
    /// cannot cancel or mutate, and is torn down when the operation finishes.
    pub async fn fork_session(
        &self,
        request: cypher_proto::PiSessionForkRequest,
    ) -> Result<cypher_proto::PiSessionForkResult, HarnessError> {
        // (0) Validate the source + take a stable byte snapshot into a unique
        // managed scratch file. Outside/missing sources are rejected here.
        let (source, scratch) = self.snapshot_source(&request.source_session_path)?;
        let result = async {
            let (mut child, client, incoming) = self.spawn_fork_helper().await?;
            let outcome = async {
                // (1) Load the SCRATCH copy — never the source file.
                let mut switch = Map::new();
                switch.insert(
                    "sessionPath".into(),
                    Value::String(scratch.to_string_lossy().into_owned()),
                );
                let switched = client.request("switch_session", switch).await?;
                if switched
                    .get("cancelled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return Err(HarnessError::Protocol(
                        "session fork: switch_session was cancelled".into(),
                    ));
                }

                // (2) Read the session entries + active leaf.
                let entries_resp = client.request("get_entries", Map::new()).await?;
                let (raw_entries, leaf_id) = parse_entries_response(&entries_resp);
                let active_users = active_branch_user_entries(&raw_entries, leaf_id.as_deref());
                let mapping = map_prompts_to_entries(&active_users, &request.visible_user_prompts)?;

                // (3) Execute the boundary: fork before the mapped user entry
                // or clone at the current leaf.
                let entry_id = match request.boundary {
                    cypher_proto::PiForkBoundary::CloneLeaf => {
                        // Extensions append user messages of their own
                        // (`/goal` continuations), so pi may legitimately hold
                        // user entries past the last Cypher prompt. The race
                        // this once guarded — a prompt sent while the helper
                        // ran — is caught by the engine, which rechecks the
                        // transcript (Cypher writes a prompt there BEFORE
                        // dispatching it to pi) before publishing the fork.
                        // The mapping above still had to succeed: it is what
                        // proves this session belongs to this transcript.
                        let resp = client.request("clone", Map::new()).await?;
                        if resp
                            .get("cancelled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            return Err(HarnessError::Protocol(
                                "session fork: clone was cancelled".into(),
                            ));
                        }
                        None
                    }
                    cypher_proto::PiForkBoundary::BeforeUser(index) => {
                        let entry = match mapping.get(index) {
                            Some(PromptEntry::Entry(i)) => active_users.get(*i),
                            Some(PromptEntry::Command) => {
                                return Err(HarnessError::Protocol(
                                    "session fork: that message is a slash command, which \
                                     has no user entry of its own in the Pi session — \
                                     fork or restart from a neighbouring message instead"
                                        .into(),
                                ));
                            }
                            Some(PromptEntry::Ambiguous) => {
                                return Err(HarnessError::Protocol(
                                    "session fork mapping: ambiguous — the Pi session holds \
                                     that message more than once and the transcript cannot \
                                     tell which one it is; refusing a positional guess"
                                        .into(),
                                ));
                            }
                            None => None,
                        };
                        let Some(entry) = entry else {
                            return Err(HarnessError::Protocol(format!(
                                "session fork: boundary user index {index} out of range"
                            )));
                        };
                        let mut params = Map::new();
                        params.insert("entryId".into(), Value::String(entry.id.clone()));
                        let resp = client.request("fork", params).await?;
                        if resp
                            .get("cancelled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            return Err(HarnessError::Protocol(
                                "session fork: fork was cancelled".into(),
                            ));
                        }
                        Some(entry.id.clone())
                    }
                };

                // (4) The NEW session's file: pi switches the active session
                // to the fork/clone, so `get_state.sessionFile` names it.
                let state = client.request("get_state", Map::new()).await?;
                let session_file = state
                    .get("sessionFile")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        HarnessError::Protocol(
                            "session fork: get_state returned no sessionFile".into(),
                        )
                    })?;

                // (5) Verify the new session is inside the managed root and
                // differs from the source + scratch, then enforce the
                // materialization contract:
                // - a fork BEFORE THE FIRST USER is empty-context — real pi
                //   does not persist the new session file until the first
                //   user message lands, so a NON-EXISTENT (but managed)
                //   path returns `session_path: None`; if pi DID materialize
                //   it, the path is returned as-is.
                // - every other boundary (BeforeUser(n>0), CloneLeaf) copies
                //   a real prefix, so pi MUST have persisted a REGULAR
                //   managed session file — a missing path is a loud error,
                //   never silently forwarded to a `switch_session` that
                //   would fail on first send.
                let session_path = self.verify_new_session(&session_file, &source, &scratch)?;
                let session_path = match request.boundary {
                    cypher_proto::PiForkBoundary::BeforeUser(0)
                        if !Path::new(&session_path).exists() =>
                    {
                        None
                    }
                    _ => {
                        let path = Path::new(&session_path);
                        if !path.is_file() {
                            return Err(HarnessError::Protocol(format!(
                                "session fork: the forked session file was not \
                                 materialized as a regular managed file: {}",
                                path.display()
                            )));
                        }
                        Some(session_path)
                    }
                };
                let _ = entry_id;
                Ok::<cypher_proto::PiSessionForkResult, HarnessError>(
                    cypher_proto::PiSessionForkResult { session_path },
                )
            };
            let result = tokio::time::timeout(Duration::from_secs(30), outcome).await;
            // Keep `incoming` alive until the child is reaped: the reader task
            // keeps draining stdout so the pipe can never backpressure the
            // helper into a deadlock (events are unlikely, but a large reply
            // would otherwise stall the teardown).
            drop(incoming);
            shutdown_child(&mut child, self.kill_grace).await;
            match result {
                Ok(inner) => inner,
                Err(_) => Err(HarnessError::Protocol(
                    "session fork helper timed out".into(),
                )),
            }
        }
        .await;
        // Clean up the scratch on EVERY path (the fork's own session file is
        // separate and persists).
        if let Err(err) = std::fs::remove_file(&scratch) {
            tracing::debug!(
                scratch = %scratch.display(),
                error = %err,
                "session fork scratch cleanup"
            );
        }
        result
    }

    /// Validate the source session and byte-snapshot it into a unique scratch
    /// JSONL under the managed session dir; returns `(canonical_source, scratch)`.
    ///
    /// The source must be a REGULAR file under the canonicalized managed root
    /// (`session_dir`) — outside/missing paths are rejected. The snapshot is
    /// written with 0600 perms, then the source is re-read and compared: a
    /// live writer changing the bytes triggers a bounded retry, then an error
    /// (the snapshot must be stable before the helper loads it).
    fn snapshot_source(&self, source_path: &str) -> Result<(PathBuf, PathBuf), HarnessError> {
        std::fs::create_dir_all(&self.session_dir)?;
        let root = std::fs::canonicalize(&self.session_dir).map_err(|e| {
            HarnessError::Protocol(format!(
                "session fork: managed session root unavailable: {e}"
            ))
        })?;
        let source = std::fs::canonicalize(PathBuf::from(source_path)).map_err(|e| {
            HarnessError::Protocol(format!(
                "session fork: source session unavailable (missing, unreadable, \
                 or outside the managed session root): {e}"
            ))
        })?;
        if !source.is_file() {
            return Err(HarnessError::Protocol(
                "session fork: source session is not a regular file".into(),
            ));
        }
        if !source.starts_with(&root) {
            return Err(HarnessError::Protocol(format!(
                "session fork: source session outside managed session root: {}",
                source.display()
            )));
        }
        // Stable byte snapshot: read → write scratch → re-read/compare. A
        // mismatch means the source is being written concurrently. Any error
        // after the scratch is created cleans it up (no orphan on this path).
        let mut attempt = 0;
        loop {
            let before = std::fs::read(&source)?;
            let scratch = root.join(format!(".fork-{}.jsonl", uuid::Uuid::new_v4()));
            if let Err(err) = std::fs::write(&scratch, &before) {
                let _ = std::fs::remove_file(&scratch);
                return Err(HarnessError::Io(err));
            }
            if let Err(err) = set_scratch_perms(&scratch) {
                let _ = std::fs::remove_file(&scratch);
                return Err(HarnessError::Io(err));
            }
            let after = std::fs::read(&source)?;
            if before == after {
                return Ok((source.clone(), scratch));
            }
            let _ = std::fs::remove_file(&scratch);
            attempt += 1;
            if attempt >= 3 {
                return Err(HarnessError::Protocol(
                    "session fork: source session kept changing while snapshotting".into(),
                ));
            }
        }
    }

    /// Verify a returned fork session path: canonicalize it (resolving `..`
    /// and symlinks against the deepest EXISTING ancestor — the new file may
    /// not exist yet), require it inside the managed root and different from
    /// the SOURCE and the scratch copy.
    fn verify_new_session(
        &self,
        session_file: &str,
        source: &Path,
        scratch: &Path,
    ) -> Result<String, HarnessError> {
        let root = std::fs::canonicalize(&self.session_dir).map_err(|e| {
            HarnessError::Protocol(format!(
                "session fork: managed session root unavailable: {e}"
            ))
        })?;
        let new_path = canonicalize_for_managed(PathBuf::from(session_file), &root)?;
        if new_path == scratch || new_path == source {
            return Err(HarnessError::Protocol(
                "session fork: new session file equals the source or scratch \
                 snapshot (the fork did not create a new session)"
                    .into(),
            ));
        }
        Ok(new_path.to_string_lossy().into_owned())
    }

    /// Spawn the fork helper: a fresh `pi --mode rpc --session-dir` child with
    /// `--no-extensions`, returning the child + client + incoming channel.
    /// The helper's stderr is drained by a [`crate::StderrTail`] reader task
    /// (same pattern as run children) so pipe backpressure can never deadlock
    /// the operation.
    async fn spawn_fork_helper(
        &self,
    ) -> Result<
        (
            tokio::process::Child,
            PiClient,
            tokio::sync::mpsc::Receiver<Incoming>,
        ),
        HarnessError,
    > {
        let (exe, mut args) = self.resolve_program()?;
        args.push("--no-extensions".into());
        let mut cmd = Command::new(&exe);
        cmd.args(args);
        compose_child_path(&mut cmd, &exe);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(exe.to_string_lossy().into_owned())
            } else {
                HarnessError::Io(e)
            }
        })?;
        if let Some(stderr) = child.stderr.take() {
            let tail = crate::StderrTail::default();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "cypher_harness::pi", "fork stderr: {line}");
                    tail.push(&line);
                }
            });
        }
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Protocol("fork helper has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("fork helper has no stdout".into()))?;
        let (client, incoming) = PiClient::new(stdin, stdout);
        Ok((child, client, incoming))
    }
}

/// Canonicalize a path even when the final file does not exist yet: the
/// deepest EXISTING ancestor is canonicalized (resolving symlinks and `..`),
/// then the remaining components are appended lexically. Returns `Err` if the
/// result escapes `root` (via `..` or a symlinked ancestor).
fn canonicalize_for_managed(path: PathBuf, root: &Path) -> Result<PathBuf, HarnessError> {
    let mut existing = path;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name().map(std::ffi::OsStr::to_os_string) else {
            break;
        };
        tail.push(name);
        if !existing.pop() {
            break;
        }
    }
    let mut base = existing.canonicalize().map_err(|e| {
        HarnessError::Protocol(format!("session fork: cannot resolve session path: {e}"))
    })?;
    for component in tail.iter().rev() {
        if component == ".." {
            return Err(HarnessError::Protocol(
                "session fork: session path escapes via `..`".into(),
            ));
        }
        base.push(component);
    }
    if !base.starts_with(root) {
        return Err(HarnessError::Protocol(format!(
            "session fork: returned session path outside managed session root: {}",
            base.display()
        )));
    }
    Ok(base)
}

/// 0600 perms for the scratch snapshot (session bytes are private).
#[cfg(unix)]
fn set_scratch_perms(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}
#[cfg(not(unix))]
fn set_scratch_perms(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::PromptEntry::{Ambiguous, Command, Entry};
    use super::*;
    use cypher_proto::{PiForkBoundary, PiSessionForkRequest};

    fn user(id: &str, parent: Option<&str>, text: &str) -> PiEntry {
        PiEntry {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            role: Some("user".into()),
            user_text: Some(text.into()),
            ..PiEntry::default()
        }
    }

    fn msg(id: &str, parent: Option<&str>) -> PiEntry {
        PiEntry {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            role: Some("assistant".into()),
            ..PiEntry::default()
        }
    }

    fn non_message(id: &str, parent: Option<&str>) -> PiEntry {
        PiEntry {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            ..PiEntry::default()
        }
    }

    fn translation(id: &str, parent: Option<&str>, original: &str, translated: &str) -> PiEntry {
        PiEntry {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            translation: Some((original.into(), translated.into())),
            ..PiEntry::default()
        }
    }

    fn prompts(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn active_branch_ignores_abandoned_branches() {
        // Append order: u1 -> a1 -> u2a (abandoned) -> u2 -> a2.
        // leaf = a2, so the active branch is u1,a1,u2,a2 — u2a is skipped.
        let entries = vec![
            user("u1", None, "first"),
            msg("a1", Some("u1")),
            user("u2a", Some("a1"), "abandoned"),
            user("u2", Some("a1"), "second"),
            msg("a2", Some("u2")),
        ];
        let active = active_branch_user_entries(&entries, Some("a2"));
        assert_eq!(
            active.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["u1", "u2"]
        );
        // A compacted-away ancestor (missing from the map) just stops the walk.
        let dangling = active_branch_user_entries(&entries, Some("ghost"));
        assert!(dangling.is_empty());
    }

    #[test]
    fn active_branch_handles_non_message_entries() {
        let entries = vec![
            user("u1", None, "first"),
            non_message("c1", Some("u1")),
            msg("a1", Some("c1")),
            user("u2", Some("a1"), "second"),
        ];
        let active = active_branch_user_entries(&entries, Some("u2"));
        assert_eq!(
            active.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["u1", "u2"]
        );
    }

    #[test]
    fn matches_exact_and_wrapped_prompts() {
        assert!(matches_prompt("fix the build", "fix the build"));
        assert!(matches_prompt(
            "Conversation annotations (JSON): ...\n\nUser request:\nfix the build",
            "fix the build"
        ));
        assert!(!matches_prompt("fix the build, please", "fix the build"));
        // A wrapper WITHOUT the trailing prompt must not match.
        assert!(!matches_prompt(
            "x\n\nUser request:\nother",
            "fix the build"
        ));
    }

    #[test]
    fn matches_strip_attachment_trailers_on_both_sides() {
        // pi stores the prompt WITH the image trailer; cypher strips it.
        assert!(matches_prompt(
            "fix it\n\nAttached images (local files — open them to view):\n- /a.png",
            "fix it"
        ));
        // Augmented + trailer.
        assert!(matches_prompt(
            "ctx\n\nUser request:\nfix it\n\nAttached images (local files):\n- /a.png",
            "fix it"
        ));
        assert!(!matches_prompt(
            "fix it\n\nAttached images (local files — open them to view):\n- /a.png",
            "fix them"
        ));
    }

    #[test]
    fn strip_attachment_trailer_leaves_plain_text_untouched() {
        assert_eq!(strip_attachment_trailer("hello world"), "hello world");
        assert_eq!(
            strip_attachment_trailer(
                "hello\n\nAttached images (local files — open them to view):\n- /a.png"
            ),
            "hello"
        );
        // The marker requires a real `):` line end — no false trim.
        assert_eq!(
            strip_attachment_trailer("hello\n\nAttached images (local files: none"),
            "hello\n\nAttached images (local files: none"
        );
    }

    #[test]
    fn mapping_is_monotonic_and_refuses_ambiguity() {
        let users = vec![user("a", None, "one"), user("b", Some("a"), "two")];
        let mapped =
            map_prompts_to_entries(&users, &["one".to_string(), "two".to_string()]).unwrap();
        assert_eq!(mapped, vec![Entry(0), Entry(1)]);

        // Out-of-order prompts are a mismatch (nothing after "two" matches "one").
        let err =
            map_prompts_to_entries(&users, &["two".to_string(), "one".to_string()]).unwrap_err();
        assert!(err.to_string().contains("no matching user entry"), "{err}");

        // A SINGLE prompt against two identical entries is genuinely ambiguous.
        let dup = vec![user("a", None, "hi"), user("b", Some("a"), "hi")];
        let mapped = map_prompts_to_entries(&dup, &["hi".to_string()]).unwrap();
        assert_eq!(mapped, vec![Ambiguous]);

        // Missing prompt = mismatch.
        let err = map_prompts_to_entries(&users, &["nope".to_string()]).unwrap_err();
        assert!(err.to_string().contains("no matching user entry"), "{err}");
    }

    #[test]
    fn mapping_resolves_repeated_prompts_unique_by_sequence() {
        // Two identical prompts over two identical entries have EXACTLY one
        // complete monotonic alignment [0,1] — the greedy per-prompt candidate
        // scan would call this ambiguous, the global alignment resolves it.
        let users = vec![user("a", None, "hi"), user("b", Some("a"), "hi")];
        let mapped = map_prompts_to_entries(&users, &["hi".to_string(), "hi".to_string()])
            .expect("sequence pins a unique alignment");
        assert_eq!(mapped, vec![Entry(0), Entry(1)]);

        // Three identical entries, two identical prompts: [0,1], [0,2], [1,2]
        // — both prompts are genuinely ambiguous.
        let three = vec![
            user("a", None, "hi"),
            user("b", Some("a"), "hi"),
            user("c", Some("b"), "hi"),
        ];
        let mapped = map_prompts_to_entries(&three, &["hi".to_string(), "hi".to_string()])
            .expect("ambiguity is per prompt, not a failure");
        assert_eq!(mapped, vec![Ambiguous, Ambiguous]);

        // Two identical prompts over three entries with a non-matching middle:
        // the only full alignment is [0,2].
        let gapped = vec![
            user("a", None, "hi"),
            user("b", Some("a"), "other"),
            user("c", Some("b"), "hi"),
        ];
        let mapped = map_prompts_to_entries(&gapped, &["hi".to_string(), "hi".to_string()])
            .expect("gap pins the alignment");
        assert_eq!(mapped, vec![Entry(0), Entry(2)]);
    }

    #[test]
    fn ambiguity_stays_local_to_the_repeated_prompt() {
        // Pi holds "commit" twice (a resend); Cypher shows it once. Only that
        // prompt is ambiguous — the prompts around it still map exactly.
        let users = vec![
            user("a", None, "update packages"),
            user("b", Some("a"), "commit"),
            user("c", Some("b"), "commit"),
            user("d", Some("c"), "publish"),
        ];
        let mapped =
            map_prompts_to_entries(&users, &prompts(&["update packages", "commit", "publish"]))
                .unwrap();
        assert_eq!(mapped, vec![Entry(0), Ambiguous, Entry(3)]);
    }

    #[test]
    fn mapping_extra_pi_entries_remain_allowed() {
        // More pi user entries than Cypher prompts is fine: the alignment
        // skips the extras (compactions or prompts the snapshot predates).
        let users = vec![
            user("a", None, "one"),
            user("x", Some("a"), "(system note)"),
            user("b", Some("x"), "two"),
            user("c", Some("b"), "three"),
        ];
        let mapped = map_prompts_to_entries(&users, &["one".to_string(), "two".to_string()])
            .expect("extras are skipped");
        assert_eq!(mapped, vec![Entry(0), Entry(2)]);
    }

    #[test]
    fn mapping_accepts_wrapped_prompts() {
        let users = vec![
            user(
                "a",
                None,
                "Selected text:\nfoo\n\nParent chat context:\nbar\n\nUser request:\nvisible",
            ),
            user("b", Some("a"), "plain second"),
        ];
        let mapped =
            map_prompts_to_entries(&users, &["visible".to_string(), "plain second".to_string()])
                .unwrap();
        assert_eq!(mapped, vec![Entry(0), Entry(1)]);
    }

    #[test]
    fn translation_records_restore_the_typed_prompt() {
        // A queued prompt: recorded when submitted, landing as a user message
        // only after the running turn's assistant reply.
        let entries = vec![
            user("u1", None, "first"),
            translation("t2", Some("u1"), "发布一个新版本", "Release a new version"),
            msg("a1", Some("t2")),
            user("u2", Some("a1"), "Release a new version"),
        ];
        let active = active_branch_user_entries(&entries, Some("u2"));
        assert_eq!(active[0].original_text, None);
        assert_eq!(active[1].original_text.as_deref(), Some("发布一个新版本"));
        let mapped = map_prompts_to_entries(&active, &prompts(&["first", "发布一个新版本"]))
            .expect("the recorded original maps");
        assert_eq!(mapped, vec![Entry(0), Entry(1)]);

        // A record whose translation never landed attaches to nothing.
        let entries = vec![
            translation("t1", None, "原文", "Original"),
            user("u1", Some("t1"), "something else"),
        ];
        let active = active_branch_user_entries(&entries, Some("u1"));
        assert_eq!(active[0].original_text, None);
    }

    #[test]
    fn slash_commands_never_anchor_the_alignment() {
        // `/subagents` leaves no user entry; `/goal` is rewritten and followed
        // by injected continuations.
        let users = vec![
            user("a", None, "one"),
            user(
                "g",
                Some("a"),
                "Goal mode is active. <goal_objective>\nship\n</goal_objective>",
            ),
            user(
                "c",
                Some("g"),
                "Continue the active /goal until it is complete",
            ),
            user("b", Some("c"), "two"),
            user(
                "d",
                Some("b"),
                "Continue the active /goal until it is complete",
            ),
        ];
        let mapped = map_prompts_to_entries(
            &users,
            &prompts(&["one", "/subagents", "/goal ship", "two"]),
        )
        .expect("commands are skipped");
        assert_eq!(mapped, vec![Entry(0), Command, Command, Entry(3)]);

        // A transcript of nothing but commands maps nothing (and succeeds).
        let mapped = map_prompts_to_entries(&users, &prompts(&["/subagents"])).unwrap();
        assert_eq!(mapped, vec![Command]);
    }

    #[test]
    fn untranslated_legacy_sessions_map_by_position_when_counts_agree() {
        let users = vec![
            user("a", None, "Is Redis the best choice?"),
            user("b", Some("a"), "ok"),
            user("c", Some("b"), "Release a new build"),
        ];
        let mapped =
            map_prompts_to_entries(&users, &prompts(&["是 redis 最好吗？", "ok", "发布新版本"]))
                .expect("equal counts map by position");
        assert_eq!(mapped, vec![Entry(0), Entry(1), Entry(2)]);

        // A count mismatch refuses, naming the first prompt that did not map.
        let err =
            map_prompts_to_entries(&users, &prompts(&["是 redis 最好吗？", "ok"])).unwrap_err();
        assert!(err.to_string().contains("user message #1"), "{err}");

        // A slash command disables the fallback: it may have produced zero
        // or several entries, so positions no longer line up.
        let err = map_prompts_to_entries(
            &users,
            &prompts(&["是 redis 最好吗？", "/goal x", "发布新版本"]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no matching user entry"), "{err}");

        // A prompt that matches a DIFFERENT position vetoes the fallback.
        let err =
            map_prompts_to_entries(&users, &prompts(&["是 redis 最好吗？", "发布新版本", "ok"]))
                .unwrap_err();
        assert!(err.to_string().contains("no matching user entry"), "{err}");
    }

    #[test]
    fn parse_entry_reads_translation_records() {
        let v: Value = serde_json::json!({
            "type": "custom",
            "customType": "cypher-translation-input",
            "id": "t1",
            "parentId": "p0",
            "data": {"original": "你好", "translated": "Hello"}
        });
        let entry = parse_entry(&v).unwrap();
        assert_eq!(entry.translation, Some(("你好".into(), "Hello".into())));
        assert_eq!(entry.role, None);

        let v: Value = serde_json::json!({
            "type": "custom", "customType": "goal-state", "id": "g1", "data": {}
        });
        assert_eq!(parse_entry(&v).unwrap().translation, None);
    }

    #[test]
    fn parse_entry_extracts_user_text_and_roles() {
        let v: Value = serde_json::json!({
            "type": "message",
            "id": "m1",
            "parentId": "p0",
            "timestamp": "t",
            "message": {"role": "user", "content": "plain text"}
        });
        let entry = parse_entry(&v).unwrap();
        assert_eq!(entry.role.as_deref(), Some("user"));
        assert_eq!(entry.user_text.as_deref(), Some("plain text"));

        let v: Value = serde_json::json!({
            "type": "message",
            "id": "m2",
            "parentId": "m1",
            "message": {"role": "user", "content": [
                {"type": "text", "text": "hello"},
                {"type": "image", "source": {}},
                {"type": "text", "text": "world"}
            ]}
        });
        let entry = parse_entry(&v).unwrap();
        assert_eq!(entry.user_text.as_deref(), Some("hello\n\nworld"));

        let v: Value = serde_json::json!({"type": "compaction", "id": "c1", "parentId": "m2"});
        let entry = parse_entry(&v).unwrap();
        assert_eq!(entry.role, None);
        assert_eq!(entry.user_text, None);
    }

    #[test]
    fn fork_request_builds_boundary_mapping() {
        // The controller-level index is into the CYPHER prompt list; the pure
        // mapping validates it. Exercise the request type end-to-end here.
        let req = PiSessionForkRequest {
            source_session_path: "/s/a.jsonl".into(),
            visible_user_prompts: vec!["one".into(), "two".into()],
            boundary: PiForkBoundary::BeforeUser(1),
        };
        let users = vec![user("a", None, "one"), user("b", Some("a"), "two")];
        let mapping = map_prompts_to_entries(&users, &req.visible_user_prompts).unwrap();
        assert_eq!(mapping[1], Entry(1));
    }
}
