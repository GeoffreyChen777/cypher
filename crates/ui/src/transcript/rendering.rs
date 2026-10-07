//! Rendering: rows, chips, bubbles and the `Render` impl.

use super::*;

impl Transcript {
    /// The working loader, INSIDE the conversation flow: appended under the
    /// last row while the run is live (moved out of the shell's status strip
    /// — user request), so it reads as part of the streaming reply and
    /// scrolls away with it. The spinner drives this entity's frames, which
    /// keeps the elapsed timer ticking through delta-quiet tool runs.
    fn render_working_trailer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat_id = self.chat_id.clone()?;
        let now = chrono::Utc::now();
        let (sending, elapsed_secs, upload_percent, throughput) = {
            let state = self.state.read(cx);
            if state.indicator_for(&chat_id, now) != crate::state::Indicator::Working {
                return None;
            }
            // Slash-command settings: don't show the "model thinking"
            // flavour spinner while waiting for a picker or after cancel.
            let slash_quiet = {
                let last_user_slash = state
                    .pending_echoes()
                    .iter()
                    .rev()
                    .chain(state.transcript.iter().rev())
                    .find(|e| e.role == MessageRole::User)
                    .is_some_and(|e| {
                        let text: String = e
                            .parts
                            .iter()
                            .filter_map(|p| match p {
                                MessagePart::Text { text, .. } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        crate::composer::slash_command_label(&text).is_some()
                    });
                let assistant_working = state.transcript.iter().rev().any(|e| {
                    e.role == MessageRole::Assistant
                        && e.parts.iter().any(|p| match p {
                            MessagePart::Text { text, .. } => !text.trim().is_empty(),
                            MessagePart::Tool { .. } => true,
                            _ => false,
                        })
                });
                last_user_slash && !assistant_working
            };
            if slash_quiet {
                return None;
            }
            // During the send→turn window the session row's `started_at`
            // still belongs to the PREVIOUS turn — a timer based on the send
            // counted the round-trip and then restarted when the turn
            // actually began (user report). Bridge it as "Sending…" with no
            // timer instead; the word + timer start with the turn.
            let turn_started = state.session_for(&chat_id).and_then(|s| s.started_at);
            let sending = sending_bridge(state.pending_send_started(&chat_id, now), turn_started);
            let elapsed = turn_started
                .map(|t| now.signed_duration_since(t).num_seconds().max(0))
                .unwrap_or(0);
            let throughput = state
                .session_for(&chat_id)
                .and_then(|s| s.throughput.as_ref())
                .and_then(|t| throughput_label(t, now));
            (
                sending,
                elapsed,
                state.upload_progress_percent(&chat_id),
                throughput,
            )
        };
        let word = if let Some(percent) = upload_percent {
            format!("Uploading {percent}%")
        } else if sending {
            "Sending".to_string()
        } else {
            flavour_word(flavour_seed(&chat_id), elapsed_secs).to_string()
        };
        let theme = Theme::of(cx).clone();
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(Theme::SPACE_SM))
                .pt(px(10.0))
                .text_size(px(11.0))
                .child(crate::loaders::gradient_spinner(
                    "working-indicator",
                    &theme,
                    2.5,
                    cx.entity_id(),
                    cx,
                ))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(format!("{word}…"))),
                )
                .when(upload_percent.is_none() && !sending, |el| {
                    let elapsed = format_elapsed(elapsed_secs);
                    let tail = match throughput {
                        Some(throughput) => format!("{elapsed} · {throughput}"),
                        None => elapsed,
                    };
                    el.child(
                        div()
                            .text_color(theme.text_faint)
                            .child(SharedString::from(tail)),
                    )
                })
                .into_any_element(),
        )
    }

    /// Session Fork (v1): the affordance gate for THIS chat, from live
    /// state. A remote Pi chat stays enabled only while its SOURCE HOST device
    /// is present (the shell relays ForkSession to that device); embedded /
    /// offline / child / non-Pi / live are inert. The LOCAL engine is still
    /// authoritative (an offline engine disables everything).
    fn fork_gate_for(&self, cx: &App) -> ForkGate {
        let chat_id = self.chat_id.as_deref();
        let state = self.state.read(cx);
        let now = chrono::Utc::now();
        let chat = chat_id.and_then(|id| state.chats.iter().find(|c| c.id == id));
        let live = chat_id.is_some_and(|id| {
            matches!(
                state.indicator_for(id, now),
                Indicator::Working | Indicator::AwaitingInput
            )
        });
        // The source chat's host must be present: `device_online` treats the
        // local device as trivially online and gives unknown devices the
        // benefit of the doubt, so only a REMOTE host's stale heartbeat
        // disables the affordance.
        let host_online = chat.is_none_or(|c| state.device_online(&c.device_id, now));
        fork_gate(
            self.embedded,
            chat,
            live,
            state.engine().is_none(),
            host_online,
        )
    }

    /// Session Fork (v1): mark a fork RPC in flight for `(chat, anchor)` —
    /// the affordance becomes a spinner + inert (double-click guard). The
    /// shell calls this when the ForkRequested event fires.
    pub fn begin_fork(&mut self, chat_id: String, anchor_message_id: String) {
        self.fork_pending.insert((chat_id, anchor_message_id));
    }

    /// Session Fork (v1): clear the in-flight marker when the ForkSession
    /// RPC settles (success or failure) so the affordance re-arms.
    pub fn end_fork(&mut self, chat_id: String, anchor_message_id: String) {
        self.fork_pending.remove(&(chat_id, anchor_message_id));
    }

    /// Session Rewind: the in-flight marker (spinner + double-click guard),
    /// begun by the shell when RewindRequested fires.
    pub fn begin_rewind(&mut self, chat_id: String, anchor_message_id: String) {
        self.rewind_pending.insert((chat_id, anchor_message_id));
    }

    /// Session Rewind: clear the in-flight marker once the RPC settles.
    pub fn end_rewind(&mut self, chat_id: String, anchor_message_id: String) {
        self.rewind_pending.remove(&(chat_id, anchor_message_id));
    }

    /// Session Rewind: the rewind gate for THIS chat + anchor. Everything
    /// [`Self::fork_gate_for`] checks, plus "this is not the newest entry"
    /// (restarting at the tail would delete nothing).
    fn rewind_gate_for(&self, is_last_entry: bool, cx: &App) -> ForkGate {
        let chat_id = self.chat_id.as_deref();
        let state = self.state.read(cx);
        let now = chrono::Utc::now();
        let chat = chat_id.and_then(|id| state.chats.iter().find(|c| c.id == id));
        let live = chat_id.is_some_and(|id| {
            matches!(
                state.indicator_for(id, now),
                Indicator::Working | Indicator::AwaitingInput
            )
        });
        let host_online = chat.is_none_or(|c| state.device_online(&c.device_id, now));
        rewind_gate(
            self.embedded,
            chat,
            live,
            state.engine().is_none(),
            host_online,
            is_last_entry,
        )
    }

    /// First click ARMS the rewind (and disarms any other armed anchor);
    /// the second click inside [`REWIND_ARM_MS`] emits the request. The arm
    /// expires on its own so a forgotten button never stays hot.
    fn arm_or_confirm_rewind(
        &mut self,
        chat_id: String,
        anchor_message_id: String,
        cx: &mut Context<Self>,
    ) {
        let key = (chat_id.clone(), anchor_message_id.clone());
        if self.rewind_armed.as_ref() == Some(&key) {
            self.rewind_armed = None;
            self.rewind_disarm = None;
            cx.emit(TranscriptEvent::RewindRequested {
                chat_id,
                anchor_message_id,
            });
            cx.notify();
            return;
        }
        self.rewind_armed = Some(key.clone());
        self.rewind_disarm = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(REWIND_ARM_MS))
                .await;
            let _ = this.update(cx, |this: &mut Transcript, cx| {
                if this.rewind_armed.as_ref() == Some(&key) {
                    this.rewind_armed = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return gpui::Empty.into_any_element();
        };
        // Open this row's find-match counter before any of its text elements
        // resolve their highlights (see [`crate::find`]). Rows build in
        // document order, which is the order the ordinals have to follow.
        crate::find::begin_row(self.scope, &row.id);
        let theme = crate::chat_style::theme(cx);
        let (wide, message_spacing, paragraph_spacing) = {
            let style = crate::chat_style::settings(cx);
            (style.wide, style.message_spacing, style.paragraph_spacing)
        };
        // The first row's gap clears the small top fade band so a
        // top-scrolled transcript rests below it. An embedded panel
        // (temporary Side Chat) has no band to clear — a compact top gap.
        let top_gap = if ix == 0 {
            if self.embedded {
                EMBEDDED_TOP_INSET_PX + 6.0
            } else {
                TOP_CHROME_PX + message_spacing + 10.0
            }
        } else {
            top_gap_for_style(
                ix.checked_sub(1).and_then(|i| self.rows.get(i)),
                &row,
                message_spacing,
                paragraph_spacing,
            )
        };
        // The last row must clear the composer/status stack the transcript
        // scrolls under PLUS the fade band above it, or the timestamp strip
        // (the row's lowest content) renders half-faded (or hidden) when the
        // transcript is pinned to the bottom.
        let bottom_pad = if ix + 1 == self.rows.len() {
            let runway = self
                .own_turn
                .as_ref()
                .filter(|anchor| {
                    self.rows
                        .iter()
                        .any(|candidate| candidate.entry_id == anchor.message_id)
                })
                .map_or(0.0, |anchor| anchor.runway);
            self.bottom_clearance + Theme::TRANSCRIPT_FADE_BAND + 8.0 + runway
        } else {
            0.0
        };
        // Live-run loader rides under the LAST row's content (above its
        // clearance pad), so it sits right beneath the working reply.
        let trailer = (ix + 1 == self.rows.len())
            .then(|| self.render_working_trailer(cx))
            .flatten();

        let inner: AnyElement = match &row.kind {
            RowKind::User {
                text,
                mentions,
                attachments,
                comments,
                pending,
                steer,
            } => {
                let attachments = attachments.clone();
                let text = text.clone();
                let mentions = mentions.clone();
                let pending = *pending;
                let steer = *steer;
                // Attachment thumbnails ride ABOVE the bubble, right-aligned
                // (chat-view.tsx RowView: UserAttachmentStrip then the text
                // HStack); image-only sends show no bubble at all.
                let mut column = div().w_full().flex().flex_col();
                if steer {
                    // iOS UserBubble: a muted "↳ Steer" caption over the
                    // bubble, inset to line up with the bubble's text.
                    column = column.child(
                        div()
                            .w_full()
                            .flex()
                            .justify_end()
                            .pr(px(12.0))
                            .pb(px(4.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.0))
                                    .text_size(px(11.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.text_muted)
                                    .when(pending, |el| el.opacity(0.65))
                                    .child(
                                        crate::icons::icon(crate::icons::STEER)
                                            .size(px(12.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child("Steer"),
                            ),
                    );
                }
                if !attachments.is_empty() {
                    column = column.child(self.render_user_attachments(&row.id, &attachments, cx));
                }
                if !comments.is_empty() {
                    column = column.child(user_comments(
                        &row.id,
                        comments,
                        wide,
                        pending,
                        &theme,
                        self.scope,
                        Some(self.selection_ui_for(&row.id, cx)),
                    ));
                }
                if !text.is_empty() {
                    if renders_as_command_chip(&text, &mentions, &attachments) {
                        // Slash-command settings: a quiet action chip, not a
                        // user bubble that reads as a prompt to the model.
                        column = column.child(
                            div().w_full().flex().justify_start().child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .when(pending, |el| el.opacity(0.65))
                                    .child(
                                        crate::icons::icon(crate::icons::COMMAND)
                                            .size(px(13.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(theme.text_muted)
                                            .child(text),
                                    ),
                            ),
                        );
                    } else {
                        // `min_w_0` is load-bearing: gpui text answers min/max-content
                        // probes with its UNWRAPPED width, so without it the bubble's
                        // automatic min-size is the full single-line width — the flex
                        // item can't shrink, `justify_end` pushes the overflow off the
                        // left edge, and long prompts render as one clipped line
                        // instead of wrapping inside the 80% column cap.
                        column = column.child(
                            div().w_full().flex().justify_end().child(
                                div()
                                    .min_w_0()
                                    .when(!wide, |el| el.max_w(px(MAX_CONTENT_WIDTH * 0.8)))
                                    .when(wide, |el| el.max_w(gpui::relative(0.8)))
                                    .when(!steer, |el| el.bg(crate::chat_style::bubble(&theme)))
                                    // A steer reads as a side note to the
                                    // live turn: fainter fill, hairline edge.
                                    .when(steer, |el| {
                                        el.bg(crate::chat_style::bubble(&theme).opacity(0.55))
                                            .border_1()
                                            .border_color(theme.border)
                                    })
                                    .rounded(px(Theme::BUBBLE_RADIUS))
                                    .px(px(16.0))
                                    .py(px(10.0))
                                    .font_family(theme.font_sans.clone())
                                    .text_size(px(theme.markdown.body_size))
                                    .line_height(px(theme.markdown.body_line_height))
                                    .text_color(theme.text)
                                    .when(pending, |el| el.opacity(0.65))
                                    .child(user_bubble_text(
                                        &row.id,
                                        text,
                                        mentions,
                                        &theme,
                                        self.scope,
                                        Some(self.selection_ui_for(&row.id, cx)),
                                    )),
                            ),
                        );
                    }
                }
                column.into_any_element()
            }
            RowKind::Markdown { tree, block_ix } => {
                self.render_markdown_block(&row.id, tree, *block_ix, false, &theme, window, cx)
            }
            RowKind::LiveMarkdown { tree, block_ix } => {
                self.render_markdown_block(&row.id, tree, *block_ix, true, &theme, window, cx)
            }
            RowKind::ThoughtBlock {
                tree,
                block_ix,
                live,
            } => {
                // Thinking reads as the answer's quieter companion: the same
                // blocks, in the muted text tone.
                let mut muted = theme.clone();
                muted.text = theme.text_muted;
                self.render_markdown_block(&row.id, tree, *block_ix, *live, &muted, window, cx)
            }
            RowKind::ToolGroup { tools, auto_open } => {
                self.render_tool_group(&row.id, tools, *auto_open, &theme, cx)
            }
            RowKind::InputChip {
                header, resolved, ..
            } => input_chip(header.clone(), *resolved, &theme),
            RowKind::ErrorChip { message } => error_chip(message.clone(), &theme),
            RowKind::Worked { label } => worked_rule(label.clone(), &theme),
            RowKind::TranslationOriginal { .. } => {
                self.render_fold_toggle(&row.id, ("Show original", "Hide original"), &theme, cx)
            }
            RowKind::Thought { live, .. } => {
                let label = if *live { "Thinking…" } else { "Thought" };
                self.render_fold_toggle(&row.id, (label, label), &theme, cx)
            }
        };

        // Hover-revealed timestamp strip (zeron chat-view.tsx `Timestamp`):
        // a RESERVED 16px lane under the entry's last row — the label only
        // flips opacity, so revealing it never shifts the virtualizer's
        // layout. User entries align end (under the bubble), assistant start.
        let is_user_row = match &row.kind {
            RowKind::User { text, .. } => crate::composer::slash_command_label(text).is_none(),
            _ => false,
        };
        let hovered = self
            .hovered_entry
            .as_ref()
            .is_some_and(|(_, entry)| entry == &row.entry_id);
        // Vertical breathing room from the source: assistant text blocks sit
        // in a `VStack padding={4}` (chat-view.tsx:183), so the strip starts
        // 4px below the message text — the native markdown column has no such
        // bottom padding, so the strip carries it as top inset (grown into the
        // reserved height: reveal still never shifts layout). User rows are
        // flush: the Timestamp follows the bubble HStack directly (VStack gap
        // defaults to 0 in mugen), the label's centering inside the 16px lane
        // is all the gap the original has.
        // Session Fork (v1): the git-branch affordance rides the timestamp
        // strip of every SETTLED entry, beside the time. Gated on the chat's
        // forkability (embedded Side Chat / offline / child / non-Pi / live
        // are inert); a remote Pi chat stays enabled — the shell relays
        // ForkSession to the source chat's host device.
        let fork_gate = self.fork_gate_for(cx);
        let fork_enabled = fork_gate == ForkGate::Enabled;
        // Role-specific tooltip; System rows never get an affordance at all
        // (`fork_button` is None below), so no fork action can be emitted.
        let fork_role = row.role;
        let fork_tip: SharedString = fork_tooltip(fork_role, &fork_gate).into();
        let fork_chat_id = self.chat_id.clone().unwrap_or_default();
        let fork_entry_id = row.entry_id.clone();
        // In-flight marker for THIS anchor: the affordance becomes a spinner
        // and is inert (double-click guard) until the shell's ForkSession
        // RPC settles.
        let fork_pending = !fork_chat_id.is_empty()
            && self
                .fork_pending
                .contains(&(fork_chat_id.clone(), fork_entry_id.to_string()));
        let fork_clickable = fork_enabled && !fork_pending;
        // The affordance is built ONCE per row (before the hover closure) as
        // an optional child: the MAIN surface shows it (spinner while a fork
        // is in flight, branch icon otherwise); an embedded Side Chat
        // transcript and SYSTEM rows never do. `None` keeps the hover branch
        // untouched.
        let fork_button: Option<AnyElement> = if self.embedded || row.role == MessageRole::System {
            None
        } else {
            let fork_chat_id = fork_chat_id.clone();
            let fork_entry_id = fork_entry_id.clone();
            Some(
                div()
                    .id((row.id.clone(), 0usize))
                    .size(px(12.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if fork_pending {
                        crate::loaders::gradient_spinner(
                            "fork-spinner",
                            &theme,
                            2.5,
                            cx.entity_id(),
                            cx,
                        )
                        .into_any_element()
                    } else {
                        crate::icons::icon(crate::icons::GIT_BRANCH)
                            .size(px(10.0))
                            .text_color(if fork_enabled {
                                theme.text_muted
                            } else {
                                theme.text_muted.opacity(0.3)
                            })
                            .into_any_element()
                    })
                    .when(fork_clickable, |el| {
                        el.cursor_pointer()
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if fork_chat_id.is_empty() {
                                    return;
                                }
                                cx.emit(TranscriptEvent::ForkRequested {
                                    chat_id: fork_chat_id.clone(),
                                    anchor_message_id: fork_entry_id.to_string(),
                                });
                            }))
                    })
                    .tooltip(move |_, cx| {
                        cx.new(|_| MessageActionTooltip {
                            text: fork_tip.clone(),
                        })
                        .into()
                    })
                    .tooltip_show_delay(Duration::from_millis(350))
                    .into_any_element(),
            )
        };
        // Session Rewind: the restart affordance rides the same strip, right
        // after the fork one. Same prerequisites (it drives the same pi
        // machinery), plus "not the newest entry" — restarting at the tail
        // would delete nothing. Destructive, so the first click only ARMS it.
        let is_last_entry = self
            .rows
            .last()
            .is_some_and(|last| last.entry_id == row.entry_id);
        let rewind_gate = self.rewind_gate_for(is_last_entry, cx);
        let rewind_enabled = rewind_gate == ForkGate::Enabled;
        let rewind_key = (fork_chat_id.clone(), fork_entry_id.to_string());
        let rewind_armed = self.rewind_armed.as_ref() == Some(&rewind_key);
        let rewind_pending = !fork_chat_id.is_empty() && self.rewind_pending.contains(&rewind_key);
        let rewind_clickable = rewind_enabled && !rewind_pending;
        // How much the confirming click deletes — named in the armed tooltip.
        // (The anchor itself is named separately in the user-role wording.)
        let later_entries = if rewind_enabled {
            let state = self.state.read(cx);
            state
                .transcript
                .iter()
                .position(|e| e.id.as_str() == fork_entry_id.as_ref())
                .map(|ix| state.transcript.len() - ix - 1)
                .unwrap_or(0)
        } else {
            0
        };
        let rewind_tip: SharedString =
            rewind_tooltip(row.role, &rewind_gate, rewind_armed, later_entries).into();
        let rewind_button: Option<AnyElement> = if self.embedded || row.role == MessageRole::System
        {
            None
        } else {
            let rewind_chat_id = fork_chat_id.clone();
            let rewind_entry_id = fork_entry_id.clone();
            Some(
                div()
                    .id((row.id.clone(), 2usize))
                    .size(px(12.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if rewind_pending {
                        crate::loaders::gradient_spinner(
                            "rewind-spinner",
                            &theme,
                            2.5,
                            cx.entity_id(),
                            cx,
                        )
                        .into_any_element()
                    } else {
                        crate::icons::icon(crate::icons::RESTART)
                            .size(px(10.0))
                            .text_color(if rewind_armed {
                                theme.danger
                            } else if rewind_enabled {
                                theme.text_muted
                            } else {
                                theme.text_muted.opacity(0.3)
                            })
                            .into_any_element()
                    })
                    .when(rewind_clickable, |el| {
                        el.cursor_pointer()
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if rewind_chat_id.is_empty() {
                                    return;
                                }
                                this.arm_or_confirm_rewind(
                                    rewind_chat_id.clone(),
                                    rewind_entry_id.to_string(),
                                    cx,
                                );
                            }))
                    })
                    .tooltip(move |_, cx| {
                        cx.new(|_| MessageActionTooltip {
                            text: rewind_tip.clone(),
                        })
                        .into()
                    })
                    .tooltip_show_delay(Duration::from_millis(350))
                    .into_any_element(),
            )
        };
        // Copy belongs to every message strip, including System/Side Chat and
        // offline/non-Pi chats. Inspect availability only for the hovered last
        // row; resolve and assemble the complete entry lazily on click.
        let copy_button = (hovered && row.timestamp.is_some()).then(|| {
            let can_copy = match &row.kind {
                RowKind::User { text, .. } => !text.trim().is_empty(),
                _ => {
                    let state = self.state.read(cx);
                    message_for_copy(&state.transcript, state.pending_echoes(), &row.entry_id)
                        .is_some_and(|entry| {
                            entry.parts.iter().any(|part| match part {
                                MessagePart::Text { text, .. } => !text.trim().is_empty(),
                                MessagePart::Error { message, .. } => !message.trim().is_empty(),
                                _ => false,
                            })
                        })
                }
            };
            let copied = self.copied_message.as_ref() == Some(&row.entry_id);
            let chat_id = self.chat_id.clone();
            let entry_id = row.entry_id.clone();
            let tip: SharedString = if copied {
                "Copied!"
            } else if can_copy {
                "Copy message"
            } else {
                "No text to copy"
            }
            .into();
            div()
                .id((row.id.clone(), 1usize))
                .size(px(12.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(message_copy_icon(copied, can_copy, &theme))
                .when(can_copy, |el| {
                    el.cursor_pointer()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.copy_message(&chat_id, &entry_id, cx);
                        }))
                })
                .tooltip(move |_, cx| {
                    cx.new(|_| MessageActionTooltip { text: tip.clone() })
                        .into()
                })
                .tooltip_show_delay(Duration::from_millis(350))
                .into_any_element()
        });
        let strip = row.timestamp.map(|ms| {
            div()
                .h(px(if is_user_row { 16.0 } else { 20.0 }))
                .when(!is_user_row, |el| el.pt(px(4.0)))
                .w_full()
                .flex()
                .items_center()
                // No horizontal inset: the original's `px-1` netted out flush
                // because its message text was inset by the same amount (group
                // padding 4 + inner VStack 4 = 8 = group 4 + px-1 4). Here the
                // markdown text / user bubble sit AT the content column edges,
                // so the label must too — assistant label's left edge on the
                // text's first-character x, user label's right edge on the
                // bubble's right edge (user-reported 4px drift).
                .when(is_user_row, |el| el.justify_end())
                .when(hovered, |el| {
                    el.child(motion::fade_quick(
                        SharedString::from(format!("ts-{}", row.id)),
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .text_size(px(11.0))
                            .text_color(theme.text_muted.opacity(0.55))
                            .child(SharedString::from(format_timestamp(ms, &chrono::Local)))
                            .when_some(fork_button, |el, button| el.child(button))
                            .when_some(rewind_button, |el, button| el.child(button))
                            .when_some(copy_button, |el, button| el.child(button)),
                    ))
                })
        });
        let entry_id = row.entry_id.clone();
        let row_id = row.id.clone();
        div()
            .id(row.id.clone())
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    let next = Some((row_id.clone(), entry_id.clone()));
                    if this.hovered_entry != next {
                        let entry_changed = this
                            .hovered_entry
                            .as_ref()
                            .is_none_or(|(_, entry)| entry != &entry_id);
                        this.hovered_entry = next;
                        if entry_changed {
                            cx.notify();
                        }
                    }
                } else if this
                    .hovered_entry
                    .as_ref()
                    .is_some_and(|(row, _)| row == &row_id)
                {
                    // Only the row that OWNS the current reveal may clear it —
                    // a stale leave from an earlier row must not blank the
                    // strip the newly entered row just lit.
                    this.hovered_entry = None;
                    cx.notify();
                }
            }))
            .w_full()
            .flex()
            .justify_center()
            .pt(px(top_gap))
            .pb(px(bottom_pad))
            // Wide gutters (zeron `px-4 @3xl:px-12`) around the 46rem column.
            // An embedded panel is already narrow — compact gutters.
            .px(px(if self.embedded { 14.0 } else { 48.0 }))
            .child(
                div()
                    .w_full()
                    .when(!wide, |el| el.max_w(px(MAX_CONTENT_WIDTH)))
                    .min_w_0()
                    .child(inner)
                    .children(strip)
                    .children(trailer),
            )
            .into_any_element()
    }

    fn copy_message(
        &mut self,
        chat_id: &Option<String>,
        entry_id: &SharedString,
        cx: &mut Context<Self>,
    ) {
        let text = {
            let state = self.state.read(cx);
            // Reject an old row's click after navigation, even when a fork
            // contains entries whose ids also occur in the previous chat.
            if &self.chat_id != chat_id || &state.selected_chat != chat_id {
                return;
            }
            message_for_copy(&state.transcript, state.pending_echoes(), entry_id)
                .and_then(message_copy_text)
        };
        let Some(text) = text else {
            // Empty/tool-only entries must not erase the user's clipboard.
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied_message = Some(entry_id.clone());
        self.copied_message_clear = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            this.update(cx, |this, cx| {
                this.copied_message = None;
                this.copied_message_clear = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Copy-button wiring for one row's code blocks ([`render::CopyUi`]):
    /// click writes the block's code to the clipboard and shows a transient
    /// "Copied" check on that block for ~1.2s (overlay — no layout shift).
    pub(super) fn copy_ui_for(
        &self,
        row_id: &SharedString,
        cx: &mut Context<Self>,
    ) -> render::CopyUi {
        let copied_ix = self
            .copied_code
            .as_ref()
            .filter(|(id, _)| id == row_id)
            .map(|(_, ix)| *ix);
        let row_key = row_id.clone();
        let entity = cx.weak_entity();
        let handler: crate::markdown::render::CopyHandler =
            Rc::new(move |ix, code, _window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                let row_key = row_key.clone();
                entity
                    .update(cx, |this, cx| {
                        this.copied_code = Some((row_key, ix));
                        this.copied_clear = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(1200))
                                .await;
                            this.update(cx, |this, cx| {
                                this.copied_code = None;
                                this.copied_clear = None;
                                cx.notify();
                            })
                            .ok();
                        }));
                        cx.notify();
                    })
                    .ok();
            });
        render::CopyUi { handler, copied_ix }
    }
}

