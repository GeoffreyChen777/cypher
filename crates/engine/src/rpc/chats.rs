//! Subagent children, Side Chats and the per-chat agent-event watch.

use serde_json::Value;

use super::*;

pub(super) fn start_side_chat(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: StartSideChatParams = parse_params(params)?;
    let created = rpc
        .side_chats
        .start(&p.parent_chat_id, p.source, p.selected_text, p.origin)
        .map_err(failed)?;
    RpcReply::value(&created)
}

pub(super) async fn send_side_chat(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: SendSideChatParams = parse_params(params)?;
    rpc.side_chats
        .send(&p.side_chat_id, p.request, p.message_id)
        .await
        .map_err(failed)?;
    RpcReply::ok()
}

pub(super) async fn interrupt_side_chat(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: SideChatIdParams = parse_params(params)?;
    let interrupted = rpc
        .side_chats
        .interrupt(&p.side_chat_id)
        .await
        .map_err(failed)?;
    RpcReply::value(&serde_json::json!({ "interrupted": interrupted }))
}

pub(super) fn respond_side_chat_input(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: RespondSideChatInputParams = parse_params(params)?;
    let resolved = rpc
        .side_chats
        .respond_input(&p.side_chat_id, &p.request_id, p.answers)
        .map_err(failed)?;
    RpcReply::value(&serde_json::json!({ "resolved": resolved }))
}

pub(super) fn watch_side_chat_status(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: SideChatIdParams = parse_params(params)?;
    let rx = rpc
        .side_chats
        .watch_status(&p.side_chat_id)
        .map_err(failed)?;
    Ok(RpcReply::Stream(side_chat_status_stream(
        p.side_chat_id,
        rx,
    )))
}

pub(super) async fn dispose_side_chat(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: SideChatIdParams = parse_params(params)?;
    rpc.side_chats
        .dispose(&p.side_chat_id)
        .await
        .map_err(failed)?;
    RpcReply::ok()
}

pub(super) fn watch_agent_events(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: WatchAgentEventsParams = parse_params(params)?;
    if p.chat_id.chars().count() > 256 {
        return Err(RpcError::BadParams("chatId too long".into()));
    }
    rpc.watch_agent_events(p.chat_id, p.after_seq.unwrap_or(0))
}

