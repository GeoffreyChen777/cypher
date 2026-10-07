//! Selections: what a pick writes to the draft config or the chat.

use super::*;

impl Pickers {
    pub(super) fn pick_ref(&mut self, row: RepoRef, cx: &mut Context<Self>) {
        // Refs are fixed at creation: an existing session can never move
        // (wing's rule — the footer renders read-only labels there, so this
        // is a belt-and-braces guard).
        if self.state.read(cx).selected_chat_row().is_some() {
            return;
        }
        // An explicit ref pick takes over from any programmatic pin — and the
        // draft it mirrored must go too. The pick's own mode decision reads
        // the effective mode FIRST (a pinned NewWorktree canvas keeps minting
        // a worktree off the picked ref), then the clear resets the mirror so
        // no stale branch/checkout survives a failed `switch_draft_ref`.
        let new_worktree = self.config.checkout == CheckoutKind::NewWorktree
            || self
                .pinned_plan(cx)
                .is_some_and(|p| matches!(p, CheckoutPlan::NewWorktree { .. }));
        self.clear_pinned_target();
        if row.worktree_path.is_some() {
            // Reuse the ref's existing worktree ("Current worktree") — the
            // t3code `reuseExistingWorktree` path.
            self.config.branch = Some(row.name.clone());
            self.config.checkout = CheckoutKind::Local;
        } else if new_worktree || row.current {
            // Base pick for a new worktree, or the already-current ref.
            self.config.branch = Some(row.name.clone());
        } else {
            // Local mode + a plain non-current ref: CHECK OUT the space
            // folder (full t3code `switchRef` — picking `main` means "put my
            // local checkout on main", it must never flip the mode).
            self.switch_draft_ref(row, cx);
            return;
        }
        self.animate_close(cx);
        cx.notify();
    }

