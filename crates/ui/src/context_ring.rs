//! Composer context gauge: the agent's context-window occupancy drawn as a
//! short arc ([`edge_arc`]) that follows the composer pill's rounded right
//! end, round the send button, filling from the bottom up. The reading is the
//! session row's `Session::context_usage` — live from this engine for its own
//! chats, synced through the registry for chats hosted elsewhere; a click
//! compacts the session when its harness has `/compact`.

use gpui::{
    Context, Hsla, IntoElement, PathBuilder, SharedString, Window, canvas, div, point, prelude::*,
    px,
};

use cypher_proto::ContextUsage;

use crate::theme::Theme;

/// Occupancy at which the gauge turns amber, then red.
const WARN_FRACTION: f32 = 0.75;
const DANGER_FRACTION: f32 = 0.9;

/// What the ring shows for the selected chat, shared by both of its forms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RingReading {
    pub usage: ContextUsage,
    /// The chat's agent can compact (has `/compact`).
    pub compactable: bool,
    /// A turn is running or waiting on the user: compacting waits.
    pub busy: bool,
}

impl RingReading {
    /// Whether a click compacts now.
    pub fn enabled(&self) -> bool {
        self.compactable && !self.busy
    }

    pub fn summary(&self) -> SharedString {
        usage_summary(self.usage).into()
    }

    /// The tooltip's second line: what a click does.
    pub fn hint(&self) -> SharedString {
        match (self.compactable, self.busy) {
            (false, _) => "This agent can't compact its context",
            (true, true) => "Compact once the agent finishes",
            (true, false) => "Click to compact",
        }
        .into()
    }
}

/// Fill color for an occupancy: muted until compaction starts to matter.
pub fn fill_color(fraction: f32, theme: &Theme) -> Hsla {
    if fraction >= DANGER_FRACTION {
        theme.danger
    } else if fraction >= WARN_FRACTION {
        theme.warning
    } else {
        theme.text_muted
    }
}

/// Compact token count: `950`, `9.5k`, `124k`, `1.2M`.
pub fn format_tokens(tokens: u64) -> String {
    fn trimmed(value: f64, suffix: &str) -> String {
        let text = format!("{value:.1}");
        let text = text.strip_suffix(".0").unwrap_or(&text);
        format!("{text}{suffix}")
    }
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..10_000 => trimmed(tokens as f64 / 1_000.0, "k"),
        10_000..1_000_000 => format!("{}k", (tokens as f64 / 1_000.0).round() as u64),
        _ => trimmed(tokens as f64 / 1_000_000.0, "M"),
    }
}

/// The tooltip's headline: `62% context used · 124k / 200k`.
pub fn usage_summary(usage: ContextUsage) -> String {
    format!(
        "{}% context used · {} / {}",
        (usage.fraction() * 100.0).round() as u32,
        format_tokens(usage.used),
        format_tokens(usage.size),
    )
}

/// The band's thickness.
pub const EDGE_STROKE: f32 = 2.0;
/// Clear space between the pill's border and the band. On the right end the
/// send button sits as far inside it, so the band is centered in the room
/// between the two.
pub const EDGE_GAP: f32 = 5.0;
/// How much of the rounded end the band turns through: 60°. On a
/// semicircular end (the single-line pill) it is centered on the far side;
/// on a taller one it sits in the middle of the bottom corner, the send
/// button's corner.
pub const EDGE_ANGLE: f32 = std::f32::consts::FRAC_PI_3;
/// Points per rounded end.
const EDGE_CAP_STEPS: usize = 8;

/// Whether a box `height` tall has a semicircular end at `radius` (no
/// straight side between its corners), which centers the band on the far
/// side rather than in the bottom corner.
pub fn edge_is_semicircle(height: f32, radius: f32) -> bool {
    height <= 2.0 * radius
}

type Pt = (f32, f32);

/// The single-line form's centerline in the coordinates of a box `height`
/// tall whose left end is rounded with `radius` (clamped to a semicircle):
/// from the bottom tangent, round the left end, to the top tangent, `inset`
/// in from the edge and concentric with it. Measured by length, so a share
/// reads the same on the curves as on any straight part between them.
struct EdgeCurve {
    height: f32,
    r: f32,
    rc: f32,
    inset: f32,
    corner: f32,
    straight: f32,
}