impl EngineRpc {
    /// `StartSubagent` handler: validate the parent (exists + hosted locally),
    /// idempotently create the same-device child Chat with additive Cypher-owned
    /// metadata, and queue its initial durable Pi run. Strict bounded params — a
    /// bad frame is rejected before any row is written. On queue failure the
    /// child row is removed so no bogus navigable row survives. Serialized by
    /// `start_subagent_lock` so concurrent starts of the same (parent, run)
    /// cannot race the read-then-create scan.
    pub(super) async fn start_subagent(
        &self,
        params: StartSubagentParams,
    ) -> Result<RpcReply, RpcError> {
        // One `StartSubagent` at a time: the idempotence scan, the row create,
        // and the initial-run queue must be atomic relative to each other (a
        // concurrent duplicate must see the row we just created and never
        // double-queue). Everything below is synchronous, but the guard is held
        // across the whole operation for the same reason.
        let _guard = self.start_subagent_lock.lock().await;

        // Strict bounds (mirror the extension's own publisher caps + headroom).
        let bad = |msg: &str| RpcError::BadParams(msg.into());
        if params.parent_chat_id.chars().count() > 256 {
            return Err(bad("parentChatId too long"));
        }
        if params.run_id.chars().count() > 200 {
            return Err(bad("runId too long"));
        }
        if params.agent.chars().count() > 100 || params.agent.trim().is_empty() {
            return Err(bad("agent invalid"));
        }
        if params.task.chars().count() > 500 {
            return Err(bad("task too long"));
        }
        if params
            .prompt
            .as_deref()
            .is_some_and(|p| p.len() > 64 * 1024)
        {
            return Err(bad("prompt too large"));
        }
        if params.system_prompt.len() > 64 * 1024 {
            return Err(bad("systemPrompt too large"));
        }
        if params.message_root.chars().count() > 512 {
            return Err(bad("messageRoot too long"));
        }
        if params.tools.len() > 32
            || params
                .tools
                .iter()
                .any(|t| t.chars().count() > 128 || t.trim().is_empty())
        {
            return Err(bad("tools invalid"));
        }
        if params
            .cwd
            .as_deref()
            .is_some_and(|c| c.chars().count() > 1024)
        {
            return Err(bad("cwd too long"));
        }
        if params
            .address
            .as_deref()
            .is_some_and(|a| a.chars().count() > 256 || a.trim().is_empty())
        {
            return Err(bad("address invalid"));
        }
        if params.child_index > 32 {
            return Err(bad("childIndex too large"));
        }
        if params
            .model
            .as_deref()
            .is_some_and(|m| m.chars().count() > 200 || m.trim().is_empty())
        {
            return Err(bad("model invalid"));
        }
        if params
            .thinking
            .as_deref()
            .is_some_and(|t| t.chars().count() > 32 || t.trim().is_empty())
        {
            return Err(bad("thinking invalid"));
        }

        let parent = self
            .workspace
            .chat(&params.parent_chat_id)
            .map_err(failed)?
            .ok_or_else(|| RpcError::Failed("parent chat not found".into()))?;
        if parent.device_id != self.doc_host.device_id() {
            return Err(RpcError::Failed(
                "parent chat is not hosted on this device".into(),
            ));
        }

        let title = if params.task.trim().is_empty() {
            format!("{} subagent", params.agent)
        } else {
            let head: String = params.task.trim().chars().take(60).collect();
            format!("{} · {}", params.agent, head)
        };
        let profile = ChildAgentProfile {
            system_prompt: params.system_prompt.clone(),
            tools: params.tools.clone(),
            model: params.model.clone(),
            thinking: params.thinking.clone(),
        };
        // One resolved cwd for BOTH the persisted row and the initial run, so a
        // child's second turn never silently drifts back to the parent's folder.
        let child_cwd = params
            .cwd
            .clone()
            .filter(|c| !c.trim().is_empty())
            .or_else(|| parent.cwd.clone());
        let child_chat_id = self
            .workspace
            .create_child_chat(
                &parent,
                &params.run_id,
                &params.agent,
                &params.task,
                params.mode,
                params.tool_call_id.clone(),
                profile,
                &title,
                child_cwd.clone(),
            )
            .map_err(failed)?;
        let child_id = child_chat_id.id().to_string();

        // Does the initial Run still need to be queued? A FRESH child always
        // does. An EXISTING child does only when its row is an orphan — no
        // Pending/Applied Run in the durable ledger AND no dispatch evidence
        // (a message or harness session on the row). That is exactly the
        // crash-gap state (row created, process died before queueing) or a
        // child whose only run command was rejected/expired/cancelled.
        // Ordinary retries — the initial run already queued or already
        // dispatched — return the id WITHOUT queueing a second Run.
        let needs_initial_run = if child_chat_id.created() {
            true
        } else {
            !self.child_initial_run_evident(&child_id)?
        };

        if needs_initial_run {
            // Remember the messaging channel LOCALLY (never synced — the
            // channel root is an absolute host-local path) so the initial
            // queued run below can reach the parent's message root. Consumed at
            // first dispatch; later child turns have no channel.
            self.sessions.register_child_channel(
                &child_id,
                &params.message_root,
                params.child_index,
                params.address.as_deref().filter(|a| *a != params.agent),
            );

            // The normal durable Run command (idempotent by child chat + message id;
            // the engine's own executor picks it up and dispatches through the pi
            // harness with the child's persisted profile + local messaging channel).
            let task = params
                .prompt
                .as_deref()
                .filter(|p| !p.trim().is_empty())
                .unwrap_or(&params.task);
            let request = RunRequest {
                prompt: if task.trim().is_empty() {
                    "Task: (no description provided)".to_string()
                } else {
                    format!("Task: {task}")
                },
                harness: Some(HarnessId::Pi),
                model: params.model.clone(),
                reasoning: None,
                model_options: Default::default(),
                cwd: child_cwd.unwrap_or_else(|| "~".into()),
                sandbox: parent
                    .config
                    .as_ref()
                    .map(|c| c.sandbox)
                    .unwrap_or(cypher_proto::SandboxLevel::WorkspaceWrite),
                auto_approve: false,
                resume: None,
                worktree: None,
                attachments: Vec::new(),
                pending_attachments: Vec::new(),
            };
            if let Err(err) = self.doc_host.queue_command(
                &child_id,
                SessionCommandPayload::Run {
                    request,
                    message_id: crate::new_id(),
                    agent_prompt: None,
                },
            ) {
                // Rollback: no bogus navigable row — the queue is what makes the
                // child real. The local channel entry goes with it.
                let _ = self.workspace.delete_chat(&child_id);
                self.sessions.remove_child_channel(&child_id);
                return Err(RpcError::Failed(format!("child start queue failed: {err}")));
            }
        }
        RpcReply::value(&serde_json::json!({
            "childChatId": child_id
        }))
    }

