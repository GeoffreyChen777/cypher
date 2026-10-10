//! Side chats: opening one beside its session, registering and promoting
//! its tab, and closing it.

use super::*;

impl Shell {
    // ---- temporary Side Chats ----

    /// User-facing notice text for a failed `StartSideChat` RPC.
    ///
    /// `unknown method: StartSideChat` means the device hosting the parent
    /// session still runs a Cypher engine older than the Side Chat feature
    /// — the message says so and how to fix it. Every other failure gets
    /// the generic "Could not open Side Chat: …" (the full RPC error,
    /// e.g. an engine-side `Failed` message).
    pub(in crate::shell) fn side_chat_start_error_text(err: &cypher_rpc::RpcError) -> String {
        if let cypher_rpc::RpcError::UnknownMethod(method) = err
            && method == methods::START_SIDE_CHAT
        {
            "Side Chat requires a newer Cypher engine on the device hosting \
             this session. Update that device or use a session hosted on \
             this device."
                .to_string()
        } else {
            format!("Could not open Side Chat: {err}")
        }
    }

    /// `StartSideChat` for a settled selection: mint the temporary chat on
    /// the engine (relay-forwarded when the parent chat is remote — the
    /// parent's host device owns the side chat), then open its right-pane
    /// tab. `selected_text` is the settled quote IN FULL (the engine
    /// validates it — empty/oversized are rejected there — and injects it
    /// into the first send). Capped at [`MAX_SIDE_CHATS_PER_CHAT`] per chat
    /// as a UX guard (the ENGINE enforces the global cap authoritatively).
    /// Start/cap/offline failures surface as a desktop notice AND the
    /// in-app sidebar notice strip, never a silent return.
    pub(in crate::shell) fn open_side_chat(
        &mut self,
        sid: SlotId,
        parent_chat_id: String,
        source: cypher_proto::SideChatSource,
        selected_text: String,
        origin: Option<cypher_proto::agent_prompt::AgentQuote>,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.tiles.slots.get(&sid) else {
            return;
        };
        let open = slot
            .dock
            .surfaces
            .iter()
            .filter(|s| matches!(s, DockSurface::SideChat(_)))
            .count();
        if open >= MAX_SIDE_CHATS_PER_CHAT {
            tracing::warn!(%parent_chat_id, "Side Chat tab cap reached per chat");
            let notice = "Too many side chats open for this chat (max 8).";
            crate::shell::notify::post("Side Chat", notice);
            self.sidebar.notice = Some(notice.into());
            cx.notify();
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            tracing::warn!(%parent_chat_id, "StartSideChat skipped: engine offline");
            let notice = "Cannot open a side chat: engine is not connected.";
            crate::shell::notify::post("Side Chat", notice);
            self.sidebar.notice = Some(notice.into());
            cx.notify();
            return;
        };
        // Remote parent: the side chat is owned by the PARENT'S host device
        // (relay-forwarded). The reply's `targetDeviceId` then rides every
        // subsequent side-chat RPC.
        let mut params = serde_json::Map::new();
        params.insert(
            "parentChatId".into(),
            serde_json::Value::String(parent_chat_id.clone()),
        );
        params.insert(
            "source".into(),
            serde_json::to_value(&source).unwrap_or_default(),
        );
        params.insert(
            "selectedText".into(),
            serde_json::Value::String(selected_text.clone()),
        );
        // What the selection stands for in the agent's own words, when it
        // was taken from a displayed translation.
        if let Some(origin) = &origin {
            params.insert(
                "origin".into(),
                serde_json::to_value(origin).unwrap_or_default(),
            );
        }
        {
            let state = self.state.read(cx);
            if let (Some(chat), Some(local)) = (
                state.chats.iter().find(|c| c.id == parent_chat_id),
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
        let quote = selected_text;
        cx.spawn(async move |_this, cx| {
            let value = engine.client().call(methods::START_SIDE_CHAT, params).await;
            let created: cypher_proto::SideChatCreated = match value.and_then(|v| {
                serde_json::from_value(v)
                    .map_err(|e| cypher_rpc::RpcError::BadParams(e.to_string()))
            }) {
                Ok(created) => created,
                Err(err) => {
                    tracing::warn!(%parent_chat_id, error = %err, "StartSideChat failed");
                    let notice = Self::side_chat_start_error_text(&err);
                    crate::shell::notify::post("Side Chat", &notice);
                    if let Some(shell) = weak.upgrade() {
                        shell.update(cx, |shell, cx| {
                            shell.sidebar.notice = Some(notice.clone().into());
                            cx.notify();
                        });
                    }
                    return;
                }
            };
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |shell, cx| {
                    shell.register_side_chat_tab(sid, created, source.clone(), quote.clone(), cx)
                });
            }
        })
        .detach();
    }

