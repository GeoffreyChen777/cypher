//! The command ledger: queueing and retrying commands, nudging a remote host,
//! draining pending commands, and what each drained command does on this host.

use super::*;

impl DocHost {
    /// Host-only outcome write (ledger rule 2).
    pub(super) fn resolve_command(
        &self,
        handle: &ChatDocHandle,
        command_id: &str,
        status: SessionCommandStatus,
        resolution: Option<&str>,
    ) {
        if let Err(err) = handle
            .doc
            .set_command_status(command_id, status, resolution)
        {
            tracing::warn!(
                chat = %handle.chat_id,
                command = %command_id,
                error = %err,
                "command outcome write failed"
            );
        }
    }

    pub(super) async fn execute(
        &self,
        sessions: &SessionsEngine,
        handle: &Arc<ChatDocHandle>,
        entry: &SessionCommandEntry,
    ) -> Result<(SessionCommandStatus, Option<String>), EngineError> {
        let chat_id = &handle.chat_id;
        match &entry.payload {
            SessionCommandPayload::Run {
                request,
                message_id,
                agent_prompt,
            } => {
                self.execute_run(sessions, handle, request, message_id, agent_prompt)
                    .await
            }
            SessionCommandPayload::Steer {
                prompt,
                message_id,
                agent_prompt,
            } => {
                self.execute_steer(sessions, chat_id, prompt, message_id, agent_prompt)
                    .await
            }
            SessionCommandPayload::Interrupt {} => {
                sessions.interrupt(chat_id).await?;
                Ok((SessionCommandStatus::Applied, None))
            }
            SessionCommandPayload::RespondInput {
                request_id,
                answers,
            } => {
                self.execute_respond_input(sessions, handle, request_id, answers)
                    .await
            }
        }
    }

