//! Keyboard navigation inside the open popover.

use super::*;

impl Pickers {
    /// The traits popover's reasoning ladder (model levels, falling back to
    /// the harness's advertised ladder) — shared by render and keyboard nav.
    pub(super) fn trait_ladder(&self, cx: &App) -> Vec<ReasoningLevel> {
        let Some(model) = self.selected_model(cx) else {
            return Vec::new();
        };
        if !model.reasoning_levels.is_empty() {
            return model.reasoning_levels.clone();
        }
        self.effective_harness(cx)
            .and_then(|h| {
                self.harnesses
                    .ready()
                    .and_then(|list| list.iter().find(|d| d.id == h))
                    .map(|d| d.reasoning_levels.clone())
            })
            .unwrap_or_default()
    }

    /// The harness descriptors the picker rail offers, with the committed
    /// harness force-included even when it's outside the offered set (a
    /// dev session's mock harness, or one disabled after the chat existed).
    pub(super) fn rail_descriptors(&self, cx: &App) -> Vec<HarnessDescriptor> {
        let Some(list) = self.harnesses.ready() else {
            return Vec::new();
        };
        let mut descriptors = offered_harnesses(list);
        if let Some(effective) = self.effective_harness(cx)
            && !descriptors.iter().any(|d| d.id == effective)
            && let Some(descriptor) = list.iter().find(|d| d.id == effective)
        {
            descriptors.insert(0, descriptor.clone());
        }
        descriptors
    }

    /// The model rows the picker currently shows, flat and in render order —
    /// keyboard nav, ⌘N jumps, Enter and the render walk THE SAME list.
    ///
    /// A live search spans every ready harness (t3: the sidebar hides and
    /// the query ignores it); otherwise the rail selection decides —
    /// favorites across harnesses, or the effective harness's list with its
    /// starred rows floated to the top (t3 `groupFavorites`). A locked chat
    /// restricts every view to its own harness.
    pub(super) fn visible_model_rows(&self, cx: &App) -> Vec<ModelRowData> {
        let effective = self.effective_harness(cx);
        let mut descriptors = self.rail_descriptors(cx);
        if self.harness_locked(cx) {
            descriptors.retain(|d| Some(d.id) == effective);
        }
        let row = |descriptor: &HarnessDescriptor, model: &Model| {
            let provider_id = model_provider_id(descriptor.id, &model.id);
            ModelRowData {
                harness: descriptor.id,
                provider_title: provider_display_name(&provider_id),
                provider_id,
                model: model.clone(),
            }
        };
        let query = self.search.read(cx).text().trim().to_string();
        if !query.is_empty() {
            // Rank: label prefix < label substring < provider-name hit;
            // stars, then input order, break ties (t3 modelPickerSearch's
            // field ladder + favorite boost, collapsed to our ranks).
            let mut ranked: Vec<(usize, usize, usize, ModelRowData)> = Vec::new();
            let mut input_ix = 0usize;
            for descriptor in &descriptors {
                let Some(models) = self.models.get(&descriptor.id).and_then(|l| l.ready()) else {
                    continue;
                };
                for model in models {
                    let provider =
                        provider_display_name(&model_provider_id(descriptor.id, &model.id));
                    let by_label = popover::match_rank(&query, &model.label);
                    let by_provider =
                        popover::match_rank(&query, &format!("{} {}", provider, model.label))
                            .map(|rank| rank + 2);
                    if let Some(rank) = by_label.into_iter().chain(by_provider).min() {
                        let starred = !self.defaults.is_favorite(descriptor.id, &model.id);
                        ranked.push((rank, starred as usize, input_ix, row(descriptor, model)));
                    }
                    input_ix += 1;
                }
            }
            ranked.sort_by_key(|(rank, unstarred, ix, _)| (*rank, *unstarred, *ix));
            return ranked.into_iter().map(|(_, _, _, row)| row).collect();
        }
        match self.model_rail {
            ModelRail::Favorites => {
                let mut rows = Vec::new();
                for descriptor in &descriptors {
                    let Some(models) = self.models.get(&descriptor.id).and_then(|l| l.ready())
                    else {
                        continue;
                    };
                    for model in models {
                        if self.defaults.is_favorite(descriptor.id, &model.id) {
                            rows.push(row(descriptor, model));
                        }
                    }
                }
                rows
            }
            ModelRail::Provider => {
                let viewed = self.viewed_provider(cx);
                let mut rows = Vec::new();
                for descriptor in &descriptors {
                    let Some(models) = self.models.get(&descriptor.id).and_then(|l| l.ready())
                    else {
                        continue;
                    };
                    for model in models {
                        if Some(model_provider_id(descriptor.id, &model.id).as_str())
                            == viewed.as_deref()
                        {
                            rows.push(row(descriptor, model));
                        }
                    }
                }
                let (starred, rest): (Vec<ModelRowData>, Vec<ModelRowData>) = rows
                    .into_iter()
                    .partition(|row| self.defaults.is_favorite(row.harness, &row.model.id));
                starred.into_iter().chain(rest).collect()
            }
        }
    }

