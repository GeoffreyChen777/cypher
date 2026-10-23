//! What the Files surface shows for a file that is not plain text: images
//! (fetched as bytes through `ReadWorkspaceFileChunk`), SVG renders, an info
//! card for other binaries, and a hex dump of a binary's head. (HTML pages
//! render in [`super::web_view`].)

use std::sync::Arc;

use base64::Engine as _;
use gpui::{AnyElement, Image, ImageFormat, ObjectFit, SharedString, div, img, prelude::*, px};

use cypher_proto::WorkspaceFileChunk;
use cypher_rpc::{RpcError, methods};

use crate::kit::icons::{self, icon};
use crate::kit::theme::{MonoStyled as _, Theme};

/// Images larger than this are not fetched for a preview.
pub(super) const IMAGE_LIMIT: u64 = 64 * 1024 * 1024;
/// The hex view shows a binary's first bytes only.
pub(super) const HEX_LIMIT: usize = 64 * 1024;
pub(super) const HEX_ROW_HEIGHT: f32 = 18.0;

/// How a file opens, by name. Text-or-binary is still decided by content
/// for everything but images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FileKind {
    /// A raster image gpui can decode.
    Image(ImageFormat),
    /// Text that also renders as an image.
    Svg,
    Markdown,
    /// Text that renders as a web page.
    Html,
    /// Text, or a binary the info card describes.
    Other,
}

impl FileKind {
    pub(super) fn for_path(path: &str) -> Self {
        match extension(path).as_deref() {
            Some("png") => Self::Image(ImageFormat::Png),
            Some("jpg" | "jpeg") => Self::Image(ImageFormat::Jpeg),
            Some("gif") => Self::Image(ImageFormat::Gif),
            Some("webp") => Self::Image(ImageFormat::Webp),
            Some("bmp") => Self::Image(ImageFormat::Bmp),
            Some("tif" | "tiff") => Self::Image(ImageFormat::Tiff),
            Some("ico") => Self::Image(ImageFormat::Ico),
            Some("svg") => Self::Svg,
            Some("html" | "htm" | "xhtml") => Self::Html,
            _ if super::is_markdown(path) => Self::Markdown,
            _ => Self::Other,
        }
    }

    /// Has a rendered view beside its source.
    pub(super) fn previews(self) -> bool {
        match self {
            Self::Svg | Self::Markdown => true,
            Self::Html => super::web_view::AVAILABLE,
            Self::Image(_) | Self::Other => false,
        }
    }
}

fn extension(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let dot = name.rfind('.').filter(|dot| *dot > 0)?;
    Some(name[dot + 1..].to_ascii_lowercase())
}

/// The tree's glyph for a file name.
pub(super) fn file_icon(name: &str) -> &'static str {
    match FileKind::for_path(name) {
        FileKind::Image(_) | FileKind::Svg => icons::GALLERY,
        _ => icons::DOCUMENT,
    }
}

/// A human label for a binary file's type, by extension.
pub(super) fn kind_label(path: &str) -> &'static str {
    match extension(path).as_deref() {
        Some("pdf") => "PDF document",
        Some("zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst") => "Archive",
        Some("mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "opus" | "aiff") => "Audio",
        Some("mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v") => "Video",
        Some("ttf" | "otf" | "woff" | "woff2") => "Font",
        Some("heic" | "heif" | "avif" | "psd" | "raw" | "dng") => "Image",
        Some("exe" | "dll" | "so" | "dylib" | "o" | "a" | "wasm" | "class") => "Compiled binary",
        Some("sqlite" | "db" | "sqlite3") => "Database",
        Some("doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "key" | "pages" | "numbers") => {
            "Office document"
        }
        _ => "Binary file",
    }
}

/// Non-text content of one open file.
#[derive(Clone, Default)]
pub(super) enum Media {
    #[default]
    Idle,
    Loading {
        received: u64,
        total: u64,
    },
    Image {
        image: Arc<Image>,
        /// Pixel size, when the header decodes (not for SVG).
        dims: Option<(u32, u32)>,
    },
    Hex {
        bytes: Arc<Vec<u8>>,
        total: u64,
    },
    Failed(SharedString),
}

/// Image zoom: fit the pane, or one image pixel per point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Zoom {
    #[default]
    Fit,
    Actual,
}

