//! The working trailer's progress bar (a long turn's narrated progress, from
//! the engine's `session::progress`): the painted bar
//! ([`crate::kit::progress_bar`]) with a hover target over each stage dot,
//! the current stage's tip above its dot with how many steps are left, the
//! steps still to come as dots between the fill and the finish, and every
//! other dot's tip on hover. One tip shows at a time: the transcript keeps
//! which dot the pointer is on ([`HoverListener`]), and the current stage's
//! resting tip steps aside while another dot has it. Tips sit on the chat's own background, lifted by a soft shadow.
//!
//! Motion: when a reading lands the fill glides to its new percentage and a
//! new stage's dot grows in; refreshed upcoming steps glide to their new
//! places; the old stage's tip lifts away as the new one
//! rises into place ([`ProgressMotion`] notices both). The glint and the
//! current ring's breathing run continuously in the bar itself.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Div, EntityId, Hsla, Pixels, SharedString, Window, div, prelude::*, px, relative,
};

use cypher_proto::RunProgress;

use crate::kit::loaders;
use crate::kit::motion::{
    AnimationExt as _, PROGRESS_TIP_IN, PROGRESS_TIP_OUT, PROGRESS_TWEEN, lerp,
};
use crate::kit::progress_bar::{HEIGHT, INSET, ProgressBar};
use crate::kit::theme::Theme;

/// Space the trailer keeps above the bar for the current stage's tip.
/// Space the trailer keeps above the bar: the resting tip (bubble and
/// pointer, about 26px above the bar) plus a clear gap to the chat content.
pub(super) const TIP_ROOM: f32 = 44.0;
/// Width of a dot's hover target.
const HIT: f32 = 16.0;
/// How far a tip's bottom sits above the track.
const TIP_LIFT: f32 = HEIGHT / 2.0 + 9.0;
/// Distance from the bubble's anchored edge to the pointer's centre.
const POINTER_INSET: f32 = 12.0;
const POINTER_W: f32 = 9.0;
const POINTER_H: f32 = 5.0;
/// How long a left stage's tip keeps rendering while it leaves.
const LEAVE_FOR: Duration = Duration::from_millis(600);

/// Most upcoming-step dots a bar shows.
const MAX_UPCOMING: usize = crate::kit::progress_bar::MAX_UPCOMING;

/// What changed between the readings a transcript has shown, so the bar can
/// animate the change rather than jump.
#[derive(Debug, Default)]
pub(super) struct ProgressMotion {
    chat_id: Option<String>,
    stages: usize,
    percent: u8,
    upcoming: Vec<f32>,
    /// Where the fill glides from to `percent`.
    from_percent: u8,
    /// Where the upcoming dots glide from to `upcoming`.
    from_upcoming: Vec<f32>,
    /// The last change added a stage (its dot grows in).
    grew: bool,
    switched_at: Option<Instant>,
    /// Bumped on every change: the tween's key.
    revision: usize,
}

impl ProgressMotion {
    /// Note the reading on screen. A chat seen for the first time fills in
    /// from empty, its upcoming dots spreading out from the finish.
    pub fn observe(
        &mut self,
        chat_id: &str,
        stages: usize,
        percent: u8,
        upcoming: &[f32],
        now: Instant,
    ) {
        if self.chat_id.as_deref() != Some(chat_id) {
            *self = Self {
                chat_id: Some(chat_id.to_owned()),
                stages,
                percent,
                upcoming: upcoming.to_vec(),
                revision: self.revision + 1,
                ..Self::default()
            };
            return;
        }
        if stages == self.stages && percent == self.percent && upcoming == self.upcoming {
            return;
        }
        self.from_percent = self.percent;
        self.from_upcoming = std::mem::replace(&mut self.upcoming, upcoming.to_vec());
        self.grew = stages > self.stages;
        if self.grew {
            self.switched_at = Some(now);
        }
        self.stages = stages;
        self.percent = percent;
        self.revision += 1;
    }