impl EdgeCurve {
    fn new(height: f32, radius: f32, inset: f32) -> Self {
        let r = radius.min(height / 2.0).max(inset);
        let rc = r - inset;
        Self {
            height,
            r,
            rc,
            inset,
            corner: std::f32::consts::FRAC_PI_2 * rc,
            straight: (height - 2.0 * r).max(0.0),
        }
    }

    /// The point `along` the curve, its normal toward the edge, and the
    /// direction it runs (up).
    fn at(&self, along: f32) -> (Pt, Pt, Pt) {
        use std::f32::consts::{FRAC_PI_2, PI};
        let angle = |s: f32| if self.rc > 0.0 { s / self.rc } else { 0.0 };
        let round = |center_y: f32, theta: f32| {
            let (sin, cos) = theta.sin_cos();
            (
                (self.r + self.rc * cos, center_y + self.rc * sin),
                (cos, sin),
                (-sin, cos),
            )
        };
        if along <= self.corner {
            round(self.height - self.r, FRAC_PI_2 + angle(along))
        } else if along <= self.corner + self.straight {
            (
                (self.inset, self.height - self.r - (along - self.corner)),
                (-1.0, 0.0),
                (0.0, -1.0),
            )
        } else {
            round(self.r, PI + angle(along - self.corner - self.straight))
        }
    }

    /// `share` of the band ([`EDGE_ANGLE`] of the end, placed as that says),
    /// from its bottom, about a sample per pixel.
    fn samples(&self, share: f32) -> Vec<(Pt, Pt, Pt)> {
        use std::f32::consts::{FRAC_PI_2, PI};
        // The angle the end turns through before the band starts, measured
        // from the bottom tangent: half of what the band leaves of the whole
        // semicircle, or of the bottom quarter turn on a taller end.
        let lead = if self.straight > 0.0 {
            (FRAC_PI_2 - EDGE_ANGLE) / 2.0
        } else {
            (PI - EDGE_ANGLE) / 2.0
        };
        let start = lead * self.rc;
        let length = share.clamp(0.0, 1.0) * EDGE_ANGLE * self.rc;
        if length <= 0.0 {
            return Vec::new();
        }
        let steps = (length.ceil() as usize).max(1);
        (0..=steps)
            .map(|step| self.at(start + length * step as f32 / steps as f32))
            .collect()
    }
}

/// The centerline of [`edge_arc_outline`]: `share` of the band, from its
/// bottom end. Empty for nothing to fill.
pub fn edge_arc_points(height: f32, radius: f32, inset: f32, share: f32) -> Vec<Pt> {
    EdgeCurve::new(height, radius, inset)
        .samples(share)
        .into_iter()
        .map(|(point, _, _)| point)
        .collect()
}

/// The band round the left end as one closed outline, `width` thick about
/// the centerline with semicircular ends: the edge side bottom to top, the
/// top end, the inner side back down, the bottom end. One shape, so a
/// translucent track does not darken where its ends would overlap a stroke.
pub fn edge_arc_outline(height: f32, radius: f32, inset: f32, width: f32, share: f32) -> Vec<Pt> {
    let samples = EdgeCurve::new(height, radius, inset).samples(share);
    let (Some(&first), Some(&last)) = (samples.first(), samples.last()) else {
        return Vec::new();
    };
    let half = width / 2.0;
    let offset = |(x, y): Pt, (nx, ny): Pt, by: f32| (x + nx * by, y + ny * by);
    // A semicircle about `center` from direction `from` to its opposite,
    // bulging toward `via`; the two ends are already on the sides.
    let cap = |center: Pt, from: Pt, via: Pt| {
        (1..EDGE_CAP_STEPS).map(move |step| {
            let (sin, cos) = (std::f32::consts::PI * step as f32 / EDGE_CAP_STEPS as f32).sin_cos();
            offset(
                center,
                (from.0 * cos + via.0 * sin, from.1 * cos + via.1 * sin),
                half,
            )
        })
    };
    let mut outline: Vec<Pt> = samples
        .iter()
        .map(|&(point, normal, _)| offset(point, normal, half))
        .collect();
    let (end, end_normal, end_tangent) = last;
    outline.extend(cap(end, end_normal, end_tangent));
    outline.extend(
        samples
            .iter()
            .rev()
            .map(|&(point, normal, _)| offset(point, normal, -half)),
    );
    let (start, (nx, ny), (tx, ty)) = first;
    outline.extend(cap(start, (-nx, -ny), (-tx, -ty)));
    outline
}

