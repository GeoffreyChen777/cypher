//! Session Fork (v1) engine service.
//!
//! A Session Fork clones a settled transcript prefix into a NEW durable root
//! Chat (id = the client-minted `requestId`) backed by a fresh Pi session.
//! The source chat, its transcript, and its pi session file are never
//! mutated — the pi side is materialized by a SEPARATE `--no-extensions`
//! helper process (`PiHarness::fork_session`), never the source chat's live
//! client.
//!
//! Authoritative order (round-21 promotion discipline, reused from Side
//! Chats): the target doc is opened EPHEMERAL and populated, then
//! `prepare_promotion` persists the snapshot FIRST, the fork Chat row lands,
//! then `finish_promotion` flips the handle + joins chat2. Any failure
//! before the row is published purges the ephemeral target and best-effort
//! deletes ONLY the newly returned pi session file (guarded to the managed
//! session root — the source file is never touched). A fork BEFORE THE FIRST
//! USER is EMPTY-CONTEXT: real pi returns no persisted session file yet, so
//! the target row is born with `harness_session_id: None` and starts its own
//! fresh session on first send. Retries with the same `requestId` return the
//! existing target chat instead of minting a twin.
//!
//! Pi-only in v1. Child chats, temporary Side Chats, live (Working /
//! AwaitingInput) runs, non-Pi configs, missing hosts/sessions, and
//! unrepresentable boundaries answer a typed [`SessionForkUnavailable`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use cypher_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use cypher_harness::{Harness, HarnessError};
use cypher_proto::{
    Chat, PiForkBoundary, PiSessionForkRequest, PiSessionForkResult, SessionForkCreated,
    SessionForkMode, SessionForkRequest, SessionForkResponse, SessionForkUnavailable,
    SessionForkUnavailableReason, SessionRewindRequest, SessionRewindResponse, SessionRewound,
    SessionStatus,
};
use tokio::sync::Mutex as AsyncMutex;

use crate::EngineError;
use crate::doc_host::DocHost;
use crate::registry::HarnessRegistry;
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

/// Title bound for `<source title> — Fork` (chars).
const MAX_FORK_TITLE_CHARS: usize = 120;
/// Request id (client-minted target chat id) length bound.
const MAX_REQUEST_ID_CHARS: usize = 256;

/// The pi fork backend seam: production uses the registry's Pi harness; tests
/// inject a fake that answers without spawning a helper process.
#[async_trait]
pub trait PiForkBackend: Send + Sync {
    async fn fork_session(
        &self,
        request: PiSessionForkRequest,
    ) -> Result<PiSessionForkResult, HarnessError>;
}

/// The production backend: the registry-resolved Pi harness (its native
/// fork controller). `None` when the Pi harness cannot be resolved on this
/// device — the backend then answers Unsupported honestly.
struct RegistryForkBackend {
    harness: Option<Arc<dyn Harness>>,
}

#[async_trait]
impl PiForkBackend for RegistryForkBackend {
    async fn fork_session(
        &self,
        request: PiSessionForkRequest,
    ) -> Result<PiSessionForkResult, HarnessError> {
        match &self.harness {
            Some(harness) => harness.fork_session(request).await,
            None => Err(HarnessError::Protocol(
                "pi harness unavailable on this device".into(),
            )),
        }
    }
}

struct Inner {
    sessions: SessionsEngine,
    doc_host: DocHost,
    workspace: WorkspaceHost,
    pi_sessions_root: PathBuf,
    backend: Arc<dyn PiForkBackend>,
    /// Serializes the target-doc creation + promotion mutation so concurrent
    /// forks (any source) can never race the read-then-create target check
    /// and mint twins or double-populate a target doc. The slow pi helper
    /// call runs OUTSIDE this lock; only the ephemeral-create → row-publish
    /// section holds it.
    mutation_lock: AsyncMutex<()>,
}

/// Session Fork v1 manager.
#[derive(Clone)]
pub struct SessionForks {
    inner: Arc<Inner>,
}

impl SessionForks {
    /// Assemble against the registry's Pi harness. `pi_sessions_root` is the
    /// cypher-owned pi session store (`<profile store>/agent-sessions`) —
    /// the guard for best-effort deletion of a freshly returned fork session
    /// file on a failed promotion (the SOURCE file is never deleted).
    pub fn new(
        sessions: SessionsEngine,
        doc_host: DocHost,
        workspace: WorkspaceHost,
        registry: Arc<HarnessRegistry>,
        pi_sessions_root: PathBuf,
    ) -> Self {
        let harness = registry.resolve(cypher_proto::HarnessId::Pi).ok();
        Self {
            inner: Arc::new(Inner {
                sessions,
                doc_host,
                workspace,
                pi_sessions_root,
                backend: Arc::new(RegistryForkBackend { harness }),
                mutation_lock: AsyncMutex::new(()),
            }),
        }
    }