    /// The stage whose tip is leaving, while it still is.
    pub fn leaving(&self, now: Instant) -> Option<usize> {
        let at = self.switched_at?;
        (self.grew && self.stages >= 2 && now.duration_since(at) < LEAVE_FOR)
            .then(|| self.stages - 2)
    }

    /// The upcoming dots `t` of the way through the tween: each glides from
    /// where it was, a new one in from the finish.
    fn upcoming_at(&self, t: f32) -> Vec<f32> {
        self.upcoming
            .iter()
            .enumerate()
            .map(|(ix, &to)| lerp(self.from_upcoming.get(ix).copied().unwrap_or(1.0), to, t))
            .collect()
    }
}

/// How many steps remain after the current one: the listed steps, or the
/// model's count when it names more.
pub(super) fn steps_left(progress: &RunProgress) -> usize {
    usize::from(progress.steps_left.unwrap_or(0)).max(progress.next.len())
}

/// Where each stage's dot sits, 0–1: where it began, shrunk in proportion
/// when the reading has since gone back below it (an engine that kept
/// stages ahead of the fill), so no stage sits ahead of the fill.
pub(super) fn stage_positions(stages: &[u8], percent: u8) -> Vec<f32> {
    let fill = f32::from(percent.min(100)) / 100.0;
    let at: Vec<f32> = stages
        .iter()
        .map(|p| f32::from((*p).min(100)) / 100.0)
        .collect();
    let reach = at.iter().copied().fold(0.0, f32::max);
    if reach <= fill {
        return at;
    }
    at.iter().map(|a| a * fill / reach).collect()
}

/// Where each of `count` upcoming steps begins, 0–1: the stretch between the
/// fill's end and the finish shared evenly between the current step and
/// those after it. Capped at [`MAX_UPCOMING`] dots.
pub(super) fn upcoming_positions(percent: u8, count: usize) -> Vec<f32> {
    let from = f32::from(percent.min(100)) / 100.0;
    let shown = count.min(MAX_UPCOMING);
    let share = (1.0 - from) / (shown + 1) as f32;
    (1..=shown).map(|n| from + share * n as f32).collect()
}

/// "3 steps left", for the current step's tip.
fn left_note(left: usize) -> String {
    match left {
        0 => "last step".to_owned(),
        1 => "1 step left".to_owned(),
        n => format!("{n} steps left"),
    }
}

/// Makes the hover listener for the dot keyed `key`: it records in the
/// transcript whether the pointer is on that dot.
pub(super) type HoverListener<'a> = &'a dyn Fn(SharedString) -> HoverHandler;
/// A dot's hover handler.
pub(super) type HoverHandler = Box<dyn Fn(&bool, &mut Window, &mut gpui::App)>;

