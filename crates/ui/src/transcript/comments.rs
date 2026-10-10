//! Transcript comments (shared CommentPopup).

use super::*;

impl Transcript {
    /// Selection lifecycle callbacks for one row's text elements: a settle
    /// shows the shared [`crate::comment_popup::CommentPopup`] pill at the
    /// selection endpoint, a new drag or a clear hides it. Built per-row
    /// (each row's elements carry their own key) but target the transcript
    /// entity as a whole.
    pub(super) fn selection_ui_for(
        &self,
        _row_id: &SharedString,
        cx: &mut Context<Self>,
    ) -> markdown::render::SelectionUi {
        let popup = self.comment_popup.clone();
        let entity = cx.weak_entity();
        // Copied: the scope rides the 'static callbacks (Copy), so the
        // closures never borrow `self`.
        let scope = self.scope;
        // A fresh drag start closes ANY surface's pill — only one floating UI
        // may exist — without touching the just-begun selection. A cleared
        // selection hides only the TRANSCRIPT pill (scoped dismissal).
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
                    .update(cx, |this: &mut Transcript, _| this.chat_id.clone())
                    .ok()
                    .flatten()
                else {
                    return;
                };
                // Hands the shared popup the quote + head (it keeps the pill
                // attached to the text and clears the transcript wash on
                // save/cancel).
                let clear = {
                    let entity = entity.clone();
                    Rc::new(move |cx: &mut gpui::App| {
                        crate::markdown::selection::clear(scope);
                        entity.update(cx, |_, cx| cx.notify()).ok();
                    })
                };
                // Side Chat source: the message whose text the
                // selection settles in — resolved from the head row (the row
                // ids encode the entry; a row a doc commit replaced falls
                // back to `None`, and the engine then labels the context
                // from the transcript tail).
                let anchor_message_id = entity
                    .update(cx, |this: &mut Transcript, _| {
                        this.entry_for_row(snapshot.head_row())
                    })
                    .ok()
                    .flatten();
                // A quote taken from a displayed translation maps back to the
                // agent's own words now, against the transcript the selected
                // rows were built from — by save time a commit may have moved
                // on.
                let origin = entity
                    .update(cx, |this: &mut Transcript, cx| {
                        crate::quote_origin::agent_quote(
                            &this.state.read(cx).transcript,
                            &snapshot.spans,
                        )
                    })
                    .ok()
                    .flatten();
                if let Some(popup) = popup.upgrade() {
                    popup.update(cx, |popup, cx| {
                        popup.offer(
                            chat_id,
                            snapshot.text.clone(),
                            origin,
                            anchor,
                            crate::comment_popup::CommentOwner::Markdown(scope),
                            Some(crate::comment_popup::CommentHead {
                                key: snapshot.head_key.clone(),
                                ix: snapshot.head_ix,
                                scope,
                            }),
                            clear,
                            Some(cypher_proto::SideChatSource::Transcript { anchor_message_id }),
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

    /// Hide the shared floating affordance when it belongs to the transcript
    /// (never another surface's pill; never clears the selection that was
    /// just begun).
    fn dismiss_comment_ui(&mut self, cx: &mut Context<Self>) {
        if let Some(popup) = self.comment_popup.upgrade() {
            popup.update(cx, |popup, cx| {
                popup.dismiss_if_owner(crate::comment_popup::CommentOwner::Markdown(self.scope), cx)
            });
        }
    }

    /// Close the floating affordance and remove the transcript selection wash.
    /// Also shell-driven: a tile's tab closing or going to the background
    /// takes its transcript's comment pill/editor and selection with it.
    pub(crate) fn dismiss_comment_ui_and_selection(&mut self, cx: &mut Context<Self>) {
        self.dismiss_comment_ui(cx);
        crate::markdown::selection::clear(self.scope);
    }

    /// Whether `row_id` is still a live row — the offer's head row; false
    /// means it was replaced by a doc commit.
    fn row_live(&self, row_id: &str) -> bool {
        self.rows.iter().any(|r| r.id.as_ref() == row_id)
    }

    /// The message entry a row belongs to (Side Chat anchor): rows
    /// encode their entry in `Row::entry_id`, but only the transcript can
    /// resolve a row id to it. `None` when the row is gone (replaced by a
    /// doc commit).
    fn entry_for_row(&self, row_id: &str) -> Option<String> {
        self.rows
            .iter()
            .find(|r| r.id.as_ref() == row_id)
            .map(|r| r.entry_id.to_string())
    }

    /// The shared popup's TRANSCRIPT offer anchors to a row a doc commit
    /// replaced — dismiss it and the stale selection wash. Scope-checked so
    /// a diff pane's offer (whose row id never matches a transcript row) is
    /// never touched. Runs from render so a streaming splice is caught the
    /// same frame it lands.
    pub(super) fn dismiss_stale_popup(&mut self, cx: &mut Context<Self>) {
        let Some(popup) = self.comment_popup.upgrade() else {
            return;
        };
        // The head may come from the OFFER or the open EDITOR — a doc commit
        // replacing the row invalidates both (the editor's quote would stale
        // under the selection).
        let stale = popup.read(cx).head().is_some_and(|head| {
            head.scope == self.scope
                && !self.row_live(crate::markdown::selection::row_of_key(&head.key))
        });
        if stale {
            // Runs from RENDER while the transcript is mid-render: defer so
            // the popup's dismiss_and_clear (which notifies the transcript
            // via its clear-selection closure) can't re-enter self mid-frame.
            let popup = popup.clone();
            cx.defer(move |cx| {
                popup.update(cx, |popup, cx| popup.dismiss_and_clear(cx));
            });
        }
    }

    /// Request highlights for the code blocks of a tree. `only` limits to one
    /// block index (split rows); `None` covers the whole tree (live rows).
    fn code_highlight_for(
        &mut self,
        row_id: &SharedString,
        tree: &Arc<BlockTree>,
        only: Option<usize>,
        cx: &mut Context<Self>,
    ) -> HashMap<usize, Option<Arc<cypher_syntax::HighlightedDocument>>> {
        let mut out = HashMap::new();
        for (ix, top) in tree.blocks.iter().enumerate() {
            if only.is_some_and(|o| o != ix) {
                continue;
            }
            if let Block::CodeBlock { language, code } = &top.block
                && let Some(lang) = language
                    .as_deref()
                    .and_then(cypher_syntax::language_for_alias)
            {
                out.insert(
                    ix,
                    self.highlights.request(row_id.clone(), ix, lang, code, cx),
                );
            }
        }
        out
    }

    fn tool_diff_highlight_for(
        &mut self,
        row_id: &SharedString,
        tool_ix: usize,
        detail: &ToolDetail,
        cx: &mut Context<Self>,
    ) -> Option<Arc<crate::changes::DiffHighlights>> {
        let ToolDetail::Diff {
            file,
            old_text,
            new_text,
        } = detail
        else {
            return None;
        };
        let cache_row: SharedString = format!("{row_id}#tool-diff-{tool_ix}").into();
        let old = match old_text {
            Some(source) => {
                let path = file.old_path.as_deref().unwrap_or(&file.path);
                let lang = cypher_syntax::language_for_path(path)?;
                Some(
                    self.highlights
                        .request(cache_row.clone(), 0, lang, source, cx)?,
                )
            }
            None => None,
        };
        let new = match new_text {
            Some(source) => {
                let lang = cypher_syntax::language_for_path(&file.path)?;
                Some(self.highlights.request(cache_row, 1, lang, source, cx)?)
            }
            None => None,
        };
        Some(Arc::new(crate::changes::DiffHighlights { old, new }))
    }

    /// One top-level markdown block row: settled, or `live` under the
    /// streaming fade veil.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_markdown_block(
        &mut self,
        row_id: &SharedString,
        tree: &Arc<BlockTree>,
        block_ix: usize,
        live: bool,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Per-appended-chunk fade veil (opacity only — layout commits
        // instantly). Reduced motion renders with no veil at all.
        // Baseline rows (text already streamed when the transcript
        // attached) start seeded: the existing reply must not fade in
        // on a session switch — only fresh appends animate.
        let veil = (live && !motion::reduced_motion(cx)).then(|| {
            self.veils
                .entry(row_id.clone())
                .or_insert_with(|| {
                    if self.veil_baseline.contains(row_id) {
                        Rc::new(RefCell::new(RowVeil::seeded()))
                    } else {
                        Rc::default()
                    }
                })
                .clone()
        });
        let opts = RenderOptions {
            row_key: row_id.clone(),
            veil: veil.clone(),
            cache: Some(self.render_cache.clone()),
            now: Instant::now(),
            copy: Some(self.copy_ui_for(row_id, cx)),
            selection: Some(self.selection_ui_for(row_id, cx)),
            scope: self.scope,
        };
        let highlight = self.code_highlight_for(row_id, tree, Some(block_ix), cx);
        let Some(top) = tree.blocks.get(block_ix) else {
            return gpui::Empty.into_any_element();
        };
        let el = markdown::render::render_block(
            &top.block,
            block_ix,
            block_ix,
            &opts,
            theme,
            window,
            highlight
                .get(&block_ix)
                .and_then(|o| o.as_deref())
                .map(|document| document.lines.as_slice()),
        );
        // The attach pass for this row is done (every element rendered
        // above seeded its baseline synchronously): elements appearing
        // from the NEXT pass on are newly streamed and fade normally.
        if let Some(veil) = &veil {
            veil.borrow_mut().finish_seeding();
        }
        // Drive the veil clock: while any chunk is still dissolving,
        // repaint next frame (self-limiting — one callback per frame).
        if veil.is_some_and(|v| v.borrow().is_fading()) {
            let id = cx.entity_id();
            window.on_next_frame(move |_, cx| cx.notify(id));
        }
        el
    }

    /// The toggle over folded rows (a translation's original, a thought, a
    /// work run): a chevron tile and a quiet label, styled like a tool
    /// group's header. Clicking pins the other state and rebuilds the rows,
    /// which shows or hides the rows below it.
    pub(super) fn render_fold_toggle(
        &self,
        row_id: &SharedString,
        open: bool,
        label: SharedString,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = row_id.clone();
        div()
            .w_full()
            .flex()
            .child(
                div()
                    .id(SharedString::from(format!("{row_id}-toggle")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(4.0))
                    .h(px(26.0))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .hover(|s| s.text_color(theme.text))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_pins.insert(key.clone(), !open);
                        // A local fold change: rebuild despite an unchanged
                        // state revision.
                        this.synced_revision = None;
                        this.sync(cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(18.0))
                            .flex_none()
                            .rounded(px(5.0))
                            .bg(crate::kit::theme::ink(0.06))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(10.0))
                            .text_color(theme.text_muted.opacity(0.7))
                            .child(SharedString::from(if open { "▾" } else { "▸" })),
                    )
                    .child(div().min_w_0().truncate().child(label)),
            )
            .into_any_element()
    }

    /// A thought inside a work run: a chip on the run's rail like the tool
    /// calls around it — the bulb, "Thought" and the thought's first line, a
    /// spinner while it streams. Clicking shows or hides its text below.
    pub(super) fn render_thought_chip(
        &self,
        row_id: &SharedString,
        open: bool,
        live: bool,
        preview: SharedString,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = row_id.clone();
        let header = div()
            .h(px(CHIP_CARD_HEIGHT))
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .text_size(px(12.0))
            .child(
                div()
                    .size(px(18.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        crate::kit::icons::icon(crate::kit::icons::LIGHTBULB)
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_muted)
                    .child(SharedString::from(if live {
                        "Thinking…"
                    } else {
                        "Thought"
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text.opacity(0.85))
                    .child(preview),
            )
            .when(live, |row| {
                row.child(tool_status_icon(
                    ToolStatus::Running,
                    SharedString::from(format!("{row_id}-status")),
                    theme,
                ))
            })
            .child(
                div()
                    .size(px(18.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.0))
                    .text_color(theme.text_muted.opacity(0.8))
                    .child(SharedString::from(if open { "▾" } else { "▸" })),
            );
        div()
            .h(px(CHIP_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .child(guide_rail().h_full())
            .child(
                div()
                    .id(SharedString::from(format!("{row_id}-toggle")))
                    .ml(px(12.0))
                    .h(px(CHIP_CARD_HEIGHT))
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .rounded(px(9.0))
                    .border_1()
                    .border_color(crate::kit::theme::hairline(0.07))
                    .bg(crate::kit::theme::ink(0.03))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_pins.insert(key.clone(), !open);
                        // A local fold change: rebuild despite an unchanged
                        // state revision.
                        this.synced_revision = None;
                        this.sync(cx);
                        cx.notify();
                    }))
                    .child(header),
            )
            .into_any_element()
    }

    /// The row a capped work run folds its start behind, on the run's rail
    /// like the group's own overflow row. A click reveals the run whole, or
    /// caps it again.
    pub(super) fn render_run_overflow(
        &self,
        row_id: &SharedString,
        run: &SharedString,
        tools: usize,
        thoughts: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = run.clone();
        div()
            .id(SharedString::from(format!("{row_id}-toggle")))
            .h(px(OVERFLOW_ROW_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .cursor_pointer()
            .text_size(px(11.0))
            .text_color(theme.text_faint)
            .hover(|s| s.text_color(theme.text_muted))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.tool_overflow.remove(&key) {
                    this.tool_overflow.insert(key.clone());
                }
                // The cap is applied when rows are built: rebuild despite an
                // unchanged state revision.
                this.synced_revision = None;
                this.sync(cx);
                cx.notify();
            }))
            .child(guide_rail().h_full())
            .child(
                div()
                    .ml(px(12.0))
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(run_overflow_label(tools, thoughts))),
            )
            .into_any_element()
    }

    /// A tool group's header and chips. A `nested` group belongs to a work
    /// run: the run's toggle is its header, so it shows only its chips, less
    /// the `skip` leading ones its run's cap folds away.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_tool_group(
        &mut self,
        row_id: &SharedString,
        tools: &Arc<Vec<ToolItem>>,
        auto_open: bool,
        nested: bool,
        skip: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fold = self.folds.get(row_id).copied().unwrap_or_default();
        let open = nested || fold.open.unwrap_or(auto_open);
        // Under a header the chips start just below it; in a run, the row's
        // own gap places them.
        let chips_top_pad = if nested { 0.0 } else { CHIPS_TOP_PAD };
        // Cap: an open group renders its LAST `limit` chips, with the older
        // ones behind one "Show N earlier tool calls" row. A long agent run
        // then costs a bounded slice of the transcript instead of pushing the
        // answer off-screen. Revealing is per row and survives re-renders.
        // A work run caps its calls as a whole, under its own overflow row
        // ([`cap_work_runs`]).
        let (hidden, overflow_row) = if nested {
            (skip, false)
        } else {
            let revealed = self.tool_overflow.contains(row_id);
            let limit = crate::appearance::chat_style::settings(cx).tool_call_limit;
            let hidden = hidden_tool_count(tools.len(), limit, revealed);
            // The row stays after revealing (as "Show fewer") so the same
            // click target puts the chips back.
            let overflow_row =
                hidden > 0 || (revealed && hidden_tool_count(tools.len(), limit, false) > 0);
            (hidden, overflow_row)
        };
        // Chips render their EFFECTIVE detail: the precomputed doc-resident
        // one, upgraded in place by a fetched sidecar blob (chat2-sync A3).
        // Resolved per paint (a HashMap probe per chip) so fetched content
        // needs no row rebuild — arrival is a cx.notify, like a fold toggle.
        let details: Vec<Option<Arc<ToolDetail>>> = tools
            .iter()
            .map(|tool| {
                // Among fetched blobs, the most recently REQUESTED one wins —
                // a tool can carry both a diff and an output ref, and the
                // user's last click decides which upgrade is showing.
                let mut best: Option<(u64, Arc<ToolDetail>)> = None;
                for blob_ref in [&tool.diff_ref, &tool.output_ref].into_iter().flatten() {
                    if let Some(BlobFetch::Ready(detail)) = self.blob_details.get(blob_ref) {
                        let order = self.blob_fetch_order.get(blob_ref).copied().unwrap_or(0);
                        if best.as_ref().is_none_or(|(o, _)| order > *o) {
                            best = Some((order, detail.clone()));
                        }
                    }
                }
                best.map(|(_, d)| d).or_else(|| tool.detail.clone())
            })
            .collect();
        // Full-invocation blocks — with them, EVERY chip expands: the click
        // always answers "what exactly was this call?", output or not.
        let invocations: Vec<Option<Arc<ToolDetail>>> =
            tools.iter().map(|tool| tool.invocation.clone()).collect();
        // Fetch affordance under each open detail whose full payload is still
        // sidecar-only: `(ref, label)`. Diff offered first (the richer
        // upgrade), then the output — a fetched ref hands the affordance to
        // the NEXT unfetched one instead of retiring it (both must stay
        // reachable when a tool has both).
        let affordances: Vec<Option<(SharedString, SharedString)>> = tools
            .iter()
            .map(|tool| {
                // The currently-displayed ref (same recency rule as
                // `details` above): its affordance is spent; any OTHER
                // Ready ref stays offered as a no-fetch toggle.
                let shown: Option<&SharedString> = {
                    let mut best: Option<(u64, &SharedString)> = None;
                    for blob_ref in [&tool.diff_ref, &tool.output_ref].into_iter().flatten() {
                        if matches!(self.blob_details.get(blob_ref), Some(BlobFetch::Ready(_))) {
                            let order = self.blob_fetch_order.get(blob_ref).copied().unwrap_or(0);
                            if best.is_none_or(|(o, _)| order > o) {
                                best = Some((order, blob_ref));
                            }
                        }
                    }
                    best.map(|(_, r)| r)
                };
                let candidates = [
                    (tool.diff_ref.as_ref(), "diff", None),
                    (tool.output_ref.as_ref(), "output", tool.output_bytes),
                ];
                for (blob_ref, what, bytes) in candidates {
                    let Some(blob_ref) = blob_ref else { continue };
                    let label = match self.blob_details.get(blob_ref) {
                        Some(BlobFetch::Ready(_)) => {
                            if shown == Some(blob_ref) {
                                continue;
                            }
                            format!("Show full {what}")
                        }
                        Some(BlobFetch::Loading(_)) => format!("Loading full {what}…"),
                        Some(BlobFetch::Failed) => {
                            format!("Couldn't load full {what} — tap to retry")
                        }
                        None => match bytes {
                            Some(b) => format!("Show full {what} ({})", format_kb(b)),
                            None => format!("Show full {what}"),
                        },
                    };
                    return Some((blob_ref.clone(), SharedString::from(label)));
                }
                None
            })
            .collect();
        // Which chips have their detail block open (render-local, analytic —
        // the FINAL state; a mid-tween detail already counts as its target).
        let detail_folds: Vec<FoldState> = details
            .iter()
            .zip(&invocations)
            .enumerate()
            .map(|(ix, (detail, invocation))| {
                if detail.is_none() && invocation.is_none() {
                    return FoldState::default();
                }
                self.tool_details
                    .get(&SharedString::from(format!("{row_id}#d{ix}")))
                    .copied()
                    .unwrap_or_default()
            })
            .collect();
        let detail_opens: Vec<bool> = details
            .iter()
            .zip(&invocations)
            .zip(&detail_folds)
            .map(|((detail, invocation), fold)| {
                (detail.is_some() || invocation.is_some()) && fold.open.unwrap_or(false)
            })
            .collect();
        // Hidden chips are not rendered, so their diffs are not tokenized.
        let detail_highlights: Vec<Option<Arc<crate::changes::DiffHighlights>>> = details
            .iter()
            .enumerate()
            .map(|(ix, detail)| {
                detail
                    .as_deref()
                    .filter(|_| detail_opens[ix] && ix >= hidden)
                    .and_then(|detail| self.tool_diff_highlight_for(row_id, ix, detail, cx))
            })
            .collect();
        let open_height = chips_height(tools.len() - hidden) - (CHIPS_TOP_PAD - chips_top_pad)
            + if overflow_row {
                OVERFLOW_ROW_HEIGHT
            } else {
                0.0
            }
            + details
                .iter()
                .zip(&invocations)
                .zip(&affordances)
                .zip(&detail_opens)
                .enumerate()
                .filter(|(ix, (_, open))| **open && *ix >= hidden)
                .map(|(_, (((detail, invocation), affordance), _))| {
                    invocation.as_deref().map_or(0.0, detail_height)
                        + detail.as_deref().map_or(0.0, detail_height)
                        + if affordance.is_some() {
                            BLOB_AFFORDANCE_HEIGHT
                        } else {
                            0.0
                        }
                })
                .sum::<f32>();
        let target = if open { open_height } else { 0.0 };
        let summary = tool_group_summary(tools);

        let toggle_id = row_id.clone();
        // Header (zeron tool-group.tsx): a small chevron tile centered over the
        // chips' guide rail, then the quiet 12px summary.
        let header = div()
            .id(SharedString::from(format!("{row_id}-hdr")))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(4.0))
            .h(px(26.0))
            .cursor_pointer()
            .text_size(px(12.0))
            // Quiet even when children failed: agents routinely have failed
            // probes mid-work, and a red HEADER read as "this whole step
            // broke" (user report). Failures still show on the individual
            // chips (destructive tint, zeron tool-chip.tsx) and in the
            // summary's "· N failed" count.
            .text_color(theme.text_muted)
            .hover(|s| s.text_color(theme.text))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_fold(toggle_id.clone(), open_height, auto_open);
                cx.notify();
            }))
            .child(
                // The group header keeps its chevron TILE (the chips' icons
                // and their own chevrons are bare): it is the row's only
                // affordance, and the tile centers the guide rail below it.
                div()
                    .size(px(18.0))
                    .flex_none()
                    .rounded(px(5.0))
                    .bg(crate::kit::theme::ink(0.06))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.0))
                    .text_color(theme.text_muted.opacity(0.7))
                    .child(SharedString::from(if open { "▾" } else { "▸" })),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(summary)),
            );

        let overflow = overflow_row.then(|| {
            let label = if hidden > 0 {
                format!(
                    "Show {hidden} earlier tool call{}",
                    if hidden == 1 { "" } else { "s" }
                )
            } else {
                "Show fewer tool calls".to_string()
            };
            let key = row_id.clone();
            div()
                .id(SharedString::from(format!("{row_id}-overflow")))
                .h(px(OVERFLOW_ROW_HEIGHT))
                .w_full()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .cursor_pointer()
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .hover(|s| s.text_color(theme.text_muted))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.tool_overflow.remove(&key) {
                        this.tool_overflow.insert(key.clone());
                    }
                    // Same trick as a detail toggle: arm the group body's
                    // height tween (open state untouched) so the chips slide
                    // in and the content below tracks them instead of
                    // teleporting. `open_height` is the pre-click height,
                    // i.e. exactly the tween's start.
                    let group = this.folds.entry(key.clone()).or_default();
                    group.from = open_height;
                    group.epoch += 1;
                    group.toggled_at = Some(Instant::now());
                    cx.notify();
                }))
                // The guide rail runs through this row like it does through
                // the chips, so the fold reads as part of the same stack.
                .child(
                    div()
                        .ml(px(12.0))
                        .h_full()
                        .w(px(1.0))
                        .flex_none()
                        .bg(crate::kit::theme::ink(0.08)),
                )
                .child(
                    div()
                        .ml(px(12.0))
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(label)),
                )
        });
        let chips = div()
            .pt(px(chips_top_pad))
            .flex()
            .flex_col()
            .gap(px(CHIP_GAP))
            .children(overflow)
            .children(tools.iter().enumerate().skip(hidden).map(|(ix, tool)| {
                let detail = details[ix].clone();
                let invocation = invocations[ix].clone();
                let key = SharedString::from(format!("{row_id}#d{ix}"));
                if detail.is_none() && invocation.is_none() {
                    return tool_chip(tool, &key, theme);
                }
                let affordance = affordances[ix].clone();
                let affordance_h = if affordance.is_some() {
                    BLOB_AFFORDANCE_HEIGHT
                } else {
                    0.0
                };
                let open = detail_opens[ix];
                let dfold = detail_folds[ix];
                // Expandable chip: ONE card whose header row is the chip and
                // whose body is the detail — not a floating card below it.
                // The guide rail stretches with the row, so an open detail
                // never breaks the rail.
                //
                // The card's height is EXPLICIT (border-box), not intrinsic:
                // an auto-height card adds its 2px of borders on top of the
                // 30px header, and with N chips that overflowed the group's
                // analytic height by 2N px — the last chips rendered clipped
                // (user report: "tool calls cut off at the bottom"). The
                // explicit height is also what the open/close tween animates.
                let closed_h = CHIP_CARD_HEIGHT;
                let open_h = CHIP_CARD_HEIGHT
                    + invocation.as_deref().map_or(0.0, detail_height)
                    + detail.as_deref().map_or(0.0, detail_height)
                    + affordance_h;
                let card_target = if open { open_h } else { closed_h };
                let animating = dfold.epoch > 0
                    && dfold
                        .toggled_at
                        .is_some_and(|at| at.elapsed() < FOLD_TWEEN_WINDOW);
                let toggle_key = key.clone();
                let group_key = row_id.clone();
                let mut card = div()
                    .my(px((CHIP_HEIGHT - CHIP_CARD_HEIGHT) / 2.0))
                    .ml(px(12.0))
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(px(9.0))
                    .border_1()
                    .border_color(crate::kit::theme::hairline(0.07))
                    .bg(crate::kit::theme::ink(0.03))
                    .child(
                        div()
                            .id(key.clone())
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let entry =
                                    this.tool_details.entry(toggle_key.clone()).or_default();
                                let currently_open = entry.open.unwrap_or(false);
                                entry.from = if currently_open { open_h } else { closed_h };
                                entry.open = Some(!currently_open);
                                entry.epoch += 1;
                                entry.toggled_at = Some(Instant::now());
                                // Arm the GROUP body's height tween too (open
                                // state untouched): the body's height is
                                // analytic over the final detail state, so
                                // without a tween the row snaps to the target
                                // height while the card is still mid-tween —
                                // content below teleported on expand and the
                                // shrinking card clipped on collapse (user
                                // report). `open_height` was computed with
                                // the detail still in its pre-click state,
                                // which is exactly the tween's start; both
                                // tweens share the click instant and the
                                // RESIZE curve, so the row tracks the card's
                                // bottom edge frame-for-frame.
                                let group = this.folds.entry(group_key.clone()).or_default();
                                group.from = open_height;
                                group.epoch += 1;
                                group.toggled_at = Some(Instant::now());
                                cx.notify();
                            }))
                            .child(chip_header(tool, open, &key, theme)),
                    );
                // The body stays mounted while the close tween shrinks over it.
                // Invocation first (what was asked), then output/diff (what
                // came back), each under its own hairline.
                if open || animating {
                    if let Some(invocation) = invocation.as_deref() {
                        card = card
                            .child(
                                div()
                                    .h(px(DETAIL_SEPARATOR))
                                    .flex_none()
                                    .bg(crate::kit::theme::hairline(0.06)),
                            )
                            .child(detail_body(invocation, None, theme));
                    }
                    if let Some(detail) = detail.as_deref() {
                        card = card
                            .child(
                                div()
                                    .h(px(DETAIL_SEPARATOR))
                                    .flex_none()
                                    .bg(crate::kit::theme::hairline(0.06)),
                            )
                            .child(detail_body(detail, detail_highlights[ix].clone(), theme));
                    }
                    if let Some((blob_ref, label)) = affordance {
                        let loading = matches!(
                            self.blob_details.get(&blob_ref),
                            Some(BlobFetch::Loading(_))
                        );
                        let mut row = div()
                            .id(SharedString::from(format!("{key}-blob")))
                            .h(px(BLOB_AFFORDANCE_HEIGHT))
                            .flex_none()
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .text_size(px(10.5))
                            .text_color(theme.text_faint)
                            .child(label);
                        if !loading {
                            row = row
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme.text_muted))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.spawn_blob_fetch(blob_ref.clone(), cx);
                                    cx.notify();
                                }));
                        }
                        card = card.child(row);
                    }
                }
                let card: AnyElement = if animating {
                    let from = dfold.from;
                    card.with_animation(
                        SharedString::from(format!("{key}-tween{}", dfold.epoch)),
                        RESIZE.animation(),
                        move |el, t| el.h(px(motion::lerp(from, card_target, t))),
                    )
                    .into_any_element()
                } else {
                    card.h(px(card_target)).into_any_element()
                };
                let card = div().min_w_0().flex_1().child(card);
                div()
                    .w_full()
                    .flex_none()
                    .flex()
                    .flex_row()
                    // Guide rail: no fixed height — stretches to the card,
                    // detail included.
                    .child(
                        div()
                            .ml(px(12.0))
                            .w(px(1.0))
                            .flex_none()
                            .bg(crate::kit::theme::ink(0.08)),
                    )
                    .children(nested_rails(tool.depth))
                    .child(card)
                    .into_any_element()
            }));

        // Fold body: 200ms committed-height tween on a USER toggle only — and
        // only within a short window of the click. Auto-open (streaming) and
        // content growth never tween, and a SETTLED fold renders at its static
        // height: leaving the tween armed replayed it on every remount, which
        // in a virtualized list means every scroll-back-into-view (only `open`
        // toggles animate — composes with the stick spring).
        let animating = fold.epoch > 0
            && fold
                .toggled_at
                .is_some_and(|at| at.elapsed() < FOLD_TWEEN_WINDOW);
        let body: AnyElement = if animating {
            let from = fold.from;
            div()
                .overflow_hidden()
                .child(chips)
                .with_animation(
                    SharedString::from(format!("{row_id}-fold{}", fold.epoch)),
                    RESIZE.animation(),
                    move |el, t| el.h(px(motion::lerp(from, target, t))),
                )
                .into_any_element()
        } else {
            div()
                .overflow_hidden()
                .h(px(target))
                .child(chips)
                .into_any_element()
        };
        if nested {
            return body;
        }

        div()
            .flex()
            .flex_col()
            .child(header)
            .child(body)
            .into_any_element()
    }
}
