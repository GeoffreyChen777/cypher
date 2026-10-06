//! Wheel input against a real [`Transcript`] in a test window: layout, the
//! list's wheel path, the spring and the own-turn hold all run for real, one
//! simulated frame at a time.
use super::*;
use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase, VisualTestContext, point};

fn entry(id: &str, role: MessageRole, text: &str, status: MessageStatus) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role,
        parts: vec![MessagePart::Text {
            id: format!("{id}#t"),
            text: text.into(),
            agent_text: None,
        }],
        created_at: 0,
        device_id: "dev".into(),
        status: Some(status),
        continuation_of: None,
        completed_at: None,
        comments: Vec::new(),
    }
}

fn user(id: &str) -> SessionMessageEntry {
    entry(id, MessageRole::User, "a question", MessageStatus::Complete)
}

fn reply(id: &str, paragraphs: usize, status: MessageStatus) -> SessionMessageEntry {
    let text = (0..paragraphs)
        .map(|i| format!("Paragraph {i} of a reply with enough words in it to wrap the line."))
        .collect::<Vec<_>>()
        .join("\n\n");
    entry(id, MessageRole::Assistant, &text, status)
}

fn history(turns: usize) -> Vec<SessionMessageEntry> {
    (0..turns)
        .flat_map(|i| {
            [
                user(&format!("u{i}")),
                reply(&format!("a{i}"), 3, MessageStatus::Complete),
            ]
        })
        .collect()
}

struct Rig {
    state: Entity<AppState>,
    transcript: Entity<Transcript>,
    visual: VisualTestContext,
}

impl Rig {
    fn new(cx: &mut gpui::TestAppContext, entries: Vec<SessionMessageEntry>) -> Self {
        let state = cx.update(|cx| {
            cx.set_global(Theme::for_appearance(crate::theme::Appearance::Dark));
            let state = cx.new(|_| AppState::new());
            state.update(cx, |s, _| {
                s.selected_chat = Some("chat".into());
                s.set_transcript(entries);
            });
            state
        });
        let window = cx.open_window(gpui::size(px(800.0), px(600.0)), |_, cx| {
            Transcript::new(state.clone(), gpui::WeakEntity::new_invalid(), cx)
        });
        let transcript = window.root(cx).unwrap();
        let mut rig = Rig {
            state,
            transcript,
            visual: VisualTestContext::from_window(window.into(), cx),
        };
        rig.transcript.update(&mut rig.visual, |t, cx| {
            t.sync(cx);
            t.set_viewport_size(800.0, 600.0, cx);
        });
        rig.frames(30);
        rig
    }

