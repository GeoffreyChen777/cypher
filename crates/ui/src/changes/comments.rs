//! Diff text selection + comments.

use super::*;

impl Changes {
    /// Selection lifecycle callbacks for the diff's code text elements: a
    /// settle shows the shared Comment pill at the selection endpoint, a new
    /// drag or a clear hides it. Built once per frame; every visible line's
    /// key rides the SAME [`markdown::render::SelectionUi`].
    pub(super) fn selection_ui_for(
        &self,
        scope: crate::markdown::selection::SelectionScope,
        side: Option<Side>,
        cx: &mut Context<Self>,
    ) -> markdown::render::SelectionUi {
        let popup = self.comment_popup.clone();
        let entity = cx.weak_entity();
        // A fresh drag start closes ANY surface's pill — only one floating
        // UI may exist — without touching the just-begun selection. A cleared
        // selection hides only THIS pane's pill (scoped dismissal).
        let dismiss_popup = popup.clone();
        let started: crate::markdown::render::WindowHandler = Rc::new(move |_window, cx| {
            if let Some(popup) = dismiss_popup.upgrade() {
                popup.update(cx, |popup, cx| {
                    popup.selection_started(crate::comment_popup::CommentOwner::Markdown(scope), cx)
                });
            }
        });
        let clear_popup = popup.clone();
        let cleared: crate::markdown::render::WindowHandler = Rc::new(move |_window, cx| {
            if let Some(popup) = clear_popup.upgrade() {
                popup.update(cx, |popup, cx| {
                    popup.dismiss_if_owner(crate::comment_popup::CommentOwner::Markdown(scope), cx)
                });
            }
        });
        let settled: crate::markdown::render::SelectionSettledHandler = {
            let popup = popup.clone();
            Rc::new(move |snapshot, anchor, _window, cx| {
                // Capture the selected chat at SETTLE time — the saved
                // comment routes to the chat the quote came from even if a
                // switch happens before Save.
                let Some(chat_id) = entity
                    .update(cx, |this: &mut Self, cx| {
                        this.state.read(cx).selected_chat.clone()
                    })
                    .ok()
                    .flatten()
                else {
                    return;
                };
                let clear = {
                    let entity = entity.clone();
                    Rc::new(move |cx: &mut gpui::App| {
                        crate::markdown::selection::clear(scope);
                        entity.update(cx, |_, cx| cx.notify()).ok();
                    })
                };
                // Side Chat source: the diff pane's scope label
                // labels the engine's context block. Split selections name
                // their version; a path is attached only when all selected
                // source lines belong to one file.
                let scope_label = entity
                    .update(cx, |this: &mut Self, _| match side {
                        Some(side) => format!("{} · {} version", this.scope.label(), side.label()),
                        None => this.scope.label().to_string(),
                    })
                    .ok();
                let file_path = entity
                    .update(cx, |this: &mut Self, _| {
                        let files = &this.parsed.as_ref()?.files;
                        layout::selected_file(
                            &this.owner,
                            snapshot.spans.iter().map(|span| span.key.as_str()),
                            files,
                            side,
                        )
                    })
                    .ok()
                    .flatten();
                if let Some(popup) = popup.upgrade() {
                    popup.update(cx, |popup, cx| {
                        popup.offer(
                            chat_id,
                            snapshot.text.clone(),
                            // Diff text is never a displayed translation.
                            None,
                            anchor,
                            crate::comment_popup::CommentOwner::Markdown(scope),
                            Some(crate::comment_popup::CommentHead {
                                key: snapshot.head_key.clone(),
                                ix: snapshot.head_ix,
                                scope,
                            }),
                            clear,
                            Some(cypher_proto::SideChatSource::GitDiff {
                                scope: scope_label,
                                file_path,
                            }),
                            cx,
                        );
                    });
                }
            })
        };
        markdown::render::SelectionUi {
            on_started: started,
            on_cleared: cleared,
            on_settled: settled,
        }
    }