/// A sent message's text with its file-mention chips. The same recipe as the
/// markdown renderer's inline code (`flat_text_element`): chip ranges shape in
/// the mono font at `code_text` emerald, [`StyledText`] supplies wrapped glyph
/// geometry through its layout handle, and a canvas paints the rounded
/// `code_wash` *beneath* the glyphs — so chips wrap, clip, and scroll exactly
/// like the text they decorate.
///
/// Per-frame cost while an assistant message streams below: shaping hits
/// gpui's line-layout cache (identical text + runs ⇒ reuse) and the underlay
/// repaints O(chips) quads — no layout work, no re-projection (spans were
/// computed once in [`rows_for_entry`]).
/// The user bubble's text: runs split at mention-chip boundaries (one plain
/// run when there are none), with the same selection machinery as rendered
/// markdown — the element registers into the frame's document-ordered
/// registry, so drags select, span into adjacent rows, and Cmd+C copies.
fn user_bubble_text(
    row_id: &SharedString,
    text: SharedString,
    mentions: Arc<Vec<crate::composer::SentMentionSpan>>,
    theme: &Theme,
    scope: crate::markdown::selection::SelectionScope,
    selection: Option<render::SelectionUi>,
) -> AnyElement {
    // Split runs at chip boundaries (spans are in order): body text keeps the
    // sans font, chips read as inline code. Size/line-height flow from the
    // bubble's div like every text child.
    let body_run = |len: usize| TextRun {
        len,
        font: gpui::font(theme.font_sans.clone()),
        color: theme.text,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let chip_run = |len: usize| TextRun {
        len,
        font: theme.mono(),
        color: theme.code_text,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut runs = Vec::with_capacity(mentions.len() * 2 + 1);
    let mut at = 0;
    for span in mentions.iter() {
        if at < span.range.start {
            runs.push(body_run(span.range.start - at));
        }
        runs.push(chip_run(span.range.len()));
        at = span.range.end;
    }
    if at < text.len() {
        runs.push(body_run(text.len() - at));
    }
    let styled = StyledText::new(text.clone()).with_runs(runs);
    let layout = styled.layout().clone();
    let wash = theme.code_wash;
    let sel_key: std::sync::Arc<str> = format!("{row_id}:u").into();
    let sel_theme = theme.clone();
    // In-chat find, resolved at BUILD time like the markdown rows' — a user
    // bubble is one text element, so it always takes its row's first
    // ordinals.
    let find_hits = crate::find::element_matches(
        scope,
        crate::markdown::selection::row_of_key(&sel_key),
        &text,
    );
    let find_washes = render::find_wash(theme);
    let underlay = canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            for span in mentions.iter() {
                for rect in render::range_rects(&layout, &span.range, 0.0, 2.0) {
                    window.paint_quad(quad(
                        rect,
                        px(5.0),
                        wash,
                        px(0.0),
                        gpui::transparent_black(),
                        BorderStyle::default(),
                    ));
                }
            }
            render::paint_find_hits(window, &layout, &find_hits, find_washes);
            render::paint_text_selection(
                window, scope, &sel_key, &text, &layout, &sel_theme, selection,
            );
        },
    )
    .absolute()
    .size_full();
    div()
        .relative()
        .child(underlay)
        .child(styled)
        .into_any_element()
}