    /// One display-link tick: the frame callbacks (spring / own-turn step),
    /// then the draw that lays their result out.
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.visual.update(|w, cx| {
                w.simulate_next_frame(cx);
            });
            self.visual.run_until_parked();
            self.visual.update(|w, cx| {
                w.refresh();
                w.draw(cx).clear();
            });
            self.visual.run_until_parked();
        }
    }

    /// Real-time frames until the own-send glide (wall-clock eased) lands the
    /// prompt at its hold.
    fn land_hold(&mut self) {
        for _ in 0..300 {
            let positioned = self.transcript.read_with(&self.visual, |t, _| {
                t.own_turn.as_ref().is_some_and(|a| a.held && a.positioned)
            });
            if positioned {
                self.frames(2);
                return;
            }
            std::thread::sleep(Duration::from_millis(16));
            self.frames(1);
        }
        panic!("the own-send glide never landed");
    }

    fn set(&mut self, entries: Vec<SessionMessageEntry>) {
        self.state.update(&mut self.visual, |s, cx| {
            s.set_transcript(entries);
            cx.notify();
        });
    }

    fn own_send(&mut self, entries: &mut Vec<SessionMessageEntry>, id: &str) {
        entries.push(user(id));
        self.set(entries.clone());
        self.transcript.update(&mut self.visual, |t, cx| {
            t.on_own_send("chat".into(), id.into(), cx)
        });
    }

    /// `dy > 0` scrolls toward older content (away from the bottom).
    fn wheel(&mut self, dy: f32) {
        self.visual.simulate_event(ScrollWheelEvent {
            position: point(px(400.0), px(300.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
            modifiers: Default::default(),
            touch_phase: TouchPhase::Moved,
        });
        self.visual.run_until_parked();
    }

    /// A row crossing the viewport's middle and its top relative to the
    /// viewport — a position layout can't renumber under us the way the
    /// list's summed pixel offset can (rows above measure in lazily).
    fn probe(&mut self) -> Option<(usize, f32)> {
        self.transcript.read_with(&self.visual, |t, _| {
            let vp = t.list.viewport_bounds();
            let mid = f32::from(vp.top()) + f32::from(vp.size.height) / 2.0;
            (0..t.rows.len()).find_map(|ix| {
                let b = t.list.bounds_for_item(ix)?;
                (f32::from(b.bottom()) > mid).then(|| (ix, f32::from(b.top() - vp.top())))
            })
        })
    }

    fn row_top(&mut self, ix: usize) -> Option<f32> {
        self.transcript.read_with(&self.visual, |t, _| {
            let vp = t.list.viewport_bounds();
            t.list
                .bounds_for_item(ix)
                .map(|b| f32::from(b.top() - vp.top()))
        })
    }

    fn at_top(&mut self) -> bool {
        self.transcript.read_with(&self.visual, |t, _| {
            let top = t.list.logical_scroll_top();
            top.item_ix == 0 && f32::from(top.offset_in_item) < 40.0
        })
    }

    fn held(&mut self) -> bool {
        self.transcript.read_with(&self.visual, |t, _| {
            t.own_turn.as_ref().is_some_and(|a| a.held)
        })
    }

    /// Wheel up by `dy` and report how far the content moved down on screen
    /// (`None`: the probed row scrolled out of view — it moved a lot).
    fn wheel_up_moves(&mut self, dy: f32) -> Option<f32> {
        let (ix, before) = self.probe().expect("a visible row");
        self.wheel(dy);
        self.frames(1);
        self.row_top(ix).map(|after| after - before)
    }
}

#[gpui::test]
fn wheel_up_releases_the_hold_while_the_reply_streams(cx: &mut gpui::TestAppContext) {
    let mut entries = history(10);
    let mut rig = Rig::new(cx, entries.clone());
    rig.own_send(&mut entries, "sent");
    rig.land_hold();
    // Each commit grows the reply a frame before the reservation pad shrinks
    // to match — the window a wheel used to be measured against a stale
    // bottom distance and snapped straight back to the hold.
    for paragraphs in 1..=4 {
        let mut live = entries.clone();
        live.push(reply("answer", paragraphs, MessageStatus::Streaming));
        rig.set(live);
        rig.frames(1);
    }
    let moved = rig.wheel_up_moves(30.0);
    assert!(moved.is_none_or(|d| d > 25.0), "content moved {moved:?}");
    assert!(!rig.held());
}

#[gpui::test]
fn a_slow_trackpad_drag_releases_the_hold(cx: &mut gpui::TestAppContext) {
    let mut entries = history(10);
    let mut rig = Rig::new(cx, entries.clone());
    rig.own_send(&mut entries, "sent");
    entries.push(reply("answer", 2, MessageStatus::Complete));
    rig.set(entries.clone());
    rig.land_hold();
    // Sub-pixel deltas: each one alone is below any size threshold.
    let (ix, before) = rig.probe().unwrap();
    for _ in 0..40 {
        rig.wheel(0.5);
        rig.frames(1);
    }
    let after = rig.row_top(ix).unwrap();
    assert!(after > before + 15.0, "{before} -> {after}");
    assert!(!rig.held());
}

#[gpui::test]
fn wheel_down_at_the_hold_is_a_hard_stop(cx: &mut gpui::TestAppContext) {
    let mut entries = history(10);
    let mut rig = Rig::new(cx, entries.clone());
    rig.own_send(&mut entries, "sent");
    entries.push(reply("answer", 2, MessageStatus::Complete));
    rig.set(entries.clone());
    rig.land_hold();
    let (ix, before) = rig.probe().unwrap();
    for _ in 0..5 {
        rig.wheel(-40.0);
        rig.frames(1);
    }
    let after = rig.row_top(ix).unwrap();
    assert!((after - before).abs() <= 1.0, "{before} -> {after}");
    assert!(rig.held());
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// Random interleavings of sends, streaming commits, completions, frames,
/// small wheels, chrome resizes and jumps, probed with 30px wheel-ups that
/// must move the content. Returns the op log tail when three probes in a row
/// leave it where it was.
fn wedges(cx: &mut gpui::TestAppContext, seed: u64) -> Option<String> {
    let mut rng = Rng(seed);
    let mut entries = history(6 + rng.below(10) as usize);
    let mut rig = Rig::new(cx, entries.clone());
    let mut log = Vec::new();
    let mut turn = 0;
    let mut streaming: Option<(String, usize)> = None;
    let mut stuck = 0;
    for step in 0..400 {
        match rng.below(14) {
            0 if streaming.is_none() => {
                turn += 1;
                let id = format!("un{turn}");
                if rng.below(2) == 0 {
                    rig.own_send(&mut entries, &id);
                    log.push(format!("{step}: own send"));
                } else {
                    entries.push(user(&id));
                    rig.set(entries.clone());
                    log.push(format!("{step}: remote send"));
                }
                streaming = Some((format!("an{turn}"), 0));
            }
            1 | 2 => {
                if let Some((id, n)) = streaming.as_mut() {
                    *n += 1 + rng.below(3) as usize;
                    let mut live = entries.clone();
                    live.push(reply(id, *n, MessageStatus::Streaming));
                    rig.set(live);
                    log.push(format!("{step}: stream {n}"));
                }
            }
            3 => {
                if let Some((id, n)) = streaming.take() {
                    entries.push(reply(&id, n.max(1), MessageStatus::Complete));
                    rig.set(entries.clone());
                    log.push(format!("{step}: complete"));
                }
            }
            4..=6 => {
                let n = 1 + rng.below(6) as usize;
                rig.frames(n);
                log.push(format!("{step}: frames {n}"));
            }
            7 => {
                let dy = -(1.0 + rng.below(80) as f32);
                rig.wheel(dy);
                log.push(format!("{step}: wheel {dy}"));
            }
            8 => {
                let dy = 0.3 + rng.below(40) as f32 / 10.0;
                rig.wheel(dy);
                log.push(format!("{step}: wheel {dy}"));
            }
            9 => {
                let clearance = if rng.below(2) == 0 { 120.0 } else { 180.0 };
                rig.transcript.update(&mut rig.visual, |t, cx| {
                    t.set_bottom_clearance(clearance, cx)
                });
                log.push(format!("{step}: clearance {clearance}"));
            }
            10 => {
                rig.transcript
                    .update(&mut rig.visual, |t, cx| t.jump_to_bottom(cx));
                log.push(format!("{step}: jump"));
            }
            _ => {
                rig.frames(1);
                if rig.probe().is_none() {
                    continue;
                }
                let at_top = rig.at_top();
                let moved = rig.wheel_up_moves(30.0);
                log.push(format!("{step}: probe wheel 30 moved {moved:?}"));
                if !at_top && moved.is_some_and(|d| d <= 5.0) {
                    stuck += 1;
                    if stuck == 3 {
                        let tail = log[log.len().saturating_sub(30)..].join("\n");
                        return Some(format!("seed {seed} wedged:\n{tail}"));
                    }
                } else {
                    stuck = 0;
                }
            }
        }
    }
    None
}

#[gpui::test]
fn interleaved_input_never_wedges_the_wheel(cx: &mut gpui::TestAppContext) {
    // Seeds that wedged the distance-baseline escape, plus a few fresh ones.
    for seed in [
        79191, 316761, 340518, 736468, 855253, 1053228, 1148256, 1, 2, 3,
    ] {
        if let Some(report) = wedges(cx, seed) {
            panic!("{report}");
        }
    }
}
