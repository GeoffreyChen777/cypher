//! LaTeX formulas in rendered markdown, typeset by RaTeX (a pure-Rust KaTeX
//! port with the KaTeX fonts embedded).
//!
//! gpui text cannot hold an inline image, so a formula takes part in text
//! layout through a stand-in: a run of non-breaking spaces ([`glue`]) as
//! wide as the typeset formula. gpui's wrapper treats those as glue, so the
//! stand-in wraps as one unbreakable word with the prose around it. The
//! renderer then paints the formula's raster over it, on the text baseline.
//! Selection, find and copy keep working on the one flat text model.
//!
//! Two caches, both per thread (the UI thread):
//! - [`metrics`]: source + style → size in em. Pure (no theme, no window), so
//!   layout and the offscreen find counts can ask it freely.
//! - [`raster`]: adds pixel size, scale factor and color, and holds the
//!   tinted `RenderImage`. LRU-bounded; evicted images are handed back so the
//!   painter can drop them from the GPU atlas.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Hsla, RenderImage, Rgba};
use ratex_types::color::Color;
use ratex_types::display_item::DisplayList;
use ratex_types::math_style::MathStyle;

use super::parser::MathKind;

/// Formula size relative to the surrounding text: KaTeX's `.katex` scale,
/// which evens out the KaTeX fonts' smaller x-height against UI sans.
pub const MATH_SCALE: f32 = 1.21;
/// Transparent margin around each raster (logical px), so antialiased edges
/// and glyphs overhanging their advance box are not cut off.
pub const RASTER_PAD: f32 = 2.0;
/// Air above and below a display formula (logical px).
pub const DISPLAY_PAD_Y: f32 = 6.0;
/// Longer sources are not typeset: they stay as text (and keep a runaway
/// stream from stalling a frame in layout).
const MAX_SOURCE_LEN: usize = 8 * 1024;
const MAX_METRICS: usize = 4096;
const MAX_RASTERS: usize = 384;

/// A typeset formula's box, in em of the math font size: advance width,
/// height above the baseline and depth below it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MathMetrics {
    pub width: f32,
    pub height: f32,
    pub depth: f32,
}

/// One formula rasterized for the screen.
pub struct Raster {
    pub image: Arc<RenderImage>,
    /// Logical size, padding included.
    pub width: f32,
    pub height: f32,
}