    /// The pane's content was replaced or it closed: drop the Changes-scoped
    /// selection wash and any popup offer that belongs to THIS pane (never
    /// another surface's).
    pub(super) fn invalidate_selection(&mut self, cx: &mut Context<Self>) {
        for scope in [self.sel_scope, self.split_scopes[0], self.split_scopes[1]] {
            crate::markdown::selection::clear(scope);
            if let Some(popup) = self.comment_popup.upgrade() {
                popup.update(cx, |popup, cx| {
                    popup.dismiss_if_owner(crate::comment_popup::CommentOwner::Markdown(scope), cx)
                });
            }
        }
    }

    /// The shell calls this as the pane closes: a fresh pane with the same
    /// scope must never inherit a stale selection or comment.
    pub fn detach(&mut self, cx: &mut Context<Self>) {
        self.invalidate_selection(cx);
    }

    /// The selected chat's host device when it differs from the connected
    /// engine's own — diffs are produced where the checkout lives, so a
    /// remote chat's watch must relay-forward (`targetDeviceId`) to its host.
    /// Without this the local stream simply never carries the remote checkout
    /// and the pane sits on "Preparing diff…" forever (user report).
    fn desired_target(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let device = state.selected_chat_row()?.device_id.clone();
        (state.local_device_id.as_deref() != Some(device.as_str())).then_some(device)
    }

    /// Start the `WatchCheckoutDiffs` subscription (idempotent per target).
    /// Retries with a flat 2 s delay if the stream fails or ends; the last
    /// content stays visible under an error banner meanwhile.
    pub fn ensure_watch(&mut self, cx: &mut Context<Self>) {
        let target = self.desired_target(cx);
        if self.started && self.watch_target == target {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            // Engine still booting — retry on the next state change via sync().
            return;
        };
        // Retarget: the old task (and its stream) drop; rows from the previous
        // device would resolve against the wrong checkouts, so clear them.
        if self.started {
            self.diffs.clear();
            self.error = None;
        }
        self.started = true;
        self.watch_target = target.clone();
        self.watch_task = Some(Self::spawn_watch(engine, target, cx));
    }

