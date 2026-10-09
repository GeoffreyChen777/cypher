//! Catalog, model and ref loads.

use super::*;

impl Pickers {
    pub(super) fn ensure_harnesses(&mut self, force: bool, cx: &mut Context<Self>) {
        self.sync_catalog_owner(cx);
        // Non-forced (the render loop's eager kick) only loads from Idle: an
        // Error that could re-trigger a load would flip back to Loading
        // before the retry row ever painted (and spam the engine); Retry
        // resets to Idle. FORCED refreshes (a Settings → Agents toggle, a
        // picker open) reload through Ready/Error too — the enabled set just
        // changed under the cache, which otherwise served the boot-time
        // catalog until restart (user report). Stale-while-revalidate: loaded
        // rows stay on screen while the fresh catalog lands.
        let reload = match self.harnesses {
            Loadable::Idle => true,
            Loadable::Loading => false,
            Loadable::Ready(_) | Loadable::Error(_) => force,
        };
        if !reload {
            return;
        }
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let target = self.space_target(cx);
        let generation = self.model_generation;
        if !matches!(self.harnesses, Loadable::Ready(_)) {
            self.harnesses = Loadable::Loading;
        }
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            if let Some(target) = &target {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target.clone()),
                );
            }
            let result = engine
                .client()
                .call(methods::LIST_HARNESSES, serde_json::Value::Object(params))
                .await;
            this.update(cx, |pickers, cx| {
                if generation != pickers.model_generation {
                    return;
                }
                pickers.harnesses = match result {
                    Ok(value) => match serde_json::from_value::<Vec<HarnessDescriptor>>(value) {
                        Ok(list) => Loadable::Ready(list),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(err.to_string()),
                };
                pickers.prefetch_models(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Kick a model load for the effective harness AND every offered one, in
    /// parallel — by the time the user opens the picker (or switches rail
    /// tabs) the lists are already there, instead of a per-selection
    /// "Loading models…" round-trip. Each `ensure_models` call is guarded by
    /// its slot state, so re-running this every catalog load/render is free.
    pub(super) fn prefetch_models(&mut self, cx: &mut Context<Self>) {
        let mut targets: Vec<HarnessId> = match self.harnesses.ready() {
            Some(list) => offered_harnesses(list).iter().map(|d| d.id).collect(),
            None => Vec::new(),
        };
        // The committed chat's harness may be outside the offered set (e.g.
        // disabled after the chat was created) — its models still matter.
        // Retired harnesses have no driver to list models from.
        if let Some(effective) = self.effective_harness(cx)
            && matches!(effective, HarnessId::Pi | HarnessId::Mock)
            && !targets.contains(&effective)
        {
            targets.push(effective);
        }
        for harness in targets {
            self.ensure_models(harness, cx);
        }
    }

    pub(super) fn ensure_models(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        // Absent or Idle only — same render-loop hazard as `ensure_harnesses`;
        // the retry row clears the map to re-arm.
        if self
            .models
            .get(&harness)
            .is_some_and(|slot| !matches!(slot, Loadable::Idle))
        {
            return;
        }
        if self.engine(cx).is_none() {
            return;
        }
        self.models.insert(harness, Loadable::Loading);
        self.load_models(harness, cx);
    }

    /// Stale-while-revalidate for the opened model menu: every catalog that
    /// already loaded is fetched again behind the rows on screen. The engine
    /// answers from its own cache unless the host's Runtime or providers
    /// changed, so a reopen costs one round trip and never a skeleton; a
    /// failed refresh keeps the rows that were already usable.
    pub(super) fn revalidate_ready_models(&mut self, cx: &mut Context<Self>) {
        let ready: Vec<HarnessId> = self
            .models
            .iter()
            .filter(|(_, slot)| matches!(slot, Loadable::Ready(_)))
            .map(|(harness, _)| *harness)
            .collect();
        for harness in ready {
            self.load_models(harness, cx);
        }
    }

    /// A refresh that failed must not replace rows the user can still pick
    /// from; only a first load (the slot is `Loading`) surfaces the error row.
    pub(super) fn keeps_current_rows(
        current: Option<&Loadable<Vec<Model>>>,
        loaded: &Loadable<Vec<Model>>,
    ) -> bool {
        matches!(loaded, Loadable::Error(_)) && matches!(current, Some(Loadable::Ready(_)))
    }

    pub(super) fn rows_changed(
        previous: Option<&Loadable<Vec<Model>>>,
        next: Option<&Loadable<Vec<Model>>>,
    ) -> bool {
        match (previous, next) {
            (Some(Loadable::Ready(before)), Some(Loadable::Ready(after))) => before != after,
            _ => true,
        }
    }

    fn load_models(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let target = self.space_target(cx);
        let generation = self.model_generation;
        cx.spawn(async move |this, cx| {
            let mut params = serde_json::json!({ "harness": harness });
            if let (Some(target), Some(object)) = (&target, params.as_object_mut()) {
                object.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target.clone()),
                );
            }
            let result = engine.client().call(methods::LIST_MODELS, params).await;
            this.update(cx, |pickers, cx| {
                if generation != pickers.model_generation {
                    return;
                }
                let loaded = match result {
                    Ok(value) => match serde_json::from_value::<Vec<Model>>(value) {
                        // Display hygiene for catalogs from older engines
                        // (`default` alias rows, orphan `[1m]` variants,
                        // version-less alias labels).
                        Ok(models) => Loadable::Ready(normalize_model_rows(models)),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(err.to_string()),
                };
                if Self::keeps_current_rows(pickers.models.get(&harness), &loaded) {
                    return;
                }
                if let Loadable::Ready(models) = &loaded {
                    let fresh = models
                        .iter()
                        .any(|m| pickers.defaults.label_for(&m.id) != Some(m.label.as_str()));
                    if fresh {
                        pickers.update_defaults(|defaults| {
                            defaults.remember_labels(
                                models.iter().map(|m| (m.id.as_str(), m.label.as_str())),
                            );
                        });
                    }
                }
                let previous = pickers.models.insert(harness, loaded);
                // A list that landed while its popover is open re-anchors the
                // keyboard highlight onto the selected row (it sat at 0 while
                // loading). A revalidation that confirmed the same rows leaves
                // the user's arrow-key position alone.
                if pickers.open_kind() == Some(PickerKind::HarnessModel)
                    && pickers.effective_harness(cx) == Some(harness)
                    && Self::rows_changed(previous.as_ref(), pickers.models.get(&harness))
                {
                    pickers.active = pickers.selected_model_index(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// ListRefs for the selected SPACE's folder — targeted at the space's
    /// device (relay-forwarded when remote), keyed/invalidated by space id.
    /// Rows carry checkout state (`current`, `worktreePath`) so the picker can
    /// tag refs and the checkout-kind selector can offer worktree reuse.
    pub(super) fn ensure_refs(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(space) = self.state.read(cx).selected_space_row().cloned() else {
            return;
        };
        if !space.git_detected {
            return;
        }
        let fresh = self.refs_space.as_deref() == Some(space.id.as_str());
        if fresh && matches!(self.refs, Loadable::Loading) {
            return; // a load is already in flight
        }
        // Non-forced (the footer's eager kick, re-run every render) only loads
        // from Idle: an Error must WAIT for an explicit retry/reopen (force),
        // or re-render would flip Error back to Loading before the retry row
        // ever paints — an eternal skeleton plus an RPC storm (user report:
        // "the ref dropdown never loads anything").
        if !force && fresh && !matches!(self.refs, Loadable::Idle) {
            return;
        }
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        // Stale-while-revalidate: a forced refresh of an already-loaded space
        // keeps the current rows on screen while the reload runs — a send that
        // just minted a worktree (or a terminal-side branch) appears on the
        // popover's next open without the list ever flashing to a skeleton.
        if !(force && fresh && matches!(self.refs, Loadable::Ready(_))) {
            self.refs = Loadable::Loading;
        }
        self.refs_space = Some(space.id.clone());
        let refs_space_id = space.id.clone();
        self.refs_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            params.insert(
                "repoPath".into(),
                serde_json::Value::String(space.path.clone()),
            );
            if local.as_deref() != Some(space.device_id.as_str()) {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(space.device_id.clone()),
                );
            }
            let result = engine
                .client()
                .call(methods::LIST_REFS, serde_json::Value::Object(params))
                .await;
            this.update(cx, |pickers, cx| {
                // Guard stale responses: a space switch mid-flight clears the
                // slot (`refs_space = None`) — the old space's rows must not
                // land under the new project.
                if pickers.refs_space.as_deref() != Some(refs_space_id.as_str()) {
                    return;
                }
                pickers.refs = match result {
                    Ok(value) => match serde_json::from_value::<Vec<RepoRef>>(value) {
                        Ok(refs) => Loadable::Ready(refs),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(err.to_string()),
                };
                // Rows landed under an open, un-searched popover: re-home the
                // nav highlight to the selected row.
                if pickers.open_kind() == Some(PickerKind::Branch)
                    && pickers.search.read(cx).text().is_empty()
                {
                    pickers.active = pickers.selected_ref_index(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }
}
