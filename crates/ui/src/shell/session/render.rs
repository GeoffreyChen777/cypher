//! Rendering a session's chat column: transcript, composer dock, the
//! jump-to-bottom button and the status strip.

use super::*;

impl Shell {
    /// A tile's chat column (the old main outlet): transcript underlay with
    /// its edge fade, find bar, jump pill, status strip and composer — all
    /// bound to the slot's session. `chat_height` is the column's height
    /// (the session area above the terminal dock).
    pub(super) fn render_session_chat(
        &mut self,
        sid: SlotId,
        chat_height: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme_owned = crate::appearance::chat_style::theme(cx);
        let theme = &theme_owned;
        let faint = theme.text_faint;
        let Some(slot) = self.slots.get(&sid) else {
            return Empty.into_any_element();
        };
        let (transcript, composer, slot_state) = (
            slot.transcript.clone(),
            slot.composer.clone(),
            slot.state.clone(),
        );
        let has_selection = slot_state.read(cx).selected_chat.is_some();
        let has_spaces = !slot_state.read(cx).spaces.is_empty();
        // The measured status strip + composer stack floating over the
        // column's bottom (last frame's — see `bottom_stack`).
        let measured = slot.bottom_stack.clone();
        let stack_h = measured.get();
        // Canvas content centers in the room ABOVE that stack (never behind
        // the composer); short tiles shed the helper line, then the
        // wordmark.
        let canvas_room = chat_height - stack_h;
        let show_wordmark = canvas_room >= 110.0;
        let show_helper = canvas_room >= 150.0;
        let space_name: SharedString = slot_state
            .read(cx)
            .selected_space_row()
            .map(|s| s.display_name().to_string())
            .unwrap_or_default()
            .into();

        // Content outlet: selected chat → transcript; nothing selected → the
        // "Send a message to start" canvas with a watermark; no spaces at all
        // → the onboarding card. The composer sits below the first two
        // (new-chat mode mints the chat id on first send).
        let outlet: AnyElement = if has_selection {
            transcript.clone().into_any_element()
        } else if !has_spaces {
            // Onboarding (first boot / after the destructive wipe / a
            // project-less opt-out with nothing to work in): no folders to
            // target yet — one clear affordance, regardless of `no_project`
            // (an unselected canvas with zero spaces has nothing to send to).
            let _ = faint;
            onboarding_canvas(stack_h, theme, cx)
        } else {
            // New-chat canvas: the Cypher wordmark over the target selectors
            // (device + project — moved up from the composer footer) and the
            // helper line.
            let helper: SharedString = if space_name.is_empty() {
                "Send a message to start a new session.".into()
            } else {
                format!("Send a message to start a session in {space_name}.").into()
            };
            let pickers = composer.read(cx).pickers().clone();
            let selectors = pickers.update(cx, |p, cx| p.render_target_selectors(cx));
            new_chat_canvas(
                selectors,
                helper,
                stack_h,
                show_wordmark,
                show_helper,
                theme,
            )
        };

        let status = self.render_status_strip(sid, cx);
        // File dropzone over the ENTIRE conversation column (transcript +
        // composer, not just the pill): dragging OS files anywhere across the
        // chat area shows the "Drop images to attach" veil; a drop stages the
        // files in the composer. `has_active_drag` gates the veil so a drag
        // that left the window (FileDrop Exited) can't strand it.
        let Some(slot) = self.slots.get(&sid) else {
            return Empty.into_any_element();
        };
        let file_drag_active = slot.file_drag_active && cx.has_active_drag();
        div()
            .id("chat-dropzone")
            // Background belongs to the rounded outer card. A rectangular
            // child fill would cover its corners despite overflow_hidden.
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            // The terminal dock yields to the composer stack
            // (`fit_terminal_height`); only an area shorter than the stack
            // itself clips it — from the top, so the input row and buttons
            // stay on screen and nothing paints over the dock below.
            .justify_end()
            .overflow_hidden()
            .on_drag_move::<gpui::ExternalPaths>(cx.listener(
                move |this, e: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                    let inside = e.bounds.contains(&e.event.position);
                    if let Some(slot) = this.slots.get_mut(&sid)
                        && slot.file_drag_active != inside
                    {
                        slot.file_drag_active = inside;
                        cx.notify();
                    }
                },
            ))
            .on_drop(
                cx.listener(move |this, paths: &gpui::ExternalPaths, _, cx| {
                    let Some(slot) = this.slots.get_mut(&sid) else {
                        return;
                    };
                    slot.file_drag_active = false;
                    let paths = paths.paths().to_vec();
                    slot.composer
                        .update(cx, |composer, cx| composer.add_paths(paths, cx));
                    cx.notify();
                }),
            )
            .child(self.render_transcript_underlay(sid, outlet, stack_h, window, cx))
            // The glass chrome stack, floating over the transcript's bottom:
            // reserved status strip (h-6, the WorkingIndicator — the composer
            // below never shifts) and composer. A paint-time
            // canvas measures the stack for next frame's fade inset and
            // transcript clearance. The flex_1 spacer has no id/listeners, so
            // pointer + wheel events over it fall through to the list below.
            .child(div().flex_1().min_h_0())
            .child(chrome_stack(
                measured,
                status,
                (has_selection || has_spaces).then(|| composer.clone()),
            ))
            .when(file_drag_active, |el| el.child(drop_veil(theme)))
            .into_any_element()
    }

    /// Full-height underlay: the transcript viewport spans the
    /// whole column, scrolling under a small top band and the
    /// composer stack below. The per-glyph EdgeFade (glass-safe,
    /// same as the sidebar's) spans the full column with
    /// ASYMMETRIC bands sized to the chrome: content is opaque at
    /// the chrome's inner edge and fades to zero at the window
    /// edge — visible mid-fade through the glass chrome it slides
    /// under. Always on (the resting paddings keep pinned content
    /// out of the bands, and gating on measured scroll state left
    /// the top unfaded for one frame on session switch — user
    /// report). The jump pill floats outside the fade scope,
    /// anchored above the measured stack.
    fn render_transcript_underlay(
        &mut self,
        sid: SlotId,
        outlet: AnyElement,
        stack_h: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        // The terminal dock lives below the whole chat column
        // (see `render_session`), so only the status strip +
        // composer overlap the transcript. Opaque from the
        // composer PILL's top (the reserved status strip above
        // it is empty air), zero at the underlay's bottom edge.
        let bottom_band = (stack_h - Theme::STATUS_STRIP_HEIGHT).max(1.0);
        div()
            .absolute()
            .inset_0()
            .child(
                crate::kit::edge_fade::edge_faded(
                    Theme::TRANSCRIPT_FADE_BAND,
                    true,
                    true,
                    div().size_full().child(outlet),
                )
                // The tile header is a normal row above the
                // viewport: content is fully faded at its
                // bottom edge and opaque one band below.
                .band_top(crate::transcript::TOP_CHROME_PX)
                .band_bottom(bottom_band),
            )
            .children(self.render_jump_to_bottom(sid, stack_h, cx))
            // The find bar floats in the same layer, at the top
            // — outside the fade scope, over the transcript.
            .children(self.render_find_bar(sid, window, cx))
    }

    /// The "↓ Scroll to bottom" pill: a LABELED rounded-full
    /// chip — down-arrow glyph + 13px label on a near-opaque raised surface
    /// with a hairline — horizontally centered over the transcript column and
    /// floating a small gap above the composer. It hangs 14px below the
    /// conversation region (through the reserved h-6 status strip, whose
    /// content is left-aligned) so its bottom edge sits ~10px above the pill.
    /// Shown past the transcript's 320px threshold; 180ms fade + 2px rise in.
    /// `stack_h` is the measured bottom chrome stack the full-height
    /// transcript scrolls under — the pill anchors just above it (the -14
    /// carries the old status-strip overlap).
    fn render_jump_to_bottom(
        &mut self,
        sid: SlotId,
        stack_h: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let transcript = self.slots.get(&sid)?.transcript.clone();
        if !transcript.read(cx).jump_button_shown() {
            return None;
        }
        let theme = Theme::of(cx);
        // Per-slot hover-fade key (the fade store is global).
        let jump_key = format!("jump-pill-{sid}");
        Some(
            div()
                .absolute()
                .bottom(px(stack_h - 14.0))
                .left_0()
                .right(px(10.0))
                .flex()
                .justify_center()
                .child(motion::dialog_in(
                    "jump-to-bottom",
                    div()
                        .id("jump-to-bottom-btn")
                        .h(px(30.0))
                        .rounded_full()
                        .border_1()
                        .border_color(theme.border)
                        .shadow_md()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .pl(px(11.0))
                        .pr(px(13.0))
                        .cursor_pointer()
                        // Hover must BRIGHTEN the opaque pill, never replace it
                        // with a translucent wash (a 10%-alpha bg here made the
                        // pill go see-through on hover — user-reported), and it
                        // fades over the CSS transition-colors 150ms, not snaps.
                        .bg(motion::hover_blend(
                            &jump_key,
                            theme.surface_raised,
                            theme.surface_raised_hover,
                        ))
                        .on_hover(motion::hover_listener(jump_key.clone()))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            transcript.update(cx, |transcript, cx| transcript.jump_to_bottom(cx));
                        }))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from("↓")),
                        )
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(theme.text)
                                .child(SharedString::from("Scroll to bottom")),
                        ),
                ))
                .into_any_element(),
        )
    }

    /// Working indicator strip: gradient spinner + rotating flavour word (7s,
    /// seeded per chat) + elapsed, staleness-gated via [`Indicator`]; falls back
    /// to a "Sending…" bridge and then the engine mode line. The strip's right
    /// side hosts the session-level subagents trigger — `[left status][flex
    /// spacer][Subagents accessory]` — so the left status and the right
    /// accessory can coexist. Both outer accessories align with the points
    /// where the composer pill's rounded top corners finish and become flat,
    /// pulling them slightly inward from the pill's outer edges.
    fn render_status_strip(&mut self, sid: SlotId, cx: &mut Context<Self>) -> AnyElement {
        let Some(slot) = self.slots.get(&sid) else {
            return Empty.into_any_element();
        };
        let (slot_state, composer, subagents) = (
            slot.state.clone(),
            slot.composer.clone(),
            slot.subagents.clone(),
        );
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let state = slot_state.read(cx);

        // Aligned with the composer column: the pill starts after the
        // composer's 16px column padding, then its 26px top-corner radius
        // ends. Inset both accessories by their sum so the left/right trigger
        // boundaries land exactly on those corner endpoints.
        let accessory_inset = Theme::SPACE_LG + crate::composer::PILL_RADIUS;
        let wide = crate::appearance::chat_style::settings(cx).wide;
        let strip = div()
            .h(px(Theme::STATUS_STRIP_HEIGHT))
            .flex_none()
            .w_full()
            .when(!wide, |el| {
                el.max_w(px(crate::appearance::chat_style::COMPOSER_WIDTH))
            })
            .mx_auto()
            .flex()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .px(px(accessory_inset + if wide { 32.0 } else { 0.0 }))
            .text_size(px(11.0));

        let left: AnyElement = match state.selected_chat.as_deref() {
            None => Empty.into_any_element(),
            Some(chat_id) => {
                let indicator = state.indicator_for(chat_id, now);
                // Timer base: the freshest of the session row's turn start and
                // the in-flight send. During the send→ack window the row (if
                // any) still carries the PREVIOUS turn's start, and using it
                // opened the timer at the old turn's elapsed instead of 0:00.
                let started = state
                    .session_for(chat_id)
                    .and_then(|s| s.started_at)
                    .into_iter()
                    .chain(state.pending_send_started(chat_id, now))
                    .max();
                let _ = started;
                let sending = composer.read(cx).is_sending();

                // Unused here since the Working loader moved into the transcript
                // (its trailer computes its own elapsed).
                match indicator {
                    // The working loader lives in the TRANSCRIPT now, under the
                    // streaming reply (user request) — the strip stays empty
                    // (its reserved height still steadies the composer).
                    Indicator::Working => Empty.into_any_element(),
                    // No label: the QuestionPanel right below IS the
                    // awaiting-input surface — a strip caption above it was
                    // redundant (user request).
                    Indicator::AwaitingInput => Empty.into_any_element(),
                    Indicator::Errored => div()
                        .text_color(theme.danger)
                        .child(SharedString::from("Run failed"))
                        .into_any_element(),
                    Indicator::None if sending => div()
                        .flex()
                        .items_center()
                        .gap(px(Theme::SPACE_SM))
                        .child(loaders::gradient_spinner(
                            "sending-indicator",
                            &theme,
                            2.5,
                            cx.entity_id(),
                            cx,
                        ))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from("Sending…")),
                        )
                        .into_any_element(),
                    Indicator::None => Empty.into_any_element(),
                }
            }
        };

        // Pending-comments indicator pinned to the strip's upper-left; the
        // session status sits centered; the Subagents trigger stays right.
        let comments_trigger =
            composer.update(cx, |composer, cx| composer.render_comments_trigger(cx));
        strip
            .child(comments_trigger)
            .child(div().flex_1())
            .child(left)
            // Flex spacer keeps the center status centered and the subagents
            // trigger pinned to the composer-aligned right edge; when the
            // trigger renders Empty (no records) the strip is just the left
            // content.
            .child(div().flex_1())
            .child(subagents)
            .into_any_element()
    }
}

