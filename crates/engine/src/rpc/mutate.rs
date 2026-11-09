//! `Mutate`: workspace-doc row edits (chats, spaces, devices).

use super::*;

impl EngineRpc {
    /// This device is a receive-only member of its trust group.
    fn local_receive_only(&self) -> bool {
        self.trust
            .as_ref()
            .is_some_and(|trust| trust.member_is_receive_only(self.doc_host.device_id()))
    }

    pub(super) fn mutate(&self, params: MutateParams) -> Result<(), RpcError> {
        match params {
            MutateParams::CreateChat {
                chat_id,
                space_id,
                device_id,
                config,
                branch,
                cwd,
            } => {
                self.workspace
                    .create_chat(
                        &chat_id,
                        space_id.as_deref(),
                        device_id.as_deref(),
                        config,
                        cwd,
                    )
                    .map_err(failed)?;
                if let Some(branch) = branch.as_deref().filter(|b| !b.is_empty()) {
                    self.workspace
                        .set_chat_branch(&chat_id, branch)
                        .map_err(failed)?;
                }
                // Sign where it runs and how, as created here (a retried
                // create adds nothing).
                self.doc_host.record_chat_launch(&chat_id, true, true, true);
                Ok(())
            }
            MutateParams::CreateSpace {
                space_id,
                device_id,
                path,
                name,
                git_detected,
            } => self
                .workspace
                .create_space(&space_id, &device_id, &path, name, git_detected)
                .map_err(failed),
            MutateParams::RenameSpace { space_id, name } => self
                .workspace
                .rename_space(&space_id, name.as_deref())
                .map_err(failed)
                .map(drop),
            MutateParams::DeleteSpace { space_id } => {
                // The rows are read first: their pi session files are deleted
                // after the rows go.
                let rows = self.workspace.read_chats().map_err(failed)?;
                let deleted = self.workspace.delete_space(&space_id).map_err(failed)?;
                // Best-effort teardown of live runs we host for the deleted chats
                // (the doc rows are already tombstoned; a straggler run would only
                // write into an orphaned session doc).
                let sessions = self.sessions.clone();
                let doc_host = self.doc_host.clone();
                let forks = self.session_forks.clone();
                let chat_ids = deleted.chat_ids;
                tokio::spawn(async move {
                    for chat_id in chat_ids {
                        if let Err(err) = sessions.interrupt(&chat_id).await {
                            tracing::debug!(chat = %chat_id, error = %err, "deleteSpace interrupt skipped");
                        }
                        doc_host.purge_chat(&chat_id);
                        if let Some(chat) = rows.iter().find(|c| c.id == chat_id) {
                            forks.delete_chat_session(chat);
                        }
                    }
                });
                Ok(())
            }
            MutateParams::RenameChat { chat_id, title } => self
                .workspace
                .rename_chat(&chat_id, &title)
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatBranch { chat_id, branch } => self
                .workspace
                .set_chat_branch(&chat_id, &branch)
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatCwd { chat_id, cwd } => {
                self.workspace
                    .set_chat_cwd(&chat_id, &cwd)
                    .map_err(failed)?;
                self.doc_host
                    .record_chat_launch(&chat_id, true, false, false);
                Ok(())
            }
            MutateParams::SetChatActivity {
                chat_id,
                last_message_at,
                created_at,
            } => self
                .workspace
                .set_chat_activity(&chat_id, last_message_at, created_at)
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatHost { chat_id, device_id } => {
                self.workspace
                    .set_chat_host(&chat_id, &device_id)
                    .map_err(failed)?;
                let cwd = self
                    .workspace
                    .chat(&chat_id)
                    .ok()
                    .flatten()
                    .and_then(|c| c.cwd);
                self.doc_host
                    .record_launch(&chat_id, &cypher_doc::LaunchFact::Host { device_id, cwd })
                    .map_err(failed)?;
                Ok(())
            }
            MutateParams::SetChatArchived { chat_id, archived } => self
                .workspace
                .set_chat_archived(&chat_id, archived)
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatPinned { chat_id, pinned } => self
                .workspace
                .set_chat_pinned(&chat_id, pinned)
                .map_err(failed)
                .map(drop),
            MutateParams::SetSpacePinned { space_id, pinned } => self
                .workspace
                .set_space_pinned(&space_id, pinned)
                .map_err(failed)
                .map(drop),
            MutateParams::SetSpaceAppearance {
                space_id,
                icon,
                color,
            } => self
                .workspace
                .set_space_appearance(&space_id, icon.as_deref(), color.as_deref())
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatConfig { chat_id, config } => {
                self.workspace
                    .set_chat_config(&chat_id, &config)
                    .map_err(failed)?;
                self.doc_host
                    .record_chat_launch(&chat_id, false, true, false);
                Ok(())
            }
            MutateParams::DeleteChat { chat_id } => {
                // Parent delete CASCADES to its Cypher child chats (rows + docs +
                // session interruption): a deleted parent must not leave orphaned
                // navigable child rows behind (no dangling navigation). Child rows
                // are tombstoned in the same mutation, then torn down async.
                let chat = self.workspace.chat(&chat_id).map_err(failed)?;
                let children = self.workspace.child_chats(&chat_id).map_err(failed)?;
                self.workspace.delete_chat(&chat_id).map_err(failed)?;
                self.doc_host.purge_chat(&chat_id);
                let sessions = self.sessions.clone();
                let doc_host = self.doc_host.clone();
                let workspace = self.workspace.clone();
                let forks = self.session_forks.clone();
                tokio::spawn(async move {
                    // The pi session file goes with the chat (on its host).
                    // Its run stops first: a live pi child would write the
                    // file back on its next append.
                    if let Some(chat) = chat.filter(|c| c.device_id == workspace.device_id()) {
                        if let Err(err) = sessions.interrupt(&chat.id).await {
                            tracing::debug!(chat = %chat.id, error = %err, "deleteChat interrupt skipped");
                        }
                        forks.delete_chat_session(&chat);
                    }
                    for child in children {
                        // Interrupt first so the run settles and its
                        // terminal bookkeeping lands BEFORE the row goes
                        // (a live run's late claim could otherwise
                        // resurrect the tombstoned row).
                        if let Err(err) = sessions.interrupt(&child.id).await {
                            tracing::debug!(chat = %child.id, error = %err, "child interrupt skipped");
                        }
                        // The host-local channel entry (initial-run only)
                        // dies with the row.
                        sessions.remove_child_channel(&child.id);
                        let _ = workspace.delete_chat(&child.id);
                        doc_host.purge_chat(&child.id);
                        forks.delete_chat_session(&child);
                    }
                });
                Ok(())
            }
            MutateParams::RenameDevice { device_id, name } => {
                if device_id != self.doc_host.device_id() && self.local_receive_only() {
                    return Err(RpcError::Failed(
                        "This device is receive-only, so it can only rename itself.".into(),
                    ));
                }
                self.workspace
                    .rename_device(&device_id, &name)
                    .map_err(failed)
                    .map(drop)
            }
            MutateParams::DeleteDevice { device_id } => {
                // Removing signs that machine out: a receive-only device
                // doesn't get to do that to the devices that control it.
                if self.local_receive_only() {
                    return Err(RpcError::Failed(
                        "This device is receive-only, so it can't remove your other devices."
                            .into(),
                    ));
                }
                self.workspace
                    .delete_device(&device_id)
                    .map_err(failed)
                    .map(drop)
            }
            #[cfg(feature = "development")]
            MutateParams::SeedBotRecord {
                kind,
                id,
                fields,
                delete,
            } => {
                if !cypher_proto::BOT_RECORD_KINDS.contains(&kind.as_str()) {
                    return Err(RpcError::BadParams(format!(
                        "not a bot record kind: {kind}"
                    )));
                }
                if delete {
                    self.workspace.delete_records(&kind, &[id]);
                    return Ok(());
                }
                self.workspace
                    .put_record(
                        &kind,
                        &id,
                        &serde_json::Value::Object(fields),
                        crate::host::workspace_host::Fields::All,
                    )
                    .map_err(failed)
            }
            #[cfg(feature = "development")]
            MutateParams::SeedDevice {
                device_id,
                name,
                platform,
                last_seen_at,
            } => self
                .workspace
                .seed_device(
                    &device_id,
                    &name,
                    &platform,
                    last_seen_at.and_then(chrono::DateTime::from_timestamp_millis),
                )
                .map_err(failed),
            MutateParams::UpdateBot {
                bot_id,
                name,
                icon,
                color,
                instructions,
                scope,
                capabilities,
            } => {
                let name = name.trim().to_string();
                cypher_proto::validate_name(&name).map_err(RpcError::BadParams)?;
                scope.validate().map_err(RpcError::BadParams)?;
                let instructions = instructions
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                if instructions
                    .as_deref()
                    .is_some_and(|s| s.chars().count() > cypher_proto::BOT_INSTRUCTIONS_MAX_CHARS)
                {
                    return Err(RpcError::BadParams("The instructions are too long.".into()));
                }
                let update = cypher_doc::BotUpdate {
                    name: Some(name),
                    icon: Some(icon.filter(|s| !s.is_empty())),
                    color: Some(color.filter(|s| !s.is_empty())),
                    instructions: Some(instructions),
                    scope: Some(scope),
                    capabilities,
                    paused: None,
                };
                if !self
                    .workspace
                    .update_bot(&bot_id, &update)
                    .map_err(failed)?
                {
                    return Err(RpcError::Failed("No such bot.".into()));
                }
                Ok(())
            }
            MutateParams::SetBotPaused { bot_id, paused } => {
                let update = cypher_doc::BotUpdate {
                    paused: Some(paused),
                    ..Default::default()
                };
                if !self
                    .workspace
                    .update_bot(&bot_id, &update)
                    .map_err(failed)?
                {
                    return Err(RpcError::Failed("No such bot.".into()));
                }
                Ok(())
            }
            record @ (MutateParams::ResolveBotPlan { .. }
            | MutateParams::StopBotWork { .. }
            | MutateParams::DeleteBotWork { .. }
            | MutateParams::ResolveBotApproval { .. }
            | MutateParams::SaveBotRoutine { .. }
            | MutateParams::DeleteBotRoutine { .. }
            | MutateParams::RunBotRoutine { .. }
            | MutateParams::SaveBotShortcut { .. }
            | MutateParams::DeleteBotShortcut { .. }
            | MutateParams::OrderBotShortcuts { .. }
            | MutateParams::UpdateBotConnection { .. }
            | MutateParams::RevokeBotConnection { .. }
            | MutateParams::RevokeBotInvite { .. }
            | MutateParams::ReviewBotExchange { .. }) => self.mutate_bot_record(record),
            MutateParams::DeleteBot { bot_id } => {
                let bot = self.workspace.bot(&bot_id).map_err(failed)?;
                let Some(chat_ids) = self.workspace.delete_bot(&bot_id).map_err(failed)? else {
                    return Ok(());
                };
                for chat_id in &chat_ids {
                    self.doc_host.purge_chat(chat_id);
                }
                if bot.is_some_and(|b| b.host_device_id == self.doc_host.device_id()) {
                    let sessions = self.sessions.clone();
                    let bots = self.bots.clone();
                    tokio::spawn(async move {
                        for chat_id in chat_ids {
                            if let Err(err) = sessions.interrupt(&chat_id).await {
                                tracing::debug!(chat = %chat_id, error = %err, "bot interrupt skipped");
                            }
                        }
                        if let Some(bots) = bots {
                            bots.remove_folder(&bot_id);
                        }
                    });
                }
                Ok(())
            }
            MutateParams::MarkChatSeen { chat_id, at } => {
                let at = at
                    .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                    .unwrap_or_else(chrono::Utc::now);
                self.workspace
                    .mark_chat_seen(&chat_id, at)
                    .map_err(failed)
                    .map(drop)
            }
        }
    }
}