    /// Test seam: assemble against an injected fork backend.
    pub fn with_backend(
        sessions: SessionsEngine,
        doc_host: DocHost,
        workspace: WorkspaceHost,
        pi_sessions_root: PathBuf,
        backend: Arc<dyn PiForkBackend>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                sessions,
                doc_host,
                workspace,
                pi_sessions_root,
                backend,
                mutation_lock: AsyncMutex::new(()),
            }),
        }
    }

    /// `ForkSession` handler. Returns a typed [`SessionForkResponse`]; only
    /// genuinely unexpected failures surface as `EngineError`.
    pub async fn fork(
        &self,
        request: SessionForkRequest,
    ) -> Result<SessionForkResponse, EngineError> {
        if request.request_id.trim().is_empty()
            || request.request_id.chars().count() > MAX_REQUEST_ID_CHARS
        {
            return Ok(unavailable(
                SessionForkUnavailableReason::BoundaryUnavailable,
                "Invalid fork request id.",
            ));
        }
        // The client-minted request id must never collide with the source chat
        // itself (the target would shadow the source row).
        if request.request_id == request.source_chat_id {
            return Ok(unavailable(
                SessionForkUnavailableReason::BoundaryUnavailable,
                "The fork request id cannot equal the source chat id.",
            ));
        }
        let source = match self.validate_source(&request.source_chat_id, Operation::Fork)? {
            Ok(chat) => chat,
            Err(unavail) => return Ok(SessionForkResponse::Unavailable(unavail)),
        };

        // Read the authoritative joined source transcript + resolve the
        // boundary + build the visible user prompts (attachment trailers
        // stripped — the pi session stores the full prompt, so the harness
        // maps stripped-to-stripped). Computed BEFORE the live check so an
        // idempotent retry can rebuild the reply from the transcript alone.
        let source_handle = self
            .inner
            .doc_host
            .open(&source.id)
            .map_err(|_| EngineError::Other("source chat doc unavailable".into()))?;
        let raw = source_handle.doc().read_entries()?;
        let joined = cypher_doc::join_continuation_entries(raw.clone());
        let plan = match compute_boundary(&joined, &request.anchor_message_id) {
            Ok(plan) => plan,
            Err(reason) => return Ok(unavailable(reason, boundary_message(reason))),
        };
        let visible_prompts: Vec<String> = joined
            .iter()
            .filter(|e| e.role == MessageRole::User)
            .map(visible_text_of)
            .collect();

        // Idempotence FIRST: a target chat for this request id already exists
        // (a lost-reply retry). Return it WITHOUT quiescing the source or
        // re-running the pi helper — even when the source is now running. The
        // mutation-lock recheck below still guards the concurrent race.
        if let Some(existing) = self.inner.workspace.chat(&request.request_id)? {
            if let Err(unavail) = self.validate_existing_target(&existing, &source, &plan) {
                return Ok(unavail);
            }
            return Ok(existing_response(
                existing,
                plan.mode,
                plan.composer_text.clone(),
            ));
        }

        // Quiesce a parked (Idle) Pi run before the helper touches the source
        // session — clean path (never an aborted stamp), bounded wait.
        match self
            .inner
            .sessions
            .session_status(&source.id)
            .map(|s| s.status)
        {
            Some(SessionStatus::Working) | Some(SessionStatus::AwaitingInput) => {
                return Ok(unavailable(
                    SessionForkUnavailableReason::LiveSession,
                    "The source chat is still running. Fork it once the current \
                     turn settles.",
                ));
            }
            Some(SessionStatus::Idle) => {
                self.inner
                    .sessions
                    .quiesce_idle_for_fork(&source.id)
                    .await?;
            }
            _ => {}
        }

        // The pi-side fork: a separate helper process; the source session
        // file is never mutated. Runs OUTSIDE the mutation lock (this is the
        // slow step). Expected backend failures are classified into typed
        // Unavailable responses; only genuine I/O/infrastructure errors
        // surface as EngineError.
        let source_session = source
            .harness_session_id
            .as_deref()
            .unwrap_or_default()
            .to_string();
        let pi_result = match self
            .inner
            .backend
            .fork_session(PiSessionForkRequest {
                source_session_path: source_session,
                visible_user_prompts: visible_prompts,
                boundary: plan.pi_boundary,
            })
            .await
        {
            Ok(result) => result,
            Err(err) => {
                return classify_backend_error(&err, Operation::Fork)
                    .map(SessionForkResponse::Unavailable);
            }
        };
        // `None` for an EMPTY-CONTEXT fork before the first user: pi does not
        // persist that session file until the target's first send.
        let new_session_path = pi_result.session_path;

        // Publish the target: ephemeral doc → snapshot → row → flip. The
        // mutation lock serializes this section; the row check inside makes
        // same-request retries idempotent even under a concurrent fork. When
        // ANOTHER concurrent request already published the target, THIS call's
        // freshly created pi session (when any) is an orphan: best-effort
        // delete it (managed-root guarded — never the source) before
        // returning the existing chat.
        let _mutation = self.inner.mutation_lock.lock().await;
        if let Some(existing) = self.inner.workspace.chat(&request.request_id)? {
            self.delete_new_fork_session(new_session_path.as_deref());
            if let Err(unavail) = self.validate_existing_target(&existing, &source, &plan) {
                return Ok(unavail);
            }
            return Ok(existing_response(
                existing,
                plan.mode,
                plan.composer_text.clone(),
            ));
        }
        let target_id = request.request_id.clone();
        let target_handle = self.inner.doc_host.open_ephemeral(&target_id)?;
        let to_copy = plan.to_copy(&joined, &raw);
        for entry in &to_copy {
            target_handle.doc().push_message(entry)?;
        }
        if let Err(err) = (|| -> Result<(), EngineError> {
            self.inner.doc_host.prepare_promotion(&target_id)?;
            let title = fork_title(&source);
            // Endpoint preview from the newest copied entry; the sidebar
            // TIMESTAMP is creation `now` (a fork is NEW activity, never
            // buried under the source's old timestamp — `create_fork_chat`
            // stamps `last_message_at`/`last_seen_at` at birth).
            let preview = last_entry_activity(&to_copy);
            // The fork's OWN harness session: a materialized pi file (any
            // boundary past the first user) stamps the row with the fresh
            // path + cwd; an EMPTY-CONTEXT first-user fork has no persisted
            // session yet, so the row keeps `harness_session_id: None` /
            // `harness_session_cwd: None` — its first send starts a FRESH
            // pi session from empty context (the source is Pi-configured, so
            // normal dispatch works).
            let session_id = new_session_path.as_deref();
            let session_cwd = session_id.map(|_| source.cwd.clone().unwrap_or_default());
            self.inner.workspace.create_fork_chat(
                &target_id,
                &source,
                &title,
                session_id,
                session_cwd.as_deref(),
                preview,
            )?;
            self.inner.doc_host.finish_promotion(&target_id)?;
            Ok(())
        })() {
            // Before the row was published: purge the ephemeral target and
            // best-effort delete ONLY the newly returned pi session file
            // (guarded to the managed session root; never the source; a no-op
            // when the fork never materialized a file).
            self.inner.doc_host.purge_chat(&target_id);
            self.delete_new_fork_session(new_session_path.as_deref());
            return Err(err);
        }
        let chat =
            self.inner.workspace.chat(&target_id)?.ok_or_else(|| {
                EngineError::Other("forked chat row vanished after publish".into())
            })?;
        Ok(SessionForkResponse::Created(SessionForkCreated {
            chat,
            mode: plan.mode,
            composer_text: plan.composer_text.clone(),
        }))
    }

    /// `RewindSession` handler (Session Rewind v1): restart the conversation
    /// from a settled anchor WITHOUT leaving the chat.
    ///
    /// Same boundary vocabulary as a fork ([`compute_boundary`]) and the same
    /// pi helper, but the result lands IN PLACE:
    /// 1. the helper materializes a truncated pi session from the source's
    ///    file (which it never mutates);
    /// 2. the transcript entries past the boundary are deleted from the
    ///    chat's own doc in ONE commit (watchers see one truncation);
    /// 3. the SAME chat row is re-pointed at the truncated session — the
    ///    chat id, tab, sidebar row and doc lineage are untouched, so the
    ///    user stays exactly where they were.
    ///
    /// A USER anchor removes that message too and hands its text back as
    /// `composer_text`; an ASSISTANT anchor keeps the reply and removes what
    /// follows. Rewinding the LATEST entry would remove nothing and is
    /// refused rather than silently re-cloning the session.
    ///
    /// Not idempotent by design and deliberately not retried under one id:
    /// after a successful rewind the anchor is either gone or terminal, so a
    /// duplicate call answers Unavailable instead of cutting deeper.
    pub async fn rewind(
        &self,
        request: SessionRewindRequest,
    ) -> Result<SessionRewindResponse, EngineError> {
        let chat = match self.validate_source(&request.chat_id, Operation::Rewind)? {
            Ok(chat) => chat,
            Err(unavail) => return Ok(SessionRewindResponse::Unavailable(unavail)),
        };

        let handle = self
            .inner
            .doc_host
            .open(&chat.id)
            .map_err(|_| EngineError::Other("chat doc unavailable".into()))?;
        let raw = handle.doc().read_entries()?;
        let joined = cypher_doc::join_continuation_entries(raw.clone());
        let plan = match compute_boundary(&joined, &request.anchor_message_id) {
            Ok(plan) => plan,
            Err(reason) => return Ok(rewind_unavailable(reason, rewind_boundary_message(reason))),
        };
        // `CloneLeaf` is exactly the "anchor is the newest entry" case: a
        // rewind there would delete nothing and still spend a pi session.
        if matches!(plan.pi_boundary, PiForkBoundary::CloneLeaf) {
            return Ok(rewind_unavailable(
                SessionForkUnavailableReason::BoundaryUnavailable,
                "This is already the last message — there is nothing after it \
                 to remove.",
            ));
        }
        let visible_prompts: Vec<String> = joined
            .iter()
            .filter(|e| e.role == MessageRole::User)
            .map(visible_text_of)
            .collect();

        // A live turn owns both the transcript tail and the pi session; only
        // a parked (Idle) run may be quiesced out of the way.
        match self
            .inner
            .sessions
            .session_status(&chat.id)
            .map(|s| s.status)
        {
            Some(SessionStatus::Working) | Some(SessionStatus::AwaitingInput) => {
                return Ok(rewind_unavailable(
                    SessionForkUnavailableReason::LiveSession,
                    "The chat is still running. Restart from a message once \
                     the current turn settles.",
                ));
            }
            Some(SessionStatus::Idle) => {
                self.inner.sessions.quiesce_idle_for_fork(&chat.id).await?;
            }
            _ => {}
        }

        // The pi side: a separate `--no-extensions` helper builds the
        // truncated session from a byte snapshot of the source file. Slow, so
        // it runs OUTSIDE the mutation lock; the source file stays intact and
        // becomes the pre-rewind copy on disk.
        let source_session = chat
            .harness_session_id
            .as_deref()
            .unwrap_or_default()
            .to_string();
        let pi_result = match self
            .inner
            .backend
            .fork_session(PiSessionForkRequest {
                source_session_path: source_session,
                visible_user_prompts: visible_prompts,
                boundary: plan.pi_boundary,
            })
            .await
        {
            Ok(result) => result,
            Err(err) => {
                return classify_backend_error(&err, Operation::Rewind)
                    .map(SessionRewindResponse::Unavailable);
            }
        };
        // `None` for a rewind to before the FIRST user: that context is empty
        // and pi persists nothing until the next send.
        let new_session_path = pi_result.session_path;

        // Publish in place. The mutation lock serializes this against forks
        // and other rewinds; the transcript recheck inside guards the window
        // the slow helper opened — if the doc moved (a queued command drained,
        // a remote device appended), the planned cut no longer describes this
        // transcript and deleting by it could cut the wrong entries.
        let _mutation = self.inner.mutation_lock.lock().await;
        let raw_now = handle.doc().read_entries()?;
        if raw_now.len() != raw.len()
            || raw_now
                .iter()
                .zip(&raw)
                .any(|(now, before)| now.id != before.id)
        {
            self.delete_new_fork_session(new_session_path.as_deref());
            return Ok(rewind_unavailable(
                SessionForkUnavailableReason::BoundaryUnavailable,
                "The transcript changed while the restart was being prepared. \
                 Try again.",
            ));
        }

        let kept = plan.to_copy(&joined, &raw);
        let keep_ids: HashSet<&str> = kept.iter().map(|e| e.id.as_str()).collect();
        let removed_message_ids: Vec<String> = raw
            .iter()
            .filter(|e| !keep_ids.contains(e.id.as_str()))
            .map(|e| e.id.clone())
            .collect();
        let removed_set: HashSet<String> = removed_message_ids.iter().cloned().collect();
        handle.doc().remove_messages(&removed_set)?;

        // Re-point the chat at the truncated session BEFORE anything can
        // dispatch: the live cache and the durable row are written together,
        // so the next send resumes the rewound context (or starts fresh when
        // the rewind emptied it).
        let session_cwd = chat
            .harness_session_cwd
            .clone()
            .or_else(|| chat.cwd.as_deref().map(crate::repos::expand_home))
            .unwrap_or_default();
        self.inner.sessions.rebind_harness_session(
            &chat.id,
            new_session_path.as_deref(),
            &session_cwd,
        );

        // The sidebar's endpoint preview now ends at the new last entry (empty
        // when the rewind emptied the chat).
        self.inner
            .workspace
            .note_message(&chat.id, &last_entry_activity(&kept).unwrap_or_default());

        // An emptied transcript is the one shape boot-time transcript salvage
        // would try to "repair" from the pre-chat2 rollback copy — which is
        // exactly the history the user just deleted. Drop that copy so the
        // removal stays removed.
        if kept.is_empty() {
            self.inner.doc_host.drop_pre_chat2_rollback(&chat.id);
        }

        let chat = self
            .inner
            .workspace
            .chat(&chat.id)?
            .ok_or_else(|| EngineError::Other("chat row vanished during rewind".into()))?;
        Ok(SessionRewindResponse::Rewound(SessionRewound {
            chat,
            mode: plan.mode,
            composer_text: plan.composer_text.clone(),
            removed_message_ids,
        }))
    }

    /// Validate the source chat against every prerequisite shared by a fork
    /// and an in-place rewind (both drive the same pi helper against the same
    /// session store, so the gate is identical — only the wording differs).
    /// Returns the source [`Chat`] or a typed refusal.
    fn validate_source(
        &self,
        source_chat_id: &str,
        op: Operation,
    ) -> Result<Result<Chat, SessionForkUnavailable>, EngineError> {
        let Some(chat) = self.inner.workspace.chat(source_chat_id)? else {
            return Ok(Err(unavail(
                SessionForkUnavailableReason::MissingHost,
                "Chat not found.",
            )));
        };
        if self.inner.sessions.is_ephemeral(source_chat_id) {
            return Ok(Err(unavail(
                SessionForkUnavailableReason::TemporarySideChat,
                &format!("Temporary Side Chats cannot be {}.", op.past()),
            )));
        }
        if chat.is_child() {
            return Ok(Err(unavail(
                SessionForkUnavailableReason::ChildChat,
                &format!("Child chats cannot be {}.", op.past()),
            )));
        }
        if chat.device_id != self.inner.doc_host.device_id() {
            return Ok(Err(unavail(
                SessionForkUnavailableReason::MissingHost,
                &format!(
                    "This chat is hosted on another device. It can only be {} \
                     on the device hosting its session.",
                    op.past()
                ),
            )));
        }
        let is_pi = chat
            .config
            .as_ref()
            .is_some_and(|c| c.harness == cypher_proto::HarnessId::Pi);
        if !is_pi {
            return Ok(Err(unavail(
                SessionForkUnavailableReason::NonPi,
                &format!("{} requires a Pi session (Pi only in v1).", op.noun()),
            )));
        }
        let session_ok = chat
            .harness_session_id
            .as_deref()
            .is_some_and(|s| !s.is_empty())
            && chat.harness_session_cwd.as_deref().is_none_or(|c| {
                // The stored session cwd is the harness's REAL directory, so the
                // row's `~` must be expanded before the two can compare equal.
                c.is_empty()
                    || chat.cwd.as_deref().map(crate::repos::expand_home) == Some(c.to_string())
            });
        if !session_ok {
            return Ok(Err(unavail(
                SessionForkUnavailableReason::MissingSession,
                &format!("This chat has no stored Pi session to {}.", op.verb()),
            )));
        }
        Ok(Ok(chat))
    }

    /// Best-effort delete of a freshly created fork session file, guarded to
    /// the MANAGED session root by CANONICAL containment (symlinks and `..`
    /// resolved — never deletes the source or anything outside cypher's own
    /// pi store). An already-deleted file is quiet; `None` (an empty-context
    /// first-user fork, which pi never persisted) is a no-op.
    fn delete_new_fork_session(&self, path: Option<&str>) {
        let Some(path) = path else { return };
        let Some(canonical) =
            canonicalize_under_root(&PathBuf::from(path), &self.inner.pi_sessions_root)
        else {
            tracing::warn!(
                session = %path,
                root = %self.inner.pi_sessions_root.display(),
                "fork cleanup refused: session path outside managed root"
            );
            return;
        };
        match std::fs::remove_file(&canonical) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // Already gone — quiet.
            }
            Err(err) => {
                tracing::warn!(session = %canonical.display(), error = %err, "fork cleanup delete failed");
            }
        }
    }

    /// Validate an already-existing target chat (a lost-reply retry or a
    /// concurrent same-request race). The target must be a ROOT chat matching
    /// the source's host/config/cwd with a DIFFERENT harness session path —
    /// anything else means the client-minted request id collides with an
    /// unrelated chat and must be refused rather than silently returned.
    ///
    /// The expected PLAN boundary gates the session-state half of the match:
    /// a fork past the FIRST USER (or a leaf clone) ALWAYS materializes a
    /// persisted session, so its target must carry one — an existing target
    /// with no session could only be an unrelated chat (a real fork of that
    /// boundary would have stamped one). An EMPTY-CONTEXT first-user fork
    /// (`BeforeUser(0)`) legitimately leaves `harness_session_id: None`
    /// (real pi persists that file only on the target's first send), so a
    /// session-less target is valid there — and an unexpectedly materialized
    /// first-user path is still required to differ from the source.
    // The `Err` here IS the protocol reply the caller returns verbatim, so its
    // size is fixed by `SessionForkResponse`; boxing would only move the same
    // bytes behind a pointer one frame earlier.
    #[allow(clippy::result_large_err)]
    fn validate_existing_target(
        &self,
        existing: &Chat,
        source: &Chat,
        plan: &BoundaryPlan,
    ) -> Result<(), SessionForkResponse> {
        if existing.is_child()
            || existing.device_id != source.device_id
            || existing.cwd != source.cwd
            || existing.config != source.config
            || existing.harness_session_id == source.harness_session_id
        {
            return Err(unavailable(
                SessionForkUnavailableReason::BoundaryUnavailable,
                "The fork request id collides with an existing unrelated chat.",
            ));
        }
        if !matches!(plan.pi_boundary, PiForkBoundary::BeforeUser(0))
            && existing
                .harness_session_id
                .as_deref()
                .is_none_or(|s| s.is_empty())
        {
            return Err(unavailable(
                SessionForkUnavailableReason::BoundaryUnavailable,
                "The fork request id collides with an existing unrelated chat.",
            ));
        }
        Ok(())
    }
}

