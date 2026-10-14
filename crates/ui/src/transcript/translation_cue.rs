//! Switching a translated message between its translation and the words the
//! model wrote: hold Shift over it and a glass toggle button appears under
//! the pointer; click it (or Shift-click the message). The version that
//! comes in fades up into place.
//!
//! The button waits for a deliberate hold. Shift is pressed constantly while
//! typing, often with the pointer resting on the transcript, so a lone Shift
//! has to stay down for [`CUE_DELAY`] with no other key typed before it
//! appears.
//!
//! Shift is read by polling the window's modifiers rather than from
//! modifier events: those only travel the focused element's path, and the
//! composer — not the transcript — holds focus while the user reads. The poll
//! runs only while the pointer is over a translated message or the button is
//! up.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, Point,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, point, px,
};

use crate::kit::icons;
use crate::kit::motion;
use crate::kit::theme::Theme;

use super::{Row, Transcript};

/// How long a lone Shift has to be held before the button appears.
pub(super) const CUE_DELAY: Duration = Duration::from_millis(350);
/// How often Shift is read while the pointer is over a translated message.
const POLL: Duration = Duration::from_millis(40);
/// How long a switched message's rows keep their entrance.
const SWAP_WINDOW: Duration = Duration::from_millis(400);
/// The button's height; the pointer lands on its icon.
const BUTTON_HEIGHT: f32 = 30.0;
/// From the button's left edge to the middle of its icon.
const ICON_CENTER: f32 = 19.0;
/// How far the pointer may stray from where the button appeared before it
/// goes, unless it is still over the message.
const NEAR: f32 = 90.0;
/// The button's entrance.
const ENTRANCE: motion::MotionSpec = motion::MotionSpec::new(220, motion::EASE_OUT_EXPO);
/// The lens's backdrop blur: light, as the slider thumb's — the veil does
/// most of the hiding. It rides the entrance, because the blur ignores
/// element opacity and would otherwise pop in at full strength.
const GLASS_BLUR: f32 = 2.5;

/// The toggle button, where the pointer was when it appeared.
struct CueButton {
    entry: SharedString,
    at: Point<Pixels>,
    since: Instant,
    id: u64,
}

pub(super) struct ShiftCue {
    /// When a lone Shift was first seen held; `None` while it is up.
    since: Option<Instant>,
    /// When a key other than a modifier last went down, from a keystroke
    /// interceptor (it sees keys the composer consumes, unlike an observer).
    last_key: Rc<Cell<Option<Instant>>>,
    button: Option<CueButton>,
    polling: bool,
    /// Messages just switched: a generation (so a second switch replays the
    /// entrance) and when.
    swapped: HashMap<SharedString, (u64, Instant)>,
    generation: u64,
    _keys: Subscription,
}

impl ShiftCue {
    pub(super) fn new(cx: &mut Context<Transcript>) -> Self {
        let last_key: Rc<Cell<Option<Instant>>> = Rc::default();
        let seen = last_key.clone();
        let keys = cx.intercept_keystrokes(move |event, _, _| {
            if !is_modifier(&event.keystroke.key) {
                seen.set(Some(Instant::now()));
            }
        });
        Self {
            since: None,
            last_key,
            button: None,
            polling: false,
            swapped: HashMap::new(),
            generation: 0,
            _keys: keys,
        }
    }

    pub(super) fn clear(&mut self) {
        self.button = None;
        self.swapped.clear();
    }

    /// The entrance generation for `entry`'s rows, while it is fresh.
    fn swap_generation(&self, entry: &SharedString, now: Instant) -> Option<u64> {
        self.swapped
            .get(entry)
            .filter(|(_, at)| now.duration_since(*at) < SWAP_WINDOW)
            .map(|(generation, _)| *generation)
    }
}

fn is_modifier(key: &str) -> bool {
    matches!(
        key,
        "shift" | "control" | "alt" | "platform" | "function" | "capslock"
    )
}

/// Whether a lone Shift held since `since` has earned the button at `now`:
/// held for [`CUE_DELAY`], and no key typed during the hold — or just before
/// it was seen, since the poll notices a press up to one interval late.
pub(super) fn cue_due(since: Instant, last_key: Option<Instant>, now: Instant) -> bool {
    let typed = last_key.is_some_and(|key| key + POLL >= since);
    !typed && now.duration_since(since) >= CUE_DELAY
}