/// The bar for `progress`, filling what its row leaves. `hovered` is the key
/// of the dot the pointer is on, if any.
#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    progress: &RunProgress,
    motion: &ProgressMotion,
    theme: &Theme,
    chat_bg: Hsla,
    hovered: Option<&str>,
    on_hover: HoverListener,
    view: EntityId,
    now: Instant,
) -> AnyElement {
    let mut labels: Vec<(String, u8)> = progress
        .stages
        .iter()
        .map(|s| (s.label.clone(), s.percent))
        .collect();
    // A reading from a host that keeps no stages still shows its current one.
    if labels.is_empty() {
        labels.push((progress.current.clone(), progress.percent));
    }
    let percent = progress.percent.min(100);
    let fractions = stage_positions(&labels.iter().map(|(_, p)| *p).collect::<Vec<_>>(), percent);
    let last = labels.len() - 1;
    let left = steps_left(progress);
    let upcoming = upcoming_positions(percent, left);
    let (from, to) = (
        f32::from(motion.from_percent.min(100)) / 100.0,
        f32::from(percent) / 100.0,
    );
    let grew = motion.grew;
    let ink = theme.text;
    let look = TipLook::new(theme, chat_bg);
    let painted = {
        let fractions = fractions.clone();
        let glide: Vec<Vec<f32>> = [0.0, 1.0].map(|t| motion.upcoming_at(t)).into();
        div().absolute().inset_0().with_animation(
            ("progress-bar", motion.revision),
            PROGRESS_TWEEN.animation(),
            move |el, t| {
                let appear = if grew { (t * 1.4).min(1.0) } else { 1.0 };
                let upcoming: Vec<f32> = glide[0]
                    .iter()
                    .zip(&glide[1])
                    .map(|(&a, &b)| lerp(a, b, t))
                    .collect();
                el.child(loaders::progress_bar(
                    ProgressBar::new(lerp(from, to, t), &fractions, &upcoming, appear, ink),
                    view,
                ))
            },
        )
    };
    let mut bar = div()
        .relative()
        .flex_1()
        .min_w(px(64.0))
        .h(px(HEIGHT))
        .child(painted);

    let target = |key: String, at: f32| {
        let group = SharedString::from(key);
        let target = div()
            .id(group.clone())
            .on_hover(on_hover(group.clone()))
            .group(group.clone())
            .absolute()
            .top_0()
            .left(relative(at))
            .ml(px(INSET - at * 2.0 * INSET - HIT / 2.0))
            .w(px(HIT))
            .h(px(HEIGHT));
        (target, group)
    };
    // A soft ring marks a hovered dot.
    let hover_ring = |group: &SharedString| {
        div()
            .absolute()
            .left(px(HIT / 2.0 - 6.5))
            .top(px(HEIGHT / 2.0 - 6.5))
            .size(px(13.0))
            .rounded_full()
            .border_1()
            .border_color(ink.opacity(0.0))
            .group_hover(group.clone(), |s| s.border_color(ink.opacity(0.22)))
    };

    // The current stage first: every other dot's tip, shown on hover, then
    // paints over the current one's.
    let at = fractions[last];
    let anchor_left = at <= 0.5;
    let label = labels[last].0.clone();
    let note = format!("{percent}% · {}", left_note(left));
    let (mut current, group) = target(format!("progress-stage-{last}"), at);
    // Its label and how many steps are left always, rising in when the
    // stage is new; on hover, a fuller tip listing every step to come.
    let resting = tip(
        vec![tip_line(&label, Some(note.clone()), &look)],
        anchor_left,
        &look,
    )
    .group_hover(group.clone(), |s| s.opacity(0.0))
    .with_animation(
        ("progress-tip-in", last),
        PROGRESS_TIP_IN.animation(),
        |el, t| el.opacity(t).bottom(px(TIP_LIFT - 5.0 * (1.0 - t))),
    );
    // One tip at a time: the resting tip steps aside while the pointer is
    // on another dot (one that is still on the bar).
    let others: Vec<String> = (0..last)
        .map(|ix| format!("progress-stage-{ix}"))
        .chain((0..upcoming.len()).map(|n| format!("progress-next-{n}")))
        .collect();
    let elsewhere = hovered.is_some_and(|key| others.iter().any(|other| other == key));
    let resting = if elsewhere {
        div()
            .absolute()
            .inset_0()
            .opacity(0.0)
            .child(resting)
            .into_any_element()
    } else {
        resting.into_any_element()
    };
    current = current.child(resting);
    let mut lines = vec![tip_line(&label, Some(note), &look)];
    for (n, step) in progress.next.iter().enumerate() {
        lines.push(
            div()
                .flex()
                .flex_row()
                .gap(px(6.0))
                .text_color(look.muted)
                .child(
                    div()
                        .text_color(look.faint)
                        .child(SharedString::from(format!("{}", n + 1))),
                )
                .child(SharedString::from(step.clone()))
                .into_any_element(),
        );
    }
    if left > progress.next.len() {
        lines.push(
            div()
                .text_color(look.faint)
                .child(SharedString::from(format!(
                    "and {} more",
                    left - progress.next.len()
                )))
                .into_any_element(),
        );
    }
    current = current.child(
        tip(lines, anchor_left, &look)
            .opacity(0.0)
            .group_hover(group, |s| s.opacity(1.0)),
    );
    bar = bar.child(current);

    // Passed stages: their label on hover; the one just left lifts away.
    let leaving = motion.leaving(now);
    for ix in 0..last {
        let at = fractions[ix];
        let label = labels[ix].0.clone();
        let (mut dot, group) = target(format!("progress-stage-{ix}"), at);
        dot = dot.child(hover_ring(&group)).child(
            tip(vec![tip_line(&label, None, &look)], at <= 0.5, &look)
                .opacity(0.0)
                .group_hover(group, |s| s.opacity(1.0)),
        );
        if leaving == Some(ix) {
            dot = dot.child(
                tip(vec![tip_line(&label, None, &look)], at <= 0.5, &look).with_animation(
                    ("progress-tip-out", ix),
                    PROGRESS_TIP_OUT.animation(),
                    |el, t| el.opacity(1.0 - t).bottom(px(TIP_LIFT + 4.0 * t)),
                ),
            );
        }
        bar = bar.child(dot);
    }

    // Steps still to come: each one's name and place on hover.
    for (n, &at) in upcoming.iter().enumerate() {
        let label = progress
            .next
            .get(n)
            .cloned()
            .unwrap_or_else(|| "A later step".to_owned());
        let (dot, group) = target(format!("progress-next-{n}"), at);
        bar = bar.child(
            dot.child(hover_ring(&group)).child(
                tip(
                    vec![tip_line(
                        &label,
                        Some(format!("{} of {left}", n + 1)),
                        &look,
                    )],
                    at <= 0.5,
                    &look,
                )
                .opacity(0.0)
                .group_hover(group, |s| s.opacity(1.0)),
            ),
        );
    }
    bar.into_any_element()
}