    fn spawn_watch(
        engine: EngineHandle,
        target: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let mut params = serde_json::Map::new();
                if let Some(target) = &target {
                    params.insert(
                        "targetDeviceId".into(),
                        serde_json::Value::String(target.clone()),
                    );
                }
                let subscribed = engine
                    .client()
                    .subscribe(
                        methods::WATCH_CHECKOUT_DIFFS,
                        serde_json::Value::Object(params),
                    )
                    .await;
                match subscribed {
                    Ok(mut rx) => {
                        while let Some(value) = rx.recv().await {
                            let alive = this.update(cx, |changes, cx| {
                                changes.error = None;
                                if apply_diff_frame(&mut changes.diffs, value) {
                                    changes.sync(cx);
                                    cx.notify();
                                }
                            });
                            if alive.is_err() {
                                return;
                            }
                        }
                        // Stream ended (engine restart / reconnect): banner + retry.
                        if this
                            .update(cx, |changes, cx| {
                                changes.error = Some("Diff stream interrupted — retrying".into());
                                cx.notify();
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(err) => {
                        if this
                            .update(cx, |changes, cx| {
                                changes.error =
                                    Some(format!("Diff watch unavailable: {err}").into());
                                cx.notify();
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        })
    }

    fn resolved(&self, cx: &App) -> Option<CheckoutDiff> {
        let state = self.state.read(cx);
        let chat = state.selected_chat_row()?;
        resolve_diff(&self.diffs, chat).cloned()
    }

    /// The checkout root the scoped RPCs address: the watch-resolved diff's
    /// canonical cwd when available, else the chat row's own.
    fn scoped_cwd(&self, cx: &App) -> Option<String> {
        if let Some(diff) = self.resolved(cx) {
            return Some(diff.cwd);
        }
        self.state.read(cx).selected_chat_row()?.cwd.clone()
    }

    /// The diff the pane currently displays: the watch stream for the working
    /// tree, the one-shot scoped capture otherwise.
    pub(super) fn active_diff(&self, cx: &App) -> Option<CheckoutDiff> {
        match self.scope {
            DiffScope::WorkingTree => self.resolved(cx),
            DiffScope::Branch | DiffScope::LatestTurn | DiffScope::Commit => self.scoped.clone(),
            DiffScope::History => None,
        }
    }

    /// Scope discriminant folded into the parse key, so a scope or base
    /// switch re-parses even when checksums collide.
    fn scope_key(&self) -> String {
        match self.scope {
            DiffScope::WorkingTree => "wt".to_string(),
            DiffScope::Branch => format!("br:{}", self.base_ref.as_deref().unwrap_or("")),
            DiffScope::LatestTurn => "turn".to_string(),
            DiffScope::History => "history".to_string(),
            DiffScope::Commit => format!(
                "commit:{}",
                self.commit.as_ref().map(|c| c.sha.as_str()).unwrap_or("")
            ),
        }
    }

    fn parse_key(&self, diff: &CheckoutDiff) -> String {
        format!(
            "{}:{}:{}",
            diff.checkout_id,
            diff.checksum,
            self.scope_key()
        )
    }

    /// Fetch the branch list for the selected chat's checkout (idempotent per
    /// device+cwd); the repo's default branch (first entry) becomes the
    /// comparison base unless the user already picked one that still exists.
    fn ensure_branches(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.scoped_cwd(cx) else {
            return;
        };
        let target = self.desired_target(cx);
        let key = format!("{}:{}", target.as_deref().unwrap_or("local"), cwd);
        if self.branches_for.as_deref() == Some(key.as_str()) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.branches_for = Some(key.clone());
        self.branches_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            params.insert("repoPath".into(), serde_json::Value::String(cwd));
            if let Some(target) = target {
                params.insert("targetDeviceId".into(), serde_json::Value::String(target));
            }
            let result = engine
                .client()
                .call(methods::LIST_BRANCHES, serde_json::Value::Object(params))
                .await;
            this.update(cx, |changes, cx| {
                if changes.branches_for.as_deref() != Some(key.as_str()) {
                    return; // superseded by a chat/device switch
                }
                match result {
                    Ok(value) => {
                        changes.branches =
                            serde_json::from_value::<Vec<String>>(value).unwrap_or_default();
                        let keep = changes
                            .base_ref
                            .as_ref()
                            .is_some_and(|base| changes.branches.contains(base));
                        if !keep {
                            let current = changes
                                .state
                                .read(cx)
                                .selected_chat_row()
                                .and_then(|chat| chat.branch.clone());
                            changes.base_ref =
                                default_base_ref(&changes.branches, current.as_deref());
                        }
                        changes.sync(cx);
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "changes: branch list failed");
                        // Allow a retry on the next state change.
                        changes.branches_for = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Keep the one-shot scoped capture fresh. The fetch key folds in the
    /// watch checksum, so any working-tree change (or commit — HEAD rides the
    /// checksum) re-captures; a context change (chat/scope/base) clears the
    /// stale content first so the pane shows the spinner, while a
    /// checksum-only refresh keeps the old diff visible until the new one
    /// lands.
    fn ensure_scoped(&mut self, cx: &mut Context<Self>) {
        if matches!(self.scope, DiffScope::WorkingTree | DiffScope::History) {
            self.scoped_inflight = None;
            self.scoped_task = None;
            return;
        }
        let Some(chat_id) = self
            .state
            .read(cx)
            .selected_chat_row()
            .map(|chat| chat.id.clone())
        else {
            return;
        };
        let Some(cwd) = self.scoped_cwd(cx) else {
            return;
        };
        let base = match self.scope {
            DiffScope::Branch => match &self.base_ref {
                Some(base) => Some(base.clone()),
                None => return, // branch list still loading
            },
            _ => None,
        };
        let commit_sha = match self.scope {
            DiffScope::Commit => match &self.commit {
                Some(commit) => Some(commit.sha.clone()),
                None => return, // a commit pane without its pin never fetches
            },
            _ => None,
        };
        let target = self.desired_target(cx);
        let context = format!(
            "{}|{}|{}|{}|{}|{}",
            target.as_deref().unwrap_or("local"),
            chat_id,
            cwd,
            self.scope.mode(),
            base.as_deref().unwrap_or(""),
            commit_sha.as_deref().unwrap_or("")
        );
        let watch_sum = self.resolved(cx).map(|d| d.checksum).unwrap_or_default();
        let key = format!("{context}|{watch_sum}");
        if self.scoped_for.as_deref() == Some(key.as_str())
            || self.scoped_inflight.as_deref() == Some(key.as_str())
        {
            return;
        }
        if self
            .scoped_for
            .as_deref()
            .is_none_or(|prev| !prev.starts_with(&format!("{context}|")))
        {
            self.scoped = None;
            self.scoped_error = None;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let mode = self.scope.mode();
        self.scoped_inflight = Some(key.clone());
        self.scoped_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            params.insert("cwd".into(), serde_json::Value::String(cwd));
            params.insert("mode".into(), serde_json::Value::String(mode.to_string()));
            params.insert("chatId".into(), serde_json::Value::String(chat_id));
            if let Some(base) = base {
                params.insert("baseRef".into(), serde_json::Value::String(base));
            }
            if let Some(sha) = commit_sha {
                params.insert("commitSha".into(), serde_json::Value::String(sha));
            }
            if let Some(target) = target {
                params.insert("targetDeviceId".into(), serde_json::Value::String(target));
            }
            let result = engine
                .client()
                .call(
                    methods::GET_CHECKOUT_DIFF,
                    serde_json::Value::Object(params),
                )
                .await;
            this.update(cx, |changes, cx| {
                if changes.scoped_inflight.as_deref() != Some(key.as_str()) {
                    return; // superseded
                }
                changes.scoped_inflight = None;
                match result.and_then(|value| {
                    serde_json::from_value::<CheckoutDiff>(value)
                        .map_err(|e| cypher_rpc::RpcError::Failed(e.to_string()))
                }) {
                    Ok(diff) => {
                        changes.scoped = Some(diff);
                        changes.scoped_error = None;
                    }
                    Err(err) => {
                        changes.scoped = None;
                        changes.scoped_error = Some(err.to_string().into());
                    }
                }
                changes.scoped_for = Some(key);
                changes.sync(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn set_scope(&mut self, scope: DiffScope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            if scope == DiffScope::History {
                self.history_pane(cx)
                    .update(cx, |history, cx| history.ensure_loaded(cx));
            }
            self.sync(cx);
        }
        cx.notify();
    }

    pub(super) fn history_pane(&mut self, cx: &mut Context<Self>) -> Entity<GitHistory> {
        if let Some(history) = &self.history {
            return history.clone();
        }
        let history = cx.new(|cx| GitHistory::new(self.state.clone(), cx));
        self.history_events =
            Some(
                cx.subscribe(&history, |this: &mut Self, _, event, cx| match event {
                    GitHistoryEvent::OpenCommit(commit) => {
                        // Bubble to the host — the surface strip opens the tab.
                        cx.emit(ChangesEvent::OpenCommit(commit.clone()));
                    }
                    GitHistoryEvent::FetchSucceeded => {
                        // Remote refs affect branch choices and every scoped diff
                        // based on a ref. Force fresh reads after the engine has
                        // also kicked its checkout-status watcher.
                        this.branches_for = None;
                        this.scoped_for = None;
                        this.scoped_inflight = None;
                        this.scoped_task = None;
                        this.ensure_branches(cx);
                        if this.scope != DiffScope::History {
                            this.ensure_scoped(cx);
                        }
                        cx.notify();
                    }
                }),
            );
        self.history = Some(history.clone());
        history
    }

    pub(super) fn history_count(&mut self, cx: &mut Context<Self>) -> Entity<GitHistoryCount> {
        if let Some(count) = &self.history_count {
            return count.clone();
        }
        let history = self.history_pane(cx);
        let count = cx.new(|cx| GitHistoryCount::new(history, cx));
        self.history_count = Some(count.clone());
        count
    }

    pub(super) fn history_fetch_button(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<GitHistoryFetchButton> {
        if let Some(button) = &self.history_fetch_button {
            return button.clone();
        }
        let history = self.history_pane(cx);
        let button = cx.new(|cx| GitHistoryFetchButton::new(history, cx));
        self.history_fetch_button = Some(button.clone());
        button
    }

    pub(super) fn set_base_ref(&mut self, base: String, cx: &mut Context<Self>) {
        if self.base_ref.as_deref() != Some(base.as_str()) {
            self.base_ref = Some(base);
            self.sync(cx);
        }
        cx.notify();
    }

    /// Everything the pane needs kicked when (re)shown: the watch plus the
    /// scope-specific loads (branches, scoped/commit capture, history) — the
    /// shell's hook for freshly-mounted surface tabs.
    pub fn ensure_content(&mut self, cx: &mut Context<Self>) {
        self.sync(cx);
    }

    /// Reconcile parsed content with the currently-active diff.
    pub(super) fn sync(&mut self, cx: &mut Context<Self>) {
        // The watch follows the selected chat's host device (idempotent when
        // the target is unchanged); a boot-deferred attempt retries here too.
        self.ensure_watch(cx);
        if self.scope == DiffScope::History {
            self.history_pane(cx)
                .update(cx, |history, cx| history.ensure_loaded(cx));
            return;
        }
        if self.scope != DiffScope::Commit {
            self.ensure_branches(cx);
        }
        self.ensure_scoped(cx);
        let Some(diff) = self.active_diff(cx) else {
            if self.parsed.take().is_some() {
                self.rows.clear();
                self.row_ranges.clear();
                self.list.reset(0);
                self.folds.clear();
                self.highlights.clear();
                // Content gone: a selection anchored to it is stale too.
                self.invalidate_selection(cx);
                cx.notify();
            }
            return;
        };
        let key = self.parse_key(&diff);
        if self.parsed.as_ref().is_some_and(|p| p.key == key) {
            return;
        }
        // Parse off the render path — patches run to megabytes.
        let patch = diff.patch.clone();
        let truncated = diff.truncated;
        let additions = diff.additions;
        let deletions = diff.deletions;
        let file_count = diff.files.len();
        self.parse_task = Some(cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { parse_patch(&patch) })
                .await;
            this.update(cx, |changes, cx| {
                // Late results for a superseded diff are re-checked by key.
                let current = changes.active_diff(cx).map(|d| changes.parse_key(&d));
                if current.as_deref() != Some(key.as_str()) {
                    return;
                }
                let file_count = if file_count > 0 {
                    file_count
                } else {
                    files.len()
                };
                changes.folds.clear();
                changes.highlights.clear();
                let (rows, ranges) = layout::flatten(changes.view_layout, &files, |_| false);
                changes.max_columns = [0; 2];
                changes.max_gutter = files.iter().map(gutter_width).fold(GUTTER_WIDTH, f32::max);
                changes.horizontal = [0.0; 2];
                for line in files.iter().flat_map(|f| &f.hunks).flat_map(|h| &h.lines) {
                    if line.kind == LineKind::Meta {
                        continue;
                    }
                    // Conservative width bound: bytes also safely cover wide
                    // Unicode; tabs reserve their display-cell advance.
                    let columns: usize = line
                        .text
                        .bytes()
                        .map(|b| if b == b'\t' { 8 } else { 1 })
                        .sum();
                    if line.kind != LineKind::Add {
                        changes.max_columns[0] = changes.max_columns[0].max(columns);
                    }
                    if line.kind != LineKind::Del {
                        changes.max_columns[1] = changes.max_columns[1].max(columns);
                    }
                }
                // The uniform hint keeps offsets for never-rendered rows
                // sane (most rows ARE lines); real heights land as rows
                // render.
                changes
                    .list
                    .reset_with_uniform_height(rows.len(), px(DIFF_LINE_HEIGHT));
                // New content: keys shift, so any selection/comment anchored
                // to the previous diff is stale.
                changes.invalidate_selection(cx);
                changes.rows = rows;
                changes.row_ranges = ranges;
                changes.parsed = Some(ParsedDiff {
                    key,
                    truncated,
                    additions,
                    deletions,
                    file_count,
                    files: Arc::new(files),
                });
                cx.notify();
            })
            .ok();
        }));
    }

    /// Swap one file's body rows (everything after its header) for
    /// `new_body`, splicing both the row model and the list state. gpui's
    /// `splice` shifts the logical scroll anchor by the count delta, so
    /// content below the fold stays put.
    fn replace_file_body(&mut self, file_ix: usize, new_body: Vec<DiffRow>) {
        let Some(range) = self.row_ranges.get(file_ix).cloned() else {
            return;
        };
        let body = range.start + 1..range.end;
        let delta = new_body.len() as isize - body.len() as isize;
        self.list.splice(body.clone(), new_body.len());
        self.rows.splice(body, new_body);
        self.row_ranges[file_ix] = range.start..(range.end as isize + delta) as usize;
        for r in &mut self.row_ranges[file_ix + 1..] {
            *r = (r.start as isize + delta) as usize..(r.end as isize + delta) as usize;
        }
    }

    pub(super) fn toggle_fold(&mut self, file_ix: usize, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else {
            return;
        };
        let Some(file) = parsed.files.get(file_ix) else {
            return;
        };
        if self.view_layout == DiffLayout::Split {
            let fold = self.folds.entry(file.path.clone()).or_default();
            fold.collapsed = !fold.collapsed;
            fold.toggled_at = None;
            let body = if fold.collapsed {
                Vec::new()
            } else {
                layout::body_rows(self.view_layout, file_ix as u32, file)
            };
            self.replace_file_body(file_ix, body);
            self.invalidate_selection(cx);
            cx.notify();
            return;
        }
        let expanded_height = body_height(file);
        let fold = self.folds.entry(file.path.clone()).or_default();
        let currently_collapsed = fold.collapsed;
        fold.from = if currently_collapsed {
            0.0
        } else {
            expanded_height
        };
        fold.to = if currently_collapsed {
            expanded_height
        } else {
            0.0
        };
        fold.collapsed = !currently_collapsed;
        fold.epoch += 1;
        fold.toggled_at = Some(std::time::Instant::now());
        // The body tweens as ONE clipped stand-in row; the settle sweep
        // swaps it for steady rows (all lines, or none) once the window
        // elapses.
        self.replace_file_body(
            file_ix,
            vec![DiffRow::FoldingBody {
                file: file_ix as u32,
            }],
        );
        // Folding hides the body's lines — a pill anchored to one would float
        // over the wrong row. The selection wash stays (it reappears on
        // unfold; folded lines simply don't paint it).
        let scope = self.sel_scope;
        if let Some(popup) = self.comment_popup.upgrade() {
            popup.update(cx, |popup, cx| {
                popup.dismiss_if_owner(crate::comment_popup::CommentOwner::Markdown(scope), cx)
            });
        }
        self.ensure_fold_settle(cx);
    }

    /// Keep a sweep alive while any [`DiffRow::FoldingBody`] stand-ins
    /// remain; each tick settles the ones whose tween window has elapsed.
    fn ensure_fold_settle(&mut self, cx: &mut Context<Self>) {
        if self.fold_settle.is_some() {
            return;
        }
        self.fold_settle = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FOLD_TWEEN_WINDOW).await;
                let more = this
                    .update(cx, |changes, cx| changes.settle_folds(cx))
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
            this.update(cx, |changes, _| changes.fold_settle = None)
                .ok();
        }));
    }

    /// Replace every settled folding stand-in with its steady-state rows.
    /// Returns whether any stand-ins are still mid-tween.
    fn settle_folds(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(parsed) = &self.parsed else {
            return false;
        };
        let files = parsed.files.clone();
        let mut pending = false;
        for file_ix in (0..self.row_ranges.len()).rev() {
            let range = &self.row_ranges[file_ix];
            let folding = self.rows.get(range.start + 1)
                == Some(&DiffRow::FoldingBody {
                    file: file_ix as u32,
                });
            if !folding {
                continue;
            }
            let Some(file) = files.get(file_ix) else {
                continue;
            };
            let fold = self.folds.get(&file.path).copied().unwrap_or_default();
            if fold.animating() {
                pending = true;
                continue;
            }
            let body = if fold.collapsed {
                Vec::new()
            } else {
                layout::body_rows(self.view_layout, file_ix as u32, file)
            };
            self.replace_file_body(file_ix, body);
        }
        cx.notify();
        pending
    }

    /// Every parsed file currently folded shut?
    fn all_collapsed(&self) -> bool {
        let Some(parsed) = &self.parsed else {
            return false;
        };
        !parsed.files.is_empty()
            && parsed.files.iter().all(|file| {
                self.folds
                    .get(&file.path)
                    .is_some_and(|fold| fold.collapsed)
            })
    }

    /// Collapse every file section, or expand them all when everything is
    /// already shut (the toolbar's fold button, t3code parity). Steady-state
    /// writes — no per-row tween arming, the whole list just snaps. List
    /// splices run bottom-up over the OLD ranges (each is O(log n)), then
    /// the row model rebuilds wholesale; the scroll anchor rides the
    /// splices, landing on the nearest file header when its body vanishes.
    pub(super) fn toggle_collapse_all(&mut self, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else {
            return;
        };
        let collapse = !self.all_collapsed();
        let files = parsed.files.clone();
        for file in files.iter() {
            let fold = self.folds.entry(file.path.clone()).or_default();
            fold.collapsed = collapse;
            fold.toggled_at = None;
        }
        for file_ix in (0..self.row_ranges.len().min(files.len())).rev() {
            let range = &self.row_ranges[file_ix];
            let body = range.start + 1..range.end;
            let new_len = if collapse {
                0
            } else {
                layout::body_rows(self.view_layout, file_ix as u32, &files[file_ix]).len()
            };
            if body.len() != new_len {
                self.list.splice(body, new_len);
            }
        }
        let (rows, ranges) = layout::flatten(self.view_layout, &files, |_| collapse);
        self.rows = rows;
        self.row_ranges = ranges;
        self.invalidate_selection(cx);
        cx.notify();
    }

    pub(super) fn close_scope_menu(&mut self, cx: &mut Context<Self>) {
        if self.scope_menu.begin_close() {
            popover::reap_popup(cx, |changes: &mut Self| &mut changes.scope_menu);
        }
    }

    pub(super) fn close_ref_menu(&mut self, cx: &mut Context<Self>) {
        if self.ref_menu.begin_close() {
            popover::reap_popup(cx, |changes: &mut Self| &mut changes.ref_menu);
        }
    }

    pub(super) fn open_ref_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // "PaletteSearch" context: ↑↓/⏎ stay unbound in the input and bubble
        // to the card's key handler.
        let search = cx.new(|cx| TextInput::with_context("Search branches…", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, TextInputEvent::Edited) {
                if let Some(menu) = this.ref_menu.open_mut() {
                    menu.active = 0;
                }
                cx.notify();
            }
        });
        let handle = search.read(cx).focus_handle(cx);
        // The highlight starts ON the current base (query is empty, so the
        // filtered rows are just the branch list).
        let active = self
            .base_ref
            .as_ref()
            .and_then(|base| self.branches.iter().position(|b| b == base))
            .unwrap_or(0);
        self.ref_menu.open(RefMenu {
            search,
            active,
            focus: cx.focus_handle(),
            list_scroll: gpui::ScrollHandle::new(),
            _search_events: search_events,
        });
        // Focusable before first paint (the add-space palette's proven order).
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Filtered branch indices for the open ref menu (ranked substring match).
    pub(super) fn ref_menu_rows(&self, cx: &App) -> Vec<usize> {
        let query = self
            .ref_menu
            .get()
            .map(|menu| menu.search.read(cx).text().to_string())
            .unwrap_or_default();
        popover::filter_indices(&query, &self.branches)
    }

    /// Dropdown keys (bubbling from the focused search input): ↑↓ navigate,
    /// ⏎ picks the highlighted branch, Esc closes.
    pub(super) fn ref_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        // The card stays mounted (and focused) through the exit animation —
        // keys must not drive a dying menu.
        if !self.ref_menu.is_open() {
            return;
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => self.close_ref_menu(cx),
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = self.ref_menu_rows(cx).len();
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(menu) = self.ref_menu.open_mut() {
                    menu.active = popover::menu_step(Some(menu.active), count, delta).unwrap_or(0);
                    menu.list_scroll.scroll_to_item(menu.active);
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.ref_menu.get().map(|m| m.active).unwrap_or(0);
                let pick = self
                    .ref_menu_rows(cx)
                    .get(active)
                    .and_then(|ix| self.branches.get(*ix).cloned());
                if let Some(branch) = pick {
                    self.set_base_ref(branch, cx);
                    self.close_ref_menu(cx);
                }
            }
            _ => {}
        }
    }

    /// Start excerpt parsing and a lazy full-source fetch for an expanded file.
    pub(super) fn request_highlight(
        &mut self,
        file: &FileDiff,
        parsed_key: &str,
        cx: &mut Context<Self>,
    ) -> Option<Arc<DiffHighlights>> {
        let lang = cypher_syntax::language_for_path(&file.path)?;
        let fingerprint = hash64(&[parsed_key, &file.path]);
        if let Some(slot) = self.highlights.get(&file.path)
            && slot.fingerprint == fingerprint
        {
            return match &slot.state {
                DiffHighlightState::Ready(highlights) | DiffHighlightState::Excerpt(highlights) => {
                    Some(highlights.clone())
                }
                DiffHighlightState::Pending | DiffHighlightState::Plain => None,
            };
        }
        if !cypher_syntax::supports_language(lang) {
            self.highlights.insert(
                file.path.clone(),
                HighlightSlot {
                    fingerprint,
                    state: DiffHighlightState::Plain,
                    _excerpt_task: None,
                    _fetch_task: None,
                },
            );
            return None;
        }
        let path = file.path.clone();
        let excerpt_file = file.clone();
        let excerpt_path = path.clone();
        let excerpt_task = cx.spawn(async move |this, cx| {
            let highlights = cx
                .background_executor()
                .spawn(async move { excerpt_highlights(&excerpt_file, lang).map(Arc::new) })
                .await;
            this.update(cx, |changes, cx| {
                if let Some(slot) = changes.highlights.get_mut(&excerpt_path)
                    && slot.fingerprint == fingerprint
                    && matches!(slot.state, DiffHighlightState::Pending)
                {
                    slot.state = match highlights {
                        Some(highlights) => DiffHighlightState::Excerpt(highlights),
                        None => DiffHighlightState::Plain,
                    };
                    cx.notify();
                }
            })
            .ok();
        });

        let active = self.active_diff(cx);
        let engine = self.state.read(cx).engine().cloned();
        let target = self.desired_target(cx);
        let chat_id = self
            .state
            .read(cx)
            .selected_chat_row()
            .map(|chat| chat.id.clone());
        let mode = self.scope.mode().to_string();
        let base_ref = self.base_ref.clone();
        let commit_sha = (self.scope == DiffScope::Commit)
            .then(|| self.commit.as_ref().map(|commit| commit.sha.clone()))
            .flatten();
        let fetch_file = file.clone();
        let fetch_path = path.clone();
        let fetch_task = match (active, engine) {
            (Some(diff), Some(engine)) => Some(cx.spawn(async move |this, cx| {
                let request = cypher_proto::GetCheckoutFileDiffTextRequest {
                    checkout_id: diff.checkout_id,
                    cwd: diff.cwd,
                    path: fetch_path.clone(),
                    mode,
                    base_ref,
                    chat_id,
                    commit_sha,
                    diff_checksum: diff.checksum,
                };
                let mut params = serde_json::to_value(request)
                    .ok()
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                if let Some(target) = target {
                    params.insert("targetDeviceId".into(), serde_json::Value::String(target));
                }
                let response = engine
                    .client()
                    .call(
                        methods::GET_CHECKOUT_FILE_DIFF_TEXT,
                        serde_json::Value::Object(params),
                    )
                    .await
                    .ok()
                    .and_then(|value| {
                        serde_json::from_value::<cypher_proto::CheckoutFileDiffText>(value).ok()
                    });
                let highlights = match response {
                    Some(response) => {
                        cx.background_executor()
                            .spawn(async move {
                                full_highlights(&fetch_file, lang, &response).map(Arc::new)
                            })
                            .await
                    }
                    None => None,
                };
                this.update(cx, |changes, cx| {
                    if let Some(slot) = changes.highlights.get_mut(&fetch_path)
                        && slot.fingerprint == fingerprint
                        && let Some(highlights) = highlights
                    {
                        slot.state = DiffHighlightState::Ready(highlights);
                        cx.notify();
                    }
                })
                .ok();
            })),
            _ => None,
        };
        self.highlights.insert(
            file.path.clone(),
            HighlightSlot {
                fingerprint,
                state: DiffHighlightState::Pending,
                _excerpt_task: Some(excerpt_task),
                _fetch_task: fetch_task,
            },
        );
        None
    }
}