/// Classify an expected backend [`HarnessError`] into a typed refusal, or
/// `Err(EngineError)` for genuine unexpected I/O/infrastructure failures
/// (which surface as an RPC `Failed`, not a typed Unavailable). Shared by the
/// fork and the in-place rewind — they drive the SAME pi helper, so they fail
/// the same ways.
fn classify_backend_error(
    err: &HarnessError,
    op: Operation,
) -> Result<SessionForkUnavailable, EngineError> {
    match err {
        // The hosting device lacks/needs a newer Pi CLI — actionable update/
        // install guidance, not a boundary problem.
        HarnessError::NotInstalled(_) | HarnessError::Install(_) => Ok(unavail(
            SessionForkUnavailableReason::Unsupported,
            &format!(
                "{} requires the Pi CLI on the device hosting the session. \
                 Install or update it on that device, then retry.",
                op.noun()
            ),
        )),
        HarnessError::Protocol(msg)
            if msg.contains("unsupported for this harness")
                || msg.contains("pi harness unavailable") =>
        {
            Ok(unavail(
                SessionForkUnavailableReason::Unsupported,
                &format!(
                    "The device hosting this session runs an engine or harness \
                     that does not support this ({} is Pi-only in v1). Update \
                     that device and retry.",
                    op.noun().to_lowercase()
                ),
            ))
        }
        // Prompt mapping / boundary / source-safety protocol refusals: the
        // transcript boundary could not be represented on the hosting device.
        HarnessError::Protocol(msg) if !msg.contains("timed out") => Ok(unavail(
            SessionForkUnavailableReason::BoundaryUnavailable,
            &format!(
                "Cannot {} this message: the session on the hosting device \
                 could not be mapped to the transcript boundary (missing or \
                 outside the managed store, mismatched prompts, or an \
                 unrepresentable boundary).",
                op.verb().split(' ').next().unwrap_or("fork")
            ),
        )),
        // A slow/hung helper is an infrastructure problem: surface as a real
        // error (the UI retry keeps the same request id, so a late-created
        // fork is still recovered idempotently).
        HarnessError::Protocol(_) | HarnessError::Io(_) => {
            Err(EngineError::Other(format!("pi session fork failed: {err}")))
        }
    }
}

