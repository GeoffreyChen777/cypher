use super::*;
use cypher_proto::sync3::{Event, Operation};
use cypher_sync::sync3::execution::{Plan, Progress};

fn engine_error(error: impl std::fmt::Display) -> EngineError {
    EngineError::Other(error.to_string())
}
fn doc_error(error: impl std::fmt::Display) -> DocError {
    DocError::Schema(error.to_string())
}

impl ChatDocHandle {
    pub fn push_message(&self, entry: &SessionMessageEntry) -> Result<(), DocError> {
        if let Some(replica) = &self.v3 {
            if replica
                .read(|j| Ok(j.projection()?.messages.contains_key(&entry.id)))
                .map_err(doc_error)?
            {
                return Ok(());
            }
            let mut writer = replica
                .read(|j| j.new_writer(j.owner()?.1.max(1), None, entry))
                .map_err(doc_error)?;
            if entry.status == Some(MessageStatus::Streaming) {
                while writer
                    .sync(&entry.parts, |frame| {
                        replica.write(|j| j.enqueue_writer_frame(frame))
                    })
                    .map_err(doc_error)?
                    .more
                {}
            } else {
                while writer
                    .finish(&entry.parts, entry.status, |frame| {
                        replica.write(|j| j.enqueue_writer_frame(frame))
                    })
                    .map_err(doc_error)?
                    .more
                {}
            }
            return Ok(());
        }
        self.doc.push_message(entry)
    }
    pub fn export_snapshot(&self) -> Result<Vec<u8>, DocError> {
        if self.v3.is_some() {
            return Err(DocError::Schema(
                "v3 sessions do not export Loro snapshots".into(),
            ));
        }
        self.doc.export_snapshot()
    }
    pub fn set_message_status(&self, id: &str, status: MessageStatus) -> Result<bool, DocError> {
        if let Some(replica) = &self.v3 {
            return replica
                .write(|j| {
                    let (_, owner_epoch) = j.owner()?;
                    j.enqueue(&Operation {
                        id: new_id(),
                        actor: self.device_id.clone(),
                        owner_epoch,
                        event: Event::MessageFinished {
                            message_id: id.into(),
                            status: Some(status),
                        },
                    })
                })
                .map(|_| true)
                .map_err(doc_error);
        }
        self.doc.set_message_status(id, status)
    }
    pub fn resolve_input(&self, request_id: &str) -> Result<bool, DocError> {
        if let Some(replica) = &self.v3 {
            for entry in self.read_entries()? {
                if entry.status != Some(MessageStatus::Streaming) {
                    continue;
                }
                for (index, mut part) in entry.parts.into_iter().enumerate() {
                    if let MessagePart::Input {
                        request_id: id,
                        resolved,
                        ..
                    } = &mut part
                    {
                        if id == request_id && !*resolved {
                            *resolved = true;
                            replica
                                .write(|j| {
                                    let (_, owner_epoch) = j.owner()?;
                                    j.enqueue(&Operation {
                                        id: new_id(),
                                        actor: self.device_id.clone(),
                                        owner_epoch,
                                        event: Event::PartPut {
                                            message_id: entry.id.clone(),
                                            index: index as u32,
                                            part: part.clone(),
                                        },
                                    })
                                })
                                .map_err(doc_error)?;
                            return Ok(true);
                        }
                    }
                }
            }
            return Ok(false);
        }
        self.doc.resolve_input(request_id)
    }
    pub fn read_entries(&self) -> Result<Vec<SessionMessageEntry>, DocError> {
        if let Some(replica) = &self.v3 {
            let mut messages: Vec<_> = replica
                .read(|j| Ok(j.projection()?.messages.into_values().collect::<Vec<_>>()))
                .map_err(doc_error)?;
            messages.sort_by_key(|m| m.created_seq);
            return Ok(messages.into_iter().map(|m| m.entry).collect());
        }
        self.doc.read_entries()
    }

    pub fn read_commands(&self) -> Result<Vec<SessionCommandEntry>, DocError> {
        if let Some(replica) = &self.v3 {
            let mut commands = replica
                .read(|j| {
                    let mut entries: HashMap<_, _> = j
                        .projection()?
                        .commands
                        .into_iter()
                        .map(|(id, c)| (id, c.command))
                        .collect();
                    for op in j.pending()? {
                        if let Event::CommandQueued {
                            command_id,
                            command,
                        } = op.event
                        {
                            entries.entry(command_id).or_insert(command);
                        }
                    }
                    Ok(entries.into_values().collect::<Vec<_>>())
                })
                .map_err(doc_error)?;
            commands.sort_by(|a, b| (a.issued_at, &a.id).cmp(&(b.issued_at, &b.id)));
            return Ok(commands);
        }
        self.doc.read_commands()
    }