/// Fetch a file's bytes slice by slice: the whole file when it is at most
/// `limit` (else an error), or with `head` just its first `limit` bytes.
/// `progress` hears `(received, total)` after each slice. A file rewritten
/// mid-read fails rather than splicing two versions.
pub(super) async fn read_bytes(
    engine: &crate::state::EngineHandle,
    params: serde_json::Map<String, serde_json::Value>,
    limit: u64,
    head: bool,
    mut progress: impl FnMut(u64, u64),
) -> Result<(Vec<u8>, u64), SharedString> {
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    let mut stamp: Option<(u64, Option<i64>)> = None;
    loop {
        let mut params = params.clone();
        params.insert("offset".into(), offset.into());
        let value = engine
            .client()
            .call(
                methods::READ_WORKSPACE_FILE_CHUNK,
                serde_json::Value::Object(params),
            )
            .await
            .map_err(|err| match err {
                RpcError::UnknownMethod(_) => {
                    SharedString::from("This device's engine needs an update to preview this file.")
                }
                other => super::rpc_message(&other),
            })?;
        let chunk: WorkspaceFileChunk =
            serde_json::from_value(value).map_err(|e| SharedString::from(e.to_string()))?;
        if chunk.bytes > limit && !head {
            return Err(format!(
                "Too large to preview ({}).",
                super::human_bytes(chunk.bytes)
            )
            .into());
        }
        let now = (chunk.bytes, chunk.modified_ms);
        if stamp.is_some_and(|before| before != now) {
            return Err("The file changed while loading — refresh.".into());
        }
        stamp = Some(now);
        let data = base64::engine::general_purpose::STANDARD
            .decode(chunk.data.as_bytes())
            .map_err(|e| SharedString::from(e.to_string()))?;
        bytes.extend_from_slice(&data);
        progress(bytes.len() as u64, chunk.bytes);
        if chunk.done || bytes.len() as u64 >= limit {
            bytes.truncate(limit as usize);
            return Ok((bytes, chunk.bytes));
        }
        if chunk.next_offset <= offset {
            return Err("The file could not be read.".into());
        }
        offset = chunk.next_offset;
    }
}

/// An image's pixel size from its header (no full decode).
pub(super) fn image_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// One classic hex dump row: offset, `per_row` bytes (split in halves),
/// printable ASCII.
pub(super) fn hex_line(bytes: &[u8], row: usize, per_row: usize) -> String {
    let start = row * per_row;
    let chunk = &bytes[start.min(bytes.len())..(start + per_row).min(bytes.len())];
    let mut line = format!("{start:08x}  ");
    for i in 0..per_row {
        match chunk.get(i) {
            Some(byte) => line.push_str(&format!("{byte:02x} ")),
            None => line.push_str("   "),
        }
        if i + 1 == per_row / 2 {
            line.push(' ');
        }
    }
    line.push_str(" |");
    line.extend(chunk.iter().map(|&b| {
        if b.is_ascii_graphic() || b == b' ' {
            b as char
        } else {
            '.'
        }
    }));
    line.push('|');
    line
}

/// Bytes per hex row for a pane `width` px wide (16 when they fit).
pub(super) fn hex_per_row(width: f32) -> usize {
    if width >= 600.0 { 16 } else { 8 }
}

/// SVG previews: gpui rasterizes an SVG at its declared size, so a 16px
/// icon would preview as a speck (and stretching it blurs). Rewrite the root
/// element's width/height to at least [`SVG_PREVIEW_MIN`] on the long side,
/// keeping (or adding) the viewBox so the drawing scales with it. Anything
/// unparseable passes through unchanged. Also returns the declared size.
pub(super) const SVG_PREVIEW_MIN: f32 = 256.0;

pub(super) fn svg_preview_bytes(source: &str) -> (Vec<u8>, Option<(f32, f32)>) {
    let unchanged = || (source.as_bytes().to_vec(), None);
    let Some(open) = source.find("<svg") else {
        return unchanged();
    };
    let Some(close) = source[open..].find('>').map(|i| open + i) else {
        return unchanged();
    };
    let tag = &source[open..close];
    let attr = |name: &str| -> Option<(std::ops::Range<usize>, String)> {
        let mut from = 0;
        while let Some(i) = tag[from..].find(name) {
            let at = from + i;
            from = at + name.len();
            let before = tag[..at].chars().next_back();
            if !before.is_some_and(char::is_whitespace) {
                continue;
            }
            let rest = tag[at + name.len()..].trim_start();
            let Some(rest) = rest.strip_prefix('=') else {
                continue;
            };
            let rest = rest.trim_start();
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let value_start = tag.len() - rest.len() + 1;
            let len = rest[1..].find(quote)?;
            let range = open + value_start..open + value_start + len;
            return Some((range, rest[1..1 + len].to_string()));
        }
        None
    };
    let length = |value: &str| -> Option<f32> {
        let value = value.trim();
        let (number, scale) = if let Some(n) = value.strip_suffix("px") {
            (n, 1.0)
        } else if let Some(n) = value.strip_suffix("em") {
            (n, 16.0)
        } else {
            (value, 1.0)
        };
        number
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|n| *n > 0.0)
            .map(|n| n * scale)
    };
    let (Some((w_range, w_value)), Some((h_range, h_value))) = (attr("width"), attr("height"))
    else {
        return unchanged();
    };
    let (Some(w), Some(h)) = (length(&w_value), length(&h_value)) else {
        return unchanged();
    };
    let declared = Some((w, h));
    let scale = (SVG_PREVIEW_MIN / w.max(h)).min(16.0);
    if scale <= 1.0 {
        return (source.as_bytes().to_vec(), declared);
    }
    let mut out = source.to_string();
    // Later range first so the earlier one's offsets stay valid.
    let mut edits = [(w_range, w * scale), (h_range, h * scale)];
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, value) in edits {
        out.replace_range(range, &format!("{value}"));
    }
    if attr("viewBox").is_none() {
        out.insert_str(open + 4, &format!(" viewBox=\"0 0 {w} {h}\""));
    }
    (out.into_bytes(), declared)
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Webp => "WebP",
        ImageFormat::Gif => "GIF",
        ImageFormat::Svg => "SVG",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Ico => "ICO",
        _ => "Image",
    }
}

