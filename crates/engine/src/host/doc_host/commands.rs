//! The command executor: what a drained ledger command does on this host.

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