/// The transcript ErrorChip — a port of zeron chat-view.tsx `ErrorChip`
/// (34px-minimum row, `rounded-[10px] border border-red-400/[0.16]
/// bg-red-400/[0.05] px-2 text-[12px]`) with a 20px red-washed tile holding a
/// 12px DangerTriangle (`bg-red-400/[0.12] text-red-300/80`), a medium
/// "Error" label, then the human message at `text-foreground/80` — a subtle
/// red-tinted wash, never a bare red-stroke box. Unlike the web port, the
/// message WRAPS instead of truncating: startup-crash errors carry the
/// agent's exit status and stderr, and a one-line ellipsis was exactly what
/// made zeronsh/comet#95 undiagnosable from the screenshot.
fn error_chip(message: SharedString, theme: &Theme) -> AnyElement {
    let red_300 = theme.danger_muted; // tailwind red-300
    let danger = theme.danger; // red-400
    div()
        .py(px(4.0))
        .w_full()
        .child(
            div()
                .min_h(px(34.0))
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .overflow_hidden()
                .rounded(px(10.0))
                .border_1()
                .border_color(danger.opacity(0.16))
                .bg(danger.opacity(0.05))
                .px(px(8.0))
                .py(px(7.0))
                .text_size(px(12.0))
                .child(
                    div()
                        .flex_none()
                        .size(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                                .size(px(12.0))
                                .text_color(red_300.opacity(0.8)),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(red_300.opacity(0.8))
                        .child(SharedString::from("Error")),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_color(theme.text.opacity(0.8))
                        .child(message),
                ),
        )
        .into_any_element()
}

/// The settled turn's work rule: the "Worked for 1m 32s" label followed by a
/// hairline that runs out to the content column's edge — the quiet seam
/// between what the turn DID (tool chips, questions, errors) and the answer it
/// finished with.
fn worked_rule(label: SharedString, theme: &Theme) -> AnyElement {
    div()
        .py(px(4.0))
        .w_full()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_none()
                .text_size(px(11.0))
                // Quieter than the timestamp strip: the rule is a seam, not a
                // thing to read.
                .text_color(theme.text_faint.opacity(0.8))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h(px(1.0))
                .bg(crate::theme::hairline(0.05)),
        )
        .into_any_element()
}