    pub fn queue_command(&self, entry: &SessionCommandEntry) -> Result<(), DocError> {
        if let Some(replica) = &self.v3 {
            let epoch = replica
                .read(|j| Ok(j.owner()?.1.max(1)))
                .map_err(doc_error)?;
            replica
                .enqueue(&Operation {
                    id: format!("queue-{}", entry.id),
                    actor: entry.issued_by.clone(),
                    owner_epoch: epoch,
                    event: Event::CommandQueued {
                        command_id: entry.id.clone(),
                        command: entry.clone(),
                    },
                })
                .map_err(doc_error)?;
            self.publish_commands();
            return Ok(());
        }
        self.doc.queue_command(entry)
    }

    pub fn sealed_attachment(&self, upload_id: &str) -> Result<Option<(String, String)>, DocError> {
        if let Some(replica) = &self.v3 {
            return replica
                .read(|j| {
                    Ok(j.projection()?
                        .attachments
                        .get(upload_id)
                        .map(|a| (a.path.clone(), a.file_name.clone())))
                })
                .map_err(doc_error);
        }
        self.doc.sealed_attachment(upload_id)
    }
    pub fn seal_attachment(
        &self,
        upload_id: &str,
        path: &str,
        file_name: &str,
    ) -> Result<(), DocError> {
        if let Some(replica) = &self.v3 {
            return replica
                .write(|journal| {
                    let (owner, epoch) = journal.owner()?;
                    if owner != self.device_id || epoch == 0 {
                        return Err(cypher_sync::sync3::Error::Protocol("not_owner".into()));
                    }
                    journal.enqueue(&Operation {
                        id: format!("seal-{upload_id}"),
                        actor: self.device_id.clone(),
                        owner_epoch: epoch,
                        event: Event::AttachmentSealed {
                            upload_id: upload_id.into(),
                            path: path.into(),
                            file_name: file_name.into(),
                        },
                    })?;
                    Ok(())
                })
                .map_err(doc_error);
        }
        self.doc.seal_attachment(upload_id, path, file_name)
    }
}

impl DocHost {
    pub(super) fn open_v3(&self, chat_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        let mut handles = lock(&self.inner.handles);
        if let Some(handle) = handles.get(chat_id) {
            handle.touch();
            return Ok(handle.clone());
        }
        let owner = self
            .workspace()
            .and_then(|ws| ws.chat(chat_id).ok().flatten())
            .map(|c| c.device_id)
            .unwrap_or_else(|| self.device_id().into());
        let replica = self
            .inner
            .replicas
            .get()
            .expect("v3 store")
            .get_for_owner(chat_id, &owner)
            .map_err(engine_error)?;
        // Only a local render cache, never loaded from or persisted to old
        // snapshots. The durable authority and watch reads are v3.
        let doc = Arc::new(SessionDoc::init(chat_id)?);
        let sub = doc.doc().subscribe_root(Arc::new(|_| {}));
        let handle = Arc::new(ChatDocHandle {
            chat_id: chat_id.into(),
            device_id: self.device_id().into(),
            doc,
            messages_tx: watch::channel(Vec::new()).0,
            commands_tx: watch::channel(Vec::new()).0,
            commands_dirty: AtomicBool::new(true),
            mirror_dirty: AtomicBool::new(true),
            last_access: AtomicI64::new(now_ms()),
            snapshot_bytes: AtomicUsize::new(0),
            room_gen: AtomicU32::new(3),
            checkpointing: Arc::new(AtomicBool::new(false)),
            retired: AtomicBool::new(false),
            ephemeral: AtomicBool::new(false),
            chat2: Mutex::new(None),
            chat2_pending_local: Mutex::new(Vec::new()),
            chat2_local_sub: Mutex::new(None),
            _sub: sub,
            v3: Some(replica.clone()),
        });
        handles.insert(chat_id.into(), handle.clone());
        drop(handles);
        let host = self.clone();
        let weak = Arc::downgrade(&handle);
        self.spawn_worker(async move {
            let mut status = replica.watch();
            let mut previous = None;
            loop {
                let current = status.borrow_and_update().clone();
                let Some(handle) = weak.upgrade() else { break; };
                if previous != Some((current.cursor, current.phase)) {
                    if previous.as_ref().is_none_or(|(cursor, _)| *cursor != current.cursor) {
                        handle.publish_messages_if_watched();
                        handle.publish_commands_if_watched();
                    }
                    host.drain_commands(&handle).await;
                    previous = Some((current.cursor, current.phase));
                }
                let deadline = replica.read(|j| {
                    if current.phase != cypher_sync::sync3::Phase::Live || j.owner()?.0 != host.device_id()
                        || !host.is_host(&handle.chat_id) {
                        return Ok(None);
                    }
                    let projection = j.projection()?;
                    Ok(projection.commands.values().filter(|c| c.accepted_op_id.is_none()
                        && c.command.status == SessionCommandStatus::Pending)
                        .filter_map(|c| {
                            let attachment_deadline = match &c.command.payload {
                                SessionCommandPayload::Run { request, .. } if !request.pending_attachments.is_empty() =>
                                    Some(c.command.issued_at.saturating_add(cypher_doc::ATTACHMENT_SEAL_GRACE_MS)),
                                _ => None,
                            };
                            [c.command.expires_at, attachment_deadline].into_iter().flatten().min()
                        }).min())
                }).ok().flatten();
                drop(handle);
                let delay = deadline.map(|d| std::time::Duration::from_millis(d.saturating_sub(now_ms()).max(1) as u64));
                tokio::select! {
                    result = status.changed() => if result.is_err() { break; },
                    _ = async { if let Some(delay) = delay { tokio::time::sleep(delay).await; } else { std::future::pending::<()>().await; } } => {
                        previous = None;
                    },
                }
            }
        });
        Ok(handle)
    }

