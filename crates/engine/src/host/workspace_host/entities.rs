//! Entity writes on the registry doc: chats, spaces, devices and the git
//! metadata the diff-sync host records.

use super::*;

impl WorkspaceHost {
    // ── Mutate surface (LWW writes accepted from any device) ────────────────

    /// Create a chat, usually *in a project*: the project fixes the host device
    /// and base cwd (`cwd` override = an isolated-worktree path). With no
    /// `space_id` the chat is project-less: `device_id` picks the host and the
    /// cwd defaults to `~` (expanded host-side when the run spawns).
    pub fn create_chat(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        config: Option<ChatConfig>,
        cwd: Option<String>,
    ) -> Result<(), EngineError> {
        if self.read(|doc| doc.chat(chat_id))?.is_some() {
            return Ok(()); // idempotent: optimistic client retries never duplicate
        }
        let space = match space_id {
            Some(space_id) => match self.read(|doc| doc.space(space_id))? {
                Some(space) => Some(space),
                None => return Err(EngineError::Other(format!("no such space: {space_id}"))),
            },
            None => None,
        };
        let host_device = match (&space, device_id) {
            (Some(space), _) => space.device_id.clone(),
            (None, Some(device_id)) => device_id.to_string(),
            (None, None) => {
                return Err(EngineError::Other(
                    "createChat needs a spaceId or a deviceId".into(),
                ));
            }
        };
        self.mutate(|doc| {
            doc.upsert_chat(&Chat {
                pinned: false,
                id: chat_id.to_string(),
                device_id: host_device.clone(),
                title: None,
                archived: false,
                cwd: Some(cwd.unwrap_or_else(|| {
                    space
                        .as_ref()
                        .map(|s| s.path.clone())
                        .unwrap_or_else(|| "~".to_string())
                })),
                branch: None,
                checkout_id: None,
                config,
                last_message_preview: None,
                last_message_at: None,
                created_at: Utc::now(),
                harness_session_id: None,
                // Nothing on desktop reads it; iOS builds through 0.2.0 (24)
                // dial a chat's room only at `roomGen >= 2`.
                room_gen: Some(2),
                harness_session_cwd: None,
                space_id: space.as_ref().map(|s| s.id.clone()),
                last_seen_at: None,
                child: None,
            })
        })?;
        Ok(())
    }

    // ── spaces (Mutate surface + owner stamps) ──────────────────────────────

    /// Create a space (any device). Idempotent by id; a live duplicate of the
    /// same `(deviceId, path)` is a no-op backstop (the UI reuses via
    /// WatchSpaces). `git_detected` is seeded from the picker's FolderEntry;
    /// the owning device's SpacesSync re-verifies.
    pub fn create_space(
        &self,
        space_id: &str,
        device_id: &str,
        path: &str,
        name: Option<String>,
        git_detected: bool,
    ) -> Result<(), EngineError> {
        let spaces = self.read(|doc| doc.read_spaces())?;
        if spaces
            .iter()
            .any(|s| s.id == space_id || (s.device_id == device_id && s.path == path))
        {
            return Ok(());
        }
        self.mutate(|doc| {
            doc.upsert_space(&Space {
                icon: None,
                color: None,
                pinned: false,
                id: space_id.to_string(),
                device_id: device_id.to_string(),
                path: path.to_string(),
                name,
                git_detected,
                git_checked_at: None,
                checkout_id: None,
                created_at: Utc::now(),
            })
        })?;
        Ok(())
    }