/// Canonicalize a path for a managed-root deletion WITHOUT requiring the file
/// to exist: the deepest EXISTING ancestor is canonicalized (resolving `..`
/// and symlinks), then the remaining components are appended lexically.
/// `None` when the result would escape `root` (a symlinked ancestor, `..`
/// traversal, or an unresolvable prefix) — the deletion is then refused.
fn canonicalize_under_root(path: &Path, root: &Path) -> Option<PathBuf> {
    let root = std::fs::canonicalize(root).ok()?;
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let name = existing.file_name()?.to_os_string();
        tail.push(name);
        if !existing.pop() {
            return None;
        }
    }
    let mut base = existing.canonicalize().ok()?;
    for component in tail.iter().rev() {
        if component == ".." {
            return None;
        }
        base.push(component);
    }
    if !base.starts_with(&root) {
        return None;
    }
    Some(base)
}

/// Which operation a shared refusal is worded for. Fork and rewind run the
/// same prerequisites through [`SessionForks::validate_source`]; only the
/// user-facing verb changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Fork,
    Rewind,
}

impl Operation {
    /// Sentence subject: "Session Fork requires a Pi session."
    fn noun(self) -> &'static str {
        match self {
            Self::Fork => "Session Fork",
            Self::Rewind => "Restarting a conversation",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Fork => "fork from",
            Self::Rewind => "restart from",
        }
    }

    fn past(self) -> &'static str {
        match self {
            Self::Fork => "forked",
            Self::Rewind => "restarted",
        }
    }
}