    /// Draft-mode checkout switch: `git checkout` in the SPACE's folder
    /// (relay-forwarded for remote spaces). Success records the pick and
    /// refreshes tags; failure keeps the popover open with git's message.
    fn switch_draft_ref(&mut self, row: RepoRef, cx: &mut Context<Self>) {
        if self.switching.is_some() {
            return; // one switch at a time
        }
        let Some(space) = self.state.read(cx).selected_space_row().cloned() else {
            return;
        };
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        self.switch_error = None;
        self.switching = Some(row.name.clone());
        let ref_name = row.name.clone();
        self.switch_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            params.insert(
                "repoPath".into(),
                serde_json::Value::String(space.path.clone()),
            );
            params.insert(
                "refName".into(),
                serde_json::Value::String(ref_name.clone()),
            );
            if local.as_deref() != Some(space.device_id.as_str()) {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(space.device_id.clone()),
                );
            }
            let result = engine
                .client()
                .call(methods::SWITCH_REF, serde_json::Value::Object(params))
                .await;
            this.update(cx, |pickers, cx| {
                pickers.switching = None;
                match result {
                    Ok(_) => {
                        // An explicit draft switch replaces any programmatic pin
                        // (mirror included) with its own branch pick.
                        pickers.clear_pinned_target();
                        pickers.config.branch = Some(ref_name);
                        pickers.animate_close(cx);
                        pickers.ensure_refs(true, cx);
                    }
                    Err(err) => pickers.switch_error = Some(err.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn pick_checkout(&mut self, kind: CheckoutKind, cx: &mut Context<Self>) {
        // An explicit checkout-kind pick takes over from any programmatic pin
        // (mirror included) — the pick's own `kind` is re-applied below. A
        // pinned mirror is reset first, so the branch-drop rule below only
        // ever sees an unpinned MANUAL NewWorktree draft (which it must keep
        // honoring); a pinned NewWorktree's mirrored branch is already gone.
        self.clear_pinned_target();
        if kind == CheckoutKind::Local
            && self.config.checkout == CheckoutKind::NewWorktree
            && self.selected_ref_worktree(cx).is_none()
            && self.selected_ref().is_some_and(|r| !r.current)
        {
            // Back to "Current checkout" with a non-current plain ref picked:
            // drop the pick (we don't checkout the main folder) — the current
            // branch takes over.
            self.config.branch = None;
        }
        self.config.checkout = kind;
        self.animate_close(cx);
        cx.notify();
    }

    pub(super) fn pick_harness(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        if self.harness_locked(cx) {
            return;
        }
        if self.config.harness != Some(harness) {
            // The remembered model for this harness takes over via the
            // defaults fallback; a foreign pick must not linger.
            self.config.model = None;
            self.config.reasoning = None;
            self.config.model_options.clear();
        }
        self.config.harness = Some(harness);
        self.update_defaults(|defaults| defaults.harness = Some(harness));
        if harness == HarnessId::Mock {
            self.selected_provider = Some("mock".into());
        }
        self.model_scroll.set_offset(gpui::Point::default());
        self.ensure_models(harness, cx);
        // Re-anchor the keyboard highlight onto the new list's selected row.
        self.active = self.selected_model_index(cx);
        cx.notify();
    }

    pub(super) fn pick_provider(&mut self, provider: String, cx: &mut Context<Self>) {
        let harness = if provider == "mock" {
            HarnessId::Mock
        } else {
            HarnessId::Pi
        };
        if self.effective_harness(cx) != Some(harness) {
            if self.harness_locked(cx) {
                return;
            }
            self.pick_harness(harness, cx);
        }
        self.model_rail = ModelRail::Provider;
        self.selected_provider = Some(provider);
        self.model_scroll.set_offset(gpui::Point::default());
        self.active = self.selected_model_index(cx);
        self.model_scroll.scroll_to_item(self.active);
        cx.notify();
    }

    pub(super) fn viewed_provider(&self, cx: &App) -> Option<String> {
        if let Some(id) = self.selected_provider.clone() {
            return Some(id);
        }
        let harness = self.effective_harness(cx)?;
        let model_id = self
            .selected_model(cx)
            .map(|m| m.id.clone())
            .or_else(|| self.effective_model_id(cx).map(str::to_string))?;
        Some(model_provider_id(harness, &model_id))
    }

    /// Unique providers in catalog order for the left rail.
    pub(super) fn provider_tabs(&self, cx: &App) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut tabs = Vec::new();
        let effective = self.effective_harness(cx);
        let mut descriptors = self.rail_descriptors(cx);
        if self.harness_locked(cx) {
            descriptors.retain(|d| Some(d.id) == effective);
        }
        for descriptor in &descriptors {
            let Some(models) = self.models.get(&descriptor.id).and_then(|l| l.ready()) else {
                if descriptor.id == HarnessId::Mock && seen.insert("mock".into()) {
                    tabs.push("mock".into());
                }
                continue;
            };
            for model in models {
                let id = model_provider_id(descriptor.id, &model.id);
                if seen.insert(id.clone()) {
                    tabs.push(id);
                }
            }
        }
        tabs
    }

    pub(super) fn pick_model(&mut self, model_id: String, cx: &mut Context<Self>) {
        if let Some(harness) = self.effective_harness(cx) {
            self.selected_provider = Some(model_provider_id(harness, &model_id));
        }
        self.animate_close(cx);
        if self.state.read(cx).selected_chat.is_some() {
            // Existing chat: persist to the chat row (Mutate setChatConfig) —
            // survives restarts and syncs; next runs in this chat use it.
            self.update_chat_config(cx, move |config| config.model = Some(model_id));
        } else {
            // New chat: draft pick + sticky last-used memory for this harness.
            self.config.model = Some(model_id.clone());
            if let Some(harness) = self.effective_harness(cx) {
                let label = self
                    .models
                    .get(&harness)
                    .and_then(|l| l.ready())
                    .and_then(|models| models.iter().find(|m| m.id == model_id))
                    .map(|m| m.label.clone())
                    .unwrap_or_else(|| model_id.clone());
                self.update_defaults(|defaults| defaults.remember_model(harness, model_id, label));
            }
        }
        cx.notify();
    }

    pub(super) fn pick_reasoning(&mut self, level: ReasoningLevel, cx: &mut Context<Self>) {
        // Always a concrete selection (no toggle-back-to-default).
        if self.state.read(cx).selected_chat.is_some() {
            self.update_chat_config(cx, move |config| config.reasoning = Some(level));
        } else {
            self.config.reasoning = Some(level);
            self.update_defaults(|defaults| defaults.reasoning = Some(level));
        }
        cx.notify();
    }

    pub(super) fn pick_option(
        &mut self,
        option_id: String,
        choice_id: String,
        default: bool,
        cx: &mut Context<Self>,
    ) {
        if self.state.read(cx).selected_chat.is_some() {
            self.update_chat_config(cx, move |config| {
                if default {
                    config.model_options.remove(&option_id);
                } else {
                    config
                        .model_options
                        .insert(option_id, serde_json::Value::String(choice_id));
                }
            });
        } else if default {
            self.config.model_options.remove(&option_id);
        } else {
            self.config
                .model_options
                .insert(option_id, serde_json::Value::String(choice_id));
        }
        cx.notify();
    }

    /// Apply `change` to the selected chat's effective config and persist it:
    /// optimistic row stamp (chips update on click) + `Mutate setChatConfig`
    /// (LWW workspace write — restarts and other devices see it). The written
    /// row always carries the CONCRETE resolved model/reasoning, with the
    /// reasoning re-clamped to the (possibly just-changed) model's ladder.
    fn update_chat_config(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut ChatConfig)) {
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
            return;
        };
        let resolved = self.resolved(cx);
        let Some(mut config) = resolved.chat_config() else {
            return; // harness unknown (catalog + chat row both missing) — nothing safe to write
        };
        // Preserve fields the pickers don't own.
        if let Some(existing) = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|c| c.config.as_ref())
        {
            config.sandbox = existing.sandbox;
        }
        change(&mut config);
        // Reasoning must stay concrete for whatever model the row now names —
        // same ladder resolution as [`Self::trait_ladder`] (model levels, else
        // the harness's advertised ladder).
        if let Some(models) = self.models.get(&config.harness).and_then(|l| l.ready()) {
            let mut ladder = config
                .model
                .as_deref()
                .and_then(|id| models.iter().find(|m| m.id == id))
                .map(|m| m.reasoning_levels.clone())
                .unwrap_or_default();
            if ladder.is_empty()
                && let Some(descriptor) = self
                    .harnesses
                    .ready()
                    .and_then(|list| list.iter().find(|d| d.id == config.harness))
            {
                ladder = descriptor.reasoning_levels.clone();
            }
            if !ladder.is_empty() {
                config.reasoning = clamp_reasoning(config.reasoning, &ladder);
            }
        }
        self.state.update(cx, |state, cx| {
            state.set_chat_config_optimistic(&chat_id, config.clone(), cx);
        });
        // Temporary Side Chat: the stamped fork row IS the config — every
        // send reads it, and promotion persists it. Never `setChatConfig` a
        // row that doesn't exist in the workspace (it vanishes on dispose).
        if self.side_chat {
            return;
        }
        let Some(engine) = self.engine(cx) else {
            return;
        };
        self.mutate_task = Some(cx.spawn(async move |_, _| {
            let params = serde_json::json!({
                "op": "setChatConfig",
                "chatId": chat_id,
                "config": config,
            });
            if let Err(err) = engine.client().call(methods::MUTATE, params).await {
                tracing::warn!(error = %err, "setChatConfig mutate failed");
            }
        }));
    }
}