/// A passive one-line chip marking a question the agent asked — the
/// interactive controls live in the composer (chat-view.tsx `InputChip`):
/// 34px row, `rounded-[10px] border-white/[0.08] bg-white/[0.045] px-2
/// text-[12px]`, a 20px `bg-white/[0.09]` icon tile with a 12px
/// ChatRoundLine, the medium "Question" label, then the truncating value —
/// the first question's header once resolved, "Awaiting your answer…" while
/// pending. Neutral tones throughout; resolution never recolors the chip.
fn input_chip(header: SharedString, resolved: bool, theme: &Theme) -> AnyElement {
    let value: SharedString = if resolved {
        header
    } else {
        "Awaiting your answer…".into()
    };
    div()
        .py(px(4.0))
        .w_full()
        .child(
            div()
                .h(px(34.0))
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .overflow_hidden()
                .rounded(px(10.0))
                .border_1()
                .border_color(crate::theme::hairline(0.08))
                .bg(crate::theme::ink(0.045))
                .px(px(8.0))
                .text_size(px(12.0))
                .child(
                    div()
                        .flex_none()
                        .size(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            crate::icons::icon(crate::icons::CHAT_ROUND_LINE)
                                .size(px(12.0))
                                .text_color(theme.text_muted),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text_muted)
                        .child(SharedString::from("Question")),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_color(theme.text.opacity(0.9))
                        .child(value),
                ),
        )
        .into_any_element()
}