fn unavail(reason: SessionForkUnavailableReason, message: &str) -> SessionForkUnavailable {
    SessionForkUnavailable {
        reason,
        message: message.to_string(),
    }
}

fn unavailable(reason: SessionForkUnavailableReason, message: &str) -> SessionForkResponse {
    SessionForkResponse::Unavailable(unavail(reason, message))
}

fn rewind_unavailable(
    reason: SessionForkUnavailableReason,
    message: &str,
) -> SessionRewindResponse {
    SessionRewindResponse::Unavailable(unavail(reason, message))
}

fn boundary_message(reason: SessionForkUnavailableReason) -> &'static str {
    match reason {
        SessionForkUnavailableReason::BoundaryUnavailable => {
            "This message cannot be forked: no representable session boundary \
             (not a settled user message, not a latest settled assistant \
             response, or the anchor was not found)."
        }
        _ => "This message cannot be forked.",
    }
}

fn rewind_boundary_message(reason: SessionForkUnavailableReason) -> &'static str {
    match reason {
        SessionForkUnavailableReason::BoundaryUnavailable => {
            "The conversation cannot restart here: no representable session \
             boundary (the message is still streaming, another reply sits \
             between it and the next prompt, or the anchor was not found)."
        }
        _ => "The conversation cannot restart from this message.",
    }
}