#[derive(Default)]
struct Caches {
    metrics: HashMap<(String, MathKind), Option<MathMetrics>>,
    rasters: HashMap<RasterKey, (Option<Arc<RenderImage>>, u64)>,
    clock: u64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct RasterKey {
    tex: String,
    kind: MathKind,
    /// Math font size and scale factor, as f32 bits.
    em_px: u32,
    scale: u32,
    rgba: [u8; 4],
}

thread_local! {
    static CACHES: RefCell<Caches> = RefCell::new(Caches::default());
}

/// The size of `tex` typeset in `kind`'s style, or `None` when RaTeX cannot
/// parse or lay it out — the caller then shows the source as code.
pub fn metrics(tex: &str, kind: MathKind) -> Option<MathMetrics> {
    CACHES.with(|caches| {
        let mut caches = caches.borrow_mut();
        if let Some(hit) = caches.metrics.get(&(tex.to_string(), kind)) {
            return *hit;
        }
        let measured = typeset(tex, kind, Color::BLACK).map(|list| MathMetrics {
            width: list.width as f32,
            height: list.height as f32,
            depth: list.depth as f32,
        });
        if caches.metrics.len() >= MAX_METRICS {
            caches.metrics.clear();
        }
        caches.metrics.insert((tex.to_string(), kind), measured);
        measured
    })
}

/// `tex` rasterized at `em_px` logical px per em and `scale` device pixels per
/// logical px, its glyphs painted `color` on transparency. Images pushed out
/// of the cache land in `evicted`; drop them from the atlas.
pub fn raster(
    tex: &str,
    kind: MathKind,
    em_px: f32,
    scale: f32,
    color: Hsla,
    evicted: &mut Vec<Arc<RenderImage>>,
) -> Option<Raster> {
    let rgba = rgba8(color);
    let key = RasterKey {
        tex: tex.to_string(),
        kind,
        em_px: em_px.to_bits(),
        scale: scale.to_bits(),
        rgba,
    };
    let image = CACHES.with(|caches| {
        let mut caches = caches.borrow_mut();
        caches.clock += 1;
        let now = caches.clock;
        if let Some((image, used)) = caches.rasters.get_mut(&key) {
            *used = now;
            return image.clone();
        }
        let image = rasterize(tex, kind, em_px, scale, rgba);
        if caches.rasters.len() >= MAX_RASTERS {
            // Evict the least recently used quarter in one sweep.
            let mut ages: Vec<u64> = caches.rasters.values().map(|(_, used)| *used).collect();
            ages.sort_unstable();
            let cutoff = ages[ages.len() / 4];
            caches.rasters.retain(|_, (image, used)| {
                let keep = *used > cutoff;
                if !keep && let Some(image) = image.take() {
                    evicted.push(image);
                }
                keep
            });
        }
        caches.rasters.insert(key, (image.clone(), now));
        image
    })?;
    let size = image.size(0);
    Some(Raster {
        width: size.width.0 as f32 / scale,
        height: size.height.0 as f32 / scale,
        image,
    })
}

/// Parse + lay out, refusing input RaTeX rejects. RaTeX is young and fed
/// model output, so a panic inside it is contained to this one formula.
fn typeset(tex: &str, kind: MathKind, color: Color) -> Option<DisplayList> {
    if tex.trim().is_empty() || tex.len() > MAX_SOURCE_LEN {
        return None;
    }
    std::panic::catch_unwind(|| {
        let nodes = ratex_parser::parse(tex).ok()?;
        let options = ratex_layout::LayoutOptions {
            style: match kind {
                MathKind::Display => MathStyle::Display,
                MathKind::Inline => MathStyle::Text,
            },
            color,
            ..Default::default()
        };
        let root = ratex_layout::layout(&nodes, &options);
        ratex_layout::validate_layout(&root).ok()?;
        let list = ratex_layout::to_display_list(&root);
        let finite = [list.width, list.height, list.depth]
            .iter()
            .all(|v| v.is_finite());
        (finite && list.width > 0.0).then_some(list)
    })
    .ok()
    .flatten()
}

fn rasterize(
    tex: &str,
    kind: MathKind,
    em_px: f32,
    scale: f32,
    [r, g, b, a]: [u8; 4],
) -> Option<Arc<RenderImage>> {
    let color = Color::new(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        f32::from(a) / 255.0,
    );
    let list = typeset(tex, kind, color)?;
    let options = ratex_render::RenderOptions {
        font_size: em_px,
        padding: RASTER_PAD,
        background_color: Color::new(0.0, 0.0, 0.0, 0.0),
        device_pixel_ratio: scale,
        ..Default::default()
    };
    let png = std::panic::catch_unwind(|| ratex_render::render_to_png(&list, &options))
        .ok()?
        .ok()?;
    let mut pixels = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    // gpui images are straight-alpha BGRA.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
}

fn rgba8(color: Hsla) -> [u8; 4] {
    let Rgba { r, g, b, a } = color.into();
    [r, g, b, a].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Widths (px) of the two glue characters in the element's prose font.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlueWidths {
    /// U+00A0 NO-BREAK SPACE.
    pub nbsp: f32,
    /// U+202F NARROW NO-BREAK SPACE.
    pub narrow: f32,
}

pub const NBSP: char = '\u{00A0}';
pub const NARROW_NBSP: char = '\u{202F}';

/// The glue text standing in for a formula `width` px wide, and its actual
/// width. gpui's wrapper treats both characters as word glue, so the run
/// never breaks inside; combining the two widths gets within a fraction of a
/// pixel of most targets, and the painter centers the formula in whatever
/// remains. Always at least one character, so the formula has a text index.
pub fn glue(width: f32, widths: GlueWidths) -> (String, f32) {
    let GlueWidths { nbsp, narrow } = widths;
    if !(nbsp > 0.0 && narrow > 0.0 && width.is_finite()) {
        return (NBSP.to_string(), nbsp.max(0.0));
    }
    let width = width.max(0.0);
    let mut best = (1usize, 0usize, nbsp);
    let max_narrow = ((nbsp / narrow).ceil() as usize + 2).min(16);
    for n in 0..=max_narrow {
        let rest = width - n as f32 * narrow;
        let base = (rest / nbsp).floor().max(0.0) as usize;
        for w in [base, base + 1] {
            if w + n == 0 {
                continue;
            }
            let total = w as f32 * nbsp + n as f32 * narrow;
            if (total - width).abs() < (best.2 - width).abs() {
                best = (w, n, total);
            }
        }
    }
    let (w, n, total) = best;
    let mut text = String::with_capacity(w * 2 + n * 3);
    text.extend(std::iter::repeat_n(NBSP, w));
    text.extend(std::iter::repeat_n(NARROW_NBSP, n));
    (text, total)
}

/// The source as a reader would copy it back out of the chat.
pub fn copy_source(tex: &str, kind: MathKind) -> String {
    match kind {
        MathKind::Inline => format!("${tex}$"),
        MathKind::Display => format!("$${tex}$$"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTHS: GlueWidths = GlueWidths {
        nbsp: 3.9,
        narrow: 2.6,
    };

    #[test]
    fn glue_lands_close_to_the_target_width() {
        for target in [0.0, 1.0, 5.5, 7.3, 12.0, 33.3, 97.1, 401.7] {
            let (text, width) = glue(target, WIDTHS);
            assert!(!text.is_empty());
            let measured: f32 = text
                .chars()
                .map(|c| {
                    if c == NBSP {
                        WIDTHS.nbsp
                    } else {
                        WIDTHS.narrow
                    }
                })
                .sum();
            assert!((measured - width).abs() < 1e-3, "reported width is real");
            if target >= WIDTHS.narrow {
                assert!((width - target).abs() <= 0.66, "{target} → {width}");
            }
        }
    }

    #[test]
    fn glue_is_only_unbreakable_space() {
        let (text, _) = glue(50.0, WIDTHS);
        assert!(text.chars().all(|c| c == NBSP || c == NARROW_NBSP));
    }

    #[test]
    fn metrics_measure_valid_tex_and_reject_broken_tex() {
        let inline = metrics(r"\frac{a}{b}", MathKind::Inline).expect("typesets");
        let display = metrics(r"\frac{a}{b}", MathKind::Display).expect("typesets");
        assert!(inline.width > 0.0 && inline.height > 0.0);
        // Display style sets fractions at full size, text style shrinks them.
        assert!(display.height > inline.height);
        assert_eq!(metrics(r"\frac{a", MathKind::Inline), None);
        assert_eq!(metrics(r"\frac{a}", MathKind::Inline), None);
        assert_eq!(metrics("   ", MathKind::Inline), None);
    }

    #[test]
    fn copy_source_restores_the_delimiters() {
        assert_eq!(copy_source("x^2", MathKind::Inline), "$x^2$");
        assert_eq!(copy_source("x^2", MathKind::Display), "$$x^2$$");
    }
}