/// The no-projects onboarding canvas: one clear affordance.
fn onboarding_canvas(stack_h: f32, theme: &Theme, cx: &mut Context<Shell>) -> AnyElement {
    div()
        .size_full()
        .pb(px(stack_h))
        .overflow_hidden()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .child(motion::fade_in(
            "no-spaces-canvas",
            div()
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .font_family(theme.font_sans.clone())
                        .text_size(px(21.0))
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(theme.text.opacity(0.18))
                        .child(SharedString::from("Let's Cypher")),
                )
                .child(
                    div()
                        .mt(px(24.0))
                        .text_size(px(16.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(SharedString::from("Add a project to get started")),
                )
                .child(
                    div()
                        .mt(px(6.0))
                        .text_size(px(13.0))
                        .text_color(theme.text_muted.opacity(0.7))
                        .child(SharedString::from(
                            "A project is a folder on one of your devices.",
                        )),
                )
                .child(
                    popover::btn_primary(theme, "Add a project")
                        .id("onboarding-add-space")
                        .mt(px(20.0))
                        .on_click(cx.listener(|this, _, _, cx| this.open_add_space(cx))),
                ),
        ))
        .into_any_element()
}

/// New-chat canvas: the Cypher wordmark over the target selectors (device +
/// project) and the helper line; short tiles shed the helper, then the
/// wordmark.
fn new_chat_canvas(
    selectors: AnyElement,
    helper: SharedString,
    stack_h: f32,
    show_wordmark: bool,
    show_helper: bool,
    theme: &Theme,
) -> AnyElement {
    div()
        .size_full()
        .pb(px(stack_h))
        .overflow_hidden()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .child(motion::fade_in(
            "new-chat-canvas",
            div()
                .max_w_full()
                .px(px(12.0))
                .flex()
                .flex_col()
                .items_center()
                .when(show_wordmark, |el| {
                    el.child(
                        div()
                            .mb(px(16.0))
                            .font_family(theme.font_sans.clone())
                            .text_size(px(21.0))
                            .font_weight(gpui::FontWeight::NORMAL)
                            .text_color(theme.text.opacity(0.32))
                            .child(SharedString::from("Let's Cypher")),
                    )
                })
                .child(selectors)
                .when(show_helper, |el| {
                    el.child(
                        div()
                            .mt(px(12.0))
                            .text_size(px(14.0))
                            .text_center()
                            .text_color(theme.text_muted.opacity(0.6))
                            .child(helper),
                    )
                }),
        ))
        .into_any_element()
}

/// The glass chrome stack floating over the transcript's bottom: the
/// status strip and (where there is something to send into) the composer,
/// measured for next frame's fade inset and transcript clearance.
fn chrome_stack(
    measured: Rc<Cell<f32>>,
    status: AnyElement,
    composer: Option<Entity<Composer>>,
) -> gpui::Div {
    div()
        .flex_none()
        .relative()
        .flex()
        .flex_col()
        .child(
            gpui::canvas(
                move |bounds, _, _| measured.set(f32::from(bounds.size.height)),
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        // The session-level subagents trigger lives INSIDE the
        // status strip (right edge, next to the composer); the
        // inspector it opens is a floating layer and never
        // participates in this stack's measurement.
        .child(status)
        // A SELECTED chat keeps its composer even with zero live
        // spaces (a selected project-less / unavailable-project
        // chat still needs its input row); an unselected canvas
        // with no spaces shows onboarding instead, so the
        // composer only mounts where there is something to send
        // into.
        .children(composer)
}

/// The "Drop images to attach" veil over the column during an OS file drag.
fn drop_veil(theme: &Theme) -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        .rounded(px(12.0))
        .bg(theme.scrim().opacity(0.4 / 0.6))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.0))
        .text_color(theme.text)
        .child("Drop images to attach")
}
