//! How big a visual preview in a chat reply is: images (Markdown, inline,
//! image file references, SVG), a PDF's pages and a video's frame. Tool-row
//! thumbnails and csv/tsv tables keep their own sizes.
//!
//! The rule is shared with the iPhone app through the `sizing` section of
//! `protocol/vectors/image-sources-v1.json` and
//! `protocol/vectors/file-previews-v1.json` (the same rows in both; its
//! `description` is the spec):
//!
//! - A preview is always the reply column's width, a small image or an SVG
//!   included (upscaled).
//! - Landscape, square and up to exactly [`CROP`] (4:5): its natural aspect.
//! - Taller: [`CROP`] tall, the content drawn whole-width from its top edge
//!   and cut off below; a click still opens all of it.
//! - A PDF lays its first pages side by side in one row ([`pdf_row`]): up
//!   to [`PDF_ROW_MAX`], as many as keep every page at least
//!   [`PDF_PAGE_MIN`] wide, all one height so the row fills the column,
//!   never cropped. A single page shows like an image (column wide, the
//!   4:5 crop). Pages left over show as a "+N" badge on the last one.

/// The tallest a preview gets, as width : height (4:5).
pub const CROP: (f32, f32) = (4.0, 5.0);
/// The most pages a PDF's row shows.
pub const PDF_ROW_MAX: usize = 4;
/// The narrowest a page in a PDF's row gets (points); a row that would
/// make one narrower shows fewer pages.
pub const PDF_PAGE_MIN: f32 = 150.0;
/// Space between the pages of a PDF's row (points).
pub const PDF_GAP: f32 = 8.0;
/// How far under [`PDF_PAGE_MIN`] a page may come out and still count as
/// wide enough (float slack, the same on every platform).
const PDF_PAGE_MIN_SLACK: f32 = 0.001;

/// A preview's size in a column, in logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewSize {
    pub width: f32,
    /// The preview's height: the content's, or the [`CROP`] cap.
    pub height: f32,
    /// The content is taller than the preview: drawn from its top edge,
    /// the rest cut off.
    pub cropped: bool,
    /// The height the whole content draws at (column wide).
    pub content_height: f32,
}

impl PreviewSize {
    const NONE: Self = Self {
        width: 0.0,
        height: 0.0,
        cropped: false,
        content_height: 0.0,
    };

    /// The preview's width : height, for layouts that size by aspect.
    pub fn aspect(&self) -> f32 {
        if self.height > 0.0 {
            self.width / self.height
        } else {
            1.0
        }
    }

    /// The whole content's width : height.
    pub fn content_aspect(&self) -> f32 {
        if self.content_height > 0.0 {
            self.width / self.content_height
        } else {
            1.0
        }
    }
}

/// The size of a preview of content `natural_w` × `natural_h` (only its
/// aspect matters) in a column `column` wide.
pub fn preview_size(column: f32, natural_w: f32, natural_h: f32) -> PreviewSize {
    if !(column > 0.0 && natural_w > 0.0 && natural_h > 0.0) {
        return PreviewSize::NONE;
    }
    let content_height = column * natural_h / natural_w;
    let cap = column * CROP.1 / CROP.0;
    let cropped = content_height > cap;
    PreviewSize {
        width: column,
        height: if cropped { cap } else { content_height },
        cropped,
        content_height,
    }
}

/// A PDF's row of pages in a column: what [`pdf_row`] decides.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfRow {
    /// Each shown page's size, in order. A row of two or more: all
    /// [`PdfRow::height`] tall, never cropped. A single page: as an image
    /// ([`preview_size`], so a tall one is cropped to 4:5, its top kept).
    pub pages: Vec<PreviewSize>,
    /// The shared height of the shown pages (0 when none show).
    pub height: f32,
    /// Pages the document has past the shown ones.
    pub hidden: usize,
}

impl PdfRow {
    pub fn shown(&self) -> usize {
        self.pages.len()
    }

    /// The badge on the last shown page when some are hidden: "+N".
    pub fn badge(&self) -> Option<String> {
        (self.hidden > 0).then(|| format!("+{}", self.hidden))
    }
}