/// A tip's colours: the chat's background, opaque, with the theme's text
/// tones on it and a soft shadow to lift it off the chat.
struct TipLook {
    fill: Hsla,
    text: Hsla,
    muted: Hsla,
    faint: Hsla,
    /// The hairline round the bubble and its pointer.
    edge: Hsla,
    shadow: Hsla,
}

impl TipLook {
    fn new(theme: &Theme, chat_bg: Hsla) -> Self {
        // A translucent chat background (glass) is flattened onto the
        // theme's, so the tip never shows what lies under it.
        let fill = Hsla {
            a: 1.0,
            ..theme.bg.blend(chat_bg)
        };
        Self {
            fill,
            text: theme.text,
            muted: theme.text_muted,
            faint: theme.text_faint,
            edge: theme.text.opacity(match theme.appearance {
                crate::kit::theme::Appearance::Light => 0.13,
                crate::kit::theme::Appearance::Dark => 0.18,
            }),
            shadow: theme.lift_shadow(),
        }
    }
}

/// A tip line: a step's label, and a faint note beside it.
fn tip_line(label: &str, note: Option<String>, look: &TipLook) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_baseline()
        .gap(px(6.0))
        .child(
            div()
                .text_color(look.text)
                .child(SharedString::from(label.to_owned())),
        )
        .children(note.map(|note| {
            div()
                .text_size(px(10.0))
                .text_color(look.faint)
                .child(SharedString::from(note))
        }))
        .into_any_element()
}

/// A bubble in the chat's colour with a pointer down to its dot, anchored on
/// the side with room so it never runs off the row.
fn tip(lines: Vec<AnyElement>, anchor_left: bool, look: &TipLook) -> Div {
    let shadow = |alpha: f32, y: f32, blur: f32| gpui::BoxShadow {
        color: look.shadow.opacity(look.shadow.a * alpha),
        offset: gpui::point(px(0.0), px(y)),
        blur_radius: px(blur),
        spread_radius: px(0.0),
        inset: false,
    };
    let tip = div()
        .absolute()
        .bottom(px(TIP_LIFT))
        .flex()
        .flex_col()
        .when(anchor_left, |el| el.items_start())
        .when(!anchor_left, |el| el.items_end())
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(6.0))
                .bg(look.fill)
                .border_1()
                .border_color(look.edge)
                // A tight shadow for the edge, a wide one for the lift.
                .shadow(vec![shadow(1.6, 1.0, 3.0), shadow(1.9, 5.0, 16.0)])
                .whitespace_nowrap()
                .text_size(px(11.0))
                .children(lines),
        )
        .child(
            pointer(look.fill, look.edge)
                // Over the bubble's bottom hairline, so the two join.
                .mt(px(-1.0))
                .mx(px(POINTER_INSET - POINTER_W / 2.0)),
        );
    if anchor_left {
        tip.left(px(HIT / 2.0 - POINTER_INSET))
    } else {
        tip.right(px(HIT / 2.0 - POINTER_INSET))
    }
}

