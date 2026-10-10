//! Forking a session at a message and rewinding one, with their error
//! notices.

use super::*;

impl Shell {
    /// Session Fork idempotence: should the `(sourceChatId, anchorMessageId)`
    /// → requestId mapping survive this RPC outcome? Errors / lost replies
    /// keep it (the retry must reuse the SAME target id so the engine returns
    /// the created chat); a definitive reply (Created or typed Unavailable)
    /// drops it.
    pub(in crate::shell) fn fork_request_id_retained(
        result: &Result<cypher_proto::SessionForkResponse, cypher_rpc::RpcError>,
    ) -> bool {
        result.is_err()
    }

    /// User-facing notice text for a failed `ForkSession` RPC.
    /// `unknown method: ForkSession` means the device hosting the source
    /// chat runs an engine too old for Session Fork — the message says so
    /// and how to fix it. Every other failure gets the generic "Could not
    /// fork: …" (the full RPC error).
    pub(in crate::shell) fn fork_session_error_text(err: &cypher_rpc::RpcError) -> String {
        if let cypher_rpc::RpcError::UnknownMethod(method) = err
            && method == methods::FORK_SESSION
        {
            "Session Fork requires a newer Cypher engine on the device \
             hosting this session. Update that device or use a session \
             hosted on this device."
                .to_string()
        } else {
            format!("Could not fork: {err}")
        }
    }