/// A small glyph standing in for the tool's icon (zeron uses an icon set; a
/// quiet monochrome character keeps the tile without shipping SVGs).
/// The glyph for a tool call (zeron tool-chip.tsx `toolIcon`, Solar set).
fn tool_icon_path(call: &ToolCall) -> &'static str {
    match call {
        ToolCall::Exec { .. } => crate::icons::COMMAND,
        ToolCall::ReadFile { .. } | ToolCall::ApplyPatch { .. } => crate::icons::DOCUMENT,
        ToolCall::WriteFile { .. } => crate::icons::DOCUMENT_ADD,
        ToolCall::EditFile { .. } => crate::icons::PEN,
        ToolCall::Search { .. } => crate::icons::MAGNIFER,
        ToolCall::Glob { .. } => crate::icons::FOLDER_WITH_FILES,
        ToolCall::WebFetch { .. } | ToolCall::WebSearch { .. } => crate::icons::GLOBAL,
        ToolCall::Todo { .. } => crate::icons::CHECKLIST,
        ToolCall::Unknown { name, .. } if name == cypher_proto::view::CODEMODE_TOOL => {
            crate::icons::CODE
        }
        ToolCall::Unknown { name, .. } if name == cypher_proto::view::TOOL_SEARCH_TOOL => {
            crate::icons::MAGNIFER
        }
        ToolCall::Mcp { .. } | ToolCall::Unknown { .. } => crate::icons::WIDGET,
    }
}