/// "Oct 22, 2026 at 14:03" in local time.
pub(super) fn modified_label(ms: i64) -> Option<String> {
    use chrono::TimeZone as _;
    let at = chrono::Local.timestamp_millis_opt(ms).single()?;
    Some(at.format("%b %-d, %Y at %H:%M").to_string())
}

/// The image view: fitted (never upscaled) or at its pixel size in a
/// scroller, over a quiet backdrop, with a one-line footer of facts.
pub(super) fn render_image(
    path: &str,
    image: &Arc<Image>,
    dims: Option<(u32, u32)>,
    bytes: u64,
    zoom: Zoom,
    theme: &Theme,
) -> AnyElement {
    let actual = matches!(zoom, Zoom::Actual) && dims.is_some();
    let picture = img(image.clone());
    let stage = if actual {
        let (w, h) = dims.unwrap_or_default();
        div()
            .id(SharedString::from(format!("files-image-actual:{path}")))
            .size_full()
            .overflow_scroll()
            .child(
                div().p(px(16.0)).child(
                    picture
                        .w(px(w as f32))
                        .h(px(h as f32))
                        .object_fit(ObjectFit::Fill),
                ),
            )
            .into_any_element()
    } else {
        div()
            .size_full()
            .p(px(16.0))
            .child(picture.size_full().object_fit(ObjectFit::ScaleDown))
            .into_any_element()
    };
    let mut facts = vec![format_name(image.format).to_string()];
    if let Some((w, h)) = dims {
        facts.push(format!("{w} × {h}"));
    }
    facts.push(super::human_bytes(bytes));
    if actual {
        facts.push("100%".into());
    }
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_1()
                .min_h_0()
                .bg(crate::kit::theme::ink(0.025))
                .child(stage),
        )
        .child(footer(facts.join("  ·  "), theme))
        .into_any_element()
}

/// The HTML view: the page edge to edge, short of the dock's floating rail
/// on the right (`right`) — a native page would cover it.
pub(super) fn render_page(page: AnyElement, right: f32) -> AnyElement {
    div()
        .size_full()
        .pr(px(right))
        .child(page)
        .into_any_element()
}

