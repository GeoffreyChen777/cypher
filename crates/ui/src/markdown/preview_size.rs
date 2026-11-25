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
//! - A PDF stacks its first [`PDF_PAGES`] pages, each uncropped at its own
//!   aspect ([`PreviewSize::content_height`]), then a row "N more pages".

/// The tallest a preview gets, as width : height (4:5).
pub const CROP: (f32, f32) = (4.0, 5.0);
/// Pages a PDF preview shows before its "N more pages" row.
pub const PDF_PAGES: usize = 3;

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

/// A PDF of `pages` pages: how many show, and how many the row below them
/// counts.
pub fn pdf_pages(pages: usize) -> (usize, usize) {
    let shown = pages.min(PDF_PAGES);
    (shown, pages - shown)
}

/// The row under a PDF's shown pages, when some are left.
pub fn more_pages_label(more: usize) -> Option<String> {
    match more {
        0 => None,
        1 => Some("1 more page".into()),
        n => Some(format!("{n} more pages")),
    }
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
            assert_eq!(caps["pdfPages"].as_u64().unwrap() as usize, PDF_PAGES);
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
    fn shared_pdf_pages() {
        for sizing in sections() {
            for case in sizing["pdf"].as_array().unwrap() {
                let name = case["name"].as_str().unwrap();
                let (shown, more) = pdf_pages(case["pages"].as_u64().unwrap() as usize);
                let expect = &case["expect"];
                assert_eq!(shown as u64, expect["shown"].as_u64().unwrap(), "{name}");
                assert_eq!(more as u64, expect["more"].as_u64().unwrap(), "{name}");
                assert_eq!(
                    more_pages_label(more).as_deref(),
                    expect["label"].as_str(),
                    "{name}"
                );
            }
        }
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