/// Left inset of a nested chip's guide rail from the rail before it: the
/// rail lands under the caller chip's icon (rail 1px + card inset 12px +
/// card border 1px + header padding 8px + half the 18px icon box − the
/// rail's own pixel), so a script's calls hang off the script's glyph.
const NESTED_RAIL_INSET: f32 = 30.0;

/// The extra guide rails in front of a chip `depth` levels deep — one per
/// level, each full row height so consecutive nested chips draw one
/// continuous line. Chips keep [`CHIP_HEIGHT`], so the group's analytic
/// heights hold at any depth.
pub(super) fn nested_rails(depth: u8) -> impl Iterator<Item = gpui::Div> {
    // Past a few levels the indent stops helping and starts eating the row.
    (0..depth.min(3)).map(|_| {
        div()
            .ml(px(NESTED_RAIL_INSET))
            .w(px(1.0))
            .flex_none()
            .bg(crate::theme::ink(0.08))
    })
}

/// The body of an expanded chip card, under the header's separator. Diffs
/// render through the changes pane's section body — the real component, with
/// hunk headers, dual line-number gutters, accent bars, row washes, and
/// syntax runs — so an inline tool diff is indistinguishable from the
/// checkout diff sidebar. Output renders as a code block: verbatim mono
/// lines, indentation intact, counted-tail truncation.
pub(super) fn detail_body(
    detail: &ToolDetail,
    diff_highlights: Option<Arc<crate::changes::DiffHighlights>>,
    theme: &Theme,
) -> AnyElement {
    let body = div().w_full().min_w_0().flex().flex_col().overflow_hidden();
    match detail {
        ToolDetail::Diff { file, .. } => body
            .child(crate::changes::render_file_body_with_syntax(
                file,
                diff_highlights,
                theme,
            ))
            .into_any_element(),
        ToolDetail::Stats { stats } => body
            .py(px(6.0))
            .mono(theme)
            .text_size(px(11.5))
            .children(stats.iter().map(|stat| {
                div()
                    .h(px(OUTPUT_LINE_HEIGHT))
                    .w_full()
                    .min_w_0()
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_color(theme.text.opacity(0.85))
                            .child(SharedString::from(stat.path.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.success)
                            .child(SharedString::from(format!("+{}", stat.additions))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.danger)
                            .child(SharedString::from(format!("−{}", stat.deletions))),
                    )
            }))
            .into_any_element(),
        ToolDetail::Output {
            lines,
            truncated_by,
        } => body
            .py(px(6.0))
            .mono(theme)
            .text_size(px(11.5))
            .children(lines.iter().map(|line| {
                div()
                    .h(px(OUTPUT_LINE_HEIGHT))
                    .w_full()
                    .min_w_0()
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .text_color(theme.text.opacity(0.85))
                    .child(div().w_full().min_w_0().truncate().child(line.clone()))
            }))
            .when(*truncated_by > 0, |block| {
                block.child(
                    div()
                        .h(px(OUTPUT_LINE_HEIGHT))
                        .px(px(12.0))
                        .flex()
                        .items_center()
                        .text_size(px(10.5))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(format!("… {truncated_by} more lines"))),
                )
            })
            .into_any_element(),
    }
}

