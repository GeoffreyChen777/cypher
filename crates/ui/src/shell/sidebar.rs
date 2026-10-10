//! Sidebar mutations (rename, archive, pin, delete, the view menu) and the
//! account actions behind the user menu (sign in/out, the sync switch).

use super::*;

impl Shell {
    /// Fire a Mutate op; failures surface in the sidebar notice strip.
    pub(super) fn mutate(&mut self, params: serde_json::Value, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine.client().call(methods::MUTATE, params).await {
                this.update(cx, |shell, cx| {
                    shell.sidebar_notice = Some(format!("{err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    pub(super) fn open_rename_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        let current = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == chat_id)
            .and_then(|c| c.title.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| TextInput::new("Session title", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted) {
                this.submit_rename_chat(cx);
            }
        });
        self.rename_dialog = Some(RenameChatDialog {
            chat_id,
            input,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    pub(super) fn submit_rename_chat(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_dialog.take() else {
            return;
        };
        let title = dialog.input.read(cx).text().trim().to_string();
        if !title.is_empty() {
            self.mutate(
                serde_json::json!({ "op": "renameChat", "chatId": dialog.chat_id, "title": title }),
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn archive_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.set_chat_archived(chat_id, true, cx);
    }

    pub(super) fn set_chat_archived(
        &mut self,
        chat_id: String,
        archived: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        self.mutate(
            serde_json::json!({ "op": "setChatArchived", "chatId": chat_id, "archived": archived }),
            cx,
        );
        cx.notify();
    }

    pub(super) fn close_sidebar_view_menu(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_view_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.sidebar_view_menu);
            cx.notify();
        }
    }

    /// The sidebar view menu's current state (persisted in ui-settings).
    pub(super) fn sidebar_view(&self) -> crate::state::SidebarView {
        crate::state::SidebarView {
            // A project window lists one project: a device filter could only
            // empty it.
            device: self
                .settings
                .sidebar_device_filter
                .clone()
                .filter(|_| !self.is_project_window()),
            sort: self.settings.sidebar_sort,
            reversed: self.settings.sidebar_sort_reversed,
        }
    }

    /// A new sort starts in its natural direction.
    pub(super) fn set_sidebar_sort(
        &mut self,
        sort: crate::prefs::SidebarSort,
        cx: &mut Context<Self>,
    ) {
        if self.settings.sidebar_sort != sort {
            self.settings.sidebar_sort_reversed = false;
        }
        self.settings.sidebar_sort = sort;
        self.close_sidebar_view_menu(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn set_sidebar_descending(&mut self, descending: bool, cx: &mut Context<Self>) {
        self.settings.sidebar_sort_reversed =
            self.settings.sidebar_sort.natural_descending() != descending;
        self.close_sidebar_view_menu(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn set_sidebar_device_filter(
        &mut self,
        device: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.settings.sidebar_device_filter = device;
        self.close_sidebar_view_menu(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// The sidebar view menu: a Devices section (All devices + one row per
    /// device, this device first) and a Sort section, each with a check on
    /// the current choice.
    pub(super) fn render_sidebar_view_menu(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let position = *self.sidebar_view_menu.get()?;
        let closing = self.sidebar_view_menu.closing_since();
        let (filter, sort) = (
            self.settings.sidebar_device_filter.clone(),
            self.settings.sidebar_sort,
        );
        let descending = self.sidebar_view().descending();
        let devices: Vec<(String, String)> = {
            let state = self.state.read(cx);
            let local = state.local_device_id.clone();
            let mut devices = state.devices.clone();
            devices.sort_by_key(|d| {
                (
                    local.as_deref() != Some(d.id.as_str()),
                    d.name.to_lowercase(),
                    d.id.clone(),
                )
            });
            devices.into_iter().map(|d| (d.id, d.name)).collect()
        };
        let check = |on: bool, theme: &Theme| {
            div().flex_none().size(px(14.0)).when(on, |el| {
                el.child(icon(icons::CHECK).size(px(13.0)).text_color(theme.text))
            })
        };
        let mut menu = popover::popover_card(theme)
            .w(px(200.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_sidebar_view_menu(cx);
            }))
            .flex()
            .flex_col()
            .child(popover::menu_heading(theme, "Devices"))
            .child(
                popover::menu_row(theme, false, "sidebar-view-all-devices")
                    .id("sidebar-view-all-devices")
                    .on_click(
                        cx.listener(|this, _, _, cx| this.set_sidebar_device_filter(None, cx)),
                    )
                    .child(check(filter.is_none(), theme))
                    .child(SharedString::from("All devices")),
            );
        for (id, name) in devices {
            let on = filter.as_deref() == Some(id.as_str());
            let pick = id.clone();
            menu = menu.child(
                popover::menu_row(theme, false, format!("sidebar-view-device-{id}"))
                    .id(SharedString::from(format!("sidebar-view-device-{id}")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_sidebar_device_filter(Some(pick.clone()), cx)
                    }))
                    .child(check(on, theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(name)),
                    ),
            );
        }
        menu = menu
            .child(popover::menu_separator())
            .child(popover::menu_heading(theme, "Sort by"));
        for option in crate::prefs::SidebarSort::ALL {
            menu = menu.child(
                popover::menu_row(
                    theme,
                    false,
                    format!("sidebar-view-sort-{}", option.label()),
                )
                .id(SharedString::from(format!(
                    "sidebar-view-sort-{}",
                    option.label()
                )))
                .on_click(cx.listener(move |this, _, _, cx| this.set_sidebar_sort(option, cx)))
                .child(check(sort == option, theme))
                .child(SharedString::from(option.label())),
            );
        }
        menu = menu
            .child(popover::menu_separator())
            .child(popover::menu_heading(theme, "Order"));
        for (label, glyph, desc) in [
            ("Ascending", icons::ARROW_UP, false),
            ("Descending", icons::ARROW_DOWN, true),
        ] {
            menu = menu.child(
                popover::menu_row(theme, false, format!("sidebar-view-order-{label}"))
                    .id(SharedString::from(format!("sidebar-view-order-{label}")))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_sidebar_descending(desc, cx)),
                    )
                    .child(check(descending == desc, theme))
                    .child(icon(glyph).size(px(13.0)).text_color(theme.text_muted))
                    .child(SharedString::from(label)),
            );
        }
        Some(popover::menu_at(
            "sidebar-view-menu-popover",
            position,
            menu.into_any_element(),
            closing,
        ))
    }

    /// Pin/unpin a session (synced): pinned sessions lead their project.
    pub(super) fn set_chat_pinned(
        &mut self,
        chat_id: String,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        self.mutate(
            serde_json::json!({ "op": "setChatPinned", "chatId": chat_id, "pinned": pinned }),
            cx,
        );
        cx.notify();
    }

    pub(super) fn delete_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.delete_confirm = None;
        let orphan = {
            let state = self.state.read(cx);
            spaces::orphan_worktree_after_delete(&state.chats, &state.spaces, &chat_id)
        };
        // A quick chat's scratch folder dies with the row: captured before
        // the mutate while the row is still in the local list.
        let scratch = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == chat_id && c.is_scratch())
            .and_then(|c| c.cwd.clone().map(|cwd| (c.device_id.clone(), cwd)));
        // Its tab closes (the slot's composer, and its per-chat stage, go
        // with it); a stashed draft of a closed tab is dropped too.
        if let Some(sid) = self.slot_for_tab(&crate::workspace::TabKey::session(chat_id.clone()))
            && let Some(slot) = self.tiles.slots.get(&sid)
        {
            slot.composer
                .update(cx, |composer, _| composer.purge_chat(&chat_id));
        }
        self.close_tab(&crate::workspace::TabKey::session(chat_id.clone()), cx);
        self.closed_drafts.remove(&chat_id);
        self.mutate(
            serde_json::json!({ "op": "deleteChat", "chatId": chat_id.clone() }),
            cx,
        );
        if let Some((device_id, cwd)) = scratch {
            self.delete_scratch_dir(chat_id, device_id, cwd, cx);
        }
        // Last session of a linked worktree: ask whether to remove the
        // checkout too. Captured before the mutate so the row is still in
        // the local list; children of this chat cascade and don't count.
        self.delete_worktree_confirm = orphan;
        cx.notify();
    }

    /// Remove a deleted quick chat's scratch folder on its host. The host
    /// verifies the path is the folder it minted for this chat id before
    /// deleting anything; a failure is surfaced, never retried silently.
    fn delete_scratch_dir(
        &mut self,
        chat_id: String,
        device_id: String,
        cwd: String,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        let mut params = serde_json::json!({ "chatId": chat_id, "path": cwd });
        if local.as_deref() != Some(device_id.as_str())
            && let Some(object) = params.as_object_mut()
        {
            object.insert("targetDeviceId".into(), device_id.into());
        }
        self.scratch_cleanup_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine
                .client()
                .call(methods::DELETE_SCRATCH_DIR, params)
                .await
            {
                this.update(cx, |shell, cx| {
                    shell.sidebar_notice =
                        Some(format!("Scratch folder not removed: {err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    pub(super) fn delete_worktree(&mut self, orphan: OrphanWorktree, cx: &mut Context<Self>) {
        self.delete_worktree_confirm = None;
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        let mut params = serde_json::Map::new();
        params.insert("repoPath".into(), orphan.repo_path.into());
        params.insert("worktreePath".into(), orphan.worktree_path.into());
        if local.as_deref() != Some(orphan.device_id.as_str()) {
            params.insert("targetDeviceId".into(), orphan.device_id.into());
        }
        self.delete_worktree_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine
                .client()
                .call(methods::DELETE_WORKTREE, serde_json::Value::Object(params))
                .await
            {
                this.update(cx, |shell, cx| {
                    shell.sidebar_notice = Some(format!("{err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
        cx.notify();
    }

    pub(super) fn request_sign_out(&mut self, cx: &mut Context<Self>) {
        self.close_user_menu(cx);
        if self.state.read(cx).workspace_scope != Some(WorkspaceScope::Synced) {
            return;
        }
        self.sync_flow = SyncFlow::SignOutConfirm;
        cx.notify();
    }

    pub(super) fn confirm_sign_out(&mut self, cx: &mut Context<Self>) {
        self.start_local_runtime_transition(true, cx);
    }

    pub(super) fn start_local_runtime_transition(
        &mut self,
        sign_out: bool,
        cx: &mut Context<Self>,
    ) {
        if self.runtime_change_task.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.runtime_change_error = Some("Engine not connected".into());
            self.sync_flow = SyncFlow::SignedOutRestartRequired;
            cx.notify();
            return;
        };
        self.sync_flow = SyncFlow::SigningOut;
        self.runtime_change_error = None;
        let ipc_socket = self.boot.ipc_socket.clone();
        let data_dir = self.boot.data_dir.clone();
        let shutdown_dir = data_dir.clone();
        let transition = Tokio::spawn(cx, async move {
            if sign_out {
                engine
                    .client()
                    .call(methods::SIGN_OUT, serde_json::json!({}))
                    .await
                    .map_err(|error| format!("Sign out failed: {error}"))?;
            }
            stop_synced_runtime(engine, ipc_socket, &shutdown_dir).await
        });
        let state = self.state.clone();
        let boot = self.boot.clone();
        self.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match transition.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.runtime_change_task = None;
                match result {
                    Ok(()) => {
                        shell.sync_flow = SyncFlow::Idle;
                        shell.runtime_change_error = None;
                        shell.org = None;
                        shell.route = Route::Chat;
                        shell.space_boot_applied = false;
                        state.update(cx, |state, cx| state.prepare_runtime_replacement(cx));
                        AppState::bootstrap(state.clone(), shell.data_dir.clone(), boot, cx);
                    }
                    Err(error) => {
                        shell.sync_flow = SyncFlow::SignedOutRestartRequired;
                        shell.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn cancel_auth_setup(&mut self, cx: &mut Context<Self>) {
        let local = self.state.read(cx).workspace_scope == Some(WorkspaceScope::Local);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let pending_auth = self.auth_task.take();
        let pending_org = self.org.as_mut().and_then(|org| org.task.take());
        if local {
            self.sync_flow = SyncFlow::Canceling;
        }
        self.auth_task = Some(cx.spawn(async move |this, cx| {
            // Do not race SignOut against an exchange or organization write
            // that can still persist a session after credentials were cleared.
            if let Some(task) = pending_auth {
                task.await;
            }
            if let Some(task) = pending_org {
                task.await;
            }
            let result = engine
                .client()
                .call(methods::SIGN_OUT, serde_json::json!({}))
                .await;
            this.update(cx, |shell, cx| {
                match result {
                    Ok(_) => {
                        shell.org = None;
                        if local {
                            shell.sync_flow = SyncFlow::Idle;
                        }
                    }
                    Err(err) => {
                        if local {
                            shell.sync_flow = SyncFlow::Enabling;
                        }
                        shell.sidebar_notice =
                            Some(format!("Could not cancel sign-in: {err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn postpone_sync_restart(&mut self, cx: &mut Context<Self>) {
        match self.sync_flow {
            SyncFlow::RestartPending { .. } => {
                self.sync_flow = SyncFlow::RestartPending { notice_open: false };
            }
            SyncFlow::SwitchOffer { .. } => {
                self.sync_flow = SyncFlow::SwitchOffer { notice_open: false };
            }
            SyncFlow::ImportFailed { .. } => {
                self.sync_flow = SyncFlow::ImportFailed { notice_open: false };
            }
            _ => return,
        }
        cx.notify();
    }

    pub(super) fn reopen_sync_notice(&mut self, cx: &mut Context<Self>) {
        self.close_user_menu(cx);
        match self.sync_flow {
            SyncFlow::RestartPending { .. } => {
                self.sync_flow = SyncFlow::RestartPending { notice_open: true };
            }
            SyncFlow::SwitchOffer { .. } => {
                self.sync_flow = SyncFlow::SwitchOffer { notice_open: true };
            }
            SyncFlow::ImportFailed { .. } => {
                self.sync_flow = SyncFlow::ImportFailed { notice_open: true };
            }
            _ => return,
        }
        cx.notify();
    }

    /// The wizard's choice step chose a path: stop the local runtime, boot the
    /// synced one in-place (mirror of the sign-out transition), then let
    /// [`Self::drive_sync_switch`] run the import once the runtime is ready.
    /// Failure falls back to the quit-and-reopen dialog — the local profile is
    /// untouched, so the old path is always a safe exit.
    pub(super) fn start_synced_switch(&mut self, import: bool, cx: &mut Context<Self>) {
        if self.runtime_change_task.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.runtime_change_error = Some("Engine not connected".into());
            self.sync_flow = SyncFlow::RestartPending { notice_open: true };
            cx.notify();
            return;
        };
        self.sync_flow = SyncFlow::Switching { import };
        self.runtime_change_error = None;
        self.import_current = None;
        let ipc_socket = self.boot.ipc_socket.clone();
        let data_dir = self.boot.data_dir.clone();
        let transition = Tokio::spawn(cx, async move {
            stop_synced_runtime(engine, ipc_socket, &data_dir).await
        });
        let state = self.state.clone();
        let boot = self.boot.clone();
        self.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match transition.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.runtime_change_task = None;
                match result {
                    Ok(()) => {
                        // Keep `Switching { import }`: the state observer sees
                        // the replacement runtime reach Ready and advances the
                        // wizard from there.
                        shell.org = None;
                        shell.route = Route::Chat;
                        shell.space_boot_applied = false;
                        state.update(cx, |state, cx| state.prepare_runtime_replacement(cx));
                        AppState::bootstrap(state.clone(), shell.data_dir.clone(), boot, cx);
                    }
                    Err(error) => {
                        shell.sync_flow = SyncFlow::RestartPending { notice_open: true };
                        shell.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// Advance the in-place switch when the replacement runtime lands: Ready +
    /// Synced starts the import stream (or finishes immediately when the user
    /// chose a fresh start); a runtime that comes back non-synced fell out of
    /// the swap — surface the quit fallback rather than pretend.
    pub(super) fn drive_sync_switch(&mut self, cx: &mut Context<Self>) {
        let SyncFlow::Switching { import } = self.sync_flow else {
            return;
        };
        if self.runtime_change_task.is_some() {
            return; // still stopping the local runtime
        }
        let (ready, scope) = {
            let state = self.state.read(cx);
            (
                matches!(state.connection, ConnectionStatus::Ready),
                state.workspace_scope,
            )
        };
        if !ready {
            if let ConnectionStatus::Failed(error) = &self.state.read(cx).connection {
                self.sync_flow = SyncFlow::RestartPending { notice_open: true };
                self.runtime_change_error = Some(error.clone().into());
                cx.notify();
            }
            return;
        }
        match scope {
            Some(WorkspaceScope::Synced) => {
                if import {
                    self.spawn_local_import(cx);
                } else {
                    self.sync_flow = SyncFlow::Idle;
                    cx.notify();
                }
            }
            Some(_) => {
                self.sync_flow = SyncFlow::RestartPending { notice_open: true };
                self.runtime_change_error =
                    Some("The synced workspace did not come up — restart to finish.".into());
                cx.notify();
            }
            None => {}
        }
    }

    /// Subscribe to the engine's one-time import stream and mirror its
    /// progress into the wizard.
    pub(super) fn spawn_local_import(&mut self, cx: &mut Context<Self>) {
        if self.import_task.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sync_flow = SyncFlow::RestartPending { notice_open: true };
            self.runtime_change_error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        self.sync_flow = SyncFlow::Importing { done: 0, total: 0 };
        self.runtime_change_error = None;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
        let stream = Tokio::spawn(cx, async move {
            let mut items = engine
                .client()
                .subscribe(methods::IMPORT_LOCAL_WORKSPACE, serde_json::json!({}))
                .await
                .map_err(|error| error.to_string())?;
            while let Some(item) = items.recv().await {
                let _ = tx.send(item);
            }
            Ok::<(), String>(())
        });
        self.import_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let item = rx.recv().await;
                let ended = item.is_none();
                this.update(cx, |shell, cx| {
                    if let Some(item) = &item {
                        shell.apply_import_event(item, cx);
                    }
                    if ended {
                        shell.import_task = None;
                        shell.import_current = None;
                        // A stream that died before its summary is a failure —
                        // offer the in-place retry (idempotent).
                        if matches!(shell.sync_flow, SyncFlow::Importing { .. }) {
                            shell.sync_flow = SyncFlow::ImportFailed { notice_open: true };
                            shell.runtime_change_error =
                                Some("The import stream ended before it finished.".into());
                        }
                        cx.notify();
                    }
                })
                .ok();
                if ended {
                    break;
                }
            }
            if let Ok(Err(error)) = stream.await {
                this.update(cx, |shell, cx| {
                    shell.import_task = None;
                    if matches!(shell.sync_flow, SyncFlow::Importing { .. }) {
                        shell.sync_flow = SyncFlow::ImportFailed { notice_open: true };
                        shell.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                })
                .ok();
            }
        }));
        cx.notify();
    }

    fn apply_import_event(&mut self, item: &serde_json::Value, cx: &mut Context<Self>) {
        match item.get("kind").and_then(|k| k.as_str()) {
            Some("start") => {
                let total = item.get("chats").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                self.sync_flow = SyncFlow::Importing { done: 0, total };
            }
            Some("chat") => {
                let index = item.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let total = item.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                self.import_current = item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(|t| SharedString::from(t.to_string()));
                self.sync_flow = SyncFlow::Importing { done: index, total };
            }
            Some("summary") => {
                self.import_current = None;
                // A summary with errors is a FAILED import, however normally
                // the stream ended — never present a partial migration as
                // complete (the engine keeps collecting per-item failures
                // precisely so this can be surfaced).
                match import_summary_outcome(item) {
                    Ok((imported, skipped)) => {
                        self.sync_flow = SyncFlow::ImportDone { imported, skipped };
                    }
                    Err(message) => {
                        self.sync_flow = SyncFlow::ImportFailed { notice_open: true };
                        self.runtime_change_error = Some(message.into());
                    }
                }
            }
            _ => return,
        }
        cx.notify();
    }

    pub(super) fn quit_for_runtime_change(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.runtime_change_error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        if engine.mode() == EngineMode::InProcess {
            cx.quit();
            return;
        }
        if self.runtime_change_task.is_some() {
            return;
        }

        self.runtime_change_error = None;
        let ipc_socket = self.boot.ipc_socket.clone();
        let data_dir = self.boot.data_dir.clone();
        let shutdown = Tokio::spawn(cx, async move {
            engine
                .client()
                .call(methods::STOP_ENGINE, serde_json::json!({}))
                .await
                .map_err(|err| err.to_string())?;
            wait_for_remote_engine_shutdown(ipc_socket, &data_dir, RUNTIME_CHANGE_TIMEOUT).await
        });
        self.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match shutdown.await {
                Ok(result) => result,
                Err(err) => Err(err.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.runtime_change_task = None;
                match result {
                    Ok(_) => cx.quit(),
                    Err(err) => {
                        shell.runtime_change_error = Some(format!(
                            "Could not stop the remote engine: {err}. Run `cypher daemon stop`, then quit and reopen Cypher."
                        ).into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn start_sign_in(&mut self, cx: &mut Context<Self>) {
        let scope = self.state.read(cx).workspace_scope;
        if scope == Some(WorkspaceScope::Development) {
            return;
        }
        self.close_user_menu(cx);
        if scope == Some(WorkspaceScope::Local) {
            self.sync_flow = SyncFlow::Enabling;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.auth_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::SIGN_IN, serde_json::json!({}))
                .await;
            this.update(cx, |shell, cx| match result {
                Ok(value) => {
                    if let Some(url) = value.get("url").and_then(|u| u.as_str()) {
                        cx.open_url(url);
                    }
                    cx.notify();
                }
                Err(err) => {
                    if scope == Some(WorkspaceScope::Local) && shell.sync_flow == SyncFlow::Enabling
                    {
                        shell.sync_flow = SyncFlow::Idle;
                    }
                    shell.sidebar_notice = Some(format!("Sign in failed: {err}").into());
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }
}