fn footer(text: String, theme: &Theme) -> AnyElement {
    div()
        .flex_none()
        .h(px(26.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .border_t_1()
        .border_color(theme.border)
        .text_size(px(11.0))
        .text_color(theme.text_muted)
        .child(SharedString::from(text))
        .into_any_element()
}

/// One action button on the info card.
pub(super) fn card_button(
    id: &'static str,
    label: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(26.0))
        .px(px(10.0))
        .rounded(px(7.0))
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(px(12.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text)
        .bg(crate::kit::theme::wash(0.08))
        .hover(|s| s.bg(crate::kit::theme::wash(0.14)))
        .child(SharedString::from(label))
}

/// The binary info card's static part: glyph, name, type, size, date.
pub(super) fn info_card(
    path: &str,
    bytes: u64,
    modified_ms: Option<i64>,
    buttons: Vec<AnyElement>,
    theme: &Theme,
) -> AnyElement {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    let mut facts = format!("{} · {}", kind_label(path), super::human_bytes(bytes));
    if let Some(label) = modified_ms.and_then(modified_label) {
        facts.push_str(&format!("\nModified {label}"));
    }
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.0))
        .p(px(24.0))
        .child(
            icon(file_icon(&name))
                .size(px(40.0))
                .text_color(theme.text_faint),
        )
        .child(
            div()
                .max_w_full()
                .truncate()
                .text_size(px(13.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(SharedString::from(name)),
        )
        .child(
            div()
                .text_center()
                .text_size(px(11.5))
                .line_height(px(17.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(facts)),
        )
        .when(!buttons.is_empty(), |el| {
            el.child(
                div()
                    .mt(px(6.0))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(8.0))
                    .children(buttons),
            )
        })
        .into_any_element()
}

/// The hex view: virtualized rows, formatted as they scroll into view.
pub(super) fn render_hex(
    bytes: Arc<Vec<u8>>,
    total: u64,
    per_row: usize,
    scroll: &gpui::UniformListScrollHandle,
    theme: &Theme,
) -> AnyElement {
    let shown = bytes.len() as u64;
    let note = if shown < total {
        format!(
            "First {} of {}",
            super::human_bytes(shown),
            super::human_bytes(total)
        )
    } else {
        super::human_bytes(total)
    };
    let color = theme.text.opacity(0.85);
    let count = bytes.len().div_ceil(per_row);
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(
            div().flex_1().min_h_0().child(
                gpui::uniform_list("files-hex", count, move |range, _window, _cx| {
                    range
                        .map(|row| {
                            div()
                                .h(px(HEX_ROW_HEIGHT))
                                .px(px(12.0))
                                .flex()
                                .items_center()
                                .whitespace_nowrap()
                                .text_color(color)
                                .child(SharedString::from(hex_line(&bytes, row, per_row)))
                                .into_any_element()
                        })
                        .collect()
                })
                .track_scroll(scroll)
                .size_full()
                .py(px(6.0))
                .mono(theme)
                .text_size(px(11.5)),
            ),
        )
        .child(footer(note, theme))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_follow_extensions_case_insensitively() {
        assert_eq!(
            FileKind::for_path("a/B.PNG"),
            FileKind::Image(ImageFormat::Png)
        );
        assert_eq!(
            FileKind::for_path("x.jpeg"),
            FileKind::Image(ImageFormat::Jpeg)
        );
        assert_eq!(FileKind::for_path("logo.svg"), FileKind::Svg);
        assert_eq!(FileKind::for_path("README.md"), FileKind::Markdown);
        assert_eq!(FileKind::for_path("site/Index.HTML"), FileKind::Html);
        assert_eq!(FileKind::for_path("page.htm"), FileKind::Html);
        assert_eq!(FileKind::for_path("main.rs"), FileKind::Other);
        assert_eq!(FileKind::for_path(".png"), FileKind::Other);
        assert_eq!(kind_label("doc.PDF"), "PDF document");
        assert_eq!(kind_label("blob"), "Binary file");
        assert!(FileKind::Svg.previews() && !FileKind::Other.previews());
    }

    #[test]
    fn hex_rows_show_offset_bytes_and_ascii() {
        let bytes = b"Hello, world!\x00\x01\x02\x03xyz";
        assert_eq!(
            hex_line(bytes, 0, 16),
            "00000000  48 65 6c 6c 6f 2c 20 77  6f 72 6c 64 21 00 01 02  |Hello, world!...|"
        );
        let last = hex_line(bytes, 1, 16);
        assert!(last.starts_with("00000010  03 78 79 7a "));
        assert!(last.ends_with("|.xyz|"));
        assert_eq!(
            hex_line(bytes, 1, 8),
            "00000008  6f 72 6c 64  21 00 01 02  |orld!...|"
        );
        assert_eq!(hex_per_row(800.0), 16);
        assert_eq!(hex_per_row(400.0), 8);
    }

    #[test]
    fn small_svgs_preview_larger_with_their_view_box() {
        let (bytes, dims) = svg_preview_bytes(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="1em" height="1em" viewBox="0 0 24 24"><g/></svg>"#,
        );
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(dims, Some((16.0, 16.0)));
        assert!(text.contains(r#"width="256""#), "{text}");
        assert!(text.contains(r#"height="256""#), "{text}");
        assert!(text.contains(r#"viewBox="0 0 24 24""#));

        let (bytes, _) =
            svg_preview_bytes(r#"<svg stroke-width="2" width='32' height='16'></svg>"#);
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            text.starts_with(
                r#"<svg viewBox="0 0 32 16" stroke-width="2" width='256' height='128'>"#
            ),
            "{text}"
        );

        // Large or size-less SVGs are left alone.
        let big = r#"<svg width="800" height="600"></svg>"#;
        assert_eq!(svg_preview_bytes(big).0, big.as_bytes());
        let bare = r#"<svg viewBox="0 0 10 10"></svg>"#;
        assert_eq!(svg_preview_bytes(bare), (bare.as_bytes().to_vec(), None));
    }

    #[test]
    fn image_headers_give_dimensions() {
        let mut png = Vec::new();
        image::RgbaImage::new(3, 2)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(image_dims(&png), Some((3, 2)));
        assert_eq!(image_dims(b"not an image"), None);
    }
}