fn distance(a: Point<Pixels>, b: Point<Pixels>) -> f32 {
    let (dx, dy) = (f32::from(a.x - b.x), f32::from(a.y - b.y));
    (dx * dx + dy * dy).sqrt()
}

impl Transcript {
    /// Start reading Shift while the pointer is over a translated message.
    pub(super) fn watch_shift(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shift_cue.polling {
            return;
        }
        self.shift_cue.polling = true;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let keep = this
                    .update_in(cx, |this, window, cx| this.poll_shift(window, cx))
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    /// One read of Shift; `false` once there is nothing left to watch.
    fn poll_shift(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        let modifiers = window.modifiers();
        let lone_shift = modifiers.shift
            && !modifiers.platform
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.function;
        let mouse = window.mouse_position();
        let hovered = self.hovered_translated_entry();
        let cue = &mut self.shift_cue;
        cue.since = if lone_shift {
            Some(cue.since.unwrap_or(now))
        } else {
            None
        };
        match &cue.button {
            // Over the button the message is no longer hovered (the button
            // covers it), so nearness keeps it as well as the message does.
            Some(button) => {
                let stays = lone_shift
                    && (hovered.as_ref() == Some(&button.entry)
                        || distance(mouse, button.at) <= NEAR);
                if !stays {
                    cue.button = None;
                    cx.notify();
                }
            }
            None => {
                let due = cue
                    .since
                    .is_some_and(|since| cue_due(since, cue.last_key.get(), now));
                if let Some(entry) = hovered.clone().filter(|_| due) {
                    cue.generation += 1;
                    cue.button = Some(CueButton {
                        entry,
                        at: mouse,
                        since: now,
                        id: cue.generation,
                    });
                    cx.notify();
                }
            }
        }
        cue.polling = hovered.is_some() || cue.button.is_some();
        cue.polling
    }

    /// The message under the pointer, if the row under it is translated. A
    /// row a switch just replaced still counts until the pointer moves on:
    /// the message is still under it, now showing its other version.
    fn hovered_translated_entry(&self) -> Option<SharedString> {
        let (row_id, entry) = self.hovered_entry.as_ref()?;
        let translated = match self.rows.iter().find(|row| &row.id == row_id) {
            Some(row) => row.translation.is_some(),
            None => self
                .rows
                .iter()
                .any(|row| &row.entry_id == entry && row.translation.is_some()),
        };
        translated.then(|| entry.clone())
    }

    /// Show a translated message as the model wrote it, or its translation
    /// again ([`super::show_translation_side`]).
    pub(super) fn toggle_original(&mut self, entry_id: &SharedString, cx: &mut Context<Self>) {
        if !self.show_original.remove(entry_id) {
            self.show_original.insert(entry_id.clone());
        }
        let cue = &mut self.shift_cue;
        cue.generation += 1;
        cue.swapped
            .insert(entry_id.clone(), (cue.generation, Instant::now()));
        // A local change: rebuild despite an unchanged state revision.
        self.synced_revision = None;
        self.sync(cx);
        cx.notify();
    }

    /// A translated row's body as a switch brings it in: it fades up into
    /// place. Every other row renders untouched.
    pub(super) fn swap_entrance(&self, row: &Row, inner: AnyElement) -> AnyElement {
        if row.translation.is_none() {
            return inner;
        }
        match self
            .shift_cue
            .swap_generation(&row.entry_id, Instant::now())
        {
            Some(generation) => motion::fade_in(
                SharedString::from(format!("{}-swap-{generation}", row.id)),
                div().w_full().min_w_0().child(inner),
            )
            .into_any_element(),
            None => inner,
        }
    }

    /// The glass toggle under the pointer, while Shift is held.
    pub(super) fn translation_button(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let button = self.shift_cue.button.as_ref()?;
        let theme = crate::appearance::chat_style::theme(cx);
        let raw = button.since.elapsed().as_secs_f32() / ENTRANCE.total().as_secs_f32();
        let t = if motion::reduced_motion(cx) || raw >= 1.0 {
            1.0
        } else {
            window.request_animation_frame();
            ENTRANCE.progress(raw)
        };
        let original = self.show_original.contains(&button.entry);
        let entry = button.entry.clone();
        let pill = glass_pill(&theme, button.id, original, t).on_click(cx.listener(
            move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_original(&entry, cx);
            },
        ));
        let origin = point(
            button.at.x - px(ICON_CENTER),
            button.at.y - px(BUTTON_HEIGHT / 2.0),
        );
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(origin)
                    .child(div().opacity(t).mt(px(6.0 * (1.0 - t))).child(pill)),
            )
            .priority(2)
            .into_any_element(),
        )
    }
}