/// Which rounded end of the pill the single-line form follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeSide {
    Left,
    Right,
}

/// The gauge: a faint track round the `side` end of the box it fills
/// ([`EDGE_ANGLE`]), with the filled share laid over it from the bottom up,
/// both
/// [`EDGE_STROKE`] thick with rounded ends, [`EDGE_GAP`] clear of an edge
/// rounded with `corner_radius`. The caller sizes it to the box (the pill's
/// inside, within its border).
pub fn edge_arc(
    side: EdgeSide,
    fraction: f32,
    corner_radius: f32,
    track: Hsla,
    fill: Hsla,
) -> gpui::Canvas<()> {
    let fraction = fraction.clamp(0.0, 1.0);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let height = f32::from(bounds.size.height);
            let top = f32::from(bounds.origin.y);
            // The geometry is the left end's; the right end is its mirror.
            let x_of = |x: f32| match side {
                EdgeSide::Left => f32::from(bounds.origin.x) + x,
                EdgeSide::Right => f32::from(bounds.origin.x + bounds.size.width) - x,
            };
            let band = |share: f32| {
                let outline = edge_arc_outline(
                    height,
                    corner_radius,
                    EDGE_GAP + EDGE_STROKE / 2.0,
                    EDGE_STROKE,
                    share,
                );
                let points: Vec<_> = outline
                    .into_iter()
                    .map(|(x, y)| point(px(x_of(x)), px(top + y)))
                    .collect();
                let mut builder = PathBuilder::fill();
                builder.add_polygon(&points, true);
                builder.build()
            };
            if let Ok(path) = band(1.0) {
                window.paint_path(path, track);
            }
            if fraction > 0.0
                && let Ok(path) = band(fraction)
            {
                window.paint_path(path, fill);
            }
        },
    )
}

/// Hover card for the ring: the reading plus what a click does.
pub struct ContextRingTooltip {
    pub summary: SharedString,
    pub hint: SharedString,
}