    async fn execute_run(
        &self,
        sessions: &SessionsEngine,
        handle: &Arc<ChatDocHandle>,
        request: &cypher_proto::RunRequest,
        message_id: &str,
        agent_prompt: &Option<String>,
    ) -> Result<(SessionCommandStatus, Option<String>), EngineError> {
        let chat_id = &handle.chat_id;
        let mut request = request.clone();
        // Worktree directive (WorktreeSpec): materialize on THIS host at
        // drain time — the durable command plane replaces the sender's
        // old blocking CreateWorktree relay RPC, whose lost reply wedged
        // the composer on "Sending…" while the run proceeded anyway.
        // `take()` resolves the request before dispatch, so the journal
        // and steer→new-turn fallbacks reuse the created path instead of
        // minting another checkout. Creation failure Rejects the command
        // — a request for a new worktree never silently runs in the base
        // checkout.
        let fresh_worktree = match request.worktree.take() {
            Some(spec) => {
                let (cwd, fresh) = self
                    .materialize_worktree(chat_id, &spec)
                    .await
                    .map_err(|err| EngineError::Other(format!("worktree create failed: {err}")))?;
                request.cwd = cwd;
                fresh
            }
            None => None,
        };
        // Queue-first attachments: the Run was queued with PENDING
        // upload ids (never the bytes, never pending refs in the
        // transcript). The drain only executes after every id is
        // sealed in the doc, so resolve them to their final paths
        // here: `attachments` gets the paths (harness image blocks)
        // and the visible + effective prompts get the refs trailer.
        let pending_attachments = std::mem::take(&mut request.pending_attachments);
        if !pending_attachments.is_empty() {
            let mut sealed_paths = Vec::with_capacity(pending_attachments.len());
            for pending in &pending_attachments {
                let (path, _file_name) = handle
                    .doc
                    .sealed_attachment(&pending.upload_id)
                    .map_err(|err| {
                        EngineError::Other(format!(
                            "attachment seal read failed for {}: {err}",
                            pending.upload_id
                        ))
                    })?
                    .ok_or_else(|| {
                        EngineError::Other(format!(
                            "attachment {} not sealed before run",
                            pending.upload_id
                        ))
                    })?;
                sealed_paths.push(path);
            }
            request.attachments = sealed_paths;
            request.prompt = attachment_refs_trailer(&request.prompt, &request.attachments);
        }
        // The annotated (comments/session-refs) effective prompt also
        // needs the refs — the composer couldn't build them before the
        // uploads sealed. Appending the same trailer keeps the agent's
        // view identical to the visible transport.
        let effective_agent_prompt = if !request.attachments.is_empty() {
            agent_prompt
                .as_ref()
                .map(|ap| attachment_refs_trailer(ap, &request.attachments))
        } else {
            agent_prompt.clone()
        };
        // Claim-on-first-command: a run for a chat with no workspace row
        // creates the row under our device id (we are about to host it).
        if let Some(ws) = self.workspace() {
            ws.claim_chat(chat_id, Some(&request.cwd))?;
            // A pre-existing row (the client's createChat raced ahead)
            // still carries the repo folder — repoint it at the fresh
            // worktree, and stamp the actual `cypher/<name>` branch and
            // checkout identity so the footer, title-rename flow, and
            // diff grouping see the real checkout.
            if let Some(wt) = &fresh_worktree {
                if let Err(err) = ws.set_chat_cwd(chat_id, &wt.path) {
                    tracing::warn!(chat = %chat_id, error = %err, "worktree cwd stamp failed");
                }
                if let Err(err) = ws.set_chat_branch(chat_id, &wt.branch) {
                    tracing::warn!(chat = %chat_id, error = %err, "worktree branch stamp failed");
                }
                if let Some(checkout_id) = &wt.checkout_id
                    && let Err(err) = ws.set_chat_checkout(chat_id, checkout_id)
                {
                    tracing::warn!(chat = %chat_id, error = %err, "worktree checkout stamp failed");
                }
            }
        }
        let harness = self.harness_for_request(chat_id, &request);
        // A row with no config renders no harness glyph (and every
        // later dispatch falls back to the engine default), so stamp
        // what this run actually executes with. Claimed rows and
        // catalog-not-loaded createChats both land here; the racing
        // real createChat carries the same picked values.
        if let Some(ws) = self.workspace()
            && ws.chat_config(chat_id).is_none()
        {
            let config = cypher_proto::ChatConfig {
                harness,
                model: request.model.clone(),
                reasoning: request.reasoning,
                model_options: request.model_options.clone(),
                sandbox: request.sandbox,
            };
            if let Err(err) = ws.set_chat_config(chat_id, &config) {
                tracing::warn!(chat = %chat_id, error = %err, "run-config backfill failed");
            }
        }
        sessions
            .dispatch_augmented(
                chat_id,
                harness,
                request,
                effective_agent_prompt,
                Some(message_id.to_string()),
            )
            .await?;
        Ok((SessionCommandStatus::Applied, None))
    }