/// The button itself, in the glass of the sidebar's last-active slider thumb
/// while it is held (`render_sidebar_time_slider`): a mostly clear lens — a
/// thin veil of the menu surface over a light backdrop blur — with a bright
/// rim, light caught on the upper edge, a refraction shade on the lower one,
/// a soft inner glow and a top sheen, floating on a wide soft shadow. The
/// translate glyph and what a click will show sit on top, crisp. `t` is the
/// entrance: the blur and the glass's light come up with it.
fn glass_pill(theme: &Theme, id: u64, original: bool, t: f32) -> gpui::Stateful<gpui::Div> {
    let dark = theme.appearance.is_dark();
    let white = gpui::white();
    let veil = theme.surface_overlay;
    let (rest, hover) = if dark { (0.55, 0.66) } else { (0.62, 0.74) };
    let shadow = |color: Hsla, y: f32, blur: f32, inset: bool| gpui::BoxShadow {
        color,
        offset: point(px(0.0), px(y)),
        blur_radius: px(blur),
        spread_radius: px(0.0),
        inset,
    };
    let group = SharedString::from(format!("translation-toggle-{id}"));
    let body = div()
        .absolute()
        .inset_0()
        .rounded_full()
        .bg(veil.opacity(rest))
        .group_hover(group.clone(), move |style| style.bg(veil.opacity(hover)));
    // Over the blur: the rim and the sheen stay crisp.
    let rim = div()
        .absolute()
        .inset_0()
        .rounded_full()
        .border_1()
        .border_color(white.opacity(if dark { 0.35 } else { 0.85 }))
        .shadow(vec![
            shadow(white.opacity(0.95 * t), 1.0, 1.5, true),
            shadow(gpui::black().opacity(0.10 * t), -1.0, 2.0, true),
            shadow(white.opacity(0.55 * t), 0.0, 4.0, true),
        ])
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded_full()
                .opacity(t)
                .bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(white.opacity(0.35), 0.0),
                    gpui::linear_color_stop(white.opacity(0.0), 0.55),
                )),
        );
    // Paint order as the thumb's: body, then the blur over it (and the
    // transcript beneath), then the rim — and the label last.
    div()
        .id(("translation-toggle", id as usize))
        .group(group)
        .occlude()
        .cursor_pointer()
        .relative()
        .h(px(BUTTON_HEIGHT))
        .rounded_full()
        .overflow_hidden()
        .shadow(vec![shadow(
            gpui::black().opacity(0.16 * t),
            3.0,
            10.0,
            false,
        )])
        .child(body)
        .child(crate::kit::frost::frosted_always(
            BUTTON_HEIGHT / 2.0,
            GLASS_BLUR * t,
            rim,
        ))
        .child(
            div()
                .relative()
                .h_full()
                .pl(px(ICON_CENTER - 7.0))
                .pr(px(13.0))
                .flex()
                .items_center()
                .gap(px(7.0))
                .text_size(px(12.5))
                .text_color(theme.text)
                .child(
                    icons::icon(icons::GLOBAL)
                        .size(px(14.0))
                        .text_color(theme.text),
                )
                .child(SharedString::from(if original {
                    "Show translation"
                } else {
                    "Show original"
                })),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_waits_for_a_deliberate_hold() {
        let since = Instant::now();
        let later = |ms| since + Duration::from_millis(ms);
        assert!(!cue_due(since, None, later(100)), "a passing Shift");
        assert!(cue_due(since, None, later(350)));
        // Typing while Shift is down is typing, however long it is held.
        assert!(!cue_due(since, Some(later(200)), later(1_000)));
        // A key typed just before the poll saw Shift counts as during it.
        assert!(!cue_due(
            since,
            Some(since - Duration::from_millis(20)),
            later(1_000)
        ));
        // Typing that ended before Shift went down does not.
        assert!(cue_due(
            since,
            Some(since - Duration::from_millis(500)),
            later(400)
        ));
    }
}