    pub fn rename_space(&self, space_id: &str, name: Option<&str>) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rename_space(space_id, name))?)
    }

    pub fn set_space_pinned(&self, space_id: &str, pinned: bool) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_space_pinned(space_id, pinned))?)
    }

    pub fn set_space_appearance(
        &self,
        space_id: &str,
        icon: Option<&str>,
        color: Option<&str>,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_space_appearance(space_id, icon, color))?)
    }

    pub fn set_chat_pinned(&self, chat_id: &str, pinned: bool) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_pinned(chat_id, pinned))?)
    }

    /// Hard-delete a space and its chats (registry cascade — one atomic batch).
    /// The caller (rpc layer) tears down live runs / doc-host handles for the
    /// returned chat ids.
    pub fn delete_space(&self, space_id: &str) -> Result<DeletedSpace, EngineError> {
        Ok(self.mutate(|doc| doc.delete_space(space_id))?)
    }

    /// Synced seen marker (any device; LWW + monotonic guard in the doc layer).
    pub fn mark_chat_seen(
        &self,
        chat_id: &str,
        at: chrono::DateTime<Utc>,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_seen(chat_id, at))?)
    }

    /// Owner-only git stamp (SpacesSync). Refuses rows owned by another device.
    pub fn set_space_git(
        &self,
        space_id: &str,
        detected: bool,
        checkout_id: Option<&str>,
    ) -> Result<bool, EngineError> {
        match self.read(|doc| doc.space(space_id))? {
            Some(space) if space.device_id == self.inner.config.device_id => {
                Ok(self
                    .mutate(|doc| doc.set_space_git(space_id, detected, checkout_id, Utc::now()))?)
            }
            Some(space) => {
                tracing::warn!(
                    space = %space_id, owner = %space.device_id,
                    "refusing git stamp on space owned by another device"
                );
                Ok(false)
            }
            None => Ok(false),
        }
    }

    pub fn read_spaces(&self) -> Result<Vec<Space>, EngineError> {
        Ok(self.read(|doc| doc.read_spaces())?)
    }

    pub fn rename_chat(&self, chat_id: &str, title: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rename_chat(chat_id, title))?)
    }

    /// Backdate a chat's activity timestamps (epoch ms). Returns false when
    /// the chat doesn't exist.
    pub fn set_chat_activity(
        &self,
        chat_id: &str,
        last_message_at: Option<i64>,
        created_at: Option<i64>,
    ) -> Result<bool, EngineError> {
        let Some(mut chat) = self.read(|doc| doc.chat(chat_id))? else {
            return Ok(false);
        };
        if let Some(ms) = last_message_at {
            chat.last_message_at = chrono::DateTime::<Utc>::from_timestamp_millis(ms);
        }
        if let Some(ms) = created_at
            && let Some(at) = chrono::DateTime::<Utc>::from_timestamp_millis(ms)
        {
            chat.created_at = at;
        }
        self.mutate(|doc| doc.upsert_chat(&chat))?;
        Ok(true)
    }

    /// Re-home a chat to another device (tooling/seeds; a future device
    /// migration flow will drive this). Returns false when the chat doesn't
    /// exist.
    pub fn set_chat_host(&self, chat_id: &str, device_id: &str) -> Result<bool, EngineError> {
        let Some(mut chat) = self.read(|doc| doc.chat(chat_id))? else {
            return Ok(false);
        };
        chat.device_id = device_id.to_string();
        self.mutate(|doc| doc.upsert_chat(&chat))?;
        Ok(true)
    }

    /// Upsert a chat row copied verbatim from another profile (local→synced
    /// import). Same write path as every live mutation, so the row persists
    /// and pushes like any other; the caller fixes `room_gen` beforehand.
    pub fn import_chat_row(&self, chat: &Chat) -> Result<(), EngineError> {
        Ok(self.mutate(|doc| doc.upsert_chat(chat))?)
    }

    /// Upsert a space row copied verbatim from another profile (local→synced
    /// import).
    pub fn import_space_row(&self, space: &Space) -> Result<(), EngineError> {
        Ok(self.mutate(|doc| doc.upsert_space(space))?)
    }

    pub fn set_chat_archived(&self, chat_id: &str, archived: bool) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_archived(chat_id, archived))?)
    }

    /// LWW full-config replace on the chat row (zeron `SetChatConfig` — the
    /// composer's mid-session model/reasoning/options changes). Returns false
    /// when the chat doesn't exist.
    pub fn set_chat_config(&self, chat_id: &str, config: &ChatConfig) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_config(chat_id, config))?)
    }

    /// Tombstone: removes the chats (and session-status) row; the per-chat session
    /// doc remains untouched.
    pub fn delete_chat(&self, chat_id: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.delete_chat(chat_id))?)
    }

    /// Sidebar freshness with an explicit timestamp: set the promoted Side
    /// Chat's preview + last-message activity from its transcript's newest
    /// message (a promoted chat must not land blank in the
    /// sidebar). Best-effort: `false` when the row is missing.
    pub fn set_chat_last_message(
        &self,
        chat_id: &str,
        preview: &str,
        at: DateTime<Utc>,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_last_message(chat_id, preview, at))?)
    }

    /// Promote a temporary Side Chat into a normal ROOT chat: a
    /// non-child Chat row with the SAME id, inheriting the parent's device /
    /// space / cwd / branch / config / checkout (deliberately NOT the parent's
    /// harness session — the promoted chat's own session continuity rides the
    /// in-memory harness-session backfill). The row's title is deterministic,
    /// derived from the selected quote (`title`). Born on chat2 (`room_gen: 2`).
    ///
    /// Idempotent: returns `Ok(false)` when a row already exists (a lost
    /// PromoteSideChat reply retried after the first promotion landed) — the
    /// caller treats that as already-promoted rather than double-writing.
    pub fn promote_side_chat(
        &self,
        side_chat_id: &str,
        parent: &Chat,
        title: &str,
    ) -> Result<bool, EngineError> {
        if self.read(|doc| doc.chat(side_chat_id))?.is_some() {
            return Ok(false);
        }
        self.mutate(|doc| {
            doc.upsert_chat(&Chat {
                pinned: false,
                id: side_chat_id.to_string(),
                device_id: parent.device_id.clone(),
                title: Some(title.to_string()),
                archived: false,
                cwd: parent.cwd.clone(),
                branch: parent.branch.clone(),
                checkout_id: parent.checkout_id.clone(),
                config: parent.config.clone(),
                last_message_preview: None,
                last_message_at: None,
                created_at: Utc::now(),
                harness_session_id: None,
                room_gen: Some(2),
                harness_session_cwd: None,
                space_id: parent.space_id.clone(),
                last_seen_at: None,
                child: None,
            })
        })?;
        Ok(true)
    }

    /// Session Fork (v1): create the NEW durable root chat row for a fork.
    /// Copies the source's host device / space / cwd / branch / checkout /
    /// config (same checkout, same root) verbatim; the fork's own identity is
    /// the `<source title> — Fork` title and — when the fork materialized a
    /// persisted pi session — the fresh harness session path + cwd. An
    /// EMPTY-CONTEXT fork before the first user carries NO session yet
    /// (`harness_session_id` / `harness_session_cwd` = `None`): its first
    /// send starts a fresh pi session from empty context (the source is
    /// Pi-configured, so normal dispatch works). Born on chat2
    /// (`room_gen: 2`) like every new chat. The sidebar TIMESTAMP is birth
    /// `now` (`last_message_at` = `last_seen_at` = now): a fork is NEW
    /// activity and must never be buried under the source's old timestamp —
    /// only the endpoint PREVIEW comes from the newest copied message.
    /// Idempotent by id — a lost-reply retry never mints a twin.
    pub fn create_fork_chat(
        &self,
        fork_chat_id: &str,
        source: &Chat,
        title: &str,
        harness_session_id: Option<&str>,
        harness_session_cwd: Option<&str>,
        last_message_preview: Option<String>,
    ) -> Result<(), EngineError> {
        if self.read(|doc| doc.chat(fork_chat_id))?.is_some() {
            return Ok(()); // idempotent: a retry never duplicates
        }
        let now = Utc::now();
        self.mutate(|doc| {
            doc.upsert_chat(&Chat {
                pinned: false,
                id: fork_chat_id.to_string(),
                device_id: source.device_id.clone(),
                title: Some(title.to_string()),
                archived: false,
                cwd: source.cwd.clone(),
                branch: source.branch.clone(),
                checkout_id: source.checkout_id.clone(),
                config: source.config.clone(),
                last_message_preview,
                // Fresh activity: a fork sorts as NEWLY created, never by the
                // source's old transcript timestamp.
                last_message_at: Some(now),
                created_at: now,
                harness_session_id: harness_session_id.map(str::to_string),
                room_gen: Some(2),
                harness_session_cwd: harness_session_cwd.map(str::to_string),
                space_id: source.space_id.clone(),
                // Seen on birth: the caller selects the fork immediately, so
                // it must never flash a "completed (unseen)" badge.
                last_seen_at: Some(now),
                child: None,
            })
        })?;
        Ok(())
    }

    /// Create a Cypher-hosted child subagent chat (`StartSubagent` bridge): a
    /// Pi-configured, titled chat row carrying the additive child metadata
    /// (parent chat id + parent run id + agent/task/mode + persisted profile).
    /// Inherits the parent's space/device/sandbox, and its cwd unless `cwd`
    /// overrides it — the override is PERSISTED on the row so the child's later
    /// turns keep running where its first one did. Deterministic +
    /// idempotent by `(parent_chat_id, parent_run_id)` — a repeat start
    /// reports [`ChildChatOutcome::Existing`] with the existing child's id
    /// instead of minting a twin (the caller must then NOT queue a second
    /// run). The messaging channel is deliberately NOT persisted (host-local
    /// absolute path — see [`ChildChat`]); the caller registers it in a local
    /// runtime map for the initial run.
    #[allow(clippy::too_many_arguments)] // child-start seam, not a public API
    pub fn create_child_chat(
        &self,
        parent: &Chat,
        parent_run_id: &str,
        agent: &str,
        task: &str,
        mode: SubagentRunMode,
        tool_call_id: Option<String>,
        profile: ChildAgentProfile,
        title: &str,
        cwd: Option<String>,
    ) -> Result<ChildChatOutcome, EngineError> {
        for chat in self.read_chats()? {
            if let Some(child) = &chat.child
                && child.parent_chat_id == parent.id
                && child.parent_run_id == parent_run_id
            {
                return Ok(ChildChatOutcome::Existing(chat.id));
            }
        }
        let chat_id = crate::util::new_id();
        let sandbox = parent
            .config
            .as_ref()
            .map(|c| c.sandbox)
            .unwrap_or(SandboxLevel::WorkspaceWrite);
        self.mutate(|doc| {
            doc.upsert_chat(&Chat {
                pinned: false,
                id: chat_id.clone(),
                device_id: parent.device_id.clone(),
                title: Some(title.to_string()),
                archived: false,
                cwd: cwd.clone().or_else(|| parent.cwd.clone()),
                branch: None,
                checkout_id: None,
                config: Some(ChatConfig {
                    harness: HarnessId::Pi,
                    model: profile.model.clone(),
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox,
                }),
                last_message_preview: None,
                last_message_at: None,
                created_at: Utc::now(),
                harness_session_id: None,
                harness_session_cwd: None,
                space_id: parent.space_id.clone(),
                last_seen_at: None,
                room_gen: Some(2),
                child: Some(ChildChat {
                    parent_chat_id: parent.id.clone(),
                    parent_run_id: parent_run_id.to_string(),
                    agent: agent.to_string(),
                    task: task.to_string(),
                    mode,
                    tool_call_id,
                    profile,
                }),
            })
        })?;
        Ok(ChildChatOutcome::Created(chat_id))
    }

    /// Child chat rows whose parent is `chat_id` (cascade-delete targets).
    pub fn child_chats(&self, parent_chat_id: &str) -> Result<Vec<Chat>, EngineError> {
        Ok(self
            .read_chats()?
            .into_iter()
            .filter(|c| c.parent_chat_id() == Some(parent_chat_id))
            .collect())
    }

    pub fn rename_device(&self, device_id: &str, name: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rename_device(device_id, name))?)
    }

    /// Development builds only: write a fake peer device row so the UI's
    /// multi-device and offline-host states can be judged without a second
    /// machine. No heartbeat ever arrives for it, so it reads as offline once
    /// `last_seen_at` falls outside the UI's online window. Refuses THIS
    /// device — its row is owned by `announce_device`.
    #[cfg(feature = "development")]
    pub fn seed_device(
        &self,
        device_id: &str,
        name: &str,
        platform: &str,
        last_seen_at: Option<DateTime<Utc>>,
    ) -> Result<(), EngineError> {
        if device_id == self.inner.config.device_id {
            return Err(EngineError::Other("cannot seed this device".into()));
        }
        Ok(self.mutate(|doc| {
            doc.upsert_device(&Device {
                id: device_id.to_string(),
                name: name.to_string(),
                platform: platform.to_string(),
                last_seen_at,
                created_at: Some(Utc::now()),
                version: None,
            })
        })?)
    }

    /// Unpair another device: tombstone its registry row so it drops out of
    /// sync. Refuses to delete THIS device — sign out is the way to leave.
    pub fn delete_device(&self, device_id: &str) -> Result<DeletedDevice, EngineError> {
        if device_id == self.inner.config.device_id {
            return Err(EngineError::Other("cannot delete this device".into()));
        }
        Ok(self.mutate(|doc| doc.delete_device(device_id))?)
    }
}

impl WorkspaceHost {
    // ── git metadata (diff-sync host writes) ────────────────────────────────

    /// HEAD-watcher reconciliation: the branch checked out at the chat's cwd.
    pub fn set_chat_branch(&self, chat_id: &str, branch: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_branch(chat_id, branch))?)
    }

    /// Retarget a chat onto another folder (mid-session switch to an existing
    /// worktree). Resume is cwd-scoped — the next run there starts fresh.
    pub fn set_chat_cwd(&self, chat_id: &str, cwd: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_cwd(chat_id, cwd))?)
    }

    /// Canonical checkout identity for the chat's cwd (diff grouping key).
    pub fn set_chat_checkout(&self, chat_id: &str, checkout_id: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_checkout(chat_id, checkout_id))?)
    }
}