/// The row a PDF of `pages` pages shows in a column `column` wide, given
/// the aspects (width / height) of its first pages, in order (those known:
/// a page with no positive aspect ends the list).
///
/// `n` = the largest count up to min(pages, known aspects, [`PDF_ROW_MAX`])
/// whose row keeps every page at least [`PDF_PAGE_MIN`] wide, where the
/// row's shared height is h = (column − (n − 1) · [`PDF_GAP`]) / Σ aspectᵢ
/// and page i is h · aspectᵢ wide (so the row fills the column exactly).
/// No such count of two or more: one page, shown like an image.
pub fn pdf_row(column: f32, aspects: &[f32], pages: usize) -> PdfRow {
    let known = aspects
        .iter()
        .take_while(|aspect| aspect.is_finite() && **aspect > 0.0)
        .count();
    let max = pages.min(known).min(PDF_ROW_MAX);
    if column.is_nan() || column <= 0.0 || max == 0 {
        // Nothing to show: no badge either.
        return PdfRow {
            pages: Vec::new(),
            height: 0.0,
            hidden: 0,
        };
    }
    for n in (2..=max).rev() {
        let shown = &aspects[..n];
        let height = (column - (n - 1) as f32 * PDF_GAP) / shown.iter().sum::<f32>();
        if shown
            .iter()
            .all(|aspect| height * aspect >= PDF_PAGE_MIN - PDF_PAGE_MIN_SLACK)
        {
            return PdfRow {
                pages: shown
                    .iter()
                    .map(|aspect| PreviewSize {
                        width: height * aspect,
                        height,
                        cropped: false,
                        content_height: height,
                    })
                    .collect(),
                height,
                hidden: pages - n,
            };
        }
    }
    let page = preview_size(column, aspects[0], 1.0);
    PdfRow {
        pages: vec![page],
        height: page.height,
        hidden: pages - 1,
    }
}