/// Reconstruct the Created response for an already-existing target chat (an
/// idempotent retry): re-derive mode/composer from the CURRENT source
/// transcript so the reply stays accurate.
fn existing_response(
    chat: Chat,
    mode: SessionForkMode,
    composer_text: Option<String>,
) -> SessionForkResponse {
    SessionForkResponse::Created(SessionForkCreated {
        chat,
        mode,
        composer_text,
    })
}

/// The visible text of one entry: joined text parts, attachment trailer
/// stripped (absolute image paths never leave the doc).
fn visible_text_of(entry: &SessionMessageEntry) -> String {
    let raw: String = entry
        .parts
        .iter()
        .filter_map(|p| match p {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    cypher_harness::pi::fork::strip_attachment_trailer(&raw).to_string()
}

/// The resolved fork boundary for one anchor on the joined transcript.
#[derive(Debug)]
pub(crate) struct BoundaryPlan {
    pub mode: SessionForkMode,
    /// Number of JOINED prefix entries the target copies (exclusive end).
    pub prefix_end: usize,
    pub pi_boundary: PiForkBoundary,
    pub composer_text: Option<String>,
}

impl BoundaryPlan {
    /// The raw entries to copy into the target: the joined prefix's root
    /// entries plus their continuation entries, in raw append order.
    pub(crate) fn to_copy<'a>(
        &self,
        joined: &[SessionMessageEntry],
        raw: &'a [SessionMessageEntry],
    ) -> Vec<&'a SessionMessageEntry> {
        let prefix_ids: HashSet<&str> = joined[..self.prefix_end]
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        raw.iter()
            .filter(|e| {
                prefix_ids.contains(e.id.as_str())
                    || e.continuation_of
                        .as_deref()
                        .is_some_and(|c| prefix_ids.contains(c))
            })
            .collect()
    }
}

/// Compute the fork boundary from the joined transcript.
///
/// - clicked USER: prefix BEFORE the anchor, pi `BeforeUser(anchor ordinal)`,
///   composer prefilled with the anchor's visible text;
/// - clicked ASSISTANT with a LATER user: prefix THROUGH the anchor, pi
///   `BeforeUser(next user ordinal)`;
/// - clicked ASSISTANT with NO later user AND it is the LATEST settled entry:
///   prefix THROUGH the anchor, pi `CloneLeaf`;
/// - anything else: unavailable.
pub(crate) fn compute_boundary(
    joined: &[SessionMessageEntry],
    anchor_id: &str,
) -> Result<BoundaryPlan, SessionForkUnavailableReason> {
    let anchor_ix = joined
        .iter()
        .position(|e| e.id == anchor_id)
        .ok_or(SessionForkUnavailableReason::BoundaryUnavailable)?;
    let anchor = &joined[anchor_ix];
    if anchor.status == Some(MessageStatus::Streaming) {
        return Err(SessionForkUnavailableReason::BoundaryUnavailable);
    }
    let user_ordinal = |up_to: usize| {
        joined[..=up_to]
            .iter()
            .filter(|e| e.role == MessageRole::User)
            .count()
            .saturating_sub(1)
    };
    match anchor.role {
        MessageRole::User => Ok(BoundaryPlan {
            mode: SessionForkMode::EditUser,
            prefix_end: anchor_ix,
            pi_boundary: PiForkBoundary::BeforeUser(user_ordinal(anchor_ix)),
            composer_text: Some(visible_text_of(anchor)),
        }),
        MessageRole::Assistant => {
            let next_user = joined[anchor_ix + 1..]
                .iter()
                .position(|e| e.role == MessageRole::User)
                .map(|rel| anchor_ix + 1 + rel);
            match next_user {
                // The later user must be the IMMEDIATELY NEXT joined entry:
                // `fork before that user` copies the pi session through the
                // anchor, and any intervening assistant/system entry would be
                // included in the Pi context but omitted from the copied
                // Cypher prefix — refuse instead of drifting.
                Some(nu) if nu == anchor_ix + 1 => Ok(BoundaryPlan {
                    mode: SessionForkMode::ContinueAfterAssistant,
                    prefix_end: anchor_ix + 1,
                    pi_boundary: PiForkBoundary::BeforeUser(user_ordinal(nu)),
                    composer_text: None,
                }),
                Some(_) => Err(SessionForkUnavailableReason::BoundaryUnavailable),
                // No later user: clone at the leaf ONLY when the anchor IS the
                // latest settled entry — otherwise clone would pull in later
                // assistant-only entries past the clicked boundary.
                None if anchor_ix == joined.len() - 1 => Ok(BoundaryPlan {
                    mode: SessionForkMode::ContinueAfterAssistant,
                    prefix_end: anchor_ix + 1,
                    pi_boundary: PiForkBoundary::CloneLeaf,
                    composer_text: None,
                }),
                None => Err(SessionForkUnavailableReason::BoundaryUnavailable),
            }
        }
        MessageRole::System => Err(SessionForkUnavailableReason::BoundaryUnavailable),
    }
}