/// The chip's content row: icon + label + parameter, then the status icon
/// (+ chevron when the chip expands). Shared between the plain chip and the
/// header of an expandable chip card. `key` is unique per chip; it names the
/// running spinner's animation.
fn chip_header_row(
    tool: &ToolItem,
    chevron: Option<bool>,
    key: &SharedString,
    theme: &Theme,
) -> gpui::Div {
    let (label, parameter) = tool_row_text(tool);
    let tint = if tool.is_error {
        theme.danger
    } else {
        theme.text_muted
    };
    div()
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
            // Bare icon (size-3), centered in the tile's former 18px box so
            // the chip's text columns stay where they were.
            div()
                .size(px(18.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    crate::icons::icon(tool_icon_path(&tool.call))
                        .size(px(12.0))
                        .text_color(theme.text_muted),
                ),
        )
        .child(
            // A generic tool's name can be long: it may take up to half the
            // line before it ellipsizes, leaving the rest to the parameter.
            div()
                .flex_none()
                .max_w(gpui::relative(0.5))
                .truncate()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(tint)
                .child(SharedString::from(label)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(if tool.is_error {
                    theme.danger
                } else {
                    theme.text.opacity(0.85)
                })
                .child(SharedString::from(parameter)),
        )
        .child(tool_status_icon(
            ToolStatus::of(tool),
            SharedString::from(format!("{key}-status")),
            theme,
        ))
        .when_some(chevron, |row, open| {
            // Output/diff affordance: a bare chevron in the tile's former
            // 18px box, flipped while the detail body is open.
            row.child(
                div()
                    .size(px(18.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.0))
                    .text_color(theme.text_muted.opacity(0.8))
                    .child(SharedString::from(if open { "▾" } else { "▸" })),
            )
        })
}

/// The two text columns of a chip line, each said once: what kind of call
/// it is ("Read") and what it acted on ("README.md"). The result body
/// belongs to the expandable card and the status to its icon. A generic
/// extension tool has no kind of its own, so its name is the label.
pub(super) fn tool_row_text(tool: &ToolItem) -> (String, String) {
    let (label, parameter) = tool_chip_content(&tool.call);
    let parameter = parameter.trim().to_owned();
    if label == "Tool" {
        (parameter, String::new())
    } else {
        (label.to_owned(), parameter)
    }
}

/// Where a tool call is in its lifecycle, shown as the chip's status icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolStatus {
    Running,
    Completed,
    Failed,
}

impl ToolStatus {
    pub(super) fn of(tool: &ToolItem) -> Self {
        if tool.is_error {
            Self::Failed
        } else if !tool.resolved {
            Self::Running
        } else {
            Self::Completed
        }
    }
}

/// One spinner turn. Linear, so the arc never appears to stall.
const STATUS_SPIN_PERIOD: Duration = Duration::from_millis(900);