/// The tip's pointer: a small downward triangle in the bubble's fill with
/// the bubble's hairline along its two slanted sides. Its top row covers the
/// bubble's bottom hairline where they meet.
fn pointer(fill: Hsla, edge: Hsla) -> gpui::Canvas<()> {
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, w, h) = (bounds.origin, bounds.size.width, bounds.size.height);
            let triangle =
                |inset: Pixels, top: Pixels, tip: Pixels, color: Hsla, window: &mut Window| {
                    let mut path = gpui::PathBuilder::fill();
                    path.move_to(gpui::point(o.x + inset, o.y + top));
                    path.line_to(gpui::point(o.x + w - inset, o.y + top));
                    path.line_to(gpui::point(o.x + w / 2.0, o.y + h - tip));
                    path.close();
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color);
                    }
                };
            triangle(px(0.0), px(1.0), px(0.0), edge, window);
            triangle(px(1.4), px(0.0), px(1.6), fill, window);
        },
    )
    .w(px(POINTER_W))
    .h(px(POINTER_H + 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upcoming_steps_share_what_is_left() {
        assert_eq!(upcoming_positions(40, 2), vec![0.6, 0.8]);
        assert!(upcoming_positions(40, 0).is_empty());
        assert_eq!(upcoming_positions(10, 40).len(), MAX_UPCOMING);
        assert_eq!(left_note(0), "last step");
        assert_eq!(left_note(1), "1 step left");
        assert_eq!(left_note(4), "4 steps left");
    }

    #[test]
    fn stages_stay_behind_the_fill() {
        assert_eq!(stage_positions(&[10, 30], 40), vec![0.1, 0.3]);
        let shrunk = stage_positions(&[30, 60], 20);
        assert!((shrunk[0] - 0.1).abs() < 1e-6 && (shrunk[1] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn motion_follows_readings() {
        let t0 = Instant::now();
        let mut motion = ProgressMotion::default();
        motion.observe("a", 1, 25, &[0.5, 0.75], t0);
        // First sight fills in from empty, nothing leaving.
        assert_eq!((motion.from_percent, motion.grew), (0, false));
        assert_eq!(motion.leaving(t0), None);
        // Same stage, further along: the fill glides, no tip switch.
        motion.observe("a", 1, 30, &[0.5, 0.75], t0);
        assert_eq!((motion.from_percent, motion.grew), (25, false));
        // Refreshed steps glide from where they were, a new one from the end.
        motion.observe("a", 1, 30, &[0.4, 0.6, 0.8], t0);
        assert_eq!(motion.upcoming_at(0.0), vec![0.5, 0.75, 1.0]);
        assert_eq!(motion.upcoming_at(1.0), vec![0.4, 0.6, 0.8]);
        // A new stage: the old tip leaves for a moment.
        let t1 = t0 + Duration::from_secs(30);
        motion.observe("a", 2, 42, &[0.7], t1);
        assert_eq!((motion.from_percent, motion.grew), (30, true));
        assert_eq!(motion.leaving(t1 + Duration::from_millis(100)), Some(0));
        assert_eq!(motion.leaving(t1 + LEAVE_FOR), None);
        // Re-observing the same reading changes nothing.
        let revision = motion.revision;
        motion.observe("a", 2, 42, &[0.7], t1 + Duration::from_secs(1));
        assert_eq!(motion.revision, revision);
        assert_eq!(motion.from_percent, 30);
        // Another chat starts over.
        motion.observe("b", 3, 60, &[], t1);
        assert_eq!((motion.from_percent, motion.leaving(t1)), (0, None));
    }
}