    async fn execute_steer(
        &self,
        sessions: &SessionsEngine,
        chat_id: &str,
        prompt: &str,
        message_id: &Option<String>,
        agent_prompt: &Option<String>,
    ) -> Result<(SessionCommandStatus, Option<String>), EngineError> {
        // The chat row carries the composer's current model pick: a
        // parked run launched with other settings ends here, so the
        // steer falls through to a fresh turn on the new model.
        let wanted = self.chat_launch_config(chat_id);
        if let Some(wanted) = &wanted {
            sessions.retire_stale_run(chat_id, wanted).await?;
        }
        match sessions
            .steer_augmented(chat_id, prompt, agent_prompt.clone(), message_id.clone())
            .await?
        {
            SteerOutcome::Accepted => Ok((SessionCommandStatus::Applied, None)),
            SteerOutcome::NotSteerable => {
                // No live steerable run: the durable command still delivers —
                // run it as the next turn. After an engine restart
                // `last_request` is empty too, so rebuild the run config from
                // the chat's workspace row; dispatch's engine-owned resume
                // then reattaches the prior harness conversation.
                let request = sessions
                    .last_request(chat_id)
                    .or_else(|| self.request_from_chat_row(chat_id, prompt));
                let Some(mut request) = request else {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("no live run and no prior run config".into()),
                    ));
                };
                request.prompt = prompt.to_string();
                // A remembered request predates any model switch.
                if let Some(wanted) = &wanted {
                    wanted.apply_to(&mut request);
                }
                request.resume = None; // dispatch re-derives the harness session
                // A reused config must not re-inline the PREVIOUS
                // turn's images; this steer's own refs (if any) already
                // ride the prompt text.
                request.attachments = Vec::new();
                let harness = self.harness_for_request(chat_id, &request);
                sessions
                    .dispatch_augmented(
                        chat_id,
                        harness,
                        request,
                        agent_prompt.clone(),
                        message_id.clone(),
                    )
                    .await?;
                Ok((
                    SessionCommandStatus::Applied,
                    Some("queued as new turn".into()),
                ))
            }
        }
    }

    async fn execute_respond_input(
        &self,
        sessions: &SessionsEngine,
        handle: &Arc<ChatDocHandle>,
        request_id: &str,
        answers: &[UserInputAnswer],
    ) -> Result<(SessionCommandStatus, Option<String>), EngineError> {
        let chat_id = &handle.chat_id;
        if sessions.respond_input(chat_id, request_id, answers.to_vec())? {
            return Ok((SessionCommandStatus::Applied, None));
        }
        // No live resolver. Only a request id the doc shows as an
        // OPEN question on a SETTLED entry gets the orphan fallback:
        // a mismatched or already-resolved id is a stale/buggy answer
        // and must still reject, and a still-streaming entry's
        // question belongs to the live run (a just-consumed resolver
        // racing a second answer must not spawn a duplicate turn).
        let questions = handle.doc.read_entries().ok().and_then(|entries| {
            entries
                .iter()
                .rev()
                .filter(|e| e.status != Some(MessageStatus::Streaming))
                .find_map(|e| {
                    e.parts.iter().find_map(|p| match p {
                        MessagePart::Input {
                            request_id: rid,
                            questions,
                            resolved: false,
                            ..
                        } if rid == request_id => Some(questions.clone()),
                        _ => None,
                    })
                })
        });
        let Some(questions) = questions else {
            return Ok((
                SessionCommandStatus::Rejected,
                Some("no pending input request".into()),
            ));
        };
        // The run died under the question (engine restart, crash).
        // The question is still open in the doc and the command is
        // durable, so honor it anyway — stamp the part resolved and
        // deliver the answers as the next (resumed) turn, the same
        // fallback a dead-run steer takes. The question UI stays up
        // until the user answers (user requirement); this is what
        // makes that answer still WORK.
        let request = sessions
            .last_request(chat_id)
            .or_else(|| self.request_from_chat_row(chat_id, ""));
        let Some(mut request) = request else {
            return Ok((
                SessionCommandStatus::Rejected,
                Some("no pending input request and no prior run config".into()),
            ));
        };
        request.prompt = respond_input_prompt(&questions, answers);
        request.resume = None; // dispatch re-derives the harness session
        request.attachments = Vec::new();
        if let Err(err) = handle.doc.resolve_input(request_id) {
            tracing::warn!(chat = %chat_id, request = %request_id, error = %err,
                "orphaned input resolve failed");
        }
        let harness = self.harness_for_request(chat_id, &request);
        sessions
            .dispatch_augmented(chat_id, harness, request, None, None)
            .await?;
        Ok((
            SessionCommandStatus::Applied,
            Some("answered as new turn".into()),
        ))
    }

    /// Create (or reuse) the isolated worktree a Run's
    /// [`cypher_proto::WorktreeSpec`] asks for, returning the resolved cwd
    /// plus the fresh worktree when one was actually created. Reuse guard: a
    /// chat whose row already points inside a linked worktree of the same
    /// repo keeps it — a duplicate Run (client retry after a lost ack, ledger
    /// reset) must not mint a second checkout.
    async fn materialize_worktree(
        &self,
        chat_id: &str,
        spec: &cypher_proto::WorktreeSpec,
    ) -> Result<(String, Option<cypher_proto::Worktree>), EngineError> {
        let repos = self
            .inner
            .repos
            .get()
            .ok_or_else(|| EngineError::Other("repos engine not wired".into()))?;
        // Reuse: the chat already runs inside a linked worktree of this repo.
        if let Some(ws) = self.workspace()
            && let Ok(Some(chat)) = ws.chat(chat_id)
            && let Some(cwd) = chat.cwd.map(|cwd| crate::git::repos::expand_home(&cwd))
            && cwd != spec.repo_path
            && repos
                .workspace_checkout(
                    std::path::Path::new(&spec.repo_path),
                    std::path::Path::new(&cwd),
                )
                .await
                .is_some()
        {
            tracing::info!(
                chat = %chat_id,
                cwd = %cwd,
                "worktree spec: reusing the chat's existing worktree"
            );
            return Ok((cwd, None));
        }
        let worktree = repos
            .ensure_chat_worktree(
                std::path::Path::new(&spec.repo_path),
                &spec.base_ref,
                chat_id,
                spec.name_hint.as_deref(),
            )
            .await?;
        tracing::info!(
            chat = %chat_id,
            path = %worktree.path,
            branch = %worktree.branch,
            "worktree materialized for run"
        );
        Ok((worktree.path.clone(), Some(worktree)))
    }

    /// The chat row's current model settings, if the row has a config.
    fn chat_launch_config(&self, chat_id: &str) -> Option<crate::session::engine::LaunchConfig> {
        let chat = self.workspace()?.chat(chat_id).ok().flatten()?;
        chat.config
            .as_ref()
            .map(crate::session::engine::LaunchConfig::of_chat)
    }

    /// A steer-turned-run with no in-process `last_request` (engine restarted
    /// since the last turn): rebuild the run config from the chat's workspace
    /// row — cwd from the row, model/reasoning/options/sandbox from its config
    /// (composer defaults otherwise). `None` without a workspace host or row.
    /// Also the RespondInput dead-run fallback's config source.
    pub(crate) fn request_from_chat_row(
        &self,
        chat_id: &str,
        prompt: &str,
    ) -> Option<cypher_proto::RunRequest> {
        let workspace = self.workspace()?;
        let chat = match workspace.chat(chat_id) {
            Ok(chat) => chat?,
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "workspace chat read failed");
                return None;
            }
        };
        let config = chat.config;
        Some(cypher_proto::RunRequest {
            prompt: prompt.to_string(),
            harness: config.as_ref().map(|c| c.harness),
            model: config.as_ref().and_then(|c| c.model.clone()),
            reasoning: config.as_ref().and_then(|c| c.reasoning),
            model_options: config
                .as_ref()
                .map(|c| c.model_options.clone())
                .unwrap_or_default(),
            // Never an empty cwd: that would spawn the run in the ENGINE's own
            // working directory instead of the chat's. A row with no cwd means
            // project-less, which is the home directory (expanded at dispatch).
            cwd: chat.cwd.unwrap_or_else(|| "~".into()),
            sandbox: config
                .as_ref()
                .map(|c| c.sandbox)
                .unwrap_or(cypher_proto::SandboxLevel::WorkspaceWrite),
            auto_approve: false,
            attachments: Vec::new(),
            pending_attachments: Vec::new(),
            resume: None,
            worktree: None,
        })
    }
}

