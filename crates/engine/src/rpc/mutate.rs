//! `Mutate`: workspace-doc row edits (chats, spaces, devices).

use super::*;

impl EngineRpc {
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
                let deleted = self.workspace.delete_space(&space_id).map_err(failed)?;
                // Best-effort teardown of live runs we host for the deleted chats
                // (the doc rows are already tombstoned; a straggler run would only
                // write into an orphaned session doc).
                let sessions = self.sessions.clone();
                let doc_host = self.doc_host.clone();
                let chat_ids = deleted.chat_ids;
                tokio::spawn(async move {
                    for chat_id in chat_ids {
                        if let Err(err) = sessions.interrupt(&chat_id).await {
                            tracing::debug!(chat = %chat_id, error = %err, "deleteSpace interrupt skipped");
                        }
                        doc_host.purge_chat(&chat_id);
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
            MutateParams::SetChatCwd { chat_id, cwd } => self
                .workspace
                .set_chat_cwd(&chat_id, &cwd)
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatActivity {
                chat_id,
                last_message_at,
                created_at,
            } => self
                .workspace
                .set_chat_activity(&chat_id, last_message_at, created_at)
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatHost { chat_id, device_id } => self
                .workspace
                .set_chat_host(&chat_id, &device_id)
                .map_err(failed)
                .map(drop),
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
            MutateParams::SetChatConfig { chat_id, config } => self
                .workspace
                .set_chat_config(&chat_id, &config)
                .map_err(failed)
                .map(drop),
            MutateParams::DeleteChat { chat_id } => {
                // Parent delete CASCADES to its Cypher child chats (rows + docs +
                // session interruption): a deleted parent must not leave orphaned
                // navigable child rows behind (no dangling navigation). Child rows
                // are tombstoned in the same mutation, then torn down async.
                let children = self.workspace.child_chats(&chat_id).map_err(failed)?;
                self.workspace.delete_chat(&chat_id).map_err(failed)?;
                self.doc_host.purge_chat(&chat_id);
                if !children.is_empty() {
                    let sessions = self.sessions.clone();
                    let doc_host = self.doc_host.clone();
                    let workspace = self.workspace.clone();
                    tokio::spawn(async move {
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
                        }
                    });
                }
                Ok(())
            }
            MutateParams::RenameDevice { device_id, name } => self
                .workspace
                .rename_device(&device_id, &name)
                .map_err(failed)
                .map(drop),
            MutateParams::DeleteDevice { device_id } => self
                .workspace
                .delete_device(&device_id)
                .map_err(failed)
                .map(drop),
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
