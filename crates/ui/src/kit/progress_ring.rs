//! The progress ring: one small mark that says both how far a job has come
//! and that it is alive.
//!
//! The steps done are an arc from twelve o'clock, clockwise, over a faint
//! track. The spinner is part of the same ring: once per [`RING_ORBIT`]
//! period a comet leaves the tip of that arc, stretches as it gathers speed
//! round the part still to do, crosses the finished arc as a glint, and is
//! taken back in at the tip, where the ring rests for a beat. With nothing
//! done yet the comet circles alone from the top, a plain spinner; under
//! reduced motion the ring holds still.
//!
//! [`RING_ORBIT`]: crate::kit::motion::RING_ORBIT

use std::f32::consts::{PI, TAU};

use gpui::{Bounds, FillOptions, Hsla, PathBuilder, PathStyle, Pixels, Window, point, px};

use crate::kit::theme::Theme;

/// The share of a period the comet travels in; it rests in the tip for what
/// is left.
const TRAVEL: f32 = 0.86;
/// How long after the head the tail sets off, as a share of the travel: the
/// comet is longest mid-turn and nothing at either end.
const LAG: f32 = 0.18;
/// The comet is drawn in this many slices, fainter toward its tail.
const SLICES: usize = 5;
/// The longest stretch of arc between two outline points, in turns.
const SAMPLE: f32 = 1.0 / 48.0;

/// A ring as it paints: the share done, its stroke, and its colours (the
/// arc and the comet over the track, the track, the comet over the arc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgressRing {
    pub fraction: f32,
    pub stroke: f32,
    pub ink: Hsla,
    pub track: Hsla,
    pub glint: Hsla,
}

impl ProgressRing {
    /// `done` of `total` steps in `ink`.
    pub fn new(done: usize, total: usize, ink: Hsla, theme: &Theme) -> Self {
        Self {
            fraction: if total == 0 {
                0.0
            } else {
                (done as f32 / total as f32).clamp(0.0, 1.0)
            },
            stroke: 2.0,
            ink,
            track: theme.ink(0.11),
            // What reads on the ink: light on a dark arc, dark on a light one.
            glint: theme.on_solid.opacity(0.72),
        }
    }

    /// Paint the ring in the square at the middle of `bounds`, its comet
    /// where `phase` (0..1 of a period) puts it.
    pub fn paint(&self, bounds: Bounds<Pixels>, phase: f32, window: &mut Window) {
        let side = f32::from(bounds.size.width.min(bounds.size.height));
        let half = self.stroke / 2.0;
        let radius = side / 2.0 - half;
        if radius <= 0.0 {
            return;
        }
        let center = bounds.center();
        let center = (f32::from(center.x), f32::from(center.y));
        let mut fill = |from: f32, to: f32, caps: (bool, bool), color: Hsla| {
            let points: Vec<_> = band(center, radius, half, from, to, caps)
                .into_iter()
                .map(|(x, y)| point(px(x), px(y)))
                .collect();
            // Non-zero: an arc near the full turn laps its own round ends.
            let mut builder =
                PathBuilder::fill().with_style(PathStyle::Fill(FillOptions::non_zero()));
            builder.add_polygon(&points, true);
            if let Ok(path) = builder.build() {
                window.paint_path(path, color);
            }
        };
        let done = self.fraction.clamp(0.0, 1.0);
        fill(0.0, 1.0, (false, false), self.track);
        if done >= 1.0 {
            fill(0.0, 1.0, (false, false), self.ink);
        } else if done > 0.0 {
            fill(0.0, done, (true, true), self.ink);
        }
        let (tail, head) = comet(phase);
        if head - tail < 0.004 {
            return;
        }
        // From the tip of the arc: what is left of the turn is track, the
        // lap past twelve o'clock runs over the arc.
        let (from, to) = (done + tail, done + head);
        for slice in 0..SLICES {
            let at = |ix: usize| from + (to - from) * ix as f32 / SLICES as f32;
            let (start, end) = (at(slice), at(slice + 1));
            let strength = ((slice + 1) as f32 / SLICES as f32).powf(1.6);
            let leading = slice + 1 == SLICES;
            if start < 1.0 {
                let end = end.min(1.0);
                fill(
                    start,
                    end,
                    (false, leading && end < 1.0),
                    self.ink.opacity(self.ink.a * strength),
                );
            }
            if end > 1.0 {
                fill(
                    start.max(1.0),
                    end,
                    (false, leading),
                    self.glint.opacity(self.glint.a * strength),
                );
            }
        }
    }
}

/// The comet at `phase` of a period: how far its tail and its head have
/// travelled from the tip of the arc, in turns. Both ease out of the tip
/// and back into it, the tail [`LAG`] behind, so the comet grows from
/// nothing, is longest at full speed and closes up as it lands.
pub fn comet(phase: f32) -> (f32, f32) {
    let travelled = (phase.rem_euclid(1.0) / TRAVEL).min(1.0);
    let ease = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    (
        ease((travelled - LAG) / (1.0 - LAG)),
        ease(travelled / (1.0 - LAG)),
    )
}

