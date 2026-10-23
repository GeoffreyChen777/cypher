//! The working trailer's progress bar, painted: a hairline track whose
//! filled part carries a travelling glint, a solid dot where each passed
//! stage began, a hollow ring that breathes for the current stage, a faint
//! dot for each step still to come, and a faint pinpoint at the finish. Everything is neutral grey from the theme's
//! text colour.
//!
//! The bar is a [`Loader`](crate::kit::loaders::Loader): it paints from the
//! shared pulse clock, so inside the transcript's cached view the glint and
//! breathing ring animate through its live layer without rebuilding any
//! rows. The track and fill break around every dot, leaving a clear gap on
//! either side, and the current ring stays hollow on whatever surface the
//! chat sits on.
//!
//! The geometry is pure ([`dot_x`], [`segments`]) so it is unit-tested; the
//! transcript positions its hover targets with the same [`INSET`] mapping.

use gpui::{Bounds, Hsla, Pixels, Window, point, px, size};

/// Most stage dots a bar carries (the engine keeps 16 stages).
pub const MAX_DOTS: usize = 16;
/// Most upcoming-step dots a bar carries.
pub const MAX_UPCOMING: usize = 12;
/// Height of the painted band; the track runs through its middle.
pub const HEIGHT: f32 = 22.0;
/// How far the first and last dots sit inside the bar's ends, so a ring at
/// 0% or 100% is never clipped.
pub const INSET: f32 = 7.0;
/// Glint period.
pub const FLOW_MS: u64 = 2400;

const TRACK: f32 = 2.0;
const GAP: f32 = 2.5;
const PASSED_DOT: f32 = 6.0;
/// The current ring breathes between these diameters.
const CURRENT_SMALL: f32 = 8.0;
const CURRENT_LARGE: f32 = 11.0;
const CURRENT_BORDER: f32 = 1.5;
const FINISH_DOT: f32 = 4.0;
const UPCOMING_DOT: f32 = 4.5;
const GLINT: f32 = 72.0;

/// Each part's share of the ink: light greys, the current ring darkest.
const TONE_TRACK: f32 = 0.06;
const TONE_FILL: f32 = 0.26;
const TONE_GLINT: f32 = 0.6;
const TONE_FINISH: f32 = 0.14;
const TONE_UPCOMING: f32 = 0.22;
const TONE_PASSED: f32 = 0.36;
const TONE_CURRENT: f32 = 0.58;

/// One frame's worth of bar: Copy, so a live-layer slot can replay it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgressBar {
    /// Filled fraction, 0–1 (mid-tween while the fill moves).
    pub fill: f32,
    /// Where each stage began, 0–1, oldest first; the last is current.
    pub dots: [f32; MAX_DOTS],
    pub dot_count: u8,
    /// Where each step still to come begins, 0–1, in order.
    pub upcoming: [f32; MAX_UPCOMING],
    pub upcoming_count: u8,
    /// How far the newest dot has grown in, 0–1.
    pub appear: f32,
    /// The theme's text colour; every tone is a shade of it.
    pub ink: Hsla,
}

impl ProgressBar {
    pub fn new(fill: f32, stages: &[f32], upcoming: &[f32], appear: f32, ink: Hsla) -> Self {
        let mut dots = [0.0; MAX_DOTS];
        let kept = &stages[stages.len().saturating_sub(MAX_DOTS)..];
        dots[..kept.len()].copy_from_slice(kept);
        let mut ahead = [0.0; MAX_UPCOMING];
        let shown = &upcoming[..upcoming.len().min(MAX_UPCOMING)];
        ahead[..shown.len()].copy_from_slice(shown);
        Self {
            fill: fill.clamp(0.0, 1.0),
            dots,
            dot_count: kept.len() as u8,
            upcoming: ahead,
            upcoming_count: shown.len() as u8,
            appear: appear.clamp(0.0, 1.0),
            ink,
        }
    }

    fn dots(&self) -> &[f32] {
        &self.dots[..self.dot_count as usize]
    }