    /// `ForkSession` for a settled transcript entry (Session Fork v1): mint
    /// a NEW durable root Pi chat on the source chat's host device
    /// (relay-forwarded when the source is remote). The reply's composer
    /// prefill is seeded into the fork's new tab (in the source tab's group);
    /// that tab is FOCUSED only when the user is still on the source chat —
    /// a late reply (user switched away) opens it in the background, never
    /// yanks the focus. Engine/Unavailable failures surface as a desktop
    /// notice AND the in-app sidebar notice strip. The transcript's in-flight
    /// marker (spinner + double-click guard) is begun here and ended on
    /// every settle path.
    ///
    /// Idempotence: the request id (the client-minted target chat id) is
    /// cached per `(sourceChatId, anchorMessageId)` and REUSED across RPC
    /// errors / lost replies — a retry hits the engine with the same id and
    /// returns the already-created chat instead of minting a twin. The cache
    /// entry is dropped once the RPC settles definitively (Created or typed
    /// Unavailable).
    pub(in crate::shell) fn fork_session(
        &mut self,
        sid: SlotId,
        chat_id: String,
        anchor_message_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(slot_transcript) = self.tiles.slots.get(&sid).map(|s| s.transcript.clone()) else {
            return;
        };
        slot_transcript.update(cx, |t, cx| {
            t.begin_fork(chat_id.clone(), anchor_message_id.clone());
            cx.notify();
        });
        // Unwind the in-flight marker on every settle path (the RPC result
        // or an offline pre-flight failure).
        let settle = {
            let transcript = slot_transcript.clone();
            let chat_id = chat_id.clone();
            let anchor_message_id = anchor_message_id.clone();
            move |cx: &mut Context<Shell>| {
                transcript.update(cx, |t, cx| {
                    t.end_fork(chat_id.clone(), anchor_message_id.clone());
                    cx.notify();
                });
            }
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            tracing::warn!(%chat_id, "ForkSession skipped: engine offline");
            let notice = "Cannot fork: the engine is not connected.";
            crate::shell::notify::post("Fork", notice);
            self.sidebar_notice = Some(notice.into());
            settle(cx);
            cx.notify();
            return;
        };
        let params = self.fork_request_params(&chat_id, &anchor_message_id, cx);
        let weak = cx.weak_entity();
        let state = self.state.clone();
        cx.spawn(async move |_this, cx| {
            let value = engine.client().call(methods::FORK_SESSION, params).await;
            // Always clear the in-flight marker once the RPC settles.
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |_shell, cx| {
                    settle(cx);
                    cx.notify();
                });
            }
            let result: Result<cypher_proto::SessionForkResponse, cypher_rpc::RpcError> = value
                .and_then(|v| {
                    serde_json::from_value(v)
                        .map_err(|e| cypher_rpc::RpcError::BadParams(e.to_string()))
                });
            // The (source, anchor) → requestId mapping is RETAINED on errors /
            // lost replies (the retry must reuse the SAME target id so the
            // engine returns the created chat) and DROPPED on a definitive
            // reply (Created / typed Unavailable) — the settle arms below
            // honor `retained`.
            let retained = Self::fork_request_id_retained(&result);
            let response = match result {
                Ok(response) => response,
                Err(err) => {
                    // `retained` is true here — nothing removes the mapping.
                    tracing::warn!(%chat_id, error = %err, "ForkSession failed");
                    let notice = Self::fork_session_error_text(&err);
                    crate::shell::notify::post("Fork", &notice);
                    if let Some(shell) = weak.upgrade() {
                        shell.update(cx, |shell, cx| {
                            shell.sidebar_notice = Some(notice.clone().into());
                            cx.notify();
                        });
                    }
                    return;
                }
            };
            match response {
                cypher_proto::SessionForkResponse::Created(created) => {
                    // Insert the new chat first: the fork tab's context
                    // mirrors main's list when it is created.
                    state.update(cx, |state, cx| {
                        state.insert_chat_optimistic(created.chat.clone());
                        cx.notify();
                    });
                    if let Some(shell) = weak.upgrade() {
                        shell.update(cx, |shell, cx| {
                            shell.open_created_fork(
                                &chat_id,
                                &anchor_message_id,
                                retained,
                                created,
                                cx,
                            );
                        });
                    }
                }
                cypher_proto::SessionForkResponse::Unavailable(unavailable) => {
                    // Definitive refusal: drop the idempotence mapping too.
                    if let Some(shell) = weak.upgrade() {
                        shell.update(cx, |shell, cx| {
                            if !retained {
                                shell
                                    .fork_request_ids
                                    .remove(&(chat_id.clone(), anchor_message_id.clone()));
                            }
                            cx.notify();
                        });
                    }
                    let notice = format!("Fork unavailable: {}", unavailable.message);
                    crate::shell::notify::post("Fork", &notice);
                    if let Some(shell) = weak.upgrade() {
                        shell.update(cx, |shell, cx| {
                            shell.sidebar_notice = Some(notice.clone().into());
                            cx.notify();
                        });
                    }
                }
            }
        })
        .detach();
    }

    /// The `ForkSession` params: the cached (or freshly minted) request id,
    /// the source and anchor, and the host device when it is not this one.
    fn fork_request_params(
        &mut self,
        chat_id: &str,
        anchor_message_id: &str,
        cx: &mut Context<Self>,
    ) -> serde_json::Value {
        // The request id is the client-minted TARGET chat id — reuse the
        // cached id for this (source, anchor) so a lost-reply retry returns
        // the SAME chat (idempotent); mint + remember it on first click.
        let key = (chat_id.to_string(), anchor_message_id.to_string());
        let request_id = self.fork_request_ids.get(&key).cloned().unwrap_or_else(|| {
            let id = uuid::Uuid::new_v4().to_string();
            self.fork_request_ids.insert(key, id.clone());
            id
        });
        let mut params = serde_json::Map::new();
        params.insert("requestId".into(), serde_json::Value::String(request_id));
        params.insert(
            "sourceChatId".into(),
            serde_json::Value::String(chat_id.to_string()),
        );
        params.insert(
            "anchorMessageId".into(),
            serde_json::Value::String(anchor_message_id.to_string()),
        );
        {
            let state = self.state.read(cx);
            if let (Some(chat), Some(local)) = (
                state.chats.iter().find(|c| c.id == chat_id),
                state.local_device_id.clone(),
            ) && chat.device_id != local
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(chat.device_id.clone()),
                );
            }
        }
        serde_json::Value::Object(params)
    }

    /// A fork was created: drop its idempotence mapping, open it as a tab
    /// beside its source (focused only while the user is still there) and
    /// seed its composer prefill.
    fn open_created_fork(
        &mut self,
        chat_id: &str,
        anchor_message_id: &str,
        retained: bool,
        created: cypher_proto::SessionForkCreated,
        cx: &mut Context<Self>,
    ) {
        let fork_id = created.chat.id.clone();
        let title = created
            .chat
            .title
            .clone()
            .unwrap_or_else(|| "Fork".to_string());
        // Definitive reply: this fork is settled, drop the
        // idempotence mapping (a fresh future fork mints a
        // fresh id). Errors/lost replies retain it.
        if !retained {
            self.fork_request_ids
                .remove(&(chat_id.to_string(), anchor_message_id.to_string()));
        }
        // Its row may trail the next chats frame.
        self.expect_chat(&fork_id);
        // The fork opens as a tab in the source tab's
        // group, its prefill seeded into the new tile's
        // composer. Focused only when the user is still
        // on the source — a late reply (user moved on)
        // opens it in the background, never yanks focus.
        let source = crate::workspace::TabKey::session(chat_id.to_string());
        let fork = crate::workspace::TabKey::session(fork_id.clone());
        let on_source = self.workspace.focused_tab() == Some(&source);
        let before = self.workspace.focused();
        let group = self
            .workspace
            .find(&source)
            .map(|(group, _)| group)
            .unwrap_or(before);
        let shown = self
            .workspace
            .group(group)
            .and_then(|g| g.active_tab().cloned());
        self.workspace.open_in(group, fork.clone());
        if !on_source {
            // Background: the group keeps showing what it
            // showed, and focus stays where it was.
            if let Some((g, index)) = shown.and_then(|tab| self.workspace.find(&tab)) {
                self.workspace.activate(g, index);
            }
            self.workspace.focus(before);
        } else {
            self.tiles.focus_pending = true;
        }
        self.sync_slots(cx);
        if let Some(text) = created.composer_text {
            match self
                .slot_for_tab(&fork)
                .and_then(|sid| self.tiles.slots.get(&sid))
                .map(|slot| slot.composer.clone())
            {
                Some(composer) => composer.update(cx, |composer, cx| {
                    composer.seed_draft(&fork_id, text, cx);
                }),
                // A background tab gets its slot when
                // first shown; the prefill waits with
                // the closed-tab drafts.
                None => {
                    self.closed_drafts
                        .insert(fork_id.clone(), (text, Vec::new()));
                }
            }
        }
        let notice = format!("Fork created: {title}");
        crate::shell::notify::post("Fork", &notice);
        self.workspace_changed(cx);
    }

    /// User-facing notice text for a failed `RewindSession` RPC, mirroring
    /// [`Self::fork_session_error_text`]: an `unknown method` reply means the
    /// device hosting the chat runs an engine without Session Rewind.
    pub(in crate::shell) fn rewind_session_error_text(err: &cypher_rpc::RpcError) -> String {
        if let cypher_rpc::RpcError::UnknownMethod(method) = err
            && method == methods::REWIND_SESSION
        {
            "Restarting a conversation from a message requires a newer Cypher \
             engine on the device hosting this session. Update that device or \
             use a session hosted on this device."
                .to_string()
        } else {
            format!("Could not restart the conversation: {err}")
        }
    }

    /// `RewindSession` for a settled transcript entry: restart the
    /// conversation at that anchor INSIDE the same chat — the engine deletes
    /// everything after the boundary and re-points this chat's Pi session at
    /// a truncated copy. No chat is created and the selection never moves;
    /// the transcript shrinks through the doc watch the UI is already on.
    ///
    /// The confirming click happened in the transcript (the affordance arms
    /// first), so this call is the point of no return. A USER anchor hands
    /// its text back for the composer — seeded only when the composer is
    /// empty, so a draft in progress is never clobbered.
    ///
    /// Deliberately NOT retried under an idempotence key: a lost reply leaves
    /// the engine's truncation in place (the doc watch shows it), and a blind
    /// retry would cut at the next boundary instead.
    pub(in crate::shell) fn rewind_session(
        &mut self,
        sid: SlotId,
        chat_id: String,
        anchor_message_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some((slot_transcript, composer)) = self
            .tiles
            .slots
            .get(&sid)
            .map(|s| (s.transcript.clone(), s.composer.clone()))
        else {
            return;
        };
        slot_transcript.update(cx, |t, cx| {
            t.begin_rewind(chat_id.clone(), anchor_message_id.clone());
            cx.notify();
        });
        let settle = {
            let transcript = slot_transcript.clone();
            let chat_id = chat_id.clone();
            let anchor_message_id = anchor_message_id.clone();
            move |cx: &mut Context<Shell>| {
                transcript.update(cx, |t, cx| {
                    t.end_rewind(chat_id.clone(), anchor_message_id.clone());
                    cx.notify();
                });
            }
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            tracing::warn!(%chat_id, "RewindSession skipped: engine offline");
            let notice = "Cannot restart the conversation: the engine is not connected.";
            crate::shell::notify::post("Restart", notice);
            self.sidebar_notice = Some(notice.into());
            settle(cx);
            cx.notify();
            return;
        };
        let mut params = serde_json::Map::new();
        params.insert("chatId".into(), serde_json::Value::String(chat_id.clone()));
        params.insert(
            "anchorMessageId".into(),
            serde_json::Value::String(anchor_message_id.clone()),
        );
        {
            let state = self.state.read(cx);
            if let (Some(chat), Some(local)) = (
                state.chats.iter().find(|c| c.id == chat_id),
                state.local_device_id.clone(),
            ) && chat.device_id != local
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(chat.device_id.clone()),
                );
            }
        }
        let params = serde_json::Value::Object(params);
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let value = engine.client().call(methods::REWIND_SESSION, params).await;
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |_shell, cx| {
                    settle(cx);
                    cx.notify();
                });
            }
            let result: Result<cypher_proto::SessionRewindResponse, cypher_rpc::RpcError> = value
                .and_then(|v| {
                    serde_json::from_value(v)
                        .map_err(|e| cypher_rpc::RpcError::BadParams(e.to_string()))
                });
            let notice = match result {
                Ok(cypher_proto::SessionRewindResponse::Rewound(rewound)) => {
                    if let Some(text) = rewound.composer_text {
                        composer.update(cx, |composer, cx| {
                            if composer.current_draft(cx).trim().is_empty() {
                                composer.seed_draft(&chat_id, text, cx);
                            }
                        });
                    }
                    let removed = rewound.removed_message_ids.len();
                    if removed == 1 {
                        "Conversation restarted — 1 message removed".to_string()
                    } else {
                        format!("Conversation restarted — {removed} messages removed")
                    }
                }
                Ok(cypher_proto::SessionRewindResponse::Unavailable(unavailable)) => {
                    format!("Cannot restart here: {}", unavailable.message)
                }
                Err(err) => {
                    tracing::warn!(%chat_id, error = %err, "RewindSession failed");
                    Self::rewind_session_error_text(&err)
                }
            };
            crate::shell::notify::post("Restart", &notice);
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |shell, cx| {
                    shell.sidebar_notice = Some(notice.clone().into());
                    cx.notify();
                });
            }
        })
        .detach();
    }
}