    /// A successful `StartSideChat` lands here: build the panel, subscribe to
    /// its events, and open the tab.
    ///
    /// Race guard: the START round-trip may outlive the
    /// parent's tile (closed, or its context moved to another chat). A side
    /// chat belongs to its source parent's dock — attaching it anywhere else
    /// would mis-scope the tab, so the created temp is disposed immediately
    /// and nothing is opened.
    fn register_side_chat_tab(
        &mut self,
        sid: SlotId,
        created: cypher_proto::SideChatCreated,
        source: cypher_proto::SideChatSource,
        selected_text: String,
        cx: &mut Context<Self>,
    ) {
        let parent_chat_id = created.parent_chat_id.clone();
        let slot_chat = self
            .tiles
            .slots
            .get(&sid)
            .and_then(|slot| slot.state.read(cx).selected_chat.clone());
        if slot_chat.as_deref() != Some(parent_chat_id.as_str()) {
            tracing::warn!(
                parent = %parent_chat_id,
                switched_to = ?slot_chat,
                side_chat = %created.side_chat_id,
                "StartSideChat returned after the user switched away; disposing the temp"
            );
            let mut params = serde_json::Map::new();
            params.insert(
                "sideChatId".into(),
                serde_json::Value::String(created.side_chat_id.clone()),
            );
            let state = self.state.read(cx);
            if let Some(local) = state.local_device_id.clone()
                && created.target_device_id != local
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(created.target_device_id.clone()),
                );
            }
            let engine = self.state.read(cx).engine().cloned();
            if let Some(engine) = engine {
                cx.spawn(async move |_, _| {
                    let _ = engine
                        .client()
                        .call(
                            methods::DISPOSE_SIDE_CHAT,
                            serde_json::Value::Object(params),
                        )
                        .await;
                })
                .detach();
            }
            return;
        }
        let Some(slot_state) = self.tiles.slots.get(&sid).map(|slot| slot.state.clone()) else {
            return;
        };
        let panel = cx.new(|cx| {
            crate::side_chats::SideChatPanel::new(
                slot_state,
                parent_chat_id.clone(),
                created.side_chat_id.clone(),
                created.target_device_id.clone(),
                source,
                selected_text,
                cx,
            )
        });
        let Some(slot) = self.tiles.slots.get_mut(&sid) else {
            return;
        };
        slot.side_chat_seq += 1;
        let id = slot.side_chat_seq;
        let sub = cx.subscribe(&panel, move |this: &mut Self, _, event, cx| match event {
            crate::side_chats::SideChatEvent::Promoted {
                chat_id,
                side_chat_id,
            } => {
                this.promote_side_chat(sid, chat_id.clone(), side_chat_id.clone(), cx);
            }
        });
        slot.side_chats.insert(id, panel);
        slot.side_chat_subs.insert(id, sub);
        slot.dock.surfaces.push(DockSurface::SideChat(id));
        self.set_dock_active(sid, DockSurface::SideChat(id), cx);
        // Opening a side chat implies the dock is showing it — at its NORMAL
        // width (never a takeover/expanded dock: the conversation stays
        // visible beside the side chat).
        if let Some(slot) = self.tiles.slots.get_mut(&sid) {
            slot.dock.expanded = false;
            slot.dock.open = true;
        }
        cx.notify();
    }

    /// Expand: the side chat is now a normal root chat. Capture the panel's
    /// fork + composer + parent BEFORE the tab closes (close drops the
    /// panel) — the promoted row inherits the parent's device/space/cwd/
    /// config, and the fork's transcript/echoes/draft/staged attachments
    /// ride into the promoted chat's NEW TAB (same group as the parent's)
    /// so the switch is seamless (no blank flash, no lost draft). Promotion
    /// only opens the tab AFTER the RPC succeeded — this handler runs on
    /// that.
    fn promote_side_chat(
        &mut self,
        sid: SlotId,
        chat_id: String,
        side_chat_id: String,
        cx: &mut Context<Self>,
    ) {
        let handoff = self
            .tiles
            .slots
            .get(&sid)
            .and_then(|slot| {
                slot.side_chats
                    .values()
                    .find(|p| p.read(cx).side_chat_id == side_chat_id)
            })
            .map(|p| {
                let panel = p.read(cx);
                let parent_chat_id = panel.parent_chat_id().to_string();
                let fork = panel.fork();
                let composer = panel.composer();
                let fork_transcript = fork.read(cx).transcript.clone();
                let fork_echoes = fork.read(cx).pending_echoes().to_vec();
                let draft = composer.read(cx).current_draft(cx);
                let staged = composer.read(cx).staged_attachments();
                let config = panel.picked_config(cx);
                (
                    parent_chat_id,
                    fork_transcript,
                    fork_echoes,
                    draft,
                    staged,
                    config,
                )
            });
        self.close_side_chat_tab(sid, side_chat_id, cx);
        let (parent_chat_id, fork_transcript, fork_echoes, draft, staged, picked_config) =
            handoff.unwrap_or_default();
        // Optimistic insert: the engine already created the row
        // (PromoteSideChat is synchronous engine-side), so the sidebar
        // renders and the new tab's context resolves it before the next
        // chats frame replaces it with the authoritative row.
        self.state.update(cx, |s, cx| {
            if let Some(parent) = s.chats.iter().find(|c| c.id == parent_chat_id).cloned()
                && !s.chats.iter().any(|c| c.id == chat_id)
            {
                s.insert_chat_optimistic(cypher_proto::Chat {
                    pinned: false,
                    id: chat_id.clone(),
                    device_id: parent.device_id.clone(),
                    title: None,
                    archived: false,
                    cwd: parent.cwd.clone(),
                    branch: parent.branch.clone(),
                    checkout_id: parent.checkout_id.clone(),
                    // The side chat's own model/traits picks (persisted by
                    // the panel's promote), else the inherited config.
                    config: picked_config.or_else(|| parent.config.clone()),
                    last_message_preview: None,
                    last_message_at: None,
                    created_at: chrono::Utc::now(),
                    harness_session_id: None,
                    harness_session_cwd: None,
                    space_id: parent.space_id.clone(),
                    last_seen_at: None,
                    room_gen: Some(2),
                    child: None,
                });
                cx.notify();
            }
        });
        // Its row may trail the next chats frame.
        self.expect_chat(&chat_id);
        let tab = crate::workspace::TabKey::session(chat_id.clone());
        let group = self
            .tiles
            .slots
            .get(&sid)
            .and_then(|slot| self.workspace.find(&slot.tab))
            .map(|(group, _)| group)
            .unwrap_or(self.workspace.focused());
        self.workspace.open_in(group, tab.clone());
        self.tiles.focus_pending = true;
        self.sync_slots(cx);
        if let Some(slot) = self
            .slot_for_tab(&tab)
            .and_then(|id| self.tiles.slots.get(&id))
        {
            // Seed the new tile's transcript from the fork so there is no
            // blank flash while the promoted chat's doc watch reset lands
            // (same content — the doc watch diff is a no-op), and carry any
            // unconfirmed optimistic echoes over.
            slot.state.update(cx, |s, cx| {
                s.set_transcript(fork_transcript);
                for echo in fork_echoes {
                    s.push_echo(&chat_id, echo);
                }
                cx.notify();
            });
            // Hand the side composer's unsent draft + staged attachments off
            // to the new tile's composer (keyed by the promoted chat id).
            slot.composer.update(cx, |composer, cx| {
                composer.seed_draft(&chat_id, draft, cx);
                composer.seed_attachments(&chat_id, staged, cx);
            });
        }
        self.workspace_changed(cx);
    }

    /// Remove a side chat tab by its panel's side-chat id (event path):
    /// dispose (no-op after promotion) and drop the panel (its transcript /
    /// status tasks die with it).
    fn close_side_chat_tab(&mut self, sid: SlotId, side_chat_id: String, cx: &mut Context<Self>) {
        let Some(slot) = self.tiles.slots.get(&sid) else {
            return;
        };
        if let Some(id) = slot
            .side_chats
            .iter()
            .find(|(_, p)| p.read(cx).side_chat_id == side_chat_id)
            .map(|(id, _)| *id)
        {
            self.close_side_chat_by_seq(sid, id, cx);
        }
    }

    /// Remove a side chat tab by its slot-minted sequence id (tab-strip ✕
    /// path). A side chat lives in its PARENT tile's dock — the slot that
    /// opened it — so a hidden tab (another tab active in that group) is
    /// removed from that dock, never from whichever tile is focused.
    pub(in crate::shell) fn close_side_chat_by_seq(
        &mut self,
        sid: SlotId,
        id: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.tiles.slots.get_mut(&sid) else {
            return;
        };
        let Some(panel) = slot.side_chats.remove(&id) else {
            return;
        };
        slot.side_chat_subs.remove(&id);
        slot.dock
            .surfaces
            .retain(|s| *s != DockSurface::SideChat(id));
        if slot.dock.active == DockSurface::SideChat(id) {
            slot.dock.active = DockSurface::Picker;
        }
        panel.update(cx, |panel, cx| panel.dispose(cx));
        cx.notify();
    }
}