    fn upcoming(&self) -> &[f32] {
        &self.upcoming[..self.upcoming_count as usize]
    }

    /// Paint at glint/breath phase `phase` (0–1; 0 under reduced motion,
    /// where the glint hides and the ring rests small).
    pub fn paint(&self, bounds: Bounds<Pixels>, phase: f32, window: &mut Window) {
        let width = f32::from(bounds.size.width);
        let mid = f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.0;
        let left = f32::from(bounds.origin.x);
        let dots = self.dots();
        let last = dots.len().checked_sub(1);
        // Every dot with the room it keeps clear on the track. The current
        // ring keeps room for its largest breath, so the gap holds still.
        let mut holes: Vec<(f32, f32)> = dots
            .iter()
            .enumerate()
            .map(|(ix, &at)| {
                let radius = if Some(ix) == last {
                    CURRENT_LARGE
                } else {
                    PASSED_DOT
                } / 2.0;
                (dot_x(at, width), radius + GAP)
            })
            .collect();
        let upcoming = self.upcoming();
        holes.extend(
            upcoming
                .iter()
                .map(|&at| (dot_x(at, width), UPCOMING_DOT / 2.0 + GAP)),
        );
        // The finish shows unless the last dot before it crowds it.
        let finish = dot_x(1.0, width);
        let before_finish = upcoming
            .last()
            .map(|&at| (dot_x(at, width), UPCOMING_DOT))
            .or_else(|| last.map(|ix| (dot_x(dots[ix], width), CURRENT_LARGE)));
        let finish_shown = before_finish.is_none_or(|(x, size)| finish - x > size);
        if finish_shown {
            holes.push((finish, FINISH_DOT / 2.0 + GAP));
        }
        let fill_x = dot_x(self.fill, width);
        let bar = |from: f32, to: f32| {
            Bounds::new(
                point(px(left + from), px(mid - TRACK / 2.0)),
                size(px(to - from), px(TRACK)),
            )
        };
        let round = px(TRACK / 2.0);

        // Track, then fill over it, both broken around the rings.
        for (from, to) in segments(fill_x, width, &holes) {
            window.paint_quad(
                gpui::fill(bar(from, to), self.ink.opacity(TONE_TRACK)).corner_radii(round),
            );
        }
        let fill_segments = segments(0.0, fill_x, &holes);
        for &(from, to) in &fill_segments {
            window.paint_quad(
                gpui::fill(bar(from, to), self.ink.opacity(TONE_FILL)).corner_radii(round),
            );
        }

        // The glint: a soft band sweeping the filled part once per period,
        // entering from off the left edge and leaving past the fill's end.
        if phase > 0.0 && fill_x > 0.0 {
            let eased = crate::kit::motion::EASE_IN_OUT.eval(phase);
            let centre = -GLINT / 2.0 + eased * (fill_x + GLINT);
            let peak = self.ink.opacity(TONE_GLINT);
            let clear = self.ink.opacity(0.0);
            for &(from, to) in &fill_segments {
                let clip = gpui::ContentMask {
                    bounds: bar(from, to),
                };
                window.with_content_mask(Some(clip), |window| {
                    let half = GLINT / 2.0;
                    window.paint_quad(gpui::fill(
                        bar(centre - half, centre),
                        gpui::linear_gradient(
                            90.0,
                            gpui::linear_color_stop(clear, 0.0),
                            gpui::linear_color_stop(peak, 1.0),
                        ),
                    ));
                    window.paint_quad(gpui::fill(
                        bar(centre, centre + half),
                        gpui::linear_gradient(
                            90.0,
                            gpui::linear_color_stop(peak, 0.0),
                            gpui::linear_color_stop(clear, 1.0),
                        ),
                    ));
                });
            }
        }

        let circle = |x: f32, diameter: f32| {
            Bounds::new(
                point(px(left + x - diameter / 2.0), px(mid - diameter / 2.0)),
                size(px(diameter), px(diameter)),
            )
        };
        let ring = |window: &mut Window, x: f32, diameter: f32, border: f32, color: Hsla| {
            window.paint_quad(gpui::quad(
                circle(x, diameter),
                px(diameter / 2.0),
                gpui::transparent_black(),
                px(border),
                color,
                gpui::BorderStyle::Solid,
            ));
        };
        let disc = |window: &mut Window, x: f32, diameter: f32, color: Hsla| {
            window.paint_quad(
                gpui::fill(circle(x, diameter), color).corner_radii(px(diameter / 2.0)),
            );
        };

        if finish_shown {
            disc(window, finish, FINISH_DOT, self.ink.opacity(TONE_FINISH));
        }
        for &at in upcoming {
            disc(
                window,
                dot_x(at, width),
                UPCOMING_DOT,
                self.ink.opacity(TONE_UPCOMING),
            );
        }
        for (ix, &at) in dots.iter().enumerate() {
            let x = dot_x(at, width);
            if Some(ix) != last {
                disc(window, x, PASSED_DOT, self.ink.opacity(TONE_PASSED));
                continue;
            }
            // Current: a hollow ring breathing in and out once per period,
            // grown in from nothing when its stage is new.
            let breath = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
            let diameter = CURRENT_SMALL + (CURRENT_LARGE - CURRENT_SMALL) * breath;
            ring(
                window,
                x,
                diameter * self.appear,
                CURRENT_BORDER,
                self.ink.opacity(TONE_CURRENT * self.appear),
            );
        }
    }
}

