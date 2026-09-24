//! The plain-text attachment-ref transport: committed upload paths ride the
//! user message as a trailer (`…\n\nAttached files (local files — open them to
//! view):\n- /path`), which is what persists in the doc, what the agent reads,
//! and what every client parses back out to render thumbnails / file tiles.
//!
//! One implementation for every producer and consumer (the composer's
//! pre-upload path, the host's queue-first trailer, the UI parser, the Pi
//! fork prompt matcher) so the byte format can't drift between them.
//!
//! Wire compatibility: image-only sends keep the historical `Attached images`
//! header and `See the attached image(s).` body, byte-for-byte, so older
//! clients keep parsing them. Only a send carrying a non-image file uses the
//! `Attached files` header. Parsers accept both. Whether a ref renders as an
//! image is decided per path ([`is_image_path`]), never by the header.

use std::path::Path;

const IMAGES_HEADER: &str = "Attached images (local files — open them to view):";
const FILES_HEADER: &str = "Attached files (local files — open them to view):";
/// Body placeholder for an image-only send with no typed text.
pub const IMAGES_ONLY_TEXT: &str = "See the attached image(s).";
/// Body placeholder for a text-less send carrying any non-image file.
pub const FILES_ONLY_TEXT: &str = "See the attached file(s).";

/// Header prefixes (lowercase) a parser accepts; the header line must also
/// end with `):`.
const MARKER_PREFIXES: [&str; 2] = [
    "attached images (local files",
    "attached files (local files",
];

/// Image types the attachment pipeline previews as thumbnails (staging decode,
/// transcript read-back). Everything else is a plain file ref.
pub fn is_image_path(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "tif" | "tiff"
    )
}

/// Append the refs trailer for `paths` to `text` (identity when empty). A
/// text-less send gets the placeholder body.
pub fn with_refs(text: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return text.to_string();
    }
    let images_only = paths.iter().all(|p| is_image_path(p));
    let (header, placeholder) = if images_only {
        (IMAGES_HEADER, IMAGES_ONLY_TEXT)
    } else {
        (FILES_HEADER, FILES_ONLY_TEXT)
    };
    let body = if text.is_empty() { placeholder } else { text };
    let refs: Vec<String> = paths.iter().map(|p| format!("- {p}")).collect();
    format!("{body}\n\n{header}\n{}", refs.join("\n"))
}

/// Whether `body` is one of the text-less placeholders (hidden in bubbles).
pub fn is_placeholder_body(body: &str) -> bool {
    matches!(body.trim(), IMAGES_ONLY_TEXT | FILES_ONLY_TEXT)
}

/// Locate the trailer: a blank line, then a header line starting
/// (case-insensitive) with either accepted prefix and ending `):`. Returns
/// `(body_end, refs_start)` byte offsets.
pub fn find_marker(content: &str) -> Option<(usize, usize)> {
    let lower = content.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find("\n\nattached ") {
        let gap = from + rel;
        let line_start = gap + 2;
        let line_end = content[line_start..]
            .find('\n')
            .map(|p| line_start + p)
            .unwrap_or(content.len());
        let line = content[line_start..line_end].trim_end_matches('\r');
        let prefixed = MARKER_PREFIXES
            .iter()
            .any(|prefix| lower[line_start..].starts_with(prefix));
        if prefixed && line.ends_with("):") {
            let refs_start = (line_end + 1).min(content.len());
            return Some((gap, refs_start));
        }
        from = line_start;
    }
    None
}

/// The `- path` refs listed after the marker.
pub fn parse_refs(content: &str, refs_start: usize) -> Vec<String> {
    content[refs_start..]
        .lines()
        .filter_map(|line| {
            let path = line.trim_start().strip_prefix("- ")?.trim();
            (!path.is_empty()).then(|| path.to_string())
        })
        .collect()
}

/// The visible prompt with any trailer removed (the header alone suffices —
/// matches how prompt mapping has always normalized both sides).
pub fn strip(content: &str) -> &str {
    match find_marker(content) {
        Some((body_end, _)) => content[..body_end].trim_end(),
        None => content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_only_sends_keep_the_historical_bytes() {
        assert_eq!(
            with_refs("look", &["/u/ab-cat.png".into()]),
            "look\n\nAttached images (local files — open them to view):\n- /u/ab-cat.png"
        );
        assert_eq!(
            with_refs("", &["/u/a.JPG".into()]),
            "See the attached image(s).\n\nAttached images (local files — open them to view):\n- /u/a.JPG"
        );
        assert_eq!(with_refs("plain", &[]), "plain");
    }

    #[test]
    fn any_non_image_switches_to_the_files_header() {
        let mixed = with_refs("", &["/u/a.png".into(), "/u/report.pdf".into()]);
        assert_eq!(
            mixed,
            "See the attached file(s).\n\nAttached files (local files — open them to view):\n- /u/a.png\n- /u/report.pdf"
        );
        let (body_end, refs_start) = find_marker(&mixed).unwrap();
        assert!(is_placeholder_body(&mixed[..body_end]));
        assert_eq!(
            parse_refs(&mixed, refs_start),
            ["/u/a.png", "/u/report.pdf"]
        );
    }

    #[test]
    fn strip_accepts_both_headers_and_requires_the_colon_line() {
        assert_eq!(strip(&with_refs("hi", &["/a.png".into()])), "hi");
        assert_eq!(strip(&with_refs("hi", &["/a.zip".into()])), "hi");
        assert_eq!(
            strip("hi\n\nATTACHED FILES (local files):\n- /x"),
            "hi",
            "case-insensitive"
        );
        assert_eq!(
            strip("hi\n\nAttached files (local files: none"),
            "hi\n\nAttached files (local files: none"
        );
        assert_eq!(
            strip("hi\n\nattached notes follow"),
            "hi\n\nattached notes follow"
        );
    }

    #[test]
    fn image_detection_is_by_extension() {
        for path in [
            "/a.png",
            "/a.JPEG",
            "pending/id/image.webp",
            "/a.tiff",
            "/a.svg",
        ] {
            assert!(is_image_path(path), "{path}");
        }
        for path in [
            "/a.pdf",
            "/Makefile",
            "/a.heic",
            "/a.png.zip",
            "/dir.png/notes",
        ] {
            assert!(!is_image_path(path), "{path}");
        }
    }
}
