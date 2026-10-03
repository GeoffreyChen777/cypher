//! A soft drop shadow drawn only OUTSIDE a rounded shape.
//!
//! gpui's box shadow is a blurred filled shape painted beneath the element,
//! so under a translucent fill (the composer pill on glass) it shows through
//! as a grey plate. This shadow is instead a stack of thin bands around the
//! shape — each one the space between two rounded outlines — fading outward
//! and reaching further below than above, so it reads as a lift without ever
//! touching the shape's inside. Paint it after the shape's own backdrop blur:
//! nothing of it lies where the blur samples.

use gpui::{Bounds, FillOptions, Hsla, PathBuilder, PathStyle, Pixels, Window, canvas, point, px};

/// How far the shadow reaches past the shape on each side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reach {
    pub top: f32,
    pub side: f32,
    pub bottom: f32,
}

/// One band: its outer outline's distance past the shape on each side (its
/// inner outline is the previous band's outer one, the shape's own for the
/// first), and its opacity as a share of the shadow's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub outer: Reach,
    pub opacity: f32,
}

/// The bands of a shadow reaching `reach`, nearest the shape first: one per
/// pixel of the furthest reach, opacity easing out to nothing at the edge.
pub fn bands(reach: Reach) -> Vec<Band> {
    let count = reach.top.max(reach.side).max(reach.bottom).ceil().max(1.0) as usize;
    (1..=count)
        .map(|step| {
            let t = step as f32 / count as f32;
            Band {
                outer: Reach {
                    top: reach.top * t,
                    side: reach.side * t,
                    bottom: reach.bottom * t,
                },
                // Densest at the shape, easing to nothing at the far edge.
                opacity: (1.0 - (step - 1) as f32 / count as f32).powi(2),
            }
        })
        .collect()
}

/// A rounded rectangle's outline, clockwise from the top-left corner's end,
/// its radius clamped to half the shorter side (as gpui clamps a quad's).
fn rounded_rect(bounds: Bounds<Pixels>, radius: f32) -> Vec<gpui::Point<Pixels>> {
    use std::f32::consts::{FRAC_PI_2, PI};
    const STEPS: usize = 8;
    let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    // Corner centers and the angle each corner's arc starts at.
    let corners = [
        (x0 + w - r, y0 + r, -FRAC_PI_2),
        (x0 + w - r, y0 + h - r, 0.0),
        (x0 + r, y0 + h - r, FRAC_PI_2),
        (x0 + r, y0 + r, PI),
    ];
    corners
        .iter()
        .flat_map(|&(cx, cy, start)| {
            (0..=STEPS).map(move |step| {
                let theta = start + FRAC_PI_2 * step as f32 / STEPS as f32;
                point(px(cx + r * theta.cos()), px(cy + r * theta.sin()))
            })
        })
        .collect()
}

fn grown(bounds: Bounds<Pixels>, by: Reach) -> Bounds<Pixels> {
    Bounds {
        origin: point(bounds.origin.x - px(by.side), bounds.origin.y - px(by.top)),
        size: gpui::size(
            bounds.size.width + px(2.0 * by.side),
            bounds.size.height + px(by.top + by.bottom),
        ),
    }
}

/// The shadow for a shape rounded with `radius`, in `color` at its densest.
/// Position the canvas over the shape grown by `reach` on each side (negative
/// insets on an absolute canvas beside it); the shape is that box shrunk back.
pub fn outside_shadow(radius: f32, reach: Reach, color: Hsla) -> gpui::Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, _, window: &mut Window, _| {
            let shape = Bounds {
                origin: point(
                    bounds.origin.x + px(reach.side),
                    bounds.origin.y + px(reach.top),
                ),
                size: gpui::size(
                    bounds.size.width - px(2.0 * reach.side),
                    bounds.size.height - px(reach.top + reach.bottom),
                ),
            };
            if shape.size.width <= px(0.0) || shape.size.height <= px(0.0) {
                return;
            }
            let mut inner = Reach {
                top: 0.0,
                side: 0.0,
                bottom: 0.0,
            };
            for band in bands(reach) {
                // Corners grow with the band, so every outline stays the
                // shape's own rounding pushed outward.
                let mut builder =
                    PathBuilder::fill().with_style(PathStyle::Fill(FillOptions::even_odd()));
                builder.add_polygon(
                    &rounded_rect(grown(shape, band.outer), radius + band.outer.side),
                    true,
                );
                builder.add_polygon(
                    &rounded_rect(grown(shape, inner), radius + inner.side),
                    true,
                );
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color.opacity(color.a * band.opacity));
                }
                inner = band.outer;
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_nest_outward_and_fade_to_nothing() {
        let reach = Reach {
            top: 4.0,
            side: 8.0,
            bottom: 14.0,
        };
        let bands = bands(reach);
        // One band per pixel of the furthest reach, ending exactly there.
        assert_eq!(bands.len(), 14);
        assert_eq!(bands.last().unwrap().outer, reach);
        assert_eq!(bands[0].opacity, 1.0);
        for pair in bands.windows(2) {
            // Each band lies outside the last on every side…
            assert!(pair[1].outer.top > pair[0].outer.top);
            assert!(pair[1].outer.side > pair[0].outer.side);
            assert!(pair[1].outer.bottom > pair[0].outer.bottom);
            // …and is fainter.
            assert!(pair[1].opacity < pair[0].opacity);
        }
        assert!(bands.last().unwrap().opacity < 0.01);
        // It always reaches further below than above: a lift, not a halo.
        assert!(bands.iter().all(|b| b.outer.bottom > b.outer.top));
    }

    #[test]
    fn rounded_outlines_stay_within_their_box() {
        let bounds = Bounds {
            origin: point(px(10.0), px(20.0)),
            size: gpui::size(px(300.0), px(49.0)),
        };
        // A radius past half the height clamps to a semicircular end.
        let points = rounded_rect(bounds, 26.0);
        for p in &points {
            assert!(f32::from(p.x) >= 10.0 - 0.01 && f32::from(p.x) <= 310.0 + 0.01);
            assert!(f32::from(p.y) >= 20.0 - 0.01 && f32::from(p.y) <= 69.0 + 0.01);
        }
        let leftmost = points
            .iter()
            .map(|p| f32::from(p.x))
            .fold(f32::MAX, f32::min);
        assert!((leftmost - 10.0).abs() < 0.01);
    }
}