fn same_message_identity(left: &SessionCommandPayload, right: &SessionCommandPayload) -> bool {
    match (left, right) {
        (
            SessionCommandPayload::Run {
                message_id: left, ..
            },
            SessionCommandPayload::Run {
                message_id: right, ..
            },
        ) => left == right,
        (
            SessionCommandPayload::Steer {
                message_id: Some(left),
                ..
            },
            SessionCommandPayload::Steer {
                message_id: Some(right),
                ..
            },
        ) => left == right,
        _ => false,
    }
}

impl DocHost {
    /// Composer path: append an immutable pending command entry (rule 1). Durable by
    /// construction — the change subscription kicks the drain, so a local host executes
    /// immediately and an offline doc simply holds the entry until it syncs.
    pub fn queue_command(
        &self,
        chat_id: &str,
        payload: SessionCommandPayload,
    ) -> Result<String, EngineError> {
        let handle = self.open(chat_id)?;
        let id = new_id();
        let now = now_ms();
        let based_on = handle.doc.read_entries()?.last().map(|m| CommandBasedOn {
            turn_id: Some(m.id.clone()),
            frontier: None,
        });
        let is_message = matches!(
            payload,
            SessionCommandPayload::Run { .. } | SessionCommandPayload::Steer { .. }
        );
        handle.doc.queue_command(&SessionCommandEntry {
            id: id.clone(),
            payload,
            issued_by: self.inner.config.device_id.clone(),
            issued_at: now,
            based_on,
            expires_at: Some(now + COMMAND_DEFAULT_TTL_MS),
            status: SessionCommandStatus::Pending,
            resolution: None,
            // The first attempt IS the original send: the UI uses sent_at as
            // the stable message-send clock across retries.
            sent_at: Some(now),
        })?;
        // Sending a message revives an archived chat: the user is acting in it
        // again, so the LWW row flips back to active on every device. Best-
        // effort — the command itself is durable regardless.
        if is_message && let Some(workspace) = self.workspace() {
            match workspace.chat(chat_id) {
                Ok(Some(chat)) if chat.archived => {
                    if let Err(err) = workspace.set_chat_archived(chat_id, false) {
                        tracing::warn!(chat = %chat_id, error = %err, "unarchive on send failed");
                    }
                }
                _ => {}
            }
        }
        // §7 durable delivery: when another device hosts this chat, nudge its device
        // room so a cold host opens the doc and drains the queue. Fire-and-forget —
        // the command is durable in the doc either way (a host that opens the chat
        // for any other reason still executes it).
        self.nudge_remote_host(chat_id);
        Ok(id)
    }
}