impl Render for ContextRingTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_overlay)
            .text_size(px(12.0))
            .child(div().text_color(theme.text).child(self.summary.clone()))
            .child(div().text_color(theme.text_muted).child(self.hint.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_read_compactly() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(1_000), "1k");
        assert_eq!(format_tokens(9_540), "9.5k");
        assert_eq!(format_tokens(124_400), "124k");
        assert_eq!(format_tokens(200_000), "200k");
        assert_eq!(format_tokens(1_000_000), "1M");
        assert_eq!(format_tokens(1_250_000), "1.2M");
    }

    #[test]
    fn edge_arc_turns_sixty_degrees_from_the_bottom() {
        let close = |(ax, ay): Pt, (bx, by): Pt| (ax - bx).abs() < 0.01 && (ay - by).abs() < 0.01;
        // A semicircular end (the single-line pill's inside: 47 tall, 25
        // radius), the centerline 5.5 in.
        let (r, rc) = (23.5, 18.0);
        let full = edge_arc_points(47.0, 25.0, 5.5, 1.0);
        // 60° centered on the far left: 30° below to 30° above.
        let (across, up) = (rc * 3f32.sqrt() / 2.0, rc / 2.0);
        assert!(close(full[0], (r - across, r + up)), "{:?}", full[0]);
        assert!(close(*full.last().unwrap(), (r - across, r - up)));
        // On a circle concentric with the edge, climbing.
        for (x, y) in &full {
            let distance = ((x - r).powi(2) + (y - r).powi(2)).sqrt();
            assert!((distance - rc).abs() < 0.05, "{x},{y}");
        }
        assert!(full.windows(2).all(|pair| pair[1].1 <= pair[0].1 + 0.001));
        // Half the reading reaches the far left; nothing draws nothing.
        let half = edge_arc_points(47.0, 25.0, 5.5, 0.5);
        assert!(close(*half.last().unwrap(), (5.5, 23.5)));
        assert!(edge_arc_points(47.0, 25.0, 5.5, 0.0).is_empty());
        // A taller end (the multi-line pill, or attachments staged): 60° in
        // the middle of the bottom corner, where the send button sits — 15°
        // to 75° round from the bottom tangent.
        assert!(edge_is_semicircle(47.0, 25.0) && !edge_is_semicircle(120.0, 25.0));
        let (center, rc) = ((25.0, 95.0), 19.5);
        let corner = |degrees: f32| {
            let theta = (90.0 + degrees).to_radians();
            (center.0 + rc * theta.cos(), center.1 + rc * theta.sin())
        };
        let tall = edge_arc_points(120.0, 25.0, 5.5, 1.0);
        assert!(close(tall[0], corner(15.0)), "{:?}", tall[0]);
        assert!(close(*tall.last().unwrap(), corner(75.0)));
        for (x, y) in &tall {
            let distance = ((x - center.0).powi(2) + (y - center.1).powi(2)).sqrt();
            assert!((distance - rc).abs() < 0.05, "{x},{y}");
        }
    }

    #[test]
    fn the_band_keeps_off_the_edge_and_rounds_its_ends() {
        let (inset, width) = (EDGE_GAP + EDGE_STROKE / 2.0, EDGE_STROKE);
        let (r, rc) = (23.5, 23.5 - inset);
        let outline = edge_arc_outline(47.0, 25.0, inset, width, 1.0);
        // Clear of the border: nothing nearer the edge than the gap.
        let leftmost = outline.iter().map(|p| p.0).fold(f32::MAX, f32::min);
        assert!((leftmost - EDGE_GAP).abs() < 0.05, "{leftmost}");
        // Every outline point is within half the width of the centerline
        // circle: the band's sides and its rounded ends alike.
        for &(x, y) in &outline {
            let distance = ((x - r).powi(2) + (y - r).powi(2)).sqrt();
            assert!((distance - rc).abs() < width / 2.0 + 0.05, "{x},{y}");
        }
        // The ends are round: each reaches half the width past the
        // centerline's end, along the way the band was heading.
        let curve = EdgeCurve::new(47.0, 25.0, inset);
        let samples = curve.samples(1.0);
        let (start, _, (tx, ty)) = samples[0];
        let (end, _, (ux, uy)) = *samples.last().unwrap();
        let reach = |(px, py): Pt, (dx, dy): Pt| {
            outline
                .iter()
                .map(|&(x, y)| (x - px) * dx + (y - py) * dy)
                .fold(f32::MIN, f32::max)
        };
        assert!((reach(end, (ux, uy)) - width / 2.0).abs() < 0.05);
        assert!((reach(start, (-tx, -ty)) - width / 2.0).abs() < 0.05);
        // A sliver is still a rounded dot, and nothing draws nothing.
        assert!(edge_arc_outline(47.0, 25.0, inset, width, 0.001).len() > EDGE_CAP_STEPS * 2);
        assert!(edge_arc_outline(47.0, 25.0, inset, width, 0.0).is_empty());
    }

    #[test]
    fn readings_say_whether_a_click_compacts() {
        let usage = ContextUsage { used: 1, size: 2 };
        let reading = |compactable, busy| RingReading {
            usage,
            compactable,
            busy,
        };
        assert!(reading(true, false).enabled());
        assert_eq!(reading(true, false).hint().as_ref(), "Click to compact");
        assert!(!reading(true, true).enabled());
        assert_eq!(
            reading(true, true).hint().as_ref(),
            "Compact once the agent finishes"
        );
        assert!(!reading(false, false).enabled());
        assert_eq!(
            reading(true, false).summary().as_ref(),
            "50% context used · 1 / 2"
        );
    }

    #[test]
    fn summary_rounds_the_share_and_clamps_overflow() {
        let usage = ContextUsage {
            used: 124_000,
            size: 200_000,
        };
        assert_eq!(usage_summary(usage), "62% context used · 124k / 200k");
        let over = ContextUsage {
            used: 250_000,
            size: 200_000,
        };
        assert_eq!(over.fraction(), 1.0);
        let unknown = ContextUsage { used: 5, size: 0 };
        assert_eq!(unknown.fraction(), 0.0);
    }
}