/// The widths (points) to draw a PDF's first pages at so each stays sharp
/// in any column up to `column`, given their aspects (as [`pdf_row`]): a
/// page's widest across every row it can be in. A page's width in a row of
/// a given count only grows with the column, and a narrower column may
/// show fewer, wider pages; so this is, per page, the most over every count
/// that shows it (one: the whole column), whether or not [`PDF_PAGE_MIN`]
/// would allow it.
pub fn pdf_draw_widths(column: f32, aspects: &[f32], pages: usize) -> Vec<f32> {
    let known = aspects
        .iter()
        .take_while(|aspect| aspect.is_finite() && **aspect > 0.0)
        .count();
    let max = pages.min(known).min(PDF_ROW_MAX);
    if column.is_nan() || column <= 0.0 {
        return vec![0.0; max];
    }
    (0..max)
        .map(|ix| {
            ((ix + 1).max(2)..=max)
                .map(|n| {
                    let height =
                        (column - (n - 1) as f32 * PDF_GAP) / aspects[..n].iter().sum::<f32>();
                    height * aspects[ix]
                })
                .chain((ix == 0).then_some(column))
                .fold(0.0, f32::max)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn sections() -> [Value; 2] {
        let parse = |text: &str| -> Value {
            let all: Value = serde_json::from_str(text).unwrap();
            all["sizing"].clone()
        };
        [
            parse(include_str!(
                "../../../../protocol/vectors/image-sources-v1.json"
            )),
            parse(include_str!(
                "../../../../protocol/vectors/file-previews-v1.json"
            )),
        ]
    }

    #[test]
    fn both_vector_files_carry_the_same_rule() {
        let [images, files] = sections();
        assert!(images.is_object());
        assert_eq!(images, files);
    }

    #[test]
    fn shared_caps() {
        for sizing in sections() {
            let caps = &sizing["caps"];
            assert_eq!(caps["crop"]["width"].as_f64().unwrap() as f32, CROP.0);
            assert_eq!(caps["crop"]["height"].as_f64().unwrap() as f32, CROP.1);
            let row = &caps["pdfRow"];
            assert_eq!(row["maxPages"].as_u64().unwrap() as usize, PDF_ROW_MAX);
            assert_eq!(row["minPageWidth"].as_f64().unwrap() as f32, PDF_PAGE_MIN);
            assert_eq!(row["gap"].as_f64().unwrap() as f32, PDF_GAP);
        }
    }

    #[test]
    fn shared_sizes() {
        for sizing in sections() {
            let cases = sizing["cases"].as_array().unwrap();
            assert!(cases.len() >= 10);
            for case in cases {
                let name = case["name"].as_str().unwrap();
                let num = |v: &Value| v.as_f64().unwrap() as f32;
                let got = preview_size(
                    num(&case["column"]),
                    num(&case["natural"]["width"]),
                    num(&case["natural"]["height"]),
                );
                let expect = &case["expect"];
                let close = |a: f32, b: &Value| (a - num(b)).abs() < 0.01;
                assert!(close(got.width, &expect["width"]), "{name}: {got:?}");
                assert!(close(got.height, &expect["height"]), "{name}: {got:?}");
                assert!(
                    close(got.content_height, &expect["contentHeight"]),
                    "{name}: {got:?}"
                );
                assert_eq!(got.cropped, expect["cropped"].as_bool().unwrap(), "{name}");
            }
        }
    }

    #[test]
    fn shared_pdf_rows() {
        for sizing in sections() {
            let cases = sizing["pdfRow"].as_array().unwrap();
            assert!(cases.len() >= 8);
            for case in cases {
                let name = case["name"].as_str().unwrap();
                let num = |v: &Value| v.as_f64().unwrap() as f32;
                let aspects: Vec<f32> = case["aspects"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(num)
                    .collect();
                let row = pdf_row(
                    num(&case["column"]),
                    &aspects,
                    case["pages"].as_u64().unwrap() as usize,
                );
                let expect = &case["expect"];
                let close = |a: f32, b: &Value| (a - num(b)).abs() < 0.01;
                assert_eq!(
                    row.shown() as u64,
                    expect["shown"].as_u64().unwrap(),
                    "{name}"
                );
                assert!(close(row.height, &expect["height"]), "{name}: {row:?}");
                let widths = expect["widths"].as_array().unwrap();
                assert_eq!(widths.len(), row.shown(), "{name}");
                for (page, width) in row.pages.iter().zip(widths) {
                    assert!(close(page.width, width), "{name}: {row:?}");
                    assert!(close(page.height, &expect["height"]), "{name}: {row:?}");
                }
                let first = row.pages.first();
                assert_eq!(
                    first.is_some_and(|page| page.cropped),
                    expect["cropped"].as_bool().unwrap(),
                    "{name}"
                );
                assert!(
                    close(
                        first.map_or(0.0, |page| page.content_height),
                        &expect["contentHeight"]
                    ),
                    "{name}: {row:?}"
                );
                assert_eq!(
                    row.hidden as u64,
                    expect["hidden"].as_u64().unwrap(),
                    "{name}"
                );
                assert_eq!(row.badge().as_deref(), expect["badge"].as_str(), "{name}");
            }
        }
    }

    #[test]
    fn a_row_fills_the_column_exactly() {
        let letter = 612.0 / 792.0;
        for column in [300.0, 343.0, 480.0, 600.0, 640.0, 900.0] {
            let row = pdf_row(column, &[letter, 1.0 / letter, letter, 2.0], 9);
            if row.shown() < 2 {
                continue;
            }
            let gaps = (row.shown() - 1) as f32 * PDF_GAP;
            let total: f32 = row.pages.iter().map(|page| page.width).sum::<f32>() + gaps;
            assert!((total - column).abs() < 0.01, "{column}: {row:?}");
            assert!(
                row.pages
                    .iter()
                    .all(|page| page.width >= PDF_PAGE_MIN - 0.01)
            );
        }
    }

    #[test]
    fn drawn_pages_stay_sharp_in_any_narrower_column() {
        let aspects = [612.0 / 792.0, 792.0 / 612.0, 612.0 / 792.0, 1.0];
        let drawn = pdf_draw_widths(768.0, &aspects, 7);
        assert_eq!(drawn.len(), 4);
        assert_eq!(drawn[0], 768.0);
        for column in (100..=768).step_by(7) {
            let row = pdf_row(column as f32, &aspects, 7);
            for (page, drawn) in row.pages.iter().zip(&drawn) {
                assert!(page.width <= drawn + 0.01, "{column}: {row:?} {drawn}");
            }
        }
        assert_eq!(pdf_draw_widths(600.0, &aspects, 1), vec![600.0]);
        assert!(pdf_draw_widths(600.0, &[], 3).is_empty());
    }

    #[test]
    fn aspects_follow_the_size() {
        let wide = preview_size(600.0, 2000.0, 1000.0);
        assert_eq!(wide.aspect(), 2.0);
        assert_eq!(wide.content_aspect(), 2.0);
        let tall = preview_size(600.0, 400.0, 800.0);
        assert_eq!(tall.aspect(), 0.8);
        assert_eq!(tall.content_aspect(), 0.5);
        assert_eq!(preview_size(0.0, 1.0, 1.0).aspect(), 1.0);
    }
}