/// `<source title> — Fork`, bounded.
pub(crate) fn fork_title(source: &Chat) -> String {
    let base = source
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("New session");
    let mut title = format!("{base} — Fork");
    if title.chars().count() > MAX_FORK_TITLE_CHARS {
        title = title.chars().take(MAX_FORK_TITLE_CHARS).collect();
    }
    title
}

/// The fork row's sidebar ENDPOINT PREVIEW from the NEWEST copied entry: its
/// visible text (first 120 chars). `None` for an empty prefix (a fork before
/// the very first user message). The row's TIMESTAMP is the creation time
/// (see `create_fork_chat`), so the preview and the recency sort are
/// independent.
fn last_entry_activity(entries: &[&SessionMessageEntry]) -> Option<String> {
    let last = entries.last()?;
    let text: String = last
        .parts
        .iter()
        .filter_map(|p| match p {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (!text.trim().is_empty()).then(|| text.chars().take(120).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_doc::MessageStatus;

    fn user(id: &str, text: &str) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: format!("{id}-p"),
                text: text.into(),
            }],
            created_at: 0,
            device_id: "dev".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
            completed_at: None,
        }
    }

    fn assistant(id: &str, text: &str) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Text {
                id: format!("{id}-p"),
                text: text.into(),
            }],
            created_at: 0,
            device_id: "dev".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
            completed_at: None,
        }
    }

    fn streaming_assistant(id: &str) -> SessionMessageEntry {
        let mut e = assistant(id, "live");
        e.status = Some(MessageStatus::Streaming);
        e
    }

    fn joined() -> Vec<SessionMessageEntry> {
        vec![
            user("u1", "first"),
            assistant("a1", "reply one"),
            user("u2", "second"),
            assistant("a2", "reply two"),
        ]
    }

    #[test]
    fn user_anchor_forks_before_and_prefills() {
        let plan = compute_boundary(&joined(), "u2").unwrap();
        assert_eq!(plan.mode, SessionForkMode::EditUser);
        assert_eq!(plan.prefix_end, 2); // u1 + a1
        assert_eq!(plan.pi_boundary, PiForkBoundary::BeforeUser(1));
        assert_eq!(plan.composer_text.as_deref(), Some("second"));
        let joined = joined();
        let copied = plan.to_copy(&joined, &joined);
        assert_eq!(
            copied.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["u1", "a1"]
        );
    }

    #[test]
    fn assistant_with_later_user_includes_the_clicked_reply() {
        let plan = compute_boundary(&joined(), "a1").unwrap();
        assert_eq!(plan.mode, SessionForkMode::ContinueAfterAssistant);
        assert_eq!(plan.prefix_end, 2); // u1 + a1
        assert_eq!(plan.pi_boundary, PiForkBoundary::BeforeUser(1)); // next user = u2
        assert_eq!(plan.composer_text, None);
        let joined = joined();
        let copied = plan.to_copy(&joined, &joined);
        assert_eq!(
            copied.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["u1", "a1"]
        );
    }

    #[test]
    fn last_assistant_clones_at_leaf() {
        let plan = compute_boundary(&joined(), "a2").unwrap();
        assert_eq!(plan.mode, SessionForkMode::ContinueAfterAssistant);
        assert_eq!(plan.prefix_end, 4);
        assert_eq!(plan.pi_boundary, PiForkBoundary::CloneLeaf);
    }

    #[test]
    fn mid_assistant_with_no_later_user_is_unavailable() {
        // u1, a1(clicked), a2 — no later user and a2 sits past the clicked
        // boundary: CloneLeaf would over-include, so it must be refused.
        let transcript = vec![
            user("u1", "first"),
            assistant("a1", "one"),
            assistant("a2", "two"),
        ];
        let err = compute_boundary(&transcript, "a1").unwrap_err();
        assert_eq!(err, SessionForkUnavailableReason::BoundaryUnavailable);
    }

    #[test]
    fn assistant_intervened_by_assistant_before_user_is_unavailable() {
        // u1, a1(clicked), a2, u2 — the next USER is NOT the immediately next
        // joined entry (an assistant intervenes). `fork before u2` would copy
        // pi context through a2 while the Cypher prefix omits it: refuse.
        let transcript = vec![
            user("u1", "first"),
            assistant("a1", "one"),
            assistant("a2", "two"),
            user("u2", "second"),
        ];
        assert_eq!(
            compute_boundary(&transcript, "a1").unwrap_err(),
            SessionForkUnavailableReason::BoundaryUnavailable
        );
    }

    #[test]
    fn assistant_intervened_by_system_before_user_is_unavailable() {
        // u1, a1(clicked), sys, u2 — a system entry sits between the clicked
        // assistant and the next user: same over-inclusion drift, refused.
        let mut sys = user("sys", "note");
        sys.role = MessageRole::System;
        let transcript = vec![
            user("u1", "first"),
            assistant("a1", "one"),
            sys,
            user("u2", "second"),
        ];
        assert_eq!(
            compute_boundary(&transcript, "a1").unwrap_err(),
            SessionForkUnavailableReason::BoundaryUnavailable
        );
    }

    #[test]
    fn assistant_directly_followed_by_user_still_forks() {
        // The immediately-next-entry user case remains valid: u1, a1(clicked),
        // u2 — prefix through a1, fork before u2.
        let plan = compute_boundary(&joined(), "a1").unwrap();
        assert_eq!(plan.mode, SessionForkMode::ContinueAfterAssistant);
        assert_eq!(plan.prefix_end, 2);
        assert_eq!(plan.pi_boundary, PiForkBoundary::BeforeUser(1));
    }

    #[test]
    fn streaming_and_missing_anchors_are_unavailable() {
        assert_eq!(
            compute_boundary(&joined(), "nope").unwrap_err(),
            SessionForkUnavailableReason::BoundaryUnavailable
        );
        let live = vec![user("u1", "first"), streaming_assistant("a1")];
        assert_eq!(
            compute_boundary(&live, "a1").unwrap_err(),
            SessionForkUnavailableReason::BoundaryUnavailable
        );
    }

    #[test]
    fn continuation_entries_copied_with_their_roots() {
        let mut a1b = assistant("a1b", "continued");
        a1b.continuation_of = Some("a1".into());
        let raw = vec![
            user("u1", "first"),
            assistant("a1", "one"),
            a1b.clone(),
            user("u2", "second"),
        ];
        let joined = cypher_doc::join_continuation_entries(raw.clone());
        let plan = compute_boundary(&joined, "u2").unwrap(); // prefix u1+a1(+cont)
        let copied = plan.to_copy(&joined, &raw);
        assert_eq!(
            copied.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["u1", "a1", "a1b"]
        );
        assert_eq!(copied[2].continuation_of.as_deref(), Some("a1"));
    }

    #[test]
    fn visible_text_strips_attachment_trailers() {
        let mut e = user("u1", "prompt");
        e.parts[0] = MessagePart::Text {
            id: "p".into(),
            text: "prompt\n\nAttached images (local files — open them to view):\n- /a.png".into(),
        };
        assert_eq!(visible_text_of(&e), "prompt");
    }

    #[test]
    fn canonical_cleanup_guard_refuses_escapes_and_accepts_managed_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("agent-sessions");
        std::fs::create_dir_all(&root).unwrap();
        let root_c = std::fs::canonicalize(&root).unwrap();

        // A non-existent file under the root resolves (the delete may run
        // before pi persists it).
        let inside = canonicalize_under_root(&root.join("fork-1.jsonl"), &root).unwrap();
        assert_eq!(inside, root_c.join("fork-1.jsonl"));
        // An existing file under the root resolves too.
        std::fs::write(root.join("fork-2.jsonl"), b"{}").unwrap();
        let inside = canonicalize_under_root(&root.join("fork-2.jsonl"), &root).unwrap();
        assert_eq!(inside, root_c.join("fork-2.jsonl"));
        // A path OUTSIDE the root (lexically and canonically) is refused.
        assert!(canonicalize_under_root(&dir.path().join("x.jsonl"), &root).is_none());
        assert!(canonicalize_under_root(&root.join("..").join("x.jsonl"), &root).is_none());

        // A symlinked ancestor escaping the root is refused even though the
        // lexical path starts with the root.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), root.join("escape")).unwrap();
            assert!(
                canonicalize_under_root(&root.join("escape").join("secret.jsonl"), &root).is_none()
            );
        }
    }

    #[test]
    fn fork_title_appends_and_bounds() {
        let mut chat = Chat {
            pinned: false,
            id: "c".into(),
            device_id: "d".into(),
            title: Some("My chat".into()),
            archived: false,
            cwd: None,
            branch: None,
            checkout_id: None,
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: chrono::Utc::now(),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: None,
            room_gen: None,
            child: None,
        };
        assert_eq!(fork_title(&chat), "My chat — Fork");
        chat.title = None;
        assert_eq!(fork_title(&chat), "New session — Fork");
        chat.title = Some("x".repeat(300));
        assert!(fork_title(&chat).chars().count() <= MAX_FORK_TITLE_CHARS);
    }

    #[test]
    fn first_user_fork_has_empty_prefix() {
        let plan = compute_boundary(&joined(), "u1").unwrap();
        assert_eq!(plan.prefix_end, 0);
        assert_eq!(plan.pi_boundary, PiForkBoundary::BeforeUser(0));
        assert!(plan.to_copy(&joined(), &joined()).is_empty());
    }
}