impl DocHost {
    /// Re-issue a failed or expired message command as a fresh durable
    /// attempt. The logical message id remains stable so the executor's
    /// idempotent user-entry write cannot duplicate the transcript, while the
    /// command id is new so the processed ledger does not suppress the retry.
    /// The retry inherits the ORIGINAL `sent_at` (the user's send clock) —
    /// only `issued_at` moves forward.
    pub fn retry_command(&self, chat_id: &str, command_id: &str) -> Result<String, EngineError> {
        let handle = self.open(chat_id)?;
        let commands = handle.doc.read_commands()?;
        let old = commands
            .iter()
            .find(|command| command.id == command_id)
            .cloned()
            .ok_or_else(|| EngineError::Other("command not found".into()))?;
        if !matches!(
            old.status,
            SessionCommandStatus::Rejected | SessionCommandStatus::Expired
        ) {
            return Err(EngineError::Other(
                "only failed or expired commands can be retried".into(),
            ));
        }
        if !matches!(
            &old.payload,
            SessionCommandPayload::Run { .. } | SessionCommandPayload::Steer { .. }
        ) {
            return Err(EngineError::Other(
                "only message commands can be retried".into(),
            ));
        }
        let has_live_attempt = |candidate: &SessionCommandEntry| {
            commands.iter().any(|other| {
                other.id != candidate.id
                    && other.status == SessionCommandStatus::Pending
                    && !self.inner.store.is_processed(&other.id).unwrap_or(false)
                    && same_message_identity(&other.payload, &candidate.payload)
            })
        };
        if has_live_attempt(&old) {
            return Err(EngineError::Other(
                "a retry for this message is already pending".into(),
            ));
        }
        let now = now_ms();
        let retry = SessionCommandEntry {
            id: new_id(),
            payload: old.payload,
            issued_by: self.inner.config.device_id.clone(),
            issued_at: now,
            based_on: handle
                .doc
                .read_entries()?
                .last()
                .map(|message| CommandBasedOn {
                    turn_id: Some(message.id.clone()),
                    frontier: None,
                }),
            expires_at: Some(now + COMMAND_DEFAULT_TTL_MS),
            status: SessionCommandStatus::Pending,
            resolution: None,
            // The user's original send time, not the retry's: the message's
            // place in the transcript/order is set by when it was first sent.
            sent_at: old.sent_at.or(Some(old.issued_at)),
        };
        let retry_id = retry.id.clone();
        handle.doc.queue_command(&retry)?;
        self.nudge_remote_host(chat_id);
        Ok(retry_id)
    }
}