    /// Does an EXISTING child already show its initial run was queued or
    /// dispatched? True when the durable command ledger holds a Pending or
    /// Applied Run command, or the chat row carries dispatch evidence
    /// (`last_message_at` set, or a harness session id recorded). False for an
    /// orphan row — the crash gap between row creation and queueing, or a child
    /// whose only run command was rejected/expired/cancelled — which the caller
    /// then recovers by registering the fresh channel and queueing exactly one
    /// initial Run.
    fn child_initial_run_evident(&self, child_id: &str) -> Result<bool, RpcError> {
        if let Some(chat) = self.workspace.chat(child_id).map_err(failed)?
            && (chat.last_message_at.is_some() || chat.harness_session_id.is_some())
        {
            // A message or a harness session means the initial run dispatched —
            // it must have been queued to dispatch.
            return Ok(true);
        }
        match self.doc_host.open(child_id) {
            Ok(handle) => {
                let commands = handle.doc().read_commands().map_err(failed)?;
                Ok(commands.iter().any(|c| {
                    matches!(c.payload, SessionCommandPayload::Run { .. })
                        && matches!(
                            c.status,
                            SessionCommandStatus::Pending | SessionCommandStatus::Applied
                        )
                }))
            }
            // No (readable) ledger — treat as an orphan: the caller recovers by
            // queueing the initial Run rather than leaving the row stuck.
            Err(_) => Ok(false),
        }
    }

    /// `WatchAgentEvents` handler: replayable per-chat agent events from the
    /// sessions journal/hub (journal replay after `afterSeq`, then live). The
    /// parent extension subscribes to the child chat and maps terminal
    /// `Done.result` back into its own result semantics.
    pub(super) fn watch_agent_events(
        &self,
        chat_id: String,
        after_seq: u64,
    ) -> Result<RpcReply, RpcError> {
        let (replay, rx) = self
            .sessions
            .subscribe(&chat_id, after_seq)
            .map_err(failed)?;
        // The hub subscription opens before the journal is read, so an event
        // published in between is in both; the live leg starts after the
        // newest replayed seq (a doubled text delta would double the text).
        let replayed_through = replay.last().map(|entry| entry.seq).unwrap_or(0);
        let replay = futures::stream::iter(
            replay
                .into_iter()
                .map(|entry| serde_json::to_value(&entry.event).map_err(failed)),
        );
        // Journaled events are tagged JSON (`AgentEvent`'s own serde); the
        // live hub carries the same shape. A lagging subscriber skips the
        // deltas it missed rather than ending the stream: the parent extension
        // treats an ended stream as a lost child, and the terminal `Done` it
        // waits for is still ahead of it.
        let live = futures::stream::unfold(rx, move |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(entry) if entry.seq != 0 && entry.seq <= replayed_through => continue,
                    Ok(entry) => {
                        let value = serde_json::to_value(&entry.event).map_err(failed);
                        return Some((value, rx));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        let stream = replay
            .chain(live)
            .filter_map({
                let chat_id = chat_id.clone();
                move |item: Result<serde_json::Value, RpcError>| {
                    let chat = chat_id.clone();
                    async move {
                        match item {
                            Ok(value) => Some(value),
                            Err(err) => {
                                tracing::warn!(chat = %chat, error = %err, "watch agent events serialization failed");
                                None
                            }
                        }
                    }
                }
            })
            .boxed();
        Ok(RpcReply::Stream(stream))
    }
}

/// The Side Chat status watch as a stream: current value first (`null` until
/// the first transition), then every change. Ends when the private channel
/// closes — after dispose, or at promotion when the panel switches to the
/// normal chat surface.
fn side_chat_status_stream(
    side_chat_id: String,
    rx: watch::Receiver<Option<cypher_proto::Session>>,
) -> BoxStream<'static, serde_json::Value> {
    use cypher_proto::SideChatStatus;
    futures::stream::unfold(
        (side_chat_id, rx, false),
        |(side_chat_id, mut rx, emitted)| async move {
            if emitted {
                rx.changed().await.ok()?;
            }
            let frame = {
                let session = rx.borrow_and_update().clone();
                session.map(|s| SideChatStatus {
                    side_chat_id: side_chat_id.clone(),
                    status: s.status,
                    started_at: s.started_at,
                    updated_at: s.updated_at,
                })
            };
            let value = serde_json::to_value(&frame).ok()?;
            Some((value, (side_chat_id, rx, true)))
        },
    )
    .boxed()
}