/// The chip's status: a check once the call completed, a cross when it
/// failed, and a rotating arc while it runs (`key` names the rotation; under
/// reduced motion gpui holds it still). Same 18px slot as the other icons.
fn tool_status_icon(status: ToolStatus, key: SharedString, theme: &Theme) -> AnyElement {
    let slot = div()
        .size(px(18.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center();
    let icon = |path| crate::icons::icon(path).size(px(12.0));
    match status {
        ToolStatus::Completed => slot.child(icon(crate::icons::CHECK).text_color(theme.success)),
        ToolStatus::Failed => slot.child(icon(crate::icons::CROSS).text_color(theme.danger)),
        ToolStatus::Running => slot.child(
            icon(crate::icons::SPINNER)
                .text_color(theme.text_muted)
                .with_animation(
                    key,
                    gpui::Animation::new(STATUS_SPIN_PERIOD).repeat(),
                    |icon, t| {
                        icon.with_transformation(gpui::Transformation::rotate(gpui::percentage(t)))
                    },
                ),
        ),
    }
    .into_any_element()
}

/// The header row of an expandable chip card.
pub(super) fn chip_header(
    tool: &ToolItem,
    open: bool,
    key: &SharedString,
    theme: &Theme,
) -> gpui::Div {
    chip_header_row(tool, Some(open), key, theme)
}

/// A plain (non-expandable) chip: guide rail + bordered card.
pub(super) fn tool_chip(tool: &ToolItem, key: &SharedString, theme: &Theme) -> AnyElement {
    div()
        .h(px(CHIP_HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        // Guide rail: hairline centered under the header's chevron tile.
        .child(
            div()
                .ml(px(12.0))
                .h_full()
                .w(px(1.0))
                .flex_none()
                .bg(crate::theme::ink(0.08)),
        )
        .children(nested_rails(tool.depth).map(|rail| rail.h_full()))
        .child(
            div()
                .ml(px(12.0))
                .h(px(CHIP_CARD_HEIGHT))
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .rounded(px(9.0))
                .border_1()
                .border_color(crate::theme::hairline(0.07))
                .bg(crate::theme::ink(0.03))
                .child(chip_header_row(tool, None, key, theme)),
        )
        .into_any_element()
}

pub(super) fn entry_fingerprint(entry: &SessionMessageEntry, pending: bool) -> u64 {
    let mut acc: Vec<u8> = Vec::with_capacity(entry.parts.len() * 8 + 16);
    acc.extend_from_slice(entry.id.as_bytes());
    acc.push(match entry.status {
        None => 0,
        Some(MessageStatus::Streaming) => 1,
        Some(MessageStatus::Complete) => 2,
        Some(MessageStatus::Aborted) => 3,
    });
    acc.push(pending as u8);
    for part in &entry.parts {
        acc.extend_from_slice(part.id().as_bytes());
        acc.extend_from_slice(&(part.byte_len() as u64).to_le_bytes());
        if let MessagePart::Tool {
            is_error, resolved, ..
        } = part
        {
            acc.push(*is_error as u8 | (*resolved as u8) << 1);
        }
        if let MessagePart::Input { resolved, .. } = part {
            acc.push(0x10 | *resolved as u8);
        }
    }
    fnv1a(&acc)
}

impl Render for Transcript {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Release gpui-side decoded copies of any images the attachment LRU
        // evicted since the last frame (no-op when nothing was evicted).
        crate::attachments::flush_evicted(Some(window), cx);
        // Own-turn driver: measurements are only authoritative after layout,
        // so reservation sizing, the send glide, and the outgrown-handoff
        // each advance at most once per requested frame. Scheduled on every
        // frame while an anchor is live (not just on kicks) so viewport
        // resizes and streaming growth re-derive the reservation; the step
        // only notifies on change, so a settled hold schedules no next frame.
        if (self.own_turn.is_some() || self.own_turn_kick) && !self.own_turn_scheduled {
            self.own_turn_scheduled = true;
            let entity = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                entity
                    .update(cx, |this: &mut Transcript, cx| {
                        this.own_turn_scheduled = false;
                        this.step_own_turn(cx);
                    })
                    .ok();
            });
        }
        // Spring driver: one on_next_frame callback at a time; each tick
        // notifies, which re-enters render and schedules the next frame until
        // the spring parks. Reduced motion never schedules (sync snaps).
        if self.pinned
            && !motion::reduced_motion(cx)
            && !self.spring_scheduled
            && self.spring_should_run()
        {
            self.spring_scheduled = true;
            let entity = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                entity
                    .update(cx, |this: &mut Transcript, cx| {
                        this.spring_scheduled = false;
                        this.step_spring(cx);
                    })
                    .ok();
            });
        }
        // Hand the find query + active match to the painter BEFORE the list
        // builds its rows (gpui requests items during layout, after this
        // returns), so every row built this frame washes against the current
        // state.
        self.publish_find();
        let rail = self.render_rail(cx);
        // The scroll-to-bottom pill is rendered by the SHELL (conversation
        // region overlay): it must float just above the composer and paint
        // OVER the bottom fade gradient, which is a later sibling of this
        // outlet — an overlay here would be tinted by the fade. The find bar
        // is shell chrome for the same reason.
        let root = div()
            .relative()
            .size_full()
            .min_h_0()
            // A click anywhere in the chat history focuses it (gpui moves
            // focus on mouse down); ⌘C still reaches the shell root's copy
            // handler, an ancestor on the key dispatch path.
            .track_focus(&self.focus)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &PrevPrompt, _, cx| this.step_prompt(false, cx)))
            .on_action(cx.listener(|this, _: &NextPrompt, _, cx| this.step_prompt(true, cx)))
            // The main/side-chat rounded card owns the background. Keeping
            // this viewport transparent preserves that card's corner cutouts.
            // FIRST child ⇒ paints first: clears the frame's TRANSCRIPT
            // selection registry before any row's text elements re-register
            // (document paint order = selection order; see markdown/render.rs).
            // Scoped: the diff pane resets its own registry separately.
            .child(crate::markdown::render::selection_frame_reset(self.scope))
            .child(
                list(self.list.clone(), cx.processor(Self::render_row))
                    .size_full()
                    .with_sizing_behavior(gpui::ListSizingBehavior::Auto),
            )
            .child(self.wheel_observer())
            .child(rail);
        // The shared Comment pill/editor lives in the SHELL's deferred layer
        // (paints above every clipped surface). The transcript only guards
        // liveness: a doc commit that replaced the offer's head row drops it.
        self.dismiss_stale_popup(cx);
        // Full-size viewer for a clicked user-bubble thumbnail
        // (AttachmentPreviewDialog: bare lightbox, click closes).
        if let Some(preview) = self.attachment_preview.clone() {
            let weak = cx.weak_entity();
            return root.child(crate::attachments::lightbox(
                window.viewport_size(),
                &preview,
                &self.attachment_preview_focus,
                move |_, cx| {
                    weak.update(cx, |this, cx| {
                        this.attachment_preview = None;
                        cx.notify();
                    })
                    .ok();
                },
            ));
        }
        root
    }
}
