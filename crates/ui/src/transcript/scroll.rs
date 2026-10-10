//! Rail plumbing (rendering lives in `rail.rs`), scrolling, stick-to-bottom
//! and the doc → rows sync.

use super::*;

impl Transcript {
    /// Shell-driven width gate: the rail hides below 48rem of container width.
    pub fn set_rail_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.rail_enabled != enabled {
            self.rail_enabled = enabled;
            cx.notify();
        }
    }

    pub fn rail_enabled(&self) -> bool {
        self.rail_enabled
    }

    /// Shell-driven: the measured height of the bottom chrome stack the
    /// transcript scrolls under. Sub-pixel jitter is ignored so steady-state
    /// frames don't re-notify.
    pub fn set_bottom_clearance(&mut self, height: f32, cx: &mut Context<Self>) {
        if (self.bottom_clearance - height).abs() > 0.5 {
            self.bottom_clearance = height;
            if self.own_turn.is_some() {
                self.remeasure_last_row();
                self.own_turn_kick = true;
            }
            cx.notify();
        }
    }

    /// Shell-driven: the tile's chat column size. A layout change or tile
    /// resize moves the viewport under a list anchored for the old size —
    /// the pinned tail could sit off screen (blank until the next commit),
    /// so re-anchor: a pinned list snaps back to its end, a held own-turn
    /// prompt re-sizes its runway.
    pub fn set_viewport_size(&mut self, width: f32, height: f32, cx: &mut Context<Self>) {
        let changed = self
            .viewport_size
            .is_none_or(|(w, h)| (w - width).abs() > 0.5 || (h - height).abs() > 0.5);
        if !changed {
            return;
        }
        let first = self.viewport_size.is_none();
        self.viewport_size = Some((width, height));
        if first || self.rows.is_empty() {
            return;
        }
        if self.own_turn.is_some() {
            self.remeasure_last_row();
            self.own_turn_kick = true;
        } else if self.pinned {
            self.list.scroll_to_end();
            self.spring.reset();
            self.spring_kick = true;
        }
        cx.notify();
    }

    pub fn rail_hover(&self) -> Option<usize> {
        self.rail_hover
    }

    pub fn set_rail_hover(&mut self, hover: Option<usize>) {
        self.rail_hover = hover;
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn list_state(&self) -> &ListState {
        &self.list
    }

    pub fn state_entity(&self) -> &Entity<AppState> {
        &self.state
    }

    /// Replace the transcript's scroll animation task (rail click / jump).
    pub fn set_scroll_task(&mut self, task: Task<()>) {
        // Rail navigation within the session RELEASES the hold but keeps the
        // runway (user spec: only leaving and revisiting the session clears
        // it) — scrolling back down re-arms the hold like any restick.
        self.release_own_turn_hold();
        self.pinned = false;
        self.prompt_nav = None;
        self.scroll_anim = Some(task);
    }

    /// ↑/↓ on a focused transcript: glide to the previous/next prompt — the
    /// rail's stops and the rail's glide. Past the last prompt, ↓ returns to
    /// the live bottom.
    pub(super) fn step_prompt(&mut self, forward: bool, cx: &mut Context<Self>) {
        // The attachment lightbox sits inside this context; keys are its own.
        if self.attachment_preview.is_some() {
            return;
        }
        let glide = motion::SCROLL_GLIDE.total();
        let in_flight = self
            .prompt_nav
            .filter(|(_, started)| started.elapsed() < glide)
            .map(|(row, _)| row);
        let top = match in_flight {
            Some(row) => (row, 0.0),
            None => {
                if forward && (self.is_glued() || self.distance_from_bottom() <= AT_BOTTOM_PX) {
                    // Already at the end: nothing below can reach the top.
                    if !self.pinned {
                        self.jump_to_bottom(cx);
                    }
                    return;
                }
                // The glued anchor sits one viewport below the visible top;
                // materialize it as the true top, as `scroll_to_row` does.
                let viewport = f32::from(self.list.viewport_bounds().size.height);
                if self.is_glued() && viewport > 0.0 {
                    self.list.scroll_by(px(-(viewport + 0.5)));
                }
                let top = self.list.logical_scroll_top();
                (top.item_ix, f32::from(top.offset_in_item))
            }
        };
        let rows: Vec<usize> = self
            .prompt_rows(cx)
            .into_iter()
            .map(|(_, row)| row)
            .collect();
        match crate::transcript::rail::prompt_step(&rows, top, forward) {
            Some(target) => {
                self.scroll_to_row(target, cx);
                self.prompt_nav = Some((target, Instant::now()));
            }
            None if forward => {
                self.scroll_anim = None;
                self.prompt_nav = None;
                self.jump_to_bottom(cx);
            }
            None => {}
        }
    }

    /// Give the viewport to the user/navigation without dropping the
    /// reservation: the pad stays, the hold stands down until a restick.
    fn release_own_turn_hold(&mut self) {
        if let Some(anchor) = self.own_turn.as_mut() {
            anchor.held = false;
        }
        self.own_turn_last_tick = None;
    }

    fn remeasure_last_row(&self) {
        if let Some(last) = self.rows.len().checked_sub(1) {
            self.list.remeasure_items(last..last + 1);
        }
    }

    pub fn distance_from_bottom(&self) -> f32 {
        let max = f32::from(self.list.max_offset_for_scrollbar().y);
        let cur = f32::from(self.list.scroll_px_offset_for_scrollbar().y);
        (max + cur).max(0.0)
    }

    /// Whether a user scroll should re-engage the bottom pin: inside the 70px
    /// stick band *and* moving toward the bottom. Direction matters — a small
    /// wheel-up notch near the bottom stays inside the band, and re-sticking
    /// on it would snap the view straight back, making the pin unbreakable.
    /// `wheel_dy` is the input's own delta (see [`Self::wheel_dy`]).
    pub fn should_restick(distance: f32, wheel_dy: f32) -> bool {
        distance <= STICK_THRESHOLD_PX && wheel_dy < 0.0
    }

    /// Records each wheel event's vertical delta for [`Self::handle_scroll`]
    /// (the list's scroll event carries none). Capture phase: it runs before
    /// the list scrolls, and no row's bubble-phase `stop_propagation` can
    /// hide the event from it. Hitbox-free, so it never blocks input.
    pub(super) fn wheel_observer(&self) -> impl IntoElement {
        let wheel_dy = self.wheel_dy.clone();
        gpui::canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                window.on_mouse_event(move |event: &gpui::ScrollWheelEvent, phase, _, _| {
                    if phase == gpui::DispatchPhase::Capture && bounds.contains(&event.position) {
                        // Same line height the list scrolls lines by.
                        wheel_dy.set(f32::from(event.delta.pixel_delta(px(20.0)).y));
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }

    pub(super) fn handle_scroll(&mut self, _event: &ListScrollEvent, cx: &mut Context<Self>) {
        // The list invokes this handler ONLY from its wheel/touch input path
        // (programmatic scroll_by/scroll_to never re-enter it), while holding
        // its internal RefCell borrow — reading the ListState back
        // synchronously panics with "already mutably borrowed". Defer to the
        // end of the effect cycle, after the list has released its borrow.
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            this.update(cx, |this: &mut Transcript, cx| {
                // User input scrolled: the Comment pill would float over the
                // wrong text — dismiss it and the selection wash (transient
                // UI).
                if this
                    .comment_popup
                    .upgrade()
                    .is_some_and(|p| p.read(cx).is_active())
                {
                    this.dismiss_comment_ui_and_selection(cx);
                }
                // Wheel/touch while a runway lives: input owns the viewport,
                // and the BOTTOM PIN must stay out of it entirely. Escaping
                // releases the hold (the reservation stays behind as plain
                // scrollable space); returning toward the bottom re-arms the
                // HOLD, never `pinned` — a restick pin glued the view to the
                // bottom of the reservation pad, where streaming reads as
                // text stuck at the viewport top with the runway never
                // filling (user report; the pad can't resize there either,
                // its anchor being off-screen). macOS trackpad momentum can
                // even release-and-restick within one gesture right after a
                // send, so under the old rules the prompt never landed at
                // the top at all.
                //
                // Direction comes from the wheel event itself, never from
                // the bottom distance moving: layout moves the bottom too (a
                // streaming commit grows the content a frame before the pad
                // shrinks to match), so a distance baseline went stale and a
                // wheel-up shorter than the drift read as "toward the
                // bottom" — the hold re-asserted on every notch and the chat
                // would not scroll at all (user report, rig-reproduced).
                let wheel_dy = this.wheel_dy.take();
                let away = wheel_dy > 0.0;
                if this.own_turn.is_some() {
                    let distance = this.distance_from_bottom();
                    let held = this.own_turn.as_ref().is_some_and(|a| a.held);
                    if away {
                        // Input moving away from the bottom breaks the hold —
                        // any amount: the hard stop below re-asserts per
                        // event, so a size threshold would swallow a slow
                        // trackpad drag whole.
                        if let Some(anchor) = this.own_turn.as_mut() {
                            anchor.held = false;
                        }
                        this.own_turn_last_tick = None;
                        this.pinned = false;
                        this.spring.reset();
                        this.spring_last_tick = None;
                    } else if !held
                        && (distance <= AT_BOTTOM_PX || Self::should_restick(distance, wheel_dy))
                    {
                        // Returning to the bottom returns to the RUNWAY: the
                        // glide re-lands the prompt at its inset.
                        if let Some(anchor) = this.own_turn.as_mut() {
                            anchor.held = true;
                            anchor.positioned = false;
                        }
                        this.own_turn_last_tick = None;
                        this.own_turn_kick = true;
                    } else if held {
                        // Wheel-down while held: the bottom is a HARD STOP.
                        // The pad runs one frame behind a streaming commit,
                        // so the list's own end-clamp can briefly admit
                        // travel into the transient surplus — re-assert the
                        // hold in the same effect cycle, before anything
                        // paints, and the sink never reaches the screen.
                        // (scroll_to is bounds-free, so this also covers the
                        // wheel gluing the offset at the end.)
                        if let Some(ix) = this.own_turn_anchor_ix() {
                            this.list.scroll_to(ListOffset {
                                item_ix: ix,
                                offset_in_item: px(0.0),
                            });
                            this.list.scroll_by(px(-this.own_send_inset(ix)));
                        }
                    }
                    let show = distance > SCROLL_BUTTON_THRESHOLD_PX
                        && !this.own_turn.as_ref().is_some_and(|a| a.held);
                    if show != this.show_jump_button {
                        this.show_jump_button = show;
                    }
                    cx.notify();
                    return;
                }
                let distance = this.distance_from_bottom();
                if away && distance > AT_BOTTOM_PX {
                    // User input moving away from the bottom breaks the pin.
                    // Content growth never lands here — it doesn't fire the
                    // scroll handler (as in mugen: interrupt from input, not
                    // scrollbar position).
                    this.pinned = false;
                    this.spring.reset();
                    this.spring_last_tick = None;
                } else if distance <= AT_BOTTOM_PX || Self::should_restick(distance, wheel_dy) {
                    // Returning toward the bottom inside the 70px band (or
                    // arriving at it) re-engages the pin with a glide.
                    if !this.pinned {
                        this.pinned = true;
                        this.wake_spring();
                    }
                }
                let show = distance > SCROLL_BUTTON_THRESHOLD_PX && !this.pinned;
                if show != this.show_jump_button {
                    this.show_jump_button = show;
                }
                cx.notify();
            })
            .ok();
        });
    }

    /// Reserve the reply's space below a locally-sent prompt — EVERY send,
    /// not just the first (a steer or a post-turn send used to collapse the
    /// previous reservation and drop the messages back down — user report).
    /// [`Self::step_own_turn`] sizes the reservation; the motion is just the
    /// bottom pin: with the pad installed, the spring's glide to the new
    /// bottom lands the prompt at the top. Replacing a still-held previous
    /// anchor collapses its pad into the same glide — one continuous motion.
    pub fn on_own_send(&mut self, chat_id: String, message_id: String, cx: &mut Context<Self>) {
        self.pinned = false;
        self.show_jump_button = false;
        self.spring.reset();
        self.spring_last_tick = None;
        self.spring_settled_at = None;
        self.spring_kick = false;
        self.scroll_anim = None;
        // A glued offset re-snaps to the end on EVERY layout — the pad would
        // land and the viewport hard-track its bottom in the same frame,
        // skipping the glide entirely (rig-traced). Pin the offset to a
        // CONCRETE visible item first; the pad then reads as scrollable
        // distance for the glide to cover.
        self.materialize_scroll_anchor();
        self.own_turn = Some(OwnTurnAnchor {
            chat_id,
            message_id: SharedString::from(message_id),
            runway: 0.0,
            held: true,
            positioned: false,
        });
        self.own_turn_last_tick = None;
        self.own_turn_kick = true;
        self.remeasure_last_row();
        cx.notify();
    }

    /// Convert a glued scroll offset (`None`/past-the-end — layout re-snaps
    /// it to the end each frame) into a concrete `{item, offset}` anchored at
    /// the first visible row, which layout holds still.
    fn materialize_scroll_anchor(&mut self) {
        if !self.is_glued() {
            return;
        }
        let vp_top = f32::from(self.list.viewport_bounds().top());
        for ix in 0..self.rows.len() {
            if let Some(bounds) = self.list.bounds_for_item(ix)
                && f32::from(bounds.bottom()) > vp_top + 0.5
            {
                self.list.scroll_to(ListOffset {
                    item_ix: ix,
                    offset_in_item: px(vp_top - f32::from(bounds.top())),
                });
                return;
            }
        }
    }

    /// The held prompt's top offset from the viewport top. Row 0 already
    /// carries the titlebar chrome inside its own box (the first row's
    /// top gap), so the hold adds nothing — adding the inset on top parked
    /// a new chat's first prompt a double-chrome ~66px low (user report).
    /// An embedded panel (temporary Side Chat) has no titlebar chrome, so its
    /// non-first-row inset is the smaller panel top gap.
    fn own_send_inset(&self, anchor_ix: usize) -> f32 {
        if anchor_ix == 0 {
            0.0
        } else if self.embedded {
            EMBEDDED_TOP_INSET_PX
        } else {
            OWN_SEND_TOP_INSET_PX
        }
    }

    fn own_turn_anchor_ix(&self) -> Option<usize> {
        let anchor = self.own_turn.as_ref()?;
        self.rows
            .iter()
            .position(|row| row.turn_start && row.entry_id == anchor.message_id)
    }

    /// One post-layout own-turn step: size the reservation pad. Pure layout —
    /// all motion is the ordinary bottom pin (see [`OwnTurnAnchor`]).
    pub(super) fn step_own_turn(&mut self, cx: &mut Context<Self>) {
        self.own_turn_kick = false;
        let Some(anchor_ix) = self.own_turn_anchor_ix() else {
            // The optimistic echo may arrive on the next state notification.
            return;
        };
        let viewport = self.list.viewport_bounds();
        let viewport_height = f32::from(viewport.size.height);
        if viewport_height <= 0.0 {
            self.own_turn_kick = true;
            cx.notify();
            return;
        }
        let Some(last_ix) = self.rows.len().checked_sub(1) else {
            return;
        };
        let base_pad = self.bottom_clearance + Theme::TRANSCRIPT_FADE_BAND + 8.0;
        let inset = self.own_send_inset(anchor_ix);
        // A glued offset hard-tracks a GROWING end — streamed text visually
        // pushes everything above it up while the runway blank persists
        // below (user report; the glued representation also hides every
        // item's bounds, so the sizing that would consume the runway goes
        // blind). Dissolve it for HELD and RELEASED views alike. The glued
        // sentinel resolves NUMERICALLY to the total content height (a
        // viewport top past the last item), so a small nudge lands in an
        // absurd overscroll that layout's under-fill normalizer re-glues on
        // the very next frame — an invisible wedge loop (rig-traced).
        // Stepping back a FULL viewport from the sentinel is exactly "end
        // at the screen bottom": the same visual position, concrete.
        if self.is_glued() {
            self.list.scroll_by(px(-viewport_height));
        }
        // The slack keeps the held layout scrollable (see the constant) —
        // the reservation deliberately over-fills by this much.
        let usable = viewport_height - inset - base_pad + OWN_SEND_SCROLL_SLACK_PX;
        let current = self.own_turn.as_ref().map_or(0.0, |a| a.runway);

        // A fresh anchor installs a provisional pad BEFORE anything needs
        // bounds: the just-sent rows sit below the fold, unmeasured, and
        // without the pad there is no scroll room to bring them into the
        // measured window (gating the pad on their bounds deadlocked — the
        // clamped scroll kept them unmeasured forever). Sized at FULL
        // `usable` — a deliberate overshoot by the turn's own height, safe
        // under the absolute hold (scroll_to pins the prompt regardless) and
        // REQUIRED for short chats: gpui's bottom-aligned list reports no
        // item bounds while its content is shorter than the viewport
        // (rig-traced: a new session's first send sat ~150px below the
        // inset forever — the old undershot pad left the content short, the
        // bounds-free scroll_to clamped, and the bounds-gated refinement
        // could never rescue it). Overshooting guarantees the scroll room;
        // the surplus sits below the fold until the refinement trues it.
        if current <= 0.0 {
            if let Some(anchor) = self.own_turn.as_mut() {
                anchor.runway = usable.max(0.0);
            }
            self.remeasure_last_row();
            cx.notify();
            return;
        }

        let frame = OwnTurnFrame {
            anchor_ix,
            last_ix,
            viewport,
            viewport_height,
            inset,
            base_pad,
            usable,
            current,
        };
        // ---- reservation sizing (skipped while unmeasured: the provisional
        // pad stands; the render gate re-runs this every live frame) --------
        if self.size_own_turn_reservation(&frame, cx) {
            return;
        }

        // ---- entry glide, then absolute hold -------------------------------
        let (held, positioned) = self
            .own_turn
            .as_ref()
            .map_or((false, false), |a| (a.held, a.positioned));
        if !held {
            return;
        }
        if positioned {
            self.hold_own_turn(&frame, cx);
            return;
        }
        self.glide_own_turn(&frame, cx);
    }

    /// Size the reservation from the measured turn (skipped while
    /// unmeasured: the provisional pad stands; the render gate re-runs this
    /// every live frame). Returns true when the step is over: the reply
    /// outgrew the reservation and the anchor was dropped.
    fn size_own_turn_reservation(&mut self, frame: &OwnTurnFrame, cx: &mut Context<Self>) -> bool {
        let OwnTurnFrame {
            anchor_ix,
            last_ix,
            usable,
            current,
            base_pad,
            ..
        } = *frame;
        if let (Some(anchor_bounds), Some(last_bounds)) = (
            self.list.bounds_for_item(anchor_ix),
            self.list.bounds_for_item(last_ix),
        ) {
            // Content height of the turn, excluding the pads on the last row.
            let turn_height = f32::from(last_bounds.bottom())
                - f32::from(anchor_bounds.top())
                - current
                - base_pad;
            let target = own_turn_reservation(usable, turn_height);
            // FLOOR: never shrink the pad faster than the viewport allows.
            // The step runs a frame behind content growth, so a wheel that
            // lands inside that window can sink the view toward the stale
            // end; snapping the pad straight to `target` then pulls the end
            // UP THROUGH the viewport (the list clamps instantly — a visible
            // yank, user report "stutter push back"). Shrinking is capped so
            // the end never rises above the current view; deferred surplus
            // burns off as the view moves away from the stop.
            let dist = self.distance_from_bottom();
            let floor = current - (dist - OWN_SEND_SCROLL_SLACK_PX).max(0.0);
            let target = target.max(floor.min(current));
            if target <= 0.5 {
                // The reply has outgrown the reserved space (or the prompt
                // alone overfills it): the pad is ~0, so dropping it is
                // height-neutral. A still-held view hands off to the bottom
                // pin; a released one doesn't move at all.
                let held = self.own_turn.take().is_some_and(|a| a.held);
                self.remeasure_last_row();
                if held {
                    self.engage_pin(cx);
                } else {
                    cx.notify();
                }
                return true;
            }
            if (target - current).abs() > 0.5 {
                if let Some(anchor) = self.own_turn.as_mut() {
                    anchor.runway = target;
                }
                // Growth into the reservation shrinks the pad 1:1 — the held
                // layout never moves.
                self.remeasure_last_row();
                cx.notify();
            }
        }
        false
    }

    /// The absolute hold after landing (see the comment inside).
    fn hold_own_turn(&mut self, frame: &OwnTurnFrame, cx: &mut Context<Self>) {
        let OwnTurnFrame {
            anchor_ix,
            viewport,
            inset,
            ..
        } = *frame;
        // Landed: re-assert the prompt's position after every layout.
        // scroll_to is absolute and bounds-independent, so neither glue
        // re-snaps, pad-sizing lag, nor a splice's unmeasured flicker can
        // carry the view off the prompt (each broke the spring-held
        // variants of this — rig-traced). ONE-SIDED: only upward drift
        // (view above the hold) is corrected. The scroll slack under the
        // reservation is legal resting space — wheel-down sinks into it
        // and stops hard at the list's own clamp; snapping back up from
        // there made the bottom bounce/stutter on every scroll event
        // (user report). Way-below-slack (impossible short of a bug)
        // still re-asserts.
        let moved = match self.list.bounds_for_item(anchor_ix) {
            Some(b) => {
                let err = f32::from(b.top()) - (f32::from(viewport.top()) + inset);
                // The legal rest zone below the hold is the epsilon plus
                // rounding; anything deeper is a transient-collision sink
                // and rubber-bands back.
                err > 0.5 || err < -(OWN_SEND_SCROLL_SLACK_PX + 2.0)
            }
            // Bounds vanish in the glued representation (dissolved
            // above, so at most for this one frame) and through splice
            // flicker. Near the stop that is dead-band space — no
            // assert (asserting on None here was the bottom bounce);
            // far from it the position is unknowable flicker: re-assert.
            None => self.distance_from_bottom() > OWN_SEND_SCROLL_SLACK_PX + 8.0,
        };
        if moved {
            // Correct with the entry glide's ease, not a snap: the only
            // in-band escapes are one-frame commit transients and splice
            // flicker, and an eased ~200ms return reads as native
            // rubber-banding where an instant re-assert read as stutter
            // (user report). Bounds-less flicker still snaps — there is
            // nothing to ease against.
            match self.list.bounds_for_item(anchor_ix) {
                Some(b) => {
                    let err = f32::from(b.top()) - (f32::from(viewport.top()) + inset);
                    let now = Instant::now();
                    let frames = match self.own_turn_last_tick {
                        Some(last) => (now.duration_since(last).as_secs_f32() * 1000.0
                            / SPRING_FRAME_MS)
                            .min(SPRING_MAX_CATCHUP_FRAMES),
                        None => 1.0,
                    };
                    self.own_turn_last_tick = Some(now);
                    let ease = 1.0 - OWN_SEND_GLIDE_RETAIN.powf(frames);
                    if err.abs() <= OWN_SEND_GLIDE_SNAP_PX {
                        self.list.scroll_by(px(err));
                        self.own_turn_last_tick = None;
                    } else {
                        self.list.scroll_by(px(err * ease));
                    }
                    self.own_turn_kick = true;
                }
                None => {
                    self.list.scroll_to(ListOffset {
                        item_ix: anchor_ix,
                        offset_in_item: px(0.0),
                    });
                    self.list.scroll_by(px(-inset));
                    self.own_turn_last_tick = None;
                }
            }
            cx.notify();
        } else {
            self.own_turn_last_tick = None;
        }
    }

    /// The entry glide toward the hold, landing with an absolute snap.
    fn glide_own_turn(&mut self, frame: &OwnTurnFrame, cx: &mut Context<Self>) {
        let OwnTurnFrame {
            anchor_ix,
            viewport,
            viewport_height,
            inset,
            ..
        } = *frame;
        let now = Instant::now();
        let frames = match self.own_turn_last_tick {
            Some(last) => (now.duration_since(last).as_secs_f32() * 1000.0 / SPRING_FRAME_MS)
                .min(SPRING_MAX_CATCHUP_FRAMES),
            None => 1.0,
        };
        self.own_turn_last_tick = Some(now);
        let ease = 1.0 - OWN_SEND_GLIDE_RETAIN.powf(frames);
        // Remaining travel: the anchor's own error once it measures; the
        // bottom distance while it is still below the measured window (the
        // undershot provisional pad guarantees the bottom stops short of the
        // prompt, so this leg can never overshoot it).
        // The two error legs mean DIFFERENT things at zero: on the bounds
        // leg, err 0 is AT the hold (no correction needed); on the bounds-
        // less leg, err is the distance to the pad's bottom — arrival there
        // still needs the absolute snap onto the anchor (the short-chat/
        // glued landing, where bounds never appear). Conflating them once
        // marked entries "positioned" at the pad bottom without ever
        // landing (rig-caught: sends parked deep in blank runway).
        let (err, anchored) = match self.list.bounds_for_item(anchor_ix) {
            Some(bounds) => (
                f32::from(bounds.top()) - (f32::from(viewport.top()) + inset),
                true,
            ),
            None => (self.distance_from_bottom(), false),
        };
        let glide_max = GLIDE_MAX_VIEWPORTS * viewport_height;
        let err = if err > glide_max {
            self.list.scroll_by(px(err - glide_max));
            glide_max
        } else {
            err
        };
        let land = |list: &ListState| {
            list.scroll_to(ListOffset {
                item_ix: anchor_ix,
                offset_in_item: px(0.0),
            });
            list.scroll_by(px(-inset));
        };
        if motion::reduced_motion(cx) {
            land(&self.list);
            if let Some(anchor) = self.own_turn.as_mut() {
                anchor.positioned = true;
            }
            self.own_turn_last_tick = None;
        } else if anchored
            && (-(OWN_SEND_SCROLL_SLACK_PX + 2.0)..=OWN_SEND_GLIDE_SNAP_PX).contains(&err)
        {
            // At the hold — or resting inside the slack under it (a restick
            // that fired at the true bottom): land WITHOUT pulling the view
            // up. Only a still-above position gets the snap.
            if err > 0.5 {
                land(&self.list);
            }
            if let Some(anchor) = self.own_turn.as_mut() {
                anchor.positioned = true;
            }
            self.own_turn_last_tick = None;
        } else if !anchored && err <= OWN_SEND_GLIDE_SNAP_PX {
            // Arrived at the bottom with the anchor still unmeasured: the
            // absolute, bounds-free snap IS the landing.
            land(&self.list);
            if let Some(anchor) = self.own_turn.as_mut() {
                anchor.positioned = true;
            }
            self.own_turn_last_tick = None;
        } else {
            self.list.scroll_by(px(err * ease));
        }
        self.own_turn_kick = true;
        cx.notify();
    }

    /// Whether the transcript is currently pinned to the bottom.
    #[cfg(test)]
    pub fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// Whether the shell should float the "Scroll to bottom" pill (scrolled
    /// more than [`SCROLL_BUTTON_THRESHOLD_PX`] off the end, unpinned).
    pub fn jump_button_shown(&self) -> bool {
        self.show_jump_button
    }

    /// The scroll-to-bottom pill's click: glide back to the end and re-pin.
    pub fn jump_to_bottom(&mut self, cx: &mut Context<Self>) {
        // With a live runway, "bottom" IS the held position (the reservation
        // makes prompt-at-top and pad-bottom the same place): re-arm the hold
        // and glide back instead of destroying the runway (user spec — only
        // navigating away and back clears it).
        if let Some(anchor) = self.own_turn.as_mut() {
            anchor.held = true;
            anchor.positioned = false;
            self.own_turn_last_tick = None;
            self.own_turn_kick = true;
            self.show_jump_button = false;
            cx.notify();
            return;
        }
        self.engage_pin(cx);
    }

    /// Re-engage the bottom pin with a glide. Long jumps teleport to within
    /// [`GLIDE_MAX_VIEWPORTS`] of the end first (mugen `springToBottom`);
    /// reduced motion snaps.
    fn engage_pin(&mut self, cx: &mut Context<Self>) {
        self.pinned = true;
        self.show_jump_button = false;
        if motion::reduced_motion(cx) {
            self.list.scroll_to_end();
            cx.notify();
            return;
        }
        let viewport = f32::from(self.list.viewport_bounds().size.height);
        let distance = self.distance_from_bottom();
        let glide_max = GLIDE_MAX_VIEWPORTS * viewport;
        if viewport > 0.0 && distance > glide_max {
            self.list.scroll_by(px(distance - glide_max));
        }
        self.wake_spring();
        cx.notify();
    }

    /// Arm the per-frame spring driver — `render` schedules the next frame
    /// while [`Self::spring_should_run`].
    fn wake_spring(&mut self) {
        self.spring_settled_at = None;
        self.spring_kick = true;
    }

    /// Whether the spring loop needs another frame: off the bottom, carrying
    /// residual motion, or inside the post-landing settle grace.
    pub(super) fn spring_should_run(&self) -> bool {
        self.spring_kick
            || self.distance_from_bottom() > 0.5
            || !self.spring.is_idle()
            || self.spring_settled_at.is_some()
    }

    /// Whether the scroll offset is in a bottom-glued representation (`None`
    /// or anchored past the end) — states where the next layout hard-snaps to
    /// the new end instead of holding a pixel position.
    pub fn is_glued(&self) -> bool {
        self.list.logical_scroll_top().item_ix >= self.rows.len()
    }

    /// One spring frame: observe target growth, step the stepper, apply the
    /// delta, park after the settle grace. Runs from `window.on_next_frame`,
    /// i.e. after layout — measurements are fresh.
    pub(super) fn step_spring(&mut self, cx: &mut Context<Self>) {
        self.spring_kick = false;
        if !self.pinned {
            self.spring_last_tick = None;
            return;
        }
        let now = Instant::now();
        let frames = match self.spring_last_tick {
            Some(last) => (now.duration_since(last).as_secs_f32() * 1000.0 / SPRING_FRAME_MS)
                .min(SPRING_MAX_CATCHUP_FRAMES),
            None => 1.0,
        };
        self.spring_last_tick = Some(now);

        let target = f32::from(self.list.max_offset_for_scrollbar().y);
        let mut distance = self.distance_from_bottom();
        // Long jumps (chat switch mid-history, huge pastes) teleport first.
        let viewport = f32::from(self.list.viewport_bounds().size.height);
        let glide_max = GLIDE_MAX_VIEWPORTS * viewport;
        if viewport > 0.0 && distance > glide_max {
            self.list.scroll_by(px(distance - glide_max));
            distance = glide_max;
        }
        let pos = target - distance;
        let next = self.spring.step(pos, target, frames);
        if next > pos {
            self.list.scroll_by(px(next - pos));
        }

        if target - next <= 0.5 {
            let settled = *self.spring_settled_at.get_or_insert(now);
            if now.duration_since(settled) >= Duration::from_millis(SPRING_SETTLE_GRACE_MS)
                && self.spring.is_idle()
            {
                // Park: stop scheduling frames until the next wake.
                self.spring.reset();
                self.spring_last_tick = None;
                self.spring_settled_at = None;
                return;
            }
        } else {
            self.spring_settled_at = None;
        }
        cx.notify();
    }

    /// Rebuild rows from app state; splice minimal ranges into the list.
    pub(super) fn sync(&mut self, cx: &mut Context<Self>) {
        // Cheap gate first: every tile's context notifies on unrelated list
        // frames too; the rows only depend on what the revision covers.
        let revision = {
            let s = self.state.read(cx);
            (
                s.transcript_revision(),
                s.selected_chat.clone(),
                self.attachment_device_ids(cx),
                crate::appearance::chat_style::settings(cx).tool_call_limit,
            )
        };
        if self.synced_revision.as_ref() == Some(&revision) {
            return;
        }
        let tool_call_limit = revision.3;
        self.synced_revision = Some(revision);
        let (selected, entries, echoes, steers) = {
            let s = self.state.read(cx);
            let echoes: Vec<(SessionMessageEntry, bool)> = s
                .pending_echoes()
                .iter()
                .map(|echo| (echo.clone(), s.echo_pending(&echo.id)))
                .collect();
            (
                s.selected_chat.clone(),
                s.transcript.clone(),
                echoes,
                s.steer_message_ids(),
            )
        };
        // A replay can briefly contain both an unresolved live part (which
        // drives the composer wizard) and a resolved mirror of the same
        // request (which would otherwise render a second, different-looking
        // question chip). Keep the interactive wizard as the single source of
        // truth while that request is pending.
        let pending_request_id = crate::composer::pending_input_request(&entries).map(|(id, _)| id);

        let attached = selected != self.chat_id;
        if attached {
            self.copied_message = None;
            self.copied_message_clear = None;
            let keep_own_turn = self
                .own_turn
                .as_ref()
                .is_some_and(|anchor| selected.as_deref() == Some(anchor.chat_id.as_str()));
            if !keep_own_turn {
                self.own_turn = None;
                self.own_turn_kick = false;
            }
            // Switching chats discards the transient comment pill/selection.
            self.dismiss_comment_ui_and_selection(cx);
            // … and the find bar with them: its matches, its counter and its
            // query all belonged to the transcript being left behind.
            self.find = None;
            crate::markdown::find::clear(self.scope);
            self.chat_id = selected;
            self.rows.clear();
            self.row_cache.clear();
            self.live_parsers.clear();
            self.tree_cache.clear();
            self.folds.clear();
            self.tool_overflow.clear();
            self.toggle_pins.clear();
            self.veils.clear();
            self.render_cache.borrow_mut().clear();
            self.highlights.entries.clear();
            self.list.reset(0);
            // A kept own-turn hold (send-created chat) owns the viewport;
            // otherwise the fresh attach pins to the bottom.
            self.pinned = self.own_turn.is_none();
            self.spring.reset();
            self.spring_last_tick = None;
            self.spring_settled_at = None;
            self.spring_kick = false;
            self.show_jump_button = false;
        }

        let mut new_rows: Vec<Row> = Vec::new();
        let mut after_slash_command = false;
        for entry in &entries {
            if entry.role == MessageRole::User {
                after_slash_command = user_entry_is_slash_command(entry);
            }
            let mut rows = self.rows_for(entry, false, steers.contains(&entry.id));
            if after_slash_command && entry.role != MessageRole::User {
                rows.retain(|r| !matches!(r.kind, RowKind::InputChip { .. }));
            }
            rows.retain(|r| !is_pending_input_duplicate(r, pending_request_id.as_deref()));
            fold_closed_toggles(&mut rows, &self.toggle_pins);
            cap_work_runs(&mut rows, tool_call_limit, &self.tool_overflow);
            new_rows.extend(rows);
        }
        for (echo, pending) in &echoes {
            if echo.role == MessageRole::User {
                after_slash_command = user_entry_is_slash_command(echo);
            }
            let mut rows = self.rows_for(echo, *pending, steers.contains(&echo.id));
            if after_slash_command && echo.role != MessageRole::User {
                rows.retain(|r| !matches!(r.kind, RowKind::InputChip { .. }));
            }
            rows.retain(|r| !is_pending_input_duplicate(r, pending_request_id.as_deref()));
            new_rows.extend(rows);
        }

        // Text already streamed before this (re)attach is the veil BASELINE:
        // its rows' veils seed instead of fading (render creates them from
        // this set), so only post-switch appends animate. Captured from the
        // first NON-EMPTY transcript after attach — the replay frame — never
        // the attach-time sync, whose transcript is still empty (selection
        // clears it; the doc watch refills it async).
        if attached {
            self.veil_baseline.clear();
            self.veil_attach_pending = true;
        }
        if self.veil_attach_pending && !entries.is_empty() {
            self.veil_attach_pending = false;
            self.veil_baseline = new_rows
                .iter()
                .filter(|r| is_live_markdown(&r.kind))
                .map(|r| r.id.clone())
                .collect();
        }

        // Veils live exactly as long as their live row — drop them on the
        // live→complete flip (any mid-fade chunk snaps to full, matching the
        // row's version splice).
        self.veils.retain(|id, _| {
            new_rows
                .iter()
                .any(|r| &r.id == id && is_live_markdown(&r.kind))
        });
        self.veil_baseline.retain(|id| {
            new_rows
                .iter()
                .any(|r| &r.id == id && is_live_markdown(&r.kind))
        });

        let was_empty = self.rows.is_empty();
        let old_last = self.rows.len().checked_sub(1);
        match diff_rows(&self.rows, &new_rows) {
            None => {
                // Identical ids AND versions: every row's match count is
                // already indexed (the memo is keyed on exactly that pair).
                self.rows = new_rows;
                self.refresh_protected_attachments(cx);
                return;
            }
            Some((old_range, count)) => {
                // A doc commit replaced rows: the shared popup's offer anchors
                // to a replaced row's text — dismiss it (its quote may have
                // streamed/changed under the selection).
                if let Some(popup) = self.comment_popup.upgrade()
                    && popup.read(cx).is_active()
                    && let Some(row) = popup.read(cx).offer_row().map(str::to_owned)
                    && self.rows[old_range.clone()]
                        .iter()
                        .any(|r| r.id.as_ref() == row)
                {
                    self.dismiss_comment_ui_and_selection(cx);
                }
                // Any replaced row's cached flatten results are stale — and
                // because live replies splice only the rows whose content hash
                // changed (the tail), this is O(changed rows) per commit, never
                // O(reply).
                for row in &self.rows[old_range.clone()] {
                    self.render_cache.borrow_mut().invalidate_row(&row.id);
                }
                if old_range.len() == count {
                    // In-place content change, same row count — notably the
                    // live→complete flip, where EVERY row of the streamed
                    // message changes version (streaming bit, tool auto_open,
                    // timestamp bit) with identical ids. `splice` would reset
                    // those items to hint-less Unmeasured (heights read 0
                    // until the next paint) and, when the viewport-top item is
                    // inside the range, clobber the scroll anchor to the range
                    // start — the end-of-turn up/down jump the spring then has
                    // to walk back. `remeasure_items` keeps old sizes as hints
                    // and holds the anchor across the remeasure.
                    self.list.remeasure_items(old_range);
                } else {
                    self.list.splice(old_range, count);
                }
            }
        }
        self.rows = new_rows;
        self.refresh_protected_attachments(cx);
        // Rows moved: re-derive the find counts (memoized per row version, so
        // a streaming commit only rescans its own tail) and hold the active
        // match inside the new total.
        self.reindex_find();
        if self.own_turn.is_some() {
            // Appending a reply moves the runway from the previous last row to
            // the new one. Both measurements must be invalidated because the
            // row diff itself only knows that rows were appended at the tail.
            if let Some(old_last) = old_last.filter(|&ix| ix < self.rows.len()) {
                self.list.remeasure_items(old_last..old_last + 1);
            }
            self.remeasure_last_row();
            self.own_turn_kick = true;
        }
        if self.pinned {
            if motion::reduced_motion(cx) || was_empty {
                // First fill (chat open) lands at the bottom instantly
                // (mugen initialScroll:'bottom'); reduced motion always snaps.
                self.list.scroll_to_end();
            } else if self.is_glued() {
                // A glued offset (`None` / anchored past the end) makes the
                // upcoming layout hard-snap to the new end — the per-commit
                // stutter. Materialize a pixel anchor a hair above the bottom
                // so layout holds position and the spring glides the growth.
                self.list.scroll_by(px(-0.75));
            }
            self.spring_kick = true;
        }
        cx.notify();
    }

    /// Cached row build for one entry (streaming entries bypass the cache).
    fn rows_for(&mut self, entry: &SessionMessageEntry, pending: bool, steer: bool) -> Vec<Row> {
        let mut rows = self.cached_rows_for(entry, pending);
        if steer {
            mark_steer_rows(&mut rows);
        }
        rows
    }

    fn cached_rows_for(&mut self, entry: &SessionMessageEntry, pending: bool) -> Vec<Row> {
        let streaming = entry.status == Some(MessageStatus::Streaming);
        let fingerprint = entry_fingerprint(entry, pending);
        if !streaming
            && let Some(cached) = self.row_cache.get(&entry.id)
            && cached.fingerprint == fingerprint
        {
            return cached.rows.clone();
        }

        let live_parsers = &mut self.live_parsers;
        let tree_cache = &mut self.tree_cache;
        let mut parse = |key: &str, text: &str| -> Arc<BlockTree> {
            // Render-cache invalidation rides on the row diff in `sync` (only
            // rows whose content hash changed are spliced — the reparsed tail).
            parse_for_row(streaming, key, text, live_parsers, tree_cache).0
        };
        let rows = rows_for_entry(entry, pending, &mut parse);

        if !streaming {
            self.row_cache.insert(
                entry.id.clone(),
                CachedRows {
                    fingerprint,
                    rows: rows.clone(),
                },
            );
        }
        rows
    }

    /// Fetch a sidecar blob (full tool output or diff) and build its upgraded
    /// [`ToolDetail`] once, off the render path. Re-entry while Loading/Ready
    /// is a no-op; Failed re-arms as a retry (the affordance label says so).
    pub(super) fn spawn_blob_fetch(&mut self, blob_ref: SharedString, cx: &mut Context<Self>) {
        // Rank BEFORE the already-fetched guard: clicking a Ready ref is the
        // "show me this one again" toggle (recency bump + repaint, no
        // re-fetch) — with both a diff and an output fetched, the two
        // affordances must be able to trade places forever.
        self.blob_fetch_counter += 1;
        self.blob_fetch_order
            .insert(blob_ref.clone(), self.blob_fetch_counter);
        match self.blob_details.get(&blob_ref) {
            Some(BlobFetch::Ready(_)) => {
                cx.notify();
                return;
            }
            Some(BlobFetch::Loading(_)) => return,
            Some(BlobFetch::Failed) | None => {}
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let is_diff = blob_ref.ends_with(".diff");
        let ref_key = blob_ref.clone();
        let task = cx.spawn(async move |this, cx| {
            let reply = crate::attachments::call_with_timeout(
                &engine,
                cx.background_executor(),
                cypher_rpc::methods::FETCH_TOOL_BLOB,
                serde_json::json!({ "blobRef": ref_key.as_ref() }),
                Duration::from_secs(20),
            )
            .await;
            let fetched = match reply {
                Ok(value) => {
                    let text = value
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default();
                    blob_detail(text, is_diff)
                        .map(|d| BlobFetch::Ready(Arc::new(d)))
                        .unwrap_or(BlobFetch::Failed)
                }
                Err(_) => BlobFetch::Failed,
            };
            this.update(cx, |this, cx| {
                this.blob_details.insert(ref_key, fetched);
                cx.notify();
            })
            .ok();
        });
        self.blob_details.insert(blob_ref, BlobFetch::Loading(task));
    }

    pub(super) fn toggle_fold(&mut self, row_id: SharedString, open_height: f32, auto_open: bool) {
        let entry = self.folds.entry(row_id).or_default();
        let currently_open = entry.open.unwrap_or(auto_open);
        entry.from = if currently_open { open_height } else { 0.0 };
        entry.open = Some(!currently_open);
        entry.epoch += 1;
        entry.toggled_at = Some(Instant::now());
    }
}

/// One own-turn step's measurements, shared by its phases.
#[derive(Clone, Copy)]
struct OwnTurnFrame {
    anchor_ix: usize,
    last_ix: usize,
    viewport: gpui::Bounds<gpui::Pixels>,
    viewport_height: f32,
    /// Where the prompt rests below the viewport top.
    inset: f32,
    /// The last row's pad without any reservation.
    base_pad: f32,
    /// Room the reservation may fill.
    usable: f32,
    /// The reservation already installed.
    current: f32,
}

#[cfg(test)]
mod tests;