impl DocHost {
    /// POST `{edge}/device/{host}/nudge {chatId}` when the chat's workspace row names
    /// another device as host. Best-effort: offline/edge-less engines skip silently.
    fn nudge_remote_host(&self, chat_id: &str) {
        let Some(edge) = self.inner.config.edge.clone() else {
            return;
        };
        let Some(workspace) = self.workspace() else {
            return;
        };
        let host_device = match workspace.chat(chat_id) {
            Ok(Some(chat)) => chat.device_id,
            // Unclaimed chat: whoever drains first claims it — nobody to nudge.
            _ => return,
        };
        if host_device == self.inner.config.device_id {
            return;
        }
        // Only meaningful inside a runtime (RPC handlers, executors); bare sync
        // callers (unit tests) skip rather than panic.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let url = format!(
            "{}/device/{}/nudge",
            edge.url.trim_end_matches('/'),
            host_device
        );
        let chat = chat_id.to_string();
        self.spawn_worker_on(&runtime, async move {
            // Fresh bearer per request — never the boot-time snapshot.
            let Some(bearer) = edge.bearer().await else {
                tracing::warn!(chat = %chat, "nudge skipped: signed out");
                return;
            };
            let send = reqwest::Client::new()
                .post(&url)
                .bearer_auth(&bearer)
                .json(&serde_json::json!({ "chatId": chat }))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await;
            match send {
                Ok(res) if res.status().is_success() => {
                    tracing::info!(chat = %chat, device = %host_device, "host nudged");
                }
                Ok(res) => tracing::warn!(chat = %chat, device = %host_device,
                    status = res.status().as_u16(), "nudge rejected"),
                Err(err) => {
                    tracing::warn!(chat = %chat, error = %err, "nudge failed (best-effort)")
                }
            }
        });
    }
}

