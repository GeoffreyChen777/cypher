//! Composer context ring: the agent's context-window occupancy drawn as a
//! small progress ring beside the model chip. The reading is the engine's
//! local session projection (`Session::context_usage`); a click compacts the
//! session when its harness has `/compact`.

use gpui::{
    Context, Hsla, IntoElement, PathBuilder, SharedString, Window, canvas, div, point, prelude::*,
    px,
};

use cypher_proto::ContextUsage;

use crate::theme::Theme;

/// Ring diameter — the chips' 16px icon size.
pub const RING_SIZE: f32 = 16.0;
const RING_STROKE: f32 = 2.0;
/// Polyline resolution for a full turn; arcs use the proportional share.
const RING_SEGMENTS: usize = 64;
/// Occupancy at which the ring turns amber, then red.
const WARN_FRACTION: f32 = 0.75;
const DANGER_FRACTION: f32 = 0.9;

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

/// The ring itself: a faint full-circle track with the filled share painted
/// clockwise from 12 o'clock.
pub fn ring(fraction: f32, track: Hsla, fill: Hsla) -> impl IntoElement {
    let fraction = fraction.clamp(0.0, 1.0);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let (cx, cy) = (f32::from(center.x), f32::from(center.y));
            let radius =
                f32::from(bounds.size.width.min(bounds.size.height)) / 2.0 - RING_STROKE / 2.0;
            let arc = |share: f32| {
                let steps = ((RING_SEGMENTS as f32 * share).ceil() as usize).max(1);
                let mut builder = PathBuilder::stroke(px(RING_STROKE));
                for step in 0..=steps {
                    let angle = -std::f32::consts::FRAC_PI_2
                        + std::f32::consts::TAU * share * step as f32 / steps as f32;
                    let at = point(px(cx + radius * angle.cos()), px(cy + radius * angle.sin()));
                    if step == 0 {
                        builder.move_to(at);
                    } else {
                        builder.line_to(at);
                    }
                }
                builder.build()
            };
            if let Ok(path) = arc(1.0) {
                window.paint_path(path, track);
            }
            if fraction > 0.0
                && let Ok(path) = arc(fraction)
            {
                window.paint_path(path, fill);
            }
        },
    )
    .size(px(RING_SIZE))
    .flex_none()
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
