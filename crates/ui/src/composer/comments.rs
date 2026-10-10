//! Surface comments (transcript, Git diff, terminal).

use super::*;

impl Composer {
    /// Append a comment saved in a surface's anchored editor (transcript,
    /// Git diff, terminal). Ignores comments whose chat no longer matches the
    /// selection (the shell forwards — a switch in between drops the
    /// comment).
    pub fn add_comment(
        &mut self,
        chat_id: String,
        quote: String,
        origin: Option<cypher_proto::agent_prompt::AgentQuote>,
        comment: String,
        cx: &mut Context<Self>,
    ) {
        // Temporary side chats offer no annotation surface (no comment pill,
        // no nested Side Chat) — drop any stray forwarded comment defensively.
        if matches!(self.transport, ComposerTransport::SideChat(_)) {
            return;
        }
        if self.state.read(cx).selected_chat.as_deref() != Some(chat_id.as_str()) {
            return;
        }
        self.comments.push(DraftComment {
            id: uuid::Uuid::new_v4().to_string(),
            quote,
            origin,
            comment,
        });
        cx.notify();
    }

    fn remove_comment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.comments.len() {
            return;
        }
        self.comments.remove(index);
        // Keep the edit session's index coherent (or drop it if it was the
        // removed row).
        if let Some(edit) = &mut self.comment_edit {
            if edit.index == index {
                self.comment_edit = None;
            } else if edit.index > index {
                edit.index -= 1;
            }
        }
        // An empty list closes the inspector (nothing left to show).
        if self.comments.is_empty() && self.comments_popup.begin_close() {
            crate::kit::popover::reap_popup(cx, |this: &mut Self| &mut this.comments_popup);
        }
        cx.notify();
    }

    fn begin_comment_edit(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.comments.len() {
            return;
        }
        let input = cx.new(|cx| ComposerInput::new("Edit comment…", cx));
        input.update(cx, |input, cx| {
            input.set_text(self.comments[index].comment.clone(), cx)
        });
        // Enter saves; Shift+Enter newlines (the input's standard mapping);
        // Escape propagates (no mentions open) to the inspector's key handler.
        let _events = cx.subscribe(&input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.save_comment_edit(cx);
            }
        });
        let focus = input.read(cx).focus_handle(cx);
        self.comment_edit = Some(CommentEdit {
            index,
            input,
            _events,
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Save an in-progress edit. A blank body is a no-op (the editor stays).
    fn save_comment_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.comment_edit.take() else {
            return;
        };
        let text = edit.input.read(cx).text().trim().to_string();
        if text.is_empty() {
            self.comment_edit = Some(edit);
            return;
        }
        if let Some(comment) = self.comments.get_mut(edit.index) {
            comment.comment = text;
        }
        cx.notify();
    }

    fn cancel_comment_edit(&mut self, cx: &mut Context<Self>) {
        if self.comment_edit.take().is_some() {
            cx.notify();
        }
    }

    fn close_comments_popup(&mut self, cx: &mut Context<Self>) {
        if self.comments_popup.begin_close() {
            crate::kit::popover::reap_popup(cx, |this: &mut Self| &mut this.comments_popup);
        }
        self.comment_edit = None;
        cx.notify();
    }

    fn toggle_comments_popup(&mut self, cx: &mut Context<Self>) {
        if self.comments_popup.take_press_was_open() {
            self.close_comments_popup(cx);
            return;
        }
        if self.comments_popup.is_open() {
            self.close_comments_popup(cx);
        } else {
            self.comments_popup.open(());
            self.comment_edit = None;
            cx.notify();
        }
    }

    /// The comments inspector (upward popover): one row per pending comment —
    /// quote preview + comment text + edit/remove. Editing swaps the row's
    /// comment for an inline input with Save/Cancel. Right edge aligned with
    /// the status-strip trigger (mirrors the Subagents inspector).
    fn render_comments_inspector(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let rows = self.comments.clone();
        let mut list = div()
            .id("comments-inspector-list")
            .flex()
            .flex_col()
            .min_w(px(280.0))
            .max_w(px(380.0))
            .py(px(4.0));
        for (ix, comment) in rows.iter().enumerate() {
            let editing = self.comment_edit.as_ref().is_some_and(|e| e.index == ix);
            let row = if editing {
                self.render_comment_edit_row(ix, comment, cx)
            } else {
                self.render_comment_row(ix, comment, &theme, cx)
            };
            list = list.child(
                div()
                    .px(px(8.0))
                    .py(px(6.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(row),
            );
        }
        let closing = self.comments_popup.closing_since();
        crate::kit::popover::anchored_menu_above_end(
            "comments-inspector",
            div()
                .w(px(360.0))
                .child(
                    div()
                        .px(px(12.0))
                        .py(px(8.0))
                        .text_size(px(11.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(if self.comments.len() == 1 {
                            "1 comment pending".to_string()
                        } else {
                            format!("{} comments pending", self.comments.len())
                        })),
                )
                .child(list)
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_comments_popup(cx)))
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.cancel_comment_edit(cx);
                        this.close_comments_popup(cx);
                        cx.stop_propagation();
                    }
                }))
                .into_any_element(),
            closing,
        )
    }

    fn render_comment_row(
        &self,
        index: usize,
        comment: &DraftComment,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .line_height(px(15.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(comment_quote_preview(&comment.quote))),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.5))
                            .line_height(px(17.0))
                            .text_color(theme.text)
                            .child(SharedString::from(comment.comment.clone())),
                    )
                    .child(
                        div()
                            .id(("comment-edit", index))
                            .flex_none()
                            .size(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(crate::kit::theme::ink(0.08)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.begin_comment_edit(index, window, cx)
                            }))
                            .child(
                                crate::kit::icons::icon(crate::kit::icons::PEN)
                                    .size(px(12.0))
                                    .text_color(theme.text_faint),
                            ),
                    )
                    .child(
                        div()
                            .id(("comment-remove", index))
                            .flex_none()
                            .size(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(crate::kit::theme::ink(0.08)))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_comment(index, cx)),
                            )
                            .child(
                                crate::kit::icons::icon(crate::kit::icons::TRASH_BIN_MINIMALISTIC)
                                    .size(px(12.0))
                                    .text_color(theme.text_faint),
                            ),
                    ),
            )
    }

    fn render_comment_edit_row(
        &mut self,
        _index: usize,
        _comment: &DraftComment,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let theme = Theme::of(cx).clone();
        let Some(edit) = &self.comment_edit else {
            return div();
        };
        let has_text = !edit.input.read(cx).text().trim().is_empty();
        let save = div()
            .id("comment-edit-save")
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(6.0))
            .bg(theme.text)
            .text_size(px(10.5))
            .text_color(if has_text {
                theme.on_solid
            } else {
                theme.text_faint
            })
            .cursor_pointer()
            .child(SharedString::from("Save"))
            .when(has_text, |el| {
                el.on_click(cx.listener(|this, _, _, cx| this.save_comment_edit(cx)))
            });
        div().flex().flex_col().gap(px(6.0)).children(vec![
            edit.input.clone().into_any_element(),
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(6.0))
                .child(
                    div()
                        .id("comment-edit-cancel")
                        .px(px(8.0))
                        .py(px(3.0))
                        .rounded(px(6.0))
                        .text_size(px(10.5))
                        .text_color(theme.text_muted)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_comment_edit(cx)))
                        .child(SharedString::from("Cancel")),
                )
                .child(save)
                .into_any_element(),
        ])
    }

    /// The status-strip trigger: a compact Subagents-style indicator (`1
    /// comment` / `N comments`, speech icon, frosted backing and faint hover
    /// wash) that opens the upward inspector. Empty with no comments.
    pub fn render_comments_trigger(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.comments.is_empty() {
            return gpui::Empty.into_any_element();
        }
        let theme = Theme::of(cx).clone();
        let label = if self.comments.len() == 1 {
            "1 comment".to_string()
        } else {
            format!("{} comments", self.comments.len())
        };
        let open = self.comments_popup.get().is_some();
        let backing = theme.composer_accessory_bg();
        let comments_fade = format!("comments-trigger-{}", cx.entity_id());
        let mut trigger = div()
            .id("comments-trigger")
            .h(px(22.0))
            .min_w_0()
            .px(px(8.0))
            .rounded_full()
            .flex()
            .items_center()
            .gap(px(5.0))
            .cursor_pointer()
            .bg(crate::kit::motion::hover_blend(
                &comments_fade,
                backing,
                backing.blend(crate::kit::theme::ink(0.06)),
            ))
            .on_hover(crate::kit::motion::hover_listener(comments_fade.clone()))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _cx| this.comments_popup.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_comments_popup(cx);
            }))
            .child(
                crate::kit::icons::icon(crate::kit::icons::CHAT_ROUND_LINE)
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(label)),
            );
        if open {
            trigger = trigger.child(self.render_comments_inspector(cx));
        }
        crate::kit::frost::composer_accessory(trigger).into_any_element()
    }

    /// The staged-attachment strip (attachment-ui.tsx AttachmentStrip): image
    /// thumbs in a `flex flex-wrap gap-2 px-4 pt-3` row (56px, click opens the
    /// full-size preview), then non-image files as stacked bars (icon + name).
    /// Every item reveals a remove button on hover. Layout must match
    /// [`attachment_strip_height`].
    pub(super) fn render_attachment_strip(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let staged = self.staged();
        if staged.is_empty() {
            return None;
        }
        let mut thumbs = div().flex().flex_row().flex_wrap().gap(px(STRIP_GAP));
        let mut bars = div()
            .flex()
            .flex_col()
            .items_start()
            .gap(px(attachments::FILE_BAR_GAP));
        let (mut has_thumbs, mut has_bars) = (false, false);
        for (ix, att) in staged.iter().enumerate() {
            let group: SharedString = format!("composer-att-{}", att.id).into();
            let remove_id = att.id.clone();
            let body = match att.image() {
                Some(image) => {
                    let preview = attachments::PreviewImage {
                        name: att.name.clone().into(),
                        image: image.clone(),
                    };
                    div()
                        .id(("composer-att-thumb", ix))
                        .size(px(STRIP_THUMB))
                        .rounded(px(8.0))
                        .overflow_hidden()
                        .border_1()
                        .border_color(crate::kit::theme::hairline(0.10))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.preview = Some(preview.clone());
                            this.preview_focus_pending = true;
                            cx.notify();
                        }))
                        .child(
                            img(image.clone())
                                .size_full()
                                // Own radii — the frame's rounding only
                                // clips rectangularly (7 = 8 - border).
                                .rounded(px(7.0))
                                .object_fit(ObjectFit::Cover),
                        )
                        .into_any_element()
                }
                None => attachments::file_bar(&att.name, theme).into_any_element(),
            };
            let item = div()
                .group(group.clone())
                .relative()
                .max_w_full()
                .child(body)
                // Own layer: inside the frosted pill everything shares one
                // draw order and images render last, so without it the
                // thumbnail paints OVER this button (user report).
                .child(crate::kit::frost::layered(
                    div()
                        .id(("composer-att-remove", ix))
                        .absolute()
                        .top(px(-6.0))
                        .right(px(-6.0))
                        .size(px(18.0))
                        .rounded_full()
                        .bg(theme.bg)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .shadow_sm()
                        .opacity(0.0)
                        .group_hover(group, |s| s.opacity(1.0))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // The button overhangs the thumbnail, whose
                            // hitbox is right underneath — don't let the
                            // same click also open the preview.
                            cx.stop_propagation();
                            this.remove_attachment(&remove_id, cx);
                        }))
                        .child(
                            crate::kit::icons::icon(crate::kit::icons::CLOSE_CIRCLE)
                                .size(px(14.0))
                                .text_color(theme.text_muted),
                        ),
                ));
            if att.image().is_some() {
                thumbs = thumbs.child(item);
                has_thumbs = true;
            } else {
                bars = bars.child(item);
                has_bars = true;
            }
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(STRIP_GAP))
                .px(px(STRIP_PAD_X))
                .pt(px(STRIP_PAD_TOP))
                .when(has_thumbs, |strip| strip.child(thumbs))
                .when(has_bars, |strip| strip.child(bars)),
        )
    }

    /// Attach files (the `/` menu): the native file picker (any file type —
    /// images preview, everything else stages as a file tile).
    pub(super) fn open_file_picker(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        self.picker_task = Some(cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                this.update(cx, |composer, cx| composer.add_paths(paths, cx))
                    .ok();
            }
        }));
    }

    pub(super) fn sync_mention_controls(&mut self, cx: &mut Context<Self>) {
        let open = self.mention.token.is_some()
            || self.slash.token.is_some()
            || self.issue.token.is_some();
        let has_selection = if self.slash.token.is_some() {
            self.slash.active.is_some()
        } else if self.issue.token.is_some() {
            self.issue.active.is_some()
        } else {
            self.mention.active.is_some()
        };
        self.input.update(cx, |input, cx| {
            input.set_mention_controls(open, has_selection, cx)
        });
    }

    /// Tear down the entire completion lifecycle. Advancing the generation is
    /// important even when the spawned task is dropped: an RPC response may
    /// already be queued for delivery on the UI executor.
    pub(super) fn reset_mention(
        &mut self,
        dismissed: Option<(Range<usize>, String)>,
        cx: &mut Context<Self>,
    ) {
        let request = self.mention.request.wrapping_add(1);
        self.mention_task = None;
        self.mention = MentionState {
            request,
            dismissed,
            ..MentionState::default()
        };
        self.sync_mention_controls(cx);
    }

    pub(super) fn on_input_edited(&mut self, cx: &mut Context<Self>) {
        if self.wizard.is_some() {
            if self.mention.token.is_some() || self.mention_task.is_some() {
                self.reset_mention(None, cx);
            }
            if self.slash.token.is_some() || self.slash_task.is_some() {
                self.reset_slash(None, cx);
            }
            if self.issue.token.is_some() || self.issue_task.is_some() {
                self.reset_issue(None, cx);
            }
            return;
        }
        let (text, cursor) = {
            let input = self.input.read(cx);
            (input.text().to_string(), input.cursor_offset())
        };
        self.update_slash(&text, cursor, cx);
        self.update_issue(&text, cursor, cx);
        let token = mention_token(&text, cursor);
        let still_dismissed = token.as_ref().is_some_and(|token| {
            self.mention
                .dismissed
                .as_ref()
                .is_some_and(|(range, value)| {
                    token.range == *range && text.get(range.clone()) == Some(value.as_str())
                })
        });
        if still_dismissed {
            self.mention.token = None;
            self.mention_task = None;
            self.sync_mention_controls(cx);
            cx.notify();
            return;
        }
        self.mention.dismissed = None;
        if token == self.mention.token {
            self.sync_mention_controls(cx);
            cx.notify();
            return;
        }
        self.mention.request = self.mention.request.wrapping_add(1);
        self.mention_task = None;
        // Refining an open menu keeps the stale rows visible until the new
        // response lands — clearing here made the popup bounce through the
        // skeleton (and a different height) on every keystroke.
        let refining = self.mention.token.is_some() && token.is_some();
        self.mention.token = token.clone();
        if !refining {
            self.mention.files.clear();
            self.mention.active = None;
            self.mention_scroll.set_offset(Point::default());
        }
        self.mention.error = None;
        self.mention.loading = token.is_some();
        // Session candidates are local and deterministic — recomputed
        // synchronously on every edit (main transport only; temporary Side
        // Chats never offer sessions). Candidates span every project and
        // device; the CURRENT chat's project and host device rank first. The
        // new-chat canvas has no current chat, so the project and device the
        // new chat is minted into govern there.
        self.mention.sessions = if let Some(token) = token.as_ref()
            && matches!(self.transport, ComposerTransport::Main)
        {
            let state = self.state.read(cx);
            let (project, device) = match state.selected_chat_row() {
                Some(chat) => (chat.space_id.clone(), Some(chat.device_id.clone())),
                None => (
                    state.selected_space_row().map(|space| space.id.clone()),
                    state.effective_device_id(),
                ),
            };
            session_candidates(
                &state.chats,
                &token.query,
                state.selected_chat.as_deref(),
                project.as_deref(),
                device.as_deref(),
            )
        } else {
            Vec::new()
        };
        // Seed the first row the moment any candidate exists (session rows
        // are local and must not wait on the file RPC or the engine), and
        // clear an index the candidate list has outgrown. This runs BEFORE
        // the no-engine / no-target early returns so session-only keyboard
        // selection works even when the file search never starts.
        self.mention.active = reconcile_mention_active(self.mention.active, self.mention.count());
        self.sync_mention_controls(cx);
        let Some(token) = token else {
            cx.notify();
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.mention.loading = false;
            cx.notify();
            return;
        };
        let selected_worktree = match self.pickers.read(cx).checkout_plan(cx) {
            crate::pickers::CheckoutPlan::ReuseWorktree { path, .. } => Some(path),
            _ => None,
        };
        let (params, target) = {
            let state = self.state.read(cx);
            let mut params = serde_json::Map::new();
            params.insert("query".into(), token.query.clone().into());
            let target = if let Some(chat) = state.selected_chat_row() {
                params.insert("chatId".into(), chat.id.clone().into());
                Some(chat.device_id.clone())
            } else if let Some(space) = state.selected_space_row() {
                params.insert("spaceId".into(), space.id.clone().into());
                if let Some(path) = selected_worktree {
                    params.insert("path".into(), path.into());
                }
                Some(space.device_id.clone())
            } else {
                None
            };
            if let Some(target) = &target {
                params.insert("targetDeviceId".into(), target.clone().into());
            }
            (serde_json::Value::Object(params), target)
        };
        if target.is_none() {
            self.mention.loading = false;
            cx.notify();
            return;
        }
        let request = self.mention.request;
        self.mention_task = Some(cx.spawn(async move |this, cx| {
            // A short debounce prevents one full workspace walk per keystroke
            // during normal typing. The generation check below still guards
            // requests that were already in flight when the query changed.
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let mut result = engine
                .client()
                .call(methods::SEARCH_FILES, params.clone())
                .await;
            if matches!(result, Err(RpcError::Transport(_)) | Err(RpcError::Closed)) {
                // One retry rides out a cold relay dial to the host device
                // (the diffs pane retries forever; a keystroke-scoped search
                // gets a single second chance).
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                result = engine.client().call(methods::SEARCH_FILES, params).await;
            }
            this.update(cx, |composer, cx| {
                if !mention_response_is_current(&composer.mention, request) {
                    return;
                }
                composer.mention.loading = false;
                match result {
                    Ok(value) => match serde_json::from_value::<Vec<FileSearchMatch>>(value) {
                        Ok(results) => {
                            composer.mention.error = None;
                            composer.mention.files = results;
                            // Reconcile, never reset: an arrow selection made
                            // while SearchFiles was in flight survives the
                            // response; an index the grown list still fits
                            // stays put.
                            composer.mention.active = reconcile_mention_active(
                                composer.mention.active,
                                composer.mention.count(),
                            );
                        }
                        Err(err) => tracing::warn!(%err, "file mention response decode failed"),
                    },
                    Err(err) => {
                        tracing::warn!(%err, "file mention search failed");
                        composer.mention.files.clear();
                        composer.mention.active = reconcile_mention_active(
                            composer.mention.active,
                            composer.mention.count(),
                        );
                        composer.mention.error = Some(mention_error_message(&err));
                    }
                }
                composer.sync_mention_controls(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn move_mention(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.mention.active =
            crate::kit::popover::menu_step(self.mention.active, self.mention.count(), delta);
        // Keep the keyboard-highlighted row in view, including the wrap from
        // the last row back to the first.
        if let Some(active) = self.mention.active {
            self.mention_scroll
                .scroll_to_item(mention_scroll_child(active, self.mention.sessions.len()));
        }
        self.sync_mention_controls(cx);
        cx.notify();
    }

    pub(super) fn dismiss_mention(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.mention.token.as_ref().and_then(|token| {
            self.input
                .read(cx)
                .text()
                .get(token.range.clone())
                .map(|text| (token.range.clone(), text.to_string()))
        });
        self.reset_mention(dismissed, cx);
        cx.notify();
    }

    pub(super) fn accept_mention(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.mention.token.clone() else {
            return;
        };
        let Some(active) = self.mention.active else {
            return;
        };
        let n_sessions = self.mention.sessions.len();
        let candidate = if active < n_sessions {
            self.mention
                .sessions
                .get(active)
                .cloned()
                .map(MentionCandidate::Session)
        } else {
            self.mention
                .files
                .get(active - n_sessions)
                .cloned()
                .map(MentionCandidate::File)
        };
        let Some(candidate) = candidate else {
            return;
        };
        match candidate {
            MentionCandidate::File(file) => {
                self.input.update(cx, |input, cx| {
                    input.replace_mention(token.range, &file.path, file.is_dir, cx)
                });
            }
            MentionCandidate::Session(session) => {
                // Up to MAX_SESSION_REFS distinct sessions per send: a 4th
                // distinct reference shows a visible composer error and is
                // NOT inserted. Duplicates of an already-referenced session
                // still insert (they dedupe at send).
                let text = self.input.read(cx).text().to_string();
                let existing = session_ref_chat_ids(&text);
                if session_cap_reached(&existing, &session.chat_id) {
                    self.failure =
                        Some("Up to 3 session references per message — remove one first.".into());
                    cx.notify();
                    return;
                }
                self.input.update(cx, |input, cx| {
                    input.replace_session_mention(token.range, &session.title, &session.chat_id, cx)
                });
            }
        }
        self.reset_mention(None, cx);
        cx.notify();
    }

    /// The popup subtitle for a session row: where the session lives —
    /// "Archived", its project, its device, and "offline" when that device's
    /// presence is stale (its context then comes from this device's synced
    /// copy). Best effort: missing rows just shorten it.
    fn session_row_subtitle(&self, session: &MentionSession, cx: &App) -> String {
        let state = self.state.read(cx);
        let mut parts: Vec<String> = Vec::new();
        if session.archived {
            parts.push("Archived".to_string());
        }
        if let Some(project) = session.project.as_deref().and_then(|id| {
            state
                .spaces
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.display_name().to_string())
        }) {
            parts.push(project);
        }
        if let Some(device) = state.device_name(&session.device_id) {
            parts.push(device.to_string());
        }
        if !state.device_online(&session.device_id, chrono::Utc::now()) {
            parts.push("offline".to_string());
        }
        parts.join(" · ")
    }

    fn render_mention_popup(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let token = self.mention.token.as_ref()?;
        let sessions = &self.mention.sessions;
        let files = &self.mention.files;
        let n_sessions = sessions.len();
        let n_files = files.len();
        let mut card = crate::kit::popover::popover_card(theme)
            .w(px(380.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_mention(cx)));
        if self.mention.loading && n_files == 0 && n_sessions == 0 {
            card = card.child(crate::kit::popover::skeleton_rows(
                "file-mention-loading",
                theme,
                3,
                cx.entity_id(),
                cx,
            ));
        } else if n_sessions == 0 && n_files == 0 {
            // Nothing at all: the file-search error (if any) or the empty
            // state. Session rows, when present, are never hidden by a file
            // search failure — the error becomes a note under them instead.
            if let Some(error) = self.mention.error.clone() {
                card = card.child(
                    div()
                        .px(px(12.0))
                        .py(px(10.0))
                        .text_size(px(12.0))
                        .text_color(theme.danger_muted)
                        .child(error),
                );
            } else {
                card = card.child(
                    div()
                        .px(px(12.0))
                        .py(px(10.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(if token.query.is_empty() {
                            "No files available"
                        } else {
                            "No matching files"
                        }),
                );
            }
        } else {
            // Sessions first, then files, under ONE keyboard active index.
            // The scroll container owns the height cap; headers and rows are
            // its direct children so `scroll_to_item` can follow the keyboard
            // (see [`mention_scroll_child`]).
            let mut list = div()
                .id("mention-menu-scroll")
                .max_h(px(312.0))
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .track_scroll(&self.mention_scroll);
            if n_sessions > 0 {
                list = list.child(
                    div()
                        .px(px(10.0))
                        .pt(px(6.0))
                        .pb(px(2.0))
                        .text_size(px(10.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from("Sessions")),
                );
                for (ix, session) in sessions.iter().enumerate() {
                    let selected = self.mention.active == Some(ix);
                    let subtitle = self.session_row_subtitle(session, cx);
                    let tooltip_title: SharedString = session.title.clone().into();
                    let tooltip_range = token.range.clone();
                    list = list.child(
                        crate::kit::popover::menu_row(
                            theme,
                            selected,
                            format!("session-mention-result-{ix}"),
                        )
                        .id(("session-mention-result", ix))
                        .tooltip(move |_, cx| {
                            cx.new(|_| MentionPathTooltip {
                                target: MentionTooltipTarget::Session {
                                    range: tooltip_range.clone(),
                                    title: tooltip_title.clone(),
                                },
                                activation: ix as u64,
                            })
                            .into()
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mention.active = Some(ix);
                            this.accept_mention(cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .flex_1()
                                .min_w_0()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    crate::kit::icons::icon(crate::kit::icons::CHAT_ROUND_LINE)
                                        .size(px(14.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .overflow_hidden()
                                        .truncate()
                                        .text_size(px(12.5))
                                        .text_color(theme.text)
                                        .child(SharedString::from(session.title.clone())),
                                )
                                .when(!subtitle.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .flex_none()
                                            .max_w(px(190.0))
                                            .overflow_hidden()
                                            .truncate()
                                            .text_size(px(11.0))
                                            .text_color(theme.text_faint)
                                            .child(SharedString::from(subtitle)),
                                    )
                                }),
                        ),
                    );
                }
            }
            if n_files > 0
                || (n_sessions > 0 && (self.mention.loading || self.mention.error.is_some()))
            {
                if n_sessions > 0 {
                    list = list.child(
                        div()
                            .px(px(10.0))
                            .pt(px(6.0))
                            .pb(px(2.0))
                            .text_size(px(10.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from("Files")),
                    );
                }
                if n_files > 0 {
                    for (ix, result) in files.iter().enumerate() {
                        let selected = self.mention.active == Some(n_sessions + ix);
                        let path = result.path.clone();
                        let tooltip_path: SharedString = path.clone().into();
                        let tooltip_range = token.range.clone();
                        list = list.child(
                            crate::kit::popover::menu_row(
                                theme,
                                selected,
                                format!("file-mention-result-{ix}"),
                            )
                            .id(("file-mention-result", ix))
                            .tooltip(move |_, cx| {
                                cx.new(|_| MentionPathTooltip {
                                    target: MentionTooltipTarget::File {
                                        range: tooltip_range.clone(),
                                        path: tooltip_path.clone(),
                                    },
                                    activation: ix as u64,
                                })
                                .into()
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mention.active = Some(n_sessions + ix);
                                this.accept_mention(cx);
                            }))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .flex_1()
                                    .min_w_0()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        crate::kit::icons::icon(if result.is_dir {
                                            crate::kit::icons::FOLDER
                                        } else {
                                            crate::kit::icons::DOCUMENT
                                        })
                                        .size(px(14.0))
                                        .text_color(theme.text_muted),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .overflow_hidden()
                                            .truncate()
                                            .text_size(px(12.5))
                                            .text_color(theme.text)
                                            .child(path),
                                    ),
                            ),
                        );
                    }
                } else if let Some(error) = self.mention.error.clone() {
                    // Sessions stay visible; the failed file search is a note
                    // under the Files header instead of hiding them.
                    list = list.child(
                        div()
                            .px(px(12.0))
                            .py(px(8.0))
                            .text_size(px(11.5))
                            .text_color(theme.danger_muted)
                            .child(error),
                    );
                } else {
                    list = list.child(crate::kit::popover::skeleton_rows(
                        "file-mention-loading",
                        theme,
                        2,
                        cx.entity_id(),
                        cx,
                    ));
                }
            }
            card = card.child(list);
        }
        let anchor = self
            .input
            .read(cx)
            .visible_point_for_index(token.range.start)?;
        // No exit phase: the completion popup tracks the token under the
        // caret — a fade-out on every keystroke-driven dismissal would read
        // as input lag, not polish.
        Some(crate::kit::popover::anchored_menu_above_at(
            "file-mention-popup",
            anchor,
            card.into_any_element(),
            None,
        ))
    }

    /// Where this composer's `#` lookups run, or `None` when there is no
    /// project to take issues from (project-less chats, Side Chats).
    fn issue_scope(&self, cx: &App) -> Option<IssueScope> {
        if !matches!(self.transport, ComposerTransport::Main) {
            return None;
        }
        let state = self.state.read(cx);
        let mut params = serde_json::Map::new();
        if let Some(chat) = state.selected_chat_row() {
            let space = chat.space_id.clone()?;
            params.insert("chatId".into(), chat.id.clone().into());
            return Some(IssueScope {
                params,
                target: chat.device_id.clone(),
                space,
            });
        }
        if state.selected_chat.is_some() {
            return None;
        }
        let space = state.selected_space_row()?;
        params.insert("spaceId".into(), space.id.clone().into());
        if let crate::pickers::CheckoutPlan::ReuseWorktree { path, .. } =
            self.pickers.read(cx).checkout_plan(cx)
        {
            params.insert("path".into(), path.into());
        }
        Some(IssueScope {
            params,
            target: space.device_id.clone(),
            space: space.id.clone(),
        })
    }

    /// How popup notices name the project's device.
    fn issue_device_label(&self, target: &str, cx: &App) -> String {
        let state = self.state.read(cx);
        if state.local_device_id.as_deref() == Some(target) {
            "this device".to_string()
        } else {
            state
                .device_name(target)
                .map(|name| format!("“{name}”"))
                .unwrap_or_else(|| "the project's device".to_string())
        }
    }

    fn update_issue(&mut self, text: &str, cursor: usize, cx: &mut Context<Self>) {
        let token = issue_token(text, cursor);
        let still_dismissed = token.as_ref().is_some_and(|token| {
            self.issue.dismissed.as_ref().is_some_and(|(range, value)| {
                token.range == *range && text.get(range.clone()) == Some(value.as_str())
            })
        });
        if still_dismissed {
            self.issue.token = None;
            self.issue_task = None;
            self.sync_mention_controls(cx);
            return;
        }
        self.issue.dismissed = None;
        if token == self.issue.token {
            return;
        }
        let scope = self.issue_scope(cx);
        let (Some(token), Some(scope)) = (token, scope) else {
            if self.issue.token.is_some() || self.issue_task.is_some() {
                self.reset_issue(None, cx);
                cx.notify();
            }
            return;
        };
        if self.no_github_spaces.contains(&scope.space) {
            return;
        }
        // Refining keeps the previous rows up until the new response lands
        // (the mention popup's anti-bounce rule).
        let refining = self.issue.token.is_some();
        self.issue.request = self.issue.request.wrapping_add(1);
        self.issue.token = Some(token.clone());
        if !refining {
            self.issue.clear_rows();
            self.issue.repo = None;
            self.issue_scroll.set_offset(Point::default());
        }
        self.issue.notice = None;
        self.issue.action = None;
        self.issue.loading = true;
        self.sync_mention_controls(cx);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.issue.loading = false;
            self.issue.notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let device = self.issue_device_label(&scope.target, cx);
        let target_device = scope.target.clone();
        let mut params = scope.params;
        params.insert("query".into(), token.query.clone().into());
        params.insert("targetDeviceId".into(), scope.target.into());
        let params = serde_json::Value::Object(params);
        let space = scope.space;
        let request = self.issue.request;
        self.issue_task = Some(cx.spawn(async move |this, cx| {
            // Each keystroke would otherwise be a GitHub API round trip.
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let result = engine
                .client()
                .call(methods::SEARCH_GITHUB_ISSUES, params)
                .await;
            this.update(cx, |composer, cx| {
                if composer.issue.request != request || composer.issue.token.is_none() {
                    return;
                }
                composer.issue.loading = false;
                match result.map(serde_json::from_value::<cypher_proto::GithubIssueSearch>) {
                    Ok(Ok(cypher_proto::GithubIssueSearch::Ok {
                        repo,
                        issues,
                        pull_requests,
                    })) => {
                        composer.issue.repo = Some(repo);
                        composer.issue.issues = issues;
                        composer.issue.pull_requests = pull_requests;
                        composer.issue.active = reconcile_mention_active(
                            composer.issue.active,
                            composer.issue.row_count(),
                        );
                    }
                    Ok(Ok(cypher_proto::GithubIssueSearch::Unavailable {
                        reason,
                        repo,
                        install_url,
                    })) => {
                        use cypher_proto::GithubUnavailable;
                        if reason == GithubUnavailable::NoGithubRemote {
                            composer.no_github_spaces.insert(space);
                        }
                        composer.issue.clear_rows();
                        composer.issue.notice = Some(
                            issue_unavailable_message(reason, repo.as_deref(), &device).into(),
                        );
                        composer.issue.action = match (reason, install_url) {
                            (GithubUnavailable::SignedOut, _) => Some(IssueAction::SignIn {
                                device: target_device,
                            }),
                            (GithubUnavailable::NoAccess, Some(url)) => {
                                Some(IssueAction::Install { url, repo })
                            }
                            (GithubUnavailable::NoAccess, None) => Some(IssueAction::SignIn {
                                device: target_device,
                            }),
                            (GithubUnavailable::NoGithubRemote, _) => None,
                        };
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(%err, "issue search response decode failed");
                        composer.issue.notice = Some("Issue search failed".into());
                    }
                    Err(err) => {
                        tracing::warn!(%err, "issue search failed");
                        composer.issue.clear_rows();
                        composer.issue.notice = Some(issue_error_message(&err));
                    }
                }
                composer.sync_mention_controls(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Tear down the `#` completion (mirrors [`Self::reset_mention`]).
    pub(super) fn reset_issue(
        &mut self,
        dismissed: Option<(Range<usize>, String)>,
        cx: &mut Context<Self>,
    ) {
        let request = self.issue.request.wrapping_add(1);
        self.issue_task = None;
        self.issue = IssueState {
            request,
            dismissed,
            ..IssueState::default()
        };
        self.sync_mention_controls(cx);
    }

    pub(super) fn move_issue(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.issue.active =
            crate::kit::popover::menu_step(self.issue.active, self.issue.row_count(), delta);
        if let Some(active) = self.issue.active {
            self.issue_scroll
                .scroll_to_item(self.issue.scroll_index(active));
        }
        self.sync_mention_controls(cx);
        cx.notify();
    }

    pub(super) fn dismiss_issue(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.issue.token.as_ref().and_then(|token| {
            self.input
                .read(cx)
                .text()
                .get(token.range.clone())
                .map(|text| (token.range.clone(), text.to_string()))
        });
        self.reset_issue(dismissed, cx);
        cx.notify();
    }

    pub(super) fn accept_issue(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.issue.token.clone() else {
            return;
        };
        let (Some(repo), Some((issue, pull))) = (
            self.issue.repo.clone(),
            self.issue
                .active
                .and_then(|active| self.issue.row(active))
                .map(|(row, pull)| (row.clone(), pull)),
        ) else {
            return;
        };
        let existing = issue_refs(self.input.read(cx).text());
        if existing.len() >= MAX_ISSUE_REFS
            && !existing
                .iter()
                .any(|r| r.repo == repo && r.number == issue.number)
        {
            self.failure = Some(
                "Up to 3 issue or pull request references per message — remove one first.".into(),
            );
            cx.notify();
            return;
        }
        self.input.update(cx, |input, cx| {
            input.replace_issue_mention(token.range, &repo, issue.number, &issue.title, pull, cx)
        });
        self.reset_issue(None, cx);
        cx.notify();
    }

    fn run_issue_action(&mut self, action: IssueAction, cx: &mut Context<Self>) {
        self.dismiss_issue(cx);
        match action {
            IssueAction::SignIn { device } => cx.emit(ComposerEvent::OpenGithubSettings {
                target_device: device,
            }),
            IssueAction::Install { url, .. } => cx.open_url(&url),
        }
    }

    fn render_issue_popup(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let token = self.issue.token.as_ref()?;
        let mut card = crate::kit::popover::popover_card(theme)
            .w(px(420.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_issue(cx)));
        let note = |text: SharedString, color: gpui::Hsla| {
            div()
                .px(px(12.0))
                .py(px(10.0))
                .text_size(px(12.0))
                .text_color(color)
                .child(text)
        };
        if self.issue.row_count() == 0 {
            card = if self.issue.loading {
                card.child(crate::kit::popover::skeleton_rows(
                    "issue-mention-loading",
                    theme,
                    3,
                    cx.entity_id(),
                    cx,
                ))
            } else if let Some(notice) = self.issue.notice.clone() {
                let action = self.issue.action.clone().map(|action| {
                    let label = match &action {
                        IssueAction::SignIn { .. } => "Sign in to GitHub…".to_string(),
                        IssueAction::Install {
                            repo: Some(repo), ..
                        } => {
                            format!("Install Cypher on {repo}…")
                        }
                        IssueAction::Install { repo: None, .. } => {
                            "Install the Cypher GitHub App…".to_string()
                        }
                    };
                    crate::kit::popover::menu_row(theme, false, "issue-mention-action")
                        .id("issue-mention-action")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.run_issue_action(action.clone(), cx)
                        }))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child(SharedString::from(label)),
                        )
                });
                card.child(note(notice, theme.text_muted))
                    .children(action.map(|row| div().px(px(4.0)).pb(px(4.0)).child(row)))
            } else {
                card.child(note(
                    if token.query.is_empty() {
                        "No open issues or pull requests".into()
                    } else {
                        "No matching open issues or pull requests".into()
                    },
                    theme.text_muted,
                ))
            };
        } else {
            let header = |title: &str, first: bool| {
                let text = match (&self.issue.repo, first) {
                    (Some(repo), true) => format!("{title} · {repo}"),
                    _ => title.to_string(),
                };
                div()
                    .px(px(10.0))
                    .pt(px(if first { 6.0 } else { 10.0 }))
                    .pb(px(2.0))
                    .text_size(px(10.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(text))
            };
            let mut list = div()
                .id("issue-menu-scroll")
                .max_h(px(312.0))
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .track_scroll(&self.issue_scroll);
            let sections = [
                ("Issues", crate::kit::icons::ISSUE, &self.issue.issues, 0),
                (
                    "Pull requests",
                    crate::kit::icons::PULL_REQUEST,
                    &self.issue.pull_requests,
                    self.issue.issues.len(),
                ),
            ];
            let mut first = true;
            for (title, icon, rows, offset) in sections {
                if rows.is_empty() {
                    continue;
                }
                list = list.child(header(title, first));
                first = false;
                for (row_ix, issue) in rows.iter().enumerate() {
                    let ix = offset + row_ix;
                    list = list.child(self.render_issue_row(theme, ix, icon, issue, cx));
                }
            }
            card = card.child(list);
        }
        let anchor = self
            .input
            .read(cx)
            .visible_point_for_index(token.range.start)?;
        Some(crate::kit::popover::anchored_menu_above_at(
            "issue-mention-popup",
            anchor,
            card.into_any_element(),
            None,
        ))
    }

    fn render_issue_row(
        &self,
        theme: &Theme,
        ix: usize,
        icon: &'static str,
        issue: &cypher_proto::GithubIssueSummary,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.issue.active == Some(ix);
        let trailing = if issue.assigned_to_me {
            Some("Assigned to you".to_string())
        } else if !issue.state.eq_ignore_ascii_case("open") {
            Some(issue.state.to_lowercase())
        } else if issue.draft {
            Some("draft".to_string())
        } else {
            issue.labels.first().cloned()
        };
        crate::kit::popover::menu_row(theme, selected, format!("issue-mention-result-{ix}"))
            .id(("issue-mention-result", ix))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.issue.active = Some(ix);
                this.accept_issue(cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        crate::kit::icons::icon(icon)
                            .size(px(14.0))
                            .text_color(theme.text_muted),
                    )
                    .child(
                        div()
                            .flex_none()
                            .mono(theme)
                            .text_size(px(11.5))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(format!("#{}", issue.number))),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(theme.text)
                            .child(SharedString::from(issue.title.clone())),
                    )
                    .when_some(trailing, |el, trailing| {
                        el.child(
                            div()
                                .flex_none()
                                .max_w(px(140.0))
                                .overflow_hidden()
                                .truncate()
                                .text_size(px(11.0))
                                .text_color(theme.text_faint)
                                .child(SharedString::from(trailing)),
                        )
                    }),
            )
    }

    pub(super) fn render_input_with_completion(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .relative()
            .child(self.input.clone())
            .children(self.render_mention_popup(theme, cx))
            .children(self.render_issue_popup(theme, cx))
            .children(self.render_slash_popup(theme, cx))
    }
}