impl DocHost {
    /// Drain pending commands (host-only): evaluate → mark processed BEFORE execute →
    /// execute → write the outcome as the sole outcome writer.
    pub async fn drain_commands(&self, handle: &Arc<ChatDocHandle>) {
        let Some(sessions) = self.sessions() else {
            return; // executor not wired yet (or retired); the set_sessions kick re-drains
        };
        // Temporary Side Chat docs carry no durable command ledger — the
        // side-chat manager dispatches sends directly (no SQLite processed-
        // ledger writes, no claim-on-first-command workspace row).
        if handle.is_ephemeral() {
            return;
        }
        if !self.is_host(&handle.chat_id) {
            return;
        }
        // Entries this pass decided to leave alone (processed dedupe hits).
        let mut skipped: HashSet<String> = HashSet::new();
        loop {
            let commands = match handle.doc.read_commands() {
                Ok(commands) => commands,
                Err(err) => {
                    tracing::warn!(chat = %handle.chat_id, error = %err, "command read failed");
                    return;
                }
            };
            let is_processed = |id: &str| self.inner.store.is_processed(id).unwrap_or(false);

            // Dead-command recovery: a previous process may have committed
            // the processed-ledger claim and died before writing the outcome.
            // Without this sweep every future drain sees the entry as already
            // processed and leaves it Pending forever. The in-memory
            // `executing` set excludes commands currently running in this
            // process.
            //
            // `commands` is a snapshot, and a concurrent drain may resolve a
            // command and leave `executing` after it was taken. So once a
            // candidate is out of `executing`, re-read its status: the
            // executor writes the outcome before it leaves the set, so a
            // command it finished reads resolved here, and a stale Pending
            // never overwrites Applied.
            let dead: Vec<String> = commands
                .iter()
                .filter(|command| {
                    command.status == SessionCommandStatus::Pending
                        && !skipped.contains(&command.id)
                        && is_processed(&command.id)
                        && !lock(&self.inner.executing).contains(&command.id)
                })
                .map(|command| command.id.clone())
                .collect();
            let still_pending: HashSet<String> = if dead.is_empty() {
                HashSet::new()
            } else {
                handle
                    .doc
                    .read_commands()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|command| command.status == SessionCommandStatus::Pending)
                    .map(|command| command.id)
                    .collect()
            };
            for command_id in dead {
                if !still_pending.contains(&command_id) {
                    skipped.insert(command_id);
                    continue;
                }
                tracing::warn!(
                    chat = %handle.chat_id,
                    command = %command_id,
                    "command consumed but never resolved; marking interrupted"
                );
                self.resolve_command(
                    handle,
                    &command_id,
                    SessionCommandStatus::Rejected,
                    Some("interrupted before completion — retry to send again"),
                );
                skipped.insert(command_id);
            }

            let Some(entry) = commands
                .iter()
                .find(|c| {
                    c.status == SessionCommandStatus::Pending
                        && !skipped.contains(&c.id)
                        && !is_processed(&c.id)
                })
                .cloned()
            else {
                return;
            };
            let messages = handle.doc.read_entries().unwrap_or_default();
            let current_turn_id = messages.last().map(|m| m.id.clone());
            let turn_is_past = |turn_id: &str| messages.iter().any(|m| m.id == turn_id);
            let sealed_path = |upload_id: &str| {
                handle
                    .doc
                    .sealed_attachment(upload_id)
                    .ok()
                    .flatten()
                    .map(|(path, _)| path)
            };
            let disposition = evaluate_command(
                &entry,
                &EvaluationContext {
                    is_processed: &is_processed,
                    now_ms: now_ms(),
                    entries: &commands,
                    current_turn_id: current_turn_id.as_deref(),
                    turn_is_past: &turn_is_past,
                    sealed_attachment_path: &sealed_path,
                },
            );
            // Attachments still uploading: hold WITHOUT marking processed so
            // the seal commit (which re-triggers this drain) releases the
            // Run; an expired grace window instead resolves Expired. The
            // `return` (not `continue`) also keeps later commands behind this
            // one — a newer Run must not jump a Run waiting on its uploads.
            if matches!(disposition, CommandDisposition::WaitForAttachments) {
                tracing::debug!(
                    chat = %handle.chat_id,
                    command = %entry.id,
                    "run waiting for attachment seal"
                );
                return;
            }
            // In-flight claim: a concurrent drain must not classify this
            // command as crashed while this task is between mark and resolve.
            if !lock(&self.inner.executing).insert(entry.id.clone()) {
                skipped.insert(entry.id.clone());
                continue;
            }
            // Mark BEFORE executing: a crash mid-execution must never double-run a
            // command whose side effect may already have happened.
            match self.inner.store.mark_processed(&entry.id) {
                Ok(true) => {}
                Ok(false) => {
                    lock(&self.inner.executing).remove(&entry.id);
                    skipped.insert(entry.id.clone());
                    continue;
                }
                Err(err) => {
                    lock(&self.inner.executing).remove(&entry.id);
                    tracing::error!(chat = %handle.chat_id, error = %err, "processed-ledger write failed; halting drain");
                    return;
                }
            }
            match disposition {
                CommandDisposition::Skip => {
                    skipped.insert(entry.id.clone());
                }
                CommandDisposition::Expired => {
                    self.resolve_command(handle, &entry.id, SessionCommandStatus::Expired, None);
                }
                CommandDisposition::Superseded => {
                    self.resolve_command(handle, &entry.id, SessionCommandStatus::Superseded, None);
                }
                // Returned above (before the processed-ledger mark) — the
                // seal commit re-triggers this drain.
                CommandDisposition::WaitForAttachments => {
                    lock(&self.inner.executing).remove(&entry.id);
                    skipped.insert(entry.id.clone());
                }
                CommandDisposition::Execute => {
                    let (status, resolution) = match self.execute(&sessions, handle, &entry).await {
                        Ok(outcome) => outcome,
                        Err(err) => (SessionCommandStatus::Rejected, Some(err.to_string())),
                    };
                    self.resolve_command(handle, &entry.id, status, resolution.as_deref());
                }
            }
            lock(&self.inner.executing).remove(&entry.id);
        }
    }
}