    pub(super) async fn drain_v3(&self, handle: &Arc<ChatDocHandle>) -> Result<(), EngineError> {
        let Some(sessions) = self.sessions() else {
            return Ok(());
        };
        if !self.is_host(&handle.chat_id) {
            return Ok(());
        }
        let replica = handle.v3.as_ref().expect("v3 handle");
        if replica.watch().borrow().phase != cypher_sync::sync3::Phase::Live {
            return Ok(());
        }
        // A real process can exit before its last runFinished reaches the
        // server. Retry the remembered, verified closure as commits arrive,
        // not only when a later user submits another command.
        sessions.reconcile_closed_process(&handle.chat_id)?;
        let mut after = String::new();
        loop {
            let observations = replica
                .read(|j| j.open_observations(&after, 32))
                .map_err(engine_error)?;
            if observations.is_empty() {
                break;
            }
            for run in observations {
                if !replica.has_active_observation(&run) {
                    if replica.write(|j| j.quarantine_observation(&run,
                        "Autonomous output was interrupted by engine restart. No new command was dispatched; review the retained output before continuing."))
                        .map_err(engine_error)? {
                        sessions.mark_recovery_required(&handle.chat_id);
                    }
                }
                after = run;
            }
            tokio::task::yield_now().await;
        }
        let projection = replica.read(|j| j.projection()).map_err(engine_error)?;
        let commands = handle.read_commands()?;
        let messages = handle.read_entries()?;
        for command in &commands {
            if command.status != SessionCommandStatus::Pending {
                lock(&self.inner.executing).remove(&command.id);
                continue;
            }
            // An optimistic/outbox entry cannot authorize execution.
            if !projection.commands.contains_key(&command.id) {
                continue;
            }
            if lock(&self.inner.executing).contains(&command.id) {
                continue;
            }
            if replica.has_active_claim(&command.id) {
                continue;
            }
            if let Some(info) = replica
                .read(|j| j.execution_info(&command.id))
                .map_err(engine_error)?
            {
                match info.state {
                    cypher_sync::sync3::execution::State::Settled
                    | cypher_sync::sync3::execution::State::Declined => continue,
                    cypher_sync::sync3::execution::State::Claimed => {
                        let note = "Execution outcome is uncertain after engine restart. External effects may have occurred; this request was not automatically retried. Review before explicitly continuing.";
                        replica
                            .write(|j| j.quarantine_execution(&command.id, note))
                            .map_err(engine_error)?;
                        sessions.mark_recovery_required(&handle.chat_id);
                        continue;
                    }
                    cypher_sync::sync3::execution::State::Prepared => {}
                }
            }
            let disposition = evaluate_command(
                command,
                &EvaluationContext {
                    is_processed: &|_| false,
                    now_ms: now_ms(),
                    entries: &commands,
                    current_turn_id: messages.last().map(|m| m.id.as_str()),
                    turn_is_past: &|id| messages.iter().any(|m| m.id == id),
                    sealed_attachment_path: &|id| {
                        projection.attachments.get(id).map(|a| a.path.clone())
                    },
                },
            );
            let (owner, epoch) = replica.read(|j| j.owner()).map_err(engine_error)?;
            if owner != self.device_id() {
                return Ok(());
            }
            let resolve = |status, resolution| {
                replica
                    .enqueue(&Operation {
                        id: format!("resolve-{}", command.id),
                        actor: self.device_id().into(),
                        owner_epoch: epoch,
                        event: Event::CommandResolved {
                            command_id: command.id.clone(),
                            status,
                            resolution,
                        },
                    })
                    .map_err(engine_error)
            };
            match disposition {
                CommandDisposition::Skip => continue,
                CommandDisposition::WaitForAttachments => return Ok(()),
                CommandDisposition::Expired => {
                    resolve(SessionCommandStatus::Expired, None)?;
                    continue;
                }
                CommandDisposition::Superseded => {
                    resolve(SessionCommandStatus::Superseded, None)?;
                    continue;
                }
                CommandDisposition::Execute => {}
            }
            if !lock(&self.inner.executing).insert(command.id.clone()) {
                continue;
            }
            let run_command = matches!(
                command.payload,
                SessionCommandPayload::Run { .. } | SessionCommandPayload::Steer { .. }
            );
            if run_command && !sessions.reconcile_closed_process(&handle.chat_id)? {
                lock(&self.inner.executing).remove(&command.id);
                return Ok(());
            }
            let result = if run_command {
                let output = self
                    .inner
                    .replicas
                    .get()
                    .expect("v3 store")
                    .acquire(replica.clone(), &command.id, &new_id())
                    .await
                    .map_err(engine_error);
                match output {
                    Ok(mut output) => {
                        if command
                            .expires_at
                            .is_some_and(|deadline| deadline <= now_ms())
                        {
                            output.observe(&cypher_proto::AgentEvent::Error {
                                message: "Command expired while awaiting committed execution admission".into(),
                            }).and_then(|_| output.finish_with(
                                cypher_proto::sync3::Outcome::Failed, SessionCommandStatus::Expired, None,
                            )).map_err(engine_error)?;
                            lock(&self.inner.executing).remove(&command.id);
                            continue;
                        }
                        self.execute(&sessions, handle, command, Some(output)).await
                    }
                    Err(error) => Err(error),
                }
            } else {
                let run_id = sessions.current_v3_run(&handle.chat_id);
                if matches!(command.payload, SessionCommandPayload::RespondInput { .. })
                    && run_id.is_none()
                {
                    resolve(
                        SessionCommandStatus::Rejected,
                        Some("no active input run".into()),
                    )?;
                    lock(&self.inner.executing).remove(&command.id);
                    continue;
                }
                let control = async {
                    let mut status = replica.watch();
                    replica
                        .prepare(&command.id, Plan::Control { run_id })
                        .map_err(engine_error)?;
                    let permit = loop {
                        status.borrow_and_update();
                        match replica.advance(&command.id).map_err(engine_error)? {
                            Progress::Dispatch(permit) => break permit,
                            Progress::WaitingForClaim | Progress::WaitingForRun => {}
                            _ => {
                                return Err(EngineError::Other(
                                    "control requires reconciliation".into(),
                                ));
                            }
                        }
                        status.changed().await.map_err(engine_error)?;
                    };
                    let (status, resolution) =
                        self.execute(&sessions, handle, command, None).await?;
                    replica
                        .write(|j| j.complete_execution(&permit, None, status, resolution.clone()))
                        .map_err(engine_error)?;
                    replica.finish_claim(&command.id);
                    sessions.reconcile_closed_process(&handle.chat_id)?;
                    Ok((status, resolution))
                };
                match tokio::time::timeout(std::time::Duration::from_secs(30), control).await {
                    Ok(result) => result,
                    Err(_) => Err(EngineError::Other("control commit timeout".into())),
                }
            };
            match result {
                Ok((SessionCommandStatus::Applied, _)) if run_command => {
                    // Publication owns completion. Retain the in-process
                    // claim marker until the terminal command is committed.
                }
                Ok((status, resolution)) => {
                    if run_command {
                        resolve(status, resolution)?;
                    }
                    lock(&self.inner.executing).remove(&command.id);
                }
                Err(error) => {
                    // Unknown/failed external effects are visible rejection,
                    // not permission to regenerate the command and retry.
                    if run_command {
                        let latest = replica.read(|j| j.projection()).map_err(engine_error)?;
                        if let Some(run) = latest
                            .commands
                            .get(&command.id)
                            .and_then(|c| c.run_id.as_ref())
                        {
                            if latest.runs.get(run).is_some_and(|r| r.outcome.is_none())
                                && !latest.messages.values().any(|m| {
                                    m.run_id.as_ref() == Some(run)
                                        && m.entry.status == Some(MessageStatus::Streaming)
                                })
                            {
                                replica
                                    .enqueue(&Operation {
                                        id: format!("abort-{}", command.id),
                                        actor: self.device_id().into(),
                                        owner_epoch: epoch,
                                        event: Event::RunFinished {
                                            run_id: run.clone(),
                                            outcome: cypher_proto::sync3::Outcome::Failed,
                                        },
                                    })
                                    .map_err(engine_error)?;
                            }
                        }
                    }
                    resolve(SessionCommandStatus::Rejected, Some(error.to_string()))?;
                    lock(&self.inner.executing).remove(&command.id);
                }
            }
        }
        Ok(())
    }
}