    /// The row the keyboard-nav highlight starts on: the resolved selected
    /// model's index in the VISIBLE rows (the favorites/search views may not
    /// contain it — then 0), 0 while the list is loading.
    pub(super) fn selected_model_index(&self, cx: &App) -> usize {
        let selected = self.selected_model(cx).map(|m| m.id.clone());
        let effective = self.effective_harness(cx);
        self.visible_model_rows(cx)
            .iter()
            .position(|row| {
                Some(row.harness) == effective && selected.as_deref() == Some(row.model.id.as_str())
            })
            .unwrap_or(0)
    }

    /// The picker's visible row count (keyboard nav bounds).
    pub(super) fn model_rows_len(&self, cx: &App) -> usize {
        self.visible_model_rows(cx).len()
    }

    /// Enter on the harness/model popover: pick the highlighted model.
    pub(super) fn activate_model_row(&mut self, cx: &mut Context<Self>) {
        self.activate_model_index(self.active, cx);
    }

    /// Pick the visible row at `ix` — a foreign-harness row (favorites /
    /// search) switches the harness first, exactly like clicking its rail
    /// icon and then the model.
    pub(super) fn activate_model_index(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.visible_model_rows(cx).into_iter().nth(ix) else {
            return;
        };
        if self.effective_harness(cx) != Some(row.harness) {
            if self.harness_locked(cx) {
                return;
            }
            self.pick_harness(row.harness, cx);
        }
        self.pick_model(row.model.id, cx);
    }

    /// Star/unstar a model and persist it with the sticky defaults.
    pub(super) fn toggle_model_favorite(
        &mut self,
        harness: HarnessId,
        model: &str,
        cx: &mut Context<Self>,
    ) {
        // Set to the opposite of what THIS picker shows (a blind toggle on
        // the re-read file would undo a star another tile just made).
        let starred = !self.defaults.is_favorite(harness, model);
        self.update_defaults(|defaults| {
            if defaults.is_favorite(harness, model) != starred {
                defaults.toggle_favorite(harness, model);
            }
        });
        // Starring REORDERS the list (stars float to the top / leave the
        // favorites view) — re-home the keyboard highlight onto the SELECTED
        // row so exactly one row reads highlighted afterwards. Following the
        // starred row instead left its cursor wash next to the selected
        // row's ring: "two highlighted rows" (user report, twice).
        self.active = self.selected_model_index(cx);
        cx.notify();
    }

    pub(super) fn filtered_ref_rows(&self, cx: &App) -> Vec<RepoRef> {
        let Some(refs) = self.refs.ready() else {
            return Vec::new();
        };
        let names: Vec<String> = refs.iter().map(|r| r.name.clone()).collect();
        let query = self.search.read(cx).text().to_string();
        popover::filter_indices(&query, &names)
            .into_iter()
            .map(|ix| refs[ix].clone())
            .collect()
    }
}