/// The x of a stage at `at` (0–1) on a bar `width` wide: inset from both
/// ends by [`INSET`].
pub fn dot_x(at: f32, width: f32) -> f32 {
    INSET + at.clamp(0.0, 1.0) * (width - 2.0 * INSET).max(0.0)
}

/// `[from, to]` with every `(centre, radius)` hole cut out, in order.
pub fn segments(from: f32, to: f32, holes: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut cuts: Vec<(f32, f32)> = holes.iter().map(|&(c, r)| (c - r, c + r)).collect();
    cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    let mut start = from;
    for (cut_from, cut_to) in cuts {
        if cut_to <= start {
            continue;
        }
        if cut_from >= to {
            break;
        }
        if cut_from > start {
            out.push((start, cut_from));
        }
        start = start.max(cut_to);
    }
    if start < to {
        out.push((start, to));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dots_sit_inside_the_ends() {
        assert_eq!(dot_x(0.0, 200.0), INSET);
        assert_eq!(dot_x(1.0, 200.0), 200.0 - INSET);
        assert_eq!(dot_x(0.5, 200.0), 100.0);
        assert_eq!(dot_x(2.0, 200.0), 200.0 - INSET);
    }

    #[test]
    fn segments_break_around_holes() {
        assert_eq!(segments(0.0, 100.0, &[]), vec![(0.0, 100.0)]);
        assert_eq!(
            segments(0.0, 100.0, &[(50.0, 5.0), (20.0, 5.0)]),
            vec![(0.0, 15.0), (25.0, 45.0), (55.0, 100.0)]
        );
        // Holes past either end, or overlapping, cut only what they cover.
        assert_eq!(
            segments(10.0, 60.0, &[(5.0, 8.0), (58.0, 4.0), (61.0, 4.0)]),
            vec![(13.0, 54.0)]
        );
        assert!(segments(0.0, 10.0, &[(5.0, 20.0)]).is_empty());
    }

    #[test]
    fn a_bar_keeps_its_newest_dots() {
        let stages: Vec<f32> = (0..20).map(|n| n as f32 / 20.0).collect();
        let ahead: Vec<f32> = (0..20).map(|n| 0.95 + n as f32 / 1000.0).collect();
        let bar = ProgressBar::new(1.4, &stages, &ahead, 1.0, gpui::black());
        assert_eq!(bar.dot_count as usize, MAX_DOTS);
        assert_eq!(bar.upcoming().len(), MAX_UPCOMING);
        assert_eq!(bar.upcoming()[0], 0.95);
        assert_eq!(bar.dots()[MAX_DOTS - 1], 19.0 / 20.0);
        assert_eq!(bar.fill, 1.0);
    }
}