/// The outline of the band `half` either side of the circle of `radius`
/// about `center`, from `from` to `to` turns clockwise from twelve o'clock;
/// `caps` round its start and its end. A full turn without caps is the
/// whole ring.
pub fn band(
    center: (f32, f32),
    radius: f32,
    half: f32,
    from: f32,
    to: f32,
    caps: (bool, bool),
) -> Vec<(f32, f32)> {
    let angle = |turn: f32| turn * TAU - PI / 2.0;
    let at = |angle: f32, reach: f32| {
        (
            center.0 + reach * angle.cos(),
            center.1 + reach * angle.sin(),
        )
    };
    let steps = (((to - from) / SAMPLE).ceil() as usize).max(1);
    let turn = |ix: usize| from + (to - from) * ix as f32 / steps as f32;
    // A half circle about the band's middle at `turn`, from the edge at
    // `start` round through the side the band does not cover.
    let cap = |turn: f32, start: f32| {
        let middle = at(angle(turn), radius);
        (1..6).map(move |ix| {
            let around = start + PI * ix as f32 / 6.0;
            (
                middle.0 + half * around.cos(),
                middle.1 + half * around.sin(),
            )
        })
    };
    let mut outline = Vec::with_capacity(2 * steps + 14);
    outline.extend((0..=steps).map(|ix| at(angle(turn(ix)), radius + half)));
    if caps.1 {
        outline.extend(cap(to, angle(to)));
    }
    outline.extend(
        (0..=steps)
            .rev()
            .map(|ix| at(angle(turn(ix)), radius - half)),
    );
    if caps.0 {
        outline.extend(cap(from, angle(from) + PI));
    }
    outline
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_comet_leaves_the_tip_and_comes_back_to_it() {
        assert_eq!(comet(0.0), (0.0, 0.0));
        // It rests in the tip once it has gone round.
        for phase in [TRAVEL, 0.93, 0.999] {
            assert_eq!(comet(phase), (1.0, 1.0), "at {phase}");
        }
        // Mid-turn it is stretched over about a third of the ring.
        let (tail, head) = comet(TRAVEL / 2.0);
        assert!((0.3..0.45).contains(&(head - tail)), "{tail}..{head}");
        assert!((head + tail - 1.0).abs() < 1e-4, "{tail}..{head}");
    }

    #[test]
    fn the_comet_only_moves_forward_and_never_turns_inside_out() {
        let mut last = (0.0_f32, 0.0_f32);
        // Up to the end of the period: the next one starts from the tip.
        for step in 0..200 {
            let (tail, head) = comet(step as f32 / 200.0);
            assert!(tail <= head + 1e-6, "{tail} > {head}");
            assert!(tail + 1e-6 >= last.0 && head + 1e-6 >= last.1);
            assert!((0.0..=1.0).contains(&tail) && (0.0..=1.0).contains(&head));
            last = (tail, head);
        }
    }

    #[test]
    fn a_band_starts_at_twelve_and_runs_clockwise() {
        // A quarter turn about the origin: from the top to the right.
        let outline = band((0.0, 0.0), 6.0, 1.0, 0.0, 0.25, (false, false));
        let near = |(x, y): (f32, f32), (wx, wy): (f32, f32)| {
            (x - wx).abs() < 1e-3 && (y - wy).abs() < 1e-3
        };
        let half = outline.len() / 2;
        assert!(near(outline[0], (0.0, -7.0)), "{:?}", outline[0]);
        assert!(near(outline[half - 1], (7.0, 0.0)));
        assert!(near(outline[half], (5.0, 0.0)));
        assert!(near(outline[outline.len() - 1], (0.0, -5.0)));
        // Every point stays within the stroke.
        for (x, y) in outline {
            let reach = (x * x + y * y).sqrt();
            assert!((5.0 - 1e-3..=7.0 + 1e-3).contains(&reach));
        }
    }

    #[test]
    fn round_ends_reach_half_a_stroke_past_the_arc() {
        let outline = band((0.0, 0.0), 6.0, 1.0, 0.0, 0.25, (true, true));
        // Past the end (pointing down from the right) and before the start
        // (pointing left from the top).
        let lowest = outline.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        let leftmost = outline.iter().map(|p| p.0).fold(f32::MAX, f32::min);
        assert!((lowest - 1.0).abs() < 1e-3, "{lowest}");
        assert!((leftmost + 1.0).abs() < 1e-3, "{leftmost}");
    }

    #[test]
    fn a_ring_reads_its_share_from_the_steps() {
        let theme = Theme::dark();
        let ring = |done, total| ProgressRing::new(done, total, theme.accent, &theme).fraction;
        assert_eq!(ring(0, 0), 0.0);
        assert_eq!(ring(1, 4), 0.25);
        assert_eq!(ring(5, 4), 1.0);
    }
}
